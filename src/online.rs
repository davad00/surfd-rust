use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use bevy::clipboard::{Clipboard, ClipboardRead};
use bevy::input::{ButtonState, keyboard::KeyboardInput, mouse::AccumulatedMouseMotion};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_surf::{
    network::{
        self, Client, ClientMessage, PlayerInfo, PlayerState, RoomIdentity, RoomInfo,
        RoomJoinTicket, RoomModeration, RoomReportPage, ServerEvent, VoteInfo,
    },
    record_protocol::{
        Attempt, AttemptRequest, Board, Credentials, ManagedRoomEntry, OAuthPoll, OAuthProvider,
        OAuthStart, OAuthStartRequest, PLAYER_RUNS_PAGE_SIZE, PlayerRunSort, PlayerRunsPage,
        Playtime, PlaytimePrivacy, Profile, ProfileExtrasUpdate, Progression, SignedIn, StageBests,
        StageSpan,
    },
    reference_ghost::ReferenceGhost,
    replay::RunReplay,
    session::{RunPhase, Session},
    social_protocol::{
        MessageReportPage, MessageReportReason, MessageReportReceipt, MessageReportRequest,
        PlayerActivity, PresenceUpdate, SocialAction, SocialState,
    },
    tutorial,
};

use crate::{
    library::{LoadEvent, LoadJob, LoadedCourse, MapLibrary, MapSource},
    menu::{MenuRequest, MenuState, Page, Settings},
    records_client as api,
    view::Game,
};

mod ranked_attempt;
use ranked_attempt::{AttemptProgress, RankedAttempt};
mod ranked_journal;
use ranked_journal::{PendingRankedSave, RECENT_RANKED_SAVES, RankedJournal};
mod web_history;
use crate::surfd_history::{SourcePlayer, SurfdHistory, fetch_history, fetch_run_splits};
pub use web_history::RunSplits;
use web_history::{RunDetails, WebHistory};

pub const SURFD_HISTORY_PAGE_SIZE: usize = 10;
const MAX_SURFD_PROFILE_INPUT: usize = 220;

const ATTEMPT_RETRY_STATUS: &str = "Waiting for ranked access. Retrying the records service…";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OnlinePage {
    Servers,
    Players,
    Records,
    Profile,
    Friends,
    Store,
    Moderation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Address,
    Name,
    Endpoint,
    Password,
    Chat,
    Nomination,
    Motto,
    Title,
    Banner,
    Showcase,
    FriendName,
    DirectMessage,
    ReportReason,
    SurfdProfile,
}

#[derive(Clone)]
pub enum Action {
    Edit(Field),
    Discover,
    Join,
    JoinAddress(String),
    Leave,
    Chat,
    Nominate,
    Vote(String),
    RockTheVote,
    Extend,
    Spectate(Option<u64>),
    Report(u64),
    RoomChangeMap,
    RoomModerate(u64, RoomModeration),
    ReviewRoomReports(u16),
    CloseRoomReports,
    RefreshBoard,
    StageRecords,
    StageBoard(u16),
    Progression,
    RecordsPage(usize),
    Watch(String),
    WatchStage(String, StageSpan),
    Ghost(String),
    Register,
    SignIn,
    ProviderSignIn(OAuthProvider),
    CancelProviderSignIn,
    SignOut,
    SaveProfile,
    OpenSurfdHistory,
    CloseSurfdHistory,
    PasteSurfdProfile,
    LoadSurfdHistory,
    ForgetSurfdHistory,
    SurfdHistoryPage(isize),
    ShowSurfdScores(bool),
    ShowSurfdRun(String),
    SetPlaytimePrivacy(bool),
    ViewProfile(i64),
    OwnProfile,
    ProfileRuns(PlayerRunSort, u32),
    CloseProfileRuns,
    ProfileReplay(String, bool),
    CancelProfileReplay,
    DeleteAccount,
    ConfirmDelete,
    Submit,
    Social(SocialAction),
    RequestFriend,
    SelectFriend(i64),
    DirectMessage,
    ShowMessageArchive(bool),
    SelectMessageReport(Option<i64>),
    SendMessageReport(MessageReportReason),
    ReviewMessageReports(Option<i64>),
    CloseMessageReports,
    Invite(i64),
}

pub struct ServerRow {
    pub address: String,
    pub availability: RoomAvailability,
}

pub enum RoomAvailability {
    Online { info: RoomInfo, ping_ms: u128 },
    Unavailable { last_name: Option<String> },
}

impl ServerRow {
    fn known_name(&self) -> Option<&str> {
        match &self.availability {
            RoomAvailability::Online { info, .. } => Some(&info.name),
            RoomAvailability::Unavailable { last_name } => last_name.as_deref(),
        }
    }

    fn has_space(&self) -> bool {
        matches!(&self.availability, RoomAvailability::Online { info, .. } if info.players < info.capacity)
    }
}

enum Reply {
    Discovered {
        generation: u64,
        directory: Option<Vec<String>>,
        servers: Vec<ServerRow>,
    },
    Connected(u64, Result<Client, String>),
    RoomRefresh(u64, Result<RoomJoinTicket, api::RetryableRequestFailure>),
    SignedIn(Result<SignedIn, String>),
    ProviderSignedIn(Arc<AtomicBool>, Result<SignedIn, String>),
    SignedOut(bool, Result<bool, String>),
    Profile(Result<Profile, String>),
    SurfdHistory(Result<SurfdHistory, String>),
    SurfdRunSplits(String, String, Result<Vec<u64>, String>),
    PublicProfile(u64, Result<(Profile, Option<Playtime>), String>),
    ProfileRuns(u64, Result<PlayerRunsPage, String>),
    ProfileReplay(u64, u64, bool, Result<RunReplay, String>),
    Playtime(i64, Result<Playtime, String>),
    Presence(i64, Result<Playtime, String>),
    Attempt(u64, u8, Result<Attempt, api::RetryableRequestFailure>),
    Board(Result<Board, String>),
    Progression(Result<Progression, String>),
    Stages(Result<(StageBests, Option<StageBests>), String>),
    Replay(bool, Option<StageSpan>, Result<RunReplay, String>),
    ReferenceGhost {
        course_version: u64,
        bonus: Option<u16>,
        watch: bool,
        result: Result<ReferenceGhost, String>,
    },
    Submitted(
        String,
        u64,
        Result<api::RankedSaveReceipt, api::RankedSaveFailure>,
    ),
    RankedJournalLoaded(Result<Vec<PendingRankedSave>, String>),
    RankedProfile(Result<Profile, String>),
    Social(Result<SocialState, String>),
    MessageReported(Result<MessageReportReceipt, String>),
    MessageReports(Result<MessageReportPage, String>),
}

impl Reply {
    fn uses_account_scope(&self) -> bool {
        // Room connections and ranked tickets have their own lifetime checks.
        !matches!(
            self,
            Self::Discovered { .. }
                | Self::Connected(..)
                | Self::Attempt(..)
                | Self::Submitted(..)
                | Self::RankedJournalLoaded(..)
                | Self::Replay(..)
                | Self::ReferenceGhost { .. }
        )
    }
}

struct PendingReply {
    account_generation: u64,
    records_origin: String,
    owner: Option<i64>,
    reply: Reply,
}

pub struct ProfileRunsView {
    pub sort: PlayerRunSort,
    pub page: u32,
    pub result: Option<Result<PlayerRunsPage, String>>,
}

struct ProfileReplayLoad {
    epoch: u64,
    replay: RunReplay,
    ghost: bool,
    job: LoadJob,
}

struct RankedSave {
    id: String,
    epoch: Option<u64>,
    owner: i64,
    origin: String,
    payload: Option<Arc<PendingRankedSave>>,
    attempt: Option<String>,
    state: RankedSaveState,
}

impl RankedSave {
    fn queued(payload: PendingRankedSave, epoch: Option<u64>, attempt: Option<String>) -> Self {
        Self {
            id: payload.save_id.clone(),
            epoch,
            owner: payload.owner,
            origin: payload.origin.clone(),
            payload: Some(Arc::new(payload)),
            attempt,
            state: RankedSaveState::Queued,
        }
    }

    fn belongs_to(&self, owner: i64, origin: &str) -> bool {
        self.owner == owner && self.origin == origin
    }
}

enum RankedSaveState {
    Queued,
    InFlight(u64),
    Failed {
        generation: u64,
        error: api::RankedSaveFailure,
        retry_at: Instant,
        reported: bool,
    },
    Saved {
        generation: u64,
        receipt: api::RankedSaveReceipt,
        reported: bool,
    },
}

const ROOM_REFRESH_INTERVAL: Duration = Duration::from_secs(120);

#[derive(Default)]
struct RoomRefreshSchedule {
    next: Option<Instant>,
    pending: bool,
    failures: u8,
}

impl RoomRefreshSchedule {
    fn renewed(&mut self, now: Instant) {
        *self = Self {
            next: Some(now + ROOM_REFRESH_INTERVAL),
            ..Self::default()
        };
    }

    fn expedite(&mut self, now: Instant) {
        if !self.pending && self.failures == 0 && self.next.is_some() {
            self.next = Some(now);
        }
    }

    fn due(&self, now: Instant) -> bool {
        !self.pending && self.next.is_some_and(|next| now >= next)
    }

    fn failed(&mut self, now: Instant, retry_after: Option<Duration>) {
        self.pending = false;
        self.next = retry_after.and_then(|delay| {
            self.failures = (self.failures + 1).min(5);
            let backoff = Duration::from_secs((1 << self.failures).min(20));
            now.checked_add(backoff.max(delay))
        });
    }
}

const MAX_QUICK_PLAY_ROOMS: usize = 4;
const SERVER_BROWSER_REFRESH: Duration = Duration::from_secs(20);
const MAX_PARALLEL_ROOM_PROBES: usize = 8;
const MAX_DIRECTORY_ROOMS: usize = 32;

#[derive(Default)]
enum QuickPlay {
    #[default]
    Idle,
    Discovering,
    Joining(VecDeque<String>),
}

#[derive(Resource)]
pub struct Online {
    client: Option<Mutex<Client>>,
    connection_generation: u64,
    joining: bool,
    room_refresh: RoomRefreshSchedule,
    pub room: Option<RoomInfo>,
    pub id: Option<u64>,
    pub players: Vec<PlayerInfo>,
    remote_poses: bevy_surf::remote_pose::RoomPoses,
    pose_clock: Instant,
    pose_epoch: u32,
    last_sent_pose: Option<(u64, u32)>,
    pub room_reports: Option<RoomReportPage>,
    pub vote: Option<VoteInfo>,
    pub spectating: Option<u64>,
    pub messages: VecDeque<String>,
    pub servers: Vec<ServerRow>,
    pub address: String,
    pub name: String,
    pub endpoint: String,
    pub status: String,
    pub record_status: String,
    pub board: Option<Board>,
    pub stage_records: Option<StageBests>,
    pub stage_bests: Option<StageBests>,
    pub show_stage_records: bool,
    pub show_progression: bool,
    pub progression: Option<Progression>,
    pub board_page: usize,
    pub profile: Option<Profile>,
    pub web_history: WebHistory,
    pub viewed_profile: Option<(Profile, Option<Playtime>)>,
    pub profile_runs: Option<ProfileRunsView>,
    pub profile_replay_confirm: Option<(String, bool)>,
    pub profile_replay_busy: bool,
    profile_replay_load: Option<ProfileReplayLoad>,
    profile_runs_generation: u64,
    pub playtime: Option<Playtime>,
    pub confirm_delete: bool,
    pub social: SocialState,
    pub selected_friend: Option<i64>,
    pub show_message_archive: bool,
    pub reporting_message: Option<i64>,
    pub message_reports: Option<MessageReportPage>,
    pub message_report_status: String,
    pub message_report_busy: bool,
    pub field: Option<Field>,
    consumed_input: bool,
    password: String,
    chat: String,
    nomination: String,
    motto: String,
    title: String,
    banner: String,
    showcase: String,
    friend_name: String,
    direct_message: String,
    report_reason: String,
    token: Option<String>,
    account_generation: u64,
    records_origin: String,
    actions: Vec<Action>,
    receiver: Mutex<mpsc::Receiver<PendingReply>>,
    sender: mpsc::Sender<PendingReply>,
    attempt: Option<RankedAttempt>,
    ranked_saves: VecDeque<RankedSave>,
    ranked_journal: Option<RankedJournal>,
    last_state: Instant,
    desired_map: Option<String>,
    quick_play: QuickPlay,
    discovering: bool,
    discovery_generation: u64,
    next_discovery: Option<Instant>,
    announce_discovery: bool,
    configured_servers: Vec<String>,
    records_directory: Vec<String>,
    account_busy: bool,
    provider_sign_in: Option<Arc<AtomicBool>>,
    board_busy: bool,
    replay_busy: bool,
    social_busy: bool,
    presence_busy: bool,
    presence: Option<(PresenceUpdate, Instant)>,
    last_game_input: Option<Instant>,
    unfocused_since: Option<Instant>,
    presence_clean: bool,
    last_social_fetch: Option<Instant>,
    playtime_busy: bool,
}

impl Default for Online {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        let endpoint = api::configured_endpoint();
        let records_origin = endpoint.trim().trim_end_matches('/').to_owned();
        let configured_servers: Vec<_> = std::env::var("SURF_SERVERS")
            .unwrap_or_default()
            .split(';')
            .map(str::trim)
            .filter(|address| !address.is_empty())
            .take(12)
            .map(str::to_owned)
            .collect();
        Self {
            client: None,
            connection_generation: 0,
            joining: false,
            room_refresh: RoomRefreshSchedule::default(),
            room: None,
            id: None,
            players: Vec::new(),
            remote_poses: bevy_surf::remote_pose::RoomPoses::default(),
            pose_clock: Instant::now(),
            pose_epoch: 0,
            last_sent_pose: None,
            room_reports: None,
            vote: None,
            spectating: None,
            messages: VecDeque::new(),
            servers: Vec::new(),
            address: configured_servers
                .first()
                .cloned()
                .unwrap_or_else(|| network::DEFAULT_ADDRESS.into()),
            name: "Guest".into(),
            endpoint,
            status: "Choose a server or enter its address.".into(),
            record_status: String::new(),
            board: None,
            stage_records: None,
            stage_bests: None,
            show_stage_records: false,
            show_progression: false,
            progression: None,
            board_page: 0,
            profile: None,
            web_history: WebHistory::default(),
            viewed_profile: None,
            profile_runs: None,
            profile_replay_confirm: None,
            profile_replay_busy: false,
            profile_replay_load: None,
            profile_runs_generation: 0,
            playtime: None,
            confirm_delete: false,
            social: SocialState::default(),
            selected_friend: None,
            show_message_archive: false,
            reporting_message: None,
            message_reports: None,
            message_report_status: String::new(),
            message_report_busy: false,
            field: None,
            consumed_input: false,
            password: String::new(),
            chat: String::new(),
            nomination: String::new(),
            motto: String::new(),
            title: String::new(),
            banner: String::new(),
            showcase: String::new(),
            token: None,
            actions: Vec::new(),
            friend_name: String::new(),
            direct_message: String::new(),
            report_reason: String::new(),
            receiver: Mutex::new(receiver),
            sender,
            account_generation: 0,
            records_origin,
            attempt: None,
            ranked_saves: VecDeque::new(),
            ranked_journal: None,
            last_state: Instant::now(),
            desired_map: None,
            quick_play: QuickPlay::Idle,
            discovering: false,
            discovery_generation: 0,
            next_discovery: None,
            announce_discovery: false,
            configured_servers,
            records_directory: Vec::new(),
            account_busy: false,
            provider_sign_in: None,
            board_busy: false,
            replay_busy: false,
            social_busy: false,
            presence_busy: false,
            presence: None,
            last_game_input: None,
            unfocused_since: None,
            presence_clean: true,
            last_social_fetch: None,
            playtime_busy: false,
        }
    }
}

