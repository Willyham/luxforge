//! Independent f64 references for panorama merges (`docs/design/panorama-merge.md`, "Panorama
//! processing contract"): a procedural scene defined on every viewing direction, cameras that turn
//! about their optical centre with one shared lens (focal length and radial term), frames recorded
//! through them with per-frame gains, noise and a moving subject, and the ground-truth panorama of
//! the same scene drawn under a projection.
//!
//! Conventions, which the design's "Registration" section records as the contract:
//!
//! - **Pixels.** Pixel `(x, y)`'s centre is at `(x, y)`. A `W × H` frame's principal point is its
//!   centre, `c = ((W − 1)/2, (H − 1)/2)`.
//! - **Directions.** Camera and world coordinates are right-handed with `x` right, `y` down and
//!   `z` forward. A camera direction `(u, v, 1)` records at the ideal point `c + f·(u, v)`.
//! - **Radial term.** The lens records an ideal offset `p` from `c` at `c + p·(1 + k·|p|²/R²)`,
//!   with `R = √(W² + H²)/2`, half the frame's diagonal: `k < 0` is barrel distortion.
//! - **Orientation.** A [`Pose`] turns the camera by yaw `ψ` (positive to the right), then pitch
//!   `θ` (positive up), then roll `φ` (positive turns the image's `+x` towards `+y`): its
//!   camera-to-world rotation is `Yaw(ψ)·Pitch(θ)·Roll(φ)`, and a world direction `d` is the
//!   camera direction `Mᵀd`. A level camera (`θ = φ = 0`) has its `y` axis along the world's
//!   down, `+y`.
//!
//! Written from the design, not from the production code; no production code depends on this file
//! and it imports nothing from the core.

use crate::SplitMix64;

pub type Vec3 = [f64; 3];
/// A 3 × 3 matrix, row-major.
pub type Mat3 = [[f64; 3]; 3];

pub fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn normalize(v: Vec3) -> Vec3 {
    let n = dot(v, v).sqrt();
    [v[0] / n, v[1] / n, v[2] / n]
}

pub fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
}

pub fn transpose(m: &Mat3) -> Mat3 {
    std::array::from_fn(|r| std::array::from_fn(|c| m[c][r]))
}

pub fn apply(m: &Mat3, v: Vec3) -> Vec3 {
    std::array::from_fn(|r| dot(m[r], v))
}

/// The angle between two directions, in radians, from the stable `atan2` form.
pub fn angle_between(a: Vec3, b: Vec3) -> f64 {
    let c = cross(a, b);
    dot(c, c).sqrt().atan2(dot(a, b))
}

/// A camera's orientation in degrees. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub yaw_deg: f64,
    pub pitch_deg: f64,
    pub roll_deg: f64,
}

impl Pose {
    pub const fn new(yaw_deg: f64, pitch_deg: f64, roll_deg: f64) -> Self {
        Self {
            yaw_deg,
            pitch_deg,
            roll_deg,
        }
    }

    /// `Yaw(ψ)·Pitch(θ)·Roll(φ)`: a camera direction to the world.
    pub fn camera_to_world(&self) -> Mat3 {
        let (sy, cy) = self.yaw_deg.to_radians().sin_cos();
        let (sp, cp) = self.pitch_deg.to_radians().sin_cos();
        let (sr, cr) = self.roll_deg.to_radians().sin_cos();
        let yaw = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
        let pitch = [[1.0, 0.0, 0.0], [0.0, cp, -sp], [0.0, sp, cp]];
        let roll = [[cr, -sr, 0.0], [sr, cr, 0.0], [0.0, 0.0, 1.0]];
        mat_mul(&yaw, &mat_mul(&pitch, &roll))
    }

    /// A world direction to the camera.
    pub fn world_to_camera(&self) -> Mat3 {
        transpose(&self.camera_to_world())
    }

    /// The optical axis in the world.
    pub fn axis(&self) -> Vec3 {
        apply(&self.camera_to_world(), [0.0, 0.0, 1.0])
    }
}

/// One lens and frame size shared by every frame. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lens {
    pub width: usize,
    pub height: usize,
    pub focal_px: f64,
    pub radial: f64,
}

impl Lens {
    /// A lens whose horizontal field of view is `hfov_deg`, undistorted.
    pub fn with_hfov(width: usize, height: usize, hfov_deg: f64) -> Self {
        Self {
            width,
            height,
            focal_px: (width as f64 / 2.0) / (hfov_deg.to_radians() / 2.0).tan(),
            radial: 0.0,
        }
    }

