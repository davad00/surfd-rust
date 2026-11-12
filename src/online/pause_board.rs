use std::time::{Duration, Instant};

use bevy_surf::record_protocol::Board;

use crate::{menu::Page, view::Game};

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
pub(super) enum ReplayKind {
    Watch,
    Race,
    RecordRoute,
    RecordGhost,
}

impl ReplayKind {
    pub(super) fn is_ghost(self) -> bool {
        matches!(self, Self::Race | Self::RecordGhost)
    }

    fn is_row(self) -> bool {
        matches!(self, Self::Watch | Self::Race)
    }
}

struct Route {
    course_version: u64,
    map: String,
    bonus: Option<u16>,
    origin: String,
    ranked: bool,
}

impl Route {
    fn matches(&self, game: &Game, origin: &str) -> bool {
        self.course_version == game.course_version
            && self.map == game.course.name
            && self.bonus == game.selected_bonus()
            && self.origin == origin
    }

    fn matches_board(&self, board: &Board) -> bool {
        board.map.eq_ignore_ascii_case(&self.map)
            && board.bonus == self.bonus
            && board.stage.is_none()
    }
}

#[derive(Default)]
pub(super) struct PauseBoard {
    route: Option<Route>,
    visible: bool,
    generation: u64,
    requested_at: Option<Instant>,
    loading: bool,
    board: Option<Board>,
    board_status: String,
    replay_status: String,
    replay_request: u64,
    pending_replay: Option<u64>,
    pub(super) ghost_id: Option<String>,
}

impl PauseBoard {
    pub(super) fn observe(
        &mut self,
        game: &Game,
        page: Page,
        origin: &str,
        now: Instant,
    ) -> (bool, Option<(u64, String)>) {
        let visible = page == Page::Pause && game.pending.is_none();
        let changed_route = self
            .route
            .as_ref()
            .is_some_and(|route| !route.matches(game, origin));
        if changed_route {
            self.route = None;
            self.board = None;
            self.requested_at = None;
            self.loading = false;
            self.board_status.clear();
            self.ghost_id = None;
        }
        let changed = self.visible != visible || changed_route;
        if changed {
            self.visible = visible;
            self.generation = self.generation.wrapping_add(1);
            self.pending_replay = None;
            self.replay_status.clear();
            // A response for the closed menu will be ignored. Let the next
            // opening retry if no result for this route has been received yet.
            if !visible && self.loading {
                self.loading = false;
                self.requested_at = None;
                self.board_status.clear();
            }
        }
        if !visible || (!changed && self.route.is_some()) {
            return (changed, None);
        }
        let route = self.route.get_or_insert_with(|| Route {
            course_version: game.course_version,
            map: game.course.name.clone(),
            bonus: game.selected_bonus(),
            origin: origin.to_owned(),
            ranked: game.course.board_revision(game.selected_bonus()).is_some(),
        });
        if !route.ranked {
            self.board_status = "Records are unavailable for this route.".into();
            return (changed, None);
        }
        if self
            .requested_at
            .is_some_and(|at| now.saturating_duration_since(at) < REFRESH_INTERVAL)
        {
            return (changed, None);
        }
        self.requested_at = Some(now);
        self.loading = true;
        self.board_status = "Loading the board…".into();
        let route = format!("/v1/boards/{}/{}", route.map, route.bonus.unwrap_or(0));
        (true, Some((self.generation, route)))
    }

    pub(super) fn accepts(&self, generation: u64) -> bool {
        self.visible && self.generation == generation
    }

