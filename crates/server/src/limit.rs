//! Counting attempts per client, so that one client cannot lock everybody
//! else out.
//!
//! A [`Limiter`] counts attempts per key within a sliding window: one IPv4
//! address, or one IPv6 /64 (a single machine usually holds a whole /64, so
//! counting single IPv6 addresses would hand it billions of counters). A
//! global ceiling across all keys is the backstop against many addresses at
//! once. An attempt takes its place before the work is done and may be given
//! back afterwards (a right password does not count), so attempts running at
//! the same time cannot all get in before the first one is counted.
//!
//! Memory is bounded: at most `max_keys` keys are tracked, and keys whose
//! attempts have all expired or been given back are dropped (on every access
//! to the key, and in a sweep at least once per window or when the map is
//! full). A tracked key holds at least one live attempt, and live attempts
//! are capped by the global ceiling, so with `max_keys` above the ceiling the
//! map cannot fill up with live keys. Should it be full anyway, a key it does
//! not know is refused (fails closed): keys already tracked keep their own
//! counts, and the refusal lasts only until attempts expire. Letting a new
//! key in uncounted would be the hole an attacker looks for; evicting a live
//! key would reset an attacker's count.
//!
//! The log says when the global ceiling or the key cap refuses someone (at
//! most once a window), with counts only, never an address.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::client_ip::normalize;

/// What attempts are counted by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RateKey {
    V4(u32),
    /// The first 64 bits of the address.
    V6(u64),
    /// No client address is known (the router driven without a listener, as
    /// in tests): all such requests share this one key.
    Unknown,
}

impl RateKey {
    pub fn of(ip: Option<IpAddr>) -> Self {
        match ip.map(normalize) {
            Some(IpAddr::V4(v4)) => RateKey::V4(u32::from(v4)),
            Some(IpAddr::V6(v6)) => RateKey::V6((u128::from(v6) >> 64) as u64),
            None => RateKey::Unknown,
        }
    }
}

/// How many attempts a limiter allows.
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    /// Attempts per key within the window.
    pub per_key: usize,
    /// Attempts from all keys together within the window.
    pub global: usize,
    pub window: Duration,
    /// Keys tracked at once.
    pub max_keys: usize,
}

type Place = (u64, Instant);

#[derive(Default)]
struct Counts {
    keys: HashMap<RateKey, Vec<Place>>,
    /// Every live attempt, whatever its key.
    all: Vec<Place>,
    swept: Option<Instant>,
    warned: Option<Instant>,
}

pub struct Limiter {
    /// For the log: what is being counted.
    name: &'static str,
    rule: Rule,
    counts: Mutex<Counts>,
    next_id: AtomicU64,
}

/// A place in a limiter. It counts (as a failure, say) unless given back.
#[must_use]
pub struct Attempt<'a> {
    limiter: &'a Limiter,
    key: RateKey,
    id: u64,
}

impl Limiter {
    pub fn new(name: &'static str, rule: Rule) -> Self {
        Self {
            name,
            rule,
            counts: Mutex::new(Counts::default()),
            next_id: AtomicU64::new(0),
        }
    }

    /// Takes a place for `key`, or says how many seconds until one may be free.
    pub fn reserve(&self, key: RateKey) -> Result<Attempt<'_>, u64> {
        self.reserve_at(key, Instant::now())
    }

    fn reserve_at(&self, key: RateKey, now: Instant) -> Result<Attempt<'_>, u64> {
        let Rule {
            per_key,
            global,
            window,
            max_keys,
        } = self.rule;
        let live = |(_, at): &Place| now.saturating_duration_since(*at) < window;
        let wait = |places: &[Place]| {
            let oldest = places.iter().map(|(_, at)| *at).min().unwrap_or(now);
            window
                .saturating_sub(now.saturating_duration_since(oldest))
                .as_secs()
                .max(1)
        };
        let mut counts = self.counts.lock().expect("lock");
        let counts = &mut *counts;
        counts.all.retain(live);
        if counts.keys.len() >= max_keys
            || counts
                .swept
                .is_none_or(|at| now.saturating_duration_since(at) >= window)
        {
            counts.keys.retain(|_, places| {
                places.retain(live);
                !places.is_empty()
            });
            counts.swept = Some(now);
        }
        if let Some(places) = counts.keys.get_mut(&key) {
            places.retain(live);
            if places.len() >= per_key {
                return Err(wait(places));
            }
        } else if counts.keys.len() >= max_keys {
            self.warn(
                counts,
                now,
                "it tracks as many clients as it may; new ones are refused until some expire",
            );
            return Err(wait(&counts.all));
        }
        if counts.all.len() >= global {
            self.warn(
                counts,
                now,
                "all clients together reached its ceiling; everyone is refused until the oldest attempts expire",
            );
            return Err(wait(&counts.all));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        counts.keys.entry(key).or_default().push((id, now));
        counts.all.push((id, now));
        Ok(Attempt { limiter: self, key, id })
    }

    fn warn(&self, counts: &mut Counts, now: Instant, what: &str) {
        let window = self.rule.window;
        if counts
            .warned
            .is_none_or(|at| now.saturating_duration_since(at) >= window)
        {
            counts.warned = Some(now);
            tracing::warn!(
                limiter = self.name,
                attempts = counts.all.len(),
                clients = counts.keys.len(),
                window_secs = window.as_secs(),
                "rate limit: {what}"
            );
        }
    }
}