    pub fn centre(&self) -> (f64, f64) {
        (
            (self.width as f64 - 1.0) / 2.0,
            (self.height as f64 - 1.0) / 2.0,
        )
    }

    /// `R`, half the frame's diagonal.
    pub fn radial_scale(&self) -> f64 {
        ((self.width * self.width + self.height * self.height) as f64).sqrt() / 2.0
    }

    /// Where an ideal point records.
    pub fn distort(&self, ideal: (f64, f64)) -> (f64, f64) {
        let (cx, cy) = self.centre();
        let (dx, dy) = (ideal.0 - cx, ideal.1 - cy);
        let scale = 1.0 + self.radial * (dx * dx + dy * dy) / self.radial_scale().powi(2);
        (cx + dx * scale, cy + dy * scale)
    }

    /// The ideal point a recorded point shows, by Newton's method on the radius safeguarded by
    /// bisection. The distortion is monotone in the ideal radius up to `R/√(3|k|)` when `k < 0`,
    /// which the search stays within; a recorded radius past that radius's image has no ideal
    /// point and returns the limit.
    pub fn undistort(&self, recorded: (f64, f64)) -> (f64, f64) {
        let (cx, cy) = self.centre();
        let (dx, dy) = (recorded.0 - cx, recorded.1 - cy);
        let rd = (dx * dx + dy * dy).sqrt();
        if rd == 0.0 || self.radial == 0.0 {
            return recorded;
        }
        let r2 = self.radial_scale().powi(2);
        let forward = |r: f64| r * (1.0 + self.radial * r * r / r2);
        let mut hi = 2.0 * rd + self.radial_scale();
        if self.radial < 0.0 {
            hi = hi.min(self.radial_scale() / (3.0 * -self.radial).sqrt());
        }
        let mut lo = 0.0;
        let mut ru = rd.min(hi);
        for _ in 0..100 {
            let value = forward(ru) - rd;
            if value < 0.0 {
                lo = ru;
            } else {
                hi = ru;
            }
            let slope = 1.0 + 3.0 * self.radial * ru * ru / r2;
            let newton = ru - value / slope;
            let next = if slope > 0.0 && newton > lo && newton < hi {
                newton
            } else {
                0.5 * (lo + hi)
            };
            if (next - ru).abs() <= 1e-13 * rd {
                ru = next;
                break;
            }
            ru = next;
        }
        (cx + dx * ru / rd, cy + dy * ru / rd)
    }

    /// The camera direction (unit) a recorded point shows.
    pub fn ray(&self, recorded: (f64, f64)) -> Vec3 {
        let (cx, cy) = self.centre();
        let (x, y) = self.undistort(recorded);
        normalize([(x - cx) / self.focal_px, (y - cy) / self.focal_px, 1.0])
    }

    /// Where a camera direction records, or `None` behind the camera.
    pub fn project(&self, camera: Vec3) -> Option<(f64, f64)> {
        if camera[2] <= 1e-12 {
            return None;
        }
        let (cx, cy) = self.centre();
        let ideal = (
            cx + self.focal_px * camera[0] / camera[2],
            cy + self.focal_px * camera[1] / camera[2],
        );
        Some(self.distort(ideal))
    }

    /// The world direction a recorded point of a frame at `pose` shows.
    pub fn world_ray(&self, pose: &Pose, recorded: (f64, f64)) -> Vec3 {
        apply(&pose.camera_to_world(), self.ray(recorded))
    }

    /// Where a world direction records in a frame at `pose`, if in front of it (it may fall
    /// outside the frame).
    pub fn world_project(&self, pose: &Pose, world: Vec3) -> Option<(f64, f64)> {
        self.project(apply(&pose.world_to_camera(), world))
    }

    /// The frame's four corner pixel centres, clockwise from the top left.
    pub fn corners(&self) -> [(f64, f64); 4] {
        let (w, h) = ((self.width - 1) as f64, (self.height - 1) as f64);
        [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]
    }
}

/// One rectangular patch of the scene: a box in space whose intersection with the unit sphere is
/// what a camera sees.
#[derive(Clone, Copy, Debug)]
struct Patch {
    centre: Vec3,
    /// Rows: the box's axes.
    axes: Mat3,
    half: Vec3,
    stops: f64,
    tint: [f64; 3],
    /// No point further than this from the centre is inside the box or its soft edge.
    reach: f64,
}

