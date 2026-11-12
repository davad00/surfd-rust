use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{catalog::MapMode, replay::RunReplay};

pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:27166";
pub const MAX_REPLAY_BYTES: usize = 16 * 1024 * 1024;
pub const VERIFICATION_BUSY: &str =
    "The ranked verifier is busy. Try saving this run again shortly.";

#[derive(Clone, Deserialize, Serialize)]
pub struct ManagedRoomEntry {
    pub room_id: String,
    pub address: String,
    #[serde(default)]
    pub mode: crate::catalog::MapMode,
}

pub type RoomTicketRequest = ManagedRoomEntry;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressionRank {
    #[default]
    Unranked,
    Rookie,
    Surfer,
    Regular,
    Skilled,
    Expert,
    Master,
    Legend,
}

impl ProgressionRank {
    pub fn from_points(points: u32) -> Self {
        match points {
            20_000.. => Self::Legend,
            11_000.. => Self::Master,
            7_000.. => Self::Expert,
            4_000.. => Self::Skilled,
            2_000.. => Self::Regular,
            500.. => Self::Surfer,
            100.. => Self::Rookie,
            _ => Self::Unranked,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unranked => "Unranked",
            Self::Rookie => "Rookie",
            Self::Surfer => "Surfer",
            Self::Regular => "Regular",
            Self::Skilled => "Skilled",
            Self::Expert => "Expert",
            Self::Master => "Master",
            Self::Legend => "Legend",
        }
    }

    pub fn label_for(self, mode: MapMode) -> &'static str {
        if self == Self::Surfer && mode == MapMode::Bhop {
            "Hopper"
        } else {
            self.label()
        }
    }
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ProgressionStats {
    pub points: u32,
    pub rank: ProgressionRank,
    pub completions: u16,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ProgressionRow {
    pub account_id: i64,
    pub name: String,
    #[serde(flatten)]
    pub stats: ProgressionStats,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Progression {
    #[serde(default)]
    pub mode: MapMode,
    pub entries: Vec<ProgressionRow>,
    pub total: usize,
    pub own: Option<ProgressionRow>,
}

#[derive(Deserialize, Serialize)]
pub struct Credentials {
    pub name: String,
    pub password: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Profile {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    pub motto: String,
    pub title: String,
    #[serde(default)]
    pub banner: String,
    #[serde(default)]
    pub showcase: Vec<String>,
    #[serde(default)]
    pub revision: u64,
    #[serde(flatten)]
    pub progression: ProgressionStats,
    #[serde(default)]
    pub bhop_progression: ProgressionStats,
}

#[derive(Deserialize, Serialize)]
pub struct SignedIn {
    pub token: String,
    pub profile: Profile,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthProvider {
    Google,
    X,
}

impl OAuthProvider {
    pub fn id(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::X => "x",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::X => "X",
        }
    }

    pub fn authorization_endpoint(self) -> &'static str {
        match self {
            Self::Google => "https://accounts.google.com/o/oauth2/v2/auth",
            Self::X => "https://x.com/i/oauth2/authorize",
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct OAuthStartRequest {
    pub provider: OAuthProvider,
}

#[derive(Deserialize, Serialize)]
pub struct OAuthPollRequest {
    pub state: String,
    pub token: String,
}

#[derive(Deserialize, Serialize)]
pub struct OAuthStart {
    pub url: String,
    pub poll: OAuthPollRequest,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", content = "session", rename_all = "snake_case")]
pub enum OAuthPoll {
    Pending,
    SignedIn(SignedIn),
}

#[derive(Deserialize, Serialize)]
pub struct ProfileUpdate {
    pub motto: String,
    pub title: String,
}

#[derive(Deserialize, Serialize)]
pub struct ProfileExtrasUpdate {
    pub motto: String,
    pub title: String,
    pub banner: String,
    pub showcase: Vec<String>,
    pub revision: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Playtime {
    pub hidden: bool,
    pub seconds: Option<u64>,
    pub since: Option<i64>,
}

#[derive(Deserialize, Serialize)]
pub struct PlaytimePrivacy {
    pub hidden: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct AttemptRequest {
    pub map: String,
    pub bonus: Option<u16>,
}

#[derive(Deserialize, Serialize)]
pub struct Attempt {
    pub token: String,
}

#[derive(Deserialize, Serialize)]
pub struct SubmitRun {
    pub attempt: String,
    pub replay: RunReplay,
}

#[derive(Deserialize, Serialize)]
pub struct SubmissionLookup {
    pub attempt: String,
    pub replay_sha256: String,
}

#[derive(Deserialize, Serialize)]
pub struct ReserveRankedSave {
    pub attempt: String,
    #[serde(flatten)]
    pub save: RankedSaveLookup,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct RankedSaveLookup {
    pub save_id: String,
    pub replay_sha256: String,
}

#[derive(Deserialize, Serialize)]
pub struct ResumeRankedSave {
    pub save_id: String,
    pub replay: RunReplay,
}

pub fn valid_ranked_save_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", content = "result", rename_all = "snake_case")]
pub enum SubmissionStatus {
    Pending,
    Saved(Submission),
}

pub fn replay_sha256(canonical_replay_json: &[u8]) -> String {
    format!("{:x}", Sha256::digest(canonical_replay_json))
}

#[derive(Clone, Deserialize, Serialize)]
pub struct BoardRow {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<i64>,
    pub time_us: u64,
    pub splits_us: Vec<u64>,
    pub checkpoint_speeds: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_span: Option<StageSpan>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct StageSpan {
    pub start_tick: f64,
    pub end_tick: f64,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Board {
    pub map: String,
    pub bonus: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<u16>,
    pub rows: Vec<BoardRow>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct StageBests {
    pub map: String,
    pub bonus: Option<u16>,
    pub rows: Vec<StageBoardRow>,
}

pub const PLAYER_RUNS_PAGE_SIZE: usize = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayerRunSort {
    #[default]
    BestPlace,
    Newest,
}

impl PlayerRunSort {
    pub fn path(self) -> &'static str {
        match self {
            Self::BestPlace => "place",
            Self::Newest => "newest",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PlayerRunsPage {
    pub account_id: i64,
    pub page: u32,
    pub total: usize,
    pub runs: Vec<PlayerRun>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PlayerRun {
    pub id: String,
    pub map: String,
    pub bonus: Option<u16>,
    pub time_us: u64,
    pub created_at: Option<String>,
    pub ticks: Option<u32>,
    pub place: Option<u64>,
    pub points: Option<u32>,
    pub archived: bool,
    pub replay_available: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct StageBoardRow {
    pub stage: u16,
    pub row: BoardRow,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct StageRecord {
    pub stage: u16,
    pub time_us: u64,
    pub previous_record_us: Option<u64>,
}

#[derive(Deserialize, Serialize)]
pub struct Submission {
    pub time_us: u64,
    pub personal_best: bool,
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage_records: Vec<StageRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progression: Option<ProgressionStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bhop_progression: Option<ProgressionStats>,
}

#[derive(Deserialize, Serialize)]
pub struct ServiceError {
    pub error: String,
}
