//! The Basic and histogram chapter of `cargo xtask editor-acceptance`.
//!
//! Everything here is driven through the JSON method table with [`OwnerHandle::call`], exactly as
//! an independent client reaches it: `catalog.import`, `edit.set-basic`, `draft.*`, `render.sample`,
//! `analysis.*`, `history.*` and `recipe.describe`. No desktop, no window and no pointer.
//!
//! The oracle is the independent f64 reference crate `lightwell-reference`, the one the core's own
//! numerical tests use, so the acceptance journey and those tests check production against one
//! written-from-the-formulas implementation that cannot depend on the core it checks. This file
//! composes the frozen unit order — white balance, exposure, tone, vibrance, saturation —
//! quantizes once at the output boundary, and implements the crop spec's rotated-box mapping and
//! linear-light bilinear sampler from `docs/specs/single-image.md` so a mixed stack is checked
//! stepwise rather than against itself.
//! The histogram reduction below is a plain serial loop written here, not `analysis::reduce`, so a
//! reported count is compared with a second implementation.
use crate::*;
use lightwell_core::{
    BASIC_EFFECT, ClientId, ModuleRegistry, OwnerHandle, RECIPE_FORMAT, Recipe, SnapshotId,
    SourceImage, render as core_render,
};
use lightwell_reference as reference;
use lightwell_testkit::client::{self, analyse, as_str, call};
use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

/// The fixture this chapter runs on: the 480x320 synthetic quadrant pattern with a white centre
/// line at code 255 and a band of black dashes at code 0, so clipped and unclipped populations are
/// both present before any edit.
pub(crate) const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";

/// Output codes may differ from the f64 reference by at most one, the tolerance frozen by the
/// numerical tasks for every Basic unit (`docs/design/basic-and-histogram.md`, "Numerical and
/// color contract").
const CODE_TOLERANCE: i32 = 1;

/// A resampled pixel may differ by one further code: the crop spec's own declared tolerance for the
/// linear-light bilinear sampler, on top of the Basic stage that fed it.
const RESAMPLE_TOLERANCE: i32 = 2;

/// Who this chapter's mutations and imports name.
const ACTOR: &str = "xtask-basic-acceptance";

/// The mutation envelope every asset change of the acceptance chapters carries, with the request
/// identity the caller chose, so a deliberate retry can reuse it.
pub(crate) fn mutation(revision: u64, request: &str) -> Value {
    client::mutation(revision, request, ACTOR)
}

/// Import one file through the source job an independent client waits on, answering the asset.
pub(crate) fn import(owner: &OwnerHandle, client: ClientId, path: &Path) -> Result<Value> {
    Ok(client::import(owner, client, path, ACTOR)?)
}

// ---------------------------------------------------------------------------------------------
// The independent oracle: the frozen unit order in f64, and the crop spec's sampler.
// ---------------------------------------------------------------------------------------------

/// The ten Basic fields, as the reference evaluates them. Every field is neutral at zero, so
/// `Basic::default()` is the identity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Basic {
    pub temperature: f64,
    pub tint: f64,
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub vibrance: f64,
    pub saturation: f64,
}

impl Basic {
    /// The patch a client sends for exactly these values: every field named, which is also what a
    /// module reset or a full-panel edit looks like on the wire.
    fn patch(&self) -> Value {
        json!({
            "temperature": self.temperature,
            "tint": self.tint,
            "exposure": self.exposure,
            "contrast": self.contrast,
            "highlights": self.highlights,
            "shadows": self.shadows,
            "whites": self.whites,
            "blacks": self.blacks,
            "vibrance": self.vibrance,
            "saturation": self.saturation,
        })
    }

    /// One pixel through the frozen internal order, quantized once at the output boundary. Nothing
    /// clamps between units, matching the design's "preserve finite values outside [0,1]" rule.
    fn pixel(&self, rgb: [u8; 3]) -> [u8; 3] {
        let mut channels = [
            reference::srgb_to_linear(rgb[0]),
            reference::srgb_to_linear(rgb[1]),
            reference::srgb_to_linear(rgb[2]),
        ];
        channels = reference::white_balance::apply(self.temperature, self.tint, channels);
        for channel in &mut channels {
            *channel = reference::exposure(*channel, self.exposure);
        }
        channels = reference::tone::tone_pixel(
            channels,
            reference::tone::ToneParams {
                contrast: self.contrast,
                highlights: self.highlights,
                shadows: self.shadows,
                whites: self.whites,
                blacks: self.blacks,
            },
        );
        channels = reference::colour::apply_basic_colour(channels, self.vibrance, self.saturation);
        [
            reference::linear_to_srgb_code(channels[0]),
            reference::linear_to_srgb_code(channels[1]),
            reference::linear_to_srgb_code(channels[2]),
        ]
    }

    /// The whole stage this layer produces: every pixel through [`Self::pixel`], alpha untouched.
    fn raster(&self, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(rgba.len());
        for pixel in rgba.chunks_exact(4) {
            let coded = self.pixel([pixel[0], pixel[1], pixel[2]]);
            out.extend_from_slice(&[coded[0], coded[1], coded[2], pixel[3]]);
        }
        out
    }
}

/// The largest per-channel difference between two RGBA buffers of the same size, and where it is.
fn largest_difference(a: &[u8], b: &[u8]) -> Result<(i32, usize)> {
    ensure(
        a.len() == b.len(),
        format!("Buffers differ in size: {} against {}", a.len(), b.len()),
    )?;
    let mut worst = 0i32;
    let mut at = 0usize;
    for (index, (left, right)) in a.iter().zip(b).enumerate() {
        // Alpha is compared too: the supported JPEG path is opaque and must stay so.
        let difference = i32::from(*left) - i32::from(*right);
        if difference.abs() > worst {
            worst = difference.abs();
            at = index;
        }
    }
    Ok((worst, at / 4))
}

