//! `cargo xtask editor-acceptance`: what `cargo test` cannot prove at the same layer.
//!
//! A chapter exists only if no `cargo test` proves its property at that layer: the shared
//! field-patch conformance suite in release, Basic's numerics on the photo fixture against the
//! independent reference, the placement of Presence, the mixer and the vignette, and a masked
//! catalog reopened through a fresh owner. The M1 through M4 journey of history, orientation, crop
//! and reopen is the core's own tests (`editor::history`, `editor::plan`, `modules::transform` and
//! `modules::crop`), and the host behaviour every module shares is the conformance suite's.
use crate::*;
use std::time::Instant;

pub fn run(root: &Path, out: &Path) -> Result {
    ensure(!out.exists(), "Editor acceptance output must be new")?;
    fs::create_dir_all(out)?;
    let fixture = root.join("fixtures/s0/orientation-1.jpg");
    let fixture_hash = hash(&fixture)?;
    let mut result = json!({
        "status":"failed",
        "scope":["Basic adjustments and histogram","Field-patch module conformance in release","Presence, mixer and vignette placement","Mask reopen through a fresh owner"],
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "platform":host(root)?,
        "fixture":"fixtures/s0/orientation-1.jpg",
        "fixture_sha256":fixture_hash,
        "method":"Chapters driven through the JSON method table as independent clients, each against its own catalog in this directory; timings use the recorded xtask profile and warm filesystem cache. Native UI evidence is recorded separately.",
    });
    let checked = (|| -> Result {
        let total = Instant::now();
        // The Basic and histogram chapter runs as an independent JSON client against its own
        // catalog in the same output directory.
        let basic_started = Instant::now();
        let basic = basic_acceptance::run(root, out)?;
        let basic_ms = basic_started.elapsed().as_secs_f64() * 1000.0;

        // The field-patch conformance suite: the same function the core's
        // `modules` test runs as `field_patch`, here in release, over every field-patch module the
        // registry holds, each as its own independent JSON client against its own catalog in its
        // own directory. What it returns is this chapter's evidence.
        let conformance_started = Instant::now();
        let conformance_out = out.join("field-patch-conformance");
        fs::create_dir_all(&conformance_out)?;
        let field_patch_conformance = conformance::run(&fixture, &conformance_out)?;
        let conformance_ms = conformance_started.elapsed().as_secs_f64() * 1000.0;

        // What Presence, the mixer and the vignette each do that no other module does, each
        // against its own catalog in the same output directory.
        let pmv_started = Instant::now();
        let presence_mixer_vignette = presence_mixer_vignette_acceptance::run(root, out)?;
        let pmv_ms = pmv_started.elapsed().as_secs_f64() * 1000.0;

        // The masking chapter: a catalog of masks written by one owner and reopened by another.
        let masks_started = Instant::now();
        let masks = mask_acceptance::run(root, out)?;
        let masks_ms = masks_started.elapsed().as_secs_f64() * 1000.0;

        ensure(hash(&fixture)? == fixture_hash, "Original source changed")?;
        result["basic_and_histogram"] = basic;
        result["field_patch_conformance"] = field_patch_conformance;
        result["presence_mixer_vignette"] = presence_mixer_vignette;
        result["masks"] = masks;
        result["status"] = json!("passed");
        result["timings_ms"] = json!({
            "basic_and_histogram_chapter":basic_ms,
            "field_patch_conformance":conformance_ms,
            "presence_mixer_vignette_chapter":pmv_ms,
            "masks_chapter":masks_ms,
            "total":total.elapsed().as_secs_f64()*1000.0,
        });
        result["checks"] = json!([
            "Basic and histogram: the whole chapter under basic_and_histogram, driven through the JSON method table against the independent f64 reference",
            "Field-patch conformance: every field-patch module the registry holds (Basic, Presence, the colour mixer and the vignette, with the developer controls proof held to the payload rules over the non-numeric field kinds) passes one suite under field_patch_conformance, driven through the JSON method table and both evaluation paths, here in release",
            "Presence, mixer and vignette: each module's own placement under presence_mixer_vignette, driven through the JSON method table — Presence after the colour run and before the geometry tail in every touch order, the mixer after Basic in both touch orders with identical bytes, and the vignette last and recentred on the stage each crop update produces",
            "Masks: a catalog holding every component kind and mode, a brush's strokes and a masked layer each of Basic, Presence and the colour mixer, reopened through a fresh owner under masks, returning the masks, components and bound layers by identity and the same sampled pixels",
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
        "# Editor acceptance\n\nRun from the repository root with:\n\n```sh\ncargo xtask editor-acceptance --output NEW_DIRECTORY\n```\n\n`result.json` records exact state, hashes, timings and the tested platform. Native UI capture and platform classification are recorded in the engineering results document.\n",
    )?;
    checked?;
    println!("PASS editor acceptance: {}", out.display());
    Ok(())
}
