//! Pipelines compile off the UI thread: a first-seen sequence draws the CPU frame and names it
//! compiling until the compile thread has finished, warming compiles ahead of the first frame,
//! holding keeps the slot behind the CPU frame, and the cache stays bounded.
use super::super::{PhotoPipeline, PhotoPrimitive, SurfaceId};
use super::tests::{
    assert_codes, assert_cpu_frame, boundary_with_codes, diagnostics, headless, own_pipeline,
    paint_prepared, primitive, settle,
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
        region: None,
        lights: Vec::new(),
    }
}

fn with_options(mut drawn: PhotoPrimitive, options: GpuOptions) -> PhotoPrimitive {
    drawn.gpu_options = options;
    drawn
}

/// `sequences` warmed over a half-float boundary.
fn half(sequences: &[Vec<GpuStep>]) -> Vec<(Vec<GpuStep>, super::BoundaryFormat)> {
    sequences
        .iter()
        .map(|steps| (steps.clone(), super::BoundaryFormat::Half))
        .collect()
}

/// Wait until the compile thread has finished `steps`, drawing no frame.
fn compiled(pipeline: &mut PhotoPipeline, steps: &[GpuStep]) {
    wait_until("the compile thread", || {
        !pipeline
            .gpu
            .pipelines
            .compiling(steps, super::OUTPUT_FORMAT)
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
    let warm = GpuWarm::new(
        1,
        vec![(gesture.steps.clone(), super::BoundaryFormat::Half)],
    );
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

/// One frame that draws a plan and hands a warm list: the compile thread takes the frame's own
/// sequence first — the picture on screen, as a picture at rest's view plan is — then the warm
/// list's open stack's part, then the rest, and the warm-up it records runs from the list until
/// the queue drains, its open part done before its end. A second list with nothing new to compile
/// is a warm-up that ends at once.
#[test]
fn the_picture_on_screen_compiles_first_then_the_open_stack_then_the_rest() {
    let test = "the_picture_on_screen_compiles_first_then_the_open_stack_then_the_rest";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(8);
    let picture = plan_of(&boundary, named("order_picture"));
    let list: Vec<Vec<GpuStep>> = ["order_open_1", "order_open_2", "order_rest"]
        .into_iter()
        .map(|entry| plan_of(&boundary, named(entry)).steps)
        .collect();
    let warm = GpuWarm::new(1, half(&list)).with_open(2);
    let drawn = with_options(
        primitive(ID, Some(picture.clone())),
        GpuOptions {
            warm: Some(warm),
            ..GpuOptions::default()
        },
    );
    assert_cpu_frame(&paint_prepared(&device, &queue, &mut pipeline, &drawn));
    assert_eq!(
        diagnostics(&pipeline, ID).gpu_fallback,
        Some(GpuFallback::Compiling),
        "the reference frame until the picture's programs compile"
    );
    let figures = Arc::clone(&pipeline.figures);
    wait_until("the warm-up to end", || {
        figures
            .preview
            .warm_up()
            .is_some_and(|warm_up| !warm_up.running())
    });
    assert_eq!(
        pipeline.gpu.pipelines.taken(),
        [
            "order_picture",
            "order_open_1",
            "order_open_2",
            "order_rest"
        ],
        "the picture, then the open stack, then the rest"
    );
    let warm_up = diagnostics(&pipeline, ID)
        .gpu_warm_up
        .expect("the warm-up's figures");
    assert_eq!(
        (warm_up.period, warm_up.version, warm_up.sequences),
        (1, 1, 3)
    );
    assert_eq!(warm_up.open_sequences, 2);
    let (open, all) = (
        warm_up.open_us.expect("the open part"),
        warm_up.us.expect("the end"),
    );
    assert!(open <= all, "{open} µs before {all} µs");
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_compile_pending, 0);
    // The picture's GPU frame replaces the reference frame at the next draw.
    assert_codes(
        &paint_prepared(&device, &queue, &mut pipeline, &drawn),
        &codes,
    );
    assert_eq!(
        diagnostics(&pipeline, ID).drawn_path,
        Some(DrawingPath::Gpu)
    );
    // A list naming only compiled sequences begins and ends a warm-up at once.
    pipeline.warm_gpu(&device, Some(&GpuWarm::new(2, half(&list))));
    let again = figures.preview.warm_up().expect("a second warm-up");
    assert_eq!((again.period, again.version, again.sequences), (2, 2, 0));
    assert!(!again.running());
    assert_eq!(again.open_us, Some(0));
    eprintln!("{test}: the open stack's part in {open} µs, the whole warm-up in {all} µs");
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
    pipeline.compile_now(&device, &gesture);
    let drawn = |hold: bool, tag: u64| {
        with_options(
            primitive(ID, Some(gesture.clone())),
            GpuOptions {
                hold,
                tag: Some(tag),
                warm: None,
                change: None,
            },
        )
    };
    assert_codes(
        &paint_prepared(&device, &queue, &mut pipeline, &drawn(false, 11)),
        &codes,
    );
    // A tick drawn on the GPU is counted for the histogram, whose readback's staging copy is
    // charged until the retirement worker takes it back: the figures are compared once it has.
    settle(&pipeline);
    let first = diagnostics(&pipeline, ID);
    assert_eq!(first.drawn_gpu_tag, Some(11));
    assert_cpu_frame(&paint_prepared(
        &device,
        &queue,
        &mut pipeline,
        &drawn(true, 12),
    ));
    settle(&pipeline);
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
        pipeline.warm_gpu(&device, Some(&GpuWarm::new(version as u64, half(chunk))));
        for steps in chunk {
            compiled(&mut pipeline, steps);
        }
        assert!(pipeline.gpu.pipelines.len() <= PIPELINE_CACHE);
    }
    assert_eq!(pipeline.gpu.pipelines.len(), PIPELINE_CACHE);
    assert_eq!(
        figures.preview.compiles(),
        (PIPELINE_CACHE + 4) as u64,
        "every warmed sequence compiled once"
    );
}

/// The compile thread drains its queue whether or not a frame is drawn: a warm list of
/// [`PIPELINE_CACHE`] sequences all compile and are counted with no frame drawn, and a second list
/// handed after it, again with no frame between, compiles too, the cache keeping the newest.
#[test]
fn the_compile_queue_drains_with_no_frame_drawn() {
    let test = "the_compile_queue_drains_with_no_frame_drawn";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, _) = boundary_with_codes(7);
    let sequences: Vec<Vec<GpuStep>> = (0..2 * PIPELINE_CACHE)
        .map(|index| {
            let entry: &'static str = Box::leak(format!("drained_{index}").into_boxed_str());
            plan_of(&boundary, named(entry)).steps
        })
        .collect();
    let figures = Arc::clone(&pipeline.figures);
    for (version, list) in sequences.chunks(PIPELINE_CACHE).enumerate() {
        let before = figures.preview.compile_us().0;
        pipeline.warm_gpu(&device, Some(&GpuWarm::new(version as u64, half(list))));
        let wanted = before + list.len() as u64;
        assert_eq!(
            figures.preview.compiles(),
            wanted,
            "every sequence is queued"
        );
        assert_eq!(
            figures.preview.compile_pending().1,
            Some(version as u64),
            "the warm list taken"
        );
        wait_until("every sequence to compile", || {
            figures.preview.compile_us().0 == wanted
        });
        assert!(list.iter().all(|steps| {
            !pipeline
                .gpu
                .pipelines
                .compiling(steps, super::OUTPUT_FORMAT)
        }));
        // Nothing left once the last compile is kept: what a scripted wait for warming reads.
        assert_eq!(figures.preview.compile_pending().0, 0);
    }
    assert_eq!(pipeline.gpu.pipelines.len(), PIPELINE_CACHE);
    for steps in &sequences[PIPELINE_CACHE..] {
        assert!(
            pipeline
                .gpu
                .pipelines
                .failure(steps, super::OUTPUT_FORMAT)
                .is_none()
        );
    }
    eprintln!(
        "{test}: {} sequences compiled with no frame drawn, the longest in {} µs",
        sequences.len(),
        figures.preview.compile_us().1
    );
}

/// A plan's programs are ready only once every one of them has compiled: not before the stage has
/// a compile thread, not for a sequence it has never seen, and after its compile; asking queues
/// nothing.
#[test]
fn a_plans_programs_are_ready_once_they_have_compiled() {
    let test = "a_plans_programs_are_ready_once_they_have_compiled";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, _) = boundary_with_codes(3);
    let plan = plan_of(&boundary, named("ready_probe"));
    assert!(
        !pipeline.figures.preview.programs_ready(&plan),
        "no compile thread yet"
    );
    let drawn = primitive(ID, Some(plan.clone()));
    assert_cpu_frame(&paint_prepared(&device, &queue, &mut pipeline, &drawn));
    compiled(&mut pipeline, &plan.steps);
    assert!(pipeline.figures.preview.programs_ready(&plan), "compiled");
    let compiles = pipeline.figures.preview.compiles();
    let unseen = plan_of(&boundary, named("ready_probe_unseen"));
    assert!(
        !pipeline.figures.preview.programs_ready(&unseen),
        "never seen"
    );
    assert_eq!(
        pipeline.figures.preview.compiles(),
        compiles,
        "asking queued nothing"
    );
}
