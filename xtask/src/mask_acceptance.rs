//! The masking chapter of `cargo xtask editor-acceptance`: a catalog holding masks reopens through
//! a fresh owner with the same masks, components, bound layers and pixels.
//!
//! Everything here is driven through the JSON method table with [`OwnerHandle::call`], exactly as an
//! **independent client** reaches it: the `mask.*` family, `edit.set-basic`, `edit.set-presence`,
//! `edit.set-mixer`, `edit.set-curve`, `mask.list`, `asset.state` and `render.sample`. No desktop,
//! no window and no pointer. The one exception is the oracle `render.sample` is compared with: the
//! owner's own bound stack for the current entry, rendered here (see [`sample_equals_render`]).
//!
//! What only this chapter proves is the reopen. Every mask command, refusal, conflict and history
//! step, and a stack served with a maskable module unavailable or without its original, is a
//! `cargo test`'s at the same layer (the mask command tests, the field-patch conformance suite and
//! the owner tests), so the chapter builds one catalog that holds every component kind, every mode,
//! a masked layer of Basic, Presence, the Tone curve and the colour mixer, and a brush's strokes in
//! the artifact store, stops the owner that wrote it, and asks a fresh owner over the same file for
//! all of it. On the way it proves the masked Tone curve's placement: listed before the mixer on
//! the mask it shares with it, copied by `mask.duplicate`, re-sorted by `mask.reorder`, and sampled
//! equal to the rendered byte. The panel's own evidence is the `mask-*` smoke scenarios.
use crate::basic_acceptance::{mutation, open};
use crate::*;
use luxforge_core::{
    AssetId, CURVE_EFFECT, MIXER_EFFECT, OwnerHandle, PreviewRequest, RenderOptions, SnapshotId,
    SourceImage,
};
use luxforge_testkit::client::{Checked, as_str, as_u64, call, prepare};
use std::{cell::RefCell, time::Instant};

/// The fixture this chapter runs on: the 480x320 synthetic quadrant pattern every other chapter
/// uses, so a masked reading is comparable with the unmasked ones beside it.
const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";

/// The gradient the chapter's first mask is drawn with, top to bottom down the middle: `p0` at
/// coverage 0 and `p1` at coverage 1, so the bottom of the frame is selected and the top is not.
const GRADIENT: [f64; 4] = [0.5, 0.2, 0.5, 0.8];

/// A position the gradient above covers fully, one it does not cover at all, and four more across
/// the frame, in content pixels of the 480x320 fixture. `render.sample` is read at all of them
/// before and after the reopen.
const PROBES: [(u32, u32); 6] = [
    (240, 300),
    (240, 20),
    (120, 160),
    (360, 160),
    (40, 280),
    (440, 40),
];

/// The masked exposure the chapter lifts through its masks, in EV.
const MASKED_EV: f64 = 1.0;

/// The Tone curve the chapter binds to the radial mask beside the mixer: a mid-tone lift.
const MASKED_CURVE: [[f64; 2]; 3] = [[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]];

/// Positions for the radial mask, in content pixels: a red pixel beside its centre, deep in the core
/// the inverted radial excludes (the centre itself is the fixture's white cross, which a curve
/// through `(1, 1)` leaves white anyway), and three corners it covers.
const RADIAL_PROBES: [(u32, u32); 4] = [(225, 150), (20, 20), (460, 20), (20, 300)];

/// What the writing owner leaves for the reopen to answer: the asset, its current entry and
/// revision, the masks by identity and the sampled pixels, and what the masked curve showed.
struct Written {
    asset: Value,
    entry: String,
    revision: u64,
    masks: Vec<Value>,
    samples: Vec<Value>,
    curve: Value,
}

