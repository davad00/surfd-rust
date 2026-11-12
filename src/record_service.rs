use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs,
    io::Read,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::{
    catalog::{self, MapMode},
    course::Course,
    loadout::Loadout,
    network::{
        RoomJoinTicket, RoomTicketKey, normalize_address, room_address_authority,
        room_address_is_loopback, room_address_uses_tls, ticket_time,
    },
    record_protocol::*,
    replay,
    rules::PhysicsProfile,
    social_protocol::{PlayerActivity, PresenceUpdate},
    social_service,
};

mod backup;
mod oauth;
pub use backup::write_backup;
use oauth::{OAuth, ProviderIdentity};

const DATABASE_FILE: &str = "records.sqlite3";
const SESSION_SECONDS: i64 = 7 * 24 * 60 * 60;
const ATTEMPT_SECONDS: i64 = 24 * 60 * 60;
const SUBMISSION_RECEIPT_SECONDS: i64 = 24 * 60 * 60;
const HTTP_WORKERS: usize = 4;
const VERIFICATION_WORKERS: usize = 2;
const VERIFICATION_QUEUE: usize = 4;
const VERIFICATION_RETRY_SECONDS: u64 = 3;
const OAUTH_PASSWORD: &str = "!oauth-only!";
const SCHEMA: &str = "
    PRAGMA foreign_keys = ON;
    PRAGMA journal_mode = WAL;
    CREATE TABLE IF NOT EXISTS accounts (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE COLLATE NOCASE,
        password TEXT NOT NULL,
        motto TEXT NOT NULL DEFAULT '',
        title TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE IF NOT EXISTS account_sequence (
        singleton INTEGER PRIMARY KEY CHECK (singleton=1),
        last_id INTEGER NOT NULL
    );
    INSERT INTO account_sequence(singleton,last_id)
        SELECT 1,coalesce(max(id),0) FROM accounts WHERE true
        ON CONFLICT(singleton) DO UPDATE SET last_id=max(last_id,excluded.last_id);
    CREATE TABLE IF NOT EXISTS sessions (
        token BLOB PRIMARY KEY,
        account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        expires INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS account_loadouts (
        account INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
        loadout TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS oauth_identities (
        provider TEXT NOT NULL CHECK (provider IN ('google', 'x')),
        subject TEXT NOT NULL,
        account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        PRIMARY KEY(provider,subject),
        UNIQUE(provider,account)
    );
    CREATE TABLE IF NOT EXISTS attempts (
        token BLOB PRIMARY KEY,
        account INTEGER REFERENCES accounts(id) ON DELETE CASCADE,
        map TEXT NOT NULL,
        bonus INTEGER NOT NULL,
        expires INTEGER NOT NULL,
        used INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS ranked_submission_receipts (
        attempt BLOB PRIMARY KEY,
        account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        replay_sha256 TEXT NOT NULL,
        response TEXT NOT NULL,
        expires INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS ranked_receipt_expiration ON ranked_submission_receipts(expires);
    CREATE INDEX IF NOT EXISTS ranked_receipt_account ON ranked_submission_receipts(account);
    CREATE TABLE IF NOT EXISTS ranked_replay_owners (
        replay_sha256 TEXT NOT NULL,
        account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        PRIMARY KEY(replay_sha256, account)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS ranked_save_reservations (
        id TEXT PRIMARY KEY,
        attempt BLOB NOT NULL UNIQUE,
        account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        replay_sha256 TEXT NOT NULL,
        expires INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS ranked_reservation_expiration ON ranked_save_reservations(expires);
    CREATE INDEX IF NOT EXISTS ranked_reservation_account ON ranked_save_reservations(account);
    CREATE TABLE IF NOT EXISTS best_runs (
        id TEXT PRIMARY KEY,
        account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        map TEXT NOT NULL,
        revision TEXT NOT NULL,
        bonus INTEGER NOT NULL,
        time_us INTEGER NOT NULL,
        splits TEXT NOT NULL,
        speeds TEXT NOT NULL,
        replay TEXT NOT NULL,
        UNIQUE(account, map, revision, bonus)
    );
    CREATE INDEX IF NOT EXISTS board_times ON best_runs(map, revision, bonus, time_us);
    CREATE TABLE IF NOT EXISTS best_run_details (
        id TEXT PRIMARY KEY REFERENCES best_runs(id) ON DELETE CASCADE ON UPDATE CASCADE,
        created_at INTEGER NOT NULL,
        ticks INTEGER NOT NULL,
        bonus_pack_revision TEXT
    );
    CREATE TABLE IF NOT EXISTS stage_replays (
        id TEXT PRIMARY KEY,
        account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        replay TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS stage_bests (
        account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        map TEXT NOT NULL,
        revision TEXT NOT NULL,
        bonus INTEGER NOT NULL,
        stage INTEGER NOT NULL,
        replay_id TEXT NOT NULL REFERENCES stage_replays(id) ON DELETE CASCADE,
        time_us INTEGER NOT NULL,
        start_tick REAL NOT NULL,
        end_tick REAL NOT NULL,
        PRIMARY KEY(account, map, revision, bonus, stage)
    );
    CREATE INDEX IF NOT EXISTS stage_board_times
        ON stage_bests(map, revision, bonus, stage, time_us, replay_id);
    CREATE INDEX IF NOT EXISTS stage_replay_refs ON stage_bests(replay_id);
    CREATE TABLE IF NOT EXISTS profile_extras (
        account INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
        banner TEXT NOT NULL DEFAULT '',
        showcase TEXT NOT NULL DEFAULT '[]',
        revision INTEGER NOT NULL DEFAULT 0
    );
";

struct Service {
    db: Mutex<Connection>,
    maps: PathBuf,
    courses: Mutex<VecDeque<(String, Arc<Course>)>>,
    limits: Mutex<HashMap<(IpAddr, &'static str), (Instant, usize)>>,
    trust_real_ip: bool,
    room_directory: RoomDirectory,
    oauth: OAuth,
}

enum RankedTicket<'a> {
    Token(&'a str),
    Reserved(&'a str),
}

#[derive(Default)]
struct RoomDirectory {
    rooms: Vec<(ManagedRoomEntry, RoomTicketKey)>,
    moderators: BTreeSet<i64>,
}

impl RoomDirectory {
    fn from_config(rooms: &str, moderators: &str) -> std::result::Result<Self, String> {
        let mut directory = Self::default();
        for config in rooms
            .split(';')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let mut fields = config.split('|').map(str::trim);
            let (Some(id), Some(address), Some(secret), mode, None) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                return Err(
                    "Use room_id|address|key or room_id|address|key|mode entries separated by semicolons in SURF_ROOM_TICKETS."
                        .into(),
                );
            };
            let mode = mode.map_or(Ok(crate::catalog::MapMode::Surf), |mode| {
                crate::catalog::MapMode::parse(mode)
                    .ok_or("Managed room mode must be surf or bhop.")
            })?;
            let key = RoomTicketKey::from_hex(id, secret)?;
            let address = normalize_address(address)?;
            if !room_address_is_loopback(&address) && !room_address_uses_tls(&address) {
                return Err("Managed room addresses outside loopback must use tls://.".into());
            }
            if directory.rooms.len() >= 16
                || directory.rooms.iter().any(|(entry, _)| {
                    entry.room_id == id
                        || room_address_authority(&entry.address)
                            == room_address_authority(&address)
                })
            {
                return Err(
                    "Configure up to 16 managed rooms with unique IDs and addresses.".into(),
                );
            }
            if directory
                .rooms
                .iter()
                .any(|(_, existing)| key.shares_secret_with(existing))
            {
                return Err("Use a different ticket key for each managed room.".into());
            }
            directory.rooms.push((
                ManagedRoomEntry {
                    room_id: id.into(),
                    address,
                    mode,
                },
                key,
            ));
        }
        for id in moderators
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let id = id
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or("Use comma-separated account IDs in SURF_ROOM_MODERATOR_IDS.")?;
            directory.moderators.insert(id);
        }
        Ok(directory)
    }
}

#[derive(Debug)]
struct ApiError {
    status: u16,
    message: String,
    retry_after: Option<u64>,
}

type Result<T> = std::result::Result<T, ApiError>;

impl ApiError {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
        }
    }

    fn rate_limited(retry_after: u64) -> Self {
        let mut error = Self::new(429, "Too many requests. Try again shortly.");
        error.retry_after = Some(retry_after);
        error
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(error: rusqlite::Error) -> Self {
        eprintln!("records database: {error}");
        Self::new(500, "The records database is unavailable")
    }
}

impl From<social_service::SocialError> for ApiError {
    fn from(error: social_service::SocialError) -> Self {
        match error {
            social_service::SocialError::Rule(message) => Self::new(400, message),
            social_service::SocialError::Database(error) => error.into(),
        }
    }
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    eprintln!("records service: {error}");
    ApiError::new(500, "The records service could not complete the request")
}

pub fn serve(address: SocketAddr, data: &Path, maps: &Path) -> std::result::Result<(), String> {
    if !address.ip().is_loopback() {
        return Err(
            "Bind the records service to loopback and put public HTTPS in front of it".into(),
        );
    }
    let room_directory = RoomDirectory::from_config(
        &std::env::var("SURF_ROOM_TICKETS").unwrap_or_default(),
        &std::env::var("SURF_ROOM_MODERATOR_IDS").unwrap_or_default(),
    )?;
    let oauth = OAuth::from_env()?;
    let trust_real_ip = match std::env::var("SURF_RECORDS_TRUST_REAL_IP") {
        Ok(value) if value == "1" => true,
        Ok(value) if value == "0" => false,
        Err(std::env::VarError::NotPresent) => false,
        _ => return Err("SURF_RECORDS_TRUST_REAL_IP must be 0 or 1.".into()),
    };
    fs::create_dir_all(data).map_err(|error| error.to_string())?;
    let mut db = Connection::open(data.join(DATABASE_FILE)).map_err(|error| error.to_string())?;
    db.execute_batch(SCHEMA)
        .map_err(|error| error.to_string())?;
    index_ranked_replay_owners(&mut db).map_err(|error| error.to_string())?;
    prune_submission_receipts(&db, unix_now()).map_err(|error| error.message)?;
    crate::social_service::init(&db).map_err(|error| error.to_string())?;
    let service = Arc::new(Service {
        db: Mutex::new(db),
        maps: maps.to_owned(),
        courses: Mutex::default(),
        limits: Mutex::default(),
        trust_real_ip,
        room_directory,
        oauth,
    });
    let http = Arc::new(Server::http(address).map_err(|error| error.to_string())?);
    println!("Surf records listening on http://{}", http.server_addr());
    std::thread::scope(|scope| {
        let (ranked, queue) = mpsc::sync_channel(VERIFICATION_QUEUE);
        let queue = Arc::new(Mutex::new(queue));
        for _ in 0..VERIFICATION_WORKERS {
            let queue = queue.clone();
            let service = service.clone();
            scope.spawn(move || {
                loop {
                    let request = queue.lock().ok().and_then(|queue| queue.recv().ok());
                    let Some(request) = request else { break };
                    service.respond(request);
                }
            });
        }
        for _ in 0..HTTP_WORKERS {
            let http = http.clone();
            let service = service.clone();
            let ranked = ranked.clone();
            scope.spawn(move || {
                while let Ok(request) = http.recv() {
                    service.accept_http(request, &ranked);
                }
            });
        }
        drop(ranked);
    });
    Ok(())
}

fn oauth_callback_route(route: &str) -> Option<(OAuthProvider, &str)> {
    let (path, query) = route.split_once('?').unwrap_or((route, ""));
    let provider = match path {
        "/v1/oauth/google/callback" => OAuthProvider::Google,
        "/v1/oauth/x/callback" => OAuthProvider::X,
        _ => return None,
    };
    Some((provider, query))
}

fn request_bearer(request: &Request) -> Option<&str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
        .and_then(|header| header.value.as_str().strip_prefix("Bearer "))
}

fn rate_source(peer: IpAddr, headers: &[Header], trust_real_ip: bool) -> IpAddr {
    let peer = peer.to_canonical();
    if !trust_real_ip || !peer.is_loopback() {
        return peer;
    }
    let mut forwarded = headers
        .iter()
        .filter(|header| header.field.equiv("X-Real-IP"));
    let Some(header) = forwarded.next() else {
        return peer;
    };
    if forwarded.next().is_some() {
        return peer;
    }
    header
        .value
        .as_str()
        .trim()
        .parse::<IpAddr>()
        .map_or(peer, |address| address.to_canonical())
}

fn send_http_response(request: Request, result: Result<Vec<u8>>) {
    let callback = oauth_callback_route(request.url()).is_some();
    let (status, body, retry_after) = match result {
        Ok(body) => (200, body, None),
        Err(error) if callback => (error.status, error.message.into_bytes(), None),
        Err(error) => (
            error.status,
            serde_json::to_vec(&ServiceError {
                error: error.message,
            })
            .unwrap_or_default(),
            error.retry_after,
        ),
    };
    let content_type = if callback {
        "text/plain; charset=utf-8"
    } else {
        "application/json"
    };
    let mut response = Response::from_data(body)
        .with_status_code(StatusCode(status))
        .with_header(Header::from_bytes("Content-Type", content_type).expect("valid header"))
        .with_header(Header::from_bytes("Cache-Control", "no-store").expect("valid header"));
    if callback {
        response = response
            .with_header(
                Header::from_bytes("Referrer-Policy", "no-referrer").expect("valid header"),
            )
            .with_header(
                Header::from_bytes(
                    "Content-Security-Policy",
                    "default-src 'none'; frame-ancestors 'none'",
                )
                .expect("valid header"),
            );
    }
    if let Some(seconds) = retry_after {
        response = response.with_header(
            Header::from_bytes("Retry-After", seconds.to_string()).expect("valid header"),
        );
    }
    let _ = request.respond(response);
}

impl Service {
    fn accept_http(&self, request: Request, ranked: &mpsc::SyncSender<Request>) {
        if request.method() != &Method::Post
            || !matches!(
                request.url().trim_start_matches('/'),
                "v1/runs" | "v1/runs/resume"
            )
        {
            self.respond(request);
            return;
        }
        if let Err(error) = self.admit_ranked(&request) {
            send_http_response(request, Err(error));
            return;
        }
        if let Err(error) = ranked.try_send(request) {
            let request = match error {
                mpsc::TrySendError::Full(request) | mpsc::TrySendError::Disconnected(request) => {
                    request
                }
            };
            let mut busy = ApiError::new(503, VERIFICATION_BUSY);
            busy.retry_after = Some(VERIFICATION_RETRY_SECONDS);
            send_http_response(request, Err(busy));
        }
    }

    fn admit_ranked(&self, request: &Request) -> Result<()> {
        self.limit(request, "verify", 20)?;
        if request
            .body_length()
            .is_some_and(|length| length > MAX_REPLAY_BYTES)
        {
            return Err(ApiError::new(413, "Request is too large"));
        }
        let db = self.db.lock().map_err(internal)?;
        account(&db, request_bearer(request))?;
        Ok(())
    }

    fn respond(&self, mut request: Request) {
        let result = self.dispatch(&mut request);
        send_http_response(request, result);
    }

    fn dispatch(&self, request: &mut Request) -> Result<Vec<u8>> {
        let method = request.method().clone();
        let route = request.url().to_owned();
        if let Some((provider, query)) = oauth_callback_route(&route) {
            if method != Method::Get {
                return Err(ApiError::new(
                    405,
                    "The sign-in callback requires a browser request.",
                ));
            }
            self.oauth.callback(provider, query)?;
            return Ok(b"Sign-in complete. You can close this tab and return to Surfd.".to_vec());
        }
        let bearer = request_bearer(request).map(str::to_owned);
        let parts: Vec<_> = route.trim_start_matches('/').split('/').collect();
        match (method, parts.as_slice()) {
            (Method::Get, ["v1", "status"]) => encode(&serde_json::json!({"version": 1})),
            (Method::Get, ["v1", "ready"]) => {
                let db = self.db.lock().map_err(internal)?;
                db.query_row(
                    "SELECT 1 FROM sqlite_schema WHERE name='best_runs' AND type='table'",
                    [],
                    |_| Ok(()),
                )
                .map_err(|_| ApiError::new(503, "The records database is not ready"))?;
                encode(&serde_json::json!({"version": 1, "ready": true}))
            }
            (Method::Get, ["v1", "rooms"]) => {
                let configured = std::env::var("SURF_SERVERS").unwrap_or_default();
                let mut rooms: Vec<_> = self
                    .room_directory
                    .rooms
                    .iter()
                    .map(|(entry, _)| entry.address.as_str())
                    .collect();
                for address in configured
                    .split(';')
                    .map(str::trim)
                    .filter(|address| !address.is_empty() && address.len() <= 255)
                    .take(16)
                {
                    if !rooms.contains(&address) {
                        rooms.push(address);
                    }
                }
                encode(&rooms)
            }
            (Method::Get, ["v1", "managed-rooms"]) => encode(
                &self
                    .room_directory
                    .rooms
                    .iter()
                    .map(|(entry, _)| entry)
                    .collect::<Vec<_>>(),
            ),
            (
                Method::Get,
                ["v1", "progression"]
                | ["v1", "progression", "surf"]
                | ["v1", "progression", "bhop"],
            ) => {
                let mode = parts
                    .get(2)
                    .and_then(|mode| MapMode::parse(mode))
                    .unwrap_or_default();
                let db = self.db.lock().map_err(internal)?;
                let own = bearer
                    .as_deref()
                    .map(|token| account(&db, Some(token)))
                    .transpose()?;
                encode(&progression(&db, own, mode)?)
            }
            (Method::Post, ["v1", "room-tickets"]) => {
                self.limit(
                    request,
                    "room-ticket",
                    if bearer.is_some() { 600 } else { 120 },
                )?;
                encode(&self.issue_room_ticket(bearer.as_deref(), read(request, 4096)?)?)
            }
            (Method::Post, ["v1", "register"]) => {
                self.limit(request, "auth", 20)?;
                encode(&self.register(read(request, 4096)?)?)
            }
            (Method::Get, ["v1", "oauth", "providers"]) => encode(&self.oauth.providers()),
            (Method::Post, ["v1", "oauth", "start"]) => {
                self.limit(request, "oauth-start", 120)?;
                let start: OAuthStartRequest = read(request, 4096)?;
                encode(&self.oauth.start(start.provider)?)
            }
            (Method::Post, ["v1", "oauth", "poll"]) => {
                let poll = read(request, 4096)?;
                encode(
                    &self
                        .oauth
                        .poll(poll, |identity| self.oauth_sign_in(identity))?,
                )
            }
            (Method::Post, ["v1", "login"]) => {
                self.limit(request, "auth", 20)?;
                encode(&self.login(read(request, 4096)?)?)
            }
            (Method::Post, ["v1", "logout"]) => {
                let mut db = self.db.lock().map_err(internal)?;
                let transaction = db.transaction()?;
                let key = token_hash(bearer.as_deref().ok_or_else(unauthorized)?)?;
                social_service::leave_presence(&transaction, &key, unix_now())?;
                transaction.execute("DELETE FROM sessions WHERE token=?1", [key])?;
                transaction.commit()?;
                encode(&true)
            }
            (Method::Get, ["v1", "profile"]) => {
                let db = self.db.lock().map_err(internal)?;
                let account = account(&db, bearer.as_deref())?;
                encode(&profile(&db, account)?)
            }
            (Method::Get, ["v1", "loadout"]) => {
                let db = self.db.lock().map_err(internal)?;
                let id = account(&db, bearer.as_deref())?;
                encode(&account_loadout(&db, id)?)
            }
            (Method::Post, ["v1", "loadout"]) => {
                encode(&self.set_loadout(bearer.as_deref(), read(request, 4096)?)?)
            }
            (Method::Post, ["v1", "profile"]) => {
                let changes: ProfileUpdate = read(request, 4096)?;
                encode(&self.update_profile(bearer.as_deref(), changes)?)
            }
            (Method::Post, ["v1", "profile", "extras"]) => {
                encode(&self.update_profile_extras(bearer.as_deref(), read(request, 4096)?)?)
            }
            (Method::Get, ["v1", "profiles", id]) => {
                let db = self.db.lock().map_err(internal)?;
                encode(&profile(&db, parse_account_id(id)?)?)
            }
            (Method::Get, ["v1", "profiles", id, "runs"]) => {
                let db = self.db.lock().map_err(internal)?;
                encode(&player_runs(
                    &db,
                    parse_account_id(id)?,
                    PlayerRunSort::BestPlace,
                    0,
                )?)
            }
            (Method::Get, ["v1", "profiles", id, "runs", sort, page]) => {
                let sort = match *sort {
                    "place" => PlayerRunSort::BestPlace,
                    "newest" => PlayerRunSort::Newest,
                    _ => return Err(ApiError::new(400, "Invalid run sort")),
                };
                let page = page
                    .parse::<u32>()
                    .map_err(|_| ApiError::new(400, "Invalid run page"))?;
                let db = self.db.lock().map_err(internal)?;
                encode(&player_runs(&db, parse_account_id(id)?, sort, page)?)
            }
            (Method::Get, ["v1", "playtime"]) => {
                let db = self.db.lock().map_err(internal)?;
                let actor = account(&db, bearer.as_deref())?;
                encode(&social_service::playtime(&db, actor, true)?)
            }
            (Method::Post, ["v1", "playtime"]) => {
                let changes: PlaytimePrivacy = read(request, 4096)?;
                let db = self.db.lock().map_err(internal)?;
                let actor = account(&db, bearer.as_deref())?;
                encode(&social_service::set_playtime_privacy(
                    &db,
                    actor,
                    changes.hidden,
                )?)
            }
            (Method::Get, ["v1", "profiles", id, "playtime"]) => {
                let db = self.db.lock().map_err(internal)?;
                let actor = parse_account_id(id)?;
                if !db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1)",
                    [actor],
                    |row| row.get::<_, bool>(0),
                )? {
                    return Err(ApiError::new(404, "Player not found"));
                }
                encode(&social_service::playtime(&db, actor, false)?)
            }
            (Method::Post, ["v1", "presence"]) => {
                self.limit(request, "presence", 120)?;
                encode(&self.presence(bearer.as_deref(), read(request, 4096)?)?)
            }
            (Method::Delete, ["v1", "profile"]) => {
                let db = self.db.lock().map_err(internal)?;
                let id = account(&db, bearer.as_deref())?;
                db.execute("DELETE FROM accounts WHERE id=?1", [id])?;
                encode(&true)
            }
            (Method::Post, ["v1", "attempts"]) => {
                self.limit(request, "attempt", 100)?;
                encode(&self.issue(bearer.as_deref(), read(request, 4096)?)?)
            }
            (Method::Post, ["v1", "runs"]) => {
                encode(&self.submit(bearer.as_deref(), read(request, MAX_REPLAY_BYTES)?)?)
            }
            (Method::Post, ["v1", "runs", "reserve"]) => {
                self.limit(request, "ranked-reserve", 100)?;
                encode(&self.reserve_ranked_save(bearer.as_deref(), read(request, 4096)?)?)
            }
            (Method::Post, ["v1", "runs", "resume"]) => {
                let request: ResumeRankedSave = read(request, MAX_REPLAY_BYTES)?;
                encode(&self.submit_ranked(
                    bearer.as_deref(),
                    RankedTicket::Reserved(&request.save_id),
                    request.replay,
                )?)
            }
            (Method::Post, ["v1", "runs", "resume", "result"]) => {
                self.limit(request, "ranked-result", 120)?;
                encode(&self.reserved_save_status(bearer.as_deref(), read(request, 4096)?)?)
            }
            (Method::Post, ["v1", "runs", "result"]) => {
                self.limit(request, "ranked-result", 120)?;
                encode(&self.submission_status(bearer.as_deref(), read(request, 4096)?)?)
            }
            (Method::Post, ["v1", "social"]) => {
                self.limit(request, "social", 120)?;
                let action = read(request, 8192)?;
                let mut db = self.db.lock().map_err(internal)?;
                let actor = account(&db, bearer.as_deref())?;
                let mut state = social_service::apply(&mut db, actor, action)?;
                state.can_review_message_reports = self.room_directory.moderators.contains(&actor);
                encode(&state)
            }
            (Method::Post, ["v1", "social", "message-reports"]) => {
                self.limit(request, "message-report", 30)?;
                let report = read(request, 1024)?;
                let mut db = self.db.lock().map_err(internal)?;
                let actor = account(&db, bearer.as_deref())?;
                encode(&social_service::report_message(&mut db, actor, report)?)
            }
            (Method::Get, ["v1", "social", "message-reports", cursor]) => {
                self.limit(request, "review-message-reports", 60)?;
                let db = self.db.lock().map_err(internal)?;
                let actor = account(&db, bearer.as_deref())?;
                if !self.room_directory.moderators.contains(&actor) {
                    return Err(ApiError::new(
                        403,
                        "Only moderators can review message reports.",
                    ));
                }
                let before = if *cursor == "latest" {
                    None
                } else {
                    Some(
                        cursor
                            .parse::<i64>()
                            .ok()
                            .filter(|id| *id > 0)
                            .ok_or_else(|| ApiError::new(400, "Invalid report page"))?,
                    )
                };
                encode(&social_service::message_reports(&db, before)?)
            }
            (Method::Get, ["v1", "boards", map, bonus]) => {
                encode(&self.board(map, parse_bonus(bonus)?)?)
            }
            (Method::Get, ["v1", "boards", map, bonus, "stages"]) => {
                encode(&self.stage_bests(map, parse_bonus(bonus)?, None)?)
            }
            (Method::Get, ["v1", "boards", map, bonus, "stages", "mine"]) => {
                let owner = account(&*self.db.lock().map_err(internal)?, bearer.as_deref())?;
                encode(&self.stage_bests(map, parse_bonus(bonus)?, Some(owner))?)
            }
            (Method::Get, ["v1", "boards", map, bonus, stage]) => {
                let stage = stage
                    .parse::<u16>()
                    .ok()
                    .filter(|stage| (1..=64).contains(stage))
                    .ok_or_else(|| ApiError::new(400, "Invalid stage number"))?;
                encode(&self.stage_board(map, parse_bonus(bonus)?, stage)?)
            }
            (Method::Get, ["v1", "replays", id])
                if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                let db = self.db.lock().map_err(internal)?;
                Ok(find_replay(&db, id)?.into_bytes())
            }
            _ => Err(ApiError::new(404, "Unknown records endpoint")),
        }
    }

    fn register(&self, credentials: Credentials) -> Result<SignedIn> {
        if !(3..=24).contains(&credentials.name.len())
            || !credentials
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || !(8..=256).contains(&credentials.password.len())
        {
            return Err(ApiError::new(
                400,
                "Use a 3–24 character name and an 8–256 character password",
            ));
        }
        let mut salt = [0; 16];
        getrandom::fill(&mut salt).map_err(internal)?;
        let salt = SaltString::encode_b64(&salt).map_err(internal)?;
        let password = Argon2::default()
            .hash_password(credentials.password.as_bytes(), &salt)
            .map_err(internal)?
            .to_string();
        let mut db = self.db.lock().map_err(internal)?;
        let transaction = db.transaction()?;
        let id = allocate_account_id(&transaction)?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO accounts(id,name,password) VALUES (?1,?2,?3)",
            params![id, credentials.name, password],
        )?;
        if inserted == 0 {
            return Err(ApiError::new(409, "That name is already in use"));
        }
        let signed_in = create_session(&transaction, id)?;
        transaction.commit()?;
        Ok(signed_in)
    }

    fn issue_room_ticket(
        &self,
        bearer: Option<&str>,
        request: RoomTicketRequest,
    ) -> Result<RoomJoinTicket> {
        let address =
            normalize_address(&request.address).map_err(|message| ApiError::new(400, message))?;
        let (entry, signer) = self
            .room_directory
            .rooms
            .iter()
            .find(|(entry, _)| entry.room_id == request.room_id && entry.address == address)
            .ok_or_else(|| {
                ApiError::new(
                    404,
                    "This room ID and address are not registered with the records service.",
                )
            })?;
        let (identity, loadout) = if let Some(token) = bearer {
            let db = self.db.lock().map_err(internal)?;
            let id = account(&db, Some(token))?;
            let name: String =
                db.query_row("SELECT name FROM accounts WHERE id=?1", [id], |row| {
                    row.get(0)
                })?;
            let points = account_progression(&db, id, entry.mode)?.points;
            let loadout = account_loadout(&db, id)?.unwrap_or_default();
            (
                Some((
                    id,
                    name,
                    self.room_directory.moderators.contains(&id),
                    points,
                )),
                loadout,
            )
        } else {
            (None, Loadout::default())
        };
        signer
            .issue_for_mode_with_loadout(entry.mode, identity, ticket_time(), loadout)
            .map_err(internal)
    }

    fn set_loadout(&self, bearer: Option<&str>, loadout: Loadout) -> Result<Loadout> {
        let db = self.db.lock().map_err(internal)?;
        let id = account(&db, bearer)?;
        db.execute(
            "INSERT INTO account_loadouts(account,loadout) VALUES (?1,?2)
             ON CONFLICT(account) DO UPDATE SET loadout=excluded.loadout
             WHERE account_loadouts.loadout<>excluded.loadout",
            params![id, serde_json::to_string(&loadout).map_err(internal)?],
        )?;
        Ok(loadout)
    }

    fn login(&self, credentials: Credentials) -> Result<SignedIn> {
        if credentials.name.len() > 24 || credentials.password.len() > 256 {
            return Err(unauthorized());
        }
        let row: Option<(i64, String)> = {
            let db = self.db.lock().map_err(internal)?;
            db.query_row(
                "SELECT id,password FROM accounts WHERE name=?1",
                [&credentials.name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
        };
        let (id, hash) = row.ok_or_else(unauthorized)?;
        if hash == OAUTH_PASSWORD {
            return Err(unauthorized());
        }
        let hash = PasswordHash::new(&hash).map_err(internal)?;
        Argon2::default()
            .verify_password(credentials.password.as_bytes(), &hash)
            .map_err(|_| unauthorized())?;
        let db = self.db.lock().map_err(internal)?;
        create_session(&db, id)
    }

    fn oauth_sign_in(&self, identity: &ProviderIdentity) -> Result<SignedIn> {
        let mut db = self.db.lock().map_err(internal)?;
        let transaction = db.transaction()?;
        let provider = identity.provider.id();
        let existing: Option<i64> = transaction
            .query_row(
                "SELECT account FROM oauth_identities WHERE provider=?1 AND subject=?2",
                params![provider, identity.subject],
                |row| row.get(0),
            )
            .optional()?;
        let id = if let Some(id) = existing {
            id
        } else {
            let id = allocate_account_id(&transaction)?;
            let mut stem: String = identity
                .name
                .chars()
                .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
                .take(16)
                .collect();
            if stem.len() < 3 {
                stem = "Surfer".to_owned();
            }
            let mut name = stem.clone();
            let mut created = false;
            for _ in 0..8 {
                if transaction.execute(
                    "INSERT OR IGNORE INTO accounts(id,name,password) VALUES (?1,?2,?3)",
                    params![id, name, OAUTH_PASSWORD],
                )? == 1
                {
                    created = true;
                    break;
                }
                name = format!("{stem}-{}", random_hex::<3>()?);
            }
            if !created {
                return Err(ApiError::new(
                    503,
                    "A player name could not be allocated. Try signing in again.",
                ));
            }
            transaction.execute(
                "INSERT INTO oauth_identities(provider,subject,account) VALUES (?1,?2,?3)",
                params![provider, identity.subject, id],
            )?;
            id
        };
        let signed_in = create_session(&transaction, id)?;
        transaction.commit()?;
        Ok(signed_in)
    }

    fn update_profile(&self, bearer: Option<&str>, changes: ProfileUpdate) -> Result<Profile> {
        let mut db = self.db.lock().map_err(internal)?;
        let transaction = db.transaction()?;
        let id = account(&transaction, bearer)?;
        let current = profile(&transaction, id)?;
        let changes = ProfileExtrasUpdate {
            motto: changes.motto,
            title: changes.title,
            banner: current.banner,
            showcase: current.showcase,
            revision: current.revision,
        };
        let profile = self.save_profile_extras(&transaction, id, changes)?;
        transaction.commit()?;
        Ok(profile)
    }

    fn update_profile_extras(
        &self,
        bearer: Option<&str>,
        changes: ProfileExtrasUpdate,
    ) -> Result<Profile> {
        let mut db = self.db.lock().map_err(internal)?;
        let transaction = db.transaction()?;
        let id = account(&transaction, bearer)?;
        let profile = self.save_profile_extras(&transaction, id, changes)?;
        transaction.commit()?;
        Ok(profile)
    }

    fn save_profile_extras(
        &self,
        db: &Connection,
        id: i64,
        changes: ProfileExtrasUpdate,
    ) -> Result<Profile> {
        let current = profile(db, id)?;
        if changes.revision != current.revision {
            return Err(ApiError::new(
                409,
                "The profile changed. Reload it and try again.",
            ));
        }
        let motto = changes.motto.trim();
        let title = changes.title.trim();
        if (motto != current.motto && motto.chars().count() > 60)
            || motto.chars().chain(title.chars()).any(char::is_control)
            || changes.showcase.len() > 3
        {
            return Err(ApiError::new(
                400,
                "Use up to 60 characters for your motto and up to three showcase maps",
            ));
        }
        let banner = match changes.banner.as_str() {
            "" => "",
            name => self.completed_profile_map(db, id, name)?,
        };
        let mut showcase = Vec::with_capacity(changes.showcase.len());
        for name in &changes.showcase {
            let map = self.completed_profile_map(db, id, name)?;
            if showcase.contains(&map) {
                return Err(ApiError::new(400, "Choose different showcase maps"));
            }
            showcase.push(map);
        }
        if !title.is_empty() && title != current.title && !self.earned_title(db, id, title)? {
            return Err(ApiError::new(400, "Choose a title you have earned"));
        }
        db.execute(
            "UPDATE accounts SET motto=?1,title=?2 WHERE id=?3",
            params![motto, title, id],
        )?;
        db.execute(
            "INSERT INTO profile_extras(account,banner,showcase,revision) VALUES (?1,?2,?3,1)
             ON CONFLICT(account) DO UPDATE SET banner=excluded.banner,showcase=excluded.showcase,revision=revision+1",
            params![id, banner, serde_json::to_string(&showcase).map_err(internal)?],
        )?;
        profile(db, id)
    }

    fn completed_profile_map(&self, db: &Connection, id: i64, name: &str) -> Result<&'static str> {
        let map = if let Some(entry) = catalog::entries()
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
        {
            &entry.id
        } else {
            canonical_map(name)?
        };
        let completed: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM best_runs WHERE account=?1 AND map=?2 AND bonus=0)",
            params![id, map],
            |row| row.get(0),
        )?;
        if !completed {
            return Err(ApiError::new(
                400,
                "Finish this map in a verified run to unlock it",
            ));
        }
        Ok(map)
    }

    fn earned_title(&self, db: &Connection, id: i64, title: &str) -> Result<bool> {
        if matches!(
            title,
            "Unranked"
                | "Rookie"
                | "Surfer"
                | "Regular"
                | "Skilled"
                | "Expert"
                | "Master"
                | "Legend"
        ) {
            return Ok(account_progression(db, id, MapMode::Surf)?.rank.label() == title);
        }
        if title == "Ten maps deep" {
            let completed: i64 = db.query_row(
                "SELECT count(DISTINCT map) FROM best_runs WHERE account=?1 AND bonus=0",
                [id],
                |row| row.get(0),
            )?;
            return Ok(completed >= 10);
        }
        let Some(name) = title.strip_suffix(" #1") else {
            return Ok(false);
        };
        let map = if name == "Downline" {
            "Downline"
        } else if let Some(entry) = catalog::entries().iter().find(|entry| entry.name == name) {
            &entry.id
        } else {
            return Ok(false);
        };
        let revision = self.board_revision(map, None)?;
        let first: Option<i64> = db
            .query_row(
                "SELECT account FROM best_runs WHERE map=?1 AND revision=?2 AND bonus=0
             ORDER BY time_us,id LIMIT 1",
                params![map, revision],
                |row| row.get(0),
            )
            .optional()?;
        Ok(first == Some(id))
    }

    fn presence(&self, bearer: Option<&str>, changes: PresenceUpdate) -> Result<Playtime> {
        let activity = match changes.activity {
            PlayerActivity::Menu => PlayerActivity::Menu,
            PlayerActivity::Solo { map } => PlayerActivity::Solo {
                map: activity_map(map)?,
            },
            PlayerActivity::Room { map, room } => {
                if !social_service::valid_invite_address(&room) {
                    return Err(ApiError::new(400, "Invalid room address"));
                }
                PlayerActivity::Room {
                    map: activity_map(map)?,
                    room,
                }
            }
        };
        let db = self.db.lock().map_err(internal)?;
        let actor = account(&db, bearer)?;
        let session = token_hash(bearer.ok_or_else(unauthorized)?)?;
        Ok(social_service::update_presence(
            &db,
            actor,
            &session,
            &activity,
            changes.active,
            unix_now(),
        )?)
    }

    fn issue(&self, bearer: Option<&str>, request: AttemptRequest) -> Result<Attempt> {
        let map = canonical_map(&request.map)?;
        if request.bonus == Some(0) || request.bonus.is_some_and(|bonus| bonus > 128) {
            return Err(ApiError::new(400, "Invalid bonus number"));
        }
        let db = self.db.lock().map_err(internal)?;
        let owner = bearer.map(|token| account(&db, Some(token))).transpose()?;
        let token = random_hex::<32>()?;
        let now = unix_now();
        db.execute("DELETE FROM attempts WHERE expires<=?1", [now])?;
        prune_submission_receipts(&db, now)?;
        db.execute(
            "INSERT INTO attempts(token,account,map,bonus,expires) VALUES (?1,?2,?3,?4,?5)",
            params![
                token_hash(&token)?,
                owner,
                map,
                request.bonus.unwrap_or(0),
                now + ATTEMPT_SECONDS
            ],
        )?;
        Ok(Attempt { token })
    }

    fn submit(&self, bearer: Option<&str>, submission: SubmitRun) -> Result<Submission> {
        self.submit_ranked(
            bearer,
            RankedTicket::Token(&submission.attempt),
            submission.replay,
        )
    }

    fn submit_ranked(
        &self,
        bearer: Option<&str>,
        source: RankedTicket<'_>,
        replay: replay::RunReplay,
    ) -> Result<Submission> {
        let map = canonical_map(&replay.map)?;
        let bonus = replay.bonus.unwrap_or(0);
        let payload = serde_json::to_string(&replay).map_err(internal)?;
        let fingerprint = replay_sha256(payload.as_bytes());
        let (owner, ticket) = {
            let db = self.db.lock().map_err(internal)?;
            let owner = account(&db, bearer)?;
            let now = unix_now();
            let ticket = match source {
                RankedTicket::Token(token) => token_hash(token)?,
                RankedTicket::Reserved(save_id) => {
                    reserved_ticket(&db, save_id, owner, &fingerprint, now)?
                }
            };
            if let Some(saved) = submission_receipt(&db, &ticket, owner, &fingerprint, now)? {
                return Ok(saved);
            }
            let valid = db.query_row("SELECT EXISTS(SELECT 1 FROM attempts
                WHERE token=?1 AND map=?2 AND bonus=?3 AND expires>?4 AND used=0 AND (account IS NULL OR account=?5)
                AND NOT EXISTS(SELECT 1 FROM ranked_save_reservations r WHERE r.attempt=attempts.token
                  AND (r.account<>?5 OR r.replay_sha256<>?6)))",
                params![ticket, map, bonus, now, owner, fingerprint], |row| row.get::<_, bool>(0))?;
            if !valid {
                return Err(unavailable_attempt());
            }
            (owner, ticket)
        };
        let course = self.course(map)?;
        let verified = replay::verify_ranked(&course, &replay)
            .map_err(|error| ApiError::new(422, format!("Replay did not verify: {error}")))?;
        let revision = course
            .board_revision(replay.bonus)
            .ok_or_else(|| ApiError::new(422, "This course has no ranked revision"))?;
        let revision = versioned_revision(&revision);
        let splits = serde_json::to_string(&verified.splits_us).map_err(internal)?;
        let speeds = serde_json::to_string(&verified.checkpoint_speeds).map_err(internal)?;
        let id = random_hex::<16>()?;
        let mut db = self.db.lock().map_err(internal)?;
        let transaction = db.transaction()?;
        if account(&transaction, bearer)? != owner {
            return Err(unauthorized());
        }
        let now = unix_now();
        if let Some(saved) = submission_receipt(&transaction, &ticket, owner, &fingerprint, now)? {
            return Ok(saved);
        }
        let distinct_replay = replay.has_distinct_aim_pattern();
        if distinct_replay && copied_ranked_replay(&transaction, owner, &fingerprint)? {
            return Err(ApiError::new(
                409,
                "This replay was already published by another account.",
            ));
        }
        let consumed = transaction.execute(
            "UPDATE attempts SET used=1
             WHERE token=?1 AND expires>?2 AND used=0 AND (account IS NULL OR account=?3)
             AND NOT EXISTS(SELECT 1 FROM ranked_save_reservations r WHERE r.attempt=attempts.token
               AND (r.account<>?3 OR r.replay_sha256<>?4))",
            params![ticket, now, owner, fingerprint],
        )?;
        if consumed != 1 {
            return Err(unavailable_attempt());
        }
        let changed = transaction.execute("INSERT INTO best_runs(id,account,map,revision,bonus,time_us,splits,speeds,replay) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(account,map,revision,bonus) DO UPDATE SET id=excluded.id,time_us=excluded.time_us,splits=excluded.splits,speeds=excluded.speeds,replay=excluded.replay WHERE excluded.time_us<best_runs.time_us",
            params![id,owner,map,revision,bonus,verified.time_us,splits,speeds,payload])?;
        if changed == 1 {
            let pack = (bonus != 0).then(|| bonus_pack_revision(map)).flatten();
            transaction.execute(
                "INSERT INTO best_run_details(id,created_at,ticks,bonus_pack_revision)
                 VALUES (?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET
                 created_at=excluded.created_at,ticks=excluded.ticks,
                 bonus_pack_revision=excluded.bonus_pack_revision",
                params![id, now, replay.commands.len(), pack],
            )?;
        }
        let (stage_improved, stage_records) = record_stages(
            &transaction,
            StageSubmission {
                owner,
                map,
                revision: &revision,
                bonus,
                id: &id,
                payload: &payload,
                stages: &verified.stages,
            },
        )?;
        if distinct_replay && (changed == 1 || stage_improved) {
            remember_ranked_replay(&transaction, owner, &fingerprint)?;
        }
        let mode = catalog::find(map).map_or(MapMode::Surf, |entry| entry.mode);
        let progression = (changed == 1)
            .then(|| account_progression(&transaction, owner, mode))
            .transpose()?;
        let saved = Submission {
            time_us: verified.time_us,
            personal_best: changed == 1,
            run_id: (changed == 1 || stage_improved).then_some(id),
            stage_records,
            progression: progression.filter(|_| mode == MapMode::Surf),
            bhop_progression: progression.filter(|_| mode == MapMode::Bhop),
        };
        prune_submission_receipts(&transaction, now)?;
        transaction.execute(
            "INSERT INTO ranked_submission_receipts(attempt,account,replay_sha256,response,expires)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                ticket,
                owner,
                fingerprint,
                serde_json::to_string(&saved).map_err(internal)?,
                now + SUBMISSION_RECEIPT_SECONDS
            ],
        )?;
        transaction.execute(
            "UPDATE ranked_save_reservations SET expires=?2 WHERE attempt=?1",
            params![ticket, now + SUBMISSION_RECEIPT_SECONDS],
        )?;
        transaction.commit()?;
        Ok(saved)
    }

    fn submission_status(
        &self,
        bearer: Option<&str>,
        lookup: SubmissionLookup,
    ) -> Result<SubmissionStatus> {
        let db = self.db.lock().map_err(internal)?;
        let owner = account(&db, bearer)?;
        let ticket = token_hash(&lookup.attempt)?;
        validate_replay_fingerprint(&lookup.replay_sha256)?;
        ranked_submission_state(&db, &ticket, owner, &lookup.replay_sha256, unix_now())
    }

    fn reserve_ranked_save(
        &self,
        bearer: Option<&str>,
        request: ReserveRankedSave,
    ) -> Result<SubmissionStatus> {
        let mut db = self.db.lock().map_err(internal)?;
        let transaction = db.transaction()?;
        let owner = account(&transaction, bearer)?;
        let ticket = token_hash(&request.attempt)?;
        let save = request.save;
        if !valid_ranked_save_id(&save.save_id) {
            return Err(ApiError::new(400, "Invalid ranked save ID"));
        }
        validate_replay_fingerprint(&save.replay_sha256)?;
        let now = unix_now();
        prune_submission_receipts(&transaction, now)?;
        let reserved: Option<(Vec<u8>, i64, String)> = transaction
            .query_row(
                "SELECT attempt,account,replay_sha256 FROM ranked_save_reservations WHERE id=?1",
                [&save.save_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((attempt, claimant, fingerprint)) = reserved {
            if claimant != owner || attempt != ticket || fingerprint != save.replay_sha256 {
                return Err(unavailable_attempt());
            }
        } else {
            let claimed = transaction.execute(
                "UPDATE attempts SET account=?2
                 WHERE token=?1 AND used=0 AND expires>?3 AND (account IS NULL OR account=?2)
                 AND NOT EXISTS(SELECT 1 FROM ranked_save_reservations WHERE attempt=?1)",
                params![ticket, owner, now],
            )?;
            if claimed != 1 {
                return Err(unavailable_attempt());
            }
            transaction.execute(
                "INSERT INTO ranked_save_reservations(id,attempt,account,replay_sha256,expires)
                 SELECT ?1,token,account,?2,expires FROM attempts WHERE token=?3",
                params![save.save_id, save.replay_sha256, ticket],
            )?;
        }
        let state =
            ranked_submission_state(&transaction, &ticket, owner, &save.replay_sha256, now)?;
        transaction.commit()?;
        Ok(state)
    }

    fn reserved_save_status(
        &self,
        bearer: Option<&str>,
        lookup: RankedSaveLookup,
    ) -> Result<SubmissionStatus> {
        let db = self.db.lock().map_err(internal)?;
        let owner = account(&db, bearer)?;
        validate_replay_fingerprint(&lookup.replay_sha256)?;
        let now = unix_now();
        let ticket = reserved_ticket(&db, &lookup.save_id, owner, &lookup.replay_sha256, now)?;
        ranked_submission_state(&db, &ticket, owner, &lookup.replay_sha256, now)
    }

    fn board_revision(&self, map: &str, bonus: Option<u16>) -> Result<String> {
        let revision = if let Some(bonus) = bonus {
            self.course(map)?.board_revision(Some(bonus))
        } else if map == "Downline" {
            Course::practice().board_revision(None)
        } else {
            catalog::find(map).and_then(|entry| entry.native_board_revision())
        }
        .ok_or_else(|| ApiError::new(404, "This board does not exist"))?;
        Ok(versioned_revision(&revision))
    }

    fn board(&self, name: &str, bonus: Option<u16>) -> Result<Board> {
        let map = canonical_map(name)?;
        let revision = self.board_revision(map, bonus)?;
        let db = self.db.lock().map_err(internal)?;
        let mut query = db.prepare("SELECT best_runs.id,accounts.name,time_us,splits,speeds,account FROM best_runs JOIN accounts ON accounts.id=best_runs.account WHERE map=?1 AND revision=?2 AND bonus=?3 ORDER BY time_us,best_runs.id LIMIT 50")?;
        let rows = query.query_map(params![map, revision, bonus.unwrap_or(0)], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let rows = rows
            .map(|row| {
                let (id, name, time_us, splits, speeds, account) = row?;
                Ok(BoardRow {
                    id,
                    name,
                    account_id: Some(account),
                    time_us,
                    splits_us: serde_json::from_str(&splits).map_err(internal)?,
                    checkpoint_speeds: serde_json::from_str(&speeds).map_err(internal)?,
                    stage_span: None,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Board {
            map: map.into(),
            bonus,
            stage: None,
            rows,
        })
    }

    fn stage_board(&self, name: &str, bonus: Option<u16>, stage: u16) -> Result<Board> {
        let map = canonical_map(name)?;
        let revision = self.board_revision(map, bonus)?;
        let db = self.db.lock().map_err(internal)?;
        let mut query = db.prepare(
            "SELECT stage,replay_id,name,time_us,start_tick,end_tick,account
             FROM stage_bests JOIN accounts ON accounts.id=stage_bests.account
             WHERE map=?1 AND revision=?2 AND bonus=?3 AND stage=?4
             ORDER BY time_us,replay_id LIMIT 50",
        )?;
        let rows = query
            .query_map(params![map, revision, bonus.unwrap_or(0), stage], stage_row)?
            .map(|row| row.map(|row| row.row))
            .collect::<rusqlite::Result<_>>()?;
        Ok(Board {
            map: map.into(),
            bonus,
            stage: Some(stage),
            rows,
        })
    }

    fn stage_bests(
        &self,
        name: &str,
        bonus: Option<u16>,
        owner: Option<i64>,
    ) -> Result<StageBests> {
        let map = canonical_map(name)?;
        let revision = self.board_revision(map, bonus)?;
        let db = self.db.lock().map_err(internal)?;
        let sql = if owner.is_some() {
            "SELECT stage,replay_id,name,time_us,start_tick,end_tick,account
             FROM stage_bests JOIN accounts ON accounts.id=stage_bests.account
             WHERE map=?1 AND revision=?2 AND bonus=?3 AND account=?4 ORDER BY stage"
        } else {
            "WITH ranked AS (
                 SELECT stage,replay_id,name,time_us,start_tick,end_tick,account,
                     row_number() OVER (PARTITION BY stage ORDER BY time_us,replay_id) AS place
                 FROM stage_bests JOIN accounts ON accounts.id=stage_bests.account
                 WHERE map=?1 AND revision=?2 AND bonus=?3
             )
             SELECT stage,replay_id,name,time_us,start_tick,end_tick,account
             FROM ranked WHERE place=1 ORDER BY stage"
        };
        let mut query = db.prepare(sql)?;
        let rows = match owner {
            Some(owner) => {
                query.query_map(params![map, revision, bonus.unwrap_or(0), owner], stage_row)?
            }
            None => query.query_map(params![map, revision, bonus.unwrap_or(0)], stage_row)?,
        }
        .collect::<rusqlite::Result<_>>()?;
        Ok(StageBests {
            map: map.into(),
            bonus,
            rows,
        })
    }

    fn course(&self, map: &str) -> Result<Arc<Course>> {
        let mut cache = self.courses.lock().map_err(internal)?;
        if let Some(index) = cache.iter().position(|(name, _)| name == map) {
            let entry = cache.remove(index).expect("index from cache");
            let result = entry.1.clone();
            cache.push_front(entry);
            return Ok(result);
        }
        let course = Arc::new(
            catalog::files::load_ranked(&self.maps, map).map_err(|error| {
                ApiError::new(503, format!("Ranked course is unavailable: {error}"))
            })?,
        );
        cache.push_front((map.into(), course.clone()));
        cache.truncate(2);
        Ok(course)
    }

    fn limit(&self, request: &Request, group: &'static str, maximum: usize) -> Result<()> {
        let Some(address) = request.remote_addr() else {
            return Ok(());
        };
        let source = rate_source(address.ip(), request.headers(), self.trust_real_ip);
        let now = Instant::now();
        let mut limits = self.limits.lock().map_err(internal)?;
        limits.retain(|_, (started, _)| now.duration_since(*started) < Duration::from_secs(60));
        if limits.len() >= 4096 && !limits.contains_key(&(source, group)) {
            return Err(ApiError::rate_limited(60));
        }
        let (started, count) = limits.entry((source, group)).or_insert((now, 0));
        if *count >= maximum {
            let remaining = Duration::from_secs(60).saturating_sub(now.duration_since(*started));
            return Err(ApiError::rate_limited(
                remaining.as_millis().div_ceil(1000).max(1) as u64,
            ));
        }
        *count += 1;
        Ok(())
    }
}

fn read<T: DeserializeOwned>(request: &mut Request, limit: usize) -> Result<T> {
    if request.body_length().is_some_and(|length| length > limit) {
        return Err(ApiError::new(413, "Request is too large"));
    }
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ApiError::new(400, "Request could not be read"))?;
    if bytes.len() > limit {
        return Err(ApiError::new(413, "Request is too large"));
    }
    serde_json::from_slice(&bytes).map_err(|_| ApiError::new(400, "Invalid request data"))
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(internal)
}

fn parse_bonus(bonus: &str) -> Result<Option<u16>> {
    let number = bonus
        .parse::<u16>()
        .map_err(|_| ApiError::new(400, "Invalid bonus number"))?;
    Ok((number != 0).then_some(number))
}

fn find_replay(db: &Connection, id: &str) -> Result<String> {
    db.query_row(
        "SELECT replay FROM best_runs WHERE id=?1
         UNION ALL SELECT replay FROM stage_replays WHERE id=?1 LIMIT 1",
        [id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| ApiError::new(404, "Replay not found"))
}

fn unavailable_attempt() -> ApiError {
    ApiError::new(409, "This ranked attempt has expired or already been used")
}

fn copied_ranked_replay(db: &Connection, owner: i64, fingerprint: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM ranked_replay_owners WHERE replay_sha256=?1)
          AND NOT EXISTS(SELECT 1 FROM ranked_replay_owners WHERE replay_sha256=?1 AND account=?2)",
        params![fingerprint, owner],
        |row| row.get(0),
    )
}

fn remember_ranked_replay(db: &Connection, owner: i64, fingerprint: &str) -> rusqlite::Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO ranked_replay_owners(replay_sha256,account) VALUES (?1,?2)",
        params![fingerprint, owner],
    )?;
    Ok(())
}

