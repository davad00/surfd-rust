use bevy_surf::replay::RunReplay;
use bevy_surf::{
    network::RoomJoinTicket,
    record_protocol::{
        Attempt, AttemptRequest, DEFAULT_ENDPOINT, MAX_REPLAY_BYTES, ManagedRoomEntry,
        RankedSaveLookup, ReserveRankedSave, ServiceError, Submission, SubmissionLookup,
        SubmissionStatus, SubmitRun, replay_sha256,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{self, Read},
    time::Duration,
};

pub fn configured_endpoint() -> String {
    let default = if cfg!(test) {
        DEFAULT_ENDPOINT
    } else {
        "https://13.63.105.24"
    };
    std::env::var("SURF_RECORDS_URL").unwrap_or_else(|_| default.into())
}

pub struct RetryableRequestFailure {
    pub message: String,
    pub retry_after: Option<Duration>,
}

impl RetryableRequestFailure {
    fn permanent(message: String) -> Self {
        Self {
            message,
            retry_after: None,
        }
    }
}

pub fn request_attempt(
    base: &str,
    token: Option<&str>,
    attempt: &AttemptRequest,
) -> Result<Attempt, RetryableRequestFailure> {
    post_retryable(
        base,
        "/v1/attempts",
        token,
        attempt,
        Some(Duration::from_secs(60)),
    )
}

pub fn renew_room_ticket(
    base: &str,
    token: &str,
    room: &ManagedRoomEntry,
) -> Result<RoomJoinTicket, RetryableRequestFailure> {
    post_retryable(base, "/v1/room-tickets", Some(token), room, None)
}

fn post_retryable<T: DeserializeOwned>(
    base: &str,
    route: &str,
    token: Option<&str>,
    body: &impl Serialize,
    maximum_retry_after: Option<Duration>,
) -> Result<T, RetryableRequestFailure> {
    let (agent, url) =
        target(base, route, Duration::from_secs(6)).map_err(RetryableRequestFailure::permanent)?;
    let bytes = serde_json::to_vec(body)
        .map_err(|error| RetryableRequestFailure::permanent(error.to_string()))?;
    let response =
        send_json(&agent, &url, token, &bytes).map_err(|error| RetryableRequestFailure {
            message: format!("Records service: {error}"),
            retry_after: transient_request(&error).then_some(Duration::ZERO),
        })?;
    let status = response.status();
    let retry_after = response_retry_delay(status, response.headers())
        .filter(|delay| maximum_retry_after.is_none_or(|maximum| *delay <= maximum));
    decode_response(response, 4096).map_err(|error| {
        let retry_after = if status.is_success()
            && matches!(&error, ResponseFailure::Read(error) if transient_io(error.kind()))
        {
            Some(Duration::ZERO)
        } else {
            retry_after
        };
        RetryableRequestFailure {
            message: error.message(),
            retry_after,
        }
    })
}

fn transient_request(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => true,
        ureq::Error::Io(error) => transient_io(error.kind()),
        _ => false,
    }
}

#[derive(Debug)]
pub struct RankedSaveFailure {
    pub message: String,
    pub status: Option<u16>,
    pub retry_after: Option<Duration>,
}

pub struct RankedSaveReceipt {
    pub submission: Submission,
    pub restored: bool,
}

impl RankedSaveFailure {
    fn permanent(message: String) -> Self {
        Self {
            message,
            status: None,
            retry_after: None,
        }
    }
}

pub fn submit_ranked(
    base: &str,
    token: &str,
    submission: &SubmitRun,
    recover: bool,
) -> Result<RankedSaveReceipt, RankedSaveFailure> {
    let replay = ranked_json(&submission.replay)?;
    let lookup = SubmissionLookup {
        attempt: submission.attempt.clone(),
        replay_sha256: replay_sha256(&replay),
    };
    post_ranked_save(
        base,
        token,
        "/v1/runs",
        ranked_json(submission)?,
        ranked_json(&lookup)?,
        recover,
    )
}