impl Online {
    pub fn is_connected(&self) -> bool {
        self.id.is_some()
    }

    pub fn discovering_servers(&self) -> bool {
        self.discovering
    }
    pub fn is_signed_in(&self) -> bool {
        self.token.is_some()
    }

    pub fn provider_sign_in_pending(&self) -> bool {
        self.provider_sign_in.is_some()
    }

    pub fn room_identity(&self) -> Option<RoomIdentity> {
        self.players
            .iter()
            .find(|player| Some(player.id) == self.id)?
            .identity
    }

    pub fn managed_guest(&self) -> bool {
        self.room
            .as_ref()
            .is_some_and(|room| room.room_id.is_some())
            && self
                .room_identity()
                .and_then(|identity| identity.account_id)
                .is_none()
    }
    pub fn is_typing(&self) -> bool {
        self.field.is_some() || self.consumed_input
    }
    pub fn queue(&mut self, action: Action) {
        self.actions.push(action);
    }

    pub fn field_value(&self, field: Field) -> &str {
        match field {
            Field::Address => &self.address,
            Field::Name => &self.name,
            Field::Endpoint => &self.endpoint,
            Field::Password => &self.password,
            Field::Chat => &self.chat,
            Field::Nomination => &self.nomination,
            Field::Motto => &self.motto,
            Field::Title => &self.title,
            Field::Banner => &self.banner,
            Field::Showcase => &self.showcase,
            Field::FriendName => &self.friend_name,
            Field::DirectMessage => &self.direct_message,
            Field::ReportReason => &self.report_reason,
            Field::SurfdProfile => &self.web_history.input,
        }
    }

