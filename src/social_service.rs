use crate::{
    record_protocol::Playtime,
    social_protocol::{
        DirectMessage, MessageReport, MessageReportPage, MessageReportReason, MessageReportReceipt,
        MessageReportRequest, NotificationKind, PlayerActivity, PlayerPresence, SocialAction,
        SocialNotification, SocialState, SocialUser,
    },
};
use rusqlite::{Connection, OptionalExtension, Params, Row, TransactionBehavior, params};
use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

const RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;
const REPORT_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;
const REPORT_PAGE_SIZE: usize = 8;
const MAX_FRIENDS: usize = 200;
const MAX_PENDING: usize = 100;
const MAX_BLOCKED: usize = 200;
const FETCH_LIMIT: i64 = 100;
const MESSAGE_HISTORY: i64 = 1000;
const NOTIFICATION_HISTORY: i64 = 200;
const PRESENCE_SECONDS: i64 = 180;
const PLAYTIME_GAP_SECONDS: i64 = 90;

const SCHEMA: &str = "
    PRAGMA foreign_keys = ON;
    CREATE TABLE IF NOT EXISTS social_friends (
        a_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        b_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        CHECK (a_id < b_id),
        PRIMARY KEY (a_id, b_id)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS social_friends_other ON social_friends(b_id);

    CREATE TABLE IF NOT EXISTS social_requests (
        sender_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        recipient_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        CHECK (sender_id != recipient_id),
        PRIMARY KEY (sender_id, recipient_id)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS social_requests_recipient ON social_requests(recipient_id);
    CREATE INDEX IF NOT EXISTS social_requests_time ON social_requests(created_at);

    CREATE TABLE IF NOT EXISTS social_blocks (
        blocker_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        blocked_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        CHECK (blocker_id != blocked_id),
        PRIMARY KEY (blocker_id, blocked_id)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS social_blocks_other ON social_blocks(blocked_id);

    CREATE TABLE IF NOT EXISTS social_messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        sender_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        recipient_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        body TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        CHECK (sender_id != recipient_id)
    );
    CREATE INDEX IF NOT EXISTS social_messages_sender ON social_messages(sender_id, id DESC);
    CREATE INDEX IF NOT EXISTS social_messages_recipient ON social_messages(recipient_id, id DESC);
    CREATE INDEX IF NOT EXISTS social_messages_time ON social_messages(created_at);

    CREATE TABLE IF NOT EXISTS social_message_reports (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        message_id INTEGER NOT NULL UNIQUE,
        reporter_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        sender_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        reason INTEGER NOT NULL CHECK (reason BETWEEN 1 AND 4),
        body TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        CHECK (reporter_id != sender_id)
    );
    CREATE INDEX IF NOT EXISTS social_message_reports_time ON social_message_reports(created_at);
    CREATE INDEX IF NOT EXISTS social_message_reports_reporter ON social_message_reports(reporter_id);
    CREATE INDEX IF NOT EXISTS social_message_reports_sender ON social_message_reports(sender_id);

    CREATE TABLE IF NOT EXISTS social_notifications (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        sender_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        recipient_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        kind INTEGER NOT NULL CHECK (kind BETWEEN 1 AND 4),
        payload TEXT,
        created_at INTEGER NOT NULL,
        is_read INTEGER NOT NULL DEFAULT 0 CHECK (is_read IN (0, 1)),
        CHECK (sender_id != recipient_id)
    );
    CREATE INDEX IF NOT EXISTS social_notifications_sender ON social_notifications(sender_id);
    CREATE INDEX IF NOT EXISTS social_notifications_recipient ON social_notifications(recipient_id, id DESC);
    CREATE INDEX IF NOT EXISTS social_notifications_time ON social_notifications(created_at);

    CREATE TABLE IF NOT EXISTS social_rates (
        account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        operation TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        hits INTEGER NOT NULL CHECK (hits > 0),
        PRIMARY KEY (account_id, operation)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS social_presence (
        account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
        session BLOB,
        activity TEXT,
        seen_at INTEGER,
        seconds INTEGER NOT NULL DEFAULT 0,
        since INTEGER,
        hidden INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0, 1))
    );
";

#[derive(Debug)]
pub enum SocialError {
    Rule(&'static str),
    Database(rusqlite::Error),
}

impl fmt::Display for SocialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rule(reason) => formatter.write_str(reason),
            Self::Database(_) => {
                formatter.write_str("The social service could not read or save your changes.")
            }
        }
    }
}

