//! Qualification only: the Presence CPU filters over whole frames, for the desktop's per-filter
//! readback tests, which hold each GPU kernel to the CPU filter it transcribes on synthetic planes
//! (`docs/design/gpu-preview.md`, "Qualifying a program").
//!
//! Each function runs the production filter in `filters.rs` over one whole frame as one tile, in
//! `f32` with the CPU's own `f64` running sums. Built only with the core's `qualification`
//! feature, which only a `[dev-dependencies]` table may turn on (`cargo xtask check-repository`),
//! so no build of a binary has it.
use super::{
    dehaze,
    filters::{self, Geometry, PlaneMut, Rect, Scratch},
};
use crate::{
    Cancel,
    modules::{Parallelism, Region, SpatialUnit, Stage},
};

/// Every regularization and limit the units hold, for a test's cases.
pub const EPS_TEXTURE: f32 = super::texture::EPS_TEXTURE;
pub const EPS_CLARITY: f32 = super::clarity::EPS_CLARITY;
pub const EPS_DEHAZE: f32 = super::dehaze::EPS_DEHAZE;
pub const LIMIT_TEXTURE: f32 = super::texture::LIMIT_TEXTURE;
pub const LIMIT_CLARITY: f32 = super::clarity::LIMIT_CLARITY;

fn frame(width: u32, height: u32) -> (Geometry, Rect) {
    let rect = Rect::frame(i64::from(width), i64::from(height));
    (Geometry::new(rect.x1, rect.y1, rect), rect)
}

fn plane<'a>(values: &'a mut [f32], width: u32, height: u32) -> PlaneMut<'a> {
    let (geometry, rect) = frame(width, height);
    PlaneMut::over(values, geometry, rect).expect("a whole frame's plane")
}

/// The box mean of radius `r` over a `width × height` plane, edge-clamped.
pub fn box_mean(width: u32, height: u32, values: &[f32], r: u32) -> Vec<f32> {
    let mut source = values.to_vec();
    let mut out = vec![0.0; values.len()];
    let mut temp = vec![0.0; values.len()];
    let source = plane(&mut source, width, height);
    let mut dst = plane(&mut out, width, height);
    filters::box_mean(
        &source.as_plane(),
        i64::from(r),
        &mut dst,
        &mut temp,
        Parallelism::Serial,
    )
    .expect("the box mean's planes");
    out
}

/// The box minimum of radius `r`.
pub fn box_min(width: u32, height: u32, values: &[f32], r: u32) -> Vec<f32> {
    let mut source = values.to_vec();
    let mut out = vec![0.0; values.len()];
    let mut temp = vec![0.0; values.len()];
    let source = plane(&mut source, width, height);
    let mut dst = plane(&mut out, width, height);
    filters::box_min(
        &source.as_plane(),
        i64::from(r),
        &mut dst,
        &mut temp,
        Parallelism::Serial,
    )
    .expect("the box minimum's planes");
    out
}

/// The self-guided smoother of radius `r` and regularization `eps`.
pub fn guided_self(width: u32, height: u32, values: &[f32], r: u32, eps: f32) -> Vec<f32> {
    let mut source = values.to_vec();
    let mut out = vec![0.0; values.len()];
    let mut scratch = vec![0.0; values.len() * filters::GUIDED_SELF_PLANES];
    let source = plane(&mut source, width, height);
    let mut dst = plane(&mut out, width, height);
    filters::guided_self(
        &source.as_plane(),
        i64::from(r),
        eps,
        &mut dst,
        &mut Scratch::new(&mut scratch),
        Parallelism::Serial,
    )
    .expect("the smoother's planes");
    out
}

/// The guided filter of `input` under `guide`.
pub fn guided_filter(
    width: u32,
    height: u32,
    guide: &[f32],
    input: &[f32],
    r: u32,
    eps: f32,
) -> Vec<f32> {
    let (mut guide, mut input) = (guide.to_vec(), input.to_vec());
    let mut out = vec![0.0; guide.len()];
    let mut scratch = vec![0.0; guide.len() * filters::GUIDED_PLANES];
    let guide = plane(&mut guide, width, height);
    let input = plane(&mut input, width, height);
    let mut dst = plane(&mut out, width, height);
    filters::guided_filter(
        &guide.as_plane(),
        &input.as_plane(),
        i64::from(r),
        eps,
        &mut dst,
        &mut Scratch::new(&mut scratch),
        Parallelism::Serial,
    )
    .expect("the guided filter's planes");
    out
}

/// The block mean by `s` anchored at the origin, partial blocks over their own pixels: the reduced
/// plane's width, height and values.
pub fn downsample(width: u32, height: u32, values: &[f32], s: u32) -> (u32, u32, Vec<f32>) {
    let (reduced_width, reduced_height) =
        filters::reduced_frame(i64::from(width), i64::from(height), i64::from(s));
    let mut source = values.to_vec();
    let mut out = vec![0.0; (reduced_width * reduced_height) as usize];
    let source = plane(&mut source, width, height);
    let reduced = Rect::frame(reduced_width, reduced_height);
    let mut dst = PlaneMut::over(
        &mut out,
        Geometry::new(reduced_width, reduced_height, reduced),
        reduced,
    )
    .expect("the reduced plane");
    filters::downsample(
        &source.as_plane(),
        i64::from(s),
        &mut dst,
        Parallelism::Serial,
    );
    (reduced_width as u32, reduced_height as u32, out)
}

/// The bilinear upsample by `s` of a `reduced_width × reduced_height` plane to `width × height`.
pub fn upsample(
    width: u32,
    height: u32,
    reduced_width: u32,
    reduced_height: u32,
    reduced: &[f32],
    s: u32,
) -> Vec<f32> {
    let mut source = reduced.to_vec();
    let mut out = vec![0.0; (width * height) as usize];
    let source = plane(&mut source, reduced_width, reduced_height);
    let mut dst = plane(&mut out, width, height);
    filters::upsample(
        &source.as_plane(),
        i64::from(s),
        &mut dst,
        Parallelism::Serial,
    );
    out
}

/// The compressive soft clip.
pub fn soft_clip(raw: f32, encoded: f32, limit: f32) -> f32 {
    filters::soft_clip(raw, encoded, limit)
}

/// The encoded luminance the luminance units work in.
pub fn encoded_luminance(rgb: [f32; 3]) -> f32 {
    filters::encoded_luminance(rgb)
}

/// Dehaze's atmospheric light over a `width × height` stage of planar linear RGB (one red plane,
/// then green, then blue): the host's 1/16 reduction of it and the unit's own preparation, as a
/// render prepares it on a store miss.
pub fn atmosphere(width: u32, height: u32, planes: &[f32]) -> [f64; 3] {
    let stage = Stage { width, height };
    let len = (width * height) as usize;
    let reduction = crate::render::spatial::build_reduction_cancellable(
        stage,
        &Cancel::never(),
        |region: Region, out: &mut [f32]| {
            let count = region.pixels() as usize;
            for (row, y) in (region.y0..region.y1()).enumerate() {
                for (column, x) in (region.x0..region.x1()).enumerate() {
                    let at = (y * width + x) as usize;
                    let index = row * region.width as usize + column;
                    for channel in 0..3 {
                        out[channel * count + index] = planes[channel * len + at];
                    }
                }
            }
            Ok(())
        },
    )
    .expect("the reduction");
    let unit = dehaze::Dehaze::new(100.0, width.max(height));
    let global = unit.prepare(&reduction).expect("an atmospheric light");
    let values = global.values();
    [values[0], values[1], values[2]]
}
