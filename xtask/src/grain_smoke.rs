//! The `grain` smoke scenario: the Grain section offered once another client turns the `grain`
//! experiment on, its own expand, an Amount drag and commit, Size and Roughness commits, a crop,
//! and its undo and redo, on the real editor, captured at Fit and 100%.
//!
//! The fixture is `fixtures/generated/grain.jpg` (`cargo xtask generate-fixtures`): 2400 × 1600, so
//! at 100% the window's edges cut it, a flat mid-grey where grain shows plainly, crossed in its
//! upper third by a band of black, white and four saturated patches
//! ([`crate::fixtures::GRAIN_FIXTURE`]). 100% shows the frame's top-left corner, the canvas's right
//! and bottom edges cutting it. Each 100% frame is read in capture pixels: grey patches near the
//! visible photograph's four sides and its centre column, and one interior of each band patch,
//! located through the photograph's recorded rectangle.
//!
//! What the 100% frames must show, against the same view before any grain (`baseline`): grain on
//! the grey, of about the same strength at every patch, the grey's mean and neutrality kept, black
//! and white untouched, and each colour's mean kept. Size coarsens the grain, which raises the
//! correlation of neighbouring pixels; Roughness changes the pattern; a crop leaves the grain's
//! size where it was, since grain is sized in output pixels; and undo and redo return each state's
//! grain. Fit frames are reduced stages, where grain is band-limited and attenuated by design
//! (`docs/design/grain.md`), so they are noted, not judged.
use crate::{
    fixtures::{GRAIN_BAND, GRAIN_FIXTURE, GRAIN_PATCH, GRAIN_PATCHES},
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, pixels, plan::only},
    *,
};
use luxforge_core::{CROP_EFFECT, GRAIN_EFFECT};
use luxforge_evidence::{self as script, SliderStep, ViewStep};

const GRAIN_MODULE: &str = "luxforge.grain";
/// The experiment flag the Grain section and `set-grain` are offered under.
const GRAIN_FLAG: &str = "grain";
/// The sections above Grain that are expanded by their own descriptor's default, collapsed first
/// so the module's own sliders are on screen without scrolling. Every other section above it is
/// collapsed by default or draws no section.
const BASIC_MODULE: &str = "luxforge.basic";
const CROP_MODULE: &str = "luxforge.crop";
const SET_GRAIN: &str = "set-grain";
const AMOUNT: &str = "amount";
const SIZE: &str = "size";
const ROUGHNESS: &str = "roughness";
pub const FIXTURE: &str = "fixtures/generated/grain.jpg";

/// Half the side, in capture pixels, of a grey patch read for grain.
const GREY_HALF: u32 = 24;
/// Where the grey patches sit, as fractions of the visible photograph: near its left, right, top
/// and bottom edges, the right and bottom ones where the canvas cuts the photograph at 100%, and
/// at its centre column, above and below the band.
const GREY_AT: [[f64; 2]; 6] = [
    [0.08, 0.12],
    [0.5, 0.12],
    [0.92, 0.12],
    [0.08, 0.88],
    [0.5, 0.88],
    [0.92, 0.88],
];
/// How far inside a band patch, in photograph pixels, its interior starts: clear of a JPEG's
/// ringing at the patch's edges.
const PATCH_INSET: u32 = 20;

/// The grey's standard deviation must rise by more than this many codes for grain to count as
/// shown: Amount 60 at Size 25 raises it several times over, and a flat JPEG grey reads below 1.
const SHOWN: f64 = 2.0;
/// The grey's mean, and a colour patch's channel means, kept within this many codes: grain is
/// mean-preserving.
const MEAN_KEPT: f64 = 1.5;
/// The grey's channel means kept within this many codes of each other: luminance grain adds no
/// colour cast.
const NO_CAST: f64 = 1.0;
/// Black and white untouched: their mean and standard deviation within this many codes of the
/// same view's before any grain.
const UNTOUCHED: f64 = 0.5;
/// Even across the view: the strongest grey patch's deviation at most this share above the
/// weakest's.
const EVEN: f64 = 0.5;
/// Size 80 against Size 25 raises the neighbouring-pixel correlation by more than this.
const COARSER: f64 = 0.1;
/// The same settings before and after a crop, or before and after an undo and redo, keep the grey's
/// deviation within this share and its neighbouring-pixel correlation within [`COARSER`].
const SAME_GRAIN: f64 = 0.2;
/// Roughness changes the pattern: the mean absolute difference over the grey patches between the
/// Size 80 frame and the Roughness 90 frame, in codes, is more than this.
const ROUGHER: f64 = 1.0;

