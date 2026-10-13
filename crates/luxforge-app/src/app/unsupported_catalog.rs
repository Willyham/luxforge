//! Launching past an unsupported catalog ([design](../../../../docs/design/auto-update.md#launching-past-an-unsupported-catalog)).
//!
//! When the catalog the launch chose itself — the remembered one or the default — is refused as
//! another version's, the window opens on a sheet instead of the launch failing. No catalog is
//! open behind it, so there is no owner and no command registry yet: the sheet's two choices are
//! the launch's own. Start a new catalog opens a new catalog beside the refused one through the
//! same path a launch takes, remembers it with `preferences.set`, and hands the window to the
//! editor; Quit ends the launch. The refused catalog is never opened again, written, moved or
//! removed. The programmatic equivalent is a launch with `--catalog NEW_PATH` and, to keep it,
//! `preferences.set` with `catalog`.
use super::{
    Editor, EventLog,
    evidence::{CaptureSync, marked},
    lifecycle::{self, Prepared},
    message::{
        Message,
        unsupported_catalog::{Started, UnsupportedCatalogMessage as Sheet},
    },
    tasks,
};
use crate::{
    state::{
        preferences::LaunchCatalog,
        unsupported_catalog::{Choice, Phase, RELEASES_URL, Refusal, SheetModel, new_catalog_path},
    },
    view,
};
use iced::{Subscription, Task};
use luxforge_ui::Element;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// What the window runs: the editor over an open catalog, or the unsupported-catalog sheet before
/// one is open. Iced holds one of these; every message, view, theme and subscription goes to the
/// one it holds.
pub(crate) enum Shell {
    Editor(Box<Editor>),
    Unsupported(Box<Unsupported>),
}

impl Shell {
    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        match self {
            Self::Editor(editor) => editor.update(message),
            Self::Unsupported(sheet) => match sheet.update(message) {
                Next::Stay(task) => task,
                Next::Open(started) => {
                    let (editor, task) = sheet.open(started);
                    *self = Self::Editor(editor);
                    task
                }
            },
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        match self {
            Self::Editor(editor) => editor.view(),
            Self::Unsupported(sheet) => sheet.view(),
        }
    }

    pub(crate) fn theme(&self) -> luxforge_ui::Theme {
        match self {
            Self::Editor(editor) => editor.theme(),
            Self::Unsupported(sheet) => sheet.theme.clone(),
        }
    }

    pub(crate) fn interface_scale(&self) -> f32 {
        match self {
            Self::Editor(editor) => editor.interface_scale(),
            Self::Unsupported(sheet) => sheet.scale,
        }
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        match self {
            Self::Editor(editor) => editor.subscription(),
            Self::Unsupported(sheet) => sheet.subscription(),
        }
    }
}

/// What one sheet message leads to.
pub(crate) enum Next {
    Stay(Task<Message>),
    /// The new catalog is open: the window becomes the editor over it.
    Open(Started),
}

/// The unsupported-catalog sheet and what the launch holds for the catalog it may start.
pub(crate) struct Unsupported {
    pub(crate) refusal: Refusal,
    /// The launch as it stood when its catalog was refused, for the catalog Start a new catalog
    /// opens; the sheet itself takes nothing from it.
    prepared: Prepared,
    /// Where Start a new catalog would create its catalog, decided when the sheet opens and again
    /// when it is pressed; `None` when every name beside the refused catalog is taken.
    pub(crate) next: Option<PathBuf>,
    pub(crate) phase: Phase,
    /// Why the releases page could not be opened, when it could not.
    pub(crate) link_problem: Option<String>,
    log: EventLog,
    theme: luxforge_ui::Theme,
    scale: f32,
    pub(crate) evidence: Option<SheetEvidence>,
    /// A problem the process exits with once the window has closed: an evidence run refused its
    /// catalog with no choice to take.
    exit: Arc<Mutex<Option<String>>>,
}

/// An evidence run's capture of the sheet: one frame, `unsupported-catalog.png`, with its state in
/// `unsupported-catalog.json`, then the choice the run names.
pub(crate) struct SheetEvidence {
    dir: PathBuf,
    pub(crate) choice: Option<Choice>,
    sync: CaptureSync,
    /// The frame is wanted and not yet requested.
    pub(crate) pending: bool,
    /// The state recorded with the screenshot in flight.
    state: Option<Value>,
}

