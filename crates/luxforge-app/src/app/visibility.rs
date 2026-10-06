//! One event-driven visibility gate for presentation consumers. Native callbacks replace a
//! single fact slot and post one buffered wake, including across subscription gaps.
use super::{
    Editor,
    message::{Message, visibility::VisibilityMessage},
    waker::Signal,
};
use iced::{Subscription, Task};
use luxforge_input::Visibility;
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug)]
struct Pending {
    facts: Visibility,
    sequence: u64,
    /// Even a hide followed by a restore before the update loop wakes invalidates old samples.
    minimized_or_app_hidden: bool,
    window_hidden: bool,
}

#[derive(Default)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Mailbox {
    sequence: u64,
    pending: Option<Pending>,
}

static MAILBOX: Mutex<Mailbox> = Mutex::new(Mailbox {
    sequence: 0,
    pending: None,
});

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn signal() -> &'static Signal<Message> {
    static SIGNAL: OnceLock<Signal<Message>> = OnceLock::new();
    SIGNAL.get_or_init(|| Signal::new(|| Message::Visibility(VisibilityMessage::Pending)))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn post(facts: Visibility) {
    let mut mailbox = MAILBOX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    mailbox.sequence = mailbox.sequence.wrapping_add(1);
    let previous = mailbox.pending;
    mailbox.pending = Some(Pending {
        facts,
        sequence: mailbox.sequence,
        minimized_or_app_hidden: facts.minimized
            || facts.app_hidden
            || previous.is_some_and(|previous| previous.minimized_or_app_hidden),
        window_hidden: facts.window_hidden
            || previous.is_some_and(|previous| previous.window_hidden),
    });
    drop(mailbox);
    signal().post();
}

fn take() -> Option<Pending> {
    MAILBOX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pending
        .take()
}

/// Runtime facts only. The explicit evidence override concerns an intentionally invisible
/// launch's window-hidden fact; native minimization and app hiding still pause it.
#[derive(Debug)]
pub(crate) struct WindowVisibility {
    pub(crate) facts: Visibility,
    pub(crate) ready: bool,
    pub(crate) evidence_invisible_window: bool,
    pub(crate) native_sequence: u64,
    pub(crate) transitions: u64,
    pub(crate) progress_coalesced: u64,
    pub(crate) error: Option<String>,
    pub(crate) evidence_operation: Option<EvidenceOperation>,
}

#[derive(Debug)]
pub(crate) struct EvidenceOperation {
    pub(crate) action: luxforge_input::EvidenceVisibility,
    pub(crate) before_sequence: u64,
    pub(crate) answered: bool,
}

impl EvidenceOperation {
    pub(crate) fn observed(&self, facts: Visibility, sequence: u64) -> bool {
        use luxforge_input::EvidenceVisibility as A;
        sequence > self.before_sequence
            && match self.action {
                A::Minimize => facts.minimized,
                A::Restore => !facts.minimized && !facts.window_hidden,
                A::HideWindow => facts.window_hidden,
                A::ShowWindow => !facts.window_hidden,
                A::HideApp => facts.app_hidden,
                A::ShowApp => !facts.app_hidden,
            }
    }
}

impl WindowVisibility {
    pub(crate) fn new(hidden: bool, evidence: bool) -> Self {
        Self {
            facts: Visibility {
                window_hidden: hidden,
                ..Visibility::unsupported()
            },
            ready: cfg!(test) || !cfg!(target_os = "macos"),
            evidence_invisible_window: hidden && evidence,
            native_sequence: 0,
            transitions: 0,
            progress_coalesced: 0,
            error: (!cfg!(target_os = "macos")).then(|| {
                "Native minimize/hide facts are not supported on this platform yet".into()
            }),
            evidence_operation: None,
        }
    }

    pub(crate) fn sampling_allowed(&self) -> bool {
        self.ready
            && !self.facts.minimized
            && !self.facts.app_hidden
            && (!self.facts.window_hidden || self.evidence_invisible_window)
    }

