//! Independent f64 whole-frame reference for the frozen Presence units: Dehaze,
//! Texture and Clarity.
//!
//! This module shares no code with production. It exists so a later production
//! implementation of the `lightwell.presence` spatial operation has an oracle it
//! cannot influence. The frozen equations are written out in full in
//! `docs/design/presence-study.md`; this file is their literal transcription, and
//! the two must be read together. Every constant below is named identically to the
//! constant of the same name in that document.
//!
//! Domain: linear-sRGB (D65) `f64` RGB, with no gamut clamp and no rounding, matching
//! the spatial-primitive contract in `docs/design/presence-mixer-vignette.md`: values
//! outside `[0, 1]` and negative values are preserved between units, and the host
//! clamps and quantizes only at the output boundary.
//!
//! **Tile invariance is the point of the structure below.** Every filter is a
//! finite-support box, min or box-based guided filter evaluated by direct summation
//! over its window in a fixed order, and every plane carries the frame it belongs to
//! plus the sub-rectangle it actually holds. Reads outside the frame clamp to the
//! frame edge; reads outside the held rectangle panic. A caller that asks for an
//! output rectangle while holding only the rectangle plus the declared halo therefore
//! gets bit-identical results to the whole-frame evaluation, or a panic — never a
//! silently different answer. `studies/presence.rs` uses exactly that to prove the
//! declared halos are sufficient.
//!
//! The sRGB transfer function is transcribed here from the standard constants rather
//! than imported from `reference::tone`, so a parallel study's edits to that file
//! cannot change this study's frozen numbers.

// ---------------------------------------------------------------------------
// Frozen constants
// ---------------------------------------------------------------------------

/// Rec. 709 / sRGB luma coefficients on *linear* sRGB, as in the tone study.
pub const LUMA_R: f64 = 0.2126;
pub const LUMA_G: f64 = 0.7152;
pub const LUMA_B: f64 = 0.0722;

/// Below this linear luminance (absolute value) the luminance-ratio reconstruction
/// switches to the tone study's additive rule.
pub const EPSILON_L: f64 = 1e-6;

/// The reference long side every radius below is quoted at: 6000 px, a 24 MP stage.
pub const REFERENCE_LONG_SIDE: f64 = 6000.0;

/// Texture: the fine and coarse guided-filter radii in pixels at `REFERENCE_LONG_SIDE`.
pub const R_TEXTURE_FINE_6000: f64 = 1.0;
pub const R_TEXTURE_COARSE_6000: f64 = 4.0;
/// Texture: the guided filter's regularization in squared encoded units. `a = 0.5` at a
/// window standard deviation of `sqrt(EPS_TEXTURE) = 0.05` encoded (about 13 of 255 codes).
pub const EPS_TEXTURE: f64 = 2.5e-3;
/// Texture: the soft-clip limit on the encoded excursion, in encoded units.
pub const LIMIT_TEXTURE: f64 = 0.10;
/// Texture: gain applied to the band at `+100` (the band's own amplitude is multiplied by
/// `1 + GAIN_TEXTURE_POS`) and at `-100` (the band is removed at `GAIN_TEXTURE_NEG = 1`).
pub const GAIN_TEXTURE_POS: f64 = 3.0;
pub const GAIN_TEXTURE_NEG: f64 = 1.0;

/// Clarity: the integer reduction factor per axis its base is computed on.
pub const CLARITY_REDUCTION: i64 = 4;
/// Clarity: the base radius in full-resolution pixels at `REFERENCE_LONG_SIDE`
/// (1.6% of the long side).
pub const R_CLARITY_6000: f64 = 96.0;
/// Clarity: the guided filter's regularization in squared encoded units. `a = 0.5` at a
/// window standard deviation of `0.1` encoded (about 26 of 255 codes).
pub const EPS_CLARITY: f64 = 1.0e-2;
/// Clarity: the soft-clip limit on the encoded excursion, in encoded units.
pub const LIMIT_CLARITY: f64 = 0.15;
/// Clarity: gain on the broad residual at `+100` (the residual is doubled) and at `-100`
/// (three quarters of it is removed). The negative gain is not 1: removing the residual
/// entirely replaces the image with its own base, which measures as 3% of the input
/// amplitude surviving on low-contrast content -- a blur rather than the soft rendering a
/// negative Clarity is reached for.
pub const GAIN_CLARITY_POS: f64 = 1.0;
pub const GAIN_CLARITY_NEG: f64 = 0.75;

/// Dehaze: the integer reduction factor per axis its transmission map is computed on.
pub const DEHAZE_REDUCTION: i64 = 4;
/// Dehaze: the dark-channel min-filter radius in full-resolution pixels at
/// `REFERENCE_LONG_SIDE` (0.2% of the long side).
pub const R_DEHAZE_DARK_6000: f64 = 12.0;
/// Dehaze: the guided-filter refinement radius in full-resolution pixels at
/// `REFERENCE_LONG_SIDE` (0.4% of the long side).
pub const R_DEHAZE_GUIDE_6000: f64 = 24.0;
/// Dehaze: the transmission guided filter's regularization, in squared encoded units.
pub const EPS_DEHAZE: f64 = 1.0e-4;
/// Dehaze: the veil fraction removed at `|amount| = 100`. `1.0` means `+100` is the exact
/// inverse of the forward model wherever the dark-channel estimate of `t` is exact.
pub const OMEGA_MAX: f64 = 1.0;
/// Dehaze: the transmission floor, bounding the recovery gain at `1 / T_FLOOR = 10`.
pub const T_FLOOR: f64 = 0.1;
/// Dehaze: the extra uniform veil a negative amount adds on top of deepening the estimated
/// one. Without it a negative amount would be an exact no-op on a haze-free photograph,
/// because the dark-channel estimate of a haze-free scene is `t = 1` everywhere.
pub const VEIL_MAX: f64 = 0.5;
/// Dehaze: the floor on each channel of the atmospheric light, so `I / A` is always finite.
pub const A_FLOOR: f64 = 1.0e-3;
/// Dehaze: the host reduction the atmospheric light is estimated from, per axis.
pub const ESTIMATE_REDUCTION: i64 = 16;
/// Dehaze: the fraction of the reduction's brightest dark-channel pixels averaged for `A`.
pub const ATMOSPHERE_FRACTION: f64 = 0.001;
/// Dehaze: the minimum number of reduction pixels averaged for `A`.
pub const ATMOSPHERE_MIN_COUNT: usize = 16;

