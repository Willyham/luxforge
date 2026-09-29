//! One request in flight, with only the newest waiting behind it.
//!
//! Pointer samples, pans, curve samples, the event sync and the Performance section's reads all
//! arrive faster than their answers, and each keeps the same rule: at most one request out, and of
//! everything asked for while it is out, only the newest waits. The slot is a throttle, not a
//! timer: nothing wakes up to check it. An answer frees it, and the waiting value, if any, is the
//! next one sent.

/// One request in flight, and the newest value waiting for it to be answered.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Coalesce<T> {
    in_flight: bool,
    pending: Option<T>,
}

impl<T> Default for Coalesce<T> {
    fn default() -> Self {
        Self {
            in_flight: false,
            pending: None,
        }
    }
}

impl<T> Coalesce<T> {
    /// Offer the newest value. It waits in the slot, replacing an older value still waiting, which
    /// is handed back; [`Self::start`] sends it once nothing is in flight.
    pub(crate) fn offer(&mut self, value: T) -> Option<T> {
        self.pending.replace(value)
    }

    /// Take the waiting value to send now, when nothing is in flight: the slot counts it in flight
    /// until [`Self::answered`]. With one in flight, or nothing waiting, there is nothing to send.
    pub(crate) fn start(&mut self) -> Option<T> {
        if self.in_flight {
            return None;
        }
        let value = self.pending.take()?;
        self.in_flight = true;
        Some(value)
    }

    /// The request in flight was answered; the value waiting, if any, may start now.
    pub(crate) fn answered(&mut self) {
        self.in_flight = false;
    }

    /// Forget the value waiting, which nothing needs any more.
    pub(crate) fn drop_pending(&mut self) {
        self.pending = None;
    }

    /// A request is in flight.
    pub(crate) fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// The value waiting for the request in flight.
    pub(crate) fn pending(&self) -> Option<&T> {
        self.pending.as_ref()
    }

    /// Nothing is in flight and nothing waits.
    pub(crate) fn idle(&self) -> bool {
        !self.in_flight && self.pending.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::Coalesce;

    #[test]
    fn one_request_is_in_flight_and_only_the_newest_waits_behind_it() {
        let mut slot = Coalesce::default();
        assert!(slot.idle());
        assert_eq!(slot.offer(1), None);
        assert_eq!(
            slot.start(),
            Some(1),
            "nothing in flight: the value goes out"
        );
        assert!(slot.in_flight() && slot.pending().is_none());
        assert_eq!(slot.offer(2), None);
        assert_eq!(slot.start(), None, "one in flight: the next waits");
        assert_eq!(
            slot.offer(3),
            Some(2),
            "a newer value displaces the older one"
        );
        assert_eq!(slot.pending(), Some(&3));
        slot.answered();
        assert_eq!(slot.start(), Some(3), "the answer lets the newest go out");
        slot.answered();
        assert_eq!(slot.start(), None);
        assert!(slot.idle());
    }

    #[test]
    fn a_dropped_value_is_never_sent() {
        let mut slot = Coalesce::default();
        slot.offer(1);
        slot.start();
        slot.offer(2);
        slot.drop_pending();
        slot.answered();
        assert_eq!(slot.start(), None);
        assert!(slot.idle());
    }
}