impl std::error::Error for SocialError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rule(_) => None,
            Self::Database(error) => Some(error),
        }
    }
}

impl From<rusqlite::Error> for SocialError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

type Result<T> = std::result::Result<T, SocialError>;

pub fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(SCHEMA)?;
    if !db.pragma_query_value(None, "foreign_keys", |row| row.get::<_, bool>(0))? {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if !db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('social_messages') WHERE name='archived')",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        db.execute(
            "ALTER TABLE social_messages ADD COLUMN archived INTEGER NOT NULL DEFAULT 0
             CHECK (archived IN (0, 1))",
            [],
        )?;
    }
    prune_message_reports(db, unix_seconds())?;
    Ok(())
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn prune_message_reports(db: &Connection, now: i64) -> rusqlite::Result<()> {
    db.execute(
        "DELETE FROM social_message_reports WHERE created_at < ?1",
        [now - REPORT_RETENTION_SECONDS],
    )?;
    Ok(())
}

pub fn apply(db: &mut Connection, actor: i64, action: SocialAction) -> Result<SocialState> {
    let fetch_only = matches!(action, SocialAction::Fetch);
    let transaction = db.transaction_with_behavior(if fetch_only {
        TransactionBehavior::Deferred
    } else {
        TransactionBehavior::Immediate
    })?;
    if actor <= 0 || !account_exists(&transaction, actor)? {
        return Err(SocialError::Rule("Sign in to use friends and messages."));
    }
    let now = unix_seconds();
    let cutoff = now - RETENTION_SECONDS;
    if !fetch_only {
        transaction.execute(
            "DELETE FROM social_requests WHERE created_at < ?1",
            [cutoff],
        )?;
        transaction.execute(
            "DELETE FROM social_messages WHERE created_at < ?1",
            [cutoff],
        )?;
        transaction.execute(
            "DELETE FROM social_notifications WHERE created_at < ?1",
            [cutoff],
        )?;
        prune_message_reports(&transaction, now)?;
        change(&transaction, actor, action, now)?;
    }
    let state = fetch(&transaction, actor, cutoff, now)?;
    transaction.commit()?;
    Ok(state)
}

pub(crate) fn report_message(
    db: &mut Connection,
    actor: i64,
    report: MessageReportRequest,
) -> Result<MessageReportReceipt> {
    let db = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = unix_seconds();
    prune_message_reports(&db, now)?;
    let duplicate: bool = db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM social_message_reports WHERE message_id = ?1 AND reporter_id = ?2
        )",
        params![report.message, actor],
        |row| row.get(0),
    )?;
    if !duplicate {
        let Some((sender, body)) = db
            .query_row(
                "SELECT sender_id, body FROM social_messages
             WHERE id = ?1 AND recipient_id = ?2 AND created_at >= ?3",
                params![report.message, actor, now - RETENTION_SECONDS],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Err(SocialError::Rule("Choose a message you received."));
        };
        rate(&db, actor, "message_report", 6, now)?;
        // Keep only the reported message. Blocking the sender removes the thread.
        db.execute(
            "INSERT INTO social_message_reports
                 (message_id, reporter_id, sender_id, reason, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                report.message,
                actor,
                sender,
                report.reason as i64,
                body,
                now
            ],
        )?;
    }
    db.commit()?;
    Ok(MessageReportReceipt { duplicate })
}

