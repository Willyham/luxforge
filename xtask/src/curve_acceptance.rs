//! The Tone curve chapter of `cargo xtask editor-acceptance`: what the curve does that no other
//! module does, driven through the JSON method table with [`OwnerHandle::call`] exactly as an
//! independent client reaches it — its place after Basic and before the colour mixer in every touch
//! order with identical bytes, masked curve layers after the global one in mask order and re-sorted
//! by `mask.reorder`, `query.sample-curve` against the independent f64 reference at every sample,
//! and an 8-bit grey ramp through an S-curve within one code of the quantized reference.
//!
//! The host behaviour the curve shares with every field-patch module — discovery, drafts, no-ops,
//! deduplication, resets, one layer per target and per mask, history, sample equal to render on
//! both paths, an unavailable provider and reopen — is the field-patch conformance chapter's
//! ([`crate::conformance`]). The frozen fixture through render on both paths is the core's
//! `modules` test (`curve`).
use crate::basic_acceptance::{FIXTURE, import, mutation, render};
use crate::*;
use luxforge_core::{
    BASIC_EFFECT, CURVE_EFFECT, ClientId, MIXER_EFFECT, OwnerHandle, Recipe, SourceImage,
};
use luxforge_reference::{
    self as reference,
    curve::{CurvePoints, curve, curve_pixel},
};
use luxforge_testkit::client::{self, as_str, call, prepare};
use std::time::Instant;

/// The curve every section commits: an S through four points, gentle enough that its peak slope
/// stays well inside the forward tolerance.
const S_CURVE: [[f64; 2]; 4] = [[0.0, 0.0], [0.25, 0.18], [0.75, 0.82], [1.0, 1.0]];
/// The number of segments `query.sample-curve` answers the curve at, so `SAMPLE_SEGMENTS + 1`
/// samples.
const SAMPLE_SEGMENTS: u32 = 256;
/// The sample query's frozen tolerance against the reference.
const SAMPLE_TOLERANCE: f64 = 1e-12;
/// The grey ramp's width in pixels per code: one whole 8 × 8 JPEG block, so a flat block's DC-only
/// coding decodes to exactly its code.
const RAMP_BLOCK: u32 = 8;
/// The grey ramp's height: one 16-row MCU.
const RAMP_ROWS: u32 = 16;
/// Probes on the photo fixture, one per quadrant and the centre line.
const PROBES: [(u32, u32); 5] = [(120, 80), (360, 80), (120, 240), (360, 240), (240, 160)];

/// The chapter's entry point, wired into `editor-acceptance` after the Presence, mixer and vignette
/// chapter. Runs each section against its own catalog in `out`.
pub fn run(root: &Path, out: &Path) -> Result<Value> {
    let fixture = root.join(FIXTURE);
    let fixture_hash = hash(&fixture)?;
    let total = Instant::now();
    let placement = section(
        &fixture,
        &out.join("curve-placement-catalog.sqlite"),
        placement,
    )?;
    let masks = section(&fixture, &out.join("curve-mask-catalog.sqlite"), mask_order)?;
    let ramp = out.join("curve-grey-ramp.jpg");
    write_grey_ramp(&ramp)?;
    let ramp_hash = hash(&ramp)?;
    let sampled = section(
        &ramp,
        &out.join("curve-sample-catalog.sqlite"),
        sample_and_ramp,
    )?;
    ensure(
        hash(&fixture)? == fixture_hash,
        "The original source changed",
    )?;
    ensure(hash(&ramp)? == ramp_hash, "The grey ramp source changed")?;
    Ok(json!({
        "status": "passed",
        "fixture": FIXTURE,
        "fixture_sha256": fixture_hash,
        "grey_ramp": {"path": "curve-grey-ramp.jpg", "sha256": ramp_hash,
            "size": [256 * RAMP_BLOCK, RAMP_ROWS], "code": "column / 8"},
        "curve": S_CURVE,
        "placement": placement,
        "masks": masks,
        "sample_query_and_grey_ramp": sampled,
        "generic": "the host behaviour the curve shares with every field-patch module is proved under field_patch_conformance",
        "elapsed_ms": total.elapsed().as_secs_f64() * 1000.0,
    }))
}

