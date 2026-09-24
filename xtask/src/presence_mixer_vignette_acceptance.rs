//! The Presence, colour mixer and vignette chapter of `cargo xtask editor-acceptance`: what each of
//! the three modules does that no other module does, driven through the JSON method table with
//! [`OwnerHandle::call`] exactly as an independent client reaches it — Presence's place after the
//! colour run, the mixer's order after Basic, and the vignette recentring on the stage a crop update
//! produces.
//!
//! The host behaviour the three share with every field-patch module — discovery, drafts, no-ops,
//! deduplication, resets, one layer per target, history, sample equal to render on both paths, an
//! unavailable provider and reopen — is the field-patch conformance chapter's
//! ([`crate::conformance`]), which runs the same suite the core's own test does. Their numerics
//! against their frozen references are the core's tests.
use crate::basic_acceptance::{
    FIXTURE, call, current_recipe, current_revision, import, mutation, render,
};
use crate::*;
use lightwell_core::{
    BASIC_EFFECT, ClientId, MIXER_EFFECT, ORIENTATION_EFFECT, OwnerHandle, PRESENCE_EFFECT,
    SourceImage, VIGNETTE_EFFECT,
};
use std::time::Instant;

/// The centre of the fixture's blue quadrant (`xtask/src/fixtures.rs`'s pattern), where the mixer's
/// blue range and Basic's exposure both move the byte.
const BLUE_PROBE: (u32, u32) = (120, 240);

/// The chapter's entry point, wired into `editor-acceptance` after the field-patch conformance
/// chapter. Runs each module's section against its own catalog in `out`.
pub fn run(root: &Path, out: &Path) -> Result<Value> {
    let fixture = root.join(FIXTURE);
    let fixture_hash = hash(&fixture)?;
    let total = Instant::now();
    let presence = section(
        &fixture,
        &out.join("presence-catalog.sqlite"),
        presence_placement,
    )?;
    let mixer = section(&fixture, &out.join("mixer-catalog.sqlite"), mixer_order)?;
    let vignette = section(
        &fixture,
        &out.join("vignette-catalog.sqlite"),
        vignette_recentring,
    )?;
    ensure(
        hash(&fixture)? == fixture_hash,
        "The original source changed",
    )?;
    Ok(json!({
        "status": "passed",
        "fixture": FIXTURE,
        "fixture_sha256": fixture_hash,
        "presence": presence,
        "mixer": mixer,
        "vignette": vignette,
        "generic": "the host behaviour these modules share with every field-patch module is proved under field_patch_conformance",
        "elapsed_ms": total.elapsed().as_secs_f64() * 1000.0,
    }))
}

/// What one module's section receives: the owner, the editing client, the imported asset, its
/// Original entry and the decoded fixture, and where it records what it showed.
type Section =
    fn(&OwnerHandle, ClientId, &Value, &Value, &SourceImage, &mut dyn FnMut(&str, Value)) -> Result;