/// The crop spec's rotated box for one input stage, in f64, written from
/// `docs/specs/single-image.md` rather than from the production geometry.
struct Box {
    width: f64,
    height: f64,
    input_width: f64,
    input_height: f64,
    cos: f64,
    sin: f64,
}

impl Box {
    fn new(input_width: u32, input_height: u32, angle_degrees: f64) -> Self {
        let radians = angle_degrees.to_radians();
        let (sin, cos) = radians.sin_cos();
        let (w, h) = (f64::from(input_width), f64::from(input_height));
        Self {
            width: w * cos.abs() + h * sin.abs(),
            height: w * sin.abs() + h * cos.abs(),
            input_width: w,
            input_height: h,
            cos,
            sin,
        }
    }

    /// box -> input, exactly the spec's inverse mapping.
    fn to_input(&self, x: f64, y: f64) -> (f64, f64) {
        let dx = x - self.width / 2.0;
        let dy = y - self.height / 2.0;
        (
            self.cos * dx + self.sin * dy + self.input_width / 2.0,
            -self.sin * dx + self.cos * dy + self.input_height / 2.0,
        )
    }
}

/// One channel of one pixel of an RGBA raster, with indices clamped to the edge.
fn at(rgba: &[u8], width: u32, height: u32, x: i64, y: i64, channel: usize) -> u8 {
    let x = x.clamp(0, i64::from(width) - 1) as usize;
    let y = y.clamp(0, i64::from(height) - 1) as usize;
    rgba[(y * width as usize + x) * 4 + channel]
}