/// A textured environment: linear radiance for every viewing direction. Space is cut into cubes
/// of side `cell` (radians, near the unit sphere); each holds one randomly turned box with sides
/// 30–84% of the cell, ±1.5 stops and a slight colour tint, whose edges soften over a fifth of the
/// cell, so a frame that point-samples it at a few pixels a cell does not alias. On a base of −2 stops brightening by 0.6 stops towards the zenith. Each box reaches less
/// than one cell from its centre, so only the 27 cells around a point can touch it: the scene is
/// exactly continuous. Deterministic in `seed`.
#[derive(Clone, Debug)]
pub struct Scene {
    cell: f64,
    edge: f64,
    cells: usize,
    origin: f64,
    patches: Vec<Patch>,
}

impl Scene {
    pub fn new(seed: u64, cell: f64) -> Self {
        let origin = -1.0 - 2.0 * cell;
        let cells = ((2.0 * -origin) / cell).ceil() as usize;
        let edge = 0.2 * cell;
        let mut patches = Vec::with_capacity(cells.pow(3));
        for k in 0..cells {
            for j in 0..cells {
                for i in 0..cells {
                    let mut rng = SplitMix64(
                        seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                            ^ (j as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
                            ^ (k as u64).wrapping_mul(0x1656_67B1_9E37_79F9),
                    );
                    let centre = [
                        origin + cell * (i as f64 + rng.next_range(0.0, 1.0)),
                        origin + cell * (j as f64 + rng.next_range(0.0, 1.0)),
                        origin + cell * (k as f64 + rng.next_range(0.0, 1.0)),
                    ];
                    // A uniformly random rotation from three uniforms (Shoemake).
                    let (u1, u2, u3) = (
                        rng.next_range(0.0, 1.0),
                        rng.next_range(0.0, std::f64::consts::TAU),
                        rng.next_range(0.0, std::f64::consts::TAU),
                    );
                    let (a, b) = ((1.0 - u1).sqrt(), u1.sqrt());
                    let q = [a * u2.sin(), a * u2.cos(), b * u3.sin(), b * u3.cos()];
                    let axes = quaternion_matrix(q);
                    let half = [
                        cell * rng.next_range(0.15, 0.42),
                        cell * rng.next_range(0.15, 0.42),
                        cell * rng.next_range(0.15, 0.42),
                    ];
                    let stops = rng.next_range(-1.5, 1.5);
                    let tint = [rng.next_range(0.85, 1.15), 1.0, rng.next_range(0.85, 1.15)];
                    let reach = (dot(half, half)).sqrt() + edge;
                    patches.push(Patch {
                        centre,
                        axes,
                        half,
                        stops,
                        tint,
                        reach,
                    });
                }
            }
        }
        Self {
            cell,
            edge,
            cells,
            origin,
            patches,
        }
    }

    /// The linear sRGB radiance seen along `direction` (any length; normalized here).
    pub fn radiance(&self, direction: Vec3) -> [f64; 3] {
        let d = normalize(direction);
        let mut stops = -2.0 - 0.6 * d[1];
        let mut tint = [1.0f64; 3];
        let index = |v: f64| ((v - self.origin) / self.cell).floor() as i64;
        let (ci, cj, ck) = (index(d[0]), index(d[1]), index(d[2]));
        let n = self.cells as i64;
        for k in ck - 1..=ck + 1 {
            for j in cj - 1..=cj + 1 {
                for i in ci - 1..=ci + 1 {
                    if !(0..n).contains(&i) || !(0..n).contains(&j) || !(0..n).contains(&k) {
                        continue;
                    }
                    let patch = &self.patches[((k * n + j) * n + i) as usize];
                    let offset = [
                        d[0] - patch.centre[0],
                        d[1] - patch.centre[1],
                        d[2] - patch.centre[2],
                    ];
                    if dot(offset, offset) > patch.reach * patch.reach {
                        continue;
                    }
                    let local = apply(&patch.axes, offset);
                    let inside: f64 = local
                        .iter()
                        .zip(patch.half)
                        .map(|(l, half)| smoothstep((half - l.abs()) / self.edge + 0.5))
                        .product();
                    if inside > 0.0 {
                        stops += patch.stops * inside;
                        for (t, p) in tint.iter_mut().zip(patch.tint) {
                            *t *= 1.0 + (p - 1.0) * inside;
                        }
                    }
                }
            }
        }
        let l = stops.exp2();
        [l * 0.95 * tint[0], l * tint[1], l * 0.85 * tint[2]]
    }
}

fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn quaternion_matrix([x, y, z, w]: [f64; 4]) -> Mat3 {
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

/// A moving subject: a soft-edged bright disc in front of the scene, at a world direction that
/// differs from frame to frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Subject {
    pub yaw_deg: f64,
    pub pitch_deg: f64,
    pub radius_deg: f64,
    /// Its radiance, in stops above the scene's base.
    pub stops: f64,
}

impl Subject {
    fn direction(&self) -> Vec3 {
        Pose::new(self.yaw_deg, self.pitch_deg, 0.0).axis()
    }

