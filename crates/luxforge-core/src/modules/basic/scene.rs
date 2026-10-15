//! The Basic module's scene unit: Temperature, Tint and Exposure as one shift of each channel on
//! the scene scale (`docs/design/basic-scene.md`), and its GPU program (`scene.wgsl`).
//!
//! This file is the production transcription of that document and of the independent `f64`
//! reference at `crates/luxforge-reference/src/scene.rs`; every table here is named, and holds the
//! same values, as the table of the same name there, and the unit evaluates in `f64` so it is held
//! to the reference within `1e-9`. White balance shifts each channel by its own frozen stops,
//! Exposure every channel by its value, and the two add on the scene scale, so white balance before
//! exposure is one shift: the frozen order is kept exactly.
use crate::modules::PointwiseColor;
use crate::render::gpu::{GpuDescription, GpuProgram, GpuProgramKind};

/// The scene unit's GPU program: each channel's shift in stops, three words.
pub(crate) static PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_basic_scene",
    source: include_str!("scene.wgsl"),
    kind: GpuProgramKind::Colour,
    words: 3,
    enabled: true,
};

/// The lift's knots, in log2 of linear light.
const KNOTS: [f64; 33] = [
    -9.0, -8.625, -8.25, -7.875, -7.5, -7.125, -6.75, -6.375, -6.0, -5.625, -5.25, -4.875, -4.5,
    -4.125, -3.75, -3.375, -3.0, -2.625, -2.25, -1.875, -1.5, -1.375, -1.25, -1.125, -1.0, -0.875,
    -0.75, -0.625, -0.5, -0.375, -0.25, -0.125, 0.0,
];

/// The lift at each knot, in stops.
const LIFT: [f64; 33] = [
    0.0, 0.0044, 0.0299, 0.0461, 0.0306, -0.0078, -0.0605, -0.1373, -0.2248, -0.3224, -0.4384,
    -0.5707, -0.7072, -0.8218, -0.9321, -1.0384, -1.1378, -1.1815, -1.274, -1.3646, -1.4104,
    -1.4165, -1.4199, -1.4188, -1.4096, -1.3848, -1.3315, -1.2425, -1.1329, -0.9404, -0.566,
    -0.1909, 0.1841,
];

/// The slider values at which Temperature's and Tint's stops are frozen.
const WHITE_BALANCE_VALUES: [f64; 11] = [
    -100.0, -75.0, -50.0, -25.0, -10.0, 0.0, 10.0, 25.0, 50.0, 75.0, 100.0,
];

/// Temperature's stops for red, green and blue at each of [`WHITE_BALANCE_VALUES`].
const TEMPERATURE_STOPS: [[f64; 11]; 3] = [
    [
        0.3, -0.26, -0.4, -0.25, -0.1, 0.0, 0.33, 0.76, 1.38, 1.82, 2.12,
    ],
    [
        0.87, 0.58, 0.34, 0.15, 0.06, 0.0, 0.18, 0.44, 0.79, 1.06, 1.25,
    ],
    [
        4.1, 2.77, 1.65, 0.75, 0.28, 0.0, -0.03, -0.07, -0.13, -0.16, -0.18,
    ],
];

/// Tint's stops for red, green and blue at each of [`WHITE_BALANCE_VALUES`].
const TINT_STOPS: [[f64; 11]; 3] = [
    [
        -0.4, -0.12, 0.0, 0.02, 0.01, 0.0, 0.05, 0.14, 0.31, 0.47, 0.6,
    ],
    [
        1.33, 0.87, 0.49, 0.21, 0.07, 0.0, -0.01, -0.02, -0.05, -0.09, -0.11,
    ],
    [
        -0.16, -0.11, -0.07, -0.03, -0.01, 0.0, 0.08, 0.22, 0.55, 1.03, 1.75,
    ],
];

fn lift(u: f64) -> f64 {
    if u <= KNOTS[0] {
        return LIFT[0];
    }
    if u >= KNOTS[KNOTS.len() - 1] {
        return LIFT[LIFT.len() - 1];
    }
    let i = KNOTS.partition_point(|k| *k <= u) - 1;
    let t = (u - KNOTS[i]) / (KNOTS[i + 1] - KNOTS[i]);
    LIFT[i] + t * (LIFT[i + 1] - LIFT[i])
}

/// The scene log of a positive linear value.
fn scene_log(x: f64) -> f64 {
    let u = x.log2();
    u + lift(u)
}

/// The positive linear value whose scene log is `z`.
pub(super) fn from_scene_log(z: f64) -> f64 {
    let n = KNOTS.len();
    let first = KNOTS[0] + LIFT[0];
    let last = KNOTS[n - 1] + LIFT[n - 1];
    let u = if z <= first {
        z - LIFT[0]
    } else if z >= last {
        z - LIFT[n - 1]
    } else {
        let i = (0..n - 1)
            .find(|&i| z < KNOTS[i + 1] + LIFT[i + 1])
            .unwrap_or(n - 2);
        let (z0, z1) = (KNOTS[i] + LIFT[i], KNOTS[i + 1] + LIFT[i + 1]);
        KNOTS[i] + (z - z0) / (z1 - z0) * (KNOTS[i + 1] - KNOTS[i])
    };
    u.exp2()
}

