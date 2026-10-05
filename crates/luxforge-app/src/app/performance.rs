//! The Performance section's sampler: the one-second read of `resources.read` and the activity
//! board behind the state panel's last block, and the gate that keeps it asleep.
//!
//! The section samples only while it is expanded **and** the state panel is on screen. Collapsed,
//! with the panel hidden or with the component gallery in its place, it sets no timer and makes no
//! request, so an idle editor stays asleep (performance rule 8). Whenever sampling starts, the
//! history is cleared and one read is sent at once rather than a second later; whenever it starts
//! or stops, the epoch moves, so a read that was in flight across the change is dropped when it
//! lands instead of being drawn into a window it does not belong to. At most one read is in flight:
//! a tick that finds one still out does nothing, so a slow owner stretches the interval rather than
//! queueing reads behind itself.
//!
//! **One board.** The section's job rows are drawn from the board long-running work holds
//! (`app/long_work.rs`), the snapshot the status bar's job comes from: the sampler reads it at each
//! tick, on the update loop through long work's watch (what `activity.list` answers, without an
//! owner round trip), and every read long work takes when the board wakes it refreshes the rows
//! too. So the rows follow a catalog job's begin, progress and end between samples, and they and
//! the status bar never show two reads of the board.
use crate::app::Before;
use crate::coalesce::Coalesce;
use crate::{
    app::{
        Editor,
        message::{Message, performance::PerformanceMessage},
        outcome::Outcome,
        tasks::{PerformanceRead, performance_task},
    },
    state::{performance::PerformanceHistory, preferences::PreferenceChange},
};
use iced::{Subscription, Task};
use luxforge_core::{JobId, resources::ResourceReport};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::Duration;

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
    /// Local to this client, initialized from the saved user preference (open on first use).
    /// Collapsing it, or hiding the state panel, stops the timer, which is the only way the
    /// section costs anything.
    pub(crate) expanded: bool,
    pub(crate) cancelling: BTreeSet<JobId>,
    pub(crate) history: PerformanceHistory,
    /// The one read in flight: a read is never duplicated beside it.
    pub(crate) read: Coalesce<()>,
    /// The gate as the sampler last acted on it. Holding it here, rather than comparing the gate
    /// before and after each message, makes a message handled inside another — an evidence step
    /// runs its own update from within one — start sampling once rather than twice.
    pub(crate) running: bool,
    /// Moves whenever sampling starts or stops; a read carries the epoch it was asked under.
    pub(crate) epoch: u64,
    /// Reads asked of the owner since launch, for evidence: a run proves the section asleep by this
    /// staying put while it is collapsed.
    pub(crate) requested: u64,
    /// Why the last read could not be used, until one can.
    pub(crate) error: Option<String>,
}

impl Sampler {
    /// The section as a launch finds it: open, having read nothing yet. It starts sampling on the
    /// first message the editor handles, like any other change to the gate.
    pub(crate) fn new(expanded: bool) -> Self {
        Self {
            expanded,
            ..Self::default()
        }
    }

    /// Start a fresh window: nothing read before the section last stopped sampling is kept.
    fn restart(&mut self) {
        self.history.clear();
        self.error = None;
    }

    /// Take up one read: parse the answer into the core's own report type, the way any Rust API
    /// client would, and add it to the window. An answer that does not parse changes nothing but
    /// the error it leaves. The answer is read in place: nothing of it is kept.
    fn adopt(&mut self, read: &PerformanceRead) -> Result<(), String> {
        let sample = ResourceReport::deserialize(&read.resources)
            .map_err(|error| format!("resources.read: {error}"))?;
        self.history.push(sample);
        self.error = None;
        Ok(())
    }
}

