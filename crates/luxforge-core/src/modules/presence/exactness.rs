//! The Presence passes against the code they replaced, bit for bit.
//!
//! The functions at the top are the passes as they were before they read row slices, frozen as the
//! reference: every tap of the box passes read through the clamped, index-checked [`Plane::get`],
//! one row's running sum at a time and the vertical strip written one value at a time. The tests
//! hold the production passes to them with `to_bits` over planes of distinct values whose exponents
//! span eighty binary orders, so a sum taken in another order rounds differently, on odd and
//! degenerate frames, radii from 1 to past the frame, output rectangles on every frame edge and on
//! none, held rectangles larger than a pass needs, and both parallelisms.
use super::filters::{self, Geometry, Plane, PlaneMut, Rect};
use crate::modules::Parallelism;
use luxforge_reference::SplitMix64;

// ---------------------------------------------------------------------------------------------
// The frozen passes.
// ---------------------------------------------------------------------------------------------

const STRIP: usize = 64;

fn horizontal_mean(src: &Plane<'_>, r: i64, dst: &mut PlaneMut<'_>, parallelism: Parallelism) {
    let out = dst.rect();
    if out.is_empty() {
        return;
    }
    let n = (2 * r + 1) as f64;
    dst.for_rows(parallelism, |y, row| {
        let mut sum = 0.0_f64;
        for dx in -r..=r {
            sum += f64::from(src.get(out.x0 + dx, y));
        }
        row[0] = (sum / n) as f32;
        for x in (out.x0 + 1)..out.x1 {
            sum += f64::from(src.get(x + r, y)) - f64::from(src.get(x - 1 - r, y));
            row[(x - out.x0) as usize] = (sum / n) as f32;
        }
    });
}

fn vertical_mean(src: &Plane<'_>, r: i64, dst: &mut PlaneMut<'_>, parallelism: Parallelism) {
    let out = dst.rect();
    if out.is_empty() {
        return;
    }
    match parallelism {
        Parallelism::Serial => {
            let mut x0 = out.x0;
            while x0 < out.x1 {
                let columns = ((out.x1 - x0) as usize).min(STRIP);
                vertical_strip(src, r, out, x0, columns, |row, column, value| {
                    dst.set(x0 + column as i64, out.y0 + row as i64, value);
                });
                x0 += columns as i64;
            }
        }
        Parallelism::Pool => {
            use rayon::prelude::*;
            let width = out.width() as usize;
            let mut strips: Vec<Vec<&mut [f32]>> = (0..width.div_ceil(STRIP))
                .map(|_| Vec::with_capacity(out.height() as usize))
                .collect();
            for row in dst.values_mut()[..out.pixels()].chunks_mut(width) {
                for (strip, segment) in row.chunks_mut(STRIP).enumerate() {
                    strips[strip].push(segment);
                }
            }
            strips
                .par_iter_mut()
                .enumerate()
                .for_each(|(strip, segments)| {
                    let columns = segments[0].len();
                    let x0 = out.x0 + (strip * STRIP) as i64;
                    vertical_strip(src, r, out, x0, columns, |row, column, value| {
                        segments[row][column] = value;
                    });
                });
        }
    }
}

