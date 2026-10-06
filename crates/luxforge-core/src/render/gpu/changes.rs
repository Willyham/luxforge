//! Where one of a gesture's plans draws other values than an earlier one, so the surface can
//! evaluate only that part again (`docs/design/gpu-preview.md`, "Incremental ticks").
//!
//! Two plans of one gesture usually differ in one place: a painted tick appends segments to its
//! stroke, which changes its mask's coverage only near them, and a shape moved, a mask's amount or
//! a component changed changes coverage only inside the mask's bounds before and after, outside
//! which each is exactly zero. Every other difference — a unit's words, a mask's placement on its
//! stage, the boundary, the geometry tail, any operation after the tail — counts as a change
//! anywhere. The rectangle is in the pixels of the plan's boundary stage, before any spatial
//! operation's neighbourhood grows it: the surface grows it by each spatial operation's units'
//! reach as it carries it through the chain.
use super::{GpuMask, GpuOperation, GpuPlan, GpuPosition, GpuSpatial};
use crate::modules::Region;

/// Where a plan may draw other values than an earlier plan of the same gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuChange {
    /// The same values everywhere.
    Nothing,
    /// Other values only inside this rectangle of the boundary's stage, before any spatial
    /// operation's neighbourhood grows it.
    Inside(Region),
    /// Other values anywhere.
    Anywhere,
}

/// A rectangle of pixels, `[x0, y0, x1, y1)`, signed while it is carried between stages.
type Span = [i64; 4];

fn union(a: Option<Span>, b: Span) -> Span {
    match a {
        None => b,
        Some(a) => [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ],
    }
}

/// The pass pixels `position` takes into `span` of the mask's stage: its linear part is a signed
/// permutation, so the preimage of a rectangle is the rectangle of its corners' preimages.
fn back(position: GpuPosition, [x0, y0, x1, y1]: Span) -> Span {
    let GpuPosition { a, b, tx, c, d, ty } = position;
    // The inverse of a signed permutation is its transpose.
    let point = |qx: i64, qy: i64| {
        let (u, v) = (qx - tx, qy - ty);
        (a * u + c * v, b * u + d * v)
    };
    let corners = [
        point(x0, y0),
        point(x1 - 1, y0),
        point(x0, y1 - 1),
        point(x1 - 1, y1 - 1),
    ];
    let low = |axis: fn(&(i64, i64)) -> i64| corners.iter().map(axis).min().unwrap_or(0);
    let high = |axis: fn(&(i64, i64)) -> i64| corners.iter().map(axis).max().unwrap_or(0) + 1;
    [
        low(|corner| corner.0),
        low(|corner| corner.1),
        high(|corner| corner.0),
        high(|corner| corner.1),
    ]
}

/// Where `new`'s coverage may differ from `old`'s, in the pixels of the pass the masked operation
/// runs in: `Some(None)` for nowhere and `None` for anywhere. Over one stage and position map each
/// mask's coverage is zero outside its bounds, so they differ only inside the two bounds together —
/// a shape moved, a component added or inverted, the amount — and a brush component whose segments
/// alone changed only near them.
fn mask_change(old: &GpuMask, new: &GpuMask) -> Option<Option<Span>> {
    if old.position != new.position || old.supersample != new.supersample {
        return None;
    }
    let bounded = || {
        let span = |region: &Region| {
            [
                i64::from(region.x0),
                i64::from(region.y0),
                i64::from(region.x0) + i64::from(region.width),
                i64::from(region.y0) + i64::from(region.height),
            ]
        };
        let spans = [&old.bounds, &new.bounds]
            .into_iter()
            .filter(|region| region.width > 0 && region.height > 0)
            .map(span)
            .fold(None, |union: Option<Span>, next| {
                Some(self::union(union, next))
            });
        Some(spans.map(|span| back(new.position, span)))
    };
    if old.invert != new.invert
        || old.scale.to_bits() != new.scale.to_bits()
        || old.components.len() != new.components.len()
    {
        return bounded();
    }
    let mut changed: Option<[f64; 4]> = None;
    for (old, new) in old.components.iter().zip(&new.components) {
        if old.kind != new.kind
            || old.mode != new.mode
            || old.invert != new.invert
            || !std::ptr::eq(old.program.program, new.program.program)
        {
            return bounded();
        }
        match crate::mask::component_gpu_changed(old.kind, &old.program, &new.program) {
            None => return bounded(),
            Some(Some([x0, y0, x1, y1])) => {
                changed = Some(match changed {
                    None => [x0, y0, x1, y1],
                    Some([a, b, c, d]) => [a.min(x0), b.min(y0), c.max(x1), d.max(y1)],
                });
            }
            Some(None) => {}
        }
    }
    let Some(rect) = changed else {
        return Some(None);
    };
    // Under the thin-feature supersample the components address the doubled stage, whose pixel
    // `2q + (i, j)` is the mask stage's `q`.
    let scale = if new.supersample { 2.0 } else { 1.0 };
    let span = [
        (rect[0] / scale).floor() as i64 - 1,
        (rect[1] / scale).floor() as i64 - 1,
        (rect[2] / scale).ceil() as i64 + 2,
        (rect[3] / scale).ceil() as i64 + 2,
    ];
    Some(Some(back(new.position, span)))
}