pub(crate) fn message_reports(db: &Connection, before: Option<i64>) -> Result<MessageReportPage> {
    let now = unix_seconds();
    prune_message_reports(db, now)?;
    let mut query = db.prepare(
        "SELECT r.id, r.message_id, s.id, s.name, a.id, a.name, r.body, r.reason
         FROM social_message_reports r
         JOIN accounts s ON s.id = r.sender_id
         JOIN accounts a ON a.id = r.reporter_id
         WHERE r.id < ?1 AND r.created_at >= ?2
         ORDER BY r.id DESC LIMIT ?3",
    )?;
    let mut reports = query
        .query_map(
            params![
                before.unwrap_or(i64::MAX),
                now - REPORT_RETENTION_SECONDS,
                REPORT_PAGE_SIZE + 1,
            ],
            |row| {
                let reason = match row.get::<_, i64>(7)? {
                    1 => MessageReportReason::Harassment,
                    2 => MessageReportReason::Spam,
                    3 => MessageReportReason::Hate,
                    4 => MessageReportReason::Other,
                    value => return Err(rusqlite::Error::IntegralValueOutOfRange(7, value)),
                };
                Ok(MessageReport {
                    id: row.get(0)?,
                    message: row.get(1)?,
                    sender: user(row, 2)?,
                    reporter: user(row, 4)?,
                    text: row.get(6)?,
                    reason,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_older = reports.len() > REPORT_PAGE_SIZE;
    reports.truncate(REPORT_PAGE_SIZE);
    let next_before = reports.last().filter(|_| has_older).map(|row| row.id);
    Ok(MessageReportPage {
        reports,
        next_before,
    })
}

fn change(db: &Connection, actor: i64, action: SocialAction, now: i64) -> Result<()> {
    match action {
        SocialAction::Fetch => {}
        SocialAction::Request { name } => request_friend(db, actor, &name, now)?,
        SocialAction::Accept { account } => accept_friend(db, actor, account, now)?,
        SocialAction::Decline { account } => {
            check_other(actor, account)?;
            delete_request(db, account, actor)?;
        }
        SocialAction::Cancel { account } => {
            check_other(actor, account)?;
            delete_request(db, actor, account)?;
        }
        SocialAction::Remove { account } => {
            check_other(actor, account)?;
            if are_friends(db, actor, account)? {
                revoke_friendship(db, actor, account)?;
            }
        }
        SocialAction::Block { account } => block_account(db, actor, account)?,
        SocialAction::Unblock { account } => {
            check_other(actor, account)?;
            db.execute(
                "DELETE FROM social_blocks WHERE blocker_id = ?1 AND blocked_id = ?2",
                [actor, account],
            )?;
        }
        SocialAction::Message { account, text } => message_friend(db, actor, account, &text, now)?,
        SocialAction::Invite { account, address } => {
            invite_friend(db, actor, account, &address, now)?
        }
        SocialAction::Dismiss { notification } => {
            check_notification(notification)?;
            db.execute(
                "DELETE FROM social_notifications WHERE recipient_id = ?1 AND id = ?2",
                [actor, notification],
            )?;
        }
        SocialAction::MarkRead { notification } => {
            check_notification(notification)?;
            db.execute(
                "UPDATE social_notifications SET is_read = 1 WHERE recipient_id = ?1 AND id = ?2",
                [actor, notification],
            )?;
        }
    }
    Ok(())
}

fn request_friend(db: &Connection, actor: i64, name: &str, now: i64) -> Result<()> {
    let name = printable(
        name,
        32,
        "Enter an account name with 1 to 32 printable characters.",
    )?;
    let account = db
        .query_row(
            "SELECT id FROM accounts WHERE name = ?1 COLLATE NOCASE",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(SocialError::Rule("That account could not be contacted."))?;
    check_other(actor, account)?;
    check_unblocked(db, actor, account)?;
    if are_friends(db, actor, account)? || has_request(db, actor, account)? {
        return Ok(());
    }
    if has_request(db, account, actor)? {
        return Err(SocialError::Rule(
            "Accept the request already in your inbox.",
        ));
    }
    let outgoing: usize = db.query_row(
        "SELECT COUNT(*) FROM social_requests WHERE sender_id = ?1",
        [actor],
        |row| row.get(0),
    )?;
    let incoming: usize = db.query_row(
        "SELECT COUNT(*) FROM social_requests WHERE recipient_id = ?1",
        [account],
        |row| row.get(0),
    )?;
    if outgoing >= MAX_PENDING {
        return Err(SocialError::Rule(
            "Your pending friend request list is full.",
        ));
    }
    if incoming >= MAX_PENDING {
        return Err(SocialError::Rule(
            "That account is unable to receive another request.",
        ));
    }
    rate(db, actor, "friend_request", 12, now)?;
    db.execute(
        "INSERT INTO social_requests (sender_id, recipient_id, created_at) VALUES (?1, ?2, ?3)",
        params![actor, account, now],
    )?;
    add_notice(
        db,
        actor,
        account,
        NotificationKind::FriendRequest,
        None,
        now,
    )
}

fn accept_friend(db: &Connection, actor: i64, account: i64, now: i64) -> Result<()> {
    check_other(actor, account)?;
    check_unblocked(db, actor, account)?;
    if are_friends(db, actor, account)? {
        return Ok(());
    }
    if !has_request(db, account, actor)? {
        return Err(SocialError::Rule(
            "This friend request is no longer pending.",
        ));
    }
    if friend_count(db, actor)? >= MAX_FRIENDS || friend_count(db, account)? >= MAX_FRIENDS {
        return Err(SocialError::Rule("One of the friend lists is full."));
    }
    let (a, b) = pair(actor, account);
    db.execute(
        "INSERT INTO social_friends (a_id, b_id) VALUES (?1, ?2)",
        [a, b],
    )?;
    delete_request(db, account, actor)?;
    delete_request(db, actor, account)?;
    add_notice(
        db,
        actor,
        account,
        NotificationKind::FriendAccepted,
        None,
        now,
    )
}

fn delete_request(db: &Connection, sender: i64, recipient: i64) -> rusqlite::Result<()> {
    db.execute(
        "DELETE FROM social_requests WHERE sender_id = ?1 AND recipient_id = ?2",
        [sender, recipient],
    )?;
    db.execute(
        "DELETE FROM social_notifications WHERE sender_id = ?1 AND recipient_id = ?2 AND kind = ?3",
        [sender, recipient, NotificationKind::FriendRequest as i64],
    )?;
    Ok(())
}

fn block_account(db: &Connection, actor: i64, account: i64) -> Result<()> {
    check_other(actor, account)?;
    if !account_exists(db, account)? {
        return Err(SocialError::Rule("That account no longer exists."));
    }
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM social_blocks WHERE blocker_id = ?1 AND blocked_id = ?2)",
        [actor, account],
        |row| row.get(0),
    )?;
    if !exists {
        let blocked: usize = db.query_row(
            "SELECT COUNT(*) FROM social_blocks WHERE blocker_id = ?1",
            [actor],
            |row| row.get(0),
        )?;
        if blocked >= MAX_BLOCKED {
            return Err(SocialError::Rule("Your blocked account list is full."));
        }
        db.execute(
            "INSERT INTO social_blocks (blocker_id, blocked_id) VALUES (?1, ?2)",
            [actor, account],
        )?;
    }
    delete_request(db, actor, account)?;
    delete_request(db, account, actor)?;
    revoke_friendship(db, actor, account)?;
    Ok(())
}

fn revoke_friendship(db: &Connection, actor: i64, account: i64) -> rusqlite::Result<()> {
    let (a, b) = pair(actor, account);
    db.execute(
        "DELETE FROM social_friends WHERE a_id = ?1 AND b_id = ?2",
        [a, b],
    )?;
    db.execute(
        "UPDATE social_messages SET archived = 1
         WHERE sender_id IN (?1, ?2) AND recipient_id IN (?1, ?2) AND archived = 0",
        [a, b],
    )?;
    db.execute(
        "DELETE FROM social_notifications
         WHERE sender_id IN (?1, ?2) AND recipient_id IN (?1, ?2)",
        [a, b],
    )?;
    Ok(())
}

fn message_friend(db: &Connection, actor: i64, account: i64, text: &str, now: i64) -> Result<()> {
    check_friend(db, actor, account)?;
    let text = printable(
        text,
        400,
        "Enter a message with 1 to 400 printable characters.",
    )?;
    let lower = text.to_ascii_lowercase();
    if ["http:", "https:", "www.", "discord.gg/"]
        .iter()
        .any(|link| lower.contains(link))
    {
        return Err(SocialError::Rule("Messages cannot contain links."));
    }
    rate(db, actor, "message", 12, now)?;
    db.execute(
        "INSERT INTO social_messages (sender_id, recipient_id, body, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![actor, account, text, now],
    )?;
    db.execute(
        "DELETE FROM social_messages
         WHERE sender_id = ?1 AND id < (
             SELECT id FROM social_messages WHERE sender_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2
         )",
        params![actor, MESSAGE_HISTORY - 1],
    )?;
    add_notice(db, actor, account, NotificationKind::Message, None, now)
}

fn invite_friend(db: &Connection, actor: i64, account: i64, address: &str, now: i64) -> Result<()> {
    check_friend(db, actor, account)?;
    let address = address.trim();
    if !valid_invite_address(address) {
        return Err(SocialError::Rule(
            "Enter a server address such as localhost:27165.",
        ));
    }
    rate(db, actor, "invite", 8, now)?;
    add_notice(
        db,
        actor,
        account,
        NotificationKind::ServerInvite,
        Some(address),
        now,
    )
}

pub(crate) fn valid_invite_address(address: &str) -> bool {
    crate::network::normalize_address(address).is_ok()
}

fn add_notice(
    db: &Connection,
    sender: i64,
    recipient: i64,
    kind: NotificationKind,
    payload: Option<&str>,
    now: i64,
) -> Result<()> {
    db.execute(
        "INSERT INTO social_notifications (sender_id, recipient_id, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![sender, recipient, kind as i64, payload, now],
    )?;
    db.execute(
        "DELETE FROM social_notifications
         WHERE recipient_id = ?1 AND id < (
             SELECT id FROM social_notifications
             WHERE recipient_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2
         )",
        params![recipient, NOTIFICATION_HISTORY - 1],
    )?;
    Ok(())
}

fn rate(db: &Connection, actor: i64, operation: &str, maximum: i64, now: i64) -> Result<()> {
    let hits: i64 = db.query_row(
        "INSERT INTO social_rates (account_id, operation, started_at, hits) VALUES (?1, ?2, ?3, 1)
         ON CONFLICT (account_id, operation) DO UPDATE SET
           hits = CASE WHEN started_at <= ?3 - 60 THEN 1 ELSE hits + 1 END,
           started_at = CASE WHEN started_at <= ?3 - 60 THEN ?3 ELSE started_at END
         RETURNING hits",
        params![actor, operation, now],
        |row| row.get(0),
    )?;
    if hits > maximum {
        return Err(SocialError::Rule(
            "Too many requests. Wait a minute and try again.",
        ));
    }
    Ok(())
}

fn account_exists(db: &Connection, account: i64) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM accounts WHERE id = ?1)",
        [account],
        |row| row.get(0),
    )
}