// ---------------------------------------------------------------------------
// sRGB working domain (transcribed from the standard constants)
// ---------------------------------------------------------------------------

/// The sRGB OETF, analytically continued to every finite real value, exactly as the
/// tone study defines the encoded working domain. No clamping.
pub fn encode_srgb_extended(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// The inverse of [`encode_srgb_extended`], equally continued. No clamping.
pub fn decode_srgb_extended(e: f64) -> f64 {
    if e <= 0.040_45 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

/// Rec. 709 relative luminance of a linear-sRGB triple. Not gamut-clamped.
pub fn luminance(rgb: [f64; 3]) -> f64 {
    LUMA_R * rgb[0] + LUMA_G * rgb[1] + LUMA_B * rgb[2]
}

/// The tone study's luminance-ratio reconstruction with its additive near-black rule.
pub fn reconstruct(rgb: [f64; 3], l_in: f64, l_out: f64) -> [f64; 3] {
    if l_in.abs() < EPSILON_L {
        let delta = l_out - l_in;
        [rgb[0] + delta, rgb[1] + delta, rgb[2] + delta]
    } else {
        let ratio = l_out / l_in;
        [rgb[0] * ratio, rgb[1] * ratio, rgb[2] * ratio]
    }
}

// ---------------------------------------------------------------------------
// Radii and halos
// ---------------------------------------------------------------------------

/// The long side above which radii stop growing. The host accepts stages up to 16384 px per
/// side; letting the radii keep scaling there would push the summed halo to 732 px, past the
/// host's 512 bound, and the operation would be refused with `resource-limit` on a wide
/// panorama. Capping the scale keeps Presence available at every accepted stage size, at the
/// cost that above 60 MP the effect is slightly finer relative to the frame than at 24 MP.
pub const MAX_SCALE_LONG_SIDE: i64 = 10000;

/// The frozen radius scaling rule: a radius quoted at `REFERENCE_LONG_SIDE` scales with the
/// stage's long side (capped at [`MAX_SCALE_LONG_SIDE`]) and is rounded to an integer with a
/// minimum of 1.
pub fn scaled_radius(radius_at_6000: f64, long_side: i64) -> i64 {
    let long_side = long_side.min(MAX_SCALE_LONG_SIDE);
    let scaled = radius_at_6000 * (long_side as f64) / REFERENCE_LONG_SIDE;
    (scaled.round() as i64).max(1)
}

/// Texture's fine radius in full-resolution pixels.
pub fn texture_fine_radius(long_side: i64) -> i64 {
    scaled_radius(R_TEXTURE_FINE_6000, long_side)
}

/// Texture's coarse radius in full-resolution pixels. The band-nondegeneracy rule keeps it
/// at least one pixel above the fine radius, so the band is never identically zero on a
/// small stage (which would make Texture a silent no-op below a long side of about 750 px).
pub fn texture_coarse_radius(long_side: i64) -> i64 {
    scaled_radius(R_TEXTURE_COARSE_6000, long_side).max(texture_fine_radius(long_side) + 1)
}

/// Texture's halo: two box passes of the coarse guided filter, at full resolution.
pub fn texture_halo(long_side: i64) -> i64 {
    2 * texture_coarse_radius(long_side)
}

/// Clarity's guided-filter radius on its reduced grid.
pub fn clarity_reduced_radius(long_side: i64) -> i64 {
    let full = scaled_radius(R_CLARITY_6000, long_side);
    (((full as f64) / (CLARITY_REDUCTION as f64)).round() as i64).max(1)
}

/// The full-resolution halo of a stage computed on a reduced grid.
///
/// `reach` is how many reduced pixels beyond its own reduced index a reduced result depends
/// on (`2r` for one guided filter, `r` for one box or min filter, and the sum along a chain
/// of them). The bilinear upsample adds one more reduced index, and a full pixel can sit at
/// the far end of its own block, so the worst-case full-resolution reach is
/// `(reach + 1) * s + (s - 1) = (reach + 2) * s - 1`.
pub fn reduced_halo(reach: i64, reduction: i64) -> i64 {
    (reach + 2) * reduction - 1
}

/// Clarity's halo in full-resolution pixels: one guided filter on the reduced grid, so the
/// reduced reach is twice its reduced radius.
pub fn clarity_halo(long_side: i64) -> i64 {
    reduced_halo(2 * clarity_reduced_radius(long_side), CLARITY_REDUCTION)
}

/// Dehaze's dark-channel min-filter radius on its reduced grid.
pub fn dehaze_dark_reduced_radius(long_side: i64) -> i64 {
    let full = scaled_radius(R_DEHAZE_DARK_6000, long_side);
    (((full as f64) / (DEHAZE_REDUCTION as f64)).round() as i64).max(1)
}

/// Dehaze's transmission guided-filter radius on its reduced grid.
pub fn dehaze_guide_reduced_radius(long_side: i64) -> i64 {
    let full = scaled_radius(R_DEHAZE_GUIDE_6000, long_side);
    (((full as f64) / (DEHAZE_REDUCTION as f64)).round() as i64).max(1)
}

/// Dehaze's halo in full-resolution pixels: the min filter and the guided filter are
/// sequential on the reduced grid, so their reduced reaches add.
pub fn dehaze_halo(long_side: i64) -> i64 {
    let reach = dehaze_dark_reduced_radius(long_side) + 2 * dehaze_guide_reduced_radius(long_side);
    reduced_halo(reach, DEHAZE_REDUCTION)
}

/// The summed halo of the three units in their frozen order, which the host bounds at 512.
pub fn presence_halo(long_side: i64) -> i64 {
    dehaze_halo(long_side) + texture_halo(long_side) + clarity_halo(long_side)
}

// ---------------------------------------------------------------------------
// Planes: a frame, the sub-rectangle actually held, and edge-clamped reads
// ---------------------------------------------------------------------------

/// A half-open rectangle in pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl Rect {
    pub fn new(x0: i64, y0: i64, x1: i64, y1: i64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn frame(width: i64, height: i64) -> Self {
        Self::new(0, 0, width, height)
    }

    pub fn width(&self) -> i64 {
        (self.x1 - self.x0).max(0)
    }

    pub fn height(&self) -> i64 {
        (self.y1 - self.y0).max(0)
    }

    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    pub fn contains(&self, x: i64, y: i64) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    pub fn expand(&self, by: i64) -> Self {
        Self::new(self.x0 - by, self.y0 - by, self.x1 + by, self.y1 + by)
    }

    pub fn clip(&self, other: Rect) -> Self {
        Self::new(
            self.x0.max(other.x0),
            self.y0.max(other.y0),
            self.x1.min(other.x1),
            self.y1.min(other.y1),
        )
    }
}

/// One scalar plane of a frame, holding only `rect` of it.
///
/// `get` clamps the requested coordinate to the frame (the host's edge-clamp rule) and then
/// requires the clamped coordinate to be inside `rect`. Anything else is a halo bug and
/// panics rather than returning a different number than the whole-frame evaluation would.
#[derive(Clone, Debug)]
pub struct Plane {
    pub frame_width: i64,
    pub frame_height: i64,
    pub rect: Rect,
    data: Vec<f64>,
}

impl Plane {
    pub fn zeros(frame_width: i64, frame_height: i64, rect: Rect) -> Self {
        let rect = rect.clip(Rect::frame(frame_width, frame_height));
        let len = (rect.width() * rect.height()) as usize;
        Self {
            frame_width,
            frame_height,
            rect,
            data: vec![0.0; len],
        }
    }

    pub fn frame_rect(&self) -> Rect {
        Rect::frame(self.frame_width, self.frame_height)
    }

    fn index(&self, x: i64, y: i64) -> usize {
        ((y - self.rect.y0) * self.rect.width() + (x - self.rect.x0)) as usize
    }

    /// Edge-clamped read. Panics when the clamped coordinate is outside the held rectangle.
    pub fn get(&self, x: i64, y: i64) -> f64 {
        let cx = x.clamp(0, self.frame_width - 1);
        let cy = y.clamp(0, self.frame_height - 1);
        assert!(
            self.rect.contains(cx, cy),
            "read of ({cx}, {cy}) outside the held rectangle {:?}: the declared halo is too small",
            self.rect
        );
        self.data[self.index(cx, cy)]
    }

    pub fn set(&mut self, x: i64, y: i64, value: f64) {
        let index = self.index(x, y);
        self.data[index] = value;
    }

    /// A new plane over `rect` whose value at each pixel is `f` of this plane's value there.
    pub fn map_rect(&self, rect: Rect, f: impl Fn(f64) -> f64) -> Plane {
        let mut out = Plane::zeros(self.frame_width, self.frame_height, rect);
        let rect = out.rect;
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                out.set(x, y, f(self.get(x, y)));
            }
        }
        out
    }
}

