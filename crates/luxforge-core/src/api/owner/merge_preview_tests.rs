//! The merge preview through the real owner and the shared registry, with several clients
//! (`docs/design/panorama-merge.md`, "The merge preview"): a preview of a synthetic bracket answers
//! a picture, diagnostics and motion overlays; updates read no frame and a newer one cancels a
//! stale one; a merge of the preview reads and aligns nothing and equals a merge without a preview
//! bit for bit; the lane, the temporary ceiling, idleness, a disconnect, cancellation and restart
//! cleanup bound the sessions; and a stale `preview_id` is a conflict.
//!
//! The synthetic camera and the bench are the HDR merge job's (`hdr_merge_job.rs`): each input
//! file is read once by the camera, which counts every read.
use super::*;
use luxforge_test_macros::slow;
use std::time::Duration;

fn preview_params(bench: &Bench, bracket: usize) -> Value {
    json!({"kind": "hdr", "targets": {"kind": "paths", "paths": bench.paths(bracket)}})
}

/// Open `client`'s preview of `bracket` and wait for its first revision.
fn preview(bench: &Bench, client: ClientId, bracket: usize) -> Value {
    let started = bench
        .call(client, "merge.preview", preview_params(bench, bracket))
        .unwrap_or_else(|error| panic!("merge.preview: {error}"));
    let read = bench.settled(&started["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    assert_eq!(read["kind"], "merge-preview");
    read["result"].clone()
}

/// Update `client`'s preview to `hdr` and wait for the revision.
fn update(bench: &Bench, client: ClientId, preview_id: &Value, hdr: Value) -> Value {
    let started = bench
        .call(
            client,
            "merge.preview-update",
            json!({"preview_id": preview_id, "hdr": hdr}),
        )
        .unwrap_or_else(|error| panic!("merge.preview-update: {error}"));
    let read = bench.settled(&started["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    read["result"].clone()
}

/// The open preview sessions on the owner.
fn sessions(bench: &Bench) -> usize {
    bench.on_owner(|owner| owner.catalog.merge.previews.count())
}

/// The temporary bytes ready previews keep.
fn held(bench: &Bench) -> u64 {
    bench.on_owner(|owner| {
        owner
            .catalog
            .merge
            .previews
            .held
            .load(std::sync::atomic::Ordering::SeqCst)
    })
}

/// A preview's spool folder.
fn folder(bench: &Bench, preview_id: &Value) -> PathBuf {
    bench
        .root
        .join("merge-previews")
        .join(preview_id.as_str().unwrap())
}

/// The previews' pictures folder.
fn pictures(bench: &Bench) -> PathBuf {
    crate::index_dir(&bench.catalog)
        .join(crate::PREVIEWS_DIR)
        .join("merges")
}

/// Every file in `folder`.
fn files(folder: &Path) -> Vec<PathBuf> {
    fs::read_dir(folder).map_or_else(
        |_| Vec::new(),
        |entries| entries.flatten().map(|entry| entry.path()).collect(),
    )
}

/// A JPEG's size, from its first frame header.
fn jpeg_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF], "a JPEG");
    let mut at = 2;
    while at + 9 < bytes.len() {
        assert_eq!(bytes[at], 0xFF);
        let marker = bytes[at + 1];
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if matches!(marker, 0xC0..=0xC3) {
            let height = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]);
            let width = u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]);
            return (u32::from(width), u32::from(height));
        }
        at += 2 + length;
    }
    panic!("no frame header");
}

/// Hold every merge and preview job from now on as it reaches `stage`, until `gate` opens.
fn hold(bench: &Bench, stage: Stage, gate: &Arc<Gate>) {
    gate.shut();
    bench.hold_at(stage, gate.clone());
}

/// Record every stage a job reaches from now on.
fn record_stages(bench: &Bench) -> Arc<Mutex<Vec<Stage>>> {
    let stages = Arc::new(Mutex::new(Vec::new()));
    let seen = stages.clone();
    bench.lane(move |lane| {
        lane.hooks.stage = Some(Arc::new(move |stage| seen.lock().unwrap().push(stage)));
    });
    stages
}

