use crate::network::{
    CONTINUE_MAP, ClientMessage, DEFAULT_ADDRESS, EXTEND_MAP, FramedSocket, MAP_VOTE_CHOICES,
    PROTOCOL_VERSION, PlayerInfo, PlayerState, Request, Response, RoomCapabilities,
    RoomChatExcerpt, RoomIdentity, RoomInfo, RoomModeration, RoomReport, RoomReportPlayer,
    RoomTicketKey, ServerEvent, SnapshotEncoder, TicketClaims, VoteInfo, VoteKind, encode,
    map_time_label, room_server_proof, ticket_time, verify_room_proof,
};
use crate::network::{RoomTlsConfig, lan_address};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    net::{IpAddr, SocketAddr, TcpListener},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod moderation;
use moderation::{Moderation, REPORT_RETENTION, Sanctions};

const SERVER_STEP: Duration = Duration::from_millis(5);
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(50);
const JOIN_TIMEOUT: Duration = Duration::from_secs(4);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_REASON: &str = "The room is shutting down.";
const IDLE_TIMEOUT: Duration = Duration::from_secs(22);
const HEARTBEAT_RECEIPT_TIMEOUT: Duration = Duration::from_secs(20);
const WRITE_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const ROOM_IDENTITY_LEASE: Duration = Duration::from_secs(5 * 60);
const CHAT_INTERVAL: Duration = Duration::from_millis(750);
const REPORT_INTERVAL: Duration = Duration::from_secs(10);
const REPORT_EXCERPT_LINES: usize = 3;
const MAX_MESSAGES_PER_SECOND: u16 = 120;
const MAX_ROOM_PLAYERS: usize = 64;
const MAX_PENDING_CONNECTIONS: usize = 24;
const MAX_PENDING_PER_SOURCE: usize = 8;
const MAX_ROTATION_MAPS: usize = 128;
const MAP_EXTENSION: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug)]
pub struct RoomConfig {
    pub bind: SocketAddr,
    pub name: String,
    pub map: String,
    pub rotation: Vec<String>,
    pub capacity: usize,
    pub map_duration: Duration,
    pub vote_duration: Duration,
    pub managed: Option<RoomTicketKey>,
    pub tls: Option<RoomTlsConfig>,
    pub moderation_file: Option<PathBuf>,
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            bind: DEFAULT_ADDRESS.parse().unwrap(),
            name: "Classic Surf".to_owned(),
            map: "downline".to_owned(),
            rotation: ["downline", "surf_boreas", "surf_kitsune"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            capacity: 24,
            map_duration: Duration::from_secs(30 * 60),
            vote_duration: Duration::from_secs(20),
            managed: None,
            tls: None,
            moderation_file: None,
        }
    }
}

impl RoomConfig {
    fn validated(mut self) -> io::Result<Self> {
        if self.managed.is_some() && !self.bind.ip().is_loopback() {
            if self.tls.is_none() {
                return Err(bad_config(
                    "managed rooms on non-loopback addresses require a TLS certificate and key",
                ));
            }
            if self.moderation_file.is_none() {
                return Err(bad_config(
                    "managed rooms on non-loopback addresses need a moderation state file",
                ));
            }
        }
        self.name = room_text(&self.name, 48)
            .ok_or_else(|| bad_config("room name must have 1 to 48 printable characters"))?
            .to_owned();
        if !map_id_valid(&self.map) || self.rotation.iter().any(|map| !map_id_valid(map)) {
            return Err(bad_config("invalid map ID"));
        }
        if !self.rotation.contains(&self.map) {
            self.rotation.insert(0, self.map.clone());
        }
        let mut seen = BTreeSet::new();
        self.rotation.retain(|map| seen.insert(map.clone()));
        if self.rotation.len() > MAX_ROTATION_MAPS {
            return Err(bad_config("too many maps in the rotation"));
        }
        if !(1..=MAX_ROOM_PLAYERS).contains(&self.capacity) {
            return Err(bad_config("room capacity must be between 1 and 64"));
        }
        if !(Duration::from_secs(1)..=Duration::from_secs(24 * 3600)).contains(&self.map_duration)
            || !(Duration::from_secs(1)..=Duration::from_secs(120)).contains(&self.vote_duration)
        {
            return Err(bad_config("map or vote duration is out of range"));
        }
        Ok(self)
    }
}

fn bad_config(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn map_id_valid(map: &str) -> bool {
    !map.is_empty()
        && map.len() <= 64
        && map
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, b'_' | b'-'))
}

fn admission_source(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(address) => address.to_ipv4_mapped().map_or_else(
            || IpAddr::V6((u128::from(address) & !(u64::MAX as u128)).into()),
            IpAddr::V4,
        ),
    }
}

/// Operator commands for the local server console.
pub enum AdminCommand {
    Players,
    Kick(u64),
    Mute(u64),
    Unmute(u64),
    Ban(u64),
    Unban(IpAddr),
    UnbanAccount(i64),
    ChangeMap(String),
    Announce(String),
}

#[derive(Clone)]
pub struct RoomControl {
    commands: SyncSender<AdminCommand>,
    stop: Arc<AtomicBool>,
}

impl RoomControl {
    pub fn send(&self, command: AdminCommand) -> Result<(), String> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "The room control queue is busy.".to_owned(),
                mpsc::TrySendError::Disconnected(_) => "The room has stopped.".to_owned(),
            })
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

pub struct RoomServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    control: RoomControl,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl RoomServer {
    pub fn bind(config: RoomConfig) -> io::Result<Self> {
        let config = config.validated()?;
        let listener = TcpListener::bind(config.bind)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let moderation = match &config.moderation_file {
            Some(path) => Moderation::open(
                path,
                config
                    .managed
                    .as_ref()
                    .map_or("lan", RoomTicketKey::room_id),
                ticket_time(),
            )?,
            None => Moderation::default(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (controls, commands) = mpsc::sync_channel(64);
        let worker = thread::Builder::new()
            .name("surf-room-server".to_owned())
            .spawn(move || Room::new(config, moderation).serve(listener, commands, &worker_stop))?;
        Ok(Self {
            address,
            control: RoomControl {
                commands: controls,
                stop: Arc::clone(&stop),
            },
            stop,
            worker: Some(worker),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub fn control(&self) -> RoomControl {
        self.control.clone()
    }

    pub fn wait(mut self) -> io::Result<()> {
        self.join_worker()
    }

    pub fn stop(mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        self.join_worker()
    }

    fn join_worker(&mut self) -> io::Result<()> {
        self.worker.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| io::Error::other("the room worker panicked"))?
        })
    }
}

impl Drop for RoomServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.join_worker();
    }
}

struct Peer {
    socket: FramedSocket,
    address: SocketAddr,
    player: Option<PlayerInfo>,
    pending_join: Option<PendingJoin>,
    identity_until: Option<Instant>,
    dead: bool,
    closing_at: Option<Instant>,
    muted: bool,
    capabilities: RoomCapabilities,
    heartbeat_receipt: Option<HeartbeatReceipt>,
    write_deadline: WriteDeadline,
    last_activity: Instant,
    accepted_at: Instant,
    rate_started: Instant,
    message_count: u16,
    last_pose: Option<Instant>,
    last_chat: Option<Instant>,
    last_report: Option<Instant>,
    recent_chat: VecDeque<RoomChatExcerpt>,
}

struct PendingJoin {
    claims: TicketClaims,
    key: [u8; 32],
    challenge: [u8; 32],
    refresh: bool,
}

struct HeartbeatReceipt {
    confirmed_at: Instant,
    challenge: Option<[u8; 8]>,
}

#[derive(Default)]
struct WriteDeadline {
    written_bytes: u64,
    stalled_since: Option<Instant>,
}

impl WriteDeadline {
    fn stalled(&mut self, now: Instant, drained: bool, written_bytes: u64) -> bool {
        if drained || written_bytes != self.written_bytes {
            self.written_bytes = written_bytes;
            self.stalled_since = (!drained).then_some(now);
            return false;
        }
        let since = *self.stalled_since.get_or_insert(now);
        now.saturating_duration_since(since) >= WRITE_DRAIN_TIMEOUT
    }
}

impl Peer {
    fn new(socket: FramedSocket, address: SocketAddr, now: Instant) -> Self {
        Self {
            socket,
            address,
            player: None,
            pending_join: None,
            identity_until: None,
            dead: false,
            closing_at: None,
            muted: false,
            capabilities: RoomCapabilities::default(),
            heartbeat_receipt: None,
            write_deadline: WriteDeadline::default(),
            last_activity: now,
            accepted_at: now,
            rate_started: now,
            message_count: 0,
            last_pose: None,
            last_chat: None,
            last_report: None,
            recent_chat: VecDeque::new(),
        }
    }

    fn active_player(&self) -> Option<&PlayerInfo> {
        self.player
            .as_ref()
            .filter(|_| !self.dead && self.closing_at.is_none())
    }

    fn close(&mut self, now: Instant) {
        self.closing_at.get_or_insert(now);
        self.socket.discard_latest_snapshot();
    }

    fn expired(&self, now: Instant) -> bool {
        let (started, timeout) = if let Some(closing_at) = self.closing_at {
            (closing_at, CLOSE_TIMEOUT)
        } else if self.player.is_some() {
            (self.last_activity, IDLE_TIMEOUT)
        } else {
            (self.accepted_at, JOIN_TIMEOUT)
        };
        now.saturating_duration_since(started) >= timeout
    }