/// A linear-sRGB RGB image (or region of one) as three planes over one rectangle.
#[derive(Clone, Debug)]
pub struct Rgb {
    pub planes: [Plane; 3],
}

impl Rgb {
    pub fn zeros(frame_width: i64, frame_height: i64, rect: Rect) -> Self {
        Self {
            planes: [
                Plane::zeros(frame_width, frame_height, rect),
                Plane::zeros(frame_width, frame_height, rect),
                Plane::zeros(frame_width, frame_height, rect),
            ],
        }
    }

    /// A whole frame from row-major RGB triples.
    pub fn from_pixels(width: i64, height: i64, pixels: &[[f64; 3]]) -> Self {
        assert_eq!(pixels.len() as i64, width * height, "pixel count mismatch");
        let mut rgb = Rgb::zeros(width, height, Rect::frame(width, height));
        for y in 0..height {
            for x in 0..width {
                let pixel = pixels[(y * width + x) as usize];
                for (plane, value) in rgb.planes.iter_mut().zip(pixel.iter()) {
                    plane.set(x, y, *value);
                }
            }
        }
        rgb
    }

    pub fn rect(&self) -> Rect {
        self.planes[0].rect
    }

    pub fn frame_width(&self) -> i64 {
        self.planes[0].frame_width
    }

    pub fn frame_height(&self) -> i64 {
        self.planes[0].frame_height
    }

    pub fn frame_rect(&self) -> Rect {
        self.planes[0].frame_rect()
    }

    pub fn long_side(&self) -> i64 {
        self.frame_width().max(self.frame_height())
    }

    pub fn get(&self, x: i64, y: i64) -> [f64; 3] {
        [
            self.planes[0].get(x, y),
            self.planes[1].get(x, y),
            self.planes[2].get(x, y),
        ]
    }

    pub fn set(&mut self, x: i64, y: i64, value: [f64; 3]) {
        for (plane, component) in self.planes.iter_mut().zip(value.iter()) {
            plane.set(x, y, *component);
        }
    }

    /// The encoded luminance plane over `rect`.
    pub fn encoded_luminance(&self, rect: Rect) -> Plane {
        let mut out = Plane::zeros(self.frame_width(), self.frame_height(), rect);
        let rect = out.rect;
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                out.set(x, y, encode_srgb_extended(luminance(self.get(x, y))));
            }
        }
        out
    }

    pub fn is_finite(&self) -> bool {
        let rect = self.rect();
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                if !self.get(x, y).iter().all(|value| value.is_finite()) {
                    return false;
                }
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Finite-support filters
// ---------------------------------------------------------------------------

/// The box mean over a `(2r+1)x(2r+1)` window, evaluated separably: the horizontal mean
/// first, then the vertical mean of that, each by direct summation over the window in
/// increasing-offset order. Direct summation (rather than a running sum) is what makes the
/// result independent of where evaluation started, so a tile and the whole frame agree
/// bit for bit. Halo: `r`.
pub fn box_mean(src: &Plane, r: i64, out: Rect) -> Plane {
    let frame = src.frame_rect();
    let out = out.clip(frame);
    let n = (2 * r + 1) as f64;
    // The horizontal pass is evaluated over the output's rows expanded by `r` *in y only*:
    // expanding in x as well would make the pass read `2r` columns beyond the output and
    // inflate the filter's halo from `r` to `2r`.
    let mid = Rect::new(out.x0, out.y0 - r, out.x1, out.y1 + r).clip(frame);
    let mut horizontal = Plane::zeros(src.frame_width, src.frame_height, mid);
    for y in mid.y0..mid.y1 {
        for x in mid.x0..mid.x1 {
            let mut sum = 0.0;
            for dx in -r..=r {
                sum += src.get(x + dx, y);
            }
            horizontal.set(x, y, sum / n);
        }
    }
    let mut out_plane = Plane::zeros(src.frame_width, src.frame_height, out);
    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            let mut sum = 0.0;
            for dy in -r..=r {
                sum += horizontal.get(x, y + dy);
            }
            out_plane.set(x, y, sum / n);
        }
    }
    out_plane
}

