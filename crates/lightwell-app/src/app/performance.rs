//! The Performance section's sampler: the one-second read of `resources.read` and `activity.list`
//! behind the state panel's last block, and the gate that keeps it asleep.
//!
//! The section samples only while it is expanded **and** the state panel is on screen. Collapsed,
//! with the panel hidden or with the component gallery in its place, it sets no timer and makes no
//! request, so an idle editor stays asleep (performance rule 8). Whenever sampling starts, the
//! history is cleared and one read is sent at once rather than a second later; whenever it starts
//! or stops, the epoch moves, so a read that was in flight across the change is dropped when it
//! lands instead of being drawn into a window it does not belong to. At most one read is in flight:
//! a tick that finds one still out does nothing, so a slow owner stretches the interval rather than
//! queueing reads behind itself.
use crate::{
    app::{
        Editor,
        evidence::Settle,
        message::{Message, PerformanceMessage},
        tasks::{PerformanceRead, performance_task},
    },
    state::performance::PerformanceHistory,
};
use iced::Task;
use lightwell_core::{ActivitySnapshot, resources::ResourceReport};
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Duration};

/// The sampler's interval, which is the sparklines' resolution: sixty points span a minute.
pub(crate) const INTERVAL: Duration = Duration::from_secs(1);

/// Whether the Performance section samples: only while it is expanded and the state panel that
/// holds it is shown. This is the one gate for the timer, the reads and the history's restart.
pub(crate) fn sampling(expanded: bool, state_panel_shown: bool) -> bool {
    expanded && state_panel_shown
}

/// The section's local state: its expanded flag, what it has read and the one read in flight.
#[derive(Debug, Default)]
pub(crate) struct Sampler {
    /// Local to this client and this launch, open at start (the owner's decision of 2026-09-23).
    /// Collapsing it, or hiding the state panel, stops the timer, which is the only way the
    /// section costs anything.
    pub(crate) expanded: bool,
    pub(crate) history: PerformanceHistory,
    pub(crate) in_flight: bool,
    /// The gate as the sampler last acted on it. Holding it here, rather than comparing the gate
    /// before and after each message, makes a message handled inside another — an evidence step
    /// runs its own update from within one — start sampling once rather than twice.
    pub(crate) running: bool,
    /// Moves whenever sampling starts or stops; a read carries the epoch it was asked under.
    pub(crate) epoch: u64,
    /// Reads asked of the owner since launch, for evidence: a run proves the section asleep by this
    /// staying put while it is collapsed.
    pub(crate) requested: u64,
    /// The last two `resources.read` answers exactly as the owner sent them, oldest first, each with
    /// the wall-clock moment it was read. Evidence records them so a runner can re-derive the shown
    /// figures and the newest rate without trusting the model that derived them.
    pub(crate) raw: VecDeque<(u64, Value)>,
    /// The last `activity.list` answer as the owner sent it.
    pub(crate) activity: Option<Value>,
    /// Why the last read could not be used, until one can.
    pub(crate) error: Option<String>,
}

impl Sampler {
    /// The section as a launch finds it: open, having read nothing yet. It starts sampling on the
    /// first message the editor handles, like any other change to the gate.
    pub(crate) fn open() -> Self {
        Self {
            expanded: true,
            ..Self::default()
        }
    }

    /// Start a fresh window: nothing read before the section last stopped sampling is kept.
    fn restart(&mut self) {
        self.history.clear();
        self.raw.clear();
        self.activity = None;
        self.error = None;
    }

    /// Take up one read: parse both answers into the core's own report types, the way any Rust
    /// API client would, and add them to the window. An answer that does not parse changes nothing
    /// but the error it leaves.
    fn adopt(&mut self, read: PerformanceRead) -> Result<(), String> {
        let sample: ResourceReport = serde_json::from_value(read.resources.clone())
            .map_err(|error| format!("resources.read: {error}"))?;
        let activity: ActivitySnapshot = serde_json::from_value(read.activity.clone())
            .map_err(|error| format!("activity.list: {error}"))?;
        self.history.push(sample, activity);
        if self.raw.len() == 2 {
            self.raw.pop_front();
        }
        self.raw.push_back((read.wall_ms, read.resources));
        self.activity = Some(read.activity);
        self.error = None;
        Ok(())
    }
}

impl Editor {
    /// One message about the state panel's Performance section.
    pub(super) fn performance_update(&mut self, message: PerformanceMessage) -> Task<Message> {
        match message {
            PerformanceMessage::Toggle => self.performance.expanded = !self.performance.expanded,
            PerformanceMessage::Tick => return self.performance_tick(),
            PerformanceMessage::Sampled { epoch, result } => {
                return self.performance_sampled(epoch, result);
            }
        }
        Task::none()
    }

