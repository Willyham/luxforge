//! Pipelines compile off the UI thread: a first-seen sequence draws the CPU frame and names it
//! compiling until the compile thread has finished, warming compiles ahead of the first frame,
//! holding keeps the slot behind the CPU frame, and the cache stays bounded.
use super::super::{PhotoPipeline, PhotoPrimitive, SurfaceId};
use super::tests::{
    assert_codes, assert_cpu_frame, boundary_with_codes, diagnostics, headless, own_pipeline,
    paint_prepared, primitive,
};
use super::*;
use luxforge_testbase::wait_until;

const ID: SurfaceId = SurfaceId::new(0);

/// An identity program under its own entry name, so each test's sequence is first seen.
fn named(entry: &'static str) -> GpuProgram {
    GpuProgram::new(
        entry,
        format!(
            "fn {entry}(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {{\n    \
             return rgb;\n}}\n"
        ),
    )
}

fn plan_of(boundary: &GpuBoundary, program: GpuProgram) -> GpuPlan {
    GpuPlan {
        boundary: boundary.clone(),
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::colour(program)],
    }
}

fn with_options(mut drawn: PhotoPrimitive, options: GpuOptions) -> PhotoPrimitive {
    drawn.gpu_options = options;
    drawn
}

/// Wait until the compile thread has finished every sequence `pipeline` asked of it.
fn compiled(pipeline: &mut PhotoPipeline, steps: &[GpuStep]) {
    let figures = Arc::clone(&pipeline.figures);
    wait_until("the compile thread", || {
        pipeline.gpu.pipelines.settle(&figures.preview);
        !pipeline.gpu.pipelines.compiling(steps)
    });
}

/// A first-seen sequence is handed to the compile thread: the frame draws the CPU frame and says
/// it is compiling, `prepare` compiles nothing itself, and once the thread is done the next frame
/// draws the GPU output with no second compile.
#[test]
fn a_first_seen_sequence_compiles_off_the_ui_thread() {
    let test = "a_first_seen_sequence_compiles_off_the_ui_thread";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(3);
    let gesture = primitive(ID, Some(plan_of(&boundary, named("first_seen"))));
    let steps = gesture.gpu.as_ref().unwrap().steps.clone();
    assert_cpu_frame(&paint_prepared(&device, &queue, &mut pipeline, &gesture));
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Cpu));
    assert_eq!(seen.gpu_fallback, Some(GpuFallback::Compiling));
    assert_eq!(seen.gpu_ready_boundary, None);
    assert_eq!(seen.gpu_preview_compiles, 1, "handed to the thread once");
    assert_eq!(seen.gpu_preview_in_use_bytes, 0, "nothing allocated yet");
    compiled(&mut pipeline, &steps);
    assert_codes(
        &paint_prepared(&device, &queue, &mut pipeline, &gesture),
        &codes,
    );
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert_eq!(seen.gpu_fallback, None);
    assert_eq!(seen.gpu_ready_boundary, Some(3));
    assert_eq!(
        (seen.gpu_preview_compiles, seen.gpu_preview_compiled),
        (1, 1)
    );
    assert!(seen.gpu_preview_compile_max_us > 0);
    eprintln!(
        "{test}: compiled in {} µs on the compile thread",
        seen.gpu_preview_compile_last_us
    );
}

/// Warming hands a sequence to the compile thread before any frame needs it, once per version,
/// so the first frame of the gesture draws on the GPU.
#[test]
fn a_warmed_sequence_draws_on_its_first_frame() {
    let test = "a_warmed_sequence_draws_on_its_first_frame";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(4);
    let gesture = plan_of(&boundary, named("warmed"));
    let warm = GpuWarm::new(1, vec![gesture.steps.clone()]);
    let idle = with_options(
        primitive(ID, None),
        GpuOptions {
            warm: Some(warm.clone()),
            ..GpuOptions::default()
        },
    );
    for _ in 0..3 {
        assert_cpu_frame(&paint_prepared(&device, &queue, &mut pipeline, &idle));
    }
    assert_eq!(
        diagnostics(&pipeline, ID).gpu_preview_compiles,
        1,
        "a version is warmed once"
    );
    compiled(&mut pipeline, &gesture.steps);
    assert_codes(
        &paint_prepared(
            &device,
            &queue,
            &mut pipeline,
            &primitive(ID, Some(gesture)),
        ),
        &codes,
    );
    assert_eq!(
        diagnostics(&pipeline, ID).drawn_path,
        Some(DrawingPath::Gpu)
    );
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_compiles, 1);
}