/// A committed Grain slider release.
fn release(name: &str, parameter: &str, value: f64) -> Step {
    Step::new(
        name,
        SliderStep::new(SET_GRAIN, parameter, [value]).release(),
    )
    .no_draft()
    .commits(1)
}

/// Every frame, in order: the open, then one per step.
pub fn plan(_: &[PathBuf]) -> Plan {
    let section = |name: &str, module: &str, expanded: bool| {
        Step::new(name, script::Step::section(module, expanded)).commits(0)
    };
    let view = |name: &str, step: ViewStep| Step::new(name, step).no_draft().commits(0);
    let all = json!({AMOUNT: 60.0, SIZE: 80.0, ROUGHNESS: 90.0});
    Plan::new(vec![
        // The fixture opens with the grain experiment off: no Grain section, no draft and no grain
        // layer.
        Step::opened("opened").no_draft().no_layer(GRAIN_EFFECT),
        // Another client turns the experiment on; the desktop follows its `flags.set` event and
        // lists the Grain section, collapsed, every field at its default.
        Step::new(
            "grain-on",
            script::Step::agent("flags.set", json!({"flag": GRAIN_FLAG, "value": true})),
        )
        .commits(0)
        .collapsed(GRAIN_MODULE)
        .field(SET_GRAIN, AMOUNT, "0")
        .field(SET_GRAIN, SIZE, "25")
        .field(SET_GRAIN, ROUGHNESS, "50")
        .no_draft()
        .no_layer(GRAIN_EFFECT),
        section("basic-collapsed", BASIC_MODULE, false).collapsed(BASIC_MODULE),
        section("crop-collapsed", CROP_MODULE, false).collapsed(CROP_MODULE),
        section("expanded", GRAIN_MODULE, true)
            .expanded(GRAIN_MODULE)
            .collapsed(BASIC_MODULE)
            .collapsed(CROP_MODULE),
        // The view at 100% before any grain: what every 100% frame is judged against.
        view("baseline", ViewStep::Percent(100.0))
            .percent(100.0)
            .no_layer(GRAIN_EFFECT),
        view("fit", ViewStep::Fit).fit(),
        // A drag on Amount, left open: the frame shows the drafted preview, nothing committed.
        Step::new(
            "drag",
            SliderStep::new(SET_GRAIN, AMOUNT, [20.0, 40.0, 60.0]),
        )
        .commits(0)
        .draft(SET_GRAIN, json!({ AMOUNT: 60.0 }))
        .no_layer(GRAIN_EFFECT)
        .field(SET_GRAIN, AMOUNT, "60"),
        // The same gesture released: one entry, committed at Fit.
        release("release", AMOUNT, 60.0)
            .label("Grain amount 60")
            .payload(GRAIN_EFFECT, json!({ AMOUNT: 60.0 })),
        // The committed Amount at 100%.
        view("percent", ViewStep::Percent(100.0)).percent(100.0),
        // Size and Roughness, each committed at 100%, updating the same layer.
        release("size", SIZE, 80.0)
            .label("Grain size 80")
            .payload(GRAIN_EFFECT, json!({ AMOUNT: 60.0, SIZE: 80.0 }))
            .same_layer(GRAIN_EFFECT, "release"),
        release("roughness", ROUGHNESS, 90.0)
            .label("Grain roughness 90")
            .payload(GRAIN_EFFECT, all.clone())
            .same_layer(GRAIN_EFFECT, "release"),
        view("fit-rough", ViewStep::Fit).fit(),
        // A crop after the grain exists: the host places the crop layer before it.
        Step::new(
            "cropped",
            script::Step::call("edit.crop-fit", json!({"aspect":"1:1","angle":0})),
        )
        .commits(1)
        .payload(GRAIN_EFFECT, all.clone())
        .same_layer(GRAIN_EFFECT, "release"),
        view("cropped-percent", ViewStep::Percent(100.0)).percent(100.0),
        // Undo takes the crop back off and redo puts it back, the grain layer untouched by both.
        Step::new("undo", script::Step::call("history.undo", json!({})))
            .commits(1)
            .label("Grain roughness 90")
            .payload(GRAIN_EFFECT, all.clone())
            .same_layer(GRAIN_EFFECT, "release"),
        Step::new("redo", script::Step::call("history.redo", json!({})))
            .commits(1)
            .payload(GRAIN_EFFECT, all)
            .same_layer(GRAIN_EFFECT, "release"),
        view("fit-redone", ViewStep::Fit).fit(),
    ])
}