/// The minimum over a `(2r+1)x(2r+1)` window, evaluated separably. Halo: `r`.
pub fn box_min(src: &Plane, r: i64, out: Rect) -> Plane {
    let frame = src.frame_rect();
    let out = out.clip(frame);
    let mid = Rect::new(out.x0, out.y0 - r, out.x1, out.y1 + r).clip(frame);
    let mut horizontal = Plane::zeros(src.frame_width, src.frame_height, mid);
    for y in mid.y0..mid.y1 {
        for x in mid.x0..mid.x1 {
            let mut value = f64::INFINITY;
            for dx in -r..=r {
                value = value.min(src.get(x + dx, y));
            }
            horizontal.set(x, y, value);
        }
    }
    let mut out_plane = Plane::zeros(src.frame_width, src.frame_height, out);
    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            let mut value = f64::INFINITY;
            for dy in -r..=r {
                value = value.min(horizontal.get(x, y + dy));
            }
            out_plane.set(x, y, value);
        }
    }
    out_plane
}

/// The box-based guided filter of `input` under `guide` (He, Sun and Tang), with box radius
/// `r` and regularization `eps`:
///
/// ```text
/// var = max(0, mean(G*G) - mean(G)^2)      cov = mean(G*I) - mean(G)*mean(I)
/// a   = cov / (var + eps)                  b   = mean(I) - a * mean(G)
/// q   = mean(a) * G + mean(b)
/// ```
///
/// Two sequential box passes of radius `r`, so the halo is `2r`. `var` is floored at 0
/// because `mean(G*G) - mean(G)^2` can be a very small negative number in floating point on
/// a constant window; the floor never changes a mathematically positive variance.
pub fn guided_filter(guide: &Plane, input: &Plane, r: i64, eps: f64, out: Rect) -> Plane {
    let frame = guide.frame_rect();
    let out = out.clip(frame);
    let inner = out.expand(r).clip(frame);
    let source = inner.expand(r).clip(frame);

    let guide_squared = guide.map_rect(source, |g| g * g);
    let mut guide_input = Plane::zeros(guide.frame_width, guide.frame_height, source);
    for y in source.y0..source.y1 {
        for x in source.x0..source.x1 {
            guide_input.set(x, y, guide.get(x, y) * input.get(x, y));
        }
    }

    let mean_guide = box_mean(guide, r, inner);
    let mean_input = box_mean(input, r, inner);
    let mean_guide_squared = box_mean(&guide_squared, r, inner);
    let mean_guide_input = box_mean(&guide_input, r, inner);

    let mut a = Plane::zeros(guide.frame_width, guide.frame_height, inner);
    let mut b = Plane::zeros(guide.frame_width, guide.frame_height, inner);
    for y in inner.y0..inner.y1 {
        for x in inner.x0..inner.x1 {
            let mg = mean_guide.get(x, y);
            let mi = mean_input.get(x, y);
            let variance = (mean_guide_squared.get(x, y) - mg * mg).max(0.0);
            let covariance = mean_guide_input.get(x, y) - mg * mi;
            let a_value = covariance / (variance + eps);
            a.set(x, y, a_value);
            b.set(x, y, mi - a_value * mg);
        }
    }

    let mean_a = box_mean(&a, r, out);
    let mean_b = box_mean(&b, r, out);
    let mut q = Plane::zeros(guide.frame_width, guide.frame_height, out);
    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            q.set(x, y, mean_a.get(x, y) * guide.get(x, y) + mean_b.get(x, y));
        }
    }
    q
}

/// The self-guided case (`guide == input`), which is the edge-preserving smoother Texture
/// and Clarity use. Written out separately because `cov == var` there, so `a` and `b`
/// reduce to `var/(var+eps)` and `(1-a)*mean`, with no second set of box passes.
pub fn guided_self(src: &Plane, r: i64, eps: f64, out: Rect) -> Plane {
    let frame = src.frame_rect();
    let out = out.clip(frame);
    let inner = out.expand(r).clip(frame);
    let source = inner.expand(r).clip(frame);

    let squared = src.map_rect(source, |v| v * v);
    let mean = box_mean(src, r, inner);
    let mean_squared = box_mean(&squared, r, inner);

    let mut a = Plane::zeros(src.frame_width, src.frame_height, inner);
    let mut b = Plane::zeros(src.frame_width, src.frame_height, inner);
    for y in inner.y0..inner.y1 {
        for x in inner.x0..inner.x1 {
            let m = mean.get(x, y);
            let variance = (mean_squared.get(x, y) - m * m).max(0.0);
            let a_value = variance / (variance + eps);
            a.set(x, y, a_value);
            b.set(x, y, (1.0 - a_value) * m);
        }
    }

    let mean_a = box_mean(&a, r, out);
    let mean_b = box_mean(&b, r, out);
    let mut q = Plane::zeros(src.frame_width, src.frame_height, out);
    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            q.set(x, y, mean_a.get(x, y) * src.get(x, y) + mean_b.get(x, y));
        }
    }
    q
}

// ---------------------------------------------------------------------------
// Integer-factor reduction and bilinear upsample
// ---------------------------------------------------------------------------

/// The reduced frame size for an integer reduction factor: `ceil(size / s)` per axis, so the
/// right and bottom blocks may be partial.
pub fn reduced_frame(width: i64, height: i64, reduction: i64) -> (i64, i64) {
    (
        (width + reduction - 1) / reduction,
        (height + reduction - 1) / reduction,
    )
}

/// The reduced rectangle covering a full-resolution rectangle.
pub fn reduced_rect(rect: Rect, reduction: i64) -> Rect {
    if rect.is_empty() {
        return Rect::new(0, 0, 0, 0);
    }
    Rect::new(
        rect.x0.div_euclid(reduction),
        rect.y0.div_euclid(reduction),
        (rect.x1 - 1).div_euclid(reduction) + 1,
        (rect.y1 - 1).div_euclid(reduction) + 1,
    )
}

/// The full-resolution rectangle a reduced rectangle's blocks cover.
pub fn full_rect(rect: Rect, reduction: i64) -> Rect {
    Rect::new(
        rect.x0 * reduction,
        rect.y0 * reduction,
        rect.x1 * reduction,
        rect.y1 * reduction,
    )
}