fn pair(actor: i64, account: i64) -> (i64, i64) {
    (actor.min(account), actor.max(account))
}

fn are_friends(db: &Connection, actor: i64, account: i64) -> rusqlite::Result<bool> {
    let (a, b) = pair(actor, account);
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM social_friends WHERE a_id = ?1 AND b_id = ?2)",
        [a, b],
        |row| row.get(0),
    )
}

fn friend_count(db: &Connection, account: i64) -> rusqlite::Result<usize> {
    db.query_row(
        "SELECT COUNT(*) FROM social_friends WHERE a_id = ?1 OR b_id = ?1",
        [account],
        |row| row.get(0),
    )
}

fn has_request(db: &Connection, sender: i64, recipient: i64) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM social_requests WHERE sender_id = ?1 AND recipient_id = ?2)",
        [sender, recipient],
        |row| row.get(0),
    )
}

fn check_unblocked(db: &Connection, actor: i64, account: i64) -> Result<()> {
    let blocked: bool = db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM social_blocks
            WHERE (blocker_id = ?1 AND blocked_id = ?2) OR (blocker_id = ?2 AND blocked_id = ?1)
         )",
        [actor, account],
        |row| row.get(0),
    )?;
    if blocked {
        return Err(SocialError::Rule("That account could not be contacted."));
    }
    Ok(())
}

