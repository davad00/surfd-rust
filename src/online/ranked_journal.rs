use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bevy_surf::{
    record_protocol::{MAX_REPLAY_BYTES, valid_ranked_save_id},
    replay::{MAX_COMMANDS, RunReplay},
};
use serde::{Deserialize, Serialize};

pub(super) const RECENT_RANKED_SAVES: usize = 4;
const MAX_ENTRY_BYTES: u64 = MAX_REPLAY_BYTES as u64 + 4096;
const RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PendingRankedSave {
    pub(super) save_id: String,
    pub(super) owner: i64,
    pub(super) origin: String,
    pub(super) replay: RunReplay,
    queued_at: u64,
}

impl PendingRankedSave {
    pub(super) fn new(owner: i64, origin: String, replay: RunReplay) -> io::Result<Self> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(io::Error::other)?;
        Ok(Self {
            save_id: format!("{:032x}", u128::from_le_bytes(nonce)),
            owner,
            origin,
            replay,
            queued_at: now(),
        })
    }

    fn valid(&self, expected_id: &str, now: u64) -> bool {
        self.save_id == expected_id
            && self.owner > 0
            && !self.origin.is_empty()
            && self.origin.len() <= 512
            && now.saturating_sub(self.queued_at) < RETENTION.as_secs()
            && self.queued_at <= now.saturating_add(300)
            && (1..=MAX_COMMANDS).contains(&self.replay.commands.len())
    }
}

#[derive(Clone)]
pub(super) struct RankedJournal(PathBuf);

impl RankedJournal {
    pub(super) fn new(data: &Path) -> Self {
        Self(data.join("ranked-pending"))
    }

