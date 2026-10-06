//! Copied desktop facts. Obscuration and focus are deliberately absent.

/// Whether a desktop window is minimized or explicitly hidden. Unsupported platforms retain
/// sampling and report that no native visibility facts are available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visibility {
    pub supported: bool,
    pub minimized: bool,
    pub window_hidden: bool,
    pub app_hidden: bool,
}

impl Visibility {
    pub const fn unsupported() -> Self {
        Self {
            supported: false,
            minimized: false,
            window_hidden: false,
            app_hidden: false,
        }
    }

    pub const fn sampling_allowed(self) -> bool {
        !(self.minimized || self.window_hidden || self.app_hidden)
    }
}

#[cfg(target_os = "macos")]
pub(crate) struct Delivery {
    handler: std::sync::Arc<dyn Fn(Visibility) + Send + Sync>,
    last: std::cell::Cell<Option<Visibility>>,
    closed: std::cell::Cell<bool>,
}

#[cfg(target_os = "macos")]
impl Delivery {
    pub(crate) fn new(handler: std::sync::Arc<dyn Fn(Visibility) + Send + Sync>) -> Self {
        Self {
            handler,
            last: std::cell::Cell::new(None),
            closed: std::cell::Cell::new(false),
        }
    }

    pub(crate) fn publish(&self, state: Visibility) {
        if !self.closed.get() && self.last.replace(Some(state)) != Some(state) {
            (self.handler)(state);
        }
    }

    pub(crate) fn close(&self) {
        self.closed.set(true);
    }
}

/// Native evidence operations, permitted only in a background-only application. They never
/// activate the app or make its window opaque. These are operations, not synthetic facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceVisibility {
    Minimize,
    Restore,
    HideWindow,
    ShowWindow,
    HideApp,
    ShowApp,
}

#[cfg(test)]
mod tests {
    use super::Visibility;

    #[test]
    fn visibility_pauses_only_actual_minimized_or_hidden_facts() {
        for bits in 0..8 {
            let state = Visibility {
                supported: true,
                minimized: bits & 1 != 0,
                window_hidden: bits & 2 != 0,
                app_hidden: bits & 4 != 0,
            };
            assert_eq!(state.sampling_allowed(), bits == 0);
        }
        assert!(Visibility::unsupported().sampling_allowed());
        assert!(!Visibility::unsupported().supported);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn visibility_delivery_replacement_stops_old_callbacks_and_publishes_initial_facts() {
        use std::sync::{Arc, Mutex};

        let pending = Arc::new(Mutex::new(None));
        let handler = {
            let pending = pending.clone();
            Arc::new(move |facts| *pending.lock().unwrap() = Some(facts))
        };
        let visible = Visibility {
            supported: true,
            ..Visibility::unsupported()
        };
        let hidden = Visibility {
            window_hidden: true,
            ..visible
        };
        let old = super::Delivery::new(handler.clone());
        old.publish(visible);
        assert_eq!(pending.lock().unwrap().take(), Some(visible));
        old.publish(hidden);
        old.publish(visible);
        assert_eq!(pending.lock().unwrap().take(), Some(visible));
        old.close();
        let current = super::Delivery::new(handler);
        current.publish(hidden);
        old.publish(visible);
        assert_eq!(pending.lock().unwrap().take(), Some(hidden));
        current.publish(hidden);
        assert_eq!(pending.lock().unwrap().take(), None);
        current.close();
        current.close();
        current.publish(visible);
        assert_eq!(pending.lock().unwrap().take(), None);
    }
}
