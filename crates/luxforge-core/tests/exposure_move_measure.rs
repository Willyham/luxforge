//! Measurement, not a regression check: how far a RAW development's own exposure
//! (`set-raw-exposure`, `LinearSettings::exposure_ev`, multiplied in f64 into the source before
//! any pixel-stage layer) differs from Basic's colour-stage exposure (`set-basic.exposure`, an f32
//! `2^EV` gain) applied to the same photo at the same nominal EV. This is the evidence for
//! [decision 1](../../../docs/design/source-controls.md#decisions): the RAW development is about
//! to stop carrying its own exposure, so every RAW photo's Exposure becomes Basic's. Run before
//! that change lands, with today's code, on the three supplied RAW files. See
//! `docs/specs/performance.md`, "Exposure on RAW: source development against Basic", for the
//! recorded numbers and the command that reproduces them.
//!
//! Deleted or adapted once the RAW development's `exposure_ev` is removed: there is no "before"
//! left to compare against then.
//!
//! Run with `LUXFORGE_RAW_FIXTURE` pointing to one private qualified NEF, RAF or DNG, once per
//! file:
//!
//! ```text
//! LUXFORGE_RAW_FIXTURE=/path/to/file.NEF \
//!   cargo test --release -p luxforge-core --test exposure_move_measure -- --ignored --nocapture
//! ```

use luxforge_core::{Cancel, Draft, EditorService, ProxyBounds, Raster, RenderOptions, render};
use luxforge_testkit::fixtures::temp_catalog;
use serde_json::{Map, json};
use std::path::PathBuf;

/// The EV values the exposure-move decision was measured at.
const EVS: [f64; 5] = [-2.37, -0.5, 0.01, 1.0, 3.3];

/// A stand-in for a display-bounded proxy: large enough to matter, well under
/// [`ProxyBounds::MAX_PIXELS`], and never the source's own size for any of the three supplied
/// files.
const DISPLAY_BOUNDS: ProxyBounds = ProxyBounds {
    width: 2048,
    height: 2048,
};

/// The largest per-channel code difference over R, G and B (alpha is opaque on both sides and
/// carries no exposure), and the share of those channel samples more than one code apart.
fn difference(a: &Raster, b: &Raster) -> (u8, f64) {
    assert_eq!(
        (a.width, a.height),
        (b.width, b.height),
        "same output stage"
    );
    let mut max_diff: u8 = 0;
    let mut beyond = 0usize;
    let mut total = 0usize;
    for (pa, pb) in a.rgba.chunks_exact(4).zip(b.rgba.chunks_exact(4)) {
        for channel in 0..3 {
            let diff = pa[channel].abs_diff(pb[channel]);
            max_diff = max_diff.max(diff);
            if diff > 1 {
                beyond += 1;
            }
            total += 1;
        }
    }
    (max_diff, beyond as f64 / total as f64)
}

/// `recipe` drafted as one action's fields over the freshly imported development, rendered at
/// full size and at [`DISPLAY_BOUNDS`], through the same render entry point and the same proxy
/// path the preview worker uses.
fn render_both(
    service: &EditorService,
    asset: &luxforge_core::AssetId,
    base_revision: u64,
    action: &str,
    field: &str,
    ev: f64,
) -> (Raster, Raster) {
    let mut draft = Draft::new(action, asset.clone(), base_revision);
    draft.merge(Map::from_iter([(field.to_owned(), json!(ev))]));
    let job = service
        .preview_job(asset, None, None, Some(&draft), Some(DISPLAY_BOUNDS))
        .expect("the drafted development previews");
    assert!(
        !job.source.approximate_white_balance(),
        "an exposure draft never approximates white balance"
    );
    let cancel = Cancel::never();
    let exact = render(
        &job.registry,
        job.source.input(),
        &job.recipe,
        RenderOptions::exact(&cancel),
        &job.context,
    )
    .expect("the exact phase compiles and renders");
    let full = exact
        .frame(job.entry.snapshot.id.clone())
        .expect("the full-size frame renders");

    job.registry
        .proxy_eligible(&job.recipe)
        .expect("an exposure-only stack is proxy eligible");
    let plan = exact
        .proxy_plan(DISPLAY_BOUNDS)
        .expect("the source is larger than the display bounds");
    let plan = exact.proxy_window(&job.registry, &job.recipe, plan);
    let proxy_source = job.source.proxy(plan).expect("the proxy source downscales");
    let proxy_render = exact
        .render_proxy(
            &job.registry,
            proxy_source.input(),
            &job.recipe,
            plan,
            &cancel,
            &job.context,
        )
        .expect("the proxy phase compiles and renders");
    let proxy = proxy_render
        .frame(job.entry.snapshot.id.clone())
        .expect("the proxy frame renders");
    (proxy, full)
}

/// (A) a RAW development at `ev` with no Basic layer, against (B) a development at 0 EV plus a
/// Basic layer `{exposure: ev}`, at proxy and full size, for every EV the decision was measured
/// at. Prints one line per EV and size; the doc table is built from this output, run once per
/// supplied file.
#[test]
#[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
fn exposure_move_source_development_against_basic() {
    let path = PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("LUXFORGE_RAW_FIXTURE"));
    let catalog = temp_catalog("exposure-move-measure");
    let mut service = EditorService::open(&catalog).unwrap();
    let state = service.import(&path).unwrap();
    let asset = state.asset.id.clone();

    eprintln!("fixture={} revision={}", path.display(), state.revision);
    for ev in EVS {
        let (a_proxy, a_full) = render_both(
            &service,
            &asset,
            state.revision,
            "set-raw-exposure",
            "ev",
            ev,
        );
        let (b_proxy, b_full) = render_both(
            &service,
            &asset,
            state.revision,
            "set-basic",
            "exposure",
            ev,
        );

        let (proxy_max, proxy_far) = difference(&a_proxy, &b_proxy);
        let (full_max, full_far) = difference(&a_full, &b_full);

        eprintln!(
            "ev={ev:+.2} proxy[{}x{}] max_diff={proxy_max} beyond_1={proxy_far:.6} \
             full[{}x{}] max_diff={full_max} beyond_1={full_far:.6}",
            a_proxy.width, a_proxy.height, a_full.width, a_full.height,
        );
        if ev.fract() == 0.0 && (proxy_max != 0 || full_max != 0) {
            eprintln!(
                "  NOTE: integer EV {ev} was not byte-identical (proxy max {proxy_max}, full max {full_max})"
            );
        }
    }
}
