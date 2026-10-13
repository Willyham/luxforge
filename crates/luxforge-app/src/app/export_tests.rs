//! The desktop's export flow: the Export menu, the Export sheet and how it asks for plans, the
//! refusals that keep one export per window, the status line and progress bar each answer leaves,
//! and the reader that exists only while a job is live and sends the desktop only what changed.
//! The owner's answers are handed in as the messages the runtime delivers.
use super::{
    export::{
        BAR_DELAY, ExportChoice, ExportRequest, export_params, export_pass, exported_text,
        refused_text, write_now,
    },
    gesture,
    job_reads::reads,
    message::export::{ExportMessage, SheetMessage},
    tasks::CallError,
    testing::{
        Followed, boot, derive_ran, descriptors, finish, idle_workers, mark_no_derive,
        opened_with_modules,
    },
    *,
};
use crate::state::export_sheet::Fit;
use crate::state::palette::PaletteAction;
use luxforge_core::{ExportSettings, MetadataLevel};
use luxforge_test_macros::slow;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn sheet(editor: &mut Editor, message: SheetMessage) {
    let _ = editor.update(Message::Export(ExportMessage::Sheet(message)));
}

fn open_sheet(editor: &mut Editor) {
    let _ = editor.update(Message::Export(ExportMessage::OpenSheet {
        reference: false,
    }));
}

/// An export of the displayed entry into `/tmp/<name>` with today's full-size settings.
fn request(editor: &Editor, name: &str) -> ExportRequest {
    let state = editor
        .document
        .state
        .as_ref()
        .expect("a photograph is open");
    ExportRequest {
        asset_id: state.asset.id.clone(),
        entry_id: state.current_entry.id.clone(),
        settings: ExportSettings::baseline(MetadataLevel::None),
        destination: Some(std::path::PathBuf::from("/tmp").join(name)),
        reference: false,
        remember: None,
    }
}

fn start(editor: &mut Editor) {
    let request = request(editor, "photo-edited.jpg");
    let _ = editor.start_export(request);
}

/// The plan answered for `/tmp/<name>` and `export.write` answered `result`, as the chain's one
/// message delivers them.
fn sent(editor: &mut Editor, name: &str, plan: Value, result: Result<Value, CallError>) {
    let state = editor
        .document
        .state
        .as_ref()
        .expect("a photograph is open");
    let choice = ExportChoice {
        asset_id: state.asset.id.clone(),
        entry_id: state.current_entry.id.clone(),
        destination: std::path::PathBuf::from("/tmp").join(name),
        settings: ExportSettings::baseline(MetadataLevel::None),
        reference: false,
        plan,
        acknowledge_stale: None,
        remember: None,
    };
    let _ = editor.update(Message::Export(ExportMessage::Sent {
        choice: Some(Box::new(choice)),
        result,
    }));
}

fn quiet_plan() -> Value {
    json!({"suggested": null, "notices": []})
}

fn queued(job: &str) -> Result<Value, CallError> {
    Ok(json!({"job_id": job, "status": "queued"}))
}

#[test]
fn with_no_photograph_export_is_disabled_and_refused() {
    let (mut editor, catalog) = boot();
    assert!(!editor.workspace.title.can_export);
    open_sheet(&mut editor);
    assert!(editor.export.sheet.is_none());
    assert_eq!(editor.status.text, "Open a photograph to export it");
    let _ = editor.update(Message::Export(ExportMessage::Again));
    assert!(editor.export.run.is_none());
    assert!(editor.export_reader_subscription().is_none());
    finish(editor, catalog);
}

/// Export's busy half is the one refusal's, and the Export button's enabled state is the same
/// answer as the press: refused and disabled while a request is in flight, but neither for an open
/// draft, since an export writes the displayed entry the draft does not change.
#[test]
fn export_refusal_and_the_menu_agree() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    let agree = |editor: &mut Editor, case: &str| {
        editor.rederive();
        assert_eq!(
            editor.workspace.title.can_export,
            editor.export_refusal().is_none(),
            "{case}"
        );
        editor.workspace.title.can_export
    };
    editor.busy = true;
    assert!(!agree(&mut editor, "busy"));
    assert_eq!(
        editor.export_refusal(),
        editor.gesture_refusal(gesture::Starting::Export)
    );
    open_sheet(&mut editor);
    assert_eq!(editor.status.text, crate::state::IN_FLIGHT);
    assert!(editor.export.sheet.is_none(), "nothing opens");

    editor.busy = false;
    let (action, parameter) = crate::app::testing::patch_control(&editor);
    let _ = crate::app::testing::slide(&mut editor, &action, &parameter, 25.0);
    assert!(editor.slider_gesture().is_some());
    assert!(agree(&mut editor, "a draft is open"));
    open_sheet(&mut editor);
    assert!(editor.export.sheet.is_some(), "{}", editor.status.text);
    finish(editor, catalog);
}

