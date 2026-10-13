//! The rotating-camera model a panorama is registered under: every frame is taken by one camera
//! turning about its optical centre, so a frame is one [`Rotation`], and every frame shares one
//! [`Lens`], a focal length and a radial term.
//!
//! Coordinates are full-resolution upright frame pixels: pixel `(x, y)`'s centre is `(x, y)`, and
//! the principal point is the frame's centre `c = ((W − 1)/2, (H − 1)/2)`. Camera and world
//! directions are right-handed with `x` right, `y` down and `z` forward; the camera direction
//! `(u, v, 1)` lands on the ideal point `c + f·(u, v)`, which the lens records at
//! `c + p·(1 + k·|p|²/R²)` for the ideal offset `p`, with `R = √(W² + H²)/2`, half the frame's
//! diagonal (`k < 0` is barrel distortion). A [`Rotation`] takes a world direction to the camera's.

use super::linalg::{self, M3};
use serde::{Deserialize, Serialize};

/// The largest radial term the fit reaches either way. Within it every recorded point of the
/// frame has exactly one ideal point: barrel distortion `k < 0` turns back at the ideal radius
/// `R/√(3|k|)`, whose recorded radius `(2/3)·R/√(3|k|)` reaches the corners (`R`) only while
/// `|k| ≤ 4/27`.
pub const MAX_RADIAL: f64 = 0.14;
/// The most Newton steps of the radial inversion. From the recorded radius it converges
/// quadratically, to `f64` precision in under eight steps for every `|k| ≤ MAX_RADIAL` within
/// the frame.
const UNDISTORT_STEPS: usize = 16;

/// A frame's orientation: the rotation from world directions to the camera's, row-major. Its rows
/// are the camera's right, down and forward axes in the world.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rotation(pub [[f64; 3]; 3]);

impl Rotation {
    pub const IDENTITY: Self = Self(linalg::IDENTITY);

    /// The camera direction of world direction `world`.
    #[inline]
    pub fn to_camera(&self, world: [f64; 3]) -> [f64; 3] {
        linalg::apply(&self.0, world)
    }

    /// The world direction of camera direction `camera`.
    #[inline]
    pub fn to_world(&self, camera: [f64; 3]) -> [f64; 3] {
        let m = &self.0;
        [
            m[0][0] * camera[0] + m[1][0] * camera[1] + m[2][0] * camera[2],
            m[0][1] * camera[0] + m[1][1] * camera[1] + m[2][1] * camera[2],
            m[0][2] * camera[0] + m[1][2] * camera[1] + m[2][2] * camera[2],
        ]
    }

    /// The optical axis in the world.
    pub fn forward(&self) -> [f64; 3] {
        self.0[2]
    }

    /// The camera's image down direction in the world.
    pub fn down(&self) -> [f64; 3] {
        self.0[1]
    }

    /// The camera's image right direction in the world.
    pub fn right(&self) -> [f64; 3] {
        self.0[0]
    }

    /// `self · other`: `other` first, then `self`.
    pub fn then(&self, other: &Rotation) -> Rotation {
        Rotation(linalg::mul(&self.0, &other.0))
    }

    pub fn inverse(&self) -> Rotation {
        Rotation(linalg::transpose(&self.0))
    }

    /// The rotation between two frames: camera directions of `from` to camera directions of
    /// `self`, `self · fromᵀ`.
    pub fn relative_to(&self, from: &Rotation) -> Rotation {
        Rotation(linalg::mul(&self.0, &linalg::transpose(&from.0)))
    }

    /// The angle of the rotation, in radians.
    pub fn angle(&self) -> f64 {
        let q = linalg::quaternion(&self.0);
        2.0 * (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt().atan2(q[3])
    }
}

/// The lens every frame shares: its frame size, focal length and radial term. See the module
/// documentation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lens {
    /// The upright full-resolution frame.
    pub width: u32,
    pub height: u32,
    /// In full-resolution pixels.
    pub focal_px: f64,
    /// `k`, at most [`MAX_RADIAL`] either way.
    pub radial: f64,
}

impl Lens {
    pub fn centre(&self) -> (f64, f64) {
        (
            (f64::from(self.width) - 1.0) / 2.0,
            (f64::from(self.height) - 1.0) / 2.0,
        )
    }

