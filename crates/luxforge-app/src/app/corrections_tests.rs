//! The Corrections workspace against a real owner, catalog and the production Corrections module.
//!
//! Every gesture runs through the update function as the canvas, the band and the keyboard send
//! it; the owner tasks it starts are run here as the plain calls they wrap. Each committing gesture
//! is checked as exactly one history entry with its label, and its stored result against the one an
//! independent JSON client's request makes.
use super::{
    Editor,
    corrections::answer_reads,
    draft::Round,
    message::{
        Message,
        corrections::{CanvasHit, CorrectionMessage},
        sync::SyncMessage,
        view::ViewMessage,
    },
    tasks::{self, call},
    testing,
};
use crate::brush::{Handle, Modifiers};
use iced::keyboard::{Key, Modifiers as Keys, key::Named};
use luxforge_core::{AssetId, ClientId, OwnerHandle, POINTER_MODE};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

const MODULE: &str = "luxforge.corrections";

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "luxforge-corrections-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

struct Correcting {
    editor: Editor,
    catalog: PathBuf,
    asset: AssetId,
    agent: ClientId,
}

impl Drop for Correcting {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.catalog);
    }
}

impl Correcting {
    fn opened() -> Self {
        let catalog = scratch("catalog.sqlite");
        let (editor, asset, agent) = testing::real_photo(&catalog);
        let mut correcting = Self {
            editor,
            catalog,
            asset,
            agent,
        };
        correcting.enter();
        correcting
    }

    fn owner(&self) -> OwnerHandle {
        self.editor.owner.clone()
    }

    fn update(&mut self, message: Message) {
        let _ = self.editor.update(message);
        answer_reads(&mut self.editor);
    }

    fn message(&mut self, message: CorrectionMessage) {
        self.update(Message::Corrections(message));
    }

    fn adopt_session(&mut self) {
        let (session, _) = call(
            &self.owner(),
            self.editor.client,
            "session.state",
            json!({}),
        )
        .unwrap();
        let session = serde_json::from_value(session).unwrap();
        self.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
    }

    /// Enter Corrections with its letter, through the one key table.
    fn enter(&mut self) {
        self.key(Key::Character("q".into()), Keys::empty());
        let _ = call(
            &self.owner(),
            self.editor.client,
            "workspace.set",
            json!({ "mode": MODULE }),
        );
        self.adopt_session();
        assert!(
            self.editor.corrections_active(),
            "{}",
            self.editor.status.text
        );
        assert!(self.editor.correction_reads.map.is_some(), "the pins' map");
        assert!(self.editor.correction_listing().is_some(), "the list");
    }

    fn key(&mut self, key: Key, modifiers: Keys) {
        let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        });
        if let Some(message) = super::keymap::keymap(
            &event,
            iced::event::Status::Ignored,
            &self.editor.key_context(),
        ) {
            self.update(message);
        }
    }

    fn history(&self) -> Vec<String> {
        let (page, _) = call(
            &self.owner(),
            self.agent,
            "history.list",
            json!({"asset_id": self.asset, "before_sequence": null, "limit": 100}),
        )
        .unwrap();
        page["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["label"].as_str().unwrap().to_owned())
            .collect()
    }

    fn corrections(&self) -> Value {
        call(
            &self.owner(),
            self.agent,
            "query.corrections",
            json!({"asset_id": self.asset}),
        )
        .unwrap()
        .0
    }

    /// Run the open gesture's commit as its task would.
    fn commit(&mut self) {
        let gesture = self.editor.core_gesture().expect("a gesture is open");
        assert_eq!(
            gesture.draft.in_flight(),
            Some(Round::Commit),
            "{}",
            self.editor.status.text
        );
        assert!(testing::run_commit(&mut self.editor));
        answer_reads(&mut self.editor);
    }

    /// Run the direct request the workspace just sent, as its task would.
    fn run_sent(&mut self) {
        let (method, params) = self
            .editor
            .correction_reads
            .last_request
            .take()
            .expect("a request was sent");
        let refreshed = tasks::command_now(
            &self.owner(),
            self.editor.client,
            self.asset.clone(),
            &method,
            params,
            None,
        );
        self.update(Message::Sync(SyncMessage::Refreshed(
            refreshed.map(Box::new),
        )));
    }

    /// Assert that what ran since `before` is exactly one entry labelled `label`.
    fn one_entry(&self, before: usize, label: &str) {
        let history = self.history();
        assert_eq!(history.len(), before + 1, "{history:?}");
        assert_eq!(history[0], label, "{history:?}");
    }

    fn press(&mut self, at: [f64; 2], modifiers: Modifiers, hit: CanvasHit) {
        self.message(CorrectionMessage::Press {
            view: ((at[0] * 1000.0) as f32, (at[1] * 1000.0) as f32),
            content: at,
            modifiers,
            hit,
        });
    }

    fn drag(&mut self, at: [f64; 2]) {
        self.message(CorrectionMessage::Drag {
            view: ((at[0] * 1000.0) as f32, (at[1] * 1000.0) as f32),
            content: at,
        });
    }

    fn release(&mut self, at: [f64; 2]) {
        self.message(CorrectionMessage::Release {
            view: ((at[0] * 1000.0) as f32, (at[1] * 1000.0) as f32),
            content: at,
        });
    }

    fn operations(&self) -> Vec<Value> {
        self.corrections()["operations"].as_array().unwrap().clone()
    }
}