/// The effects of the layers `mask.list` lists as bound to `mask`, in the order it lists them.
fn bound_effects(listed: &Value, mask: &str) -> Result<Vec<String>> {
    listed["masks"]
        .as_array()
        .ok_or("No masks listed")?
        .iter()
        .find(|held| held["id"] == json!(mask))
        .ok_or_else(|| format!("mask.list does not list mask {mask}"))?["layers"]
        .as_array()
        .ok_or_else(|| format!("mask {mask} lists no layers"))?
        .iter()
        .map(|layer| Ok(as_str(&layer["effect"], "effect")?))
        .collect()
}

/// The masks the stack's Tone curve layers are bound to, in stack order, as `asset.state`'s current
/// recipe holds them, with the mask list's order beside it.
fn curve_targets(
    owner: &OwnerHandle,
    client: luxforge_core::ClientId,
    asset: &Value,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let state = call(owner, client, "asset.state", json!({"asset_id": asset}))?;
    let recipe = &state["current_entry"]["snapshot"]["recipe"];
    let layers = recipe["layers"]
        .as_array()
        .ok_or("asset.state answered no layers")?
        .iter()
        .filter(|layer| layer["effect_id"] == json!(CURVE_EFFECT))
        .map(|layer| layer["mask"].clone())
        .collect();
    let masks = recipe["masks"]
        .as_array()
        .ok_or("asset.state answered no masks")?
        .iter()
        .map(|mask| mask["id"].clone())
        .collect();
    Ok((layers, masks))
}

/// `render.sample` equals the rendered byte at every probe, for the stack the owner holds now.
///
/// The oracle is the owner's own bound stack for the current entry — its preview evaluation, with
/// every brush stroke's points bound in from the artifact store — rendered here on the fixture's
/// decoded source with the owner's registry. That is the one way a stack holding brushes can be
/// rendered outside the owner (see [`sample`]); the sample itself is still the JSON request.
fn sample_equals_render(
    owner: &OwnerHandle,
    client: luxforge_core::ClientId,
    asset: &Value,
    source: &SourceImage,
) -> Result<Value> {
    let asset_id: AssetId = serde_json::from_value(asset.clone())?;
    let job = owner.preview_job(PreviewRequest::new(client, asset_id))?;
    let evaluation = &job.evaluation;
    let raster = luxforge_core::render(
        evaluation.registry(),
        source,
        evaluation.recipe(),
        RenderOptions::default(),
        evaluation.context(),
    )?
    .frame(SnapshotId::new())?;
    let mut pixels = Vec::new();
    for at in PROBES.iter().chain(RADIAL_PROBES.iter()) {
        let sampled = sample(owner, client, asset, *at)?;
        let rendered: Vec<u64> = raster
            .pixel(at.0, at.1)
            .ok_or("The rendered raster has no such pixel")?
            .iter()
            .take(3)
            .map(|code| u64::from(*code))
            .collect();
        ensure(
            sampled == rendered,
            format!(
                "render.sample read {sampled:?} at {at:?} where the stack renders {rendered:?}"
            ),
        )?;
        pixels.push(json!({"x": at.0, "y": at.1, "rgb": rendered}));
    }
    Ok(
        json!({"entry": evaluation.entry().id, "layers": evaluation.recipe().layers.len(),
        "probes": pixels}),
    )
}

/// One rendered pixel of the session's selected entry, read through the API.
///
/// Pixels are read with `render.sample` and never by rendering a recipe fetched over JSON, and that
/// is a contract rather than a convenience: a brush component's payload holds its strokes by
/// **content address**, and the resolved strokes are a field of the in-memory recipe that is never
/// serialized ([stroke storage](../../docs/design/masking.md#stroke-storage)). A recipe fetched over
/// JSON therefore has addresses and no points, so rendering one outside the catalog that holds the
/// store is refused by name — which is the retention contract working, not a gap. The owner has the
/// store, so the owner is asked; [`sample_equals_render`] renders the owner's own bound stack for its
/// oracle. `render.sample` equalling the rendered byte for every component kind on both paths is
/// proved by `mask/masked_colour.rs` and `mask/masked_spatial.rs`.
fn sample(
    owner: &OwnerHandle,
    client: luxforge_core::ClientId,
    asset: &Value,
    at: (u32, u32),
) -> Checked<Vec<u64>> {
    let read = call(
        owner,
        client,
        "render.sample",
        json!({"asset_id": asset, "x": at.0, "y": at.1}),
    )?;
    read["rgba"]
        .as_array()
        .ok_or("render.sample answered no rgba")?
        .iter()
        .take(3)
        .map(|code| as_u64(code, "code"))
        .collect()
}