    /// Whether the Performance section samples now: expanded, with the state panel on screen. The
    /// component gallery replaces the whole workspace, panel included, so it counts as hidden.
    pub(crate) fn performance_sampling(&self) -> bool {
        sampling(
            self.performance.expanded,
            self.session.workspace.state_panel && self.gallery_page().is_none(),
        )
    }

    /// Called after every message. Whatever route changed the gate — the heading, the palette, a
    /// script, the state panel hidden through `workspace.set` by another client, the gallery — is
    /// answered here: starting clears the window and reads at once, and either change moves the
    /// epoch so a read in flight across it is dropped when it lands.
    pub(crate) fn performance_transition(&mut self) -> Task<Message> {
        let now = self.performance_sampling();
        if now == self.performance.running {
            return Task::none();
        }
        self.performance.running = now;
        self.performance.epoch = self.performance.epoch.wrapping_add(1);
        if !now {
            return Task::none();
        }
        self.performance.restart();
        self.performance_read()
    }

    /// Ask the owner for one read, unless one is already out. A read still in flight from an
    /// earlier epoch is not duplicated: it is dropped when it lands, and that is when the new
    /// epoch's first read goes out.
    fn performance_read(&mut self) -> Task<Message> {
        if self.performance.in_flight {
            return Task::none();
        }
        self.performance.in_flight = true;
        self.performance.requested += 1;
        performance_task(self.owner.clone(), self.client, self.performance.epoch)
    }

    /// One tick of the sampler's timer. The timer exists only while the section samples, so the
    /// gate here only guards against a tick already queued as the gate closed.
    pub(crate) fn performance_tick(&mut self) -> Task<Message> {
        if !self.performance_sampling() {
            return Task::none();
        }
        self.performance_read()
    }

    /// A read answered. One asked under an earlier epoch, or landing after the section stopped
    /// sampling, is dropped; if the section is sampling again by then, its first read goes out now.
    pub(crate) fn performance_sampled(
        &mut self,
        epoch: u64,
        result: Result<Box<PerformanceRead>, String>,
    ) -> Task<Message> {
        self.performance.in_flight = false;
        if epoch != self.performance.epoch || !self.performance_sampling() {
            return if self.performance_sampling() {
                self.performance_read()
            } else {
                Task::none()
            };
        }
        let adopted = result.and_then(|read| self.performance.adopt(*read));
        if let Err(error) = adopted {
            self.event("performance_read_failed", json!({ "reason": error }));
            self.performance.error = Some(error);
        }
        // The expanded frame is captured on the first answer, figures and all, rather than on the
        // frame before it, which could only show dashes.
        self.settle_step(Settle::Performance);
        Task::none()
    }

