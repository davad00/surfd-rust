use std::{io::Read, time::Duration};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

const PUBLIC_API: &str = "https://surfd.net/api/surf";
const PROFILE_LINK: &str = "https://surfd.net/?player=";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_HISTORY_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_RUN_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_SPLITS: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePlayer(Selector);

#[derive(Clone, Debug, Eq, PartialEq)]
enum Selector {
    UserId(String),
    Username(String),
}

impl SourcePlayer {
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if let Some(value) = input.strip_prefix(PROFILE_LINK) {
            return user_id(value)
                .map(|id| Self(Selector::UserId(id)))
                .ok_or_else(|| "The Surfd profile link does not contain an account ID".into());
        }
        if let Some(id) = user_id(input) {
            return Ok(Self(Selector::UserId(id)));
        }
        if input.is_empty()
            || input.len() > 80
            || input.contains(['/', '?', '#', ':'])
            || input.chars().any(char::is_control)
        {
            return Err("Paste a Surfd profile link or enter the exact Surfd username".into());
        }
        Ok(Self(Selector::Username(input.to_owned())))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfdHistory {
    pub source_user_id: String,
    pub player: Option<SurfdProgress>,
    pub maps: Vec<SurfdMapPoints>,
    pub runs: Vec<SurfdRun>,
    pub progression_version: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfdProgress {
    pub user_id: String,
    pub username: String,
    pub rank: String,
    pub points: u64,
    pub records: u64,
    pub completions: u64,
    pub position: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfdMapPoints {
    pub map_id: String,
    pub board_id: String,
    pub run_id: String,
    pub points: u64,
    pub time_us: u64,
    pub position: u64,
    pub completion_points: u64,
    pub performance_points: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfdRun {
    pub board_id: String,
    pub map_id: String,
    pub run_id: String,
    pub time_us: u64,
    pub created_at: String,
    pub ticks: u64,
    pub replay_available: bool,
}

#[derive(Deserialize)]
struct PublicProgression {
    player: Option<SurfdProgress>,
    maps: Vec<SurfdMapPoints>,
    version: u64,
}

#[derive(Deserialize)]
struct PublicRankings {
    entries: Vec<SurfdProgress>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicRuns {
    user_id: String,
    runs: Vec<SurfdRun>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicRun {
    run_id: String,
    map_id: String,
    time_us: u64,
    splits_us: Vec<u64>,
}

pub fn fetch_history(source: &SourcePlayer) -> Result<SurfdHistory, String> {
    let agent = public_agent();
    let id = match &source.0 {
        Selector::UserId(id) => id.clone(),
        Selector::Username(name) => {
            let rankings: PublicRankings =
                fetch_public(&agent, "progression", None, MAX_HISTORY_RESPONSE_BYTES)?;
            resolve_name(&rankings, name)?
        }
    };
    let query = Some(("userId", id.as_str()));
    let progression = fetch_public(&agent, "progression", query, MAX_HISTORY_RESPONSE_BYTES)?;
    let runs = fetch_public(&agent, "player-runs", query, MAX_HISTORY_RESPONSE_BYTES)?;
    combine(id, progression, runs)
}

pub fn fetch_run_splits(run: &SurfdRun) -> Result<Vec<u64>, String> {
    if run.run_id.is_empty() || run.run_id.len() > 128 || run.run_id.chars().any(char::is_control) {
        return Err("Invalid Surfd run ID".into());
    }
    let response = fetch_public(
        &public_agent(),
        "run",
        Some(("runId", &run.run_id)),
        MAX_RUN_RESPONSE_BYTES,
    )?;
    validate_splits(run, response)
}

fn validate_splits(run: &SurfdRun, response: PublicRun) -> Result<Vec<u64>, String> {
    if response.run_id != run.run_id
        || response.map_id != run.map_id
        || response.time_us != run.time_us
    {
        return Err("Surfd returned details for a different run".into());
    }
    let splits = response.splits_us;
    if splits.len() > MAX_SPLITS
        || splits.last().is_some_and(|split| *split > run.time_us)
        || splits.windows(2).any(|pair| pair[0] > pair[1])
    {
        return Err("Surfd returned invalid checkpoint splits".into());
    }
    Ok(splits)
}

fn user_id(value: &str) -> Option<String> {
    (value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        }))
    .then(|| value.to_ascii_lowercase())
}

fn resolve_name(rankings: &PublicRankings, name: &str) -> Result<String, String> {
    let mut matches = rankings
        .entries
        .iter()
        .filter(|entry| entry.username.eq_ignore_ascii_case(name));
    let entry = matches
        .next()
        .ok_or("Username is outside the public top 100. Paste the Surfd profile link instead.")?;
    if matches.next().is_some() {
        return Err(
            "More than one public ranking matches that name. Paste the Surfd profile link.".into(),
        );
    }
    user_id(&entry.user_id).ok_or_else(|| "Surfd returned an invalid public account ID".into())
}

fn combine(
    id: String,
    progression: PublicProgression,
    runs: PublicRuns,
) -> Result<SurfdHistory, String> {
    if !runs.user_id.eq_ignore_ascii_case(&id)
        || progression
            .player
            .as_ref()
            .is_some_and(|player| !player.user_id.eq_ignore_ascii_case(&id))
    {
        return Err("Surfd returned public history for a different account".into());
    }
    Ok(SurfdHistory {
        source_user_id: id,
        player: progression.player,
        maps: progression.maps,
        runs: runs.runs,
        progression_version: progression.version,
    })
}

fn public_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into()
}

fn fetch_public<T: DeserializeOwned>(
    agent: &ureq::Agent,
    action: &str,
    query: Option<(&str, &str)>,
    max_response_bytes: usize,
) -> Result<T, String> {
    let request = agent
        .get(PUBLIC_API)
        .query("action", action)
        .header("Accept", "application/json");
    let request = match query {
        Some((key, value)) => request.query(key, value),
        None => request,
    };
    let mut response = request
        .call()
        .map_err(|error| format!("Could not fetch Surfd public history ({error})"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("Surfd public history returned {status}"));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take((max_response_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the Surfd public history response")?;
    if bytes.len() > max_response_bytes {
        return Err("Surfd public history response is too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid Surfd public history response".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const USER: &str = "6413d934-fb4b-4ed7-bf66-2815efa8ce22";
    const OTHER: &str = "b6d7c4bc-93a2-46e6-a917-a9f432164e86";

    fn progress(id: &str, name: &str) -> serde_json::Value {
        json!({
            "userId": id, "username": name, "rank": "Surfer", "points": 681,
            "records": 1, "completions": 3, "position": 64
        })
    }

    #[test]
    fn accepts_copied_profile_links_and_rejects_other_urls() {
        let expected = SourcePlayer(Selector::UserId(USER.to_owned()));
        assert_eq!(
            SourcePlayer::parse(&format!("  {PROFILE_LINK}{}  ", USER.to_ascii_uppercase())),
            Ok(expected.clone())
        );
        assert_eq!(SourcePlayer::parse(USER), Ok(expected));
        for invalid in [
            format!("https://surfd.net.example/?player={USER}"),
            format!("https://surfd.net@invalid.example/?player={USER}"),
            format!("{PROFILE_LINK}{USER}&player={OTHER}"),
            format!("{PROFILE_LINK}not-an-id"),
        ] {
            assert!(SourcePlayer::parse(&invalid).is_err());
        }
    }

    #[test]
    fn uses_exact_unambiguous_public_ranking_names() {
        let rankings: PublicRankings = serde_json::from_value(json!({
            "entries": [progress(USER, "Surf Friend"), progress(OTHER, "Surf Friend 2")]
        }))
        .unwrap();
        assert_eq!(resolve_name(&rankings, "surf friend").unwrap(), USER);
        assert!(resolve_name(&rankings, "surf").is_err());
        let ambiguous: PublicRankings = serde_json::from_value(json!({
            "entries": [progress(USER, "Surfer"), progress(OTHER, "surfer")]
        }))
        .unwrap();
        assert!(resolve_name(&ambiguous, "surfer").is_err());
    }

    #[test]
    fn retains_source_progression_and_run_fields() {
        let progression = serde_json::from_value(json!({
            "player": progress(USER, "Surf Friend"), "version": 14,
            "entries": [],
            "maps": [{
                "mapId": "surf_boreas", "boardId": "current-board", "runId": "source-run",
                "points": 300, "position": 4, "completionPoints": 100,
                "performancePoints": 200, "timeUs": 45306791
            }]
        }))
        .unwrap();
        let runs = serde_json::from_value(json!({
            "userId": USER,
            "runs": [{
                "boardId": "current-board", "mapId": "surf_boreas", "runId": "source-run",
                "timeUs": 45306791, "createdAt": "2026-10-20T08:00:00Z",
                "ticks": 2991, "replayAvailable": false
            }]
        }))
        .unwrap();
        let history = combine(USER.into(), progression, runs).unwrap();
        assert_eq!(history.player.as_ref().unwrap().points, 681);
        assert_eq!(history.maps[0].performance_points, 200);
        assert_eq!(history.runs[0].time_us, 45_306_791);
        assert!(!history.runs[0].replay_available);
        assert_eq!(
            serde_json::from_slice::<SurfdHistory>(&serde_json::to_vec(&history).unwrap()).unwrap(),
            history
        );
    }

    #[test]
    fn preserves_empty_history_and_rejects_another_account() {
        let empty = || serde_json::from_str(r#"{"player":null,"maps":[],"version":1}"#).unwrap();
        let runs = |id: &str| serde_json::from_value(json!({"userId": id, "runs": []})).unwrap();
        let history = combine(USER.into(), empty(), runs(USER)).unwrap();
        assert!(history.player.is_none() && history.maps.is_empty() && history.runs.is_empty());
        assert!(combine(USER.into(), empty(), runs(OTHER)).is_err());
    }

    #[test]
    fn checks_source_run_details_against_the_selected_history_row() {
        let run: SurfdRun = serde_json::from_value(json!({
            "boardId": "current-board", "mapId": "surf_boreas", "runId": "source-run",
            "timeUs": 45306791, "createdAt": "2026-10-20T08:00:00Z",
            "ticks": 2991, "replayAvailable": true
        }))
        .unwrap();
        let response = |run_id: &str, map_id: &str, time_us: u64, splits_us: Vec<u64>| {
            serde_json::from_value(json!({
                "runId": run_id, "mapId": map_id, "timeUs": time_us, "splitsUs": splits_us,
                "mapName": "Boreas", "username": "Surf Friend"
            }))
            .unwrap()
        };
        let exact = |splits| response(&run.run_id, &run.map_id, run.time_us, splits);
        let splits = vec![10_000_000, 25_000_000, 25_000_000];
        assert_eq!(
            validate_splits(&run, exact(splits.clone())).unwrap(),
            splits
        );
        assert!(validate_splits(&run, exact(vec![2, 1])).is_err());
        assert!(validate_splits(&run, exact(vec![run.time_us + 1])).is_err());
        assert!(validate_splits(&run, exact(vec![1; MAX_SPLITS + 1])).is_err());
        assert!(
            validate_splits(
                &run,
                response("another-run", &run.map_id, run.time_us, vec![1])
            )
            .is_err()
        );
        assert!(
            validate_splits(
                &run,
                response(&run.run_id, "surf_ace", run.time_us, vec![1])
            )
            .is_err()
        );
        assert!(
            validate_splits(
                &run,
                response(&run.run_id, &run.map_id, run.time_us + 1, vec![1])
            )
            .is_err()
        );
    }
}
