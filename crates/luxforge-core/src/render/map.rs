//! The host's continuous geometry evaluator. Coordinates are pixel edges; centres are i + 0.5.
use crate::{
    Error,
    modules::{ExactGeometry, Stage},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub const MAX_WARP_STEPS: usize = 3;
pub const MAX_COVER_SCALE: f64 = 4.0;
pub const MAX_LOCAL_MINIFICATION: f64 = 1.8;
const COVER_SAFETY: f64 = 1e-12;
const INVERSE_MAX_ITERATIONS: usize = 32;
const INVERSE_TOLERANCE_PX: f64 = 1e-4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    Outside,
    Unconverged,
}
impl MapError {
    pub(crate) fn error(self) -> Error {
        match self {
            Self::Outside => Error::validation("point is outside the geometry domain"),
            Self::Unconverged => Error::render("geometry inversion did not converge"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Affine(pub(crate) [f64; 6]);
impl Affine {
    pub(crate) const IDENTITY: Self = Self([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    pub(crate) fn from_exact(s: ExactGeometry) -> Self {
        let (a, b, c, d) = (s.a as f64, s.b as f64, s.c as f64, s.d as f64);
        Self([
            a,
            b,
            s.tx as f64 + 0.5 - 0.5 * (a + b),
            c,
            d,
            s.ty as f64 + 0.5 - 0.5 * (c + d),
        ])
    }
    pub(crate) fn then(self, next: Self) -> Self {
        let [a, b, c, d, e, f] = self.0;
        let [g, h, i, j, k, l] = next.0;
        Self([
            g * a + h * d,
            g * b + h * e,
            g * c + h * f + i,
            j * a + k * d,
            j * b + k * e,
            j * c + k * f + l,
        ])
    }
    pub(crate) fn invert(self) -> Result<Self, Error> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * e - b * d;
        let inv = Self([
            e / det,
            -b / det,
            (b * f - e * c) / det,
            -d / det,
            a / det,
            (d * c - a * f) / det,
        ]);
        if det == 0.0 || !inv.0.iter().all(|v| v.is_finite()) {
            return Err(Error::validation(
                "a geometry layer declares a mapping that cannot be inverted",
            ));
        }
        Ok(inv)
    }
    fn at(self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + b * y + c, d * x + e * y + f)
    }
}

/// A resample's output-to-input map. Every raster and query uses this evaluator.
#[derive(Clone, Debug, PartialEq)]
pub enum Mapping {
    Affine([f64; 6]),
    Warp(Arc<WarpChain>),
}
impl Mapping {
    #[inline]
    pub fn input_at(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            Self::Affine(m) => Affine(*m).at(x, y),
            Self::Warp(chain) => chain.input_at(x, y),
        }
    }
    pub(crate) fn finite(&self) -> bool {
        match self {
            Self::Affine(m) => m.iter().all(|v| v.is_finite()),
            Self::Warp(chain) => chain.validate().is_ok(),
        }
    }
    pub(crate) fn has_warp(&self) -> bool {
        matches!(self, Self::Warp(_))
    }
    pub(crate) fn bounds(&self, bounds: (f64, f64, f64, f64)) -> Option<(f64, f64, f64, f64)> {
        match self {
            Self::Affine(m) => bounds_of(WarpStep::Affine(*m), bounds),
            Self::Warp(chain) => {
                let mut b = bounds;
                for step in &chain.steps {
                    b = bounds_of(*step, b)?;
                }
                Some(b)
            }
        }
    }
    pub(crate) fn steps_forward(&self) -> Vec<WarpStep> {
        match self {
            Self::Affine(m) => vec![WarpStep::Affine(*m)],
            Self::Warp(c) => c.steps.iter().rev().copied().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RadialModel {
    Poly3,
    Poly5,
    PtLens,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projective {
    h: f64,
    v: f64,
    centre: (f64, f64),
    unit: f64,
    cover: f64,
    local_scale_max: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Radial {
    model: RadialModel,
    terms: [f64; 3],
    centre: (f64, f64),
    px_per_unit: f64,
    cover: f64,
    monotone_radius: f64,
    local_scale_max: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "kebab-case")]
pub enum WarpStep {
    Affine([f64; 6]),
    Projective(Projective),
    Radial(Radial),
}
#[derive(Clone, Debug, PartialEq)]
pub struct WarpChain {
    pub(crate) steps: Vec<WarpStep>,
}
impl WarpChain {
    pub(crate) fn new(step: WarpStep) -> Self {
        Self { steps: vec![step] }
    }
    pub(crate) fn prepend(&mut self, step: WarpStep) -> Result<(), Error> {
        if self.steps.len() >= MAX_WARP_STEPS {
            return Err(Error::validation("too many fused geometry steps"));
        }
        self.steps.insert(0, step);
        self.validate()
    }
    fn validate(&self) -> Result<(), Error> {
        if self.steps.is_empty() || self.steps.len() > MAX_WARP_STEPS {
            return Err(Error::validation("invalid fused geometry step count"));
        }
        for step in &self.steps {
            step.validate()?;
        }
        self.validate_limits()
    }
    fn validate_limits(&self) -> Result<(), Error> {
        let cover: f64 = self.steps.iter().map(|s| s.cover()).product();
        let scale: f64 = self.steps.iter().map(|s| s.local_scale_max()).product();
        if !cover.is_finite() || cover <= 0.0 || !scale.is_finite() || scale <= 0.0 {
            return Err(Error::validation(
                "a warp declares non-finite or degenerate derived geometry",
            ));
        }
        if cover > MAX_COVER_SCALE {
            return Err(Error::validation(format!(
                "lens and perspective need a {cover:.2}× zoom, more than the 4× limit"
            )));
        }
        if scale > MAX_LOCAL_MINIFICATION {
            return Err(Error::unsupported_input(
                "strong minification: bilinear would alias",
            ));
        }
        Ok(())
    }
    #[inline]
    fn input_at(&self, mut x: f64, mut y: f64) -> (f64, f64) {
        for step in &self.steps {
            (x, y) = step.input_at(x, y);
        }
        (x, y)
    }
}

fn eval(p: &[f64], x: f64) -> f64 {
    p.iter().rev().fold(0.0, |s, c| s * x + c)
}
/// Isolate real roots using derivative turning points; degrees are at most three.
fn roots(p: &[f64], lo: f64, hi: f64) -> Vec<f64> {
    let mut n = p.len();
    while n > 1 && p[n - 1] == 0.0 {
        n -= 1;
    }
    let p = &p[..n];
    if n <= 1 {
        return Vec::new();
    }
    if n == 2 {
        let r = -p[0] / p[1];
        return if r >= lo && r <= hi {
            vec![r]
        } else {
            Vec::new()
        };
    }
    let derivative: Vec<_> = (1..n).map(|i| p[i] * i as f64).collect();
    let mut points = vec![lo];
    points.extend(roots(&derivative, lo, hi));
    points.push(hi);
    let mut out = Vec::new();
    for &x in &points {
        if eval(p, x).abs() < 1e-14 {
            out.push(x);
        }
    }
    for pair in points.windows(2) {
        let (mut a, mut b) = (pair[0], pair[1]);
        let fa = eval(p, a);
        if fa * eval(p, b) >= 0.0 {
            continue;
        }
        for _ in 0..64 {
            let m = (a + b) / 2.0;
            if eval(p, m) * fa > 0.0 {
                a = m;
            } else {
                b = m;
            }
        }
        out.push((a + b) / 2.0);
    }
    out.sort_by(f64::total_cmp);
    out.dedup_by(|a, b| (*a - *b).abs() < 1e-11);
    out
}
impl Radial {
    fn coefficients(self) -> [f64; 4] {
        let [a, b, c] = self.terms;
        match self.model {
            RadialModel::Poly3 => [1.0 - a, 0.0, a, 0.0],
            RadialModel::Poly5 => [1.0, 0.0, a, b],
            RadialModel::PtLens => [1.0 - a - b - c, c, b, a],
        }
    }
    fn g(self, r: f64) -> f64 {
        let [a, b, c] = self.terms;
        match self.model {
            RadialModel::Poly3 => 1.0 - a + a * r * r,
            RadialModel::Poly5 => 1.0 + a * r * r + b * r * r * r * r,
            RadialModel::PtLens => ((a * r + b) * r + c) * r + (1.0 - a - b - c),
        }
    }
    fn f(self, r: f64) -> f64 {
        r * self.g(r)
    }
    fn derivative(self, r: f64) -> f64 {
        let [a, b, c] = self.terms;
        match self.model {
            RadialModel::Poly3 => 1.0 - a + 3.0 * a * r * r,
            RadialModel::Poly5 => 1.0 + 3.0 * a * r * r + 5.0 * b * r * r * r * r,
            RadialModel::PtLens => ((4.0 * a * r + 3.0 * b) * r + 2.0 * c) * r + (1.0 - a - b - c),
        }
    }
    fn critical_g(self, max: f64) -> Vec<f64> {
        let [a, b, c] = self.terms;
        match self.model {
            RadialModel::Poly3 => Vec::new(),
            RadialModel::Poly5 => {
                let q = -a / (2.0 * b);
                if q > 0.0 && q.sqrt() <= max {
                    vec![q.sqrt()]
                } else {
                    Vec::new()
                }
            }
            RadialModel::PtLens => roots(&[c, 2.0 * b, 3.0 * a], 0.0, max),
        }
    }
    fn critical_derivative(self, max: f64) -> Vec<f64> {
        let [a, b, _] = self.terms;
        match self.model {
            RadialModel::Poly3 => Vec::new(),
            RadialModel::Poly5 => {
                let q = -3.0 * a / (10.0 * b);
                if q > 0.0 && q.sqrt() <= max {
                    vec![q.sqrt()]
                } else {
                    Vec::new()
                }
            }
            RadialModel::PtLens => {
                let q = self.coefficients();
                roots(&[2.0 * q[1], 6.0 * q[2], 12.0 * q[3]], 0.0, max)
            }
        }
    }
    fn folds(self, max: f64) -> Vec<f64> {
        let [a, b, _] = self.terms;
        match self.model {
            RadialModel::Poly3 => roots(&[1.0 - a, 0.0, 3.0 * a], 0.0, max),
            RadialModel::Poly5 => roots(&[1.0, 3.0 * a, 5.0 * b], 0.0, max * max)
                .into_iter()
                .map(f64::sqrt)
                .collect(),
            RadialModel::PtLens => {
                let q = self.coefficients();
                roots(&[q[0], 2.0 * q[1], 3.0 * q[2], 4.0 * q[3]], 0.0, max)
            }
        }
    }
    fn inverse(self, r: f64) -> Result<f64, MapError> {
        if !r.is_finite() || r < 0.0 || self.f(self.monotone_radius) < r {
            return Err(MapError::Outside);
        }
        if r == 0.0 {
            return Ok(0.0);
        }
        let (mut lo, mut hi) = (0.0, self.monotone_radius);
        let mut x = r.clamp(lo, hi);
        for _ in 0..INVERSE_MAX_ITERATIONS {
            let delta = self.f(x) - r;
            if delta.abs() * self.px_per_unit <= INVERSE_TOLERANCE_PX.min(1e-10) {
                return Ok(x);
            }
            if delta > 0.0 {
                hi = x;
            } else {
                lo = x;
            }
            let next = x - delta / self.derivative(x);
            x = if next > lo && next < hi {
                next
            } else {
                (lo + hi) / 2.0
            };
        }
        if (self.f(x) - r).abs() * self.px_per_unit <= INVERSE_TOLERANCE_PX {
            Ok(x)
        } else {
            Err(MapError::Unconverged)
        }
    }
}
impl WarpStep {
    pub fn radial(
        model: RadialModel,
        terms: [f64; 3],
        unit_scale: f64,
        stage: Stage,
    ) -> Result<Self, Error> {
        if stage.width == 0
            || stage.height == 0
            || !unit_scale.is_finite()
            || unit_scale <= f64::MIN_POSITIVE.sqrt()
            || !terms.iter().all(|v| v.is_finite())
        {
            return Err(Error::validation(
                "a lens mapping has non-finite terms or an empty stage",
            ));
        }
        let (w, h) = (f64::from(stage.width), f64::from(stage.height));
        let px = w.min(h) / (2.0 * unit_scale);
        let rmax = w.hypot(h) / (2.0 * px);
        let rmin = w.min(h) / (2.0 * px);
        let monotone_radius = 4.0 * rmax;
        if !px.is_finite()
            || px <= 0.0
            || !rmax.is_finite()
            || rmax <= 0.0
            || !rmin.is_finite()
            || rmin <= 0.0
            || !monotone_radius.is_finite()
        {
            return Err(Error::validation(
                "a lens mapping has degenerate normalization",
            ));
        }
        let mut r = Radial {
            model,
            terms,
            centre: (w / 2.0, h / 2.0),
            px_per_unit: px,
            cover: 1.0,
            monotone_radius,
            local_scale_max: 1.0,
        };
        if !r.f(rmax).is_finite()
            || !r.derivative(0.0).is_finite()
            || !r.derivative(rmax).is_finite()
            || r.derivative(0.0) <= 0.0
            || r.f(rmax) <= 0.0
            || r.folds(rmax).iter().any(|v| *v > 0.0)
        {
            return Err(Error::validation("a lens mapping folds within the canvas"));
        }
        if let Some(fold) = r.folds(4.0 * rmax).into_iter().find(|v| *v > 0.0) {
            r.monotone_radius = fold;
        }
        // A barrel mapping can cover the canvas while outer input points lie beyond its
        // monotone forward domain. The upper endpoint then remains rmax; no inversion is
        // needed for those already-covered boundary rays.
        let lower = match r.inverse(rmin) {
            Ok(v) => v,
            Err(MapError::Outside) => f64::INFINITY,
            Err(e) => return Err(e.error()),
        };
        let upper = match r.inverse(rmax) {
            Ok(v) => rmax.min(v),
            Err(MapError::Outside) => rmax,
            Err(e) => return Err(e.error()),
        };
        let mut cover = 1.0_f64;
        if lower <= upper {
            for t in std::iter::once(lower)
                .chain(std::iter::once(upper))
                .chain(r.critical_g(upper).into_iter().filter(|v| *v >= lower))
            {
                cover = cover.max(r.g(t));
            }
        }
        r.cover = cover * (1.0 + COVER_SAFETY);
        let end = rmax / r.cover;
        r.local_scale_max = std::iter::once(0.0)
            .chain(std::iter::once(end))
            .chain(r.critical_g(end))
            .chain(r.critical_derivative(end))
            .map(|t| r.g(t).max(r.derivative(t)) / r.cover)
            .fold(0.0_f64, f64::max);
        let step = Self::Radial(r);
        WarpChain::new(step).validate_limits()?;
        Ok(step)
    }
    pub fn projective(horizontal: i64, vertical: i64, stage: Stage) -> Result<Self, Error> {
        if !(-100..=100).contains(&horizontal)
            || !(-100..=100).contains(&vertical)
            || stage.width == 0
            || stage.height == 0
        {
            return Err(Error::validation(
                "perspective needs values from -100 to 100 and a non-empty stage",
            ));
        }
        let (w, h) = (f64::from(stage.width), f64::from(stage.height));
        let unit = w.max(h) / 2.0;
        let (nh, nv) = (horizontal as f64 / 400.0, vertical as f64 / 400.0);
        let cover =
            (1.0 + nh.abs() * w / w.max(h) + nv.abs() * h / w.max(h)) * (1.0 + COVER_SAFETY);
        let wx = w / (2.0 * unit * cover);
        let wy = h / (2.0 * unit * cover);
        let d = 1.0 - nh.abs() * wx - nv.abs() * wy;
        let local = (d + wx.hypot(wy) * nh.hypot(nv)) / (d * d * cover);
        let step = Self::Projective(Projective {
            h: nh,
            v: nv,
            centre: (w / 2.0, h / 2.0),
            unit,
            cover,
            local_scale_max: local,
        });
        WarpChain::new(step).validate_limits()?;
        Ok(step)
    }
    /// Module declarations and wire maps must agree with the host's derived geometry. The
    /// constructors check limits directly, so reconstructing here cannot recurse through this
    /// validation. Centre coordinates encode the fixed canvas that produced each nonlinear step.
    pub(crate) fn validate(self) -> Result<(), Error> {
        let invalid = || Error::validation("a warp declares invalid derived geometry");
        let same = |a: f64, b: f64| {
            a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-12 * a.abs().max(b.abs())
        };
        let stage = |centre: (f64, f64)| -> Result<Stage, Error> {
            let (w, h) = (centre.0 * 2.0, centre.1 * 2.0);
            if !w.is_finite()
                || !h.is_finite()
                || w < 1.0
                || h < 1.0
                || w > u32::MAX as f64
                || h > u32::MAX as f64
                || w.fract() != 0.0
                || h.fract() != 0.0
            {
                return Err(invalid());
            }
            Ok(Stage {
                width: w as u32,
                height: h as u32,
            })
        };
        match self {
            Self::Affine(m) => {
                if !m.iter().all(|v| v.is_finite()) {
                    return Err(invalid());
                }
                Affine(m).invert()?;
            }
            Self::Projective(p) => {
                if !p.h.is_finite()
                    || !p.v.is_finite()
                    || !p.unit.is_finite()
                    || p.unit <= 0.0
                    || !p.cover.is_finite()
                    || p.cover < 1.0
                    || !p.local_scale_max.is_finite()
                    || p.local_scale_max <= 0.0
                {
                    return Err(invalid());
                }
                let (h, v) = ((p.h * 400.0).round() as i64, (p.v * 400.0).round() as i64);
                let Self::Projective(expected) = Self::projective(h, v, stage(p.centre)?)? else {
                    unreachable!()
                };
                if !same(p.h, expected.h)
                    || !same(p.v, expected.v)
                    || !same(p.unit, expected.unit)
                    || !same(p.cover, expected.cover)
                    || !same(p.local_scale_max, expected.local_scale_max)
                {
                    return Err(invalid());
                }
            }
            Self::Radial(r) => {
                if !r.px_per_unit.is_finite()
                    || r.px_per_unit <= 0.0
                    || !r.cover.is_finite()
                    || r.cover < 1.0
                    || !r.monotone_radius.is_finite()
                    || r.monotone_radius <= 0.0
                    || !r.local_scale_max.is_finite()
                    || r.local_scale_max <= 0.0
                {
                    return Err(invalid());
                }
                let stage = stage(r.centre)?;
                let unit_scale = f64::from(stage.width.min(stage.height)) / (2.0 * r.px_per_unit);
                let Self::Radial(expected) = Self::radial(r.model, r.terms, unit_scale, stage)?
                else {
                    unreachable!()
                };
                if !same(r.px_per_unit, expected.px_per_unit)
                    || !same(r.cover, expected.cover)
                    || !same(r.monotone_radius, expected.monotone_radius)
                    || !same(r.local_scale_max, expected.local_scale_max)
                {
                    return Err(invalid());
                }
            }
        }
        WarpChain::new(self).validate_limits()
    }
    pub(crate) fn validate_for(self, stage: Stage) -> Result<(), Error> {
        self.validate()?;
        let centre = match self {
            Self::Projective(p) => Some(p.centre),
            Self::Radial(r) => Some(r.centre),
            Self::Affine(_) => None,
        };
        if centre
            .is_some_and(|p| p != (f64::from(stage.width) / 2.0, f64::from(stage.height) / 2.0))
        {
            return Err(Error::validation(
                "a warp's canvas differs from its input stage",
            ));
        }
        Ok(())
    }
    fn jacobian(self, x: f64, y: f64) -> [f64; 4] {
        match self {
            Self::Affine(m) => [m[0], m[1], m[3], m[4]],
            Self::Projective(p) => {
                let u = (x - p.centre.0) / (p.unit * p.cover);
                let v = (y - p.centre.1) / (p.unit * p.cover);
                let d = 1.0 - p.h * u - p.v * v;
                let k = 1.0 / (p.cover * d * d);
                [
                    (d + u * p.h) * k,
                    u * p.v * k,
                    v * p.h * k,
                    (d + v * p.v) * k,
                ]
            }
            Self::Radial(r) => {
                let u = (x - r.centre.0) / (r.px_per_unit * r.cover);
                let v = (y - r.centre.1) / (r.px_per_unit * r.cover);
                let radius = u.hypot(v);
                let g = r.g(radius);
                let slope = if radius == 0.0 {
                    0.0
                } else {
                    (r.derivative(radius) - g) / (radius * radius)
                };
                [
                    (g + slope * u * u) / r.cover,
                    slope * u * v / r.cover,
                    slope * u * v / r.cover,
                    (g + slope * v * v) / r.cover,
                ]
            }
        }
    }
    pub fn is_identity(self) -> bool {
        match self {
            Self::Affine(m) => m == Affine::IDENTITY.0,
            Self::Radial(r) => r.terms == [0.0; 3],
            Self::Projective(p) => p.h == 0.0 && p.v == 0.0,
        }
    }
    pub fn cover(self) -> f64 {
        match self {
            Self::Affine(_) => 1.0,
            Self::Radial(r) => r.cover,
            Self::Projective(p) => p.cover,
        }
    }
    pub fn local_scale_max(self) -> f64 {
        match self {
            Self::Affine(m) => singular(m[0], m[1], m[3], m[4]),
            Self::Radial(r) => r.local_scale_max,
            Self::Projective(p) => p.local_scale_max,
        }
    }
    #[inline]
    fn input_at(self, x: f64, y: f64) -> (f64, f64) {
        match self {
            Self::Affine(m) => Affine(m).at(x, y),
            Self::Radial(r) => {
                let u = (x - r.centre.0) / (r.px_per_unit * r.cover);
                let v = (y - r.centre.1) / (r.px_per_unit * r.cover);
                let g = r.g(u.hypot(v));
                (
                    r.centre.0 + r.px_per_unit * u * g,
                    r.centre.1 + r.px_per_unit * v * g,
                )
            }
            Self::Projective(p) => {
                let u = (x - p.centre.0) / (p.unit * p.cover);
                let v = (y - p.centre.1) / (p.unit * p.cover);
                let d = 1.0 - p.h * u - p.v * v;
                (p.centre.0 + p.unit * u / d, p.centre.1 + p.unit * v / d)
            }
        }
    }
    fn output_at(self, x: f64, y: f64) -> Result<(f64, f64), MapError> {
        let point = match self {
            Self::Affine(m) => Affine(m)
                .invert()
                .map_err(|_| MapError::Unconverged)?
                .at(x, y),
            Self::Radial(r) => {
                let u = (x - r.centre.0) / r.px_per_unit;
                let v = (y - r.centre.1) / r.px_per_unit;
                let radius = u.hypot(v);
                if radius == 0.0 {
                    return Ok(r.centre);
                }
                let scale = r.inverse(radius)? / radius * r.cover;
                (
                    r.centre.0 + r.px_per_unit * u * scale,
                    r.centre.1 + r.px_per_unit * v * scale,
                )
            }
            Self::Projective(p) => {
                let u = (x - p.centre.0) / p.unit;
                let v = (y - p.centre.1) / p.unit;
                let d = 1.0 + p.h * u + p.v * v;
                if d <= 0.0 {
                    return Err(MapError::Outside);
                }
                (
                    p.centre.0 + p.unit * p.cover * u / d,
                    p.centre.1 + p.unit * p.cover * v / d,
                )
            }
        };
        if point.0.is_finite() && point.1.is_finite() {
            Ok(point)
        } else {
            Err(MapError::Outside)
        }
    }
}
fn singular(a: f64, b: f64, c: f64, d: f64) -> f64 {
    let sum = a * a + b * b + c * c + d * d;
    let det = a * d - b * c;
    ((sum + (sum * sum - 4.0 * det * det).max(0.0).sqrt()) / 2.0).sqrt()
}
fn bounds_of(
    step: WarpStep,
    (x0, y0, x1, y1): (f64, f64, f64, f64),
) -> Option<(f64, f64, f64, f64)> {
    let mut points = vec![(x0, y0), (x0, y1), (x1, y0), (x1, y1)];
    if let WarpStep::Radial(r) = step {
        let scale = r.px_per_unit * r.cover;
        let max = ((x0 - r.centre.0).abs().max((x1 - r.centre.0).abs()))
            .hypot((y0 - r.centre.1).abs().max((y1 - r.centre.1).abs()))
            / scale;
        let radii = r.critical_g(max);
        for x in [x0, x1] {
            if y0 <= r.centre.1 && y1 >= r.centre.1 {
                points.push((x, r.centre.1));
            }
            for radius in &radii {
                let q = (radius * scale).powi(2) - (x - r.centre.0).powi(2);
                if q >= 0.0 {
                    for y in [r.centre.1 - q.sqrt(), r.centre.1 + q.sqrt()] {
                        if y >= y0 && y <= y1 {
                            points.push((x, y));
                        }
                    }
                }
            }
        }
        for y in [y0, y1] {
            if x0 <= r.centre.0 && x1 >= r.centre.0 {
                points.push((r.centre.0, y));
            }
            for radius in &radii {
                let q = (radius * scale).powi(2) - (y - r.centre.1).powi(2);
                if q >= 0.0 {
                    for x in [r.centre.0 - q.sqrt(), r.centre.0 + q.sqrt()] {
                        if x >= x0 && x <= x1 {
                            points.push((x, y));
                        }
                    }
                }
            }
        }
    }
    let (mut lowx, mut lowy, mut highx, mut highy) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for (x, y) in points {
        let (u, v) = step.input_at(x, y);
        if !u.is_finite() || !v.is_finite() {
            return None;
        }
        lowx = lowx.min(u);
        lowy = lowy.min(v);
        highx = highx.max(u);
        highy = highy.max(v);
    }
    Some((lowx, lowy, highx, highy))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageSize {
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cover {
    pub lens: f64,
    pub perspective: f64,
    pub combined: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MappingShape {
    Affine {
        forward: [f64; 6],
        inverse: [f64; 6],
    },
    Warp {
        steps: Vec<WarpStep>,
        domain: StageSize,
        cover: Cover,
        local_scale_max: f64,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GeometryMap {
    pub content: StageSize,
    pub output: StageSize,
    pub mapping: MappingShape,
    pub mapping_sha256: String,
}
impl<'de> Deserialize<'de> for GeometryMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            content: StageSize,
            output: StageSize,
            mapping: MappingShape,
            mapping_sha256: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        let map = Self {
            content: wire.content,
            output: wire.output,
            mapping: wire.mapping,
            mapping_sha256: wire.mapping_sha256,
        };
        map.validate()
            .map_err(|error| de::Error::custom(error.detail))?;
        Ok(map)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MappingDescriptor {
    pub entry_id: crate::EntryId,
    pub snapshot_id: crate::SnapshotId,
    pub source_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<crate::DraftStamp>,
    #[serde(flatten)]
    pub geometry: GeometryMap,
}
impl std::ops::Deref for MappingDescriptor {
    type Target = GeometryMap;
    fn deref(&self) -> &GeometryMap {
        &self.geometry
    }
}
impl GeometryMap {
    fn validate(&self) -> Result<(), Error> {
        let invalid = || Error::validation("invalid geometry mapping descriptor");
        if self.content.width == 0
            || self.content.height == 0
            || self.output.width == 0
            || self.output.height == 0
        {
            return Err(invalid());
        }
        let same = |a: f64, b: f64| {
            a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
        };
        match &self.mapping {
            MappingShape::Affine { forward, inverse } => {
                if !forward.iter().chain(inverse).all(|v| v.is_finite()) {
                    return Err(invalid());
                }
                let expected = Affine(*forward).invert()?;
                if !inverse.iter().zip(expected.0).all(|(a, b)| same(*a, b)) {
                    return Err(invalid());
                }
            }
            MappingShape::Warp {
                steps,
                domain,
                cover,
                local_scale_max,
            } => {
                if *domain != self.output || steps.is_empty() {
                    return Err(invalid());
                }
                let same = |a: f64, b: f64| {
                    a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-12 * a.abs().max(b.abs())
                };
                let mut lens = 1.0;
                let mut perspective = 1.0;
                let mut local = 1.0;
                for step in steps {
                    step.validate()?;
                    match step {
                        WarpStep::Radial(r) => lens *= r.cover,
                        WarpStep::Projective(p) => perspective *= p.cover,
                        _ => {}
                    }
                    local *= step.local_scale_max();
                }
                if lens * perspective > MAX_COVER_SCALE
                    || local > MAX_LOCAL_MINIFICATION
                    || !same(cover.lens, lens)
                    || !same(cover.perspective, perspective)
                    || !same(cover.combined, lens * perspective)
                    || !same(*local_scale_max, local)
                {
                    return Err(invalid());
                }
            }
        }
        let expected = Self::new(self.content, self.output, self.mapping.clone());
        if self.mapping_sha256 != expected.mapping_sha256 {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn affine(
        content: StageSize,
        output: StageSize,
        forward: [f64; 6],
        inverse: [f64; 6],
    ) -> Self {
        Self::new(content, output, MappingShape::Affine { forward, inverse })
    }
    fn new(content: StageSize, output: StageSize, mapping: MappingShape) -> Self {
        let bytes =
            serde_json::to_vec(&(content, output, &mapping)).expect("finite compiled geometry");
        let mapping_sha256 = format!("{:x}", Sha256::digest(bytes));
        Self {
            content,
            output,
            mapping,
            mapping_sha256,
        }
    }
    pub(crate) fn from_steps(
        content: StageSize,
        output: StageSize,
        steps: Vec<WarpStep>,
    ) -> Result<Self, Error> {
        if steps.iter().all(|s| matches!(s, WarpStep::Affine(_))) {
            let mut forward = Affine::IDENTITY;
            for step in steps {
                let WarpStep::Affine(inverse) = step else {
                    unreachable!()
                };
                forward = forward.then(Affine(inverse).invert()?);
            }
            return Ok(Self::affine(
                content,
                output,
                forward.0,
                forward.invert()?.0,
            ));
        }
        let mut cover = Cover {
            lens: 1.0,
            perspective: 1.0,
            combined: 1.0,
        };
        let mut local = 1.0;
        for step in &steps {
            match step {
                WarpStep::Radial(r) => cover.lens *= r.cover,
                WarpStep::Projective(p) => cover.perspective *= p.cover,
                _ => {}
            }
            local *= step.local_scale_max();
        }
        cover.combined = cover.lens * cover.perspective;
        Ok(Self::new(
            content,
            output,
            MappingShape::Warp {
                steps,
                domain: output,
                cover,
                local_scale_max: local,
            },
        ))
    }
    pub fn sha256(&self) -> &str {
        &self.mapping_sha256
    }
    pub fn to_content(&self, mut x: f64, mut y: f64) -> Result<(f64, f64), MapError> {
        match &self.mapping {
            MappingShape::Affine { inverse, .. } => Ok(Affine(*inverse).at(x, y)),
            MappingShape::Warp { steps, .. } => {
                if x < 0.0
                    || y < 0.0
                    || x > f64::from(self.output.width)
                    || y > f64::from(self.output.height)
                {
                    return Err(MapError::Outside);
                }
                for step in steps.iter().rev() {
                    (x, y) = step.input_at(x, y);
                }
                if x.is_finite() && y.is_finite() {
                    Ok((x, y))
                } else {
                    Err(MapError::Outside)
                }
            }
        }
    }
    /// [`Self::to_content`] without its domain check, by the same steps: the content coordinate
    /// any output coordinate maps to, which may be non-finite far outside the output stage. What a
    /// GPU coordinate grid's nodes read, the last of which may lie a cell past the output's edge so
    /// that every pixel centre falls inside a cell.
    pub(crate) fn content_at(&self, mut x: f64, mut y: f64) -> (f64, f64) {
        match &self.mapping {
            MappingShape::Affine { inverse, .. } => Affine(*inverse).at(x, y),
            MappingShape::Warp { steps, .. } => {
                for step in steps.iter().rev() {
                    (x, y) = step.input_at(x, y);
                }
                (x, y)
            }
        }
    }
    pub fn to_output(&self, mut x: f64, mut y: f64) -> Result<(f64, f64), MapError> {
        match &self.mapping {
            MappingShape::Affine { forward, .. } => Ok(Affine(*forward).at(x, y)),
            MappingShape::Warp { steps, .. } => {
                for step in steps {
                    (x, y) = step.output_at(x, y)?;
                }
                if x < 0.0
                    || y < 0.0
                    || x > f64::from(self.output.width)
                    || y > f64::from(self.output.height)
                {
                    return Err(MapError::Outside);
                }
                Ok((x, y))
            }
        }
    }
    pub fn local_scale_at(&self, mut x: f64, mut y: f64) -> Result<f64, MapError> {
        match &self.mapping {
            MappingShape::Affine { inverse, .. } => {
                Ok(singular(inverse[0], inverse[1], inverse[3], inverse[4]))
            }
            MappingShape::Warp { steps, .. } => {
                self.to_content(x, y)?;
                let mut j = [1.0, 0.0, 0.0, 1.0];
                for step in steps.iter().rev() {
                    let k = step.jacobian(x, y);
                    j = [
                        k[0] * j[0] + k[1] * j[2],
                        k[0] * j[1] + k[1] * j[3],
                        k[2] * j[0] + k[3] * j[2],
                        k[2] * j[1] + k[3] * j[3],
                    ];
                    (x, y) = step.input_at(x, y);
                }
                Ok(singular(j[0], j[1], j[2], j[3]))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../../fixtures/geometry/lens-perspective.json"
        ))
        .unwrap()
    }
    fn close(got: f64, want: f64, tolerance: f64) {
        assert!(
            (got - want).abs() <= tolerance,
            "{got:.16} != {want:.16}, difference {}",
            (got - want).abs()
        );
    }
    fn profile(row: &Value) -> WarpStep {
        let model = match row["model"].as_str().unwrap() {
            "Poly3" => RadialModel::Poly3,
            "Poly5" => RadialModel::Poly5,
            "PtLens" => RadialModel::PtLens,
            _ => panic!("model"),
        };
        WarpStep::radial(
            model,
            serde_json::from_value(row["terms"].clone()).unwrap(),
            row["unit_scale"].as_f64().unwrap(),
            Stage {
                width: 6048,
                height: 4024,
            },
        )
        .unwrap()
    }
    #[test]
    fn inverse_unconverged_is_explicit() {
        // At this scale no representable root can meet the pixel tolerance for some targets.
        // The bounded solver must report that fact, rather than returning its last estimate.
        let r = Radial {
            model: RadialModel::Poly3,
            terms: [0.1, 0.0, 0.0],
            centre: (0.0, 0.0),
            px_per_unit: 1e25,
            cover: 1.0,
            monotone_radius: 100.0,
            local_scale_max: 1.0,
        };
        assert!((1..=256).any(|i| r.inverse(2.0 + i as f64 / 257.0) == Err(MapError::Unconverged)));
    }

    #[test]
    fn degenerate_normalization_and_derived_overflow_are_refused() {
        let stage = Stage {
            width: 6000,
            height: 4000,
        };
        for unit in [f64::MIN_POSITIVE * 0.01, f64::MAX, 1e300] {
            assert!(WarpStep::radial(RadialModel::Poly5, [0.1, 0.01, 0.0], unit, stage).is_err());
        }
        assert!(
            WarpStep::radial(RadialModel::Poly5, [f64::MAX, f64::MAX, 0.0], 1.0, stage).is_err()
        );
        assert!(WarpStep::radial(RadialModel::Poly3, [0.1, 0.0, 0.0], 1e-300, stage).is_err());
    }

    #[test]
    fn mapping_hash_changes_with_crop_angle_and_not_colour() {
        let r = crate::ModuleRegistry::builtin();
        let mut p = crate::Recipe {
            layers: vec![crate::Layer::crop(crate::render::tests::fitted_crop(
                480,
                320,
                4.0,
                [0.1, 0.1, 0.7, 0.7],
            ))],
            ..crate::Recipe::default()
        };
        let first = crate::stage_transform(&r, 480, 320, &p).unwrap();
        p.layers[0] = crate::Layer::crop(crate::render::tests::fitted_crop(
            480,
            320,
            5.0,
            [0.1, 0.1, 0.7, 0.7],
        ));
        let angle = crate::stage_transform(&r, 480, 320, &p).unwrap();
        assert_ne!(first.sha256(), angle.sha256());
        p.layers.insert(
            0,
            crate::Layer {
                id: crate::LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload: serde_json::json!({"exposure":1.0}),
                mask: None,
                artifacts: Vec::new(),
            },
        );
        assert_eq!(
            angle.sha256(),
            crate::stage_transform(&r, 480, 320, &p).unwrap().sha256()
        );
    }

    #[test]
    fn warp_mapping_matches_reference_fixture() {
        let f = fixture();
        for row in f["mappings"].as_array().unwrap() {
            let step = profile(row);
            let p = step.input_at(1200.5, 3400.5);
            close(step.cover(), row["cover"].as_f64().unwrap(), 2e-11);
            close(
                step.local_scale_max(),
                row["local_scale_max"].as_f64().unwrap(),
                2e-11,
            );
            close(p.0, row["point"][0].as_f64().unwrap(), 1e-9);
            close(p.1, row["point"][1].as_f64().unwrap(), 1e-9);
        }
        for row in f["perspective"].as_array().unwrap() {
            let step = WarpStep::projective(
                100,
                100,
                Stage {
                    width: row["width"].as_u64().unwrap() as u32,
                    height: row["height"].as_u64().unwrap() as u32,
                },
            )
            .unwrap();
            close(step.cover(), row["cover"].as_f64().unwrap(), 2e-11);
            close(
                step.local_scale_max(),
                row["local_scale_max"].as_f64().unwrap(),
                2e-11,
            );
        }
    }
    #[test]
    fn warp_reads_bound_never_underestimates_dense_reference() {
        let mut state = 0x31567u64;
        let mut random = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        for row in fixture()["mappings"].as_array().unwrap() {
            let step = profile(row);
            let reference_model = match row["model"].as_str().unwrap() {
                "Poly3" => luxforge_reference::geometry::Model::Poly3,
                "Poly5" => luxforge_reference::geometry::Model::Poly5,
                _ => luxforge_reference::geometry::Model::PtLens,
            };
            let terms = serde_json::from_value(row["terms"].clone()).unwrap();
            let ns = row["normalization"].as_f64().unwrap();
            let cover = row["cover"].as_f64().unwrap();
            for _ in 0..3000 {
                let x0 = random() * 5900.0;
                let y0 = random() * 3900.0;
                let x1 = (x0 + random() * 2200.0).min(6048.0);
                let y1 = (y0 + random() * 2000.0).min(4024.0);
                let (lowx, lowy, highx, highy) = bounds_of(step, (x0, y0, x1, y1)).unwrap();
                for j in 0..=10 {
                    for i in 0..=10 {
                        let [u, v] = luxforge_reference::geometry::radial_input(
                            [
                                x0 + (x1 - x0) * i as f64 / 10.0,
                                y0 + (y1 - y0) * j as f64 / 10.0,
                            ],
                            [3024.0, 2012.0],
                            ns,
                            reference_model,
                            terms,
                            cover,
                        );
                        assert!(
                            u >= lowx - 1e-9
                                && u <= highx + 1e-9
                                && v >= lowy - 1e-9
                                && v <= highy + 1e-9,
                            "{}: ({u},{v}) outside ({lowx},{lowy},{highx},{highy})",
                            row["name"]
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn geometry_map_round_trips_affine_tail() {
        let angle = 7f64.to_radians();
        let (s, c) = angle.sin_cos();
        let forward = Affine([c, -s, 34.0, s, c, -28.0]);
        let map = GeometryMap::affine(
            StageSize {
                width: 6048,
                height: 4024,
            },
            StageSize {
                width: 5000,
                height: 3000,
            },
            forward.0,
            forward.invert().unwrap().0,
        );
        for i in 0..10_000 {
            let (x, y) = (
                (i * 2531 % 6048) as f64 + 0.25,
                (i * 1573 % 4024) as f64 + 0.75,
            );
            let (u, v) = map.to_output(x, y).unwrap();
            let (a, b) = map.to_content(u, v).unwrap();
            close(x, a, 1e-9);
            close(y, b, 1e-9);
        }
    }
    #[test]
    fn lens_forward_reports_outside_beyond_visible_radius() {
        let step = WarpStep::radial(
            RadialModel::Poly3,
            [-0.079, 0.0, 0.0],
            1.0,
            Stage {
                width: 6000,
                height: 4000,
            },
        )
        .unwrap();
        assert_eq!(step.output_at(1e8, 1e8), Err(MapError::Outside));
    }
    #[test]
    fn folded_radial_step_is_refused() {
        assert!(
            WarpStep::radial(
                RadialModel::Poly3,
                [-1.0, 0.0, 0.0],
                1.0,
                Stage {
                    width: 6000,
                    height: 4000
                }
            )
            .is_err()
        );
    }
    #[test]
    fn strong_minification_is_refused() {
        let result = WarpStep::radial(
            RadialModel::Poly5,
            [1.0, 0.0, 0.0],
            1.0,
            Stage {
                width: 6000,
                height: 4000,
            },
        );
        let error = result.unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::UnsupportedInput);
        assert!(error.detail.contains("strong minification"));
    }
    #[test]
    fn cover_scale_above_four_is_refused() {
        let mut chain = WarpChain::new(WarpStep::Projective(Projective {
            h: 0.1,
            v: 0.0,
            centre: (1.0, 1.0),
            unit: 1.0,
            cover: 3.0,
            local_scale_max: 0.5,
        }));
        let e = chain
            .prepend(WarpStep::Projective(Projective {
                h: 0.1,
                v: 0.0,
                centre: (1.0, 1.0),
                unit: 1.0,
                cover: 2.0,
                local_scale_max: 0.5,
            }))
            .unwrap_err();
        assert_eq!(e.kind, crate::ErrorKind::Validation);
    }
    #[test]
    fn warp_map_hash_changes_with_lens_terms() {
        let size = StageSize {
            width: 6000,
            height: 4000,
        };
        let one = WarpStep::radial(
            RadialModel::Poly3,
            [-0.07, 0.0, 0.0],
            1.0,
            Stage {
                width: 6000,
                height: 4000,
            },
        )
        .unwrap();
        let two = WarpStep::radial(
            RadialModel::Poly3,
            [-0.06, 0.0, 0.0],
            1.0,
            Stage {
                width: 6000,
                height: 4000,
            },
        )
        .unwrap();
        let a = GeometryMap::from_steps(size, size, vec![one]).unwrap();
        let b = GeometryMap::from_steps(size, size, vec![two]).unwrap();
        assert_ne!(a.sha256(), b.sha256());
        assert_eq!(a.sha256().len(), 64);
    }
    #[test]
    fn local_scale_at_output_edge_stays_nonzero() {
        let size = StageSize {
            width: 6000,
            height: 4000,
        };
        let step = WarpStep::projective(
            100,
            100,
            Stage {
                width: 6000,
                height: 4000,
            },
        )
        .unwrap();
        let map = GeometryMap::from_steps(size, size, vec![step]).unwrap();
        assert!(map.local_scale_at(6000.0, 4000.0).unwrap() > 0.5);
    }
}