    pub(crate) fn summary(&self) -> Value {
        json!({"supported": self.facts.supported, "ready": self.ready,
            "minimized": self.facts.minimized, "window_hidden": self.facts.window_hidden,
            "app_hidden": self.facts.app_hidden, "sampling_allowed": self.sampling_allowed(),
            "evidence_invisible_window_override": self.evidence_invisible_window,
            "native_sequence": self.native_sequence, "transitions": self.transitions,
            "progress_coalesced": self.progress_coalesced,
            "unavailable": self.error})
    }
}

impl Editor {
    /// Keep the newest presentation progress without running hidden display hooks. Business
    /// changes (including partial answers) and every terminal record retain their normal route.
    pub(crate) fn hidden_progress_only(&self, message: &Message) -> bool {
        if self.visibility.sampling_allowed() {
            return false;
        }
        match message {
            Message::Capability(super::message::capability::CapabilityMessage::Polled(polled)) => {
                !polled.is_empty()
                    && polled.iter().all(|(module, _, result)| {
                        result.as_ref().is_ok_and(|record| {
                            self.capabilities.only_progress_changed(module, record)
                        })
                    })
            }
            Message::Export(super::message::export::ExportMessage::Read {
                result: Ok(record),
                ..
            }) => {
                matches!(record["status"].as_str(), Some("queued" | "running"))
            }
            _ => false,
        }
    }

    pub(crate) fn visibility_update(&mut self, message: VisibilityMessage) -> Task<Message> {
        match message {
            VisibilityMessage::Pending => match take() {
                Some(pending) => self.adopt_visibility(pending),
                None => Task::none(),
            },
            VisibilityMessage::Installed(result) => match result {
                Ok(facts) => {
                    // Prefer any newer callback to the snapshot returned by installation.
                    let pending = take();
                    if pending.is_none()
                        && self.visibility.ready
                        && self.visibility.native_sequence > 0
                    {
                        return Task::none();
                    }
                    let pending = pending.unwrap_or(Pending {
                        facts,
                        sequence: self.visibility.native_sequence,
                        minimized_or_app_hidden: facts.minimized || facts.app_hidden,
                        window_hidden: facts.window_hidden,
                    });
                    self.visibility.error = None;
                    self.adopt_visibility(pending)
                }
                Err(error) => {
                    self.visibility.ready = true;
                    self.visibility.error = Some(error.clone());
                    self.event("window_visibility_unavailable", || json!({"reason": error}));
                    self.long_work_visibility_changed()
                }
            },
        }
    }

    fn adopt_visibility(&mut self, pending: Pending) -> Task<Message> {
        let before = self.visibility.sampling_allowed();
        self.visibility.facts = pending.facts;
        self.visibility.ready = true;
        self.visibility.native_sequence = pending.sequence;
        let now = self.visibility.sampling_allowed();
        let crossed_hidden = pending.minimized_or_app_hidden
            || (pending.window_hidden && !self.visibility.evidence_invisible_window);
        // A coalesced native hide+restore still ends the old sampling epoch.
        if before && now && crossed_hidden && self.performance.running {
            self.performance.running = false;
            self.performance.epoch = self.performance.epoch.wrapping_add(1);
        }
        self.visibility.transitions += 1;
        self.event("window_visibility", || self.visibility.summary());
        if before != now || crossed_hidden {
            self.long_work_visibility_changed()
        } else {
            Task::none()
        }
    }
}

pub(super) fn install() -> Task<Message> {
    #[cfg(target_os = "macos")]
    {
        iced::window::oldest()
            .and_then(|id| {
                iced::window::run(id, |window| {
                    luxforge_input::install_visibility_handler(window, std::sync::Arc::new(post))
                })
            })
            .map(|result| Message::Visibility(VisibilityMessage::Installed(result)))
    }
    #[cfg(not(target_os = "macos"))]
    Task::none()
}

pub(super) fn subscription(_: &Editor) -> Subscription<Message> {
    #[cfg(target_os = "macos")]
    return Subscription::run(|| signal().stream());
    #[cfg(not(target_os = "macos"))]
    Subscription::none()
}