/// The button opens the menu: Export…, Export again (disabled until something was exported) and
/// one row per preset. Export… opens the sheet on Web the first time, which asks for its plan; the
/// quality slider and the metadata level never ask, a size change while one is in flight waits as
/// one pending plan, and Cancel closes it with nothing exported.
#[test]
fn the_menu_opens_the_sheet_which_plans_only_what_it_must_one_at_a_time() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    let _ = editor.update(Message::Export(ExportMessage::Menu));
    assert!(editor.workspace.title.export_menu_open);
    let menu = editor.workspace.title.export_menu.clone().unwrap();
    assert_eq!(menu.again, None, "nothing has been exported yet");
    assert_eq!(
        menu.presets
            .iter()
            .map(|(_, name, detail)| (name.as_str(), detail.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Web", "JPEG \u{b7} 2048 px"),
            ("Print", "JPEG \u{b7} full \u{b7} 300 ppi"),
            ("Master", "TIFF \u{b7} 16-bit \u{b7} full \u{b7} 300 ppi"),
        ]
    );
    assert_eq!(editor.snapshot()["export"]["menu_open"], json!(true));

    open_sheet(&mut editor);
    assert!(!editor.workspace.title.export_menu_open);
    let open = editor.export.sheet.as_ref().unwrap();
    assert_eq!(open.preset_id.as_deref(), Some("luxforge.web"));
    assert!(open.plan.in_flight && open.plan.sent == 1);
    sheet(&mut editor, SheetMessage::Quality(55));
    sheet(&mut editor, SheetMessage::Metadata(MetadataLevel::All));
    let open = editor.export.sheet.as_ref().unwrap();
    assert!(!open.plan.pending && open.plan.sent == 1, "neither asks");
    sheet(&mut editor, SheetMessage::Fit(Fit::Full));
    sheet(&mut editor, SheetMessage::Fit(Fit::LongEdge));
    sheet(&mut editor, SheetMessage::Fit(Fit::Full));
    let open = editor.export.sheet.as_ref().unwrap();
    assert!(open.plan.pending && open.plan.sent == 1, "one in flight");
    assert!(!editor.workspace.export_sheet.as_ref().unwrap().can_export);
    // The first plan answers: the newest settings are planned, once.
    editor.answer_sheet_plan();
    let open = editor.export.sheet.as_ref().unwrap();
    assert_eq!(open.plan.sent, 2);
    assert!(!open.plan.in_flight && !open.plan.pending);
    let shown = editor.workspace.export_sheet.as_ref().unwrap();
    assert!(shown.can_export, "{:?}", shown.refusals);
    assert_eq!(shown.cards[0].detail, "Changed from preset");

    sheet(&mut editor, SheetMessage::Close);
    assert!(editor.export.sheet.is_none() && editor.export.run.is_none());
    assert!(editor.workspace.export_sheet.is_none());
    finish(editor, catalog);
}

#[test]
fn a_closed_export_sheets_late_plan_cannot_answer_the_reopened_sheet() {
    use crate::state::export_sheet::SheetPlan;

    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    open_sheet(&mut editor);
    let generation = editor.export.sheet_generation;
    let settings = editor.export.sheet.as_ref().unwrap().settings.clone();
    sheet(&mut editor, SheetMessage::Close);
    editor.document.display_entry = Some(luxforge_core::EntryId::new());
    open_sheet(&mut editor);
    let target = editor.export.sheet.as_ref().unwrap().target.clone();
    assert_ne!(editor.export.sheet_generation, generation);
    // Even identical settings belong to different sheets and saved entries. Both an answer and
    // a refusal for the closed one leave the new plan in flight and Export waiting for it.
    for result in [
        Ok(SheetPlan::Single(
            json!({"suggested": "/tmp/old-photo.jpg", "notices": []}),
        )),
        Err("old entry refused".into()),
    ] {
        let _ = editor.update(Message::Export(ExportMessage::Planned {
            generation,
            settings: Box::new(settings.clone()),
            result,
        }));
        let reopened = editor.export.sheet.as_ref().unwrap();
        assert_eq!(reopened.target, target);
        assert!(reopened.plan.in_flight);
        assert!(reopened.plan.answer.is_none());
        assert!(reopened.plan.error.is_none());
        assert!(reopened.plan.answered.is_none());
        assert!(!editor.workspace.export_sheet.as_ref().unwrap().can_export);
    }
    // The current sheet's answer still finishes its own plan.
    let _ = editor.update(Message::Export(ExportMessage::Planned {
        generation: editor.export.sheet_generation,
        settings: Box::new(settings),
        result: Ok(SheetPlan::Single(quiet_plan())),
    }));
    assert!(!editor.export.sheet.as_ref().unwrap().plan.in_flight);
    assert!(editor.export.sheet.as_ref().unwrap().plan.answer.is_some());
    finish(editor, catalog);
}

