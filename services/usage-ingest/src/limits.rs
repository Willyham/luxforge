//! Admission limits: a global token bucket and an optional bounded limiter keyed by the approved
//! reporting (installation) ID. Nothing here sees, derives from or stores a network address; the
//! only identity it ever holds is the 16 bytes of a validated installation ID, in memory, for at
//! most [`MAX_ID_TTL_SECS`] after that ID's last request.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;

/// The longest an ID-keyed entry may outlive its last request.
pub const MAX_ID_TTL_SECS: u64 = 15 * 60;

/// Time, as two readings the service can fake in tests.
pub trait Clock: Send + Sync + 'static {
    /// Wall-clock UTC seconds since the Unix epoch, for `received_at`.
    fn unix_secs(&self) -> u64;
    /// Monotonic milliseconds from an arbitrary start, for limits and expiry.
    fn now_ms(&self) -> u64;
}

pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> SystemClock {
        SystemClock::new()
    }
}

impl Clock for SystemClock {
    fn unix_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// A token bucket measured in milliseconds of refill: a request costs `interval_ms`, the bucket
/// holds `burst * interval_ms`.
#[derive(Debug, Clone, Copy)]
pub struct Bucket {
    interval_ms: u64,
    capacity: u64,
    level: u64,
    last_ms: u64,
}

impl Bucket {
    pub fn new(interval_ms: u64, burst: u64, now_ms: u64) -> Bucket {
        let interval_ms = interval_ms.max(1);
        let capacity = interval_ms.saturating_mul(burst.max(1));
        Bucket {
            interval_ms,
            capacity,
            level: capacity,
            last_ms: now_ms,
        }
    }

    fn refill(&mut self, now_ms: u64) {
        let elapsed = now_ms.saturating_sub(self.last_ms);
        self.level = self.level.saturating_add(elapsed).min(self.capacity);
        self.last_ms = self.last_ms.max(now_ms);
    }

    /// Take one request, or say how many whole seconds until one is available.
    pub fn take(&mut self, now_ms: u64) -> Result<(), u64> {
        self.refill(now_ms);
        if self.level >= self.interval_ms {
            self.level -= self.interval_ms;
            Ok(())
        } else {
            Err((self.interval_ms - self.level).div_ceil(1000).max(1))
        }
    }
}

/// The service-wide limit on statistics requests.
pub struct Global {
    bucket: Mutex<GlobalBucket>,
}

/// Fixed-point requests: each costs 1000 units, and every millisecond adds `per_second` units.
/// This preserves the configured rate without truncating its reciprocal interval.
struct GlobalBucket {
    per_second: u64,
    capacity: u64,
    level: u64,
    last_ms: u64,
}

impl Global {
    pub fn new(per_second: u32, burst: u32, now_ms: u64) -> Global {
        let capacity = u64::from(burst.max(1)) * 1000;
        Global {
            bucket: Mutex::new(GlobalBucket {
                per_second: u64::from(per_second.clamp(1, 1000)),
                capacity,
                level: capacity,
                last_ms: now_ms,
            }),
        }
    }

    pub fn admit(&self, now_ms: u64) -> Result<(), u64> {
        let mut bucket = self.bucket.lock().unwrap_or_else(|e| e.into_inner());
        let refill = now_ms
            .saturating_sub(bucket.last_ms)
            .saturating_mul(bucket.per_second);
        bucket.level = bucket.level.saturating_add(refill).min(bucket.capacity);
        bucket.last_ms = bucket.last_ms.max(now_ms);
        if bucket.level >= 1000 {
            bucket.level -= 1000;
            Ok(())
        } else {
            Err((1000 - bucket.level)
                .div_ceil(bucket.per_second * 1000)
                .max(1))
        }
    }
}

/// What the ID limiter decided.
#[derive(Debug, PartialEq, Eq)]
pub enum IdVerdict {
    Admit,
    /// Refuse; the seconds to suggest in `Retry-After`.
    Refuse(u64),
    /// The table is full of live entries, so the limiter stands aside (the global limit still
    /// bounds the service): a flood of forged IDs must not lock real installations out.
    TableFull,
}

pub struct IdLimiter {
    interval_ms: u64,
    burst: u64,
    ttl_ms: u64,
    max_entries: usize,
    state: Mutex<IdState>,
    /// Wakes the deadline worker only when its next deadline changes.
    pub(crate) changed: Notify,
}

#[derive(Default)]
struct IdState {
    /// ID bytes to (bucket, expiry).
    entries: HashMap<[u8; 16], (Bucket, u64)>,
    /// Exactly one deadline per retained ID; refreshing an ID replaces its old record.
    deadlines: BTreeSet<(u64, [u8; 16])>,
}

impl IdState {
    fn next_deadline(&self) -> Option<u64> {
        self.deadlines.first().map(|(deadline, _)| *deadline)
    }

