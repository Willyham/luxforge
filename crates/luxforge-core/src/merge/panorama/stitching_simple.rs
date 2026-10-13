//! A small, exact-by-construction stitch for the job's tests, standing in for the production
//! stitch until it is built: each output pixel's world direction is projected into every frame
//! that sees it and taken, bilinearly and at the frame's gain, from the frame whose centre it is
//! nearest. Projection, field of view, bounds, scale and the crop follow the contract's rules; the
//! seams are the nearest-centre boundaries, there is no blend and no warp mesh. `magnify` scales
//! the output past 1 (never done by a real stitch) so a test can publish a large result from small
//! frames, and `refuse` makes the plan refuse with a stitch refusal.
use super::{ChunkStats, FrameWindows, PreviewStitch, StitchPlan, StitchRequest, Stitcher};
use crate::{
    Cancel, Error,
    catalog_types::{
        PanoramaFrameOutline, PanoramaPreviewOverlays, PanoramaProjection, PanoramaSeams,
        PanoramaStitch,
    },
    merge::{
        hdr::{ColourProxy, PixelRect},
        panorama::{Lens, Rotation},
    },
};
use rayon::prelude::*;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// The bounding rectangle's low and high corners on the output plane.
type Bounds = ((f64, f64), (f64, f64));

/// Border samples a frame edge is outlined by.
const EDGE: usize = 32;
/// The coverage grid's longer side, in cells.
const GRID: f64 = 256.0;

#[derive(Default)]
pub(crate) struct SimpleStitcher {
    pub magnify: f64,
    /// Refuse every plan as `OutputTooLarge` naming frame 0.
    pub refuse: bool,
    /// Plans made, for a test that checks a redraw re-plans without reading.
    pub plans: AtomicUsize,
}

impl SimpleStitcher {
    #[cfg(test)]
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            magnify: 1.0,
            ..Self::default()
        })
    }

    #[cfg(test)]
    pub(crate) fn magnified(magnify: f64) -> Arc<Self> {
        Arc::new(Self {
            magnify,
            ..Self::default()
        })
    }
}

/// The simple stitch's state.
struct Plan {
    lens: Lens,
    rotations: Vec<Rotation>,
    projection: PanoramaProjection,
    /// The output plane's focal length, in output pixels.
    focal: f64,
    /// The bounding rectangle's top left on the output plane.
    origin: (f64, f64),
    crop: PixelRect,
    gains: Vec<[f64; 3]>,
}

impl Plan {
    /// The output plane's point of a world direction.
    fn forward(&self, world: [f64; 3]) -> Option<(f64, f64)> {
        plane(self.projection, self.focal, world)
    }

    /// The world direction of the bounding rectangle's pixel `(x, y)`.
    fn direction(&self, x: f64, y: f64) -> [f64; 3] {
        let (u, v) = (
            (x + self.origin.0) / self.focal,
            (y + self.origin.1) / self.focal,
        );
        match self.projection {
            PanoramaProjection::Spherical | PanoramaProjection::Auto => {
                [v.cos() * u.sin(), v.sin(), v.cos() * u.cos()]
            }
            PanoramaProjection::Cylindrical => normalize([u.sin(), v, u.cos()]),
            PanoramaProjection::Perspective => normalize([u, v, 1.0]),
        }
    }

    /// The frame nearest its centre among `frames` that sees `world`, and where.
    fn pick(
        &self,
        world: [f64; 3],
        frames: impl Iterator<Item = usize>,
    ) -> Option<(usize, (f64, f64))> {
        let (cx, cy) = self.lens.centre();
        let mut best: Option<(f64, usize, (f64, f64))> = None;
        for frame in frames {
            let Some(at) = self.lens.world_project(&self.rotations[frame], world) else {
                continue;
            };
            let inside = self.lens.contains(at);
            let distance = ((at.0 - cx) / cx).powi(2)
                + ((at.1 - cy) / cy).powi(2)
                + if inside { 0.0 } else { 1e6 };
            if best.is_none_or(|(d, ..)| distance < d) {
                best = Some((distance, frame, at));
            }
        }
        best.map(|(_, frame, at)| (frame, at))
    }
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / n, v[1] / n, v[2] / n]
}

/// A world direction on a projection's plane of focal length `focal`.
fn plane(projection: PanoramaProjection, focal: f64, world: [f64; 3]) -> Option<(f64, f64)> {
    let [x, y, z] = world;
    match projection {
        PanoramaProjection::Spherical | PanoramaProjection::Auto => {
            Some((focal * x.atan2(z), focal * y.clamp(-1.0, 1.0).asin()))
        }
        PanoramaProjection::Cylindrical => {
            let r = x.hypot(z);
            (r > 1e-9).then(|| (focal * x.atan2(z), focal * y / r))
        }
        PanoramaProjection::Perspective => (z > 1e-6).then(|| (focal * x / z, focal * y / z)),
    }
}