    /// The subject's opacity along `direction`, with a soft edge 10% of its radius wide.
    pub fn coverage(&self, direction: Vec3) -> f64 {
        let angle = angle_between(normalize(direction), self.direction()).to_degrees();
        let edge = 0.1 * self.radius_deg;
        smoothstep((self.radius_deg - angle) / edge + 0.5)
    }

    fn over(&self, direction: Vec3, scene: [f64; 3]) -> [f64; 3] {
        let alpha = self.coverage(direction);
        let level = (self.stops - 2.0).exp2();
        let own = [level, level * 0.9, level * 0.6];
        std::array::from_fn(|c| scene[c] * (1.0 - alpha) + own[c] * alpha)
    }
}

/// The recording noise: each sample becomes `v + σ·n`, `σ = √(shot·max(v, 0) + read²)`, `n` the
/// generator's bounded symmetric noise. Seeded per frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Noise {
    pub seed: u64,
    pub shot: f64,
    pub read: f64,
}

/// One frame to record: where the camera points, its per-channel gain (exposure and white
/// balance), and the subject as it was when this frame was taken.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shot {
    pub pose: Pose,
    pub gain: [f64; 3],
    pub subject: Option<Subject>,
}

impl Shot {
    pub fn at(pose: Pose) -> Self {
        Self {
            pose,
            gain: [1.0; 3],
            subject: None,
        }
    }
}

/// A recorded frame: planar linear sRGB, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgb: [Vec<f64>; 3],
}

impl Frame {
    pub fn at(&self, x: usize, y: usize) -> [f64; 3] {
        let i = y * self.width + x;
        [self.rgb[0][i], self.rgb[1][i], self.rgb[2][i]]
    }
}

/// Record `shot` through `lens`: each pixel point-samples the scene (with the subject over it)
/// along the world direction its centre shows, times the shot's gain, then noise (seeded by
/// `noise.seed` and `index`). Unclamped.
pub fn record(
    scene: &Scene,
    lens: &Lens,
    shot: &Shot,
    noise: Option<Noise>,
    index: usize,
) -> Frame {
    let n = lens.width * lens.height;
    let mut rgb = [vec![0.0; n], vec![0.0; n], vec![0.0; n]];
    let mut rng = noise.map(|noise| {
        SplitMix64(noise.seed ^ (index as u64 + 1).wrapping_mul(0xD6E8_FEB8_6659_FD93))
    });
    let to_world = shot.pose.camera_to_world();
    for y in 0..lens.height {
        for x in 0..lens.width {
            let direction = apply(&to_world, lens.ray((x as f64, y as f64)));
            let mut value = scene.radiance(direction);
            if let Some(subject) = &shot.subject {
                value = subject.over(direction, value);
            }
            let i = y * lens.width + x;
            for c in 0..3 {
                let mut v = value[c] * shot.gain[c];
                if let (Some(noise), Some(rng)) = (noise, rng.as_mut()) {
                    let sigma = (noise.shot * v.max(0.0) + noise.read * noise.read).sqrt();
                    v += sigma * rng.next_noise();
                }
                rgb[c][i] = v;
            }
        }
    }
    Frame {
        width: lens.width,
        height: lens.height,
        rgb,
    }
}

/// `count` level frames in one row, `yaw_step_deg` apart and centred on yaw 0, at `pitch_deg`,
/// left to right.
pub fn row(count: usize, yaw_step_deg: f64, pitch_deg: f64) -> Vec<Pose> {
    let start = -yaw_step_deg * (count as f64 - 1.0) / 2.0;
    (0..count)
        .map(|i| Pose::new(start + yaw_step_deg * i as f64, pitch_deg, 0.0))
        .collect()
}

/// `count` level frames evenly around the full circle, from yaw 0.
pub fn full_circle(count: usize) -> Vec<Pose> {
    (0..count)
        .map(|i| Pose::new(360.0 * i as f64 / count as f64, 0.0, 0.0))
        .collect()
}

