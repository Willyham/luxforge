//! The `presets` smoke scenario: the Presets section on the real editor. It imports a Lightroom XMP
//! preset and a Luxforge preset document through the section's own import task, applies each from
//! its row, undoes, creates a native preset from the Basic Tone group, applies it to the Original,
//! lists the library through a host `api` step and deletes the native preset through its row menu.
//!
//! Every frame is checked for the correlated revision, current entry and history label, the stored
//! layer payloads and the section's rows. The layers a preset leaves are computed here, stepwise:
//! the fixture's settings come from the core importer run on the same file, each named field
//! overwrites the layer the stack already held, and the stored form omits every field at its
//! declared default, which is the modules' own canonical payload. The apply frames are also checked
//! against the photograph: the frame before and after each apply differ in the direction the
//! preset's own settings move them, and an undo returns the pixels of the stack it returns to.
//!
//! The fixture is `fixtures/s0/orientation-1.jpg`, four flat quadrant colours, so one patch per
//! quadrant reads a colour a preset moves without the white labels, the centre line or the dash
//! band in it.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, plan::only},
    *,
};
use luxforge_evidence::{self as script, PresetCreateStep, PresetPick};
use std::collections::BTreeMap;

pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
const XMP: &str = "fixtures/presets/develop.xmp";
const DOCUMENT: &str = "fixtures/presets/soft-film.lfpreset";
const PRESETS_MODULE: &str = "luxforge.presets";
const BASIC_MODULE: &str = "luxforge.basic";
/// The native preset the scenario creates, in the default group the create form offers.
const NATIVE: &str = "Tone only";
const USER_GROUP: &str = "User presets";
const TONE_GROUP: &str = "Basic \u{00b7} Tone";
/// The Basic fields the Tone group captures, as its controls declare them.
const TONE_FIELDS: [&str; 6] = [
    "exposure",
    "contrast",
    "highlights",
    "shadows",
    "whites",
    "blacks",
];

/// How far the mean luminance of the four quadrant patches must rise before this scenario calls a
/// frame brighter. +0.35 EV lifts every quadrant by well over this.
const BRIGHTER: f64 = 5.0;
/// How close two frames of the same stack must read. Both are the same render, so this is
/// readback noise only.
const SAME: f64 = 1.5;
/// The smallest mean per-channel change across the four patches that counts as a changed picture.
const CHANGED: f64 = 2.0;
/// Patch centres, as fractions of the photograph: one per quadrant, between its label and the dash
/// band, and clear of the centre line.
const PATCHES: [(&str, f64, f64); 4] = [
    ("red", 0.25, 0.30),
    ("green", 0.75, 0.30),
    ("blue", 0.25, 0.80),
    ("gold", 0.75, 0.80),
];
const PATCH_HALF: i64 = 6;