/// A queued export with its first read already applied: the desktop holds a running record at
/// `progress`.
fn running_export(progress: f64) -> (Editor, std::path::PathBuf) {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    idle_workers(&mut editor);
    start(&mut editor);
    sent(
        &mut editor,
        "photo-edited.jpg",
        quiet_plan(),
        queued("job-1"),
    );
    let _ = editor.update(read(
        "job-1",
        json!({"status":"running","progress":{"fraction": progress, "message": "rendering"}}),
    ));
    (editor, catalog)
}

fn read(job_id: &str, record: Value) -> Message {
    let mut record = record;
    record["job_id"] = json!(job_id);
    Message::Export(ExportMessage::Read {
        job_id: job_id.into(),
        result: Ok(record),
    })
}

#[test]
fn a_queued_job_is_followed_by_a_reader_until_it_ends_and_says_what_was_written() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    start(&mut editor);
    assert!(
        editor.export_reader_subscription().is_none(),
        "nothing is queued at the core yet"
    );
    sent(
        &mut editor,
        "photo-edited.jpg",
        quiet_plan(),
        queued("job-1"),
    );
    assert!(editor.export_reader_subscription().is_some());

    // A read for a job this window no longer follows is dropped.
    let _ = editor.update(read("job-0", json!({"status":"ready"})));
    assert!(editor.export.run.is_some());

    let _ = editor.update(read("job-1", json!({"status":"running","progress":0.5})));
    assert!(editor.export_reader_subscription().is_some());

    let _ = editor.update(read(
        "job-1",
        json!({"status":"ready","progress":1.0,
            "result":{"path":"/tmp/photo-edited.jpg","bytes":8_412_345,"width":6000,"height":4000,"quality":90,"metadata":[]}}),
    ));
    assert_eq!(
        editor.status.text,
        exported_text("photo-edited.jpg", 6000, 4000, 8_412_345, Some(90), None)
    );
    assert!(editor.export.run.is_none());
    assert!(
        editor.export_reader_subscription().is_none(),
        "no reader once the job has ended"
    );
    finish(editor, catalog);
}

/// The status bar shows an export's progress from the start when its plan warned `large-image`,
/// and otherwise only once a read finds it has run a second: a quick export never shows a bar.
/// The bar names the phase and the file, at the job's fraction, and opens the Performance section.
#[test]
fn the_progress_bar_shows_from_the_start_for_a_warned_export_and_after_a_second_otherwise() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    let progress = |fraction: f64, phase: &str| {
        read(
            "job-1",
            json!({"status":"running","progress":{"fraction":fraction,"message":phase}}),
        )
    };
    // A quick export: read at once, no bar.
    start(&mut editor);
    sent(&mut editor, "quick.jpg", quiet_plan(), queued("job-1"));
    let _ = editor.update(progress(0.3, "rendering"));
    assert_eq!(editor.workspace.long_work.export, None);
    // The same export a second on shows it.
    editor.export.run.as_mut().unwrap().queued = Some(std::time::Instant::now() - BAR_DELAY);
    let _ = editor.update(progress(0.64, "encoding"));
    let bar = editor.workspace.long_work.export.clone().expect("the bar");
    assert_eq!(bar.label, "Encoding quick.jpg");
    assert_eq!(bar.fraction, Some(0.64));
    let _ = editor.update(read("job-1", json!({"status":"cancelled"})));
    assert_eq!(editor.workspace.long_work.export, None);

    // A warned export shows the bar from its queueing, rising with each read.
    start(&mut editor);
    let warned = json!({"suggested": "/tmp/large.jpg", "notices": [
        {"code": "large-image", "refuses": false, "bytes": 720_000_000, "message": "Large image"}]});
    sent(&mut editor, "large.jpg", warned, queued("job-1"));
    assert_eq!(
        editor
            .workspace
            .long_work
            .export
            .as_ref()
            .map(|bar| bar.fraction),
        Some(Some(0.0))
    );
    let _ = editor.update(progress(0.12, "optimising"));
    let bar = editor.workspace.long_work.export.clone().unwrap();
    assert_eq!(
        (bar.label.as_str(), bar.fraction),
        ("Optimising large.jpg", Some(0.12))
    );
    assert_eq!(
        editor.snapshot()["export"]["status_bar"]["label"],
        "Optimising large.jpg"
    );
    finish(editor, catalog);
}

/// The reader is identified by the job alone: the subscription the desktop rebuilds after every
/// message is the same one while the job lives, so its reader keeps its memory of what it sent,
/// and another job is another reader.
#[test]
fn an_export_reader_is_identified_by_its_job_alone() {
    use iced::advanced::subscription::{Hasher, into_recipes};
    use std::hash::Hasher as _;
    let identity = |editor: &Editor| {
        let mut recipes = into_recipes(editor.export_reader_subscription().expect("a live job"));
        assert_eq!(recipes.len(), 1);
        let mut hasher = Hasher::default();
        recipes.remove(0).hash(&mut hasher);
        hasher.finish()
    };
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    start(&mut editor);
    sent(
        &mut editor,
        "photo-edited.jpg",
        quiet_plan(),
        queued("job-1"),
    );
    let first = identity(&editor);
    let _ = editor.update(read("job-1", json!({"status":"running","progress":0.5})));
    assert_eq!(identity(&editor), first, "a read does not make a new one");
    if let Some(run) = &mut editor.export.run {
        run.job_id = Some("job-2".into());
    }
    assert_ne!(identity(&editor), first, "another job is another reader");
    finish(editor, catalog);
}

