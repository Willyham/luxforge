//! The core's ignored release timings. The photo-sized cancellation case: how promptly a
//! superseded full-resolution render stops, and what it leaves behind. The stop is a timing, so
//! both cases are ignored and assert their 25 ms bound only where it means something, in release on
//! the owner's Mac:
//!
//! ```sh
//! cargo test --release --locked -p luxforge-core --test cancellation -- --ignored --nocapture cancelled
//! ```
//!
//! What a cancel leaves behind at any size (the cancelled kind, no frame, every reservation
//! released) is proven without timing by the core's `render::cancellation_tests`.
//!
//! Each test renders through a context of its own, so the scratch budget it reads holds its own
//! render's reservations and nothing else's. The owner-side cost of `resources.read` is the other
//! timing here (`resources_cost`); each runs by its own filter, so neither shares its process with
//! the other.

mod resources_cost;

use luxforge_core::{
    BASIC_EFFECT, BoxRect, Cancel, CropStage, EFFECT_FORMAT, Error, ErrorKind, Layer, LayerId,
    ModuleRegistry, Orientation, RECIPE_FORMAT, Recipe, RenderContext, RenderOptions, SnapshotId,
    SourceImage, Transform,
};
use serde_json::json;
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

/// A programmatically filled source, so a 24 MP case costs an allocation and a fill and reads no
/// file.
fn source(width: u32, height: u32) -> SourceImage {
    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
        pixel.copy_from_slice([index as u8, (index >> 5) as u8, (index >> 11) as u8, 255].as_ref());
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:cancellation".into(),
        orientation: 1,
        capture: Default::default(),
    }
}

/// The same stack the unit tests render: the one orientation layer, one colour-stage Basic layer
/// and a 7 degree straightening crop, so the exact transform pass, the streamed colour pass and the
/// resample all run over the frame. The quarter turn means the crop is fitted onto the turned
/// stage, which is the stage the crop layer receives.
fn stack(width: u32, height: u32) -> Recipe {
    let stage = CropStage {
        width: height,
        height: width,
        angle: 7.0,
    };
    let (box_width, box_height) = stage.bounding_box();
    let fitted = stage.fit_about_center(BoxRect {
        x: 0.05 * box_width,
        y: 0.05 * box_height,
        width: 0.9 * box_width,
        height: 0.9 * box_height,
    });
    Recipe {
        format: RECIPE_FORMAT,
        layers: vec![
            Layer::orientation(Orientation::of(Transform::RotateRight)),
            Layer {
                id: LayerId::new(),
                effect_id: BASIC_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"exposure": 0.5, "contrast": 20.0, "vibrance": 30.0}),
                mask: None,
                artifacts: Vec::new(),
            },
            Layer::crop(fitted.normalized(&stage)),
        ],
        masks: Vec::new(),
        ..Recipe::default()
    }
}

/// Start a 24 MP render of the stack on its own thread, through `context`: the parallel path in
/// every pass, and far more work than either bound below allows.
fn start_render(
    cancel: &Cancel,
    context: &RenderContext,
) -> thread::JoinHandle<Result<luxforge_core::Raster, Error>> {
    let source = Arc::new(source(6000, 4000));
    let recipe = Arc::new(stack(6000, 4000));
    let cancel = cancel.clone();
    let context = context.clone();
    thread::spawn(move || {
        let registry = ModuleRegistry::builtin();
        luxforge_core::render(
            &registry,
            &*source,
            &recipe,
            RenderOptions::exact(&cancel),
            &context,
        )?
        .frame(SnapshotId::new())
    })
}

/// A timing means something only with optimizations on; a dev-profile run is refused rather than
/// failed on its bound.
fn release_only() {
    if cfg!(debug_assertions) {
        panic!(
            "a timing: run it in release (`cargo test --release --locked -p luxforge-core --test \
             cancellation -- --ignored --nocapture`)"
        );
    }
}

/// What a cancelled render must always be: the cancelled kind, and no frame at all, stopped within
/// the bound.
fn cancelled(outcome: Result<luxforge_core::Raster, Error>, elapsed: Duration, what: &str) {
    let error = outcome.expect_err("a cancelled render yields no frame, partial or otherwise");
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(error.kind.code(), "cancelled");
    println!("measured cancellation latency on 6000x4000, {what}: {elapsed:?} (bound 25 ms)");
    // The design target is about a millisecond; the bound is 25 ms so a loaded host does not turn
    // a prompt stop into a failure.
    assert!(
        elapsed < Duration::from_millis(25),
        "{what}: the render stopped {elapsed:?} after the cancel, over the 25 ms bound"
    );
}

#[test]
#[ignore = "timing: run in release; asserts the 25 ms stop bound"]
fn a_cancelled_render_returns_promptly_and_yields_no_frame() {
    release_only();
    let context = RenderContext::new();
    let cancel = Cancel::new();
    let worker = start_render(&cancel, &context);
    // Five milliseconds into a 24 MP render the exact transform pass is still copying the frame,
    // so this is the rasterizing pass's own latency. Where the cancel lands is only what the
    // printed figure describes: the outcome asserted below holds wherever it lands.
    let started = Instant::now();
    luxforge_testbase::wait_until("five milliseconds of the render", || {
        started.elapsed() >= Duration::from_millis(5)
    });
    let asked = Instant::now();
    cancel.cancel();
    let outcome = worker.join().expect("the render thread does not panic");
    cancelled(
        outcome,
        asked.elapsed(),
        "5 ms in, during the transform pass",
    );
    assert_eq!(
        context.scratch().in_use(),
        0,
        "a cancelled render leaves no reservation behind"
    );
}

#[test]
#[ignore = "timing: run in release; asserts the 25 ms stop bound"]
fn a_colour_pass_cancelled_mid_chunk_releases_every_reservation() {
    release_only();
    let context = RenderContext::new();
    let budget = context.scratch();
    let cancel = Cancel::new();
    let worker = start_render(&cancel, &context);
    // A non-zero counter means a colour chunk is holding its scratch right now, which is the only
    // thing that reserves. Waiting for it is what makes this the colour pass's test and not the
    // transform pass's: a fixed delay lands in the transform pass on this host.
    luxforge_testbase::wait_until("the colour pass to reserve scratch", || {
        budget.in_use() != 0
    });
    let asked = Instant::now();
    cancel.cancel();
    let outcome = worker.join().expect("the render thread does not panic");
    cancelled(outcome, asked.elapsed(), "during the streamed colour pass");
    assert_eq!(
        budget.in_use(),
        0,
        "a cancelled colour pass releases every reservation"
    );
    assert!(
        budget.peak() > 0,
        "the counter this test waited on was a real reservation"
    );
}