impl Editor {
    /// One message about the state panel's Performance section.
    pub(super) fn performance_update(&mut self, message: PerformanceMessage) -> Task<Message> {
        match message {
            PerformanceMessage::Toggle => {
                // The next launch starts the section as this one leaves it: the desktop's
                // preference writer stores it, and closing the window waits for it.
                self.performance.expanded = !self.performance.expanded;
                self.store_preferences(PreferenceChange {
                    performance_expanded: Some(self.performance.expanded),
                    ..PreferenceChange::default()
                })
            }
            PerformanceMessage::Cancel(job_id) => {
                if self.performance.cancelling.len() >= crate::state::performance::MAX_JOB_ROWS
                    || !self
                        .workspace
                        .performance
                        .jobs
                        .iter()
                        .any(|row| row.running && row.job_id.as_ref() == Some(&job_id))
                    || !self.performance.cancelling.insert(job_id.clone())
                {
                    return Task::none();
                }
                let owner = self.owner.clone();
                let client = self.client;
                let answer_id = job_id.clone();
                crate::app::tasks::owner_task(
                    move || {
                        crate::app::tasks::call(
                            &owner,
                            client,
                            "job.cancel",
                            json!({"job_id": job_id}),
                        )
                        .map(|(answer, _)| answer)
                    },
                    move |result| {
                        Message::Performance(PerformanceMessage::Cancelled {
                            job_id: answer_id,
                            result,
                        })
                    },
                )
            }
            PerformanceMessage::Cancelled { job_id, result } => {
                self.performance.cancelling.remove(&job_id);
                let failed = result.is_err();
                self.status.text = match result {
                    Ok(answer) => match answer["status"].as_str() {
                        Some("cancelled") => "Background job cancelled".into(),
                        Some("ready") => "Background job already finished".into(),
                        Some("failed" | "superseded") => "Background job already ended".into(),
                        _ => "Cancellation requested for background job".into(),
                    },
                    Err(reason) => format!("Could not cancel background job: {reason}"),
                };
                self.outcome(Outcome::PerformanceCancelled { failed });
                self.performance_tick()
            }
            PerformanceMessage::Tick => self.performance_tick(),
            PerformanceMessage::Sampled { epoch, result } => {
                self.performance_sampled(epoch, result)
            }
        }
    }

    /// Whether the Performance section samples now: expanded, with the state panel on screen. The
    /// component gallery replaces the whole workspace, panel included, so it counts as hidden.
    pub(crate) fn performance_sampling(&self) -> bool {
        sampling(
            self.performance.expanded,
            // The section is pinned under the state panel in Develop and the sources panel in
            // Select.
            self.left_panel_shown() && self.gallery_page().is_none(),
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
        self.outcome(Outcome::PerformanceRestarted);
        self.performance_read()
    }

    /// Read the board and ask the owner for the counters, unless a read is already out. A read
    /// still in flight from an earlier epoch is not duplicated: it is dropped when it lands, and
    /// that is when the new epoch's first read goes out. The board is long work's, read here on
    /// the update loop, so the job rows age with the section's own cadence while nothing wakes
    /// long work.
    fn performance_read(&mut self) -> Task<Message> {
        self.performance.read.offer(());
        if self.performance.read.start().is_none() {
            return Task::none();
        }
        self.performance.requested += 1;
        Task::batch([
            self.section_reads_board(),
            performance_task(self.owner.clone(), self.client, self.performance.epoch),
        ])
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
        self.performance.read.answered();
        if epoch != self.performance.epoch || !self.performance_sampling() {
            return if self.performance_sampling() {
                self.performance_read()
            } else {
                Task::none()
            };
        }
        let adopted = result.and_then(|read| self.performance.adopt(&read).map(|()| read));
        let read = match adopted {
            Ok(read) => Some(read),
            Err(error) => {
                self.event("performance_read_failed", || json!({ "reason": error }));
                self.performance.error = Some(error);
                None
            }
        };
        // The answer is reported with the read it took up, which only evidence keeps.
        self.outcome(Outcome::PerformanceRead(read));
        Task::none()
    }

    /// The section as a captured frame records it: the flag, the reads asked for, the raw counter
    /// answers the figures came from, which the evidence driver keeps ([`Recorded`]), the board
    /// the job rows came from (long work's, as `activity.list` would answer it), and the rows
    /// exactly as the model gave them to the view, so a runner can re-derive every figure from the
    /// recorded answers and compare.
    ///
    /// [`Recorded`]: crate::app::evidence::Recorded
    pub(crate) fn performance_summary(&self) -> Value {
        let model = &self.workspace.performance;
        let recorded = self.evidence.as_ref().map(|evidence| &evidence.recorded);
        let raw = recorded.map(|recorded| &recorded.performance);
        let previous = raw.filter(|raw| raw.len() == 2).map(|raw| &raw[0]);
        json!({
            "expanded": self.performance.expanded,
            "sampling": self.performance_sampling(),
            "reads_requested": self.performance.requested,
            "in_flight": self.performance.read.in_flight(),
            "samples": self.performance.history.len(),
            "pid": std::process::id(),
            "wall_ms": raw.and_then(|raw| raw.back()).map(|(wall_ms, _)| wall_ms),
            "resources": raw.and_then(|raw| raw.back()).map(|(_, resources)| resources),
            "previous_wall_ms": previous.map(|(wall_ms, _)| wall_ms),
            "previous_resources": previous.map(|(_, resources)| resources),
            "activity": self.long_work.state.board,
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
            "jobs": model.jobs.iter().map(|job| {
                let mut row = json!({
                    "job_id": job.job_id,
                    "cancelling": job.cancelling,
                    "label": job.label,
                    "trailing": job.trailing,
                    "detail": job.detail,
                    "running": job.running,
                    "progress": job.progress,
                });
                // Catalog work's row, drawn as a work row with Cancel, says which job it stops.
                if let Some(work) = &job.work {
                    row["work"] = json!({
                        "job_id": work.job_id,
                        "count": work.count,
                        "estimate": work.estimate,
                    });
                }
                row
            }).collect::<Vec<_>>(),
            "reserve_detail": model.reserve_detail,
            "more": model.more,
            "version": model.version,
        })
    }
}

/// After every message: whatever route opened, closed, hid or showed the section is answered here
/// ([`Editor::performance_transition`]).
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    editor.performance_transition()
}

