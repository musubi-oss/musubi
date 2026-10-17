//! Ready-check between "group formed" and "match delivered", and the dodge penalty for members
//! who do not confirm in time.
//!
//! A formed group with a ready-check becomes [`Pending`]: every member gets a deadline and must
//! confirm. When all tickets confirm the match is delivered as usual; when the deadline passes,
//! the tickets that confirmed go back to the pool with their original wait (they lose no
//! priority) and the accounts of the ones that did not get a penalty that keeps them out of the
//! matchmaker — doubling per strike inside the strike window, capped at 8× the base.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use musubi_core::AccountId;

use crate::{Matched, Ticket};

#[derive(Debug, Clone)]
pub struct Pending {
    pub token: String,
    pub match_id: String,
    pub matched: Matched,
    pub deadline: Instant,
    pub ready: HashSet<String>,
    pub penalty: Duration,
}

impl Pending {
    pub fn all_ready(&self) -> bool {
        self.matched
            .tickets
            .iter()
            .all(|t| self.ready.contains(&t.id))
    }
}

/// Outcome of forming a group: deliver it now, or hold it for confirmation.
#[derive(Debug)]
pub enum ReadyStart {
    Deliver(Matched),
    Check(Pending),
}

/// What a ready-check that ran out of time leaves behind.
#[derive(Debug)]
pub struct ReadyExpired {
    pub token: String,
    pub requeued: Vec<Ticket>,
    pub dodged: Vec<Ticket>,
    pub penalty: Duration,
}

/// Window inside which repeated dodges escalate the penalty.
pub const STRIKE_WINDOW: Duration = Duration::from_secs(24 * 3600);
pub const MAX_ESCALATION: u32 = 8;

#[derive(Default)]
pub struct Penalties {
    by_account: HashMap<AccountId, Strike>,
}

struct Strike {
    until: Instant,
    count: u32,
    last: Instant,
}

impl Penalties {
    /// Record a dodge; returns how long the account is kept out from `now`.
    pub fn strike(&mut self, account: AccountId, base: Duration, now: Instant) -> Duration {
        let e = self.by_account.entry(account).or_insert(Strike {
            until: now,
            count: 0,
            last: now,
        });
        if now.duration_since(e.last) > STRIKE_WINDOW {
            e.count = 0;
        }
        e.count += 1;
        e.last = now;
        let factor = 2u32.pow((e.count - 1).min(3)).min(MAX_ESCALATION);
        let d = base * factor;
        e.until = e.until.max(now).max(now + d);
        d
    }

    /// Replica side: adopt the leader's verdict.
    pub fn set(&mut self, account: AccountId, until: Instant) {
        let e = self.by_account.entry(account).or_insert(Strike {
            until,
            count: 1,
            last: until,
        });
        e.until = e.until.max(until);
    }

    pub fn remaining(&self, account: AccountId, now: Instant) -> Option<Duration> {
        let e = self.by_account.get(&account)?;
        (e.until > now).then(|| e.until - now)
    }

    pub fn prune(&mut self, now: Instant) {
        self.by_account
            .retain(|_, e| e.until > now || now.duration_since(e.last) <= STRIKE_WINDOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn penalties_escalate_and_reset() {
        let mut p = Penalties::default();
        let a = AccountId::new();
        let t0 = Instant::now();
        let base = Duration::from_secs(60);
        assert_eq!(p.strike(a, base, t0), base);
        assert_eq!(p.strike(a, base, t0), base * 2);
        assert_eq!(p.strike(a, base, t0), base * 4);
        assert_eq!(p.strike(a, base, t0), base * 8);
        assert_eq!(p.strike(a, base, t0), base * 8);
        assert_eq!(p.remaining(a, t0), Some(base * 8));
        assert_eq!(p.remaining(a, t0 + base * 8), None);
        let later = t0 + STRIKE_WINDOW + Duration::from_secs(1);
        assert_eq!(p.strike(a, base, later), base);
        p.prune(later + STRIKE_WINDOW * 2);
        assert!(p.remaining(a, later).is_none());
    }
}