fn vertical_strip(
    src: &Plane<'_>,
    r: i64,
    out: Rect,
    x0: i64,
    columns: usize,
    mut write: impl FnMut(usize, usize, f32),
) {
    let n = (2 * r + 1) as f64;
    let mut accumulator = [0.0_f64; STRIP];
    for dy in -r..=r {
        for (column, slot) in accumulator.iter_mut().take(columns).enumerate() {
            *slot += f64::from(src.get(x0 + column as i64, out.y0 + dy));
        }
    }
    for (column, slot) in accumulator.iter().take(columns).enumerate() {
        write(0, column, (*slot / n) as f32);
    }
    for (row, y) in ((out.y0 + 1)..out.y1).enumerate() {
        for (column, slot) in accumulator.iter_mut().take(columns).enumerate() {
            let x = x0 + column as i64;
            *slot += f64::from(src.get(x, y + r)) - f64::from(src.get(x, y - 1 - r));
        }
        for (column, slot) in accumulator.iter().take(columns).enumerate() {
            write(row + 1, column, (*slot / n) as f32);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Planes and rectangles to compare them over.
// ---------------------------------------------------------------------------------------------

const PARALLELISMS: [Parallelism; 2] = [Parallelism::Serial, Parallelism::Pool];

/// Odd, thin and degenerate frames, and two larger ones whose rows outnumber a pass's row group
/// and whose columns outnumber a vertical strip.
const FRAMES: [(i64, i64); 10] = [
    (1, 1),
    (1, 9),
    (9, 1),
    (2, 3),
    (5, 4),
    (7, 6),
    (17, 9),
    (40, 33),
    (97, 61),
    (131, 70),
];

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

/// A value whose sums with its neighbours round in `f64`: a random 24-bit mantissa at a binary
/// exponent from -40 to 40 and either sign, so reordering or reassociating any sum changes bits,
/// with exact and negative zeros among them.
fn value(rng: &mut SplitMix64) -> f32 {
    match rng.next_u32(40) {
        0 => 0.0,
        1 => -0.0,
        _ => {
            let magnitude = rng.next_range(1.0, 2.0) * 2f64.powi(rng.next_u32(81) as i32 - 40);
            (if rng.next_bool() {
                magnitude
            } else {
                -magnitude
            }) as f32
        }
    }
}

/// `len` values of one of three kinds: distinct values from [`value`], all negative zero (a sum
/// seeded anywhere but `0.0` keeps its sign), or one value in a field of negative zeros.
fn values(rng: &mut SplitMix64, len: usize) -> Vec<f32> {
    match rng.next_u32(8) {
        0 => vec![-0.0; len],
        1 => {
            let mut values = vec![-0.0; len];
            values[rng.next_usize(len)] = value(rng);
            values
        }
        _ => (0..len).map(|_| value(rng)).collect(),
    }
}

/// Output rectangles of a frame: the whole frame, each corner, a strip down the middle touching
/// the top and bottom, one across touching the left and right, one pixel in the middle, and
/// random rectangles, some touching no edge where the frame allows it.
fn rects(rng: &mut SplitMix64, width: i64, height: i64) -> Vec<Rect> {
    let (w, h) = (width, height);
    let mut rects = vec![
        Rect::frame(w, h),
        Rect::new(0, 0, w.min(5), h.min(3)),
        Rect::new(w - w.min(6), 0, w, h.min(4)),
        Rect::new(0, h - h.min(2), w.min(3), h),
        Rect::new(w - w.min(4), h - h.min(5), w, h),
        Rect::new(w / 3, 0, w / 3 + (w / 3).max(1), h),
        Rect::new(0, h / 3, w, h / 3 + (h / 3).max(1)),
        Rect::new(w / 2, h / 2, w / 2 + 1, h / 2 + 1),
    ];
    if w > 2 && h > 2 {
        rects.push(Rect::new(1, 1, w - 1, h - 1));
    }
    for _ in 0..3 {
        let x0 = i64::from(rng.next_u32(w as u32));
        let y0 = i64::from(rng.next_u32(h as u32));
        let x1 = x0 + 1 + i64::from(rng.next_u32((w - x0) as u32));
        let y1 = y0 + 1 + i64::from(rng.next_u32((h - y0) as u32));
        rects.push(Rect::new(x0, y0, x1, y1));
    }
    rects
}

/// Box radii from 1 to past the frame: the small ones, about half the frame, and beyond it.
fn radii(width: i64, height: i64) -> Vec<i64> {
    let long = width.max(height);
    let mut radii = vec![1, 2, 3, long / 2, long / 2 + 1, long, 2 * long + 1];
    radii.retain(|r| *r >= 1);
    radii.sort_unstable();
    radii.dedup();
    radii
}

/// `before`, written over `rect` of a frame by `pass`, as bits.
fn written(
    geometry: Geometry,
    rect: Rect,
    before: &[f32],
    pass: impl FnOnce(&mut PlaneMut<'_>),
) -> Vec<u32> {
    let mut values = before.to_vec();
    pass(&mut PlaneMut::over(&mut values, geometry, rect).expect("the output plane"));
    bits(&values)
}

/// `rect` grown by `x` columns and `y` rows on each side and clipped to `frame`: the rectangle a
/// pass's source holds, at least what it reads.
fn grown(rect: Rect, x: i64, y: i64, frame: Rect) -> Rect {
    Rect::new(rect.x0 - x, rect.y0 - y, rect.x1 + x, rect.y1 + y).clip(frame)
}

// ---------------------------------------------------------------------------------------------
// The box passes.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_horizontal_mean_matches_the_tap_by_tap_reference_bit_for_bit() {
    let mut rng = SplitMix64(0x4B0E_57A1);
    let mut compared = 0;
    for (width, height) in FRAMES {
        let frame = Rect::frame(width, height);
        let geometry = Geometry::new(width, height, frame);
        for out in rects(&mut rng, width, height) {
            for r in radii(width, height) {
                let extra = [0, 0, 1, 3][rng.next_usize(4)];
                let held = grown(out, r + extra, extra, frame);
                let mut source = values(&mut rng, held.pixels());
                let source = PlaneMut::over(&mut source, geometry, held).expect("the source");
                let source = source.as_plane();
                let before = values(&mut rng, out.pixels());
                for parallelism in PARALLELISMS {
                    let expected = written(geometry, out, &before, |dst| {
                        horizontal_mean(&source, r, dst, parallelism)
                    });
                    let actual = written(geometry, out, &before, |dst| {
                        filters::horizontal_mean(&source, r, dst, parallelism)
                    });
                    assert_eq!(
                        actual, expected,
                        "{width}x{height} {out:?} held {held:?} radius {r} {parallelism:?}"
                    );
                    compared += 1;
                }
            }
        }
    }
    assert!(compared > 1000, "{compared} comparisons");
}

#[test]
fn the_vertical_mean_matches_the_tap_by_tap_reference_bit_for_bit() {
    let mut rng = SplitMix64(0x7E27_1CA1);
    let mut compared = 0;
    for (width, height) in FRAMES {
        let frame = Rect::frame(width, height);
        let geometry = Geometry::new(width, height, frame);
        for out in rects(&mut rng, width, height) {
            for r in radii(width, height) {
                let extra = [0, 0, 1, 3][rng.next_usize(4)];
                let held = grown(out, extra, r + extra, frame);
                let mut source = values(&mut rng, held.pixels());
                let source = PlaneMut::over(&mut source, geometry, held).expect("the source");
                let source = source.as_plane();
                let before = values(&mut rng, out.pixels());
                for parallelism in PARALLELISMS {
                    let expected = written(geometry, out, &before, |dst| {
                        vertical_mean(&source, r, dst, parallelism)
                    });
                    let actual = written(geometry, out, &before, |dst| {
                        filters::vertical_mean(&source, r, dst, parallelism)
                    });
                    assert_eq!(
                        actual, expected,
                        "{width}x{height} {out:?} held {held:?} radius {r} {parallelism:?}"
                    );
                    compared += 1;
                }
            }
        }
    }
    assert!(compared > 1000, "{compared} comparisons");
}