pub fn submit_resumable(
    base: &str,
    token: &str,
    save_id: &str,
    replay: &RunReplay,
    attempt: Option<&str>,
    recover: bool,
) -> Result<RankedSaveReceipt, RankedSaveFailure> {
    #[derive(Serialize)]
    struct Upload<'a> {
        save_id: &'a str,
        replay: &'a RunReplay,
    }
    let upload = ranked_json(&Upload { save_id, replay })?;
    if upload.len() > MAX_REPLAY_BYTES {
        return Err(RankedSaveFailure::permanent(
            "The replay is too large to save.".into(),
        ));
    }
    let lookup = RankedSaveLookup {
        save_id: save_id.into(),
        replay_sha256: replay_sha256(&ranked_json(replay)?),
    };
    if let Some(attempt) = attempt {
        let (agent, url) = target(base, "/v1/runs/reserve", Duration::from_secs(6))
            .map_err(RankedSaveFailure::permanent)?;
        let body = ranked_json(&ReserveRankedSave {
            attempt: attempt.into(),
            save: lookup.clone(),
        })?;
        match retry_ranked(|_| ranked_response::<SubmissionStatus>(&agent, &url, token, &body)) {
            Ok(SubmissionStatus::Saved(submission)) => {
                return Ok(RankedSaveReceipt {
                    submission,
                    restored: true,
                });
            }
            Ok(SubmissionStatus::Pending) => {}
            Err(error) if error.status == Some(404) => {
                return submit_ranked(
                    base,
                    token,
                    &SubmitRun {
                        attempt: attempt.into(),
                        replay: replay.clone(),
                    },
                    recover,
                );
            }
            Err(error) => return Err(error),
        }
    }
    post_ranked_save(
        base,
        token,
        "/v1/runs/resume",
        upload,
        ranked_json(&lookup)?,
        recover || attempt.is_none(),
    )
}

fn ranked_json(value: &impl Serialize) -> Result<Vec<u8>, RankedSaveFailure> {
    serde_json::to_vec(value).map_err(|error| RankedSaveFailure::permanent(error.to_string()))
}

fn post_ranked_save(
    base: &str,
    token: &str,
    route: &str,
    upload_body: Vec<u8>,
    lookup_body: Vec<u8>,
    recover: bool,
) -> Result<RankedSaveReceipt, RankedSaveFailure> {
    if upload_body.len() > MAX_REPLAY_BYTES {
        return Err(RankedSaveFailure::permanent(
            "The replay is too large to save.".into(),
        ));
    }
    let (upload, upload_url) =
        target(base, route, Duration::from_secs(600)).map_err(RankedSaveFailure::permanent)?;
    let (lookup, lookup_url) = target(base, &format!("{route}/result"), Duration::from_secs(6))
        .map_err(RankedSaveFailure::permanent)?;
    retry_ranked(|retrying| {
        if recover || retrying {
            match ranked_response::<SubmissionStatus>(&lookup, &lookup_url, token, &lookup_body) {
                Ok(SubmissionStatus::Saved(submission)) => {
                    return Ok(RankedSaveReceipt {
                        submission,
                        restored: true,
                    });
                }
                Ok(SubmissionStatus::Pending) => {}
                Err(error) if error.status == Some(404) && route == "/v1/runs" => {}
                Err(error) => return Err(error),
            }
        }
        ranked_response(&upload, &upload_url, token, &upload_body).map(|submission| {
            RankedSaveReceipt {
                submission,
                restored: false,
            }
        })
    })
}

fn retry_ranked<T>(
    mut send: impl FnMut(bool) -> Result<T, RankedSaveFailure>,
) -> Result<T, RankedSaveFailure> {
    for retry in 0..3 {
        match send(retry != 0) {
            Err(error) if retry < 2 && error.retry_after.is_some() => {
                let backoff = Duration::from_secs(1 << retry);
                std::thread::sleep(error.retry_after.unwrap().max(backoff));
            }
            result => return result,
        }
    }
    unreachable!("final ranked save request returns its result")
}

fn ranked_response<T: DeserializeOwned>(
    agent: &ureq::Agent,
    url: &str,
    token: &str,
    body: &[u8],
) -> Result<T, RankedSaveFailure> {
    let response = send_json(agent, url, Some(token), body).map_err(|error| RankedSaveFailure {
        message: format!("Records service: {error}"),
        status: None,
        retry_after: transient_request(&error).then_some(Duration::ZERO),
    })?;
    let status = response.status();
    let retry_after = if status.is_success() {
        // A missing or truncated success response may follow a committed save.
        Some(Duration::ZERO)
    } else {
        attempt_retry_delay(status, response.headers())
    };
    decode_response(response, 16 * 1024).map_err(|error| RankedSaveFailure {
        message: error.message(),
        status: Some(status.as_u16()),
        retry_after,
    })
}

fn attempt_retry_delay(
    status: ureq::http::StatusCode,
    headers: &ureq::http::HeaderMap,
) -> Option<Duration> {
    response_retry_delay(status, headers).filter(|delay| *delay <= Duration::from_secs(60))
}