/// `rows × cols` level frames, top row first and each row left to right, `yaw_step_deg` and
/// `pitch_step_deg` apart and centred on yaw 0 and pitch 0.
pub fn grid(rows: usize, cols: usize, yaw_step_deg: f64, pitch_step_deg: f64) -> Vec<Pose> {
    let top = pitch_step_deg * (rows as f64 - 1.0) / 2.0;
    (0..rows)
        .flat_map(|r| row(cols, yaw_step_deg, top - pitch_step_deg * r as f64))
        .collect()
}

/// `poses`, each turned by up to `max_deg` in yaw, pitch and roll: a handheld sequence.
pub fn jitter(poses: &[Pose], seed: u64, max_deg: f64) -> Vec<Pose> {
    let mut rng = SplitMix64(seed);
    poses
        .iter()
        .map(|pose| {
            Pose::new(
                pose.yaw_deg + rng.next_range(-max_deg, max_deg),
                pose.pitch_deg + rng.next_range(-max_deg, max_deg),
                pose.roll_deg + rng.next_range(-max_deg, max_deg),
            )
        })
        .collect()
}

/// How a panorama's output plane maps to directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projection {
    /// Straight lines stay straight: `(u, v)` is the direction `(u, v, 1)`.
    Rectilinear,
    /// `u` is the yaw in radians, `v` the height on a unit cylinder: `(sin u, v, cos u)`.
    Cylindrical,
    /// `u` is the yaw, `v` the latitude below the horizon, in radians:
    /// `(cos v·sin u, sin v, cos v·cos u)`.
    Equirectangular,
}

/// A ground-truth panorama's frame: output pixel `(x, y)` is the plane point
/// `(u, v) = ((x − cx)/scale, (y − cy)/scale)` about the output's centre, mapped through the
/// projection to a direction about `centre`'s camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanoramaView {
    pub projection: Projection,
    pub centre: Pose,
    /// Output pixels per unit of the plane (per radian at the centre).
    pub scale: f64,
    pub width: usize,
    pub height: usize,
}

impl PanoramaView {
    /// The world direction output pixel `(x, y)` shows.
    pub fn direction(&self, x: f64, y: f64) -> Vec3 {
        let u = (x - (self.width as f64 - 1.0) / 2.0) / self.scale;
        let v = (y - (self.height as f64 - 1.0) / 2.0) / self.scale;
        let camera = match self.projection {
            Projection::Rectilinear => [u, v, 1.0],
            Projection::Cylindrical => [u.sin(), v, u.cos()],
            Projection::Equirectangular => [v.cos() * u.sin(), v.sin(), v.cos() * u.cos()],
        };
        apply(&self.centre.camera_to_world(), normalize(camera))
    }
}

/// The scene drawn under `view`, with `subject` over it when given: the stitch a perfect merge of
/// frames at gain 1 would make.
pub fn render_panorama(scene: &Scene, view: &PanoramaView, subject: Option<&Subject>) -> Frame {
    let n = view.width * view.height;
    let mut rgb = [vec![0.0; n], vec![0.0; n], vec![0.0; n]];
    for y in 0..view.height {
        for x in 0..view.width {
            let direction = view.direction(x as f64, y as f64);
            let mut value = scene.radiance(direction);
            if let Some(subject) = subject {
                value = subject.over(direction, value);
            }
            for c in 0..3 {
                rgb[c][y * view.width + x] = value[c];
            }
        }
    }
    Frame {
        width: view.width,
        height: view.height,
        rgb,
    }
}

/// Where the corners of frame `b` land in frame `a`'s pixels, both frames recorded through `lens`
/// at the given poses: the ground-truth pairwise geometry. `None` for a corner behind `a`.
pub fn corners_in(lens: &Lens, a: &Pose, b: &Pose) -> [Option<(f64, f64)>; 4] {
    lens.corners()
        .map(|corner| lens.world_project(a, lens.world_ray(b, corner)))
}

// ------------------------------------------------------------------------------------------------
// The stitched panorama (`docs/design/panorama-merge.md`, "Projection and output size" and
// "Boundary Warp and the crop"): where every output pixel of a stitch looks, which frames record
// it, the largest covered rectangle, and what a perfect stitch shows there.
// ------------------------------------------------------------------------------------------------

/// Boundary samples an edge of each frame for a stitch's extent: points `t = k/256` along each
/// edge, clockwise from the top-left corner, as the design's contract states.
pub const STITCH_EDGE_SAMPLES: usize = 256;
/// Added to an extent in pixels before flooring it to a size, as the contract states (`ε`).
pub const STITCH_SLACK: f64 = 1e-6;