fn alt() -> Modifiers {
    Modifiers {
        alt: true,
        ..Modifiers::default()
    }
}

/// The mode strip lists Corrections right after Mask with its icon and letter, the palette offers
/// it, and the band reads the declared limit.
#[test]
fn the_mode_follows_mask_and_its_band_reads_the_declared_limit() {
    let correcting = Correcting::opened();
    let modes = &correcting.editor.workspace.canvas.modes;
    let mask = modes
        .iter()
        .position(|mode| mode.id == luxforge_core::MASK_MODE)
        .unwrap();
    let entry = &modes[mask + 1];
    assert_eq!(entry.id, MODULE);
    assert_eq!(entry.icon.as_deref(), Some("heal"));
    assert_eq!(entry.shortcut.as_deref(), Some("Q"));
    assert!(entry.selected);
    let band = &correcting.editor.workspace.corrections;
    assert!(band.active);
    assert_eq!(band.footer, "0 of 100");
    assert_eq!(
        band.kinds
            .iter()
            .map(|kind| kind.title.as_str())
            .collect::<Vec<_>>(),
        ["Heal", "Clone"]
    );
    // The section stands in for nothing while the band is shown.
    assert!(
        correcting
            .editor
            .workspace
            .tools
            .all()
            .all(|section| section.module_id != MODULE)
    );
    assert!(
        correcting
            .editor
            .status
            .text
            .starts_with("Corrections \u{b7} Heal")
    );
}

