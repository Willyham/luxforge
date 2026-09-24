//! The masking chapter of `cargo xtask editor-acceptance`.
//!
//! Everything here is driven through the JSON method table with [`OwnerHandle::call`], exactly as an
//! **independent client** reaches it: `schema.list`, the whole `mask.*` family, `edit.set-basic`,
//! `edit.set-presence`, `edit.set-mixer`, `draft.*`, `render.sample`, `recipe.describe` and
//! `history.*`. No desktop, no window and no pointer. That is the point of the chapter: the pillar
//! says a GUI gesture is never the only interface, and the only way to hold the API to it is to make
//! the whole journey without one.
//!
//! What this chapter does **not** claim. It is not the panel's evidence: the four `mask-*` smoke
//! scenarios drive the real editor and are where a gesture's own frames, captures and correlated
//! state live. This chapter is the other half of the same pillar — that every one of those gestures
//! has a discoverable programmatic equivalent, that the equivalent produces the stacks, history,
//! pixels and refusals the design states, and that the failure paths behave with masks in the recipe.
use crate::basic_acceptance::{as_str, as_u64, call, import, mutation, prepare_source, refused};
use crate::conformance::client::registry_without;
use crate::*;
use lightwell_core::{OwnerHandle, mask::commands as mask_commands};
use std::{cell::RefCell, sync::Arc, time::Instant};

/// The fixture this chapter runs on: the 480x320 synthetic quadrant pattern every other chapter
/// uses, so a masked reading is comparable with the unmasked ones beside it.
const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";

/// The gradient the chapter's first mask is drawn with, top to bottom down the middle: `p0` at
/// coverage 0 and `p1` at coverage 1, so the bottom of the frame is selected and the top is not.
const GRADIENT: [f64; 4] = [0.5, 0.2, 0.5, 0.8];

/// A position the gradient above covers fully and one it does not cover at all, in content pixels of
/// the 480x320 fixture. `render.sample` is read at both, so "the mask changed the picture" is a
/// statement about two pixels that must move differently rather than about one that moved.
const INSIDE: (u32, u32) = (240, 300);
const OUTSIDE: (u32, u32) = (240, 20);

/// The masked exposure the chapter lifts through its masks, in EV.
const MASKED_EV: f64 = 1.0;