    fn field_value_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Address => &mut self.address,
            Field::Name => &mut self.name,
            Field::Endpoint => &mut self.endpoint,
            Field::Password => &mut self.password,
            Field::Chat => &mut self.chat,
            Field::Nomination => &mut self.nomination,
            Field::Motto => &mut self.motto,
            Field::Title => &mut self.title,
            Field::Banner => &mut self.banner,
            Field::Showcase => &mut self.showcase,
            Field::FriendName => &mut self.friend_name,
            Field::DirectMessage => &mut self.direct_message,
            Field::ReportReason => &mut self.report_reason,
            Field::SurfdProfile => &mut self.web_history.input,
        }
    }

    pub fn viewed_player(&self) -> Option<&PlayerState> {
        let id = self.spectating?;
        self.players
            .iter()
            .find(|player| player.id == id)?
            .state
            .as_ref()
    }

    pub fn viewed_pose(&self) -> Option<PlayerState> {
        self.remote_poses
            .get(self.spectating?)
            .or_else(|| self.viewed_player().copied())
    }

    pub fn displayed_pose(&self, id: u64, fallback: PlayerState) -> PlayerState {
        self.remote_poses.get(id).unwrap_or(fallback)
    }

    pub(super) fn accept_roster(&mut self, players: Vec<PlayerInfo>, arrival: Instant) -> bool {
        let changed = self
            .players
            .iter()
            .map(|p| (p.id, p.spectating, p.identity))
            .ne(players.iter().map(|p| (p.id, p.spectating, p.identity)));
        self.spectating = players
            .iter()
            .find(|player| Some(player.id) == self.id)
            .and_then(|player| player.spectating);
        let arrival_ms = arrival
            .saturating_duration_since(self.pose_clock)
            .as_secs_f64()
            * 1000.0;
        self.remote_poses.ingest(&players, self.id, arrival_ms);
        self.players = players;
        if !self
            .room_identity()
            .is_some_and(|identity| identity.moderator)
        {
            self.room_reports = None;
        }
        changed
    }

    pub(super) fn sample_room_poses(&mut self, now: Instant) {
        let now_ms = now.saturating_duration_since(self.pose_clock).as_secs_f64() * 1000.0;
        self.remote_poses.sample(now_ms);
    }

    pub fn show_players(&self, game: &Game) -> bool {
        self.room
            .as_ref()
            .is_some_and(|room| room.map.eq_ignore_ascii_case(&game.course.name))
            && game.pending.is_none()
    }

    pub fn can_submit(&self, game: &Game) -> bool {
        let Some(owner) = self.profile.as_ref().map(|profile| profile.id) else {
            return false;
        };
        self.token.is_some()
            && !self.ranked_saves.iter().any(|save| {
                save.epoch == Some(game.run_epoch)
                    || (save.belongs_to(owner, &self.records_origin)
                        && matches!(save.state, RankedSaveState::InFlight(_)))
            })
            && self.attempt.as_ref().is_some_and(|attempt| {
                attempt.applies_to(
                    game.run_epoch,
                    &self.endpoint,
                    self.profile.as_ref().map(|profile| profile.id),
                ) && attempt.ticket().is_some()
            })
            && game.replay_epoch == Some(game.run_epoch)
            && game.finished_replay().is_some()
    }

    fn notify(&mut self, message: String) {
        self.messages.push_back(message);
        while self.messages.len() > 100 {
            self.messages.pop_front();
        }
    }

    fn connect(&mut self, address: String) {
        self.leave();
        let name = self
            .profile
            .as_ref()
            .map_or(self.name.as_str(), |profile| profile.name.as_str())
            .trim()
            .to_owned();
        let address = match network::normalize_address(&address) {
            Ok(address) => address,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        self.address.clone_from(&address);
        self.status = format!("Connecting to {address}…");
        self.joining = true;
        let generation = self.connection_generation;
        let endpoint = self.endpoint.clone();
        let token = self.token.clone();
        self.background(move || {
            Reply::Connected(
                generation,
                connect_room(&endpoint, &address, &name, token.as_deref()),
            )
        });
    }

    fn leave(&mut self) {
        self.quick_play = QuickPlay::Idle;
        self.connection_generation = self.connection_generation.wrapping_add(1);
        self.joining = false;
        self.room_refresh = RoomRefreshSchedule::default();
        if let Some(client) = self.client.take()
            && let Ok(client) = client.into_inner()
        {
            let _ = client.send(ClientMessage::Leave);
        }
        self.room = None;
        self.id = None;
        self.players.clear();
        self.remote_poses.clear();
        self.last_sent_pose = None;
        self.room_reports = None;
        self.report_reason.clear();
        self.messages.clear();
        if self.field == Some(Field::Chat) {
            self.field = None;
        }
        self.vote = None;
        self.spectating = None;
        self.desired_map = None;
    }

    fn connect_next_quick_room(&mut self) -> bool {
        let QuickPlay::Joining(mut remaining) = std::mem::take(&mut self.quick_play) else {
            return false;
        };
        while let Some(address) = remaining.pop_front() {
            self.connect(address);
            if self.joining {
                self.quick_play = QuickPlay::Joining(remaining);
                return true;
            }
        }
        false
    }

    fn refresh_room_identity(&mut self) {
        if self
            .room
            .as_ref()
            .is_some_and(|room| room.room_id.is_some())
        {
            self.connect(self.address.clone());
        } else if self.joining
            || (self.room.is_none() && self.client.is_some())
            || !matches!(self.quick_play, QuickPlay::Idle)
        {
            self.leave();
            self.status = "Join the room again to use your updated sign-in.".into();
        }
    }

    fn refresh_room_progression(&mut self) {
        let Some(owner) = self.profile.as_ref().map(|profile| profile.id) else {
            return;
        };
        if !self.room_refresh.due(Instant::now())
            || self
                .room_identity()
                .and_then(|identity| identity.account_id)
                != Some(owner)
        {
            return;
        }
        let (Some(room_id), Some(token)) = (
            self.room.as_ref().and_then(|room| room.room_id.clone()),
            self.token.clone(),
        ) else {
            return;
        };
        let request = ManagedRoomEntry {
            room_id,
            address: self.address.clone(),
        };
        let endpoint = self.endpoint.clone();
        let generation = self.connection_generation;
        self.room_refresh.pending = true;
        self.background(move || {
            Reply::RoomRefresh(
                generation,
                api::renew_room_ticket(&endpoint, &token, &request),
            )
        });
    }

    fn complete_room_refresh(
        &mut self,
        generation: u64,
        result: Result<RoomJoinTicket, api::RetryableRequestFailure>,
        now: Instant,
    ) {
        if generation != self.connection_generation || !self.room_refresh.pending {
            return;
        }
        match result {
            Ok(ticket) => {
                let sent = self
                    .client
                    .as_mut()
                    .and_then(|client| client.get_mut().ok())
                    .is_some_and(|client| client.refresh_identity(ticket).is_ok());
                if sent {
                    self.room_refresh.renewed(now);
                } else {
                    self.room_refresh.failed(now, Some(Duration::ZERO));
                }
            }
            Err(error) => {
                self.room_refresh.failed(now, error.retry_after);
                if error.retry_after.is_none() {
                    self.status = format!("Could not renew room sign-in: {}", error.message);
                }
            }
        }
    }

    fn send(&mut self, message: ClientMessage) -> bool {
        let result = self
            .client
            .as_mut()
            .and_then(|client| client.get_mut().ok())
            .ok_or_else(|| "Join a server first".to_owned())
            .and_then(|client| client.send(message));
        if let Err(error) = result {
            self.status = error;
            return false;
        }
        true
    }

    fn background(&self, work: impl FnOnce() -> Reply + Send + 'static) {
        let sender = self.sender.clone();
        let account_generation = self.account_generation;
        let records_origin = self.records_origin.clone();
        let owner = self.profile.as_ref().map(|profile| profile.id);
        std::thread::spawn(move || {
            let _ = sender.send(PendingReply {
                account_generation,
                records_origin,
                owner,
                reply: work(),
            });
        });
    }

    fn next_reply(&mut self) -> Option<Reply> {
        let owner = self.profile.as_ref().map(|profile| profile.id);
        let receiver = self.receiver.get_mut().ok()?;
        while let Ok(pending) = receiver.try_recv() {
            if !pending.reply.uses_account_scope()
                || (pending.account_generation == self.account_generation
                    && pending.records_origin == self.records_origin
                    && pending.owner == owner)
            {
                return Some(pending.reply);
            }
        }
        None
    }

    fn reset_account(&mut self) {
        self.account_generation = self.account_generation.wrapping_add(1);
        self.close_profile_runs();
        if let Some(flow) = self.provider_sign_in.take() {
            flow.store(true, Ordering::Release);
        }
        self.token = None;
        self.profile = None;
        self.web_history = WebHistory::default();
        self.viewed_profile = None;
        self.playtime = None;
        self.stage_bests = None;
        if let Some(progression) = &mut self.progression {
            progression.own = None;
        }
        self.confirm_delete = false;
        self.password.clear();
        self.motto.clear();
        self.title.clear();
        self.banner.clear();
        self.showcase.clear();
        self.social = SocialState::default();
        self.selected_friend = None;
        self.show_message_archive = false;
        self.reporting_message = None;
        self.message_reports = None;
        self.message_report_status.clear();
        self.message_report_busy = false;
        self.friend_name.clear();
        self.direct_message.clear();
        self.last_social_fetch = None;
        self.presence = None;
        self.presence_clean = true;
        self.unfocused_since = None;
        self.last_game_input = None;
        self.account_busy = false;
        self.board_busy = false;
        self.social_busy = false;
        self.playtime_busy = false;
        self.presence_busy = false;
    }

    fn sync_records_origin(&mut self) {
        let origin = self.endpoint.trim().trim_end_matches('/');
        if self.records_origin == origin {
            return;
        }
        self.records_origin = origin.to_owned();
        self.discovery_generation = self.discovery_generation.wrapping_add(1);
        self.discovering = false;
        self.next_discovery = None;
        self.announce_discovery = false;
        self.servers.clear();
        self.records_directory.clear();
        if self.joining
            || (self.room.is_none() && self.client.is_some())
            || !matches!(self.quick_play, QuickPlay::Idle)
        {
            self.leave();
        }
        let signed_in = self.is_signed_in();
        self.reset_account();
        self.board = None;
        self.stage_records = None;
        self.progression = None;
        self.record_status.clear();
        if signed_in {
            self.refresh_room_identity();
        }
    }

    fn close_profile_runs(&mut self) {
        self.cancel_profile_replay();
        self.profile_runs_generation = self.profile_runs_generation.wrapping_add(1);
        self.profile_runs = None;
    }

    fn cancel_profile_replay(&mut self) {
        if self.profile_replay_busy {
            self.profile_runs_generation = self.profile_runs_generation.wrapping_add(1);
        }
        if self.profile_replay_busy || self.profile_replay_confirm.is_some() {
            self.record_status.clear();
        }
        self.profile_replay_confirm = None;
        self.profile_replay_busy = false;
        self.profile_replay_load = None;
    }

    fn open_profile_runs(&mut self, sort: PlayerRunSort, page: u32) {
        let Some(profile) = self
            .viewed_profile
            .as_ref()
            .map(|(profile, _)| profile)
            .or(self.profile.as_ref())
        else {
            return;
        };
        let id = profile.id;
        self.close_profile_runs();
        self.record_status.clear();
        self.profile_runs = Some(ProfileRunsView {
            sort,
            page,
            result: None,
        });
        let generation = self.profile_runs_generation;
        let endpoint = self.endpoint.clone();
        self.background(move || {
            let result = api::get_fast::<PlayerRunsPage>(
                &endpoint,
                &format!("/v1/profiles/{id}/runs/{}/{page}", sort.path()),
            )
            .and_then(|runs| {
                if runs.account_id != id
                    || runs.page != page
                    || runs.runs.len() > PLAYER_RUNS_PAGE_SIZE
                {
                    return Err("The records service returned the wrong run history page.".into());
                }
                Ok(runs)
            });
            Reply::ProfileRuns(generation, result)
        });
    }

    fn request_profile_replay(&mut self, game: &Game, id: String, ghost: bool) {
        if self.profile_replay_busy || self.replay_busy || game.pending.is_some() {
            return;
        }
        let Some(run) = self
            .profile_runs
            .as_ref()
            .and_then(|view| view.result.as_ref())
            .and_then(|result| result.as_ref().ok())
            .and_then(|page| {
                page.runs
                    .iter()
                    .find(|run| run.id == id && run.replay_available)
            })
        else {
            return;
        };
        let different_map = !game.course.name.eq_ignore_ascii_case(&run.map);
        let ends_run = game.session.phase == RunPhase::Running
            && (!ghost || different_map || game.selected_bonus() != run.bonus);
        let leaves_room = self.profile_replay_leaves_room(&run.map);
        let requested = (id, ghost);
        if (ends_run || leaves_room) && self.profile_replay_confirm.as_ref() != Some(&requested) {
            self.record_status = match (ends_run, leaves_room) {
                (true, true) => "This ends your current run and leaves the server.",
                (true, false) => "This ends your current run.",
                _ => "This leaves the server.",
            }
            .into();
            self.profile_replay_confirm = Some(requested);
            return;
        }
        let (id, ghost) = requested;
        let map = run.map.clone();
        let bonus = run.bonus;
        let generation = self.profile_runs_generation;
        let epoch = game.run_epoch;
        let endpoint = self.endpoint.clone();
        self.profile_replay_confirm = None;
        self.profile_replay_busy = true;
        self.record_status = "Loading replay…".into();
        self.background(move || {
            let result = api::get::<RunReplay>(&endpoint, &format!("/v1/replays/{id}"), None)
                .and_then(|replay| {
                    if !replay.map.eq_ignore_ascii_case(&map) || replay.bonus != bonus {
                        return Err(
                            "The records service returned a replay for a different route.".into(),
                        );
                    }
                    Ok(replay)
                });
            Reply::ProfileReplay(generation, epoch, ghost, result)
        });
    }

    fn start_profile_replay(
        &mut self,
        game: &mut Game,
        menu: &mut MenuState,
        library: &MapLibrary,
        epoch: u64,
        ghost: bool,
        result: Result<RunReplay, String>,
    ) {
        self.profile_replay_busy = false;
        if menu.page != Page::Profile || game.run_epoch != epoch || game.pending.is_some() {
            return;
        }
        let replay = match result {
            Ok(replay) => replay,
            Err(error) => {
                self.record_status = error;
                return;
            }
        };
        if game.course.name.eq_ignore_ascii_case(&replay.map)
            && game
                .course
                .supports_board_replay(replay.bonus, &replay.revision)
        {
            let map = replay.map.clone();
            match game.play_replay_when(replay, ghost, || self.prepare_profile_replay_room(&map)) {
                Ok(()) => {
                    self.record_status.clear();
                    menu.go(Page::Playing);
                }
                Err(error) => self.record_status = error,
            }
            return;
        }
        let source = if replay.map.eq_ignore_ascii_case("Downline") {
            Some(MapSource::Downline)
        } else {
            library.catalog_source(&replay.map)
        };
        let Some(source) = source else {
            self.record_status = "This replay's course is outside the installed catalog.".into();
            return;
        };
        match library.start_load(source) {
            Ok(job) => {
                self.record_status = format!("Opening {}…", replay.map);
                self.profile_replay_busy = true;
                self.profile_replay_load = Some(ProfileReplayLoad {
                    epoch,
                    replay,
                    ghost,
                    job,
                });
            }
            Err(error) => self.record_status = error,
        }
    }

    fn profile_replay_leaves_room(&self, map: &str) -> bool {
        self.room
            .as_ref()
            .is_some_and(|room| !room.map.eq_ignore_ascii_case(map))
    }

    fn prepare_profile_replay_room(&mut self, map: &str) -> Result<(), String> {
        if self.profile_replay_leaves_room(map) {
            self.leave();
        } else if self.spectating.is_some() {
            if !self.send(ClientMessage::Spectate(None)) {
                return Err(self.status.clone());
            }
            self.spectating = None;
        }
        Ok(())
    }

    fn poll_profile_replay(&mut self, game: &mut Game, menu: &mut MenuState) {
        let Some(load) = self.profile_replay_load.as_ref() else {
            return;
        };
        if menu.page != Page::Profile || game.run_epoch != load.epoch || game.pending.is_some() {
            self.profile_replay_load = None;
            self.profile_replay_busy = false;
            self.record_status.clear();
            return;
        }
        while let Some(event) = self
            .profile_replay_load
            .as_ref()
            .and_then(|load| load.job.poll())
        {
            match event {
                LoadEvent::Progress(status) => self.record_status = status,
                LoadEvent::Finished(result) => {
                    let load = self
                        .profile_replay_load
                        .take()
                        .expect("pending profile replay");
                    self.profile_replay_busy = false;
                    match result.and_then(|course| {
                        let map = load.replay.map.clone();
                        game.play_loaded_replay(
                            *course,
                            &menu.settings,
                            load.replay,
                            load.ghost,
                            || self.prepare_profile_replay_room(&map),
                        )
                    }) {
                        Ok(()) => {
                            self.record_status.clear();
                            menu.go(Page::Playing);
                        }
                        Err(error) => self.record_status = error,
                    }
                }
            }
            if menu.page == Page::Profile {
                menu.refresh();
            }
        }
    }

    fn discover(&mut self, announce: bool) {
        self.announce_discovery |= announce;
        if self.discovering {
            return;
        }
        self.discovering = true;
        self.discovery_generation = self.discovery_generation.wrapping_add(1);
        let generation = self.discovery_generation;
        if announce && !self.joining && self.client.is_none() {
            self.status = "Looking for servers…".into();
        }
        let selected = self.address.clone();
        let configured = self.configured_servers.clone();
        let previous_directory = self.records_directory.clone();
        let endpoint = self.endpoint.clone();
        self.background(move || {
            let directory = api::get_fast::<Vec<String>>(&endpoint, "/v1/rooms")
                .ok()
                .map(|rooms| {
                    rooms
                        .into_iter()
                        .take(MAX_DIRECTORY_ROOMS)
                        .filter_map(|address| network::normalize_address(&address).ok())
                        .collect::<Vec<_>>()
                });
            let mut addresses = BTreeMap::new();
            if let Ok(address) = network::normalize_address(&selected) {
                addresses.insert(address, false);
            }
            for address in configured
                .into_iter()
                .filter_map(|address| network::normalize_address(&address).ok())
                .chain(
                    directory
                        .as_ref()
                        .unwrap_or(&previous_directory)
                        .iter()
                        .cloned(),
                )
            {
                addresses.insert(address, true);
            }
            Reply::Discovered {
                generation,
                directory,
                servers: probe_room_addresses(addresses),
            }
        });
    }

    fn complete_discovery(
        &mut self,
        generation: u64,
        directory: Option<Vec<String>>,
        mut servers: Vec<ServerRow>,
    ) -> bool {
        if generation != self.discovery_generation {
            return false;
        }
        self.discovering = false;
        let stagger =
            Duration::from_millis(u64::from(getrandom::u32().unwrap_or_default() % 4_000));
        self.next_discovery = Some(Instant::now() + SERVER_BROWSER_REFRESH + stagger);
        if let Some(directory) = directory {
            self.records_directory = directory;
        }
        for row in &mut servers {
            if let RoomAvailability::Unavailable { last_name } = &mut row.availability {
                *last_name = self
                    .servers
                    .iter()
                    .find(|previous| previous.address == row.address)
                    .and_then(ServerRow::known_name)
                    .map(str::to_owned);
            }
        }
        if std::mem::take(&mut self.announce_discovery) && !self.joining && self.client.is_none() {
            let available = servers
                .iter()
                .filter(|row| matches!(&row.availability, RoomAvailability::Online { .. }))
                .count();
            self.status = if available == 0 {
                "No configured servers responded.".into()
            } else {
                format!("{available} server(s) available")
            };
        }
        self.servers = servers;
        true
    }

    fn refresh_board(&mut self, game: &Game, stage: Option<u16>) {
        if self.board_busy {
            return;
        }
        self.show_stage_records = false;
        self.show_progression = false;
        self.board_page = 0;
        self.board_busy = true;
        self.record_status = "Loading verified records…".into();
        let endpoint = self.endpoint.clone();
        let mut route = format!(
            "/v1/boards/{}/{}",
            game.course.name,
            game.selected_bonus().unwrap_or(0)
        );
        if let Some(stage) = stage {
            route.push_str(&format!("/{stage}"));
        }
        self.background(move || Reply::Board(api::get(&endpoint, &route, None)));
    }

    fn refresh_stages(&mut self, game: &Game) {
        if self.board_busy {
            return;
        }
        self.show_stage_records = true;
        self.show_progression = false;
        self.board_page = 0;
        self.board_busy = true;
        self.record_status = "Loading verified stage records…".into();
        let endpoint = self.endpoint.clone();
        let route = format!(
            "/v1/boards/{}/{}/stages",
            game.course.name,
            game.selected_bonus().unwrap_or(0)
        );
        let token = self.token.clone();
        self.background(move || {
            let result = api::get(&endpoint, &route, None).map(|records| {
                let mine = token.as_deref().and_then(|token| {
                    api::get(&endpoint, &format!("{route}/mine"), Some(token)).ok()
                });
                (records, mine)
            });
            Reply::Stages(result)
        });
    }

    fn update_presence(&mut self, game: &Game, page: Page, focused: bool, input: bool) {
        let (Some(token), Some(profile)) = (self.token.as_ref(), self.profile.as_ref()) else {
            return;
        };
        let now = Instant::now();
        if !focused {
            self.unfocused_since.get_or_insert(now);
        } else if self
            .unfocused_since
            .take()
            .is_some_and(|time| now.duration_since(time) >= Duration::from_secs(20))
        {
            self.presence_clean = false;
        }
        if focused && input && page == Page::Playing && !game.watching_replay() {
            self.last_game_input = Some(now);
        }
        if self.presence_busy {
            return;
        }
        let activity = if self.show_players(game) {
            PlayerActivity::Room {
                map: game.course.name.clone(),
                room: self.address.trim().into(),
            }
        } else if matches!(page, Page::Playing | Page::Pause | Page::Finished)
            && game.pending.is_none()
            && !game.watching_replay()
        {
            PlayerActivity::Solo {
                map: game.course.name.clone(),
            }
        } else {
            PlayerActivity::Menu
        };
        let active = focused
            && self.presence_clean
            && self
                .last_game_input
                .is_some_and(|time| now.duration_since(time) < Duration::from_secs(120));
        let changes = PresenceUpdate { activity, active };
        if self.presence.as_ref().is_some_and(|(last, time)| {
            let interval = if last.activity == changes.activity {
                60
            } else {
                2
            };
            now.duration_since(*time) < Duration::from_secs(interval)
        }) {
            return;
        }
        let endpoint = self.endpoint.clone();
        let token = token.clone();
        let owner = profile.id;
        self.presence = Some((changes.clone(), now));
        self.presence_busy = true;
        self.presence_clean = focused;
        self.background(move || {
            Reply::Presence(
                owner,
                api::post(&endpoint, "/v1/presence", Some(&token), &changes),
            )
        });
    }

    fn refresh_progression(&mut self) {
        if self.board_busy {
            return;
        }
        self.show_stage_records = false;
        self.show_progression = true;
        self.board_page = 0;
        self.board_busy = true;
        self.record_status = "Loading player rankings…".into();
        let endpoint = self.endpoint.clone();
        let token = self.token.clone();
        self.background(move || {
            Reply::Progression(api::get(&endpoint, "/v1/progression", token.as_deref()))
        });
    }

    fn ensure_attempt(&mut self, game: &Game, page: Page) {
        let epoch = game.run_epoch;
        let finished = game
            .finished_replay()
            .filter(|_| game.replay_epoch == Some(epoch));
        if game.watching_replay()
            || (finished.is_none() && (page != Page::Playing || !game.session.ranked_eligible()))
        {
            return;
        }
        let owner = self.profile.as_ref().map(|profile| profile.id);
        let now = Instant::now();
        if self
            .attempt
            .as_ref()
            .is_none_or(|attempt| attempt.epoch != epoch)
        {
            if game.course.board_revision(game.selected_bonus()).is_none() {
                return;
            }
            let attempt_owner = if finished.is_some() && !game.signed_at_finish {
                None
            } else {
                owner
            };
            self.attempt = Some(RankedAttempt::new(
                epoch,
                &self.endpoint,
                attempt_owner,
                now,
            ));
        }
        let attempt = self.attempt.as_mut().expect("attempt for this run");
        if !attempt.applies_to(epoch, &self.endpoint, owner) {
            return;
        }
        let Some(number) = attempt.start_request(now) else {
            return;
        };
        let endpoint = attempt.endpoint.clone();
        let token = attempt.owner.and_then(|_| self.token.clone());
        let request = if let Some(replay) = finished {
            AttemptRequest {
                map: replay.map.clone(),
                bonus: replay.bonus,
            }
        } else {
            AttemptRequest {
                map: game.course.name.clone(),
                bonus: game.selected_bonus(),
            }
        };
        self.background(move || {
            Reply::Attempt(
                epoch,
                number,
                api::request_attempt(&endpoint, token.as_deref(), &request),
            )
        });
    }

    fn complete_attempt(
        &mut self,
        epoch: u64,
        number: u8,
        result: Result<Attempt, api::RetryableRequestFailure>,
    ) {
        let Some(attempt) = &mut self.attempt else {
            return;
        };
        let visible = attempt.applies_to(
            epoch,
            &self.endpoint,
            self.profile.as_ref().map(|profile| profile.id),
        );
        let (outcome, message) = match result {
            Ok(attempt) => (Ok(attempt.token), String::new()),
            Err(error) => (Err(error.retry_after), error.message),
        };
        let progress = attempt.complete(epoch, number, outcome, Instant::now());
        if visible {
            match progress {
                AttemptProgress::Retrying => self.record_status = ATTEMPT_RETRY_STATUS.into(),
                AttemptProgress::Unavailable => self.record_status = message,
                AttemptProgress::Ready if self.record_status == ATTEMPT_RETRY_STATUS => {
                    self.record_status.clear()
                }
                AttemptProgress::Stale | AttemptProgress::Ready => {}
            }
        }
    }

    fn submit(&mut self, game: &Game) {
        let existing = self
            .ranked_saves
            .iter()
            .position(|save| save.epoch == Some(game.run_epoch))
            .or_else(|| {
                let profile = self.profile.as_ref()?;
                if self.can_submit(game) {
                    return None;
                }
                self.ranked_saves.iter().rposition(|save| {
                    save.epoch.is_none()
                        && save.belongs_to(profile.id, &self.records_origin)
                        && save.payload.is_some()
                })
            });
        if let Some(index) = existing {
            let save = &self.ranked_saves[index];
            if self.token.is_none()
                || !self
                    .profile
                    .as_ref()
                    .is_some_and(|profile| save.belongs_to(profile.id, &self.records_origin))
            {
                self.record_status = "Sign back in to the account that submitted this run.".into();
                return;
            }
            match &save.state {
                RankedSaveState::Queued if !self.ranked_save_in_flight(save.owner) => {
                    self.start_ranked_save(index, true)
                }
                RankedSaveState::Queued => {}
                RankedSaveState::InFlight(_) => {
                    self.record_status = "Verifying recorded inputs…".into()
                }
                RankedSaveState::Saved { .. } => {
                    self.record_status = "This run has already been verified.".into()
                }
                RankedSaveState::Failed {
                    generation,
                    error,
                    retry_at,
                    ..
                } => {
                    if save.payload.is_some()
                        && Instant::now() >= *retry_at
                        && (error.status != Some(401) || *generation != self.account_generation)
                        && !self.ranked_save_in_flight(save.owner)
                    {
                        self.start_ranked_save(index, true);
                    } else {
                        self.record_status = error.message.clone();
                    }
                }
            }
            return;
        }
        if !self.can_submit(game) {
            let waiting = self.is_signed_in()
                && game.replay_epoch == Some(game.run_epoch)
                && game.finished_replay().is_some()
                && self.attempt.as_ref().is_some_and(|attempt| {
                    attempt.applies_to(
                        game.run_epoch,
                        &self.endpoint,
                        self.profile.as_ref().map(|profile| profile.id),
                    ) && attempt.pending()
                });
            self.record_status = if waiting {
                ATTEMPT_RETRY_STATUS.into()
            } else {
                "Finish an eligible run and sign in to save it.".into()
            };
            return;
        }
        let owner = self.profile.as_ref().expect("checked account").id;
        let attempt = self
            .attempt
            .as_ref()
            .and_then(RankedAttempt::ticket)
            .expect("checked attempt");
        let replay = game.finished_replay().expect("checked replay").clone();
        let payload = match PendingRankedSave::new(owner, self.records_origin.clone(), replay) {
            Ok(payload) => payload,
            Err(error) => {
                self.record_status = format!("Could not start the ranked save: {error}");
                return;
            }
        };
        if self.ranked_saves.len() == RECENT_RANKED_SAVES {
            self.ranked_saves.pop_front();
        }
        self.ranked_saves.push_back(RankedSave::queued(
            payload,
            Some(game.run_epoch),
            Some(attempt.to_owned()),
        ));
        self.start_ranked_save(self.ranked_saves.len() - 1, false);
    }

    fn ranked_save_in_flight(&self, owner: i64) -> bool {
        self.ranked_saves.iter().any(|save| {
            save.belongs_to(owner, &self.records_origin)
                && matches!(save.state, RankedSaveState::InFlight(_))
        })
    }

    fn start_ranked_save(&mut self, index: usize, recover: bool) {
        let save = &mut self.ranked_saves[index];
        let generation = self.account_generation;
        let id = save.id.clone();
        let attempt = save.attempt.clone();
        let payload = Arc::clone(save.payload.as_ref().expect("pending ranked replay"));
        let token = self.token.clone().expect("ranked save requires sign-in");
        let journal = self.ranked_journal.clone();
        save.state = RankedSaveState::InFlight(generation);
        self.record_status = if recover {
            "Checking the previous ranked save…"
        } else {
            "Verifying recorded inputs…"
        }
        .into();
        self.background(move || {
            if attempt.is_some()
                && let Some(journal) = &journal
                && let Err(error) = journal.store(&payload)
            {
                warn!("Could not keep ranked save on disk: {error}");
            }
            Reply::Submitted(
                id,
                generation,
                api::submit_resumable(
                    &payload.origin,
                    &token,
                    &payload.save_id,
                    &payload.replay,
                    attempt.as_deref(),
                    recover,
                ),
            )
        });
    }

    fn init_ranked_journal(&mut self, directory: &std::path::Path) {
        if self.ranked_journal.is_some() {
            return;
        }
        let journal = RankedJournal::new(directory);
        self.ranked_journal = Some(journal.clone());
        self.background(move || {
            Reply::RankedJournalLoaded(journal.load().map_err(|error| error.to_string()))
        });
    }

    fn restore_ranked_journal(&mut self, result: Result<Vec<PendingRankedSave>, String>) {
        match result {
            Ok(pending) => {
                for save in pending {
                    if self.ranked_saves.len() == RECENT_RANKED_SAVES {
                        break;
                    }
                    if !self
                        .ranked_saves
                        .iter()
                        .any(|current| current.id == save.save_id)
                    {
                        self.ranked_saves
                            .push_back(RankedSave::queued(save, None, None));
                    }
                }
            }
            Err(error) => warn!("Could not reopen pending ranked saves: {error}"),
        }
    }

    fn complete_ranked_save(
        &mut self,
        id: &str,
        generation: u64,
        result: Result<api::RankedSaveReceipt, api::RankedSaveFailure>,
    ) {
        let Some(save) = self.ranked_saves.iter_mut().find(|save| {
            save.id == id
                && matches!(save.state, RankedSaveState::InFlight(current) if current == generation)
        }) else {
            return;
        };
        save.state = match result {
            Ok(receipt) => {
                save.payload = None;
                save.attempt = None;
                RankedSaveState::Saved {
                    generation,
                    receipt,
                    reported: false,
                }
            }
            Err(error) => {
                let retry_at = Instant::now() + error.retry_after.unwrap_or_default();
                if error.retry_after.is_none() && error.status != Some(401) {
                    save.payload = None;
                    save.attempt = None;
                }
                RankedSaveState::Failed {
                    generation,
                    error,
                    retry_at,
                    reported: false,
                }
            }
        };
    }

    fn update_ranked_saves(&mut self, menu: &mut MenuState) {
        let (Some(profile), Some(token)) = (self.profile.as_mut(), self.token.as_ref()) else {
            return;
        };
        let owner = profile.id;
        let generation = self.account_generation;
        let mut refresh_profile = false;
        for save in self
            .ranked_saves
            .iter_mut()
            .filter(|save| save.belongs_to(owner, &self.records_origin))
        {
            match &mut save.state {
                RankedSaveState::Saved {
                    generation: requested,
                    receipt,
                    reported,
                } if !*reported => {
                    *reported = true;
                    if let Some(journal) = &self.ranked_journal
                        && let Err(error) = journal.remove(&save.id)
                    {
                        warn!("Could not clear a completed ranked save: {error}");
                    }
                    let result = &receipt.submission;
                    if let Some(stats) = result.progression {
                        if receipt.restored || *requested != generation {
                            refresh_profile = true;
                        } else {
                            profile.progression = stats;
                            self.room_refresh.expedite(Instant::now());
                        }
                    }
                    self.record_status = if result.personal_best {
                        "Verified personal best saved"
                    } else {
                        "Run verified"
                    }
                    .into();
                    if !result.stage_records.is_empty() {
                        self.record_status.push_str(&format!(
                            " · {} stage record{}",
                            result.stage_records.len(),
                            if result.stage_records.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ));
                    }
                    menu.set_status(self.record_status.clone());
                }
                RankedSaveState::Failed {
                    generation: requested,
                    error,
                    reported,
                    ..
                } if !*reported && (*requested == generation || save.payload.is_none()) => {
                    *reported = true;
                    if save.payload.is_none()
                        && let Some(journal) = &self.ranked_journal
                        && let Err(error) = journal.remove(&save.id)
                    {
                        warn!("Could not clear an expired or rejected ranked save: {error}");
                    }
                    self.record_status = error.message.clone();
                    menu.set_status(self.record_status.clone());
                }
                _ => {}
            }
        }
        if refresh_profile {
            let endpoint = self.records_origin.clone();
            let token = token.clone();
            self.background(move || {
                Reply::RankedProfile(api::get(&endpoint, "/v1/profile", Some(&token)))
            });
        }
        if !self.ranked_save_in_flight(owner)
            && let Some(index) = self.ranked_saves.iter().position(|save| {
                save.belongs_to(owner, &self.records_origin) && save.payload.is_some()
                    && (matches!(save.state, RankedSaveState::Queued)
                        || matches!(save.state, RankedSaveState::Failed { generation: requested, retry_at, .. }
                            if requested != generation && Instant::now() >= retry_at))
            })
        {
            self.start_ranked_save(index, true);
        }
    }
}