/// The stack's layer effects in stored order.
fn layer_effects(frame: &Frame) -> Vec<String> {
    frame.state()["stack"]["layers"]
        .as_array()
        .map(|layers| {
            layers
                .iter()
                .filter_map(|layer| layer["effect"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The output stage's size in photograph pixels: the fixture's, or the 1:1 crop's square of its
/// full height.
fn stage(frame: &Frame) -> (u32, u32) {
    let (width, height) = GRAIN_FIXTURE;
    if layer_effects(frame)
        .iter()
        .any(|effect| effect == CROP_EFFECT)
    {
        (height, height)
    } else {
        (width, height)
    }
}

/// What one patch of a capture reads: each channel's mean, the Rec. 709 luminance's standard
/// deviation, and the correlation of horizontally neighbouring luminances.
#[derive(Clone, Copy, Debug)]
struct Reading {
    mean: [f64; 3],
    deviation: f64,
    correlation: f64,
}

impl Reading {
    fn luminance(&self) -> f64 {
        0.2126 * self.mean[0] + 0.7152 * self.mean[1] + 0.0722 * self.mean[2]
    }

    fn record(&self) -> Value {
        json!({"mean_rgb": self.mean, "deviation": self.deviation, "correlation": self.correlation})
    }
}

/// [`Reading`] of the capture pixels `[left, right) × [top, bottom)`.
fn read(frame: &Frame, [left, top, right, bottom]: [u32; 4]) -> Result<Reading> {
    let image = frame.image()?;
    ensure(
        left + 2 < right && top < bottom && right <= image.width() && bottom <= image.height(),
        format!(
            "Patch {:?} is outside the capture",
            [left, top, right, bottom]
        ),
    )?;
    let mut sum = [0.0; 3];
    let mut luma = Vec::new();
    for y in top..bottom {
        for x in left..right {
            let pixel = image.get_pixel(x, y).0;
            for (total, channel) in sum.iter_mut().zip(pixel) {
                *total += f64::from(channel);
            }
            luma.push(pixels::luminance(pixel));
        }
    }
    let count = luma.len() as f64;
    let mean = sum.map(|total| total / count);
    let average = luma.iter().sum::<f64>() / count;
    let variance = luma.iter().map(|l| (l - average).powi(2)).sum::<f64>() / count;
    let width = (right - left) as usize;
    let (mut covariance, mut pairs) = (0.0, 0.0);
    for row in luma.chunks(width) {
        for pair in row.windows(2) {
            covariance += (pair[0] - average) * (pair[1] - average);
            pairs += 1.0;
        }
    }
    let correlation = if variance > 0.0 {
        covariance / pairs / variance
    } else {
        0.0
    };
    Ok(Reading {
        mean,
        deviation: variance.sqrt(),
        correlation,
    })
}

/// The capture rectangle of the photograph pixels `[x0, x1) × [y0, y1)` of the output stage, when
/// it is all inside the part of the photograph on screen.
fn on_screen(frame: &Frame, [x0, y0, x1, y1]: [u32; 4]) -> Result<Option<[u32; 4]>> {
    let [left, top, right, bottom] = frame.photo_rect()?;
    let visible = frame.visible_photo()?;
    let (width, height) = stage(frame);
    let x = |px: u32| left as f64 + f64::from(px) * (right - left) as f64 / f64::from(width);
    let y = |py: u32| top as f64 + f64::from(py) * (bottom - top) as f64 / f64::from(height);
    let rect = [x(x0).ceil(), y(y0).ceil(), x(x1).floor(), y(y1).floor()];
    Ok((rect[0] >= f64::from(visible[0])
        && rect[1] >= f64::from(visible[1])
        && rect[2] <= f64::from(visible[2])
        && rect[3] <= f64::from(visible[3]))
    .then(|| rect.map(|edge| edge as u32)))
}

/// The grey patches at [`GREY_AT`], each clear of the band.
fn greys(frame: &Frame) -> Result<Vec<Reading>> {
    let visible = frame.visible_photo()?;
    let [left, top, right, bottom] = frame.photo_rect()?;
    let (_, height) = stage(frame);
    GREY_AT
        .iter()
        .map(|at| {
            let (cx, cy) = pixels::at(visible, *at);
            let row = (cy - top as f64) / (bottom - top) as f64 * f64::from(height);
            ensure(
                row < f64::from(GRAIN_BAND.0) - 48.0 || row > f64::from(GRAIN_BAND.1) + 48.0,
                format!("Grey patch {at:?} falls on the band at row {row:.0}"),
            )?;
            ensure(
                (left as f64) < cx && cx < right as f64,
                "Grey patch off the photograph",
            )?;
            let (cx, cy) = (cx as u32, cy as u32);
            read(
                frame,
                [
                    cx - GREY_HALF,
                    cy - GREY_HALF,
                    cx + GREY_HALF,
                    cy + GREY_HALF,
                ],
            )
        })
        .collect()
}

/// The interior of the leftmost wholly visible patch of each kind, in [`GRAIN_PATCHES`] order, of
/// an uncropped frame.
fn band(frame: &Frame) -> Result<Vec<Reading>> {
    let (width, _) = stage(frame);
    let period = GRAIN_PATCH * GRAIN_PATCHES.len() as u32;
    let rows = [GRAIN_BAND.0 + PATCH_INSET, GRAIN_BAND.1 - PATCH_INSET];
    let mut readings = Vec::new();
    for (index, (name, _)) in GRAIN_PATCHES.iter().enumerate() {
        let mut found = None;
        for x in (index as u32 * GRAIN_PATCH..width).step_by(period as usize) {
            let interior = [
                x + PATCH_INSET,
                rows[0],
                x + GRAIN_PATCH - PATCH_INSET,
                rows[1],
            ];
            found = on_screen(frame, interior)?;
            if found.is_some() {
                break;
            }
        }
        let rect = found.ok_or_else(|| format!("No {name} patch is wholly on screen"))?;
        readings.push(read(frame, rect)?);
    }
    Ok(readings)
}

/// The mean absolute channel difference between two captures over the grey patches.
fn grey_difference(first: &Frame, second: &Frame) -> Result<f64> {
    let visible = first.visible_photo()?;
    ensure(
        second.visible_photo()? == visible,
        "The two frames show different rectangles",
    )?;
    let (a, b) = (first.image()?, second.image()?);
    let (mut total, mut count) = (0.0, 0.0);
    for at in GREY_AT {
        let (cx, cy) = pixels::at(visible, at);
        let (cx, cy) = (cx as u32, cy as u32);
        for y in cy - GREY_HALF..cy + GREY_HALF {
            for x in cx - GREY_HALF..cx + GREY_HALF {
                for (p, q) in a.get_pixel(x, y).0.into_iter().zip(b.get_pixel(x, y).0) {
                    total += f64::from(p.abs_diff(q));
                    count += 1.0;
                }
            }
        }
    }
    Ok(total / count)
}

fn records(readings: &[Reading]) -> Value {
    Value::Array(readings.iter().map(Reading::record).collect())
}

fn mean_of(readings: &[Reading], value: impl Fn(&Reading) -> f64) -> f64 {
    readings.iter().map(value).sum::<f64>() / readings.len() as f64
}

/// Which path drew the frame's picture at rest, and the adapter the session reports.
fn drawing(frame: &Frame) -> Value {
    let state = frame.state();
    json!({
        "drawing_path": state["surface"]["gpu"]["drawing_path"],
        "renderer": state["renderer"],
    })
}

/// The grain the grey patches of `frame` show against `before`'s, the same settings: deviation and
/// correlation both kept.
fn same_grain(
    checks: &mut Checks,
    frame: &Frame,
    what: &str,
    grey: &[Reading],
    before: &[Reading],
) -> Result {
    let (deviation, previous) = (
        mean_of(grey, |r| r.deviation),
        mean_of(before, |r| r.deviation),
    );
    checks.compare(
        frame,
        &format!("{what}: the grain's strength kept"),
        deviation,
        previous,
        Tolerance::Within(SAME_GRAIN * previous),
    )?;
    checks.compare(
        frame,
        &format!("{what}: the grain's size kept"),
        mean_of(grey, |r| r.correlation),
        mean_of(before, |r| r.correlation),
        Tolerance::Within(COARSER),
    )
}

/// What the photograph shows at each step, once the plan has held.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();

    let opened = launch.at("opened")?;
    opened.module_available(GRAIN_MODULE)?;
    ensure(
        opened.state()["expanded"][GRAIN_MODULE].is_null(),
        "The Grain section is listed with the grain experiment off",
    )?;
    checks.note(
        opened,
        "no Grain section as launched, with the grain experiment off",
        drawing(opened),
    );
    let on = launch.at("grain-on")?;
    ensure(
        on.state()["active"][GRAIN_MODULE] == json!(false),
        "The Grain section carries a dot with no grain layer",
    )?;
    checks.note(
        on,
        "the collapsed Grain section once the experiment is on",
        drawing(on),
    );

    // The view at 100% before any grain.
    let baseline = launch.at("baseline")?;
    let (base_grey, base_band) = (greys(baseline)?, band(baseline)?);
    checks.note(
        baseline,
        "100% before any grain",
        json!({"grey": records(&base_grey), "band": records(&base_band), "drawing": drawing(baseline)}),
    );

    // Mid-gesture: the frame on screen is the drafted one, and no dot until a commit.
    let drag = launch.at("drag")?;
    drag.displays_draft()?;
    ensure(
        drag.state()["active"][GRAIN_MODULE] == json!(false),
        "The dot follows a draft rather than the committed recipe",
    )?;

    // The release at Fit lights the dot. Fit is a reduced stage: noted, not judged.
    let released = launch.at("release")?;
    ensure(
        released.state()["active"][GRAIN_MODULE] == json!(true),
        "The Grain section carries no dot at Amount 60",
    )?;
    checks.note(
        released,
        "Amount 60 at Fit, a reduced stage",
        json!({"grey": records(&greys(released)?), "opened_grey": records(&greys(opened)?), "drawing": drawing(released)}),
    );

    // Amount 60 at 100%: grain on every grey patch, even across the view, mean and neutrality
    // kept; black and white untouched; each colour's mean kept.
    let percent = launch.at("percent")?;
    let (grey, patches) = (greys(percent)?, band(percent)?);
    for (index, (after, before)) in grey.iter().zip(&base_grey).enumerate() {
        checks.compare(
            percent,
            &format!("grey patch {index}: grain shown"),
            after.deviation,
            before.deviation,
            Tolerance::Above(SHOWN),
        )?;
        checks.compare(
            percent,
            &format!("grey patch {index}: mean kept"),
            after.luminance(),
            before.luminance(),
            Tolerance::Within(MEAN_KEPT),
        )?;
        for (channel, name) in [(0, "red"), (2, "blue")] {
            checks.compare(
                percent,
                &format!("grey patch {index}: no {name} cast"),
                after.mean[channel] - after.mean[1],
                before.mean[channel] - before.mean[1],
                Tolerance::Within(NO_CAST),
            )?;
        }
    }
    let deviations = grey.iter().map(|r| r.deviation);
    let (weakest, strongest) = (
        deviations.clone().fold(f64::INFINITY, f64::min),
        deviations.fold(0.0, f64::max),
    );
    checks.compare(
        percent,
        "grain even across the view's edges and centre",
        strongest,
        weakest,
        Tolerance::AtMost(EVEN * weakest),
    )?;
    for ((name, _), (after, before)) in GRAIN_PATCHES.iter().zip(patches.iter().zip(&base_band)) {
        if matches!(*name, "black" | "white") {
            checks.compare(
                percent,
                &format!("{name} untouched: mean"),
                after.luminance(),
                before.luminance(),
                Tolerance::Within(UNTOUCHED),
            )?;
            checks.compare(
                percent,
                &format!("{name} untouched: deviation"),
                after.deviation,
                before.deviation,
                Tolerance::Within(UNTOUCHED),
            )?;
        } else {
            for (channel, mean) in after.mean.iter().enumerate() {
                checks.compare(
                    percent,
                    &format!("{name} channel {channel} mean kept"),
                    *mean,
                    before.mean[channel],
                    Tolerance::Within(MEAN_KEPT),
                )?;
            }
        }
    }
    checks.note(
        percent,
        "Amount 60 at 100%",
        json!({"grey": records(&grey), "band": records(&patches), "drawing": drawing(percent)}),
    );

    // Size 80 coarsens the grain: neighbouring pixels correlate more.
    let size = launch.at("size")?;
    let coarse = greys(size)?;
    checks.compare(
        size,
        "Size 80's grain coarser than Size 25's",
        mean_of(&coarse, |r| r.correlation),
        mean_of(&grey, |r| r.correlation),
        Tolerance::Above(COARSER),
    )?;
    checks.note(size, "Size 80 at 100%", json!({"grey": records(&coarse)}));

    // Roughness 90 changes the pattern.
    let roughness = launch.at("roughness")?;
    let rough = greys(roughness)?;
    checks.compare(
        roughness,
        "Roughness 90 changes the grain",
        grey_difference(size, roughness)?,
        0.0,
        Tolerance::Above(ROUGHER),
    )?;
    checks.note(
        roughness,
        "Roughness 90 at 100%",
        json!({"grey": records(&rough), "drawing": drawing(roughness)}),
    );

    // The crop precedes the grain and leaves its size and strength where they were.
    let cropped = launch.at("cropped")?;
    let effects = layer_effects(cropped);
    let position = |effect: &str| effects.iter().position(|e| e == effect);
    ensure(
        position(CROP_EFFECT)
            .is_some_and(|crop| position(GRAIN_EFFECT).is_some_and(|grain| crop < grain)),
        format!("The crop layer does not precede the Grain layer: {effects:?}"),
    )?;
    let cropped_percent = launch.at("cropped-percent")?;
    let cropped_grey = greys(cropped_percent)?;
    same_grain(
        &mut checks,
        cropped_percent,
        "the 1:1 crop at 100%",
        &cropped_grey,
        &rough,
    )?;

    // Undo takes the crop off and redo puts it back, each with its state's grain.
    let undo = launch.at("undo")?;
    ensure(
        !layer_effects(undo).contains(&CROP_EFFECT.to_owned()),
        "Undo left the crop in the stack",
    )?;
    same_grain(&mut checks, undo, "undo at 100%", &greys(undo)?, &rough)?;
    let redo = launch.at("redo")?;
    ensure(
        layer_effects(redo).contains(&CROP_EFFECT.to_owned()),
        "Redo did not put the crop back",
    )?;
    same_grain(
        &mut checks,
        redo,
        "redo at 100%",
        &greys(redo)?,
        &cropped_grey,
    )?;
    let redone = launch.at("fit-redone")?;
    checks.note(
        redone,
        "the redone crop at Fit, a reduced stage",
        json!({"grey": records(&greys(redone)?), "drawing": drawing(redone)}),
    );

    checks.write(
        &launch.evidence,
        "grain",
        json!({
            "thresholds": {"shown": SHOWN, "mean_kept": MEAN_KEPT, "no_cast": NO_CAST, "untouched": UNTOUCHED, "even": EVEN, "coarser": COARSER, "same_grain": SAME_GRAIN, "rougher": ROUGHER},
            "sample_geometry": {"grey_at": GREY_AT, "grey_half": GREY_HALF, "patch_inset": PATCH_INSET},
            "scope": "8-bit captures of the photograph the editor records drawing, read in capture pixels at 100% against the same view before any grain; grain's statistics, not its reference values, which the GPU qualification and unit tests hold",
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_is_valid() {
        plan(&[]).validate().unwrap();
    }
}