/// What one section receives: the owner, the editing client, the imported asset, its Original
/// entry and the decoded source, and where it records what it showed.
type Section =
    fn(&OwnerHandle, ClientId, &Value, &Value, &SourceImage, &mut dyn FnMut(&str, Value)) -> Result;

/// One section against its own catalog: a new owner, one client and the source imported and
/// prepared once. The owner is stopped however the section ends.
fn section(source_path: &Path, catalog: &Path, run: Section) -> Result<Value> {
    let source = luxforge_core::open_source(source_path)?;
    let total = Instant::now();
    let mut checks = Vec::new();
    let (owner, join) = OwnerHandle::start(catalog)?;
    let outcome = (|| -> Result {
        let editor = owner.register();
        let imported = import(&owner, editor, source_path)?;
        let asset = imported["asset"]["id"].clone();
        let original = imported["current_entry"]["id"].clone();
        prepare(&owner, editor, &asset)?;
        let mut record =
            |shows: &str, detail: Value| checks.push(json!({"shows": shows, "detail": detail}));
        run(&owner, editor, &asset, &original, &source, &mut record)
    })();
    owner.stop();
    join.join().map_err(|_| "The owner thread panicked")?;
    outcome?;
    Ok(json!({
        "status": "passed",
        "checks": checks,
        "elapsed_ms": total.elapsed().as_secs_f64() * 1000.0,
    }))
}

/// One mutating call at the asset's current revision, answering its result.
fn commit(
    owner: &OwnerHandle,
    editor: ClientId,
    asset: &Value,
    method: &str,
    mut params: Value,
    request: &str,
) -> Result<Value> {
    let revision = client::revision(owner, editor, asset)?;
    params["asset_id"] = asset.clone();
    params["mutation"] = mutation(revision, request);
    Ok(call(owner, editor, method, params)?)
}

