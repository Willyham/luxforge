//! The `presets` smoke scenario: the Presets section on the real editor. It imports a Lightroom XMP
//! preset and a Lightwell preset document through the section's own import task, applies each from
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
    smoke::{columns, frame_identity},
    vignette_smoke::bright_bounds,
    *,
};
use std::collections::BTreeMap;

pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
const XMP: &str = "fixtures/presets/develop.xmp";
const DOCUMENT: &str = "fixtures/presets/soft-film.lwpreset";
const PRESETS_MODULE: &str = "lightwell.presets";
const BASIC_MODULE: &str = "lightwell.basic";
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

/// One open frame plus one per script step.
pub fn frames(scenario: &str) -> Option<usize> {
    (scenario == "presets").then_some(14)
}

pub fn source(scenario: &str) -> Option<&'static str> {
    (scenario == "presets").then_some(FIXTURE)
}

pub fn script(scenario: &str) -> Option<Value> {
    (scenario == "presets").then(|| {
        json!([
            // 1-2: Basic out of the way, the Presets section open.
            {"section":{"module":BASIC_MODULE,"expanded":false}},
            {"section":{"module":PRESETS_MODULE,"expanded":true}},
            // 3-4: two imports through the section's own task.
            {"preset_import":{"path":XMP}},
            {"preset_import":{"path":DOCUMENT}},
            // 5: the document's preset, whose name differs from the XMP's only in case.
            {"preset":{"name":"Soft film","group":"Synthetic"}},
            // 6: the XMP's preset over it, by its exact name alone.
            {"preset":{"name":"Soft Film"}},
            // 7: undo returns to the document's preset.
            {"api":{"method":"history.undo"}},
            // 8: the create form filled, Basic Tone alone, and left open for its frame.
            {"preset_create":{"name":NATIVE,"groups":[TONE_GROUP],"submit":false}},
            // 9: the same form submitted: a native preset captured from that entry.
            {"preset_create":{"name":NATIVE,"groups":[TONE_GROUP]}},
            // 10: undo to the Original.
            {"api":{"method":"history.undo"}},
            // 11: the native preset on the Original.
            {"preset":{"name":NATIVE}},
            // 12: the library through the generic api step, a host method that takes no asset.
            {"api":{"method":"preset.list"}},
            // 13: the native preset deleted through its row's menu.
            {"preset_delete":{"name":NATIVE}}
        ])
    })
}

fn revision(frame: &Value) -> Result<u64> {
    frame["state"]["stack"]["revision"]
        .as_u64()
        .ok_or_else(|| "Frame records no revision".into())
}

fn entry(frame: &Value) -> Result<&str> {
    frame["state"]["stack"]["entry"]
        .as_str()
        .ok_or_else(|| "Frame records no current entry".into())
}

fn label(frame: &Value) -> Result<&str> {
    frame["state"]["stack"]["label"]
        .as_str()
        .ok_or_else(|| "Frame records no history label".into())
}

fn presets(frame: &Value) -> &Value {
    &frame["state"]["presets"]
}