/// Every probe position read through `owner`, with its coordinates beside the value.
fn sampled(
    owner: &OwnerHandle,
    client: luxforge_core::ClientId,
    asset: &Value,
) -> Result<Vec<Value>> {
    PROBES
        .iter()
        .map(|at| Ok(json!({"x": at.0, "y": at.1, "rgb": sample(owner, client, asset, *at)?})))
        .collect()
}

/// Every mask of a `mask.list` answer by the identities and values a reopen must return.
fn identities(listed: &Value) -> Result<Vec<Value>> {
    Ok(listed["masks"]
        .as_array()
        .ok_or("No masks listed")?
        .iter()
        .map(|mask| {
            json!({"id": mask["id"], "name": mask["name"], "amount": mask["amount"],
                "invert": mask["invert"], "layers": mask["layers"],
                "components": mask["components"].as_array().map(|components| components
                    .iter()
                    .map(|component| json!({"id": component["id"], "kind": component["kind"],
                        "mode": component["mode"], "invert": component["invert"],
                        "payload": component["payload"]}))
                    .collect::<Vec<_>>())})
        })
        .collect())
}

/// The catalog the reopen is asked about, written by `owner` as an independent client.
fn write_catalog(owner: &OwnerHandle, fixture: &Path, source: &SourceImage) -> Result<Written> {
    let editor = owner.register();
    let imported = open(owner, editor, fixture)?;
    let asset = imported["asset"]["id"].clone();
    prepare(owner, editor, &asset)?;
    let mut revision = as_u64(
        &call(owner, editor, "asset.state", json!({"asset_id": asset}))?["revision"],
        "revision",
    )?;
    let mut next = |method: &str, params: Value| -> Result<Value> {
        let mut params = params;
        params["asset_id"] = asset.clone();
        params["mutation"] = mutation(revision, &format!("mask-{method}-{revision}"));
        let result = call(owner, editor, method, params)?;
        revision = as_u64(&result["revision"], "revision")?;
        Ok(result)
    };

    // All five component kinds, each created from JSON: four by their generated geometry methods
    // and the brush by the one command a stroke travels through.
    let linear = next(
        "mask.create-linear",
        json!({"x0": GRADIENT[0], "y0": GRADIENT[1], "x1": GRADIENT[2], "y1": GRADIENT[3]}),
    )?;
    let gradient_mask = as_str(&linear["mask"], "mask id")?;
    let gradient_component = as_str(&linear["component"], "component id")?;
    let radial = next(
        "mask.create-radial",
        json!({"x": 0.5, "y": 0.5, "radius_x": 0.2, "radius_y": 0.2, "angle": 0.0, "feather": 50.0}),
    )?;
    let radial_mask = as_str(&radial["mask"], "mask id")?;
    next(
        "mask.create-luminance-range",
        json!({"low": 20.0, "low_feather": 5.0, "high": 80.0, "high_feather": 5.0}),
    )?;
    next("mask.create-colour-range", json!({"refine": 50.0}))?;
    let painted = next(
        "mask.add-stroke",
        json!({"points": [[0.2, 0.6], [0.8, 0.6]], "size": 0.08, "feather": 0.0,
            "flow": 100.0, "erase": false}),
    )?;
    let brush_mask = as_str(&painted["mask"], "mask id")?;
    let brush_component = as_str(&painted["component"], "component id")?;

    // One mask composing three kinds in every mode, and a second stroke on the brush, so the
    // artifact store holds more than one stroke of one component.
    next(
        "mask.add-radial",
        json!({"mask": gradient_mask, "mode": "add", "x": 0.3, "y": 0.3,
            "radius_x": 0.15, "radius_y": 0.15, "angle": 0.0, "feather": 20.0}),
    )?;
    next(
        "mask.add-stroke",
        json!({"mask": gradient_mask, "mode": "subtract",
            "points": [[0.3, 0.85], [0.7, 0.85]], "size": 0.06, "feather": 0.0,
            "flow": 100.0, "erase": false}),
    )?;
    next(
        "mask.add-luminance-range",
        json!({"mask": gradient_mask, "mode": "intersect", "low": 0.0,
            "low_feather": 0.0, "high": 100.0, "high_feather": 0.0}),
    )?;
    next(
        "mask.add-stroke",
        json!({"mask": brush_mask, "component": brush_component,
            "points": [[0.2, 0.4], [0.8, 0.4]], "size": 0.08, "feather": 50.0,
            "flow": 80.0, "erase": false}),
    )?;

    // Whole-mask and component values that are not the defaults, a geometry patch, a rename, a
    // masked layer of each maskable module, and a duplicate that copies the bound layers.
    next(
        "mask.set-linear",
        json!({"mask": gradient_mask, "component": gradient_component, "y1": 0.9}),
    )?;
    next(
        "mask.set-component-invert",
        json!({"mask": gradient_mask, "component": gradient_component, "invert": true}),
    )?;
    next(
        "mask.set-amount",
        json!({"mask": radial_mask, "amount": 60.0}),
    )?;
    next(
        "mask.set-invert",
        json!({"mask": radial_mask, "invert": true}),
    )?;
    next("mask.rename", json!({"mask": gradient_mask, "name": "Sky"}))?;
    next(
        "edit.set-basic",
        json!({"mask": gradient_mask, "exposure": MASKED_EV}),
    )?;
    next(
        "edit.set-presence",
        json!({"mask": radial_mask, "clarity": 40.0}),
    )?;
    next(
        "edit.set-mixer",
        json!({"mask": radial_mask, "blue-luminance": -30.0}),
    )?;

    // The Tone curve on the radial mask beside the mixer. Declaring `maskable` is the whole of the
    // module's part: the host places the masked curve layer by the curve's declared order, before
    // the mixer, and `render.sample` reads the byte the stack renders through it.
    let before_curve = RADIAL_PROBES
        .iter()
        .map(|at| sample(owner, editor, &asset, *at))
        .collect::<Checked<Vec<_>>>()?;
    next(
        "edit.set-curve",
        json!({"mask": radial_mask, "luminance": MASKED_CURVE}),
    )?;
    let listed = call(owner, editor, "mask.list", json!({"asset_id": asset}))?;
    let placed = bound_effects(&listed, &radial_mask)?;
    let at = |effect: &str| placed.iter().position(|held| held == effect);
    ensure(
        matches!((at(CURVE_EFFECT), at(MIXER_EFFECT)), (Some(curve), Some(mixer)) if curve < mixer)
            && placed.iter().filter(|held| *held == CURVE_EFFECT).count() == 1,
        format!(
            "mask.list lists the radial mask's layers as {placed:?}, expected one curve layer before the mixer"
        ),
    )?;
    let after_curve = RADIAL_PROBES
        .iter()
        .map(|at| sample(owner, editor, &asset, *at))
        .collect::<Checked<Vec<_>>>()?;
    ensure(
        after_curve[0] == before_curve[0],
        format!(
            "The masked curve moved the radial's core, which the inverted radial excludes, from \
             {:?} to {:?}",
            before_curve[0], after_curve[0]
        ),
    )?;
    ensure(
        after_curve[1..] != before_curve[1..],
        "The masked curve changed no pixel the inverted radial covers",
    )?;
    let curve_sampled = sample_equals_render(owner, editor, &asset, source)?;

    next("mask.duplicate", json!({"mask": gradient_mask}))?;

    // A duplicate copies the curve with the mask, placed after its source's curve layer in mask
    // order, and a reorder re-sorts exactly the masked curve layers into the new mask order.
    let copied = next("mask.duplicate", json!({"mask": radial_mask}))?;
    let radial_copy = as_str(&copied["mask"], "the copy's mask id")?;
    let listed = call(owner, editor, "mask.list", json!({"asset_id": asset}))?;
    let copy_placed = bound_effects(&listed, &radial_copy)?;
    ensure(
        copy_placed == placed,
        format!("The radial mask's copy holds {copy_placed:?}, its source holds {placed:?}"),
    )?;
    let (duplicated, duplicated_masks) = curve_targets(owner, editor, &asset)?;
    ensure(
        duplicated == [json!(radial_mask), json!(radial_copy)],
        format!(
            "After mask.duplicate the curve layers are bound to {duplicated:?}, expected the radial mask then its copy"
        ),
    )?;
    next("mask.reorder", json!({"mask": radial_copy, "index": 0}))?;
    let (reordered, reordered_masks) = curve_targets(owner, editor, &asset)?;
    ensure(
        reordered_masks.first() == Some(&json!(radial_copy))
            && reordered == [json!(radial_copy), json!(radial_mask)],
        format!(
            "After mask.reorder the curve layers are bound to {reordered:?} and the masks are {reordered_masks:?}, expected the copy first in both"
        ),
    )?;
    let reorder_sampled = sample_equals_render(owner, editor, &asset, source)?;
    let curve = json!({
        "mask": radial_mask,
        "points": MASKED_CURVE,
        "bound_layers": placed,
        "copy": radial_copy,
        "copy_bound_layers": copy_placed,
        "curve_masks_after_duplicate": duplicated,
        "mask_order_after_duplicate": duplicated_masks,
        "curve_masks_after_reorder": reordered,
        "mask_order_after_reorder": reordered_masks,
        "radial_probes": RADIAL_PROBES.iter().zip(before_curve.iter().zip(after_curve.iter()))
            .map(|(at, (before, after))| json!({"x": at.0, "y": at.1, "before": before, "after": after}))
            .collect::<Vec<_>>(),
        "sample_equals_render": {"after_curve": curve_sampled, "after_reorder": reorder_sampled},
    });

    let state = call(owner, editor, "asset.state", json!({"asset_id": asset}))?;
    let masks = identities(&call(
        owner,
        editor,
        "mask.list",
        json!({"asset_id": asset}),
    )?)?;
    let holding = |count: usize| {
        masks
            .iter()
            .filter(|mask| {
                mask["layers"]
                    .as_array()
                    .is_some_and(|layers| layers.len() == count)
            })
            .count()
    };
    let (bound, radial_bound) = (holding(1), holding(3));
    ensure(
        masks.len() == 7 && bound == 2 && radial_bound == 2,
        format!(
            "The catalog holds {} masks with {bound} holding one bound layer and {radial_bound} \
             holding three, expected five masks and two duplicates: the gradient and its copy \
             holding one layer each, and the radial and its copy holding Presence, the Tone curve \
             and the mixer",
            masks.len()
        ),
    )?;
    // A spread of sampled positions across the frame stands in for a whole-frame comparison,
    // because a recipe fetched over JSON cannot be rendered outside the stroke store that holds
    // its brushes.
    Ok(Written {
        samples: sampled(owner, editor, &asset)?,
        entry: as_str(&state["current_entry"]["id"], "entry id")?,
        revision: as_u64(&state["revision"], "revision")?,
        masks,
        asset,
        curve,
    })
}