pub fn keyboard(
    mut online: ResMut<Online>,
    mut menu: ResMut<MenuState>,
    mut input: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
) {
    online.consumed_input = false;
    if online.field.is_none()
        && online.is_connected()
        && !online.managed_guest()
        && menu.page == Page::Playing
        && (keys.just_pressed(KeyCode::KeyY) || keys.just_pressed(KeyCode::KeyT))
    {
        online.field = Some(Field::Chat);
        menu.refresh();
        input.clear();
        return;
    }
    let Some(field) = online.field else {
        input.clear();
        return;
    };
    online.consumed_input = true;
    for event in input
        .read()
        .filter(|event| event.state == ButtonState::Pressed)
    {
        match event.key_code {
            KeyCode::Escape | KeyCode::Enter => {
                online.field = None;
                if event.key_code == KeyCode::Enter {
                    if field == Field::Chat {
                        online.actions.push(Action::Chat);
                    }
                    if field == Field::DirectMessage {
                        online.actions.push(Action::DirectMessage);
                    }
                    if field == Field::SurfdProfile {
                        online.actions.push(Action::LoadSurfdHistory);
                    }
                }
                menu.refresh();
                break;
            }
            KeyCode::Backspace => {
                online.field_value_mut(field).pop();
            }
            _ => {
                if let Some(text) = &event.text {
                    let limit = match field {
                        Field::Name => 24,
                        Field::Password => 256,
                        Field::Motto => 60,
                        Field::ReportReason => 240,
                        Field::Title | Field::Banner => 96,
                        Field::SurfdProfile => MAX_SURFD_PROFILE_INPUT,
                        _ => 220,
                    };
                    let value = online.field_value_mut(field);
                    for character in text.chars().filter(|character| !character.is_control()) {
                        if value.chars().count() < limit {
                            value.push(character);
                        }
                    }
                }
            }
        }
        menu.refresh();
    }
}

fn watch_lesson_demo(
    online: &mut Online,
    game: &mut Game,
    settings: &Settings,
    lesson: &'static tutorial::Lesson,
) -> Result<(), String> {
    let loaded = if game
        .tutorial_lesson()
        .is_some_and(|active| active.id == lesson.id)
    {
        None
    } else {
        Some(
            tutorial::course(lesson.id)
                .ok_or_else(|| format!("Could not open {}.", lesson.name))?,
        )
    };
    let replay = tutorial::demonstration(loaded.as_ref().unwrap_or(&game.course))
        .ok_or_else(|| format!("Could not play the demonstration for {}.", lesson.name))?;
    let before_play = || {
        online.leave();
        online.field = None;
        Ok(())
    };
    match loaded {
        Some(course) => game.play_loaded_replay(
            LoadedCourse {
                session: Session::new(&course),
                course,
                lesson: Some(lesson),
                starter_guide: false,
            },
            settings,
            replay,
            false,
            before_play,
        ),
        None => game.play_replay_when(replay, false, before_play),
    }
}

