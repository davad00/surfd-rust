use std::time::{Duration, Instant};

use bevy_surf::loadout::{AvatarSkin, KnifeFinish, Loadout};

#[derive(Clone, Copy)]
pub(crate) enum Equip {
    Avatar(AvatarSkin),
    Knife(KnifeFinish),
}

impl Equip {
    pub(crate) fn apply(self, loadout: &mut Loadout) {
        match self {
            Self::Avatar(skin) => loadout.avatar_skin = skin,
            Self::Knife(finish) => loadout.knife_finish = finish,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Request {
    Read,
    Save(Loadout),
}

#[derive(Default)]
pub(super) struct AccountLoadout {
    confirmed: Option<Loadout>,
    avatar: Option<AvatarSkin>,
    knife: Option<KnifeFinish>,
    pending: bool,
    retry_at: Option<Instant>,
    failures: u8,
    unavailable: bool,
}

impl AccountLoadout {
    pub(super) fn effective(&self, local: Loadout) -> Loadout {
        let base = self.confirmed.unwrap_or(local);
        Loadout {
            avatar_skin: self.avatar.unwrap_or(base.avatar_skin),
            knife_finish: self.knife.unwrap_or(base.knife_finish),
        }
    }

    pub(super) fn equip(&mut self, equip: Equip) -> bool {
        if self.unavailable {
            return false;
        }
        match equip {
            Equip::Avatar(skin) => self.avatar = Some(skin),
            Equip::Knife(finish) => self.knife = Some(finish),
        }
        if self.failures >= 3 {
            self.failures = 0;
            self.retry_at = None;
        }
        true
    }

    pub(super) fn next_request(&mut self, now: Instant) -> Option<Request> {
        if self.unavailable
            || self.pending
            || self.failures >= 3
            || self.retry_at.is_some_and(|time| now < time)
        {
            return None;
        }
        let request = match self.confirmed {
            None => Request::Read,
            Some(confirmed) => {
                let wanted = self.effective(confirmed);
                if wanted == confirmed {
                    return None;
                }
                Request::Save(wanted)
            }
        };
        self.pending = true;
        Some(request)
    }

    pub(super) fn loaded(&mut self, server: Option<Loadout>, local: Loadout) {
        self.succeeded();
        self.confirmed = Some(server.unwrap_or_default());
        if server.is_none() {
            self.avatar.get_or_insert(local.avatar_skin);
            self.knife.get_or_insert(local.knife_finish);
        }
    }

    pub(super) fn saved(&mut self, loadout: Loadout) {
        self.succeeded();
        self.confirmed = Some(loadout);
        if self.avatar == Some(loadout.avatar_skin) {
            self.avatar = None;
        }
        if self.knife == Some(loadout.knife_finish) {
            self.knife = None;
        }
    }

    fn succeeded(&mut self) {
        self.pending = false;
        self.failures = 0;
        self.retry_at = None;
    }

    pub(super) fn failed(&mut self, now: Instant, retry_after: Option<Duration>) {
        self.pending = false;
        self.failures = if retry_after.is_some() {
            self.failures + 1
        } else {
            3
        };
        self.retry_at = retry_after
            .map(|delay| delay.max(Duration::from_secs(2 * u64::from(self.failures))))
            .and_then(|delay| now.checked_add(delay));
    }

    pub(super) fn use_local(&mut self, local: Loadout) -> Loadout {
        let selected = self.effective(local);
        *self = Self {
            unavailable: true,
            ..Self::default()
        };
        selected
    }

    pub(super) fn status(&self) -> &'static str {
        if self.unavailable {
            "Cosmetics are saved on this PC."
        } else if self.failures >= 3 {
            "Could not sync cosmetics. Equip an item to try again."
        } else if let Some(confirmed) = self.confirmed {
            if self.effective(confirmed) == confirmed {
                "Saved to your account."
            } else {
                "Saving cosmetics to your account…"
            }
        } else {
            "Loading your account cosmetics…"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_during_account_load_preserves_the_other_device_selection() {
        let now = Instant::now();
        let mut state = AccountLoadout::default();
        assert_eq!(state.next_request(now), Some(Request::Read));
        assert!(state.equip(Equip::Avatar(AvatarSkin::Ember)));
        assert_eq!(state.next_request(now), None);
        state.loaded(
            Some(Loadout {
                knife_finish: KnifeFinish::Glacier,
                ..Loadout::default()
            }),
            Loadout::default(),
        );
        assert_eq!(
            state.next_request(now),
            Some(Request::Save(Loadout {
                avatar_skin: AvatarSkin::Ember,
                knife_finish: KnifeFinish::Glacier,
            }))
        );
    }

    #[test]
    fn a_late_save_does_not_overwrite_a_newer_equip() {
        let now = Instant::now();
        let mut state = AccountLoadout::default();
        let local = Loadout {
            knife_finish: KnifeFinish::Glacier,
            ..Loadout::default()
        };
        state.loaded(None, local);
        assert_eq!(state.next_request(now), Some(Request::Save(local)));
        assert!(state.equip(Equip::Knife(KnifeFinish::Ember)));
        state.saved(local);
        assert_eq!(
            state.next_request(now),
            Some(Request::Save(Loadout {
                knife_finish: KnifeFinish::Ember,
                ..local
            }))
        );
    }
}
