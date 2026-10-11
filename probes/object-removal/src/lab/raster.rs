//! What the lab hands its window to draw: the downscaled photograph and tinted object overlays.
//!
//! Each raster has a unique id, so the window makes one image handle per raster when it first sees
//! it and never one per frame. Overlays are sampled from the candidate's logits over the object's
//! bounds only, at a few samples per logit cell, and stay under iced's synchronous upload size so a
//! new one is drawn in the frame it arrives in.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::image::Rgb8;
use crate::mask::{Logits, Point};
use crate::segment;
use crate::stroke;

/// The photograph's display copy is at most this long on its long side: well within the 8192
/// texture limit iced asks wgpu for.
pub const DISPLAY_LONG: u32 = 4096;

/// Overlay rasters stay below this many pixels: 4 bytes each keeps them under iced_wgpu's 2 MiB
/// synchronous upload, above which an image is uploaded on a worker and first drawn a frame later.
pub const MAX_OVERLAY_PIXELS: usize = 500_000;

/// Overlay samples per logit cell along each axis: the logits are the mask's true resolution, so
/// more only smooths the bilinear edge.
pub const SAMPLES_PER_CELL: f32 = 6.0;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A process-wide unique raster id.
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// The photograph as displayed.
#[derive(Clone, Debug)]
pub struct Picture {
    pub id: u64,
    /// The full-resolution image it shows.
    pub source: (u32, u32),
    pub size: (u32, u32),
    /// RGBA, taken by the window when it makes the handle.
    pub rgba: Option<Vec<u8>>,
}

/// `image` reduced to at most `long` on its long side (never enlarged), as RGBA.
pub fn picture(image: &Rgb8, long: u32) -> Picture {
    let reduced;
    let shown = if image.width.max(image.height) > long {
        let (w, h) = image.long_side_size(long);
        reduced = segment::resample_rgb8(image, w, h);
        &reduced
    } else {
        image
    };
    let mut rgba = Vec::with_capacity(shown.data.len() / 3 * 4);
    for px in shown.data.chunks_exact(3) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    Picture {
        id: next_id(),
        source: (image.width, image.height),
        size: (shown.width, shown.height),
        rgba: Some(rgba),
    }
}

/// A tinted overlay over part of the image.
#[derive(Clone, Debug)]
pub struct Raster {
    pub id: u64,
    /// Where it lies, in source pixels: `[x, y, width, height]`.
    pub rect: [f32; 4],
    pub size: (u32, u32),
    /// Straight-alpha RGBA, row-major; taken by the window when it makes the handle.
    pub rgba: Option<Vec<u8>>,
    /// The mask drawn, as a hash of its cells above 0: an unchanged result keeps its raster.
    pub signature: u64,
}

/// A sampling grid over the source: sample `(i, j)` is at `origin + ((i + ½)·step.0, (j + ½)·step.1)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampling {
    pub origin: Point,
    pub step: (f32, f32),
    pub size: (usize, usize),
}

impl Sampling {
    pub fn at(&self, i: usize, j: usize) -> Point {
        Point::new(
            self.origin.x + (i as f32 + 0.5) * self.step.0,
            self.origin.y + (j as f32 + 0.5) * self.step.1,
        )
    }
}

