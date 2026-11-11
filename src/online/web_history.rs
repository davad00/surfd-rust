use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::surfd_history::SurfdHistory;

const CACHE_VERSION: u32 = 1;
const MAX_CACHE_BYTES: u64 = 4 * 1024 * 1024 + 4096;

#[derive(Default)]
pub struct WebHistory {
    pub visible: bool,
    pub input: String,
    pub status: String,
    pub snapshot: Option<SurfdHistory>,
    pub page: usize,
    pub busy: bool,
    pub show_scores: bool,
    pub details: Option<RunDetails>,
    scope: Option<Scope>,
}

pub struct RunDetails {
    pub run_id: String,
    pub splits: RunSplits,
}

pub enum RunSplits {
    Loading,
    Loaded(Vec<u64>),
    Error(String),
}

impl WebHistory {
    pub fn open(&mut self, directory: &Path, origin: &str, owner: i64) {
        self.visible = true;
        let scope = (owner > 0 && !origin.is_empty()).then(|| Scope::new(directory, origin, owner));
        if self.scope == scope {
            return;
        }
        let saved = scope.as_ref().map(Scope::load).transpose();
        *self = Self {
            visible: true,
            scope,
            ..Self::default()
        };
        match saved.map(Option::flatten) {
            Ok(Some(snapshot)) => {
                self.input = player_link(&snapshot.source_user_id);
                self.snapshot = Some(snapshot);
                self.status = "Showing saved Surfd web history.".into();
            }
            Ok(None) => {}
            Err(error) => {
                self.status = format!("Could not load saved Surfd web history ({error}).")
            }
        }
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    pub fn save(&mut self, snapshot: SurfdHistory) {
        self.input = player_link(&snapshot.source_user_id);
        self.page = 0;
        self.busy = false;
        self.details = None;
        self.status = match self.scope.as_ref() {
            Some(scope) => match scope.store(&snapshot) {
                Ok(()) => "Imported Surfd web history.".into(),
                Err(error) => format!("Surfd web history loaded. Local save failed ({error})."),
            },
            None => "Surfd web history loaded for this session.".into(),
        };
        self.snapshot = Some(snapshot);
    }

    pub fn forget(&mut self) {
        if let Some(scope) = &self.scope
            && let Err(error) = scope.remove()
        {
            self.status = format!("Could not remove saved Surfd web history ({error}).");
            return;
        }
        self.input.clear();
        self.snapshot = None;
        self.page = 0;
        self.busy = false;
        self.show_scores = false;
        self.details = None;
        self.status = "Forgot saved Surfd web history.".into();
    }

    pub fn purge(directory: &Path, origin: &str, owner: i64) -> Result<(), String> {
        Scope::new(directory, origin, owner)
            .remove()
            .map_err(|error| format!("Could not remove saved Surfd web history ({error})."))
    }
}

#[derive(PartialEq, Eq)]
struct Scope {
    path: PathBuf,
    owner: i64,
    origin_hash: String,
}

#[derive(Deserialize, Serialize)]
struct SavedHistory<T> {
    version: u32,
    owner: i64,
    origin_hash: String,
    snapshot: T,
}

impl Scope {
    fn new(directory: &Path, origin: &str, owner: i64) -> Self {
        let origin_hash = format!("{:x}", Sha256::digest(origin.as_bytes()));
        let path = directory
            .join("web-history")
            .join(format!("{owner}-{origin_hash}.json"));
        Self {
            path,
            owner,
            origin_hash,
        }
    }

    fn load(&self) -> io::Result<Option<SurfdHistory>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        let size = file.take(MAX_CACHE_BYTES + 1).read_to_end(&mut bytes)?;
        if size as u64 > MAX_CACHE_BYTES {
            return Err(io::Error::other("saved history exceeds its size limit"));
        }
        let saved: SavedHistory<SurfdHistory> = serde_json::from_slice(&bytes)
            .map_err(|_| io::Error::other("saved history could not be read"))?;
        if saved.version != CACHE_VERSION
            || saved.owner != self.owner
            || saved.origin_hash != self.origin_hash
            || !is_player_id(&saved.snapshot.source_user_id)
        {
            return Err(io::Error::other(
                "saved history does not match this account",
            ));
        }
        Ok(Some(saved.snapshot))
    }

