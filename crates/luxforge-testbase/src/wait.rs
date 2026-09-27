//! The one wait: poll a condition until it holds, and fail naming it once the hang bound passes.
use std::{
    thread,
    time::{Duration, Instant},
};

/// How long a test waits for something before it calls it hung. It only bounds a hang: nothing
/// asserts that anything happened sooner, so a loaded host makes a test slower, never failed. It
/// is generous because the tests run unoptimized beside other builds.
pub const HANG: Duration = Duration::from_secs(120);

/// How long [`wait_for`] rests between two looks at its condition.
const POLL: Duration = Duration::from_millis(1);

/// Wait until `done` holds, looking again every millisecond. Panics naming `what` once [`HANG`]
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
pub fn try_wait_for<T>(what: &str, ready: impl FnMut() -> Option<T>) -> Result<T, String> {
    wait_within(HANG, what, ready)
}

/// [`try_wait_for`] with its hang bound given, so this crate's own tests can reach it.
fn wait_within<T>(
    bound: Duration,
    what: &str,
    mut ready: impl FnMut() -> Option<T>,
) -> Result<T, String> {
    let deadline = Instant::now() + bound;
    loop {
        if let Some(value) = ready() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{what} never happened within the {bound:?} hang bound"
            ));
        }
        thread::sleep(POLL);
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
            wait_within(Duration::from_millis(1), "the impossible", || None::<()>),
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