pub fn update(
    mut online: ResMut<Online>,
    mut menu: ResMut<MenuState>,
    mut game: ResMut<Game>,
    library: Res<MapLibrary>,
    mut requests: MessageReader<MenuRequest>,
    window: Query<&Window, With<PrimaryWindow>>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<AccumulatedMouseMotion>,
    mut clipboard: Option<ResMut<Clipboard>>,
    mut surfd_clipboard: Local<Option<(u64, String, ClipboardRead)>>,
) {
    online.sync_records_origin();
    online.init_ranked_journal(&library.directory);
    if menu.page != Page::Profile {
        online.cancel_profile_replay();
        online.web_history.close();
        *surfd_clipboard = None;
        if online.field == Some(Field::SurfdProfile) {
            online.field = None;
        }
    }
    if let Some((generation, original_input, read)) = &mut *surfd_clipboard
        && let Some(result) = read.poll_result()
    {
        let same_input =
            *generation == online.account_generation && *original_input == online.web_history.input;
        *surfd_clipboard = None;
        if same_input && online.web_history.visible && !online.web_history.busy {
            match result {
                Ok(text) if text.trim().len() <= MAX_SURFD_PROFILE_INPUT => {
                    online.web_history.input = text.trim().to_owned();
                    online.web_history.status.clear();
                }
                Ok(_) => online.web_history.status = "The profile link is too long.".into(),
                Err(_) => online.web_history.status = "Could not read the clipboard.".into(),
            }
            menu.refresh();
        }
    }
    for request in requests.read() {
        match request {
            MenuRequest::QuickPlay => {
                if online.is_connected() {
                    menu.go(Page::Playing);
                } else if matches!(online.quick_play, QuickPlay::Idle) {
                    online.leave();
                    online.quick_play = QuickPlay::Discovering;
                    online.discover(true);
                }
            }
            MenuRequest::LeaveServer => online.actions.push(Action::Leave),
            MenuRequest::StartLesson(id) => {
                let Some(lesson) = tutorial::lesson(id) else {
                    continue;
                };
                if game
                    .tutorial_lesson()
                    .is_some_and(|current| current.id == lesson.id)
                    && game.session.phase != bevy_surf::session::RunPhase::Finished
                    && game.pending.is_none()
                {
                    online.leave();
                    online.field = None;
                    game.exit_lesson_demo();
                    menu.go(Page::Playing);
                    continue;
                }
                match library.start_load(crate::library::MapSource::Tutorial(lesson.id)) {
                    Ok(job) => {
                        online.leave();
                        online.field = None;
                        game.pending = Some(job);
                        menu.set_status(format!("Opening {}…", lesson.name));
                    }
                    Err(error) => menu.set_status(error),
                }
            }
            MenuRequest::WatchLesson(id) => {
                if game.pending.is_some() {
                    continue;
                }
                let Some(lesson) = tutorial::lesson(id) else {
                    continue;
                };
                match watch_lesson_demo(&mut online, &mut game, &menu.settings, lesson) {
                    Ok(()) => {
                        menu.go(Page::Playing);
                        menu.refresh();
                    }
                    Err(error) => menu.set_status(error),
                }
            }
            MenuRequest::StartPathCourse(id) => {
                if game.pending.is_some() {
                    continue;
                }
                let Some(course) = bevy_surf::catalog::find(id) else {
                    continue;
                };
                match library.start_path_course(course.id.as_str()) {
                    Ok(job) => {
                        online.leave();
                        online.field = None;
                        game.pending = Some(job);
                        menu.set_status(format!("Opening {}…", course.name));
                    }
                    Err(error) => menu.set_status(error),
                }
                menu.refresh();
            }
            MenuRequest::WatchReplay => {
                if let Some(replay) = game.finished_replay().cloned() {
                    play_replay(&mut game, &mut menu, replay, false);
                }
            }
            MenuRequest::WatchReferenceReplay => {
                if !request_reference_ghost(&mut online, &game, &library, &mut menu, true) {
                    menu.set_status("The original record is unavailable for this course".into());
                }
            }
            MenuRequest::ToggleGhost => {
                if game.ghost_enabled() {
                    game.clear_ghost();
                } else if !request_reference_ghost(&mut online, &game, &library, &mut menu, false) {
                    request_native_record_ghost(&mut online, &mut game, &mut menu);
                }
            }
            MenuRequest::ExportReplay => {
                if let Some(replay) = game.finished_replay() {
                    let directory = library.directory.join("replays");
                    let map: String = replay
                        .map
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis();
                    let path = directory.join(format!("{map}-{stamp}.json"));
                    let result = serde_json::to_vec(replay)
                        .map_err(|error| error.to_string())
                        .and_then(|bytes| {
                            fs::create_dir_all(&directory)
                                .and_then(|_| fs::write(&path, bytes))
                                .map_err(|error| error.to_string())
                        });
                    menu.set_status(match result {
                        Ok(()) => format!("Replay saved to {}", path.display()),
                        Err(error) => error,
                    });
                }
            }
        }
    }

    let actions = std::mem::take(&mut online.actions);
    for action in actions {
        perform(
            action,
            &mut online,
            &mut menu,
            &mut game,
            &library.directory,
            clipboard.as_deref_mut(),
            &mut surfd_clipboard,
        );
        menu.refresh();
    }
    while let Some((event, arrival)) = online
        .client
        .as_mut()
        .and_then(|client| client.get_mut().ok())
        .and_then(|client| client.poll_with_arrival())
    {
        let refresh = match event {
            ServerEvent::RoomInfo(_) => false,
            ServerEvent::Welcome { id, room } => {
                info!("Connected to room {} on {}", room.name, room.map);
                online.quick_play = QuickPlay::Idle;
                online.id = Some(id);
                online.status = format!("Connected to {}", room.name);
                online.desired_map = Some(room.map.clone());
                online.room = Some(room);
                online.room_refresh.renewed(Instant::now());
                true
            }
            ServerEvent::Players(players) => {
                online.accept_roster(players, arrival.unwrap_or_else(Instant::now))
            }
            ServerEvent::Chat { from, text } => {
                online.notify(format!("{from}: {text}"));
                true
            }
            ServerEvent::MapChanged { map } => {
                if let Some(room) = &mut online.room {
                    room.map.clone_from(&map);
                }
                online.desired_map = Some(map);
                online.players.clear();
                online.remote_poses.clear();
                true
            }
            ServerEvent::Vote(vote) => {
                online.vote = Some(vote);
                true
            }
            ServerEvent::Reports(page) => {
                if online
                    .room_identity()
                    .is_some_and(|identity| identity.moderator)
                {
                    online.room_reports = Some(page);
                }
                true
            }
            ServerEvent::Notice(text) => {
                if menu.page == Page::Moderation {
                    online.status.clone_from(&text);
                }
                online.notify(text);
                true
            }
            ServerEvent::Error(error) => {
                online.status = error;
                true
            }
            ServerEvent::Disconnected(reason) => {
                let trying_rooms = matches!(online.quick_play, QuickPlay::Joining(_));
                if !trying_rooms || !online.connect_next_quick_room() {
                    let was_connected = online.is_connected();
                    online.leave();
                    if was_connected {
                        online.next_discovery = None;
                        online.notify(reason.clone());
                    }
                    online.status = reason;
                    if trying_rooms {
                        play_solo(&library, &mut game, &mut menu);
                    }
                }
                true
            }
        };
        if refresh && menu.page != Page::Playing {
            menu.refresh();
        }
    }
    if game.pending.is_none()
        && let Some(map) = online.desired_map.take()
    {
        if map.eq_ignore_ascii_case(&game.course.name) {
            game.set_bonus(None);
            menu.go(Page::Playing);
        } else {
            let source = if map.eq_ignore_ascii_case("downline") {
                Some(MapSource::Downline)
            } else {
                library.catalog_source(&map)
            };
            match source
                .ok_or_else(|| format!("The server selected an unknown map: {map}"))
                .and_then(|source| library.start_load(source))
            {
                Ok(job) => {
                    game.pending = Some(job);
                    menu.set_status(format!("Loading {map}…"));
                }
                Err(error) => {
                    online.status = error;
                    menu.go(Page::Servers);
                }
            }
        }
    }
    let show_players = online.show_players(&game);
    if show_players {
        online.sample_room_poses(Instant::now());
    }
    if show_players
        && online.spectating.is_none()
        && !game.watching_replay()
        && online.last_state.elapsed() >= Duration::from_millis(50)
    {
        online.last_state = Instant::now();
        let (yaw, pitch) = game.look_angles();
        let revision = (game.run_epoch, game.session.pose_revision());
        if online.last_sent_pose != Some(revision) {
            online.last_sent_pose = Some(revision);
            online.pose_epoch = online.pose_epoch.wrapping_add(1);
        }
        let motion = Some([
            online.pose_clock.elapsed().as_millis() as u32,
            online.pose_epoch,
        ]);
        online.send(ClientMessage::State(PlayerState {
            position: game.session.player.position.to_array(),
            yaw,
            pitch,
            elapsed: game.session.elapsed,
            finished: game.session.phase == RunPhase::Finished,
            crouched: game.session.player.is_crouched(),
            eye_height: Some(game.session.player.eye_height()),
            motion,
        }));
    }
    while let Some(reply) = online.next_reply() {
        match reply {
            Reply::Discovered {
                generation,
                directory,
                servers,
            } => {
                if !online.complete_discovery(generation, directory, servers) {
                    continue;
                }
                if matches!(online.quick_play, QuickPlay::Discovering) {
                    online.quick_play = QuickPlay::Joining(
                        online
                            .servers
                            .iter()
                            .filter(|row| row.has_space())
                            .take(MAX_QUICK_PLAY_ROOMS)
                            .map(|row| row.address.clone())
                            .collect(),
                    );
                    if !online.connect_next_quick_room() {
                        play_solo(&library, &mut game, &mut menu);
                    }
                } else if menu.page != Page::Servers {
                    continue;
                }
            }
            Reply::Connected(generation, result) => {
                if generation != online.connection_generation {
                    continue;
                }
                online.joining = false;
                match result {
                    Ok(client) => {
                        online.address = client.address().to_owned();
                        online.client = Some(Mutex::new(client));
                    }
                    Err(error) => {
                        online.status = error;
                        if matches!(online.quick_play, QuickPlay::Joining(_))
                            && !online.connect_next_quick_room()
                        {
                            play_solo(&library, &mut game, &mut menu);
                        }
                    }
                }
            }
            Reply::RoomRefresh(generation, result) => {
                online.complete_room_refresh(generation, result, Instant::now());
            }
            Reply::SignedIn(result) => {
                complete_sign_in(&mut online, result);
            }
            Reply::ProviderSignedIn(flow, result) => {
                if online
                    .provider_sign_in
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(active, &flow))
                {
                    online.provider_sign_in = None;
                    complete_sign_in(&mut online, result);
                }
            }
            Reply::SignedOut(deleted, result) => {
                online.account_busy = false;
                match result {
                    Ok(_) => {
                        let cleanup = if deleted && let Some(profile) = &online.profile {
                            WebHistory::purge(
                                &library.directory,
                                &online.records_origin,
                                profile.id,
                            )
                        } else {
                            Ok(())
                        };
                        online.reset_account();
                        online.field = None;
                        online.record_status = match (deleted, cleanup) {
                            (_, Err(error)) => format!("Account deleted. {error}"),
                            (true, Ok(())) => "Account deleted".into(),
                            (false, Ok(())) => "Signed out".into(),
                        };
                        online.refresh_room_identity();
                    }
                    Err(error) => online.record_status = error,
                }
            }
            Reply::Profile(result) => {
                online.account_busy = false;
                match result {
                    Ok(profile) => {
                        set_profile(&mut online, profile);
                        online.record_status = "Profile saved".into();
                    }
                    Err(error) => online.record_status = error,
                }
            }
            Reply::SurfdHistory(result) => {
                online.web_history.busy = false;
                match result {
                    Ok(snapshot) => {
                        online.web_history.save(snapshot);
                        if online.web_history.visible {
                            menu.reset_content_scroll();
                        }
                    }
                    Err(error) => online.web_history.status = error,
                }
            }
            Reply::SurfdRunSplits(source_id, run_id, result) => {
                let history = &mut online.web_history;
                if history
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.source_user_id == source_id)
                    && let Some(details) = &mut history.details
                    && details.run_id == run_id
                {
                    details.splits = match result {
                        Ok(splits) => RunSplits::Loaded(splits),
                        Err(error) => RunSplits::Error(error),
                    };
                }
            }
            Reply::PublicProfile(generation, result) => {
                online.account_busy = false;
                if generation != online.profile_runs_generation {
                    continue;
                }
                match result {
                    Ok(profile) => {
                        online.viewed_profile = Some(profile);
                        online.record_status.clear();
                        menu.go(Page::Profile);
                    }
                    Err(error) => online.record_status = error,
                }
            }
            Reply::ProfileRuns(generation, result) => {
                if generation != online.profile_runs_generation {
                    continue;
                }
                if let Some(view) = &mut online.profile_runs {
                    view.result = Some(result);
                }
            }
            Reply::ProfileReplay(generation, epoch, ghost, result) => {
                if generation != online.profile_runs_generation {
                    continue;
                }
                online.start_profile_replay(&mut game, &mut menu, &library, epoch, ghost, result);
            }
            Reply::Playtime(owner, result) => {
                online.playtime_busy = false;
                if online
                    .profile
                    .as_ref()
                    .is_some_and(|profile| profile.id == owner)
                {
                    match result {
                        Ok(value) => online.playtime = Some(value),
                        Err(error) => online.record_status = error,
                    }
                }
            }
            Reply::Presence(owner, result) => {
                online.presence_busy = false;
                if online
                    .profile
                    .as_ref()
                    .is_some_and(|profile| profile.id == owner)
                    && let Ok(value) = result
                {
                    let playtime_busy = online.playtime_busy;
                    if let Some(playtime) = &mut online.playtime {
                        playtime.seconds = value.seconds;
                        playtime.since = value.since;
                    } else if !playtime_busy {
                        online.playtime = Some(value);
                    }
                }
            }
            Reply::Attempt(epoch, number, result) if epoch == game.run_epoch => {
                online.complete_attempt(epoch, number, result);
            }
            Reply::Attempt(..) => {}
            Reply::Board(result) => {
                online.board_busy = false;
                match result {
                    Ok(board) => {
                        online.board = Some(board);
                        online.record_status.clear();
                    }
                    Err(error) => online.record_status = error,
                }
            }
            Reply::Progression(result) => {
                online.board_busy = false;
                match result {
                    Ok(mut progression) => {
                        let owner = online.profile.as_ref().map(|profile| profile.id);
                        if progression.own.as_ref().map(|row| row.account_id) != owner {
                            progression.own = None;
                        }
                        online.progression = Some(progression);
                        online.record_status.clear();
                    }
                    Err(error) => online.record_status = error,
                }
            }
            Reply::Stages(result) => {
                online.board_busy = false;
                match result {
                    Ok((records, bests)) => {
                        online.stage_records = Some(records);
                        online.stage_bests = bests;
                        online.record_status.clear();
                    }
                    Err(error) => online.record_status = error,
                }
            }
            Reply::Replay(ghost, span, result) => {
                online.replay_busy = false;
                match result {
                    Ok(replay) => {
                        if let Some(span) = span {
                            match game.play_record_span(replay, span.start_tick, span.end_tick) {
                                Ok(()) => menu.go(Page::Playing),
                                Err(error) => menu.set_status(error),
                            }
                        } else {
                            play_replay(&mut game, &mut menu, replay, ghost);
                        }
                    }
                    Err(error) => online.record_status = error,
                }
            }
            Reply::ReferenceGhost {
                course_version,
                bonus,
                watch,
                result,
            } => {
                online.replay_busy = false;
                if game.course_version != course_version
                    || game.selected_bonus() != bonus
                    || game.pending.is_some()
                {
                    continue;
                }
                let playback = result.and_then(|track| {
                    if watch {
                        let map = game.course.name.clone();
                        game.play_reference_replay_when(track, || {
                            online.prepare_profile_replay_room(&map)
                        })
                    } else {
                        game.set_reference_ghost(track)
                    }
                });
                match playback {
                    Ok(()) if watch => menu.go(Page::Playing),
                    Ok(()) => menu.set_status("Record ghost loaded".into()),
                    Err(error) => {
                        if watch || !request_native_record_ghost(&mut online, &mut game, &mut menu)
                        {
                            menu.set_status(error);
                        }
                    }
                }
            }
            Reply::Submitted(id, generation, result) => {
                online.complete_ranked_save(&id, generation, result);
            }
            Reply::RankedJournalLoaded(result) => online.restore_ranked_journal(result),
            Reply::RankedProfile(result) => {
                if let Ok(fresh) = result
                    && let Some(profile) = &mut online.profile
                    && profile.id == fresh.id
                {
                    profile.progression = fresh.progression;
                    online.room_refresh.expedite(Instant::now());
                }
            }
            Reply::Social(result) => {
                online.social_busy = false;
                match result {
                    Ok(state) => {
                        online.social = state;
                        if !online.social.can_review_message_reports {
                            online.message_reports = None;
                        }
                        online.record_status.clear();
                    }
                    Err(error) => online.record_status = error,
                }
            }
            Reply::MessageReported(result) => {
                online.message_report_busy = false;
                match result {
                    Ok(receipt) => {
                        online.reporting_message = None;
                        online.message_report_status = if receipt.duplicate {
                            "You already reported this message."
                        } else {
                            "Reported. A moderator will review it."
                        }
                        .into();
                    }
                    Err(error) => online.message_report_status = error,
                }
            }
            Reply::MessageReports(result) => {
                online.message_report_busy = false;
                match result {
                    Ok(page) if online.social.can_review_message_reports => {
                        online.message_reports = Some(page);
                        online.message_report_status.clear();
                        menu.reset_content_scroll();
                    }
                    Ok(_) => {}
                    Err(error) => {
                        online.message_reports = None;
                        online.message_report_status = error;
                    }
                }
            }
        }
        if menu.page != Page::Playing {
            menu.refresh();
        }
    }
    online.poll_profile_replay(&mut game, &mut menu);
    online.update_ranked_saves(&mut menu);
    online.ensure_attempt(&game, menu.page);
    if game.signed_at_finish && online.can_submit(&game) {
        online.submit(&game);
    }
    let focused = window.single().is_ok_and(|window| window.focused);
    let input = keys.get_just_pressed().next().is_some() || mouse.delta != Vec2::ZERO;
    online.update_presence(&game, menu.page, focused, input);
    online.refresh_room_progression();
    if menu.page == Page::Servers
        && online
            .next_discovery
            .is_none_or(|deadline| Instant::now() >= deadline)
    {
        online.discover(false);
    }
    if online.is_signed_in()
        && menu.page == Page::Friends
        && online
            .last_social_fetch
            .is_none_or(|time| time.elapsed() >= Duration::from_secs(60))
    {
        social(&mut online, SocialAction::Fetch);
    }
}