/// Every frame, in order: the open, then one per step. The plan says what each step commits and
/// records; `verify` below checks the section's rows, the stored layers against a stepwise merge of
/// the fixtures' own settings, and the photograph.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        // 1-2: Basic out of the way, the Presets section open. Nothing is committed.
        Step::new(
            "basic-collapsed",
            script::Step::section(BASIC_MODULE, false),
        )
        .commits(0)
        .collapsed(BASIC_MODULE),
        Step::new(
            "presets-expanded",
            script::Step::section(PRESETS_MODULE, true),
        )
        .commits(0)
        .expanded(PRESETS_MODULE),
        // 3-4: two imports through the section's own task; an import commits no edit, and says
        // what it imported.
        Step::new("xmp", script::Step::preset_import(XMP))
            .commits(0)
            .status_starts("Imported \u{201c}Soft Film\u{201d}: "),
        Step::new("document", script::Step::preset_import(DOCUMENT)).commits(0),
        // 5: the document's preset, whose name differs from the XMP's only in case: one entry.
        Step::new(
            "soft-film",
            script::Step::Preset(PresetPick {
                name: "Soft film".into(),
                group: Some("Synthetic".into()),
            }),
        )
        .commits(1)
        .label("Preset: Soft film"),
        // 6: the XMP's preset over it, by its exact name alone: one more entry.
        Step::new("soft-film-xmp", script::Step::preset("Soft Film"))
            .commits(1)
            .label("Preset: Soft Film"),
        // 7: undo returns to the document's preset.
        Step::new("undo", script::Step::api("history.undo"))
            .commits(1)
            .label("Preset: Soft film"),
        // 8: the create form filled, Basic Tone alone, and left open for its frame.
        Step::new(
            "form",
            PresetCreateStep {
                name: NATIVE.into(),
                group: None,
                groups: vec![TONE_GROUP.into()],
                submit: false,
            },
        )
        .commits(0),
        // 9: the same form submitted: a native preset captured from that entry, no edit.
        Step::new(
            "created",
            PresetCreateStep {
                name: NATIVE.into(),
                group: None,
                groups: vec![TONE_GROUP.into()],
                submit: true,
            },
        )
        .commits(0),
        // 10: undo to the Original.
        Step::new("undo-original", script::Step::api("history.undo"))
            .commits(1)
            .label("Original"),
        // 11: the native preset on the Original: one entry.
        Step::new("native", script::Step::preset(NATIVE))
            .commits(1)
            .label(format!("Preset: {NATIVE}")),
        // 12: the library through the generic api step, a host method that takes no asset.
        Step::new("list", script::Step::api("preset.list")).commits(0),
        // 13: the native preset deleted through its row's menu. History keeps the entry.
        Step::new(
            "deleted",
            script::Step::PresetDelete(PresetPick {
                name: NATIVE.into(),
                group: None,
            }),
        )
        .commits(0)
        .label(format!("Preset: {NATIVE}")),
    ])
}

fn presets(frame: &Frame) -> &Value {
    &frame["state"]["presets"]
}

/// The section's rows as (name, group, partial), in listed order.
fn rows(frame: &Frame) -> Result<Vec<(String, String, bool)>> {
    presets(frame)["rows"]
        .as_array()
        .ok_or("Frame records no preset rows")?
        .iter()
        .map(|row| {
            Ok((
                row["name"].as_str().ok_or("A row has no name")?.to_owned(),
                row["group"]
                    .as_str()
                    .ok_or("A row has no group")?
                    .to_owned(),
                row["partial"]
                    .as_bool()
                    .ok_or("A row has no partial flag")?,
            ))
        })
        .collect()
}

fn expect_rows(frame: &Frame, what: &str, expected: &[(&str, &str, bool)]) -> Result {
    let found = rows(frame)?;
    let expected: Vec<(String, String, bool)> = expected
        .iter()
        .map(|(name, group, partial)| ((*name).to_owned(), (*group).to_owned(), *partial))
        .collect();
    ensure(
        found == expected,
        format!("{what}: the section lists {found:?}, expected {expected:?}"),
    )
}

/// The committed stack's layers, by effect. A preset's modules each hold one layer.
fn layers(frame: &Frame) -> Result<BTreeMap<String, Value>> {
    let mut layers = BTreeMap::new();
    for layer in frame["state"]["stack"]["layers"]
        .as_array()
        .ok_or("Frame records no layers")?
    {
        let effect = layer["effect"]
            .as_str()
            .ok_or("A layer has no effect")?
            .to_owned();
        ensure(
            layers
                .insert(effect.clone(), layer["payload"].clone())
                .is_none(),
            format!("The stack holds two {effect} layers"),
        )?;
    }
    Ok(layers)
}

/// Each registered field-patch action's module effect and its parameters' declared defaults: what
/// a stored payload omits.
struct Patches(BTreeMap<String, (String, BTreeMap<String, f64>)>);