/// The sampler's timer, gated on the section being expanded with the state panel on screen.
/// Collapsed or hidden, there is no timer at all, in evidence runs too.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    if !editor.performance_sampling() {
        return Subscription::none();
    }
    iced::time::every(INTERVAL).map(|_| Message::Performance(PerformanceMessage::Tick))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::message::{
        palette::PaletteMessage, performance::PerformanceMessage, view::ViewMessage,
    };
    use crate::app::{
        tasks::call,
        testing::{boot, finish},
    };
    use crate::state::palette::PaletteAction;

    /// Model work only: compare the former full derivation route with the idle sampler's
    /// scoped refresh. Import/decoding, owner calls, widgets, layout and GPU work are not timed.
    #[test]
    #[ignore = "release model-cost diagnostic over the generated 24 MP photo"]
    fn performance_model_work() {
        use std::time::Instant;

        let catalog = luxforge_testbase::paths::temp_path("performance-model.sqlite");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/generated/24mp.jpg");
        let (mut editor, _, _) = crate::app::testing::real_photo_at(&catalog, &fixture);
        editor.performance.expanded = true;
        editor.presentation.queue.cancel();
        editor.overlays.queue.cancel();
        editor.thumbnailer.queue.cancel();
        editor.coverage_worker.queue.cancel();
        luxforge_testbase::wait_until("model measurement has no pixel worker", || {
            !editor.workers_busy()
        });
        let (resource, _) =
            call(&editor.owner, editor.client, "resources.read", json!({})).unwrap();
        let mut sample: ResourceReport = serde_json::from_value(resource).unwrap();
        let mut full = Vec::with_capacity(1000);
        let mut scoped = Vec::with_capacity(1000);
        for iteration in 0..1200 {
            for only_performance in [iteration % 2 == 0, iteration % 2 != 0] {
                sample.monotonic_ns += 1_000_000_000;
                if let Some(time) = &mut sample.cpu.time_ns {
                    *time += 5_000_000;
                }
                editor.performance.history.push(sample.clone());
                // Each route must derive a newly changed sample; measuring the second route
                // against the first route's cached version would time a no-op.
                assert_ne!(
                    editor.workspace.performance.version,
                    editor.performance.history.version()
                );
                let started = Instant::now();
                if only_performance {
                    editor.workspace.performance.refresh_sample(
                        editor.performance.expanded,
                        &editor.performance.history,
                        &editor.long_work.state,
                        editor.select.state.home.as_deref(),
                    );
                } else {
                    editor.rederive();
                }
                let micros = started.elapsed().as_secs_f64() * 1_000_000.0;
                std::hint::black_box(&editor.workspace);
                if iteration >= 200 {
                    if only_performance {
                        scoped.push(micros);
                    } else {
                        full.push(micros);
                    }
                }
            }
        }
        let distribution = |samples| {
            let d = luxforge_testbase::Distribution::of(samples).unwrap();
            json!({"count": d.count, "p50": d.p50, "p95": d.p95,
                "min": d.min, "max": d.max, "samples": d.samples})
        };
        println!(
            "performance_model_work {}",
            json!({"unit": "us", "full": distribution(full), "scoped": distribution(scoped)})
        );
        finish(editor, catalog);
    }

    #[test]
    fn disclosure_is_a_discovered_user_preference_and_reopening_initializes_its_gate() {
        use crate::app::Boot;
        use std::sync::Arc;
        let root = luxforge_testbase::paths::temp_path("performance-preference");
        let launch = |catalog: &std::path::Path| {
            let mut host = luxforge_core::HostConfig::unconfigured();
            host.preferences_dir = Some(root.clone());
            let (owner, join) = luxforge_core::OwnerHandle::start_with_host(
                catalog,
                Arc::new(luxforge_core::ModuleRegistry::builtin()),
                host,
            )
            .unwrap();
            Editor::new(Boot {
                owner,
                join,
                live_server: None,
                config: crate::Config::default(),
                client: None,
                initial_import: None,
                window: (1440.0, 900.0),
            })
            .0
        };
        let catalog = root.with_extension("sqlite");
        let mut editor = launch(&catalog);
        assert!(editor.performance.expanded);
        let (schemas, _) = call(&editor.owner, editor.client, "schema.list", json!({})).unwrap();
        let schemas = schemas.to_string();
        assert!(
            schemas.contains("preferences.read")
                && schemas.contains("preferences.set")
                && schemas.contains("performance_expanded")
        );
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        assert!(!editor.performance_sampling());
        // The toggle's write goes through the desktop's one preference writer.
        let params = editor
            .preferences
            .writing()
            .expect("a write in flight")
            .params();
        assert_eq!(params, json!({"performance_expanded": false}));
        let answer =
            crate::app::tasks::call_own(&editor.owner, editor.client, "preferences.set", params)
                .and_then(|(saved, request)| {
                    Ok((crate::state::preferences::parse(saved)?, request))
                });
        let _ = editor.update(Message::Preferences(
            crate::app::message::preferences::PreferenceMessage::Saved(answer),
        ));
        assert!(editor.preferences.idle());
        assert!(!editor.preferences.stored().unwrap().performance_expanded);
        assert!(
            editor.document.state.is_none(),
            "a user preference requires no photo or history"
        );
        finish(editor, catalog.clone());
        let mut reopened = launch(&catalog);
        assert!(!reopened.performance.expanded);
        let _ = reopened.update(Message::Performance(PerformanceMessage::Tick));
        assert_eq!(
            reopened.performance.requested, 0,
            "a saved collapse creates no timer/read"
        );
        finish(reopened, catalog);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_requires_a_running_listed_id_and_disables_duplicates_until_answered() {
        let (mut editor, catalog) = boot_collapsed();
        let id = JobId::new();
        let _ = editor.performance_update(PerformanceMessage::Cancel(id.clone()));
        assert!(editor.performance.cancelling.is_empty());
        editor
            .workspace
            .performance
            .jobs
            .push(crate::state::performance::JobRow {
                job_id: Some(id.clone()),
                running: true,
                ..Default::default()
            });
        let _ = editor.performance_update(PerformanceMessage::Cancel(id.clone()));
        assert_eq!(editor.performance.cancelling.len(), 1);
        let _ = editor.performance_update(PerformanceMessage::Cancel(id.clone()));
        assert_eq!(editor.performance.cancelling.len(), 1);
        let _ = editor.performance_update(PerformanceMessage::Cancelled {
            job_id: id,
            result: Err("job is no longer available".into()),
        });
        assert!(editor.performance.cancelling.is_empty());
        assert!(editor.status.text.contains("job is no longer available"));
        finish(editor, catalog);
    }

    /// A real read, answered by the editor's own owner exactly as the task would ask for it.
    fn read(editor: &Editor) -> Box<PerformanceRead> {
        let (resources, _) =
            call(&editor.owner, editor.client, "resources.read", json!({})).unwrap();
        Box::new(PerformanceRead {
            resources,
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
        editor
            .performance
            .history
            .push(luxforge_core::resources::read(
                &luxforge_core::RenderContext::new(),
            ));
        let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
        assert!(editor.performance.expanded);
        assert_eq!(editor.performance.history.len(), 0, "a fresh window");
        assert_eq!(
            editor.performance.requested, 1,
            "read at once, not a second later"
        );
        assert!(editor.performance.read.in_flight());
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
        assert!(!editor.performance.read.in_flight());
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
        assert!(!editor.performance.read.in_flight());
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
        assert!(editor.performance.read.in_flight());
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
    /// The raw answers are only a captured frame's, so the evidence driver keeps them: without an
    /// evidence run nothing of a read is kept but the figures.
    #[test]
    fn the_snapshot_records_the_answers_and_the_rows_as_shown() {
        let (mut editor, catalog) = boot_collapsed();
        editor.evidence = Some(crate::app::testing::scripted_evidence("[]"));
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
        let _ = editor.update(Message::Palette(PaletteMessage::Open));
        let show = index(&editor, "Show performance").expect("offered while collapsed");
        assert_eq!(
            editor.workspace.palette.entries[show].action,
            PaletteAction::TogglePerformance
        );
        assert!(index(&editor, "Hide performance").is_none());
        let _ = editor.update(Message::Palette(PaletteMessage::RunIndex(show)));
        assert!(editor.performance.expanded);
        assert_eq!(editor.performance.requested, 1);
        let _ = editor.update(Message::Palette(PaletteMessage::Open));
        assert!(index(&editor, "Hide performance").is_some());
        finish(editor, catalog);
    }
}