/// The source rectangle `[x, y, width, height]` outside which the logits are never above 0: the
/// positive cells' bounds padded by a cell (bilinear sampling reaches under a cell beyond a cell
/// centre, and the clamped edge reaches the image border), clamped to the image.
pub fn positive_bounds(l: &Logits) -> Option<[f32; 4]> {
    let (u0, v0, u1, v1) = stroke::cells_above(l, 0.0)?;
    let (cx, cy) = l.cells_per_px;
    let (w, h) = (l.source.0 as f32, l.source.1 as f32);
    let x0 = ((u0 as f32 - 0.5) / cx).clamp(0.0, w);
    let y0 = ((v0 as f32 - 0.5) / cy).clamp(0.0, h);
    let x1 = ((u1 as f32 + 1.5) / cx).clamp(0.0, w);
    let y1 = ((v1 as f32 + 1.5) / cy).clamp(0.0, h);
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

/// Whether each sample's logit is above 0, row-major.
pub fn inside(l: &Logits, s: &Sampling) -> Vec<bool> {
    let (w, h) = s.size;
    let mut out = Vec::with_capacity(w * h);
    for j in 0..h {
        for i in 0..w {
            out.push(l.sample(s.at(i, j)) > 0.0);
        }
    }
    out
}

/// The grid for an overlay of `l` over its positive bounds: `SAMPLES_PER_CELL` per cell, never
/// finer than `min_step` source pixels, and fewer than `MAX_OVERLAY_PIXELS` samples.
pub fn overlay_sampling(l: &Logits, min_step: f32) -> Option<Sampling> {
    let [x, y, w, h] = positive_bounds(l)?;
    let cell = (1.0 / l.cells_per_px.0).max(1.0 / l.cells_per_px.1);
    let mut step = (cell / SAMPLES_PER_CELL).max(min_step).max(1.0);
    let count =
        |step: f32| ((w / step).ceil() as usize).max(1) * ((h / step).ceil() as usize).max(1);
    while count(step) >= MAX_OVERLAY_PIXELS {
        step *= ((count(step) as f32 / MAX_OVERLAY_PIXELS as f32).sqrt()).max(1.01);
    }
    let size = (
        ((w / step).ceil() as usize).max(1),
        ((h / step).ceil() as usize).max(1),
    );
    Some(Sampling {
        origin: Point::new(x, y),
        step: (w / size.0 as f32, h / size.1 as f32),
        size,
    })
}

/// A tinted overlay of the candidate: the edge opaque in `colour`, the inside translucent.
/// `None` when no logit is above 0.
pub fn object_raster(l: &Logits, min_step: f32, colour: [u8; 3]) -> Option<Raster> {
    let s = overlay_sampling(l, min_step)?;
    let bits = inside(l, &s);
    if !bits.contains(&true) {
        return None;
    }
    let (w, h) = s.size;
    let at = |i: isize, j: isize| {
        i >= 0
            && j >= 0
            && (i as usize) < w
            && (j as usize) < h
            && bits[j as usize * w + i as usize]
    };
    let mut rgba = Vec::with_capacity(w * h * 4);
    let [r, g, b] = colour;
    for j in 0..h as isize {
        for i in 0..w as isize {
            let alpha = if !at(i, j) {
                0
            } else if !at(i - 1, j) || !at(i + 1, j) || !at(i, j - 1) || !at(i, j + 1) {
                255
            } else {
                96
            };
            // Transparent samples keep the tint so linear filtering never darkens the edge.
            rgba.extend_from_slice(&[r, g, b, alpha]);
        }
    }
    Some(Raster {
        id: next_id(),
        rect: [
            s.origin.x,
            s.origin.y,
            w as f32 * s.step.0,
            h as f32 * s.step.1,
        ],
        size: (w as u32, h as u32),
        rgba: Some(rgba),
        signature: signature(l),
    })
}

/// A hash of the grid's size and which cells are above 0.
pub fn signature(l: &Logits) -> u64 {
    // FNV-1a over the size and the sign bits, eight cells to a byte.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |byte: u8| {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    };
    for b in (l.width as u64)
        .to_le_bytes()
        .into_iter()
        .chain((l.height as u64).to_le_bytes())
    {
        eat(b);
    }
    for chunk in l.data.chunks(8) {
        let mut byte = 0u8;
        for (k, &v) in chunk.iter().enumerate() {
            byte |= ((v > 0.0) as u8) << k;
        }
        eat(byte);
    }
    hash
}

/// Where to put a candidate's pin: the centre of the cell above 0 farthest from any cell that is
/// not (a 3-4 chamfer distance, counting the image's edge as outside), in source pixels. Ties go to
/// the first in row order. `None` when no cell is above 0.
pub fn pin(l: &Logits) -> Option<Point> {
    let (u0, v0, u1, v1) = stroke::cells_above(l, 0.0)?;
    let (w, h) = (u1 - u0 + 3, v1 - v0 + 3);
    const FAR: u32 = u32::MAX / 2;
    let mut d = vec![0u32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let (u, v) = (u0 + x - 1, v0 + y - 1);
            if l.data[v * l.width + u] > 0.0 {
                d[y * w + x] = FAR;
            }
        }
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let best = [(i - 1, 3), (i - w, 3), (i - w - 1, 4), (i - w + 1, 4)]
                .iter()
                .map(|&(k, c)| d[k] + c)
                .min()
                .unwrap_or(FAR);
            d[i] = d[i].min(best);
        }
    }
    for y in (1..h - 1).rev() {
        for x in (1..w - 1).rev() {
            let i = y * w + x;
            let best = [(i + 1, 3), (i + w, 3), (i + w + 1, 4), (i + w - 1, 4)]
                .iter()
                .map(|&(k, c)| d[k] + c)
                .min()
                .unwrap_or(FAR);
            d[i] = d[i].min(best);
        }
    }
    let (mut best, mut at) = (0, None);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            if d[y * w + x] > best {
                best = d[y * w + x];
                at = Some((u0 + x - 1, v0 + y - 1));
            }
        }
    }
    let (u, v) = at?;
    let (cx, cy) = l.cells_per_px;
    Some(Point::new(
        ((u as f32 + 0.5) / cx).clamp(0.0, l.source.0 as f32),
        ((v as f32 + 0.5) / cy).clamp(0.0, l.source.1 as f32),
    ))
}

