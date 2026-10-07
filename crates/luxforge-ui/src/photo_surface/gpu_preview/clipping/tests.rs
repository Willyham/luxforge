//! The clipping marks, on a headless device the test creates and read back through the
//! photograph's real draw, against the independent reference's composite of the CPU overlay.
//!
//! A headless test with no adapter prints that it was skipped and asserts nothing. It is not GPU
//! evidence: the skip is the report.
use super::super::super::{Frame, Layer, PhotoPrimitive};
use super::super::tests::{ID, SIDE, diagnostics, headless, identity, own_pipeline, paint, plan};
use super::super::{End, GpuBoundary, GpuFallback, GpuStep, assemble_passes, output_encoding};
use super::*;
use iced::{Size, Vector};
use luxforge_reference::srgb;
use std::sync::Arc;

/// The CPU overlay's palette: its theme colours at its 0.72 opacity, as the desktop paints them.
const PALETTE: [[u8; 4]; 3] = [
    [0x4c, 0x8b, 0xe0, 184],
    [0xe5, 0x53, 0x4b, 184],
    [0xe5, 0x53, 0xe0, 184],
];

fn marks(shadows: bool, highlights: bool) -> ClipMarks {
    luxforge_gpu::qualification::install_reference_encoding();
    let encoding = output_encoding().expect("the test's output encoding");
    ClipMarks {
        shadows,
        highlights,
        shadow_below: encoding.thresholds[0],
        highlight_from: encoding.thresholds[254],
        palette: PALETTE,
    }
}

/// Each band of 16 rows: grey, a channel at code 0, one at code 255, and both.
fn band(y: u32) -> [f32; 3] {
    match y / 16 {
        0 => [0.5, 0.25, 0.75],
        1 => [0.0, 0.5, 0.25],
        2 => [0.5, 1.5, 0.25],
        _ => [-0.5, 0.25, 4.0],
    }
}

/// The band's texels, with one NaN red in the grey band's first column: the quantizer takes it to
/// code 0.
fn boundary() -> GpuBoundary {
    GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        SIDE,
        SIDE,
        3,
        (0..SIDE * SIDE).map(|index| {
            let (x, y) = (index % SIDE, index / SIDE);
            let [r, g, b] = band(y);
            let r = if x == 0 && y < 16 { f32::NAN } else { r };
            [r, g, b, 1.0]
        }),
    )
    .expect("a boundary")
}

/// What the CPU draws: the pixel's codes, and over a marked pixel the overlay's colour at its
/// opacity, blended in linear light and encoded once.
fn composite(rgb: [f32; 3], colour: Option<[u8; 4]>) -> [u8; 3] {
    let codes = rgb.map(|value| {
        if value.is_nan() {
            0
        } else {
            srgb::code(f64::from(value))
        }
    });
    match colour {
        None => codes,
        Some(colour) => {
            let alpha = f64::from(colour[3]) / 255.0;
            std::array::from_fn(|channel| {
                let photo = srgb::decode(codes[channel]);
                let mark = srgb::decode(colour[channel]);
                srgb::code(photo + (mark - photo) * alpha)
            })
        }
    }
}

/// The colour a pixel of `rgb` is marked with under `marks`, as the CPU overlay's palette and
/// classes give it.
fn class(rgb: [f32; 3], marks: &ClipMarks) -> Option<[u8; 4]> {
    // Below the threshold, or NaN, which compares with nothing.
    let low = rgb
        .iter()
        .any(|value| value.is_nan() || *value < marks.shadow_below)
        && marks.shadows;
    let high = rgb.iter().any(|value| *value >= marks.highlight_from) && marks.highlights;
    match (low, high) {
        (true, true) => Some(PALETTE[2]),
        (true, false) => Some(PALETTE[0]),
        (false, true) => Some(PALETTE[1]),
        (false, false) => None,
    }
}

fn primitive(steps: Vec<GpuStep>) -> PhotoPrimitive {
    let pixels: Vec<u8> = std::iter::repeat_n([0u8, 0, 0, 255], (SIDE * SIDE) as usize)
        .flatten()
        .collect();
    PhotoPrimitive {
        surface: ID,
        layers: vec![(
            Layer::Photo,
            Frame::new(Arc::new(pixels), SIDE, SIDE, 1).expect("a frame"),
        )],
        viewport: None,
        region_overlays: [None, None],
        gpu: Some(super::super::GpuPlan {
            steps,
            ..plan(&boundary(), Vec::new())
        }),
        gpu_options: Default::default(),
        dissolve: None,
        source: None,
        rest: None,
        offset: Vector::new(0.0, 0.0),
        size: Size::new(SIDE as f32, SIDE as f32),
        bright: None,
        angle: 0.0,
        snap: true,
    }
}

