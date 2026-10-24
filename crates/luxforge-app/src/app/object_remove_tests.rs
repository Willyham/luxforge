//! Corrections › Object (`docs/design/ai-tools-workspace.md`, "Object") against a real owner
//! serving the developer `luxforge.ai` module, its fake segmenter and generator and the
//! deterministic fake runtime, on the four-quadrant fixture, whose quadrants the fake decoder finds
//! as objects.
//!
//! Every control goes through the update function as the band, the canvas and the keyboard send
//! it; the owner work it starts is run here as the plain calls its tasks wrap, and each answer is
//! handed back as the task's message ([`Filling`] is Remove's harness).
use super::{
    message::{
        Message,
        object_pick::{ObjectPickMessage, ObjectPointer},
        remove::{RemoveMessage, RemoveRequest},
    },
    object_pick::{pick, queue_analysis, selection_limit},
    remove_tests::{Filling, same_request},
    tasks, testing,
};
use crate::{
    brush::Modifiers,
    object_pick::{self as shared, Decision, Found, FoundCandidate, PickUse, SelectFor},
    state::capabilities::Operation,
};
use iced::keyboard::{Key, Modifiers as Keys, key::Named};
use luxforge_test_macros::slow;
use serde_json::{Value, json};
use std::{
    ops::{Deref, DerefMut},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

const AI: &str = "luxforge.ai";
const ENCODER: &str = "fake-segmenter-encoder";
const DECODER: &str = "fake-segmenter-decoder";
const GENERATOR: &str = "fake-generator";
/// The top quadrants and the bottom-left one, each one object to the fake decoder.
const LEFT: [f64; 2] = [0.25, 0.25];
const RIGHT: [f64; 2] = [0.75, 0.25];
const LOWER: [f64; 2] = [0.25, 0.75];
/// The centres of [`patches`]' red and green patches.
const RED: [f64; 2] = [0.225, 0.3];
const GREEN: [f64; 2] = [0.55, 0.3];
const SHIFT: Modifiers = Modifiers {
    shift: true,
    command: false,
    alt: false,
};
const ALT: Modifiers = Modifiers {
    shift: false,
    command: false,
    alt: true,
};

/// Remove's harness with `ai.analysis` on, so the band offers Object, and the photograph it wrote
/// when it made one.
struct Removing(Filling, Option<PathBuf>);

impl Drop for Removing {
    fn drop(&mut self) {
        if let Some(photograph) = &self.1 {
            let _ = std::fs::remove_file(photograph);
        }
    }
}

impl Deref for Removing {
    type Target = Filling;
    fn deref(&self) -> &Filling {
        &self.0
    }
}

impl DerefMut for Removing {
    fn deref_mut(&mut self) -> &mut Filling {
        &mut self.0
    }
}

impl Removing {
    /// The photograph open in Corrections with the experiment on, Object chosen and its setup
    /// not installed yet.
    fn opened() -> Self {
        Self::opened_on(Filling::corrections(), None)
    }

    fn opened_on(filling: Filling, photograph: Option<PathBuf>) -> Self {
        let mut removing = Self(filling, photograph);
        removing.api("flags.set", json!({"flag": "ai.analysis", "value": true}));
        testing::read_flags(&mut removing.editor);
        removing.remove(RemoveMessage::ChooseObject);
        removing.answer_capabilities();
        assert!(
            removing.editor.object_active(),
            "{}",
            removing.editor.status.text
        );
        removing
    }

    /// Object chosen with its setup installed: the session armed and its entry answered.
    fn armed() -> Self {
        let mut removing = Self::opened();
        removing.install_object();
        removing.answer_entry();
        removing
    }

    /// As [`Self::armed`], on [`patches`]: two small objects on grey.
    fn armed_on_patches() -> Self {
        let photograph = patches();
        let mut removing = Self::opened_on(Filling::corrections_on(&photograph), Some(photograph));
        removing.install_object();
        removing.answer_entry();
        removing
    }

    /// Grant and install the `object` setup's three files through the API and read the status
    /// again, as an install's end does: the session arms.
    fn install_object(&mut self) {
        for resource in [ENCODER, DECODER, GENERATOR] {
            self.install(resource);
        }
        let _ = self.editor.capability_op(AI, Operation::Refresh);
        self.answer_capabilities();
        self.refresh_tiers();
        let session = self.editor.object_pick.as_ref().expect("the session armed");
        assert_eq!(session.purpose, PickUse::Object);
    }

    fn object(&mut self, message: ObjectPickMessage) {
        self.update(Message::ObjectPick(message));
    }

    /// Answer the session's entry: each model load, the analysis and the content map.
    fn answer_entry(&mut self) {
        let session = self.editor.object_pick.as_ref().expect("picking");
        for resource in session.loads.clone() {
            let result = tasks::call_detailed(
                &self.owner(),
                self.editor.client,
                shared::RUNTIME_LOAD,
                json!({"module_id": AI, "resource_id": resource}),
            )
            .map_err(|error| error.message);
            self.object(ObjectPickMessage::Loaded { resource, result });
        }
        let session = self.editor.object_pick.as_ref().unwrap();
        if let Some((asset, revision)) = session.analysing.clone() {
            let job = queue_analysis(&self.owner(), self.editor.client, &asset).unwrap();
            assert_eq!(self.finished(&job)["status"], "ready");
            self.object(ObjectPickMessage::Analysed {
                asset,
                revision,
                result: Ok(()),
            });
        }
        let Some(gesture) = self
            .editor
            .object_pick
            .as_ref()
            .and_then(|session| session.request)
        else {
            return;
        };
        let (transform, _) = tasks::call(
            &self.owner(),
            self.editor.client,
            "render.transform",
            json!({"asset_id": self.asset}),
        )
        .unwrap();
        self.object(ObjectPickMessage::Transform(
            gesture,
            Ok(serde_json::from_value(transform).unwrap()),
        ));
    }

    /// Hover at `point` and answer the pick it sends, with the held preview Object keeps.
    fn hover(&mut self, point: [f64; 2]) {
        self.object(ObjectPickMessage::Hover(Some(point)));
        let session = self.editor.object_pick.as_ref().unwrap();
        assert_eq!(session.asking, Some(point));
        let params = session.last_request.clone().unwrap();
        let revision = self.editor.document.state.as_ref().unwrap().revision;
        let result = pick(&self.owner(), self.editor.client, params, true);
        self.object(ObjectPickMessage::Hovered {
            asset: self.asset.clone(),
            revision,
            point,
            result,
        });
        assert!(
            self.editor.object_overlay().is_some(),
            "{point:?} is outlined"
        );
    }

    fn press(&mut self, at: [f64; 2], modifiers: Modifiers) {
        self.object(ObjectPickMessage::Pointer(ObjectPointer::Press {
            view: view(at),
            at,
            modifiers,
        }));
    }

    fn click(&mut self, at: [f64; 2], modifiers: Modifiers) {
        self.press(at, modifiers);
        self.object(ObjectPickMessage::Pointer(ObjectPointer::Release {
            view: view(at),
            at,
        }));
    }

    /// A pointer stroke through `points`: a loose drag, or Paint shadow's stroke once armed.
    fn stroke(&mut self, points: &[[f64; 2]]) {
        self.press(points[0], Modifiers::default());
        for at in &points[1..] {
            self.object(ObjectPickMessage::Pointer(ObjectPointer::Move {
                view: view(*at),
                at: *at,
            }));
        }
        let last = *points.last().unwrap();
        self.object(ObjectPickMessage::Pointer(ObjectPointer::Release {
            view: view(last),
            at: last,
        }));
    }

    /// A loose drag the picker answers with `decision` over `candidates` (bounds by position), as
    /// its answer decodes: the stroke sent.
    fn found(&mut self, decision: Decision, candidates: &[(u32, [f64; 4])]) -> Vec<[f64; 2]> {
        self.stroke(&[[0.2, 0.5], [0.5, 0.5], [0.8, 0.5]]);
        let session = self.editor.object_pick.as_ref().unwrap();
        let (sent, _) = session.finding.clone().expect("the stroke is being found");
        assert_eq!(session.last_request.as_ref().unwrap()["previews"], "all");
        let found = Found {
            decision,
            chosen: candidates.iter().map(|(position, _)| *position).collect(),
            candidates: candidates
                .iter()
                .map(|&(position, bounds)| FoundCandidate {
                    position,
                    bounds,
                    image: None,
                    tinted: None,
                })
                .collect(),
        };
        let revision = self.editor.document.state.as_ref().unwrap().revision;
        self.object(ObjectPickMessage::Found {
            asset: self.asset.clone(),
            revision,
            stroke: sent.clone(),
            result: Ok(Box::new(found)),
        });
        sent
    }

    /// Run the selection the desktop is running, as its task would, and hand back its answer.
    fn select(&mut self) -> Value {
        let (purpose, params) = self
            .editor
            .object_selects
            .running
            .clone()
            .expect("a selection is running");
        assert!(matches!(purpose, SelectFor::Object { .. }), "{purpose:?}");
        let queued = loop {
            match tasks::call_detailed(
                &self.owner(),
                self.editor.client,
                shared::SELECT,
                params.clone(),
            ) {
                Ok(queued) => break queued,
                Err(error) if error.code == "preparation-required" => {
                    let job = error.job_id.unwrap();
                    assert_eq!(self.finished(&job)["status"], "ready");
                }
                Err(error) => panic!("task.select-object: {}", error.message),
            }
        };
        let record = self.finished(queued["job_id"].as_str().unwrap());
        assert_eq!(record["status"], "ready", "{record}");
        let selection = record["result"]["result"].clone();
        self.object(ObjectPickMessage::Selected {
            purpose,
            result: Ok(selection.clone()),
        });
        selection
    }

    /// Hover at `at`, click it with `modifiers` and run its selection: its artifact.
    fn pick_at(&mut self, at: [f64; 2], modifiers: Modifiers) -> String {
        self.hover(at);
        self.click(at, modifiers);
        let selection = self.select();
        selection["artifact"].as_str().unwrap().to_owned()
    }

    /// The selected objects' prompts and artifacts, in pick order.
    fn objects(&self) -> Vec<(Value, Option<String>)> {
        self.editor
            .remove
            .selection
            .as_ref()
            .map(|selection| {
                selection
                    .objects
                    .iter()
                    .map(|object| {
                        (
                            Value::Object(object.prompt.request_fields()),
                            object.artifact.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn escape(&mut self) {
        self.key(Key::Named(Named::Escape), Keys::empty());
    }
}

/// A 480 × 320 JPEG of two small saturated patches, red then green, on mid grey: two objects to
/// the fake decoder whose bounds together stay well under half the frame.
fn patches() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!(
        "luxforge-object-patches-{}-{}.jpg",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let image = image::RgbImage::from_fn(480, 320, |x, y| {
        let inside = |x0: u32| (x0..x0 + 96).contains(&x) && (48..144).contains(&y);
        if inside(60) {
            image::Rgb([220, 40, 40])
        } else if inside(216) {
            image::Rgb([40, 180, 60])
        } else {
            image::Rgb([128, 128, 128])
        }
    });
    image.save(&path).unwrap();
    path
}

fn view(at: [f64; 2]) -> (f32, f32) {
    ((at[0] * 1000.0) as f32, (at[1] * 1000.0) as f32)
}

fn point(at: [f64; 2]) -> Value {
    json!({"point": [at]})
}

/// Object joins the kind control only while `ai.analysis` is on; without its setup the segment is
/// marked for setup, the group shows the setup line and a press on the photograph says what to do.
/// Once installed, the session arms on its own: both of the object finder's models load and the
/// photograph is analysed, saying Finding objects… in the group and over the canvas until the
/// analysis answers.
#[test]
#[slow]
fn object_is_offered_with_the_experiment_and_finds_objects_once_set_up() {
    let filling = Filling::opened();
    assert!(!filling.model().object_offered, "the experiment is off");
    drop(filling);

    let mut removing = Removing::opened();
    let model = removing.model();
    assert!(model.object_offered && model.object_needs_setup);
    let object = model.object.clone().expect("the Object group");
    let setup = object.setup.expect("the setup line");
    assert_eq!(model.hint, setup.reason);
    assert!(removing.editor.object_pick.is_none(), "nothing picks yet");
    removing.content_press([0.3, 0.3]);
    assert_eq!(removing.editor.status.text, setup.reason);

    removing.install_object();
    let session = removing.editor.object_pick.as_ref().unwrap();
    assert_eq!(session.loads, vec![ENCODER.to_owned(), DECODER.to_owned()]);
    assert_eq!(session.analyses, 1);
    assert!(session.finding_objects());
    let model = removing.model();
    assert!(!model.object_needs_setup);
    let object = model.object.as_ref().unwrap();
    assert!(object.setup.is_none() && object.finding);
    assert!(
        removing
            .editor
            .workspace
            .canvas
            .notices
            .iter()
            .any(|notice| notice.title == shared::FINDING_OBJECTS)
    );
    // The status bar says Finding objects… until the finder is ready, then what Object does.
    removing.editor.status.text = removing.model().hint.clone();
    assert!(
        removing
            .editor
            .status
            .text
            .starts_with(shared::FINDING_OBJECTS)
    );
    removing.answer_entry();
    assert!(
        !removing
            .editor
            .object_pick
            .as_ref()
            .unwrap()
            .finding_objects()
    );
    assert!(!removing.model().object.as_ref().unwrap().finding);
    assert_eq!(
        removing.editor.status.text,
        crate::state::remove::OBJECT_IDLE
    );
}

/// A click on the outlined object sends the selection an API client sends and replaces the
/// selection; `⇧`-click adds a second, numbered in pick order; `⌥`-click takes the first away by a
/// hit test of its held preview, with nothing sent; a click with nothing outlined says so.
#[test]
#[slow]
fn object_click_selects_shift_adds_and_alt_takes_away() {
    let mut removing = Removing::armed();
    removing.hover(LEFT);
    removing.click(LEFT, Modifiers::default());
    let (_, sent) = removing.editor.object_selects.running.clone().unwrap();
    assert_eq!(sent["point"], json!([LEFT]));
    assert_eq!(sent["asset_id"], json!(removing.asset));
    removing.select();
    let left = removing.objects();
    assert_eq!(left.len(), 1);
    assert!(left[0].1.is_some(), "the artifact is held");
    let rows = &removing.model().object.as_ref().unwrap().rows;
    assert_eq!(
        (rows.len(), rows[0].number, rows[0].selecting),
        (1, 1, false)
    );

    // A click on another object replaces it.
    removing.pick_at(RIGHT, Modifiers::default());
    assert_eq!(
        removing
            .objects()
            .iter()
            .map(|(prompt, _)| prompt.clone())
            .collect::<Vec<_>>(),
        vec![point(RIGHT)]
    );
    // ⇧ adds; the same object again is refused.
    removing.pick_at(LEFT, SHIFT);
    assert_eq!(
        removing
            .objects()
            .iter()
            .map(|(prompt, _)| prompt.clone())
            .collect::<Vec<_>>(),
        vec![point(RIGHT), point(LEFT)]
    );
    assert!(
        removing
            .editor
            .status
            .text
            .starts_with("2 objects selected"),
        "{}",
        removing.editor.status.text
    );
    removing.click(LEFT, SHIFT);
    assert_eq!(
        removing.editor.status.text,
        "That object is already selected"
    );
    assert!(removing.editor.object_selects.idle());

    // ⌥-click takes the object under the pointer away, with nothing sent.
    removing.editor.object_pick.as_mut().unwrap().last_request = None;
    removing.click(RIGHT, ALT);
    assert_eq!(
        removing
            .objects()
            .iter()
            .map(|(prompt, _)| prompt.clone())
            .collect::<Vec<_>>(),
        vec![point(LEFT)]
    );
    assert!(removing.editor.object_selects.idle());
    assert!(
        removing
            .editor
            .object_pick
            .as_ref()
            .unwrap()
            .last_request
            .is_none()
    );
    assert_eq!(removing.model().object.as_ref().unwrap().rows[0].number, 1);

    // Nothing outlined: nothing selected, and the bar says so.
    removing.object(ObjectPickMessage::Hover(None));
    removing.click(LOWER, Modifiers::default());
    assert_eq!(removing.editor.status.text, shared::NOTHING_TO_SELECT);
    assert_eq!(removing.objects().len(), 1);
}

/// A loose drag asks the picker about its stroke with every preview; Several puts up numbered
/// pins, a click on a pin toggles it and Return selects each chosen one by its position, adding
/// nothing it was not asked to; Escape puts pins down first.
#[test]
#[slow]
fn object_a_loose_drag_with_several_objects_puts_up_pins_to_choose() {
    let mut removing = Removing::armed();
    let stroke = removing.found(
        Decision::Several,
        &[(0, [0.0, 0.0, 0.5, 0.5]), (2, [0.5, 0.0, 1.0, 0.5])],
    );
    let notice = removing.editor.workspace.canvas.notices.clone();
    assert!(
        notice
            .iter()
            .any(|notice| notice.title == "2 objects found"),
        "{notice:?}"
    );
    assert!(removing.model().hint.starts_with("2 objects found"));
    removing.object(ObjectPickMessage::Pin(0));
    removing.object(ObjectPickMessage::Pin(1));
    removing.object(ObjectPickMessage::Pin(0));
    removing.key(Key::Named(Named::Enter), Keys::empty());
    assert!(removing.editor.object_pick.as_ref().unwrap().pins.is_none());
    let (_, params) = removing.editor.object_selects.running.clone().unwrap();
    assert_eq!(params["candidate"], json!(2));
    assert_eq!(params["stroke"], json!(stroke));
    assert_eq!(removing.objects().len(), 1);
    let rows = &removing.model().object.as_ref().unwrap().rows;
    assert!(rows[0].selecting, "selecting\u{2026} until it answers");

    // Pins again, then Escape: the pins go and the selection stays.
    removing.found(
        Decision::Several,
        &[(0, [0.0, 0.0, 0.5, 0.5]), (1, [0.0, 0.5, 0.5, 1.0])],
    );
    removing.escape();
    assert!(removing.editor.object_pick.as_ref().unwrap().pins.is_none());
    assert_eq!(removing.objects().len(), 1);
}

/// A loose drag that finds nothing keeps its footprint under Nothing found here, with Remove the
/// painted area; Return switches the band to Remove with the stroke as its painted selection.
#[test]
#[slow]
fn object_a_loose_drag_that_finds_nothing_hands_the_stroke_to_remove() {
    let mut removing = Removing::armed();
    let stroke = removing.found(Decision::Nothing, &[]);
    let notice = removing
        .editor
        .workspace
        .canvas
        .notices
        .iter()
        .find(|notice| notice.title == shared::NOTHING_FOUND)
        .cloned()
        .expect("Nothing found here");
    assert_eq!(notice.actions[0].0, "Remove the painted area \u{21b5}");
    assert!(removing.model().hint.starts_with("Nothing found here"));
    removing.key(Key::Named(Named::Enter), Keys::empty());
    assert!(!removing.editor.remove.object, "the band is Remove now");
    assert!(
        removing.editor.object_pick.is_none(),
        "Object's session ended"
    );
    let selection = removing.editor.remove.selection.as_ref().unwrap();
    assert_eq!(selection.points, stroke);
    assert!(selection.objects.is_empty());
    assert!(removing.model().selection);
    assert!(removing.model().generate_reason.is_none());
}

/// At most eight objects: a ninth is refused in the status bar, with nothing sent.
#[test]
#[slow]
fn object_a_ninth_object_is_refused() {
    let mut removing = Removing::armed();
    let candidates: Vec<(u32, [f64; 4])> = (0..8)
        .map(|position| {
            let x = f64::from(position) / 8.0;
            (position, [x, 0.0, x + 0.125, 0.5])
        })
        .collect();
    removing.found(Decision::Several, &candidates);
    for pin in 0..8 {
        removing.object(ObjectPickMessage::Pin(pin));
    }
    removing.key(Key::Named(Named::Enter), Keys::empty());
    assert_eq!(removing.objects().len(), 8);
    removing.hover(LOWER);
    let queued = removing.editor.object_selects.queued.len();
    removing.click(LOWER, SHIFT);
    assert_eq!(removing.editor.status.text, selection_limit());
    assert_eq!(removing.objects().len(), 8);
    assert_eq!(removing.editor.object_selects.queued.len(), queued);
}

/// Switching between Remove and Object clears the selection, while the candidates already
/// generated stay in the strip; turning the experiment off while Object is chosen returns the band
/// to Remove.
#[test]
#[slow]
fn object_switching_kind_clears_the_selection_and_the_flag_returns_to_remove() {
    let mut removing = Removing::armed();
    removing.pick_at(LEFT, Modifiers::default());
    removing.generate(false);
    assert_eq!(removing.model().strip.len(), 1);
    removing.remove(RemoveMessage::Choose);
    assert!(!removing.editor.remove.object);
    assert!(!removing.model().selection, "Remove has no stroke yet");
    assert_eq!(removing.model().strip.len(), 1, "the candidates stay");
    removing.remove(RemoveMessage::ChooseObject);
    assert!(
        !removing.model().selection,
        "the objects went with the switch"
    );
    assert_eq!(removing.model().strip.len(), 1);
    assert!(removing.editor.object_pick.is_some(), "Object arms again");

    removing.api("flags.set", json!({"flag": "ai.analysis", "value": false}));
    testing::read_flags(&mut removing.editor);
    assert!(!removing.editor.remove.object);
    assert!(removing.editor.remove_active(), "the band is Remove");
    assert!(removing.editor.object_pick.is_none());
    assert!(!removing.model().object_offered);
}

/// Choosing a kind says its own line at once rather than the one it left: Object's after Remove's,
/// Remove's after Object's, and Heal's after either.
#[test]
#[slow]
fn object_choosing_a_kind_says_its_own_line() {
    let mut removing = Removing::armed();
    assert_eq!(
        removing.editor.status.text,
        crate::state::remove::OBJECT_IDLE
    );
    removing.remove(RemoveMessage::Choose);
    let remove = removing.model().hint.clone();
    assert_eq!(removing.editor.status.text, remove);
    assert!(!remove.contains("Object"), "{remove}");
    removing.remove(RemoveMessage::ChooseObject);
    removing.answer_entry();
    assert_eq!(
        removing.editor.status.text,
        crate::state::remove::OBJECT_IDLE
    );
    removing.update(Message::Corrections(
        super::message::corrections::CorrectionMessage::Kind("heal".into()),
    ));
    assert!(
        removing
            .editor
            .status
            .text
            .starts_with("Corrections \u{b7} Heal"),
        "{}",
        removing.editor.status.text
    );
}

/// The outline of an object hovered before its removal candidate appeared is not drawn over the
/// candidate while the pointer stays; the next hover, once the pointer moves, is drawn again.
#[test]
#[slow]
fn object_the_hover_outline_gives_way_to_a_shown_candidate() {
    let mut removing = Removing::armed();
    removing.pick_at(LEFT, Modifiers::default());
    assert!(removing.editor.object_overlay().is_some());
    removing.generate(false);
    assert!(
        removing.editor.shown_candidate().is_some(),
        "the candidate shows"
    );
    assert!(
        removing.editor.object_overlay().is_none(),
        "the outline answered before it is not drawn over it"
    );
    assert_eq!(
        removing.editor.object_pick_record()["overlay_drawn"],
        json!(false)
    );
    removing.hover(RIGHT);
    assert!(removing.editor.shown_candidate().is_some());
    assert_eq!(
        removing.editor.object_pick_record()["overlay_drawn"],
        json!(true)
    );
}

/// Escape in Object, one level at a time: a running generation is cancelled, then pins (or
/// Nothing found here) are put down, then the selection is cleared, and then nothing holds the
/// mode.
#[test]
#[slow]
fn object_escape_cancels_then_puts_pins_down_then_clears_then_leaves() {
    let mut removing = Removing::armed();
    removing
        .fake
        .controls()
        .set_step_delay(Duration::from_millis(200));
    removing.pick_at(LEFT, Modifiers::default());
    removing.remove(RemoveMessage::Generate);
    let job = removing.start();
    assert!(removing.editor.generation_lock().is_some());
    // A pick while it runs is refused by name.
    removing.click(RIGHT, Modifiers::default());
    assert_eq!(removing.editor.status.text, crate::state::GENERATING);
    removing.escape();
    removing.run_cancel();
    assert!(removing.editor.generation_lock().is_none());
    assert_eq!(removing.finished(&job)["status"], "cancelled");
    assert_eq!(
        removing.objects().len(),
        1,
        "the first Escape keeps the selection"
    );

    removing.found(
        Decision::Several,
        &[(0, [0.0, 0.0, 0.5, 0.5]), (1, [0.5, 0.0, 1.0, 0.5])],
    );
    removing.escape();
    assert!(removing.editor.object_pick.as_ref().unwrap().pins.is_none());
    assert_eq!(removing.objects().len(), 1);

    removing.found(Decision::Nothing, &[]);
    removing.escape();
    assert!(
        removing
            .editor
            .object_pick
            .as_ref()
            .unwrap()
            .nothing
            .is_none()
    );
    assert!(removing.editor.remove.object, "Escape does not hand off");
    assert_eq!(removing.objects().len(), 1);

    assert!(!removing.editor.correction_escape_leaves());
    removing.escape();
    assert!(removing.objects().is_empty());
    assert_eq!(removing.editor.status.text, "Selection cleared");
    assert!(removing.editor.corrections_active());

    // Nothing is left to put down: the next Escape leaves the mode, as the band's does.
    assert!(removing.editor.correction_escape_leaves());
}

/// Two picked objects and a painted shadow: Generate sends `task.fill-generative` naming both
/// artifacts with the shadow as the effect region, exactly as Copy as JSON copies it, and Accept
/// commits one history entry.
#[test]
#[slow]
fn object_two_objects_and_a_shadow_commit_one_entry() {
    let mut removing = Removing::armed_on_patches();
    let first = removing.pick_at(RED, Modifiers::default());
    let second = removing.pick_at(GREEN, SHIFT);
    // Paint shadow arms the brush for one stroke.
    removing.remove(RemoveMessage::Shadow);
    assert!(removing.model().object.as_ref().unwrap().shadow_armed);
    let shadow = [[0.15, 0.55], [0.3, 0.55], [0.45, 0.55]];
    removing.stroke(&shadow);
    assert!(!removing.model().object.as_ref().unwrap().shadow_armed);
    let effect = removing
        .editor
        .remove
        .selection
        .as_ref()
        .unwrap()
        .effect
        .clone()
        .expect("the shadow is the effect region");
    let start = effect.points[0];
    assert!(
        (start[0] - shadow[0][0]).abs() < 1e-3 && (start[1] - shadow[0][1]).abs() < 1e-3,
        "{start:?}"
    );
    assert!(
        removing.model().generate_reason.is_none(),
        "{:?}",
        removing.model()
    );

    let copied = removing.copy(RemoveRequest::Generate);
    let picks = removing.copy(RemoveRequest::Select);
    assert_eq!(picks.as_array().map(Vec::len), Some(2));
    removing.remove(RemoveMessage::Generate);
    let sent = removing.editor.remove_reads.last_request.clone().unwrap();
    same_request(copied, &sent);
    assert_eq!(sent.0, "task.fill-generative");
    assert_eq!(sent.1["asset_id"], json!(removing.asset));
    assert_eq!(sent.1["object-artifacts"], json!([first, second]));
    assert_eq!(sent.1["effect-points"], json!(effect.points));
    assert_eq!(sent.1["effect-radius"], json!(effect.radius));
    assert!(sent.1.get("points").is_none());

    let job = removing.start();
    let record = removing.finished(&job);
    assert_eq!(record["status"], "ready", "{record}");
    removing.poll(&job);
    removing.read_previews();
    removing.show();
    assert!(removing.shown().is_some());
    let before = removing.history().len();
    removing.remove(RemoveMessage::Accept);
    removing.run_accept();
    let history = removing.history();
    assert_eq!(history.len(), before + 1, "{history:?}");
    assert_eq!(history[0], "Add Generative fill 1");
    assert!(removing.objects().is_empty(), "the selection is spent");
}