    pub(super) fn load(&self) -> io::Result<Vec<PendingRankedSave>> {
        let entries = match fs::read_dir(&self.0) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let current = now();
        let mut pending = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let path = entry.path();
            let Some(id) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|id| valid_ranked_save_id(id))
            else {
                continue;
            };
            match path.extension().and_then(|extension| extension.to_str()) {
                Some("json") => {}
                Some("tmp") => {
                    if entry
                        .metadata()
                        .and_then(|meta| meta.modified())
                        .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age >= RETENTION))
                    {
                        let _ = fs::remove_file(&path);
                    }
                    continue;
                }
                _ => continue,
            }
            let mut file = match File::open(&path) {
                Ok(file) => file,
                Err(_) => continue,
            };
            let mut bytes = Vec::new();
            let Ok(size) = Read::take(&mut file, MAX_ENTRY_BYTES + 1).read_to_end(&mut bytes)
            else {
                continue;
            };
            let parsed = (size as u64 <= MAX_ENTRY_BYTES)
                .then_some(())
                .and_then(|_| serde_json::from_slice::<PendingRankedSave>(&bytes).ok())
                .filter(|save| save.valid(id, current));
            drop(file);
            if let Some(save) = parsed {
                pending.push(save);
                if pending.len() > RECENT_RANKED_SAVES {
                    let oldest = pending
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, save)| save.queued_at)
                        .map(|(index, _)| index)
                        .expect("pending saves");
                    pending.swap_remove(oldest);
                }
            } else {
                let _ = fs::remove_file(&path);
            }
        }
        pending.sort_unstable_by(|left, right| {
            right
                .queued_at
                .cmp(&left.queued_at)
                .then_with(|| left.save_id.cmp(&right.save_id))
        });
        pending.truncate(RECENT_RANKED_SAVES);
        Ok(pending)
    }

    pub(super) fn store(&self, save: &PendingRankedSave) -> io::Result<()> {
        if !valid_ranked_save_id(&save.save_id) || !save.valid(&save.save_id, now()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid pending ranked save",
            ));
        }
        fs::create_dir_all(&self.0)?;
        let path = self.0.join(format!("{}.json", save.save_id));
        if path.is_file() {
            return Ok(());
        }
        if self.load()?.len() >= RECENT_RANKED_SAVES {
            return Err(io::Error::other("pending ranked save storage is full"));
        }
        let bytes = serde_json::to_vec(save).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_ENTRY_BYTES {
            return Err(io::Error::other("pending ranked save is too large"));
        }
        let temporary = self.0.join(format!("{}.tmp", save.save_id));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        let result = file.write_all(&bytes).and_then(|()| file.sync_all());
        drop(file);
        let result = result.and_then(|()| fs::rename(&temporary, &path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        #[cfg(unix)]
        if result.is_ok() {
            let _ = File::open(&self.0).and_then(|directory| directory.sync_all());
        }
        result
    }

    pub(super) fn remove(&self, save_id: &str) -> io::Result<()> {
        if !valid_ranked_save_id(save_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid ranked save ID",
            ));
        }
        match fs::remove_file(self.0.join(format!("{save_id}.json"))) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_surf::{
        movement::MoveInput,
        record_protocol::{
            RankedSaveLookup, ResumeRankedSave, Submission, SubmissionStatus, replay_sha256,
        },
        replay::{REPLAY_VERSION, TickCommand},
    };
    use std::{net::TcpListener, time::Instant};

    #[test]
    fn reopened_journal_resumes_the_original_replay_without_a_saved_ticket_or_login() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let replay = RunReplay {
            version: REPLAY_VERSION,
            map: "Downline".into(),
            revision: "builtin-downline-v1".into(),
            bonus: None,
            options: Default::default(),
            commands: vec![TickCommand {
                input: MoveInput::default(),
                pitch: 0.0,
                restart_stage: false,
                source_yaw_degrees: None,
            }],
        };
        let saved = PendingRankedSave::new(23, origin.clone(), replay.clone()).unwrap();
        let root = std::env::temp_dir().join(format!("surfd-ranked-restart-{}", saved.save_id));
        let original = RankedJournal::new(&root);
        original.store(&saved).unwrap();
        let data = fs::read(original.0.join(format!("{}.json", saved.save_id))).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 5);
        assert!(json.get("attempt").is_none() && json.get("token").is_none());
        drop(original);
        let reopened = RankedJournal::new(&root);
        let restored = reopened.load().unwrap().pop().unwrap();
        assert_eq!(
            (restored.owner, restored.origin.as_str()),
            (23, origin.as_str())
        );
        assert_eq!(restored.replay, replay);
        assert_eq!(restored.save_id, saved.save_id);

        let id = saved.save_id.clone();
        let fingerprint = replay_sha256(&serde_json::to_vec(&replay).unwrap());
        let server = std::thread::spawn(move || {
            for route in ["/v1/runs/resume/result", "/v1/runs/resume"] {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(error) => panic!("expected resumed ranked request: {error}"),
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
                assert!(header.contains("authorization: bearer fresh-login\r\n"));
                let size = header
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut body = vec![0; size];
                stream.read_exact(&mut body).unwrap();
                let response = if route.ends_with("/result") {
                    let lookup: RankedSaveLookup = serde_json::from_slice(&body).unwrap();
                    assert_eq!(lookup.save_id, id);
                    assert_eq!(lookup.replay_sha256, fingerprint);
                    serde_json::to_vec(&SubmissionStatus::Pending).unwrap()
                } else {
                    let upload: ResumeRankedSave = serde_json::from_slice(&body).unwrap();
                    assert_eq!(upload.save_id, id);
                    assert_eq!(upload.replay, replay);
                    serde_json::to_vec(&Submission {
                        time_us: 4_060_017,
                        personal_best: true,
                        run_id: Some("saved-run".into()),
                        stage_records: Vec::new(),
                        progression: None,
                    })
                    .unwrap()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                )
                .unwrap();
                stream.write_all(&response).unwrap();
            }
        });
        let result = crate::records_client::submit_resumable(
            &restored.origin,
            "fresh-login",
            &restored.save_id,
            &restored.replay,
            None,
            true,
        )
        .unwrap();
        assert_eq!(result.submission.run_id.as_deref(), Some("saved-run"));
        server.join().unwrap();
        reopened.remove(&restored.save_id).unwrap();
        assert!(reopened.load().unwrap().is_empty());
        fs::remove_dir(reopened.0).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
