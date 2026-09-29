//! The masking chapter of `cargo xtask editor-acceptance`: a catalog holding masks reopens through
//! a fresh owner with the same masks, components, bound layers and pixels.
//!
//! Everything here is driven through the JSON method table with [`OwnerHandle::call`], exactly as an
//! **independent client** reaches it: the `mask.*` family, `edit.set-basic`, `edit.set-presence`,
//! `edit.set-mixer`, `mask.list`, `asset.state` and `render.sample`. No desktop, no window and no
//! pointer.
//!
//! What only this chapter proves is the reopen. Every mask command, refusal, conflict and history
//! step, and a stack served with a maskable module unavailable or without its original, is a
//! `cargo test`'s at the same layer (the mask command tests, the field-patch conformance suite and
//! the owner tests), so the chapter builds one catalog that holds every component kind, every mode,
//! a masked layer of Basic, Presence and the colour mixer, and a brush's strokes in the artifact
//! store, stops the owner that wrote it, and asks a fresh owner over the same file for all of it.
//! The panel's own evidence is the `mask-*` smoke scenarios.
use crate::basic_acceptance::{import, mutation};
use crate::*;
use luxforge_core::OwnerHandle;
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

/// What the writing owner leaves for the reopen to answer: the asset, its current entry and
/// revision, the masks by identity and the sampled pixels.
struct Written {
    asset: Value,
    entry: String,
    revision: u64,
    masks: Vec<Value>,
    samples: Vec<Value>,
}

/// One rendered pixel of the session's selected entry, read through the API.
///
/// Pixels are read with `render.sample` and never by rendering a recipe in this process, and that is
/// a contract rather than a convenience: a brush component's payload holds its strokes by **content
/// address**, and the resolved strokes are a field of the in-memory recipe that is never serialized
/// ([stroke storage](../../docs/design/masking.md#stroke-storage)). A recipe fetched over JSON
/// therefore has addresses and no points, so rendering one outside the catalog that holds the store
/// is refused by name — which is the retention contract working, not a gap. The owner has the store,
/// so the owner is asked. `render.sample` equalling the rendered byte for every component kind on
/// both paths is proved by `mask/masked_colour.rs` and `mask/masked_spatial.rs`.
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
fn write_catalog(owner: &OwnerHandle, fixture: &Path) -> Result<Written> {
    let editor = owner.register();
    let imported = import(owner, editor, fixture)?;
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
    next("mask.duplicate", json!({"mask": gradient_mask}))?;

    let state = call(owner, editor, "asset.state", json!({"asset_id": asset}))?;
    let masks = identities(&call(
        owner,
        editor,
        "mask.list",
        json!({"asset_id": asset}),
    )?)?;
    let bound = masks
        .iter()
        .filter(|mask| {
            mask["layers"]
                .as_array()
                .is_some_and(|layers| layers.len() == 1)
        })
        .count();
    ensure(
        masks.len() == 6 && bound == 2,
        format!(
            "The catalog holds {} masks with {bound} holding one bound layer, expected five \
             masks and a duplicate, the gradient and its copy holding one layer each",
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
    let written = write_catalog(&owner, &fixture);
    owner.stop();
    join.join().map_err(|_| "The owner thread panicked")?;
    let written = written?;
    record(
        "a catalog is written holding all five component kinds, a mask of three kinds in every mode, a brush with two strokes, non-default amount, inversion, name and geometry, a duplicate, and a masked layer each of Basic, Presence and the colour mixer",
        json!({"revision": written.revision, "masks": written.masks.len(),
            "sampled_positions": written.samples.len()}),
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
        "method": "Every step is one JSON request through OwnerHandle::call, as an independent client reaches it: no desktop, no window and no pointer. The panel's own evidence is the mask-* smoke scenarios.",
        "checks": checks.borrow().clone(),
        "timings_ms": {"total": total.elapsed().as_secs_f64() * 1000.0},
    }))
}