fn index_ranked_replay_owners(db: &mut Connection) -> rusqlite::Result<()> {
    let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= 1 {
        return Ok(());
    }
    let transaction = db.transaction()?;
    {
        let mut statement = transaction.prepare(
            "SELECT account,replay FROM best_runs UNION ALL SELECT account,replay FROM stage_replays",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let payload: String = row.get(1)?;
            if serde_json::from_str::<replay::RunReplay>(&payload)
                .is_ok_and(|replay| replay.has_distinct_aim_pattern())
            {
                remember_ranked_replay(
                    &transaction,
                    row.get(0)?,
                    &replay_sha256(payload.as_bytes()),
                )?;
            }
        }
    }
    transaction.pragma_update(None, "user_version", 1)?;
    transaction.commit()
}

fn prune_submission_receipts(db: &Connection, now: i64) -> Result<()> {
    db.execute(
        "DELETE FROM ranked_submission_receipts WHERE expires<=?1",
        [now],
    )?;
    db.execute(
        "DELETE FROM ranked_save_reservations WHERE expires<=?1",
        [now],
    )?;
    Ok(())
}

fn validate_replay_fingerprint(fingerprint: &str) -> Result<()> {
    if fingerprint.len() != 64
        || !fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ApiError::new(400, "Invalid replay fingerprint"));
    }
    Ok(())
}