/// Through the registry, a preview of a moving bracket reads each frame once on the merge lane,
/// reporting its phases, and answers a JPEG under the preview cache with its diagnostics, the
/// deghost overlay in its own pixels and the output the merge would save, publishing nothing.
#[test]
#[slow]
fn merge_preview_answers_a_picture_diagnostics_and_motion_overlays_through_the_registry() {
    let bench = Bench::new("preview-picture", 1);
    bench
        .developer
        .moving
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let before = bench.untouched();
    let changes = bench.count("SELECT COUNT(*) FROM library_changes");

    // The commands are registered with their schemas.
    let schema = bench.ok("schema.list", json!({}));
    for method in [
        "merge.preview",
        "merge.preview-update",
        "merge.preview-close",
    ] {
        assert!(
            schema["methods"][method]["notes"].is_string(),
            "{method} is registered"
        );
    }
    let kind = schema["methods"]["merge.preview"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|parameter| parameter["name"] == "kind")
        .cloned()
        .unwrap();
    assert_eq!(kind["options"], json!(["hdr", "panorama"]));
    // A build without the panorama stitch: not-ready, naming its provider.
    bench.lane(|lane| lane.remove_panorama());
    let panorama = bench.failure(
        "merge.preview",
        json!({"kind": "panorama", "targets": {"kind": "paths", "paths": bench.paths(0)}}),
    );
    assert_eq!(panorama["code"], "not-ready", "{panorama}");
    assert_eq!(panorama["data"]["provider"], "luxforge.panorama");

    // Held at its second read, the job says where it is.
    let gate = Arc::new(Gate::new());
    hold(&bench, Stage::Reading(0), &gate);
    let started = bench.ok("merge.preview", preview_params(&bench, 0));
    gate.wait_reached(1, "the preview's second read");
    let running = bench.job(&started["job_id"]);
    assert_eq!(running["kind"], "merge-preview", "{running}");
    assert_eq!(running["status"], "running");
    assert_eq!(
        running["progress"]["message"], "Reading 2 of 3",
        "{running}"
    );
    assert_eq!(running["result"]["stage"]["stage"], "reading", "{running}");
    let activity = bench.ok("activity.list", json!({}));
    assert!(
        activity.to_string().contains("Preparing merge preview"),
        "{activity}"
    );
    gate.open();
    let read = bench.settled(&started["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    let result = read["result"].clone();
    assert_eq!(bench.reads(), 3, "each frame read once");

    let preview_id = result["preview_id"].as_str().unwrap();
    assert!(preview_id.starts_with("merge-preview-"), "{result}");
    assert_eq!(result["revision"], 1);
    assert_eq!(result["kind"], "hdr");
    assert_eq!(
        result["options"],
        json!({"hdr": {"alignment": "auto", "deghost": "auto"}})
    );
    assert!(result.get("refusal").is_none(), "{result}");

    // The picture: a JPEG of the answer's size under the preview cache.
    let path = PathBuf::from(result["path"].as_str().unwrap());
    assert_eq!(path.parent().unwrap(), pictures(&bench));
    assert_eq!(
        path.file_name().unwrap().to_string_lossy(),
        format!("{preview_id}-1.jpg")
    );
    let (width, height) = (
        result["width"].as_u64().unwrap() as u32,
        result["height"].as_u64().unwrap() as u32,
    );
    assert_eq!(jpeg_size(&fs::read(&path).unwrap()), (width, height));
    assert!(width > 300 && width <= W as u32 && height > 200 && height <= H as u32);

    // The diagnostics.
    let diagnostics = &result["diagnostics"]["hdr"];
    assert_eq!(diagnostics["proxy_side"], W as u64, "{diagnostics}");
    assert_eq!(diagnostics["alignment"], "auto");
    assert_eq!(diagnostics["deghost"], "auto");
    assert_eq!(diagnostics["reference"], 1);
    let residual = diagnostics["max_residual_px"].as_f64().unwrap();
    assert!(residual > 0.0 && residual < 0.5, "{diagnostics}");
    assert!(diagnostics["highlights_extended"].is_boolean());
    assert!(diagnostics["shadows_extended"].is_boolean());
    let exposures: Vec<f64> = diagnostics["exposures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|offset| offset.as_f64().unwrap())
        .collect();
    for (offset, expected) in exposures.iter().zip(OFFSETS) {
        assert!((offset - expected).abs() < 1e-6, "{exposures:?}");
    }

    // The deghost overlay: the moving disc, from the reference, in the picture's pixels.
    let motion = result["overlays"]["hdr"]["motion"].as_array().unwrap();
    assert!(!motion.is_empty(), "{result}");
    for region in motion {
        let input = region["input"].as_u64().unwrap() as usize;
        assert!(input < 3);
        assert!((region["offset_ev"].as_f64().unwrap() - exposures[input]).abs() < 1e-9);
        assert!(region["fallback"].is_boolean());
        let bounds = &region["bounds"];
        let right = bounds["x"].as_u64().unwrap() + bounds["width"].as_u64().unwrap();
        let bottom = bounds["y"].as_u64().unwrap() + bounds["height"].as_u64().unwrap();
        assert!(right <= u64::from(width) && bottom <= u64::from(height));
        let outline = region["outline"].as_array().unwrap();
        assert!(outline.len() >= 3);
        for point in outline {
            let (x, y) = (point[0].as_f64().unwrap(), point[1].as_f64().unwrap());
            assert!((0.0..=f64::from(width)).contains(&x));
            assert!((0.0..=f64::from(height)).contains(&y));
        }
    }

    // The output the merge saves: its coverage, at the reference's scale.
    let output = &result["output"];
    let (out_w, out_h) = (
        output["width"].as_u64().unwrap(),
        output["height"].as_u64().unwrap(),
    );
    assert!(out_w <= W as u64 && out_h <= H as u64 && out_w >= u64::from(width));
    assert_eq!(output["scale"], 1.0);
    assert_eq!(output["source_bytes"], json!(out_w * out_h * 12));

    // The spool is kept for the merge and counted against the ceiling; nothing is published.
    let spooled = files(&folder(&bench, &result["preview_id"]));
    assert_eq!(spooled.len(), 3, "{spooled:?}");
    assert_eq!(held(&bench), 3 * (W * H) as u64 * job::SPOOL_PIXEL);
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0);
    assert_eq!(
        bench.count("SELECT COUNT(*) FROM library_changes"),
        changes,
        "a preview is no library change"
    );
    assert_eq!(bench.untouched(), before);

    // Closing discards the spool and the picture; closing again changes nothing.
    let closed = bench.ok(
        "merge.preview-close",
        json!({"preview_id": result["preview_id"]}),
    );
    assert_eq!(closed["closed"], true);
    let again = bench.ok(
        "merge.preview-close",
        json!({"preview_id": result["preview_id"]}),
    );
    assert_eq!(again["closed"], false);
    luxforge_testbase::wait_for("the preview's files to go", || {
        (!folder(&bench, &result["preview_id"]).exists() && !path.exists()).then_some(())
    });
    assert_eq!(held(&bench), 0);
    assert_eq!(sessions(&bench), 0);
    bench.originals_unchanged();
}

/// Updates read no frame: deghosting redraws, alignment and reference align the retained proxies
/// again, a refusal keeps the session open, each revision replaces the last picture, and a newer
/// update cancels a stale one.
#[test]
#[slow]
fn merge_preview_updates_read_no_frame_and_a_newer_update_cancels_a_stale_one() {
    let bench = Bench::new("preview-update", 1);
    bench
        .developer
        .moving
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let first = preview(&bench, bench.client, 0);
    let id = first["preview_id"].clone();
    assert_eq!(bench.reads(), 3);
    let first_path = PathBuf::from(first["path"].as_str().unwrap());

    // Deghosting off: the alignment stands, the overlay goes.
    let stages = record_stages(&bench);
    let off = update(&bench, bench.client, &id, json!({"deghost": "off"}));
    assert_eq!(off["revision"], 2);
    assert_eq!(off["preview_id"], id);
    assert_eq!(off["diagnostics"]["hdr"]["deghost"], "off");
    assert_eq!(
        off["diagnostics"]["hdr"]["max_residual_px"],
        first["diagnostics"]["hdr"]["max_residual_px"]
    );
    assert_eq!(off["overlays"]["hdr"]["motion"], json!([]));
    assert!(
        !stages.lock().unwrap().contains(&Stage::Aligning),
        "a deghosting change is a redraw: {stages:?}"
    );
    let off_path = PathBuf::from(off["path"].as_str().unwrap());
    assert!(off_path.to_string_lossy().ends_with("-2.jpg"));
    assert!(
        off_path.exists() && !first_path.exists(),
        "one picture a preview"
    );

    // Alignment off aligns again: within the framing tolerance, it draws the frames in place.
    let unaligned = update(&bench, bench.client, &id, json!({"alignment": "off"}));
    assert_eq!(unaligned["revision"], 3);
    assert_eq!(unaligned["diagnostics"]["hdr"]["alignment"], "off");
    assert!(unaligned.get("refusal").is_none(), "{unaligned}");
    assert!(stages.lock().unwrap().contains(&Stage::Aligning));

    // Overrides the frames' headers already refuse are refused at once, as merge.create
    // refuses them; the revision stands.
    let paths = bench.paths(0);
    let header = bench.failure(
        "merge.preview-update",
        json!({"preview_id": id, "hdr": {"exposure_overrides": [
            {"path": paths[0], "offset_ev": 1.0},
            {"path": paths[2], "offset_ev": 1.0},
        ]}}),
    );
    assert_eq!(header["code"], "validation", "{header}");
    assert!(header["data"]["reason"].is_string(), "{header}");

    // Another reference aligns again on the retained proxies.
    let reference = update(
        &bench,
        bench.client,
        &id,
        json!({"reference": bench.paths(0)[0]}),
    );
    assert_eq!(reference["revision"], 4);
    assert_eq!(
        reference["diagnostics"]["hdr"]["reference"], 0,
        "{reference}"
    );
    assert_eq!(
        reference["options"]["hdr"]["reference"],
        json!(bench.paths(0)[0])
    );
    assert_eq!(bench.reads(), 3, "updates read no frame");

    // A newer update cancels a stale one held as it aligns.
    let gate = Arc::new(Gate::new());
    hold(&bench, Stage::Aligning, &gate);
    let stale = bench.ok(
        "merge.preview-update",
        json!({"preview_id": id, "hdr": {"reference": bench.paths(0)[2]}}),
    );
    gate.wait_reached(1, "the stale update's alignment");
    let newer = bench.ok("merge.preview-update", json!({"preview_id": id, "hdr": {}}));
    gate.open();
    assert_eq!(bench.settled(&stale["job_id"])["status"], "cancelled");
    let read = bench.settled(&newer["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    assert_eq!(read["result"]["revision"], 6);
    assert_eq!(read["result"]["diagnostics"]["hdr"]["reference"], 1);
    assert_eq!(bench.reads(), 3);
    let pictures = files(&pictures(&bench));
    assert_eq!(pictures.len(), 1, "{pictures:?}");

    // Options naming no input, and another client's preview, are refused.
    let wrong = bench.failure(
        "merge.preview-update",
        json!({"preview_id": id, "hdr": {"reference": "/nowhere/DSC_9999.NEF"}}),
    );
    assert_eq!(wrong["code"], "validation", "{wrong}");
    let other = bench.owner.register();
    let theirs = bench
        .call(
            other,
            "merge.preview-update",
            json!({"preview_id": id, "hdr": {}}),
        )
        .unwrap_err();
    assert_eq!(theirs["code"], "conflict", "{theirs}");
}

/// A merge given a preview reads and aligns nothing and saves exactly what a merge without a
/// preview of the same options saves: the same pixels, bit for bit, and the same resolution.
#[test]
#[slow]
fn merge_preview_merge_reads_and_aligns_nothing_and_equals_a_merge_without_a_preview() {
    let plain = Bench::new("preview-equal-plain", 1);
    let previewed = Bench::new("preview-equal-previewed", 1);
    for bench in [&plain, &previewed] {
        bench
            .developer
            .moving
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let expected = plain.merged(0, "plain");

    let opened = preview(&previewed, previewed.client, 0);
    assert_eq!(previewed.reads(), 3);
    let stages = record_stages(&previewed);
    let mut params = previewed.create_params(0, "from-preview");
    params["preview_id"] = opened["preview_id"].clone();
    params["hdr"] = opened["options"]["hdr"].clone();
    let started = previewed.ok("merge.create", params);
    let read = previewed.settled(&started["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    assert_eq!(read["kind"], "hdr-merge");
    let result = read["result"].clone();
    assert_eq!(previewed.reads(), 3, "the merge read no frame");
    let reached = stages.lock().unwrap().clone();
    assert!(
        !reached.iter().any(|stage| matches!(
            stage,
            Stage::Reading(_) | Stage::Spooled(_) | Stage::Aligning
        )),
        "the merge read and aligned nothing: {reached:?}"
    );
    assert!(reached.contains(&Stage::Saved), "{reached:?}");

    // The same pixels, bit for bit.
    let pixels = |bench: &Bench, asset: &Value| -> (u32, u32, Vec<u32>) {
        let asset = AssetId::parse(asset.as_str().unwrap().to_owned()).unwrap();
        let read = bench
            .on_owner(move |owner| owner.service.derived_source_read(&asset))
            .unwrap();
        let image = crate::artifacts::derived_source::read_derived_source(
            &read,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        let bits = image.planes().iter().map(|value| value.to_bits()).collect();
        (image.width(), image.height(), bits)
    };
    assert_eq!(
        pixels(&previewed, &result["result_asset_id"]),
        pixels(&plain, &expected["result_asset_id"]),
        "the merged pixels are the same"
    );
    // The same resolution, options and frames.
    let inspect = |bench: &Bench, merge: &Value| {
        bench.ok("merge.inspect", json!({"merge_id": merge["merge_id"]}))["provenance"].clone()
    };
    let (ours, theirs) = (inspect(&previewed, &result), inspect(&plain, &expected));
    assert_eq!(ours["resolved"], theirs["resolved"]);
    assert_eq!(ours["parameters"], theirs["parameters"]);
    for at in 0..3 {
        assert_eq!(ours["inputs"][at]["frame"], theirs["inputs"][at]["frame"]);
    }
    assert!(
        ours["resolved"]["hdr"]["motion"]["regions"]
            .as_u64()
            .unwrap()
            > 0,
        "{ours}"
    );
    assert_eq!(result["file_name"], expected["file_name"]);

    // The preview is gone with its spool and picture.
    luxforge_testbase::wait_for("the merged preview to go", || {
        (sessions(&previewed) == 0
            && !folder(&previewed, &opened["preview_id"]).exists()
            && files(&pictures(&previewed)).is_empty())
        .then_some(())
    });
    assert_eq!(held(&previewed), 0);
    let gone = previewed.failure(
        "merge.preview-update",
        json!({"preview_id": opened["preview_id"], "hdr": {}}),
    );
    assert_eq!(gone["code"], "conflict");
    previewed.originals_unchanged();
}

/// A refusal the preview finds as it aligns is its answer, with no picture, and the session stays
/// open: a merge of that revision is refused with it, and another setting draws the frames.
#[test]
fn merge_preview_refusal_is_an_answer_and_keeps_the_session_open() {
    let bench = Bench::new("preview-refusal", 1);
    bench
        .developer
        .flat
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let refused = preview(&bench, bench.client, 0);
    let id = refused["preview_id"].clone();
    assert_eq!(refused["revision"], 1);
    let reason = refused["refusal"]["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("{refused}"));
    assert!(refused["refusal"]["message"].is_string(), "{refused}");
    assert!(refused.get("path").is_none(), "{refused}");
    assert!(refused.get("diagnostics").is_none(), "{refused}");
    assert_eq!(sessions(&bench), 1);
    assert!(files(&pictures(&bench)).is_empty());

    // A merge of the refused revision answers its refusal.
    let mut params = bench.create_params(0, "refused");
    params["preview_id"] = id.clone();
    let merge = bench.failure("merge.create", params);
    assert_eq!(merge["code"], "validation", "{merge}");
    assert_eq!(merge["data"]["reason"], reason, "{merge}");
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0);

    // Alignment off draws the textureless frames where they lie.
    let drawn = update(&bench, bench.client, &id, json!({"alignment": "off"}));
    assert_eq!(drawn["revision"], 2);
    assert!(drawn.get("refusal").is_none(), "{drawn}");
    assert!(PathBuf::from(drawn["path"].as_str().unwrap()).exists());
    assert_eq!(bench.reads(), 3);
}

/// A stale or foreign `preview_id` is a conflict, a refused revision refuses the merge, and a merge
/// of a preview still preparing continues it behind its own job, outside the lane's bound.
#[test]
#[slow]
fn merge_preview_stale_preview_id_is_a_conflict_and_a_preparing_preview_is_continued() {
    let bench = Bench::new("preview-stale", 3);
    let opened = preview(&bench, bench.client, 0);
    let id = opened["preview_id"].clone();
    let with = |request: &str, hdr: Value, preview_id: &Value| {
        let mut params = bench.create_params(0, request);
        params["preview_id"] = preview_id.clone();
        params["hdr"] = hdr;
        params
    };
    // Other options than the latest revision's.
    let other = bench.failure(
        "merge.create",
        with("other", json!({"deghost": "off"}), &id),
    );
    assert_eq!(other["code"], "conflict", "{other}");
    assert_eq!(other["data"]["revision"], 1);
    update(&bench, bench.client, &id, json!({"deghost": "off"}));
    let stale = bench.failure("merge.create", with("stale", json!({}), &id));
    assert_eq!(stale["code"], "conflict", "{stale}");
    assert_eq!(stale["data"]["revision"], 2);
    // A preview that is not open, or not the caller's.
    let unknown = bench.failure(
        "merge.create",
        with(
            "unknown",
            json!({}),
            &json!(crate::catalog_types::MergePreviewId::new()),
        ),
    );
    assert_eq!(unknown["code"], "conflict", "{unknown}");
    let stranger = bench.owner.register();
    let foreign = bench
        .call(
            stranger,
            "merge.create",
            with("foreign", json!({"deghost": "off"}), &id),
        )
        .unwrap_err();
    assert_eq!(foreign["code"], "conflict", "{foreign}");
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0);
    assert_eq!(bench.reads(), 3);

    // A preview still preparing, with a merge of another bracket already waiting: a merge of the
    // preview is queued behind it, outside the lane's bound, and reads nothing again.
    let gate = Arc::new(Gate::new());
    hold(&bench, Stage::Aligning, &gate);
    let started = bench.ok("merge.preview", preview_params(&bench, 0));
    gate.wait_reached(1, "the preview's alignment");
    assert_eq!(bench.job(&started["job_id"])["status"], "running");
    let waiting = bench.create(1, "waiting");
    let full = bench.failure("merge.create", bench.create_params(2, "full"));
    assert_eq!(full["code"], "resource-limit", "{full}");
    let preview_id =
        bench.on_owner(|owner| owner.catalog.merge.previews.open_ids().into_iter().next());
    let preview_id = json!(preview_id.expect("an open preview"));
    let continued = bench.ok("merge.create", with("continued", json!({}), &preview_id));
    assert_eq!(continued["status"], "queued", "{continued}");
    assert_eq!(continued["deduplicated"], false);
    // The preview is being merged: it takes no update now.
    let merging = bench.failure(
        "merge.preview-update",
        json!({"preview_id": preview_id, "hdr": {}}),
    );
    assert_eq!(merging["code"], "conflict", "{merging}");
    gate.open();
    assert_eq!(bench.settled(&started["job_id"])["status"], "ready");
    let read = bench.settled(&continued["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    assert_eq!(bench.settled(&waiting["job_id"])["status"], "ready");
    assert_eq!(
        bench.reads(),
        9,
        "each preview read its bracket once, the merge of the second nothing"
    );
    luxforge_testbase::wait_for("the merged preview to go", || {
        (sessions(&bench) == 0).then_some(())
    });
}

/// Previews share the merge lane's bound with merges and count their spools against the kind's
/// ceiling beside every merge's; a ready preview's client may keep it while another client opens
/// its own, and opening a second closes a client's first.
#[test]
#[slow]
fn merge_preview_sessions_share_the_lane_and_the_ceiling_across_clients() {
    let bench = Bench::new("preview-lane", 3);
    let second = bench.owner.register();
    // One preview reading, one merge waiting: a further preview or merge is refused.
    bench.developer.reading.shut();
    let preparing = bench.ok("merge.preview", preview_params(&bench, 0));
    bench
        .developer
        .reading
        .wait_reached(1, "the preview's first read");
    let waiting = bench.create(1, "waiting");
    let refused = bench
        .call(second, "merge.preview", preview_params(&bench, 1))
        .unwrap_err();
    assert_eq!(refused["code"], "resource-limit", "{refused}");
    assert_eq!(
        bench.failure("merge.create", bench.create_params(2, "third"))["code"],
        "resource-limit"
    );
    bench.developer.reading.open();
    assert_eq!(bench.settled(&preparing["job_id"])["status"], "ready");
    assert_eq!(bench.settled(&waiting["job_id"])["status"], "ready");
    let mine = bench.job(&preparing["job_id"])["result"].clone();

    // Another client's preview opens beside it; both count against the ceiling.
    let theirs = preview(&bench, second, 2);
    assert_eq!(sessions(&bench), 2);
    let one = 3 * (W * H) as u64 * job::SPOOL_PIXEL;
    assert_eq!(held(&bench), 2 * one);

    // A merge that would pass the ceiling beside them is refused naming what they hold.
    let need = job::temp_bytes(3, W as u32, H as u32);
    bench.lane(move |lane| lane.limits_mut(MergeKind::Hdr).temp_ceiling = 2 * one + need - 1);
    let over = bench.create(2, "over");
    let over = bench.settled(&over["job_id"]);
    assert_eq!(over["status"], "failed", "{over}");
    assert_eq!(over["error"]["code"], "resource-limit", "{over}");
    assert_eq!(
        over["error"]["data"]["held_bytes"],
        json!(2 * one),
        "{over}"
    );
    assert_eq!(bench.reads(), 10, "refused once its first frame was read");
    // A preview's own merge counts its spool once, so it fits.
    let mut params = bench.create_params(0, "own");
    params["preview_id"] = mine["preview_id"].clone();
    let own = bench.ok("merge.create", params);
    assert_eq!(bench.settled(&own["job_id"])["status"], "ready");
    assert_eq!(held(&bench), one);

    // The first client opens another preview: its first is closed, the other client's stays.
    bench.lane(|lane| *lane.limits_mut(MergeKind::Hdr) = Limits::of(MergeKind::Hdr));
    let again = preview(&bench, bench.client, 1);
    assert_eq!(sessions(&bench), 2);
    let next = preview(&bench, bench.client, 1);
    assert_eq!(sessions(&bench), 2, "one open preview a client");
    let closed = bench.failure(
        "merge.preview-update",
        json!({"preview_id": again["preview_id"], "hdr": {}}),
    );
    assert_eq!(closed["code"], "conflict");
    assert!(
        bench
            .call(
                second,
                "merge.preview-update",
                json!({"preview_id": theirs["preview_id"], "hdr": {}}),
            )
            .is_ok()
    );
    assert!(next["path"].is_string());
}

/// A preview closes when idle past the limit, when its client disconnects, and when its job is
/// cancelled or closed while it prepares; restart cleanup sweeps what a stopped process left.
#[test]
#[slow]
fn merge_preview_closes_when_idle_disconnected_cancelled_and_sweeps_after_a_restart() {
    let mut bench = Bench::new("preview-bounds", 1);
    // Idle: a short limit, which the owner's own wake-up enforces.
    bench.lane(|lane| lane.previews.idle = Duration::from_millis(300));
    let idle = preview(&bench, bench.client, 0);
    luxforge_testbase::wait_for("the idle preview to close", || {
        (sessions(&bench) == 0 && !folder(&bench, &idle["preview_id"]).exists()).then_some(())
    });
    let gone = bench.failure(
        "merge.preview-update",
        json!({"preview_id": idle["preview_id"], "hdr": {}}),
    );
    assert_eq!(gone["code"], "conflict");
    bench.lane(|lane| lane.previews.idle = Duration::from_secs(30 * 60));

    // A client that disconnects closes its preview; another client's stays.
    let leaving = bench.owner.register();
    let left = preview(&bench, leaving, 0);
    let stays = preview(&bench, bench.client, 0);
    assert_eq!(sessions(&bench), 2);
    bench.owner.disconnect(leaving);
    luxforge_testbase::wait_for("the disconnected client's preview to close", || {
        (sessions(&bench) == 1 && !folder(&bench, &left["preview_id"]).exists()).then_some(())
    });
    assert!(folder(&bench, &stays["preview_id"]).exists());

    // Cancelled while it reads: closed, its spool gone.
    bench.developer.reading.shut();
    let cancelled = bench.ok("merge.preview", preview_params(&bench, 0));
    bench
        .developer
        .reading
        .wait_reached(1, "the preview's read");
    bench.cancel(&cancelled["job_id"]);
    bench.developer.reading.open();
    assert_eq!(bench.settled(&cancelled["job_id"])["status"], "cancelled");
    luxforge_testbase::wait_for("the cancelled preview to close", || {
        (sessions(&bench) == 0).then_some(())
    });
    // Closed while it aligns: its job is cancelled.
    let gate = Arc::new(Gate::new());
    hold(&bench, Stage::Aligning, &gate);
    let closing = bench.ok("merge.preview", preview_params(&bench, 0));
    gate.wait_reached(1, "the preview's alignment");
    let open = bench.on_owner(|owner| owner.catalog.merge.previews.open_ids());
    let closed = bench.ok("merge.preview-close", json!({"preview_id": open[0]}));
    assert_eq!(closed["closed"], true);
    gate.open();
    assert_eq!(bench.settled(&closing["job_id"])["status"], "cancelled");
    bench.lane(|lane| lane.hooks.stage = None);
    luxforge_testbase::wait_for("the closed preview's spool to go", || {
        files(&bench.root.join("merge-previews"))
            .is_empty()
            .then_some(())
    });

    // What a stopped process left goes with the next job; an open preview's files stay.
    let kept = preview(&bench, bench.client, 0);
    bench.restart();
    let dead = bench.root.join("merge-previews").join("merge-preview-dead");
    fs::create_dir_all(&dead).unwrap();
    fs::write(dead.join(job::spool_name(0)), b"left").unwrap();
    let picture = pictures(&bench).join("merge-preview-dead-3.jpg");
    fs::create_dir_all(pictures(&bench)).unwrap();
    fs::write(&picture, b"left").unwrap();
    let after = bench.failure(
        "merge.preview-update",
        json!({"preview_id": kept["preview_id"], "hdr": {}}),
    );
    assert_eq!(after["code"], "conflict", "a restart forgets every preview");
    let fresh = preview(&bench, bench.client, 0);
    assert!(
        !dead.exists(),
        "the stopped process's preview spool is swept"
    );
    assert!(!picture.exists(), "its picture is swept");
    assert!(folder(&bench, &fresh["preview_id"]).exists());
    assert!(PathBuf::from(fresh["path"].as_str().unwrap()).exists());
    assert!(!folder(&bench, &kept["preview_id"]).exists());
    bench.originals_unchanged();
}