    /// The section as a captured frame records it: the flag, the reads asked for, the raw answers
    /// the figures came from, and the rows exactly as the model gave them to the view, so a runner
    /// can re-derive every figure from the recorded answers and compare.
    pub(crate) fn performance_summary(&self) -> Value {
        let model = &self.workspace.performance;
        let raw = &self.performance.raw;
        let previous = (raw.len() == 2).then(|| &raw[0]);
        json!({
            "expanded": self.performance.expanded,
            "sampling": self.performance_sampling(),
            "reads_requested": self.performance.requested,
            "in_flight": self.performance.in_flight,
            "samples": self.performance.history.len(),
            "pid": std::process::id(),
            "wall_ms": raw.back().map(|(wall_ms, _)| wall_ms),
            "resources": raw.back().map(|(_, resources)| resources),
            "previous_wall_ms": previous.map(|(wall_ms, _)| wall_ms),
            "previous_resources": previous.map(|(_, resources)| resources),
            "activity": self.performance.activity,
            "error": self.performance.error,
            "caption": model.caption,
            "rows": model.metrics.iter().map(|row| json!({
                "label": row.label,
                "value": row.value,
                "unit": row.unit,
                "available": row.available,
                "series_len": row.series.len(),
                "tooltip": row.tooltip,
            })).collect::<Vec<_>>(),
            "jobs": model.jobs.iter().map(|job| json!({
                "label": job.label,
                "trailing": job.trailing,
                "detail": job.detail,
                "running": job.running,
                "progress": job.progress,
            })).collect::<Vec<_>>(),
            "reserve_detail": model.reserve_detail,
            "more": model.more,
            "version": model.version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::message::{PaletteMessage, PerformanceMessage, ViewMessage};
    use crate::app::{
        message::PaletteAction,
        tasks::call,
        testing::{boot, finish},
    };

    /// A real read, answered by the editor's own owner exactly as the task would ask for it.
    fn read(editor: &Editor) -> Box<PerformanceRead> {
        let (resources, _) =
            call(&editor.owner, editor.client, "resources.read", json!({})).unwrap();
        let (activity, _) = call(&editor.owner, editor.client, "activity.list", json!({})).unwrap();
        Box::new(PerformanceRead {
            resources,
            activity,
            wall_ms: 1_758_600_000_000,
        })
    }

    /// An editor whose section was collapsed before its first message, so it has started nothing:
    /// the state a test that opens the section itself begins from.
    fn boot_collapsed() -> (Editor, std::path::PathBuf) {
        let (mut editor, catalog) = boot();
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        assert!(!editor.performance.expanded);
        assert_eq!(editor.performance.requested, 0);
        (editor, catalog)
    }

    #[test]
    fn the_timer_runs_only_while_expanded_and_the_state_panel_is_shown() {
        assert!(sampling(true, true));
        assert!(!sampling(true, false), "the panel is hidden");
        assert!(!sampling(false, true), "the section is collapsed");
        assert!(!sampling(false, false));

        let (mut editor, catalog) = boot();
        assert!(editor.performance.expanded, "open at start");
        assert!(editor.performance_sampling());
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        assert!(!editor.performance_sampling(), "collapsed");
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        assert!(editor.performance_sampling());
        // Hidden by a session answer — this client's toggle or another client's `workspace.set`.
        let mut session = editor.session.clone();
        session.workspace.state_panel = false;
        session.revision += 1;
        let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(
            session.clone()
        ))));
        assert!(!editor.performance_sampling());
        session.workspace.state_panel = true;
        session.revision += 1;
        let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(
            session.clone()
        ))));
        assert!(editor.performance_sampling());
        // The component gallery replaces the workspace, panel and all.
        editor.developer = true;
        let _ = editor.update(Message::View(ViewMessage::Gallery(Some(2))));
        assert_eq!(editor.gallery_page(), Some(2));
        assert!(!editor.performance_sampling());
        finish(editor, catalog);
    }

    /// Expanding starts a fresh window and reads at once; collapsing asks for nothing more.
    #[test]
    fn expanding_clears_the_history_and_reads_at_once() {
        let (mut editor, catalog) = boot_collapsed();
        editor.performance.history.push(
            lightwell_core::resources::read(&lightwell_core::RenderContext::new()),
            ActivitySnapshot::default(),
        );
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        assert!(editor.performance.expanded);
        assert_eq!(editor.performance.history.len(), 0, "a fresh window");
        assert_eq!(
            editor.performance.requested, 1,
            "read at once, not a second later"
        );
        assert!(editor.performance.in_flight);
        // A message that carried the toggle inside it — an evidence step's own update — sees the
        // gate already acted on and starts nothing again.
        let epoch = editor.performance.epoch;
        let _ = editor.performance_transition();
        assert_eq!(editor.performance.epoch, epoch);
        assert_eq!(editor.performance.requested, 1);

        let epoch = editor.performance.epoch;
        let answer = read(&editor);
        let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
            epoch,
            result: Ok(answer),
        }));
        assert!(!editor.performance.in_flight);
        assert_eq!(editor.performance.history.len(), 1);
        let rows = &editor.workspace.performance.metrics;
        assert_eq!(rows.len(), 3);
        assert_ne!(
            rows[0].value,
            crate::state::performance::DASH,
            "memory from one sample"
        );
        assert_eq!(
            rows[1].value,
            crate::state::performance::DASH,
            "CPU needs two"
        );

        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        assert!(!editor.performance_sampling());
        let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
        assert_eq!(
            editor.performance.requested, 1,
            "collapsed, nothing is asked"
        );
        assert_eq!(
            editor.performance.history.len(),
            1,
            "collapsing keeps the window, so a collapsed run shows it unchanged"
        );
        assert!(editor.workspace.performance.metrics.is_empty());
        finish(editor, catalog);
    }

    #[test]
    fn a_tick_while_a_read_is_in_flight_does_nothing() {
        let (mut editor, catalog) = boot_collapsed();
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        assert_eq!(editor.performance.requested, 1);
        for _ in 0..3 {
            let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
        }
        assert_eq!(
            editor.performance.requested, 1,
            "one read in flight at a time"
        );
        let epoch = editor.performance.epoch;
        let answer = read(&editor);
        let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
            epoch,
            result: Ok(answer),
        }));
        let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
        assert_eq!(editor.performance.requested, 2, "the next tick reads again");
        finish(editor, catalog);
    }

    /// A read that lands after the section collapsed is dropped, and so is one asked for before it
    /// was collapsed and expanded again, whose place a fresh read takes.
    #[test]
    fn a_read_from_before_a_collapse_is_dropped() {
        let (mut editor, catalog) = boot_collapsed();
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        let first = editor.performance.epoch;
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        let answer = read(&editor);
        let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
            epoch: first,
            result: Ok(answer),
        }));
        assert_eq!(
            editor.performance.history.len(),
            0,
            "dropped after collapsing"
        );
        assert!(!editor.performance.in_flight);
        assert_eq!(
            editor.performance.requested, 1,
            "and nothing asked in its place"
        );

        // Expanded, collapsed and expanded again with the first read still out: no second read is
        // sent beside it, and when it lands it is dropped and the fresh read goes out.
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        let stale = editor.performance.epoch;
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        assert_eq!(editor.performance.requested, 2);
        let answer = read(&editor);
        let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
            epoch: stale,
            result: Ok(answer),
        }));
        assert_eq!(editor.performance.history.len(), 0);
        assert_eq!(
            editor.performance.requested, 3,
            "the new epoch's first read"
        );
        assert!(editor.performance.in_flight);
        let current = editor.performance.epoch;
        let answer = read(&editor);
        let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
            epoch: current,
            result: Ok(answer),
        }));
        assert_eq!(editor.performance.history.len(), 1);
        finish(editor, catalog);
    }

    /// A read that cannot be used changes nothing but the error the frame records.
    #[test]
    fn a_failed_read_leaves_its_reason_and_keeps_the_window() {
        let (mut editor, catalog) = boot_collapsed();
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        let epoch = editor.performance.epoch;
        let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
            epoch,
            result: Err("owner stopped".into()),
        }));
        assert_eq!(editor.performance.error.as_deref(), Some("owner stopped"));
        let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
        let mut answer = read(&editor);
        answer.resources = json!({"monotonic_ns": "soon"});
        let epoch = editor.performance.epoch;
        let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
            epoch,
            result: Ok(answer),
        }));
        assert!(
            editor
                .performance
                .error
                .as_deref()
                .is_some_and(|error| error.starts_with("resources.read: ")),
            "{:?}",
            editor.performance.error
        );
        assert_eq!(editor.performance.history.len(), 0);
        finish(editor, catalog);
    }

    /// The frame records the raw answers the figures came from and the rows as the view shows them.
    #[test]
    fn the_snapshot_records_the_answers_and_the_rows_as_shown() {
        let (mut editor, catalog) = boot_collapsed();
        assert_eq!(editor.snapshot()["performance"]["expanded"], json!(false));
        assert_eq!(
            editor.snapshot()["performance"]["reads_requested"],
            json!(0)
        );
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        for _ in 0..2 {
            let epoch = editor.performance.epoch;
            let answer = read(&editor);
            let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
                epoch,
                result: Ok(answer),
            }));
            let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
        }
        let summary = &editor.snapshot()["performance"];
        assert_eq!(summary["expanded"], json!(true));
        assert_eq!(summary["samples"], json!(2));
        assert_eq!(summary["reads_requested"], json!(3));
        assert_eq!(summary["pid"], json!(std::process::id()));
        assert!(summary["resources"]["memory"]["bytes"].is_u64());
        assert!(summary["previous_resources"]["cpu"]["time_ns"].is_u64());
        assert_eq!(summary["wall_ms"], json!(1_758_600_000_000_u64));
        assert!(summary["activity"]["active"].is_array());
        let rows = summary["rows"].as_array().unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row["label"].clone())
                .collect::<Vec<_>>(),
            [json!("Memory"), json!("CPU"), json!("GPU")]
        );
        assert_eq!(rows[0]["series_len"], json!(2));
        assert_eq!(rows[1]["series_len"], json!(1), "one rate from two samples");
        assert_eq!(rows[1]["unit"], json!("%"));
        assert_eq!(summary["jobs"][0]["label"], json!("No background work"));
        finish(editor, catalog);
    }

    /// The palette offers the section's toggle named for what it would do, and running it does it.
    #[test]
    fn the_palette_toggles_the_section() {
        let (mut editor, catalog) = boot_collapsed();
        let index = |editor: &Editor, label: &str| {
            editor
                .workspace
                .palette
                .entries
                .iter()
                .position(|entry| entry.label == label)
        };
        let show = index(&editor, "Show performance").expect("offered while collapsed");
        assert_eq!(
            editor.workspace.palette.entries[show].action,
            PaletteAction::TogglePerformance
        );
        assert!(index(&editor, "Hide performance").is_none());
        let _ = editor.update(Message::Palette(PaletteMessage::Open));
        let _ = editor.update(Message::Palette(PaletteMessage::RunIndex(show)));
        assert!(editor.performance.expanded);
        assert_eq!(editor.performance.requested, 1);
        assert!(index(&editor, "Hide performance").is_some());
        finish(editor, catalog);
    }
}