fn reserved_ticket(
    db: &Connection,
    save_id: &str,
    owner: i64,
    fingerprint: &str,
    now: i64,
) -> Result<Vec<u8>> {
    if !valid_ranked_save_id(save_id) {
        return Err(ApiError::new(400, "Invalid ranked save ID"));
    }
    db.query_row(
        "SELECT attempt FROM ranked_save_reservations
        WHERE id=?1 AND account=?2 AND replay_sha256=?3 AND expires>?4",
        params![save_id, owner, fingerprint, now],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(unavailable_attempt)
}

fn ranked_submission_state(
    db: &Connection,
    ticket: &[u8],
    owner: i64,
    fingerprint: &str,
    now: i64,
) -> Result<SubmissionStatus> {
    if let Some(saved) = submission_receipt(db, ticket, owner, fingerprint, now)? {
        return Ok(SubmissionStatus::Saved(saved));
    }
    let pending = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts WHERE token=?1 AND expires>?2 AND used=0
         AND (account IS NULL OR account=?3)
         AND NOT EXISTS(SELECT 1 FROM ranked_save_reservations r WHERE r.attempt=attempts.token
           AND (r.account<>?3 OR r.replay_sha256<>?4)))",
        params![ticket, now, owner, fingerprint],
        |row| row.get::<_, bool>(0),
    )?;
    if pending {
        Ok(SubmissionStatus::Pending)
    } else {
        Err(unavailable_attempt())
    }
}

