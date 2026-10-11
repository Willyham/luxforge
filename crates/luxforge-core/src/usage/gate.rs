//! The process-wide collection gate every producer checks first: off, or on in one consent
//! generation. Turning it off and on again starts a new generation, so work started under an earlier
//! one can tell that its consent has ended and contributes nothing.
use std::sync::atomic::{AtomicU64, Ordering};

/// One period of effective consent. A new opt-in, or a revocation followed by one, is a new
/// generation; a session ID belongs to exactly one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Generation(u64);

impl Generation {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Off, or the current generation. Producers call [`Gate::current`], one atomic load.
#[derive(Debug, Default)]
pub struct Gate {
    /// 0 while off, otherwise the current generation.
    state: AtomicU64,
    /// The newest generation ever started, so the next one differs from every earlier one.
    newest: AtomicU64,
}

impl Gate {
    /// A gate that is off.
    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
            newest: AtomicU64::new(0),
        }
    }

    /// The current generation, or `None` while collection is off.
    pub fn current(&self) -> Option<Generation> {
        match self.state.load(Ordering::Acquire) {
            0 => None,
            value => Some(Generation(value)),
        }
    }

    /// Whether `generation` is still the current one.
    pub fn is_current(&self, generation: Generation) -> bool {
        self.state.load(Ordering::Acquire) == generation.0
    }

    /// Turns collection on in a new generation and returns it; already on, returns the current one.
    pub fn enable(&self) -> Generation {
        if let Some(current) = self.current() {
            return current;
        }
        let next = self.newest.fetch_add(1, Ordering::AcqRel) + 1;
        match self
            .state
            .compare_exchange(0, next, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Generation(next),
            // Another caller enabled it first; theirs is the current generation.
            Err(current) => Generation(current),
        }
    }

    /// Turns collection off at once, ending the current generation.
    pub fn disable(&self) {
        self.state.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_on_off_on_gives_distinct_generations() {
        let gate = Gate::new();
        assert_eq!(gate.current(), None);
        let first = gate.enable();
        assert_eq!(gate.enable(), first);
        assert!(gate.is_current(first));
        gate.disable();
        assert_eq!(gate.current(), None);
        assert!(!gate.is_current(first));
        let second = gate.enable();
        assert_ne!(first, second);
        assert!(!gate.is_current(first));
        assert!(gate.is_current(second));
    }
}
