//! Geometry in source pixels, coverage masks and decoder logits.
//!
//! Coordinates are continuous source pixels: the origin is the top-left corner of the top-left
//! pixel, `x` grows right and `y` down, and pixel `(i, j)` covers `[i, i+1) × [j, j+1)` with its
//! centre at `(i + 0.5, j + 0.5)`.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An integer pixel rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RectPx {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl RectPx {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    /// The smallest rectangle containing both.
    pub fn union(&self, o: &RectPx) -> RectPx {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.width).max(o.x + o.width);
        let y1 = (self.y + self.height).max(o.y + o.height);
        RectPx {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }

    /// Grown by `by` pixels on every side, clamped to an image of `size`.
    pub fn grown(&self, by: u32, size: (u32, u32)) -> RectPx {
        let x0 = self.x.saturating_sub(by);
        let y0 = self.y.saturating_sub(by);
        let x1 = (self.x + self.width + by).min(size.0);
        let y1 = (self.y + self.height + by).min(size.1);
        RectPx {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }
}

/// Coverage in `0..=255` over a window of the source image; every pixel outside the window is 0.
/// 255 means "remove this pixel entirely", 0 means "leave it byte-identical".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mask {
    /// The image the mask belongs to.
    pub image: (u32, u32),
    pub window: RectPx,
    pub data: Vec<u8>,
}

impl Mask {
    pub fn empty(image: (u32, u32)) -> Self {
        Self {
            image,
            window: RectPx::default(),
            data: Vec::new(),
        }
    }

    pub fn new(image: (u32, u32), window: RectPx, data: Vec<u8>) -> Self {
        assert_eq!(
            data.len(),
            window.width as usize * window.height as usize,
            "mask window and buffer disagree"
        );
        assert!(
            window.x + window.width <= image.0 && window.y + window.height <= image.1,
            "mask window outside the image"
        );
        Self {
            image,
            window,
            data,
        }
    }

    pub fn get(&self, x: u32, y: u32) -> u8 {
        if !self.window.contains(x, y) {
            return 0;
        }
        self.data[(y - self.window.y) as usize * self.window.width as usize
            + (x - self.window.x) as usize]
    }

    /// Pixels at or above half coverage.
    pub fn area(&self) -> u64 {
        self.data.iter().filter(|&&v| v >= 128).count() as u64
    }

    /// The tight bounds of non-zero coverage, or `None` for an empty mask.
    pub fn bounds(&self) -> Option<RectPx> {
        let w = self.window.width as usize;
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, &v) in self.data.iter().enumerate() {
            if v > 0 {
                let (x, y) = (i % w, i / w);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
        (x0 != usize::MAX).then(|| RectPx {
            x: self.window.x + x0 as u32,
            y: self.window.y + y0 as u32,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }

    /// Writes the whole-image coverage as an 8-bit grey PNG.
    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        let (w, h) = self.image;
        let mut img = image::GrayImage::new(w, h);
        for y in self.window.y..self.window.y + self.window.height {
            for x in self.window.x..self.window.x + self.window.width {
                img.put_pixel(x, y, image::Luma([self.get(x, y)]));
            }
        }
        img.save(path)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Reads a grey or colour PNG as coverage (its luminance), over the whole image.
    pub fn load_png(path: &Path) -> Result<Self, String> {
        let img = image::open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .into_luma8();
        let (w, h) = img.dimensions();
        Ok(Mask::new(
            (w, h),
            RectPx {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            img.into_raw(),
        ))
    }
}

/// Low-resolution mask logits as a SAM-family decoder returns them, with the map from source
/// pixels. A logit above 0 is inside the object.
#[derive(Clone, Debug, PartialEq)]
pub struct Logits {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    /// The source image the logits describe.
    pub source: (u32, u32),
    /// Logit cells per source pixel along x and y. A source pixel centre `(i + 0.5, j + 0.5)` maps
    /// to the cell coordinate `((i + 0.5) · cx − 0.5, (j + 0.5) · cy − 0.5)`, where cell `(u, v)`
    /// has its centre at integer `(u, v)` (the `align_corners = false` convention).
    pub cells_per_px: (f32, f32),
}

impl Logits {
    /// The bilinearly interpolated logit at a continuous source position, clamping at the edges.
    pub fn sample(&self, p: Point) -> f32 {
        let u = (p.x * self.cells_per_px.0 - 0.5).clamp(0.0, (self.width - 1) as f32);
        let v = (p.y * self.cells_per_px.1 - 0.5).clamp(0.0, (self.height - 1) as f32);
        let (u0, v0) = (u.floor() as usize, v.floor() as usize);
        let (u1, v1) = ((u0 + 1).min(self.width - 1), (v0 + 1).min(self.height - 1));
        let (fu, fv) = (u - u0 as f32, v - v0 as f32);
        let at = |x: usize, y: usize| self.data[y * self.width + x];
        let top = at(u0, v0) * (1.0 - fu) + at(u1, v0) * fu;
        let bottom = at(u0, v1) * (1.0 - fu) + at(u1, v1) * fu;
        top * (1.0 - fv) + bottom * fv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logits_map_pixel_centres_to_cell_centres() {
        // A 2×2 grid over a 4×4 image: source pixel centres 1.0 and 3.0 fall on cell centres 0 and 1.
        let l = Logits {
            width: 2,
            height: 2,
            data: vec![0.0, 1.0, 2.0, 3.0],
            source: (4, 4),
            cells_per_px: (0.5, 0.5),
        };
        assert_eq!(l.sample(Point::new(1.0, 1.0)), 0.0);
        assert_eq!(l.sample(Point::new(3.0, 1.0)), 1.0);
        assert_eq!(l.sample(Point::new(3.0, 3.0)), 3.0);
        assert_eq!(l.sample(Point::new(2.0, 1.0)), 0.5);
    }

    #[test]
    fn bounds_are_tight() {
        let mut data = vec![0u8; 16];
        data[5] = 10;
        data[10] = 255;
        let m = Mask::new(
            (10, 10),
            RectPx {
                x: 2,
                y: 3,
                width: 4,
                height: 4,
            },
            data,
        );
        assert_eq!(
            m.bounds(),
            Some(RectPx {
                x: 3,
                y: 4,
                width: 2,
                height: 2
            })
        );
        assert_eq!(m.get(4, 5), 255);
        assert_eq!(m.get(0, 0), 0);
    }
}