/// Box-average downsample by an integer factor: reduced pixel `(i, j)` is the mean of the
/// full pixels in `[i*s, min((i+1)*s, width)) x [j*s, min((j+1)*s, height))`, so a partial
/// block at the right or bottom edge is averaged over its actual pixels and never over
/// clamped copies. The block grid is anchored at the frame origin, which is what makes a
/// tile's reduced pixels identical to the whole frame's.
pub fn downsample(src: &Plane, reduction: i64, out: Rect) -> Plane {
    let (rw, rh) = reduced_frame(src.frame_width, src.frame_height, reduction);
    let out = out.clip(Rect::frame(rw, rh));
    let mut plane = Plane::zeros(rw, rh, out);
    for j in out.y0..out.y1 {
        let y0 = j * reduction;
        let y1 = ((j + 1) * reduction).min(src.frame_height);
        for i in out.x0..out.x1 {
            let x0 = i * reduction;
            let x1 = ((i + 1) * reduction).min(src.frame_width);
            let mut sum = 0.0;
            let mut count = 0.0;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += src.get(x, y);
                    count += 1.0;
                }
            }
            plane.set(i, j, sum / count);
        }
    }
    plane
}

/// Bilinear upsample from a reduced plane back to full resolution. A full pixel `x` samples
/// the reduced coordinate `u = (x + 0.5)/s - 0.5`, blending reduced indices `floor(u)` and
/// `floor(u) + 1`, each clamped to the reduced frame. A full pixel therefore reaches at most
/// one reduced index beyond its own block, which is the `+1` in [`reduced_halo`].
pub fn upsample(
    reduced: &Plane,
    reduction: i64,
    frame_width: i64,
    frame_height: i64,
    out: Rect,
) -> Plane {
    let out = out.clip(Rect::frame(frame_width, frame_height));
    let mut plane = Plane::zeros(frame_width, frame_height, out);
    let s = reduction as f64;
    for y in out.y0..out.y1 {
        let v = ((y as f64) + 0.5) / s - 0.5;
        let j0 = v.floor();
        let fy = v - j0;
        let j0 = j0 as i64;
        for x in out.x0..out.x1 {
            let u = ((x as f64) + 0.5) / s - 0.5;
            let i0 = u.floor();
            let fx = u - i0;
            let i0 = i0 as i64;
            let top = reduced.get(i0, j0) * (1.0 - fx) + reduced.get(i0 + 1, j0) * fx;
            let bottom = reduced.get(i0, j0 + 1) * (1.0 - fx) + reduced.get(i0 + 1, j0 + 1) * fx;
            plane.set(x, y, top * (1.0 - fy) + bottom * fy);
        }
    }
    plane
}

// ---------------------------------------------------------------------------
// The shared compressive gain
// ---------------------------------------------------------------------------

/// The compressive (soft-clipping) gain both luminance units apply to their raw encoded
/// excursion:
///
/// ```text
/// headroom = 1 - e        for raw > 0          headroom = e      for raw < 0
/// lim      = min(limit, max(0, headroom))
/// delta    = lim * tanh(raw / lim)             (0 when lim == 0)
/// ```
///
/// Consequences, all used by the frozen claims:
/// * `|delta| < lim <= limit`, so the encoded excursion is bounded by the unit's limit and
///   a step edge's overshoot cannot exceed it.
/// * `lim <= headroom`, so an encoded value inside `[0, 1]` stays strictly inside `[0, 1]`:
///   the units cannot create a clipped highlight or a crushed black that was not there.
/// * `lim = 0` outside `[0, 1]`, so an already out-of-range value passes through untouched
///   rather than being clamped.
/// * `delta` is continuous and strictly increasing in `raw` (derivative `sech^2 > 0` on each
///   side, value and slope `1` at `raw = 0`), so the response is monotone in the amount.
/// * `raw = 0` gives exactly `0`, with no rounding.
pub fn soft_clip(raw: f64, encoded: f64, limit: f64) -> f64 {
    if raw == 0.0 {
        return 0.0;
    }
    let headroom = if raw > 0.0 { 1.0 - encoded } else { encoded };
    let lim = limit.min(headroom.max(0.0));
    if lim <= 0.0 {
        return 0.0;
    }
    lim * (raw / lim).tanh()
}

/// Texture's amount-to-gain mapping: `+100` amplifies the band to `1 + GAIN_TEXTURE_POS`
/// times its own amplitude, `-100` removes it exactly. Piecewise linear, continuous and
/// strictly increasing through 0.
pub fn texture_gain(amount: f64) -> f64 {
    if amount >= 0.0 {
        amount / 100.0 * GAIN_TEXTURE_POS
    } else {
        amount / 100.0 * GAIN_TEXTURE_NEG
    }
}

/// Clarity's amount-to-gain mapping: `+100` doubles the broad residual, `-100` removes it.
pub fn clarity_gain(amount: f64) -> f64 {
    if amount >= 0.0 {
        amount / 100.0 * GAIN_CLARITY_POS
    } else {
        amount / 100.0 * GAIN_CLARITY_NEG
    }
}

/// Dehaze's amount-to-strength mapping: the veil fraction the transmission estimate removes
/// (`amount > 0`) or adds (`amount < 0`).
pub fn dehaze_omega(amount: f64) -> f64 {
    OMEGA_MAX * amount.abs() / 100.0
}

// ---------------------------------------------------------------------------
// Texture
// ---------------------------------------------------------------------------