/// A scripted export job: what `job.read` answers on each read, the last answer repeating, and
/// how many reads were made.
struct ScriptedJob {
    script: Vec<Result<Value, String>>,
    reads: AtomicUsize,
}

impl ScriptedJob {
    /// Feed notifications into the reader's filtering seam without a test clock.
    fn follow(script: Vec<Result<Value, String>>) -> (Arc<Self>, Followed<Message>) {
        let job = Arc::new(Self {
            script,
            reads: AtomicUsize::new(0),
        });
        let held = job.clone();
        let mut pass = export_pass("job-1".into(), move || {
            let read = held.reads.fetch_add(1, Ordering::Relaxed);
            held.script[read.min(held.script.len() - 1)].clone()
        });
        let stream = Followed::new(reads(move || std::future::ready(pass())));
        (job, stream)
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }
}

/// What an export read carries: the job and its answer.
fn read_of(message: Message) -> (String, Result<Value, String>) {
    let Message::Export(ExportMessage::Read { job_id, result }) = message else {
        panic!("the export's reader sent {message:?}");
    };
    (job_id, result)
}

fn running(progress: f64) -> Value {
    json!({"job_id":"job-1","status":"running","progress":progress})
}

fn ready() -> Value {
    json!({"job_id":"job-1","status":"ready","progress":1.0,
        "result":{"path":"/tmp/photo-edited.jpg","bytes":8_412_345,"width":6000,"height":4000,"quality":90,"metadata":[]}})
}

/// `record` read `times` over.
fn repeated(record: Value, times: usize) -> impl Iterator<Item = Result<Value, String>> {
    std::iter::repeat_n(Ok(record), times)
}

/// While an export's record stays as it is, however many times it is read, the reader sends the
/// update loop exactly one message, the first, and then nothing until the record changes or the
/// job ends: an unchanged read costs no message, no update and no view rebuild.
#[test]
fn an_export_reader_sends_one_message_for_a_job_that_does_not_change() {
    let script = repeated(running(0.0), 100)
        .chain(repeated(running(0.5), 50))
        .chain([Ok(ready())])
        .collect();
    let (job, mut stream) = ScriptedJob::follow(script);

    let first = stream.next().expect("the first read is sent");
    assert_eq!(read_of(first), ("job-1".to_owned(), Ok(running(0.0))));
    assert_eq!(job.reads(), 1, "the first read is made at once");

    // The next message is the change, a hundred reads on: ninety-nine reads of the same record
    // in between sent nothing, or the message that came next would have been one of them.
    let moved = stream.next().expect("a change is sent");
    assert_eq!(read_of(moved), ("job-1".to_owned(), Ok(running(0.5))));
    assert_eq!(job.reads(), 101);

    // And the one after it is the end, fifty reads on, with the same silence between.
    let end = stream.next().expect("the end is sent");
    assert_eq!(read_of(end), ("job-1".to_owned(), Ok(ready())));
    assert_eq!(job.reads(), 151);

    // Nothing follows the end, and no read is made for it.
    assert!(stream.next().is_none());
    assert_eq!(job.reads(), 151);
}

/// Every status that is not queued or running ends the export the way the desktop's handler
/// takes it: a failed or cancelled job, and a record with no status at all, are each sent once,
/// and the reader reads no more.
#[test]
fn an_export_reader_ends_on_any_status_but_queued_or_running() {
    for record in [
        json!({"job_id":"job-1","status":"failed","error":{"code":"export","message":"no space left"}}),
        json!({"job_id":"job-1","status":"cancelled"}),
        json!({"job_id":"job-1"}),
    ] {
        let script = repeated(running(0.5), 3)
            .chain([Ok(record.clone())])
            .collect();
        let (job, mut stream) = ScriptedJob::follow(script);
        assert_eq!(
            read_of(stream.next().unwrap()),
            ("job-1".to_owned(), Ok(running(0.5)))
        );
        assert_eq!(
            read_of(stream.next().unwrap()),
            ("job-1".to_owned(), Ok(record))
        );
        assert!(stream.next().is_none());
        assert_eq!(job.reads(), 4, "the end is the last read");
    }
}

