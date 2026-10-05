//! The picture at rest drawn in tiles (`docs/design/gpu-preview.md`, "The picture at rest"), on a
//! headless device the test creates, read back through the photograph's real draw: the tiles drawn
//! a frame at a time, each over its own window of the source, reduced into the view's size as the
//! reference's area average reduces the frame they make, and drawn in place of the photograph once
//! the last is in.
use super::*;
use crate::photo_surface::{GpuRegion, GpuRest, GpuSource, RestFigures};

/// Codes that vary along both axes, `width` × `height` pixels of four bytes each.
fn codes(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend([
                ((x * 5 + y * 3) % 256) as u8,
                ((x * 2 + y * 9 + 7) % 256) as u8,
                ((x ^ (2 * y)) % 256) as u8,
                255,
            ]);
        }
    }
    rgba
}

/// The area average of an axis of `from` pixels into `to`, as the reference reduces it: for each
/// output index the first source index, its weights and the weights in `f64`.
fn coverage(from: u32, to: u32) -> (Vec<u32>, Vec<u32>, Vec<f64>) {
    let (mut first, mut offsets, mut weights) = (Vec::new(), vec![0], Vec::new());
    let scale = f64::from(from) / f64::from(to);
    for index in 0..to {
        let (start, end) = (f64::from(index) * scale, f64::from(index + 1) * scale);
        let lowest = start.floor() as u32;
        first.push(lowest);
        let mut at = lowest;
        while f64::from(at) < end && at < from {
            let covered = end.min(f64::from(at + 1)) - start.max(f64::from(at));
            weights.push(covered / scale);
            at += 1;
        }
        offsets.push(weights.len() as u32);
    }
    (first, offsets, weights)
}

fn axis(
    (first, offsets, weights): &(Vec<u32>, Vec<u32>, Vec<f64>),
) -> crate::photo_surface::AxisCoverage {
    crate::photo_surface::AxisCoverage {
        first: first.clone(),
        offsets: offsets.clone(),
        weights: weights.iter().map(|weight| *weight as f32).collect(),
    }
}