/// Texture over `out`: a medium-frequency luminance band isolated as the difference of two
/// self-guided edge-preserving smoothers of the encoded luminance, scaled by the compressive
/// gain and reapplied.
///
/// ```text
/// E     = encode(luminance(rgb))
/// F     = guided_self(E, r_fine,   EPS_TEXTURE)
/// C     = guided_self(E, r_coarse, EPS_TEXTURE)
/// band  = F - C
/// delta = soft_clip(texture_gain(amount) * band, E, LIMIT_TEXTURE)
/// out   = reconstruct(rgb, L, decode(E + delta))
/// ```
///
/// Structure finer than `r_fine` survives both smoothers and cancels; structure coarser than
/// `r_coarse` is in both and cancels; a strong edge drives both filters' `a` toward 1, so
/// both reproduce it and the band is near zero there. `amount == 0` returns the input
/// untouched, with no encode/decode round trip, and so does any pixel whose `delta` is
/// exactly zero.
pub fn texture_region(src: &Rgb, amount: f64, long_side: i64, out: Rect) -> Rgb {
    let out = out.clip(src.frame_rect());
    let mut result = Rgb::zeros(src.frame_width(), src.frame_height(), out);
    if amount == 0.0 {
        for y in out.y0..out.y1 {
            for x in out.x0..out.x1 {
                result.set(x, y, src.get(x, y));
            }
        }
        return result;
    }

    let r_fine = texture_fine_radius(long_side);
    let r_coarse = texture_coarse_radius(long_side);
    let gain = texture_gain(amount);
    let encoded = src.encoded_luminance(out.expand(2 * r_coarse).clip(src.frame_rect()));
    let fine = guided_self(&encoded, r_fine, EPS_TEXTURE, out);
    let coarse = guided_self(&encoded, r_coarse, EPS_TEXTURE, out);

    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            let rgb = src.get(x, y);
            let e = encoded.get(x, y);
            let band = fine.get(x, y) - coarse.get(x, y);
            let delta = soft_clip(gain * band, e, LIMIT_TEXTURE);
            if delta == 0.0 {
                result.set(x, y, rgb);
                continue;
            }
            let l_in = luminance(rgb);
            let l_out = decode_srgb_extended(e + delta);
            result.set(x, y, reconstruct(rgb, l_in, l_out));
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Clarity
// ---------------------------------------------------------------------------

/// Clarity over `out`: the residual of encoded luminance against a broad edge-preserving
/// base computed on a `CLARITY_REDUCTION`-times reduced grid, scaled by the compressive gain
/// and reapplied.
///
/// ```text
/// E        = encode(luminance(rgb))
/// E_red    = downsample(E, 4)
/// B_red    = guided_self(E_red, clarity_reduced_radius, EPS_CLARITY)
/// B        = upsample(B_red, 4)
/// delta    = soft_clip(clarity_gain(amount) * (E - B), E, LIMIT_CLARITY)
/// out      = reconstruct(rgb, L, decode(E + delta))
/// ```
///
/// The base is reduced because a full-resolution guided filter at 1.6% of the long side
/// would cost 321x321 window sums per pixel at 60 MP; on the reduced grid the same
/// neighbourhood costs 1/16 of the work, and the halo is the one [`reduced_halo`] states
/// (the reduction shrinks the work, not the neighbourhood).
pub fn clarity_region(src: &Rgb, amount: f64, long_side: i64, out: Rect) -> Rgb {
    let frame = src.frame_rect();
    let out = out.clip(frame);
    let mut result = Rgb::zeros(src.frame_width(), src.frame_height(), out);
    if amount == 0.0 {
        for y in out.y0..out.y1 {
            for x in out.x0..out.x1 {
                result.set(x, y, src.get(x, y));
            }
        }
        return result;
    }

    let s = CLARITY_REDUCTION;
    let r_red = clarity_reduced_radius(long_side);
    let gain = clarity_gain(amount);
    let (rw, rh) = reduced_frame(src.frame_width(), src.frame_height(), s);
    let reduced_frame_rect = Rect::frame(rw, rh);

    // The bilinear upsample reaches one reduced index beyond the output's own blocks, and the
    // guided filter reaches 2*r_red beyond that.
    let base_rect = reduced_rect(out, s).expand(1).clip(reduced_frame_rect);
    let reduced_source = base_rect.expand(2 * r_red).clip(reduced_frame_rect);
    let encoded_rect = full_rect(reduced_source, s).clip(frame);

    let encoded = src.encoded_luminance(encoded_rect);
    let encoded_reduced = downsample(&encoded, s, reduced_source);
    let base_reduced = guided_self(&encoded_reduced, r_red, EPS_CLARITY, base_rect);
    let base = upsample(&base_reduced, s, src.frame_width(), src.frame_height(), out);

    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            let rgb = src.get(x, y);
            let e = encoded.get(x, y);
            let residual = e - base.get(x, y);
            let delta = soft_clip(gain * residual, e, LIMIT_CLARITY);
            if delta == 0.0 {
                result.set(x, y, rgb);
                continue;
            }
            let l_in = luminance(rgb);
            let l_out = decode_srgb_extended(e + delta);
            result.set(x, y, reconstruct(rgb, l_in, l_out));
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Dehaze
// ---------------------------------------------------------------------------

/// Dehaze's global estimate: the atmospheric light `A`, one linear-sRGB triple.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Atmosphere(pub [f64; 3]);

/// Estimate `A` from the host's `1/ESTIMATE_REDUCTION`-per-side box-average reduction of the
/// whole stage.
///
/// ```text
/// R        = box-average reduction of the stage by 16 (partial edge blocks over their
///            actual pixels)
/// d(p)     = min(R_r(p), R_g(p), R_b(p))
/// N        = clamp(max(16, ceil(0.001 * pixels)), 1, pixels)
/// S        = the N pixels of R with the largest d, ties broken by smaller row-major index
/// A_c      = max(mean over S of R_c, A_FLOOR)
/// ```
///
/// The pointwise channel minimum is used rather than a second windowed dark channel because
/// the 16x16 box average has already removed isolated bright pixels, and because the host
/// caps this estimate's input at 0.25 megapixels. The result is 3 f64 (24 bytes), far inside
/// the host's 4 KiB limit for a global estimate.
pub fn estimate_atmosphere(stage: &Rgb) -> Atmosphere {
    assert_eq!(
        stage.rect(),
        stage.frame_rect(),
        "the atmospheric light is estimated from the whole stage, not a region"
    );
    let s = ESTIMATE_REDUCTION;
    let (rw, rh) = reduced_frame(stage.frame_width(), stage.frame_height(), s);
    let reduced_rect = Rect::frame(rw, rh);
    let reduced: Vec<Plane> = (0..3)
        .map(|channel| downsample(&stage.planes[channel], s, reduced_rect))
        .collect();

    let pixels = (rw * rh) as usize;
    let mut dark: Vec<(f64, usize)> = Vec::with_capacity(pixels);
    for j in 0..rh {
        for i in 0..rw {
            let value = reduced[0]
                .get(i, j)
                .min(reduced[1].get(i, j))
                .min(reduced[2].get(i, j));
            dark.push((value, (j * rw + i) as usize));
        }
    }
    // Descending by dark value, ties broken by the smaller row-major index: a total order,
    // so the selection is deterministic for any input including a constant frame.
    dark.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .expect("the reference refuses non-finite input")
            .then(left.1.cmp(&right.1))
    });
    let count = (ATMOSPHERE_MIN_COUNT.max((ATMOSPHERE_FRACTION * pixels as f64).ceil() as usize))
        .clamp(1, pixels);

    let mut sums = [0.0; 3];
    for (_, index) in dark.iter().take(count) {
        let i = (*index as i64) % rw;
        let j = (*index as i64) / rw;
        for (channel, sum) in sums.iter_mut().enumerate() {
            *sum += reduced[channel].get(i, j);
        }
    }
    let mut a = [0.0; 3];
    for channel in 0..3 {
        a[channel] = (sums[channel] / count as f64).max(A_FLOOR);
    }
    Atmosphere(a)
}