fn check_friend(db: &Connection, actor: i64, account: i64) -> Result<()> {
    check_other(actor, account)?;
    check_unblocked(db, actor, account)?;
    if !are_friends(db, actor, account)? {
        return Err(SocialError::Rule(
            "Only friends can message and invite each other.",
        ));
    }
    Ok(())
}

fn check_other(actor: i64, account: i64) -> Result<()> {
    if account <= 0 || actor == account {
        return Err(SocialError::Rule("Choose another account."));
    }
    Ok(())
}

fn check_notification(notification: i64) -> Result<()> {
    if notification <= 0 {
        return Err(SocialError::Rule("Choose a notification."));
    }
    Ok(())
}

fn printable<'a>(value: &'a str, limit: usize, problem: &'static str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().take(limit + 1).count() > limit
        || value.chars().any(|ch| {
            ch.is_control()
                || matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
    {
        return Err(SocialError::Rule(problem));
    }
    Ok(value)
}

fn fetch(db: &Connection, actor: i64, cutoff: i64, now: i64) -> Result<SocialState> {
    let mut friend_query = db.prepare(
        "SELECT a.id, a.name, p.activity, p.seen_at FROM social_friends f
         JOIN accounts a ON a.id = CASE WHEN f.a_id = ?1 THEN f.b_id ELSE f.a_id END
         LEFT JOIN social_presence p ON p.account_id = a.id
         WHERE f.a_id = ?1 OR f.b_id = ?1 ORDER BY a.name COLLATE NOCASE, a.id",
    )?;
    let friends = friend_query
        .query_map([actor], |row| {
            let mut friend = user(row, 0)?;
            let last_seen: Option<i64> = row.get(3)?;
            let activity = if last_seen.is_some_and(|seen| now - seen < PRESENCE_SECONDS) {
                let raw: Option<String> = row.get(2)?;
                raw.as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?
            } else {
                None
            };
            friend.presence = Some(PlayerPresence {
                activity,
                last_seen,
            });
            Ok(friend)
        })?
        .collect::<rusqlite::Result<_>>()?;
    let incoming = users(
        db,
        "SELECT a.id, a.name FROM social_requests r JOIN accounts a ON a.id = r.sender_id
         WHERE r.recipient_id = ?1 AND r.created_at >= ?2 ORDER BY r.created_at DESC, a.id",
        [actor, cutoff],
    )?;
    let outgoing = users(
        db,
        "SELECT a.id, a.name FROM social_requests r JOIN accounts a ON a.id = r.recipient_id
         WHERE r.sender_id = ?1 AND r.created_at >= ?2 ORDER BY r.created_at DESC, a.id",
        [actor, cutoff],
    )?;
    let blocked = users(
        db,
        "SELECT a.id, a.name FROM social_blocks b JOIN accounts a ON a.id = b.blocked_id
         WHERE b.blocker_id = ?1 ORDER BY a.name COLLATE NOCASE, a.id",
        [actor],
    )?;
    let mut notice_query = db.prepare(
        "SELECT n.id, a.id, a.name, n.kind, n.payload, n.created_at, n.is_read
         FROM social_notifications n JOIN accounts a ON a.id = n.sender_id
         WHERE n.recipient_id = ?1 AND n.created_at >= ?2 ORDER BY n.id DESC LIMIT ?3",
    )?;
    let notifications = notice_query
        .query_map([actor, cutoff, FETCH_LIMIT], |row| {
            let kind = match row.get::<_, i64>(3)? {
                1 => NotificationKind::FriendRequest,
                2 => NotificationKind::FriendAccepted,
                3 => NotificationKind::Message,
                4 => NotificationKind::ServerInvite,
                unknown => return Err(rusqlite::Error::IntegralValueOutOfRange(3, unknown)),
            };
            Ok(SocialNotification {
                id: row.get(0)?,
                from: user(row, 1)?,
                kind,
                text: row.get(4)?,
                created_at: row.get(5)?,
                read: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(SocialState {
        friends,
        incoming,
        outgoing,
        blocked,
        notifications,
        messages: messages(db, actor, cutoff, false)?,
        archived_messages: messages(db, actor, cutoff, true)?,
        can_review_message_reports: false,
    })
}

fn messages(
    db: &Connection,
    actor: i64,
    cutoff: i64,
    archived: bool,
) -> Result<Vec<DirectMessage>> {
    let mut query = db.prepare(
        "SELECT m.id, sender.id, sender.name, recipient.id, recipient.name, m.body, m.created_at
         FROM social_messages m
         JOIN accounts sender ON sender.id = m.sender_id
         JOIN accounts recipient ON recipient.id = m.recipient_id
         WHERE (m.recipient_id = ?1 OR (?4 = 0 AND m.sender_id = ?1))
           AND m.created_at >= ?2 AND m.archived = ?4
         ORDER BY m.id DESC LIMIT ?3",
    )?;
    let messages = query
        .query_map(params![actor, cutoff, FETCH_LIMIT, archived], |row| {
            Ok(DirectMessage {
                id: row.get(0)?,
                from: user(row, 1)?,
                to: user(row, 3)?,
                text: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(messages)
}

fn users(db: &Connection, sql: &str, parameters: impl Params) -> rusqlite::Result<Vec<SocialUser>> {
    let mut statement = db.prepare(sql)?;
    let rows = statement.query_map(parameters, |row| user(row, 0))?;
    rows.collect()
}

fn user(row: &Row<'_>, start: usize) -> rusqlite::Result<SocialUser> {
    Ok(SocialUser {
        id: row.get(start)?,
        name: row.get(start + 1)?,
        presence: None,
    })
}

pub(crate) fn update_presence(
    db: &Connection,
    actor: i64,
    session: &[u8],
    activity: &PlayerActivity,
    active: bool,
    now: i64,
) -> Result<Playtime> {
    let playing = !matches!(activity, PlayerActivity::Menu);
    let activity = serde_json::to_string(activity)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    // The timestamp and duration come from the server. One row per account prevents
    // concurrent clients from accumulating overlapping intervals.
    db.execute(
        "INSERT INTO social_presence(account_id,session,activity,seen_at) VALUES (?1,?2,?3,?4)
         ON CONFLICT(account_id) DO UPDATE SET
             seconds = seconds + CASE WHEN ?5 AND session=?2 AND activity=?3
                 AND ?4 - seen_at BETWEEN 1 AND ?6 THEN ?4 - seen_at ELSE 0 END,
             since = coalesce(since, CASE WHEN ?5 AND session=?2 AND activity=?3
                 AND ?4 - seen_at BETWEEN 1 AND ?6 THEN seen_at END),
             session=excluded.session,activity=excluded.activity,seen_at=excluded.seen_at",
        params![
            actor,
            session,
            activity,
            now,
            playing && active,
            PLAYTIME_GAP_SECONDS
        ],
    )?;
    playtime(db, actor, true)
}

pub(crate) fn leave_presence(db: &Connection, session: &[u8], now: i64) -> Result<()> {
    db.execute(
        "UPDATE social_presence SET activity=NULL, session=NULL, seen_at=?2 WHERE session=?1",
        params![session, now],
    )?;
    Ok(())
}

pub(crate) fn set_playtime_privacy(db: &Connection, actor: i64, hidden: bool) -> Result<Playtime> {
    db.execute(
        "INSERT INTO social_presence(account_id,hidden) VALUES (?1,?2)
         ON CONFLICT(account_id) DO UPDATE SET hidden=excluded.hidden",
        params![actor, hidden],
    )?;
    playtime(db, actor, true)
}

pub(crate) fn playtime(db: &Connection, actor: i64, own: bool) -> Result<Playtime> {
    let (hidden, seconds, since): (bool, u64, Option<i64>) = db
        .query_row(
            "SELECT hidden,seconds,since FROM social_presence WHERE account_id=?1",
            [actor],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .unwrap_or_default();
    let visible = own || !hidden;
    Ok(Playtime {
        hidden,
        seconds: visible.then_some(seconds),
        since: if visible { since } else { None },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_friends_see_presence_and_hidden_playtime_is_redacted() {
        let mut db = database();
        apply(&mut db, 1, SocialAction::Request { name: "Bob".into() }).unwrap();
        apply(&mut db, 2, SocialAction::Accept { account: 1 }).unwrap();
        let activity = PlayerActivity::Room {
            map: "surf_kitsune".into(),
            room: "localhost:27165".into(),
        };
        assert_eq!(
            update_presence(&db, 1, b"one", &activity, true, 100)
                .unwrap()
                .seconds,
            Some(0)
        );
        assert_eq!(
            update_presence(&db, 1, b"one", &activity, true, 160)
                .unwrap()
                .seconds,
            Some(60)
        );
        assert_eq!(
            update_presence(&db, 1, b"two", &activity, true, 200)
                .unwrap()
                .seconds,
            Some(60)
        );
        assert_eq!(
            update_presence(&db, 1, b"two", &activity, false, 260)
                .unwrap()
                .seconds,
            Some(60)
        );
        assert_eq!(
            update_presence(&db, 1, b"two", &activity, true, 400)
                .unwrap()
                .seconds,
            Some(60)
        );
        assert_eq!(
            fetch(&db, 2, 0, 401).unwrap().friends[0]
                .presence
                .as_ref()
                .unwrap()
                .activity,
            Some(activity)
        );
        assert!(fetch(&db, 3, 0, 401).unwrap().friends.is_empty());
        assert!(
            fetch(&db, 2, 0, 581).unwrap().friends[0]
                .presence
                .as_ref()
                .unwrap()
                .activity
                .is_none()
        );
        assert_eq!(
            set_playtime_privacy(&db, 1, true).unwrap().seconds,
            Some(60)
        );
        let public = playtime(&db, 1, false).unwrap();
        assert!(public.hidden);
        assert!(public.seconds.is_none());
        assert!(public.since.is_none());
        assert_eq!(playtime(&db, 1, true).unwrap().since, Some(100));
        leave_presence(&db, b"two", 410).unwrap();
        assert!(
            fetch(&db, 2, 0, 411).unwrap().friends[0]
                .presence
                .as_ref()
                .unwrap()
                .activity
                .is_none()
        );
    }

    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT UNIQUE COLLATE NOCASE);
             INSERT INTO accounts VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Carol');",
        )
        .unwrap();
        init(&db).unwrap();
        db
    }

    fn request_bob(db: &mut Connection) -> SocialState {
        apply(
            db,
            1,
            SocialAction::Request {
                name: "bob".to_owned(),
            },
        )
        .unwrap()
    }

    fn befriend(db: &mut Connection) {
        request_bob(db);
        apply(db, 2, SocialAction::Accept { account: 1 }).unwrap();
    }

    #[test]
    fn directed_request_requires_recipient_and_notifies_once() {
        let mut db = database();
        assert_eq!(request_bob(&mut db).outgoing[0].id, 2);
        request_bob(&mut db);
        assert!(apply(&mut db, 3, SocialAction::Accept { account: 1 }).is_err());
        assert!(
            apply(
                &mut db,
                1,
                SocialAction::Message {
                    account: 2,
                    text: "Hi!".to_owned(),
                }
            )
            .is_err()
        );
        let bob = apply(&mut db, 2, SocialAction::Fetch).unwrap();
        assert_eq!(bob.incoming[0].id, 1);
        assert_eq!(bob.notifications.len(), 1);
        let bob = apply(&mut db, 2, SocialAction::Accept { account: 1 }).unwrap();
        assert_eq!(bob.friends[0].id, 1);
        assert!(bob.incoming.is_empty());
        assert!(bob.notifications.is_empty());
        let alice = apply(&mut db, 1, SocialAction::Fetch).unwrap();
        assert_eq!(alice.friends[0].id, 2);
        assert_eq!(
            alice.notifications[0].kind,
            NotificationKind::FriendAccepted
        );
    }

    #[test]
    fn only_friends_can_message_and_invite_and_block_revokes_access() {
        let mut db = database();
        befriend(&mut db);
        apply(
            &mut db,
            1,
            SocialAction::Message {
                account: 2,
                text: "Nice route.".to_owned(),
            },
        )
        .unwrap();
        assert!(
            apply(
                &mut db,
                1,
                SocialAction::Invite {
                    account: 2,
                    address: "https://example.com".to_owned(),
                }
            )
            .is_err()
        );
        apply(
            &mut db,
            1,
            SocialAction::Invite {
                account: 2,
                address: "[::1]:27165".to_owned(),
            },
        )
        .unwrap();
        let bob = apply(&mut db, 2, SocialAction::Fetch).unwrap();
        assert_eq!(bob.messages[0].text, "Nice route.");
        assert_eq!(bob.notifications[0].text.as_deref(), Some("[::1]:27165"));
        let notice = bob.notifications[0].id;
        apply(
            &mut db,
            3,
            SocialAction::Dismiss {
                notification: notice,
            },
        )
        .unwrap();
        let bob = apply(
            &mut db,
            2,
            SocialAction::MarkRead {
                notification: notice,
            },
        )
        .unwrap();
        assert!(bob.notifications[0].read);
        let bob = apply(&mut db, 2, SocialAction::Block { account: 1 }).unwrap();
        assert_eq!(bob.blocked[0].id, 1);
        assert!(bob.friends.is_empty() && bob.messages.is_empty() && bob.notifications.is_empty());
        assert_eq!(bob.archived_messages[0].text, "Nice route.");
        assert!(
            apply(&mut db, 1, SocialAction::Fetch)
                .unwrap()
                .archived_messages
                .is_empty()
        );
        assert!(
            apply(
                &mut db,
                1,
                SocialAction::Request {
                    name: "Bob".to_owned()
                }
            )
            .is_err()
        );
        assert!(
            apply(
                &mut db,
                1,
                SocialAction::Invite {
                    account: 2,
                    address: "localhost:27165".to_owned(),
                }
            )
            .is_err()
        );
    }

    #[test]
    fn deleting_account_cascades_friendship_and_private_data() {
        let mut db = database();
        befriend(&mut db);
        apply(
            &mut db,
            1,
            SocialAction::Message {
                account: 2,
                text: "Good run.".to_owned(),
            },
        )
        .unwrap();
        db.execute("DELETE FROM accounts WHERE id = 1", []).unwrap();
        let bob = apply(&mut db, 2, SocialAction::Fetch).unwrap();
        assert_eq!(bob, SocialState::default());
        for table in [
            "social_friends",
            "social_messages",
            "social_notifications",
            "social_rates",
        ] {
            let query = format!("SELECT COUNT(*) FROM {table}");
            assert_eq!(
                db.query_row(&query, [], |row| row.get::<_, u64>(0))
                    .unwrap(),
                0
            );
        }
    }
}
