use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    catalog::MapMode,
    loadout::Loadout,
    movement::{CROUCH_EYE_HEIGHT, EYE_HEIGHT},
};

mod compact_snapshot;
mod room_tickets;
mod snapshots;
mod transport;
pub use room_tickets::{RoomIdentity, RoomJoinTicket, RoomTicketKey};
pub(crate) use room_tickets::{TicketClaims, room_server_proof, ticket_time, verify_room_proof};
pub(crate) use snapshots::SnapshotEncoder;
use snapshots::{COMPACT_FRAME, COMPRESSED_FRAME, SnapshotDecoder};
pub(crate) use transport::lan_address;
use transport::{RoomAddress, RoomClientTls, Transport};
pub use transport::{
    RoomTlsConfig, normalize_address, room_address_authority, room_address_is_loopback,
    room_address_uses_tls,
};

pub const DEFAULT_ADDRESS: &str = "127.0.0.1:27165";
pub const EXTEND_MAP: &str = "@extend";
pub const CONTINUE_MAP: &str = "@continue";
pub const MAP_VOTE_CHOICES: usize = 5;
const DEFAULT_PORT: u16 = 27165;
pub(crate) const PROTOCOL_VERSION: u16 = 1;
pub(crate) const MAX_FRAME_BYTES: usize = 48 * 1024;
const MAX_PENDING_BYTES: usize = 256 * 1024;
const FRAME_BATCH: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const ADDRESS_STAGGER: Duration = Duration::from_millis(200);
const MAX_RESOLVED_ADDRESSES: usize = 8;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(20);
const STATE_INTERVAL: Duration = Duration::from_millis(33);
const POLL_INTERVAL: Duration = Duration::from_millis(4);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfo {
    pub name: String,
    pub map: String,
    #[serde(default)]
    pub mode: MapMode,
    pub players: usize,
    pub capacity: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_remaining_ms: Option<u64>,
}

pub fn map_time_label(remaining: Duration) -> String {
    MapTimeLabel(remaining).to_string()
}

pub struct MapTimeLabel(pub Duration);

impl std::fmt::Display for MapTimeLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let remaining = self.0;
        let seconds = remaining.as_nanos().div_ceil(1_000_000_000);
        if seconds == 0 {
            f.write_str("Changing map…")
        } else if remaining < Duration::from_secs(60) {
            write!(f, "{seconds}s left")
        } else {
            write!(f, "{} min left", seconds.div_ceil(60))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub elapsed: f64,
    pub finished: bool,
    #[serde(default)]
    pub crouched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eye_height: Option<f32>,
    /// Monotonic sender milliseconds and a generation that changes on a pose discontinuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<[u32; 2]>,
}

impl PlayerState {
    pub(crate) fn valid_eye_height(height: f32) -> bool {
        (CROUCH_EYE_HEIGHT..=EYE_HEIGHT).contains(&height)
    }