/// A click adds a spot with an automatic source, a drag paints a stroke, each one entry; the new
/// correction is selected; `⇧`-click draws a straight stroke from the last end; and the stored
/// result matches what an agent's request with the same fields stores.
#[test]
fn presses_add_one_entry_each_with_their_labels_and_parity() {
    let mut c = Correcting::opened();
    let before = c.history().len();
    c.press([0.3, 0.4], Modifiers::default(), CanvasHit::Add);
    assert!(
        c.editor.correction_gesture().is_some(),
        "{}",
        c.editor.status.text
    );
    c.release([0.3, 0.4]);
    c.commit();
    c.one_entry(before, "Add Heal 1");
    let operations = c.operations();
    assert_eq!(operations.len(), 1);
    assert_eq!(
        operations[0]["points"].as_array().unwrap().len(),
        1,
        "a click is a spot"
    );
    assert_eq!(operations[0]["source-origin"], "auto");
    assert_eq!(
        c.editor.corrections.selected.as_deref(),
        Some("correction-1")
    );
    assert!(
        c.editor
            .status
            .text
            .starts_with("Added Heal 1 \u{b7} source picked"),
        "{}",
        c.editor.status.text
    );

    // A drag paints a stroke. Until repair rendering lands, a search over an earlier correction
    // is refused, which is the no-source path: the stroke stays drafted under the notice and an
    // `⌥`-click finishes it with that source.
    c.press([0.5, 0.5], Modifiers::default(), CanvasHit::Add);
    for step in 1..=10 {
        c.drag([0.5 + 0.01 * f64::from(step), 0.5]);
    }
    c.release([0.6, 0.5]);
    c.commit();
    if c.editor.core_gesture().is_some() {
        assert!(
            matches!(
                c.editor.corrections.notice,
                Some(crate::state::corrections::CorrectionNotice::NoSource { .. })
            ),
            "{}",
            c.editor.status.text
        );
        assert_eq!(
            c.history().len(),
            before + 1,
            "a refused add writes nothing"
        );
        c.press([0.55, 0.2], alt(), CanvasHit::Add);
        c.commit();
        assert!(c.editor.corrections.notice.is_none());
    }
    c.one_entry(before + 1, "Add Heal 2");
    let stroke = &c.operations()[1];
    assert!(
        stroke["points"].as_array().unwrap().len() >= 2,
        "a drag is a stroke"
    );

    // `⇧`-click draws a straight stroke from where the last one ended.
    c.press(
        [0.7, 0.6],
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        CanvasHit::Add,
    );
    c.release([0.7, 0.6]);
    c.commit();
    c.one_entry(before + 2, "Add Heal 3");
    let straight = &c.operations()[2];
    let points = straight["points"].as_array().unwrap();
    assert!(
        (points[0][0].as_f64().unwrap() - 0.6).abs() < 1e-3,
        "{points:?}"
    );
    assert!((points.last().unwrap()[0].as_f64().unwrap() - 0.7).abs() < 1e-3);

    // Parity: an agent's add with the stored fields of the spot stores the same path and radius.
    let spot = &c.operations()[0];
    let (agent, _) = call(
        &c.owner(),
        c.agent,
        "edit.add-correction",
        json!({
            "asset_id": c.asset,
            "mutation": {"expected_revision": c.editor.document.state.as_ref().unwrap().revision, "request_id": "parity-1", "actor": "agent"},
            "points": spot["points"],
            "radius": spot["radius"],
            "source-x": spot["source-x"],
            "source-y": spot["source-y"],
        }),
    )
    .unwrap();
    let mirrored = &c.operations()[3];
    assert_eq!(mirrored["points"], spot["points"]);
    assert_eq!(mirrored["radius"], spot["radius"]);
    let _ = agent;
}

/// `⌥`-click sets the source; the next press measures its offset and later presses keep it;
/// Escape clears it. A `⌘`-drag pulls a spot's source out to the release.
#[test]
fn a_set_source_persists_and_a_command_drag_pulls_one_out() {
    let mut c = Correcting::opened();
    let before = c.history().len();
    c.press([0.5, 0.2], alt(), CanvasHit::Add);
    c.release([0.5, 0.2]);
    assert_eq!(
        c.history().len(),
        before,
        "an \u{2325}-click writes nothing"
    );
    assert!(c.editor.workspace.corrections.source_set);
    let clone = c.editor.workspace.corrections.kinds[1].option.clone();
    c.message(CorrectionMessage::Kind(clone));
    c.press([0.5, 0.5], Modifiers::default(), CanvasHit::Add);
    c.release([0.5, 0.5]);
    c.commit();
    c.one_entry(before, "Add Clone 1");
    c.press([0.6, 0.6], Modifiers::default(), CanvasHit::Add);
    c.release([0.6, 0.6]);
    c.commit();
    c.one_entry(before + 1, "Add Clone 2");
    let operations = c.operations();
    for operation in &operations {
        assert_eq!(operation["source-origin"], "set");
        let dy =
            operation["source-y"].as_f64().unwrap() - operation["points"][0][1].as_f64().unwrap();
        assert!((dy + 0.3).abs() < 1e-3, "the offset is kept: {operation}");
    }
    // Escape with nothing selected clears the set source, one level at a time.
    c.key(Key::Named(Named::Escape), Keys::empty());
    assert!(c.editor.corrections.selected.is_none());
    c.key(Key::Named(Named::Escape), Keys::empty());
    assert!(!c.editor.workspace.corrections.source_set);
    // `⌘`-drag: a spot at the press with its source at the release.
    let command = Modifiers {
        command: true,
        ..Modifiers::default()
    };
    c.press([0.3, 0.7], command, CanvasHit::Add);
    c.drag([0.35, 0.72]);
    c.drag([0.4, 0.75]);
    c.release([0.4, 0.75]);
    c.commit();
    // The kind chosen for new corrections persists with the window.
    c.one_entry(before + 2, "Add Clone 3");
    let pulled = c.operations().last().unwrap().clone();
    assert_eq!(pulled["points"].as_array().unwrap().len(), 1);
    assert!(
        (pulled["source-x"].as_f64().unwrap() - 0.4).abs() < 1e-3,
        "{pulled}"
    );
    assert_eq!(pulled["source-origin"], "set");
    // Escape with nothing set leaves the mode.
    c.key(Key::Named(Named::Escape), Keys::empty());
    c.key(Key::Named(Named::Escape), Keys::empty());
    assert_eq!(
        c.editor
            .sync
            .mode
            .as_deref()
            .unwrap_or(&c.editor.session.workspace.mode),
        c.editor.session.workspace.mode
    );
}

