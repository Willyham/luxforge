//! The banded reference's export outputs against the whole-frame reference's: full size at 8 and
//! 16 bits, resized at both depths and sharpened, banded code for code as the whole frame's
//! (`tiles::output::render_output`) on stacks of every family, at the default budget and at one
//! that cuts the output into many bands; the resample's rolling rows held to their bound; and a
//! cancellation.
use super::{OutputBands, Resampled, output_bands};
use crate::{
    Cancel, LinearImage, ModuleRegistry, Recipe, SnapshotId,
    render::{
        LinearSettings, Render, RenderContext, RenderOptions,
        bands::{BAND_WINDOW_BYTES, Banded, WholeSource},
        bands_tests::{Step, cutting_budgets, stack, textured},
        export_output::{LANCZOS_LOBES, RESAMPLE_BAND_ROWS},
        testing::linear,
    },
    tiles::{
        Depth, OutputSharpen, OutputSpec, PixelRows, Sharpen, SharpenAmount, SharpenMedium,
        output::render_output,
    },
};
use luxforge_test_macros::slow;

const WIDTH: u32 = 150;
const HEIGHT: u32 = 100;

fn values(rows: PixelRows<'_>) -> Vec<u32> {
    match rows {
        PixelRows::Eight(codes) => codes.iter().map(|&code| u32::from(code)).collect(),
        PixelRows::Sixteen(codes) => codes.iter().map(|&code| u32::from(code)).collect(),
    }
}

/// The whole-frame reference's `output` of `recipe` over `image`: what the export renders when its
/// source is held whole.
fn whole(
    registry: &ModuleRegistry,
    image: &LinearImage,
    recipe: &Recipe,
    output: OutputSpec,
) -> Vec<u32> {
    let compiled = registry
        .compile(image.width(), image.height(), recipe)
        .unwrap();
    let (context, cancel) = (RenderContext::new(), Cancel::never());
    let render = Render::compiled(
        linear(image, LinearSettings::default()),
        compiled,
        RenderOptions::exact(&cancel),
        &context,
    )
    .unwrap();
    let frame = render_output(render, output, SnapshotId::new(), &cancel, &mut || Ok(())).unwrap();
    assert_eq!((frame.width, frame.height), (output.width, output.height));
    values(frame.rows())
}

/// Every band of `bands` joined, each checked to hold whole rows of a `width`-pixel output at
/// `depth`, with how many there were.
fn joined(bands: OutputBands<'_>, width: u32, depth: Depth) -> (Vec<u32>, usize) {
    let (mut out, mut count) = (Vec::new(), 0);
    for band in bands {
        let (rows, pixels) = band.unwrap();
        assert_eq!(pixels.depth(), depth);
        let rows_of = pixels.rows();
        assert_eq!(rows_of.len(), (rows * width * 4) as usize);
        out.extend(values(rows_of));
        count += 1;
    }
    (out, count)
}

/// The banded reference's `output` of `recipe` over `image`, its windows held to `budget`, with
/// how many bands it came in.
fn banded(
    registry: &ModuleRegistry,
    image: &LinearImage,
    recipe: &Recipe,
    output: OutputSpec,
    budget: u64,
) -> (Vec<u32>, usize) {
    let compiled = registry
        .compile(image.width(), image.height(), recipe)
        .unwrap();
    let context = RenderContext::new();
    let source = WholeSource {
        image,
        settings: LinearSettings::default(),
    };
    let cancel = Cancel::never();
    let banded = Banded::with_budget(&source, &compiled, &context, &cancel, budget).unwrap();
    let bands = output_bands(&banded, output, &cancel).unwrap();
    joined(bands, output.width, output.depth)
}

fn sharpened(medium: SharpenMedium, amount: SharpenAmount, density: u16) -> Option<OutputSharpen> {
    Some(OutputSharpen::new(
        Sharpen { medium, amount },
        Some(density),
    ))
}

/// The outputs every stack is held to on a `width` × `height` stage: full size at 16 bits; resized
/// at 8 and 16 bits, by a mild and a large reduction and by one side only; and sharpened at full
/// size at both depths and resized, at the narrowest and the widest radius.
fn outputs(width: u32, height: u32) -> Vec<OutputSpec> {
    let spec = |width: u32, height: u32, depth, sharpen| OutputSpec {
        width: width.max(1),
        height: height.max(1),
        depth,
        sharpen,
    };
    vec![
        spec(width, height, Depth::Sixteen, None),
        spec(width * 2 / 3, height * 2 / 3, Depth::Eight, None),
        spec(width / 7, height / 7, Depth::Sixteen, None),
        spec(width, height * 3 / 5, Depth::Eight, None),
        spec(
            width,
            height,
            Depth::Eight,
            sharpened(SharpenMedium::Matte, SharpenAmount::High, 300),
        ),
        spec(
            width,
            height,
            Depth::Sixteen,
            sharpened(SharpenMedium::Screen, SharpenAmount::Low, 72),
        ),
        // Matte at 720 ppi is held to the widest kernel, nine taps a side.
        spec(
            width * 3 / 4,
            height * 3 / 4,
            Depth::Eight,
            sharpened(SharpenMedium::Matte, SharpenAmount::High, 720),
        ),
        spec(
            width / 2,
            height / 2,
            Depth::Sixteen,
            sharpened(SharpenMedium::Glossy, SharpenAmount::Standard, 300),
        ),
    ]
}