/// The stored stack as `(effect, mask name)` pairs, a global layer's mask reported as null.
fn stack(recipe: &Recipe) -> Vec<Value> {
    recipe
        .layers
        .iter()
        .map(|layer| {
            let mask = layer.mask.as_ref().map(|id| {
                recipe
                    .masks
                    .iter()
                    .find(|mask| &mask.id == id)
                    .map_or_else(|| format!("unknown mask {id:?}"), |mask| mask.name.clone())
            });
            json!({"effect": layer.effect_id, "mask": mask})
        })
        .collect()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn placement(
    owner: &OwnerHandle,
    editor: ClientId,
    asset: &Value,
    original: &Value,
    source: &SourceImage,
    record: &mut dyn FnMut(&str, Value),
) -> Result {
    // The curve always lands after Basic and before the mixer, whichever of the three is touched
    // first, and the stack renders the same bytes however it was reached.
    let payload_for = |action: &str| -> Value {
        match action {
            "set-basic" => json!({"exposure": 0.3, "contrast": 20.0}),
            "set-curve" => json!({"luminance": S_CURVE}),
            _ => json!({"blue-saturation": 40.0, "red-hue": 10.0}),
        }
    };
    let mut orders = Vec::new();
    let mut first: Option<(String, Vec<Value>)> = None;
    for order in [
        ["set-mixer", "set-curve", "set-basic"],
        ["set-curve", "set-mixer", "set-basic"],
        ["set-basic", "set-mixer", "set-curve"],
        ["set-basic", "set-curve", "set-mixer"],
        ["set-mixer", "set-basic", "set-curve"],
        ["set-curve", "set-basic", "set-mixer"],
    ] {
        let tag = order.join("-");
        commit(
            owner,
            editor,
            asset,
            "history.restore",
            json!({"entry_id": original}),
            &format!("curve-placement-reset-{tag}"),
        )?;
        ensure(
            client::recipe(owner, editor, asset)?.layers.is_empty(),
            "Restoring the Original entry left layers behind",
        )?;
        for action in order {
            let result = commit(
                owner,
                editor,
                asset,
                &format!("edit.{action}"),
                payload_for(action),
                &format!("curve-placement-{tag}-{action}"),
            )?;
            ensure(
                result["outcome"] == json!("applied"),
                format!("touch order {tag}: {action} answered {}", result["outcome"]),
            )?;
        }
        let described = call(owner, editor, "recipe.describe", json!({"asset_id": asset}))?;
        let described: Vec<Value> = described["layers"]
            .as_array()
            .ok_or("recipe.describe answered no layers")?
            .iter()
            .map(|layer| layer["effect"].clone())
            .collect();
        let recipe = client::recipe(owner, editor, asset)?;
        let stored: Vec<Value> = recipe
            .layers
            .iter()
            .map(|layer| json!(layer.effect_id))
            .collect();
        let expected = vec![
            json!(BASIC_EFFECT),
            json!(CURVE_EFFECT),
            json!(MIXER_EFFECT),
        ];
        ensure(
            stored == expected && described == expected,
            format!("touch order {tag}: stored {stored:?}, described {described:?}"),
        )?;
        let raster = render(source, &recipe)?;
        let digest = sha256(&raster.rgba);
        let samples = PROBES
            .iter()
            .map(|&(x, y)| {
                Ok(call(
                    owner,
                    editor,
                    "render.sample",
                    json!({"asset_id": asset, "x": x, "y": y}),
                )?["rgba"]
                    .clone())
            })
            .collect::<Result<Vec<Value>>>()?;
        match &first {
            None => first = Some((digest.clone(), samples.clone())),
            Some((first_digest, first_samples)) => {
                ensure(
                    &digest == first_digest,
                    format!(
                        "touch order {tag} rendered bytes {digest}, not the first order's {first_digest}"
                    ),
                )?;
                ensure(
                    &samples == first_samples,
                    format!(
                        "touch order {tag} sampled {samples:?}, not the first order's {first_samples:?}"
                    ),
                )?;
            }
        }
        orders.push(json!({
            "touch_order": order,
            "stored_order": stored,
            "rgba_sha256": digest,
            "samples": samples,
        }));
    }
    record(
        "committing Basic, the curve and the mixer in every touch order (the mixer then the curve, the curve then the mixer, each with Basic last, and the other four) on the photo fixture: the stored and described stack is Basic, curve, mixer every time, and the whole rendered raster and render.sample at five probes are identical across the orders",
        json!({"orders": orders, "byte_comparisons": orders.len() - 1}),
    );
    Ok(())
}

fn mask_order(
    owner: &OwnerHandle,
    editor: ClientId,
    asset: &Value,
    _: &Value,
    _: &SourceImage,
    record: &mut dyn FnMut(&str, Value),
) -> Result {
    // A global curve, then a masked curve on each of two masks, then a masked mixer on the first
    // mask: masked curve layers follow the global one in mask-list order and precede every mixer
    // layer, and `mask.reorder` re-sorts them with the mask list.
    commit(
        owner,
        editor,
        asset,
        "edit.set-curve",
        json!({"luminance": S_CURVE}),
        "curve-mask-global",
    )?;
    let first = commit(
        owner,
        editor,
        asset,
        "mask.create-radial",
        json!({"x": 0.3, "y": 0.4, "radius_x": 0.2, "radius_y": 0.2, "angle": 0.0, "feather": 50.0}),
        "curve-mask-first",
    )?;
    let first_mask = as_str(&first["mask"], "mask id")?;
    commit(
        owner,
        editor,
        asset,
        "edit.set-mixer",
        json!({"mask": first_mask, "blue-luminance": -30.0}),
        "curve-mask-first-mixer",
    )?;
    commit(
        owner,
        editor,
        asset,
        "edit.set-curve",
        json!({"mask": first_mask, "luminance": [[0.0, 0.1], [0.5, 0.6], [1.0, 1.0]]}),
        "curve-mask-first-curve",
    )?;
    let second = commit(
        owner,
        editor,
        asset,
        "mask.create-radial",
        json!({"x": 0.7, "y": 0.6, "radius_x": 0.2, "radius_y": 0.2, "angle": 0.0, "feather": 50.0}),
        "curve-mask-second",
    )?;
    let second_mask = as_str(&second["mask"], "mask id")?;
    commit(
        owner,
        editor,
        asset,
        "edit.set-curve",
        json!({"mask": second_mask, "luminance": [[0.0, 0.0], [0.5, 0.4], [1.0, 0.9]]}),
        "curve-mask-second-curve",
    )?;
    let masks_listed = |owner: &OwnerHandle| -> Result<Vec<Value>> {
        Ok(
            call(owner, editor, "mask.list", json!({"asset_id": asset}))?["masks"]
                .as_array()
                .ok_or("mask.list answered no masks")?
                .iter()
                .map(|mask| mask["name"].clone())
                .collect(),
        )
    };
    let before_recipe = client::recipe(owner, editor, asset)?;
    let before = stack(&before_recipe);
    let before_masks = masks_listed(owner)?;
    let expected_before = vec![
        json!({"effect": CURVE_EFFECT, "mask": null}),
        json!({"effect": CURVE_EFFECT, "mask": "Mask 1"}),
        json!({"effect": CURVE_EFFECT, "mask": "Mask 2"}),
        json!({"effect": MIXER_EFFECT, "mask": "Mask 1"}),
    ];
    ensure(
        before == expected_before,
        format!("before mask.reorder the stack is {before:?}"),
    )?;
    ensure(
        before_masks == vec![json!("Mask 1"), json!("Mask 2")],
        format!("before mask.reorder the masks are listed {before_masks:?}"),
    )?;
    let reordered = commit(
        owner,
        editor,
        asset,
        "mask.reorder",
        json!({"mask": second_mask, "index": 0}),
        "curve-mask-reorder",
    )?;
    let after_recipe = client::recipe(owner, editor, asset)?;
    let after = stack(&after_recipe);
    let after_masks = masks_listed(owner)?;
    let expected_after = vec![
        json!({"effect": CURVE_EFFECT, "mask": null}),
        json!({"effect": CURVE_EFFECT, "mask": "Mask 2"}),
        json!({"effect": CURVE_EFFECT, "mask": "Mask 1"}),
        json!({"effect": MIXER_EFFECT, "mask": "Mask 1"}),
    ];
    ensure(
        after == expected_after,
        format!("after mask.reorder the stack is {after:?}"),
    )?;
    ensure(
        after_masks == vec![json!("Mask 2"), json!("Mask 1")],
        format!("after mask.reorder the masks are listed {after_masks:?}"),
    )?;
    // The reorder moves layers, never rewrites them: every layer keeps its identity and payload.
    for layer in &before_recipe.layers {
        ensure(
            after_recipe.layers.iter().any(|moved| moved == layer),
            format!("mask.reorder rewrote layer {:?}", layer.id),
        )?;
    }
    record(
        "a global curve, a masked curve on each of two radial masks and a masked mixer on the first: the masked curve layers follow the global one in mask-list order and precede the mixer; mask.reorder {mask: Mask 2, index: 0} re-sorts them with the mask list, keeping every layer's identity and payload",
        json!({
            "before": {"masks": before_masks, "stack": before},
            "reorder": {"mask": "Mask 2", "index": 0, "label": reordered["label"], "outcome": reordered["outcome"]},
            "after": {"masks": after_masks, "stack": after},
        }),
    );
    Ok(())
}

/// A grey ramp of every 8-bit code, each an 8-pixel-wide column of whole JPEG blocks, encoded at
/// quality 100 so every flat block decodes to exactly its code.
fn write_grey_ramp(path: &Path) -> Result {
    let image = image::RgbImage::from_fn(256 * RAMP_BLOCK, RAMP_ROWS, |x, _| {
        let code = (x / RAMP_BLOCK) as u8;
        image::Rgb([code, code, code])
    });
    image::codecs::jpeg::JpegEncoder::new_with_quality(fs::File::create(path)?, 100)
        .encode_image(&image)?;
    Ok(())
}

/// The quantized reference for a grey input code through `points`.
fn reference_code(points: &CurvePoints, code: u8) -> [u8; 3] {
    let linear = reference::srgb::decode_encoded(f64::from(code) / 255.0);
    curve_pixel(points, [linear; 3]).map(reference::srgb::code)
}

fn sample_and_ramp(
    owner: &OwnerHandle,
    editor: ClientId,
    asset: &Value,
    _: &Value,
    source: &SourceImage,
    record: &mut dyn FnMut(&str, Value),
) -> Result {
    let neutral = render(source, &client::recipe(owner, editor, asset)?)?;
    let width = neutral.width as usize;
    ensure(
        neutral.width == 256 * RAMP_BLOCK && neutral.height == RAMP_ROWS,
        format!(
            "the grey ramp decoded to {} x {}",
            neutral.width, neutral.height
        ),
    )?;
    // Every block holds exactly its code before any layer, so the ramp holds every code.
    for (index, pixel) in neutral.rgba.chunks_exact(4).enumerate() {
        let code = ((index % width) as u32 / RAMP_BLOCK) as u8;
        ensure(
            pixel[..3] == [code; 3],
            format!("grey ramp pixel {index} decoded to {pixel:?}, not code {code}"),
        )?;
    }

    commit(
        owner,
        editor,
        asset,
        "edit.set-curve",
        json!({"luminance": S_CURVE}),
        "curve-ramp-s",
    )?;
    let state = call(owner, editor, "asset.state", json!({"asset_id": asset}))?;
    let entry = state["current_entry"]["id"].clone();
    let recipe = client::recipe(owner, editor, asset)?;
    ensure(
        recipe.layers.len() == 1 && recipe.layers[0].effect_id == CURVE_EFFECT,
        format!("the ramp's stack is {:?}", stack(&recipe)),
    )?;
    let stored = recipe.layers[0].payload["luminance"].clone();
    let described = call(
        owner,
        editor,
        "recipe.describe",
        json!({"asset_id": asset, "entry_id": entry}),
    )?;
    let described_values = described["layers"][0]["values"].clone();
    ensure(
        described_values["luminance"] == stored,
        format!("recipe.describe reports {described_values} for the stored points {stored}"),
    )?;

    // The sample query for the displayed entry's points, against the independent reference.
    let points: Vec<[f64; 2]> = stored
        .as_array()
        .ok_or("the stored layer holds no points")?
        .iter()
        .map(|point| {
            Ok([
                point[0].as_f64().ok_or("a point's x")?,
                point[1].as_f64().ok_or("a point's y")?,
            ])
        })
        .collect::<Result<_>>()?;
    let reference_points = CurvePoints::new(&points);
    let answer = call(
        owner,
        editor,
        "query.sample-curve",
        json!({"asset_id": asset, "entry_id": entry, "luminance": stored}),
    )?;
    let samples = answer["points"]
        .as_array()
        .ok_or("query.sample-curve answered no points")?;
    ensure(
        samples.len() == SAMPLE_SEGMENTS as usize + 1,
        format!("query.sample-curve answered {} samples", samples.len()),
    )?;
    let mut largest_difference = 0.0_f64;
    let mut equal = 0;
    for (index, sample) in samples.iter().enumerate() {
        let x = f64::from(index as u32) / f64::from(SAMPLE_SEGMENTS);
        let (at, value) = (
            sample[0].as_f64().ok_or("a sample's x")?,
            sample[1].as_f64().ok_or("a sample's y")?,
        );
        ensure(at == x, format!("sample {index} is at {at}, not {x}"))?;
        let expected = curve(&reference_points, x);
        let difference = (value - expected).abs();
        ensure(
            difference <= SAMPLE_TOLERANCE,
            format!("sample {index} at {x}: {value} against the reference's {expected}"),
        )?;
        largest_difference = largest_difference.max(difference);
        equal += 1;
    }
    record(
        "query.sample-curve for the displayed entry's stored points answers 257 samples at i / 256, each equal to luxforge_reference::curve::curve within 1e-12",
        json!({
            "entry_id": entry,
            "points": stored,
            "samples": samples.len(),
            "equal_within_1e-12": equal,
            "largest_difference": largest_difference,
        }),
    );

    // The grey ramp through the S-curve: every rendered pixel within one code of the quantized
    // reference for its input code, and render.sample at every block's centre equal to the raster.
    let curved = render(source, &recipe)?;
    let mut off_by_one_codes = Vec::new();
    let mut off_by_one_pixels = 0_u64;
    let mut outputs = Vec::with_capacity(256);
    for code in 0..=255_u8 {
        let expected = reference_code(&reference_points, code);
        let mut codes_off = false;
        let left = u32::from(code) * RAMP_BLOCK;
        let block_output = &curved.rgba[(left as usize) * 4..(left as usize) * 4 + 3];
        for y in 0..RAMP_ROWS as usize {
            for x in left as usize..(left + RAMP_BLOCK) as usize {
                let pixel = &curved.rgba[(y * width + x) * 4..(y * width + x) * 4 + 3];
                ensure(
                    pixel == block_output,
                    format!(
                        "code {code}'s block is not flat: {pixel:?} at ({x}, {y}) against {block_output:?}"
                    ),
                )?;
                for channel in 0..3 {
                    let distance = pixel[channel].abs_diff(expected[channel]);
                    ensure(
                        distance <= 1,
                        format!(
                            "code {code} rendered {pixel:?}, more than one code from the reference {expected:?}"
                        ),
                    )?;
                    if distance == 1 {
                        codes_off = true;
                    }
                }
                if pixel != expected {
                    off_by_one_pixels += 1;
                }
            }
        }
        let centre = (left + RAMP_BLOCK / 2, RAMP_ROWS / 2);
        let sampled = call(
            owner,
            editor,
            "render.sample",
            json!({"asset_id": asset, "x": centre.0, "y": centre.1}),
        )?["rgba"]
            .clone();
        ensure(
            sampled.as_array().is_some_and(|rgba| {
                rgba.iter()
                    .take(3)
                    .zip(block_output)
                    .all(|(sampled, rendered)| sampled.as_u64() == Some(u64::from(*rendered)))
            }),
            format!(
                "render.sample at code {code}'s centre answered {sampled}, the raster {block_output:?}"
            ),
        )?;
        if codes_off {
            off_by_one_codes
                .push(json!({"code": code, "rendered": block_output, "reference": expected}));
        }
        outputs.push(block_output[0]);
    }
    ensure(
        outputs.windows(2).all(|pair| pair[0] <= pair[1]),
        "the S-curve's rendered grey ramp decreases somewhere",
    )?;
    record(
        "an 8-bit grey ramp of every code rendered through the S-curve layer: every pixel within one code of the quantized f64 reference at every code, each block flat, render.sample at every block centre equal to the raster, and the output non-decreasing along the ramp",
        json!({
            "codes": 256,
            "within_one_code": 256,
            "off_by_one_code_count": off_by_one_codes.len(),
            "off_by_one_pixel_count": off_by_one_pixels,
            "off_by_one_codes": off_by_one_codes,
            "rendered_grey": outputs,
        }),
    );
    Ok(())
}