/// A read that fails is sent, once, wherever it falls, and the reader reads no more.
#[test]
fn an_export_reader_sends_a_failed_read_and_reads_no_more() {
    let script = repeated(running(0.5), 5)
        .chain([Err("the owner stopped".to_owned())])
        .collect();
    let (job, mut stream) = ScriptedJob::follow(script);
    assert_eq!(
        read_of(stream.next().unwrap()),
        ("job-1".to_owned(), Ok(running(0.5)))
    );
    assert_eq!(
        read_of(stream.next().unwrap()),
        ("job-1".to_owned(), Err("the owner stopped".to_owned()))
    );
    assert!(stream.next().is_none());
    assert_eq!(job.reads(), 6);

    // Failing at the first read is sent too.
    let (job, mut stream) = ScriptedJob::follow(vec![Err("the owner stopped".to_owned())]);
    assert_eq!(
        read_of(stream.next().unwrap()),
        ("job-1".to_owned(), Err("the owner stopped".to_owned()))
    );
    assert!(stream.next().is_none());
    assert_eq!(job.reads(), 1);
}

/// A job that is over by the first read is sent as its first and last message.
#[test]
fn an_export_reader_of_a_job_already_over_sends_its_end_and_stops() {
    let (job, mut stream) = ScriptedJob::follow(vec![Ok(ready())]);
    assert_eq!(
        read_of(stream.next().unwrap()),
        ("job-1".to_owned(), Ok(ready()))
    );
    assert!(stream.next().is_none());
    assert_eq!(job.reads(), 1, "one read, and no more");
}

/// A read that ends or fails is a change the full update applies: the status line says why, the
/// export is forgotten and the Export button is enabled again, whatever the record held was.
#[test]
#[slow]
fn a_read_that_ends_or_fails_ends_the_export_and_says_so() {
    for (record, status) in [
        (
            json!({"status":"failed","error":{"code":"export","message":"no space left"}}),
            "Export failed: no space left",
        ),
        (json!({"status":"cancelled"}), "Export cancelled"),
    ] {
        let (mut editor, catalog) = running_export(0.5);
        let updates = editor.full_updates;
        let _ = editor.update(read("job-1", record));
        assert_eq!(editor.full_updates, updates + 1, "{status}");
        assert_eq!(editor.status.text, status);
        assert!(editor.export.run.is_none());
        assert!(editor.workspace.title.can_export);
        assert!(editor.export_reader_subscription().is_none());
        finish(editor, catalog);
    }

    let (mut editor, catalog) = running_export(0.5);
    let updates = editor.full_updates;
    let _ = editor.update(Message::Export(ExportMessage::Read {
        job_id: "job-1".into(),
        result: Err("the owner stopped".into()),
    }));
    assert_eq!(editor.full_updates, updates + 1);
    assert_eq!(editor.status.text, "Export failed: the owner stopped");
    assert!(editor.export.run.is_none());
    assert!(editor.workspace.title.can_export);
    finish(editor, catalog);
}

/// The job's end shows in the status line and re-enables Export through the full update, and
/// the derive that follows it is what enables the button.
#[test]
fn the_end_of_the_job_takes_the_full_update_and_shows_in_the_status_line_and_the_button() {
    let (mut editor, catalog) = running_export(0.5);
    assert!(!editor.workspace.title.can_export, "an export is running");
    let updates = editor.full_updates;
    mark_no_derive(&editor);
    let _ = editor.update(read("job-1", ready()));
    assert_eq!(editor.full_updates, updates + 1);
    assert!(derive_ran(&editor));
    assert_eq!(
        editor.status.text,
        exported_text("photo-edited.jpg", 6000, 4000, 8_412_345, Some(90), None)
    );
    assert!(editor.workspace.title.can_export);
    finish(editor, catalog);
}

#[test]
fn a_destination_that_exists_is_refused_in_the_status_bar() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    start(&mut editor);
    sent(
        &mut editor,
        "photo-edited.jpg",
        quiet_plan(),
        Err(CallError {
            code: "conflict".into(),
            message: "a file already exists at the destination".into(),
            data: None,
            job_id: None,
        }),
    );
    assert_eq!(editor.status.text, refused_text("photo-edited.jpg"));
    assert!(editor.export.run.is_none());

    start(&mut editor);
    sent(
        &mut editor,
        "b.png",
        quiet_plan(),
        Err(CallError {
            code: "validation".into(),
            message: "the destination must end in .jpg or .jpeg".into(),
            data: None,
            job_id: None,
        }),
    );
    assert_eq!(
        editor.status.text,
        "Not exported: the destination must end in .jpg or .jpeg"
    );
    finish(editor, catalog);
}