/// Each frame's border pixel centres, clockwise from the top left.
fn border(lens: &Lens) -> Vec<(f64, f64)> {
    let (w, h) = (f64::from(lens.width) - 1.0, f64::from(lens.height) - 1.0);
    let steps = (0..EDGE).map(|at| at as f64 / EDGE as f64);
    let mut points = Vec::with_capacity(4 * EDGE);
    points.extend(steps.clone().map(|t| (t * w, 0.0)));
    points.extend(steps.clone().map(|t| (w, t * h)));
    points.extend(steps.clone().map(|t| (w - t * w, h)));
    points.extend(steps.map(|t| (0.0, h - t * h)));
    points
}

/// The largest rectangle of `true` cells, as `(x, y, width, height)`, ties to the smallest top
/// row, then the smallest bottom row.
fn largest_rectangle(cells: &[bool], columns: usize, rows: usize) -> (usize, usize, usize, usize) {
    let mut heights = vec![0usize; columns];
    let mut best = (0, 0, 0, 0);
    let mut area = 0;
    for row in 0..rows {
        for column in 0..columns {
            heights[column] = if cells[row * columns + column] {
                heights[column] + 1
            } else {
                0
            };
        }
        for left in 0..columns {
            let mut height = usize::MAX;
            for (span, column) in heights[left..].iter().enumerate() {
                height = height.min(*column);
                if height == 0 {
                    break;
                }
                let candidate = (span + 1) * height;
                if candidate > area {
                    area = candidate;
                    best = (left, row + 1 - height, span + 1, height);
                }
            }
        }
    }
    best
}