/// Pin, source and size-handle drags each commit one entry with their label; a press that moved
/// nothing writes nothing; row sliders, kind, reorder and delete each commit one entry.
#[test]
fn handles_rows_and_keys_edit_one_entry_each() {
    let mut c = Correcting::opened();
    c.press([0.3, 0.15], alt(), CanvasHit::Add);
    c.release([0.3, 0.15]);
    for at in [[0.3, 0.3], [0.6, 0.6]] {
        c.press(at, Modifiers::default(), CanvasHit::Add);
        c.release(at);
        c.commit();
    }
    let before = c.history().len();
    // Select Heal 1 by its pin, then press its pin and let go without moving: nothing.
    c.press(
        [0.3, 0.3],
        Modifiers::default(),
        CanvasHit::Pin("correction-1".into()),
    );
    assert_eq!(
        c.editor.corrections.selected.as_deref(),
        Some("correction-1")
    );
    c.release([0.3, 0.3]);
    c.press(
        [0.3, 0.3],
        Modifiers::default(),
        CanvasHit::Handle(Handle::Pin),
    );
    c.release([0.3, 0.3]);
    assert_eq!(
        c.history().len(),
        before,
        "a release that moved nothing writes nothing"
    );
    assert!(c.editor.core_gesture().is_none());
    // Pin drag: Move.
    let source_before = c.operations()[0]["source-x"].clone();
    c.press(
        [0.3, 0.3],
        Modifiers::default(),
        CanvasHit::Handle(Handle::Pin),
    );
    c.drag([0.32, 0.31]);
    c.drag([0.34, 0.32]);
    c.release([0.34, 0.32]);
    c.commit();
    c.one_entry(before, "Move Heal 1");
    assert_eq!(
        c.operations()[0]["source-x"],
        source_before,
        "the source stays put"
    );
    // Source drag.
    let source = [
        c.operations()[0]["source-x"].as_f64().unwrap(),
        c.operations()[0]["source-y"].as_f64().unwrap(),
    ];
    c.press(
        source,
        Modifiers::default(),
        CanvasHit::Handle(Handle::Source),
    );
    c.drag([source[0] + 0.02, source[1]]);
    c.release([source[0] + 0.02, source[1]]);
    c.commit();
    c.one_entry(before + 1, "Move Heal 1 source");
    assert_eq!(c.operations()[0]["source-origin"], "set");
    // Size handle.
    let radius = c.operations()[0]["radius"].as_f64().unwrap();
    let anchor = [
        c.operations()[0]["points"][0][0].as_f64().unwrap(),
        c.operations()[0]["points"][0][1].as_f64().unwrap(),
    ];
    c.press(
        [anchor[0] + 0.01, anchor[1]],
        Modifiers::default(),
        CanvasHit::Handle(Handle::Size),
    );
    c.drag([anchor[0] + 0.03, anchor[1]]);
    c.release([anchor[0] + 0.03, anchor[1]]);
    c.commit();
    c.one_entry(before + 2, "Resize Heal 1");
    assert!(c.operations()[0]["radius"].as_f64().unwrap() > radius);
    // A row slider: Opacity, committed on release.
    c.message(CorrectionMessage::RowField {
        operation: "correction-1".into(),
        name: "opacity".into(),
        fraction: 0.7,
    });
    c.message(CorrectionMessage::RowField {
        operation: "correction-1".into(),
        name: "opacity".into(),
        fraction: 0.8,
    });
    c.message(CorrectionMessage::RowFieldReleased);
    c.commit();
    c.one_entry(before + 3, "Heal 1 \u{b7} Opacity 80");
    // Kind, a direct request.
    c.message(CorrectionMessage::RowKind {
        operation: "correction-1".into(),
        kind: "clone".into(),
    });
    c.run_sent();
    c.one_entry(before + 4, "Change Heal 1 to Clone");
    // `⌥↑` moves the selected correction up the stack.
    c.key(Key::Named(Named::ArrowUp), Keys::ALT);
    c.run_sent();
    c.one_entry(before + 5, "Reorder Clone 1");
    assert_eq!(c.operations()[1]["operation"], "correction-1");
    // `↓` selects the one below it; `⌫` deletes it.
    c.key(Key::Named(Named::ArrowDown), Keys::empty());
    assert_eq!(
        c.editor.corrections.selected.as_deref(),
        Some("correction-2")
    );
    c.key(Key::Named(Named::Backspace), Keys::empty());
    c.run_sent();
    c.one_entry(before + 6, "Delete Heal 2");
    assert!(c.editor.corrections.selected.is_none());
    assert!(
        c.editor
            .status
            .text
            .starts_with("Deleted Heal 2 \u{b7} entry"),
        "{}",
        c.editor.status.text
    );
    // `H` hides pins and outlines: no canvas targets.
    c.key(Key::Character("h".into()), Keys::empty());
    assert!(!c.editor.session.workspace.mask_handles);
    // Reset, the band's own.
    c.message(CorrectionMessage::Reset);
    c.run_sent();
    c.one_entry(before + 7, "Reset corrections");
    // Leaving with `Q`.
    c.key(Key::Character("q".into()), Keys::empty());
    let _ = call(
        &c.owner(),
        c.editor.client,
        "workspace.set",
        json!({"mode": POINTER_MODE}),
    );
    c.adopt_session();
    assert!(!c.editor.corrections_active());
}

