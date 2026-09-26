//! A value that says when it may have changed.
//!
//! [`Tracked`] reads through to its value like a reference, and every mutable access takes a new
//! stamp from one process-wide counter. A section of the workspace built from a tracked value keeps
//! the stamp it was built from, so deciding whether it must be built again is one comparison of two
//! numbers rather than a comparison, a hash or a clone of the value. The stamp moves on every
//! mutable access whether or not the value then changes, which can only cost a rebuild that was not
//! needed, never skip one that was. Stamps are unique for the life of the process, so replacing a
//! tracked value whole can never bring back a stamp a section already holds.
use std::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A stamp no value has held before.
pub(crate) fn stamp() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug)]
pub(crate) struct Tracked<T> {
    value: T,
    stamp: u64,
}

impl<T> Tracked<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            value,
            stamp: stamp(),
        }
    }

    /// The stamp of the value as it is now; any mutable access since the last read moved it.
    pub(crate) fn stamp(&self) -> u64 {
        self.stamp
    }
}

impl<T: Default> Default for Tracked<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for Tracked<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

/// A tracked value equals whatever its value equals; the stamp is not part of what it is.
impl<T: PartialEq<U>, U> PartialEq<U> for Tracked<T> {
    fn eq(&self, other: &U) -> bool {
        self.value == *other
    }
}

impl<T> Deref for Tracked<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> DerefMut for Tracked<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.stamp = stamp();
        &mut self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_keeps_the_stamp_and_every_mutable_access_moves_it() {
        let mut tracked = Tracked::new(vec![1, 2]);
        let first = tracked.stamp();
        assert_eq!(tracked.len(), 2);
        assert_eq!(tracked.stamp(), first, "a read moves nothing");
        tracked.push(3);
        let second = tracked.stamp();
        assert_ne!(second, first);
        let _ = &mut *tracked;
        assert_ne!(
            tracked.stamp(),
            second,
            "a mutable access moves it, changed or not"
        );
        let replaced = Tracked::new(vec![1, 2]);
        assert!(
            replaced.stamp() != first && replaced.stamp() != second,
            "a new value never repeats a stamp"
        );
    }
}
