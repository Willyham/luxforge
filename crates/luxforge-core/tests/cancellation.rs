//! The photo-sized cancellation case: how promptly a superseded full-resolution render stops, and
//! what it leaves behind.
//!
//! This is its own test binary because the scratch budget is process-wide. In the lib test binary
//! 250 other tests run beside this one and several hold colour-pass reservations while it would be
//! reading the counter, so `in_use` there describes the whole process rather than this render. Here
//! nothing else reserves, and the figure means what it says.

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

/// The two tests below both read the process-wide scratch counter, so they take turns rather than
/// observing each other's reservations.
static BUDGET: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn budget_guard() -> std::sync::MutexGuard<'static, ()> {
    BUDGET
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Start a 24 MP render of the stack on its own thread: the parallel path in every pass, and far
/// more work than either bound below allows.
fn start_render(cancel: &Cancel) -> thread::JoinHandle<Result<luxforge_core::Raster, Error>> {
    let source = Arc::new(source(6000, 4000));
    let recipe = Arc::new(stack(6000, 4000));
    let cancel = cancel.clone();
    thread::spawn(move || {
        let registry = ModuleRegistry::builtin();
        luxforge_core::render(
            &registry,
            &*source,
            &recipe,
            RenderOptions::exact(&cancel),
            context(),
        )?
        .frame(SnapshotId::new())
    })
}

/// What a cancelled render must always be: the cancelled kind, and no frame at all.
fn cancelled(outcome: Result<luxforge_core::Raster, Error>, elapsed: Duration, what: &str) {
    let error = outcome.expect_err("a cancelled render yields no frame, partial or otherwise");
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(error.kind.code(), "cancelled");
    println!(
        "measured cancellation latency on 6000x4000, {what}: {elapsed:?} (bound 25 ms, {} profile)",
        if cfg!(debug_assertions) {
            "debug, not asserted"
        } else {
            "release"
        }
    );
    // The design target is about a millisecond; the bound is 25 ms so a loaded host does not turn
    // a prompt stop into a failure. Timing is only asserted with optimizations on, where the figure
    // means something.
    if !cfg!(debug_assertions) {
        assert!(
            elapsed < Duration::from_millis(25),
            "{what}: the render stopped {elapsed:?} after the cancel, over the 25 ms bound"
        );
    }
}

#[test]
fn a_cancelled_render_returns_promptly_and_yields_no_frame() {
    let _guard = budget_guard();
    let cancel = Cancel::new();
    let worker = start_render(&cancel);
    // Five milliseconds into a 24 MP render the exact transform pass is still copying the frame,
    // so this is the rasterizing pass's own latency.
    thread::sleep(Duration::from_millis(5));
    let asked = Instant::now();
    cancel.cancel();
    let outcome = worker.join().expect("the render thread does not panic");
    cancelled(
        outcome,
        asked.elapsed(),
        "5 ms in, during the transform pass",
    );
    assert_eq!(
        context().scratch().in_use(),
        0,
        "a cancelled render leaves no reservation behind"
    );
}

#[test]
fn a_colour_pass_cancelled_mid_chunk_releases_every_reservation() {
    let _guard = budget_guard();
    let budget = context().scratch();
    assert_eq!(budget.in_use(), 0, "nothing else in this process reserves");
    let cancel = Cancel::new();
    let worker = start_render(&cancel);
    // A non-zero counter means a colour chunk is holding its scratch right now, which is the only
    // thing that reserves. Waiting for it is what makes this the colour pass's test and not the
    // transform pass's: a fixed delay lands in the transform pass on this host.
    let deadline = Instant::now() + Duration::from_secs(10);
    while budget.in_use() == 0 {
        assert!(
            Instant::now() < deadline,
            "the colour pass never reserved scratch"
        );
        thread::sleep(Duration::from_micros(50));
    }
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

/// The one context this binary's renders share, so a test reads the scratch budget its render
/// reserved from.
fn context() -> &'static RenderContext {
    static CONTEXT: std::sync::OnceLock<RenderContext> = std::sync::OnceLock::new();
    CONTEXT.get_or_init(RenderContext::new)
}