/// The plane point `(u, v)` of a direction under `projection` (see [`Projection`]), or `None`
/// where it has none: behind the rectilinear plane, or at the cylinder's poles.
pub fn plane_point(projection: Projection, direction: Vec3) -> Option<(f64, f64)> {
    let d = normalize(direction);
    match projection {
        Projection::Rectilinear => (d[2] > 1e-9).then(|| (d[0] / d[2], d[1] / d[2])),
        Projection::Cylindrical => {
            let rho = (d[0] * d[0] + d[2] * d[2]).sqrt();
            (rho > 1e-12).then(|| (d[0].atan2(d[2]), d[1] / rho))
        }
        Projection::Equirectangular => Some((d[0].atan2(d[2]), d[1].clamp(-1.0, 1.0).asin())),
    }
}

/// The unit direction of plane point `(u, v)` under `projection`.
pub fn plane_direction(projection: Projection, u: f64, v: f64) -> Vec3 {
    match projection {
        Projection::Rectilinear => normalize([u, v, 1.0]),
        Projection::Cylindrical => normalize([u.sin(), v, u.cos()]),
        Projection::Equirectangular => [v.cos() * u.sin(), v.sin(), v.cos() * u.cos()],
    }
}

/// `a` into `(−π, π]`.
fn principal(a: f64) -> f64 {
    let t = std::f64::consts::TAU;
    let mut a = a - t * (a / t).round();
    if a <= -std::f64::consts::PI {
        a += t;
    }
    if a > std::f64::consts::PI {
        a -= t;
    }
    a
}

/// Full scale: plane units to output pixels such that the reference frame's centre pixel maps to
/// at most one output pixel either way, from central differences of the plane point by the
/// reference's pixel.
pub fn stitch_full_scale(projection: Projection, lens: &Lens, reference: &Pose) -> f64 {
    let (cx, cy) = lens.centre();
    let h = 1e-3;
    let at = |x: f64, y: f64| plane_point(projection, lens.world_ray(reference, (x, y))).unwrap();
    let (u0, v0) = at(cx, cy);
    let delta = |(u, v): (f64, f64)| (principal(u - u0), v - v0);
    let (ax, ay) = delta(at(cx + h, cy));
    let (bx, by) = delta(at(cx - h, cy));
    let (cx2, cy2) = delta(at(cx, cy + h));
    let (dx, dy) = delta(at(cx, cy - h));
    let j = [
        [(ax - bx) / (2.0 * h), (cx2 - dx) / (2.0 * h)],
        [(ay - by) / (2.0 * h), (cy2 - dy) / (2.0 * h)],
    ];
    // σ_max from the eigenvalues of JᵀJ.
    let a = j[0][0] * j[0][0] + j[1][0] * j[1][0];
    let b = j[0][0] * j[0][1] + j[1][0] * j[1][1];
    let c = j[0][1] * j[0][1] + j[1][1] * j[1][1];
    let largest = (a + c) / 2.0 + (((a - c) / 2.0).powi(2) + b * b).sqrt();
    1.0 / largest.sqrt()
}

/// A stitch's output frame: output pixel `(x, y)` of its bounding rectangle is the plane point
/// `(u0 + x/scale, v0 + y/scale)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StitchLayout {
    pub projection: Projection,
    /// Output pixels per plane unit.
    pub scale: f64,
    pub u0: f64,
    pub v0: f64,
    pub width: usize,
    pub height: usize,
    /// The yaws cover the full circle and the rectangle's ends meet.
    pub wrap: bool,
}

impl StitchLayout {
    /// The world direction output point `(x, y)` shows.
    pub fn direction(&self, x: f64, y: f64) -> Vec3 {
        plane_direction(
            self.projection,
            self.u0 + x / self.scale,
            self.v0 + y / self.scale,
        )
    }
}

/// A frame's boundary pixels for a stitch's extent: see [`STITCH_EDGE_SAMPLES`].
fn edge_pixels(lens: &Lens) -> Vec<(f64, f64)> {
    let (w, h) = ((lens.width - 1) as f64, (lens.height - 1) as f64);
    let n = STITCH_EDGE_SAMPLES;
    let mut out = Vec::new();
    for k in 0..n {
        out.push((w * k as f64 / n as f64, 0.0));
    }
    for k in 0..n {
        out.push((w, h * k as f64 / n as f64));
    }
    for k in 0..n {
        out.push((w * (1.0 - k as f64 / n as f64), h));
    }
    for k in 0..n {
        out.push((0.0, h * (1.0 - k as f64 / n as f64)));
    }
    out
}