/// The palette lists the Export menu's entries, one per preset, and the reference renderer's
/// export, which opens the sheet marked to send `reference: true`.
#[test]
fn the_palette_lists_the_export_menu_and_the_reference_export() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    let _ = editor.update(Message::Palette(
        crate::app::message::palette::PaletteMessage::Open,
    ));
    let _ = editor.update(Message::Palette(
        crate::app::message::palette::PaletteMessage::Query("export".into()),
    ));
    let exports: Vec<_> = editor
        .workspace
        .palette
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.action,
                PaletteAction::Export { .. }
                    | PaletteAction::ExportAgain
                    | PaletteAction::ExportWithPreset(_)
            )
        })
        .map(|entry| (entry.label.as_str(), entry.action.clone()))
        .collect();
    assert_eq!(
        exports,
        vec![
            ("Export\u{2026}", PaletteAction::Export { reference: false }),
            (
                "Export with preset: Web",
                PaletteAction::ExportWithPreset("luxforge.web".into())
            ),
            (
                "Export with preset: Print",
                PaletteAction::ExportWithPreset("luxforge.print".into())
            ),
            (
                "Export with preset: Master",
                PaletteAction::ExportWithPreset("luxforge.master".into())
            ),
            (
                "Export reference render\u{2026}",
                PaletteAction::Export { reference: true }
            ),
        ]
    );
    let _ = editor.update(Message::Palette(
        crate::app::message::palette::PaletteMessage::Query("reference render".into()),
    ));
    let _ = editor.update(Message::Palette(
        crate::app::message::palette::PaletteMessage::Run,
    ));
    let open = editor.export.sheet.as_ref().expect("the sheet opened");
    assert!(open.reference);
    assert_eq!(
        editor.snapshot()["export"]["sheet"]["reference"],
        json!(true)
    );
    finish(editor, catalog);
}

/// The reference export sends what an API client sends for it, `reference: true`, through the same
/// chain as every other, which sends no `reference`: the owner echoes each as it took it, and the
/// reference renderer writes the file, naming the reference as requested where it was asked for.
#[test]
#[slow]
fn the_reference_export_sends_reference_true_and_the_others_none() {
    use super::export::read_now;
    let dir = luxforge_testbase::paths::temp_dir("reference-export");
    let catalog = dir.join("catalog.sqlite");
    let (editor, _, _) = super::testing::real_photo(&catalog);
    let export = |name: &str, reference: bool| {
        let request = ExportRequest {
            destination: Some(dir.join(name)),
            reference,
            ..request(&editor, name)
        };
        let (choice, queued) = write_now(&editor.owner, editor.client, request);
        let choice = choice.expect("the plan named it");
        let sent = export_params(&choice);
        assert_eq!(
            sent.get("reference"),
            reference.then_some(&json!(true)),
            "{sent}"
        );
        let queued = queued.unwrap();
        let job = queued["job_id"].as_str().unwrap().to_owned();
        let read = luxforge_testbase::wait_for("the export to end", || {
            let read = read_now(&editor.owner, editor.client, &job).unwrap();
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        });
        assert_eq!(read["status"], "ready", "{read}");
        (queued, read)
    };
    let (queued, read) = export("asked.jpg", true);
    assert_eq!(queued["reference"], json!(true));
    assert_eq!(
        read["result"]["renderer"],
        json!({"record": "reference", "reason": "requested"})
    );
    // An owner with no GPU provider, as a test's is, renders the other with the reference too,
    // for no reason of its own: the file is the same.
    let (queued, read) = export("default.jpg", false);
    assert_eq!(queued["reference"], json!(false), "its default");
    assert_eq!(
        read["result"]["renderer"],
        json!({"record": "reference", "reason": null})
    );
    assert!(
        std::fs::read(dir.join("asked.jpg")).unwrap()
            == std::fs::read(dir.join("default.jpg")).unwrap()
    );
    finish(editor, catalog);
    let _ = std::fs::remove_dir_all(&dir);
}