    /// `R`, half the frame's diagonal: the radius the radial term is measured against.
    pub fn radial_scale(&self) -> f64 {
        (f64::from(self.width).powi(2) + f64::from(self.height).powi(2)).sqrt() / 2.0
    }

    /// The frame's diagonal in pixels.
    pub fn diagonal(&self) -> f64 {
        2.0 * self.radial_scale()
    }

    /// Where an ideal offset from the centre records, as an offset from the centre.
    #[inline]
    pub fn distort(&self, ideal: [f64; 2]) -> [f64; 2] {
        let s = 1.0
            + self.radial * (ideal[0] * ideal[0] + ideal[1] * ideal[1])
                / self.radial_scale().powi(2);
        [ideal[0] * s, ideal[1] * s]
    }

    /// The ideal offset a recorded offset from the centre shows, by Newton's method on the radius.
    #[inline]
    pub fn undistort(&self, recorded: [f64; 2]) -> [f64; 2] {
        let rd = (recorded[0] * recorded[0] + recorded[1] * recorded[1]).sqrt();
        if self.radial == 0.0 || rd == 0.0 {
            return recorded;
        }
        let k = self.radial / self.radial_scale().powi(2);
        let mut r = rd;
        for _ in 0..UNDISTORT_STEPS {
            let slope = 1.0 + 3.0 * k * r * r;
            if slope <= 0.05 {
                break;
            }
            let step = (r * (1.0 + k * r * r) - rd) / slope;
            r -= step;
            if step.abs() <= 1e-14 * rd {
                break;
            }
        }
        [recorded[0] * r / rd, recorded[1] * r / rd]
    }

    /// The camera direction `(u, v, 1)` a recorded pixel shows (not normalized).
    #[inline]
    pub fn ray(&self, pixel: (f64, f64)) -> [f64; 3] {
        let (cx, cy) = self.centre();
        let ideal = self.undistort([pixel.0 - cx, pixel.1 - cy]);
        [ideal[0] / self.focal_px, ideal[1] / self.focal_px, 1.0]
    }

    /// Where a camera direction records, or `None` when it is not in front of the camera.
    #[inline]
    pub fn project(&self, camera: [f64; 3]) -> Option<(f64, f64)> {
        if camera[2] <= 1e-9 * linalg::norm(camera) {
            return None;
        }
        let (cx, cy) = self.centre();
        let recorded = self.distort([
            self.focal_px * camera[0] / camera[2],
            self.focal_px * camera[1] / camera[2],
        ]);
        Some((cx + recorded[0], cy + recorded[1]))
    }

    /// The world direction (unit) a recorded pixel of a frame at `rotation` shows.
    pub fn world_ray(&self, rotation: &Rotation, pixel: (f64, f64)) -> [f64; 3] {
        linalg::normalize(rotation.to_world(self.ray(pixel)))
    }

    /// Where a world direction records in a frame at `rotation`; it may fall outside the frame.
    pub fn world_project(&self, rotation: &Rotation, world: [f64; 3]) -> Option<(f64, f64)> {
        self.project(rotation.to_camera(world))
    }

    /// Whether a point lies within the frame's pixel centres.
    pub fn contains(&self, p: (f64, f64)) -> bool {
        p.0 >= 0.0
            && p.1 >= 0.0
            && p.0 <= f64::from(self.width) - 1.0
            && p.1 <= f64::from(self.height) - 1.0
    }

    /// The frame's four corner pixel centres, clockwise from the top left.
    pub fn corners(&self) -> [(f64, f64); 4] {
        let (w, h) = (f64::from(self.width) - 1.0, f64::from(self.height) - 1.0);
        [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]
    }

    /// Where a pixel of frame `from` lands in frame `to`.
    pub fn transfer(
        &self,
        to: &Rotation,
        from: &Rotation,
        pixel: (f64, f64),
    ) -> Option<(f64, f64)> {
        self.project(to.relative_to(from).to_camera(self.ray(pixel)))
    }
}

pub(crate) fn rotation_from(m: M3) -> Rotation {
    Rotation(linalg::orthonormalize(&m))
}