/// The section's rows as (name, group, partial), in listed order.
fn rows(frame: &Value) -> Result<Vec<(String, String, bool)>> {
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

fn expect_rows(frame: &Value, what: &str, expected: &[(&str, &str, bool)]) -> Result {
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
fn layers(frame: &Value) -> Result<BTreeMap<String, Value>> {
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
        let registry = lightwell_core::ModuleRegistry::builtin();
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

fn expect_layers(frame: &Value, what: &str, expected: &BTreeMap<String, Value>) -> Result {
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
    lightwell_core::inspect_preset(
        &text,
        Some(name),
        &lightwell_core::ModuleRegistry::builtin(),
    )
    .map(|preset| preset.settings)
    .map_err(|error| format!("{path}: {error}").into())
}

/// The mean RGB of one quadrant's patch of the photograph.
fn patches(path: &Path, frame: &Value) -> Result<Vec<[f64; 3]>> {
    let bounds = bright_bounds(path, frame)?;
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [left, top, right, bottom] = bounds;
    PATCHES
        .iter()
        .map(|(_, fx, fy)| {
            let px = f64::from(left) + fx * f64::from(right - left);
            let py = f64::from(top) + fy * f64::from(bottom - top);
            let mut sum = [0.0; 3];
            let mut count = 0.0;
            for dy in -PATCH_HALF..=PATCH_HALF {
                for dx in -PATCH_HALF..=PATCH_HALF {
                    let x = (px as i64 + dx).clamp(0, i64::from(width) - 1) as u32;
                    let y = (py as i64 + dy).clamp(0, i64::from(height) - 1) as u32;
                    let pixel = image.get_pixel(x, y).0;
                    for channel in 0..3 {
                        sum[channel] += f64::from(pixel[channel]);
                    }
                    count += 1.0;
                }
            }
            Ok(sum.map(|value| value / count))
        })
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

fn patch_record(patches: &[[f64; 3]]) -> Value {
    Value::Object(
        PATCHES
            .iter()
            .zip(patches)
            .map(|((name, ..), rgb)| {
                (
                    (*name).to_owned(),
                    json!({"rgb":rgb.map(|v| (v * 10.0).round() / 10.0),"luminance":(luminance(*rgb) * 10.0).round() / 10.0}),
                )
            })
            .collect(),
    )
}

fn step_status(frame: &Value) -> &Value {
    &frame["step"]["status"]
}

pub fn verify(evidence: &Path, app: &Value, _events: &[Value]) -> Result {
    let root = root()?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?.clone();
    ensure(
        app["had_input_errors"] == json!(false),
        "The run recorded an input error",
    )?;
    let paths: Vec<PathBuf> = frames
        .iter()
        .map(|frame| frame_identity(evidence, app, frame))
        .collect::<Result<Vec<_>>>()?;
    for (index, frame) in frames.iter().enumerate().skip(1) {
        ensure(
            step_status(frame) == &json!("sent"),
            format!("Step {index} did not run: {}", frame["step"]),
        )?;
        ensure(
            columns(frame)?.is_some(),
            format!("Frame {index} records no photo surface"),
        )?;
    }
    let patches_of = |index: usize| patches(&paths[index], &frames[index]);
    let registry = Patches::load()?;
    let document = fixture_settings(&root, DOCUMENT)?;
    let xmp = fixture_settings(&root, XMP)?;
    let mut checks = Vec::new();
    let mut record = |index: usize, shows: &str, detail: Value| {
        checks.push(json!({"frame":frames[index]["file"],"shows":shows,"detail":detail}));
    };

    // Frame 0: the photograph opens with the library listed and empty, the section collapsed.
    let opened = revision(&frames[0])?;
    ensure(
        presets(&frames[0])["expanded"] == json!(false)
            && presets(&frames[0])["empty"] == json!(true)
            && presets(&frames[0])["loading"] == json!(false),
        format!(
            "Frame 0: the Presets section is not a collapsed, loaded, empty library: {}",
            presets(&frames[0])
        ),
    )?;
    ensure(
        layers(&frames[0])?.is_empty(),
        "Frame 0: the opened stack already holds a layer",
    )?;
    let original = patches_of(0)?;
    record(
        0,
        "the opened photograph, the Presets section collapsed and empty",
        json!({"revision":opened,"patches":patch_record(&original)}),
    );

    // Frames 1-2: Basic collapsed, then Presets expanded. Nothing is committed.
    ensure(
        frames[1]["state"]["expanded"][BASIC_MODULE] == json!(false),
        "Frame 1: Basic is still expanded",
    )?;
    ensure(
        presets(&frames[2])["expanded"] == json!(true)
            && frames[2]["state"]["expanded"][PRESETS_MODULE] == json!(true),
        "Frame 2: the Presets section did not expand",
    )?;
    for (index, frame) in frames.iter().enumerate().take(3).skip(1) {
        ensure(
            revision(frame)? == opened,
            format!("Frame {index}: a section toggle committed something"),
        )?;
    }
    expect_rows(&frames[2], "Frame 2", &[])?;
    record(
        2,
        "the Presets section expanded with its empty-state line",
        json!({"presets":presets(&frames[2])}),
    );

    // Frame 3: the XMP imports as a partial preset in its own group.
    expect_rows(
        &frames[3],
        "Frame 3",
        &[("Soft Film", "Synthetic Looks", true)],
    )?;
    let status = frames[3]["state"]["status"].as_str().unwrap_or_default();
    ensure(
        status.starts_with("Imported \u{201c}Soft Film\u{201d}: ")
            && status.contains(" mapped, ")
            && status.contains(" unsupported, ")
            && status.ends_with(" refused"),
        format!("Frame 3: the status line reads {status:?}"),
    )?;
    ensure(
        revision(&frames[3])? == opened,
        "Frame 3: an import committed an edit",
    )?;
    record(
        3,
        "the XMP imported: one Partial row in Synthetic Looks and the import status line",
        json!({"status":status,"rows":presets(&frames[3])["rows"]}),
    );

    // Frame 4: the document imports into its own group, complete, listed before the XMP's group.
    expect_rows(
        &frames[4],
        "Frame 4",
        &[
            ("Soft film", "Synthetic", false),
            ("Soft Film", "Synthetic Looks", true),
        ],
    )?;
    let before_apply = patches_of(4)?;
    record(
        4,
        "the document imported: two groups, only the XMP row Partial",
        json!({"status":frames[4]["state"]["status"],"rows":presets(&frames[4])["rows"]}),
    );

    // Frame 5: the document's preset applied from its row: one entry, exactly its settings.
    ensure(
        revision(&frames[5])? == opened + 1,
        "Frame 5: applying the preset did not commit exactly one revision",
    )?;
    ensure(
        label(&frames[5])? == "Preset: Soft film",
        format!("Frame 5 is labelled {:?}", label(&frames[5])?),
    )?;
    let soft = registry.applied(&BTreeMap::new(), &document)?;
    expect_layers(&frames[5], "Frame 5", &soft)?;
    let soft_patches = patches_of(5)?;
    ensure(
        mean_luminance(&soft_patches) > mean_luminance(&before_apply) + BRIGHTER,
        format!(
            "Frame 5: +0.35 EV did not brighten the quadrants: {:.1} against {:.1}",
            mean_luminance(&soft_patches),
            mean_luminance(&before_apply)
        ),
    )?;
    record(
        5,
        "Soft film applied from its row: one entry \"Preset: Soft film\", its settings exactly, and a brighter photograph",
        json!({"revision":revision(&frames[5])?,"label":label(&frames[5])?,"layers":layers(&frames[5])?,"patches":patch_record(&soft_patches)}),
    );

    // Frame 6: the XMP's preset over it: one more entry, its fields merged over the stack.
    ensure(
        revision(&frames[6])? == opened + 2,
        "Frame 6: applying the second preset did not commit exactly one revision",
    )?;
    ensure(
        label(&frames[6])? == "Preset: Soft Film",
        format!("Frame 6 is labelled {:?}", label(&frames[6])?),
    )?;
    let merged = registry.applied(&soft, &xmp)?;
    expect_layers(&frames[6], "Frame 6", &merged)?;
    let merged_patches = patches_of(6)?;
    ensure(
        change(&merged_patches, &soft_patches) > CHANGED,
        format!(
            "Frame 6: the photograph did not change: {:.2} mean change",
            change(&merged_patches, &soft_patches)
        ),
    )?;
    // Two of the XMP's own mixer fields move one quadrant each in a known direction: the green
    // range's luminance falls, and red's hue turns toward orange, which adds green to red.
    let mixer = &xmp["set-mixer"];
    ensure(
        mixer["green-luminance"].as_f64().is_some_and(|v| v < 0.0)
            && mixer["red-hue"].as_f64().is_some_and(|v| v > 0.0),
        format!("The XMP fixture no longer lowers green luminance and turns red's hue: {mixer}"),
    )?;
    let (green_before, green_after) = (luminance(soft_patches[1]), luminance(merged_patches[1]));
    ensure(
        green_after < green_before - BRIGHTER,
        format!(
            "Frame 6: the green quadrant did not darken: {green_after:.1} against {green_before:.1}"
        ),
    )?;
    let (red_before, red_after) = (soft_patches[0][1], merged_patches[0][1]);
    ensure(
        red_after > red_before + BRIGHTER,
        format!(
            "Frame 6: the red quadrant did not turn toward orange: green channel {red_after:.1} against {red_before:.1}"
        ),
    )?;
    record(
        6,
        "Soft Film applied over it: one more entry, the XMP's fields merged over the stack, the green quadrant darker and the red one turned toward orange",
        json!({"revision":revision(&frames[6])?,"label":label(&frames[6])?,"layers":layers(&frames[6])?,"patches":patch_record(&merged_patches),"change":change(&merged_patches, &soft_patches)}),
    );

    // Frame 7: undo returns to the document's preset, its stack and its pixels.
    ensure(
        entry(&frames[7])? == entry(&frames[5])? && label(&frames[7])? == "Preset: Soft film",
        "Frame 7: undo did not return to the Soft film entry",
    )?;
    expect_layers(&frames[7], "Frame 7", &soft)?;
    let undone = patches_of(7)?;
    ensure(
        change(&undone, &soft_patches) < SAME,
        format!(
            "Frame 7: the undone photograph differs from Frame 5's: {:.2}",
            change(&undone, &soft_patches)
        ),
    )?;
    record(
        7,
        "undo: the Soft film entry, its stack and its pixels again",
        json!({"entry":entry(&frames[7])?,"patches":patch_record(&undone)}),
    );

    // Frame 8: the create form filled but not submitted: the name, the default group and the Tone
    // checkbox alone, with Create enabled. Nothing is stored yet.
    let form = &presets(&frames[8])["form"];
    ensure(
        form["open"] == json!(true)
            && form["name"] == json!(NATIVE)
            && form["group"] == json!(USER_GROUP)
            && form["checked"] == json!([TONE_GROUP])
            && form["can_create"] == json!(true),
        format!("Frame 8: the create form is not filled as scripted: {form}"),
    )?;
    expect_rows(
        &frames[8],
        "Frame 8",
        &[
            ("Soft film", "Synthetic", false),
            ("Soft Film", "Synthetic Looks", true),
        ],
    )?;
    record(
        8,
        "the create form open and filled: the name, User presets and the Tone group alone",
        json!({"form":form}),
    );

    // Frame 9: the native preset, the Basic Tone group only, in User presets.
    expect_rows(
        &frames[9],
        "Frame 9",
        &[
            ("Soft film", "Synthetic", false),
            ("Soft Film", "Synthetic Looks", true),
            (NATIVE, USER_GROUP, false),
        ],
    )?;
    ensure(
        presets(&frames[9])["form"]["open"] == json!(false),
        "Frame 9: the create form is still open after a successful create",
    )?;
    ensure(
        frames[9]["step"]["request"]["preset_create"]["groups"] == json!([TONE_GROUP]),
        "Frame 9: the step did not keep the Tone group alone",
    )?;
    ensure(
        revision(&frames[9])? == revision(&frames[8])?,
        "Frame 9: creating a preset committed an edit",
    )?;
    record(
        9,
        "a native preset of the Tone group alone, listed in User presets",
        json!({"status":frames[9]["state"]["status"],"rows":presets(&frames[9])["rows"]}),
    );

    // Frame 10: undo to the Original.
    ensure(
        layers(&frames[10])?.is_empty(),
        "Frame 10: undo did not return to the Original's empty stack",
    )?;
    let at_original = patches_of(10)?;
    ensure(
        change(&at_original, &original) < SAME,
        format!(
            "Frame 10: the Original reads differently from Frame 0: {:.2}",
            change(&at_original, &original)
        ),
    )?;
    record(
        10,
        "undo to the Original: an empty stack and the opened pixels",
        json!({"label":label(&frames[10])?,"patches":patch_record(&at_original)}),
    );

    // Frame 11: the native preset on the Original: one entry, a Basic layer holding exactly the
    // Tone fields the Soft film entry held, and nothing else.
    ensure(
        revision(&frames[11])? == revision(&frames[10])? + 1,
        "Frame 11: applying the native preset did not commit exactly one revision",
    )?;
    ensure(
        label(&frames[11])? == format!("Preset: {NATIVE}"),
        format!("Frame 11 is labelled {:?}", label(&frames[11])?),
    )?;
    let basic = lightwell_core::BASIC_EFFECT.to_owned();
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
    expect_layers(&frames[11], "Frame 11", &native)?;
    let native_patches = patches_of(11)?;
    ensure(
        mean_luminance(&native_patches) > mean_luminance(&at_original) + BRIGHTER,
        format!(
            "Frame 11: the Tone preset did not brighten the Original: {:.1} against {:.1}",
            mean_luminance(&native_patches),
            mean_luminance(&at_original)
        ),
    )?;
    record(
        11,
        "the native preset on the Original: one entry, exactly the captured Tone fields, a brighter photograph",
        json!({"label":label(&frames[11])?,"layers":layers(&frames[11])?,"patches":patch_record(&native_patches)}),
    );

    // Frame 12: `preset.list` through the generic api step answers with the whole library and
    // commits nothing.
    let listed = frames[12]["step"]["result"]["presets"]
        .as_array()
        .ok_or("Frame 12: the api step recorded no listing")?;
    ensure(
        listed.len() == 3,
        format!("Frame 12: preset.list answered {} presets", listed.len()),
    )?;
    ensure(
        revision(&frames[12])? == revision(&frames[11])?,
        "Frame 12: listing committed something",
    )?;
    record(
        12,
        "preset.list through the api step: three presets, nothing committed",
        json!({"listed":listed.len()}),
    );

    // Frame 13: the native preset deleted through its row menu. History keeps the entry.
    expect_rows(
        &frames[13],
        "Frame 13",
        &[
            ("Soft film", "Synthetic", false),
            ("Soft Film", "Synthetic Looks", true),
        ],
    )?;
    ensure(
        revision(&frames[13])? == revision(&frames[11])?
            && label(&frames[13])? == format!("Preset: {NATIVE}"),
        "Frame 13: deleting the preset changed the photograph's history",
    )?;
    record(
        13,
        "the native preset deleted from its row menu; the entry that applied it remains",
        json!({"status":frames[13]["state"]["status"],"rows":presets(&frames[13])["rows"]}),
    );

    write_json(
        &evidence.join("presets-checks.json"),
        &json!({
            "checks": checks,
            "brighter_margin": BRIGHTER,
            "same_tolerance": SAME,
            "changed_threshold": CHANGED,
            "patches": PATCHES.iter().map(|(name, x, y)| json!({"quadrant":name,"x":x,"y":y})).collect::<Vec<_>>(),
            "scope": "Stored payloads against a stepwise merge of the fixtures' imported settings; mean RGB of one patch per quadrant, read back from the renderer. A direction and correlation check, not a colorimetric claim",
        }),
    )?;
    Ok(())
}