/// Where world direction `d` records in a frame at `pose`, refusing directions past barrel
/// distortion's fold (ideal radius `R/√(3|k|)`), where the radial model turns back.
pub fn stitch_frame_position(lens: &Lens, pose: &Pose, d: Vec3) -> Option<(f64, f64)> {
    let camera = apply(&pose.world_to_camera(), normalize(d));
    if camera[2] <= 1e-9 {
        return None;
    }
    let (ix, iy) = (
        lens.focal_px * camera[0] / camera[2],
        lens.focal_px * camera[1] / camera[2],
    );
    if lens.radial < 0.0 && 3.0 * -lens.radial * (ix * ix + iy * iy) >= lens.radial_scale().powi(2)
    {
        return None;
    }
    let (cx, cy) = lens.centre();
    Some(lens.distort((cx + ix, cy + iy)))
}

/// Whether a frame records `d` within its pixel centres.
pub fn stitch_records(lens: &Lens, pose: &Pose, d: Vec3) -> bool {
    stitch_frame_position(lens, pose, d).is_some_and(|(x, y)| {
        x >= 0.0 && y >= 0.0 && x <= (lens.width - 1) as f64 && y <= (lens.height - 1) as f64
    })
}

/// The bounding rectangle of frames at `poses` under `projection`, at `factor` times full scale
/// (the reference frame `reference`'s), as the design's "The bounding rectangle" states.
pub fn stitch_layout(
    projection: Projection,
    lens: &Lens,
    poses: &[Pose],
    reference: usize,
    factor: f64,
) -> StitchLayout {
    let tau = std::f64::consts::TAU;
    let pi = std::f64::consts::PI;
    let scale = stitch_full_scale(projection, lens, &poses[reference]) * factor;
    let edges = edge_pixels(lens);
    let mut v_range = (f64::INFINITY, f64::NEG_INFINITY);
    let mut u_range = (f64::INFINITY, f64::NEG_INFINITY);
    let mut intervals: Vec<(f64, f64)> = Vec::new();
    let mut whole_circle = false;
    for pose in poses {
        let rays: Vec<Vec3> = edges.iter().map(|&p| lens.world_ray(pose, p)).collect();
        for d in &rays {
            if let Some((u, v)) = plane_point(projection, *d) {
                v_range = (v_range.0.min(v), v_range.1.max(v));
                u_range = (u_range.0.min(u), u_range.1.max(u));
            }
        }
        let up = stitch_records(lens, pose, [0.0, -1.0, 0.0]);
        let down = stitch_records(lens, pose, [0.0, 1.0, 0.0]);
        if projection == Projection::Equirectangular {
            if up {
                v_range.0 = v_range.0.min(-pi / 2.0);
            }
            if down {
                v_range.1 = v_range.1.max(pi / 2.0);
            }
        }
        if up || down {
            whole_circle = true;
            continue;
        }
        let axis = pose.axis();
        let centre = axis[0].atan2(axis[2]);
        let offsets: Vec<f64> = rays
            .iter()
            .map(|d| principal(d[0].atan2(d[2]) - centre))
            .collect();
        let lo = offsets.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = offsets.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        intervals.push((centre + lo, centre + hi));
    }
    if projection == Projection::Rectilinear {
        return StitchLayout {
            projection,
            scale,
            u0: u_range.0,
            v0: v_range.0,
            width: (scale * (u_range.1 - u_range.0) + STITCH_SLACK).floor() as usize + 1,
            height: (scale * (v_range.1 - v_range.0) + STITCH_SLACK).floor() as usize + 1,
            wrap: false,
        };
    }
    // The uncovered arcs of the circle, from the intervals' starts in [−π, π).
    let mut pieces: Vec<(f64, f64)> = Vec::new();
    for &(lo, hi) in &intervals {
        if hi - lo >= tau {
            whole_circle = true;
        }
        let mut a = principal(lo);
        if a == pi {
            a = -pi;
        }
        let b = a + (hi - lo);
        if b <= pi {
            pieces.push((a, b));
        } else {
            pieces.push((a, pi));
            pieces.push((-pi, b - tau));
        }
    }
    pieces.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.total_cmp(&y.1)));
    let mut widest: Option<(f64, f64)> = None; // (width, centre)
    if !whole_circle && !pieces.is_empty() {
        let mut reach = pieces[0].1;
        for &(a, b) in &pieces[1..] {
            if a > reach {
                let width = a - reach;
                if widest.is_none_or(|(w, _)| width > w) {
                    widest = Some((width, (a + reach) / 2.0));
                }
            }
            reach = reach.max(b);
        }
        let around = pieces[0].0 + tau - reach;
        if around > 0.0 && widest.is_none_or(|(w, _)| around >= w) {
            // The arc across ±π wins ties with earlier arcs only when strictly wider: keep the
            // production's rule, which starts from the arc across ±π.
            widest = Some((around, (reach + pieces[0].0 + tau) / 2.0));
        }
    }
    match widest {
        None => StitchLayout {
            projection,
            scale,
            u0: -pi,
            v0: v_range.0,
            width: (tau * scale + STITCH_SLACK).floor() as usize,
            height: (scale * (v_range.1 - v_range.0) + STITCH_SLACK).floor() as usize + 1,
            wrap: true,
        },
        Some((_, gap)) => {
            let gap = principal(gap);
            let (mut u0, mut u1) = (f64::INFINITY, f64::NEG_INFINITY);
            for &(lo, hi) in &intervals {
                let mut shift = 0.0;
                while lo + shift > gap {
                    shift -= tau;
                }
                while lo + shift <= gap - tau {
                    shift += tau;
                }
                u0 = u0.min(lo + shift);
                u1 = u1.max(hi + shift);
            }
            StitchLayout {
                projection,
                scale,
                u0,
                v0: v_range.0,
                width: (scale * (u1 - u0) + STITCH_SLACK).floor() as usize + 1,
                height: (scale * (v_range.1 - v_range.0) + STITCH_SLACK).floor() as usize + 1,
                wrap: false,
            }
        }
    }
}