    fn accept_message(&mut self, now: Instant) -> bool {
        if self.closing_at.is_some() || self.dead {
            return false;
        }
        if now.duration_since(self.rate_started) >= Duration::from_secs(1) {
            self.rate_started = now;
            self.message_count = 0;
        }
        self.message_count += 1;
        self.last_activity = now;
        if self.message_count > MAX_MESSAGES_PER_SECOND {
            self.dead = true;
            return false;
        }
        true
    }

    fn queue(&mut self, frame: Arc<[u8]>) {
        if !self.dead && self.socket.queue(frame).is_err() {
            self.dead = true;
        }
    }

    fn flush(&mut self, now: Instant) {
        if self.socket.flush().is_err() {
            self.dead = true;
            return;
        }
        let drained = self.socket.drained();
        if self.closing_at.is_some() && drained {
            self.dead = self.socket.shutdown().unwrap_or(true);
        } else if self.active_player().is_some()
            && self
                .write_deadline
                .stalled(now, drained, self.socket.written_bytes())
        {
            self.dead = true;
        }
    }
}

#[derive(Default)]
struct Ballots {
    nominations: BTreeMap<u64, String>,
    rtv: BTreeSet<u64>,
    votes: BTreeMap<u64, String>,
    deadline: Option<Instant>,
    choices: Option<VoteChoices>,
    kind: VoteKind,
    scheduled: bool,
}

#[derive(Clone)]
struct VoteChoices {
    maps: Vec<String>,
    nominations: Vec<String>,
}

enum RoomChatCommand<'a> {
    TimeLeft,
    NextMap,
    Help,
    RockTheVote,
    Extend,
    Nominate(&'a str),
    Vote(&'a str),
    Unknown,
}

impl<'a> RoomChatCommand<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        if text.eq_ignore_ascii_case("timeleft") {
            return Some(Self::TimeLeft);
        }
        if text.eq_ignore_ascii_case("nextmap") {
            return Some(Self::NextMap);
        }
        let command = text.strip_prefix(['/', '!'])?;
        let (name, argument) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        let argument = argument.trim();
        Some(match name.to_ascii_lowercase().as_str() {
            "timeleft" => Self::TimeLeft,
            "nextmap" => Self::NextMap,
            "help" => Self::Help,
            "rtv" => Self::RockTheVote,
            "extend" => Self::Extend,
            "nominate" => Self::Nominate(argument),
            "vote" => Self::Vote(argument),
            _ => Self::Unknown,
        })
    }
}

struct Room {
    config: RoomConfig,
    map: String,
    previous_map: Option<String>,
    epoch: u64,
    map_deadline: Instant,
    next_snapshot: Instant,
    snapshot_encoder: SnapshotEncoder,
    next_vote_update: Instant,
    peers: BTreeMap<u64, Peer>,
    next_id: u64,
    moderation: Moderation,
    redeemed_tickets: BTreeMap<[u8; 16], i64>,
    ballots: Ballots,
}

impl Room {
    fn new(config: RoomConfig, moderation: Moderation) -> Self {
        let now = Instant::now();
        Self {
            map: config.map.clone(),
            previous_map: None,
            epoch: 1,
            map_deadline: now + config.map_duration,
            next_snapshot: now,
            snapshot_encoder: SnapshotEncoder::default(),
            next_vote_update: now,
            config,
            peers: BTreeMap::new(),
            next_id: 1,
            moderation,
            redeemed_tickets: BTreeMap::new(),
            ballots: Ballots::default(),
        }
    }