/// The refined transmission map over `out`, in full-resolution coordinates, before the floor.
///
/// ```text
/// I_red      = downsample(rgb, 4)
/// d(p)       = min_c clamp(I_red_c(p) / A_c, 0, 1)
/// dark(p)    = min over the (2*r_dc+1)^2 window of d
/// t_raw      = 1 - omega * dark
/// G          = encode(luminance(I_red))
/// t_reduced  = guided_filter(G, t_raw, r_gf, EPS_DEHAZE)
/// t          = upsample(t_reduced, 4)
/// ```
///
/// `d` is clamped to `[0, 1]` inside the estimator (not as a gamut clamp on pixel values):
/// without it a pixel brighter than `A` would be read as opaque haze rather than as no haze,
/// and `t_raw` could leave `[0, 1]` entirely. With it, `t_raw` is in `[1 - omega, 1]` for
/// every input.
pub fn transmission_region(
    src: &Rgb,
    atmosphere: Atmosphere,
    omega: f64,
    long_side: i64,
    out: Rect,
) -> Plane {
    let frame = src.frame_rect();
    let out = out.clip(frame);
    let s = DEHAZE_REDUCTION;
    let r_dark = dehaze_dark_reduced_radius(long_side);
    let r_guide = dehaze_guide_reduced_radius(long_side);
    let (rw, rh) = reduced_frame(src.frame_width(), src.frame_height(), s);
    let reduced_frame_rect = Rect::frame(rw, rh);

    let refined_rect = reduced_rect(out, s).expand(1).clip(reduced_frame_rect);
    let raw_rect = refined_rect.expand(2 * r_guide).clip(reduced_frame_rect);
    let dark_source_rect = raw_rect.expand(r_dark).clip(reduced_frame_rect);
    let full_source = full_rect(dark_source_rect, s).clip(frame);

    debug_assert!(
        full_source.is_empty() || src.rect().clip(full_source) == full_source,
        "the dehaze input region is smaller than the declared halo"
    );
    let mut reduced = Rgb::zeros(rw, rh, dark_source_rect);
    for channel in 0..3 {
        reduced.planes[channel] = downsample(&src.planes[channel], s, dark_source_rect);
    }

    let a = atmosphere.0;
    let mut normalized = Plane::zeros(rw, rh, dark_source_rect);
    for y in dark_source_rect.y0..dark_source_rect.y1 {
        for x in dark_source_rect.x0..dark_source_rect.x1 {
            let pixel = reduced.get(x, y);
            let mut value = f64::INFINITY;
            for channel in 0..3 {
                value = value.min((pixel[channel] / a[channel]).clamp(0.0, 1.0));
            }
            normalized.set(x, y, value);
        }
    }

    let dark = box_min(&normalized, r_dark, raw_rect);
    let mut raw = Plane::zeros(rw, rh, raw_rect);
    let mut guide = Plane::zeros(rw, rh, raw_rect);
    for y in raw_rect.y0..raw_rect.y1 {
        for x in raw_rect.x0..raw_rect.x1 {
            raw.set(x, y, 1.0 - omega * dark.get(x, y));
            guide.set(x, y, encode_srgb_extended(luminance(reduced.get(x, y))));
        }
    }

    let refined = guided_filter(&guide, &raw, r_guide, EPS_DEHAZE, refined_rect);
    upsample(&refined, s, src.frame_width(), src.frame_height(), out)
}

/// Dehaze over `out`, on linear-light RGB because a veil is chromatic.
///
/// ```text
/// t          = clamp(transmission(omega(|amount|)), T_FLOOR, 1)
/// amount > 0 : out_c = (I_c - A_c) / t + A_c                       (the model inverted)
/// amount < 0 : t_veil = t * (1 - VEIL_MAX * |amount| / 100)
///              out_c  = t_veil * I_c + (1 - t_veil) * A_c          (the model forward)
/// ```
///
/// Both branches use the same estimated transmission and the same `A`. The negative branch
/// scales that transmission by a further uniform factor because the estimate alone cannot
/// add haze to a photograph that has none: a haze-free scene has `t = 1` everywhere, so a
/// strict `out = t*I + (1-t)*A` would be an exact no-op there. With the factor, `-100`
/// blends a haze-free pixel `1 - VEIL_MAX` of the way toward `A` and deepens an existing
/// veil multiplicatively, which is what more atmosphere does. Both branches are continuous
/// at `amount = 0`, where `omega = 0` gives `t = 1` and the factor gives 1.
///
/// The forward branch is a convex combination of `I` and `A`, so it never leaves their span;
/// the inverse branch's gain is bounded by `1/T_FLOOR = 10` and is not clamped, so a
/// recovered value may leave `[0, 1]` and is preserved, per the host contract.
pub fn dehaze_region(
    src: &Rgb,
    amount: f64,
    atmosphere: Atmosphere,
    long_side: i64,
    out: Rect,
) -> Rgb {
    let out = out.clip(src.frame_rect());
    let mut result = Rgb::zeros(src.frame_width(), src.frame_height(), out);
    if amount == 0.0 {
        for y in out.y0..out.y1 {
            for x in out.x0..out.x1 {
                result.set(x, y, src.get(x, y));
            }
        }
        return result;
    }

    let omega = dehaze_omega(amount);
    let transmission = transmission_region(src, atmosphere, omega, long_side, out);
    let a = atmosphere.0;
    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            let pixel = src.get(x, y);
            // The guided refinement can overshoot slightly past 1 next to a transition, and a
            // transmission above 1 has no physical meaning: it would attenuate the scene
            // rather than restore it. The estimate is clamped to `[T_FLOOR, 1]` here; this is
            // part of the estimator, not a gamut clamp on pixel values.
            let t = transmission.get(x, y).clamp(T_FLOOR, 1.0);
            let mut value = [0.0; 3];
            for channel in 0..3 {
                value[channel] = if amount > 0.0 {
                    (pixel[channel] - a[channel]) / t + a[channel]
                } else {
                    let veil = t * (1.0 - VEIL_MAX * amount.abs() / 100.0);
                    veil * pixel[channel] + (1.0 - veil) * a[channel]
                };
            }
            result.set(x, y, value);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// The operation
// ---------------------------------------------------------------------------

/// The three Presence amounts, each in the agreed -100..100 UI range. All-zero is the
/// identity and compiles to no operation at all.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresenceParams {
    pub texture: f64,
    pub clarity: f64,
    pub dehaze: f64,
}