    /// The latest eye height, or a stance-based height for older peers.
    pub fn camera_eye_height(&self) -> f32 {
        self.eye_height
            .filter(|&height| Self::valid_eye_height(height))
            .unwrap_or(if self.crouched {
                CROUCH_EYE_HEIGHT
            } else {
                EYE_HEIGHT
            })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerInfo {
    pub id: u64,
    pub name: String,
    pub state: Option<PlayerState>,
    pub spectating: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<RoomIdentity>,
    #[serde(default, skip_serializing_if = "Loadout::is_default")]
    pub loadout: Loadout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomModeration {
    Kick,
    Mute,
    Unmute,
    Ban,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomReportPlayer {
    pub account_id: Option<i64>,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomChatExcerpt {
    pub sent_at: i64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomReport {
    pub reporter: RoomReportPlayer,
    pub target: RoomReportPlayer,
    pub reason: String,
    pub reported_at: i64,
    pub excerpts: Vec<RoomChatExcerpt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomReportPage {
    pub offset: u16,
    pub total: u16,
    pub reports: Vec<RoomReport>,
}

pub const ROOM_REPORT_PAGE_SIZE: u16 = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteInfo {
    pub maps: Vec<String>,
    pub nominations: Vec<String>,
    pub votes: BTreeMap<String, usize>,
    pub needed: usize,
    pub open: bool,
    pub seconds_left: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_remaining_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vote_remaining_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vote_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<u8>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub runoff: bool,
    pub rtv: usize,
    #[serde(default)]
    pub kind: VoteKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoteKind {
    #[default]
    Map,
    Extend,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ClientMessage {
    State(PlayerState),
    Chat(String),
    Nominate(String),
    RockTheVote,
    Vote(String),
    VoteFor {
        vote_id: u64,
        choice: u8,
    },
    Extend,
    Spectate(Option<u64>),
    Report {
        player: u64,
        reason: String,
    },
    ReviewReports {
        offset: u16,
    },
    ChangeMap(String),
    Moderate {
        player: u64,
        action: RoomModeration,
        minutes: Option<u16>,
    },
    Leave,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerEvent {
    RoomInfo(RoomInfo),
    Welcome { id: u64, room: RoomInfo },
    Players(Vec<PlayerInfo>),
    Chat { from: String, text: String },
    MapChanged { map: String },
    Vote(VoteInfo),
    Reports(RoomReportPage),
    Notice(String),
    Error(String),
    Disconnected(String),
}

// Additive Join fields. Old servers ignore them and new servers default them
// to false or zero for old clients.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub(crate) struct RoomCapabilities {
    #[serde(default, skip_serializing_if = "is_false")]
    pub compressed_snapshots: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub compact_snapshots: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub heartbeat_receipts: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bhop_rooms: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub loadouts: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub community_maps: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub source_map_rules: u8,
}

const CLIENT_CAPABILITIES: RoomCapabilities = RoomCapabilities {
    compressed_snapshots: true,
    compact_snapshots: true,
    heartbeat_receipts: true,
    bhop_rooms: true,
    loadouts: true,
    community_maps: false,
    source_map_rules: crate::catalog::SOURCE_MAP_RULES,
};

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum Request {
    Probe {
        version: u16,
    },
    Join {
        version: u16,
        name: String,
        #[serde(flatten)]
        capabilities: RoomCapabilities,
    },
    TicketJoin {
        version: u16,
        ticket: String,
        #[serde(flatten)]
        capabilities: RoomCapabilities,
    },
    TicketRefresh {
        ticket: String,
    },
    TicketProof([u8; 32]),
    Command(ClientMessage),
    Pose {
        epoch: u64,
        state: PlayerState,
    },
    Heartbeat,
    HeartbeatReceipt([u8; 8]),
}

fn is_false(value: &bool) -> bool {
    !value
}

fn is_zero(value: &u8) -> bool {
    *value == 0
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum Response {
    Event {
        epoch: u64,
        event: ServerEvent,
    },
    JoinChallenge {
        challenge: [u8; 32],
        server_proof: [u8; 32],
    },
    Heartbeat,
    HeartbeatChallenge([u8; 8]),
}

pub(crate) trait RoomWireMessage: DeserializeOwned {
    fn decode_compact_snapshot(_: &[u8]) -> io::Result<Self> {
        Err(invalid_data("unexpected compact roster"))
    }
}

impl RoomWireMessage for Request {}

impl RoomWireMessage for Response {
    fn decode_compact_snapshot(payload: &[u8]) -> io::Result<Self> {
        let (epoch, players) = compact_snapshot::decode(payload)?;
        Ok(Self::Event {
            epoch,
            event: ServerEvent::Players(players),
        })
    }
}

/// A room connection with background I/O. State updates replace the previous
/// unsent update so rendering never queues an outdated stream of poses.
pub struct Client {
    address: String,
    name: String,
    managed: bool,
    commands: SyncSender<ClientMessage>,
    events: Mutex<Receiver<(u64, ServerEvent)>>,
    shared: Arc<ClientState>,
}

#[derive(Default)]
struct ClientState {
    stop: AtomicBool,
    observed_epoch: AtomicU64,
    pose: Mutex<Option<(u64, PlayerState)>>,
    refresh: Mutex<Option<RoomJoinTicket>>,
    roster: Mutex<Option<(u64, Instant, Vec<PlayerInfo>)>>,
    disconnected: Mutex<Option<String>>,
}

impl Client {
    /// Start a connection. The server's response arrives as a Welcome or
    /// Disconnected event. Neither DNS nor socket I/O runs on the caller.
    pub fn connect(address: &str, name: &str) -> Result<Self, String> {
        Self::connect_inner(address, name, None)
    }

    pub fn connect_with_ticket(address: &str, ticket: RoomJoinTicket) -> Result<Self, String> {
        Self::connect_inner(address, "", Some(ticket))
    }

    fn connect_inner(
        address: &str,
        name: &str,
        ticket: Option<RoomJoinTicket>,
    ) -> Result<Self, String> {
        let address = normalize_address(address)?;
        let name = name.trim().to_owned();
        let managed = ticket.is_some();
        let (commands, outgoing) = mpsc::sync_channel(96);
        let (incoming, events) = mpsc::sync_channel(128);
        let shared = Arc::new(ClientState::default());
        let worker_state = Arc::clone(&shared);
        let worker_address = address.clone();
        let worker_name = name.clone();
        thread::Builder::new()
            .name("surf-room-client".to_owned())
            .spawn(move || {
                let result = client_worker(
                    &worker_address,
                    worker_name,
                    ticket,
                    outgoing,
                    &incoming,
                    &worker_state,
                );
                if !worker_state.stop.swap(true, Ordering::AcqRel) {
                    let reason = match result {
                        Ok(()) => "The server closed the connection.".to_owned(),
                        Err(error) => format!("Room connection ended: {error}"),
                    };
                    if let Ok(mut disconnected) = worker_state.disconnected.lock() {
                        *disconnected = Some(reason);
                    }
                }
            })
            .map_err(|error| format!("Could not start the room connection: {error}"))?;
        Ok(Self {
            address,
            name,
            managed,
            commands,
            events: Mutex::new(events),
            shared,
        })
    }

    pub fn send(&self, message: ClientMessage) -> Result<(), String> {
        if self.shared.stop.load(Ordering::Acquire) {
            return Err("The room connection has ended.".to_owned());
        }
        if let ClientMessage::State(state) = message {
            *self
                .shared
                .pose
                .lock()
                .map_err(|_| "The room connection has ended.".to_owned())? =
                Some((self.shared.observed_epoch.load(Ordering::Acquire), state));
            return Ok(());
        }
        self.commands
            .try_send(message)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "The room connection is busy.".to_owned(),
                mpsc::TrySendError::Disconnected(_) => "The room connection has ended.".to_owned(),
            })
    }

    pub fn refresh_identity(&self, ticket: RoomJoinTicket) -> Result<(), String> {
        if !self.managed || self.shared.stop.load(Ordering::Acquire) {
            return Err("There is no active managed room connection.".into());
        }
        *self
            .shared
            .refresh
            .lock()
            .map_err(|_| "The room connection has ended.")? = Some(ticket);
        Ok(())
    }

    pub fn poll(&self) -> Option<ServerEvent> {
        self.poll_with_arrival().map(|(event, _)| event)
    }

    pub fn poll_with_arrival(&self) -> Option<(ServerEvent, Option<Instant>)> {
        if let Ok((epoch, event)) = self.events.lock().ok()?.try_recv() {
            if matches!(
                event,
                ServerEvent::Welcome { .. } | ServerEvent::MapChanged { .. }
            ) {
                self.shared.observed_epoch.store(epoch, Ordering::Release);
            }
            return Some((event, None));
        }
        if let Some((epoch, arrival, players)) = self.shared.roster.lock().ok()?.take()
            && epoch == self.shared.observed_epoch.load(Ordering::Acquire)
        {
            return Some((ServerEvent::Players(players), Some(arrival)));
        }
        self.shared
            .disconnected
            .lock()
            .ok()?
            .take()
            .map(|reason| (ServerEvent::Disconnected(reason), None))
    }

    pub fn reconnect(&self) -> Result<Self, String> {
        if self.managed {
            return Err("Request a fresh room ticket to reconnect.".into());
        }
        self.shared.stop.store(true, Ordering::Release);
        Self::connect(&self.address, &self.name)
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

/// Query a configured endpoint without occupying a player slot.
pub fn discover(address: &str) -> Result<RoomInfo, String> {
    discover_with_latency(address).map(|(room, _)| room)
}

pub fn discover_with_latency(address: &str) -> Result<(RoomInfo, Duration), String> {
    let address = normalize_address(address)?;
    let started = Instant::now();
    let response = connect_socket(&address)
        .and_then(probe_room)
        .map_err(|error| format!("Could not query {address}: {error}"))?;
    match response {
        Response::Event {
            event: ServerEvent::RoomInfo(room),
            ..
        } => Ok((room, started.elapsed())),
        Response::Event {
            event: ServerEvent::Error(error),
            ..
        } => Err(error),
        _ => Err(format!("{address} did not return room information.")),
    }
}

fn probe_room(mut socket: FramedSocket) -> io::Result<Response> {
    socket.queue(encode(&Request::Probe {
        version: PROTOCOL_VERSION,
    })?)?;
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        socket.flush()?;
        let batch = socket.receive::<Response>()?;
        if let Some(response) = batch.messages.into_iter().next() {
            return Ok(response);
        }
        if batch.eof {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "room closed discovery",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "room discovery timed out",
    ))
}

fn connect_socket(address: &str) -> io::Result<FramedSocket> {
    let address = RoomAddress::parse(address).map_err(io::Error::other)?;
    let mut last_error = io::Error::new(io::ErrorKind::NotFound, "server address was not found");
    let mut endpoints = Vec::new();
    for endpoint in address
        .authority
        .to_socket_addrs()?
        .take(MAX_RESOLVED_ADDRESSES)
    {
        if !address.encrypted && !lan_address(endpoint.ip()) {
            last_error = io::Error::new(
                io::ErrorKind::PermissionDenied,
                "public rooms require a tls:// address",
            );
            continue;
        }
        if !endpoints.contains(&endpoint) {
            endpoints.push(endpoint);
        }
    }
    if endpoints.is_empty() {
        return Err(last_error);
    }
    let tls = address
        .encrypted
        .then(|| RoomClientTls::for_hostname(address.host))
        .transpose()?;
    connect_endpoints(endpoints, tls)
}

fn connect_endpoints(
    mut endpoints: Vec<SocketAddr>,
    tls: Option<RoomClientTls>,
) -> io::Result<FramedSocket> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    if let [endpoint] = endpoints.as_slice() {
        return dial_room(*endpoint, tls.as_ref(), deadline, &AtomicBool::new(false));
    }
    // Try the resolver's preferred address first, then give the other family a
    // turn before cycling through the remaining addresses in resolver order.
    if let Some(index) = endpoints
        .iter()
        .position(|endpoint| endpoint.is_ipv6() != endpoints[0].is_ipv6())
    {
        let alternate = endpoints.remove(index);
        endpoints.insert(1, alternate);
    }
    let mut endpoints = endpoints.into_iter().peekable();
    let (send, receive) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let mut active = 0;
    let mut launch_at = Instant::now();
    let mut last_error = room_connect_timeout();
    let mut tls_error = None;
    let result = loop {
        let now = Instant::now();
        if now >= deadline {
            break Err(tls_error.unwrap_or_else(room_connect_timeout));
        }
        if active == 0 || now >= launch_at {
            if let Some(endpoint) = endpoints.next() {
                let send = send.clone();
                let tls = tls.clone();
                let stop = Arc::clone(&stop);
                match thread::Builder::new()
                    .name("surf-room-dial".into())
                    .spawn(move || {
                        let _ = send.send(dial_room(endpoint, tls.as_ref(), deadline, &stop));
                    }) {
                    Ok(_) => active += 1,
                    Err(error) => {
                        last_error = error;
                        continue;
                    }
                }
                launch_at = now + ADDRESS_STAGGER;
            } else if active == 0 {
                break Err(tls_error.unwrap_or(last_error));
            }
        }
        let wake_at = if endpoints.peek().is_some() {
            launch_at.min(deadline)
        } else {
            deadline
        };
        match receive.recv_timeout(wake_at.saturating_duration_since(Instant::now())) {
            Ok(Ok(socket)) => break Ok(socket),
            Ok(Err(error)) => {
                active -= 1;
                if error.kind() == io::ErrorKind::InvalidData {
                    tls_error = Some(error);
                } else {
                    last_error = error;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err(tls_error.unwrap_or(last_error)),
        }
    };
    stop.store(true, Ordering::Release);
    result
}

fn dial_room(
    endpoint: SocketAddr,
    tls: Option<&RoomClientTls>,
    deadline: Instant,
    stop: &AtomicBool,
) -> io::Result<FramedSocket> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() || stop.load(Ordering::Acquire) {
        return Err(room_connect_timeout());
    }
    let stream = TcpStream::connect_timeout(&endpoint, remaining)?;
    if stop.load(Ordering::Acquire) {
        return Err(room_connect_timeout());
    }
    let Some(tls) = tls else {
        return FramedSocket::new(stream);
    };
    let mut transport = tls.connect(stream)?;
    while Instant::now() < deadline && !stop.load(Ordering::Acquire) {
        if transport.handshake_ready()? {
            return Ok(FramedSocket::from_transport(transport));
        }
        thread::sleep(POLL_INTERVAL);
    }
    Err(room_connect_timeout())
}

fn room_connect_timeout() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "room connection timed out")
}

fn client_worker(
    address: &str,
    name: String,
    mut ticket: Option<RoomJoinTicket>,
    outgoing: Receiver<ClientMessage>,
    incoming: &SyncSender<(u64, ServerEvent)>,
    shared: &ClientState,
) -> io::Result<()> {
    let mut socket = connect_socket(address)?;
    socket.accept_compact_snapshots();
    let capabilities = RoomCapabilities {
        community_maps: crate::catalog::community_catalog()
            .is_some_and(|catalog| !catalog.maps.is_empty()),
        ..CLIENT_CAPABILITIES
    };
    let join = if let Some(ticket) = &ticket {
        Request::TicketJoin {
            version: PROTOCOL_VERSION,
            ticket: ticket.token().into(),
            capabilities,
        }
    } else {
        Request::Join {
            version: PROTOCOL_VERSION,
            name,
            capabilities,
        }
    };
    socket.queue(encode(&join)?)?;
    let mut joined = false;
    let mut rejected = None;
    let started = Instant::now();
    let mut last_server_frame = started;
    let mut next_heartbeat = last_server_frame + HEARTBEAT_INTERVAL;
    let mut next_state = last_server_frame;
    while !shared.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if joined {
            for _ in 0..FRAME_BATCH {
                match outgoing.try_recv() {
                    Ok(message) => socket.queue(encode(&Request::Command(message))?)?,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }
            if ticket.is_none()
                && let Some(refresh) = shared.refresh.lock().unwrap().take()
            {
                socket.queue(encode(&Request::TicketRefresh {
                    ticket: refresh.token().into(),
                })?)?;
                ticket = Some(refresh);
            }
        }
        if joined && now >= next_state {
            next_state = now + STATE_INTERVAL;
            if let Some((epoch, state)) = shared.pose.lock().unwrap().take() {
                socket.queue(encode(&Request::Pose { epoch, state })?)?;
            }
        }
        if joined && now >= next_heartbeat {
            socket.queue(encode(&Request::Heartbeat)?)?;
            next_heartbeat = now + HEARTBEAT_INTERVAL;
        }
        socket.flush()?;

        let batch = socket.receive::<Response>()?;
        if !batch.messages.is_empty() {
            last_server_frame = now;
        }
        for response in batch.messages {
            match response {
                Response::Event {
                    epoch,
                    event: ServerEvent::Players(players),
                } => {
                    *shared.roster.lock().unwrap() = Some((epoch, now, players));
                }
                Response::Event { epoch, event } => {
                    if let ServerEvent::Disconnected(reason) = event {
                        shared.roster.lock().unwrap().take();
                        shared.stop.store(true, Ordering::Release);
                        *shared.disconnected.lock().unwrap() = Some(reason);
                        return Ok(());
                    }
                    if matches!(event, ServerEvent::Welcome { .. }) {
                        if ticket.is_some() {
                            return Err(invalid_data(
                                "the managed room did not prove its identity",
                            ));
                        }
                        joined = true;
                    }
                    if !joined && let ServerEvent::Error(reason) = &event {
                        rejected = Some(reason.clone());
                    }
                    if matches!(event, ServerEvent::MapChanged { .. }) {
                        shared.roster.lock().unwrap().take();
                        shared.pose.lock().unwrap().take();
                    }
                    incoming.try_send((epoch, event)).map_err(|_| {
                        io::Error::new(io::ErrorKind::BrokenPipe, "room events are not being read")
                    })?;
                }
                Response::JoinChallenge {
                    challenge,
                    server_proof,
                } => {
                    let ticket = ticket
                        .take()
                        .ok_or_else(|| invalid_data("unexpected room challenge"))?;
                    let proof = ticket.respond(&challenge, &server_proof).ok_or_else(|| {
                        invalid_data("the managed room could not prove its identity")
                    })?;
                    socket.queue(encode(&Request::TicketProof(proof))?)?;
                }
                Response::Heartbeat => {}
                Response::HeartbeatChallenge(challenge) if joined => {
                    socket.queue(encode(&Request::HeartbeatReceipt(challenge))?)?;
                }
                Response::HeartbeatChallenge(_) => {
                    return Err(invalid_data("unexpected room heartbeat challenge"));
                }
            }
        }
        if batch.eof {
            return rejected.map_or(Ok(()), |reason| Err(io::Error::other(reason)));
        }
        if !joined && now.duration_since(started) > CONNECT_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the room join timed out",
            ));
        }
        if now.duration_since(last_server_frame) > HEARTBEAT_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the room stopped responding",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

pub(crate) fn encode(value: &impl Serialize) -> io::Result<Arc<[u8]>> {
    let payload = serde_json::to_vec(value).map_err(io::Error::other)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(invalid_data("room message exceeds the wire limit"));
    }
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame.into())
}

fn decode<T: DeserializeOwned>(payload: &[u8]) -> io::Result<T> {
    serde_json::from_slice(payload).map_err(|_| invalid_data("invalid room message"))
}

fn invalid_data(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

pub(crate) struct ReadBatch<T> {
    pub messages: Vec<T>,
    pub eof: bool,
    #[cfg(target_os = "linux")]
    pub drained: bool,
}

struct QueuedFrame {
    bytes: Arc<[u8]>,
    written: usize,
    snapshot: bool,
}

pub(crate) struct FramedSocket {
    stream: Transport,
    input: Vec<u8>,
    snapshot_decoder: Option<SnapshotDecoder>,
    accept_compact_snapshots: bool,
    output: VecDeque<QueuedFrame>,
    active: Option<QueuedFrame>,
    latest_snapshot: Option<QueuedFrame>,
    pending: usize,
    closing: bool,
}

impl FramedSocket {
    #[cfg(target_os = "linux")]
    pub fn poll_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.stream.poll_fd()
    }

    #[cfg(target_os = "linux")]
    pub fn transport_read_pending(&self) -> bool {
        self.stream.read_pending()
    }

    pub fn new(stream: TcpStream) -> io::Result<Self> {
        Ok(Self::from_transport(Transport::plain(stream)?))
    }

    pub fn encrypted_server(stream: TcpStream, config: &RoomTlsConfig) -> io::Result<Self> {
        Ok(Self::from_transport(Transport::encrypted_server(
            stream, config,
        )?))
    }

    fn from_transport(stream: Transport) -> Self {
        Self {
            stream,
            input: Vec::new(),
            snapshot_decoder: None,
            accept_compact_snapshots: false,
            output: VecDeque::new(),
            active: None,
            latest_snapshot: None,
            pending: 0,
            closing: false,
        }
    }

    fn accept_compressed_snapshots(&mut self) {
        self.snapshot_decoder = Some(SnapshotDecoder::default());
    }

    pub(crate) fn accept_compact_snapshots(&mut self) {
        self.accept_compressed_snapshots();
        self.accept_compact_snapshots = true;
    }

    pub fn queue(&mut self, bytes: Arc<[u8]>) -> io::Result<()> {
        if self.pending + bytes.len() > MAX_PENDING_BYTES {
            self.discard_latest_snapshot();
        }
        if self.pending + bytes.len() > MAX_PENDING_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "room connection cannot keep up",
            ));
        }
        self.pending += bytes.len();
        self.output.push_back(QueuedFrame {
            bytes,
            written: 0,
            snapshot: false,
        });
        Ok(())
    }

    pub fn queue_snapshot(&mut self, bytes: Arc<[u8]>) {
        self.discard_latest_snapshot();
        if self.pending + bytes.len() <= MAX_PENDING_BYTES {
            self.pending += bytes.len();
            self.latest_snapshot = Some(QueuedFrame {
                bytes,
                written: 0,
                snapshot: true,
            });
        }
    }

    pub fn discard_latest_snapshot(&mut self) {
        if let Some(snapshot) = self.latest_snapshot.take() {
            self.pending -= snapshot.bytes.len();
        }
    }

    pub fn drained(&self) -> bool {
        self.pending == 0 && self.stream.drained()
    }

    pub fn written_bytes(&self) -> u64 {
        self.stream.written_bytes()
    }

    pub fn shutdown(&mut self) -> io::Result<bool> {
        if !self.closing {
            self.stream.close_notify();
            self.closing = true;
        }
        self.flush()?;
        Ok(self.drained())
    }

    pub fn drain_shutdown(&mut self) -> io::Result<bool> {
        self.flush()?;
        if !self.drained() || !self.shutdown()? {
            return Ok(false);
        }
        self.stream.drain_shutdown()
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.flush_transport()?;
        for _ in 0..FRAME_BATCH {
            if !self.stream.drained() {
                break;
            }
            let Some(mut frame) = self
                .active
                .take()
                .or_else(|| self.output.pop_front())
                .or_else(|| self.latest_snapshot.take())
            else {
                break;
            };
            match self.stream.write(&frame.bytes[frame.written..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => {
                    frame.written += written;
                    self.pending -= written;
                    if frame.written < frame.bytes.len() {
                        self.active = Some(frame);
                    }
                    self.flush_transport()?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    if frame.snapshot && frame.written == 0 {
                        self.latest_snapshot = Some(frame);
                    } else {
                        self.active = Some(frame);
                    }
                    if error.kind() == io::ErrorKind::WouldBlock {
                        break;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn flush_transport(&mut self) -> io::Result<()> {
        match self.stream.flush() {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(())
            }
            result => result,
        }
    }

    pub fn receive<T: RoomWireMessage>(&mut self) -> io::Result<ReadBatch<T>> {
        let mut messages = Vec::new();
        let mut eof = false;
        #[cfg(target_os = "linux")]
        let mut drained = false;
        let mut scratch = [0; 4096];
        for read_count in 0..=8 {
            let mut consumed = 0;
            while self.input.len() - consumed >= 4 && messages.len() < FRAME_BATCH {
                let header =
                    u32::from_be_bytes(self.input[consumed..consumed + 4].try_into().unwrap());
                let compressed = header & COMPRESSED_FRAME != 0;
                let compact = header & COMPACT_FRAME != 0;
                let size = (header & !(COMPRESSED_FRAME | COMPACT_FRAME)) as usize;
                if !(1..=MAX_FRAME_BYTES).contains(&size) {
                    return Err(invalid_data("invalid room message length"));
                }
                if compressed && self.snapshot_decoder.is_none() {
                    return Err(invalid_data("unexpected compressed room message"));
                }
                if compact && !self.accept_compact_snapshots {
                    return Err(invalid_data("unexpected compact roster"));
                }
                let end = consumed + 4 + size;
                if self.input.len() < end {
                    break;
                }
                let mut payload = &self.input[consumed + 4..end];
                if compressed {
                    payload = self.snapshot_decoder.as_mut().unwrap().decode(payload)?;
                }
                messages.push(if compact {
                    T::decode_compact_snapshot(payload)?
                } else {
                    decode(payload)?
                });
                consumed = end;
            }
            self.input.drain(..consumed);
            if messages.len() == FRAME_BATCH || read_count == 8 {
                break;
            }
            match self.stream.read(&mut scratch) {
                Ok(0) => {
                    eof = true;
                    if !self.input.is_empty() {
                        return Err(invalid_data("incomplete room message"));
                    }
                    break;
                }
                Ok(read) => self.input.extend_from_slice(&scratch[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    #[cfg(target_os = "linux")]
                    {
                        drained = true;
                    }
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error)
                    if error.kind() == io::ErrorKind::UnexpectedEof && self.input.is_empty() =>
                {
                    eof = true;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(ReadBatch {
            messages,
            eof,
            #[cfg(target_os = "linux")]
            drained,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener};

    #[test]
    fn room_clocks_keep_legacy_wire_payloads_readable() {
        let room: RoomInfo =
            serde_json::from_str(r#"{"name":"LAN","map":"downline","players":1,"capacity":8}"#)
                .unwrap();
        let vote: VoteInfo = serde_json::from_str(
            r#"{"maps":[],"nominations":[],"votes":{},"needed":1,"open":false,"seconds_left":0,"rtv":0}"#,
        ).unwrap();
        assert_eq!(room.map_remaining_ms, None);
        assert_eq!(vote.map_remaining_ms, None);
        assert_eq!(vote.vote_remaining_ms, None);
        assert_eq!(vote.vote_id, None);
        assert_eq!(vote.selected, None);
        assert!(!vote.runoff);
        assert_eq!(map_time_label(Duration::from_millis(59_001)), "60s left");
        assert_eq!(map_time_label(Duration::from_millis(60_001)), "2 min left");
        assert_eq!(map_time_label(Duration::ZERO), "Changing map…");
    }

    #[test]
    fn room_capabilities_opt_in_without_changing_legacy_joins() {
        #[derive(Deserialize)]
        #[serde(tag = "type", content = "data", rename_all = "snake_case")]
        enum LegacyJoin {
            Join { version: u16, name: String },
            TicketJoin { version: u16, ticket: String },
        }

        let join = encode(&Request::Join {
            version: PROTOCOL_VERSION,
            name: "Player".to_owned(),
            capabilities: CLIENT_CAPABILITIES,
        })
        .unwrap();
        assert!(matches!(
            decode(&join[4..]).unwrap(),
            LegacyJoin::Join { version: PROTOCOL_VERSION, name } if name == "Player"
        ));
        let ticket = encode(&Request::TicketJoin {
            version: PROTOCOL_VERSION,
            ticket: "ticket".to_owned(),
            capabilities: CLIENT_CAPABILITIES,
        })
        .unwrap();
        assert!(matches!(
            decode(&ticket[4..]).unwrap(),
            LegacyJoin::TicketJoin { version: PROTOCOL_VERSION, ticket } if ticket == "ticket"
        ));
        assert!(matches!(
            decode(br#"{"type":"join","data":{"version":1,"name":"Player"}}"#).unwrap(),
            Request::Join {
                capabilities: RoomCapabilities {
                    compressed_snapshots: false,
                    compact_snapshots: false,
                    heartbeat_receipts: false,
                    bhop_rooms: false,
                    loadouts: false,
                    community_maps: false,
                    source_map_rules: 0,
                },
                ..
            }
        ));
        assert!(matches!(
            decode(br#"{"type":"ticket_join","data":{"version":1,"ticket":"ticket"}}"#).unwrap(),
            Request::TicketJoin {
                capabilities: RoomCapabilities {
                    compressed_snapshots: false,
                    compact_snapshots: false,
                    heartbeat_receipts: false,
                    bhop_rooms: false,
                    loadouts: false,
                    community_maps: false,
                    source_map_rules: 0,
                },
                ..
            }
        ));
        assert!(matches!(
            decode(br#"{"type":"join","data":{"version":1,"name":"Player","compressed_snapshots":true}}"#).unwrap(),
            Request::Join {
                capabilities: RoomCapabilities { compressed_snapshots: true, compact_snapshots: false, heartbeat_receipts: false, bhop_rooms: false, loadouts: false, community_maps: false, source_map_rules: 0 },
                ..
            }
        ));
        assert!(matches!(
            decode(
                br#"{"type":"join","data":{"version":1,"name":"Player","community_maps":true}}"#
            )
            .unwrap(),
            Request::Join {
                capabilities: RoomCapabilities {
                    community_maps: true,
                    source_map_rules: 0,
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            decode(
                br#"{"type":"join","data":{"version":1,"name":"Player","community_maps":true,"source_map_rules":1}}"#
            )
            .unwrap(),
            Request::Join {
                capabilities: RoomCapabilities {
                    community_maps: true,
                    source_map_rules: 1,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn native_client_acknowledges_the_rooms_heartbeat_challenge() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = FramedSocket::new(stream).unwrap();
            let next_request = |socket: &mut FramedSocket| {
                let deadline = Instant::now() + CONNECT_TIMEOUT;
                loop {
                    socket.flush().unwrap();
                    let batch = socket.receive::<Request>().unwrap();
                    if let Some(request) = batch.messages.into_iter().next() {
                        return request;
                    }
                    assert!(!batch.eof && Instant::now() < deadline);
                    thread::sleep(POLL_INTERVAL);
                }
            };
            assert!(matches!(
                next_request(&mut socket),
                Request::Join {
                    capabilities: RoomCapabilities {
                        heartbeat_receipts: true,
                        source_map_rules: crate::catalog::SOURCE_MAP_RULES,
                        ..
                    },
                    ..
                }
            ));
            socket
                .queue(
                    encode(&Response::Event {
                        epoch: 1,
                        event: ServerEvent::Welcome {
                            id: 1,
                            room: RoomInfo {
                                name: "Test room".into(),
                                map: "downline".into(),
                                mode: MapMode::Surf,
                                players: 1,
                                capacity: 1,
                                room_id: None,
                                map_remaining_ms: None,
                            },
                        },
                    })
                    .unwrap(),
                )
                .unwrap();
            let challenge = [42; 8];
            socket
                .queue(encode(&Response::HeartbeatChallenge(challenge)).unwrap())
                .unwrap();
            assert!(matches!(
                next_request(&mut socket),
                Request::HeartbeatReceipt(receipt) if receipt == challenge
            ));
        });
        let _client = Client::connect(&address, "Reader").unwrap();
        server.join().unwrap();
    }

    #[test]
    fn native_client_preserves_a_single_room_shutdown_reason() {
        use crate::server::{RoomConfig, RoomServer};

        let server = RoomServer::bind(RoomConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            ..RoomConfig::default()
        })
        .unwrap();
        let client = Client::connect(&server.local_addr().to_string(), "Reader").unwrap();
        let wait_for = |accept: fn(&ServerEvent) -> bool| {
            let deadline = Instant::now() + CONNECT_TIMEOUT;
            loop {
                if let Some(event) = client.poll()
                    && accept(&event)
                {
                    return event;
                }
                assert!(Instant::now() < deadline, "room event missing");
                thread::sleep(POLL_INTERVAL);
            }
        };
        wait_for(|event| matches!(event, ServerEvent::Welcome { .. }));
        server.stop().unwrap();
        assert!(matches!(
            wait_for(|event| matches!(event, ServerEvent::Disconnected(_))),
            ServerEvent::Disconnected(reason) if reason == "The room is shutting down."
        ));
        assert!(matches!(
            client.events.lock().unwrap().recv_timeout(CONNECT_TIMEOUT),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        assert!(client.poll().is_none());
    }

    #[test]
    fn compressed_room_frames_are_rejected_without_opt_in() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (receiver, _) = listener.accept().unwrap();
        let mut socket = FramedSocket::new(receiver).unwrap();
        sender
            .write_all(&(COMPRESSED_FRAME | 1).to_be_bytes())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match socket.receive::<Request>() {
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                    break;
                }
                Ok(_) => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }
    }

    #[test]
    fn reliable_room_events_are_flushed_before_the_latest_unsent_roster() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut reader =
            FramedSocket::new(TcpStream::connect(listener.local_addr().unwrap()).unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut writer = FramedSocket::new(stream).unwrap();
        reader.accept_compressed_snapshots();
        let roster: Vec<_> = (0..24)
            .map(|id| PlayerInfo {
                id,
                name: format!("Room player {id}"),
                state: None,
                spectating: None,
                identity: None,
                loadout: Loadout::default(),
            })
            .collect();
        let mut compressor = SnapshotEncoder::default();
        for epoch in 1..=4 {
            let plain = encode(&Response::Event {
                epoch,
                event: ServerEvent::Players(roster.clone()),
            })
            .unwrap();
            writer.queue_snapshot(compressor.encode(&plain).unwrap());
        }
        writer
            .queue(
                encode(&Response::Event {
                    epoch: 4,
                    event: ServerEvent::MapChanged {
                        map: "surf_boreas".into(),
                    },
                })
                .unwrap(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut messages = Vec::new();
        while messages.len() < 2 {
            assert!(Instant::now() < deadline);
            writer.flush().unwrap();
            messages.extend(reader.receive::<Response>().unwrap().messages);
            thread::sleep(POLL_INTERVAL);
        }
        assert!(writer.drained());
        assert!(matches!(
            messages.as_slice(),
            [
                Response::Event {
                    epoch: 4,
                    event: ServerEvent::MapChanged { .. }
                },
                Response::Event {
                    epoch: 4,
                    event: ServerEvent::Players(players)
                },
            ] if players == &roster
        ));
    }

    #[test]
    fn tls_rooms_require_a_trusted_certificate_with_the_requested_hostname() {
        use crate::server::{RoomConfig, RoomServer};
        use rustls::{
            RootCertStore,
            pki_types::{PrivatePkcs8KeyDer, ServerName},
        };

        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let certificate = cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(signing_key.serialize_der());
        let tls = RoomTlsConfig::from_der(vec![certificate.clone()], key.into()).unwrap();
        let ticket_key =
            RoomTicketKey::from_hex("tls-test", &"0123456789abcdef".repeat(4)).unwrap();
        let server = RoomServer::bind(RoomConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            tls: Some(tls),
            managed: Some(ticket_key.clone()),
            ..RoomConfig::default()
        })
        .unwrap();
        let transport = |host, roots| {
            let stream = TcpStream::connect(server.local_addr()).unwrap();
            let name = ServerName::try_from(host).unwrap();
            let tls = RoomClientTls::with_roots(name, roots).unwrap();
            let transport = tls.connect(stream).unwrap();
            FramedSocket::from_transport(transport)
        };
        let attempt = |host, roots| probe_room(transport(host, roots));
        let mut trusted = RootCertStore::empty();
        trusted.add(certificate).unwrap();
        assert!(matches!(
            attempt("localhost", trusted.clone()),
            Ok(Response::Event {
                event: ServerEvent::RoomInfo(_),
                ..
            })
        ));
        assert_eq!(
            attempt("127.0.0.1", trusted.clone()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            attempt("localhost", RootCertStore::empty())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(discover(&server.local_addr().to_string()).is_err());

        let mut secure = transport("localhost", trusted);
        secure.accept_compact_snapshots();
        let ticket = ticket_key.issue(None, ticket_time()).unwrap();
        secure
            .queue(
                encode(&Request::TicketJoin {
                    version: PROTOCOL_VERSION,
                    ticket: ticket.token().into(),
                    capabilities: CLIENT_CAPABILITIES,
                })
                .unwrap(),
            )
            .unwrap();
        let wait_for = |socket: &mut FramedSocket, predicate: fn(&Response) -> bool| {
            let deadline = Instant::now() + CONNECT_TIMEOUT;
            loop {
                socket.flush().unwrap();
                let batch = socket.receive::<Response>().unwrap();
                if let Some(response) = batch.messages.into_iter().find(predicate) {
                    return response;
                }
                assert!(
                    !batch.eof && Instant::now() < deadline,
                    "secure room response missing"
                );
                thread::sleep(POLL_INTERVAL);
            }
        };
        let Response::JoinChallenge {
            challenge,
            server_proof,
        } = wait_for(&mut secure, |response| {
            matches!(response, Response::JoinChallenge { .. })
        })
        else {
            unreachable!()
        };
        let proof = ticket.respond(&challenge, &server_proof).unwrap();
        secure
            .queue(encode(&Request::TicketProof(proof)).unwrap())
            .unwrap();
        wait_for(&mut secure, |response| {
            matches!(
                response,
                Response::Event {
                    event: ServerEvent::Welcome { .. },
                    ..
                }
            )
        });
        secure.queue(encode(&Request::Heartbeat).unwrap()).unwrap();
        let Response::HeartbeatChallenge(challenge) = wait_for(&mut secure, |response| {
            matches!(response, Response::HeartbeatChallenge(_))
        }) else {
            unreachable!()
        };
        secure
            .queue(encode(&Request::HeartbeatReceipt(challenge)).unwrap())
            .unwrap();
        secure.flush().unwrap();

        server.control().shutdown();
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut received_shutdown = false;
        loop {
            let batch = secure.receive::<Response>().unwrap();
            for response in batch.messages {
                if let Response::Event {
                    event: ServerEvent::Disconnected(reason),
                    ..
                } = response
                {
                    assert!(!received_shutdown);
                    assert_eq!(reason, "The room is shutting down.");
                    received_shutdown = true;
                }
            }
            if batch.eof {
                assert!(received_shutdown);
                // A TLS close_notify produces a clean EOF, even when TCP has closed.
                assert_eq!(secure.stream.read(&mut [0]).unwrap(), 0);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the TLS connection did not close"
            );
            thread::sleep(POLL_INTERVAL);
        }
        server.wait().unwrap();
    }

    #[test]
    fn stalled_tls_endpoint_does_not_hide_a_healthy_room_address() {
        use crate::server::{RoomConfig, RoomServer};
        use rustls::{
            RootCertStore,
            pki_types::{PrivatePkcs8KeyDer, ServerName},
        };
        use std::io::Read;

        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let certificate = cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(signing_key.serialize_der());
        let server_tls = RoomTlsConfig::from_der(vec![certificate.clone()], key.into()).unwrap();
        let server = RoomServer::bind(RoomConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            tls: Some(server_tls),
            managed: Some(
                RoomTicketKey::from_hex("alternate", &"0123456789abcdef".repeat(4)).unwrap(),
            ),
            ..RoomConfig::default()
        })
        .unwrap();
        let primary =
            TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, server.local_addr().port()))
                .or_else(|_| TcpListener::bind("127.0.0.1:0"))
                .unwrap();
        let first_address = primary.local_addr().unwrap();
        let stalled = thread::spawn(move || {
            let (mut stream, _) = primary.accept().unwrap();
            stream.set_read_timeout(Some(CONNECT_TIMEOUT)).unwrap();
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).unwrap();
            assert!(count > 0);
            assert_eq!(bytes[0], 22, "TLS ClientHello must precede any room data");
            while stream.read(&mut bytes).unwrap() != 0 {}
        });
        let mut roots = RootCertStore::empty();
        roots.add(certificate).unwrap();
        let tls =
            RoomClientTls::with_roots(ServerName::try_from("localhost").unwrap(), roots).unwrap();
        let started = Instant::now();
        let socket =
            connect_endpoints(vec![first_address, server.local_addr()], Some(tls)).unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed >= ADDRESS_STAGGER && elapsed < Duration::from_secs(2),
            "{elapsed:?}"
        );
        let response = probe_room(socket).unwrap();
        assert!(matches!(
            response,
            Response::Event { event: ServerEvent::RoomInfo(room), .. }
            if room.room_id.as_deref() == Some("alternate")
        ));
        stalled.join().unwrap();
    }

    #[test]
    fn rejects_oversized_wire_frames_before_reading_the_payload() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (receiver, _) = listener.accept().unwrap();
        let mut socket = FramedSocket::new(receiver).unwrap();
        sender
            .write_all(&((MAX_FRAME_BYTES + 1) as u32).to_be_bytes())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match socket.receive::<Request>() {
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                    break;
                }
                Ok(_) => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    #[test]
    fn legacy_room_info_defaults_to_surf() {
        let room: RoomInfo = serde_json::from_str(
            r#"{"name":"Old room","map":"surf_kitsune","players":0,"capacity":16}"#,
        )
        .unwrap();
        assert_eq!(room.mode, MapMode::Surf);
    }
}