/// Every output of every stack, banded in one band and in many, is the whole frame's code for code.
fn holds(registry: &ModuleRegistry, stacks: &[&[Step]]) {
    let image = textured(WIDTH, HEIGHT);
    for steps in stacks {
        let recipe = stack(registry, WIDTH, HEIGHT, steps);
        let stage = registry.compile(WIDTH, HEIGHT, &recipe).unwrap().stage();
        let [rows, _] = cutting_budgets(registry, &image, &recipe);
        for output in outputs(stage.width, stage.height) {
            let expected = whole(registry, &image, &recipe, output);
            let (one, _) = banded(registry, &image, &recipe, output, BAND_WINDOW_BYTES);
            assert!(one == expected, "{steps:?}: {output:?} in one band");
            let (many, count) = banded(registry, &image, &recipe, output, rows);
            // An output shorter than a group of resampled rows comes in one band whatever the
            // budget; the stage it is resampled from is still read in many.
            assert!(
                count > 1 || output.height <= RESAMPLE_BAND_ROWS,
                "{steps:?}: {output:?} in {count} bands"
            );
            assert!(many == expected, "{steps:?}: {output:?} in {count} bands");
        }
    }
}

#[test]
fn reference_bands_export_outputs_of_a_colour_stack_are_the_whole_frame_reference_s() {
    holds(&ModuleRegistry::developer(), &[&[Step::Basic, Step::Curve]]);
}

#[test]
#[slow]
fn reference_bands_export_outputs_of_spatial_and_geometry_stacks_are_the_whole_frame_reference_s() {
    holds(
        &ModuleRegistry::developer(),
        &[
            &[Step::Basic, Step::Detail, Step::Presence],
            &[Step::Turn, Step::Straighten, Step::MaskedDetail],
            &[Step::Lens, Step::Dehaze, Step::Crop, Step::Vignette],
        ],
    );
}

/// The resample holds only the stage rows its group of output rows reads: at most the taps of
/// [`RESAMPLE_BAND_ROWS`] output rows, `(16 + 6) × scale` rows and the two the taps' ends round
/// out to, at a mild reduction and a large one.
#[test]
fn reference_bands_resample_holds_a_bounded_window_of_stage_rows() {
    let registry = ModuleRegistry::developer();
    let image = textured(WIDTH, 600);
    let recipe = stack(&registry, WIDTH, 600, &[Step::Basic]);
    let compiled = registry.compile(WIDTH, 600, &recipe).unwrap();
    let context = RenderContext::new();
    let source = WholeSource {
        image: &image,
        settings: LinearSettings::default(),
    };
    let cancel = Cancel::never();
    let banded = Banded::new(&source, &compiled, &context, &cancel).unwrap();
    for height in [500, 40, 7] {
        let output = OutputSpec {
            width: WIDTH * height / 600,
            height,
            depth: Depth::Eight,
            sharpen: None,
        };
        let mut resampled = Resampled::new(&banded, output, &cancel).unwrap();
        let rows: u32 = resampled.by_ref().map(|band| band.unwrap().0).sum();
        assert_eq!(rows, height);
        let scale = 600.0 / f64::from(height);
        let bound =
            ((f64::from(RESAMPLE_BAND_ROWS) + 2.0 * LANCZOS_LOBES) * scale).ceil() as usize + 2;
        assert!(
            resampled.peak <= bound.min(600),
            "{height}: {} rows held, past {bound}",
            resampled.peak
        );
        assert!(resampled.held.is_none() && resampled.across.is_empty());
    }
}

#[test]
fn a_cancelled_banded_export_output_answers_cancelled() {
    let registry = ModuleRegistry::developer();
    let image = textured(WIDTH, HEIGHT);
    let recipe = stack(&registry, WIDTH, HEIGHT, &[Step::Basic]);
    let compiled = registry.compile(WIDTH, HEIGHT, &recipe).unwrap();
    let context = RenderContext::new();
    let source = WholeSource {
        image: &image,
        settings: LinearSettings::default(),
    };
    let banded = Banded::new(&source, &compiled, &context, &Cancel::never()).unwrap();
    let cancel = Cancel::new();
    cancel.cancel();
    for output in outputs(WIDTH, HEIGHT) {
        let mut bands = output_bands(&banded, output, &cancel).unwrap();
        let error = bands.next().unwrap().unwrap_err();
        assert_eq!(error.kind.code(), "cancelled", "{output:?}");
        assert!(bands.next().is_none(), "{output:?}");
    }
    let larger = OutputSpec {
        width: WIDTH + 1,
        ..OutputSpec::full(WIDTH, HEIGHT)
    };
    let refused = output_bands(&banded, larger, &cancel).err().unwrap();
    assert_eq!(refused.kind.code(), "validation");
}
