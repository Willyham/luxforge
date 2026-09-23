//! The Presence, colour mixer and vignette chapter of `cargo xtask editor-acceptance` (TASK-009).
//!
//! Everything here is driven through the JSON method table with [`OwnerHandle::call`], exactly as
//! an independent client reaches it, against the delivered `lightwell.presence`, `lightwell.mixer`
//! and `lightwell.vignette` modules — the same style [`basic_acceptance`] uses for Basic, and this
//! file reuses that chapter's helpers (`call`, `refused`, `mutation`, `import`, `current_recipe`,
//! `current_revision`, `field`, `render`, `analyse`, `ready_report`, `prepare_source`,
//! `registry_without`) rather than duplicating them.
//!
//! Every check below is a real round trip against the background editor; nothing here is skipped
//! silently — a check that cannot run fails the command, exactly as the M1-M4 and Basic chapters
//! already do.
use crate::basic_acceptance::{
    FIXTURE, analyse, as_str, as_u64, call, current_recipe, current_revision, field, import,
    mutation, prepare_source, ready_report, refused, registry_without, render,
};
use crate::*;
use lightwell_core::{
    BASIC_EFFECT, ClientId, EFFECT_FORMAT, MIXER_EFFECT, ORIENTATION_EFFECT, OwnerHandle,
    PRESENCE_EFFECT, VIGNETTE_EFFECT,
};
use serde_json::Map;
use std::{cell::RefCell, sync::Arc, time::Instant};

/// Every column/row probe this chapter samples on the unedited 480x320 fixture: the centre of each
/// quadrant of `fixtures/s0/orientation-1.jpg` (red top-left, green top-right, blue bottom-left,
/// yellow bottom-right; `xtask/src/fixtures.rs`'s `COLORS`/`pattern` are the generator, reused
/// unedited here as a synthetic, checked-in JPEG), so a mixer range's own quadrant is always in
/// reach of a probe.
const RED_PROBE: (u32, u32) = (120, 80);
const GREEN_PROBE: (u32, u32) = (360, 80);
const BLUE_PROBE: (u32, u32) = (120, 240);
const YELLOW_PROBE: (u32, u32) = (360, 240);
const QUADRANT_PROBES: [(u32, u32); 4] = [RED_PROBE, GREEN_PROBE, BLUE_PROBE, YELLOW_PROBE];

/// The four corners and centre of the unedited 480x320 fixture, used for the vignette's own
/// render.sample-versus-raster check, since a vignette's value is most distinctive at the corners.
const CORNERS_AND_CENTRE: [(u32, u32); 5] = [(0, 0), (479, 0), (0, 319), (479, 319), (240, 160)];

/// The quadrant centres plus every corner and the stage centre: Presence is a spatial layer, so a
/// sample through it evaluates the stage-aligned tile that contains its pixel rather than a single
/// point, and a tile's edge and the stage's own edge are exactly where a tiling bug would show.
/// This chapter checks render.sample against the independently rendered raster at every one of
/// these nine interior and edge/corner pixels, not only a convenient interior handful.
const PRESENCE_PROBES: [(u32, u32); 9] = [
    RED_PROBE,
    GREEN_PROBE,
    BLUE_PROBE,
    YELLOW_PROBE,
    (0, 0),
    (479, 0),
    (0, 319),
    (479, 319),
    (240, 160),
];

/// The chapter's entry point, wired into `editor-acceptance` after the Basic and histogram chapter.
/// Runs the presence, mixer and vignette journeys each against their own catalog in `out`.
pub fn run(root: &Path, out: &Path) -> Result<Value> {
    let fixture = root.join(FIXTURE);
    let fixture_hash = hash(&fixture)?;
    let total = Instant::now();

    let presence = presence_journey(root, out)?;
    let mixer = mixer_journey(root, out)?;
    let vignette = vignette_journey(root, out)?;

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
        "elapsed_ms": total.elapsed().as_secs_f64() * 1000.0,
    }))
}

// -------------------------------------------------------------------------------------------
// Small helpers shared by all three journeys below (module-specific, so kept local rather than
// pushed into `basic_acceptance`, which owns only the helpers Basic and this chapter share).
// -------------------------------------------------------------------------------------------

/// The one layer of a described entry matching `effect_id`, with its identity, index and effective
/// values — the generic form of `basic_acceptance::described_basic`.
fn described_layer(described: &Value, effect_id: &str) -> Result<(String, usize, Value)> {
    let layers = described["layers"]
        .as_array()
        .ok_or("recipe.describe answered no layers")?;
    let index = layers
        .iter()
        .position(|layer| layer["effect"] == json!(effect_id))
        .ok_or_else(|| format!("The described entry holds no {effect_id} layer"))?;
    Ok((
        as_str(&layers[index]["id"], "layer id")?,
        index,
        layers[index]["values"].clone(),
    ))
}

/// `render.sample` at every probe, as a `{"x,y": [r,g,b,a]}`-shaped map keyed by the probe's index,
/// so a caller can compare two captures probe for probe.
fn sample_probes(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &Value,
    probes: &[(u32, u32)],
) -> Result<Vec<Value>> {
    let mut samples = Vec::with_capacity(probes.len());
    for (x, y) in probes {
        let sample = call(
            owner,
            client,
            "render.sample",
            json!({"asset_id": asset, "x": x, "y": y}),
        )?;
        samples.push(sample["rgba"].clone());
    }
    Ok(samples)
}

/// Every probe in `expected` must equal the newly sampled value, by position.
fn expect_probes(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &Value,
    probes: &[(u32, u32)],
    expected: &[Value],
    what: &str,
) -> Result {
    let actual = sample_probes(owner, client, asset, probes)?;
    ensure(
        actual == expected,
        format!("{what}: probes read {actual:?}, expected {expected:?}"),
    )
}

/// `render.sample` at every probe equals the byte the independently rendered raster holds at that
/// same pixel, for the asset's current committed recipe: the same cross-check
/// `basic_acceptance::run` makes for Basic, generalized to any probe list.
fn render_matches_raster(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &Value,
    source: &lightwell_core::SourceImage,
    probes: &[(u32, u32)],
) -> Result<Value> {
    let recipe = current_recipe(owner, client, asset)?;
    let raster = render(source, &recipe)?;
    let mut checked = Vec::new();
    for (x, y) in probes {
        let sample = call(
            owner,
            client,
            "render.sample",
            json!({"asset_id": asset, "x": x, "y": y}),
        )?;
        let raster_pixel = raster
            .pixel(*x, *y)
            .ok_or_else(|| format!("The raster has no pixel at {x},{y}"))?;
        ensure(
            sample["rgba"] == json!(raster_pixel),
            format!(
                "render.sample at {x},{y} answered {} while the independently rendered raster holds {raster_pixel:?}",
                sample["rgba"]
            ),
        )?;
        checked.push(json!({"pixel": [x, y], "rgba": raster_pixel}));
    }
    Ok(json!({"probes": checked}))
}

/// The decoded source's own byte at one pixel (alpha 255), used to check a neutral layer's render
/// keeps the exact identity byte path.
fn source_pixel(source: &lightwell_core::SourceImage, x: u32, y: u32) -> Value {
    let index = ((y as usize) * source.width as usize + x as usize) * 4;
    json!([
        source.rgba[index],
        source.rgba[index + 1],
        source.rgba[index + 2],
        source.rgba[index + 3],
    ])
}

/// One `analysis.request`/`read` on the current stack and one on a historical entry: both settle
/// `ready` and their identities differ, since the two stacks differ.
fn distinct_analysis_identities(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &Value,
    historical_entry: &Value,
) -> Result<Value> {
    let current = ready_report(
        owner,
        client,
        asset,
        json!({"kind": "current"}),
        "the current stack",
    )?;
    let historical = ready_report(
        owner,
        client,
        asset,
        json!({"kind": "entry", "entry_id": historical_entry}),
        "the historical entry",
    )?;
    ensure(
        current["identity"] != historical["identity"],
        format!(
            "The current stack and a historical entry share one analysis identity: {} against {}",
            current["identity"], historical["identity"]
        ),
    )?;
    Ok(
        json!({"current_identity": current["identity"], "historical_identity": historical["identity"]}),
    )
}

/// The unavailable-provider check, shared by both journeys: the same catalog served with
/// `module_id` disabled still lists the layer (unavailable) and the module (unavailable), refuses to
/// render or sample the stack that names it with `incompatible` naming `effect_id`, and reports the
/// analysis failed with no counts.
fn unavailable_provider_check(
    catalog: &Path,
    module_id: &str,
    effect_id: &str,
    asset: &Value,
    entry_id: &Value,
) -> Result<Value> {
    let (limited, limited_join) =
        OwnerHandle::start_with(catalog, Arc::new(registry_without(module_id)))?;
    let outcome = (|| -> Result<Value> {
        let client = limited.register();
        prepare_source(&limited, client, asset)?;
        let described = call(
            &limited,
            client,
            "recipe.describe",
            json!({"asset_id": asset, "entry_id": entry_id}),
        )?;
        let layer = described["layers"]
            .as_array()
            .ok_or("No layers")?
            .iter()
            .find(|layer| layer["effect"] == json!(effect_id))
            .ok_or_else(|| format!("The disabled run cannot read the {effect_id} layer at all"))?
            .clone();
        ensure(
            layer["available"] == json!(false),
            format!("The {effect_id} layer reports {layer}"),
        )?;
        let modules = call(&limited, client, "module.list", json!({}))?;
        let module = modules["modules"]
            .as_array()
            .ok_or("No modules")?
            .iter()
            .find(|module| module["id"] == json!(module_id))
            .ok_or_else(|| format!("{module_id} is not listed at all"))?
            .clone();
        ensure(
            module["availability"]["kind"] == json!("unavailable"),
            format!(
                "{module_id} reports availability {}",
                module["availability"]
            ),
        )?;
        let (code, message) = refused(
            &limited,
            client,
            "render.sample",
            json!({"asset_id": asset, "x": 0, "y": 0}),
        )?;
        ensure(
            code == "incompatible" && message.contains(effect_id),
            format!("A disabled {module_id} refused rendering with {code}: {message}"),
        )?;
        let analysis = analyse(&limited, client, asset, json!({"kind": "current"}))?;
        ensure(
            analysis["status"] == json!("failed") && analysis.get("report").is_none(),
            format!("The analysis of a stack naming a disabled module answered {analysis}"),
        )?;
        Ok(json!({
            "layer": layer,
            "module": module,
            "render_error": message,
            "analysis_status": analysis["status"],
        }))
    })();
    limited.stop();
    limited_join
        .join()
        .map_err(|_| "The limited owner thread panicked")?;
    outcome
}