fn response_retry_delay(
    status: ureq::http::StatusCode,
    headers: &ureq::http::HeaderMap,
) -> Option<Duration> {
    let fallback = match status.as_u16() {
        429 => 60,
        408 | 500 | 502 | 503 | 504 => 2,
        _ => return None,
    };
    let seconds = match headers.get(ureq::http::header::RETRY_AFTER) {
        Some(value) => value.to_str().ok()?.parse::<u64>().ok()?,
        None => fallback,
    };
    Some(Duration::from_secs(seconds))
}

fn transient_io(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
    )
}

pub fn get<T: DeserializeOwned>(base: &str, route: &str, token: Option<&str>) -> Result<T, String> {
    get_with_timeout(base, route, token, Duration::from_secs(180))
}

pub fn get_fast<T: DeserializeOwned>(base: &str, route: &str) -> Result<T, String> {
    get_with_timeout(base, route, None, Duration::from_secs(3))
}

fn get_with_timeout<T: DeserializeOwned>(
    base: &str,
    route: &str,
    token: Option<&str>,
    timeout: Duration,
) -> Result<T, String> {
    let (agent, url) = target(base, route, timeout)?;
    let mut request = agent.get(&url);
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    read_response(request.call())
}

pub fn post<T: DeserializeOwned>(
    base: &str,
    route: &str,
    token: Option<&str>,
    body: &impl Serialize,
) -> Result<T, String> {
    let timeout = match route {
        "/v1/room-tickets" | "/v1/oauth/start" | "/v1/oauth/poll" => 6,
        _ => 180,
    };
    let (agent, url) = target(base, route, Duration::from_secs(timeout))?;
    let bytes = serde_json::to_vec(body).map_err(|error| error.to_string())?;
    read_response(send_json(&agent, &url, token, &bytes))
}

fn send_json(
    agent: &ureq::Agent,
    url: &str,
    token: Option<&str>,
    bytes: &[u8],
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let mut request = agent.post(url).header("Content-Type", "application/json");
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    request.send(bytes)
}

pub fn delete(base: &str, route: &str, token: &str) -> Result<bool, String> {
    let (agent, url) = target(base, route, Duration::from_secs(180))?;
    read_response(
        agent
            .delete(&url)
            .header("Authorization", format!("Bearer {token}"))
            .call(),
    )
}