/// One whole export against a real owner, with the owner's answers handed in as the runtime would:
/// the plan and the queued job in the chain's one message and the messages its reader sends, then
/// the same file again, refused.
#[test]
#[slow]
fn an_export_through_the_owner_writes_a_new_file_and_never_replaces_it() {
    use super::{export::export_reads, job_reads::Reader};
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-export-{}-{}.sqlite",
        std::process::id(),
        super::tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let (mut editor, _, _) = super::testing::real_photo(&catalog);
    let folder = catalog.with_extension("exports");
    std::fs::create_dir_all(&folder).unwrap();
    let destination = folder.join("photo-edited.jpg");

    let export_once = |editor: &mut Editor| {
        let request = ExportRequest {
            settings: ExportSettings {
                pixels_per_inch: Some(144),
                folder: luxforge_core::ExportFolder::Path(folder.clone()),
                suffix: "-edited".into(),
                ..ExportSettings::baseline(MetadataLevel::All)
            },
            destination: Some(destination.clone()),
            ..request(editor, "photo-edited.jpg")
        };
        let _ = editor.start_export(request.clone());
        assert!(editor.export.active(), "{}", editor.status.text);
        let (choice, queued) = write_now(&editor.owner, editor.client, request);
        let choice = choice.unwrap();
        assert_eq!(choice.plan["entry_id"], json!(choice.entry_id));
        assert_eq!(
            export_params(&choice)["settings"],
            json!({
                "format": "jpeg", "size": {"fit": "full"}, "quality": 90, "optimise": false,
                "chroma": "444", "limit_bytes": null, "sharpen": null, "metadata": "all",
                "pixels_per_inch": 144, "folder": {"path": folder}, "suffix": "-edited",
            })
        );
        let _ = editor.update(Message::Export(ExportMessage::Sent {
            choice: Some(Box::new(choice)),
            result: queued,
        }));
        // The reader the subscription starts while the job is live: what it sends goes to the
        // update function, and the first message is what was read at once.
        if let Some(job) = editor
            .export
            .run
            .as_ref()
            .and_then(|run| run.job_id.clone())
        {
            let mut reader = Followed::new(export_reads(&Reader {
                identity: job,
                owner: editor.owner.clone(),
                client: editor.client,
                presentation_visible: true,
            }));
            let mut previous = None;
            while let Some(message) = reader.next() {
                let Message::Export(ExportMessage::Read {
                    result: Ok(record), ..
                }) = &message
                else {
                    panic!("unexpected export message {message:?}");
                };
                assert_ne!(
                    previous.as_ref(),
                    Some(record),
                    "unchanged export records never reach update"
                );
                previous = Some(record.clone());
                let _ = editor.update(message);
            }
            assert_eq!(
                previous.unwrap()["status"],
                "ready",
                "the terminal record is delivered exactly once"
            );
            assert!(editor.export.run.is_none(), "the last message was the end");
        }
    };

    export_once(&mut editor);
    let written = std::fs::read(&destination).unwrap();
    let decoded = image::load_from_memory(&written).unwrap();
    // The JFIF header right after SOI: its units, then 144 pixels per inch both ways.
    assert_eq!(&written[2..4], [0xff, 0xe0]);
    assert_eq!(&written[6..11], b"JFIF\0");
    assert_eq!(&written[13..18], [1, 0, 144, 0, 144]);
    let expected = format!(
        "Exported photo-edited.jpg \u{b7} {} \u{d7} {} \u{b7} ",
        decoded.width(),
        decoded.height()
    );
    assert!(
        editor.status.text.starts_with(&expected) && editor.status.text.ends_with("quality 90"),
        "{}",
        editor.status.text
    );

    // The same file again is refused, and the file is exactly as it was.
    export_once(&mut editor);
    assert_eq!(editor.status.text, refused_text("photo-edited.jpg"));
    assert_eq!(std::fs::read(&destination).unwrap(), written);
    assert!(editor.export.run.is_none());

    std::fs::remove_dir_all(&folder).unwrap();
    finish(editor, catalog);
}

/// A real slider commit must be the After raster and the JPEG target, including while the
/// comparison session selects the Original for its Before surface.
#[test]
#[slow]
fn a_slider_edit_is_compared_and_exported_while_before_is_selected() {
    use super::{
        message::{history::HistoryMessage, preview::PreviewMessage},
        testing::{let_go, real_photo, run_commit, slide},
    };
    use luxforge_core::PreviewRequest;

    let dir = luxforge_testbase::paths::temp_dir("compare-export-edited");
    let catalog = dir.join("catalog.sqlite");
    let (mut editor, asset, _) = real_photo(&catalog);
    let settle = |editor: &mut Editor| {
        luxforge_testbase::wait_until("the saved entry's exact frame", || {
            let _ = editor.update(Message::Preview(PreviewMessage::Poll));
            editor.presentation.exact().is_some()
                && editor.presentation.presented_entry == editor.document.display_entry
                && editor.presentation.displayed_draft_id.is_none()
        });
    };
    settle(&mut editor);
    let original = editor.presentation.exact().unwrap().raster.clone();
    let original_entry = editor.displayed_entry().unwrap();
    let _ = slide(&mut editor, "set-basic", "exposure", 0.8);
    let _ = let_go(&mut editor, "set-basic", "exposure");
    assert!(run_commit(&mut editor));
    settle(&mut editor);
    let edited = editor.presentation.exact().unwrap().raster.clone();
    let edited_entry = editor.displayed_entry().unwrap();
    assert_ne!(edited_entry, original_entry);
    assert_ne!(edited.rgba, original.rgba, "the commit changes real pixels");

    let _ = editor.update(Message::History(HistoryMessage::CompareToggle));
    let after = editor.presentation.compare_after.as_ref().unwrap();
    assert_eq!(after.full().pixels(), edited.rgba.as_slice());
    let before_entry = editor
        .session
        .preview
        .selected_entry(&asset)
        .unwrap()
        .clone();
    assert_eq!(before_entry, original_entry);
    let job = editor
        .owner
        .preview_job(
            PreviewRequest::new(editor.client, asset.clone())
                .entry(Some(before_entry))
                .analyse(),
        )
        .unwrap();
    let session = editor.session.clone();
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        tasks::PreviewPayload { job, session },
    )))));
    settle(&mut editor);
    assert_eq!(
        editor.presentation.exact().unwrap().raster.rgba,
        original.rgba
    );

    // Export with a preset exports the displayed entry, which Compare fixes at After.
    let destination = dir.join("edited.jpg");
    editor.list_export_presets();
    let request = ExportRequest {
        destination: Some(destination.clone()),
        ..editor.preset_request("luxforge.web").unwrap()
    };
    assert_eq!(request.entry_id, edited_entry, "comparison exports After");
    let request = ExportRequest {
        settings: ExportSettings {
            size: luxforge_core::OutputSize::Full,
            optimise: false,
            ..request.settings
        },
        ..request
    };
    let (choice, queued) = write_now(&editor.owner, editor.client, request);
    assert_eq!(choice.unwrap().plan["entry_id"], json!(edited_entry));
    let queued = queued.unwrap();
    let job = queued["job_id"].as_str().unwrap();
    luxforge_testbase::wait_until("the edited JPEG", || {
        let read = super::export::read_now(&editor.owner, editor.client, job).unwrap();
        if read["status"] == "ready" {
            return true;
        }
        assert!(
            matches!(read["status"].as_str(), Some("queued" | "running")),
            "{read}"
        );
        false
    });
    let decoded = image::open(destination).unwrap().to_rgba8();
    let delta = |reference: &luxforge_core::Raster| -> u64 {
        decoded
            .as_raw()
            .iter()
            .zip(reference.rgba.iter())
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum()
    };
    assert!(
        delta(&edited) < delta(&original),
        "JPEG pixels follow the edit"
    );
    finish(editor, catalog);
    std::fs::remove_dir_all(dir).unwrap();
}