/// One request id sent twice through a patch action: the second answers the first's own result and
/// creates nothing new.
fn retry_dedup_check(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &Value,
    method: &str,
    revision: u64,
    request_id: &str,
    fields: Value,
) -> Result<Value> {
    let mut params = fields.clone();
    params["asset_id"] = asset.clone();
    params["mutation"] = mutation(revision, request_id);
    let first = call(owner, client, method, params.clone())?;
    let retried = call(owner, client, method, params)?;
    ensure(
        retried["current_entry_id"] == first["current_entry_id"]
            && retried["revision"] == first["revision"]
            && retried["deduplicated"] == json!(true),
        format!("The retry answered {retried} against the original {first}"),
    )?;
    Ok(json!({"first": first, "retried": retried}))
}

/// Return an asset to its Original entry (empty layers), so a later section of a journey starts
/// from a known, clean slate on the one asset the whole chapter reuses. `history.restore` is a
/// no-op when the asset is already at `original`.
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

// -------------------------------------------------------------------------------------------
// The Presence journey.
// -------------------------------------------------------------------------------------------

fn presence_journey(root: &Path, out: &Path) -> Result<Value> {
    let fixture = root.join(FIXTURE);
    let catalog = out.join("presence-catalog.sqlite");
    let source = lightwell_core::open_source(&fixture)?;
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

        // 1. Discovery.
        let modules = call(&owner, editor, "module.list", json!({}))?;
        let listed = modules["modules"]
            .as_array()
            .ok_or("module.list answered no modules")?
            .clone();
        let descriptor = listed
            .iter()
            .find(|module| module["id"] == json!("lightwell.presence"))
            .ok_or("lightwell.presence is not registered")?
            .clone();
        ensure(
            descriptor["collapsed"] == json!(true),
            "The presence section does not start collapsed",
        )?;
        ensure(
            descriptor["effects"]
                == json!([{"id": PRESENCE_EFFECT, "format": EFFECT_FORMAT, "stage": "spatial", "order": 0, "maskable": true}]),
            format!(
                "The presence effect is described as {}",
                descriptor["effects"]
            ),
        )?;
        let positions: Vec<&str> = listed
            .iter()
            .map(|module| module["id"].as_str().unwrap_or(""))
            .collect();
        let basic_index = positions
            .iter()
            .position(|id| *id == "lightwell.basic")
            .ok_or("lightwell.basic is not registered")?;
        let presence_index = positions
            .iter()
            .position(|id| *id == "lightwell.presence")
            .ok_or("lightwell.presence is not registered")?;
        let mixer_index = positions
            .iter()
            .position(|id| *id == "lightwell.mixer")
            .ok_or("lightwell.mixer is not registered")?;
        ensure(
            basic_index < presence_index && presence_index < mixer_index,
            format!(
                "module.list orders modules {positions:?}, expected presence after Basic and before the mixer"
            ),
        )?;
        let groups = descriptor["controls"]
            .as_array()
            .ok_or("The presence module declares no controls")?;
        ensure(
            groups.len() == 1 && groups[0]["label"] == json!("Presence"),
            format!("The presence module's groups are {groups:?}"),
        )?;
        ensure(
            !groups[0]["collapsed"].as_bool().unwrap_or(false),
            "The Presence group does not start expanded",
        )?;
        let sliders = groups[0]["controls"]
            .as_array()
            .ok_or("The Presence group declares no controls")?;
        ensure(
            sliders
                .iter()
                .map(|slider| slider["label"].clone())
                .collect::<Vec<_>>()
                == vec![json!("Texture"), json!("Clarity"), json!("Dehaze")],
            format!("The Presence group's sliders are {sliders:?}"),
        )?;
        let set_action = descriptor["actions"]
            .as_array()
            .ok_or("The presence module declares no actions")?
            .iter()
            .find(|action| action["id"] == json!("set-presence"))
            .ok_or("set-presence is not declared")?
            .clone();
        ensure(
            set_action["patch"] == json!(true)
                && set_action["parameters"].as_array().map(Vec::len) == Some(3),
            format!("set-presence is described as {set_action}"),
        )?;
        let schema = call(&owner, editor, "schema.list", json!({}))?;
        ensure(
            schema["methods"].get("edit.set-presence").is_some()
                && schema["methods"].get("edit.reset-presence").is_some(),
            "edit.set-presence or edit.reset-presence is not discoverable",
        )?;
        record(
            "module.list describes presence collapsed at spatial order 0, registered after Basic and before the mixer, with one expanded three-slider group; schema.list carries edit.set-presence (3 parameters) and edit.reset-presence",
            json!({"effects": descriptor["effects"], "order": positions}),
        );

        // 2. The gesture/cancel/no-op/retry/reset journey. One asset carries the whole chapter:
        //    importing the same fixture path again dedupes to this very asset by file identity
        //    rather than creating a fresh one, so every later section that needs a clean slate
        //    restores to this Original entry first instead of importing again.
        let imported = import(&owner, editor, &fixture)?;
        let asset = imported["asset"]["id"].clone();
        let original = imported["current_entry"]["id"].clone();

        // 2a. A slider gesture commits exactly one entry with the expected label.
        let revision = current_revision(&owner, editor, &asset)?;
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-presence"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"texture": 40.0}}),
        )?;
        let committed = call(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "gesture-commit")}),
        )?;
        ensure(
            committed["outcome"] == json!("applied"),
            format!("The gesture commit answered {committed}"),
        )?;
        let entry = call(
            &owner,
            editor,
            "history.inspect",
            json!({"asset_id": asset, "entry_id": committed["current_entry_id"]}),
        )?;
        ensure(
            entry["label"] == json!("Texture +40"),
            format!("The gesture's label is {}", entry["label"]),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let (presence_layer, _, values) = described_layer(&described, PRESENCE_EFFECT)?;
        ensure(
            field(&values, "texture")? == 40.0,
            format!("The committed layer reads {values}"),
        )?;
        record(
            "a draft.begin/set/commit slider gesture on texture commits exactly one entry labelled Texture +40",
            json!({"entry": entry["label"], "layer": presence_layer}),
        );

        // 2b. A cancelled draft commits nothing.
        let revision = current_revision(&owner, editor, &asset)?;
        let before_entry =
            call(&owner, editor, "asset.state", json!({"asset_id": asset}))?["current_entry"]["id"]
                .clone();
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-presence"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"texture": -70.0}}),
        )?;
        let cancelled = call(
            &owner,
            editor,
            "draft.cancel",
            json!({"draft_id": draft_id}),
        )?;
        ensure(
            cancelled["cancelled"] == json!(true),
            format!("draft.cancel answered {cancelled}"),
        )?;
        ensure(
            call(&owner, editor, "session.state", json!({}))?["draft"] == Value::Null,
            "A cancelled draft outlived its cancel",
        )?;
        ensure(
            current_revision(&owner, editor, &asset)? == revision
                && call(&owner, editor, "asset.state", json!({"asset_id": asset}))?["current_entry"]
                    ["id"]
                    == before_entry,
            "A cancelled draft changed the committed state",
        )?;
        record(
            "a cancelled draft commits nothing: the revision and current entry are unchanged",
            json!({"revision": revision}),
        );

        // 2c. A gesture that returns to its start is a no-op.
        let revision = current_revision(&owner, editor, &asset)?;
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-presence"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"texture": -70.0}}),
        )?;
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"texture": 40.0}}),
        )?;
        let no_op = call(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "return-to-start")}),
        )?;
        ensure(
            no_op["outcome"] == json!("no-op")
                && no_op["revision"] == json!(revision)
                && no_op["created_entry_id"] == Value::Null,
            format!("A return-to-start gesture answered {no_op}"),
        )?;
        record(
            "a gesture that returns to its start: a no-op outcome, no entry and no revision",
            no_op,
        );

        // 2d. A retried request id is deduplicated.
        let revision = current_revision(&owner, editor, &asset)?;
        let retry = retry_dedup_check(
            &owner,
            editor,
            &asset,
            "edit.set-presence",
            revision,
            "retry-clarity",
            json!({"clarity": -20.0}),
        )?;
        record(
            "a retried request id on edit.set-presence is deduplicated: the original entry and revision, nothing new",
            retry,
        );

        // 2e. The group's own reset (a three-field all-neutral patch) keeps the layer identity;
        //     production labels it by its field count, not "Reset Presence" (that label belongs
        //     only to the dedicated reset-presence action, checked next).
        let revision = current_revision(&owner, editor, &asset)?;
        let group_reset = call(
            &owner,
            editor,
            "edit.set-presence",
            json!({"asset_id": asset, "mutation": mutation(revision, "group-reset"), "texture": 0.0, "clarity": 0.0, "dehaze": 0.0}),
        )?;
        let inspected = call(
            &owner,
            editor,
            "history.inspect",
            json!({"asset_id": asset, "entry_id": group_reset["current_entry_id"]}),
        )?;
        ensure(
            inspected["label"] == json!("Reset Presence"),
            format!("The group's own reset is labelled {}", inspected["label"]),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let (layer_after_group_reset, _, values) = described_layer(&described, PRESENCE_EFFECT)?;
        ensure(
            layer_after_group_reset == presence_layer,
            "The group's own reset replaced the presence layer",
        )?;
        for name in ["texture", "clarity", "dehaze"] {
            ensure(
                field(&values, name)? == 0.0,
                format!(
                    "After the group reset the layer still reports {name} as {}",
                    values[name]
                ),
            )?;
        }

        // The module reset keeps the same identity, is labelled Reset Presence, a neutral layer
        // keeps the exact identity byte path, and a second reset is a no-op.
        let revision = current_revision(&owner, editor, &asset)?;
        call(
            &owner,
            editor,
            "edit.set-presence",
            json!({"asset_id": asset, "mutation": mutation(revision, "presence-before-module-reset"), "dehaze": -30.0}),
        )?;
        let revision = current_revision(&owner, editor, &asset)?;
        let module_reset = call(
            &owner,
            editor,
            "edit.reset-presence",
            json!({"asset_id": asset, "mutation": mutation(revision, "reset-presence")}),
        )?;
        let inspected = call(
            &owner,
            editor,
            "history.inspect",
            json!({"asset_id": asset, "entry_id": module_reset["current_entry_id"]}),
        )?;
        ensure(
            inspected["label"] == json!("Reset Presence"),
            format!("The module reset is labelled {}", inspected["label"]),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let (layer_after_module_reset, _, values) = described_layer(&described, PRESENCE_EFFECT)?;
        ensure(
            layer_after_module_reset == presence_layer,
            "The module reset replaced the presence layer",
        )?;
        for name in ["texture", "clarity", "dehaze"] {
            ensure(
                field(&values, name)? == 0.0,
                format!("The reset layer still reports {name} as {}", values[name]),
            )?;
        }
        let neutral_sample = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": asset, "x": RED_PROBE.0, "y": RED_PROBE.1}),
        )?;
        ensure(
            neutral_sample["rgba"] == source_pixel(&source, RED_PROBE.0, RED_PROBE.1),
            format!(
                "A neutral presence layer changed the rendered byte: {} against the decoded source {}",
                neutral_sample["rgba"],
                source_pixel(&source, RED_PROBE.0, RED_PROBE.1)
            ),
        )?;
        let repeated_reset = call(
            &owner,
            editor,
            "edit.reset-presence",
            json!({"asset_id": asset, "mutation": mutation(current_revision(&owner, editor, &asset)?, "reset-presence-again")}),
        )?;
        ensure(
            repeated_reset["outcome"] == json!("no-op"),
            format!("Resetting an already-neutral presence layer answered {repeated_reset}"),
        )?;
        record(
            "the group's own three-field reset and the module reset both keep the presence layer's identity (both labelled Reset Presence); a neutral layer renders the decoded source byte and a second reset is a no-op",
            json!({"layer": presence_layer, "group_label": "Reset Presence", "module_label": "Reset Presence"}),
        );

        // 3. The undo/redo/preview/restore/reopen journey, render.sample against the raster (at
        //    every quadrant, corner and the centre, since a spatial layer samples the stage-aligned
        //    tile that contains its pixel rather than a single point), and analysis identities.
        //    Dehaze is used because it is well-defined and visibly non-zero even over the fixture's
        //    flat quadrant interiors, unlike texture/clarity, whose local edge-aware bands read as
        //    zero far from any edge in this synthetic image.
        restore_to_original(
            &owner,
            editor,
            &asset,
            &original,
            "restore-before-undo-redo-journey",
        )?;
        let revision = current_revision(&owner, editor, &asset)?;
        let commit_a = call(
            &owner,
            editor,
            "edit.set-presence",
            json!({"asset_id": asset, "mutation": mutation(revision, "commit-a-dehaze"), "dehaze": -40.0}),
        )?;
        let entry_a = commit_a["current_entry_id"].clone();
        let samples_after_a = sample_probes(&owner, editor, &asset, &PRESENCE_PROBES)?;

        let revision = current_revision(&owner, editor, &asset)?;
        let commit_b = call(
            &owner,
            editor,
            "edit.set-presence",
            json!({"asset_id": asset, "mutation": mutation(revision, "commit-b-dehaze"), "dehaze": -80.0}),
        )?;
        let entry_b = commit_b["current_entry_id"].clone();
        let samples_after_b = sample_probes(&owner, editor, &asset, &PRESENCE_PROBES)?;
        ensure(
            samples_after_a != samples_after_b,
            "Strengthening dehaze left every quadrant, corner and centre probe unchanged",
        )?;

        let raster_check =
            render_matches_raster(&owner, editor, &asset, &source, &PRESENCE_PROBES)?;
        let analysis_check = distinct_analysis_identities(&owner, editor, &asset, &entry_a)?;

        let undone = call(
            &owner,
            editor,
            "history.undo",
            json!({"asset_id": asset, "mutation": mutation(current_revision(&owner, editor, &asset)?, "undo-b")}),
        )?;
        ensure(
            undone["outcome"] != json!("no-op"),
            format!("Undo answered {undone}"),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset,
            &PRESENCE_PROBES,
            &samples_after_a,
            "after undo",
        )?;

        call(
            &owner,
            editor,
            "history.redo",
            json!({"asset_id": asset, "mutation": mutation(current_revision(&owner, editor, &asset)?, "redo-b")}),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset,
            &PRESENCE_PROBES,
            &samples_after_b,
            "after redo",
        )?;

        let revision_before_preview = current_revision(&owner, editor, &asset)?;
        call(
            &owner,
            editor,
            "preview.select",
            json!({"asset_id": asset, "entry_id": entry_a}),
        )?;
        ensure(
            current_revision(&owner, editor, &asset)? == revision_before_preview,
            "Previewing an entry committed something",
        )?;
        expect_probes(
            &owner,
            editor,
            &asset,
            &PRESENCE_PROBES,
            &samples_after_a,
            "the preview of entry A",
        )?;
        call(&owner, editor, "preview.return-current", json!({}))?;

        let restored_a = call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset, "mutation": mutation(current_revision(&owner, editor, &asset)?, "restore-a"), "entry_id": entry_a}),
        )?;
        ensure(
            restored_a["outcome"] == json!("applied") || restored_a["outcome"] == json!("no-op"),
            format!("Restoring entry A answered {restored_a}"),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset,
            &PRESENCE_PROBES,
            &samples_after_a,
            "after restoring A",
        )?;

        let restored_b = call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset, "mutation": mutation(current_revision(&owner, editor, &asset)?, "restore-b"), "entry_id": entry_b}),
        )?;
        ensure(
            restored_b["outcome"] == json!("applied") || restored_b["outcome"] == json!("no-op"),
            format!("Restoring entry B answered {restored_b}"),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset,
            &PRESENCE_PROBES,
            &samples_after_b,
            "after restoring B",
        )?;
        record(
            "undo, redo, preview, restore and render.sample all evaluate byte-identically at every quadrant, corner and the centre against the values recorded at commit A and commit B, and render.sample equals an independently rendered raster through the spatial layer",
            json!({"entry_a": entry_a, "entry_b": entry_b, "raster_check": raster_check, "analysis_identities": analysis_check}),
        );

        // 4. Placement: presence always follows the colour run (Basic, then the mixer) whichever
        //    order the three actions are touched in, and a later rotate joins the geometry tail
        //    after it.
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
                &owner,
                editor,
                &asset,
                &original,
                &format!("placement-reset-{tag}"),
            )?;
            for action in order {
                let revision = current_revision(&owner, editor, &asset)?;
                let mut params = payload_for(action);
                params["asset_id"] = asset.clone();
                params["mutation"] = mutation(revision, &format!("placement-{tag}-{action}"));
                call(&owner, editor, &format!("edit.{action}"), params)?;
            }
            let revision = current_revision(&owner, editor, &asset)?;
            call(
                &owner,
                editor,
                "edit.transform",
                json!({"asset_id": asset, "mutation": mutation(revision, &format!("placement-rotate-{tag}")), "transform": "rotate-right"}),
            )?;
            let described = call(
                &owner,
                editor,
                "recipe.describe",
                json!({"asset_id": asset}),
            )?;
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

        // 5. Two-client races: a conflicted draft, a refused commit, a successful reapply, and a
        //    historical selection that survives another client's commit.
        restore_to_original(&owner, editor, &asset, &original, "restore-before-races")?;
        let races_asset = asset.clone();
        let races_original = original.clone();
        let revision = current_revision(&owner, editor, &races_asset)?;
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": races_asset, "action": "set-presence"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"texture": 30.0}}),
        )?;
        call(
            &owner,
            agent,
            "edit.set-basic",
            json!({"asset_id": races_asset, "mutation": mutation(revision, "agent-temperature"), "temperature": 40.0}),
        )?;
        let conflicted = call(&owner, editor, "draft.read", json!({"draft_id": draft_id}))?;
        ensure(
            conflicted["conflicted"] == json!(true)
                && conflicted["fields"] == json!({"texture": 30.0}),
            format!("The drafted gesture answered {conflicted}"),
        )?;
        let (code, _) = refused(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "editor-commit")}),
        )?;
        ensure(
            code == "conflict",
            format!("A conflicted commit was refused with {code}"),
        )?;
        let reapplied = call(
            &owner,
            editor,
            "draft.reapply",
            json!({"draft_id": draft_id}),
        )?;
        let rebased = current_revision(&owner, editor, &races_asset)?;
        ensure(
            reapplied["conflicted"] == json!(false) && reapplied["base_revision"] == json!(rebased),
            format!("Reapply answered {reapplied}"),
        )?;
        let before_sequence = call(
            &owner,
            editor,
            "history.list",
            json!({"asset_id": races_asset, "limit": 1}),
        )?["entries"][0]["sequence"]
            .clone();
        let committed = call(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(rebased, "editor-commit-rebased")}),
        )?;
        ensure(
            committed["outcome"] == json!("applied") && committed["revision"] == json!(rebased + 1),
            format!("The reapplied commit answered {committed}"),
        )?;
        let after_sequence = call(
            &owner,
            editor,
            "history.list",
            json!({"asset_id": races_asset, "limit": 1}),
        )?["entries"][0]["sequence"]
            .clone();
        ensure(
            after_sequence.as_u64() == before_sequence.as_u64().map(|s| s + 1),
            "The reapplied commit created more or fewer than one entry",
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": races_asset}),
        )?;
        let (_, _, presence_values) = described_layer(&described, PRESENCE_EFFECT)?;
        let (_, _, basic_values) = described_layer(&described, BASIC_EFFECT)?;
        ensure(
            field(&presence_values, "texture")? == 30.0
                && field(&basic_values, "temperature")? == 40.0,
            format!("Reapply lost a field: presence {presence_values}, basic {basic_values}"),
        )?;

        call(
            &owner,
            editor,
            "preview.select",
            json!({"asset_id": races_asset, "entry_id": races_original}),
        )?;
        let selected_report = ready_report(
            &owner,
            editor,
            &races_asset,
            json!({"kind": "entry", "entry_id": races_original}),
            "the selected historical entry",
        )?;
        call(
            &owner,
            agent,
            "edit.set-basic",
            json!({"asset_id": races_asset, "mutation": mutation(current_revision(&owner, agent, &races_asset)?, "agent-during-preview"), "saturation": -60.0}),
        )?;
        let session = call(&owner, editor, "session.state", json!({}))?;
        ensure(
            session["preview"]["selection"] == json!({"entry": races_original}),
            format!("The selection moved to {}", session["preview"]["selection"]),
        )?;
        let sample = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": races_asset, "x": 0, "y": 0}),
        )?;
        ensure(
            sample["rgba"] == source_pixel(&source, 0, 0),
            "The previewed sample moved to the newest stack instead of staying on the Original entry",
        )?;
        call(&owner, editor, "preview.return-current", json!({}))?;
        record(
            "client A drafts presence's texture while client B commits a Basic field: conflicted, commit refused, reapply keeps both fields and commits exactly one entry; a historical selection on B stays selected through A's commit",
            json!({"reapplied": reapplied, "committed": committed, "presence_values": presence_values, "basic_values": basic_values, "selected_identity": selected_report["identity"]}),
        );

        // 6. Unavailable provider, and 7. catalog reopen, both against the asset's final committed
        //    state (whatever the races section above left it at).
        let final_recipe = current_recipe(&owner, editor, &asset)?;
        let final_render = render(&source, &final_recipe)?;
        let final_state = call(&owner, editor, "asset.state", json!({"asset_id": asset}))?;
        let final_revision = as_u64(&final_state["revision"], "revision")?;
        let final_entry = final_state["current_entry"]["id"].clone();
        let final_probes = sample_probes(&owner, editor, &asset, &PRESENCE_PROBES)?;
        owner.stop();
        join.take()
            .ok_or("The presence owner thread was already joined")?
            .join()
            .map_err(|_| "The presence owner thread panicked")?;

        let unavailable = unavailable_provider_check(
            &catalog,
            "lightwell.presence",
            PRESENCE_EFFECT,
            &asset,
            &final_entry,
        )?;
        record(
            "the same catalog served with lightwell.presence disabled refuses to render or sample the stack naming it, keeps the layer and module readable, and reports the analysis failed with no counts",
            unavailable,
        );

        let (reopened, reopened_join) = OwnerHandle::start(&catalog)?;
        let restart = (|| -> Result<Value> {
            let client = reopened.register();
            prepare_source(&reopened, client, &asset)?;
            let state = call(&reopened, client, "asset.state", json!({"asset_id": asset}))?;
            ensure(
                as_u64(&state["revision"], "revision")? == final_revision,
                format!("The reopened revision is {}", state["revision"]),
            )?;
            ensure(
                state["current_entry"]["id"] == final_entry,
                "The reopened current entry changed",
            )?;
            let reopened_render = render(&source, &current_recipe(&reopened, client, &asset)?)?;
            ensure(
                reopened_render.rgba == final_render.rgba,
                "The reopened render is not byte-identical",
            )?;
            expect_probes(
                &reopened,
                client,
                &asset,
                &PRESENCE_PROBES,
                &final_probes,
                "the reopened catalog",
            )?;
            Ok(json!({"revision": final_revision, "entry": final_entry}))
        })();
        reopened.stop();
        reopened_join
            .join()
            .map_err(|_| "The reopened presence owner thread panicked")?;
        let restart_detail = restart?;
        record(
            "the catalog reopened: the presence layer's values, the history and the render all reproduce byte-identically",
            restart_detail,
        );

        Ok(json!({
            "status": "passed",
            "fixture": FIXTURE,
            "asset_id": asset,
            "presence_layer_id": presence_layer,
            "checks": checks.borrow().clone(),
            "elapsed_ms": total.elapsed().as_secs_f64() * 1000.0,
        }))
    })();
    if let Some(join) = join.take() {
        owner.stop();
        let _ = join.join();
    }
    outcome
}

