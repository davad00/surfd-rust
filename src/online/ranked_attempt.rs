use std::time::{Duration, Instant};

const MAX_ATTEMPT_REQUESTS: u8 = 4;

pub(super) struct RankedAttempt {
    pub(super) epoch: u64,
    pub(super) endpoint: String,
    pub(super) owner: Option<i64>,
    requests: u8,
    ticket: TicketState,
}

enum TicketState {
    Waiting(Instant),
    Fetching,
    Ready(String),
    Unavailable,
}

#[derive(PartialEq, Eq)]
pub(super) enum AttemptProgress {
    Stale,
    Ready,
    Retrying,
    Unavailable,
}

impl RankedAttempt {
    pub(super) fn new(epoch: u64, endpoint: &str, owner: Option<i64>, now: Instant) -> Self {
        Self {
            epoch,
            endpoint: endpoint.trim().trim_end_matches('/').to_owned(),
            owner,
            requests: 0,
            ticket: TicketState::Waiting(now),
        }
    }

    pub(super) fn applies_to(&self, epoch: u64, endpoint: &str, owner: Option<i64>) -> bool {
        self.epoch == epoch
            && self.endpoint == endpoint.trim().trim_end_matches('/')
            && (self.owner.is_none() || self.owner == owner)
    }

    pub(super) fn start_request(&mut self, now: Instant) -> Option<u8> {
        if !matches!(self.ticket, TicketState::Waiting(when) if now >= when) {
            return None;
        }
        self.requests += 1;
        self.ticket = TicketState::Fetching;
        Some(self.requests)
    }

    pub(super) fn complete(
        &mut self,
        epoch: u64,
        request: u8,
        result: Result<String, Option<Duration>>,
        now: Instant,
    ) -> AttemptProgress {
        if self.epoch != epoch
            || self.requests != request
            || !matches!(self.ticket, TicketState::Fetching)
        {
            return AttemptProgress::Stale;
        }
        match result {
            Ok(ticket) => {
                self.ticket = TicketState::Ready(ticket);
                AttemptProgress::Ready
            }
            Err(Some(delay)) if request < MAX_ATTEMPT_REQUESTS => {
                let backoff = Duration::from_secs(1 << (request - 1));
                self.ticket = TicketState::Waiting(now + delay.max(backoff));
                AttemptProgress::Retrying
            }
            Err(_) => {
                self.ticket = TicketState::Unavailable;
                AttemptProgress::Unavailable
            }
        }
    }

    pub(super) fn ticket(&self) -> Option<&str> {
        match &self.ticket {
            TicketState::Ready(ticket) => Some(ticket),
            _ => None,
        }
    }

    pub(super) fn pending(&self) -> bool {
        matches!(self.ticket, TicketState::Waiting(_) | TicketState::Fetching)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_failures_are_backed_off_and_stop_after_four_requests() {
        let mut now = Instant::now();
        let mut attempt = RankedAttempt::new(7, "https://records.example", Some(42), now);
        for request in 1..=MAX_ATTEMPT_REQUESTS {
            assert_eq!(attempt.start_request(now), Some(request));
            assert!(attempt.start_request(now).is_none());
            assert!(
                attempt.complete(6, request, Ok("old run".into()), now) == AttemptProgress::Stale
            );
            let delay = Duration::from_secs(10);
            let progress = attempt.complete(7, request, Err(Some(delay)), now);
            if request < MAX_ATTEMPT_REQUESTS {
                assert!(progress == AttemptProgress::Retrying);
                assert!(
                    attempt
                        .start_request(now + delay - Duration::from_secs(1))
                        .is_none()
                );
            } else {
                assert!(progress == AttemptProgress::Unavailable);
                assert!(!attempt.pending());
            }
            now += delay;
        }
        assert!(attempt.start_request(now).is_none());
        assert!(attempt.ticket().is_none());
    }

    #[test]
    fn ticket_recovery_preserves_origin_and_guest_claiming() {
        let now = Instant::now();
        let mut guest = RankedAttempt::new(5, " https://records.example/ ", None, now);
        assert!(guest.applies_to(5, "https://records.example", Some(42)));
        assert!(!guest.applies_to(5, "https://other.example", Some(42)));
        assert!(!guest.applies_to(6, "https://records.example", Some(42)));
        let first = guest.start_request(now).unwrap();
        assert!(
            guest.complete(5, first, Err(Some(Duration::ZERO)), now) == AttemptProgress::Retrying
        );
        let next = guest.start_request(now + Duration::from_secs(1)).unwrap();
        assert!(guest.complete(5, first, Ok("stale".into()), now) == AttemptProgress::Stale);
        assert!(guest.complete(5, next, Ok("guest-ticket".into()), now) == AttemptProgress::Ready);
        assert_eq!(guest.ticket(), Some("guest-ticket"));

        let mut account = RankedAttempt::new(5, "https://records.example", Some(42), now);
        assert!(!account.applies_to(5, "https://records.example", None));
        assert!(!account.applies_to(5, "https://records.example", Some(43)));
        let request = account.start_request(now).unwrap();
        assert!(account.complete(5, request, Err(None), now) == AttemptProgress::Unavailable);
        assert!(
            account
                .start_request(now + Duration::from_secs(60))
                .is_none()
        );
    }
}