fn perform(
    action: Action,
    online: &mut Online,
    menu: &mut MenuState,
    game: &mut Game,
    directory: &Path,
    clipboard: Option<&mut Clipboard>,
    surfd_clipboard: &mut Option<(u64, String, ClipboardRead)>,
) {
    if !matches!(action, Action::Edit(_)) {
        online.field = None;
    }
    match action {
        Action::Edit(field) => online.field = (online.field != Some(field)).then_some(field),
        Action::Discover => online.discover(true),
        Action::Join => {
            online.connect(online.address.clone());
        }
        Action::JoinAddress(address) => {
            online.connect(address);
        }
        Action::Leave => {
            online.leave();
            online.status = "Left the server".into();
            menu.go(Page::Servers);
        }
        Action::Chat => {
            let text = std::mem::take(&mut online.chat);
            if !text.trim().is_empty() {
                online.send(ClientMessage::Chat(text));
            }
        }
        Action::Nominate => {
            let name = std::mem::take(&mut online.nomination);
            if !name.trim().is_empty() {
                online.send(ClientMessage::Nominate(name.trim().to_owned()));
            }
        }
        Action::Vote(map) => {
            online.send(ClientMessage::Vote(map));
        }
        Action::RockTheVote => {
            online.send(ClientMessage::RockTheVote);
        }
        Action::Extend => {
            online.send(ClientMessage::Extend);
        }
        Action::Spectate(id) => {
            if online.send(ClientMessage::Spectate(id)) {
                game.session.mark_unranked();
                menu.go(Page::Playing);
            }
        }
        Action::Report(player) => {
            let reason = online.report_reason.trim().to_owned();
            if reason.is_empty() {
                online.status = "Enter what happened before sending the report.".into();
            } else if online.send(ClientMessage::Report { player, reason }) {
                online.report_reason.clear();
                online.status = "Sending report…".into();
            }
        }
        Action::ReviewRoomReports(offset) => {
            online.send(ClientMessage::ReviewReports { offset });
        }
        Action::CloseRoomReports => {
            online.room_reports = None;
        }
        Action::RoomChangeMap => {
            let map = online.nomination.trim().to_ascii_lowercase();
            if !map.is_empty() {
                online.send(ClientMessage::ChangeMap(map));
            }
        }
        Action::RoomModerate(player, action) => {
            online.send(ClientMessage::Moderate {
                player,
                action,
                minutes: (action == RoomModeration::Mute).then_some(10),
            });
        }
        Action::RefreshBoard => online.refresh_board(game, None),
        Action::StageRecords => online.refresh_stages(game),
        Action::StageBoard(stage) => online.refresh_board(game, Some(stage)),
        Action::Progression => online.refresh_progression(),
        Action::RecordsPage(page) => online.board_page = page,
        Action::Watch(id) => request_replay(online, id, false, None),
        Action::WatchStage(id, span) => request_replay(online, id, false, Some(span)),
        Action::Ghost(id) => request_replay(online, id, true, None),
        Action::Register | Action::SignIn if !online.account_busy => {
            online.account_busy = true;
            let endpoint = online.endpoint.clone();
            let route = if matches!(action, Action::Register) {
                "/v1/register"
            } else {
                "/v1/login"
            };
            let credentials = Credentials {
                name: online.name.trim().into(),
                password: std::mem::take(&mut online.password),
            };
            online.background(move || {
                Reply::SignedIn(api::post(&endpoint, route, None, &credentials))
            });
        }
        Action::ProviderSignIn(provider) if !online.account_busy && !online.is_signed_in() => {
            online.account_busy = true;
            let canceled = Arc::new(AtomicBool::new(false));
            online.provider_sign_in = Some(canceled.clone());
            online.record_status = format!("Continue with {} in your browser…", provider.label());
            let endpoint = online.endpoint.clone();
            online.background(move || {
                let result = provider_sign_in(&endpoint, provider, &canceled);
                Reply::ProviderSignedIn(canceled, result)
            });
        }
        Action::CancelProviderSignIn => {
            if let Some(active) = online.provider_sign_in.take() {
                active.store(true, Ordering::Release);
                online.account_busy = false;
                online.record_status = "Sign-in canceled".into();
            }
        }
        Action::SignOut | Action::DeleteAccount if !online.account_busy => {
            if let Some(token) = online.token.clone() {
                online.account_busy = true;
                let endpoint = online.endpoint.clone();
                let delete = matches!(action, Action::DeleteAccount);
                online.background(move || {
                    Reply::SignedOut(
                        delete,
                        if delete {
                            api::delete(&endpoint, "/v1/profile", &token)
                        } else {
                            api::post(&endpoint, "/v1/logout", Some(&token), &())
                        },
                    )
                });
            }
        }
        Action::OpenSurfdHistory if menu.page == Page::Profile => {
            *surfd_clipboard = None;
            if let Some(profile) = &online.profile {
                online
                    .web_history
                    .open(directory, &online.records_origin, profile.id);
                menu.reset_content_scroll();
            }
        }
        Action::CloseSurfdHistory => {
            *surfd_clipboard = None;
            online.web_history.close();
            menu.reset_content_scroll();
        }
        Action::PasteSurfdProfile if online.web_history.visible && !online.web_history.busy => {
            if let Some(clipboard) = clipboard {
                *surfd_clipboard = Some((
                    online.account_generation,
                    online.web_history.input.clone(),
                    clipboard.fetch_text(),
                ));
            }
        }
        Action::LoadSurfdHistory if online.web_history.visible && !online.web_history.busy => {
            *surfd_clipboard = None;
            match SourcePlayer::parse(&online.web_history.input) {
                Ok(player) => {
                    online.web_history.busy = true;
                    online.web_history.status = "Reading Surfd.net history…".into();
                    online.background(move || Reply::SurfdHistory(fetch_history(&player)));
                }
                Err(error) => online.web_history.status = error,
            }
        }
        Action::ForgetSurfdHistory if online.web_history.visible && !online.web_history.busy => {
            *surfd_clipboard = None;
            online.web_history.forget();
            menu.reset_content_scroll();
        }
        Action::SurfdHistoryPage(change) if online.web_history.visible => {
            if let Some(history) = &online.web_history.snapshot {
                let rows = if online.web_history.show_scores {
                    history.maps.len()
                } else {
                    history.runs.len()
                };
                let last_page = rows.div_ceil(SURFD_HISTORY_PAGE_SIZE).saturating_sub(1);
                online.web_history.page = online
                    .web_history
                    .page
                    .saturating_add_signed(change)
                    .min(last_page);
                menu.reset_content_scroll();
            }
        }
        Action::ShowSurfdScores(show_scores) if online.web_history.visible => {
            online.web_history.show_scores = show_scores;
            online.web_history.page = 0;
            menu.reset_content_scroll();
        }
        Action::ShowSurfdRun(run_id) if online.web_history.visible && !online.web_history.busy => {
            let history = &mut online.web_history;
            if history
                .details
                .as_ref()
                .is_some_and(|details| matches!(details.splits, RunSplits::Loading))
            {
                return;
            }
            if let Some(snapshot) = &history.snapshot
                && let Some(run) = snapshot.runs.iter().find(|run| run.run_id == run_id)
            {
                let run = run.clone();
                let source_id = snapshot.source_user_id.clone();
                history.details = Some(RunDetails {
                    run_id: run_id.clone(),
                    splits: RunSplits::Loading,
                });
                online.background(move || {
                    Reply::SurfdRunSplits(source_id, run_id, fetch_run_splits(&run))
                });
            }
        }
        Action::ConfirmDelete => online.confirm_delete = !online.confirm_delete,
        Action::SaveProfile if !online.account_busy => {
            if let (Some(token), Some(profile)) = (online.token.clone(), online.profile.as_ref()) {
                online.account_busy = true;
                let endpoint = online.endpoint.clone();
                let changes = ProfileExtrasUpdate {
                    motto: online.motto.clone(),
                    title: online.title.clone(),
                    banner: online.banner.trim().into(),
                    showcase: online
                        .showcase
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    revision: profile.revision,
                };
                online.background(move || {
                    Reply::Profile(api::post(
                        &endpoint,
                        "/v1/profile/extras",
                        Some(&token),
                        &changes,
                    ))
                });
            }
        }
        Action::SetPlaytimePrivacy(hidden) if !online.playtime_busy => {
            if let (Some(token), Some(profile)) = (online.token.clone(), online.profile.as_ref()) {
                let owner = profile.id;
                online.playtime_busy = true;
                let endpoint = online.endpoint.clone();
                online.background(move || {
                    Reply::Playtime(
                        owner,
                        api::post(
                            &endpoint,
                            "/v1/playtime",
                            Some(&token),
                            &PlaytimePrivacy { hidden },
                        ),
                    )
                });
            }
        }
        Action::ViewProfile(id) if !online.account_busy => {
            online.close_profile_runs();
            if online
                .profile
                .as_ref()
                .is_some_and(|profile| profile.id == id)
            {
                online.viewed_profile = None;
                menu.go(Page::Profile);
            } else {
                online.account_busy = true;
                let endpoint = online.endpoint.clone();
                let route = format!("/v1/profiles/{id}");
                let generation = online.profile_runs_generation;
                online.background(move || {
                    Reply::PublicProfile(
                        generation,
                        api::get(&endpoint, &route, None).map(|profile| {
                            let playtime =
                                api::get(&endpoint, &format!("{route}/playtime"), None).ok();
                            (profile, playtime)
                        }),
                    )
                });
            }
        }
        Action::OwnProfile => {
            online.close_profile_runs();
            online.viewed_profile = None;
        }
        Action::ProfileRuns(sort, page) => {
            online.open_profile_runs(sort, page);
            menu.reset_content_scroll();
        }
        Action::CloseProfileRuns => {
            online.close_profile_runs();
            menu.reset_content_scroll();
        }
        Action::ProfileReplay(id, ghost) => online.request_profile_replay(game, id, ghost),
        Action::CancelProfileReplay => online.cancel_profile_replay(),
        Action::Submit => online.submit(game),
        Action::Social(action) => social(online, action),
        Action::RequestFriend => {
            let name = std::mem::take(&mut online.friend_name);
            social(online, SocialAction::Request { name });
        }
        Action::SelectFriend(account) => {
            online.selected_friend = Some(account);
            if !online.message_report_busy {
                online.reporting_message = None;
                online.message_report_status.clear();
            }
        }
        Action::DirectMessage => {
            if let Some(account) = online.selected_friend {
                let text = std::mem::take(&mut online.direct_message);
                if !text.trim().is_empty() {
                    social(online, SocialAction::Message { account, text });
                }
            }
        }
        Action::ShowMessageArchive(show) => {
            if !online.message_report_busy {
                online.show_message_archive = show;
                online.reporting_message = None;
                online.message_report_status.clear();
                menu.reset_content_scroll();
            }
        }
        Action::SelectMessageReport(message) => {
            if !online.message_report_busy {
                online.reporting_message = message;
                online.message_report_status.clear();
            }
        }
        Action::SendMessageReport(reason) => {
            let (Some(message), Some(token), Some(profile)) = (
                online.reporting_message,
                online.token.clone(),
                &online.profile,
            ) else {
                return;
            };
            if online.message_report_busy {
                return;
            }
            if !online
                .social
                .messages
                .iter()
                .chain(&online.social.archived_messages)
                .any(|received| received.id == message && received.to.id == profile.id)
            {
                online.reporting_message = None;
                online.message_report_status = "This message is no longer available.".into();
                return;
            }
            let endpoint = online.endpoint.clone();
            online.message_report_busy = true;
            online.message_report_status = "Sending report…".into();
            online.background(move || {
                Reply::MessageReported(api::post(
                    &endpoint,
                    "/v1/social/message-reports",
                    Some(&token),
                    &MessageReportRequest { message, reason },
                ))
            });
        }
        Action::ReviewMessageReports(before) => {
            let Some(token) = online.token.clone() else {
                return;
            };
            if online.message_report_busy || !online.social.can_review_message_reports {
                return;
            }
            let endpoint = online.endpoint.clone();
            let cursor = before.map_or_else(|| "latest".into(), |id| id.to_string());
            online.message_report_busy = true;
            online.message_report_status = "Loading reports…".into();
            online.background(move || {
                Reply::MessageReports(api::get(
                    &endpoint,
                    &format!("/v1/social/message-reports/{cursor}"),
                    Some(&token),
                ))
            });
        }
        Action::CloseMessageReports => {
            if !online.message_report_busy {
                online.message_reports = None;
                online.message_report_status.clear();
                menu.reset_content_scroll();
            }
        }
        Action::Invite(account) if online.is_connected() => {
            let address = online.address.clone();
            social(online, SocialAction::Invite { account, address });
        }
        _ => {}
    }
}

fn probe_room_addresses(addresses: BTreeMap<String, bool>) -> Vec<ServerRow> {
    let workers = addresses.len().min(MAX_PARALLEL_ROOM_PROBES);
    let addresses = Mutex::new(addresses.into_iter());
    let mut rows = std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut rows = Vec::new();
                    loop {
                        let next = addresses.lock().unwrap().next();
                        let Some((address, listed)) = next else { break };
                        let availability = match network::discover_with_latency(&address) {
                            Ok((info, ping)) => RoomAvailability::Online {
                                info,
                                ping_ms: ping.as_millis(),
                            },
                            Err(_) if listed => RoomAvailability::Unavailable { last_name: None },
                            Err(_) => continue,
                        };
                        rows.push(ServerRow {
                            address,
                            availability,
                        });
                    }
                    rows
                })
            })
            .collect();
        let mut rows = Vec::new();
        for job in jobs {
            if let Ok(batch) = job.join() {
                rows.extend(batch);
            }
        }
        rows
    });
    let latency = |row: &ServerRow| match &row.availability {
        RoomAvailability::Online { ping_ms, .. } => *ping_ms,
        RoomAvailability::Unavailable { .. } => u128::MAX,
    };
    rows.sort_by(|left, right| {
        latency(left)
            .cmp(&latency(right))
            .then_with(|| left.address.cmp(&right.address))
    });
    rows
}

fn connect_room(
    endpoint: &str,
    address: &str,
    name: &str,
    token: Option<&str>,
) -> Result<Client, String> {
    let managed_rooms = api::get_fast::<Vec<ManagedRoomEntry>>(endpoint, "/v1/managed-rooms").ok();
    let registered = managed_rooms.as_ref().and_then(|rooms| {
        rooms.iter().find(|room| {
            room.address == address
                || (!network::room_address_uses_tls(address)
                    && network::room_address_authority(&room.address)
                        == network::room_address_authority(address))
        })
    });
    let address = registered.map_or(address, |room| room.address.as_str());
    let room = network::discover(address)?;
    if registered.is_some_and(|entry| Some(entry.room_id.as_str()) != room.room_id.as_deref()) {
        return Err("The room identity does not match the records service directory.".into());
    }
    if let Some(room_id) = room.room_id {
        let request = ManagedRoomEntry {
            room_id,
            address: address.to_owned(),
        };
        crate::room_check::join_managed_room(endpoint, &request, token)
    } else {
        Client::connect(address, name)
    }
}

fn play_replay(game: &mut Game, menu: &mut MenuState, replay: RunReplay, ghost: bool) {
    match game.play_replay(replay, ghost) {
        Ok(()) => menu.go(Page::Playing),
        Err(error) => menu.set_status(error),
    }
}

fn play_solo(library: &MapLibrary, game: &mut Game, menu: &mut MenuState) {
    if game.course.name == "Downline"
        && game.pending.is_none()
        && let Some(source) = library.catalog_source("boreas")
    {
        match library.start_load(source) {
            Ok(job) => {
                game.pending = Some(job);
                menu.set_status("Opening Boreas in solo…".into());
            }
            Err(error) => menu.set_status(error),
        }
    } else if game.pending.is_none() {
        menu.go(Page::Playing);
    }
}

fn request_replay(online: &mut Online, id: String, ghost: bool, span: Option<StageSpan>) {
    if online.replay_busy || online.profile_replay_busy {
        return;
    }
    online.replay_busy = true;
    let endpoint = online.endpoint.clone();
    online.background(move || {
        Reply::Replay(
            ghost,
            span,
            api::get(&endpoint, &format!("/v1/replays/{id}"), None),
        )
    });
}

fn request_native_record_ghost(online: &mut Online, game: &mut Game, menu: &mut MenuState) -> bool {
    let id = online
        .board
        .as_ref()
        .filter(|board| {
            board.map == game.course.name
                && board.bonus == game.selected_bonus()
                && board.stage.is_none()
        })
        .and_then(|board| board.rows.first())
        .map(|row| row.id.clone());
    if let Some(id) = id {
        request_replay(online, id, true, None);
    } else if let Some(replay) = game.finished_replay().cloned() {
        play_replay(game, menu, replay, true);
    } else {
        return false;
    }
    true
}

fn request_reference_ghost(
    online: &mut Online,
    game: &Game,
    library: &MapLibrary,
    menu: &mut MenuState,
    watch: bool,
) -> bool {
    let Some(entry) = bevy_surf::catalog::find(&game.course.name) else {
        return false;
    };
    let Some(revision) = bevy_surf::catalog::revision(&entry.geometry) else {
        return false;
    };
    if game.course.catalog_revision.as_deref() != Some(revision) {
        return false;
    }
    let bonus = game.selected_bonus();
    let (record, file_name) = match bonus {
        Some(number) => {
            let Some(pack_revision) = entry
                .bonus_pack
                .as_deref()
                .and_then(bevy_surf::catalog::revision)
            else {
                return false;
            };
            (
                format!("{}/bonus-{number}", entry.slug),
                format!("{}-bonus-{number}-{pack_revision}.json", entry.id),
            )
        }
        None => (entry.slug.clone(), format!("{}.json", entry.id)),
    };
    if online.replay_busy {
        return true;
    }
    let (directory, converter) = if watch || bonus.is_some() {
        ("reference-replays", "import-reference-replay.ts")
    } else {
        ("reference-ghosts", "import-reference-ghost.ts")
    };
    let label = if watch {
        "record replay"
    } else {
        "record ghost"
    };
    let root = library.directory.clone();
    let path = root.join(directory).join(file_name);
    let script = crate::reference_tool(converter);
    if !path.is_file() && script.is_none() {
        return false;
    }
    let course_version = game.course_version;
    online.replay_busy = true;
    menu.set_status(format!("Loading the original {label}…"));
    online.background(move || Reply::ReferenceGhost {
        course_version,
        bonus,
        watch,
        result: (|| {
            if !path.is_file() {
                let script = script.ok_or("The original replay conversion tool is unavailable")?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                let assets = crate::reference_replay_assets(&root, entry, bonus)?;
                let mut command = std::process::Command::new("bun");
                command
                    .arg(script)
                    .arg(record)
                    .arg(assets.geometry)
                    .arg(&path);
                if let Some(pack) = assets.bonus_pack {
                    command.arg("--bonus-pack").arg(pack);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    command.creation_flags(0x0800_0000);
                }
                let output = command.output().map_err(|error| {
                    format!("Could not start the Bun {label} converter: {error}")
                })?;
                if !output.status.success() {
                    let error = String::from_utf8_lossy(&output.stderr);
                    return Err(format!(
                        "Original {label} unavailable: {}",
                        error.lines().last().unwrap_or("conversion failed")
                    ));
                }
            }
            let track = ReferenceGhost::load(&path)?;
            if track.bonus() != bonus {
                return Err("The original record belongs to a different route".into());
            }
            Ok(track)
        })(),
    });
    true
}

