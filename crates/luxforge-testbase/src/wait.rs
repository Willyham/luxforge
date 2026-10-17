//! The one wait: poll a condition until it holds, and fail naming it once the hang bound passes.
use std::{
    thread,
    time::{Duration, Instant},
};

/// How long a test waits for something before it calls it hung. It only bounds a hang: nothing
/// asserts that anything happened sooner, so a loaded host makes a test slower, never failed. It
/// is generous because the tests run unoptimized beside other builds.
pub const HANG: Duration = Duration::from_secs(120);

/// How long [`wait_for`] first rests between two looks at its condition, and the most it rests:
/// the rest doubles from the first to the most. Most waits are for another thread's next step,
/// which is microseconds away; a millisecond's rest each time made a test of thousands of steps
/// sleep for seconds.
const FIRST_POLL: Duration = Duration::from_micros(10);
const POLL: Duration = Duration::from_millis(1);

/// Wait until `done` holds, looking again within a millisecond. Panics naming `what` once [`HANG`]
/// has passed without it. `done` may itself panic to fail at once when what it waits for can no
/// longer happen.
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    wait_for(what, || done().then_some(()));
}

/// Wait until `ready` answers a value, and return it; see [`wait_until`].
pub fn wait_for<T>(what: &str, ready: impl FnMut() -> Option<T>) -> T {
    try_wait_for(what, ready).unwrap_or_else(|hung| panic!("{hung}"))
}

/// [`wait_for`] for a check that reports what it found broken as a value rather than a panic:
/// past [`HANG`] it answers an error naming `what`.
pub fn try_wait_for<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> Result<T, String> {
    wait_within(HANG, what, || ready().map_or(Look::Waiting, Look::Ready))
}

/// [`wait_for`] with a longer hang bound than [`HANG`], for one step that legitimately takes
/// longer on a slow host, such as a photo-sized export through the reference on a three-core
/// runner.
pub fn wait_for_within<T>(bound: Duration, what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
    wait_within(bound, what, || ready().map_or(Look::Waiting, Look::Ready))
        .unwrap_or_else(|hung| panic!("{hung}"))
}

/// One look of [`try_wait_progressing`] at what it waits for.
pub enum Look<T> {
    /// It holds, with this value.
    Ready(T),
    /// It does not hold yet, but the look moved it on, as a frame that drew a tile does: look again
    /// at once.
    Progressed,
    /// Nothing has changed since the last look.
    Waiting,
}

/// [`try_wait_for`] for a look that itself moves what it waits for on, such as drawing a frame:
/// it rests only after a look that changed nothing. The hang bound is the same.
pub fn try_wait_progressing<T>(what: &str, look: impl FnMut() -> Look<T>) -> Result<T, String> {
    wait_within(HANG, what, look)
}

/// [`try_wait_for`] with its hang bound given.
fn wait_within<T>(
    bound: Duration,
    what: &str,
    mut look: impl FnMut() -> Look<T>,
) -> Result<T, String> {
    let deadline = Instant::now() + bound;
    let mut rest = FIRST_POLL;
    loop {
        match look() {
            Look::Ready(value) => return Ok(value),
            Look::Progressed => {
                rest = FIRST_POLL;
                if Instant::now() < deadline {
                    continue;
                }
            }
            Look::Waiting => {}
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{what} never happened within the {bound:?} hang bound"
            ));
        }
        thread::sleep(rest);
        rest = (rest * 2).min(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    };

    #[test]
    fn a_wait_returns_as_soon_as_its_condition_holds() {
        let looks = AtomicU32::new(0);
        wait_until("the third look", || {
            looks.fetch_add(1, Ordering::SeqCst) == 2
        });
        assert_eq!(looks.load(Ordering::SeqCst), 3);
        assert_eq!(wait_for("a value", || Some(7)), 7);
    }

    #[test]
    fn a_wait_sees_what_another_thread_does() {
        let done = Arc::new(AtomicBool::new(false));
        let setter = {
            let done = done.clone();
            thread::spawn(move || done.store(true, Ordering::SeqCst))
        };
        wait_until("the other thread's flag", || done.load(Ordering::SeqCst));
        setter.join().unwrap();
    }

    #[test]
    fn a_wait_that_never_holds_fails_naming_what_it_waited_for() {
        assert_eq!(
            wait_within(Duration::from_millis(1), "the impossible", || {
                Look::<()>::Waiting
            }),
            Err("the impossible never happened within the 1ms hang bound".to_owned())
        );
        assert_eq!(try_wait_for("a value", || Some(7)), Ok(7));
    }

    #[test]
    #[should_panic(expected = "the impossible can never happen")]
    fn a_condition_can_fail_its_wait_at_once() {
        wait_until("the impossible", || {
            panic!("the impossible can never happen")
        });
    }
}
