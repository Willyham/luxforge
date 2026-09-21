use crate::*;
use lightwell_core::{
    CROP_EFFECT, CropPayload, CropStage, EditorService, ModuleRegistry, Mutation, MutationOutcome,
    ORIENTATION_EFFECT, Transform,
};
use std::time::Instant;

fn mutation(revision: u64, request: impl Into<String>) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "xtask-acceptance".into(),
    }
}

pub fn run(root: &Path, out: &Path) -> Result {
    ensure(!out.exists(), "Editor acceptance output must be new")?;
    fs::create_dir_all(out)?;
    let fixture = root.join("fixtures/s0/orientation-1.jpg");
    let fixture_hash = hash(&fixture)?;
    let catalog = out.join("catalog.sqlite");
    let mut result = json!({
        "status":"failed",
        "scope":["M1 history foundation","M2 basic transforms","M3 tool modules","M4 crop module core"],
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "platform":host(root)?,
        "fixture":"fixtures/s0/orientation-1.jpg",
        "fixture_sha256":fixture_hash,
        "method":"Exact core journey; timings use the recorded xtask profile and warm filesystem cache. Native UI evidence is recorded separately.",
    });
    let checked = (|| -> Result {
        let total = Instant::now();
        // Registration builds descriptor lookups only; first use is measured separately below.
        let registration_started = Instant::now();
        let registry = ModuleRegistry::builtin();
        let module_registration_ms = registration_started.elapsed().as_secs_f64() * 1000.0;
        let modules: Vec<String> = registry
            .descriptors()
            .iter()
            .map(|descriptor| descriptor.id.clone())
            .collect();
        let actions: Vec<String> = registry
            .descriptors()
            .iter()
            .flat_map(|descriptor| descriptor.actions.iter())
            .map(|action| action.id.clone())
            .collect();
        ensure(
            modules == ["lightwell.pixel", "lightwell.transform", "lightwell.crop"]
                && actions == ["set-pixel", "transform", "crop", "crop-fit", "crop-reset"],
            "Built-in module discovery changed",
        )?;
        drop(registry);
        let started = Instant::now();
        let mut service = EditorService::open(&catalog)?;
        let state = service.import(&fixture)?;
        let import_ms = started.elapsed().as_secs_f64() * 1000.0;
        let asset = state.asset.id.clone();
        let original = state.current_entry.id.clone();
        let first_use_started = Instant::now();
        let first_render = service.render_current(&asset)?;
        let module_first_use_ms = first_use_started.elapsed().as_secs_f64() * 1000.0;
        let source_pixel = first_render.pixel(0, 0).unwrap();

        let a = service.apply_pixel(&asset, mutation(0, "pixel-a"), 0, 0, [1, 2, 3])?;
        ensure(
            a.outcome == MutationOutcome::Applied,
            "Pixel A was not applied",
        )?;
        let a_entry = a.current_entry_id;
        let b = service.apply_pixel(&asset, mutation(1, "pixel-b"), 0, 0, [4, 5, 6])?;
        ensure(
            service.render_current(&asset)?.pixel(0, 0) == Some([4, 5, 6, 255]),
            "Pixel B did not win stack order",
        )?;
        let preview_started = Instant::now();
        let preview_a = service.render_entry(&asset, &a_entry)?;
        let preview_ms = preview_started.elapsed().as_secs_f64() * 1000.0;
        ensure(
            preview_a.pixel(0, 0) == Some([1, 2, 3, 255]),
            "Historical preview A changed",
        )?;
        ensure(
            service.state(&asset)?.current_entry.id == b.current_entry_id,
            "Preview changed committed state",
        )?;

        service.undo(&asset, mutation(2, "undo-b"))?;
        service.redo(&asset, mutation(3, "redo-b"))?;
        service.restore(&asset, mutation(4, "restore-a"), &a_entry)?;
        service.apply_pixel(&asset, mutation(5, "pixel-c"), 1, 0, [7, 8, 9])?;
        service.apply_transform(&asset, mutation(6, "rotate-right"), Transform::RotateRight)?;
        service.apply_transform(
            &asset,
            mutation(7, "mirror-horizontal"),
            Transform::MirrorHorizontal,
        )?;
        service.apply_transform(
            &asset,
            mutation(8, "flip-vertical"),
            Transform::FlipVertical,
        )?;

        let long_log_started = Instant::now();
        for index in 0..200u64 {
            let revision = 9 + index;
            service.apply_transform(
                &asset,
                mutation(revision, format!("long-log-{index:03}")),
                Transform::MirrorHorizontal,
            )?;
        }
        let long_log_commit_ms = long_log_started.elapsed().as_secs_f64() * 1000.0;
        let page_started = Instant::now();
        let mut cursor = None;
        let mut listed = 0usize;
        loop {
            let page = service.history(&asset, cursor, 25)?;
            listed += page.entries.len();
            match page.next_before_sequence {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let history_paging_ms = page_started.elapsed().as_secs_f64() * 1000.0;
        ensure(
            listed == 208,
            format!("Expected 208 retained entries, found {listed}"),
        )?;

        // Two hundred and three transform actions are 203 history entries and one orientation
        // layer: the stack describes the resulting orientation, not the gestures that reached it.
        let orientations: Vec<Value> = service
            .state(&asset)?
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .filter(|layer| layer.effect_id == ORIENTATION_EFFECT)
            .map(|layer| layer.payload.clone())
            .collect();
        ensure(
            orientations == [json!({"mirror": false, "turns": 3})],
            format!(
                "Expected one orientation layer at three quarter turns, found {orientations:?}"
            ),
        )?;

        // Crop: one angle-zero rectangle that must be an exact copy of its input stage, then a
        // straightened 16:9 fit that updates the same layer in place.
        let before_crop = service.render_current(&asset)?;
        ensure(
            (before_crop.width, before_crop.height) == (320, 480),
            "Crop input stage is not the transformed 320x480 stage",
        )?;
        let crop_started = Instant::now();
        let cropped_entry = service
            .apply_action(
                &asset,
                mutation(209, "crop-exact"),
                "crop",
                json!({"x":0.25,"y":0.25,"width":0.5,"height":0.5}),
            )?
            .current_entry_id;
        let crop_commit_ms = crop_started.elapsed().as_secs_f64() * 1000.0;
        let crop_layer = service
            .entry(&asset, &cropped_entry)?
            .snapshot
            .recipe
            .layers
            .iter()
            .find(|layer| layer.effect_id == CROP_EFFECT)
            .ok_or("Crop did not append a crop layer")?
            .id
            .clone();
        let cropped = service.render_current(&asset)?;
        ensure(
            (cropped.width, cropped.height) == (160, 240),
            format!(
                "Exact crop dimensions are {}x{}, expected 160x240",
                cropped.width, cropped.height
            ),
        )?;
        let row_bytes = 160 * 4;
        for y in 0..240usize {
            let start = (120 + y) * 320 * 4 + 80 * 4;
            ensure(
                cropped.rgba[y * row_bytes..(y + 1) * row_bytes]
                    == before_crop.rgba[start..start + row_bytes],
                format!("Exact crop row {y} is not a byte-for-byte copy of its input stage"),
            )?;
        }
        let crop_corner = cropped.pixel(0, 0).ok_or("Crop has no first pixel")?;
        ensure(
            Some(crop_corner) == before_crop.pixel(80, 120),
            "Exact crop origin pixel does not match its input stage",
        )?;

        // Content-space edits: a pixel set on the cropped photograph addresses the content stage
        // (the 480x320 source), lands before the orientation layer and the crop, and stays put
        // while the crop moves. Three quarter turns map content (10, 10) to (10, 469) of the
        // 320x480 stage the crop reads, which the quarter crop at (80, 120) does not show.
        let content_pixel_entry = service
            .apply_action(
                &asset,
                mutation(210, "content-pixel"),
                "set-pixel",
                json!({"x":10,"y":10,"rgb":[7,8,9]}),
            )?
            .current_entry_id;
        let content_layers = service
            .entry(&asset, &content_pixel_entry)?
            .snapshot
            .recipe
            .layers;
        let pixel_index = content_layers
            .iter()
            .position(|layer| layer.payload == json!({"x":10,"y":10,"rgb":[7,8,9]}))
            .ok_or("The content pixel layer is missing")?;
        let geometry_index = content_layers
            .iter()
            .position(|layer| layer.effect_id == ORIENTATION_EFFECT)
            .ok_or("The orientation layer is missing")?;
        ensure(
            pixel_index < geometry_index,
            "The content pixel was not placed before the geometry tail",
        )?;
        ensure(
            service.render_current(&asset)?.rgba == cropped.rgba,
            "A content pixel outside the quarter crop changed its rendered bytes",
        )?;
        // Moving the crop to the lower-left quarter uncovers the pixel at (10, 229) of the output
        // and is never rejected because of it; moving it back keeps the same crop layer.
        service.apply_action(
            &asset,
            mutation(211, "crop-lower-left"),
            "crop",
            json!({"x":0.0,"y":0.5,"width":0.5,"height":0.5}),
        )?;
        let uncovered = service.render_current(&asset)?;
        ensure(
            (uncovered.width, uncovered.height) == (160, 240)
                && uncovered.pixel(10, 229) == Some([7, 8, 9, 255]),
            format!(
                "The content pixel did not stay at content (10, 10) through the crop move: {:?}",
                uncovered.pixel(10, 229)
            ),
        )?;
        let located =
            service.locate_entry(&asset, &service.state(&asset)?.current_entry.id, 10, 229)?;
        ensure(
            (located.content_x, located.content_y) == (10, 10),
            format!("render.locate answered {located:?} for the content pixel"),
        )?;
        service.apply_action(
            &asset,
            mutation(212, "crop-back"),
            "crop",
            json!({"x":0.25,"y":0.25,"width":0.5,"height":0.5}),
        )?;
        ensure(
            service.render_current(&asset)?.pixel(0, 0) == Some(crop_corner)
                && service
                    .state(&asset)?
                    .current_entry
                    .snapshot
                    .recipe
                    .layers
                    .iter()
                    .filter(|layer| layer.effect_id == CROP_EFFECT)
                    .all(|layer| layer.id == crop_layer),
            "Moving the crop back did not restore the exact quarter on the same layer",
        )?;

        let fit_started = Instant::now();
        let fitted_entry = service
            .apply_action(
                &asset,
                mutation(213, "crop-fit-16-9"),
                "crop-fit",
                json!({"aspect":"16:9","angle":10.0}),
            )?
            .current_entry_id;
        let fit_commit_ms = fit_started.elapsed().as_secs_f64() * 1000.0;
        let fitted_layers = service.entry(&asset, &fitted_entry)?.snapshot.recipe.layers;
        let fitted_layer = fitted_layers
            .iter()
            .find(|layer| layer.effect_id == CROP_EFFECT)
            .ok_or("The fit lost the crop layer")?;
        ensure(
            fitted_layer.id == crop_layer,
            "The fit did not update the crop layer in place",
        )?;
        ensure(
            fitted_layers
                .iter()
                .filter(|layer| layer.effect_id == CROP_EFFECT)
                .count()
                == 1,
            "More than one crop layer in the stack",
        )?;
        let fitted_payload: CropPayload = serde_json::from_value(fitted_layer.payload.clone())?;
        let fitted_rect = fitted_payload.output_rect(&CropStage {
            width: 320,
            height: 480,
            angle: fitted_payload.angle,
        })?;
        let angled_render_started = Instant::now();
        let angled = service.render_current(&asset)?;
        let angled_render_ms = angled_render_started.elapsed().as_secs_f64() * 1000.0;
        ensure(
            (angled.width, angled.height) == (fitted_rect.width, fitted_rect.height),
            format!(
                "Straightened crop renders {}x{}, its payload declares {}x{}",
                angled.width, angled.height, fitted_rect.width, fitted_rect.height
            ),
        )?;
        let fitted_ratio = f64::from(angled.width) / f64::from(angled.height);
        ensure(
            (fitted_ratio - 16.0 / 9.0).abs() <= 3.0 / f64::from(angled.height),
            format!("Straightened crop ratio is {fitted_ratio}, expected 16:9 within a pixel"),
        )?;
        drop(service);

        let reopen_started = Instant::now();
        let service = EditorService::open(&catalog)?;
        let reopened = service.state(&asset)?;
        let reopen_ms = reopen_started.elapsed().as_secs_f64() * 1000.0;
        ensure(reopened.revision == 214, "Revision did not survive reopen")?;
        ensure(
            service
                .entry(&asset, &original)?
                .snapshot
                .recipe
                .layers
                .is_empty(),
            "Original snapshot did not survive reopen",
        )?;
        ensure(
            service.render_entry(&asset, &a_entry)?.pixel(0, 0) == Some([1, 2, 3, 255]),
            "Historical pixel did not survive reopen",
        )?;
        let render_started = Instant::now();
        let current = service.render_current(&asset)?;
        let current_render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
        ensure(
            (current.width, current.height) == (fitted_rect.width, fitted_rect.height),
            format!(
                "Crop dimensions did not survive reopen: {}x{} against {}x{}",
                current.width, current.height, fitted_rect.width, fitted_rect.height
            ),
        )?;
        let reopened_layers = reopened.current_entry.snapshot.recipe.layers.clone();
        ensure(
            reopened_layers
                .iter()
                .any(|layer| layer.effect_id == CROP_EFFECT && layer.id == crop_layer),
            "The crop layer's identity did not survive reopen",
        )?;
        ensure(
            service.render_entry(&asset, &cropped_entry)?.pixel(0, 0) == Some(crop_corner),
            "The exact crop's pixels did not survive reopen",
        )?;
        let mut after_cursor = None;
        let mut retained = 0usize;
        loop {
            let page = service.history(&asset, after_cursor, 50)?;
            retained += page.entries.len();
            match page.next_before_sequence {
                Some(next) => after_cursor = Some(next),
                None => break,
            }
        }
        ensure(
            retained == listed + 5,
            format!(
                "Expected {} retained entries after the crop journey, found {retained}",
                listed + 5
            ),
        )?;
        ensure(hash(&fixture)? == fixture_hash, "Original source changed")?;
        result["status"] = json!("passed");
        result["asset_id"] = json!(asset);
        result["original_entry_id"] = json!(original);
        result["pixel_a_entry_id"] = json!(a_entry);
        result["revision"] = json!(reopened.revision);
        result["history_entries"] = json!(listed);
        result["history_entries_after_crop"] = json!(retained);
        result["current_dimensions"] = json!([current.width, current.height]);
        result["current_layers"] = json!(reopened_layers.len());
        result["orientation_layers"] = json!(orientations);
        result["source_pixel_before_edits"] = json!(source_pixel);
        result["crop_layer_id"] = json!(crop_layer);
        result["crop_entry_id"] = json!(cropped_entry);
        result["crop_fit_entry_id"] = json!(fitted_entry);
        result["content_pixel"] = json!({
            "entry_id": content_pixel_entry,
            "layer_index": pixel_index,
            "first_geometry_index": geometry_index,
            "located": [located.content_x, located.content_y],
        });
        result["exact_crop"] = json!({
            "input_dimensions":[before_crop.width,before_crop.height],
            "output_dimensions":[cropped.width,cropped.height],
            "origin_pixel":crop_corner,
        });
        result["straightened_crop"] = json!({
            "payload":fitted_layer.payload,
            "output_dimensions":[angled.width,angled.height],
            "ratio":fitted_ratio,
        });
        result["modules"] = json!(modules);
        result["actions"] = json!(actions);
        result["timings_ms"] = json!({
            "module_registration":module_registration_ms,
            "module_first_use":module_first_use_ms,
            "import":import_ms,
            "historical_preview":preview_ms,
            "long_log_200_commits":long_log_commit_ms,
            "history_208_entries_paged_by_25":history_paging_ms,
            "exact_crop_commit":crop_commit_ms,
            "crop_fit_commit":fit_commit_ms,
            "straightened_crop_render":angled_render_ms,
            "catalog_reopen":reopen_ms,
            "current_render_full_stack":current_render_ms,
            "total":total.elapsed().as_secs_f64()*1000.0,
        });
        result["catalog_bytes"] = json!(fs::metadata(&catalog)?.len());
        result["checks"] = json!([
            "Original -> pixel A -> pixel B ordering",
            "Read-only historical preview",
            "Undo/redo and Restore A -> pixel C",
            "Exact rotate-right, mirror-horizontal and flip-vertical",
            "Two hundred and three transform actions compose into one orientation layer, with bounded history paging over their 203 entries",
            "Angle-zero crop is a byte-for-byte copy of its input stage with exact dimensions",
            "Straightened 16:9 crop-fit updates the one crop layer in place and renders its declared stage",
            "Catalog reopen retains revision, identities, snapshots, the crop layer and dimensions",
            "Module registry and descriptor discovery",
            "Source SHA-256 unchanged"
        ]);
        Ok(())
    })();
    if let Err(error) = &checked {
        result["error"] = json!(error.to_string());
    }
    write_json(&out.join("result.json"), &result)?;
    fs::write(
        out.join("README.md"),
        "# M1/M2/M3/M4 automated acceptance\n\nRun from the repository root with:\n\n```sh\ncargo xtask editor-acceptance --output NEW_DIRECTORY\n```\n\n`result.json` records exact state, hashes, timings and the tested platform. Native UI capture and platform classification are recorded in the engineering results document.\n",
    )?;
    checked?;
    println!("PASS M1/M2/M3/M4 editor acceptance: {}", out.display());
    Ok(())
}