/// A picture at rest over a 128 × 96 output stage in tiles of 48, reduced to the 64 × 64 view the
/// surface draws: drawn a tile a frame, the CPU frame drawn until the last is in, then the rest
/// output in place of the photograph, each view pixel the reference's area average of the tiles'
/// codes within a code, and the same bytes when drawn again. Another version starts over.
#[test]
fn a_picture_at_rest_is_drawn_in_tiles_and_reduced_to_the_view() {
    let test = "a_picture_at_rest_is_drawn_in_tiles_and_reduced_to_the_view";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    if let Err(error) = super::super::rest::RestPasses::new(&device) {
        panic!("the rest passes: {error}");
    }
    let (width, height) = (128u32, 96u32);
    let rgba = codes(width, height);
    let source = GpuSource::codes(3, Arc::new(rgba.clone()), width, height).expect("a source");
    let side = 48u32;
    let mut tiles = Vec::new();
    for y0 in (0..height).step_by(side as usize) {
        for x0 in (0..width).step_by(side as usize) {
            let (x1, y1) = ((x0 + side).min(width), (y0 + side).min(height));
            let boundary = GpuBoundary::derived(
                &source,
                Derivation::Cut { origin: (x0, y0) },
                x1 - x0,
                y1 - y0,
                10 + tiles.len() as u64,
            )
            .expect("a tile's boundary");
            tiles.push(GpuPlan {
                boundary,
                texels: TexelMap {
                    origin: [x0 as f32, y0 as f32],
                    step: [1.0, 1.0],
                },
                steps: vec![GpuStep::colour(identity())],
                region: Some(GpuRegion {
                    rect: [x0, y0, x1, y1],
                    stage: (width, height),
                }),
            });
        }
    }
    assert_eq!(tiles.len(), 6);
    let (across, down) = (coverage(width, SIDE), coverage(height, SIDE));
    let rest = GpuRest {
        version: 1,
        tiles: tiles.clone().into(),
        view: (SIDE, SIDE),
        across: axis(&across),
        down: axis(&down),
    };
    pipeline.compile_now(&device, &tiles[0]);
    let primitive = PhotoPrimitive {
        source: Some(source.clone()),
        rest: Some(rest.clone()),
        ..primitive(ID, None)
    };
    // The tiles' codes: each pixel's code decoded and held as the nearest half float, encoded.
    let held: Vec<[u8; 3]> = rgba
        .chunks_exact(4)
        .map(|pixel| {
            [0, 1, 2].map(|channel| {
                srgb::code(f64::from(
                    half::f16::from_f32(srgb::decode(pixel[channel]) as f32).to_f32(),
                ))
            })
        })
        .collect();
    let mut frames = 0;
    let drawn = loop {
        let drawn = paint(&device, &queue, &mut pipeline, &primitive);
        frames += 1;
        let seen = diagnostics(&pipeline, ID);
        let figures = seen.gpu_rest.expect("the rest's figures");
        assert_eq!(figures.fallback, None, "frame {frames}: {figures:?}");
        if seen.drawn_rest == Some(1) {
            assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
            assert!(figures.done);
            break drawn;
        }
        assert_cpu_frame(&drawn);
        assert!(figures.drawn <= 6 && !figures.done, "{figures:?}");
        assert!(frames < 6, "the rest was never drawn: {figures:?}");
    };
    assert_eq!(
        frames, 6,
        "one tile a frame, drawn in the frame that draws the last"
    );
    let mut exact = 0;
    for y in 0..SIDE as usize {
        for x in 0..SIDE as usize {
            let reference: [u8; 3] = [0, 1, 2].map(|channel| {
                let mut sum = 0.0;
                for j in down.1[y]..down.1[y + 1] {
                    let sy = (down.0[y] + j - down.1[y]) as usize;
                    for i in across.1[x]..across.1[x + 1] {
                        let sx = (across.0[x] + i - across.1[x]) as usize;
                        sum += down.2[j as usize]
                            * across.2[i as usize]
                            * srgb::decode(held[sy * width as usize + sx][channel]);
                    }
                }
                srgb::code(sum)
            });
            let at = (y * SIDE as usize + x) * 4;
            let gpu = [drawn[at + 2], drawn[at + 1], drawn[at]];
            for channel in 0..3 {
                assert!(
                    gpu[channel].abs_diff(reference[channel]) <= 1,
                    "({x}, {y}) channel {channel}: {} against {}",
                    gpu[channel],
                    reference[channel]
                );
            }
            exact += usize::from(gpu == reference);
        }
    }
    eprintln!(
        "{test}: {exact} of {} view pixels equal to the f64 reference's codes",
        SIDE * SIDE
    );
    // Drawn again, the same bytes, and no tile drawn again.
    let again = paint(&device, &queue, &mut pipeline, &primitive);
    assert_eq!(again, drawn);
    // Another version starts over from its first tile, the CPU frame drawn meanwhile.
    let primitive = PhotoPrimitive {
        rest: Some(GpuRest { version: 2, ..rest }),
        ..primitive
    };
    assert_cpu_frame(&paint(&device, &queue, &mut pipeline, &primitive));
    assert_eq!(
        diagnostics(&pipeline, ID).gpu_rest,
        Some(RestFigures {
            version: 2,
            tiles: 6,
            drawn: 1,
            done: false,
            waiting: false,
            fallback: None,
        })
    );
    // None lets it go.
    paint(&device, &queue, &mut pipeline, &handing_none());
    assert_eq!(diagnostics(&pipeline, ID).gpu_rest, None);
    settle(&pipeline);
}

/// Surface [`ID`]'s photograph with no plan and no picture at rest.
fn handing_none() -> PhotoPrimitive {
    primitive(ID, None)
}