/// Holding draws the CPU frame while the slot, and its boundary, stay: the next frame without
/// the hold draws the GPU output again with no upload and no new allocation.
#[test]
fn a_held_plan_draws_the_cpu_frame_and_keeps_its_slot() {
    let test = "a_held_plan_draws_the_cpu_frame_and_keeps_its_slot";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(5);
    let gesture = plan_of(&boundary, named("held"));
    pipeline.compile_now(&device, &gesture.steps);
    let drawn = |hold: bool, tag: u64| {
        with_options(
            primitive(ID, Some(gesture.clone())),
            GpuOptions {
                hold,
                tag: Some(tag),
                warm: None,
            },
        )
    };
    assert_codes(
        &paint_prepared(&device, &queue, &mut pipeline, &drawn(false, 11)),
        &codes,
    );
    let first = diagnostics(&pipeline, ID);
    assert_eq!(first.drawn_gpu_tag, Some(11));
    assert_cpu_frame(&paint_prepared(
        &device,
        &queue,
        &mut pipeline,
        &drawn(true, 12),
    ));
    let held = diagnostics(&pipeline, ID);
    assert_eq!(held.drawn_path, Some(DrawingPath::Cpu));
    assert_eq!(held.gpu_fallback, None, "holding is no fallback");
    assert_eq!(held.drawn_gpu_tag, None);
    assert_eq!(held.gpu_ready_boundary, Some(5), "the slot still holds it");
    assert_eq!(
        held.gpu_preview_in_use_bytes,
        first.gpu_preview_in_use_bytes
    );
    assert_codes(
        &paint_prepared(&device, &queue, &mut pipeline, &drawn(false, 13)),
        &codes,
    );
    let again = diagnostics(&pipeline, ID);
    assert_eq!(again.drawn_gpu_tag, Some(13));
    assert_eq!(again.gpu_preview_peak_bytes, first.gpu_preview_peak_bytes);
}

/// The cache holds at most [`PIPELINE_CACHE`] sequences: warming more evicts the least recently
/// used ones that are not compiling.
#[test]
fn the_pipeline_cache_stays_bounded() {
    let test = "the_pipeline_cache_stays_bounded";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, _) = boundary_with_codes(6);
    let entries: Vec<&'static str> = (0..PIPELINE_CACHE + 4)
        .map(|index| &*Box::leak(format!("bounded_{index}").into_boxed_str()))
        .collect();
    let sequences: Vec<Vec<GpuStep>> = entries
        .iter()
        .map(|entry| plan_of(&boundary, named(entry)).steps)
        .collect();
    let figures = Arc::clone(&pipeline.figures);
    for (version, chunk) in sequences.chunks(4).enumerate() {
        pipeline.warm_gpu(&device, Some(&GpuWarm::new(version as u64, chunk.to_vec())));
        for steps in chunk {
            compiled(&mut pipeline, steps);
        }
        assert!(pipeline.gpu.pipelines.len() <= PIPELINE_CACHE);
    }
    pipeline.gpu.pipelines.settle(&figures.preview);
    assert_eq!(pipeline.gpu.pipelines.len(), PIPELINE_CACHE);
    assert_eq!(
        figures.preview.compiles(),
        (PIPELINE_CACHE + 4) as u64,
        "every warmed sequence compiled once"
    );
}
