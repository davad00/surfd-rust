use std::{
    collections::BTreeMap,
    sync::mpsc,
    time::{Duration, Instant},
};

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    Result, Service, internal, remember_ranked_trajectory, replay_course_available, unix_now,
    versioned_revision,
};
use crate::{catalog, course::Course, record_protocol::MAX_REPLAY_BYTES, replay};

const RETRY_SECONDS: i64 = 60 * 60;
const MIN_PAUSE: Duration = Duration::from_secs(1);
// One synchronous worker gets less than 1% of a two-core host on average.
const WORK_PAUSE_RATIO: u32 = 60;

#[derive(Clone, Copy)]
#[repr(i64)]
enum Source {
    Main = 1,
    Stage = 2,
}

impl Source {
    fn record(
        self,
        db: &Connection,
        id: &str,
        inclusive: bool,
    ) -> rusqlite::Result<Option<Record>> {
        let query = match self {
            Self::Main => {
                "SELECT id,account,map,revision,bonus,
                        CASE WHEN typeof(replay)='text' AND length(CAST(replay AS BLOB))<=?3 THEN replay END
                 FROM best_runs
                 WHERE id>=?1 AND (?2 OR id>?1) ORDER BY id LIMIT 1"
            }
            Self::Stage => {
                "WITH candidate AS MATERIALIZED (
                    SELECT id,account,
                           CASE WHEN typeof(replay)='text' AND length(CAST(replay AS BLOB))<=?3 THEN replay END AS replay
                    FROM stage_replays
                    WHERE id>=?1 AND (?2 OR id>?1) ORDER BY id LIMIT 1
                 )
                 SELECT s.id,s.account,b.map,b.revision,b.bonus,s.replay
                 FROM candidate s LEFT JOIN stage_bests b ON b.rowid=(
                    SELECT min(rowid) FROM stage_bests WHERE replay_id=s.id AND account=s.account
                 )"
            }
        };
        db.query_row(query, params![id, inclusive, MAX_REPLAY_BYTES], |row| {
            Ok(Record {
                id: row.get(0)?,
                owner: row.get(1)?,
                board: row
                    .get::<_, Option<String>>(2)?
                    .map(|map| Ok::<_, rusqlite::Error>((map, row.get(3)?, row.get(4)?)))
                    .transpose()?,
                payload: row.get(5)?,
            })
        })
        .optional()
    }

    fn next(self, db: &Connection) -> rusqlite::Result<Option<Record>> {
        let cursor: Option<Option<String>> = db
            .query_row(
                "SELECT cursor FROM ranked_trajectory_backfill WHERE source=?1",
                [self as i64],
                |row| row.get(0),
            )
            .optional()?;
        match cursor {
            Some(None) => Ok(None),
            Some(Some(id)) => self.record(db, &id, false),
            None => self.record(db, "", true),
        }
    }
}

struct Record {
    id: String,
    owner: i64,
    board: Option<(String, String, u16)>,
    payload: Option<String>,
}

enum Outcome {
    Indexed([u8; 32]),
    Skip,
    Unavailable(i64),
}

impl Record {
    fn verify(&self, service: &Service, unavailable: &mut BTreeMap<String, i64>) -> Outcome {
        let (Some((map, revision, bonus)), Some(payload)) = (&self.board, &self.payload) else {
            return Outcome::Skip;
        };
        let bonus = (*bonus != 0).then_some(*bonus);
        if !replay_course_available(map, bonus, revision, None) {
            return Outcome::Skip;
        }
        if bonus.is_none()
            && let Some(current) =
                catalog::find(map).and_then(|entry| entry.native_board_revision())
            && versioned_revision(&current) != *revision
        {
            return Outcome::Skip;
        }
        let Ok(replay) = serde_json::from_str::<replay::RunReplay>(payload) else {
            return Outcome::Skip;
        };
        if replay.version != replay::REPLAY_VERSION
            || &replay.map != map
            || replay.bonus != bonus
            || versioned_revision(&replay.revision) != *revision
        {
            return Outcome::Skip;
        }
        if let Some(&retry_at) = unavailable
            .get(map)
            .filter(|&&retry_at| retry_at > unix_now())
        {
            return Outcome::Unavailable(retry_at);
        }
        let verify = |course: &Course| match replay::verify_ranked_with_trajectory(course, &replay)
        {
            Ok((_, Some(digest))) => Outcome::Indexed(digest),
            _ => Outcome::Skip,
        };
        let cached = service.courses.lock().ok().and_then(|courses| {
            courses
                .iter()
                .find(|(name, _)| name == map)
                .map(|(_, course)| course.clone())
        });
        // Borrow a live cache hit, but avoid evicting popular maps or retaining
        // another collision map for a migration-only cache miss.
        if let Some(course) = cached {
            return verify(&course);
        }
        match catalog::files::load_ranked(&service.maps, map) {
            Ok(course) => verify(&course),
            Err(_) => {
                let retry_at = unix_now() + RETRY_SECONDS;
                unavailable.insert(map.clone(), retry_at);
                Outcome::Unavailable(retry_at)
            }
        }
    }
}