fn submission_receipt(
    db: &Connection,
    ticket: &[u8],
    owner: i64,
    fingerprint: &str,
    now: i64,
) -> Result<Option<Submission>> {
    let result: Option<String> = db
        .query_row(
            "SELECT response FROM ranked_submission_receipts
         WHERE attempt=?1 AND account=?2 AND replay_sha256=?3 AND expires>?4",
            params![ticket, owner, fingerprint, now],
            |row| row.get(0),
        )
        .optional()?;
    result
        .map(|body| serde_json::from_str(&body).map_err(internal))
        .transpose()
}

fn bonus_pack_revision(map: &str) -> Option<&'static str> {
    let entry = catalog::find(map)?;
    if let Some(source) = &entry.source_bsp {
        return Some(&source.timer_sha256);
    }
    catalog::revision(entry.bonus_pack.as_deref()?)
}

fn replay_course_available(
    map: &str,
    bonus: Option<u16>,
    revision: &str,
    saved_bonus_pack: Option<&str>,
) -> bool {
    let Some((version, board)) = revision.split_once(':') else {
        return false;
    };
    if version != "v1" && version != format!("v{}", replay::REPLAY_VERSION) {
        return false;
    }
    if let Some(entry) = catalog::find(map)
        && let Some(source) = &entry.source_bsp
    {
        if version != format!("v{}", replay::REPLAY_VERSION) {
            return false;
        }
        return if let Some(number) = bonus {
            number > 0
                && usize::from(number) <= entry.bonuses
                && saved_bonus_pack.is_none_or(|saved| saved == source.timer_sha256)
                && source.bonus_board_revision(number).as_deref() == Some(board)
        } else {
            entry.native_board_revision().as_deref() == Some(board)
        };
    }
    let (board, profile) = PhysicsProfile::split_revision(board);
    let builtin;
    let geometry = if map == "Downline" {
        if profile != PhysicsProfile::Legacy {
            return false;
        }
        builtin = Course::practice().catalog_revision;
        builtin.as_deref()
    } else {
        catalog::find(map)
            .filter(|entry| {
                profile != PhysicsProfile::AuthoredContacts || entry.had_authored_contacts()
            })
            .and_then(|entry| catalog::revision(&entry.geometry))
    };
    let Some(geometry) = geometry else {
        return false;
    };
    if let Some(number) = bonus {
        bonus_pack_revision(map)
            .is_some_and(|pack| saved_bonus_pack.is_none_or(|saved| saved == pack))
            && board.starts_with(&format!("{geometry}:bonus:{number}:"))
    } else {
        profile != PhysicsProfile::SurfdBonus && board == geometry
    }
}