impl Patches {
    fn load() -> Result<Self> {
        let registry = luxforge_core::ModuleRegistry::builtin();
        let mut actions = BTreeMap::new();
        for module in registry.descriptors() {
            for action in module.actions.iter().filter(|action| action.patch) {
                ensure(
                    module.effects.len() == 1,
                    format!("{} patches more than one effect", module.id),
                )?;
                let defaults = action
                    .parameters
                    .iter()
                    .filter_map(|parameter| {
                        Some((
                            parameter.name.clone(),
                            parameter.default.as_ref()?.as_f64()?,
                        ))
                    })
                    .collect();
                actions.insert(action.id.clone(), (module.effects[0].id.clone(), defaults));
            }
        }
        Ok(Self(actions))
    }

    /// The stack `before` leaves once a settings set is applied: each named field overwrites the
    /// layer's value, every other field keeps it, and the stored payload omits the defaults.
    fn applied(
        &self,
        before: &BTreeMap<String, Value>,
        settings: &serde_json::Map<String, Value>,
    ) -> Result<BTreeMap<String, Value>> {
        let mut after = before.clone();
        for (action, fields) in settings {
            let (effect, defaults) = self
                .0
                .get(action)
                .ok_or_else(|| format!("{action} is not a registered field patch"))?;
            let mut payload = after
                .get(effect)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (field, value) in fields
                .as_object()
                .ok_or("A settings value is not an object")?
            {
                let number = value.as_f64().ok_or("A preset field is not a number")?;
                if defaults.get(field) == Some(&number) {
                    payload.remove(field);
                } else {
                    payload.insert(field.clone(), json!(number));
                }
            }
            after.insert(effect.clone(), Value::Object(payload));
        }
        Ok(after)
    }
}

/// Two payload maps hold the same fields at the same numbers.
fn same_layers(a: &BTreeMap<String, Value>, b: &BTreeMap<String, Value>) -> bool {
    a.len() == b.len()
        && a.iter().all(|(effect, payload)| {
            let (Some(left), Some(right)) = (
                payload.as_object(),
                b.get(effect).and_then(Value::as_object),
            ) else {
                return false;
            };
            left.len() == right.len()
                && left.iter().all(|(field, value)| {
                    match (value.as_f64(), right.get(field).and_then(Value::as_f64)) {
                        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
                        _ => false,
                    }
                })
        })
}

fn expect_layers(frame: &Frame, what: &str, expected: &BTreeMap<String, Value>) -> Result {
    let found = layers(frame)?;
    ensure(
        same_layers(&found, expected),
        format!("{what}: the stack holds {found:?}, expected {expected:?}"),
    )
}

/// The settings the core importer reads from one fixture file.
fn fixture_settings(root: &Path, path: &str) -> Result<serde_json::Map<String, Value>> {
    let text = fs::read_to_string(root.join(path))?;
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("A fixture has no file name")?;
    luxforge_core::inspect_preset(&text, Some(name), &luxforge_core::ModuleRegistry::builtin())
        .map(|preset| preset.settings)
        .map_err(|error| format!("{path}: {error}").into())
}

/// The mean RGB of one patch per quadrant of the photograph.
fn patches(frame: &Frame) -> Result<Vec<[f64; 3]>> {
    PATCHES
        .iter()
        .map(|(_, fx, fy)| frame.rgb_at([*fx, *fy], PATCH_HALF))
        .collect()
}

fn luminance(rgb: [f64; 3]) -> f64 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

fn mean_luminance(patches: &[[f64; 3]]) -> f64 {
    patches.iter().map(|rgb| luminance(*rgb)).sum::<f64>() / patches.len() as f64
}