fn target(base: &str, route: &str, timeout: Duration) -> Result<(ureq::Agent, String), String> {
    let base = base.trim().trim_end_matches('/');
    let uri: ureq::http::Uri = base.parse().map_err(|_| "Invalid records server address")?;
    let authority = uri.authority().ok_or("Records server needs a host")?;
    if authority.as_str().contains('@') || uri.query().is_some() || !matches!(uri.path(), "" | "/")
    {
        return Err("Enter the base address of the records server".into());
    }
    let scheme = uri.scheme_str();
    let local = matches!(authority.host(), "127.0.0.1" | "localhost" | "[::1]");
    if scheme != Some("https") && !(scheme == Some("http") && local) {
        return Err("Remote records servers require HTTPS".into());
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    Ok((agent, format!("{base}{route}")))
}

fn read_response<T: DeserializeOwned>(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<T, String> {
    let response = response.map_err(|error| format!("Records service: {error}"))?;
    decode_response(response, MAX_REPLAY_BYTES).map_err(ResponseFailure::message)
}

enum ResponseFailure {
    Read(io::Error),
    Message(String),
}

impl ResponseFailure {
    fn message(self) -> String {
        match self {
            Self::Read(_) => "Could not read the records response".into(),
            Self::Message(message) => message,
        }
    }
}

fn decode_response<T: DeserializeOwned>(
    mut response: ureq::http::Response<ureq::Body>,
    limit: usize,
) -> Result<T, ResponseFailure> {
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(ResponseFailure::Read)?;
    if bytes.len() > limit {
        return Err(ResponseFailure::Message(
            "Records response is too large".into(),
        ));
    }
    if !status.is_success() {
        return Err(ResponseFailure::Message(
            serde_json::from_slice::<ServiceError>(&bytes).map_or_else(
                |_| format!("Records service returned {status}"),
                |error| error.error,
            ),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| ResponseFailure::Message("Records service returned invalid data".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ureq::http::{HeaderMap, HeaderValue, StatusCode, header::RETRY_AFTER};

    #[test]
    fn attempt_retry_delays_respect_server_backoff_and_permanent_errors() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            attempt_retry_delay(StatusCode::SERVICE_UNAVAILABLE, &headers),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            attempt_retry_delay(StatusCode::TOO_MANY_REQUESTS, &headers),
            Some(Duration::from_secs(60))
        );
        headers.insert(RETRY_AFTER, HeaderValue::from_static("9"));
        assert_eq!(
            attempt_retry_delay(StatusCode::TOO_MANY_REQUESTS, &headers),
            Some(Duration::from_secs(9))
        );
        assert_eq!(
            attempt_retry_delay(StatusCode::UNAUTHORIZED, &headers),
            None
        );
        assert_eq!(attempt_retry_delay(StatusCode::NOT_FOUND, &headers), None);
        headers.insert(RETRY_AFTER, HeaderValue::from_static("61"));
        assert_eq!(
            attempt_retry_delay(StatusCode::SERVICE_UNAVAILABLE, &headers),
            None
        );
    }

    #[test]
    fn attempt_recovery_keeps_remote_https_requirements() {
        let request = AttemptRequest {
            map: "Downline".into(),
            bonus: None,
        };
        let result = request_attempt("http://records.example", None, &request);
        let failure = result.err().expect("remote plaintext is rejected");
        assert_eq!(failure.retry_after, None);
        assert_eq!(failure.message, "Remote records servers require HTTPS");
        assert!(transient_io(io::ErrorKind::ConnectionReset));
        assert!(!transient_io(io::ErrorKind::InvalidData));
    }

    #[test]
    fn room_ticket_renewal_classifies_a_real_busy_response_and_preserves_authentication() {
        use std::{io::Write, net::TcpListener};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let room = ManagedRoomEntry {
            room_id: "eu".into(),
            address: "tls://rooms.example.com:27165".into(),
        };
        let server = std::thread::spawn(move || {
            for (status, retry_after) in [
                ("503 Service Unavailable", "Retry-After: 90\r\n"),
                ("401 Unauthorized", ""),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                    assert!(header.len() < 4096);
                }
                let header = String::from_utf8(header).unwrap().to_ascii_lowercase();
                assert!(header.starts_with("post /v1/room-tickets http/1.1\r\n"));
                assert!(header.contains("authorization: bearer local-account\r\n"));
                let size = header
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut body = vec![0; size];
                stream.read_exact(&mut body).unwrap();
                let request: ManagedRoomEntry = serde_json::from_slice(&body).unwrap();
                assert_eq!(request.room_id, "eu");
                assert_eq!(request.address, "tls://rooms.example.com:27165");
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\n{retry_after}Content-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            }
        });
        let busy = renew_room_ticket(&base, "local-account", &room)
            .err()
            .unwrap();
        assert_eq!(busy.retry_after, Some(Duration::from_secs(90)));
        let revoked = renew_room_ticket(&base, "local-account", &room)
            .err()
            .unwrap();
        assert_eq!(revoked.retry_after, None);
        assert!(revoked.message.contains("401"));
        server.join().unwrap();
    }

    #[test]
    fn ranked_upload_recovers_a_lost_response_with_a_small_lookup() {
        use std::{io::Write, net::TcpListener, time::Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let submission = SubmitRun {
            attempt: "local-test-attempt".into(),
            replay: bevy_surf::replay::RunReplay {
                version: bevy_surf::replay::REPLAY_VERSION,
                map: "Downline".into(),
                revision: "builtin-downline-v1".into(),
                bonus: None,
                options: Default::default(),
                commands: Vec::new(),
            },
        };
        let expected = replay_sha256(&serde_json::to_vec(&submission.replay).unwrap());
        let server = std::thread::spawn(move || {
            for route in ["/v1/runs", "/v1/runs/result"] {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("expected ranked request: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                    assert!(header.len() < 4096);
                }
                let header = String::from_utf8(header).unwrap().to_ascii_lowercase();
                assert!(header.starts_with(&format!("post {route} http/1.1\r\n")));
                assert!(header.contains("authorization: bearer local-account\r\n"));
                let size = header
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut body = vec![0; size];
                stream.read_exact(&mut body).unwrap();
                if route == "/v1/runs" {
                    let uploaded: SubmitRun = serde_json::from_slice(&body).unwrap();
                    assert_eq!(uploaded.attempt, "local-test-attempt");
                    // The service committed the run, then the connection closed before it replied.
                    continue;
                }
                let lookup: SubmissionLookup = serde_json::from_slice(&body).unwrap();
                assert_eq!(lookup.attempt, "local-test-attempt");
                assert_eq!(lookup.replay_sha256, expected);
                let response = serde_json::to_vec(&SubmissionStatus::Saved(Submission {
                    time_us: 4_060_017,
                    personal_best: true,
                    run_id: Some("saved-run".into()),
                    stage_records: Vec::new(),
                    progression: None,
                }))
                .unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                )
                .unwrap();
                stream.write_all(&response).unwrap();
            }
        });
        let result = submit_ranked(&base, "local-account", &submission, false).unwrap();
        assert!(result.restored);
        assert_eq!(result.submission.run_id.as_deref(), Some("saved-run"));
        server.join().unwrap();
    }
}
