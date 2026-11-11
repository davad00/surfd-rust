use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SocialAction {
    Fetch,
    Request { name: String },
    Accept { account: i64 },
    Decline { account: i64 },
    Cancel { account: i64 },
    Remove { account: i64 },
    Block { account: i64 },
    Unblock { account: i64 },
    Message { account: i64, text: String },
    Invite { account: i64, address: String },
    Dismiss { notification: i64 },
    MarkRead { notification: i64 },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SocialUser {
    pub id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<PlayerPresence>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlayerActivity {
    Menu,
    Solo { map: String },
    Room { map: String, room: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PlayerPresence {
    /// None when offline.
    pub activity: Option<PlayerActivity>,
    /// Unix seconds.
    pub last_seen: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PresenceUpdate {
    pub activity: PlayerActivity,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(i64)]
pub enum NotificationKind {
    FriendRequest = 1,
    FriendAccepted = 2,
    Message = 3,
    ServerInvite = 4,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SocialNotification {
    pub id: i64,
    pub from: SocialUser,
    pub kind: NotificationKind,
    /// For an invite, the destination room address.
    pub text: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
    pub read: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DirectMessage {
    pub id: i64,
    pub from: SocialUser,
    pub to: SocialUser,
    pub text: String,
    /// Unix seconds.
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(i64)]
pub enum MessageReportReason {
    Harassment = 1,
    Spam = 2,
    Hate = 3,
    Other = 4,
}

impl MessageReportReason {
    pub const ALL: [Self; 4] = [Self::Harassment, Self::Spam, Self::Hate, Self::Other];

    pub fn label(self) -> &'static str {
        match self {
            Self::Harassment => "Harassment",
            Self::Spam => "Spam",
            Self::Hate => "Hate",
            Self::Other => "Other",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MessageReportRequest {
    pub message: i64,
    pub reason: MessageReportReason,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MessageReportReceipt {
    pub duplicate: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MessageReport {
    pub id: i64,
    pub message: i64,
    pub sender: SocialUser,
    pub reporter: SocialUser,
    pub text: String,
    pub reason: MessageReportReason,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MessageReportPage {
    pub reports: Vec<MessageReport>,
    pub next_before: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct SocialState {
    pub friends: Vec<SocialUser>,
    pub incoming: Vec<SocialUser>,
    pub outgoing: Vec<SocialUser>,
    pub blocked: Vec<SocialUser>,
    /// Most recent first.
    pub notifications: Vec<SocialNotification>,
    /// Most recent first.
    pub messages: Vec<DirectMessage>,
    /// Received messages from conversations that have ended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archived_messages: Vec<DirectMessage>,
    #[serde(default)]
    pub can_review_message_reports: bool,
}