    fn serve(
        &mut self,
        listener: TcpListener,
        commands: Receiver<AdminCommand>,
        stop: &AtomicBool,
    ) -> io::Result<()> {
        let mut next_moderation_cleanup = Instant::now() + Duration::from_secs(60);
        'running: while !stop.load(Ordering::Acquire) {
            let now = Instant::now();
            if now >= next_moderation_cleanup {
                next_moderation_cleanup = now + Duration::from_secs(60);
                if let Err(error) = self.moderation.purge_expired(ticket_time()) {
                    eprintln!("Could not remove expired room moderation data: {error}");
                }
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
            self.accept_connections(&listener, now)?;
            if stop.load(Ordering::Acquire) {
                break;
            }
            for id in self.peers.keys().copied().collect::<Vec<_>>() {
                if stop.load(Ordering::Acquire) {
                    break 'running;
                }
                self.receive_from(id, now);
            }
            for command in commands.try_iter().take(64) {
                if stop.load(Ordering::Acquire) {
                    break 'running;
                }
                self.admin(command, now);
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
            self.remove_departed(now);
            self.advance_vote(now);
            let snapshot_due = Instant::now() >= self.next_snapshot;
            if snapshot_due {
                self.publish_snapshot();
            }
            let flush_at = Instant::now();
            for peer in self.peers.values_mut().filter(|peer| !peer.dead) {
                peer.flush(flush_at);
            }
            let work_finished = Instant::now();
            if snapshot_due {
                self.next_snapshot = next_snapshot_deadline(self.next_snapshot, work_finished);
            }
            let wake_at = (now + SERVER_STEP).min(self.next_snapshot);
            if wake_at > work_finished {
                thread::sleep(wake_at - work_finished);
            }
        }
        // Closing the listener first prevents new admissions while peers drain.
        drop(listener);
        self.drain_shutdown();
        Ok(())
    }

    fn drain_shutdown(&mut self) {
        let started = Instant::now();
        let deadline = started + CLOSE_TIMEOUT;
        self.broadcast(ServerEvent::Disconnected(SHUTDOWN_REASON.into()));
        self.peers.retain(|_, peer| {
            if peer.active_player().is_some() {
                peer.close(started);
            }
            !peer.dead && peer.closing_at.is_some()
        });
        while !self.peers.is_empty() {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            self.peers.retain(|_, peer| {
                !peer.expired(now) && !peer.socket.drain_shutdown().unwrap_or(true)
            });
            if !self.peers.is_empty() {
                thread::sleep(SERVER_STEP.min(deadline.saturating_duration_since(Instant::now())));
            }
        }
        self.peers.clear();
    }

    fn accept_connections(&mut self, listener: &TcpListener, now: Instant) -> io::Result<()> {
        for _ in 0..32 {
            let (stream, address) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            if !self.can_accept(address.ip()) {
                continue;
            }
            let socket = if let Some(tls) = &self.config.tls {
                FramedSocket::encrypted_server(stream, tls)
            } else {
                FramedSocket::new(stream)
            };
            let Ok(socket) = socket else {
                continue;
            };
            let id = self.next_id;
            self.next_id += 1;
            self.peers.insert(id, Peer::new(socket, address, now));
            if self.moderation.sanctions.ip_bans.contains(&address.ip()) {
                self.reject(id, "You are banned from this room.");
            } else if self.config.managed.is_none() && !lan_address(address.ip()) {
                self.reject(id, "Unmanaged rooms only accept local and LAN connections.");
            }
        }
        Ok(())
    }

    fn can_accept(&self, address: IpAddr) -> bool {
        if self.peers.len() >= self.config.capacity + MAX_PENDING_CONNECTIONS {
            return false;
        }
        let source = admission_source(address);
        let (mut pending, mut same_source) = (0, 0);
        for peer in self
            .peers
            .values()
            .filter(|peer| !peer.dead && peer.active_player().is_none())
        {
            pending += 1;
            if admission_source(peer.address.ip()) == source {
                same_source += 1;
            }
        }
        pending < MAX_PENDING_CONNECTIONS && same_source < MAX_PENDING_PER_SOURCE
    }

    fn receive_from(&mut self, id: u64, now: Instant) {
        let peer = self.peers.get_mut(&id).unwrap();
        if peer.expired(now) {
            peer.dead = true;
        }
        if peer.dead || peer.closing_at.is_some() {
            return;
        }
        if peer.heartbeat_receipt.as_ref().is_some_and(|receipt| {
            now.saturating_duration_since(receipt.confirmed_at) >= HEARTBEAT_RECEIPT_TIMEOUT
        }) {
            self.reject(
                id,
                "The room could no longer reach your connection. Reconnect to continue.",
            );
            return;
        }
        if peer.identity_until.is_some_and(|expires| now >= expires) {
            self.reject(
                id,
                "The room could not renew your sign-in. Reconnect to continue.",
            );
            return;
        }
        let Ok(batch) = peer.socket.receive::<Request>() else {
            peer.dead = true;
            return;
        };
        for request in batch.messages {
            if !self.peers.get_mut(&id).unwrap().accept_message(now) {
                break;
            }
            self.handle(id, request, now);
        }
        if batch.eof {
            self.peers.get_mut(&id).unwrap().close(now);
        }
    }

    fn handle(&mut self, id: u64, request: Request, now: Instant) {
        let joined = self.peers[&id].active_player().is_some();
        match request {
            Request::Probe { version } if !joined => {
                if version != PROTOCOL_VERSION {
                    self.reject(id, "This room uses a different client protocol.");
                } else {
                    self.send_event(id, ServerEvent::RoomInfo(self.info(now)));
                    self.peers.get_mut(&id).unwrap().close(now);
                }
            }
            Request::Join {
                version,
                name,
                capabilities,
            } if !joined => {
                if self.config.managed.is_some() {
                    self.reject(id, "This room requires a ticket from the records service.");
                } else {
                    self.peers.get_mut(&id).unwrap().capabilities = capabilities;
                    self.join(id, version, &name, None, now);
                }
            }
            Request::TicketJoin {
                version,
                ticket,
                capabilities,
            } if !joined => {
                self.peers.get_mut(&id).unwrap().capabilities = capabilities;
                self.begin_ticket_join(id, version, &ticket, false);
            }
            Request::TicketRefresh { ticket } if joined => {
                self.begin_ticket_join(id, PROTOCOL_VERSION, &ticket, true)
            }
            Request::TicketProof(proof) => self.finish_ticket_join(id, proof, now),
            Request::Heartbeat if joined => self.heartbeat(id),
            Request::HeartbeatReceipt(challenge) if joined => {
                if let Some(receipt) = self.peers.get_mut(&id).unwrap().heartbeat_receipt.as_mut()
                    && receipt.challenge == Some(challenge)
                {
                    receipt.confirmed_at = now;
                    receipt.challenge = None;
                }
            }
            Request::Pose { epoch, state } if joined => {
                if epoch == self.epoch {
                    self.update_pose(id, state, now);
                }
            }
            Request::Command(message) if joined => self.handle_command(id, message, now),
            _ => self.reject(id, "Unexpected room handshake."),
        }
    }

    fn heartbeat(&mut self, id: u64) {
        let Some(receipt) = self.peers.get_mut(&id).unwrap().heartbeat_receipt.as_mut() else {
            self.send_response(id, Response::Heartbeat);
            return;
        };
        if receipt.challenge.is_some() {
            return;
        }
        let mut challenge = [0; 8];
        if getrandom::fill(&mut challenge).is_err() {
            self.reject(id, "The room could not verify your connection.");
            return;
        }
        receipt.challenge = Some(challenge);
        self.send_response(id, Response::HeartbeatChallenge(challenge));
    }

    fn begin_ticket_join(&mut self, id: u64, version: u16, ticket: &str, refresh: bool) {
        if version != PROTOCOL_VERSION {
            self.reject(id, "This room uses a different client protocol.");
            return;
        }
        if self.peers[&id].pending_join.is_some() {
            self.reject(id, "Unexpected room handshake.");
            return;
        }
        let wall_time = ticket_time();
        self.redeemed_tickets
            .retain(|_, expires| *expires > wall_time);
        let Some(signer) = self.config.managed.as_ref() else {
            self.reject(id, "This room does not use records tickets.");
            return;
        };
        let Some(claims) = signer
            .verify(ticket, wall_time)
            .filter(|claims| !self.redeemed_tickets.contains_key(&claims.nonce))
        else {
            self.reject(id, "The room ticket is invalid, expired or already used.");
            return;
        };
        if refresh
            && (claims.identity.account_id.is_none()
                || claims.identity.account_id != self.room_account(id))
        {
            self.reject(
                id,
                "A room identity refresh must belong to the connected account.",
            );
            return;
        }
        let mut challenge = [0; 32];
        if getrandom::fill(&mut challenge).is_err() {
            self.reject(id, "The room could not start a secure join.");
            return;
        }
        let key = signer.proof_key(ticket);
        let server_proof = room_server_proof(&key, &challenge);
        self.peers.get_mut(&id).unwrap().pending_join = Some(PendingJoin {
            claims,
            key,
            challenge,
            refresh,
        });
        self.send_response(
            id,
            Response::JoinChallenge {
                challenge,
                server_proof,
            },
        );
    }

    fn finish_ticket_join(&mut self, id: u64, proof: [u8; 32], now: Instant) {
        let Some(PendingJoin {
            claims,
            key,
            challenge,
            refresh,
        }) = self.peers.get_mut(&id).unwrap().pending_join.take()
        else {
            self.reject(id, "Unexpected room handshake.");
            return;
        };
        if claims.expires <= ticket_time()
            || self.redeemed_tickets.contains_key(&claims.nonce)
            || !verify_room_proof(&key, &challenge, &proof)
        {
            self.reject(id, "The room ticket is invalid, expired or already used.");
            return;
        }
        if refresh {
            let peer = self.peers.get_mut(&id).unwrap();
            let player = peer
                .player
                .as_mut()
                .expect("refresh requires an active player");
            player.identity = Some(claims.identity);
            player.name = claims.name;
            peer.identity_until = Some(now + ROOM_IDENTITY_LEASE);
            self.redeemed_tickets.insert(claims.nonce, claims.expires);
            return;
        }
        if self.join(
            id,
            PROTOCOL_VERSION,
            &claims.name,
            Some(claims.identity),
            now,
        ) {
            self.redeemed_tickets.insert(claims.nonce, claims.expires);
        }
    }

    fn join(
        &mut self,
        id: u64,
        version: u16,
        name: &str,
        identity: Option<RoomIdentity>,
        now: Instant,
    ) -> bool {
        if version != PROTOCOL_VERSION {
            self.reject(id, "This room uses a different client protocol.");
            return false;
        }
        let Some(name) = room_text(name, 24) else {
            self.reject(
                id,
                "Enter a display name with 1 to 24 printable characters.",
            );
            return false;
        };
        let players = self.player_count();
        if players >= self.config.capacity {
            self.reject(id, "This room is full.");
            return false;
        }
        if let Some(account) = identity.and_then(|identity| identity.account_id) {
            if self.moderation.sanctions.account_bans.contains(&account) {
                self.reject(id, "You are banned from this room.");
                return false;
            }
            if self
                .peers
                .values()
                .filter_map(Peer::active_player)
                .any(|player| {
                    player.identity.and_then(|identity| identity.account_id) == Some(account)
                })
            {
                self.reject(id, "This account already has a connection to the room.");
                return false;
            }
        }
        if players == 0 {
            self.ballots = Ballots::default();
            self.map_deadline = now + self.config.map_duration;
        }
        let peer = self.peers.get_mut(&id).unwrap();
        peer.heartbeat_receipt = peer
            .capabilities
            .heartbeat_receipts
            .then_some(HeartbeatReceipt {
                confirmed_at: now,
                challenge: None,
            });
        peer.identity_until = identity
            .and_then(|identity| identity.account_id)
            .map(|_| now + ROOM_IDENTITY_LEASE);
        peer.player = Some(PlayerInfo {
            id,
            name: name.to_owned(),
            state: None,
            spectating: None,
            identity,
        });
        self.send_event(
            id,
            ServerEvent::Welcome {
                id,
                room: self.info(now),
            },
        );
        self.publish_vote(now);
        true
    }

    fn handle_command(&mut self, id: u64, message: ClientMessage, now: Instant) {
        if self.config.managed.is_some() && self.room_account(id).is_none() {
            let reason = match &message {
                ClientMessage::Nominate(_) => Some("You need an account to nominate a map."),
                ClientMessage::RockTheVote => Some("You need an account to rock the vote."),
                ClientMessage::Vote(_) => Some("You need an account to vote."),
                _ => None,
            };
            if let Some(reason) = reason {
                self.error(id, reason);
                return;
            }
        }
        match message {
            ClientMessage::State(_) => self.error(id, "Unexpected room position message."),
            ClientMessage::Chat(text) => self.chat(id, &text, now),
            ClientMessage::Nominate(name) => {
                let Some(map) = self.nomination_map(&name) else {
                    self.error(id, "Choose a different map from this room's rotation.");
                    return;
                };
                if self.ballots.deadline.is_some() {
                    self.error(id, "A map vote is already open.");
                } else if self.ballots.nominations.get(&id) != Some(&map) {
                    self.ballots.nominations.insert(id, map);
                    self.publish_vote(now);
                }
            }
            ClientMessage::RockTheVote => {
                if self.ballots.deadline.is_some() {
                    self.error(id, "A map vote is already open.");
                } else if self.ballots.rtv.insert(id) {
                    if self.ballots.rtv.len() >= self.majority() {
                        self.start_vote(now, None);
                    } else {
                        self.publish_vote(now);
                    }
                }
            }
            ClientMessage::Vote(map) => self.cast_vote(id, map, now),
            ClientMessage::Extend => {
                if self.config.managed.is_some() {
                    self.start_extension_vote(id, now);
                } else {
                    self.cast_vote(id, self.map.clone(), now);
                }
            }
            ClientMessage::Spectate(target) => self.spectate(id, target),
            ClientMessage::Report { player, reason } => self.report(id, player, &reason, now),
            ClientMessage::ReviewReports { offset } => {
                if self.is_moderator(id) {
                    let page = self.moderation.reports(offset, ticket_time());
                    self.send_event(id, ServerEvent::Reports(page));
                } else {
                    self.error(id, "Only moderators can review room reports.");
                }
            }
            ClientMessage::ChangeMap(map) => {
                if !self.is_moderator(id) {
                    self.error(id, "Only moderators can change the server map.");
                } else if self.config.rotation.contains(&map) {
                    self.set_map(map, now, None);
                } else {
                    self.error(id, "Choose a map from this room's rotation.");
                }
            }
            ClientMessage::Moderate {
                player,
                action,
                minutes,
            } => {
                self.moderate(id, player, action, minutes, now);
            }
            ClientMessage::Leave => self.peers.get_mut(&id).unwrap().dead = true,
        }
    }

    fn update_pose(&mut self, id: u64, state: PlayerState, now: Instant) {
        let peer = self.peers.get_mut(&id).unwrap();
        let player = peer.player.as_mut().unwrap();
        if player.spectating.is_some()
            || peer
                .last_pose
                .is_some_and(|last| now.duration_since(last) < Duration::from_millis(16))
        {
            return;
        }
        if !state_valid(state) {
            self.error(id, "Invalid player state.");
            return;
        }
        peer.last_pose = Some(now);
        player.state = Some(state);
    }

    fn chat(&mut self, id: u64, text: &str, now: Instant) {
        if let Some(command) = room_text(text, 160).and_then(RoomChatCommand::parse) {
            self.room_chat_command(id, command, now);
            return;
        }
        let account = self.room_account(id);
        if self.config.managed.is_some() && account.is_none() {
            self.error(
                id,
                "Sign in to send chat messages. You can still read chat.",
            );
            return;
        }
        let Some(text) = room_text(text, 160) else {
            self.error(id, "Enter a message with 1 to 160 printable characters.");
            return;
        };
        let lower = text.to_ascii_lowercase();
        if [
            "http:",
            "https:",
            "www.",
            "discord.gg/",
            "discord.com/invite/",
        ]
        .iter()
        .any(|part| lower.contains(part))
        {
            self.error(id, "Chat messages cannot contain links.");
            return;
        }
        if self.peers[&id].muted
            || account
                .is_some_and(|account| self.moderation.sanctions.muted(account, ticket_time()))
        {
            self.error(id, "The room moderator muted your chat.");
            return;
        }
        if !self.reserve_chat_slot(id, now) {
            return;
        }
        let peer = self.peers.get_mut(&id).unwrap();
        let from = peer.player.as_ref().unwrap().name.clone();
        if self.config.managed.is_some() {
            if peer.recent_chat.len() >= REPORT_EXCERPT_LINES {
                peer.recent_chat.pop_front();
            }
            peer.recent_chat.push_back(RoomChatExcerpt {
                sent_at: ticket_time(),
                text: text.to_owned(),
            });
        }
        self.broadcast(ServerEvent::Chat {
            from,
            text: text.to_owned(),
        });
    }

    fn reserve_chat_slot(&mut self, id: u64, now: Instant) -> bool {
        let peer = self.peers.get_mut(&id).unwrap();
        if peer
            .last_chat
            .is_some_and(|last| now.duration_since(last) < CHAT_INTERVAL)
        {
            self.error(id, "Chat is limited to one message at a time.");
            return false;
        }
        peer.last_chat = Some(now);
        true
    }

    fn room_chat_command(&mut self, id: u64, command: RoomChatCommand<'_>, now: Instant) {
        let action = match command {
            RoomChatCommand::TimeLeft | RoomChatCommand::NextMap => {
                if !self.reserve_chat_slot(id, now) {
                    return;
                }
                let answer = if matches!(command, RoomChatCommand::TimeLeft) {
                    map_time_label(self.map_deadline.saturating_duration_since(now))
                } else if self.ballots.kind == VoteKind::Map && self.ballots.deadline.is_some() {
                    "The next map is being voted on.".into()
                } else {
                    let next = self.next_map();
                    let title = crate::catalog::find(&next).map_or(next.as_str(), |map| &map.name);
                    format!("Next map in rotation is {title}.")
                };
                let name = &self.peers[&id].player.as_ref().unwrap().name;
                self.broadcast(ServerEvent::Notice(format!("{name} · {answer}")));
                return;
            }
            RoomChatCommand::Help => {
                self.send_event(
                    id,
                    ServerEvent::Notice(
                        "timeleft · nextmap · /rtv · /extend · /nominate map · /vote number".into(),
                    ),
                );
                return;
            }
            RoomChatCommand::RockTheVote => ClientMessage::RockTheVote,
            RoomChatCommand::Extend => ClientMessage::Extend,
            RoomChatCommand::Nominate(name) if !name.is_empty() => {
                ClientMessage::Nominate(name.into())
            }
            RoomChatCommand::Nominate(_) => {
                self.error(id, "Use /nominate followed by a map name.");
                return;
            }
            RoomChatCommand::Vote(argument) => {
                let choice = argument
                    .parse::<usize>()
                    .ok()
                    .and_then(|number| number.checked_sub(1))
                    .and_then(|index| self.ballots.choices.as_ref()?.maps.get(index));
                let Some(choice) = choice else {
                    self.error(id, "Use /vote followed by one of the displayed numbers.");
                    return;
                };
                ClientMessage::Vote(choice.clone())
            }
            RoomChatCommand::Unknown => {
                self.error(
                    id,
                    "Unknown room command. Type /help for available commands.",
                );
                return;
            }
        };
        self.handle_command(id, action, now);
    }

    fn room_account(&self, id: u64) -> Option<i64> {
        self.peers.get(&id)?.active_player()?.identity?.account_id
    }

    fn is_moderator(&self, id: u64) -> bool {
        self.peers
            .get(&id)
            .and_then(Peer::active_player)
            .and_then(|player| player.identity)
            .is_some_and(|identity| identity.moderator)
    }

    fn update_moderation(
        &mut self,
        actor: Option<u64>,
        update: impl FnOnce(&mut Sanctions),
    ) -> bool {
        if let Err(error) = self.moderation.update(update, ticket_time()) {
            eprintln!("Could not save moderation state: {error}");
            if let Some(actor) = actor {
                self.error(actor, "Could not save the room moderation change.");
            }
            return false;
        }
        true
    }

    fn moderate(
        &mut self,
        actor: u64,
        player: u64,
        action: RoomModeration,
        minutes: Option<u16>,
        now: Instant,
    ) {
        if !self.is_moderator(actor) || player == actor || self.is_moderator(player) {
            self.error(actor, "Not allowed");
            return;
        }
        if self
            .peers
            .get(&player)
            .and_then(Peer::active_player)
            .is_none()
        {
            self.error(actor, "Choose an active player.");
            return;
        }
        let account = self.room_account(player);
        match (action, account) {
            (RoomModeration::Kick, _) => self.admin(AdminCommand::Kick(player), now),
            (RoomModeration::Ban, Some(account)) => {
                if self.update_moderation(Some(actor), |state| {
                    state.account_bans.insert(account);
                }) {
                    self.reject(player, "You are banned from this room.");
                }
            }
            (RoomModeration::Ban, None) => self.ban_address(player, Some(actor)),
            (RoomModeration::Mute, Some(account)) => {
                let Some(minutes) = minutes.filter(|minutes| (1..=1440).contains(minutes)) else {
                    self.error(actor, "Choose a mute duration from 1 to 1,440 minutes.");
                    return;
                };
                let until = ticket_time() + i64::from(minutes) * 60;
                if !self.update_moderation(Some(actor), |state| {
                    state.mutes.insert(account, Some(until));
                }) {
                    return;
                }
                self.send_event(
                    player,
                    ServerEvent::Notice(format!(
                        "A moderator muted your chat for {minutes} minutes."
                    )),
                );
            }
            (RoomModeration::Unmute, Some(_)) => self.mute_player(player, false, Some(actor)),
            (RoomModeration::Mute | RoomModeration::Unmute, None) => {
                self.error(actor, "Guest chat is already read-only.");
            }
        }
    }

    fn spectate(&mut self, id: u64, target: Option<u64>) {
        if target.is_some_and(|target| {
            target == id
                || self
                    .peers
                    .get(&target)
                    .and_then(Peer::active_player)
                    .is_none_or(|player| player.spectating.is_some())
        }) {
            self.error(id, "Choose an active player to spectate.");
            return;
        }
        let player = self.peers.get_mut(&id).unwrap().player.as_mut().unwrap();
        player.spectating = target;
        player.state = None;
        if target.is_some() {
            self.release_spectators(id);
        }
    }

    fn release_spectators(&mut self, target: u64) {
        for player in self
            .peers
            .values_mut()
            .filter_map(|peer| peer.player.as_mut())
        {
            if player.spectating == Some(target) {
                player.spectating = None;
            }
        }
    }

    fn report(&mut self, id: u64, target: u64, reason: &str, now: Instant) {
        let target_peer = self.peers.get(&target);
        let Some(target_player) = target_peer
            .and_then(Peer::active_player)
            .filter(|player| player.id != id)
        else {
            self.error(id, "Choose another player in the room.");
            return;
        };
        let Some(reason) = room_text(reason, 240) else {
            self.error(
                id,
                "Enter a report reason with 1 to 240 printable characters.",
            );
            return;
        };
        let reported_at = ticket_time();
        let target = RoomReportPlayer {
            account_id: target_player
                .identity
                .and_then(|identity| identity.account_id),
            name: target_player.name.clone(),
        };
        let excerpts = target_peer
            .expect("an active target has a peer")
            .recent_chat
            .iter()
            .filter(|line| line.sent_at > reported_at - REPORT_RETENTION)
            .cloned()
            .collect();
        let peer = self.peers.get_mut(&id).unwrap();
        if peer
            .last_report
            .is_some_and(|last| now.duration_since(last) < REPORT_INTERVAL)
        {
            self.error(id, "Wait before sending another report.");
            return;
        }
        peer.last_report = Some(now);
        let player = peer.player.as_ref().unwrap();
        let reporter = RoomReportPlayer {
            account_id: player.identity.and_then(|identity| identity.account_id),
            name: player.name.clone(),
        };
        if self.config.managed.is_some() {
            let report = RoomReport {
                reporter,
                target,
                reason: reason.to_owned(),
                reported_at,
                excerpts,
            };
            if !self.update_moderation(Some(id), |state| state.push_report(report)) {
                return;
            }
        } else {
            eprintln!(
                "Player report: {} → {}: {reason}",
                reporter.name, target.name
            );
        }
        self.send_event(
            id,
            ServerEvent::Notice("Report sent to the room operator.".to_owned()),
        );
    }

    fn player_count(&self) -> usize {
        self.peers
            .values()
            .filter(|peer| peer.active_player().is_some())
            .count()
    }

    fn majority(&self) -> usize {
        let voters = if self.config.managed.is_some() {
            self.peers
                .values()
                .filter_map(Peer::active_player)
                .filter(|player| {
                    player
                        .identity
                        .and_then(|identity| identity.account_id)
                        .is_some()
                })
                .count()
        } else {
            self.player_count()
        };
        voters / 2 + 1
    }

    fn info(&self, now: Instant) -> RoomInfo {
        let players = self.player_count();
        RoomInfo {
            name: self.config.name.clone(),
            map: self.map.clone(),
            players,
            capacity: self.config.capacity,
            room_id: self
                .config
                .managed
                .as_ref()
                .map(|key| key.room_id().to_owned()),
            map_remaining_ms: (players > 0).then(|| self.map_remaining_ms(now)),
        }
    }

    fn map_remaining_ms(&self, now: Instant) -> u64 {
        self.map_deadline.saturating_duration_since(now).as_millis() as u64
    }

    fn eligible_map(&self, map: &str) -> bool {
        map != self.map
            && (self.config.managed.is_none()
                || self.config.rotation.len() <= 2
                || Some(map) != self.previous_map.as_deref())
    }

    fn nomination_map(&self, name: &str) -> Option<String> {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        self.config
            .rotation
            .iter()
            .find(|map| {
                self.eligible_map(map)
                    && (map.eq_ignore_ascii_case(&name)
                        || crate::catalog::find(map).is_some_and(|entry| {
                            entry.name.eq_ignore_ascii_case(&name)
                                || entry.slug.eq_ignore_ascii_case(&name)
                        }))
            })
            .cloned()
    }

    fn rotation_after_current(&self) -> impl Iterator<Item = &String> {
        let rotation = &self.config.rotation;
        let current = rotation.iter().position(|map| map == &self.map).unwrap();
        rotation[current + 1..].iter().chain(&rotation[..current])
    }

    fn vote_choices(&self) -> VoteChoices {
        let nominations = self
            .ballots
            .nominations
            .values()
            .filter(|map| self.eligible_map(map))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut maps = nominations.clone();
        maps.extend(
            self.rotation_after_current()
                .filter(|map| self.eligible_map(map) && !nominations.contains(map))
                .cloned(),
        );
        if self.config.managed.is_none() || maps.is_empty() {
            maps.push(self.map.clone());
        }
        if self.config.managed.is_some() {
            maps.truncate(MAP_VOTE_CHOICES);
        }
        VoteChoices { maps, nominations }
    }

    fn vote_info(&self, now: Instant) -> VoteInfo {
        let choices = self
            .ballots
            .choices
            .clone()
            .unwrap_or_else(|| self.vote_choices());
        let vote_remaining_ms = self
            .ballots
            .deadline
            .map(|deadline| deadline.saturating_duration_since(now).as_millis() as u64);
        VoteInfo {
            maps: choices.maps,
            nominations: choices.nominations,
            votes: self.vote_counts(),
            needed: self.majority(),
            open: self.ballots.deadline.is_some(),
            seconds_left: vote_remaining_ms.unwrap_or_default().div_ceil(1000),
            map_remaining_ms: Some(self.map_remaining_ms(now)),
            vote_remaining_ms,
            rtv: self.ballots.rtv.len(),
            kind: self.ballots.kind,
        }
    }

    fn vote_counts(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for map in self.ballots.votes.values() {
            *counts.entry(map.clone()).or_insert(0) += 1;
        }
        counts
    }

    fn start_vote(&mut self, now: Instant, scheduled_expiry: Option<Instant>) {
        self.ballots.kind = VoteKind::Map;
        self.ballots.choices = Some(self.vote_choices());
        self.ballots.scheduled = scheduled_expiry.is_some();
        self.ballots.deadline = Some(scheduled_expiry.unwrap_or(now + self.config.vote_duration));
        self.next_vote_update = now + Duration::from_secs(1);
        self.broadcast(ServerEvent::Notice("Map vote started.".to_owned()));
        self.publish_vote(now);
    }

    fn start_extension_vote(&mut self, player: u64, now: Instant) {
        let eligible = self
            .peers
            .get(&player)
            .and_then(Peer::active_player)
            .and_then(|player| player.identity)
            .is_some_and(|identity| identity.account_id.is_some() && identity.points > 1000);
        if !eligible {
            self.error(
                player,
                "You need an account with over 1,000 points to start an extension vote.",
            );
            return;
        }
        if self.ballots.deadline.is_some() {
            self.error(player, "A vote is already open.");
            return;
        }
        self.ballots.kind = VoteKind::Extend;
        self.ballots.scheduled = false;
        self.ballots.choices = Some(VoteChoices {
            maps: vec![EXTEND_MAP.to_owned(), CONTINUE_MAP.to_owned()],
            nominations: Vec::new(),
        });
        self.ballots.deadline = Some(now + self.config.vote_duration);
        self.next_vote_update = now + Duration::from_secs(1);
        self.publish_vote(now);
    }

    fn cast_vote(&mut self, id: u64, map: String, now: Instant) {
        let Some(choices) = self
            .ballots
            .choices
            .as_ref()
            .filter(|_| self.ballots.deadline.is_some_and(|deadline| now < deadline))
        else {
            self.error(id, "A map vote is not open.");
            return;
        };
        if !choices.maps.contains(&map) {
            self.error(id, "Choose a map from this room's rotation.");
            return;
        }
        if self.ballots.votes.get(&id) != Some(&map) {
            self.ballots.votes.insert(id, map);
            self.publish_vote(now);
        }
    }

    fn advance_vote(&mut self, now: Instant) {
        if self.player_count() == 0 {
            self.ballots = Ballots::default();
            return;
        }
        let Some(deadline) = self.ballots.deadline else {
            let remaining = self.map_deadline.saturating_duration_since(now);
            let managed = self.config.managed.is_some();
            if managed && remaining.is_zero() {
                self.set_map(self.next_map(), now, Some(self.map_deadline));
            } else if remaining.is_zero() || self.ballots.rtv.len() >= self.majority() {
                self.start_vote(now, None);
            } else if managed && remaining <= self.config.vote_duration {
                self.start_vote(now, Some(self.map_deadline));
            }
            return;
        };

        if self.ballots.scheduled && now < deadline {
            if now >= self.next_vote_update {
                self.next_vote_update = now + Duration::from_secs(1);
                self.publish_vote(now);
            }
            return;
        }

        let vote = self.vote_info(now);
        if vote.kind == VoteKind::Extend {
            let passed = vote.votes.get(EXTEND_MAP).copied().unwrap_or(0) >= vote.needed;
            let rejected = vote.votes.get(CONTINUE_MAP).copied().unwrap_or(0) >= vote.needed
                || now >= deadline;
            if passed || rejected {
                self.ballots.kind = VoteKind::Map;
                self.ballots.choices = None;
                self.ballots.votes.clear();
                self.ballots.deadline = None;
                if passed {
                    self.map_deadline += MAP_EXTENSION;
                    self.broadcast(ServerEvent::Notice(
                        "The current map was extended by 15 minutes.".into(),
                    ));
                } else {
                    self.broadcast(ServerEvent::Notice(
                        "The extension vote did not pass.".into(),
                    ));
                }
                self.publish_vote(now);
            } else if now >= self.next_vote_update {
                self.next_vote_update = now + Duration::from_secs(1);
                self.broadcast(ServerEvent::Vote(vote));
            }
            return;
        }
        let winner = vote
            .maps
            .iter()
            .enumerate()
            .filter_map(|(index, map)| {
                vote.votes
                    .get(map)
                    .map(|&count| (count, Reverse(index), map))
            })
            .max()
            .filter(|(count, _, _)| *count >= vote.needed || now >= deadline)
            .map(|(_, _, map)| map.clone());
        if let Some(map) = winner.or_else(|| (now >= deadline).then(|| self.next_map())) {
            self.set_map(map, now, self.ballots.scheduled.then_some(deadline));
        } else if now >= self.next_vote_update {
            self.next_vote_update = now + Duration::from_secs(1);
            self.broadcast(ServerEvent::Vote(vote));
        }
    }

    fn next_map(&self) -> String {
        self.rotation_after_current()
            .find(|map| self.eligible_map(map))
            .unwrap_or(&self.map)
            .clone()
    }

    fn set_map(&mut self, map: String, now: Instant, scheduled_expiry: Option<Instant>) {
        self.map_deadline = scheduled_expiry
            .map(|expiry| expiry + self.config.map_duration)
            .filter(|deadline| *deadline > now)
            .unwrap_or(now + self.config.map_duration);
        self.ballots = Ballots::default();
        if map == self.map {
            self.broadcast(ServerEvent::Notice(
                "The current map was extended.".to_owned(),
            ));
        } else {
            self.previous_map = Some(std::mem::replace(&mut self.map, map.clone()));
            self.epoch += 1;
            for peer in self.peers.values_mut() {
                if let Some(player) = &mut peer.player {
                    player.state = None;
                }
                peer.last_pose = None;
                peer.socket.discard_latest_snapshot();
            }
            self.broadcast(ServerEvent::MapChanged { map });
        }
        self.publish_vote(now);
    }

    fn remove_departed(&mut self, now: Instant) {
        let mut departed = Vec::new();
        for (&id, peer) in &mut self.peers {
            if (peer.dead || peer.closing_at.is_some()) && peer.player.take().is_some() {
                departed.push(id);
            }
        }
        self.peers.retain(|_, peer| !peer.dead);
        if departed.is_empty() {
            return;
        }
        for id in departed {
            self.ballots.nominations.remove(&id);
            self.ballots.rtv.remove(&id);
            self.ballots.votes.remove(&id);
            self.release_spectators(id);
        }
        self.publish_vote(now);
    }

    fn publish_vote(&mut self, now: Instant) {
        if self.player_count() != 0 {
            self.broadcast(ServerEvent::Vote(self.vote_info(now)));
        }
    }

    fn admin(&mut self, command: AdminCommand, now: Instant) {
        match command {
            AdminCommand::Players => {
                for peer in self.peers.values().filter_map(Peer::active_player) {
                    eprintln!("#{} {}", peer.id, peer.name);
                }
            }
            AdminCommand::Kick(id) => self.reject(id, "A room moderator removed you."),
            AdminCommand::Mute(id) | AdminCommand::Unmute(id) => {
                self.mute_player(id, matches!(command, AdminCommand::Mute(_)), None);
            }
            AdminCommand::Ban(id) => self.ban_address(id, None),
            AdminCommand::Unban(address) => {
                self.update_moderation(None, |state| {
                    state.ip_bans.remove(&address);
                });
            }
            AdminCommand::UnbanAccount(account) => {
                self.update_moderation(None, |state| {
                    state.account_bans.remove(&account);
                });
            }
            AdminCommand::ChangeMap(map) => {
                if self.config.rotation.contains(&map) {
                    self.set_map(map, now, None);
                } else {
                    eprintln!("Unknown map in this room's rotation: {map}");
                }
            }
            AdminCommand::Announce(message) => {
                if let Some(message) = room_text(&message, 160) {
                    self.broadcast(ServerEvent::Notice(message.to_owned()));
                }
            }
        }
    }

    fn mute_player(&mut self, id: u64, muted: bool, actor: Option<u64>) {
        let account = self.room_account(id);
        if let Some(account) = account {
            if !self.update_moderation(actor, |state| {
                if muted {
                    state.mutes.insert(account, None);
                } else {
                    state.mutes.remove(&account);
                }
            }) {
                return;
            }
        }
        if let Some(peer) = self.peers.get_mut(&id) {
            peer.muted = muted && account.is_none();
            let status = if muted { "muted" } else { "unmuted" };
            self.send_event(
                id,
                ServerEvent::Notice(format!("The room moderator {status} your chat.")),
            );
        }
    }

    fn ban_address(&mut self, id: u64, actor: Option<u64>) {
        let Some(address) = self.peers.get(&id).map(|peer| peer.address.ip()) else {
            return;
        };
        if self.update_moderation(actor, |state| {
            state.ip_bans.insert(address);
        }) {
            let affected: Vec<_> = self
                .peers
                .iter()
                .filter(|(_, peer)| peer.address.ip() == address)
                .map(|(&id, _)| id)
                .collect();
            for id in affected {
                self.reject(id, "You are banned from this room.");
            }
        }
    }

    fn reject(&mut self, id: u64, reason: &str) {
        self.error(id, reason);
        if let Some(peer) = self.peers.get_mut(&id) {
            peer.close(Instant::now());
        }
    }

    fn error(&mut self, id: u64, message: &str) {
        self.send_event(id, ServerEvent::Error(message.to_owned()));
    }

    fn send_event(&mut self, id: u64, event: ServerEvent) {
        self.send_response(
            id,
            Response::Event {
                epoch: self.epoch,
                event,
            },
        );
    }

    fn send_response(&mut self, id: u64, response: Response) {
        if let Some(peer) = self.peers.get_mut(&id)
            && let Ok(frame) = encode(&response)
        {
            peer.queue(frame);
        }
    }

    fn broadcast(&mut self, event: ServerEvent) {
        if let Ok(frame) = encode(&Response::Event {
            epoch: self.epoch,
            event,
        }) {
            for peer in self
                .peers
                .values_mut()
                .filter(|peer| peer.active_player().is_some())
            {
                peer.queue(Arc::clone(&frame));
            }
        }
    }

    fn publish_snapshot(&mut self) {
        let roster = self
            .peers
            .values()
            .filter_map(Peer::active_player)
            .cloned()
            .collect::<Vec<_>>();
        if roster.is_empty() {
            return;
        }
        let Ok(plain) = encode(&Response::Event {
            epoch: self.epoch,
            event: ServerEvent::Players(roster),
        }) else {
            return;
        };
        let compressed =
            if self.peers.values().any(|peer| {
                peer.capabilities.compressed_snapshots && peer.active_player().is_some()
            }) {
                self.snapshot_encoder.encode(&plain)
            } else {
                None
            };
        for peer in self
            .peers
            .values_mut()
            .filter(|peer| peer.active_player().is_some())
        {
            let frame = if peer.capabilities.compressed_snapshots {
                compressed.as_ref().unwrap_or(&plain)
            } else {
                &plain
            };
            peer.socket.queue_snapshot(Arc::clone(frame));
        }
    }
}

fn next_snapshot_deadline(scheduled: Instant, sent: Instant) -> Instant {
    let elapsed = sent.duration_since(scheduled);
    let phase = Duration::from_nanos((elapsed.as_nanos() % SNAPSHOT_INTERVAL.as_nanos()) as u64);
    let remaining = SNAPSHOT_INTERVAL - phase;
    // Keep the original phase while skipping any slot too close to a late send.
    let delay = if remaining < SNAPSHOT_INTERVAL / 2 {
        remaining + SNAPSHOT_INTERVAL
    } else {
        remaining
    };
    sent + delay
}

fn state_valid(state: PlayerState) -> bool {
    state
        .position
        .into_iter()
        .all(|value| value.is_finite() && value.abs() <= 1_000_000.0)
        && state.yaw.is_finite()
        && state.yaw.abs() <= 1_000_000.0
        && state.pitch.abs() <= std::f32::consts::FRAC_PI_2
        && (0.0..=7.0 * 24.0 * 3600.0).contains(&state.elapsed)
        && state.eye_height.is_none_or(PlayerState::valid_eye_height)
}

fn room_text(text: &str, max_characters: usize) -> Option<&str> {
    let text = text.trim();
    let mut length = 0;
    for ch in text.chars() {
        length += 1;
        if length > max_characters
            || ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            return None;
        }
    }
    (length > 0).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{Client, ClientMessage, PlayerState, ServerEvent, discover};
    use std::{
        thread,
        time::{Duration, Instant},
    };