/// An Escape while a stroke is held cancels it and writes nothing.
#[test]
fn escape_cancels_a_held_stroke_first() {
    let mut c = Correcting::opened();
    let before = c.history().len();
    c.press([0.4, 0.4], Modifiers::default(), CanvasHit::Add);
    c.drag([0.45, 0.4]);
    assert!(c.editor.correction_gesture().is_some());
    assert!(
        c.editor.workspace.corrections.drafting.is_some(),
        "the drafting row"
    );
    c.key(Key::Named(Named::Escape), Keys::empty());
    assert!(c.editor.core_gesture().is_none());
    assert_eq!(c.history().len(), before);
    assert!(
        c.editor.corrections_active(),
        "the stroke went first, the mode stays"
    );
}

/// At the declared limit a press is refused before any draft opens, with the limit's notice.
#[test]
fn the_limit_refuses_a_press_before_any_draft() {
    let mut c = Correcting::opened();
    let limit = c.corrections()["limit"].as_u64().unwrap();
    for index in 0..limit {
        let revision = c.editor.document.state.as_ref().unwrap().revision + index;
        let x = 0.05 + 0.009 * (index % 100) as f64;
        call(
            &c.owner(),
            c.agent,
            "edit.add-correction",
            json!({
                "asset_id": c.asset,
                "mutation": {"expected_revision": revision, "request_id": format!("fill-{index}"), "actor": "agent"},
                "points": [[x, 0.5]],
                "radius": 0.002,
                "source-x": x,
                "source-y": 0.2,
            }),
        )
        .unwrap_or_else(|error| panic!("add {index}: {error}"));
    }
    let refreshed = tasks::refresh(
        &c.owner(),
        c.editor.client,
        c.asset.clone(),
        tasks::Scope::Open,
        None,
    )
    .unwrap();
    c.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        refreshed,
    )))));
    let before = c.history().len();
    c.press([0.5, 0.8], Modifiers::default(), CanvasHit::Add);
    assert!(c.editor.core_gesture().is_none(), "no draft opened");
    assert!(
        matches!(
            c.editor.corrections.notice,
            Some(crate::state::corrections::CorrectionNotice::Limit { .. })
        ),
        "{:?} {}",
        c.editor.corrections.notice,
        c.editor.status.text
    );
    assert!(
        c.editor.status.text.contains("limit"),
        "{}",
        c.editor.status.text
    );
    assert_eq!(c.history().len(), before);
    assert_eq!(
        c.editor.workspace.corrections.footer,
        format!("{limit} of {limit}")
    );
}