/// Which cells of side `cell` over the layout's bounding rectangle are covered: every one of
/// their four outermost pixels (within the rectangle) recorded by some frame. Row-major, with the
/// grid's width and height.
pub fn stitch_coverage(
    layout: &StitchLayout,
    lens: &Lens,
    poses: &[Pose],
    cell: usize,
) -> (usize, usize, Vec<bool>) {
    let (gw, gh) = (layout.width.div_ceil(cell), layout.height.div_ceil(cell));
    let mut covered = vec![false; gw * gh];
    for j in 0..gh {
        for i in 0..gw {
            let xs = [i * cell, (i * cell + cell - 1).min(layout.width - 1)];
            let ys = [j * cell, (j * cell + cell - 1).min(layout.height - 1)];
            covered[j * gw + i] = ys.iter().all(|&y| {
                xs.iter().all(|&x| {
                    let d = layout.direction(x as f64, y as f64);
                    poses.iter().any(|pose| stitch_records(lens, pose, d))
                })
            });
        }
    }
    (gw, gh, covered)
}

/// The largest rectangle of `true` cells: the largest area, then the smallest top row, bottom row
/// and left column. `(x, y, width, height)`, all zero when none. By every pair of rows.
pub fn largest_covered_rectangle(
    cells: &[bool],
    width: usize,
    height: usize,
) -> (usize, usize, usize, usize) {
    let mut best = (0, (0, 0, 0, 0));
    for top in 0..height {
        let mut open = vec![true; width];
        for bottom in top..height {
            for (x, o) in open.iter_mut().enumerate() {
                *o &= cells[bottom * width + x];
            }
            let rows = bottom - top + 1;
            let mut x = 0;
            while x < width {
                if !open[x] {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < width && open[x] {
                    x += 1;
                }
                let area = (x - start) * rows;
                if area > best.0 {
                    best = (area, (start, top, x - start, rows));
                }
            }
        }
    }
    best.1
}

/// What a perfect stitch shows over `rect` (`x, y, width, height` of the layout's bounding
/// rectangle): the scene, with `subject` over it, along each pixel's direction, times `gain`.
pub fn render_stitch(
    scene: &Scene,
    layout: &StitchLayout,
    rect: (usize, usize, usize, usize),
    subject: Option<&Subject>,
    gain: [f64; 3],
) -> Frame {
    let (x0, y0, w, h) = rect;
    let mut rgb = [vec![0.0; w * h], vec![0.0; w * h], vec![0.0; w * h]];
    for y in 0..h {
        for x in 0..w {
            let d = layout.direction((x0 + x) as f64, (y0 + y) as f64);
            let mut value = scene.radiance(d);
            if let Some(subject) = subject {
                value = subject.over(d, value);
            }
            for c in 0..3 {
                rgb[c][y * w + x] = value[c] * gain[c];
            }
        }
    }
    Frame {
        width: w,
        height: h,
        rgb,
    }
}