// -------------------------------------------------------------------------------------------
// The colour mixer journey.
// -------------------------------------------------------------------------------------------

fn mixer_journey(root: &Path, out: &Path) -> Result<Value> {
    let fixture = root.join(FIXTURE);
    let catalog = out.join("mixer-catalog.sqlite");
    let source = lightwell_core::open_source(&fixture)?;
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

        // 1. Discovery.
        let modules = call(&owner, editor, "module.list", json!({}))?;
        let descriptor = modules["modules"]
            .as_array()
            .ok_or("module.list answered no modules")?
            .iter()
            .find(|module| module["id"] == json!("lightwell.mixer"))
            .ok_or("lightwell.mixer is not registered")?
            .clone();
        ensure(
            descriptor["collapsed"] == json!(true),
            "The mixer section does not start collapsed",
        )?;
        ensure(
            descriptor["effects"]
                == json!([{"id": MIXER_EFFECT, "format": EFFECT_FORMAT, "stage": "color", "order": 10, "maskable": true}]),
            format!("The mixer effect is described as {}", descriptor["effects"]),
        )?;
        let groups = descriptor["controls"]
            .as_array()
            .ok_or("The mixer declares no controls")?;
        ensure(
            groups.len() == 3
                && groups
                    .iter()
                    .map(|group| group["label"].clone())
                    .collect::<Vec<_>>()
                    == vec![json!("Hue"), json!("Saturation"), json!("Luminance")],
            format!("The mixer's groups are {groups:?}"),
        )?;
        for group in groups {
            let sliders = group["controls"]
                .as_array()
                .ok_or("A mixer group declares no controls")?;
            ensure(sliders.len() == 8, "A mixer group does not hold 8 sliders")?;
        }
        let set_action = descriptor["actions"]
            .as_array()
            .ok_or("The mixer declares no actions")?
            .iter()
            .find(|action| action["id"] == json!("set-mixer"))
            .ok_or("set-mixer is not declared")?
            .clone();
        ensure(
            set_action["patch"] == json!(true)
                && set_action["parameters"].as_array().map(Vec::len) == Some(24),
            format!("set-mixer is described as {set_action}"),
        )?;
        let schema = call(&owner, editor, "schema.list", json!({}))?;
        ensure(
            schema["methods"].get("edit.set-mixer").is_some()
                && schema["methods"].get("edit.reset-mixer").is_some(),
            "edit.set-mixer or edit.reset-mixer is not discoverable",
        )?;
        record(
            "module.list describes the mixer collapsed at colour order 10 with three eight-slider groups; schema.list carries edit.set-mixer (24 parameters) and edit.reset-mixer",
            json!({"effects": descriptor["effects"], "groups": groups.iter().map(|g| g["label"].clone()).collect::<Vec<_>>()}),
        );

        // 2. The gesture/cancel/no-op/retry/reset journey. One asset carries the whole chapter:
        //    importing the same fixture path again dedupes to this very asset by file identity
        //    rather than creating a fresh one, so every later section that needs a clean slate
        //    restores to this Original entry first instead of importing again.
        let imported = import(&owner, editor, &fixture)?;
        let asset = imported["asset"]["id"].clone();
        let original = imported["current_entry"]["id"].clone();

        // 2a. A slider gesture commits exactly one entry with the expected label.
        let revision = current_revision(&owner, editor, &asset)?;
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-mixer"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"red-hue": 20.0}}),
        )?;
        let committed = call(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "gesture-commit")}),
        )?;
        ensure(
            committed["outcome"] == json!("applied"),
            format!("The gesture commit answered {committed}"),
        )?;
        let entry = call(
            &owner,
            editor,
            "history.inspect",
            json!({"asset_id": asset, "entry_id": committed["current_entry_id"]}),
        )?;
        ensure(
            entry["label"] == json!("Red hue +20"),
            format!("The gesture's label is {}", entry["label"]),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let (mixer_layer, _, values) = described_layer(&described, MIXER_EFFECT)?;
        ensure(
            field(&values, "red-hue")? == 20.0,
            format!("The committed layer reads {values}"),
        )?;
        record(
            "a draft.begin/set/commit slider gesture on red-hue commits exactly one entry labelled Red hue +20",
            json!({"entry": entry["label"], "layer": mixer_layer}),
        );

        // 2b. A cancelled draft commits nothing.
        let revision = current_revision(&owner, editor, &asset)?;
        let before_entry =
            call(&owner, editor, "asset.state", json!({"asset_id": asset}))?["current_entry"]["id"]
                .clone();
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-mixer"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"red-saturation": 55.0}}),
        )?;
        let cancelled = call(
            &owner,
            editor,
            "draft.cancel",
            json!({"draft_id": draft_id}),
        )?;
        ensure(
            cancelled["cancelled"] == json!(true),
            format!("draft.cancel answered {cancelled}"),
        )?;
        ensure(
            call(&owner, editor, "session.state", json!({}))?["draft"] == Value::Null,
            "A cancelled draft outlived its cancel",
        )?;
        ensure(
            current_revision(&owner, editor, &asset)? == revision
                && call(&owner, editor, "asset.state", json!({"asset_id": asset}))?["current_entry"]
                    ["id"]
                    == before_entry,
            "A cancelled draft changed the committed state",
        )?;
        record(
            "a cancelled draft commits nothing: the revision and current entry are unchanged",
            json!({"revision": revision}),
        );

        // 2c. A gesture that returns to its start is a no-op.
        let revision = current_revision(&owner, editor, &asset)?;
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-mixer"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"red-hue": 55.0}}),
        )?;
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"red-hue": 20.0}}),
        )?;
        let no_op = call(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "return-to-start")}),
        )?;
        ensure(
            no_op["outcome"] == json!("no-op")
                && no_op["revision"] == json!(revision)
                && no_op["created_entry_id"] == Value::Null,
            format!("A return-to-start gesture answered {no_op}"),
        )?;
        record(
            "a gesture that returns to its start: a no-op outcome, no entry and no revision",
            no_op,
        );

        // 2d. A retried request id is deduplicated.
        let revision = current_revision(&owner, editor, &asset)?;
        let retry = retry_dedup_check(
            &owner,
            editor,
            &asset,
            "edit.set-mixer",
            revision,
            "retry-aqua-luminance",
            json!({"aqua-luminance": -15.0}),
        )?;
        record(
            "a retried request id on edit.set-mixer is deduplicated: the original entry and revision, nothing new",
            retry,
        );

        // 2e. The Hue group's own reset keeps the layer identity, and is labelled Reset Hue.
        let revision = current_revision(&owner, editor, &asset)?;
        let hue_fields = [
            "red-hue",
            "orange-hue",
            "yellow-hue",
            "green-hue",
            "aqua-hue",
            "blue-hue",
            "purple-hue",
            "magenta-hue",
        ];
        let mut hue_reset_params = Map::new();
        hue_reset_params.insert("asset_id".into(), asset.clone());
        hue_reset_params.insert("mutation".into(), mutation(revision, "reset-hue-group"));
        for name in hue_fields {
            hue_reset_params.insert(name.into(), json!(0.0));
        }
        let hue_reset = call(
            &owner,
            editor,
            "edit.set-mixer",
            Value::Object(hue_reset_params),
        )?;
        let inspected = call(
            &owner,
            editor,
            "history.inspect",
            json!({"asset_id": asset, "entry_id": hue_reset["current_entry_id"]}),
        )?;
        ensure(
            inspected["label"] == json!("Reset Hue"),
            format!("The Hue group reset is labelled {}", inspected["label"]),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let (layer_after_group_reset, _, values) = described_layer(&described, MIXER_EFFECT)?;
        ensure(
            layer_after_group_reset == mixer_layer,
            "The Hue group reset replaced the mixer layer",
        )?;
        ensure(
            field(&values, "red-hue")? == 0.0 && field(&values, "aqua-luminance")? == -15.0,
            format!("After the Hue reset the layer reads {values}"),
        )?;

        // The module reset keeps the same identity, is labelled Reset Colour mixer, and a neutral
        // layer keeps the exact identity byte path; a second reset is a no-op.
        let revision = current_revision(&owner, editor, &asset)?;
        let module_reset = call(
            &owner,
            editor,
            "edit.reset-mixer",
            json!({"asset_id": asset, "mutation": mutation(revision, "reset-mixer")}),
        )?;
        let inspected = call(
            &owner,
            editor,
            "history.inspect",
            json!({"asset_id": asset, "entry_id": module_reset["current_entry_id"]}),
        )?;
        ensure(
            inspected["label"] == json!("Reset Colour mixer"),
            format!("The module reset is labelled {}", inspected["label"]),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let (layer_after_module_reset, _, values) = described_layer(&described, MIXER_EFFECT)?;
        ensure(
            layer_after_module_reset == mixer_layer,
            "The module reset replaced the mixer layer",
        )?;
        for name in [
            "red-hue",
            "aqua-luminance",
            "red-saturation",
            "magenta-luminance",
        ] {
            ensure(
                field(&values, name)? == 0.0,
                format!("The reset layer still reports {name} as {}", values[name]),
            )?;
        }
        let neutral_sample = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": asset, "x": RED_PROBE.0, "y": RED_PROBE.1}),
        )?;
        ensure(
            neutral_sample["rgba"] == source_pixel(&source, RED_PROBE.0, RED_PROBE.1),
            format!(
                "A neutral mixer layer changed the rendered byte: {} against the decoded source {}",
                neutral_sample["rgba"],
                source_pixel(&source, RED_PROBE.0, RED_PROBE.1)
            ),
        )?;
        let repeated_reset = call(
            &owner,
            editor,
            "edit.reset-mixer",
            json!({"asset_id": asset, "mutation": mutation(current_revision(&owner, editor, &asset)?, "reset-mixer-again")}),
        )?;
        ensure(
            repeated_reset["outcome"] == json!("no-op"),
            format!("Resetting an already-neutral mixer layer answered {repeated_reset}"),
        )?;
        record(
            "the Hue group's own reset and the module reset both keep the mixer layer's identity; a neutral layer renders the decoded source byte and a second reset is a no-op",
            json!({"layer": mixer_layer, "group_label": "Reset Hue", "module_label": "Reset Colour mixer"}),
        );

        // 3. The undo/redo/preview/restore/reopen journey, render.sample against the raster, and
        //    analysis identities. The same asset carries every section of this journey (importing
        //    the same fixture path again would dedupe to this very asset rather than a fresh one),
        //    so each section restores to the Original entry first for a clean slate.
        let asset2 = asset.clone();
        restore_to_original(
            &owner,
            editor,
            &asset2,
            &original,
            "restore-before-undo-redo-journey",
        )?;
        let revision = current_revision(&owner, editor, &asset2)?;
        let commit_a = call(
            &owner,
            editor,
            "edit.set-mixer",
            json!({"asset_id": asset2, "mutation": mutation(revision, "commit-a-green-hue"), "green-hue": 40.0}),
        )?;
        let entry_a = commit_a["current_entry_id"].clone();
        let samples_after_a = sample_probes(&owner, editor, &asset2, &QUADRANT_PROBES)?;

        let revision = current_revision(&owner, editor, &asset2)?;
        let commit_b = call(
            &owner,
            editor,
            "edit.set-mixer",
            json!({"asset_id": asset2, "mutation": mutation(revision, "commit-b-blue-saturation"), "blue-saturation": 70.0}),
        )?;
        let entry_b = commit_b["current_entry_id"].clone();
        let samples_after_b = sample_probes(&owner, editor, &asset2, &QUADRANT_PROBES)?;
        ensure(
            samples_after_a != samples_after_b,
            "Adding blue-saturation left every probe unchanged",
        )?;

        // render.sample equals the independently rendered raster for this stack.
        let raster_check =
            render_matches_raster(&owner, editor, &asset2, &source, &QUADRANT_PROBES)?;

        // Two different stacks answer two different analysis identities.
        let analysis_check = distinct_analysis_identities(&owner, editor, &asset2, &entry_a)?;

        let undone = call(
            &owner,
            editor,
            "history.undo",
            json!({"asset_id": asset2, "mutation": mutation(current_revision(&owner, editor, &asset2)?, "undo-b")}),
        )?;
        ensure(
            undone["outcome"] != json!("no-op"),
            format!("Undo answered {undone}"),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset2,
            &QUADRANT_PROBES,
            &samples_after_a,
            "after undo",
        )?;

        call(
            &owner,
            editor,
            "history.redo",
            json!({"asset_id": asset2, "mutation": mutation(current_revision(&owner, editor, &asset2)?, "redo-b")}),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset2,
            &QUADRANT_PROBES,
            &samples_after_b,
            "after redo",
        )?;

        let revision_before_preview = current_revision(&owner, editor, &asset2)?;
        call(
            &owner,
            editor,
            "preview.select",
            json!({"asset_id": asset2, "entry_id": entry_a}),
        )?;
        ensure(
            current_revision(&owner, editor, &asset2)? == revision_before_preview,
            "Previewing an entry committed something",
        )?;
        expect_probes(
            &owner,
            editor,
            &asset2,
            &QUADRANT_PROBES,
            &samples_after_a,
            "the preview of entry A",
        )?;
        call(&owner, editor, "preview.return-current", json!({}))?;

        let restored_a = call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset2, "mutation": mutation(current_revision(&owner, editor, &asset2)?, "restore-a"), "entry_id": entry_a}),
        )?;
        ensure(
            restored_a["outcome"] == json!("applied") || restored_a["outcome"] == json!("no-op"),
            format!("Restoring entry A answered {restored_a}"),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset2,
            &QUADRANT_PROBES,
            &samples_after_a,
            "after restoring A",
        )?;

        let restored_b = call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset2, "mutation": mutation(current_revision(&owner, editor, &asset2)?, "restore-b"), "entry_id": entry_b}),
        )?;
        ensure(
            restored_b["outcome"] == json!("applied") || restored_b["outcome"] == json!("no-op"),
            format!("Restoring entry B answered {restored_b}"),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset2,
            &QUADRANT_PROBES,
            &samples_after_b,
            "after restoring B",
        )?;
        record(
            "undo, redo, preview, restore and render.sample all evaluate byte-identically against the values recorded at commit A and commit B, and render.sample equals an independently rendered raster",
            json!({"entry_a": entry_a, "entry_b": entry_b, "raster_check": raster_check, "analysis_identities": analysis_check}),
        );

        // 4. Ordering: the mixer always follows Basic, whichever was touched first.
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
                &owner,
                editor,
                &asset,
                &original,
                &format!("order-{case}-reset"),
            )?;
            let ordering_asset = asset.clone();
            let revision = current_revision(&owner, editor, &ordering_asset)?;
            let mut params = first_field;
            params["asset_id"] = ordering_asset.clone();
            params["mutation"] = mutation(revision, &format!("order-{case}-first"));
            call(&owner, editor, first_action, params)?;
            let revision = current_revision(&owner, editor, &ordering_asset)?;
            let mut params = second_field;
            params["asset_id"] = ordering_asset.clone();
            params["mutation"] = mutation(revision, &format!("order-{case}-second"));
            call(&owner, editor, second_action, params)?;
            let described = call(
                &owner,
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
                    &owner,
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

        // 5. Two-client races: a conflicted draft, a refused commit, a successful reapply, and a
        //    historical selection that survives another client's commit.
        restore_to_original(&owner, editor, &asset, &original, "restore-before-races")?;
        let races_asset = asset.clone();
        let races_original = original.clone();
        let revision = current_revision(&owner, editor, &races_asset)?;
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": races_asset, "action": "set-mixer"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"yellow-hue": 25.0}}),
        )?;
        call(
            &owner,
            agent,
            "edit.set-basic",
            json!({"asset_id": races_asset, "mutation": mutation(revision, "agent-temperature"), "temperature": 40.0}),
        )?;
        let conflicted = call(&owner, editor, "draft.read", json!({"draft_id": draft_id}))?;
        ensure(
            conflicted["conflicted"] == json!(true)
                && conflicted["fields"] == json!({"yellow-hue": 25.0}),
            format!("The drafted gesture answered {conflicted}"),
        )?;
        let (code, _) = refused(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "editor-commit")}),
        )?;
        ensure(
            code == "conflict",
            format!("A conflicted commit was refused with {code}"),
        )?;
        let reapplied = call(
            &owner,
            editor,
            "draft.reapply",
            json!({"draft_id": draft_id}),
        )?;
        let rebased = current_revision(&owner, editor, &races_asset)?;
        ensure(
            reapplied["conflicted"] == json!(false) && reapplied["base_revision"] == json!(rebased),
            format!("Reapply answered {reapplied}"),
        )?;
        let before_sequence = call(
            &owner,
            editor,
            "history.list",
            json!({"asset_id": races_asset, "limit": 1}),
        )?["entries"][0]["sequence"]
            .clone();
        let committed = call(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(rebased, "editor-commit-rebased")}),
        )?;
        ensure(
            committed["outcome"] == json!("applied") && committed["revision"] == json!(rebased + 1),
            format!("The reapplied commit answered {committed}"),
        )?;
        let after_sequence = call(
            &owner,
            editor,
            "history.list",
            json!({"asset_id": races_asset, "limit": 1}),
        )?["entries"][0]["sequence"]
            .clone();
        ensure(
            after_sequence.as_u64() == before_sequence.as_u64().map(|s| s + 1),
            "The reapplied commit created more or fewer than one entry",
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": races_asset}),
        )?;
        let (_, _, mixer_values) = described_layer(&described, MIXER_EFFECT)?;
        let (_, _, basic_values) = described_layer(&described, BASIC_EFFECT)?;
        ensure(
            field(&mixer_values, "yellow-hue")? == 25.0
                && field(&basic_values, "temperature")? == 40.0,
            format!("Reapply lost a field: mixer {mixer_values}, basic {basic_values}"),
        )?;

        // A historical selection stays attached to its entry while another client commits.
        call(
            &owner,
            editor,
            "preview.select",
            json!({"asset_id": races_asset, "entry_id": races_original}),
        )?;
        let selected_report = ready_report(
            &owner,
            editor,
            &races_asset,
            json!({"kind": "entry", "entry_id": races_original}),
            "the selected historical entry",
        )?;
        call(
            &owner,
            agent,
            "edit.set-basic",
            json!({"asset_id": races_asset, "mutation": mutation(current_revision(&owner, agent, &races_asset)?, "agent-during-preview"), "saturation": -60.0}),
        )?;
        let session = call(&owner, editor, "session.state", json!({}))?;
        ensure(
            session["preview"]["selection"] == json!({"entry": races_original}),
            format!("The selection moved to {}", session["preview"]["selection"]),
        )?;
        let sample = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": races_asset, "x": 0, "y": 0}),
        )?;
        ensure(
            sample["rgba"] == source_pixel(&source, 0, 0),
            "The previewed sample moved to the newest stack instead of staying on the Original entry",
        )?;
        call(&owner, editor, "preview.return-current", json!({}))?;
        record(
            "client A drafts a mixer field while client B commits a Basic field: conflicted, commit refused, reapply keeps both fields and commits exactly one entry; a historical selection on B stays selected through A's commit",
            json!({"reapplied": reapplied, "committed": committed, "mixer_values": mixer_values, "basic_values": basic_values, "selected_identity": selected_report["identity"]}),
        );

        // 6. Unavailable provider, and 7. catalog reopen, both against the asset's final committed
        //    state (whatever the races section above left it at).
        let final_recipe = current_recipe(&owner, editor, &asset2)?;
        let final_render = render(&source, &final_recipe)?;
        let final_state = call(&owner, editor, "asset.state", json!({"asset_id": asset2}))?;
        let final_revision = as_u64(&final_state["revision"], "revision")?;
        let final_entry = final_state["current_entry"]["id"].clone();
        let final_probes = sample_probes(&owner, editor, &asset2, &QUADRANT_PROBES)?;
        owner.stop();
        join.take()
            .ok_or("The mixer owner thread was already joined")?
            .join()
            .map_err(|_| "The mixer owner thread panicked")?;

        let unavailable = unavailable_provider_check(
            &catalog,
            "lightwell.mixer",
            MIXER_EFFECT,
            &asset2,
            &final_entry,
        )?;
        record(
            "the same catalog served with lightwell.mixer disabled refuses to render or sample the stack naming it, keeps the layer and module readable, and reports the analysis failed with no counts",
            unavailable,
        );

        let (reopened, reopened_join) = OwnerHandle::start(&catalog)?;
        let restart = (|| -> Result<Value> {
            let client = reopened.register();
            prepare_source(&reopened, client, &asset2)?;
            let state = call(
                &reopened,
                client,
                "asset.state",
                json!({"asset_id": asset2}),
            )?;
            ensure(
                as_u64(&state["revision"], "revision")? == final_revision,
                format!("The reopened revision is {}", state["revision"]),
            )?;
            ensure(
                state["current_entry"]["id"] == final_entry,
                "The reopened current entry changed",
            )?;
            let reopened_render = render(&source, &current_recipe(&reopened, client, &asset2)?)?;
            ensure(
                reopened_render.rgba == final_render.rgba,
                "The reopened render is not byte-identical",
            )?;
            expect_probes(
                &reopened,
                client,
                &asset2,
                &QUADRANT_PROBES,
                &final_probes,
                "the reopened catalog",
            )?;
            Ok(json!({"revision": final_revision, "entry": final_entry}))
        })();
        reopened.stop();
        reopened_join
            .join()
            .map_err(|_| "The reopened mixer owner thread panicked")?;
        let restart_detail = restart?;
        record(
            "the catalog reopened: the mixer layer's values, the history and the render all reproduce byte-identically",
            restart_detail,
        );

        Ok(json!({
            "status": "passed",
            "fixture": FIXTURE,
            "asset_id": asset,
            "mixer_layer_id": mixer_layer,
            "checks": checks.borrow().clone(),
            "elapsed_ms": total.elapsed().as_secs_f64() * 1000.0,
        }))
    })();
    if let Some(join) = join.take() {
        owner.stop();
        let _ = join.join();
    }
    outcome
}