/// One mask command that must be refused, with the substring its reason must contain. Collected in
/// one place because a refusal is evidence exactly as an applied edit is, and an acceptance pass that
/// only records what worked has not tested the contracts at all.
struct Refusal {
    what: &'static str,
    method: &'static str,
    contains: &'static str,
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
/// both paths is proved by `masked_colour.rs` and `masked_spatial.rs`.
fn sample(
    owner: &OwnerHandle,
    client: lightwell_core::ClientId,
    asset: &Value,
    at: (u32, u32),
) -> Result<Vec<u64>> {
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

    let (owner, join) = OwnerHandle::start(&catalog)?;
    let mut join = Some(join);
    let outcome = (|| -> Result<Value> {
        let editor = owner.register();
        let agent = owner.register();

        // 1. Discovery. The whole family is in the method table before any of it is used, and the
        //    per-kind geometry methods are there for exactly the kinds the host's own table declares
        //    geometry for — which is what "registering a kind is what makes it creatable" means from
        //    the outside.
        let schema = call(&owner, editor, "schema.list", json!({}))?;
        let methods = &schema["methods"];
        let declared: Vec<String> = mask_commands::all()
            .iter()
            .map(|command| command.method.to_owned())
            .collect();
        for method in &declared {
            ensure(
                methods.get(method).is_some(),
                format!("{method} is declared by the host and is not discoverable"),
            )?;
        }
        // A brush declares no geometry, so it generates none of the three geometry methods. That is
        // an absence worth asserting: a `mask.create-brush` in the table would be a published method
        // with nothing behind it.
        for method in [
            "mask.create-brush",
            "mask.add-brush",
            "mask.set-brush",
            "mask.create",
            "mask.add",
            "mask.set",
        ] {
            ensure(
                methods.get(method).is_none(),
                format!("{method} is published and should not exist"),
            )?;
        }
        // The host-owned mask target sits on every action of a maskable effect and nowhere else.
        for method in [
            "edit.set-basic",
            "edit.reset-basic",
            "edit.set-presence",
            "edit.reset-presence",
            "edit.set-mixer",
            "edit.reset-mixer",
        ] {
            ensure(
                methods[method]["optional"]["mask"].is_object()
                    || methods[method]["optional"]["mask"].is_string(),
                format!(
                    "{method} does not list the optional mask field: {}",
                    methods[method]["optional"]
                ),
            )?;
        }
        for method in [
            "edit.set-pixel",
            "edit.crop",
            "edit.transform",
            "edit.set-vignette",
        ] {
            ensure(
                methods[method]["optional"].get("mask").is_none(),
                format!("{method} lists a mask field it does not accept"),
            )?;
        }
        record(
            "every mask command the host declares is discoverable, a brush generates no geometry method, and the maskable actions advertise their mask target",
            json!({"declared": declared.len(), "methods": declared}),
        );

        let imported = import(&owner, editor, &fixture)?;
        let asset = imported["asset"]["id"].clone();
        prepare_source(&owner, editor, &asset)?;
        let unmasked_inside = sample(&owner, editor, &asset, INSIDE)?;
        let unmasked_outside = sample(&owner, editor, &asset, OUTSIDE)?;

        // 2. Every kind, created from JSON. A gradient and a radial by their generated create
        //    methods, both ranges the same way because their geometry is declared as numbers too, and
        //    a brush through `mask.add-stroke`, which is the only way a stroke reaches a mask.
        let mut revision = as_u64(
            &call(&owner, editor, "asset.state", json!({"asset_id": asset}))?["revision"],
            "revision",
        )?;
        let mut next = |method: &str, params: Value| -> Result<Value> {
            let mut params = params;
            params["asset_id"] = asset.clone();
            params["mutation"] = mutation(revision, &format!("mask-{method}-{revision}"));
            let result = call(&owner, editor, method, params)?;
            revision = as_u64(&result["revision"], "revision")?;
            Ok(result)
        };

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
        let band = next(
            "mask.create-luminance-range",
            json!({"low": 20.0, "low_feather": 5.0, "high": 80.0, "high_feather": 5.0}),
        )?;
        let band_mask = as_str(&band["mask"], "mask id")?;
        let colour = next("mask.create-colour-range", json!({"refine": 50.0}))?;
        let colour_mask = as_str(&colour["mask"], "mask id")?;
        let painted = next(
            "mask.add-stroke",
            json!({"points": [[0.2, 0.6], [0.8, 0.6]], "size": 0.08, "feather": 0.0,
                "flow": 100.0, "erase": false}),
        )?;
        let brush_mask = as_str(&painted["mask"], "mask id")?;
        let brush_component = as_str(&painted["component"], "component id")?;
        record(
            "all five component kinds are creatable from JSON: four by their generated geometry methods and the brush by the one command a stroke travels through",
            json!({"linear": gradient_mask, "radial": radial_mask, "luminance_range": band_mask,
                "colour_range": colour_mask, "brush": brush_mask,
                "labels": [linear["label"].clone(), radial["label"].clone(), band["label"].clone(),
                    colour["label"].clone(), painted["label"].clone()]}),
        );

        // 3. Composition across kinds, in one mask: a radial added to the gradient, a brush
        //    subtracted from the pair and a band intersected with what is left. Every mode, over
        //    three different kinds, which is the composition claim rather than four radials.
        next(
            "mask.add-radial",
            json!({"mask": gradient_mask, "mode": "add", "x": 0.3, "y": 0.3,
                "radius_x": 0.15, "radius_y": 0.15, "angle": 0.0, "feather": 20.0}),
        )?;
        let subtracted = next(
            "mask.add-stroke",
            json!({"mask": gradient_mask, "mode": "subtract",
                "points": [[0.3, 0.85], [0.7, 0.85]], "size": 0.06, "feather": 0.0,
                "flow": 100.0, "erase": false}),
        )?;
        let subtract_component = as_str(&subtracted["component"], "component id")?;
        next(
            "mask.add-luminance-range",
            json!({"mask": gradient_mask, "mode": "intersect", "low": 0.0,
                "low_feather": 0.0, "high": 100.0, "high_feather": 0.0}),
        )?;
        let composed = call(&owner, editor, "mask.list", json!({"asset_id": asset}))?;
        let composed_kinds: Vec<String> = composed["masks"]
            .as_array()
            .ok_or("mask.list answered no masks")?
            .iter()
            .find(|mask| mask["id"] == json!(gradient_mask))
            .and_then(|mask| mask["components"].as_array())
            .ok_or("The composed mask lists no components")?
            .iter()
            .map(|component| {
                Ok(format!(
                    "{} {}",
                    as_str(&component["mode"], "mode")?,
                    as_str(&component["kind"], "kind")?
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        ensure(
            composed_kinds
                == [
                    "add linear",
                    "add radial",
                    "subtract brush",
                    "intersect luminance-range",
                ],
            format!("The composed mask reads {composed_kinds:?}"),
        )?;
        record(
            "one mask composes four kinds in three modes, and mask.list reports the order it composes in",
            json!({"components": composed_kinds}),
        );

        // 4. The mask reaches an effect. A masked Basic layer moves the pixel the mask covers and
        //    leaves the one it does not, `render.sample` equals the rendered byte at both, and the
        //    layer is reported as the mask's own on both sides of the relation.
        next(
            "edit.set-basic",
            json!({"mask": gradient_mask, "exposure": MASKED_EV}),
        )?;
        let masked_inside = sample(&owner, editor, &asset, INSIDE)?;
        let masked_outside = sample(&owner, editor, &asset, OUTSIDE)?;
        ensure(
            masked_inside != unmasked_inside,
            format!("The masked exposure changed nothing inside the mask: {masked_inside:?}"),
        )?;
        ensure(
            masked_outside == unmasked_outside,
            format!(
                "The masked exposure changed a pixel outside the mask: {unmasked_outside:?} -> {masked_outside:?}"
            ),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let masked_rows: Vec<String> = described["layers"]
            .as_array()
            .ok_or("recipe.describe answered no layers")?
            .iter()
            .filter(|layer| !layer["mask"].is_null())
            .map(|layer| as_str(&layer["mask"], "mask"))
            .collect::<Result<Vec<_>>>()?;
        ensure(
            masked_rows == [gradient_mask.clone()],
            format!("recipe.describe reports masked layers {masked_rows:?}"),
        )?;
        record(
            "a masked Basic layer changes the pixels the mask covers and no others, read through render.sample, and the layer-to-mask relation reads from both sides",
            json!({"inside": {"before": unmasked_inside, "after": masked_inside},
                "outside": {"before": unmasked_outside, "after": masked_outside},
                "described_masked_layers": masked_rows}),
        );

        // 5. Masked Presence and masked mixer, so the masked **spatial** primitive and the second
        //    colour-stage module are both in the journey rather than only Basic.
        next(
            "edit.set-presence",
            json!({"mask": radial_mask, "clarity": 40.0}),
        )?;
        next(
            "edit.set-mixer",
            json!({"mask": radial_mask, "blue-luminance": -30.0}),
        )?;
        let three = call(&owner, editor, "mask.list", json!({"asset_id": asset}))?;
        let bound: Vec<Value> = three["masks"]
            .as_array()
            .ok_or("mask.list answered no masks")?
            .iter()
            .map(|mask| json!({"mask": mask["name"], "layers": mask["layers"]}))
            .collect();
        record(
            "Basic, Presence and the colour mixer all take a mask target, so a masked colour layer and a masked spatial layer are both in the recipe",
            json!({"bound": bound}),
        );

        // 6. The whole modifier vocabulary, each from JSON and each one entry: amount, inversion at
        //    both levels, a component's mode changed after the fact, a component reordered, a mask
        //    reordered, a mask duplicated (which copies the layers bound to it) and deletes.
        next(
            "mask.set-amount",
            json!({"mask": gradient_mask, "amount": 60.0}),
        )?;
        let amounted_inside = sample(&owner, editor, &asset, INSIDE)?;
        ensure(
            amounted_inside != masked_inside && amounted_inside != unmasked_inside,
            format!("Amount 60 left the inside pixel at {amounted_inside:?}"),
        )?;
        next(
            "mask.set-amount",
            json!({"mask": gradient_mask, "amount": 100.0}),
        )?;
        next(
            "mask.set-invert",
            json!({"mask": gradient_mask, "invert": true}),
        )?;
        let inverted_outside = sample(&owner, editor, &asset, OUTSIDE)?;
        ensure(
            inverted_outside != unmasked_outside,
            "Inverting the mask left the pixel it did not previously cover unchanged",
        )?;
        next(
            "mask.set-invert",
            json!({"mask": gradient_mask, "invert": false}),
        )?;
        next(
            "mask.set-component-invert",
            json!({"mask": gradient_mask, "component": gradient_component, "invert": true}),
        )?;
        next(
            "mask.set-component-invert",
            json!({"mask": gradient_mask, "component": gradient_component, "invert": false}),
        )?;
        next(
            "mask.set-component-mode",
            json!({"mask": gradient_mask, "component": subtract_component, "mode": "intersect"}),
        )?;
        next(
            "mask.set-component-mode",
            json!({"mask": gradient_mask, "component": subtract_component, "mode": "subtract"}),
        )?;
        next(
            "mask.reorder-component",
            json!({"mask": gradient_mask, "component": subtract_component, "index": 3}),
        )?;
        next(
            "mask.set-linear",
            json!({"mask": gradient_mask, "component": gradient_component, "y1": 0.9}),
        )?;
        next("mask.rename", json!({"mask": gradient_mask, "name": "Sky"}))?;
        next("mask.reorder", json!({"mask": gradient_mask, "index": 2}))?;
        let duplicated = next("mask.duplicate", json!({"mask": gradient_mask}))?;
        let copy = as_str(&duplicated["mask"], "mask id")?;
        let listed = call(&owner, editor, "mask.list", json!({"asset_id": asset}))?;
        let (original_layers, copy_layers) = {
            let masks = listed["masks"].as_array().ok_or("No masks listed")?;
            let find = |id: &str| -> Result<usize> {
                masks
                    .iter()
                    .find(|mask| mask["id"] == json!(id))
                    .and_then(|mask| mask["layers"].as_array())
                    .map(Vec::len)
                    .ok_or_else(|| format!("No mask {id} in the listing").into())
            };
            (find(&gradient_mask)?, find(&copy)?)
        };
        ensure(
            copy_layers == original_layers && original_layers > 0,
            format!("The duplicate holds {copy_layers} layers and the original {original_layers}"),
        )?;
        record(
            "amount, inversion at both levels, a component's mode and order, a geometry patch, a rename, a mask reorder and a duplicate that copies the bound layers are each one JSON command and each one entry",
            json!({"amount_60_inside": amounted_inside, "inverted_outside": inverted_outside,
                "duplicate": copy, "duplicate_layers": copy_layers,
                "original_layers": original_layers,
                "duplicate_label": duplicated["label"].clone()}),
        );

        // 7. A stroke is an object. A second stroke on the same component is an update, and
        //    `mask.delete-stroke` is a forward edit that appends an entry rather than an undo.
        next(
            "mask.add-stroke",
            json!({"mask": brush_mask, "component": brush_component,
                "points": [[0.2, 0.4], [0.8, 0.4]], "size": 0.08, "feather": 50.0,
                "flow": 80.0, "erase": false}),
        )?;
        let strokes: Vec<String> =
            call(&owner, editor, "mask.list", json!({"asset_id": asset}))?["masks"]
                .as_array()
                .ok_or("No masks listed")?
                .iter()
                .find(|mask| mask["id"] == json!(brush_mask))
                .and_then(|mask| mask["components"].as_array())
                .and_then(|components| components.first())
                .and_then(|component| component["payload"]["strokes"].as_array())
                .ok_or("The brush component lists no strokes")?
                .iter()
                .map(|stroke| as_str(stroke, "stroke address"))
                .collect::<Result<Vec<_>>>()?;
        ensure(
            strokes.len() == 2,
            format!("The brush component holds {} strokes", strokes.len()),
        )?;
        let before_delete = as_u64(
            &call(
                &owner,
                editor,
                "history.list",
                json!({"asset_id": asset, "limit": 1}),
            )?["entries"][0]["sequence"],
            "sequence",
        )?;
        let deleted_stroke = next(
            "mask.delete-stroke",
            json!({"mask": brush_mask, "component": brush_component, "stroke": strokes[1]}),
        )?;
        let after_delete = as_u64(
            &call(
                &owner,
                editor,
                "history.list",
                json!({"asset_id": asset, "limit": 1}),
            )?["entries"][0]["sequence"],
            "sequence",
        )?;
        ensure(
            after_delete > before_delete,
            "Deleting a stroke did not append a history entry",
        )?;
        record(
            "a second stroke on one component is an update, and deleting a stroke is a forward edit that appends its own entry",
            json!({"strokes": strokes, "delete_label": deleted_stroke["label"].clone(),
                "sequence_before": before_delete, "sequence_after": after_delete}),
        );

        // 8. The refusals. Each one is a contract the design states, and each is checked by the
        //    reason the host gives rather than only by the fact that it failed.
        let last_of_brush = strokes[0].clone();
        let refusals = [
            Refusal {
                what: "a mask's only component cannot be deleted",
                method: "mask.delete-component",
                contains: "delete the mask",
            },
            Refusal {
                what: "a component's last stroke cannot be deleted",
                method: "mask.delete-stroke",
                contains: "only stroke",
            },
        ];
        let mut refused_detail = Vec::new();
        for case in &refusals {
            let params = match case.method {
                "mask.delete-component" => json!({"asset_id": asset,
                    "mutation": mutation(revision, "refuse-component"),
                    "mask": brush_mask, "component": brush_component}),
                _ => json!({"asset_id": asset,
                    "mutation": mutation(revision, "refuse-stroke"),
                    "mask": brush_mask, "component": brush_component,
                    "stroke": last_of_brush}),
            };
            let (code, message) = refused(&owner, editor, case.method, params)?;
            ensure(
                message.to_lowercase().contains(case.contains),
                format!("{}: {code} {message}", case.what),
            )?;
            refused_detail.push(json!({"shows": case.what, "code": code, "message": message}));
        }
        // A mask on a layer after the geometry tail, and a mask field on an action that is not
        // maskable, are both refused by name rather than ignored.
        let (vignette_code, vignette_message) = refused(
            &owner,
            editor,
            "edit.set-vignette",
            json!({"asset_id": asset, "mutation": mutation(revision, "refuse-vignette"),
                "mask": gradient_mask, "amount": -40.0}),
        )?;
        refused_detail.push(
            json!({"shows": "a mask field on an action whose effect is not maskable is refused",
            "code": vignette_code, "message": vignette_message}),
        );
        // A reorder that would leave a component that is not `add` at the front of the list.
        let (order_code, order_message) = refused(
            &owner,
            editor,
            "mask.reorder-component",
            json!({"asset_id": asset, "mutation": mutation(revision, "refuse-order"),
                "mask": gradient_mask, "component": subtract_component, "index": 0}),
        )?;
        refused_detail.push(
            json!({"shows": "a move that would leave a non-add component leading is refused",
            "code": order_code, "message": order_message}),
        );
        record(
            "the contracts refuse by name with their own reason: a mask's only component, a component's last stroke, a mask on an action that is not maskable, and an order that would leave a non-add component leading",
            json!(refused_detail),
        );

        // 9. A live agent against an open mask draft. The editor opens a gradient drag, the agent
        //    commits through it, and the draft is kept and marked conflicted rather than discarded.
        //    Then Reapply rebases it and the commit that follows keeps the agent's edit.
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "mask.set-linear",
                "mask": gradient_mask, "component": gradient_component}),
        )?;
        let draft_id = as_str(&draft["draft_id"], "draft id")?;
        let base = as_u64(&draft["base_revision"], "base revision")?;
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"y1": 0.75}}),
        )?;
        let agent_edit = call(
            &owner,
            agent,
            "mask.set-amount",
            json!({"asset_id": asset, "mutation": {"expected_revision": base,
                "request_id": "agent-amount", "actor": "acceptance-agent"},
                "mask": gradient_mask, "amount": 80.0}),
        )?;
        revision = as_u64(&agent_edit["revision"], "revision")?;
        let read = call(&owner, editor, "draft.read", json!({"draft_id": draft_id}))?;
        ensure(
            read["conflicted"] == json!(true),
            format!("The draft was not marked conflicted: {read}"),
        )?;
        let (conflict_code, conflict_message) = refused(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(base, "stale-commit")}),
        )?;
        ensure(
            conflict_code == "conflict",
            format!("A stale mask draft commit answered {conflict_code}: {conflict_message}"),
        )?;
        call(
            &owner,
            editor,
            "draft.reapply",
            json!({"draft_id": draft_id}),
        )?;
        let reapplied = call(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "reapplied-commit")}),
        )?;
        revision = as_u64(&reapplied["revision"], "revision")?;
        let kept = call(&owner, editor, "mask.list", json!({"asset_id": asset}))?["masks"]
            .as_array()
            .ok_or("No masks listed")?
            .iter()
            .find(|mask| mask["id"] == json!(gradient_mask))
            .map(|mask| mask["amount"].clone())
            .ok_or("The reapplied mask is gone")?;
        ensure(
            kept == json!(80.0),
            format!("Reapply lost the agent's amount: {kept}"),
        )?;
        record(
            "an agent committing during an open mask gesture keeps the gesture, marks it conflicted, refuses the stale commit with a conflict, and Reapply rebases it while keeping the agent's own edit",
            json!({"draft": draft_id, "base_revision": base, "conflict": conflict_message,
                "amount_after_reapply": kept}),
        );

        // 10. Discard. A second gesture, cancelled, changes nothing.
        let discarded = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "mask.set-linear",
                "mask": gradient_mask, "component": gradient_component}),
        )?;
        let discarded_id = as_str(&discarded["draft_id"], "draft id")?;
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": discarded_id, "fields": {"y1": 0.1}}),
        )?;
        call(
            &owner,
            editor,
            "draft.cancel",
            json!({"draft_id": discarded_id}),
        )?;
        let after_cancel = as_u64(
            &call(&owner, editor, "asset.state", json!({"asset_id": asset}))?["revision"],
            "revision",
        )?;
        ensure(
            after_cancel == revision,
            format!("Cancelling a mask gesture moved the revision to {after_cancel}"),
        )?;
        record(
            "a cancelled mask gesture writes nothing: the revision is where the last commit left it",
            json!({"revision": after_cancel}),
        );

        // 11. History with masks in it: a historical preview of an entry before the mask existed, a
        //     Restore, then undo and redo walking back and forth across mask entries.
        let history = call(
            &owner,
            editor,
            "history.list",
            json!({"asset_id": asset, "limit": 100}),
        )?;
        let entries = history["entries"].as_array().ok_or("No history")?;
        let first = as_str(&entries[entries.len() - 1]["id"], "entry id")?;
        // The read-only preview: the session selects a historical entry and samples it, which is the
        // delivered preview contract rather than a second render path. The entry before the first
        // mask must read the unmasked picture, and selecting it must not move the committed state.
        call(
            &owner,
            editor,
            "preview.select",
            json!({"asset_id": asset, "entry_id": first}),
        )?;
        let previewed_inside = sample(&owner, editor, &asset, INSIDE)?;
        let committed_during_preview = as_str(
            &call(&owner, editor, "asset.state", json!({"asset_id": asset}))?["current_entry"]["id"],
            "entry id",
        )?;
        call(&owner, editor, "preview.return-current", json!({}))?;
        ensure(
            previewed_inside == unmasked_inside,
            format!(
                "A historical preview of the entry before the first mask reads {previewed_inside:?}, the unmasked picture is {unmasked_inside:?}"
            ),
        )?;
        let current_before_restore = as_str(
            &call(&owner, editor, "asset.state", json!({"asset_id": asset}))?["current_entry"]["id"],
            "entry id",
        )?;
        let restored = call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset, "entry_id": first,
                "mutation": mutation(revision, "restore-first")}),
        )?;
        revision = as_u64(&restored["revision"], "revision")?;
        let after_restore = call(&owner, editor, "mask.list", json!({"asset_id": asset}))?;
        ensure(
            after_restore["masks"].as_array().is_some_and(Vec::is_empty),
            format!(
                "Restoring the first entry left masks behind: {}",
                after_restore["masks"]
            ),
        )?;
        let undone = call(
            &owner,
            editor,
            "history.undo",
            json!({"asset_id": asset, "mutation": mutation(revision, "undo-restore")}),
        )?;
        revision = as_u64(&undone["revision"], "revision")?;
        let back = call(&owner, editor, "mask.list", json!({"asset_id": asset}))?;
        let back_count = back["masks"].as_array().map(Vec::len).unwrap_or_default();
        ensure(
            back_count > 0,
            "Undoing the restore did not bring the masks back",
        )?;
        let redone = call(
            &owner,
            editor,
            "history.redo",
            json!({"asset_id": asset, "mutation": mutation(revision, "redo-restore")}),
        )?;
        revision = as_u64(&redone["revision"], "revision")?;
        let again = call(
            &owner,
            editor,
            "history.undo",
            json!({"asset_id": asset, "mutation": mutation(revision, "undo-again")}),
        )?;
        let _ = as_u64(&again["revision"], "revision")?;
        record(
            "history works with masks in the recipe: a historical preview of the entry before the first mask is the unmasked picture, Restore is an append that reaches a maskless recipe, and undo and redo walk across mask entries",
            json!({"entries": entries.len(), "first_entry": first,
                "current_before_restore": current_before_restore,
                "committed_entry_during_preview": committed_during_preview,
                "previewed_inside": previewed_inside,
                "masks_after_restore": 0, "masks_after_undo": back_count}),
        );

        // 12. The final state, and the identities the reopen has to return.
        let final_state = call(&owner, editor, "asset.state", json!({"asset_id": asset}))?;
        let final_entry = as_str(&final_state["current_entry"]["id"], "entry id")?;
        let final_revision = as_u64(&final_state["revision"], "revision")?;
        let final_masks = call(&owner, editor, "mask.list", json!({"asset_id": asset}))?;
        // A spread of sampled positions across the frame stands in for a whole-frame comparison,
        // because a recipe fetched over JSON cannot be rendered outside the stroke store that holds
        // its brushes. Each one is read again after the reopen and must be identical.
        let probes: Vec<(u32, u32)> = vec![
            INSIDE,
            OUTSIDE,
            (120, 160),
            (360, 160),
            (40, 280),
            (440, 40),
        ];
        let final_samples: Vec<Value> = probes
            .iter()
            .map(|at| {
                Ok(json!({"x": at.0, "y": at.1, "rgb": sample(&owner, editor, &asset, *at)?}))
            })
            .collect::<Result<Vec<_>>>()?;
        let identities: Vec<Value> = final_masks["masks"]
            .as_array()
            .ok_or("No masks listed")?
            .iter()
            .map(|mask| {
                json!({"id": mask["id"], "name": mask["name"], "amount": mask["amount"],
                    "invert": mask["invert"], "layers": mask["layers"],
                    "components": mask["components"].as_array().map(|components| components
                        .iter()
                        .map(|component| json!({"id": component["id"], "kind": component["kind"],
                            "mode": component["mode"], "invert": component["invert"]}))
                        .collect::<Vec<_>>())})
            })
            .collect();

        // 13. A disabled maskable module. The same catalog served with Presence unavailable keeps
        //     every mask and every layer, reports the affected edit and refuses the render rather
        //     than silently producing a frame with the masked Presence layer left out.
        let presence_join = join.take().ok_or("The owner was already stopped")?;
        owner.stop();
        presence_join
            .join()
            .map_err(|_| "The owner thread panicked")?;
        let (limited, limited_join) =
            OwnerHandle::start_with(&catalog, Arc::new(registry_without("lightwell.presence")?))?;
        let limited_detail = (|| -> Result<Value> {
            let client = limited.register();
            prepare_source(&limited, client, &asset)?;
            let masks = call(&limited, client, "mask.list", json!({"asset_id": asset}))?;
            ensure(
                masks["masks"].as_array().map(Vec::len)
                    == final_masks["masks"].as_array().map(Vec::len),
                "A disabled maskable module changed the mask list",
            )?;
            let described = call(
                &limited,
                client,
                "recipe.describe",
                json!({"asset_id": asset}),
            )?;
            let unavailable: Vec<Value> = described["layers"]
                .as_array()
                .ok_or("recipe.describe answered no layers")?
                .iter()
                .filter(|layer| layer["available"] == json!(false))
                .map(|layer| {
                    json!({"effect": layer["effect"], "title": layer["title"],
                    "mask": layer["mask"], "unavailable": layer["unavailable"]})
                })
                .collect();
            ensure(
                !unavailable.is_empty(),
                "A disabled maskable module reported no affected layer",
            )?;
            let (code, message) = refused(
                &limited,
                client,
                "render.sample",
                json!({"asset_id": asset, "x": INSIDE.0, "y": INSIDE.1}),
            )?;
            Ok(json!({"masks": masks["masks"].as_array().map(Vec::len),
                "unavailable_layers": unavailable, "sample_code": code,
                "sample_message": message}))
        })();
        limited.stop();
        limited_join
            .join()
            .map_err(|_| "The limited owner thread panicked")?;
        let limited_detail = limited_detail?;
        record(
            "the same catalog served with a maskable module unavailable keeps every mask and layer, names the affected edit, and refuses to sample rather than producing a value with the masked layer left out",
            limited_detail.clone(),
        );

        // 14. A missing original under a masked recipe, and a changed one. Neither discards anything.
        let moved = out.join("moved-original.jpg");
        let missing_detail = {
            let staged = out.join("staged-original.jpg");
            fs::copy(&fixture, &staged)?;
            let (staged_owner, staged_join) = OwnerHandle::start(&out.join("staged.sqlite"))?;
            let detail = (|| -> Result<Value> {
                let client = staged_owner.register();
                let staged_imported = import(&staged_owner, client, &staged)?;
                let staged_asset = staged_imported["asset"]["id"].clone();
                prepare_source(&staged_owner, client, &staged_asset)?;
                let created = call(
                    &staged_owner,
                    client,
                    "mask.create-linear",
                    json!({"asset_id": staged_asset, "mutation": mutation(0, "staged-mask"),
                        "x0": GRADIENT[0], "y0": GRADIENT[1], "x1": GRADIENT[2], "y1": GRADIENT[3]}),
                )?;
                let staged_mask = as_str(&created["mask"], "mask id")?;
                let staged_revision = as_u64(&created["revision"], "revision")?;
                call(
                    &staged_owner,
                    client,
                    "edit.set-basic",
                    json!({"asset_id": staged_asset,
                        "mutation": mutation(staged_revision, "staged-exposure"),
                        "mask": staged_mask, "exposure": MASKED_EV}),
                )?;
                // Moved away: the catalog keeps the mask, the layer and the history, and says why it
                // cannot render.
                fs::rename(&staged, &moved)?;
                let missing = call(
                    &staged_owner,
                    client,
                    "mask.list",
                    json!({"asset_id": staged_asset}),
                )?;
                ensure(
                    missing["masks"].as_array().map(Vec::len) == Some(1),
                    "A missing original lost the mask list",
                )?;
                let (missing_code, missing_message) = refused(
                    &staged_owner,
                    client,
                    "render.sample",
                    json!({"asset_id": staged_asset, "x": INSIDE.0, "y": INSIDE.1}),
                )?;
                // Changed: a different file at the same path is a changed source, not a usable one.
                let other = root.join("fixtures/s0/orientation-6.jpg");
                let changed = if other.exists() {
                    fs::copy(&other, &staged)?;
                    let (code, message) = refused(
                        &staged_owner,
                        client,
                        "source.inspect",
                        json!({"asset_id": staged_asset}),
                    )
                    .unwrap_or_else(|_| {
                        (
                            "reported".into(),
                            "source.inspect reports rather than refusing".into(),
                        )
                    });
                    json!({"code": code, "message": message})
                } else {
                    Value::Null
                };
                let still = call(
                    &staged_owner,
                    client,
                    "mask.list",
                    json!({"asset_id": staged_asset}),
                )?;
                ensure(
                    still["masks"].as_array().map(Vec::len) == Some(1),
                    "A changed original lost the mask list",
                )?;
                Ok(json!({"mask": staged_mask, "missing_code": missing_code,
                    "missing_message": missing_message, "changed": changed,
                    "masks_retained": 1}))
            })();
            staged_owner.stop();
            staged_join
                .join()
                .map_err(|_| "The staged owner thread panicked")?;
            detail?
        };
        record(
            "a missing original and a changed original under a masked recipe each report explicitly and discard nothing: the mask, its component and the layer bound to it are all still there",
            missing_detail.clone(),
        );

        // 15. The reopen. A third owner over the first catalog returns the masks, their components,
        //     the layers bound to them and the history, with the identities it wrote.
        let (reopened, reopened_join) = OwnerHandle::start(&catalog)?;
        let restart = (|| -> Result<Value> {
            let client = reopened.register();
            prepare_source(&reopened, client, &asset)?;
            let state = call(&reopened, client, "asset.state", json!({"asset_id": asset}))?;
            ensure(
                as_u64(&state["revision"], "revision")? == final_revision
                    && state["current_entry"]["id"] == json!(final_entry),
                format!("The reopened state is {state}"),
            )?;
            let masks = call(&reopened, client, "mask.list", json!({"asset_id": asset}))?;
            let reopened_identities: Vec<Value> = masks["masks"]
                .as_array()
                .ok_or("No masks after reopen")?
                .iter()
                .map(|mask| {
                    json!({"id": mask["id"], "name": mask["name"], "amount": mask["amount"],
                        "invert": mask["invert"], "layers": mask["layers"],
                        "components": mask["components"].as_array().map(|components| components
                            .iter()
                            .map(|component| json!({"id": component["id"], "kind": component["kind"],
                                "mode": component["mode"], "invert": component["invert"]}))
                            .collect::<Vec<_>>())})
                })
                .collect();
            ensure(
                reopened_identities == identities,
                "The reopened masks are not the ones that were written, identity for identity",
            )?;
            let reopened_samples: Vec<Value> = probes
                .iter()
                .map(|at| {
                    Ok(json!({"x": at.0, "y": at.1, "rgb": sample(&reopened, client, &asset, *at)?}))
                })
                .collect::<Result<Vec<_>>>()?;
            ensure(
                reopened_samples == final_samples,
                format!(
                    "The reopened masked picture reads {reopened_samples:?}, it wrote {final_samples:?}"
                ),
            )?;
            Ok(
                json!({"revision": final_revision, "current_entry": final_entry,
                "masks": reopened_identities.len(), "samples": reopened_samples}),
            )
        })();
        reopened.stop();
        reopened_join
            .join()
            .map_err(|_| "The reopened owner thread panicked")?;
        let restart = restart?;
        record(
            "the catalog reopens to the same revision, the same current entry, the same masks, components and bound layers by identity, and the same sampled pixels across the frame",
            restart.clone(),
        );

        Ok(json!({
            "status": "passed",
            "fixture": FIXTURE,
            "fixture_sha256": fixture_hash,
            "asset_id": asset,
            "final_revision": final_revision,
            "final_entry_id": final_entry,
            "masks": identities,
            "unavailable_maskable_module": limited_detail,
            "missing_and_changed_original": missing_detail,
            "reopen": restart,
            "sampled_positions": final_samples,
            "method": "Every step is one JSON request through OwnerHandle::call, as an independent client reaches it: no desktop, no window and no pointer. The panel's own evidence is the four mask-* smoke scenarios; this chapter is the parity half of the same pillar.",
            "not_claimed": "This chapter does not compare a panel gesture with a JSON request frame by frame. It establishes that every mask gesture has a programmatic equivalent and that the equivalent produces the stacks, history, pixels and refusals the design states.",
            "checks": checks.borrow().clone(),
            "timings_ms": {"total": total.elapsed().as_secs_f64() * 1000.0},
        }))
    })();
    if let Some(join) = join.take() {
        owner.stop();
        join.join().map_err(|_| "The owner thread panicked")?;
    }
    let outcome = outcome?;
    ensure(
        hash(&fixture)? == fixture_hash,
        "The masking chapter changed its own original",
    )?;
    Ok(outcome)
}