    pub(super) fn accepts_for(&self, generation: u64, game: &Game, origin: &str) -> bool {
        self.accepts(generation)
            && self
                .route
                .as_ref()
                .is_some_and(|route| route.matches(game, origin))
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn board(&self) -> Option<&Board> {
        if self.visible {
            self.board.as_ref()
        } else {
            None
        }
    }

    pub(super) fn status(&self) -> &str {
        if self.replay_status.is_empty() {
            &self.board_status
        } else {
            &self.replay_status
        }
    }

    pub(super) fn set_replay_status(&mut self, status: impl Into<String>) {
        self.replay_status = status.into();
    }

    pub(super) fn complete(&mut self, generation: u64, result: Result<Board, String>) {
        if !self.accepts(generation) {
            return;
        }
        self.loading = false;
        match result {
            Ok(board)
                if self
                    .route
                    .as_ref()
                    .is_some_and(|route| route.matches_board(&board)) =>
            {
                self.board = Some(board);
                self.board_status.clear();
            }
            Ok(_) => self.board_status = "Received records for a different route.".into(),
            Err(_) => self.board_status = "Records for this map are unavailable right now.".into(),
        }
    }

    pub(super) fn start_replay(
        &mut self,
        generation: u64,
        id: &str,
        kind: ReplayKind,
    ) -> Option<u64> {
        if !self.accepts(generation)
            || self.replay_pending()
            || !self.board().is_some_and(|board| {
                (!matches!(kind, ReplayKind::Race) || board.bonus.is_none())
                    && board.rows.iter().any(|row| {
                        !row.id.is_empty()
                            && row.id == id
                            && (!kind.is_row() || row.time_us <= 600_000_000)
                    })
            })
        {
            return None;
        }
        self.replay_request = self.replay_request.wrapping_add(1);
        self.pending_replay = Some(self.replay_request);
        self.set_replay_status(if kind.is_ghost() {
            "Loading ghost…"
        } else {
            "Loading replay…"
        });
        Some(self.replay_request)
    }

    pub(super) fn replay_pending(&self) -> bool {
        self.pending_replay.is_some()
    }

    pub(super) fn complete_replay(&mut self, generation: u64, request: u64) -> bool {
        if !self.accepts(generation) || self.pending_replay != Some(request) {
            return false;
        }
        self.pending_replay = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_surf::{
        course::{BonusRoute, Course, PracticeSpawn},
        record_protocol::BoardRow,
    };

    fn board(map: &str, bonus: Option<u16>) -> Board {
        Board {
            map: map.into(),
            bonus,
            stage: None,
            rows: vec![BoardRow {
                id: "ranked-run".into(),
                name: "Test surfer".into(),
                account_id: Some(1),
                time_us: 23_456_000,
                splits_us: Vec::new(),
                checkpoint_speeds: Vec::new(),
                stage_span: None,
            }],
        }
    }

    #[test]
    fn pause_board_reuses_fresh_route_and_drops_previous_origins() {
        let game = Game::new(Course::practice());
        let mut pause = PauseBoard::default();
        let now = Instant::now();
        let first = "https://one.example";
        let second = "https://two.example";
        let (_, initial) = pause.observe(&game, Page::Pause, first, now);
        let (previous, route) = initial.unwrap();
        assert_eq!(route, format!("/v1/boards/{}/0", game.course.name));
        let (_, changed) = pause.observe(&game, Page::Pause, second, now);
        let (current, _) = changed.unwrap();
        assert_ne!(previous, current);
        pause.complete(previous, Ok(board(&game.course.name, None)));
        assert!(pause.board().is_none());
        pause.complete(current, Ok(board(&game.course.name, Some(1))));
        assert!(pause.board().is_none());
        pause.complete(current, Ok(board(&game.course.name, None)));
        assert!(pause.board().is_some());
        pause.observe(&game, Page::Playing, second, now);
        assert!(pause.board().is_none());
        let (_, reopening) = pause.observe(&game, Page::Pause, second, now);
        assert!(reopening.is_none());
        assert!(pause.board().is_some());
        assert!(!pause.accepts(current));
    }

    #[test]
    fn bonus_watch_is_scoped_and_record_ghost_can_use_the_bonus_board() {
        let mut course = Course::practice();
        course.bonus_routes.push(BonusRoute {
            number: 1,
            name: "Bonus 1".into(),
            spawn: PracticeSpawn {
                name: "Bonus 1".into(),
                position: course.spawn,
                yaw: course.spawn_yaw,
                pitch: 0.,
                source_yaw_degrees: None,
            },
            zones: course.zones.clone(),
            revision: "pause-bonus-fixture".into(),
        });
        let mut game = Game::new(course);
        let mut pause = PauseBoard::default();
        let now = Instant::now();
        let origin = "https://records.example";
        let (_, initial) = pause.observe(&game, Page::Pause, origin, now);
        let (main, _) = initial.unwrap();
        pause.complete(main, Ok(board(&game.course.name, None)));
        let old_replay = pause
            .start_replay(main, "ranked-run", ReplayKind::Race)
            .unwrap();

        assert!(game.set_bonus(Some(1)));
        assert!(!pause.accepts_for(main, &game, origin));
        let (_, changed) = pause.observe(&game, Page::Pause, origin, now);
        let (bonus, route) = changed.unwrap();
        assert_eq!(route, format!("/v1/boards/{}/1", game.course.name));
        assert!(!pause.complete_replay(main, old_replay));
        pause.complete(bonus, Ok(board(&game.course.name, Some(1))));
        assert!(
            pause
                .start_replay(bonus, "ranked-run", ReplayKind::Race)
                .is_none()
        );
        let watch = pause
            .start_replay(bonus, "ranked-run", ReplayKind::Watch)
            .unwrap();
        assert!(pause.complete_replay(bonus, watch));
        assert!(
            pause
                .start_replay(bonus, "ranked-run", ReplayKind::RecordGhost)
                .is_some()
        );
    }
}