    fn expire(&mut self, now_ms: u64) {
        while self
            .next_deadline()
            .is_some_and(|deadline| deadline <= now_ms)
        {
            let (_, key) = self
                .deadlines
                .pop_first()
                .expect("the first deadline exists");
            self.entries.remove(&key);
        }
    }
}

impl IdLimiter {
    pub fn new(interval_secs: u64, burst: u64, ttl_secs: u64, max_entries: usize) -> IdLimiter {
        IdLimiter {
            interval_ms: interval_secs.saturating_mul(1000).max(1),
            burst,
            ttl_ms: ttl_secs.clamp(1, MAX_ID_TTL_SECS) * 1000,
            max_entries: max_entries.max(1),
            state: Mutex::new(IdState::default()),
            changed: Notify::new(),
        }
    }

    /// Admit a request from the installation whose canonical UUID text is `id`.
    pub fn admit(&self, id: &str, now_ms: u64) -> IdVerdict {
        let Some(key) = id_bytes(id) else {
            return IdVerdict::Admit;
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let previous_deadline = state.next_deadline();
        // Even an existing ID must expire before a request can refresh its retention.
        state.expire(now_ms);
        let verdict =
            if !state.entries.contains_key(&key) && state.entries.len() >= self.max_entries {
                IdVerdict::TableFull
            } else {
                let (mut bucket, old_deadline) = state
                    .entries
                    .remove(&key)
                    .unwrap_or_else(|| (Bucket::new(self.interval_ms, self.burst, now_ms), now_ms));
                state.deadlines.remove(&(old_deadline, key));
                let deadline = now_ms.saturating_add(self.ttl_ms);
                let verdict = match bucket.take(now_ms) {
                    Ok(()) => IdVerdict::Admit,
                    Err(retry) => IdVerdict::Refuse(retry),
                };
                state.entries.insert(key, (bucket, deadline));
                state.deadlines.insert((deadline, key));
                verdict
            };
        if state.next_deadline() != previous_deadline {
            self.changed.notify_one();
        }
        verdict
    }

    /// Forget entries at their retention deadline, independently of bucket refill.
    pub fn sweep(&self, now_ms: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let previous_deadline = state.next_deadline();
        state.expire(now_ms);
        if state.next_deadline() != previous_deadline {
            self.changed.notify_one();
        }
    }

    pub(crate) fn next_deadline(&self) -> Option<u64> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .next_deadline()
    }

    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The 16 bytes of a canonical UUID's text; `None` for anything else (the parser has already
/// validated, so this is for the limiter's own safety).
fn id_bytes(id: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = id.bytes().filter(|b| *b != b'-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, pair) in hex.chunks(2).enumerate() {
        let digit = |b: u8| (b as char).to_digit(16);
        out[i] = u8::try_from(digit(pair[0])? * 16 + digit(pair[1])?).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "c9a10716-1b78-4cbe-8d1c-b8309e5e7247";
    const B: &str = "5f0c2e3a-8d47-4b6e-9a1f-2c7d9e4b1a60";

    #[test]
    fn the_global_bucket_allows_a_burst_then_the_rate() {
        let global = Global::new(10, 3, 0);
        assert!((0..3).all(|_| global.admit(0).is_ok()));
        assert_eq!(global.admit(0), Err(1));
        assert!(global.admit(99).is_err());
        assert!(global.admit(100).is_ok());
        assert!(global.admit(100).is_err());
        // A long idle refills to the burst and no further.
        assert!((0..3).all(|_| global.admit(1_000_000).is_ok()));
        assert!(global.admit(1_000_000).is_err());
    }

    #[test]
    fn global_rates_preserve_fractional_intervals_and_burst_capacity() {
        for rate in [3, 600, 1000] {
            let global = Global::new(rate, 7, 0);
            assert!((0..7).all(|_| global.admit(0).is_ok()));
            assert_eq!(global.admit(0), Err(1));
            let mut admitted = 0;
            for now in 1..=1000 {
                while global.admit(now).is_ok() {
                    admitted += 1;
                }
            }
            assert_eq!(admitted, rate);
            assert!((0..7).all(|_| global.admit(u64::MAX).is_ok()));
            assert_eq!(global.admit(u64::MAX), Err(1));
        }
        let global = Global::new(3, 1, 0);
        assert_eq!(global.admit(0), Ok(()));
        assert_eq!(global.admit(333), Err(1));
        assert_eq!(global.admit(334), Ok(()));
        assert_eq!(global.admit(666), Err(1));
        // At 334 ms the full bucket discarded two excess fixed-point units.
        assert_eq!(global.admit(667), Err(1));
        assert_eq!(global.admit(668), Ok(()));
    }

    #[test]
    fn an_id_is_limited_alone_and_forgotten_within_fifteen_minutes() {
        let limiter = IdLimiter::new(30, 2, MAX_ID_TTL_SECS, 100);
        assert_eq!(limiter.admit(A, 0), IdVerdict::Admit);
        assert_eq!(limiter.admit(A, 0), IdVerdict::Admit);
        assert_eq!(limiter.admit(A, 0), IdVerdict::Refuse(30));
        assert_eq!(limiter.admit(B, 0), IdVerdict::Admit);
        assert_eq!(limiter.len(), 2);
        limiter.sweep(1_000);
        assert_eq!(limiter.len(), 2);
        limiter.sweep(MAX_ID_TTL_SECS * 1000);
        assert_eq!(limiter.len(), 0);
    }

    #[test]
    fn the_ttl_cannot_exceed_fifteen_minutes() {
        // A bucket that never refills in time (one token a day) is still dropped at the TTL.
        let limiter = IdLimiter::new(86_400, 1, 24 * 3600, 10);
        assert_eq!(limiter.admit(A, 0), IdVerdict::Admit);
        limiter.sweep(MAX_ID_TTL_SECS * 1000 - 1);
        assert_eq!(limiter.len(), 1);
        limiter.sweep(MAX_ID_TTL_SECS * 1000);
        assert!(limiter.is_empty());
    }

    #[test]
    fn expiry_precedes_refresh_and_retains_only_one_deadline_per_id() {
        let limiter = IdLimiter::new(86_400, 1, 1, 2);
        assert_eq!(limiter.admit(A, 0), IdVerdict::Admit);
        // Even without a worker sweep, a request at expiry starts a fresh bucket.
        assert_eq!(limiter.admit(A, 1000), IdVerdict::Admit);
        for now in 1001..2000 {
            assert!(matches!(limiter.admit(A, now), IdVerdict::Refuse(_)));
            let state = limiter.state.lock().unwrap();
            assert_eq!(state.entries.len(), 1);
            assert_eq!(state.deadlines.len(), 1);
            assert_eq!(state.next_deadline(), Some(now + 1000));
        }
        assert_eq!(limiter.admit(B, 1999), IdVerdict::Admit);
        assert_eq!(limiter.state.lock().unwrap().deadlines.len(), 2);
        limiter.sweep(2998);
        assert_eq!(limiter.len(), 2);
        limiter.sweep(2999);
        assert_eq!(limiter.len(), 0);
        assert!(limiter.state.lock().unwrap().deadlines.is_empty());
    }

    #[test]
    fn a_full_table_of_live_entries_stands_aside_and_never_grows() {
        let limiter = IdLimiter::new(86_400, 1, 900, 1);
        assert_eq!(limiter.admit(A, 0), IdVerdict::Admit);
        assert_eq!(limiter.admit(B, 1), IdVerdict::TableFull);
        assert_eq!(limiter.len(), 1);
        // Once the first has expired, the second takes its place.
        assert_eq!(limiter.admit(B, 900_001), IdVerdict::Admit);
        assert_eq!(limiter.len(), 1);
    }
}