fn record_outcome(
    db: &Connection,
    source: Source,
    id: &str,
    owner: i64,
    outcome: Outcome,
) -> rusqlite::Result<()> {
    if let Outcome::Unavailable(retry_at) = outcome {
        db.execute(
            "INSERT INTO ranked_trajectory_backfill_retry(source,replay_id,retry_at) VALUES (?1,?2,?3)
             ON CONFLICT(source,replay_id) DO UPDATE SET retry_at=excluded.retry_at",
            params![source as i64, id, retry_at],
        )?;
    } else {
        if let Outcome::Indexed(digest) = outcome {
            remember_ranked_trajectory(db, owner, &digest)?;
        }
        db.execute(
            "DELETE FROM ranked_trajectory_backfill_retry WHERE source=?1 AND replay_id=?2",
            params![source as i64, id],
        )?;
    }
    Ok(())
}

enum Pass {
    Complete,
    Stopped,
    RetryAt(i64),
}

fn advance(service: &Service, mut pause: impl FnMut(Duration) -> bool) -> Result<Pass> {
    let mut unavailable = BTreeMap::new();
    for source in [Source::Main, Source::Stage] {
        loop {
            let start = Instant::now();
            let next = {
                let db = service.db.lock().map_err(internal)?;
                source.next(&db)?
            };
            if let Some(record) = next {
                let outcome = record.verify(service, &mut unavailable);
                {
                    let mut db = service.db.lock().map_err(internal)?;
                    let transaction = db.transaction()?;
                    record_outcome(&transaction, source, &record.id, record.owner, outcome)?;
                    transaction.execute(
                        "INSERT INTO ranked_trajectory_backfill(source,cursor) VALUES (?1,?2)
                         ON CONFLICT(source) DO UPDATE SET cursor=excluded.cursor",
                        params![source as i64, record.id],
                    )?;
                    transaction.commit()?;
                }
                if !pause(start.elapsed()) {
                    return Ok(Pass::Stopped);
                }
            } else {
                let db = service.db.lock().map_err(internal)?;
                db.execute(
                    "INSERT INTO ranked_trajectory_backfill(source,cursor) VALUES (?1,NULL)
                     ON CONFLICT(source) DO UPDATE SET cursor=NULL WHERE cursor IS NOT NULL",
                    [source as i64],
                )?;
                break;
            }
        }
    }

    loop {
        let start = Instant::now();
        let next = {
            let db = service.db.lock().map_err(internal)?;
            db.query_row(
                "SELECT source,replay_id,retry_at FROM ranked_trajectory_backfill_retry
                 ORDER BY retry_at,source,replay_id LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?
        };
        let Some((source, id, retry_at)) = next else {
            return Ok(Pass::Complete);
        };
        if retry_at > unix_now() {
            return Ok(Pass::RetryAt(retry_at));
        }
        let source = match source {
            1 => Source::Main,
            2 => Source::Stage,
            _ => return Err(internal("invalid ranked trajectory backfill source")),
        };
        let record = {
            let db = service.db.lock().map_err(internal)?;
            source.record(&db, &id, true)?
        }
        .filter(|record| record.id == id);
        let (owner, outcome) = record.map_or((0, Outcome::Skip), |record| {
            (record.owner, record.verify(service, &mut unavailable))
        });
        {
            let mut db = service.db.lock().map_err(internal)?;
            let transaction = db.transaction()?;
            record_outcome(&transaction, source, &id, owner, outcome)?;
            transaction.commit()?;
        }
        if !pause(start.elapsed()) {
            return Ok(Pass::Stopped);
        }
    }
}

pub(super) fn run(service: &Service, stop: &mpsc::Receiver<()>) {
    let wait = |duration| {
        matches!(
            stop.recv_timeout(duration),
            Err(mpsc::RecvTimeoutError::Timeout)
        )
    };
    while wait(MIN_PAUSE) {
        match advance(service, |work| {
            wait(MIN_PAUSE.max(work.saturating_mul(WORK_PAUSE_RATIO)))
        }) {
            Ok(Pass::Complete) => {
                println!("Ranked trajectory backfill completed");
                break;
            }
            Ok(Pass::Stopped) => break,
            Ok(Pass::RetryAt(retry_at)) => {
                let seconds = retry_at.saturating_sub(unix_now()).max(0) as u64;
                if !wait(Duration::from_secs(seconds)) {
                    break;
                }
            }
            Err(error) => {
                eprintln!("Ranked trajectory backfill paused: {}", error.message);
                if !wait(Duration::from_secs(60)) {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
pub(super) fn run_pass(service: &Service, pause: impl FnMut(Duration) -> bool) -> Result<()> {
    advance(service, pause).map(|_| ())
}