pub(super) fn after_message(editor: &mut Editor, _: &super::Before) -> Task<Message> {
    editor.visibility_evidence_settle();
    Task::none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        message::performance::PerformanceMessage,
        tasks::{PerformanceRead, call},
        testing::{boot, finish},
    };

    fn change(editor: &mut Editor, hidden: bool, minimized: bool) {
        let _ = editor.adopt_visibility(Pending {
            facts: Visibility {
                supported: true,
                window_hidden: hidden,
                minimized,
                app_hidden: false,
            },
            sequence: editor.visibility.native_sequence + 1,
            minimized_or_app_hidden: minimized,
            window_hidden: hidden,
        });
        let _ = editor.performance_transition();
    }

    fn answer(editor: &Editor) -> Box<PerformanceRead> {
        Box::new(PerformanceRead {
            resources: call(&editor.owner, editor.client, "resources.read", json!({}))
                .unwrap()
                .0,
            wall_ms: 1,
        })
    }

    #[test]
    fn hiding_rejects_late_samples_and_restore_never_spans_hidden_time() {
        let (mut editor, catalog) = boot();
        let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
        let old_epoch = editor.performance.epoch;
        let late = answer(&editor);
        let reads = editor.performance.requested;
        change(&mut editor, true, false);
        assert!(editor.performance.expanded, "disclosure is untouched");
        assert!(!editor.performance_sampling());
        assert_eq!(
            editor.long_work.timers(),
            super::super::long_work::Timers::default()
        );
        for _ in 0..3 {
            let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
        }
        assert_eq!(editor.performance.requested, reads);
        let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
            epoch: old_epoch,
            result: Ok(late),
        }));
        assert_eq!(editor.performance.history.len(), 0);
        change(&mut editor, false, false);
        assert_eq!(
            editor.performance.requested,
            reads + 1,
            "restore reads immediately"
        );
        let fresh = answer(&editor);
        let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
            epoch: editor.performance.epoch,
            result: Ok(fresh),
        }));
        assert_eq!(editor.performance.history.len(), 1);
        assert_eq!(
            editor.workspace.performance.metrics[1].value,
            crate::state::performance::DASH,
            "a CPU rate needs two new samples"
        );
        change(&mut editor, false, true);
        assert!(!editor.performance_sampling());
        change(&mut editor, false, false);
        assert!(editor.performance_sampling());
        finish(editor, catalog);
    }

    #[test]
    fn rapid_native_hide_restore_still_invalidates_the_old_epoch() {
        let (mut editor, catalog) = boot();
        let _ = editor.performance_transition();
        let old_epoch = editor.performance.epoch;
        let _ = editor.adopt_visibility(Pending {
            facts: Visibility {
                supported: true,
                minimized: false,
                window_hidden: false,
                app_hidden: false,
            },
            sequence: 3,
            minimized_or_app_hidden: true,
            window_hidden: false,
        });
        let _ = editor.performance_transition();
        assert!(editor.performance_sampling());
        assert_ne!(old_epoch, editor.performance.epoch);
        assert_eq!(
            editor.performance.requested, 1,
            "the previous read is not duplicated"
        );
        let fresh = answer(&editor);
        let _ = editor.performance_sampled(old_epoch, Ok(fresh));
        assert_eq!(editor.performance.history.len(), 0);
        assert_eq!(editor.performance.requested, 2);
        finish(editor, catalog);
    }

    #[test]
    fn only_invisible_evidence_launches_override_window_hiding() {
        let ordinary = WindowVisibility::new(true, false);
        assert!(!ordinary.sampling_allowed());
        let mut evidence = WindowVisibility::new(true, true);
        assert!(evidence.sampling_allowed());
        evidence.facts.minimized = true;
        assert!(!evidence.sampling_allowed());
        evidence.facts.minimized = false;
        evidence.facts.app_hidden = true;
        assert!(!evidence.sampling_allowed());
        assert_eq!(
            evidence.summary()["evidence_invisible_window_override"],
            true
        );
    }
}