/// The acceptance check: the GPU output marks each clipped pixel as the CPU overlay would mark it
/// — its class by the quantizer's thresholds, a NaN counting as code 0, both classes in the both
/// colour, and a class turned off unmarked — composited in linear light at the overlay's opacity,
/// within one code of the reference; and the draw records the marks it drew.
#[test]
fn the_marks_composite_each_clipped_pixel_as_the_cpu_overlay_does() {
    let test = "the_marks_composite_each_clipped_pixel_as_the_cpu_overlay_does";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    for (shadows, highlights) in [(true, true), (true, false), (false, true)] {
        let marks = marks(shadows, highlights);
        let drawn = paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(vec![GpuStep::colour(identity()), GpuStep::Clipping(marks)]),
        );
        for (index, pixel) in drawn.chunks_exact(4).enumerate() {
            let (x, y) = (index as u32 % SIDE, index as u32 / SIDE);
            let [r, g, b] = band(y);
            let r = if x == 0 && y < 16 { f32::NAN } else { r };
            // The boundary holds half floats: the reference reads the same.
            let held = [r, g, b].map(|value| half::f16::from_f32(value).to_f32());
            let wanted = composite(held, class(held, &marks));
            let close = |drawn: u8, wanted: u8| drawn.abs_diff(wanted) <= 1;
            assert!(
                close(pixel[2], wanted[0])
                    && close(pixel[1], wanted[1])
                    && close(pixel[0], wanted[2]),
                "{shadows} {highlights}: pixel ({x}, {y}) is BGRA {pixel:?}, wanted RGB {wanted:?}"
            );
        }
        let seen = diagnostics(&pipeline, ID);
        assert_eq!(seen.drawn_clipping_marks, Some([shadows, highlights]));
        assert_eq!(seen.gpu_fallback, None);
    }
    eprintln!("{test}: marks match the CPU overlay's composite within a code");
}

/// The marks read the output the last pass encodes: anywhere but last, a plan does not assemble,
/// and its frame is the CPU's.
#[test]
fn the_marks_are_a_plans_last_step() {
    let steps = vec![
        GpuStep::Clipping(marks(true, true)),
        GpuStep::colour(identity()),
    ];
    let error = assemble_passes(&steps, End::Codes).unwrap_err();
    assert!(error.contains("last step"), "{error}");
    assert!(
        assemble_passes(
            &[
                GpuStep::colour(identity()),
                GpuStep::Clipping(marks(true, false))
            ],
            End::Codes
        )
        .is_ok()
    );
    let test = "the_marks_are_a_plans_last_step";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let _ = paint(&device, &queue, &mut pipeline, &primitive(steps));
    assert_eq!(
        diagnostics(&pipeline, ID).gpu_fallback,
        Some(GpuFallback::PipelineFailed)
    );
}

/// The CPU frame's clipping overlay marks that frame's pixels, so over the GPU output drawn in its
/// place it is not drawn; once the CPU frame is drawn again, held behind the plan, it is.
#[test]
fn the_cpu_overlay_is_not_drawn_over_the_gpu_output() {
    let test = "the_cpu_overlay_is_not_drawn_over_the_gpu_output";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let overlay: Vec<u8> = std::iter::repeat_n(PALETTE[0], (SIDE * SIDE) as usize)
        .flatten()
        .collect();
    let overlaid = |hold: bool| {
        let mut primitive = primitive(vec![GpuStep::colour(identity())]);
        primitive.layers.push((
            Layer::Clipping,
            Frame::new(Arc::new(overlay.clone()), SIDE, SIDE, 2).expect("an overlay"),
        ));
        primitive.gpu_options.hold = hold;
        primitive
    };
    // A pixel of the grey band, clear of the NaN column.
    let at = (4 * SIDE + 4) as usize * 4;
    let drawn = paint(&device, &queue, &mut pipeline, &overlaid(false));
    let grey = composite(
        band(4).map(|value| half::f16::from_f32(value).to_f32()),
        None,
    );
    assert_eq!(
        [drawn[at + 2], drawn[at + 1], drawn[at]],
        grey,
        "the GPU output alone"
    );
    assert_eq!(diagnostics(&pipeline, ID).drawn_clipping_version, None);
    let drawn = paint(&device, &queue, &mut pipeline, &overlaid(true));
    let wanted = composite([0.0, 0.0, 0.0], Some(PALETTE[0]));
    let close = |drawn: u8, wanted: u8| drawn.abs_diff(wanted) <= 1;
    assert!(
        close(drawn[at + 2], wanted[0])
            && close(drawn[at + 1], wanted[1])
            && close(drawn[at], wanted[2]),
        "the CPU frame under its overlay: BGRA {:?}, wanted RGB {wanted:?}",
        &drawn[at..at + 4]
    );
    assert_eq!(diagnostics(&pipeline, ID).drawn_clipping_version, Some(2));
}
