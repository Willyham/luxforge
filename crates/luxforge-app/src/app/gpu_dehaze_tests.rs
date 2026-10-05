//! The drags the per-frame light's measurements draw (`docs/specs/performance.md`, "The per-frame
//! light's reduction factor"): Dehaze behind Detail at 100%, the colour drags between them, and
//! the drags at Fit behind the corpus's straightened crop. The measurement itself, every candidate
//! light's frame against the reference frame of the view, is [`factor`]'s.
use serde_json::{Value, json};

/// The per-frame light's reduction factor: these drags' stacks, and more, drawn with the light the
/// estimate twin would compute at each factor, against the reference frame of the view.
mod factor;

/// One drag: the stack it starts from, the steps that draft it, and the layer it drafts.
struct Drag {
    id: &'static str,
    committed: Vec<Value>,
    drafted: Value,
}

fn step(method: &str, params: Value) -> Value {
    json!({"api": {"method": method, "params": params}})
}

fn presence(texture: i32, clarity: i32, dehaze: i32) -> Value {
    step(
        "edit.set-presence",
        json!({"texture": texture, "clarity": clarity, "dehaze": dehaze}),
    )
}

fn detail(params: Value) -> Value {
    step("edit.set-detail", params)
}

fn basic(params: Value) -> Value {
    step("edit.set-basic", params)
}

/// The corpus's three Detail settings: the study's moderate one, noise stress and sharpen stress.
fn detail_moderate() -> Value {
    detail(json!({"luminance": 40, "colour": 40, "sharpening": 50, "radius": 1.0}))
}

fn detail_noise() -> Value {
    detail(json!({"luminance": 100, "colour": 100, "luminance-detail": 0, "colour-detail": 0}))
}

fn detail_sharpen() -> Value {
    detail(json!({"sharpening": 150, "radius": 3, "sharpen-detail": 100, "sharpen-masking": 0}))
}

/// The corpus's full Basic layer, its white balance on a JPEG only, as the corpus sets it.
fn basic_moderate(raw: bool) -> Value {
    let mut params = json!({"exposure": 0.5, "contrast": 25.0, "highlights": -30.0,
        "shadows": 30.0, "whites": -15.0, "blacks": 15.0, "vibrance": 30.0, "saturation": 15.0});
    if !raw {
        params["temperature"] = json!(20.0);
        params["tint"] = json!(-10.0);
    }
    basic(params)
}

/// The drags measured on each source:
///
/// - Presence over Detail, to every field at +100 and at -100;
/// - Detail from none to each of the corpus's settings, from moderate to sharpen stress, and to
///   sharpen stress under every Presence field at +100 and at -100;
/// - a Basic layer under Presence, after Detail where the placement rule puts it: the corpus's
///   settings, 1.5 stops either way with contrast, three stops either way with Whites and Blacks
///   at the same end, and three stops under every Presence field at +100 and at -100.
fn drags(raw: bool) -> Vec<Drag> {
    let moderate = || presence(50, 50, 30);
    let strong = || presence(100, 100, 100);
    let negative = || presence(-100, -100, -100);
    let up = || {
        basic(
            json!({"exposure": 1.5, "contrast": 60.0, "highlights": -60.0, "shadows": 60.0,
            "whites": 40.0, "blacks": -20.0}),
        )
    };
    let down = || basic(json!({"exposure": -1.5, "contrast": 40.0}));
    let plus3 = || basic(json!({"exposure": 3.0, "whites": 100.0, "blacks": 100.0}));
    let minus3 = || basic(json!({"exposure": -3.0, "whites": -100.0, "blacks": -100.0}));
    let drag = |id, committed: Vec<Value>, drafted| Drag {
        id,
        committed,
        drafted,
    };
    let base = || vec![detail_moderate(), moderate()];
    vec![
        drag("presence", base(), strong()),
        drag("presence-negative", base(), negative()),
        drag("detail-moderate", vec![moderate()], detail_moderate()),
        drag("detail-noise", vec![moderate()], detail_noise()),
        drag("detail-sharpen", vec![moderate()], detail_sharpen()),
        drag("detail-moderate-to-sharpen", base(), detail_sharpen()),
        drag("detail-sharpen-strong", vec![strong()], detail_sharpen()),
        drag(
            "detail-sharpen-negative",
            vec![negative()],
            detail_sharpen(),
        ),
        drag("basic-moderate", base(), basic_moderate(raw)),
        drag("basic-up", base(), up()),
        drag("basic-down", base(), down()),
        drag("basic-plus3", base(), plus3()),
        drag("basic-minus3", base(), minus3()),
        drag(
            "basic-plus3-strong",
            vec![detail_moderate(), strong()],
            plus3(),
        ),
        drag(
            "basic-minus3-negative",
            vec![detail_moderate(), negative()],
            minus3(),
        ),
    ]
}

/// Each unit's estimate, as the qualification module answers it.
type Estimates = Vec<Option<Vec<f64>>>;

/// The atmospheric light among a Presence layer's estimates.
fn light(estimates: &Estimates) -> Option<[f64; 3]> {
    estimates
        .iter()
        .flatten()
        .find(|values| values.len() == 3)
        .map(|values| [values[0], values[1], values[2]])
}

/// One drag at Fit behind the corpus's straightened crop: the stack it starts from, the step that
/// drafts it, and the layer it drafts.
struct CroppedDrag {
    id: &'static str,
    committed: Vec<Value>,
    drafted: Value,
}

/// The corpus's straightened crop.
fn cropped() -> Value {
    step("edit.crop-fit", json!({"aspect": "16:9", "angle": 7.0}))
}

/// The drags measured at Fit behind the crop: Dehaze to ±100 and every field to +100, a Basic
/// layer under Dehaze at the corpus's settings and at three stops either way, and Detail under it.
fn cropped_drags() -> Vec<CroppedDrag> {
    let drag = |id, committed: Vec<Value>, drafted| CroppedDrag {
        id,
        committed,
        drafted,
    };
    vec![
        drag(
            "crop-dehaze",
            vec![cropped(), presence(0, 0, 50)],
            presence(0, 0, 100),
        ),
        drag(
            "crop-dehaze-negative",
            vec![cropped(), presence(0, 0, -50)],
            presence(0, 0, -100),
        ),
        drag(
            "crop-presence-all",
            vec![cropped(), presence(50, 50, 50)],
            presence(100, 100, 100),
        ),
        drag(
            "crop-basic-under-dehaze",
            vec![cropped(), presence(0, 0, 100)],
            basic(json!({"exposure": 0.5, "contrast": 25.0})),
        ),
        drag(
            "crop-basic-plus3-under-dehaze",
            vec![cropped(), presence(0, 0, 100)],
            basic(json!({"exposure": 3.0, "whites": 100.0, "blacks": 100.0})),
        ),
        drag(
            "crop-basic-minus3-under-negative",
            vec![cropped(), presence(0, 0, -100)],
            basic(json!({"exposure": -3.0, "whites": -100.0, "blacks": -100.0})),
        ),
        drag(
            "crop-detail-under-dehaze",
            vec![cropped(), presence(0, 0, 100)],
            detail_sharpen(),
        ),
    ]
}