/// A correction added at `at`, as a click, committed.
fn add_spot(c: &mut Correcting, at: [f64; 2]) {
    c.press(at, Modifiers::default(), CanvasHit::Add);
    c.release(at);
    c.commit();
}

/// A band slider's value is typed into its field: Enter applies it as the one entry a slider
/// release commits, a value outside the declared range stays open with its reason, the value as
/// shown writes nothing, and Escape closes the field with nothing sent.
#[test]
fn band_sliders_take_typed_values() {
    use crate::app::message::corrections::TypingEdit;
    use crate::state::corrections::TypingTarget;
    let mut c = Correcting::opened();
    add_spot(&mut c, [0.3, 0.4]);
    let before = c.history().len();
    let row = |name: &str| TypingTarget::Row {
        operation: "correction-1".into(),
        name: name.into(),
    };
    let typing = |c: &mut Correcting, edit: TypingEdit| c.message(CorrectionMessage::Typing(edit));
    let field = |c: &Correcting, name: &str| {
        c.editor.workspace.corrections.rows[0]
            .expanded
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .find(|field| field.name == name)
            .unwrap()
            .clone()
    };
    // Opacity opens on its shown value; a typed 55 is one `Heal 1 · Opacity 55` entry.
    typing(&mut c, TypingEdit::Begin(row("opacity")));
    assert_eq!(field(&c, "opacity").typing.as_deref(), Some("100"));
    assert!(c.editor.key_context().correction_typing);
    typing(&mut c, TypingEdit::Text("55".into()));
    typing(&mut c, TypingEdit::Submit);
    c.run_sent();
    c.one_entry(before, "Heal 1 \u{b7} Opacity 55");
    assert!(c.editor.corrections.typing.is_none());
    assert_eq!(c.operations()[0]["opacity"].as_f64(), Some(55.0));
    // Out of range or not a number: the field stays open and says why; nothing is written.
    typing(&mut c, TypingEdit::Begin(row("opacity")));
    typing(&mut c, TypingEdit::Text("150".into()));
    assert_eq!(
        field(&c, "opacity").invalid.as_deref(),
        Some("Opacity is 0 to 100")
    );
    typing(&mut c, TypingEdit::Submit);
    assert!(c.editor.corrections.typing.is_some());
    assert_eq!(c.editor.status.text, "Opacity is 0 to 100");
    typing(&mut c, TypingEdit::Text("abc".into()));
    typing(&mut c, TypingEdit::Submit);
    assert_eq!(c.editor.status.text, "Type a number");
    // Escape closes it, sending nothing.
    c.key(Key::Named(Named::Escape), Keys::empty());
    assert!(c.editor.corrections.typing.is_none());
    assert_eq!(c.history().len(), before + 1);
    // The value as shown, submitted unchanged, writes nothing.
    typing(&mut c, TypingEdit::Begin(row("opacity")));
    typing(&mut c, TypingEdit::Submit);
    assert_eq!(c.history().len(), before + 1);
    assert!(c.editor.correction_reads.last_request.is_none());
    // Size is the diameter in content pixels: 40 px is a radius of 20 / content height.
    typing(&mut c, TypingEdit::Begin(row("radius")));
    typing(&mut c, TypingEdit::Text("40 px".into()));
    typing(&mut c, TypingEdit::Submit);
    c.run_sent();
    let history = c.history();
    assert_eq!(history.len(), before + 2, "{history:?}");
    assert!(history[0].starts_with("Resize Heal 1"), "{history:?}");
    let height = f64::from(c.editor.correction_content().unwrap().1);
    let radius = c.operations()[0]["radius"].as_f64().unwrap();
    assert!((radius * 2.0 * height - 40.0).abs() < 1e-6, "{radius}");
    // The New correction group types into view state and commits nothing.
    typing(
        &mut c,
        TypingEdit::Begin(TypingTarget::New("feather".into())),
    );
    typing(&mut c, TypingEdit::Text("30".into()));
    typing(&mut c, TypingEdit::Submit);
    let feather = c
        .editor
        .workspace
        .corrections
        .new_fields
        .iter()
        .find(|field| field.name == "feather")
        .unwrap()
        .text
        .clone();
    assert_eq!(feather, "30");
    assert_eq!(c.history().len(), before + 2);
    // Selecting another row closes a row's open field.
    typing(&mut c, TypingEdit::Begin(row("feather")));
    c.message(CorrectionMessage::Select(None));
    assert!(c.editor.corrections.typing.is_none());
}