impl PresenceParams {
    pub const NEUTRAL: Self = Self {
        texture: 0.0,
        clarity: 0.0,
        dehaze: 0.0,
    };

    pub fn is_neutral(&self) -> bool {
        *self == Self::NEUTRAL
    }
}

/// The whole-frame reference: Dehaze, then Texture, then Clarity, the frozen unit order.
///
/// `long_side` is the *stage's* long side, which sets every radius. It is passed explicitly
/// rather than taken from the image so a small fixture can be evaluated with the radii a
/// 24 MP stage uses, which is exactly what a tile of such a stage sees.
pub fn apply_presence(src: &Rgb, params: PresenceParams, long_side: i64) -> Rgb {
    assert_eq!(
        src.rect(),
        src.frame_rect(),
        "apply_presence evaluates a whole frame"
    );
    if params.is_neutral() {
        return src.clone();
    }
    let frame = src.frame_rect();
    let after_dehaze = if params.dehaze == 0.0 {
        src.clone()
    } else {
        let atmosphere = estimate_atmosphere(src);
        dehaze_region(src, params.dehaze, atmosphere, long_side, frame)
    };
    let after_texture = texture_region(&after_dehaze, params.texture, long_side, frame);
    clarity_region(&after_texture, params.clarity, long_side, frame)
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    fn constant_plane(width: i64, height: i64, value: f64) -> Plane {
        let mut plane = Plane::zeros(width, height, Rect::frame(width, height));
        for y in 0..height {
            for x in 0..width {
                plane.set(x, y, value);
            }
        }
        plane
    }

    #[test]
    fn box_mean_of_a_constant_is_that_constant() {
        let plane = constant_plane(16, 16, 0.375);
        let mean = box_mean(&plane, 3, Rect::frame(16, 16));
        for y in 0..16 {
            for x in 0..16 {
                assert!((mean.get(x, y) - 0.375).abs() < 1e-15, "at ({x}, {y})");
            }
        }
    }

    #[test]
    fn guided_self_reproduces_a_constant_and_leaves_a_strong_edge_alone() {
        let plane = constant_plane(16, 16, 0.5);
        let smoothed = guided_self(&plane, 2, EPS_TEXTURE, Rect::frame(16, 16));
        for y in 0..16 {
            for x in 0..16 {
                assert!((smoothed.get(x, y) - 0.5).abs() < 1e-14);
            }
        }

        // A step of 0.6 encoded is far above sqrt(EPS_TEXTURE) = 0.05, so `a` is close to 1
        // and the filter reproduces the input rather than blurring across the edge.
        let mut step = Plane::zeros(32, 8, Rect::frame(32, 8));
        for y in 0..8 {
            for x in 0..32 {
                step.set(x, y, if x < 16 { 0.2 } else { 0.8 });
            }
        }
        let smoothed = guided_self(&step, 2, EPS_TEXTURE, Rect::frame(32, 8));
        let worst = (0..32)
            .map(|x| (smoothed.get(x, 4) - step.get(x, 4)).abs())
            .fold(0.0f64, f64::max);
        assert!(worst < 0.02, "guided filter blurred a strong edge: {worst}");
    }

    #[test]
    fn downsample_then_upsample_reproduces_a_constant() {
        let plane = constant_plane(19, 7, 0.25);
        let reduced = downsample(&plane, 4, Rect::frame(5, 2));
        let restored = upsample(&reduced, 4, 19, 7, Rect::frame(19, 7));
        for y in 0..7 {
            for x in 0..19 {
                assert!((restored.get(x, y) - 0.25).abs() < 1e-15, "at ({x}, {y})");
            }
        }
    }

    #[test]
    fn partial_blocks_average_over_their_actual_pixels() {
        // 6 columns reduced by 4: block 1 holds columns 4 and 5 only.
        let mut plane = Plane::zeros(6, 1, Rect::frame(6, 1));
        for x in 0..6 {
            plane.set(x, 0, x as f64);
        }
        let reduced = downsample(&plane, 4, Rect::frame(2, 1));
        assert!((reduced.get(0, 0) - 1.5).abs() < 1e-15);
        assert!((reduced.get(1, 0) - 4.5).abs() < 1e-15);
    }

    #[test]
    fn soft_clip_is_bounded_zero_at_zero_and_monotone() {
        assert_eq!(soft_clip(0.0, 0.5, LIMIT_TEXTURE), 0.0);
        for encoded in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let mut previous = f64::NEG_INFINITY;
            for step in -40..=40 {
                let raw = f64::from(step) / 10.0;
                let delta = soft_clip(raw, encoded, LIMIT_CLARITY);
                assert!(delta.abs() <= LIMIT_CLARITY);
                assert!(delta > previous || (raw == 0.0 && delta == 0.0) || delta == previous);
                previous = delta;
                let out = encoded + delta;
                assert!(
                    (-1e-12..=1.0 + 1e-12).contains(&out),
                    "encoded {encoded} raw {raw} left [0, 1]: {out}"
                );
            }
        }
        // Outside [0, 1] there is no headroom, so the value passes through untouched.
        assert_eq!(soft_clip(5.0, 1.2, LIMIT_CLARITY), 0.0);
        assert_eq!(soft_clip(-5.0, -0.2, LIMIT_CLARITY), 0.0);
    }

    #[test]
    fn extended_encode_decode_round_trip() {
        for i in -50..=306 {
            let l = f64::from(i) / 100.0;
            let back = decode_srgb_extended(encode_srgb_extended(l));
            assert!((back - l).abs() <= 1e-9 + 1e-9 * l.abs(), "l={l}");
        }
    }

    #[test]
    fn halos_at_the_documented_sizes() {
        assert_eq!(
            (texture_halo(480), clarity_halo(480), dehaze_halo(480)),
            (4, 23, 19)
        );
        assert_eq!(
            (texture_halo(6000), clarity_halo(6000), dehaze_halo(6000)),
            (8, 199, 67)
        );
        assert_eq!(
            (texture_halo(10000), clarity_halo(10000), dehaze_halo(10000)),
            (14, 327, 107)
        );
        assert_eq!(presence_halo(480), 46);
        assert_eq!(presence_halo(6000), 274);
        assert_eq!(presence_halo(10000), 448);
        // The cap holds the halo flat above 60 MP, so the largest stage the host accepts
        // (16384 px per side) still fits the 512 bound.
        assert_eq!(presence_halo(16384), 448);
        assert!(presence_halo(16384) <= 512);
    }
}