fn player_runs(
    db: &Connection,
    account_id: i64,
    sort: PlayerRunSort,
    page: u32,
) -> Result<PlayerRunsPage> {
    if !db.query_row(
        "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1)",
        [account_id],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(ApiError::new(404, "Player not found"));
    }
    let mut query = db.prepare(
        "SELECT b.id,b.map,b.revision,b.bonus,b.time_us,
             strftime('%Y-%m-%dT%H:%M:%SZ',d.created_at,'unixepoch'),d.ticks,d.bonus_pack_revision,
             1 + (SELECT count(*) FROM best_runs faster
                  WHERE faster.map=b.map AND faster.revision=b.revision AND faster.bonus=b.bonus
                    AND faster.time_us<b.time_us)
         FROM best_runs b LEFT JOIN best_run_details d ON d.id=b.id WHERE b.account=?1",
    )?;
    let mut rows = query.query([account_id])?;
    let mut runs = Vec::new();
    let current_version = format!("v{}:", replay::REPLAY_VERSION);
    while let Some(row) = rows.next()? {
        let map: String = row.get(1)?;
        let revision: String = row.get(2)?;
        let bonus: u16 = row.get(3)?;
        let bonus = (bonus != 0).then_some(bonus);
        let pack: Option<String> = row.get(7)?;
        let time_us = row.get(4)?;
        let ticks: Option<u32> = row.get(6)?;
        let compatible = replay_course_available(&map, bonus, &revision, pack.as_deref());
        let active_profile = if map == "Downline"
            || catalog::find(&map).is_some_and(|entry| entry.source_bsp.is_some())
        {
            PhysicsProfile::Legacy
        } else if bonus.is_some() {
            PhysicsProfile::SurfdBonus
        } else {
            PhysicsProfile::Surfd
        };
        let archived = !compatible
            || !revision.starts_with(&current_version)
            || PhysicsProfile::split_revision(&revision).1 != active_profile;
        let place = (!archived && bonus.is_none())
            .then(|| row.get(8))
            .transpose()?;
        let points = place
            .and_then(|place| catalog::find(&map).map(|entry| ranked_map_points(entry, place)));
        runs.push(PlayerRun {
            id: row.get(0)?,
            map,
            bonus,
            time_us,
            created_at: row.get(5)?,
            ticks,
            place,
            points,
            archived,
            replay_available: compatible
                && time_us <= 600_000_000
                && ticks.is_none_or(|ticks| ticks <= 40_000),
        });
    }
    fn title(run: &PlayerRun) -> &str {
        catalog::find(&run.map).map_or(run.map.as_str(), |entry| entry.name.as_str())
    }
    runs.sort_unstable_by(|left, right| {
        let dates = match sort {
            PlayerRunSort::BestPlace => std::cmp::Ordering::Equal,
            PlayerRunSort::Newest => right.created_at.cmp(&left.created_at),
        };
        dates
            .then_with(|| left.archived.cmp(&right.archived))
            .then_with(|| left.bonus.is_some().cmp(&right.bonus.is_some()))
            .then_with(|| {
                left.place
                    .unwrap_or(u64::MAX)
                    .cmp(&right.place.unwrap_or(u64::MAX))
            })
            .then_with(|| title(left).cmp(title(right)))
            .then_with(|| left.bonus.cmp(&right.bonus))
            .then_with(|| left.id.cmp(&right.id))
    });
    let total = runs.len();
    let offset = (page as usize).saturating_mul(PLAYER_RUNS_PAGE_SIZE);
    let runs = runs
        .into_iter()
        .skip(offset)
        .take(PLAYER_RUNS_PAGE_SIZE)
        .collect();
    Ok(PlayerRunsPage {
        account_id,
        page,
        total,
        runs,
    })
}

fn stage_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StageBoardRow> {
    Ok(StageBoardRow {
        stage: row.get(0)?,
        row: BoardRow {
            id: row.get(1)?,
            name: row.get(2)?,
            account_id: Some(row.get(6)?),
            time_us: row.get(3)?,
            splits_us: Vec::new(),
            checkpoint_speeds: Vec::new(),
            stage_span: Some(StageSpan {
                start_tick: row.get(4)?,
                end_tick: row.get(5)?,
            }),
        },
    })
}

struct StageSubmission<'a> {
    owner: i64,
    map: &'a str,
    revision: &'a str,
    bonus: u16,
    id: &'a str,
    payload: &'a str,
    stages: &'a [replay::VerifiedStage],
}

fn record_stages(db: &Connection, run: StageSubmission<'_>) -> Result<(bool, Vec<StageRecord>)> {
    let mut records = Vec::new();
    let mut retained = false;
    for stage in run.stages {
        let old: Option<(String, u64)> = db
            .query_row(
                "SELECT replay_id,time_us FROM stage_bests
                 WHERE account=?1 AND map=?2 AND revision=?3 AND bonus=?4 AND stage=?5",
                params![run.owner, run.map, run.revision, run.bonus, stage.number],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if old.as_ref().is_some_and(|(_, time)| stage.time_us >= *time) {
            continue;
        }
        let previous_record_us: Option<u64> = db.query_row(
            "SELECT min(time_us) FROM stage_bests
             WHERE map=?1 AND revision=?2 AND bonus=?3 AND stage=?4",
            params![run.map, run.revision, run.bonus, stage.number],
            |row| row.get(0),
        )?;
        if !retained {
            db.execute(
                "INSERT INTO stage_replays(id,account,replay) VALUES (?1,?2,?3)",
                params![run.id, run.owner, run.payload],
            )?;
            retained = true;
        }
        db.execute(
            "INSERT INTO stage_bests(account,map,revision,bonus,stage,replay_id,time_us,start_tick,end_tick)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(account,map,revision,bonus,stage) DO UPDATE SET
                 replay_id=excluded.replay_id,time_us=excluded.time_us,
                 start_tick=excluded.start_tick,end_tick=excluded.end_tick",
            params![
                run.owner, run.map, run.revision, run.bonus, stage.number, run.id,
                stage.time_us, stage.start_tick, stage.end_tick
            ],
        )?;
        if let Some((previous_replay, _)) = old {
            db.execute(
                "DELETE FROM stage_replays WHERE id=?1
                 AND NOT EXISTS (SELECT 1 FROM stage_bests WHERE replay_id=?1)",
                [previous_replay],
            )?;
        }
        if previous_record_us.is_none_or(|time| stage.time_us < time) {
            records.push(StageRecord {
                stage: stage.number,
                time_us: stage.time_us,
                previous_record_us,
            });
        }
    }
    Ok((retained, records))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn random_hex<const N: usize>() -> Result<String> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(internal)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn token_hash(token: &str) -> Result<Vec<u8>> {
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(unauthorized());
    }
    Ok(Sha256::digest(token.as_bytes()).to_vec())
}

fn unauthorized() -> ApiError {
    ApiError::new(401, "Sign in with a valid account")
}

fn account(db: &Connection, bearer: Option<&str>) -> Result<i64> {
    let token = token_hash(bearer.ok_or_else(unauthorized)?)?;
    db.query_row(
        "SELECT account FROM sessions WHERE token=?1 AND expires>?2",
        params![token, unix_now()],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(unauthorized)
}

fn account_loadout(db: &Connection, id: i64) -> Result<Option<Loadout>> {
    let stored: Option<String> = db
        .query_row(
            "SELECT loadout FROM account_loadouts WHERE account=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    stored
        .map(|json| serde_json::from_str(&json).map_err(internal))
        .transpose()
}

fn profile(db: &Connection, id: i64) -> Result<Profile> {
    let (name, motto, title, banner, showcase, revision) = db.query_row(
        "SELECT name,motto,title,coalesce(banner,''),coalesce(showcase,'[]'),coalesce(revision,0)
         FROM accounts LEFT JOIN profile_extras ON account=id WHERE id=?1",
        [id],
        |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, String>(4)?, row.get(5)?))
        },
    )
    .optional()?
    .ok_or_else(|| ApiError::new(404, "Player not found"))?;
    let progress = account_ranked_progress(db, id)?;
    Ok(Profile {
        id,
        name,
        motto,
        title,
        banner,
        revision,
        showcase: serde_json::from_str(&showcase).map_err(internal)?,
        progression: progress.stats(MapMode::Surf),
        bhop_progression: progress.stats(MapMode::Bhop),
    })
}

#[derive(Default)]
struct RankedProgress<'a> {
    surf: HashMap<&'a str, u32>,
    bhop: HashMap<&'a str, u32>,
}

impl<'a> RankedProgress<'a> {
    fn add(&mut self, entry: &'a catalog::CatalogEntry, revision: &str, place: u64) {
        let Some((version, board)) = revision.split_once(':') else {
            return;
        };
        if version
            .strip_prefix('v')
            .and_then(|version| version.parse().ok())
            != Some(replay::REPLAY_VERSION)
        {
            return;
        }
        if entry.source_bsp.is_some() {
            if entry.native_board_revision().as_deref() != Some(board) {
                return;
            }
        } else {
            let (geometry, profile) = PhysicsProfile::split_revision(board);
            let previous = if entry.had_authored_contacts() {
                PhysicsProfile::AuthoredContacts
            } else {
                PhysicsProfile::Legacy
            };
            if catalog::revision(&entry.geometry) != Some(geometry)
                || (profile != PhysicsProfile::Surfd && profile != previous)
            {
                return;
            }
        }
        let points = ranked_map_points(entry, place);
        let maps = match entry.mode {
            MapMode::Surf => &mut self.surf,
            MapMode::Bhop => &mut self.bhop,
        };
        let best = maps.entry(&entry.id).or_default();
        *best = (*best).max(points);
    }

    fn stats(&self, mode: MapMode) -> ProgressionStats {
        let maps = match mode {
            MapMode::Surf => &self.surf,
            MapMode::Bhop => &self.bhop,
        };
        let points = maps.values().sum();
        ProgressionStats {
            points,
            completions: maps.len() as u16,
            rank: ProgressionRank::from_points(points),
        }
    }
}

fn ranked_map_points(entry: &catalog::CatalogEntry, place: u64) -> u32 {
    let performance = (400.0 / (place as f64).sqrt()).floor() as u32;
    let multiplier = if entry.tier == Some(2) { 2 } else { 1 };
    multiplier * (100 + performance)
}

fn account_progression(db: &Connection, id: i64, mode: MapMode) -> Result<ProgressionStats> {
    Ok(account_ranked_progress(db, id)?.stats(mode))
}

fn account_ranked_progress(db: &Connection, id: i64) -> Result<RankedProgress<'static>> {
    let mut query = db.prepare(
        "SELECT b.map,b.revision,1 + (
             SELECT count(*) FROM best_runs faster
             WHERE faster.map=b.map AND faster.revision=b.revision AND faster.bonus=0
                 AND faster.time_us<b.time_us
         ) AS place FROM best_runs b WHERE b.account=?1 AND b.bonus=0",
    )?;
    let mut rows = query.query([id])?;
    let mut progress = RankedProgress::default();
    while let Some(row) = rows.next()? {
        if let Some(entry) = catalog::find(row.get_ref(0)?.as_str().map_err(internal)?) {
            progress.add(
                entry,
                row.get_ref(1)?.as_str().map_err(internal)?,
                row.get(2)?,
            );
        }
    }
    Ok(progress)
}

fn progression(db: &Connection, owner: Option<i64>, mode: MapMode) -> Result<Progression> {
    let mut query = db.prepare(
        "SELECT b.account,a.name,b.map,b.revision,
             rank() OVER (PARTITION BY b.map,b.revision ORDER BY b.time_us)
         FROM best_runs b JOIN accounts a ON a.id=b.account WHERE b.bonus=0",
    )?;
    let mut rows = query.query([])?;
    let mut rankings = HashMap::new();
    while let Some(row) = rows.next()? {
        let Some(entry) = catalog::find(row.get_ref(2)?.as_str().map_err(internal)?)
            .filter(|entry| entry.mode == mode)
        else {
            continue;
        };
        let account_id = row.get(0)?;
        let result = match rankings.entry(account_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert((row.get::<_, String>(1)?, RankedProgress::default()))
            }
        };
        result.1.add(
            entry,
            row.get_ref(3)?.as_str().map_err(internal)?,
            row.get(4)?,
        );
    }
    let mut entries: Vec<_> = rankings
        .into_iter()
        .filter_map(|(account_id, (name, progress))| {
            let stats = progress.stats(mode);
            (stats.completions > 0).then_some(ProgressionRow {
                account_id,
                name,
                stats,
            })
        })
        .collect();
    let own = if let Some(owner) = owner {
        if let Some(row) = entries.iter().find(|row| row.account_id == owner) {
            Some(row.clone())
        } else {
            let name = db.query_row("SELECT name FROM accounts WHERE id=?1", [owner], |row| {
                row.get(0)
            })?;
            Some(ProgressionRow {
                account_id: owner,
                name,
                stats: ProgressionStats::default(),
            })
        }
    } else {
        None
    };
    entries.sort_unstable_by(|a, b| {
        b.stats
            .points
            .cmp(&a.stats.points)
            .then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
            .then_with(|| a.account_id.cmp(&b.account_id))
    });
    let total = entries.len();
    entries.truncate(100);
    Ok(Progression {
        mode,
        entries,
        total,
        own,
    })
}