/// `H` is the session's one handles key: it flips `mask_handles` in the session the owner keeps,
/// and the canvas hides its pins from it.
#[test]
fn h_flips_the_sessions_handles_key() {
    let mut c = Correcting::opened();
    assert!(c.editor.session.workspace.mask_handles);
    c.key(Key::Character("h".into()), Keys::empty());
    assert!(!c.editor.session.workspace.mask_handles);
    assert!(c.editor.correction_canvas().unwrap().hidden);
    assert_eq!(c.editor.corrections_summary()["hidden"], json!(true));
    // The same `workspace.set` anyone can send puts them back.
    let _ = call(
        &c.owner(),
        c.editor.client,
        "workspace.set",
        json!({"mask_handles": true}),
    );
    c.adopt_session();
    assert!(!c.editor.correction_canvas().unwrap().hidden);
}

/// Selecting a row whose correction is outside a zoomed view pans the photograph through
/// `view.set` until it is inside; one already in view moves nothing.
#[test]
fn selecting_a_row_scrolls_its_correction_into_view() {
    let mut c = Correcting::opened();
    add_spot(&mut c, [0.05, 0.05]);
    add_spot(&mut c, [0.95, 0.95]);
    let zoom = json!({"zoom": {"mode": "percent", "value": 1600.0}, "pan_x": 0.0, "pan_y": 0.0});
    let _ = call(&c.owner(), c.editor.client, "view.set", zoom).unwrap();
    c.adopt_session();
    c.editor.view_state.local_pan = (0.0, 0.0);
    // Heal 1 is at the top-left: in view at pan 0.
    c.message(CorrectionMessage::Select(Some("correction-1".into())));
    assert_eq!(
        (
            c.editor.session.preview.view.pan_x,
            c.editor.session.preview.view.pan_y
        ),
        (0.0, 0.0),
        "a correction already in view moves nothing"
    );
    // Heal 2 is at the bottom-right: the view moves, in the owner's session too.
    c.message(CorrectionMessage::Select(Some("correction-2".into())));
    let view = c.editor.session.preview.view.clone();
    assert!(view.pan_x > 0.0 && view.pan_y > 0.0, "{view:?}");
    let (session, _) = call(&c.owner(), c.editor.client, "session.state", json!({})).unwrap();
    assert_eq!(
        session["preview"]["view"]["pan_x"].as_f64().unwrap() as f32,
        view.pan_x
    );
    assert_eq!(c.editor.view_state.local_pan, (view.pan_x, view.pan_y));
    // And back: selecting Heal 1 scrolls to it again.
    c.message(CorrectionMessage::Select(Some("correction-1".into())));
    assert_eq!(c.editor.session.preview.view.pan_x, 0.0);
    // At Fit nothing scrolls.
    let _ = call(
        &c.owner(),
        c.editor.client,
        "view.set",
        json!({"zoom": {"mode": "fit"}}),
    )
    .unwrap();
    c.adopt_session();
    c.message(CorrectionMessage::Select(Some("correction-2".into())));
    assert_eq!(c.editor.session.preview.view.pan_x, 0.0);
}