    fn wait_for(client: &Client, predicate: impl Fn(&ServerEvent) -> bool) -> ServerEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            while let Some(event) = client.poll() {
                assert!(!matches!(event, ServerEvent::Disconnected(_)), "{event:?}");
                if predicate(&event) {
                    return event;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for server event"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn join(address: &str, name: &str) -> (Client, u64) {
        let client = Client::connect(address, name).unwrap();
        let ServerEvent::Welcome { id, .. } = wait_for(&client, |event| {
            matches!(event, ServerEvent::Welcome { .. })
        }) else {
            unreachable!()
        };
        (client, id)
    }

    #[test]
    fn snapshot_deadline_keeps_phase_after_small_delays() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(next_snapshot_deadline(at(0), at(4)), at(50));
        assert_eq!(next_snapshot_deadline(at(50), at(60)), at(100));
        assert_eq!(next_snapshot_deadline(at(100), at(125)), at(150));
    }

    #[test]
    fn snapshot_deadline_skips_missed_and_nearby_slots() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(next_snapshot_deadline(at(0), at(126)), at(200));
        assert_eq!(next_snapshot_deadline(at(50), at(167)), at(200));
        assert_eq!(next_snapshot_deadline(at(50), at(185)), at(250));
        assert_eq!(next_snapshot_deadline(at(50), at(600)), at(650));
    }

    #[test]
    fn pending_connection_slots_are_source_scoped_and_expire_from_accept() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut room = Room::new(RoomConfig::default(), Moderation::default());
        let now = Instant::now();
        let clients: Vec<_> = (0..MAX_PENDING_PER_SOURCE)
            .map(|_| std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap())
            .collect();
        room.accept_connections(&listener, now).unwrap();
        assert_eq!(room.peers.len(), clients.len());
        assert!(!room.can_accept("127.0.0.1".parse().unwrap()));
        assert!(!room.can_accept("::ffff:127.0.0.1".parse().unwrap()));
        assert!(room.can_accept("192.168.1.4".parse().unwrap()));
        let peer = room.peers.get_mut(&1).unwrap();
        assert!(peer.accept_message(now + JOIN_TIMEOUT / 2));
        assert!(peer.expired(now + JOIN_TIMEOUT));
        room.receive_from(1, now + JOIN_TIMEOUT);
        room.remove_departed(now + JOIN_TIMEOUT);
        assert!(room.can_accept("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn heartbeat_receipt_recovers_before_deadline_and_inbound_traffic_cannot_extend_it() {
        let mut room = Room::new(RoomConfig::default(), Moderation::default());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, address) = listener.accept().unwrap();
        let started = Instant::now();
        let at = |seconds| started + Duration::from_secs(seconds);
        room.peers.insert(
            1,
            Peer::new(FramedSocket::new(stream).unwrap(), address, started),
        );
        let request = |room: &mut Room, request, now| {
            assert!(room.peers.get_mut(&1).unwrap().accept_message(now));
            room.handle(1, request, now);
        };
        request(
            &mut room,
            Request::Join {
                version: PROTOCOL_VERSION,
                name: "Reader".into(),
                capabilities: RoomCapabilities {
                    heartbeat_receipts: true,
                    ..Default::default()
                },
            },
            started,
        );
        let receipt = |room: &Room| {
            let receipt = room.peers[&1].heartbeat_receipt.as_ref().unwrap();
            (receipt.confirmed_at, receipt.challenge)
        };
        request(&mut room, Request::Heartbeat, at(5));
        let (_, Some(first)) = receipt(&room) else {
            panic!("missing heartbeat challenge");
        };
        let mut invalid = first;
        invalid[0] ^= 1;
        request(&mut room, Request::HeartbeatReceipt(invalid), at(9));
        assert_eq!(receipt(&room), (started, Some(first)));
        request(&mut room, Request::HeartbeatReceipt(first), at(12));
        assert_eq!(receipt(&room), (at(12), None));
        room.receive_from(1, at(20));
        assert_eq!(room.player_count(), 1);

        request(&mut room, Request::Heartbeat, at(22));
        let (_, Some(second)) = receipt(&room) else {
            panic!("missing next heartbeat challenge");
        };
        assert_ne!(second, first);
        request(&mut room, Request::Heartbeat, at(28));
        request(&mut room, Request::HeartbeatReceipt(first), at(31));
        assert_eq!(receipt(&room), (at(12), Some(second)));
        room.receive_from(1, at(32));
        room.remove_departed(at(32));
        assert_eq!(room.player_count(), 0);
    }

    #[test]
    fn write_deadline_allows_progress_even_when_output_stays_queued() {
        let now = Instant::now();
        let at = |seconds| now + Duration::from_secs(seconds);
        let mut deadline = WriteDeadline::default();
        assert!(!deadline.stalled(now, false, 0));
        for (bytes, seconds) in [(1, 9), (2, 18), (3, 27)] {
            assert!(!deadline.stalled(at(seconds), false, bytes));
        }
        assert!(!deadline.stalled(at(36), false, 3));
        assert!(deadline.stalled(at(37), false, 3));
        assert!(!deadline.stalled(at(38), true, 3));
        assert!(!deadline.stalled(at(50), false, 3));
        assert!(deadline.stalled(at(60), false, 3));
    }

    #[test]
    fn shutdown_closes_admission_and_bounds_a_stalled_writer() {
        use std::{io::Write, net::TcpStream};

        let pair = || {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (server, address) = listener.accept().unwrap();
            (client, server, address)
        };
        let (_stalled_client, mut stalled_server, stalled_address) = pair();
        stalled_server.set_nonblocking(true).unwrap();
        let mut buffered = 0;
        let mut blocked_polls = 0;
        let payload = [0; 64 * 1024];
        loop {
            match stalled_server.write(&payload) {
                Ok(written) => {
                    assert!(written > 0);
                    blocked_polls = 0;
                    buffered += written;
                    assert!(buffered < 16 * 1024 * 1024, "test socket never blocked");
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    blocked_polls += 1;
                    if blocked_polls == 4 {
                        break;
                    }
                    thread::sleep(SERVER_STEP);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => panic!("could not fill the stalled socket: {error}"),
            }
        }

        let (healthy_client, healthy_server, healthy_address) = pair();
        let mut healthy_client = FramedSocket::new(healthy_client).unwrap();
        let now = Instant::now();
        let mut room = Room::new(RoomConfig::default(), Moderation::default());
        for (id, name, stream, address) in [
            (1, "Stalled", stalled_server, stalled_address),
            (2, "Healthy", healthy_server, healthy_address),
        ] {
            room.peers.insert(
                id,
                Peer::new(FramedSocket::new(stream).unwrap(), address, now),
            );
            assert!(room.join(id, PROTOCOL_VERSION, name, None, now));
        }
        let queued = encode(&Response::Event {
            epoch: 1,
            event: ServerEvent::Notice("x".repeat(45_000)),
        })
        .unwrap();
        for _ in 0..4 {
            room.peers.get_mut(&1).unwrap().queue(Arc::clone(&queued));
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (_control, commands) = mpsc::sync_channel(1);
        let started = Instant::now();
        let worker = thread::spawn(move || room.serve(listener, commands, &AtomicBool::new(true)));
        let mut notified = false;
        loop {
            let batch = healthy_client.receive::<Response>().unwrap();
            notified |= batch.messages.into_iter().any(|response| {
                matches!(response, Response::Event {
                    event: ServerEvent::Disconnected(reason), ..
                } if reason == SHUTDOWN_REASON)
            });
            if batch.eof {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(3));
            thread::sleep(SERVER_STEP);
        }
        assert!(notified);
        drop(healthy_client);
        assert!(
            !worker.is_finished(),
            "stalled fixture did not exercise the drain deadline"
        );
        assert!(TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_err());
        worker.join().unwrap().unwrap();
        assert!(started.elapsed() < CLOSE_TIMEOUT + Duration::from_secs(1));
    }

    #[test]
    fn first_surfer_gets_a_full_map_and_open_ballots_keep_their_choices() {
        let mut room = Room::new(RoomConfig::default(), Moderation::default());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, address) = listener.accept().unwrap();
        let now = Instant::now();
        room.map_deadline = now - Duration::from_secs(1);
        room.peers.insert(
            1,
            Peer::new(FramedSocket::new(stream).unwrap(), address, now),
        );
        assert!(room.join(1, PROTOCOL_VERSION, "Alice", None, now));
        room.advance_vote(now);
        assert_eq!(room.map_deadline, now + room.config.map_duration);
        assert!(!room.vote_info(now).open);

        room.ballots.nominations.insert(1, "surf_kitsune".into());
        room.start_vote(now, None);
        let choices = room.vote_info(now).maps;
        assert_eq!(choices.first().map(String::as_str), Some("surf_kitsune"));
        room.ballots.nominations.clear();
        assert_eq!(room.vote_info(now).maps, choices);
    }

    #[test]
    fn managed_scheduled_map_vote_opens_early_and_resolves_at_expiry() {
        let key = RoomTicketKey::from_hex("scheduled", &"0123456789abcdef".repeat(4)).unwrap();
        let mut room = Room::new(
            RoomConfig {
                managed: Some(key),
                ..RoomConfig::default()
            },
            Moderation::default(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, address) = listener.accept().unwrap();
        let now = Instant::now();
        room.peers.insert(
            1,
            Peer::new(FramedSocket::new(stream).unwrap(), address, now),
        );
        assert!(room.join(1, PROTOCOL_VERSION, "Alice", None, now));
        let expiry = room.map_deadline;
        let opening = expiry - room.config.vote_duration;
        let millisecond = Duration::from_millis(1);
        room.advance_vote(opening - millisecond);
        assert!(!room.vote_info(opening - millisecond).open);
        room.advance_vote(opening);
        assert_eq!(room.ballots.deadline, Some(expiry));
        assert_eq!(room.vote_info(opening).vote_remaining_ms, Some(20_000));

        room.cast_vote(1, "surf_kitsune".into(), opening);
        assert_eq!(room.vote_info(opening).votes.get("surf_kitsune"), Some(&1));
        assert_eq!(room.majority(), 1);
        room.advance_vote(expiry - millisecond);
        assert_eq!(room.map, "downline");

        room.advance_vote(expiry + millisecond);
        assert_eq!(room.map, "surf_kitsune");
        assert_eq!(room.map_deadline, expiry + room.config.map_duration);
        assert!(!room.vote_info(expiry + millisecond).open);

        let next_expiry = room.map_deadline;
        room.advance_vote(next_expiry);
        assert_eq!(room.map, "surf_boreas");
        assert_eq!(room.map_deadline, next_expiry + room.config.map_duration);
    }

    #[test]
    fn managed_map_votes_put_nominations_first_and_skip_the_last_map() {
        let key = RoomTicketKey::from_hex("choices", &"0123456789abcdef".repeat(4)).unwrap();
        let mut room = Room::new(
            RoomConfig {
                managed: Some(key),
                rotation: [
                    "downline",
                    "surf_ace",
                    "surf_boreas",
                    "surf_kitsune",
                    "surf_lux",
                    "surf_nyx",
                    "surf_frost",
                    "surf_reprise",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
                ..RoomConfig::default()
            },
            Moderation::default(),
        );
        room.previous_map = Some("surf_ace".into());
        room.ballots.nominations.insert(1, "surf_nyx".into());
        assert_eq!(
            room.vote_choices().maps,
            [
                "surf_nyx",
                "surf_boreas",
                "surf_kitsune",
                "surf_lux",
                "surf_frost"
            ]
        );
        assert_eq!(room.next_map(), "surf_boreas");
        assert_eq!(room.nomination_map("ACE"), None);
    }

    #[test]
    fn managed_room_chat_queries_and_numbered_map_votes() {
        let key = RoomTicketKey::from_hex("commands", &"0123456789abcdef".repeat(4)).unwrap();
        let server = RoomServer::bind(RoomConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            managed: Some(key.clone()),
            map_duration: Duration::from_secs(90),
            ..RoomConfig::default()
        })
        .unwrap();
        let address = server.local_addr().to_string();
        let guest =
            Client::connect_with_ticket(&address, key.issue(None, ticket_time()).unwrap()).unwrap();
        let joined = wait_for(&guest, |event| matches!(event, ServerEvent::Welcome { .. }));
        assert!(
            matches!(joined, ServerEvent::Welcome { room, .. } if room.map_remaining_ms == Some(90_000))
        );
        let signed = Client::connect_with_ticket(
            &address,
            key.issue(Some((17, "Alice".into(), false, 0)), ticket_time())
                .unwrap(),
        )
        .unwrap();
        wait_for(&signed, |event| {
            matches!(event, ServerEvent::Welcome { .. })
        });

        guest.send(ClientMessage::Chat("timeleft".into())).unwrap();
        wait_for(
            &signed,
            |event| matches!(event, ServerEvent::Notice(message) if message.contains("2 min left")),
        );
        guest.send(ClientMessage::Chat("!rtv".into())).unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::Error(message) if message.contains("account to rock the vote")),
        );

        signed
            .send(ClientMessage::Chat("!nominate Kitsune".into()))
            .unwrap();
        wait_for(
            &signed,
            |event| matches!(event, ServerEvent::Vote(vote) if vote.nominations == ["surf_kitsune"]),
        );
        signed.send(ClientMessage::Chat("/rtv".into())).unwrap();
        wait_for(
            &signed,
            |event| matches!(event, ServerEvent::Vote(vote) if vote.open && vote.vote_remaining_ms == Some(20_000) && vote.maps.first().is_some_and(|map| map == "surf_kitsune")),
        );
        signed.send(ClientMessage::Chat("/vote 1".into())).unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::MapChanged { map } if map == "surf_kitsune"),
        );
    }

    #[test]
    fn managed_room_checks_tickets_and_enforces_guest_and_moderator_permissions() {
        let key = RoomTicketKey::from_hex("managed", &"0123456789abcdef".repeat(4)).unwrap();
        let server = RoomServer::bind(RoomConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            managed: Some(key.clone()),
            ..RoomConfig::default()
        })
        .unwrap();
        let address = server.local_addr().to_string();
        assert_eq!(
            discover(&address).unwrap().room_id.as_deref(),
            Some("managed")
        );
        let legacy = Client::connect(&address, "Unverified Moderator").unwrap();
        wait_for(
            &legacy,
            |event| matches!(event, ServerEvent::Error(message) if message.contains("requires a ticket")),
        );

        let guest_ticket = key.issue(None, ticket_time()).unwrap();
        let replay = serde_json::to_vec(&guest_ticket).unwrap();
        let guest = Client::connect_with_ticket(&address, guest_ticket).unwrap();
        let ServerEvent::Welcome { id: guest_id, .. } =
            wait_for(&guest, |event| matches!(event, ServerEvent::Welcome { .. }))
        else {
            unreachable!()
        };
        let replayed =
            Client::connect_with_ticket(&address, serde_json::from_slice(&replay).unwrap())
                .unwrap();
        wait_for(
            &replayed,
            |event| matches!(event, ServerEvent::Error(message) if message.contains("already used")),
        );

        let alice = Client::connect_with_ticket(
            &address,
            key.issue(Some((7, "Alice".into(), false, 1000)), ticket_time())
                .unwrap(),
        )
        .unwrap();
        let ServerEvent::Welcome { id: alice_id, .. } =
            wait_for(&alice, |event| matches!(event, ServerEvent::Welcome { .. }))
        else {
            unreachable!()
        };
        wait_for(&guest, |event| {
            matches!(event, ServerEvent::Players(players) if
            players.iter().any(|player| player.id == alice_id && player.name == "Alice"
                && player.identity.is_some_and(|identity| identity.account_id == Some(7) && !identity.moderator && identity.points == 1000)))
        });

        guest.send(ClientMessage::Chat("spoof".into())).unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::Error(message) if message.contains("You can still read chat")),
        );
        guest.send(ClientMessage::RockTheVote).unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::Error(message) if message.contains("account to rock the vote")),
        );
        alice.send(ClientMessage::Chat("Nice run".into())).unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::Chat { from, text } if from == "Alice" && text == "Nice run"),
        );
        alice
            .send(ClientMessage::Moderate {
                player: guest_id,
                action: RoomModeration::Kick,
                minutes: None,
            })
            .unwrap();
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::Error(message) if message == "Not allowed"),
        );
        alice.send(ClientMessage::Extend).unwrap();
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::Error(message) if message.contains("over 1,000 points")),
        );

        let moderator = Client::connect_with_ticket(
            &address,
            key.issue(Some((8, "Morgan".into(), true, 1001)), ticket_time())
                .unwrap(),
        )
        .unwrap();
        wait_for(&moderator, |event| {
            matches!(event, ServerEvent::Welcome { .. })
        });
        guest
            .send(ClientMessage::ReviewReports { offset: 0 })
            .unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::Error(message) if message.contains("Only moderators can review")),
        );
        guest
            .send(ClientMessage::Report {
                player: alice_id,
                reason: "Review the chat.".into(),
            })
            .unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::Notice(message) if message.contains("Report sent")),
        );
        moderator
            .send(ClientMessage::ReviewReports { offset: 0 })
            .unwrap();
        wait_for(&moderator, |event| {
            matches!(event, ServerEvent::Reports(page) if
            page.total == 1 && page.reports[0].target.account_id == Some(7)
                && page.reports[0].reason == "Review the chat."
                && page.reports[0].excerpts.iter().any(|line| line.text == "Nice run"))
        });
        moderator.send(ClientMessage::Extend).unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::Vote(vote) if vote.open && vote.kind == VoteKind::Extend),
        );
        guest.send(ClientMessage::Vote(EXTEND_MAP.into())).unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::Error(message) if message.contains("account to vote")),
        );
        alice.send(ClientMessage::Vote(EXTEND_MAP.into())).unwrap();
        moderator
            .send(ClientMessage::Vote(EXTEND_MAP.into()))
            .unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::Notice(message) if message.contains("extended by 15 minutes")),
        );
        alice
            .refresh_identity(
                key.issue(Some((7, "Alice".into(), false, 1500)), ticket_time())
                    .unwrap(),
            )
            .unwrap();
        wait_for(&guest, |event| {
            matches!(event, ServerEvent::Players(players) if
            players.iter().any(|player| player.id == alice_id && player.identity.is_some_and(|identity| identity.points == 1500)))
        });
        moderator
            .send(ClientMessage::Moderate {
                player: alice_id,
                action: RoomModeration::Mute,
                minutes: Some(10),
            })
            .unwrap();
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::Notice(message) if message.contains("muted your chat")),
        );
        alice.send(ClientMessage::Chat("muted".into())).unwrap();
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::Error(message) if message.contains("muted your chat")),
        );
        moderator
            .send(ClientMessage::ChangeMap("surf_boreas".into()))
            .unwrap();
        wait_for(
            &guest,
            |event| matches!(event, ServerEvent::MapChanged { map } if map == "surf_boreas"),
        );
    }

    #[test]
    fn localhost_room_discovers_and_relays_players_chat_and_map_changes() {
        let server = RoomServer::bind(RoomConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            rotation: vec!["surf_boreas".to_owned()],
            ..RoomConfig::default()
        })
        .unwrap();
        let address = server.local_addr().to_string();
        let initial = discover(&address).unwrap();
        assert_eq!((initial.map.as_str(), initial.players), ("downline", 0));

        let (alice, alice_id) = join(&address, "Alice");
        let (bob, bob_id) = join(&address, "Bob");
        assert_ne!(alice_id, bob_id);
        assert_eq!(discover(&address).unwrap().players, 2);

        let state = PlayerState {
            position: [12.0, 32.0, 50.0],
            yaw: 0.5,
            pitch: -0.2,
            elapsed: 7.25,
            finished: false,
            crouched: true,
            eye_height: Some(55.5),
            motion: Some([53, 1]),
        };
        alice.send(ClientMessage::State(state)).unwrap();
        wait_for(
            &bob,
            |event| matches!(event, ServerEvent::Players(players) if players.iter().any(|p| p.id == alice_id && p.state == Some(state))),
        );

        alice
            .send(ClientMessage::State(PlayerState {
                eye_height: Some(72.0),
                ..state
            }))
            .unwrap();
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::Error(error) if error == "Invalid player state."),
        );

        bob.send(ClientMessage::Spectate(Some(alice_id))).unwrap();
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::Players(players) if players.iter().any(|p| p.id == bob_id && p.spectating == Some(alice_id) && p.state.is_none())),
        );

        bob.send(ClientMessage::Chat("Nice run!".to_owned()))
            .unwrap();
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::Chat { from, text } if from == "Bob" && text == "Nice run!"),
        );
        alice
            .send(ClientMessage::Chat("https://example.com".to_owned()))
            .unwrap();
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::Error(error) if error.contains("link")),
        );

        alice
            .send(ClientMessage::Nominate("surf_boreas".to_owned()))
            .unwrap();
        alice.send(ClientMessage::RockTheVote).unwrap();
        bob.send(ClientMessage::RockTheVote).unwrap();
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::Vote(vote) if vote.open),
        );
        alice
            .send(ClientMessage::Vote("surf_boreas".to_owned()))
            .unwrap();
        bob.send(ClientMessage::Vote("surf_boreas".to_owned()))
            .unwrap();
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::MapChanged { map } if map == "surf_boreas"),
        );
        assert_eq!(discover(&address).unwrap().map, "surf_boreas");

        drop(bob);
        wait_for(
            &alice,
            |event| matches!(event, ServerEvent::Players(players) if players.len() == 1),
        );
    }
}