fn provider_sign_in(
    endpoint: &str,
    provider: OAuthProvider,
    canceled: &AtomicBool,
) -> Result<SignedIn, String> {
    let start: OAuthStart = api::post(
        endpoint,
        "/v1/oauth/start",
        None,
        &OAuthStartRequest { provider },
    )?;
    if canceled.load(Ordering::Acquire) {
        return Err("Sign-in canceled".into());
    }
    let authorized: ureq::http::Uri = provider
        .authorization_endpoint()
        .parse()
        .expect("valid provider URI");
    let supplied: ureq::http::Uri = start
        .url
        .parse()
        .map_err(|_| "Invalid provider sign-in address")?;
    if supplied.scheme() != authorized.scheme()
        || supplied.authority() != authorized.authority()
        || supplied.path() != authorized.path()
        || supplied.query().is_none()
    {
        return Err("The records service supplied an unexpected provider address.".into());
    }
    open_provider_browser(&start.url)?;
    let deadline = Instant::now() + Duration::from_secs(5 * 60);
    while Instant::now() < deadline && !canceled.load(Ordering::Acquire) {
        match api::post(endpoint, "/v1/oauth/poll", None, &start.poll)? {
            OAuthPoll::Pending => std::thread::sleep(Duration::from_secs(2)),
            OAuthPoll::SignedIn(session) => return Ok(session),
        }
    }
    if canceled.load(Ordering::Acquire) {
        Err("Sign-in canceled".into())
    } else {
        Err("Sign-in timed out. Try again.".into())
    }
}