impl Stitcher for SimpleStitcher {
    fn plan(&self, request: &StitchRequest<'_>, cancel: &Cancel) -> Result<StitchPlan, Error> {
        self.plans.fetch_add(1, Ordering::SeqCst);
        cancel.check()?;
        if self.refuse {
            return Err(Error::validation(
                "frame 1 makes the panorama larger than a derived source may be",
            )
            .with_data(json!({"kind": "panorama", "reason": "output-too-large", "frames": [0]})));
        }
        let registration = request.registration;
        let lens = registration.lens;
        let n = registration.rotations.len();
        let outline = border(&lens);
        // The field of view, from every frame's border on the sphere.
        let (mut lon, mut lat) = ((f64::MAX, f64::MIN), (f64::MAX, f64::MIN));
        for rotation in &registration.rotations {
            for &point in &outline {
                let [x, y, z] = lens.world_ray(rotation, point);
                let (a, b) = (x.atan2(z), y.clamp(-1.0, 1.0).asin());
                lon = (lon.0.min(a), lon.1.max(a));
                lat = (lat.0.min(b), lat.1.max(b));
            }
        }
        let field = [
            (lon.1 - lon.0).to_degrees().min(360.0),
            (lat.1 - lat.0).to_degrees().min(180.0),
        ];
        let projection = match request.projection {
            PanoramaProjection::Auto if field[1] > 100.0 || field[0] >= 300.0 => {
                PanoramaProjection::Spherical
            }
            PanoramaProjection::Auto if field[0] > 100.0 => PanoramaProjection::Cylindrical,
            PanoramaProjection::Auto => PanoramaProjection::Perspective,
            chosen => chosen,
        };
        if projection == PanoramaProjection::Perspective && field[0].max(field[1]) >= 120.0 {
            return Err(Error::validation(format!(
                "the frames span {:.0}°, beyond what a perspective projection shows; choose \
                 cylindrical",
                field[0].max(field[1])
            ))
            .with_data(json!({
                "kind": "panorama",
                "reason": "excessive-field-of-view",
                "frames": (0..n).collect::<Vec<_>>(),
                "suggested": "cylindrical",
            })));
        }
        // Bounds at scale 1 (the reference's centre pixel is one output pixel), then the scale
        // that fits the limits.
        let bounds = |focal: f64| -> Result<Bounds, Error> {
            let mut low = (f64::MAX, f64::MAX);
            let mut high = (f64::MIN, f64::MIN);
            for rotation in &registration.rotations {
                for &point in &outline {
                    let world = lens.world_ray(rotation, point);
                    let Some(at) = plane(projection, focal, world) else {
                        continue;
                    };
                    low = (low.0.min(at.0), low.1.min(at.1));
                    high = (high.0.max(at.0), high.1.max(at.1));
                }
            }
            if low.0 > high.0 {
                return Err(Error::internal("no frame projects onto the output"));
            }
            Ok((low, high))
        };
        let size = |(low, high): ((f64, f64), (f64, f64))| {
            (
                (high.0 - low.0).floor() as u64 + 1,
                (high.1 - low.1).floor() as u64 + 1,
            )
        };
        let magnify = if self.magnify > 0.0 {
            self.magnify
        } else {
            1.0
        };
        let mut scale = 1.0;
        let mut box_ = bounds(lens.focal_px * magnify)?;
        let (w, h) = size(box_);
        let limits = request.limits;
        if w.max(h) > u64::from(limits.max_side) || w * h > limits.max_pixels {
            scale = (f64::from(limits.max_side) / w.max(h) as f64)
                .min((limits.max_pixels as f64 / (w * h) as f64).sqrt())
                * 0.999;
            box_ = bounds(lens.focal_px * magnify * scale)?;
        }
        let (bw, bh) = size(box_);
        let (bw, bh) = (bw as u32, bh as u32);
        let mut plan = Plan {
            lens,
            rotations: registration.rotations.clone(),
            projection,
            focal: lens.focal_px * magnify * scale,
            origin: box_.0,
            crop: PixelRect::default(),
            gains: request
                .exposure_scales
                .iter()
                .map(|scale| [*scale; 3])
                .collect(),
        };
        // The covered region on a coarse grid of vertices, and the largest clean rectangle.
        let cell = (f64::from(bw.max(bh)) / GRID).ceil().max(1.0);
        let columns = (f64::from(bw - 1) / cell).ceil() as usize;
        let rows = (f64::from(bh - 1) / cell).ceil() as usize;
        let (columns, rows) = (columns.max(1), rows.max(1));
        let mut vertices = vec![false; (columns + 1) * (rows + 1)];
        for row in 0..=rows {
            cancel.check()?;
            for column in 0..=columns {
                let x = (column as f64 * cell).min(f64::from(bw - 1));
                let y = (row as f64 * cell).min(f64::from(bh - 1));
                let world = plan.direction(x, y);
                vertices[row * (columns + 1) + column] = (0..n).any(|frame| {
                    lens.world_project(&plan.rotations[frame], world)
                        .is_some_and(|at| lens.contains(at))
                });
            }
        }
        let cells: Vec<bool> = (0..rows * columns)
            .map(|at| {
                let (row, column) = (at / columns, at % columns);
                let v = |r: usize, c: usize| vertices[r * (columns + 1) + c];
                v(row, column) && v(row, column + 1) && v(row + 1, column) && v(row + 1, column + 1)
            })
            .collect();
        let covered = cells.iter().filter(|cell| **cell).count();
        let (x, y, width, height) = largest_rectangle(&cells, columns, rows);
        if width == 0 || height == 0 {
            return Err(
                Error::validation("the frames cover no rectangle").with_data(
                    json!({"kind": "panorama", "reason": "insufficient-coverage", "frames": [0]}),
                ),
            );
        }
        let to = |at: usize, limit: u32| ((at as f64 * cell).round() as u32).min(limit);
        let (x0, y0) = (to(x, bw - 1), to(y, bh - 1));
        let (x1, y1) = (to(x + width, bw - 1), to(y + height, bh - 1));
        plan.crop = PixelRect {
            x: x0,
            y: y0,
            width: (x1 - x0).max(1) + 1,
            height: (y1 - y0).max(1) + 1,
        };
        plan.crop.width = plan.crop.width.min(bw - x0);
        plan.crop.height = plan.crop.height.min(bh - y0);
        let crop = plan.crop;
        let record = PanoramaStitch {
            projection,
            field_of_view_deg: field,
            scale: magnify * scale,
            bounds_width: bw,
            bounds_height: bh,
            crop,
            crop_loss: 1.0 - (width * height) as f64 / covered.max(1) as f64,
            gains: plan.gains.clone(),
            seams: PanoramaSeams {
                seam_px: vec![0; n],
                contributing_px: vec![0; n],
                clipped_px: 0,
                max_overshoot: 0.0,
            },
            warp: None,
        };
        Ok(StitchPlan {
            width: crop.width,
            height: crop.height,
            record,
            plan_bytes: 0,
            chunk_bytes: 0,
            inner: Arc::new(plan),
        })
    }