    fn store(&self, snapshot: &SurfdHistory) -> io::Result<()> {
        if !is_player_id(&snapshot.source_user_id) {
            return Err(io::Error::other("invalid Surfd player ID"));
        }
        let saved = SavedHistory {
            version: CACHE_VERSION,
            owner: self.owner,
            origin_hash: self.origin_hash.clone(),
            snapshot,
        };
        let bytes = serde_json::to_vec(&saved).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_CACHE_BYTES {
            return Err(io::Error::other("history exceeds its local size limit"));
        }
        let directory = self.path.parent().expect("history cache directory");
        fs::create_dir_all(directory)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(io::Error::other)?;
        let temporary = self
            .path
            .with_extension(format!("{:032x}.tmp", u128::from_ne_bytes(nonce)));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        let result = file.write_all(&bytes).and_then(|()| file.sync_all());
        drop(file);
        let result = result.and_then(|()| fs::rename(&temporary, &self.path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        #[cfg(unix)]
        if result.is_ok() {
            let _ = File::open(directory).and_then(|directory| directory.sync_all());
        }
        result
    }

    fn remove(&self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn player_link(id: &str) -> String {
    if is_player_id(id) {
        format!("https://surfd.net/?player={}", id.to_ascii_lowercase())
    } else {
        String::new()
    }
}

fn is_player_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "https://records.example.test";
    const OTHER_ORIGIN: &str = "https://records-alt.example.test";
    const PLAYER: &str = "0194ba52-6c97-727d-9b6a-333e0c8f33a1";
    const OTHER_PLAYER: &str = "0194ba52-6c97-727d-9b6a-333e0c8f33a2";

    fn snapshot(id: &str) -> SurfdHistory {
        SurfdHistory {
            source_user_id: id.into(),
            player: None,
            maps: Vec::new(),
            runs: Vec::new(),
            progression_version: 0,
        }
    }

    fn test_directory() -> PathBuf {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "surf-web-history-{:032x}",
            u128::from_ne_bytes(nonce)
        ));
        fs::create_dir(&directory).unwrap();
        directory
    }

    #[test]
    fn web_history_reopens_replaces_and_forgets_a_saved_snapshot() {
        let directory = test_directory();
        let mut history = WebHistory::default();
        history.open(&directory, ORIGIN, 10);
        history.save(snapshot(PLAYER));
        history.close();
        assert!(!history.visible);
        assert_eq!(history.snapshot.as_ref().unwrap().source_user_id, PLAYER);
        history.save(snapshot(OTHER_PLAYER));

        let mut reopened = WebHistory::default();
        reopened.open(&directory, ORIGIN, 10);
        assert!(reopened.visible);
        assert_eq!(
            reopened.input,
            format!("https://surfd.net/?player={OTHER_PLAYER}")
        );
        assert_eq!(
            reopened.snapshot.as_ref().unwrap().source_user_id,
            OTHER_PLAYER
        );
        reopened.page = 2;
        reopened.close();
        reopened.open(&directory, ORIGIN, 10);
        assert_eq!(reopened.page, 2);
        reopened.forget();
        assert!(reopened.snapshot.is_none() && reopened.input.is_empty());
        assert_eq!(reopened.page, 0);
        let mut after_forget = WebHistory::default();
        after_forget.open(&directory, ORIGIN, 10);
        assert!(after_forget.snapshot.is_none() && after_forget.status.is_empty());

        fs::remove_dir(directory.join("web-history")).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn web_history_isolated_by_native_owner_and_records_origin() {
        let directory = test_directory();
        let mut history = WebHistory::default();
        history.open(&directory, ORIGIN, 10);
        history.save(snapshot(PLAYER));
        history.busy = true;
        history.open(&directory, ORIGIN, 11);
        assert!(history.snapshot.is_none() && history.input.is_empty() && !history.busy);
        history.save(snapshot(OTHER_PLAYER));
        history.open(&directory, OTHER_ORIGIN, 10);
        assert!(history.snapshot.is_none());

        let original = Scope::new(&directory, ORIGIN, 10);
        let wrong_origin = Scope::new(&directory, OTHER_ORIGIN, 10);
        fs::copy(&original.path, &wrong_origin.path).unwrap();
        let mut reopened = WebHistory::default();
        reopened.open(&directory, OTHER_ORIGIN, 10);
        assert!(reopened.snapshot.is_none());
        assert!(reopened.status.contains("does not match this account"));
        WebHistory::purge(&directory, OTHER_ORIGIN, 10).unwrap();
        WebHistory::purge(&directory, ORIGIN, 10).unwrap();
        assert!(!original.path.exists());
        history.open(&directory, ORIGIN, 11);
        assert_eq!(
            history.snapshot.as_ref().unwrap().source_user_id,
            OTHER_PLAYER
        );
        WebHistory::purge(&directory, ORIGIN, 11).unwrap();

        fs::remove_dir(directory.join("web-history")).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn web_history_rejects_oversized_cache_and_keeps_snapshot_after_disk_failures() {
        let directory = test_directory();
        let mut history = WebHistory::default();
        let scope = Scope::new(&directory, ORIGIN, 10);
        let cache = directory.join("web-history");
        fs::create_dir(&cache).unwrap();
        File::create(&scope.path)
            .unwrap()
            .set_len(MAX_CACHE_BYTES + 1)
            .unwrap();
        history.open(&directory, ORIGIN, 10);
        assert!(history.snapshot.is_none());
        assert!(history.status.contains("exceeds its size limit"));
        fs::remove_file(&scope.path).unwrap();
        fs::create_dir(&scope.path).unwrap();
        history.save(snapshot(PLAYER));
        assert_eq!(history.snapshot.as_ref().unwrap().source_user_id, PLAYER);
        assert!(history.status.contains("Local save failed"));
        let input = history.input.clone();
        history.forget();
        assert_eq!(history.snapshot.as_ref().unwrap().source_user_id, PLAYER);
        assert_eq!(history.input, input);
        assert!(history.status.contains("Could not remove"));

        fs::remove_dir(scope.path).unwrap();
        history.forget();
        assert!(history.snapshot.is_none() && history.input.is_empty());
        assert_eq!(history.status, "Forgot saved Surfd web history.");
        fs::remove_dir(cache).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