/// The file names an evidence run keeps the sheet's capture under.
pub(crate) const CAPTURE: &str = "unsupported-catalog.png";
pub(crate) const CAPTURE_STATE: &str = "unsupported-catalog.json";

impl Unsupported {
    pub(crate) fn new(
        refusal: Refusal,
        prepared: Prepared,
        exit: Arc<Mutex<Option<String>>>,
    ) -> (Self, Task<Message>) {
        let config = &prepared.config;
        let theme = match &config.launch_theme {
            Some(theme) => {
                super::themes::widget_theme(&theme.id, theme.resolved.mode, &theme.resolved.tokens)
            }
            None => luxforge_ui::Theme::luxforge_dark(),
        };
        let scale = crate::window_frame::interface_scale(
            config
                .interface_size
                .unwrap_or(luxforge_core::preferences::DEFAULT_INTERFACE_SIZE),
        );
        let evidence = config.evidence.clone().map(|dir| SheetEvidence {
            dir,
            choice: config.unsupported_choice,
            sync: CaptureSync::default(),
            pending: true,
            state: None,
        });
        let next = new_catalog_path(&refusal.path, refusal.expected, exists);
        let sheet = Self {
            log: EventLog::new(config),
            refusal,
            prepared,
            next,
            phase: Phase::Offered,
            link_problem: None,
            theme,
            scale,
            evidence,
            exit,
        };
        // The process starts on the sheet: its log begins as the editor's does, and the editor
        // logs its own startup when Start a new catalog hands it the window.
        sheet.event("startup", || {
            json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"version":luxforge_core::APPLICATION_VERSION,"debug_assertions":cfg!(debug_assertions),"test_cameras":luxforge_core::test_cameras_admitted(),"mode":"unsupported-catalog"})
        });
        sheet.event("unsupported_catalog", || {
            json!({
                "catalog": sheet.refusal.path,
                "catalog_format": sheet.refusal.format,
                "expected_format": sheet.refusal.expected,
                "marked": sheet.refusal.marked,
                "refusal": sheet.refusal.detail,
                "new_catalog": sheet.next,
                "mode": if sheet.evidence.is_some() { "evidence" } else { "editor" },
                "hidden": sheet.prepared.config.hidden,
            })
        });
        (sheet, Task::none())
    }

    /// What the sheet shows now.
    pub(crate) fn model(&self) -> SheetModel {
        SheetModel::new(
            &self.refusal,
            self.next.as_deref(),
            &self.phase,
            self.link_problem.as_deref(),
        )
    }

    /// Log one event as the editor logs its own, into the launch's diagnostics log.
    fn event(&self, name: &str, detail: impl FnOnce() -> Value) {
        if self.log.diagnostics.is_none() && !self.log.verbose {
            return;
        }
        let value = json!({"event":name,"run_id":self.log.run_id,"build_version":luxforge_core::APPLICATION_VERSION,"elapsed_ms":self.log.started.elapsed().as_secs_f64()*1000.,"request_id":0,"generation":0,"detail":detail()});
        if let Some(log) = &self.log.diagnostics {
            log.event(value);
        } else {
            eprintln!("{value}");
        }
    }

    pub(crate) fn update(&mut self, message: Message) -> Next {
        let next = self.handle(message);
        // Counted after the message is handled, so a capture compares the frame drawn last with
        // the updates before it, as the view that frame was built from counted them.
        if let Some(evidence) = &mut self.evidence {
            evidence.sync.updates += 1;
        }
        next
    }

    fn handle(&mut self, message: Message) -> Next {
        let message = match message {
            Message::UnsupportedCatalog(message) => message,
            // The window's close button is Quit.
            Message::Close => Sheet::Quit,
            _ => return Next::Stay(Task::none()),
        };
        match message {
            Sheet::Start => Next::Stay(self.start()),
            Sheet::Started(Ok(started)) => {
                self.event("unsupported_catalog_started", || {
                    json!({"catalog": self.refusal.path, "new_catalog": started.path, "unremembered": started.unremembered})
                });
                Next::Open(started)
            }
            Sheet::Started(Err(reason)) => {
                self.event(
                    "unsupported_catalog_start_failed",
                    || json!({"catalog": self.refusal.path, "reason": reason}),
                );
                self.phase = Phase::Failed(reason.clone());
                self.next = new_catalog_path(&self.refusal.path, self.refusal.expected, exists);
                // An evidence run that chose Start ends with the reason rather than waiting.
                if self.evidence.is_some() {
                    return Next::Stay(
                        self.end(Some(format!("Start a new catalog failed: {reason}"))),
                    );
                }
                Next::Stay(Task::none())
            }
            Sheet::Quit => Next::Stay(self.quit()),
            Sheet::OpenReleases => Next::Stay(self.open_releases()),
            Sheet::ReleasesOpened(result) => {
                self.link_problem = result.err();
                Next::Stay(Task::none())
            }
            Sheet::Frame => Next::Stay(self.capture()),
            Sheet::Captured(shot) => Next::Stay(self.save(shot)),
            Sheet::Saved(result) => Next::Stay(self.saved(result)),
        }
    }

    /// Start a new catalog: create and open it beside the refused one off the update loop, then
    /// remember it. Pressed again while it runs, it does nothing.
    pub(crate) fn start(&mut self) -> Task<Message> {
        if self.phase == Phase::Starting || self.next.is_none() {
            return Task::none();
        }
        self.phase = Phase::Starting;
        self.event(
            "unsupported_catalog_start",
            || json!({"catalog": self.refusal.path, "new_catalog": self.next}),
        );
        let prepared = self.prepared.clone();
        let refused = self.refusal.path.clone();
        let format = self.refusal.expected;
        tasks::owner_task(
            move || start_new(prepared, &refused, format),
            |result| Message::UnsupportedCatalog(Sheet::Started(result)),
        )
    }

    /// Quit: nothing is created or written, and the launch ends.
    pub(crate) fn quit(&mut self) -> Task<Message> {
        if self.phase == Phase::Starting {
            return Task::none();
        }
        self.event(
            "unsupported_catalog_quit",
            || json!({"catalog": self.refusal.path}),
        );
        self.end(None)
    }

    /// End the launch with no catalog open, after the log is finished; `problem` is what the
    /// process exits with.
    fn end(&mut self, problem: Option<String>) -> Task<Message> {
        if let Some(problem) = problem {
            *self.exit.lock().expect("never poisoned") = Some(problem);
        }
        self.event("shutdown", || json!({"while_loading": false}));
        super::usage_supervisor::shutdown();
        let log = self.log.diagnostics.take();
        Task::perform(
            async move {
                if let Some(log) = log {
                    log.finish();
                }
            },
            |_| (),
        )
        .then(|_| iced::exit())
    }

    /// Open the releases page in the person's browser. An evidence run never does: its window is
    /// not the person's.
    fn open_releases(&mut self) -> Task<Message> {
        if self.evidence.is_some() {
            return Task::none();
        }
        self.event(
            "unsupported_catalog_releases",
            || json!({"url": RELEASES_URL}),
        );
        tasks::owner_task(
            || crate::browser::open(RELEASES_URL),
            |result| Message::UnsupportedCatalog(Sheet::ReleasesOpened(result)),
        )
    }

    /// The new catalog is open: the editor takes the window over it.
    fn open(&mut self, started: Started) -> (Box<Editor>, Task<Message>) {
        let boot = started
            .boot
            .lock()
            .expect("never poisoned")
            .take()
            .expect("a started catalog is opened once");
        let (mut editor, task) = Editor::new(boot);
        if let Some(reason) = &started.unremembered {
            editor.status.text =
                format!("New catalog not remembered for the next launch: {reason}");
        }
        (Box::new(editor), task)
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let element = view::unsupported_catalog::screen(self.model());
        match &self.evidence {
            Some(evidence) => marked(element, &evidence.sync),
            None => element,
        }
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        let close = iced::event::listen_with(|event, _, _| {
            matches!(
                event,
                iced::Event::Window(iced::window::Event::CloseRequested)
            )
            .then_some(Message::Close)
        });
        // A still sheet asks for no frames of its own, so while the capture waits a tick asks
        // again, as the editor's evidence tick does; each is an update the window redraws after.
        match &self.evidence {
            Some(evidence) if evidence.pending => Subscription::batch([
                close,
                iced::window::frames().map(|_| Message::UnsupportedCatalog(Sheet::Frame)),
                iced::time::every(std::time::Duration::from_millis(250))
                    .map(|_| Message::UnsupportedCatalog(Sheet::Frame)),
            ]),
            _ => close,
        }
    }

    /// An evidence run's capture: once the frame drawn last shows the sheet as it is, read it back.
    fn capture(&mut self) -> Task<Message> {
        let state = self.state();
        let Some(evidence) = &mut self.evidence else {
            return Task::none();
        };
        if !evidence.pending || !evidence.sync.current() {
            return Task::none();
        }
        evidence.pending = false;
        evidence.state = Some(state);
        iced::window::oldest()
            .and_then(iced::window::screenshot)
            .map(|shot| Message::UnsupportedCatalog(Sheet::Captured(shot)))
    }

    /// What the capture's state records: the sheet as it is drawn and the launch it belongs to.
    pub(crate) fn state(&self) -> Value {
        json!({
            "unsupported_catalog": self.model().summary(),
            "refusal": {
                "catalog": self.refusal.path,
                "catalog_format": self.refusal.format,
                "expected_format": self.refusal.expected,
                "marked": self.refusal.marked,
                "detail": self.refusal.detail,
            },
            "new_catalog": self.next,
            "phase": match &self.phase {
                Phase::Offered => "offered",
                Phase::Starting => "starting",
                Phase::Failed(_) => "failed",
            },
            "hidden": self.prepared.config.hidden,
            "choice": self.evidence.as_ref().and_then(|e| e.choice).map(Choice::as_str),
        })
    }

    fn save(&mut self, shot: iced::window::Screenshot) -> Task<Message> {
        let Some(evidence) = &mut self.evidence else {
            return Task::none();
        };
        let state = evidence.state.take().unwrap_or(Value::Null);
        let dir = evidence.dir.clone();
        Task::perform(
            async move {
                ::image::save_buffer(
                    dir.join(CAPTURE),
                    &shot.rgba,
                    shot.size.width,
                    shot.size.height,
                    ::image::ColorType::Rgba8,
                )
                .map_err(|error| error.to_string())?;
                let frame = json!({"file": CAPTURE, "state": state, "capture_provenance": "window-renderer-readback", "color": "sRGB", "physical_size": [shot.size.width, shot.size.height], "scale": shot.scale_factor});
                std::fs::write(
                    dir.join(CAPTURE_STATE),
                    serde_json::to_vec_pretty(&frame).expect("frame is serializable"),
                )
                .map_err(|error| error.to_string())
            },
            |result| Message::UnsupportedCatalog(Sheet::Saved(result)),
        )
    }

    /// The capture is written: take the run's choice.
    fn saved(&mut self, result: Result<(), String>) -> Task<Message> {
        let choice = self.evidence.as_ref().and_then(|evidence| evidence.choice);
        self.event("unsupported_catalog_captured", || {
            json!({"file": CAPTURE, "state": CAPTURE_STATE, "error": result.as_ref().err(), "choice": choice.map(Choice::as_str)})
        });
        if let Err(error) = result {
            return self.end(Some(format!("Evidence write failed: {error}")));
        }
        match choice {
            Some(Choice::Start) => self.start(),
            Some(Choice::Quit) => self.quit(),
            None => self.end(Some(
                "the evidence run's catalog was refused and --evidence-unsupported-catalog \
                 names no choice"
                    .into(),
            )),
        }
    }
}