fn parse_account_id(value: &str) -> Result<i64> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::new(404, "Player not found"))
}

fn activity_map(map: String) -> Result<String> {
    if let Ok(canonical) = canonical_map(&map) {
        return Ok(canonical.to_owned());
    }
    let map = map.trim();
    if map.is_empty() || map.chars().count() > 96 || map.chars().any(char::is_control) {
        return Err(ApiError::new(400, "Invalid map name"));
    }
    Ok(map.to_owned())
}

fn create_session(db: &Connection, id: i64) -> Result<SignedIn> {
    let profile = profile(db, id)?;
    let token = random_hex::<32>()?;
    db.execute("DELETE FROM sessions WHERE expires<=?1", [unix_now()])?;
    db.execute(
        "INSERT INTO sessions(token,account,expires) VALUES (?1,?2,?3)",
        params![token_hash(&token)?, id, unix_now() + SESSION_SECONDS],
    )?;
    Ok(SignedIn { token, profile })
}

fn allocate_account_id(db: &Connection) -> Result<i64> {
    Ok(db.query_row(
        "UPDATE account_sequence SET last_id=last_id+1 WHERE singleton=1 RETURNING last_id",
        [],
        |row| row.get(0),
    )?)
}

fn canonical_map(name: &str) -> Result<&'static str> {
    if name.eq_ignore_ascii_case("downline") {
        return Ok("Downline");
    }
    catalog::find(name)
        .map(|entry| entry.id.as_str())
        .ok_or_else(|| ApiError::new(404, "Unknown ranked map"))
}