/// Where `new` may draw other values than `old`, one colour operation of the same layer.
fn operation_change(old: &GpuOperation, new: &GpuOperation) -> Option<Option<Span>> {
    if old.layer != new.layer || old.position != new.position || old.units != new.units {
        return None;
    }
    match (&old.mask, &new.mask) {
        (None, None) => Some(None),
        (Some(old), Some(new)) => mask_change(old, new),
        _ => None,
    }
}

/// Where `new` may draw other values than `old`, one spatial operation of the same layer, before
/// its neighbourhood grows its input's changes: its own mask's.
fn spatial_change(old: &GpuSpatial, new: &GpuSpatial) -> Option<Option<Span>> {
    if old.layer != new.layer
        || !std::ptr::eq(old.program, new.program)
        || old.words != new.words
        || old.planes != new.planes
        || old.passes != new.passes
        || old.applies != new.applies
        || old.clamps != new.clamps
        || old.light != new.light
        || old.halos != new.halos
        || old.after.len() != new.after.len()
    {
        return None;
    }
    let mut span = match (&old.mask, &new.mask) {
        (None, None) => None,
        (Some(old), Some(new)) => mask_change(old, new)?,
        _ => return None,
    };
    for (old, new) in old.after.iter().zip(&new.after) {
        if let Some(changed) = operation_change(old, new)? {
            span = Some(union(span, changed));
        }
    }
    Some(span)
}

impl GpuPlan {
    /// Where this plan may draw other values than `previous`, an earlier plan of the same gesture
    /// over the same boundary: nothing, a rectangle of the boundary's stage — a mask's coverage
    /// that changed only there — or anywhere. Compares descriptions only: `O(units + components +
    /// segments)`, no pixel read.
    pub fn changes_since(&self, previous: &GpuPlan) -> GpuChange {
        if self.boundary != previous.boundary
            || self.linear != previous.linear
            || self.geometry != previous.geometry
            || self.clipping != previous.clipping
            || self.content.len() != previous.content.len()
            || self.spatial.len() != previous.spatial.len()
            || self.output != previous.output
        {
            return GpuChange::Anywhere;
        }
        let mut span: Option<Span> = None;
        for (old, new) in previous.content.iter().zip(&self.content) {
            match operation_change(old, new) {
                None => return GpuChange::Anywhere,
                Some(Some(changed)) => span = Some(union(span, changed)),
                Some(None) => {}
            }
        }
        for (old, new) in previous.spatial.iter().zip(&self.spatial) {
            match spatial_change(old, new) {
                None => return GpuChange::Anywhere,
                Some(Some(changed)) => span = Some(union(span, changed)),
                Some(None) => {}
            }
        }
        let Some([x0, y0, x1, y1]) = span else {
            return GpuChange::Nothing;
        };
        let stage = self.boundary.stage;
        let clamp = |value: i64, limit: u32| value.clamp(0, i64::from(limit)) as u32;
        let (x0, x1) = (clamp(x0, stage.width), clamp(x1, stage.width));
        let (y0, y1) = (clamp(y0, stage.height), clamp(y1, stage.height));
        if x0 >= x1 || y0 >= y1 {
            return GpuChange::Nothing;
        }
        GpuChange::Inside(Region {
            x0,
            y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
}