/// Whether anything is at `path`, a dangling link included.
fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// Create and open a new catalog beside `refused`, made in `format`, through the same path a launch
/// takes, then remember it with `preferences.set`. Blocking: the sheet runs it off the update loop.
/// The refused catalog is not opened, read or written.
pub(crate) fn start_new(
    mut prepared: Prepared,
    refused: &Path,
    format: i64,
) -> Result<Started, String> {
    let path = new_catalog_path(refused, format, exists)
        .ok_or("every name for a new catalog beside it is taken")?;
    let default = prepared
        .config
        .launch_catalog
        .as_ref()
        .and_then(|launch| launch.default.clone());
    prepared.config.launch_catalog = Some(LaunchCatalog {
        path: path.clone(),
        default,
        forced: None,
        missing: None,
    });
    let (owner, join) =
        lifecycle::own(&prepared, &path).map_err(|error| lifecycle::refused(&path, &error))?;
    let boot = lifecycle::boot(prepared, owner, join, &path);
    let client = boot.client.expect("a launch's boot registers its client");
    let unremembered = tasks::call(
        &boot.owner,
        client,
        "preferences.set",
        json!({"catalog": path}),
    )
    .err();
    Ok(Started {
        path,
        boot: Arc::new(Mutex::new(Some(boot))),
        unremembered,
    })
}

#[cfg(test)]
impl Unsupported {
    /// Whether a Quit has ended the launch with a problem to exit with.
    pub(crate) fn exit_problem(&self) -> Option<String> {
        self.exit.lock().expect("never poisoned").clone()
    }
}