    fn render_chunk(
        &self,
        plan: &StitchPlan,
        chunk: PixelRect,
        source: &mut dyn FrameWindows,
        out: &mut [f32],
        cancel: &Cancel,
    ) -> Result<ChunkStats, Error> {
        let mut windows = Vec::new();
        for (frame, bounds) in self.windows(plan, chunk)? {
            windows.push(Window::of(frame, &source.read(frame, bounds)?));
        }
        let inner = inner(plan)?;
        let crop = inner.crop;
        let width = chunk.width as usize;
        let pixels = width * chunk.height as usize;
        if out.len() < 3 * pixels {
            return Err(Error::internal("a chunk buffer is shorter than its chunk"));
        }
        let (red, rest) = out[..3 * pixels].split_at_mut(pixels);
        let (green, blue) = rest.split_at_mut(pixels);
        // Rows in parallel, so a test's large result renders in seconds.
        red.par_chunks_mut(width)
            .zip(green.par_chunks_mut(width))
            .zip(blue.par_chunks_mut(width))
            .enumerate()
            .try_for_each(|(row, ((red, green), blue))| {
                cancel.check()?;
                let y = f64::from(crop.y + chunk.y) + row as f64;
                for column in 0..width {
                    let x = f64::from(crop.x + chunk.x) + column as f64;
                    let world = inner.direction(x, y);
                    let rgb = match inner.pick(world, windows.iter().map(|window| window.frame)) {
                        Some((frame, at)) => {
                            let window = windows
                                .iter()
                                .find(|window| window.frame == frame)
                                .ok_or_else(|| Error::internal("a chunk picked an unread frame"))?;
                            let gain = inner.gains[frame];
                            let rgb = bilinear(window, at);
                            [
                                (f64::from(rgb[0]) * gain[0]) as f32,
                                (f64::from(rgb[1]) * gain[1]) as f32,
                                (f64::from(rgb[2]) * gain[2]) as f32,
                            ]
                        }
                        None => [0.0; 3],
                    };
                    red[column] = rgb[0];
                    green[column] = rgb[1];
                    blue[column] = rgb[2];
                }
                Ok::<(), Error>(())
            })?;
        Ok(ChunkStats::default())
    }

    fn preview(
        &self,
        plan: &StitchPlan,
        colour: &[ColourProxy],
        side: u32,
        cancel: &Cancel,
    ) -> Result<PreviewStitch, Error> {
        let inner = inner(plan)?;
        let lens = inner.lens;
        let (bw, bh) = (plan.record.bounds_width, plan.record.bounds_height);
        let s = (f64::from(side) / f64::from(bw.max(bh))).min(1.0);
        let (width, height) = (
            ((f64::from(bw) * s).round() as usize).max(1),
            ((f64::from(bh) * s).round() as usize).max(1),
        );
        let mut rgb = [
            vec![0.0_f32; width * height],
            vec![0.0_f32; width * height],
            vec![0.0_f32; width * height],
        ];
        for y in 0..height {
            if y % 32 == 0 {
                cancel.check()?;
            }
            for x in 0..width {
                let world = inner.direction(x as f64 / s, y as f64 / s);
                let Some((frame, at)) = inner.pick(world, 0..inner.rotations.len()) else {
                    continue;
                };
                if !lens.contains(at) {
                    continue;
                }
                let proxy = &colour[frame];
                let factor = f64::from(proxy.factor());
                let (pw, ph) = proxy.dimensions();
                let px = (((at.0 - (factor - 1.0) / 2.0) / factor).round().max(0.0) as usize)
                    .min(pw - 1);
                let py = (((at.1 - (factor - 1.0) / 2.0) / factor).round().max(0.0) as usize)
                    .min(ph - 1);
                let view = proxy.view();
                let gain = inner.gains[frame];
                for channel in 0..3 {
                    rgb[channel][y * width + x] =
                        (f64::from(view.rgb[channel][py * pw + px]) * gain[channel]) as f32;
                }
            }
        }
        let outline = border(&lens);
        let frames: Vec<PanoramaFrameOutline> = inner
            .rotations
            .iter()
            .enumerate()
            .map(|(frame, rotation)| PanoramaFrameOutline {
                frame: frame as u32,
                outline: outline
                    .iter()
                    .filter_map(|&point| inner.forward(lens.world_ray(rotation, point)))
                    .map(|(u, v)| [(u - inner.origin.0) * s, (v - inner.origin.1) * s])
                    .collect(),
            })
            .collect();
        let crop = inner.crop;
        Ok(PreviewStitch {
            width,
            height,
            rgb,
            overlays: PanoramaPreviewOverlays {
                covered: frames.iter().map(|frame| frame.outline.clone()).collect(),
                crop: [
                    f64::from(crop.x) * s,
                    f64::from(crop.y) * s,
                    f64::from(crop.width) * s,
                    f64::from(crop.height) * s,
                ],
                frames,
            },
        })
    }
}

