//! The one gate: a point work passes, which a test shuts to hold the work there until it says so.
use crate::HANG;
use std::{
    sync::{Condvar, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

/// A point work passes, which a test shuts to hold that work there. Work calls [`Gate::pass`];
/// while the gate is shut it waits there until the test calls [`Gate::open`]. The test knows the
/// work has arrived from [`Gate::reached`] and [`Gate::wait_reached`], so what it does next happens
/// while that work is held, whatever the host's load: the order is the test's, never a race.
///
/// A gate starts open. Every wait at it is bounded by [`HANG`] and panics past it, so a test that
/// never opens its gate fails instead of hanging.
#[derive(Debug, Default)]
pub struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct State {
    shut: bool,
    /// How many times work has passed or arrived at the gate.
    reached: u64,
    /// How many callers are waiting at it now.
    waiting: usize,
}

impl Gate {
    /// An open gate.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hold every caller that reaches the gate from now on.
    pub fn shut(&self) {
        self.lock().shut = true;
    }

    /// Release every caller waiting here, and let every later one through.
    pub fn open(&self) {
        self.lock().shut = false;
        self.changed.notify_all();
    }

    /// Whether the gate is shut.
    pub fn is_shut(&self) -> bool {
        self.lock().shut
    }

    /// How many times work has reached the gate, held or not.
    pub fn reached(&self) -> u64 {
        self.lock().reached
    }

    /// How many callers are waiting at the gate now.
    pub fn waiting(&self) -> usize {
        self.lock().waiting
    }

    /// Whether the gate is shut with a caller waiting at it: work nothing but the test can release.
    pub fn holding(&self) -> bool {
        let state = self.lock();
        state.shut && state.waiting > 0
    }

    /// Reach the gate, and wait here while it is shut. Panics once [`HANG`] has passed.
    pub fn pass(&self) {
        self.arrive(HANG, None);
    }

    /// Reach the gate and wait here while it is shut, asking `give_up` about once a millisecond
    /// whether to stop waiting: `true` when the gate opened, `false` when `give_up` ended the
    /// wait. For work that must stay responsive while it is held, such as a job that checks for
    /// its cancellation. Panics once [`HANG`] has passed.
    pub fn pass_unless(&self, mut give_up: impl FnMut() -> bool) -> bool {
        self.arrive(HANG, Some(&mut give_up))
    }

    /// Wait until work has reached the gate `count` times in all. Panics naming `what` once
    /// [`HANG`] has passed.
    pub fn wait_reached(&self, count: u64, what: &str) {
        self.wait_reached_within(HANG, count, what);
    }

    fn wait_reached_within(&self, bound: Duration, count: u64, what: &str) {
        let deadline = Instant::now() + bound;
        let mut state = self.lock();
        while state.reached < count {
            let now = Instant::now();
            assert!(
                now < deadline,
                "{what} never reached its gate within the {bound:?} hang bound"
            );
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn arrive(&self, bound: Duration, mut give_up: Option<&mut dyn FnMut() -> bool>) -> bool {
        let deadline = Instant::now() + bound;
        let mut state = self.lock();
        state.reached += 1;
        self.changed.notify_all();
        while state.shut {
            let now = Instant::now();
            if now >= deadline {
                drop(state);
                panic!("a gate stayed shut past the {bound:?} hang bound");
            }
            let mut rest = deadline - now;
            if give_up.is_some() {
                rest = rest.min(Duration::from_millis(1));
            }
            state.waiting += 1;
            state = self
                .changed
                .wait_timeout(state, rest)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            state.waiting -= 1;
            if !state.shut {
                break;
            }
            if let Some(give_up) = give_up.as_mut() {
                drop(state);
                if give_up() {
                    return false;
                }
                state = self.lock();
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };

    #[test]
    fn an_open_gate_counts_what_passes_and_holds_nothing() {
        let gate = Gate::new();
        assert!(!gate.is_shut());
        gate.pass();
        gate.pass();
        assert_eq!(gate.reached(), 2);
        assert_eq!(gate.waiting(), 0);
        assert!(!gate.holding());
    }

    #[test]
    fn a_shut_gate_holds_work_until_it_opens() {
        let gate = Arc::new(Gate::new());
        gate.shut();
        let passed = Arc::new(AtomicBool::new(false));
        let worker = {
            let (gate, passed) = (gate.clone(), passed.clone());
            thread::spawn(move || {
                gate.pass();
                passed.store(true, Ordering::SeqCst);
            })
        };
        gate.wait_reached(1, "the worker");
        crate::wait_until("the worker waits at the gate", || gate.holding());
        assert!(
            !passed.load(Ordering::SeqCst),
            "the worker is held while the gate is shut"
        );
        gate.open();
        worker.join().unwrap();
        assert!(passed.load(Ordering::SeqCst));
        assert_eq!(gate.reached(), 1);
        assert!(!gate.holding());
    }

    #[test]
    fn a_held_caller_can_give_up_its_wait() {
        let gate = Arc::new(Gate::new());
        gate.shut();
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (gate, stop) = (gate.clone(), stop.clone());
            thread::spawn(move || gate.pass_unless(|| stop.load(Ordering::SeqCst)))
        };
        gate.wait_reached(1, "the worker");
        stop.store(true, Ordering::SeqCst);
        assert!(!worker.join().unwrap(), "the worker gave up");
        assert!(gate.is_shut());
        gate.open();
        assert!(
            gate.pass_unless(|| true),
            "an open gate lets a caller through"
        );
    }

    #[test]
    #[should_panic(expected = "a gate stayed shut past the 1ms hang bound")]
    fn a_gate_never_opened_fails_its_caller() {
        let gate = Gate::new();
        gate.shut();
        gate.arrive(Duration::from_millis(1), None);
    }

    #[test]
    #[should_panic(expected = "the worker never reached its gate within the 1ms hang bound")]
    fn a_gate_nothing_reaches_fails_its_waiter() {
        Gate::new().wait_reached_within(Duration::from_millis(1), 1, "the worker");
    }
}