impl Attempt<'_> {
    /// It does not count after all.
    pub fn give_back(self) {
        let mut counts = self.limiter.counts.lock().expect("lock");
        counts.all.retain(|(id, _)| *id != self.id);
        if let Some(places) = counts.keys.get_mut(&self.key) {
            places.retain(|(id, _)| *id != self.id);
            if places.is_empty() {
                counts.keys.remove(&self.key);
            }
        }
    }

    /// It counts (the same as dropping it, said out loud).
    pub fn keep(self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(ip: &str) -> RateKey {
        RateKey::of(Some(ip.parse().unwrap()))
    }

    fn rule(per_key: usize, global: usize, max_keys: usize) -> Rule {
        Rule {
            per_key,
            global,
            window: Duration::from_secs(60),
            max_keys,
        }
    }

    #[test]
    fn keys_are_ipv4_addresses_and_ipv6_slash_64s() {
        assert_eq!(key("192.0.2.1"), key("::ffff:192.0.2.1"));
        assert_ne!(key("192.0.2.1"), key("192.0.2.2"));
        assert_eq!(key("2001:db8:1:2::1"), key("2001:db8:1:2:ffff:ffff:ffff:ffff"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(RateKey::of(None), RateKey::Unknown);
    }

    #[test]
    fn one_client_does_not_lock_out_another() {
        let limiter = Limiter::new("test", rule(5, 50, 100));
        let t = Instant::now();
        for _ in 0..5 {
            limiter.reserve_at(key("203.0.113.1"), t).unwrap().keep();
        }
        let wait = limiter.reserve_at(key("203.0.113.1"), t).err().unwrap();
        assert_eq!(wait, 60);
        limiter.reserve_at(key("203.0.113.2"), t).unwrap().keep();
        // The same /64 is the same client.
        for _ in 0..5 {
            limiter.reserve_at(key("2001:db8::1"), t).unwrap().keep();
        }
        assert!(limiter.reserve_at(key("2001:db8::ffff"), t).is_err());
        assert!(limiter.reserve_at(key("2001:db8:0:1::1"), t).is_ok());
    }

    #[test]
    fn places_given_back_do_not_count_and_old_ones_expire() {
        let limiter = Limiter::new("test", rule(2, 50, 100));
        let t = Instant::now();
        let k = key("203.0.113.1");
        for _ in 0..10 {
            limiter.reserve_at(k, t).unwrap().give_back();
        }
        limiter.reserve_at(k, t).unwrap().keep();
        limiter.reserve_at(k, t + Duration::from_secs(30)).unwrap().keep();
        assert_eq!(limiter.reserve_at(k, t + Duration::from_secs(45)).err(), Some(15));
        assert!(
            limiter.reserve_at(k, t + Duration::from_secs(60)).is_ok(),
            "the first one expired"
        );
        // A key with nothing left is not tracked any more.
        let limiter = Limiter::new("test", rule(2, 50, 100));
        limiter.reserve_at(k, t).unwrap().give_back();
        assert!(limiter.counts.lock().unwrap().keys.is_empty());
    }

    #[test]
    fn the_global_ceiling_stops_many_clients_together() {
        let limiter = Limiter::new("test", rule(5, 12, 100));
        let t = Instant::now();
        for i in 0..12 {
            limiter.reserve_at(key(&format!("198.51.100.{i}")), t).unwrap().keep();
        }
        let wait = limiter.reserve_at(key("203.0.113.200"), t + Duration::from_secs(20));
        assert_eq!(wait.err(), Some(40));
        assert!(
            limiter
                .reserve_at(key("198.51.100.0"), t + Duration::from_secs(20))
                .is_err()
        );
        assert!(
            limiter
                .reserve_at(key("203.0.113.200"), t + Duration::from_secs(60))
                .is_ok()
        );
    }

    #[test]
    fn a_full_map_refuses_new_clients_but_not_known_ones() {
        let limiter = Limiter::new("test", rule(5, 1000, 3));
        let t = Instant::now();
        for i in 0..3 {
            limiter.reserve_at(key(&format!("198.51.100.{i}")), t).unwrap().keep();
        }
        assert!(limiter.reserve_at(key("203.0.113.1"), t).is_err(), "fails closed");
        assert!(
            limiter.reserve_at(key("198.51.100.1"), t).is_ok(),
            "known clients go on"
        );
        // Once the old attempts expire, the sweep makes room.
        let later = t + Duration::from_secs(61);
        assert!(limiter.reserve_at(key("203.0.113.1"), later).is_ok());
        assert_eq!(limiter.counts.lock().unwrap().keys.len(), 1);
    }

    #[test]
    fn expired_keys_are_swept_once_a_window() {
        let limiter = Limiter::new("test", rule(5, 1000, 1000));
        let t = Instant::now();
        for i in 0..100 {
            limiter.reserve_at(key(&format!("198.51.100.{i}")), t).unwrap().keep();
        }
        assert_eq!(limiter.counts.lock().unwrap().keys.len(), 100);
        limiter
            .reserve_at(key("203.0.113.1"), t + Duration::from_secs(61))
            .unwrap()
            .keep();
        let counts = limiter.counts.lock().unwrap();
        assert_eq!((counts.keys.len(), counts.all.len()), (1, 1));
    }

    #[test]
    fn attempts_at_once_cannot_get_past_the_limit() {
        let limiter = std::sync::Arc::new(Limiter::new("test", rule(5, 50, 100)));
        let threads: Vec<_> = (0..40)
            .map(|_| {
                let limiter = limiter.clone();
                std::thread::spawn(move || limiter.reserve(key("203.0.113.1")).map(Attempt::keep).is_ok())
            })
            .collect();
        let allowed = threads.into_iter().map(|t| t.join().unwrap()).filter(|ok| *ok).count();
        assert_eq!(allowed, 5);
    }
}