/// Distinct colours for the objects offered under a stroke, in the order they are numbered.
pub fn palette(k: usize) -> [u8; 3] {
    const COLOURS: [[u8; 3]; 8] = [
        [255, 196, 0],
        [255, 64, 129],
        [0, 230, 118],
        [41, 121, 255],
        [255, 109, 0],
        [170, 0, 255],
        [0, 229, 255],
        [198, 255, 0],
    ];
    COLOURS[k % COLOURS.len()]
}

/// The hover tint.
pub const HOVER: [u8; 3] = [30, 160, 255];

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16×16 grid over a 160×160 image, positive over the cells in `cells`.
    fn grid(cells: impl Fn(usize, usize) -> bool) -> Logits {
        let mut data = vec![-4.0f32; 256];
        for v in 0..16 {
            for u in 0..16 {
                if cells(u, v) {
                    data[v * 16 + u] = 4.0;
                }
            }
        }
        Logits {
            width: 16,
            height: 16,
            data,
            source: (160, 160),
            cells_per_px: (0.1, 0.1),
        }
    }

    #[test]
    fn positive_bounds_hold_every_positive_sample() {
        let l = grid(|u, v| (4..=7).contains(&u) && (9..=10).contains(&v));
        let [x, y, w, h] = positive_bounds(&l).unwrap();
        for py in 0..160 {
            for px in 0..160 {
                let p = Point::new(px as f32 + 0.5, py as f32 + 0.5);
                if l.sample(p) > 0.0 {
                    assert!(
                        p.x >= x && p.x <= x + w && p.y >= y && p.y <= y + h,
                        "{p:?}"
                    );
                }
            }
        }
        // Tight to within a cell and a half on each side.
        assert!(x > 20.0 && x + w < 100.0 && y > 70.0 && y + h < 130.0);
        assert!(positive_bounds(&grid(|_, _| false)).is_none());
    }

    #[test]
    fn an_overlay_stays_under_the_synchronous_upload_size() {
        // A whole-frame object on a 60 MP image at one source pixel per sample would be 60 MP.
        let mut l = grid(|_, _| true);
        l.source = (10_000, 6_000);
        l.cells_per_px = (16.0 / 10_000.0, 16.0 / 6_000.0);
        let s = overlay_sampling(&l, 1.0).unwrap();
        assert!(s.size.0 * s.size.1 < MAX_OVERLAY_PIXELS, "{s:?}");
        let r = object_raster(&l, 1.0, HOVER).unwrap();
        assert!(r.rgba.as_ref().unwrap().len() < 2 * 1024 * 1024);
        assert_eq!(
            r.rgba.as_ref().unwrap().len(),
            r.size.0 as usize * r.size.1 as usize * 4
        );
    }

    #[test]
    fn the_overlay_edge_is_opaque_and_the_inside_translucent() {
        let l = grid(|u, v| (4..=11).contains(&u) && (4..=11).contains(&v));
        let r = object_raster(&l, 1.0, [1, 2, 3]).unwrap();
        let rgba = r.rgba.unwrap();
        let (w, h) = (r.size.0 as usize, r.size.1 as usize);
        let alpha = |i: usize, j: usize| rgba[(j * w + i) * 4 + 3];
        assert_eq!(alpha(w / 2, h / 2), 96);
        assert_eq!(alpha(0, 0), 0);
        let row = h / 2;
        let first = (0..w).find(|&i| alpha(i, row) > 0).unwrap();
        assert_eq!(alpha(first, row), 255);
        assert_eq!(&rgba[..3], &[1, 2, 3]);
    }

    #[test]
    fn signatures_follow_the_mask_not_the_values() {
        let a = grid(|u, v| u < 5 && v < 5);
        let mut b = a.clone();
        for v in b.data.iter_mut() {
            *v *= 2.0;
        }
        assert_eq!(signature(&a), signature(&b));
        let c = grid(|u, v| u < 5 && v < 6);
        assert_ne!(signature(&a), signature(&c));
    }

    #[test]
    fn a_pin_sits_deep_inside_its_object() {
        // An L shape: the pin must land in the thick part, inside the object.
        let l =
            grid(|u, v| ((2..=10).contains(&u) && (2..=10).contains(&v)) || (u <= 14 && v == 12));
        let p = pin(&l).unwrap();
        assert!(l.sample(p) > 0.0);
        assert!(
            (50.0..=90.0).contains(&p.x) && (50.0..=90.0).contains(&p.y),
            "{p:?}"
        );
        assert!(pin(&grid(|_, _| false)).is_none());
    }

    #[test]
    fn the_display_copy_fits_the_long_side() {
        let image = Rgb8::new(50, 20, vec![7; 50 * 20 * 3]);
        let p = picture(&image, 25);
        assert_eq!(p.size, (25, 10));
        assert_eq!(p.source, (50, 20));
        assert_eq!(p.rgba.as_ref().unwrap().len(), 25 * 10 * 4);
        let same = picture(&image, 4096);
        assert_eq!(same.size, (50, 20));
        assert_eq!(&same.rgba.unwrap()[..4], &[7, 7, 7, 255]);
    }
}
