//! How a panorama's output plane maps to world directions (`docs/design/panorama-merge.md`,
//! "Projection and output size").
//!
//! The world is the registration's levelled world: `x` right, `y` down, `z` forward, with yaw 0
//! at the centre of the frames' arc ([`super::PanoramaRegistration`]). Every projection is
//! centred on the world's forward direction, so the horizon is horizontal and vertical lines stay
//! vertical. A plane point `(u, v)` is:
//!
//! - **Perspective**: the direction `(u, v, 1)`, so straight lines stay straight. Only directions
//!   in front of the plane (`z > 0`) have a point.
//! - **Cylindrical**: `u` the yaw in radians, `v` the height `y/√(x² + z²)` on a unit cylinder; the
//!   direction `(sin u, v, cos u)`. The poles have no point.
//! - **Spherical** (equirectangular): `u` the yaw, `v` the latitude below the horizon
//!   `asin y`; the direction `(cos v·sin u, sin v, cos v·cos u)`.
//!
//! **Fields.** Perspective is refused ([`super::Refusal::ExcessiveFieldOfView`]) when any part of
//! a frame lies more than [`PERSPECTIVE_MAX_HALF_FIELD_DEG`] from the centre along either plane
//! axis (`|u|` or `|v|` beyond `tan 60°`) or behind the plane. Cylindrical is refused when a frame
//! reaches more than [`CYLINDRICAL_MAX_LATITUDE_DEG`] from the horizon, where its height runs to
//! infinity. Spherical holds every direction.
//!
//! **Auto** resolves to Perspective when the field is at most [`AUTO_PERSPECTIVE_FIELD_DEG`] both
//! ways and Perspective holds it, otherwise to Cylindrical when no frame reaches more than
//! [`AUTO_CYLINDRICAL_LATITUDE_DEG`] from the horizon, otherwise to Spherical.

use super::linalg::{self, V3};
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Perspective holds directions up to this angle from its centre along either plane axis.
pub const PERSPECTIVE_MAX_HALF_FIELD_DEG: f64 = 60.0;
/// Cylindrical holds directions up to this latitude either side of the horizon.
pub const CYLINDRICAL_MAX_LATITUDE_DEG: f64 = 70.0;
/// Auto chooses Perspective for fields up to this wide and tall.
pub const AUTO_PERSPECTIVE_FIELD_DEG: f64 = 90.0;
/// Auto chooses Cylindrical while every frame stays within this latitude of the horizon.
pub const AUTO_CYLINDRICAL_LATITUDE_DEG: f64 = 55.0;

/// A resolved projection. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Projection {
    Perspective,
    Cylindrical,
    Spherical,
}

/// The projection a merge asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectionChoice {
    /// Chosen from the registered field of view; see the module documentation.
    #[default]
    Auto,
    Perspective,
    Cylindrical,
    Spherical,
}

impl ProjectionChoice {
    /// The projection asked for, or `None` for Auto.
    pub fn fixed(self) -> Option<Projection> {
        match self {
            Self::Auto => None,
            Self::Perspective => Some(Projection::Perspective),
            Self::Cylindrical => Some(Projection::Cylindrical),
            Self::Spherical => Some(Projection::Spherical),
        }
    }
}

impl Projection {
    /// Whether `u` is the yaw, periodic with `2π`.
    pub fn is_angular(self) -> bool {
        !matches!(self, Self::Perspective)
    }

    /// The plane point of the unit direction `d`, or `None` where the projection has none.
    #[inline]
    pub fn plane(self, d: V3) -> Option<[f64; 2]> {
        match self {
            Self::Perspective => (d[2] > 1e-9).then(|| [d[0] / d[2], d[1] / d[2]]),
            Self::Cylindrical => {
                let rho = d[0].hypot(d[2]);
                (rho > 1e-12).then(|| [d[0].atan2(d[2]), d[1] / rho])
            }
            Self::Spherical => Some([d[0].atan2(d[2]), d[1].clamp(-1.0, 1.0).asin()]),
        }
    }

    /// The unit direction of plane point `p`.
    #[inline]
    pub fn direction(self, p: [f64; 2]) -> V3 {
        match self {
            Self::Perspective => linalg::normalize([p[0], p[1], 1.0]),
            Self::Cylindrical => {
                let (s, c) = p[0].sin_cos();
                linalg::normalize([s, p[1], c])
            }
            Self::Spherical => {
                let (su, cu) = p[0].sin_cos();
                let (sv, cv) = p[1].sin_cos();
                [cv * su, sv, cv * cu]
            }
        }
    }

    /// The change of the plane point of unit direction `d` along the tangent `t`.
    pub(crate) fn differential(self, d: V3, t: V3) -> [f64; 2] {
        let [x, y, z] = d;
        match self {
            Self::Perspective => [
                (t[0] * z - x * t[2]) / (z * z),
                (t[1] * z - y * t[2]) / (z * z),
            ],
            Self::Cylindrical => {
                let rr = x * x + z * z;
                let rho = rr.sqrt();
                [
                    (z * t[0] - x * t[2]) / rr,
                    t[1] / rho - y * (x * t[0] + z * t[2]) / (rr * rho),
                ]
            }
            Self::Spherical => {
                let rr = x * x + z * z;
                [(z * t[0] - x * t[2]) / rr, t[1] / (1.0 - y * y).sqrt()]
            }
        }
    }
}

/// The field a set of directions spans, in degrees: the yaw extent (360 for a full circle) and the
/// latitude extent, with the largest latitude either side of the horizon.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub horizontal_deg: f64,
    pub vertical_deg: f64,
    /// The highest latitude reached, above or below the horizon.
    pub max_latitude_deg: f64,
    /// The largest `|u|` and `|v|` of Perspective's plane, or `None` where some direction lies
    /// behind it.
    pub perspective_extent: Option<[f64; 2]>,
}

impl Field {
    /// Whether Perspective holds this field.
    pub fn fits_perspective(&self) -> bool {
        let limit = PERSPECTIVE_MAX_HALF_FIELD_DEG.to_radians().tan();
        self.perspective_extent
            .is_some_and(|[u, v]| u <= limit && v <= limit)
    }

    /// Whether Cylindrical holds this field.
    pub fn fits_cylindrical(&self) -> bool {
        self.max_latitude_deg <= CYLINDRICAL_MAX_LATITUDE_DEG
    }

    /// Auto's choice. See the module documentation.
    pub fn auto(&self) -> Projection {
        if self.horizontal_deg <= AUTO_PERSPECTIVE_FIELD_DEG
            && self.vertical_deg <= AUTO_PERSPECTIVE_FIELD_DEG
            && self.fits_perspective()
        {
            Projection::Perspective
        } else if self.max_latitude_deg <= AUTO_CYLINDRICAL_LATITUDE_DEG {
            Projection::Cylindrical
        } else {
            Projection::Spherical
        }
    }
}

/// The latitude below the horizon of unit direction `d`, in radians.
pub(crate) fn latitude(d: V3) -> f64 {
    d[1].clamp(-1.0, 1.0).asin()
}

/// `a` wrapped into `(−π, π]`.
pub(crate) fn wrap_angle(a: f64) -> f64 {
    let mut a = a % (2.0 * PI);
    if a > PI {
        a -= 2.0 * PI;
    } else if a <= -PI {
        a += 2.0 * PI;
    }
    a
}