/// `x` shifted by `stops` on the scene scale; zero and negative values take the plain gain.
fn shift(x: f64, stops: f64) -> f64 {
    if x > 0.0 {
        from_scene_log(scene_log(x) + stops)
    } else {
        x * stops.exp2()
    }
}

fn stops_at(table: &[f64; 11], value: f64) -> f64 {
    let v = &WHITE_BALANCE_VALUES;
    let i = v
        .partition_point(|k| *k <= value)
        .saturating_sub(1)
        .min(v.len() - 2);
    table[i] + (value - v[i]) / (v[i + 1] - v[i]) * (table[i + 1] - table[i])
}

/// The stops Temperature and Tint shift red, green and blue by.
pub(super) fn white_balance_stops(temperature: f64, tint: f64) -> [f64; 3] {
    [0, 1, 2].map(|c| stops_at(&TEMPERATURE_STOPS[c], temperature) + stops_at(&TINT_STOPS[c], tint))
}

/// The scene log of each channel of a colour of positive channels.
pub(super) fn scene_logs(rgb: [f64; 3]) -> [f64; 3] {
    rgb.map(scene_log)
}

/// Temperature, Tint and Exposure on each channel of linear sRGB, unclamped.
#[derive(Debug)]
pub(super) struct Scene {
    temperature: f64,
    tint: f64,
    exposure: f64,
    /// Each channel's shift in stops: its white-balance stops plus the Exposure.
    stops: [f64; 3],
}

impl Scene {
    pub(super) fn new(temperature: f64, tint: f64, exposure: f64) -> Self {
        let balance = white_balance_stops(temperature, tint);
        Self {
            temperature,
            tint,
            exposure,
            stops: balance.map(|s| s + exposure),
        }
    }
}

impl PointwiseColor for Scene {
    fn identity(&self) -> crate::OperationIdentity {
        crate::OperationIdentity::new(
            "basic.scene",
            [
                self.temperature.to_bits(),
                self.tint.to_bits(),
                self.exposure.to_bits(),
            ],
        )
    }

    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        if self.stops == [0.0; 3] {
            return;
        }
        for pixel in rgb {
            for (channel, stops) in pixel.iter_mut().zip(self.stops) {
                *channel = shift(f64::from(*channel), stops) as f32;
            }
        }
    }

    fn is_finite(&self) -> bool {
        self.stops.iter().all(|s| s.is_finite() && s.abs() < 64.0)
    }

    /// The stored values, exactly: the shifts are a pure function of them.
    fn describe(&self) -> String {
        format!(
            "scene(temperature:{:+}, tint:{:+}, exposure:{:+})",
            self.temperature, self.tint, self.exposure
        )
    }

    fn gpu(&self) -> Option<GpuDescription> {
        Some(GpuDescription::new(
            &PROGRAM,
            self.stops.map(|s| (s as f32).to_bits()).to_vec(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_reference::scene as reference;

    #[test]
    fn the_tables_are_the_references() {
        assert_eq!(KNOTS, reference::KNOTS);
        assert_eq!(LIFT, reference::LIFT);
        assert_eq!(WHITE_BALANCE_VALUES, reference::WHITE_BALANCE_VALUES);
        assert_eq!(TEMPERATURE_STOPS, reference::TEMPERATURE_STOPS);
        assert_eq!(TINT_STOPS, reference::TINT_STOPS);
    }

    /// The unit against the independent reference on a dense ramp of linear values past white and
    /// below black, at every corner and the middle of the three sliders.
    #[test]
    fn the_unit_is_the_reference() {
        let inputs: Vec<f32> = (0..=600)
            .map(|i| -0.1 + 1.6 * i as f32 / 600.0)
            .chain([1e-5, 4.0, 0.0])
            .collect();
        for temperature in [-100.0, -33.0, 0.0, 41.0, 100.0] {
            for tint in [-100.0, 0.0, 57.0, 100.0] {
                for exposure in [-5.0, -1.25, 0.0, 0.4, 5.0] {
                    let unit = Scene::new(temperature, tint, exposure);
                    for x in &inputs {
                        let mut row = [[*x, *x * 0.5, *x * 0.25]];
                        unit.apply_row(0, 0, &mut row);
                        let expected = reference::apply(
                            temperature,
                            tint,
                            exposure,
                            [f64::from(*x), f64::from(*x * 0.5), f64::from(*x * 0.25)],
                        );
                        for c in 0..3 {
                            let tolerance = 1e-6 + 1e-6 * expected[c].abs();
                            assert!(
                                (f64::from(row[0][c]) - expected[c]).abs() <= tolerance,
                                "({temperature}, {tint}, {exposure}) {x} channel {c}: {} against {}",
                                row[0][c],
                                expected[c]
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn neutral_settings_are_the_identity_and_white_stays_white() {
        let mut row = [[0.0, 0.25, 1.0], [-0.5, 2.0, 0.18]];
        let input = row;
        Scene::new(0.0, 0.0, 0.0).apply_row(0, 0, &mut row);
        assert_eq!(row, input);
        // White, warmed fully, keeps every channel within a few codes of white.
        let mut white = [[1.0f32; 3]];
        Scene::new(100.0, 0.0, 0.0).apply_row(0, 0, &mut white);
        assert!(white[0].iter().all(|c| *c > 0.93), "{white:?}");
    }
}