/// An export refused for its entry's stale frozen patches is held, never acknowledged on its own:
/// the canvas names each patch and why, the status bar says the export waits, and another export
/// cannot start. Export anyway sends it again acknowledging exactly that entry and those patches;
/// Cancel lets it go.
#[test]
fn a_stale_refusal_is_held_until_export_anyway_acknowledges_exactly_what_it_named() {
    use crate::state::canvas::NoticeAction;
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    let entry = editor
        .document
        .state
        .as_ref()
        .unwrap()
        .current_entry
        .id
        .clone();
    let refusal = || CallError {
        code: "conflict".into(),
        message: "the entry holds stale frozen patches".into(),
        data: Some(
            json!({"code": "stale-unacknowledged", "entry_id": entry, "stale": [
                {"operation": "correction-2", "name": "Patch 2", "reasons": ["earlier-correction"]},
                {"operation": "correction-4", "name": "Patch 4", "reasons": ["development"]},
            ]}),
        ),
        job_id: None,
    };
    for answer in ["anyway", "cancel"] {
        start(&mut editor);
        sent(&mut editor, "stale.jpg", quiet_plan(), Err(refusal()));
        assert!(editor.export.run.is_none());
        assert_eq!(
            editor.status.text,
            "Not exported yet: 2 frozen patches are stale in stale.jpg; Export anyway or Cancel"
        );
        let notice = editor
            .workspace
            .canvas
            .notices
            .iter()
            .find(|notice| notice.title == "2 stale patches in this export")
            .expect("the held export's notice")
            .clone();
        assert_eq!(
            notice.body,
            "Patch 2: a correction below it changed since it was generated.\n\
             Patch 4: the RAW development changed since it was generated.\n\
             Export anyway writes them as they were accepted."
        );
        assert_eq!(
            notice.actions,
            [
                ("Export anyway".to_owned(), NoticeAction::ExportAnyway),
                ("Cancel".to_owned(), NoticeAction::DismissExport),
            ]
        );
        assert!(!editor.workspace.title.can_export);
        assert_eq!(
            editor.held_stale_operation(1),
            Some("correction-4".to_owned())
        );
        if answer == "cancel" {
            let _ = editor.update(Message::Export(ExportMessage::Dismiss));
            assert!(editor.export.held.is_none() && editor.export.run.is_none());
            assert_eq!(editor.status.text, "Export cancelled");
            assert!(editor.workspace.title.can_export);
            continue;
        }
        let _ = editor.update(Message::Export(ExportMessage::Anyway));
        assert!(editor.export.held.is_none());
        let run = editor.export.run.as_ref().expect("the export runs again");
        let choice = run.choice.as_ref().unwrap();
        let acknowledgement =
            json!({"entry_id": entry, "operations": ["correction-2", "correction-4"]});
        assert_eq!(choice.acknowledge_stale, Some(acknowledgement.clone()));
        assert_eq!(export_params(choice)["acknowledge_stale"], acknowledgement);
        assert_eq!(editor.status.text, "Exporting stale.jpg\u{2026}");
        // The job's end lets the next export start; nothing stays held.
        let held = run.choice.clone();
        let _ = editor.update(Message::Export(ExportMessage::Sent {
            choice: held,
            result: Err(CallError {
                code: "validation".into(),
                message: "stop here".into(),
                data: None,
                job_id: None,
            }),
        }));
        assert!(editor.export.run.is_none() && editor.export.held.is_none());
    }
    finish(editor, catalog);
}

impl Editor {
    /// The held export's stale patch at `index`, as the evidence summary records it.
    fn held_stale_operation(&self, index: usize) -> Option<String> {
        self.snapshot()["export"]["held_stale"]["stale"][index]["operation"]
            .as_str()
            .map(str::to_owned)
    }
}