/// The mean absolute per-channel change between two frames' patches.
fn change(a: &[[f64; 3]], b: &[[f64; 3]]) -> f64 {
    let total: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (0..3).map(|c| (x[c] - y[c]).abs()).sum::<f64>())
        .sum();
    total / (a.len() * 3) as f64
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let root = run.root().to_owned();
    for (name, frame) in launch.names().iter().zip(&launch.frames).skip(1) {
        ensure(
            frame.columns()?.is_some(),
            format!("Step {name:?} records no photo surface"),
        )?;
    }
    let at = |step: &str| launch.at(step);
    let patches_of = |step: &str| -> Result<Vec<[f64; 3]>> { patches(at(step)?) };
    let registry = Patches::load()?;
    let document = fixture_settings(&root, DOCUMENT)?;
    let xmp = fixture_settings(&root, XMP)?;
    let mut checks = Checks::new();

    // The photograph opens with the library listed and empty, the section collapsed.
    let opened = at("opened")?;
    ensure(
        presets(opened)["expanded"] == json!(false)
            && presets(opened)["empty"] == json!(true)
            && presets(opened)["loading"] == json!(false),
        format!(
            "The Presets section does not open as a collapsed, loaded, empty library: {}",
            presets(opened)
        ),
    )?;
    ensure(
        layers(opened)?.is_empty(),
        "The opened stack already holds a layer",
    )?;
    let original = patches_of("opened")?;

    // Presets expanded, with its empty-state line.
    ensure(
        presets(at("presets-expanded")?)["expanded"] == json!(true),
        "The Presets section did not expand",
    )?;
    expect_rows(at("presets-expanded")?, "The expanded section", &[])?;

    // The XMP imports as a partial preset in its own group, and its status line (whose lead the
    // plan holds) counts what it mapped, left unsupported and refused.
    let imported = at("xmp")?;
    expect_rows(
        imported,
        "The XMP import",
        &[("Soft Film", "Synthetic Looks", true)],
    )?;
    let status = imported.status()?;
    ensure(
        status.contains(" mapped, ")
            && status.contains(" unsupported, ")
            && status.ends_with(" refused"),
        format!("The XMP import's status line reads {status:?}"),
    )?;

    // The document imports into its own group, complete, listed before the XMP's group.
    let both = [
        ("Soft film", "Synthetic", false),
        ("Soft Film", "Synthetic Looks", true),
    ];
    expect_rows(at("document")?, "The document import", &both)?;
    let before_apply = patches_of("document")?;

    // The document's preset applied from its row: exactly its settings, a brighter photograph.
    let soft_frame = at("soft-film")?;
    let soft = registry.applied(&BTreeMap::new(), &document)?;
    expect_layers(soft_frame, "Soft film applied", &soft)?;
    let soft_patches = patches_of("soft-film")?;
    checks.compare(
        soft_frame,
        "Soft film's +0.35 EV brightens the quadrants",
        mean_luminance(&soft_patches),
        mean_luminance(&before_apply),
        Tolerance::Above(BRIGHTER),
    )?;

    // The XMP's preset over it: its fields merged over the stack.
    let merged_frame = at("soft-film-xmp")?;
    let merged = registry.applied(&soft, &xmp)?;
    expect_layers(merged_frame, "Soft Film applied over it", &merged)?;
    let merged_patches = patches_of("soft-film-xmp")?;
    checks.compare(
        merged_frame,
        "Soft Film over Soft film changes the photograph (mean change)",
        change(&merged_patches, &soft_patches),
        0.0,
        Tolerance::Above(CHANGED),
    )?;
    // Two of the XMP's own mixer fields move one quadrant each in a known direction: the green
    // range's luminance falls, and red's hue turns toward orange, which adds green to red.
    let mixer = &xmp["set-mixer"];
    ensure(
        mixer["green-luminance"].as_f64().is_some_and(|v| v < 0.0)
            && mixer["red-hue"].as_f64().is_some_and(|v| v > 0.0),
        format!("The XMP fixture no longer lowers green luminance and turns red's hue: {mixer}"),
    )?;
    checks.compare(
        merged_frame,
        "Soft Film darkens the green quadrant (luminance before, after)",
        luminance(soft_patches[1]),
        luminance(merged_patches[1]),
        Tolerance::Above(BRIGHTER),
    )?;
    checks.compare(
        merged_frame,
        "Soft Film turns the red quadrant toward orange (green channel after, before)",
        merged_patches[0][1],
        soft_patches[0][1],
        Tolerance::Above(BRIGHTER),
    )?;

    // Undo returns to the document's preset: its entry, its stack and its pixels.
    let undo = at("undo")?;
    ensure(
        undo.entry()? == soft_frame.entry()?,
        "Undo did not return to the Soft film entry",
    )?;
    expect_layers(undo, "Undo", &soft)?;
    checks.compare(
        undo,
        "the undone photograph against Soft film's (mean change)",
        change(&patches_of("undo")?, &soft_patches),
        0.0,
        Tolerance::Under(SAME),
    )?;

    // The create form filled but not submitted: the name, the default group and the Tone
    // checkbox alone, with Create enabled. Nothing is stored yet.
    let form_frame = at("form")?;
    let form = &presets(form_frame)["form"];
    ensure(
        form["open"] == json!(true)
            && form["name"] == json!(NATIVE)
            && form["group"] == json!(USER_GROUP)
            && form["checked"] == json!([TONE_GROUP])
            && form["can_create"] == json!(true),
        format!("The create form is not filled as scripted: {form}"),
    )?;
    expect_rows(form_frame, "The open form", &both)?;

    // The native preset, the Basic Tone group only, in User presets.
    let created = at("created")?;
    expect_rows(
        created,
        "The created preset",
        &[
            ("Soft film", "Synthetic", false),
            ("Soft Film", "Synthetic Looks", true),
            (NATIVE, USER_GROUP, false),
        ],
    )?;
    ensure(
        presets(created)["form"]["open"] == json!(false),
        "The create form is still open after a successful create",
    )?;

    // Undo to the Original: an empty stack and the opened pixels.
    let at_original_frame = at("undo-original")?;
    ensure(
        layers(at_original_frame)?.is_empty(),
        "Undo did not return to the Original's empty stack",
    )?;
    let at_original = patches_of("undo-original")?;
    checks.compare(
        at_original_frame,
        "the Original against the opened photograph (mean change)",
        change(&at_original, &original),
        0.0,
        Tolerance::Under(SAME),
    )?;

    // The native preset on the Original: a Basic layer holding exactly the Tone fields the Soft
    // film entry held, and nothing else.
    let native_frame = at("native")?;
    let basic = luxforge_core::BASIC_EFFECT.to_owned();
    let captured: serde_json::Map<String, Value> = soft
        .get(&basic)
        .and_then(Value::as_object)
        .map(|payload| {
            payload
                .iter()
                .filter(|(field, _)| TONE_FIELDS.contains(&field.as_str()))
                .map(|(field, value)| (field.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    ensure(
        !captured.is_empty(),
        "The Soft film entry holds no Tone field to capture",
    )?;
    let native = BTreeMap::from([(basic, Value::Object(captured))]);
    expect_layers(native_frame, "The native preset applied", &native)?;
    checks.compare(
        native_frame,
        "the Tone preset brightens the Original",
        mean_luminance(&patches_of("native")?),
        mean_luminance(&at_original),
        Tolerance::Above(BRIGHTER),
    )?;

    // `preset.list` through the generic api step answers with the whole library.
    let listed = at("list")?["step"]["result"]["presets"]
        .as_array()
        .ok_or("The preset.list step recorded no listing")?
        .len();
    ensure(
        listed == 3,
        format!("preset.list answered {listed} presets"),
    )?;

    // The native preset deleted through its row menu; history keeps the entry that applied it.
    expect_rows(at("deleted")?, "The deleted preset", &both)?;

    checks.write(
        &launch.evidence,
        "presets",
        json!({
            "brighter_margin": BRIGHTER,
            "same_tolerance": SAME,
            "changed_threshold": CHANGED,
            "patches": PATCHES.iter().map(|(name, x, y)| json!({"quadrant":name,"x":x,"y":y})).collect::<Vec<_>>(),
            "scope": "Stored payloads against a stepwise merge of the fixtures' imported settings; mean RGB of one patch per quadrant of the photograph the editor records drawing, read back from the renderer. A direction and correlation check, not a colorimetric claim",
        }),
    )
}