impl SimpleStitcher {
    /// The rectangle of each frame the result's `chunk` reads, by request position.
    fn windows(
        &self,
        plan: &StitchPlan,
        chunk: PixelRect,
    ) -> Result<Vec<(usize, PixelRect)>, Error> {
        let inner = inner(plan)?;
        let lens = inner.lens;
        let crop = inner.crop;
        let (cw, ch) = (f64::from(chunk.width - 1), f64::from(chunk.height - 1));
        let mut samples = Vec::new();
        let steps = 16;
        for row in 0..=steps {
            for column in 0..=steps {
                let x = f64::from(crop.x + chunk.x) + cw * column as f64 / steps as f64;
                let y = f64::from(crop.y + chunk.y) + ch * row as f64 / steps as f64;
                samples.push(inner.direction(x, y));
            }
        }
        let mut windows = Vec::new();
        for (frame, rotation) in inner.rotations.iter().enumerate() {
            let mut low = (f64::MAX, f64::MAX);
            let mut high = (f64::MIN, f64::MIN);
            for &world in &samples {
                let Some(at) = lens.world_project(rotation, world) else {
                    continue;
                };
                if at.0 < -64.0
                    || at.1 < -64.0
                    || at.0 > f64::from(lens.width) + 64.0
                    || at.1 > f64::from(lens.height) + 64.0
                {
                    continue;
                }
                low = (low.0.min(at.0), low.1.min(at.1));
                high = (high.0.max(at.0), high.1.max(at.1));
            }
            if low.0 > high.0 {
                continue;
            }
            let pad = 4.0 + (cw.max(ch) / steps as f64) * 0.5;
            let clamp = |value: f64, limit: u32| value.clamp(0.0, f64::from(limit - 1)) as u32;
            let (x0, y0) = (
                clamp(low.0 - pad, lens.width),
                clamp(low.1 - pad, lens.height),
            );
            let (x1, y1) = (
                clamp(high.0 + pad + 1.0, lens.width),
                clamp(high.1 + pad + 1.0, lens.height),
            );
            if x1 > x0 && y1 > y0 {
                windows.push((
                    frame,
                    PixelRect {
                        x: x0,
                        y: y0,
                        width: x1 - x0 + 1,
                        height: y1 - y0 + 1,
                    },
                ));
            }
        }
        Ok(windows)
    }
}

/// One frame's window, copied.
struct Window {
    frame: usize,
    x: u32,
    y: u32,
    width: usize,
    height: usize,
    rgb: [Vec<f32>; 3],
}

impl Window {
    fn of(frame: usize, source: &crate::merge::hdr::SourceWindow<'_>) -> Self {
        let view = &source.frame;
        let rgb = std::array::from_fn(|c| {
            (0..view.height)
                .flat_map(|y| view.rgb[c][y * view.stride..y * view.stride + view.width].iter())
                .copied()
                .collect()
        });
        Self {
            frame,
            x: source.x,
            y: source.y,
            width: view.width,
            height: view.height,
            rgb,
        }
    }
}

fn inner(plan: &StitchPlan) -> Result<&Plan, Error> {
    plan.inner
        .downcast_ref::<Plan>()
        .ok_or_else(|| Error::internal("a stitch plan of another stitcher"))
}

/// The window's sample at frame point `at`, bilinear with taps clamped to the window.
fn bilinear(window: &Window, at: (f64, f64)) -> [f32; 3] {
    let frame = window;
    let (w, h) = (frame.width as f64, frame.height as f64);
    let x = (at.0 - f64::from(window.x)).clamp(0.0, w - 1.0);
    let y = (at.1 - f64::from(window.y)).clamp(0.0, h - 1.0);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as usize, y0 as usize);
    let (x1, y1) = (
        (x0 + 1).min(frame.width - 1),
        (y0 + 1).min(frame.height - 1),
    );
    let tap =
        |channel: usize, x: usize, y: usize| f64::from(frame.rgb[channel][y * frame.width + x]);
    std::array::from_fn(|channel| {
        let top = tap(channel, x0, y0) * (1.0 - fx) + tap(channel, x1, y0) * fx;
        let bottom = tap(channel, x0, y1) * (1.0 - fx) + tap(channel, x1, y1) * fx;
        (top * (1.0 - fy) + bottom * fy) as f32
    })
}