/// The crop spec's sampler in f64: output pixel centres mapped back through the rotated box, then
/// bilinear interpolation with the colour channels blended in linear light and alpha blended
/// linearly. This is the stepwise reference a mixed stack is proved against; it consumes an
/// already-quantized stage, which is the spec's "a resample quantizes before it interpolates" rule.
fn resample(stage: &[u8], width: u32, height: u32, payload: &Value) -> Result<(u32, u32, Vec<u8>)> {
    let number = |key: &str| -> Result<f64> {
        payload[key]
            .as_f64()
            .ok_or_else(|| format!("The crop payload has no {key}: {payload}").into())
    };
    let angle = number("angle")?;
    let rect = Box::new(width, height, angle);
    let ox = (number("x")? * rect.width).round();
    let oy = (number("y")? * rect.height).round();
    let ow = (number("width")? * rect.width).round().max(1.0);
    let oh = (number("height")? * rect.height).round().max(1.0);
    let (out_w, out_h) = (ow as u32, oh as u32);
    let mut out = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
    for j in 0..out_h {
        for i in 0..out_w {
            let (u, v) = rect.to_input(ox + f64::from(i) + 0.5, oy + f64::from(j) + 0.5);
            // Pixel-center convention: index coordinates are the mapped centre minus a half pixel.
            let (fx, fy) = (u - 0.5, v - 0.5);
            let (x0, y0) = (fx.floor(), fy.floor());
            let (tx, ty) = (fx - x0, fy - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let weights = [
                (x0, y0, (1.0 - tx) * (1.0 - ty)),
                (x0 + 1, y0, tx * (1.0 - ty)),
                (x0, y0 + 1, (1.0 - tx) * ty),
                (x0 + 1, y0 + 1, tx * ty),
            ];
            let base = ((j as usize) * (out_w as usize) + i as usize) * 4;
            for channel in 0..3 {
                let mut linear = 0.0;
                for (x, y, weight) in weights {
                    linear +=
                        weight * reference::srgb_to_linear(at(stage, width, height, x, y, channel));
                }
                out[base + channel] = reference::linear_to_srgb_code(linear);
            }
            let mut alpha = 0.0;
            for (x, y, weight) in weights {
                alpha += weight * f64::from(at(stage, width, height, x, y, 3));
            }
            out[base + 3] = alpha.round().clamp(0.0, 255.0) as u8;
        }
    }
    Ok((out_w, out_h, out))
}

/// An exact quarter turn to the right, the mapping the orientation layer holds. Written here so a
/// Basic layer under an orientation layer is checked against composed arithmetic, not against the
/// renderer's own transform.
fn rotate_right(rgba: &[u8], width: u32, height: u32) -> (u32, u32, Vec<u8>) {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = (h - 1 - y, x);
            let from = (y * w + x) * 4;
            let to = (ny * h + nx) * 4;
            out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    (height, width, out)
}

// ---------------------------------------------------------------------------------------------
// The independent reduction: a plain serial loop, not `analysis::reduce`.
// ---------------------------------------------------------------------------------------------

struct Counts {
    r: [u64; 256],
    g: [u64; 256],
    b: [u64; 256],
    r0: u64,
    g0: u64,
    b0: u64,
    r255: u64,
    g255: u64,
    b255: u64,
    any_shadow: u64,
    any_highlight: u64,
    all_shadow: u64,
    all_highlight: u64,
    both: u64,
    width: u32,
    height: u32,
}

fn reduce(rgba: &[u8], width: u32, height: u32) -> Counts {
    let mut counts = Counts {
        r: [0; 256],
        g: [0; 256],
        b: [0; 256],
        r0: 0,
        g0: 0,
        b0: 0,
        r255: 0,
        g255: 0,
        b255: 0,
        any_shadow: 0,
        any_highlight: 0,
        all_shadow: 0,
        all_highlight: 0,
        both: 0,
        width,
        height,
    };
    for pixel in rgba.chunks_exact(4) {
        let (r, g, b) = (pixel[0], pixel[1], pixel[2]);
        counts.r[usize::from(r)] += 1;
        counts.g[usize::from(g)] += 1;
        counts.b[usize::from(b)] += 1;
        counts.r0 += u64::from(r == 0);
        counts.g0 += u64::from(g == 0);
        counts.b0 += u64::from(b == 0);
        counts.r255 += u64::from(r == 255);
        counts.g255 += u64::from(g == 255);
        counts.b255 += u64::from(b == 255);
        let shadow = r == 0 || g == 0 || b == 0;
        let highlight = r == 255 || g == 255 || b == 255;
        counts.any_shadow += u64::from(shadow);
        counts.any_highlight += u64::from(highlight);
        counts.all_shadow += u64::from(r == 0 && g == 0 && b == 0);
        counts.all_highlight += u64::from(r == 255 && g == 255 && b == 255);
        counts.both += u64::from(shadow && highlight);
    }
    counts
}

impl Counts {
    fn as_json(&self) -> Value {
        json!({
            "r": self.r.to_vec(), "g": self.g.to_vec(), "b": self.b.to_vec(),
            "r0": self.r0, "g0": self.g0, "b0": self.b0,
            "r255": self.r255, "g255": self.g255, "b255": self.b255,
            "any_shadow": self.any_shadow, "any_highlight": self.any_highlight,
            "all_shadow": self.all_shadow, "all_highlight": self.all_highlight,
            "both": self.both,
            "width": self.width, "height": self.height,
            "domain": "srgb-8bit-output",
        })
    }

    /// The counters alone, for the evidence record: the 768 bins are compared but not printed.
    fn counters(&self) -> Value {
        json!({
            "r0": self.r0, "g0": self.g0, "b0": self.b0,
            "r255": self.r255, "g255": self.g255, "b255": self.b255,
            "any_shadow": self.any_shadow, "any_highlight": self.any_highlight,
            "all_shadow": self.all_shadow, "all_highlight": self.all_highlight,
            "both": self.both, "pixels": u64::from(self.width) * u64::from(self.height),
        })
    }

    /// Every bin, every counter and the output stage, against a reported `analysis.read` report.
    fn expect(&self, report: &Value, what: &str) -> Result {
        let expected = self.as_json();
        for key in [
            "r",
            "g",
            "b",
            "r0",
            "g0",
            "b0",
            "r255",
            "g255",
            "b255",
            "any_shadow",
            "any_highlight",
            "all_shadow",
            "all_highlight",
            "both",
            "width",
            "height",
            "domain",
        ] {
            ensure(
                report.get(key) == expected.get(key),
                format!(
                    "{what}: the reported {key} is {}, the independent reduction says {}",
                    report.get(key).unwrap_or(&Value::Null),
                    expected.get(key).unwrap_or(&Value::Null)
                ),
            )?;
        }
        let total: u64 = self.r.iter().sum();
        ensure(
            total == u64::from(self.width) * u64::from(self.height),
            format!("{what}: the bins do not sum to the output pixel count"),
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Small journey helpers.
// ---------------------------------------------------------------------------------------------

/// Compare a reported effective-values object with the reference parameters numerically, so a whole
/// number written as `40` and as `40.0` is one value rather than two JSON shapes.
fn expect_values(values: &Value, expected: &Basic, what: &str) -> Result {
    let patch = expected.patch();
    let fields = patch.as_object().expect("a patch is an object");
    for (name, wanted) in fields {
        let reported = values.get(name).and_then(Value::as_f64);
        ensure(
            reported == wanted.as_f64(),
            format!(
                "{what}: {name} reads {reported:?}, expected {:?}",
                wanted.as_f64()
            ),
        )?;
    }
    ensure(
        values.as_object().map(serde_json::Map::len) == Some(fields.len()),
        format!("{what}: the reported values are {values}"),
    )
}

/// The one Basic layer of a described entry, with its identity, index and effective values.
fn described_basic(described: &Value) -> Result<(String, usize, Value)> {
    let layers = described["layers"]
        .as_array()
        .ok_or("recipe.describe answered no layers")?;
    let index = layers
        .iter()
        .position(|layer| layer["effect"] == json!(BASIC_EFFECT))
        .ok_or("The described entry holds no Basic layer")?;
    Ok((
        as_str(&layers[index]["id"], "layer id")?,
        index,
        layers[index]["values"].clone(),
    ))
}

/// Render one recipe in this process. The raster is the subject of the checks below, never the
/// oracle: every expected value comes from the f64 reference above.
pub(crate) fn render(source: &SourceImage, recipe: &Recipe) -> Result<lightwell_core::Raster> {
    let registry = ModuleRegistry::builtin();
    let context = lightwell_core::RenderContext::new();
    let options = lightwell_core::RenderOptions::default();
    Ok(core_render(&registry, source, recipe, options, &context)?.frame(SnapshotId::new())?)
}

/// An independent reduction of an independent render of one recipe. The counts a reduction
/// produces are exact integers over exact bytes, so they are compared with the rendered raster
/// rather than with the f64 reference: the raster itself is proved against that reference
/// separately, within the one output code the numerical contract allows, and a one-code difference
/// on a single pixel would move a bin without any count being wrong.
fn reduce_render(
    source: &SourceImage,
    recipe: &Recipe,
) -> Result<(Counts, lightwell_core::Raster)> {
    let raster = render(source, recipe)?;
    let counts = reduce(&raster.rgba, raster.width, raster.height);
    Ok((counts, raster))
}

/// A settled analysis that must be `ready`, answering its report.
pub(crate) fn ready_report(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &Value,
    target: Value,
    what: &str,
) -> Result<Value> {
    let read = analyse(owner, client, asset, target)?;
    ensure(
        read["status"] == json!("ready"),
        format!("{what}: the analysis settled as {}", read["status"]),
    )?;
    ensure(
        read["identity"] == read["requested_identity"],
        format!(
            "{what}: the read identity {} is not the requested one {}",
            read["identity"], read["requested_identity"]
        ),
    )?;
    Ok(read)
}

// ---------------------------------------------------------------------------------------------
// The chapter.
// ---------------------------------------------------------------------------------------------

pub fn run(root: &Path, out: &Path) -> Result<Value> {
    let fixture = root.join(FIXTURE);
    let fixture_hash = hash(&fixture)?;
    let catalog = out.join("basic-catalog.sqlite");
    let source = lightwell_core::open_source(&fixture)?;
    let (width, height) = (source.width, source.height);
    let checks: RefCell<Vec<Value>> = RefCell::new(Vec::new());
    let record = |shows: &str, detail: Value| {
        checks
            .borrow_mut()
            .push(json!({"shows": shows, "detail": detail}))
    };
    let total = Instant::now();

    let (owner, join) = OwnerHandle::start(&catalog)?;
    let mut join = Some(join);
    let outcome = (|| -> Result<Value> {
        let editor = owner.register();
        let agent = owner.register();

        // 1. Discovery of what only this chapter uses: the neutral picker's query and the analysis
        //    methods, and Basic's own field table in its declared order. The generic discovery of
        //    `edit.set-basic`, `edit.reset-basic` and the draft, sample and history methods is the
        //    field-patch conformance suite's.
        let schema = call(&owner, editor, "schema.list", json!({}))?;
        let methods = &schema["methods"];
        for method in [
            "query.neutral-sample",
            "analysis.request",
            "analysis.read",
            "analysis.cancel",
        ] {
            ensure(
                methods.get(method).is_some(),
                format!("{method} is not discoverable"),
            )?;
        }
        let declared: Vec<String> = methods["edit.set-basic"]["parameters"]
            .as_array()
            .ok_or("edit.set-basic declares no parameters")?
            .iter()
            .map(|parameter| as_str(&parameter["name"], "parameter name"))
            .collect::<client::Checked<Vec<_>>>()?;
        ensure(
            declared
                == [
                    "temperature",
                    "tint",
                    "exposure",
                    "contrast",
                    "highlights",
                    "shadows",
                    "whites",
                    "blacks",
                    "vibrance",
                    "saturation",
                ],
            format!("edit.set-basic declares {declared:?}"),
        )?;
        record(
            "the neutral picker's query and the analysis methods are discoverable, and set-basic declares Basic's ten fields in their frozen order",
            json!({"parameters": declared}),
        );

        // 2. Import.
        let imported = import(&owner, editor, &fixture)?;
        let asset = imported["asset"]["id"].clone();
        let original = imported["current_entry"]["id"].clone();
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        ensure(
            described["layers"].as_array().is_some_and(Vec::is_empty),
            "The imported asset already holds layers",
        )?;

        // 3. Exposure +1 EV through the patch action. The rendered bytes and `render.sample` are
        //    both checked against the f64 reference, and against each other.
        let plus_one = Basic {
            exposure: 1.0,
            ..Basic::default()
        };
        let applied = call(
            &owner,
            editor,
            "edit.set-basic",
            json!({"asset_id": asset, "mutation": mutation(0, "exposure-plus-one"), "exposure": 1.0}),
        )?;
        ensure(
            applied["outcome"] == json!("applied") && applied["revision"] == json!(1),
            format!("The first Basic edit answered {applied}"),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let (basic_layer, basic_index, values) = described_basic(&described)?;
        expect_values(&values, &plus_one, "the exposure layer")?;
        ensure(
            basic_index == 0,
            "The one Basic layer is not the first layer",
        )?;
        let exposed = render(&source, &client::recipe(&owner, editor, &asset)?)?;
        let expected = plus_one.raster(&source.rgba);
        let (difference, at_pixel) = largest_difference(&exposed.rgba, &expected)?;
        ensure(
            difference <= CODE_TOLERANCE,
            format!(
                "+1.00 EV differs from the f64 reference by {difference} codes at pixel {at_pixel}"
            ),
        )?;
        // The API's own pixel access must answer the same bytes the raster holds.
        let probes = [(0u32, 0u32), (120, 60), (240, 200), (60, 160), (479, 319)];
        let mut sampled = Vec::new();
        for (x, y) in probes {
            let sample = call(
                &owner,
                editor,
                "render.sample",
                json!({"asset_id": asset, "x": x, "y": y}),
            )?;
            let raster_pixel = exposed.pixel(x, y).ok_or("The raster has no such pixel")?;
            ensure(
                sample["rgba"] == json!(raster_pixel),
                format!(
                    "render.sample at {x},{y} answered {} while the raster holds {raster_pixel:?}",
                    sample["rgba"]
                ),
            )?;
            let index = ((y as usize) * width as usize + x as usize) * 4;
            let reference_pixel = &expected[index..index + 4];
            for channel in 0..4 {
                ensure(
                    (i32::from(raster_pixel[channel]) - i32::from(reference_pixel[channel])).abs()
                        <= CODE_TOLERANCE,
                    format!(
                        "Pixel {x},{y} channel {channel} is {} against the reference {}",
                        raster_pixel[channel], reference_pixel[channel]
                    ),
                )?;
            }
            sampled
                .push(json!({"pixel":[x,y],"rendered":raster_pixel,"reference":reference_pixel}));
        }
        record(
            "edit.set-basic exposure +1.00 EV: the whole raster within one code of the f64 reference, render.sample byte-identical to it",
            json!({"largest_code_difference": difference, "probes": sampled, "layer": basic_layer, "layer_index": basic_index}),
        );

        // 4. The full patch: every remaining field in one request, merged over the same layer.
        let full = Basic {
            temperature: 30.0,
            tint: -20.0,
            exposure: 1.0,
            contrast: 25.0,
            highlights: -40.0,
            shadows: 35.0,
            whites: 15.0,
            blacks: -30.0,
            vibrance: 45.0,
            saturation: -25.0,
        };
        let mut params = full.patch();
        params["asset_id"] = asset.clone();
        params["mutation"] = mutation(1, "full-patch");
        let patched = call(&owner, editor, "edit.set-basic", params)?;
        ensure(
            patched["outcome"] == json!("applied"),
            format!("The full patch answered {patched}"),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let (same_layer, _, values) = described_basic(&described)?;
        ensure(
            same_layer == basic_layer,
            "The full patch replaced the Basic layer instead of updating it",
        )?;
        expect_values(&values, &full, "the merged layer")?;
        let composed = render(&source, &client::recipe(&owner, editor, &asset)?)?;
        let expected_full = full.raster(&source.rgba);
        let (difference, at_pixel) = largest_difference(&composed.rgba, &expected_full)?;
        ensure(
            difference <= CODE_TOLERANCE,
            format!(
                "The full Basic stack differs from the f64 reference by {difference} codes at pixel {at_pixel}"
            ),
        )?;
        let mut full_probes = Vec::new();
        for (x, y) in probes {
            let sample = call(
                &owner,
                editor,
                "render.sample",
                json!({"asset_id": asset, "x": x, "y": y}),
            )?;
            let raster_pixel = composed.pixel(x, y).ok_or("The raster has no such pixel")?;
            ensure(
                sample["rgba"] == json!(raster_pixel),
                format!("render.sample disagreed with the raster at {x},{y}"),
            )?;
            full_probes.push(json!({"pixel":[x,y],"rendered":raster_pixel}));
        }
        record(
            "a nine-field patch merged onto the same layer: the frozen unit order within one code of the f64 reference over every pixel",
            json!({"largest_code_difference": difference, "values": values, "probes": full_probes}),
        );

        // 5. The histogram of an open draft: the analysis of the drafted stack, identified by the
        //    draft revision it read, equals an independent reduction of that stack's own render,
        //    which is within one code of the f64 reference. The draft lifecycle itself — begin, set,
        //    cancel, and a commit that renders what the draft previewed — is the field-patch
        //    conformance suite's.
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-basic"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        for value in [0.5, 1.5, 2.0] {
            call(
                &owner,
                editor,
                "draft.set",
                json!({"draft_id": draft_id, "fields": {"exposure": value}}),
            )?;
        }
        let drafted_analysis = ready_report(
            &owner,
            editor,
            &asset,
            json!({"kind":"draft","draft_id":draft_id}),
            "the open draft",
        )?;
        // The draft's effective recipe is the committed stack with this gesture's field merged in,
        // computed here from what the API reported rather than taken from the owner.
        let drafted_basic = Basic {
            exposure: 2.0,
            ..full
        };
        let mut drafted_recipe = client::recipe(&owner, editor, &asset)?;
        for layer in &mut drafted_recipe.layers {
            if layer.effect_id == BASIC_EFFECT {
                layer.payload = drafted_basic.patch();
            }
        }
        let (drafted_counts, drafted_raster) = reduce_render(&source, &drafted_recipe)?;
        let (drafted_difference, _) =
            largest_difference(&drafted_raster.rgba, &drafted_basic.raster(&source.rgba))?;
        ensure(
            drafted_difference <= CODE_TOLERANCE,
            format!(
                "The drafted render differs from the f64 reference by {drafted_difference} codes"
            ),
        )?;
        drafted_counts.expect(&drafted_analysis["report"], "the drafted analysis")?;
        ensure(
            drafted_analysis["identity"]["draft"]["draft_revision"] == json!(3),
            format!(
                "The drafted identity carries {}",
                drafted_analysis["identity"]["draft"]
            ),
        )?;
        call(
            &owner,
            editor,
            "draft.cancel",
            json!({"draft_id": draft_id}),
        )?;
        record(
            "the analysis of an open draft, identified by its draft revision, matches an independent reduction of the drafted stack's render, which is within one code of the f64 reference",
            json!({"draft_revision": 3, "drafted_counters": drafted_counts.counters()}),
        );

        // Back to a neutral layer: the analysis sections below state their stacks from it.
        call(
            &owner,
            editor,
            "edit.reset-basic",
            json!({"asset_id": asset, "mutation": mutation(client::revision(&owner, editor, &asset)?, "reset-before-analysis")}),
        )?;

        // 9. Analysis of the current stack and of a historical entry, each against an independent
        //    reduction of that entry's own render.
        let revision = client::revision(&owner, editor, &asset)?;
        let contrasted = Basic {
            contrast: 60.0,
            ..Basic::default()
        };
        call(
            &owner,
            editor,
            "edit.set-basic",
            json!({"asset_id": asset, "mutation": mutation(revision, "contrast-60"), "contrast": 60.0}),
        )?;
        let current_report = ready_report(
            &owner,
            editor,
            &asset,
            json!({"kind": "current"}),
            "the current stack",
        )?;
        let (current_counts, current_raster) =
            reduce_render(&source, &client::recipe(&owner, editor, &asset)?)?;
        let (contrast_difference, _) =
            largest_difference(&current_raster.rgba, &contrasted.raster(&source.rgba))?;
        ensure(
            contrast_difference <= CODE_TOLERANCE,
            format!("Contrast +60 differs from the f64 reference by {contrast_difference} codes"),
        )?;
        current_counts.expect(&current_report["report"], "the current stack")?;
        let original_report = ready_report(
            &owner,
            editor,
            &asset,
            json!({"kind": "entry", "entry_id": original}),
            "the Original entry",
        )?;
        let (original_counts, original_raster) = reduce_render(
            &source,
            &Recipe {
                format: RECIPE_FORMAT,
                layers: Vec::new(),
                masks: Vec::new(),
                ..Recipe::default()
            },
        )?;
        ensure(
            original_raster.rgba[..] == source.rgba[..],
            "The Original entry does not render the decoded source bytes",
        )?;
        original_counts.expect(&original_report["report"], "the Original entry")?;
        ensure(
            current_report["identity"]["recipe_hash"] != original_report["identity"]["recipe_hash"],
            "Two different stacks share one recipe hash",
        )?;
        record(
            "analysis.request/read on the current stack and on a historical entry: every bin and counter equals an independent reduction of that entry's own render",
            json!({"current": current_counts.counters(), "original": original_counts.counters()}),
        );

        // 10. The cropped population: a Basic layer that clips the border, and a crop that removes
        //     it. The counts follow the composition after crop, so the clipped border is gone.
        let revision = client::revision(&owner, editor, &asset)?;
        let clipping = Basic {
            exposure: 4.0,
            ..Basic::default()
        };
        let mut params = clipping.patch();
        params["asset_id"] = asset.clone();
        params["mutation"] = mutation(revision, "clip-the-border");
        call(&owner, editor, "edit.set-basic", params)?;
        let full_report = ready_report(
            &owner,
            editor,
            &asset,
            json!({"kind": "current"}),
            "the clipping stack before the crop",
        )?;
        let (full_counts, clipped_raster) =
            reduce_render(&source, &client::recipe(&owner, editor, &asset)?)?;
        let (clipping_difference, _) =
            largest_difference(&clipped_raster.rgba, &clipping.raster(&source.rgba))?;
        ensure(
            clipping_difference <= CODE_TOLERANCE,
            format!(
                "The clipping stage differs from the f64 reference by {clipping_difference} codes"
            ),
        )?;
        let clipped_stage = clipped_raster.rgba.to_vec();
        full_counts.expect(&full_report["report"], "the clipping stack before the crop")?;
        ensure(
            full_counts.any_highlight > 0,
            "The exposure chosen to clip the border clipped nothing",
        )?;
        // An angle-zero crop that removes a 40 px border ring is an exact copy of the sub-rectangle.
        let margin = 40u32;
        let crop = json!({
            "angle": 0.0,
            "x": f64::from(margin) / f64::from(width),
            "y": f64::from(margin) / f64::from(height),
            "width": f64::from(width - 2 * margin) / f64::from(width),
            "height": f64::from(height - 2 * margin) / f64::from(height),
        });
        let revision = client::revision(&owner, editor, &asset)?;
        let mut params = crop.clone();
        params["asset_id"] = asset.clone();
        params["mutation"] = mutation(revision, "crop-the-border-away");
        call(&owner, editor, "edit.crop", params)?;
        let (crop_w, crop_h) = (width - 2 * margin, height - 2 * margin);
        let mut interior = Vec::with_capacity((crop_w * crop_h * 4) as usize);
        for y in margin..height - margin {
            let start = ((y * width + margin) * 4) as usize;
            interior.extend_from_slice(&clipped_stage[start..start + (crop_w * 4) as usize]);
        }
        let interior_counts = reduce(&interior, crop_w, crop_h);
        let cropped_report = ready_report(
            &owner,
            editor,
            &asset,
            json!({"kind": "current"}),
            "the cropped clipping stack",
        )?;
        interior_counts.expect(&cropped_report["report"], "the cropped clipping stack")?;
        // The border's own clipped pixels are exactly the difference between the two populations,
        // which is the hand-counted `cropped-population` pair's semantics on a photo-sized stack.
        let border_highlight = full_counts.any_highlight - interior_counts.any_highlight;
        ensure(
            border_highlight > 0 && interior_counts.width == crop_w,
            format!("The crop excluded {border_highlight} clipped border pixels"),
        )?;
        record(
            "a crop excludes the clipped border from the counts: the cropped population is the interior alone, exactly",
            json!({
                "before_crop": full_counts.counters(),
                "after_crop": interior_counts.counters(),
                "border_highlight_pixels_excluded": border_highlight,
                "fixture_pair": "fixtures/basic/cropped-population-full-6x4.json and cropped-population-crop-4x2.json state the same rule on hand-counted buffers",
            }),
        );

        // 11. Mixed stacks. Back to the Original first, so each stack is built deliberately.
        let restore = call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset, "mutation": mutation(client::revision(&owner, editor, &asset)?, "restore-original"), "entry_id": original}),
        )?;
        ensure(
            restore["outcome"] == json!("applied"),
            format!("The restore answered {restore}"),
        )?;
        ensure(
            client::recipe(&owner, editor, &asset)?.layers.is_empty(),
            "Restoring the Original left layers behind",
        )?;

        // 11a. Basic then a straightened 10 degree crop, against quantize-then-bilinear.
        let straighten = Basic {
            exposure: 0.75,
            saturation: 30.0,
            ..Basic::default()
        };
        let revision = client::revision(&owner, editor, &asset)?;
        let mut params = straighten.patch();
        params["asset_id"] = asset.clone();
        params["mutation"] = mutation(revision, "basic-before-crop");
        call(&owner, editor, "edit.set-basic", params)?;
        let basic_stage = straighten.raster(&source.rgba);
        let stage_render = render(&source, &client::recipe(&owner, editor, &asset)?)?;
        let (stage_difference, _) = largest_difference(&stage_render.rgba, &basic_stage)?;
        ensure(
            stage_difference <= CODE_TOLERANCE,
            format!("The pre-crop Basic stage differs by {stage_difference} codes"),
        )?;
        let revision = client::revision(&owner, editor, &asset)?;
        call(
            &owner,
            editor,
            "edit.crop-fit",
            json!({"asset_id": asset, "mutation": mutation(revision, "straighten-10"), "aspect": "16:9", "angle": 10.0}),
        )?;
        let recipe = client::recipe(&owner, editor, &asset)?;
        let order: Vec<String> = recipe
            .layers
            .iter()
            .map(|layer| layer.effect_id.clone())
            .collect();
        ensure(
            order == [BASIC_EFFECT, lightwell_core::CROP_EFFECT],
            format!("The mixed stack is ordered {order:?}"),
        )?;
        let crop_payload = recipe
            .layers
            .iter()
            .find(|layer| layer.effect_id == lightwell_core::CROP_EFFECT)
            .map(|layer| layer.payload.clone())
            .ok_or("The straightened stack holds no crop layer")?;
        let straightened = render(&source, &recipe)?;
        // Stepwise: the f64 Basic stage quantized, then the crop spec's linear-light bilinear.
        let (rw, rh, from_reference) = resample(&basic_stage, width, height, &crop_payload)?;
        ensure(
            (rw, rh) == (straightened.width, straightened.height),
            format!(
                "The reference crop is {rw}x{rh}, the render is {}x{}",
                straightened.width, straightened.height
            ),
        )?;
        let (mixed_difference, mixed_at) = largest_difference(&straightened.rgba, &from_reference)?;
        ensure(
            mixed_difference <= RESAMPLE_TOLERANCE,
            format!(
                "The straightened mixed stack differs from the stepwise reference by {mixed_difference} codes at pixel {mixed_at}"
            ),
        )?;
        // The sampler alone, fed the production stage, isolates the resample to its own tolerance.
        let (_, _, from_production) = resample(&stage_render.rgba, width, height, &crop_payload)?;
        let (resample_difference, _) = largest_difference(&straightened.rgba, &from_production)?;
        ensure(
            resample_difference <= CODE_TOLERANCE,
            format!("The resample alone differs by {resample_difference} codes"),
        )?;
        record(
            "Basic before a straightened 10 degree crop: quantize-then-bilinear, stepwise, over every output pixel",
            json!({
                "stack": order,
                "output": [rw, rh],
                "largest_difference_from_stepwise_reference": mixed_difference,
                "largest_difference_of_the_resample_alone": resample_difference,
                "crop_payload": crop_payload,
            }),
        );

        // 11b. A point replacement before and after the Basic layer (mixed-order.json semantics).
        call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset, "mutation": mutation(client::revision(&owner, editor, &asset)?, "restore-for-pixels"), "entry_id": original}),
        )?;
        let before_point = (10u32, 10u32);
        let after_point = (20u32, 20u32);
        let literal = [10u8, 20, 30];
        let revision = client::revision(&owner, editor, &asset)?;
        call(
            &owner,
            editor,
            "edit.set-pixel",
            json!({"asset_id": asset, "mutation": mutation(revision, "pixel-before"), "x": before_point.0, "y": before_point.1, "rgb": literal}),
        )?;
        let exposure_only = Basic {
            exposure: 1.0,
            ..Basic::default()
        };
        let revision = client::revision(&owner, editor, &asset)?;
        call(
            &owner,
            editor,
            "edit.set-basic",
            json!({"asset_id": asset, "mutation": mutation(revision, "basic-between-pixels"), "exposure": 1.0}),
        )?;
        let revision = client::revision(&owner, editor, &asset)?;
        call(
            &owner,
            editor,
            "edit.set-pixel",
            json!({"asset_id": asset, "mutation": mutation(revision, "pixel-after"), "x": after_point.0, "y": after_point.1, "rgb": literal}),
        )?;
        let recipe = client::recipe(&owner, editor, &asset)?;
        let order: Vec<String> = recipe
            .layers
            .iter()
            .map(|layer| layer.effect_id.clone())
            .collect();
        ensure(
            order
                == [
                    lightwell_core::PIXEL_EFFECT,
                    BASIC_EFFECT,
                    lightwell_core::PIXEL_EFFECT,
                ],
            format!("The point-replacement stack is ordered {order:?}"),
        )?;
        let mixed_order = render(&source, &recipe)?;
        let exposed_literal = exposure_only.pixel(literal);
        let before_pixel = mixed_order
            .pixel(before_point.0, before_point.1)
            .ok_or("No pixel")?;
        let after_pixel = mixed_order
            .pixel(after_point.0, after_point.1)
            .ok_or("No pixel")?;
        for channel in 0..3 {
            ensure(
                (i32::from(before_pixel[channel]) - i32::from(exposed_literal[channel])).abs()
                    <= CODE_TOLERANCE,
                format!(
                    "A replacement before the Basic layer was not exposed: {before_pixel:?} against {exposed_literal:?}"
                ),
            )?;
        }
        ensure(
            [after_pixel[0], after_pixel[1], after_pixel[2]] == literal,
            format!("A replacement after the Basic layer was changed: {after_pixel:?}"),
        )?;
        record(
            "a point replacement before the Basic layer is processed by it and one after it is not, exactly as mixed-order.json states",
            json!({"literal": literal, "before": before_pixel, "exposed_literal": exposed_literal, "after": after_pixel, "stack": order}),
        );

        // 11c. Basic under an orientation layer: the exact transform is a pure permutation of the
        //      coloured stage, so the two compose without interpolation.
        call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset, "mutation": mutation(client::revision(&owner, editor, &asset)?, "restore-for-orientation"), "entry_id": original}),
        )?;
        let revision = client::revision(&owner, editor, &asset)?;
        call(
            &owner,
            editor,
            "edit.set-basic",
            json!({"asset_id": asset, "mutation": mutation(revision, "basic-under-orientation"), "exposure": -1.0, "vibrance": 50.0}),
        )?;
        let revision = client::revision(&owner, editor, &asset)?;
        call(
            &owner,
            editor,
            "edit.transform",
            json!({"asset_id": asset, "mutation": mutation(revision, "rotate-right"), "transform": "rotate-right"}),
        )?;
        let recipe = client::recipe(&owner, editor, &asset)?;
        let order: Vec<String> = recipe
            .layers
            .iter()
            .map(|layer| layer.effect_id.clone())
            .collect();
        ensure(
            order == [BASIC_EFFECT, lightwell_core::ORIENTATION_EFFECT],
            format!("The oriented stack is ordered {order:?}"),
        )?;
        let oriented = render(&source, &recipe)?;
        let under = Basic {
            exposure: -1.0,
            vibrance: 50.0,
            ..Basic::default()
        };
        let (ow, oh, expected_oriented) = rotate_right(&under.raster(&source.rgba), width, height);
        ensure(
            (ow, oh) == (oriented.width, oriented.height),
            format!(
                "The oriented render is {}x{}, expected {ow}x{oh}",
                oriented.width, oriented.height
            ),
        )?;
        let (difference, _) = largest_difference(&oriented.rgba, &expected_oriented)?;
        ensure(
            difference <= CODE_TOLERANCE,
            format!("Basic under an orientation layer differs by {difference} codes"),
        )?;
        record(
            "Basic under an orientation layer: the f64 stage rotated exactly, within one code over every pixel",
            json!({"stack": order, "output": [ow, oh], "largest_code_difference": difference}),
        );

        // 12b. A historical selection and its analysis stay attached to their entry while another
        //      client commits.
        call(
            &owner,
            editor,
            "preview.select",
            json!({"asset_id": asset, "entry_id": original}),
        )?;
        let selected_report = ready_report(
            &owner,
            editor,
            &asset,
            json!({"kind": "entry", "entry_id": original}),
            "the selected historical entry",
        )?;
        let selected_job = selected_report["job_id"].clone();
        let revision = client::revision(&owner, agent, &asset)?;
        call(
            &owner,
            agent,
            "edit.set-basic",
            json!({"asset_id": asset, "mutation": mutation(revision, "agent-during-preview"), "saturation": -60.0}),
        )?;
        let session = call(&owner, editor, "session.state", json!({}))?;
        ensure(
            session["preview"]["selection"] == json!({"entry": original}),
            format!("The selection moved to {}", session["preview"]["selection"]),
        )?;
        let reread = call(
            &owner,
            editor,
            "analysis.read",
            json!({"job_id": selected_job}),
        )?;
        ensure(
            reread["status"] == json!("ready")
                && reread["identity"] == selected_report["identity"]
                && reread["report"] == selected_report["report"],
            "The historical analysis was relabelled by another client's commit",
        )?;
        original_counts.expect(&reread["report"], "the historical analysis after a commit")?;
        let sample = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": asset, "x": 0, "y": 0}),
        )?;
        let source_first = [
            source.rgba[0],
            source.rgba[1],
            source.rgba[2],
            source.rgba[3],
        ];
        ensure(
            sample["rgba"] == json!(source_first),
            format!(
                "The previewed sample moved to the newest stack: {}",
                sample["rgba"]
            ),
        )?;
        record(
            "a historical selection and its analysis stay attached to that entry while another client commits",
            json!({"selection": session["preview"]["selection"], "identity": reread["identity"]}),
        );

        // 12c. Two clients share one job; one client's cancel leaves the other's result alone.
        call(&owner, editor, "preview.return-current", json!({}))?;
        let mine = call(
            &owner,
            editor,
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "entry", "entry_id": original}}),
        )?;
        let theirs = call(
            &owner,
            agent,
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "entry", "entry_id": original}}),
        )?;
        ensure(
            mine["job_id"] == theirs["job_id"],
            "Identical identities did not share one job",
        )?;
        let withdrawn = call(
            &owner,
            editor,
            "analysis.cancel",
            json!({"job_id": mine["job_id"]}),
        )?;
        ensure(
            withdrawn["cancelled"] == json!(true),
            format!("analysis.cancel answered {withdrawn}"),
        )?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let survivor = loop {
            let read = call(
                &owner,
                agent,
                "analysis.read",
                json!({"job_id": theirs["job_id"]}),
            )?;
            match read["status"].as_str() {
                Some("pending") => {
                    ensure(Instant::now() < deadline, "The surviving job never settled")?;
                    std::thread::sleep(Duration::from_millis(2));
                }
                _ => break read,
            }
        };
        ensure(
            survivor["status"] == json!("ready"),
            format!(
                "One client's cancel took the other's result: {}",
                survivor["status"]
            ),
        )?;
        original_counts.expect(&survivor["report"], "the surviving shared job")?;
        record(
            "two clients share one analysis job and one client's cancel leaves the other's result intact",
            json!({"job": mine["job_id"], "survivor_status": survivor["status"]}),
        );

        ensure(
            hash(&fixture)? == fixture_hash,
            "The original source changed",
        )?;
        Ok(json!({
            "status": "passed",
            "fixture": FIXTURE,
            "fixture_sha256": fixture_hash,
            "asset_id": asset,
            "original_entry_id": original,
            "basic_layer_id": basic_layer,
            "code_tolerance": CODE_TOLERANCE,
            "resample_tolerance": RESAMPLE_TOLERANCE,
            "oracle": "crates/lightwell-reference, which cannot depend on the core; the crop sampler and the histogram reduction are written here from docs/specs/single-image.md and the histogram contract",
            "generic": "the host behaviour Basic shares with every field-patch module (discovery, drafts, no-ops, deduplication, resets, one layer per target, history, sample equal to render, an unavailable provider and reopen) is proved under field_patch_conformance",
            "reused_unit_tests": [
                "lightwell_core::api::owner::tests::racing_requests_supersede_the_pending_job_and_withdrawal_releases_only_its_own_interest",
                "lightwell_core::api::owner::tests::two_clients_share_one_job_and_keep_independent_current_and_historical_results",
                "lightwell_core::api::owner::tests::a_draft_target_analyses_the_drafted_recipe_and_belongs_to_one_session",
                "lightwell_core::api::methods::tests::an_external_commit_conflicts_a_draft_and_reapply_keeps_only_this_clients_fields"
            ],
            "checks": checks.borrow().clone(),
            "elapsed_ms": total.elapsed().as_secs_f64() * 1000.0,
        }))
    })();
    // A failure before the journey hands the catalog on must still release the first owner.
    if let Some(join) = join.take() {
        owner.stop();
        let _ = join.join();
    }
    outcome
}