// -------------------------------------------------------------------------------------------
// The vignette journey.
// -------------------------------------------------------------------------------------------

fn vignette_journey(root: &Path, out: &Path) -> Result<Value> {
    let fixture = root.join(FIXTURE);
    let catalog = out.join("vignette-catalog.sqlite");
    let source = lightwell_core::open_source(&fixture)?;
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

        // 1. Discovery.
        let modules = call(&owner, editor, "module.list", json!({}))?;
        let descriptor = modules["modules"]
            .as_array()
            .ok_or("module.list answered no modules")?
            .iter()
            .find(|module| module["id"] == json!("lightwell.vignette"))
            .ok_or("lightwell.vignette is not registered")?
            .clone();
        ensure(
            descriptor["collapsed"] == json!(true),
            "The vignette section does not start collapsed",
        )?;
        ensure(
            descriptor["effects"]
                == json!([{"id": VIGNETTE_EFFECT, "format": EFFECT_FORMAT, "stage": "finish", "order": 0}]),
            format!(
                "The vignette effect is described as {}",
                descriptor["effects"]
            ),
        )?;
        let groups = descriptor["controls"]
            .as_array()
            .ok_or("The vignette declares no controls")?;
        ensure(
            groups.len() == 1 && groups[0]["label"] == json!("Vignette"),
            format!("The vignette's groups are {groups:?}"),
        )?;
        let sliders = groups[0]["controls"]
            .as_array()
            .ok_or("The Vignette group declares no controls")?;
        ensure(
            sliders.len() == 4,
            "The Vignette group does not hold 4 sliders",
        )?;
        let set_action = descriptor["actions"]
            .as_array()
            .ok_or("The vignette declares no actions")?
            .iter()
            .find(|action| action["id"] == json!("set-vignette"))
            .ok_or("set-vignette is not declared")?
            .clone();
        ensure(
            set_action["patch"] == json!(true)
                && set_action["parameters"].as_array().map(Vec::len) == Some(4),
            format!("set-vignette is described as {set_action}"),
        )?;
        let schema = call(&owner, editor, "schema.list", json!({}))?;
        ensure(
            schema["methods"].get("edit.set-vignette").is_some()
                && schema["methods"].get("edit.reset-vignette").is_some(),
            "edit.set-vignette or edit.reset-vignette is not discoverable",
        )?;
        record(
            "module.list describes the vignette collapsed at finish order 0 with one four-slider group; schema.list carries edit.set-vignette (4 parameters) and edit.reset-vignette",
            json!({"effects": descriptor["effects"]}),
        );

        // 2. The gesture/cancel/no-op/retry/reset journey. One asset carries the whole chapter:
        //    importing the same fixture path again dedupes to this very asset by file identity
        //    rather than creating a fresh one, so every later section that needs a clean slate
        //    restores to this Original entry first instead of importing again.
        let imported = import(&owner, editor, &fixture)?;
        let asset = imported["asset"]["id"].clone();
        let original = imported["current_entry"]["id"].clone();

        // 2a. A slider gesture commits exactly one entry with the expected label, which names the
        //     module and the field so it reads unambiguously in a shared history list.
        let revision = current_revision(&owner, editor, &asset)?;
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-vignette"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"amount": -35.0}}),
        )?;
        let committed = call(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "gesture-commit")}),
        )?;
        ensure(
            committed["outcome"] == json!("applied"),
            format!("The gesture commit answered {committed}"),
        )?;
        let entry = call(
            &owner,
            editor,
            "history.inspect",
            json!({"asset_id": asset, "entry_id": committed["current_entry_id"]}),
        )?;
        ensure(
            entry["label"] == json!("Vignette amount -35"),
            format!("The gesture's label is {}", entry["label"]),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let (vignette_layer, layer_index, values) = described_layer(&described, VIGNETTE_EFFECT)?;
        ensure(
            layer_index == described["layers"].as_array().unwrap().len() - 1,
            "The vignette layer did not commit at the end of the stack",
        )?;
        ensure(
            field(&values, "amount")? == -35.0,
            format!("The committed layer reads {values}"),
        )?;
        record(
            "a draft.begin/set/commit slider gesture on amount commits exactly one entry labelled Vignette amount -35, at the end of the stack",
            json!({"entry": entry["label"], "layer": vignette_layer}),
        );

        // 2b. A cancelled draft commits nothing.
        let revision = current_revision(&owner, editor, &asset)?;
        let before_entry =
            call(&owner, editor, "asset.state", json!({"asset_id": asset}))?["current_entry"]["id"]
                .clone();
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-vignette"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"amount": -70.0}}),
        )?;
        let cancelled = call(
            &owner,
            editor,
            "draft.cancel",
            json!({"draft_id": draft_id}),
        )?;
        ensure(
            cancelled["cancelled"] == json!(true),
            format!("draft.cancel answered {cancelled}"),
        )?;
        ensure(
            call(&owner, editor, "session.state", json!({}))?["draft"] == Value::Null,
            "A cancelled draft outlived its cancel",
        )?;
        ensure(
            current_revision(&owner, editor, &asset)? == revision
                && call(&owner, editor, "asset.state", json!({"asset_id": asset}))?["current_entry"]
                    ["id"]
                    == before_entry,
            "A cancelled draft changed the committed state",
        )?;
        record(
            "a cancelled draft commits nothing: the revision and current entry are unchanged",
            json!({"revision": revision}),
        );

        // 2c. A gesture that returns to its start is a no-op.
        let revision = current_revision(&owner, editor, &asset)?;
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-vignette"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"amount": -70.0}}),
        )?;
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"amount": -35.0}}),
        )?;
        let no_op = call(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "return-to-start")}),
        )?;
        ensure(
            no_op["outcome"] == json!("no-op")
                && no_op["revision"] == json!(revision)
                && no_op["created_entry_id"] == Value::Null,
            format!("A return-to-start gesture answered {no_op}"),
        )?;
        record(
            "a gesture that returns to its start: a no-op outcome, no entry and no revision",
            no_op,
        );

        // 2d. A retried request id is deduplicated.
        let revision = current_revision(&owner, editor, &asset)?;
        let retry = retry_dedup_check(
            &owner,
            editor,
            &asset,
            "edit.set-vignette",
            revision,
            "retry-roundness",
            json!({"roundness": 20.0}),
        )?;
        record(
            "a retried request id on edit.set-vignette is deduplicated: the original entry and revision, nothing new",
            retry,
        );

        // 2e. The module reset keeps the layer identity, is labelled Reset Vignette, a neutral
        //     layer keeps the identity byte path, and a second reset is a no-op. (Vignette has one
        //     group, so its own reset and the module reset are the same action.)
        let revision = current_revision(&owner, editor, &asset)?;
        let module_reset = call(
            &owner,
            editor,
            "edit.reset-vignette",
            json!({"asset_id": asset, "mutation": mutation(revision, "reset-vignette")}),
        )?;
        let inspected = call(
            &owner,
            editor,
            "history.inspect",
            json!({"asset_id": asset, "entry_id": module_reset["current_entry_id"]}),
        )?;
        ensure(
            inspected["label"] == json!("Reset Vignette"),
            format!("The module reset is labelled {}", inspected["label"]),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )?;
        let (layer_after_reset, _, values) = described_layer(&described, VIGNETTE_EFFECT)?;
        ensure(
            layer_after_reset == vignette_layer,
            "The module reset replaced the vignette layer",
        )?;
        ensure(
            field(&values, "amount")? == 0.0
                && field(&values, "midpoint")? == 50.0
                && field(&values, "roundness")? == 0.0
                && field(&values, "feather")? == 50.0,
            format!("The reset layer reads {values}"),
        )?;
        let neutral_sample = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": asset, "x": 240, "y": 160}),
        )?;
        ensure(
            neutral_sample["rgba"] == source_pixel(&source, 240, 160),
            format!(
                "A neutral vignette layer changed the rendered byte: {} against the decoded source {}",
                neutral_sample["rgba"],
                source_pixel(&source, 240, 160)
            ),
        )?;
        let repeated_reset = call(
            &owner,
            editor,
            "edit.reset-vignette",
            json!({"asset_id": asset, "mutation": mutation(current_revision(&owner, editor, &asset)?, "reset-vignette-again")}),
        )?;
        ensure(
            repeated_reset["outcome"] == json!("no-op"),
            format!("Resetting an already all-default vignette layer answered {repeated_reset}"),
        )?;
        record(
            "the module reset keeps the vignette layer's identity and is labelled Reset Vignette; the all-default layer renders the decoded source byte and a second reset is a no-op",
            json!({"layer": vignette_layer, "module_label": "Reset Vignette"}),
        );

        // 3. The undo/redo/preview/restore/reopen journey, render.sample against the raster
        //    (including the corners), and analysis identities. Same asset as section 2, restored
        //    to Original first (see the note above section 2).
        let asset2 = asset.clone();
        restore_to_original(
            &owner,
            editor,
            &asset2,
            &original,
            "restore-before-undo-redo-journey",
        )?;
        let revision = current_revision(&owner, editor, &asset2)?;
        let commit_a = call(
            &owner,
            editor,
            "edit.set-vignette",
            json!({"asset_id": asset2, "mutation": mutation(revision, "commit-a-amount"), "amount": -50.0}),
        )?;
        let entry_a = commit_a["current_entry_id"].clone();
        let samples_after_a = sample_probes(&owner, editor, &asset2, &CORNERS_AND_CENTRE)?;

        // A stronger amount, not a shape parameter: at the default midpoint/feather the corners of
        // a 480x320 stage are already fully saturated (mask 1) for every roundness in range, so a
        // shape-only change would leave every probe byte-identical and prove nothing. Amount always
        // shows at a saturated corner, since its gain is `1 - |amount|*mask` for mask 1.
        let revision = current_revision(&owner, editor, &asset2)?;
        let commit_b = call(
            &owner,
            editor,
            "edit.set-vignette",
            json!({"asset_id": asset2, "mutation": mutation(revision, "commit-b-amount"), "amount": -85.0}),
        )?;
        let entry_b = commit_b["current_entry_id"].clone();
        let samples_after_b = sample_probes(&owner, editor, &asset2, &CORNERS_AND_CENTRE)?;
        ensure(
            samples_after_a != samples_after_b,
            "Adding roundness left every corner and the centre unchanged",
        )?;

        let raster_check =
            render_matches_raster(&owner, editor, &asset2, &source, &CORNERS_AND_CENTRE)?;
        let analysis_check = distinct_analysis_identities(&owner, editor, &asset2, &entry_a)?;

        let undone = call(
            &owner,
            editor,
            "history.undo",
            json!({"asset_id": asset2, "mutation": mutation(current_revision(&owner, editor, &asset2)?, "undo-b")}),
        )?;
        ensure(
            undone["outcome"] != json!("no-op"),
            format!("Undo answered {undone}"),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset2,
            &CORNERS_AND_CENTRE,
            &samples_after_a,
            "after undo",
        )?;

        call(
            &owner,
            editor,
            "history.redo",
            json!({"asset_id": asset2, "mutation": mutation(current_revision(&owner, editor, &asset2)?, "redo-b")}),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset2,
            &CORNERS_AND_CENTRE,
            &samples_after_b,
            "after redo",
        )?;

        let revision_before_preview = current_revision(&owner, editor, &asset2)?;
        call(
            &owner,
            editor,
            "preview.select",
            json!({"asset_id": asset2, "entry_id": entry_a}),
        )?;
        ensure(
            current_revision(&owner, editor, &asset2)? == revision_before_preview,
            "Previewing an entry committed something",
        )?;
        expect_probes(
            &owner,
            editor,
            &asset2,
            &CORNERS_AND_CENTRE,
            &samples_after_a,
            "the preview of entry A",
        )?;
        call(&owner, editor, "preview.return-current", json!({}))?;

        let restored_a = call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset2, "mutation": mutation(current_revision(&owner, editor, &asset2)?, "restore-a"), "entry_id": entry_a}),
        )?;
        ensure(
            restored_a["outcome"] == json!("applied") || restored_a["outcome"] == json!("no-op"),
            format!("Restoring entry A answered {restored_a}"),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset2,
            &CORNERS_AND_CENTRE,
            &samples_after_a,
            "after restoring A",
        )?;

        let restored_b = call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset2, "mutation": mutation(current_revision(&owner, editor, &asset2)?, "restore-b"), "entry_id": entry_b}),
        )?;
        ensure(
            restored_b["outcome"] == json!("applied") || restored_b["outcome"] == json!("no-op"),
            format!("Restoring entry B answered {restored_b}"),
        )?;
        expect_probes(
            &owner,
            editor,
            &asset2,
            &CORNERS_AND_CENTRE,
            &samples_after_b,
            "after restoring B",
        )?;
        record(
            "undo, redo, preview, restore and render.sample all evaluate byte-identically at every corner and the centre against the values recorded at commit A and commit B, and render.sample equals an independently rendered raster",
            json!({"entry_a": entry_a, "entry_b": entry_b, "raster_check": raster_check, "analysis_identities": analysis_check}),
        );

        // 4. Recentring: a vignette recomputes its mask on the stage a crop produces, not the one
        //    it was committed over. The centre is invariant on either stage (mask 0), a corner
        //    darkens on either stage, and this holds again after the crop moves.
        restore_to_original(
            &owner,
            editor,
            &asset,
            &original,
            "restore-before-recentring",
        )?;
        let recentre_asset = asset.clone();
        let revision = current_revision(&owner, editor, &recentre_asset)?;
        call(
            &owner,
            editor,
            "edit.crop",
            json!({"asset_id": recentre_asset, "mutation": mutation(revision, "recentre-crop-a"), "angle": 0.0, "x": 0.1, "y": 0.1, "width": 0.6, "height": 0.6}),
        )?;
        // Dimensions come from an independently rendered raster of the committed recipe, not a
        // guess: the JSON API has no dedicated "current stage size" method, and `render.sample`
        // itself refuses an out-of-range pixel, so a wrong guess would fail loudly rather than
        // silently, but the render is authoritative and needs no guess at all.
        let stage_a_raster = render(&source, &current_recipe(&owner, editor, &recentre_asset)?)?;
        let (stage_a_w, stage_a_h) = (
            u64::from(stage_a_raster.width),
            u64::from(stage_a_raster.height),
        );
        let corner_a = ((stage_a_w.max(2) - 1) as u32, (stage_a_h.max(2) - 1) as u32);
        let centre_a = ((stage_a_w / 2) as u32, (stage_a_h / 2) as u32);
        let baseline_a_centre = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": recentre_asset, "x": centre_a.0, "y": centre_a.1}),
        )?["rgba"]
            .clone();
        let baseline_a_corner = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": recentre_asset, "x": corner_a.0, "y": corner_a.1}),
        )?["rgba"]
            .clone();

        let revision = current_revision(&owner, editor, &recentre_asset)?;
        call(
            &owner,
            editor,
            "edit.set-vignette",
            json!({"asset_id": recentre_asset, "mutation": mutation(revision, "recentre-vignette"), "amount": -60.0}),
        )?;
        let described = call(
            &owner,
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
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": recentre_asset, "x": centre_a.0, "y": centre_a.1}),
        )?["rgba"]
            .clone();
        let vignetted_a_corner = call(
            &owner,
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
        let revision = current_revision(&owner, editor, &recentre_asset)?;
        call(
            &owner,
            editor,
            "edit.crop",
            json!({"asset_id": recentre_asset, "mutation": mutation(revision, "recentre-crop-b"), "angle": 0.0, "x": 0.2, "y": 0.2, "width": 0.4, "height": 0.4}),
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": recentre_asset}),
        )?;
        let recentre_layers = described["layers"].as_array().ok_or("no layers")?;
        ensure(
            recentre_layers.last().ok_or("no layers")?["effect"] == json!(VIGNETTE_EFFECT),
            format!("The vignette is no longer last after the crop moved: {recentre_layers:?}"),
        )?;
        let stage_b_raster = render(&source, &current_recipe(&owner, editor, &recentre_asset)?)?;
        let (stage_b_w, stage_b_h) = (
            u64::from(stage_b_raster.width),
            u64::from(stage_b_raster.height),
        );
        let corner_b = ((stage_b_w.max(2) - 1) as u32, (stage_b_h.max(2) - 1) as u32);
        let centre_b = ((stage_b_w / 2) as u32, (stage_b_h / 2) as u32);
        let with_vignette_b_centre = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": recentre_asset, "x": centre_b.0, "y": centre_b.1}),
        )?["rgba"]
            .clone();
        let with_vignette_b_corner = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": recentre_asset, "x": corner_b.0, "y": corner_b.1}),
        )?["rgba"]
            .clone();

        // The amount-0 baseline for the *new* stage, so the comparison is against the stage the
        // crop update actually produced, not the pre-crop 96x64-style stage from before.
        let revision = current_revision(&owner, editor, &recentre_asset)?;
        call(
            &owner,
            editor,
            "edit.set-vignette",
            json!({"asset_id": recentre_asset, "mutation": mutation(revision, "recentre-vignette-off"), "amount": 0.0}),
        )?;
        let baseline_b_centre = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": recentre_asset, "x": centre_b.0, "y": centre_b.1}),
        )?["rgba"]
            .clone();
        let baseline_b_corner = call(
            &owner,
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

        // 5. Two-client races: a conflicted draft, a refused commit, a successful reapply, and a
        //    historical selection that survives another client's commit.
        restore_to_original(&owner, editor, &asset, &original, "restore-before-races")?;
        let races_asset = asset.clone();
        let races_original = original.clone();
        let revision = current_revision(&owner, editor, &races_asset)?;
        let draft = call(
            &owner,
            editor,
            "draft.begin",
            json!({"asset_id": races_asset, "action": "set-vignette"}),
        )?;
        let draft_id = draft["draft_id"].clone();
        call(
            &owner,
            editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"amount": 25.0}}),
        )?;
        call(
            &owner,
            agent,
            "edit.set-basic",
            json!({"asset_id": races_asset, "mutation": mutation(revision, "agent-temperature"), "temperature": 40.0}),
        )?;
        let conflicted = call(&owner, editor, "draft.read", json!({"draft_id": draft_id}))?;
        ensure(
            conflicted["conflicted"] == json!(true)
                && conflicted["fields"] == json!({"amount": 25.0}),
            format!("The drafted gesture answered {conflicted}"),
        )?;
        let (code, _) = refused(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "editor-commit")}),
        )?;
        ensure(
            code == "conflict",
            format!("A conflicted commit was refused with {code}"),
        )?;
        let reapplied = call(
            &owner,
            editor,
            "draft.reapply",
            json!({"draft_id": draft_id}),
        )?;
        let rebased = current_revision(&owner, editor, &races_asset)?;
        ensure(
            reapplied["conflicted"] == json!(false) && reapplied["base_revision"] == json!(rebased),
            format!("Reapply answered {reapplied}"),
        )?;
        let before_sequence = call(
            &owner,
            editor,
            "history.list",
            json!({"asset_id": races_asset, "limit": 1}),
        )?["entries"][0]["sequence"]
            .clone();
        let committed = call(
            &owner,
            editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(rebased, "editor-commit-rebased")}),
        )?;
        ensure(
            committed["outcome"] == json!("applied") && committed["revision"] == json!(rebased + 1),
            format!("The reapplied commit answered {committed}"),
        )?;
        let after_sequence = call(
            &owner,
            editor,
            "history.list",
            json!({"asset_id": races_asset, "limit": 1}),
        )?["entries"][0]["sequence"]
            .clone();
        ensure(
            after_sequence.as_u64() == before_sequence.as_u64().map(|s| s + 1),
            "The reapplied commit created more or fewer than one entry",
        )?;
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": races_asset}),
        )?;
        let (_, _, vignette_values) = described_layer(&described, VIGNETTE_EFFECT)?;
        let (_, _, basic_values) = described_layer(&described, BASIC_EFFECT)?;
        ensure(
            field(&vignette_values, "amount")? == 25.0
                && field(&basic_values, "temperature")? == 40.0,
            format!("Reapply lost a field: vignette {vignette_values}, basic {basic_values}"),
        )?;

        call(
            &owner,
            editor,
            "preview.select",
            json!({"asset_id": races_asset, "entry_id": races_original}),
        )?;
        let selected_report = ready_report(
            &owner,
            editor,
            &races_asset,
            json!({"kind": "entry", "entry_id": races_original}),
            "the selected historical entry",
        )?;
        call(
            &owner,
            agent,
            "edit.set-basic",
            json!({"asset_id": races_asset, "mutation": mutation(current_revision(&owner, agent, &races_asset)?, "agent-during-preview"), "saturation": -60.0}),
        )?;
        let session = call(&owner, editor, "session.state", json!({}))?;
        ensure(
            session["preview"]["selection"] == json!({"entry": races_original}),
            format!("The selection moved to {}", session["preview"]["selection"]),
        )?;
        let sample = call(
            &owner,
            editor,
            "render.sample",
            json!({"asset_id": races_asset, "x": 0, "y": 0}),
        )?;
        ensure(
            sample["rgba"] == source_pixel(&source, 0, 0),
            "The previewed sample moved to the newest stack instead of staying on the Original entry",
        )?;
        call(&owner, editor, "preview.return-current", json!({}))?;
        record(
            "client A drafts the vignette's amount while client B commits a Basic field: conflicted, commit refused, reapply keeps both fields and commits exactly one entry; a historical selection on B stays selected through A's commit",
            json!({"reapplied": reapplied, "committed": committed, "vignette_values": vignette_values, "basic_values": basic_values, "selected_identity": selected_report["identity"]}),
        );

        // 6. Unavailable provider, and 7. catalog reopen, both against the asset's final committed
        //    state (whatever the races section above left it at).
        let final_recipe = current_recipe(&owner, editor, &asset2)?;
        let final_render = render(&source, &final_recipe)?;
        let final_state = call(&owner, editor, "asset.state", json!({"asset_id": asset2}))?;
        let final_revision = as_u64(&final_state["revision"], "revision")?;
        let final_entry = final_state["current_entry"]["id"].clone();
        let final_probes = sample_probes(&owner, editor, &asset2, &CORNERS_AND_CENTRE)?;
        owner.stop();
        join.take()
            .ok_or("The vignette owner thread was already joined")?
            .join()
            .map_err(|_| "The vignette owner thread panicked")?;

        let unavailable = unavailable_provider_check(
            &catalog,
            "lightwell.vignette",
            VIGNETTE_EFFECT,
            &asset2,
            &final_entry,
        )?;
        record(
            "the same catalog served with lightwell.vignette disabled refuses to render or sample the stack naming it, keeps the layer and module readable, and reports the analysis failed with no counts",
            unavailable,
        );

        let (reopened, reopened_join) = OwnerHandle::start(&catalog)?;
        let restart = (|| -> Result<Value> {
            let client = reopened.register();
            prepare_source(&reopened, client, &asset2)?;
            let state = call(
                &reopened,
                client,
                "asset.state",
                json!({"asset_id": asset2}),
            )?;
            ensure(
                as_u64(&state["revision"], "revision")? == final_revision,
                format!("The reopened revision is {}", state["revision"]),
            )?;
            ensure(
                state["current_entry"]["id"] == final_entry,
                "The reopened current entry changed",
            )?;
            let reopened_render = render(&source, &current_recipe(&reopened, client, &asset2)?)?;
            ensure(
                reopened_render.rgba == final_render.rgba,
                "The reopened render is not byte-identical",
            )?;
            expect_probes(
                &reopened,
                client,
                &asset2,
                &CORNERS_AND_CENTRE,
                &final_probes,
                "the reopened catalog",
            )?;
            Ok(json!({"revision": final_revision, "entry": final_entry}))
        })();
        reopened.stop();
        reopened_join
            .join()
            .map_err(|_| "The reopened vignette owner thread panicked")?;
        let restart_detail = restart?;
        record(
            "the catalog reopened: the vignette layer's values, the history and the render all reproduce byte-identically",
            restart_detail,
        );

        Ok(json!({
            "status": "passed",
            "fixture": FIXTURE,
            "asset_id": asset,
            "vignette_layer_id": vignette_layer,
            "checks": checks.borrow().clone(),
            "elapsed_ms": total.elapsed().as_secs_f64() * 1000.0,
        }))
    })();
    if let Some(join) = join.take() {
        owner.stop();
        let _ = join.join();
    }
    outcome
}