/// One module's section against its own catalog: a new owner, one client and the fixture imported
/// once. The owner is stopped however the section ends.
fn section(fixture: &Path, catalog: &Path, run: Section) -> Result<Value> {
    let source = lightwell_core::open_source(fixture)?;
    let total = Instant::now();
    let mut checks = Vec::new();
    let (owner, join) = OwnerHandle::start(catalog)?;
    let outcome = (|| -> Result {
        let editor = owner.register();
        let imported = import(&owner, editor, fixture)?;
        let asset = imported["asset"]["id"].clone();
        let original = imported["current_entry"]["id"].clone();
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

/// Return an asset to its Original entry (empty layers), so a case starts from a known, clean slate
/// on the one asset a section reuses. `history.restore` is a no-op when the asset is already there.
fn restore_to_original(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &Value,
    original: &Value,
    request_id: &str,
) -> Result {
    let revision = current_revision(owner, client, asset)?;
    call(
        owner,
        client,
        "history.restore",
        json!({"asset_id": asset, "mutation": mutation(revision, request_id), "entry_id": original}),
    )?;
    ensure(
        current_recipe(owner, client, asset)?.layers.is_empty(),
        "Restoring the Original entry left layers behind",
    )
}

fn presence_placement(
    owner: &OwnerHandle,
    editor: ClientId,
    asset: &Value,
    original: &Value,
    _: &SourceImage,
    record: &mut dyn FnMut(&str, Value),
) -> Result {
    // Presence always follows the colour run (Basic, then the mixer) whichever order the three
    // actions are touched in, and a later rotate joins the geometry tail after it.
    let payload_for = |action: &str| -> Value {
        match action {
            "set-basic" => json!({"exposure": 0.3}),
            "set-mixer" => json!({"red-hue": 10.0}),
            _ => json!({"texture": 25.0}),
        }
    };
    let mut placements = Vec::new();
    for order in [
        ["set-basic", "set-mixer", "set-presence"],
        ["set-presence", "set-basic", "set-mixer"],
        ["set-mixer", "set-presence", "set-basic"],
        ["set-presence", "set-mixer", "set-basic"],
    ] {
        let tag = order.join("-");
        restore_to_original(
            owner,
            editor,
            asset,
            original,
            &format!("placement-reset-{tag}"),
        )?;
        for action in order {
            let revision = current_revision(owner, editor, asset)?;
            let mut params = payload_for(action);
            params["asset_id"] = asset.clone();
            params["mutation"] = mutation(revision, &format!("placement-{tag}-{action}"));
            call(owner, editor, &format!("edit.{action}"), params)?;
        }
        let revision = current_revision(owner, editor, asset)?;
        call(
            owner,
            editor,
            "edit.transform",
            json!({"asset_id": asset, "mutation": mutation(revision, &format!("placement-rotate-{tag}")), "transform": "rotate-right"}),
        )?;
        let described = call(owner, editor, "recipe.describe", json!({"asset_id": asset}))?;
        let effects: Vec<Value> = described["layers"]
            .as_array()
            .ok_or("recipe.describe answered no layers")?
            .iter()
            .map(|layer| layer["effect"].clone())
            .collect();
        ensure(
            effects
                == vec![
                    json!(BASIC_EFFECT),
                    json!(MIXER_EFFECT),
                    json!(PRESENCE_EFFECT),
                    json!(ORIENTATION_EFFECT),
                ],
            format!("touch order {order:?}: the stack is ordered {effects:?}"),
        )?;
        placements.push(json!({"touch_order": order, "stack_order": effects}));
    }
    record(
        "committing Basic, the mixer and presence in every touch order, then a rotate: recipe.describe always orders Basic, mixer, presence and finally the orientation layer",
        json!({"placements": placements}),
    );
    Ok(())
}

fn mixer_order(
    owner: &OwnerHandle,
    editor: ClientId,
    asset: &Value,
    original: &Value,
    _: &SourceImage,
    record: &mut dyn FnMut(&str, Value),
) -> Result {
    // The mixer always follows Basic, whichever was touched first.
    let mixer_field = json!({"blue-saturation": 50.0});
    let basic_field = json!({"exposure": 0.4});
    let mut orders = Vec::new();
    let mut final_samples = Vec::new();
    for (case, first_action, first_field, second_action, second_field) in [
        (
            "mixer then Basic",
            "edit.set-mixer",
            mixer_field.clone(),
            "edit.set-basic",
            basic_field.clone(),
        ),
        (
            "Basic then mixer",
            "edit.set-basic",
            basic_field.clone(),
            "edit.set-mixer",
            mixer_field.clone(),
        ),
    ] {
        // Each case starts from the Original entry, on the one asset this chapter reuses.
        restore_to_original(
            owner,
            editor,
            asset,
            original,
            &format!("order-{case}-reset"),
        )?;
        let ordering_asset = asset.clone();
        let revision = current_revision(owner, editor, &ordering_asset)?;
        let mut params = first_field;
        params["asset_id"] = ordering_asset.clone();
        params["mutation"] = mutation(revision, &format!("order-{case}-first"));
        call(owner, editor, first_action, params)?;
        let revision = current_revision(owner, editor, &ordering_asset)?;
        let mut params = second_field;
        params["asset_id"] = ordering_asset.clone();
        params["mutation"] = mutation(revision, &format!("order-{case}-second"));
        call(owner, editor, second_action, params)?;
        let described = call(
            owner,
            editor,
            "recipe.describe",
            json!({"asset_id": ordering_asset}),
        )?;
        let effects: Vec<Value> = described["layers"]
            .as_array()
            .ok_or("recipe.describe answered no layers")?
            .iter()
            .map(|layer| layer["effect"].clone())
            .collect();
        ensure(
            effects == vec![json!(BASIC_EFFECT), json!(MIXER_EFFECT)],
            format!("{case}: the stack is ordered {effects:?}"),
        )?;
        orders.push(json!({"case": case, "order": effects}));
        final_samples.push(
            call(
                owner,
                editor,
                "render.sample",
                json!({"asset_id": ordering_asset, "x": BLUE_PROBE.0, "y": BLUE_PROBE.1}),
            )?["rgba"]
                .clone(),
        );
    }
    ensure(
        final_samples[0] == final_samples[1],
        format!(
            "The two touch orders rendered different bytes: {:?} against {:?}",
            final_samples[0], final_samples[1]
        ),
    )?;
    record(
        "committing the mixer then Basic, and Basic then the mixer, on two fresh assets: recipe.describe always orders Basic before the mixer and the rendered bytes are identical",
        json!({"orders": orders, "sample": final_samples[0]}),
    );
    Ok(())
}

fn vignette_recentring(
    owner: &OwnerHandle,
    editor: ClientId,
    asset: &Value,
    _: &Value,
    source: &SourceImage,
    record: &mut dyn FnMut(&str, Value),
) -> Result {
    // A vignette recomputes its mask on the stage a crop produces, not the one it was committed
    // over. The centre is invariant on either stage (mask 0), a corner darkens on either stage, and
    // this holds again after the crop moves.
    let recentre_asset = asset.clone();
    let revision = current_revision(owner, editor, &recentre_asset)?;
    call(
        owner,
        editor,
        "edit.crop",
        json!({"asset_id": recentre_asset, "mutation": mutation(revision, "recentre-crop-a"), "angle": 0.0, "x": 0.1, "y": 0.1, "width": 0.6, "height": 0.6}),
    )?;
    // Dimensions come from an independently rendered raster of the committed recipe, not a
    // guess: the JSON API has no dedicated "current stage size" method, and `render.sample`
    // itself refuses an out-of-range pixel, so a wrong guess would fail loudly rather than
    // silently, but the render is authoritative and needs no guess at all.
    let stage_a_raster = render(source, &current_recipe(owner, editor, &recentre_asset)?)?;
    let (stage_a_w, stage_a_h) = (
        u64::from(stage_a_raster.width),
        u64::from(stage_a_raster.height),
    );
    let corner_a = ((stage_a_w.max(2) - 1) as u32, (stage_a_h.max(2) - 1) as u32);
    let centre_a = ((stage_a_w / 2) as u32, (stage_a_h / 2) as u32);
    let baseline_a_centre = call(
        owner,
        editor,
        "render.sample",
        json!({"asset_id": recentre_asset, "x": centre_a.0, "y": centre_a.1}),
    )?["rgba"]
        .clone();
    let baseline_a_corner = call(
        owner,
        editor,
        "render.sample",
        json!({"asset_id": recentre_asset, "x": corner_a.0, "y": corner_a.1}),
    )?["rgba"]
        .clone();

    let revision = current_revision(owner, editor, &recentre_asset)?;
    call(
        owner,
        editor,
        "edit.set-vignette",
        json!({"asset_id": recentre_asset, "mutation": mutation(revision, "recentre-vignette"), "amount": -60.0}),
    )?;
    let described = call(
        owner,
        editor,
        "recipe.describe",
        json!({"asset_id": recentre_asset}),
    )?;
    let recentre_layers = described["layers"].as_array().ok_or("no layers")?;
    ensure(
        recentre_layers.last().ok_or("no layers")?["effect"] == json!(VIGNETTE_EFFECT),
        format!("The vignette is not the last layer: {recentre_layers:?}"),
    )?;
    let vignetted_a_centre = call(
        owner,
        editor,
        "render.sample",
        json!({"asset_id": recentre_asset, "x": centre_a.0, "y": centre_a.1}),
    )?["rgba"]
        .clone();
    let vignetted_a_corner = call(
        owner,
        editor,
        "render.sample",
        json!({"asset_id": recentre_asset, "x": corner_a.0, "y": corner_a.1}),
    )?["rgba"]
        .clone();
    ensure(
        vignetted_a_centre == baseline_a_centre,
        format!(
            "The vignette moved the centre pixel: {vignetted_a_centre} against the pre-vignette {baseline_a_centre}"
        ),
    )?;
    ensure(
        vignetted_a_corner != baseline_a_corner,
        "The vignette left its corner completely unchanged",
    )?;

    // Move the crop: the vignette stays last and recentres on the new stage.
    let revision = current_revision(owner, editor, &recentre_asset)?;
    call(
        owner,
        editor,
        "edit.crop",
        json!({"asset_id": recentre_asset, "mutation": mutation(revision, "recentre-crop-b"), "angle": 0.0, "x": 0.2, "y": 0.2, "width": 0.4, "height": 0.4}),
    )?;
    let described = call(
        owner,
        editor,
        "recipe.describe",
        json!({"asset_id": recentre_asset}),
    )?;
    let recentre_layers = described["layers"].as_array().ok_or("no layers")?;
    ensure(
        recentre_layers.last().ok_or("no layers")?["effect"] == json!(VIGNETTE_EFFECT),
        format!("The vignette is no longer last after the crop moved: {recentre_layers:?}"),
    )?;
    let stage_b_raster = render(source, &current_recipe(owner, editor, &recentre_asset)?)?;
    let (stage_b_w, stage_b_h) = (
        u64::from(stage_b_raster.width),
        u64::from(stage_b_raster.height),
    );
    let corner_b = ((stage_b_w.max(2) - 1) as u32, (stage_b_h.max(2) - 1) as u32);
    let centre_b = ((stage_b_w / 2) as u32, (stage_b_h / 2) as u32);
    let with_vignette_b_centre = call(
        owner,
        editor,
        "render.sample",
        json!({"asset_id": recentre_asset, "x": centre_b.0, "y": centre_b.1}),
    )?["rgba"]
        .clone();
    let with_vignette_b_corner = call(
        owner,
        editor,
        "render.sample",
        json!({"asset_id": recentre_asset, "x": corner_b.0, "y": corner_b.1}),
    )?["rgba"]
        .clone();

    // The amount-0 baseline for the *new* stage, so the comparison is against the stage the
    // crop update actually produced, not the pre-crop 96x64-style stage from before.
    let revision = current_revision(owner, editor, &recentre_asset)?;
    call(
        owner,
        editor,
        "edit.set-vignette",
        json!({"asset_id": recentre_asset, "mutation": mutation(revision, "recentre-vignette-off"), "amount": 0.0}),
    )?;
    let baseline_b_centre = call(
        owner,
        editor,
        "render.sample",
        json!({"asset_id": recentre_asset, "x": centre_b.0, "y": centre_b.1}),
    )?["rgba"]
        .clone();
    let baseline_b_corner = call(
        owner,
        editor,
        "render.sample",
        json!({"asset_id": recentre_asset, "x": corner_b.0, "y": corner_b.1}),
    )?["rgba"]
        .clone();
    ensure(
        with_vignette_b_centre == baseline_b_centre,
        format!(
            "After the crop moved, the vignette still moved the new stage's centre pixel: {with_vignette_b_centre} against {baseline_b_centre}"
        ),
    )?;
    ensure(
        with_vignette_b_corner != baseline_b_corner,
        "After the crop moved, the vignette's corner went back to unchanged",
    )?;
    record(
        "a vignette recomputes its mask on the stage a crop update produces: the centre pixel is invariant and a corner darkens on both the original and the moved stage",
        json!({
            "stage_a": [stage_a_w, stage_a_h],
            "stage_b": [stage_b_w, stage_b_h],
            "vignetted_a_centre": vignetted_a_centre,
            "vignetted_a_corner": vignetted_a_corner,
            "with_vignette_b_centre": with_vignette_b_centre,
            "with_vignette_b_corner": with_vignette_b_corner,
        }),
    );
    Ok(())
}