/// The overlay's first draw after a message is logged once, after the next message, at the draw's
/// own instant: nothing is logged without a draw, and a second draw before the next message adds
/// no second event.
#[test]
fn the_overlay_draw_is_logged_once_per_message_at_most() {
    use crate::view::corrections_canvas::stamp_overlay_drawn;
    let mut c = Correcting::opened();
    let log = testing::attach_log(&mut c.editor);
    // The first message arms the stamp; nothing was drawn before it.
    c.message(CorrectionMessage::HoverRow(None));
    stamp_overlay_drawn(3, 0);
    stamp_overlay_drawn(5, 9);
    // The next message logs the first draw only, and arms the next.
    c.message(CorrectionMessage::HoverRow(None));
    // No draw since: nothing more to log.
    c.message(CorrectionMessage::HoverRow(None));
    stamp_overlay_drawn(1, 4);
    c.message(CorrectionMessage::HoverRow(None));
    let records = testing::logged(&mut c.editor, &log);
    let drawn = testing::events(&records, "correction_overlay_drawn");
    assert_eq!(drawn.len(), 2, "{drawn:?}");
    assert_eq!(
        (&drawn[0]["pins"], &drawn[0]["live_points"]),
        (&json!(3), &json!(0))
    );
    assert_eq!(
        (&drawn[1]["pins"], &drawn[1]["live_points"]),
        (&json!(1), &json!(4))
    );
    assert!(drawn[0]["drawn_ms"].as_f64().unwrap() >= 0.0);
}

/// Usage statistics: a committing gesture records its one `tool.commit` `corrections` at the
/// core's commit, with the photograph's image; the mode, selection, a set source and `H` are view
/// state and record nothing, and nothing a gesture records carries a coordinate or a setting.
#[test]
fn usage_corrections_gestures_record_their_commits_at_the_core_and_view_state_nothing() {
    use luxforge_core::usage::{EventKind, ImageKey, SessionClock, SessionStart, Tool, Trigger};
    let mut c = Correcting::opened();
    let key = ImageKey::from_bytes([0x24; 32]);
    let usage = c.owner().usage().clone();
    usage.start(
        Trigger::OptIn,
        SessionStart::parse("2026-10-09T14:03:12Z").unwrap(),
        SessionClock::manual().0,
        key.clone(),
    );
    let events = || -> Vec<EventKind> {
        let Some(session) = usage.session() else {
            return Vec::new();
        };
        let Some(window) = session.detach() else {
            return Vec::new();
        };
        window
            .events()
            .iter()
            .map(|event| event.kind)
            .filter(|kind| !matches!(kind, EventKind::SessionStart { .. }))
            .collect()
    };
    let fingerprint = &c.editor.document.state.as_ref().unwrap().asset.fingerprint;
    let image = key.image_id_of_hex(fingerprint).unwrap();
    let commit = EventKind::ToolCommit {
        tool: Tool::Corrections,
        image,
    };

    // A set source, `H` twice and the mode left and entered again: view state.
    c.press([0.3, 0.15], alt(), CanvasHit::Add);
    c.release([0.3, 0.15]);
    c.key(Key::Character("h".into()), Keys::empty());
    c.key(Key::Character("h".into()), Keys::empty());
    c.key(Key::Character("q".into()), Keys::empty());
    c.enter();
    assert_eq!(events(), []);

    // A spot from the set source, then a painted stroke: one commit each.
    add_spot(&mut c, [0.3, 0.3]);
    assert_eq!(events(), [commit]);
    c.press([0.6, 0.6], Modifiers::default(), CanvasHit::Add);
    for step in 1..=10 {
        c.drag([0.6 + 0.01 * f64::from(step), 0.6]);
    }
    c.release([0.7, 0.6]);
    c.commit();
    assert_eq!(events(), [commit], "a stroke's moves record nothing");

    // Selecting a row records nothing; `⌫` deletes it with one commit.
    c.message(CorrectionMessage::Select(Some("correction-1".into())));
    assert_eq!(events(), []);
    c.key(Key::Named(Named::Backspace), Keys::empty());
    c.run_sent();
    assert_eq!(events(), [commit]);
    // The band's reset: one commit.
    c.message(CorrectionMessage::Reset);
    c.run_sent();
    assert_eq!(events(), [commit]);
}