fn open_provider_browser(url: &str) -> Result<(), String> {
    use std::process::Command;
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let root = std::env::var_os("SystemRoot").ok_or("Windows system folder is unavailable")?;
        let executable = std::path::PathBuf::from(root)
            .join("System32")
            .join("rundll32.exe");
        let mut command = Command::new(executable);
        command
            .arg("url.dll,FileProtocolHandler")
            .creation_flags(0x0800_0000);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    #[cfg(not(any(windows, unix)))]
    let mut command = Command::new("open");
    let mut child = command
        .arg(url)
        .spawn()
        .map_err(|_| "Could not open your default browser")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn complete_sign_in(online: &mut Online, result: Result<SignedIn, String>) {
    online.account_busy = false;
    match result {
        Ok(signed_in) => {
            online.reset_account();
            online.token = Some(signed_in.token);
            set_profile(online, signed_in.profile);
            online.field = None;
            online.record_status = "Signed in".into();
            online.refresh_room_identity();
        }
        Err(error) => online.record_status = error,
    }
}

fn set_profile(online: &mut Online, profile: Profile) {
    online.motto.clone_from(&profile.motto);
    online.title.clone_from(&profile.title);
    online.banner.clone_from(&profile.banner);
    online.showcase = profile.showcase.join(", ");
    online.profile = Some(profile);
}

fn social(online: &mut Online, action: SocialAction) {
    if online.social_busy {
        return;
    }
    let Some(token) = online.token.clone() else {
        online.record_status = "Sign in to use friends.".into();
        return;
    };
    let endpoint = online.endpoint.clone();
    online.social_busy = true;
    online.last_social_fetch = Some(Instant::now());
    online.background(move || {
        Reply::Social(api::post(&endpoint, "/v1/social", Some(&token), &action))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_surf::social_protocol::{DirectMessage, SocialUser};
    use bevy_surf::{
        course::Course,
        server::{RoomConfig, RoomServer},
    };

    const QUICK_PLAY_FIXTURE_MAP: &str = "quick-play-fixture";

    fn online_app(mut online: Online) -> App {
        let library = MapLibrary::read();
        online.ranked_journal = Some(RankedJournal::new(&library.directory));
        let mut course = Course::practice();
        course.name = QUICK_PLAY_FIXTURE_MAP.into();
        course.catalog_revision = None;
        let mut app = App::new();
        app.insert_resource(online)
            .insert_resource(Game::new(course))
            .insert_resource(MenuState::new(&library.directory))
            .insert_resource(library)
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<AccumulatedMouseMotion>()
            .add_message::<MenuRequest>()
            .add_systems(Update, update);
        app
    }

    #[test]
    fn room_shutdown_reason_reaches_the_gameplay_feed_once() {
        let server = RoomServer::bind(RoomConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            map: QUICK_PLAY_FIXTURE_MAP.into(),
            ..RoomConfig::default()
        })
        .unwrap();
        let mut online = Online::default();
        online.client = Some(Mutex::new(
            Client::connect(&server.local_addr().to_string(), "Reader").unwrap(),
        ));
        let mut app = online_app(online);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            app.update();
            if app.world().resource::<Online>().is_connected() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the room did not admit the client"
            );
            std::thread::sleep(Duration::from_millis(4));
        }
        assert_eq!(app.world().resource::<MenuState>().page, Page::Playing);
        {
            let mut online = app.world_mut().resource_mut::<Online>();
            online.field = Some(Field::Chat);
            online.chat = "Unsent message".into();
        }

        server.stop().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            app.update();
            if !app.world().resource::<Online>().is_connected() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the room shutdown did not arrive"
            );
            std::thread::sleep(Duration::from_millis(4));
        }
        app.update();
        let online = app.world().resource::<Online>();
        assert_eq!(online.status, "The room is shutting down.");
        assert_eq!(online.messages.len(), 1);
        assert_eq!(online.messages[0], online.status);
        assert!(!online.is_typing());
        assert_eq!(online.chat, "Unsent message");
        assert_eq!(app.world().resource::<MenuState>().page, Page::Playing);
    }

    #[test]
    fn quick_play_tries_the_next_room_when_discovered_rooms_go_offline_or_fill() {
        use std::io::{BufRead, BufReader, Write};

        let start = |name: &str| {
            RoomServer::bind(RoomConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                name: name.into(),
                map: QUICK_PLAY_FIXTURE_MAP.into(),
                capacity: 1,
                ..Default::default()
            })
            .unwrap()
        };
        let offline = start("Going Offline");
        let filled = start("Filling Up");
        let available = start("Still Available");
        let address = |room: &RoomServer| room.local_addr().to_string();
        let rows = [&offline, &filled, &available]
            .into_iter()
            .enumerate()
            .map(|(index, room)| {
                let address = address(room);
                let info = network::discover(&address).unwrap();
                assert_eq!(info.players, 0);
                ServerRow {
                    address,
                    availability: RoomAvailability::Online {
                        info,
                        ping_ms: index as u128,
                    },
                }
            })
            .collect();
        offline.stop().unwrap();
        let occupant = Client::connect(&address(&filled), "Occupant").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !matches!(occupant.poll(), Some(ServerEvent::Welcome { .. })) {
            assert!(Instant::now() < deadline, "could not fill the second room");
            std::thread::sleep(Duration::from_millis(4));
        }
        assert_eq!(network::discover(&address(&filled)).unwrap().players, 1);

        let records = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        records.set_nonblocking(true).unwrap();
        let mut online = Online {
            endpoint: format!("http://{}", records.local_addr().unwrap()),
            ..Default::default()
        };
        let directory = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            for _ in 0..3 {
                let mut stream = loop {
                    match records.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "missing room directory request");
                            std::thread::sleep(Duration::from_millis(4));
                        }
                        Err(error) => panic!("room directory listener failed: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = String::new();
                BufReader::new(&mut stream).read_line(&mut request).unwrap();
                assert!(request.starts_with("GET /v1/managed-rooms "));
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]",
                    )
                    .unwrap();
            }
        });
        online.sync_records_origin();
        online.quick_play = QuickPlay::Discovering;
        online.discovering = true;
        let generation = online.discovery_generation;
        online.background(move || Reply::Discovered {
            generation,
            directory: None,
            servers: rows,
        });
        let mut app = online_app(online);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.update();
            let online = app.world().resource::<Online>();
            if online.is_connected() {
                assert_eq!(online.address, address(&available));
                assert_eq!(online.room.as_ref().unwrap().name, "Still Available");
                assert!(matches!(online.quick_play, QuickPlay::Idle));
                assert_eq!(app.world().resource::<MenuState>().page, Page::Playing);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "quick play failed: {}",
                online.status
            );
            std::thread::sleep(Duration::from_millis(4));
        }
        directory.join().unwrap();
    }

    #[test]
    fn stale_discovery_cannot_restart_quick_play_after_a_records_origin_change() {
        let mut online = Online {
            endpoint: "http://127.0.0.1:2".into(),
            ..Default::default()
        };
        online.sync_records_origin();
        online.quick_play = QuickPlay::Discovering;
        online.discovering = true;
        let generation = online.discovery_generation;
        online.background(move || Reply::Discovered {
            generation,
            directory: None,
            servers: Vec::new(),
        });
        let stale = receive_background(&mut online);
        online.endpoint = "http://127.0.0.1:1".into();
        online.sync_records_origin();
        assert!(matches!(online.quick_play, QuickPlay::Idle));
        online.quick_play = QuickPlay::Discovering;
        online.discovering = true;
        online.sender.send(stale).unwrap();
        let mut app = online_app(online);
        app.update();
        let online = app.world().resource::<Online>();
        assert!(online.discovering);
        assert!(matches!(online.quick_play, QuickPlay::Discovering));
        assert!(online.client.is_none());
    }

    #[test]
    fn server_browser_keeps_a_room_during_restart_and_refreshes_only_when_visible() {
        use bevy::ecs::system::RunSystemOnce;
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            sync::atomic::AtomicUsize,
        };

        let mut config = RoomConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            name: "Before restart".into(),
            managed: Some(
                network::RoomTicketKey::from_hex("browser-fixture", &"0123456789abcdef".repeat(4))
                    .unwrap(),
            ),
            ..Default::default()
        };
        let room = RoomServer::bind(config.clone()).unwrap();
        config.bind = room.local_addr();
        let address = config.bind.to_string();
        let listed = serde_json::to_string(&[&address]).unwrap();
        let records = TcpListener::bind("127.0.0.1:0").unwrap();
        records.set_nonblocking(true).unwrap();
        let online = Online {
            endpoint: format!("http://{}", records.local_addr().unwrap()),
            address: String::new(),
            configured_servers: Vec::new(),
            ..Default::default()
        };
        let request_count = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&request_count);
        let directory = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            for (status, body) in [
                ("200 OK", listed.as_str()),
                ("503 Service Unavailable", "{}"),
                ("200 OK", listed.as_str()),
                ("200 OK", "[]"),
            ] {
                let mut stream = loop {
                    match records.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "missing browser directory request"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("browser directory listener failed: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = String::new();
                BufReader::new(&mut stream).read_line(&mut request).unwrap();
                assert!(request.starts_with("GET /v1/rooms "));
                count.fetch_add(1, Ordering::Relaxed);
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let mut app = online_app(online);
        let wait_for = |app: &mut App, expected: &dyn Fn(&Online) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                app.update();
                let online = app.world().resource::<Online>();
                if !online.discovering && expected(online) {
                    break;
                }
                assert!(Instant::now() < deadline, "browser did not refresh");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let labels = |app: &mut App| {
            let world = app.world_mut();
            let root = world
                .run_system_once(|mut commands: Commands, online: Res<Online>| {
                    commands
                        .spawn(Node::default())
                        .with_children(|parent| {
                            crate::online_ui::panel(parent, OnlinePage::Servers, &online, "");
                        })
                        .id()
                })
                .unwrap();
            let text = world
                .query::<&Text>()
                .iter(world)
                .map(|label| label.0.clone())
                .collect::<Vec<_>>();
            world.despawn(root);
            text
        };
        let force_refresh = |app: &mut App| {
            app.world_mut().resource_mut::<Online>().next_discovery = Some(Instant::now());
        };

        app.update();
        assert_eq!(request_count.load(Ordering::Relaxed), 0);
        app.world_mut()
            .resource_mut::<MenuState>()
            .go(Page::Servers);
        wait_for(&mut app, &|online| {
            matches!(
                online.servers.as_slice(),
                [ServerRow { availability: RoomAvailability::Online { info, .. }, .. }] if info.name == "Before restart"
            )
        });
        assert!(labels(&mut app).iter().any(|label| label == "Join"));

        room.stop().unwrap();
        app.world_mut().resource_mut::<Online>().status = "The room is shutting down.".into();
        force_refresh(&mut app);
        wait_for(&mut app, &|online| {
            matches!(
                online.servers.as_slice(),
                [ServerRow { availability: RoomAvailability::Unavailable { last_name: Some(name) }, .. }] if name == "Before restart"
            )
        });
        let unavailable = labels(&mut app);
        assert!(
            unavailable
                .iter()
                .any(|label| label.contains("Before restart") && label.contains("Unavailable"))
        );
        assert!(!unavailable.iter().any(|label| label == "Join"));
        assert_eq!(
            app.world().resource::<Online>().status,
            "The room is shutting down."
        );

        app.world_mut()
            .resource_mut::<MenuState>()
            .go(Page::Playing);
        force_refresh(&mut app);
        app.update();
        assert_eq!(request_count.load(Ordering::Relaxed), 2);
        assert!(app.world().resource::<Online>().client.is_none());

        config.name = "After restart".into();
        let _restarted = RoomServer::bind(config).unwrap();
        app.world_mut()
            .resource_mut::<MenuState>()
            .go(Page::Servers);
        wait_for(&mut app, &|online| {
            matches!(
                online.servers.as_slice(),
                [ServerRow { address: actual, availability: RoomAvailability::Online { info, .. } }] if *actual == address && info.name == "After restart"
            )
        });
        assert!(labels(&mut app).iter().any(|label| label == "Join"));
        assert!(app.world().resource::<Online>().client.is_none());

        force_refresh(&mut app);
        wait_for(&mut app, &|online| online.servers.is_empty());
        assert_eq!(request_count.load(Ordering::Relaxed), 4);
        directory.join().unwrap();
    }

    fn signed_in(id: i64) -> SignedIn {
        SignedIn {
            token: format!("test-session-{id}"),
            profile: Profile {
                id,
                name: format!("Surfer{id}"),
                motto: String::new(),
                title: String::new(),
                banner: String::new(),
                showcase: Vec::new(),
                revision: 0,
                progression: Default::default(),
            },
        }
    }

    #[test]
    fn signed_room_refresh_retries_before_lease_expiry_and_preserves_server_backoff() {
        let now = Instant::now();
        let mut schedule = RoomRefreshSchedule::default();
        assert!(!schedule.due(now));
        schedule.renewed(now);
        let first = now + ROOM_REFRESH_INTERVAL;
        assert!(!schedule.due(first - Duration::from_secs(1)));
        assert!(schedule.due(first));
        schedule.pending = true;
        assert!(!schedule.due(first));
        schedule.failed(first, Some(Duration::from_secs(60)));
        schedule.expedite(first);
        assert!(!schedule.due(first + Duration::from_secs(59)));
        assert!(schedule.due(first + Duration::from_secs(60)));

        let mut retry = first + Duration::from_secs(60);
        for seconds in [4, 8, 16, 20, 20] {
            schedule.pending = true;
            schedule.failed(retry, Some(Duration::ZERO));
            retry += Duration::from_secs(seconds);
            assert!(!schedule.due(retry - Duration::from_secs(1)));
            assert!(schedule.due(retry));
        }
        assert!(retry < now + Duration::from_secs(300));
        schedule.renewed(retry);
        assert!(!schedule.due(retry + Duration::from_secs(119)));
        assert!(schedule.due(retry + ROOM_REFRESH_INTERVAL));

        schedule.failed(retry, None);
        schedule.expedite(retry);
        assert!(!schedule.due(retry + Duration::from_secs(300)));
    }

    #[test]
    fn stale_room_refresh_cannot_apply_after_account_or_connection_changes() {
        let mut online = Online::default();
        complete_sign_in(&mut online, Ok(signed_in(23)));
        let generation = online.connection_generation;
        let failed = || api::RetryableRequestFailure {
            message: "expired login".into(),
            retry_after: None,
        };
        online.background(move || Reply::RoomRefresh(generation, Err(failed())));
        let old = receive_background(&mut online);

        online.reset_account();
        complete_sign_in(&mut online, Ok(signed_in(42)));
        online.sender.send(old).unwrap();
        assert!(online.next_reply().is_none());

        online.leave();
        let now = Instant::now();
        online.room_refresh.renewed(now);
        online.room_refresh.pending = true;
        online.complete_room_refresh(generation, Err(failed()), now);
        assert!(online.room_refresh.pending);
        assert!(!online.status.contains("expired login"));
    }

    fn receive_background(online: &mut Online) -> PendingReply {
        online
            .receiver
            .get_mut()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
    }

    #[test]
    fn web_history_replies_cache_and_deletion_follow_the_signed_in_account() {
        let scoped_reply = |online: &Online, reply| PendingReply {
            account_generation: online.account_generation,
            records_origin: online.records_origin.clone(),
            owner: online.profile.as_ref().map(|profile| profile.id),
            reply,
        };
        let directory = std::env::temp_dir().join(format!(
            "surfd-history-integration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let source: SurfdHistory = serde_json::from_value(serde_json::json!({
            "sourceUserId": "11111111-2222-3333-4444-555555555555",
            "player": {
                "userId": "11111111-2222-3333-4444-555555555555",
                "username": "Web Surfer", "rank": "Surfer", "points": 600,
                "records": 0, "completions": 3, "position": 18
            },
            "maps": [], "runs": [], "progressionVersion": 1
        }))
        .unwrap();
        let mut online = Online::default();
        online.endpoint = "https://history.records.example".into();
        online.sync_records_origin();
        complete_sign_in(&mut online, Ok(signed_in(23)));
        let mut app = online_app(online);
        app.world_mut().resource_mut::<MapLibrary>().directory = directory.clone();
        app.world_mut()
            .resource_mut::<MenuState>()
            .go(Page::Profile);
        app.world_mut()
            .resource_mut::<Online>()
            .queue(Action::OpenSurfdHistory);
        app.update();

        let pending = {
            let online = app.world().resource::<Online>();
            scoped_reply(online, Reply::SurfdHistory(Ok(source.clone())))
        };
        app.world_mut()
            .resource_mut::<Online>()
            .sender
            .send(pending)
            .unwrap();
        app.update();
        {
            let online = app.world().resource::<Online>();
            assert_eq!(online.web_history.snapshot.as_ref(), Some(&source));
            assert_eq!(online.profile.as_ref().unwrap().progression.points, 0);
        }

        let stale = {
            let online = app.world().resource::<Online>();
            scoped_reply(online, Reply::SurfdHistory(Ok(source.clone())))
        };
        {
            let mut online = app.world_mut().resource_mut::<Online>();
            online.endpoint = "https://other.history.records.example".into();
            online.sync_records_origin();
            complete_sign_in(&mut online, Ok(signed_in(23)));
            online.sender.send(stale).unwrap();
            online.queue(Action::OpenSurfdHistory);
        }
        app.update();
        assert!(
            app.world()
                .resource::<Online>()
                .web_history
                .snapshot
                .is_none()
        );
        {
            let mut online = app.world_mut().resource_mut::<Online>();
            online.endpoint = "https://history.records.example".into();
            online.sync_records_origin();
            complete_sign_in(&mut online, Ok(signed_in(23)));
            online.queue(Action::OpenSurfdHistory);
        }
        app.update();
        assert_eq!(
            app.world()
                .resource::<Online>()
                .web_history
                .snapshot
                .as_ref(),
            Some(&source)
        );
        {
            let online = app.world().resource::<Online>();
            let deleted = scoped_reply(online, Reply::SignedOut(true, Ok(true)));
            online.sender.send(deleted).unwrap();
        }
        app.update();
        assert!(app.world().resource::<Online>().profile.is_none());
        let mut restored = WebHistory::default();
        restored.open(&directory, "https://history.records.example", 23);
        assert!(restored.snapshot.is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn private_messages(owner: i64) -> SocialState {
        let user = |id| SocialUser {
            id,
            name: format!("Surfer{id}"),
            presence: None,
        };
        SocialState {
            messages: vec![DirectMessage {
                id: 1,
                from: user(owner),
                to: user(71),
                text: "Account-only conversation".into(),
                created_at: 1,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn signing_in_replaces_the_previous_accounts_private_state() {
        let mut online = Online::default();
        complete_sign_in(&mut online, Ok(signed_in(23)));
        online.social = private_messages(23);
        online.selected_friend = Some(71);
        online.direct_message = "An unfinished private message".into();
        online.stage_bests = Some(StageBests {
            map: "surf_kitsune".into(),
            bonus: None,
            rows: Vec::new(),
        });

        complete_sign_in(&mut online, Ok(signed_in(42)));

        assert_eq!(online.profile.unwrap().id, 42);
        assert!(online.social.messages.is_empty());
        assert!(online.selected_friend.is_none());
        assert!(online.direct_message.is_empty());
        assert!(online.stage_bests.is_none());
    }

    #[test]
    fn delayed_social_data_cannot_cross_a_session_change_for_the_same_account() {
        let mut online = Online::default();
        complete_sign_in(&mut online, Ok(signed_in(23)));
        online.social_busy = true;
        online.background(|| Reply::Social(Ok(private_messages(23))));
        let previous = receive_background(&mut online);

        online.reset_account();
        complete_sign_in(&mut online, Ok(signed_in(23)));
        assert!(!online.social_busy);
        online.background(|| Reply::Social(Ok(SocialState::default())));
        let current = receive_background(&mut online);
        online.sender.send(previous).unwrap();
        online.sender.send(current).unwrap();

        let Some(Reply::Social(Ok(result))) = online.next_reply() else {
            panic!("the current session's result should still be received");
        };
        assert!(result.messages.is_empty());
        assert!(online.next_reply().is_none());
    }

    #[test]
    fn delayed_sign_in_cannot_authorize_another_records_service() {
        let mut online = Online::default();
        online.endpoint = "https://first.records.example".into();
        online.sync_records_origin();
        online.account_busy = true;
        online.background(|| Reply::SignedIn(Ok(signed_in(23))));
        let previous = receive_background(&mut online);

        online.endpoint = "https://second.records.example".into();
        online.sync_records_origin();
        online.sender.send(previous).unwrap();

        assert!(online.next_reply().is_none());
        assert!(!online.account_busy);
        assert!(!online.is_signed_in());
    }

    #[test]
    fn guest_ranked_ticket_can_complete_after_sign_in() {
        let mut online = Online::default();
        online.background(|| {
            Reply::Attempt(
                7,
                1,
                Ok(Attempt {
                    token: "guest-attempt".into(),
                }),
            )
        });
        let pending = receive_background(&mut online);
        complete_sign_in(&mut online, Ok(signed_in(23)));
        online.sender.send(pending).unwrap();

        assert!(matches!(
            online.next_reply(),
            Some(Reply::Attempt(7, 1, Ok(_)))
        ));
    }

    #[test]
    fn delayed_ranked_save_is_visible_only_to_the_original_account_and_service() {
        use bevy_surf::record_protocol::Submission;

        let mut online = Online::default();
        online.endpoint = "https://original.records.example".into();
        online.sync_records_origin();
        complete_sign_in(&mut online, Ok(signed_in(23)));
        let generation = online.account_generation;
        online.ranked_saves.push_back(RankedSave {
            id: "pending-test-save".into(),
            epoch: Some(7),
            owner: 23,
            origin: online.records_origin.clone(),
            payload: None,
            attempt: None,
            state: RankedSaveState::InFlight(generation),
        });
        online.background(move || {
            Reply::Submitted(
                "pending-test-save".into(),
                generation,
                Ok(api::RankedSaveReceipt {
                    submission: Submission {
                        time_us: 4_060_017,
                        personal_best: true,
                        run_id: Some("saved-run".into()),
                        stage_records: Vec::new(),
                        progression: None,
                    },
                    restored: false,
                }),
            )
        });
        let delayed = receive_background(&mut online);
        complete_sign_in(&mut online, Ok(signed_in(42)));
        online.sender.send(delayed).unwrap();
        let Some(Reply::Submitted(id, generation, result)) = online.next_reply() else {
            panic!("delayed save result was lost");
        };
        online.complete_ranked_save(&id, generation, result);
        let mut menu = MenuState::new(std::path::Path::new("target"));
        online.update_ranked_saves(&mut menu);
        assert_eq!(online.record_status, "Signed in");

        online.endpoint = "https://other.records.example".into();
        online.sync_records_origin();
        complete_sign_in(&mut online, Ok(signed_in(23)));
        online.update_ranked_saves(&mut menu);
        assert_eq!(online.record_status, "Signed in");

        online.endpoint = "https://original.records.example".into();
        online.sync_records_origin();
        complete_sign_in(&mut online, Ok(signed_in(23)));
        online.update_ranked_saves(&mut menu);
        assert_eq!(online.record_status, "Verified personal best saved");
    }

    fn profile_replay_fixture() -> (bevy_surf::course::Course, RunReplay) {
        use bevy_surf::{course::Course, movement::MoveInput, replay::TickCommand};

        let course = Course::practice();
        let replay = RunReplay {
            version: bevy_surf::replay::REPLAY_VERSION,
            map: course.name.clone(),
            revision: course.board_revision(None).unwrap(),
            bonus: None,
            options: Default::default(),
            commands: vec![TickCommand {
                input: MoveInput {
                    yaw: course.spawn_yaw,
                    ..Default::default()
                },
                pitch: 0.0,
                restart_stage: false,
                source_yaw_degrees: None,
            }],
        };
        (course, replay)
    }

    #[test]
    fn profile_replay_loads_its_course_and_leaves_the_current_run_on_error() {
        let (course, replay) = profile_replay_fixture();
        let mut other = course;
        other.name = "Other course".into();
        let mut game = Game::new(other);
        let mut online = Online::default();
        online.room = Some(RoomInfo {
            name: "Test room".into(),
            map: game.course.name.clone(),
            players: 1,
            capacity: 24,
            room_id: None,
        });
        let library = MapLibrary::read();
        let mut menu = MenuState::new(&library.directory);
        menu.go(Page::Profile);
        let epoch = game.run_epoch;
        let wait_for_load = |online: &mut Online, game: &mut Game, menu: &mut MenuState| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while online.profile_replay_busy {
                assert!(
                    Instant::now() < deadline,
                    "profile replay did not finish loading"
                );
                online.poll_profile_replay(game, menu);
                std::thread::sleep(Duration::from_millis(1));
            }
        };

        let mut bad_revision = replay.clone();
        bad_revision.revision = "old-course".into();
        online.start_profile_replay(
            &mut game,
            &mut menu,
            &library,
            epoch,
            false,
            Ok(bad_revision),
        );
        wait_for_load(&mut online, &mut game, &mut menu);
        assert_eq!(game.run_epoch, epoch);
        assert_eq!(game.course.name, "Other course");
        assert_eq!(online.room.as_ref().unwrap().map, "Other course");
        assert!(!online.record_status.is_empty());

        online.room.as_mut().unwrap().map = "downline".into();
        online.start_profile_replay(&mut game, &mut menu, &library, epoch, false, Ok(replay));
        wait_for_load(&mut online, &mut game, &mut menu);
        assert_eq!(game.course.name, "Downline");
        assert!(online.room.is_some());
        assert!(game.watching_replay());
        assert!(menu.page == Page::Playing);
    }

    #[test]
    fn watching_a_profile_replay_stops_spectating_after_validation() {
        use bevy_surf::server::{RoomConfig, RoomServer};

        fn wait_for(client: &Client, accept: impl Fn(&ServerEvent) -> bool) -> ServerEvent {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(event) = client.poll() {
                    if accept(&event) {
                        return event;
                    }
                    assert!(
                        !matches!(event, ServerEvent::Disconnected(_) | ServerEvent::Error(_)),
                        "{event:?}"
                    );
                }
                assert!(Instant::now() < deadline, "room event did not arrive");
                std::thread::sleep(Duration::from_millis(4));
            }
        }

        let room = RoomServer::bind(RoomConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        })
        .unwrap();
        let address = room.local_addr().to_string();
        let watcher = Client::connect(&address, "Watcher").unwrap();
        let target = Client::connect(&address, "Surfer").unwrap();
        let welcome = |event: &ServerEvent| matches!(event, ServerEvent::Welcome { .. });
        let ServerEvent::Welcome { id, room: info } = wait_for(&watcher, welcome) else {
            unreachable!()
        };
        let ServerEvent::Welcome { id: target_id, .. } = wait_for(&target, welcome) else {
            unreachable!()
        };
        watcher
            .send(ClientMessage::Spectate(Some(target_id)))
            .unwrap();
        let spectating = |event: &ServerEvent, expected| matches!(event, ServerEvent::Players(players) if players.iter().any(|player| player.id == id && player.spectating == expected));
        wait_for(&watcher, |event| spectating(event, Some(target_id)));

        let (course, replay) = profile_replay_fixture();
        let mut game = Game::new(course);
        let epoch = game.run_epoch;
        let mut online = Online::default();
        online.id = Some(id);
        online.room = Some(info);
        online.client = Some(Mutex::new(watcher));
        online.spectating = Some(target_id);
        let library = MapLibrary::read();
        let mut menu = MenuState::new(&library.directory);
        menu.go(Page::Profile);

        let mut invalid = replay.clone();
        invalid.commands[0].pitch = 10.0;
        online.start_profile_replay(&mut game, &mut menu, &library, epoch, false, Ok(invalid));
        assert_eq!(online.spectating, Some(target_id));
        assert!(!game.watching_replay());

        online.start_profile_replay(&mut game, &mut menu, &library, epoch, false, Ok(replay));
        assert!(game.watching_replay());
        assert_eq!(online.spectating, None);
        assert!(online.room.is_some());
        let watcher = online.client.as_mut().unwrap().get_mut().unwrap();
        wait_for(watcher, |event| spectating(event, None));
    }
}