fn versioned_revision(revision: &str) -> String {
    format!("v{}:{revision}", replay::REPLAY_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        movement::MoveInput,
        session::{RunPhase, Session},
    };

    fn service() -> Service {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        crate::social_service::init(&db).unwrap();
        Service {
            db: Mutex::new(db),
            maps: PathBuf::new(),
            courses: Mutex::default(),
            limits: Mutex::default(),
            trust_real_ip: false,
            room_directory: RoomDirectory::default(),
            oauth: OAuth::default(),
        }
    }

    #[test]
    fn loadout_sync_requires_an_account_and_preserves_existing_database_data() {
        let service = service();
        let [first, second, other] = {
            let db = service.db.lock().unwrap();
            db.execute_batch(
                "DROP TABLE IF EXISTS account_loadouts;
                 INSERT INTO accounts(id,name,password) VALUES (1,'Surfer',''), (2,'Other','');",
            )
            .unwrap();
            let tokens = [1, 1, 2].map(|id| create_session(&db, id).unwrap().token);
            db.execute_batch(SCHEMA).unwrap();
            tokens
        };
        let http = Server::http("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1/loadout", http.server_addr());
        let (ranked, _queue) = mpsc::sync_channel(1);
        let timeout = Duration::from_secs(15);
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .new_agent();
        let get = |token: Option<&str>| {
            let mut request = agent.get(endpoint.as_str()).header("Connection", "close");
            if let Some(token) = token {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            request.call().unwrap()
        };
        let post = |token: Option<&str>, body: &str| {
            let mut request = agent
                .post(endpoint.as_str())
                .header("Connection", "close")
                .header("Content-Type", "application/json");
            if let Some(token) = token {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            request.send(body).unwrap()
        };
        let selected = serde_json::json!({"avatar_skin": "ember", "knife_finish": "glacier"});
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..10 {
                    let Some(request) = http.recv_timeout(timeout).unwrap() else {
                        break;
                    };
                    service.accept_http(request, &ranked);
                }
            });
            assert_eq!(get(None).status(), 401);
            assert_eq!(post(None, &selected.to_string()).status(), 401);
            assert_eq!(
                get(Some(&first)).body_mut().read_to_string().unwrap(),
                "null"
            );
            let mut saved = post(Some(&first), &selected.to_string());
            assert_eq!(saved.status(), 200);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(
                    &saved.body_mut().read_to_string().unwrap()
                )
                .unwrap(),
                selected
            );
            assert_eq!(post(Some(&first), r#"{"avatar_skin":42}"#).status(), 400);
            service.db.lock().unwrap().execute_batch(SCHEMA).unwrap();
            let mut other_device = get(Some(&second));
            assert_eq!(other_device.status(), 200);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(
                    &other_device.body_mut().read_to_string().unwrap()
                )
                .unwrap(),
                selected
            );
            let changed = serde_json::json!({"avatar_skin": "classic", "knife_finish": "ember"});
            assert_eq!(post(Some(&second), &changed.to_string()).status(), 200);
            let mut original_device = get(Some(&first));
            assert_eq!(
                serde_json::from_reader::<_, serde_json::Value>(
                    original_device.body_mut().as_reader()
                )
                .unwrap(),
                changed
            );
            assert_eq!(
                get(Some(&other)).body_mut().read_to_string().unwrap(),
                "null"
            );
            assert_eq!(get(Some("revoked-or-invalid")).status(), 401);
        });
    }

    #[test]
    fn private_message_reports_require_recipient_and_moderator() {
        let mut service = service();
        service.room_directory.moderators.insert(3);
        let [sender, recipient, moderator] = {
            let db = service.db.lock().unwrap();
            db.execute_batch(
                "INSERT INTO accounts(id,name,password) VALUES
                    (1,'Sender',''), (2,'Recipient',''), (3,'Moderator','');
                 INSERT INTO social_friends(a_id,b_id) VALUES (1,2);
                 INSERT INTO social_messages(id,sender_id,recipient_id,body,created_at)
                    VALUES (5,1,2,'Original message',unixepoch());",
            )
            .unwrap();
            [1, 2, 3].map(|id| create_session(&db, id).unwrap().token)
        };
        let http = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", http.server_addr());
        let (ranked, _queue) = mpsc::sync_channel(1);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(3)))
            .http_status_as_error(false)
            .build()
            .into();
        let report = |token: Option<&str>, reason: &str| {
            let mut request = agent.post(format!("{url}/v1/social/message-reports"));
            if let Some(token) = token {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            request
                .send(serde_json::json!({"message": 5, "reason": reason}).to_string())
                .unwrap()
        };
        let review = |token: Option<&str>| {
            let mut request = agent.get(format!("{url}/v1/social/message-reports/latest"));
            if let Some(token) = token {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            request.call().unwrap()
        };
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..11 {
                    let Some(request) = http.recv_timeout(Duration::from_secs(3)).unwrap() else {
                        break;
                    };
                    service.accept_http(request, &ranked);
                }
            });
            assert_eq!(report(None, "harassment").status(), 401);
            assert_eq!(report(Some(&sender), "harassment").status(), 400);
            assert_eq!(report(Some(&moderator), "harassment").status(), 400);
            let unfriended = agent
                .post(format!("{url}/v1/social"))
                .header("Authorization", format!("Bearer {sender}"))
                .send(r#"{"type":"remove","account":2}"#)
                .unwrap();
            assert_eq!(unfriended.status(), 200);
            let mut inbox = agent
                .post(format!("{url}/v1/social"))
                .header("Authorization", format!("Bearer {recipient}"))
                .send(r#"{"type":"fetch"}"#)
                .unwrap();
            assert_eq!(inbox.status(), 200);
            let inbox: crate::social_protocol::SocialState =
                serde_json::from_str(&inbox.body_mut().read_to_string().unwrap()).unwrap();
            assert!(inbox.friends.is_empty() && inbox.messages.is_empty());
            assert_eq!(inbox.archived_messages[0].id, 5);
            let mut accepted = report(Some(&recipient), "harassment");
            assert_eq!(accepted.status(), 200);
            let receipt: serde_json::Value =
                serde_json::from_str(&accepted.body_mut().read_to_string().unwrap()).unwrap();
            assert_eq!(receipt["duplicate"], false);
            let blocked = agent
                .post(format!("{url}/v1/social"))
                .header("Authorization", format!("Bearer {recipient}"))
                .send(r#"{"type":"block","account":1}"#)
                .unwrap();
            assert_eq!(blocked.status(), 200);
            let mut duplicate = report(Some(&recipient), "spam");
            assert_eq!(duplicate.status(), 200);
            let receipt: serde_json::Value =
                serde_json::from_str(&duplicate.body_mut().read_to_string().unwrap()).unwrap();
            assert_eq!(receipt["duplicate"], true);
            assert_eq!(review(None).status(), 401);
            assert_eq!(review(Some(&recipient)).status(), 403);
            let mut page = review(Some(&moderator));
            assert_eq!(page.status(), 200);
            let page: serde_json::Value =
                serde_json::from_str(&page.body_mut().read_to_string().unwrap()).unwrap();
            let reports = page["reports"].as_array().unwrap();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0]["message"], 5);
            assert_eq!(reports[0]["text"], "Original message");
            assert_eq!(reports[0]["reason"], "harassment");
            assert_eq!(reports[0]["reporter"]["id"], 2);
            assert_eq!(reports[0]["sender"]["id"], 1);
        });
    }

    #[test]
    fn verification_backpressure_keeps_readiness_available() {
        let service = service();
        let auth = {
            let db = service.db.lock().unwrap();
            db.execute(
                "INSERT INTO accounts(id,name,password) VALUES (1,'Tester','')",
                [],
            )
            .unwrap();
            create_session(&db, 1).unwrap().token
        };
        let http = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", http.server_addr());
        let (ranked, queue) = mpsc::sync_channel(1);
        let (admitted, admission) = mpsc::sync_channel(1);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .into();
        let submit = || {
            agent
                .post(format!("{url}/v1/runs"))
                .header("Authorization", format!("Bearer {auth}"))
                .send("{}")
                .unwrap()
        };
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for index in 0..3 {
                    let request = http.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
                    service.accept_http(request, &ranked);
                    if index == 0 {
                        admitted.send(()).unwrap();
                    }
                }
            });
            let first = scope.spawn(&submit);
            admission.recv_timeout(Duration::from_secs(5)).unwrap();
            let busy = submit();
            assert_eq!(busy.status(), 503);
            assert_eq!(
                busy.headers().get("Retry-After").unwrap(),
                VERIFICATION_RETRY_SECONDS.to_string().as_str()
            );
            let ready = agent.get(format!("{url}/v1/ready")).call().unwrap();
            assert_eq!(ready.status(), 200);
            service.respond(queue.recv_timeout(Duration::from_secs(5)).unwrap());
            assert_eq!(first.join().unwrap().status(), 400);
        });
    }

    #[test]
    fn real_ip_requires_opt_in_loopback_and_a_single_literal() {
        let loopback = "127.0.0.1".parse::<IpAddr>().unwrap();
        let public = "203.0.113.4".parse::<IpAddr>().unwrap();
        let forwarded = Header::from_bytes("X-Real-IP", public.to_string()).unwrap();
        assert_eq!(rate_source(loopback, &[forwarded.clone()], false), loopback);
        assert_eq!(rate_source(loopback, &[forwarded.clone()], true), public);
        let other = "192.0.2.7".parse::<IpAddr>().unwrap();
        assert_eq!(rate_source(other, &[forwarded.clone()], true), other);
        assert_eq!(
            rate_source(loopback, &[forwarded.clone(), forwarded], true),
            loopback
        );
        let chain = Header::from_bytes("X-Real-IP", "192.0.2.7, 203.0.113.4").unwrap();
        assert_eq!(rate_source(loopback, &[chain], true), loopback);
    }

    #[test]
    fn provider_identities_stay_with_the_same_account_without_using_display_names() {
        let service = service();
        let identity = |provider, name: &str| ProviderIdentity {
            provider,
            subject: "stable-subject".into(),
            name: name.into(),
        };
        let first = service
            .oauth_sign_in(&identity(OAuthProvider::Google, "Test Surfer"))
            .unwrap();
        let repeat = service
            .oauth_sign_in(&identity(OAuthProvider::Google, "Changed Name"))
            .unwrap();
        let other = service
            .oauth_sign_in(&identity(OAuthProvider::X, "Test Surfer"))
            .unwrap();
        assert_eq!(first.profile.id, repeat.profile.id);
        assert_ne!(first.profile.id, other.profile.id);
        assert_ne!(first.profile.name, other.profile.name);
        assert_eq!(
            service
                .login(Credentials {
                    name: first.profile.name,
                    password: OAUTH_PASSWORD.into(),
                })
                .err()
                .unwrap()
                .status,
            401
        );
    }

    #[test]
    fn managed_tickets_are_issued_only_to_registered_addresses_and_sign_verified_accounts() {
        let mut service = service();
        let secret = "0123456789abcdef".repeat(4);
        service.room_directory =
            RoomDirectory::from_config(&format!("eu|localhost:27165|{secret}"), "17").unwrap();
        let request = || RoomTicketRequest {
            room_id: "eu".into(),
            address: "LOCALHOST".into(),
            mode: crate::catalog::MapMode::Surf,
        };
        let auth = {
            let db = service.db.lock().unwrap();
            db.execute(
                "INSERT INTO accounts(id,name,password) VALUES (17,'Alice','')",
                [],
            )
            .unwrap();
            create_session(&db, 17).unwrap().token
        };
        let claims = |auth: Option<&str>| {
            let ticket = service.issue_room_ticket(auth, request()).unwrap();
            let serialized = serde_json::to_value(ticket).unwrap();
            let token = serialized["ticket"].as_str().unwrap();
            service.room_directory.rooms[0]
                .1
                .verify(token, ticket_time())
                .unwrap()
        };
        assert!(claims(None).loadout.is_default());
        assert!(claims(Some(&auth)).loadout.is_default());
        let selected = Loadout {
            avatar_skin: crate::loadout::AvatarSkin::Ember,
            knife_finish: crate::loadout::KnifeFinish::Glacier,
        };
        service.set_loadout(Some(&auth), selected).unwrap();
        let renewed = claims(Some(&auth));
        assert_eq!(renewed.identity.account_id, Some(17));
        assert_eq!(renewed.loadout, selected);
        assert!(claims(None).loadout.is_default());
        assert_eq!(
            service
                .issue_room_ticket(Some("bad"), request())
                .err()
                .unwrap()
                .status,
            401
        );
        let mut unknown = request();
        unknown.address = "localhost:27167".into();
        assert_eq!(
            service
                .issue_room_ticket(None, unknown)
                .err()
                .unwrap()
                .status,
            404
        );
        assert!(
            RoomDirectory::from_config(
                &format!("eu|localhost|{secret};na|localhost:27165|{secret}"),
                "",
            )
            .is_err()
        );
    }

    #[test]
    fn account_id_sequence_survives_deletion_and_schema_restart() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db.execute(
            "INSERT INTO accounts(id,name,password) VALUES (17,'Alice','')",
            [],
        )
        .unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db.execute("DELETE FROM accounts WHERE id=17", []).unwrap();
        db.execute_batch(SCHEMA).unwrap();
        let last_id: i64 = db
            .query_row("SELECT last_id FROM account_sequence", [], |row| row.get(0))
            .unwrap();
        assert_eq!(last_id, 17);
    }

    #[test]
    fn archived_contact_board_remains_playable_and_preserves_earned_progression() {
        let entry = catalog::find("kitsune2").unwrap();
        let old = versioned_revision(catalog::revision(&entry.geometry).unwrap());
        let contacts = versioned_revision(
            &PhysicsProfile::AuthoredContacts
                .revision(catalog::revision(&entry.geometry).unwrap().to_owned()),
        );
        let current = versioned_revision(&entry.native_board_revision().unwrap());
        assert_ne!(old, current);
        assert!(replay_course_available(&entry.id, None, &old, None));
        assert!(replay_course_available(&entry.id, None, &contacts, None));
        assert!(replay_course_available(&entry.id, None, &current, None));

        let mut progress = RankedProgress::default();
        progress.add(entry, &old, 1);
        assert_eq!(progress.stats(MapMode::Surf).completions, 0);
        progress.add(entry, &contacts, 1);
        progress.add(entry, &current, 3);
        assert_eq!(progress.stats(MapMode::Surf).completions, 1);
        assert_eq!(
            progress.stats(MapMode::Surf).points,
            ranked_map_points(entry, 1)
        );
    }

    #[test]
    fn progression_keeps_surf_and_bhop_bests_and_xp_separate() {
        let service = service();
        let db = service.db.lock().unwrap();
        db.execute("INSERT INTO accounts(id,name,password) VALUES (1,'Alice',''),(2,'Bob',''),(3,'Chris','')", []).unwrap();
        let tier_one = catalog::entries()
            .iter()
            .find(|entry| entry.mode == MapMode::Surf && entry.tier == Some(1))
            .unwrap();
        let tier_two = catalog::entries()
            .iter()
            .find(|entry| entry.mode == MapMode::Surf && entry.tier == Some(2))
            .unwrap();
        let bhop = catalog::find("bhop_easy").unwrap();
        assert_eq!(bhop.mode, MapMode::Bhop);
        let revision = |entry: &catalog::CatalogEntry| {
            versioned_revision(&entry.native_board_revision().unwrap())
        };
        for (id, owner, map, revision, bonus, time) in [
            ("a", 1, &tier_one.id, revision(tier_one), 0, 1000),
            ("b", 2, &tier_one.id, revision(tier_one), 0, 1000),
            ("c", 3, &tier_one.id, revision(tier_one), 0, 2000),
            ("d", 1, &tier_two.id, revision(tier_two), 0, 1000),
            ("e", 3, &tier_two.id, revision(tier_two), 1, 100),
            ("f", 3, &tier_two.id, "old-rules".into(), 0, 100),
            (
                "g",
                2,
                &tier_one.id,
                versioned_revision(catalog::revision(&tier_one.geometry).unwrap()),
                0,
                1000,
            ),
            ("h", 3, &bhop.id, revision(bhop), 0, 600),
            ("i", 1, &bhop.id, "v2:obsolete-source-board".into(), 0, 100),
        ] {
            db.execute(
                "INSERT INTO best_runs(id,account,map,revision,bonus,time_us,splits,speeds,replay) VALUES (?1,?2,?3,?4,?5,?6,'[]','[]','{}')",
                params![id, owner, map, revision, bonus, time],
            ).unwrap();
        }
        let board = progression(&db, Some(3), MapMode::Surf).unwrap();
        assert_eq!(board.total, 3);
        assert_eq!(
            board
                .entries
                .iter()
                .map(|row| row.stats.points)
                .collect::<Vec<_>>(),
            [1500, 500, 330]
        );
        assert_eq!(board.own.unwrap().stats.rank, ProgressionRank::Rookie);
        assert_eq!(
            account_progression(&db, 1, MapMode::Surf)
                .unwrap()
                .completions,
            2
        );
        assert_eq!(
            account_progression(&db, 2, MapMode::Surf).unwrap().points,
            500
        );
        let board = progression(&db, Some(1), MapMode::Bhop).unwrap();
        assert_eq!(board.mode, MapMode::Bhop);
        assert_eq!(board.total, 1);
        assert_eq!(board.entries[0].account_id, 3);
        assert_eq!(board.own.unwrap().stats.points, 0);
        let profile = serde_json::to_value(profile(&db, 3).unwrap()).unwrap();
        let bhop_points = ranked_map_points(bhop, 1);
        assert_eq!(profile["points"], 330);
        assert_eq!(profile["bhop_progression"]["points"], bhop_points);

        let page = player_runs(&db, 3, PlayerRunSort::BestPlace, 0).unwrap();
        let run = page.runs.iter().find(|run| run.map == bhop.id).unwrap();
        assert!(run.replay_available && !run.archived);
        assert_eq!(run.points, Some(bhop_points));
    }

    #[test]
    fn source_bonus_history_requires_the_pinned_revision() {
        let entry = catalog::find("bhop_easy").unwrap();
        let source = entry.source_bsp.as_ref().unwrap();
        let revision = versioned_revision(&source.bonus_board_revision(1).unwrap());
        let pack = bonus_pack_revision(&entry.id).unwrap();
        assert!(replay_course_available(
            &entry.id,
            Some(1),
            &revision,
            Some(pack)
        ));
        assert!(!replay_course_available(
            &entry.id,
            Some(1),
            &revision,
            Some("old")
        ));
        assert!(!replay_course_available(
            &entry.id,
            Some(2),
            &revision,
            Some(pack)
        ));
    }

    fn finished_downline() -> replay::RunReplay {
        finished_downline_with_aim(false)
    }

    fn finished_downline_with_aim(vary_aim: bool) -> replay::RunReplay {
        let course = Course::practice();
        let mut session = Session::new(&course);
        for tick in 0..600 {
            let position = session.player.position;
            let velocity = session.player.velocity;
            let mut input = MoveInput {
                yaw: course.spawn_yaw
                    + if vary_aim {
                        ((tick * tick) % 43) as f32 * 0.0002
                    } else {
                        0.0
                    },
                ..Default::default()
            };
            if position.z > -400.0 || position.z < -2100.0 {
                input.forward = 1.0;
            } else if position.x > 120.0 || velocity.x > 28.0 {
                input.strafe = -1.0;
            }
            session.tick(&course, input);
            if session.phase == RunPhase::Finished {
                break;
            }
        }
        session
            .take_finished_replay(&course)
            .expect("a completed Downline replay")
    }

    #[test]
    fn published_replay_ownership_survives_backfill_and_rejects_another_account() {
        let service = service();
        let [original, other] = {
            let db = service.db.lock().unwrap();
            db.execute_batch(
                "INSERT INTO accounts(id,name,password) VALUES (1,'Original',''),(2,'Other','');",
            )
            .unwrap();
            [1, 2].map(|owner| create_session(&db, owner).unwrap().token)
        };
        let replay = finished_downline_with_aim(true);
        assert!(replay.has_distinct_aim_pattern());
        let save = |auth: &str| {
            let attempt = service
                .issue(
                    Some(auth),
                    AttemptRequest {
                        map: replay.map.clone(),
                        bonus: replay.bonus,
                    },
                )
                .unwrap();
            service.submit(
                Some(auth),
                SubmitRun {
                    attempt: attempt.token,
                    replay: replay.clone(),
                },
            )
        };
        assert!(save(&original).unwrap().personal_best);
        assert_eq!(save(&other).err().unwrap().status, 409);
        {
            let mut db = service.db.lock().unwrap();
            db.execute("DELETE FROM ranked_replay_owners", []).unwrap();
            index_ranked_replay_owners(&mut db).unwrap();
            index_ranked_replay_owners(&mut db).unwrap();
        }
        assert_eq!(save(&other).err().unwrap().status, 409);
        assert!(!save(&original).unwrap().personal_best);
        assert_eq!(service.board("downline", None).unwrap().rows.len(), 1);
    }

    #[test]
    fn a_guest_attempt_can_be_claimed_once_and_only_with_a_verified_replay() {
        let service = service();
        let attempt = service
            .issue(
                None,
                AttemptRequest {
                    map: "Downline".into(),
                    bonus: None,
                },
            )
            .unwrap();
        let sign_in = service
            .register(Credentials {
                name: "test_surfer".into(),
                password: "test-only-password".into(),
            })
            .unwrap();
        assert_eq!(sign_in.profile.name, "test_surfer");
        assert!(
            service
                .login(Credentials {
                    name: "test_surfer".into(),
                    password: "incorrect-password".into(),
                })
                .is_err()
        );

        let replay = finished_downline();
        let ticks = replay.commands.len() as u32;
        let mut mismatched = replay.clone();
        mismatched.revision.push_str("-different");
        let auth = Some(sign_in.token.as_str());
        let rejected = service
            .submit(
                auth,
                SubmitRun {
                    attempt: attempt.token.clone(),
                    replay: mismatched,
                },
            )
            .err()
            .unwrap();
        assert_eq!(rejected.status, 422);
        let accepted = service
            .submit(
                auth,
                SubmitRun {
                    attempt: attempt.token.clone(),
                    replay: replay.clone(),
                },
            )
            .unwrap();
        assert!(accepted.personal_best);
        let repeated = service
            .submit(
                auth,
                SubmitRun {
                    attempt: attempt.token,
                    replay,
                },
            )
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&repeated).unwrap(),
            serde_json::to_vec(&accepted).unwrap()
        );
        let board = service.board("downline", None).unwrap();
        assert_eq!(board.rows.len(), 1);
        assert_eq!(board.rows[0].name, "test_surfer");
        assert_eq!(board.rows[0].time_us, accepted.time_us);
        assert_eq!(Some(&board.rows[0].id), accepted.run_id.as_ref());
        assert_eq!(board.rows[0].splits_us.len(), 1);
        assert!(accepted.stage_records.is_empty());
        let history = player_runs(
            &service.db.lock().unwrap(),
            sign_in.profile.id,
            PlayerRunSort::BestPlace,
            0,
        )
        .unwrap();
        assert_eq!(history.total, 1);
        assert_eq!(Some(&history.runs[0].id), accepted.run_id.as_ref());
        assert_eq!(history.runs[0].ticks, Some(ticks));
        assert!(history.runs[0].created_at.is_some());
        assert!(history.runs[0].replay_available);
        let profile = service
            .update_profile_extras(
                auth,
                ProfileExtrasUpdate {
                    motto: "one more run".into(),
                    banner: "downline".into(),
                    title: "Downline #1".into(),
                    showcase: vec!["Downline".into()],
                    revision: 0,
                },
            )
            .unwrap();
        assert_eq!(profile.banner, "Downline");
        assert_eq!(profile.title, "Downline #1");
        assert_eq!(profile.revision, 1);
        let stale = service
            .update_profile_extras(
                auth,
                ProfileExtrasUpdate {
                    motto: String::new(),
                    banner: String::new(),
                    title: String::new(),
                    showcase: Vec::new(),
                    revision: 0,
                },
            )
            .err()
            .unwrap();
        assert_eq!(stale.status, 409);
    }

    #[test]
    fn ranked_receipt_survives_a_lost_http_reply_and_requires_the_original_run_and_owner() {
        use std::io::Write;

        let service = service();
        let credentials = Credentials {
            name: "ReceiptTester".into(),
            password: "receipt-test-passphrase".into(),
        };
        let original = service
            .register(Credentials {
                name: credentials.name.clone(),
                password: credentials.password.clone(),
            })
            .unwrap();
        let other = {
            let db = service.db.lock().unwrap();
            db.execute(
                "INSERT INTO accounts(name,password) VALUES ('OtherTester','')",
                [],
            )
            .unwrap();
            create_session(&db, db.last_insert_rowid()).unwrap()
        };
        let attempt = service
            .issue(
                None,
                AttemptRequest {
                    map: "Downline".into(),
                    bonus: None,
                },
            )
            .unwrap();
        let replay = finished_downline();
        let lookup = SubmissionLookup {
            attempt: attempt.token.clone(),
            replay_sha256: replay_sha256(&serde_json::to_vec(&replay).unwrap()),
        };
        let payload = SubmitRun {
            attempt: attempt.token,
            replay,
        };
        let http = Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", http.server_addr());
        let saved_response = Mutex::new(None);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for number in 0..11 {
                    let Some(mut request) = http.recv_timeout(Duration::from_secs(5)).unwrap() else { break; };
                    if number == 1 {
                        assert_eq!(request.url(), "/v1/runs");
                        *saved_response.lock().unwrap() = Some(service.dispatch(&mut request).unwrap());
                        let mut writer = request.into_writer();
                        writer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\n").unwrap();
                        writer.flush().unwrap();
                    } else {
                        service.respond(request);
                    }
                }
            });
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(3)))
                .http_status_as_error(false)
                .build()
                .new_agent();
            let post = |route: &str, token: Option<&str>, json: &[u8]| {
                let mut request = agent
                    .post(format!("{base}{route}"))
                    .header("Content-Type", "application/json")
                    .header("Connection", "close");
                if let Some(token) = token {
                    request = request.header("Authorization", format!("Bearer {token}"));
                }
                request.send(json).unwrap()
            };
            let lookup_body = serde_json::to_vec(&lookup).unwrap();
            let payload_body = serde_json::to_vec(&payload).unwrap();
            let query = |token: &str| post("/v1/runs/result", Some(token), &lookup_body);
            let mut pending = query(&original.token);
            assert!(matches!(
                serde_json::from_reader::<_, SubmissionStatus>(pending.body_mut().as_reader())
                    .unwrap(),
                SubmissionStatus::Pending
            ));
            let mut lost = post("/v1/runs", Some(&original.token), &payload_body);
            assert_eq!(lost.status(), 200);
            assert!(lost.body_mut().read_to_string().is_err());
            assert_eq!(
                post("/v1/logout", Some(&original.token), b"null").status(),
                200
            );
            assert_eq!(query(&original.token).status(), 401);
            let mut login = post(
                "/v1/login",
                None,
                &serde_json::to_vec(&credentials).unwrap(),
            );
            let fresh: SignedIn = serde_json::from_reader(login.body_mut().as_reader()).unwrap();
            assert_eq!(fresh.profile.id, original.profile.id);
            assert_ne!(fresh.token, original.token);
            assert_eq!(query(&other.token).status(), 409);
            let mismatched = SubmissionLookup {
                attempt: lookup.attempt.clone(),
                replay_sha256: "0".repeat(64),
            };
            assert_eq!(
                post(
                    "/v1/runs/result",
                    Some(&fresh.token),
                    &serde_json::to_vec(&mismatched).unwrap()
                )
                .status(),
                409
            );
            let mut saved = query(&fresh.token);
            let SubmissionStatus::Saved(receipt) =
                serde_json::from_reader(saved.body_mut().as_reader()).unwrap()
            else {
                panic!("save receipt was missing");
            };
            let accepted = saved_response.lock().unwrap().clone().unwrap();
            assert_eq!(serde_json::to_vec(&receipt).unwrap(), accepted);
            let mut changed = SubmitRun {
                attempt: payload.attempt.clone(),
                replay: payload.replay.clone(),
            };
            changed.replay.commands[0].pitch += 0.1;
            assert_eq!(
                post(
                    "/v1/runs",
                    Some(&fresh.token),
                    &serde_json::to_vec(&changed).unwrap()
                )
                .status(),
                409
            );
            assert_eq!(
                post("/v1/runs", Some(&other.token), &payload_body).status(),
                409
            );
            let mut duplicate = post("/v1/runs", Some(&fresh.token), &payload_body);
            assert_eq!(duplicate.status(), 200);
            assert_eq!(duplicate.body_mut().read_to_vec().unwrap(), accepted);
            let db = service.db.lock().unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM best_runs", [], |row| row
                    .get::<_, usize>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM ranked_submission_receipts",
                    [],
                    |row| row.get::<_, usize>(0)
                )
                .unwrap(),
                1
            );
            assert_eq!(
                db.query_row("SELECT used FROM attempts", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                1
            );
            db.execute("UPDATE ranked_submission_receipts SET expires=0", [])
                .unwrap();
            assert!(
                submission_receipt(
                    &db,
                    &token_hash(&lookup.attempt).unwrap(),
                    fresh.profile.id,
                    &lookup.replay_sha256,
                    unix_now()
                )
                .unwrap()
                .is_none()
            );
            prune_submission_receipts(&db, unix_now()).unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM ranked_submission_receipts",
                    [],
                    |row| row.get::<_, usize>(0)
                )
                .unwrap(),
                0
            );
        });
    }

    #[test]
    fn reserved_save_resumes_only_the_original_unexpired_attempt_after_sign_in() {
        let service = service();
        let (original, other) = {
            let db = service.db.lock().unwrap();
            db.execute(
                "INSERT INTO accounts(id,name,password) VALUES (1,'Alice',''),(2,'Bob','')",
                [],
            )
            .unwrap();
            (
                create_session(&db, 1).unwrap(),
                create_session(&db, 2).unwrap(),
            )
        };
        let issue = || {
            service
                .issue(
                    None,
                    AttemptRequest {
                        map: "Downline".into(),
                        bonus: None,
                    },
                )
                .unwrap()
                .token
        };
        let attempt = issue();
        let replay = finished_downline();
        let lookup = RankedSaveLookup {
            save_id: "a".repeat(32),
            replay_sha256: replay_sha256(&serde_json::to_vec(&replay).unwrap()),
        };
        let reserve = || ReserveRankedSave {
            attempt: attempt.clone(),
            save: lookup.clone(),
        };
        let request = || ResumeRankedSave {
            save_id: lookup.save_id.clone(),
            replay: replay.clone(),
        };
        let http = Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", http.server_addr());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..9 {
                    let Some(request) = http.recv_timeout(Duration::from_secs(4)).unwrap() else {
                        break;
                    };
                    service.respond(request);
                }
            });
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(3)))
                .http_status_as_error(false)
                .build()
                .new_agent();
            let post = |route: &str, token: &str, body: &[u8]| {
                agent
                    .post(format!("{base}/v1/runs/{route}"))
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Content-Type", "application/json")
                    .send(body)
                    .unwrap()
            };
            let lookup_json = serde_json::to_vec(&lookup).unwrap();
            let upload_json = serde_json::to_vec(&request()).unwrap();
            assert_eq!(post("resume", &original.token, &upload_json).status(), 409);
            let mut reserved = post(
                "reserve",
                &original.token,
                &serde_json::to_vec(&reserve()).unwrap(),
            );
            assert!(matches!(
                serde_json::from_reader::<_, SubmissionStatus>(reserved.body_mut().as_reader())
                    .unwrap(),
                SubmissionStatus::Pending
            ));
            assert_eq!(
                post(
                    "reserve",
                    &other.token,
                    &serde_json::to_vec(&reserve()).unwrap()
                )
                .status(),
                409
            );
            assert_eq!(
                post("resume/result", &other.token, &lookup_json).status(),
                409
            );
            let fresh = {
                let db = service.db.lock().unwrap();
                db.execute(
                    "DELETE FROM sessions WHERE token=?1",
                    [token_hash(&original.token).unwrap()],
                )
                .unwrap();
                create_session(&db, 1).unwrap()
            };
            assert_eq!(post("resume", &original.token, &upload_json).status(), 401);
            let mut altered = request();
            altered.replay.commands[0].pitch += 0.1;
            assert_eq!(
                post(
                    "resume",
                    &fresh.token,
                    &serde_json::to_vec(&altered).unwrap()
                )
                .status(),
                409
            );
            let mut accepted = post("resume", &fresh.token, &upload_json);
            assert_eq!(accepted.status(), 200);
            let first: Submission =
                serde_json::from_reader(accepted.body_mut().as_reader()).unwrap();
            assert!(first.personal_best);
            let mut receipt = post("resume/result", &fresh.token, &lookup_json);
            let SubmissionStatus::Saved(recovered) =
                serde_json::from_reader(receipt.body_mut().as_reader()).unwrap()
            else {
                panic!("missing saved receipt");
            };
            assert_eq!(
                serde_json::to_vec(&recovered).unwrap(),
                serde_json::to_vec(&first).unwrap()
            );
            let mut duplicate = post("resume", &fresh.token, &upload_json);
            assert_eq!(
                duplicate.body_mut().read_to_vec().unwrap(),
                serde_json::to_vec(&first).unwrap()
            );
        });
        let expired = issue();
        service
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE attempts SET expires=0 WHERE token=?1",
                [token_hash(&expired).unwrap()],
            )
            .unwrap();
        assert_eq!(
            service
                .reserve_ranked_save(Some(&original.token), reserve())
                .err()
                .unwrap()
                .status,
            401
        );
        let fresh = create_session(&service.db.lock().unwrap(), 1).unwrap();
        let expired_request = ReserveRankedSave {
            attempt: expired,
            save: RankedSaveLookup {
                save_id: "b".repeat(32),
                replay_sha256: lookup.replay_sha256,
            },
        };
        assert_eq!(
            service
                .reserve_ranked_save(Some(&fresh.token), expired_request)
                .err()
                .unwrap()
                .status,
            409
        );
        assert_eq!(service.board("downline", None).unwrap().rows.len(), 1);
    }

    #[test]
    fn public_profile_runs_pages_are_scoped_sorted_and_keep_legacy_bests() {
        let service = service();
        let other_token = {
            let db = service.db.lock().unwrap();
            db.execute(
                "INSERT INTO accounts(id,name,password) VALUES (1,'Alice',''),(2,'Bob','')",
                [],
            )
            .unwrap();
            for (index, entry) in catalog::entries().iter().take(7).enumerate() {
                let revision = versioned_revision(&entry.native_board_revision().unwrap());
                let id = format!("record-{index}");
                db.execute("INSERT INTO best_runs(id,account,map,revision,bonus,time_us,splits,speeds,replay)
                            VALUES (?1,1,?2,?3,0,5000000,'[]','[]','{}')",
                    params![id, entry.id, revision]).unwrap();
                db.execute(
                    "INSERT INTO best_run_details(id,created_at,ticks) VALUES (?1,?2,320)",
                    params![id, 1_760_000_000 + index as i64],
                )
                .unwrap();
                if index == 0 {
                    db.execute("INSERT INTO best_runs(id,account,map,revision,bonus,time_us,splits,speeds,replay)
                                VALUES ('bob',2,?1,?2,0,5000000,'[]','[]','{}')", params![entry.id, revision]).unwrap();
                }
            }
            db.execute("INSERT INTO best_runs(id,account,map,revision,bonus,time_us,splits,speeds,replay)
                        VALUES ('old',1,'Downline','v1:builtin-downline-v1',0,6000000,'[]','[]','{}')", []).unwrap();
            create_session(&db, 2).unwrap().token
        };
        let http = Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", http.server_addr());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..6 {
                    let Some(request) = http.recv_timeout(Duration::from_secs(3)).unwrap() else {
                        break;
                    };
                    service.respond(request);
                }
            });
            let agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .new_agent();
            let fetch = |path: &str, token: Option<&str>| {
                let mut req = agent.get(format!("{base}{path}"));
                if let Some(token) = token {
                    req = req.header("Authorization", format!("Bearer {token}"));
                }
                req.call().unwrap()
            };
            let page = |mut response: ureq::http::Response<ureq::Body>| -> PlayerRunsPage {
                assert_eq!(response.status(), 200);
                serde_json::from_reader(response.body_mut().as_reader()).unwrap()
            };
            let first = page(fetch("/v1/profiles/1/runs/newest/0", None));
            assert_eq!(first.account_id, 1);
            assert_eq!(first.total, 8);
            assert_eq!(first.runs.len(), PLAYER_RUNS_PAGE_SIZE);
            assert_eq!(first.runs[0].id, "record-6");
            assert_eq!(first.runs[0].place, Some(1));
            assert_eq!(
                page(fetch("/v1/profiles/1/runs/newest/0", Some(&other_token))),
                first
            );
            let second = page(fetch("/v1/profiles/1/runs/newest/1", None));
            assert_eq!(second.runs.len(), 2);
            assert_eq!(second.runs[0].id, "record-0");
            assert_eq!(second.runs[0].place, Some(1));
            let legacy = &second.runs[1];
            assert_eq!(legacy.id, "old");
            assert!(legacy.archived && legacy.replay_available);
            assert_eq!(
                (legacy.created_at.as_ref(), legacy.ticks, legacy.place),
                (None, None, None)
            );
            let bob = page(fetch("/v1/profiles/2/runs", None));
            assert_eq!(bob.runs.len(), 1);
            assert_eq!(bob.runs[0].id, "bob");
            assert_eq!(fetch("/v1/profiles/999/runs", None).status(), 404);
            assert_eq!(fetch("/v1/profiles/1/runs/newest/-1", None).status(), 400);
        });
    }

    #[test]
    fn stage_personal_bests_keep_their_replay_and_ignore_other_revisions() {
        let service = service();
        let revision = service.board_revision("Downline", None).unwrap();
        let save = |owner, id, payload, stages: &[replay::VerifiedStage], revision| {
            let db = service.db.lock().unwrap();
            record_stages(
                &db,
                StageSubmission {
                    owner,
                    map: "Downline",
                    revision,
                    bonus: 0,
                    id,
                    payload,
                    stages,
                },
            )
            .unwrap()
        };
        {
            let db = service.db.lock().unwrap();
            db.execute(
                "INSERT INTO accounts(name,password) VALUES ('Alice',''),('Bob','')",
                [],
            )
            .unwrap();
        }
        let stage = |number, time_us, start_tick, end_tick| replay::VerifiedStage {
            number,
            time_us,
            start_tick,
            end_tick,
        };
        let (_, first) = save(
            1,
            "a",
            "first",
            &[stage(1, 100, 2.5, 3.0), stage(2, 200, 4.0, 5.0)],
            &revision,
        );
        assert_eq!(first.len(), 2);
        let (_, second) = save(
            1,
            "b",
            "second",
            &[stage(1, 150, 2.0, 3.0), stage(2, 180, 4.0, 5.0)],
            &revision,
        );
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].previous_record_us, Some(200));
        let (changed, records) = save(2, "c", "third", &[stage(1, 110, 1.0, 2.0)], &revision);
        assert!(changed);
        assert!(records.is_empty());
        save(2, "d", "old rules", &[stage(1, 10, 1.0, 2.0)], "v1:old");
        let board = service.stage_board("downline", None, 1).unwrap();
        assert_eq!(board.stage, Some(1));
        assert_eq!(
            board.rows.iter().map(|r| r.time_us).collect::<Vec<_>>(),
            [100, 110]
        );
        assert_eq!(board.rows[0].stage_span.unwrap().start_tick, 2.5);
        let records = service.stage_bests("Downline", None, None).unwrap();
        assert_eq!(
            records
                .rows
                .iter()
                .map(|r| r.row.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        let mine = service.stage_bests("Downline", None, Some(2)).unwrap();
        assert_eq!(mine.rows.len(), 1);
        assert_eq!(mine.rows[0].row.id, "c");
        {
            let db = service.db.lock().unwrap();
            assert_eq!(find_replay(&db, "a").unwrap(), "first");
            assert_eq!(find_replay(&db, "b").unwrap(), "second");
        }
        save(1, "e", "new first", &[stage(1, 80, 1.0, 2.0)], &revision);
        let db = service.db.lock().unwrap();
        assert_eq!(find_replay(&db, "a").err().unwrap().status, 404);
        assert_eq!(find_replay(&db, "b").unwrap(), "second");
    }
}
