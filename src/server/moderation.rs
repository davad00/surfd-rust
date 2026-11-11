use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::network::{ROOM_REPORT_PAGE_SIZE, RoomReport, RoomReportPage};

const VERSION: u8 = 1;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REPORTS: usize = 256;
pub(super) const REPORT_RETENTION: i64 = 7 * 24 * 60 * 60;

#[derive(Clone, Default, Deserialize, Serialize)]
pub(super) struct Sanctions {
    pub(super) ip_bans: BTreeSet<IpAddr>,
    pub(super) account_bans: BTreeSet<i64>,
    pub(super) mutes: BTreeMap<i64, Option<i64>>,
    #[serde(default)]
    reports: Vec<RoomReport>,
}

impl Sanctions {
    pub(super) fn muted(&self, account: i64, now: i64) -> bool {
        self.mutes
            .get(&account)
            .is_some_and(|until| until.is_none_or(|until| until > now))
    }

    pub(super) fn push_report(&mut self, report: RoomReport) {
        self.reports.push(report);
        let excess = self.reports.len().saturating_sub(MAX_REPORTS);
        self.reports.drain(..excess);
    }

    fn prune_expired(&mut self, now: i64) {
        self.mutes
            .retain(|_, until| until.is_none_or(|until| until > now));
        self.reports
            .retain(|report| report.reported_at > now - REPORT_RETENTION);
    }

    fn has_expired(&self, now: i64) -> bool {
        self.mutes
            .values()
            .any(|until| until.is_some_and(|until| until <= now))
            || self
                .reports
                .iter()
                .any(|report| report.reported_at <= now - REPORT_RETENTION)
    }
}

#[derive(Deserialize, Serialize)]
struct Snapshot {
    version: u8,
    room: String,
    sanctions: Sanctions,
}

#[derive(Default)]
pub(super) struct Moderation {
    pub(super) sanctions: Sanctions,
    storage: Option<Storage>,
}

struct Storage {
    path: PathBuf,
    room: String,
    _lock: File,
}

impl Moderation {
    pub(super) fn open(path: &Path, room: &str, now: i64) -> io::Result<Self> {
        let parent = parent_dir(path);
        fs::create_dir_all(parent)?;
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "moderation data needs a file name",
            )
        })?;
        let mut lock_name = name.to_os_string();
        lock_name.push(".lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(parent.join(lock_name))?;
        lock.try_lock()
            .map_err(|error| io::Error::other(format!("cannot lock moderation data: {error}")))?;
        let sanctions = match File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_STATE_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "moderation data is too large",
                    ));
                }
                let snapshot: Snapshot =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if snapshot.version != VERSION || snapshot.room != room {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "moderation data belongs to another room or version",
                    ));
                }
                snapshot.sanctions
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Sanctions::default(),
            Err(error) => return Err(error),
        };
        let mut moderation = Self {
            sanctions,
            storage: Some(Storage {
                path: path.to_owned(),
                room: room.to_owned(),
                _lock: lock,
            }),
        };
        moderation.purge_expired(now)?;
        Ok(moderation)
    }

    pub(super) fn purge_expired(&mut self, now: i64) -> io::Result<()> {
        if self.sanctions.has_expired(now) {
            self.update(|_| {}, now)?;
        }
        Ok(())
    }

    pub(super) fn reports(&self, offset: u16, now: i64) -> RoomReportPage {
        let active = self
            .sanctions
            .reports
            .iter()
            .rev()
            .filter(|report| report.reported_at > now - REPORT_RETENTION);
        let total = active.clone().count() as u16;
        let offset = offset.min(total);
        let reports = active
            .skip(usize::from(offset))
            .take(usize::from(ROOM_REPORT_PAGE_SIZE))
            .cloned()
            .collect();
        RoomReportPage {
            offset,
            total,
            reports,
        }
    }

    pub(super) fn update(
        &mut self,
        change: impl FnOnce(&mut Sanctions),
        now: i64,
    ) -> io::Result<()> {
        let Some(storage) = &self.storage else {
            self.sanctions.prune_expired(now);
            change(&mut self.sanctions);
            return Ok(());
        };
        let mut next = self.sanctions.clone();
        next.prune_expired(now);
        change(&mut next);
        let snapshot = Snapshot {
            version: VERSION,
            room: storage.room.clone(),
            sanctions: next,
        };
        let bytes = serde_json::to_vec(&snapshot).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(io::Error::other("moderation data is too large"));
        }
        write_atomic(&storage.path, &bytes)?;
        self.sanctions = snapshot.sanctions;
        Ok(())
    }
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let filename = format!(".moderation-{:032x}.tmp", u128::from_ne_bytes(nonce));
    let parent = parent_dir(path);
    let temporary = parent.join(filename);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    #[cfg(unix)]
    if result.is_ok() {
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{RoomChatExcerpt, RoomReportPlayer};

    #[test]
    fn sanctions_survive_restart_and_expired_mutes_do_not() {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "surf-moderation-{:032x}",
            u128::from_ne_bytes(nonce)
        ));
        let path = directory.join("room.json");
        let mut store = Moderation::open(&path, "eu", 100).unwrap();
        store
            .update(
                |state| {
                    state.account_bans.insert(5);
                    state.ip_bans.insert("192.168.0.2".parse().unwrap());
                    state.mutes.insert(7, Some(300));
                    state.mutes.insert(8, Some(150));
                    state.mutes.insert(9, None);
                    state.push_report(RoomReport {
                        reporter: RoomReportPlayer {
                            account_id: Some(1),
                            name: "Reporter".into(),
                        },
                        target: RoomReportPlayer {
                            account_id: Some(2),
                            name: "Target".into(),
                        },
                        reason: "Room chat".into(),
                        reported_at: 100,
                        excerpts: vec![RoomChatExcerpt {
                            sent_at: 99,
                            text: "Example chat".into(),
                        }],
                    });
                },
                100,
            )
            .unwrap();
        assert!(Moderation::open(&path, "eu", 100).is_err());
        drop(store);
        assert!(Moderation::open(&path, "na", 200).is_err());
        let mut store = Moderation::open(&path, "eu", 200).unwrap();
        assert!(store.sanctions.account_bans.contains(&5));
        assert!(
            store
                .sanctions
                .ip_bans
                .contains(&"192.168.0.2".parse().unwrap())
        );
        assert!(store.sanctions.muted(7, 200));
        assert!(!store.sanctions.muted(8, 200));
        assert!(store.sanctions.muted(9, 200));
        assert_eq!(
            store.reports(0, 200).reports[0].excerpts[0].text,
            "Example chat"
        );
        store.purge_expired(100 + REPORT_RETENTION).unwrap();
        assert_eq!(store.reports(0, 100 + REPORT_RETENTION).total, 0);
        store
            .update(
                |state| {
                    state.account_bans.remove(&5);
                },
                200,
            )
            .unwrap();
        drop(store);
        let store = Moderation::open(&path, "eu", 200).unwrap();
        assert!(!store.sanctions.account_bans.contains(&5));
        assert_eq!(store.reports(0, 200).total, 0);
        drop(store);
        fs::remove_file(path).unwrap();
        fs::remove_file(directory.join("room.json.lock")).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