pub fn run(root: &Path, out: &Path) -> Result<Value> {
    let fixture = root.join(FIXTURE);
    let fixture_hash = hash(&fixture)?;
    let catalog = out.join("mask-catalog.sqlite");
    let checks: RefCell<Vec<Value>> = RefCell::new(Vec::new());
    let record = |shows: &str, detail: Value| {
        checks
            .borrow_mut()
            .push(json!({"shows": shows, "detail": detail}))
    };
    let total = Instant::now();

    // The owner that writes the catalog is stopped before the reopen, so nothing the fresh owner
    // reads is held in the first one's memory.
    let (owner, join) = OwnerHandle::start(&catalog)?;
    let written = luxforge_core::open_source(&fixture)
        .map_err(Into::into)
        .and_then(|source| write_catalog(&owner, &fixture, &source));
    owner.stop();
    join.join().map_err(|_| "The owner thread panicked")?;
    let written = written?;
    record(
        "a catalog is written holding all five component kinds, a mask of three kinds in every mode, a brush with two strokes, non-default amount, inversion, name and geometry, two duplicates, and a masked layer each of Basic, Presence, the Tone curve and the colour mixer",
        json!({"revision": written.revision, "masks": written.masks.len(),
            "sampled_positions": written.samples.len()}),
    );
    record(
        "edit.set-curve binds a Tone curve layer to the radial mask beside the mixer: mask.list lists it once, before the mixer, in placement order; it changes the pixels the inverted radial covers and not the core it excludes; render.sample equals the rendered byte of the owner's bound stack at every probe; mask.duplicate copies it to the copy in the same order, placed after its source's curve layer; mask.reorder moving the copy first re-sorts the masked curve layers into the new mask order, and render.sample still equals the rendered byte",
        written.curve.clone(),
    );

    // The reopen. A fresh owner over the same catalog returns the masks, their components, the
    // layers bound to them and the history, with the identities the first one wrote.
    let (reopened, reopened_join) = OwnerHandle::start(&catalog)?;
    let restart = (|| -> Result<Value> {
        let client = reopened.register();
        prepare(&reopened, client, &written.asset)?;
        let state = call(
            &reopened,
            client,
            "asset.state",
            json!({"asset_id": written.asset}),
        )?;
        ensure(
            as_u64(&state["revision"], "revision")? == written.revision
                && state["current_entry"]["id"] == json!(written.entry),
            format!("The reopened state is {state}"),
        )?;
        let masks = identities(&call(
            &reopened,
            client,
            "mask.list",
            json!({"asset_id": written.asset}),
        )?)?;
        ensure(
            masks == written.masks,
            "The reopened masks are not the ones that were written, identity for identity",
        )?;
        let samples = sampled(&reopened, client, &written.asset)?;
        ensure(
            samples == written.samples,
            format!(
                "The reopened masked picture reads {samples:?}, it wrote {:?}",
                written.samples
            ),
        )?;
        Ok(
            json!({"revision": written.revision, "current_entry": written.entry,
            "masks": masks.len(), "samples": samples}),
        )
    })();
    reopened.stop();
    reopened_join
        .join()
        .map_err(|_| "The reopened owner thread panicked")?;
    let restart = restart?;
    record(
        "a fresh owner over the same catalog returns the same revision, the same current entry, the same masks, components, payloads and bound layers by identity, and the same sampled pixels across the frame",
        restart.clone(),
    );

    ensure(
        hash(&fixture)? == fixture_hash,
        "The masking chapter changed its own original",
    )?;
    Ok(json!({
        "status": "passed",
        "fixture": FIXTURE,
        "fixture_sha256": fixture_hash,
        "asset_id": written.asset,
        "final_revision": written.revision,
        "final_entry_id": written.entry,
        "masks": written.masks,
        "reopen": restart,
        "sampled_positions": written.samples,
        "masked_curve": written.curve,
        "method": "Every step is one JSON request through OwnerHandle::call, as an independent client reaches it: no desktop, no window and no pointer. The oracle render.sample is compared with is the owner's own bound stack for the current entry, rendered in this process. The panel's own evidence is the mask-* smoke scenarios.",
        "checks": checks.borrow().clone(),
        "timings_ms": {"total": total.elapsed().as_secs_f64() * 1000.0},
    }))
}
