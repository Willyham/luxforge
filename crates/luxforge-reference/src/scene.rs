//! The scene scale Basic's Exposure and white balance act on (`docs/design/basic-scene.md`).
//!
//! A rendered image's linear values are display-referred: a camera or editor already rendered them
//! through a curve that compresses its highlights towards white. An Exposure or a white balance
//! that multiplies those values clips the highlights hard and tints white. Acting on a scene scale
//! instead, a gain applied after the rendering is undone and before it is redone, keeps white
//! white and rolls highlights off, as Lightroom Classic's controls on a rendered JPEG were measured
//! to (`docs/design/lightroom-alignment.md`, "Basic realigned").
//!
//! The scale is the log2 of a linear value plus a frozen lift `h`, piecewise linear in the log2
//! value over [`KNOTS`]: `S(x) = log2(x) + h(log2(x))`. Below the first knot `h` is 0, so the
//! deepest shadows take an exact gain; above the last, at linear 1, `h` stays at its last value, so
//! values past white (a RAW photo's headroom) take an exact gain too. Between, `h` falls through the
//! midtones, which take about 1.4 times the stops, and rises steeply just below white, where the
//! highlights roll off. `S` is strictly increasing, so it inverts exactly segment by segment. The
//! lift is fitted to measured outputs alone: Lightroom Classic's exports of the round 1 grey wedge
//! at ten Exposure values, by least squares with a smoothness penalty, `S(out) - S(in) = EV` for
//! every unclipped code.
//!
//! Zero and negative values have no log: they keep their linear value on the scene scale, so a
//! shift multiplies them by its gain, continuous with the exact gain of the deepest shadows.
//!
//! Exposure shifts every channel by its value in stops. Temperature and Tint shift each channel by
//! its own frozen stops ([`TEMPERATURE_STOPS`], [`TINT_STOPS`]), measured the same way, so white
//! stays white where the curve is steep and the cast lives in the midtones and shadows.

/// The lift's knots, in log2 of linear light: every 0.375 stop from -9 to -1.5, then every 0.125
/// stop to 0, linear white.
pub const KNOTS: [f64; 33] = [
    -9.0, -8.625, -8.25, -7.875, -7.5, -7.125, -6.75, -6.375, -6.0, -5.625, -5.25, -4.875, -4.5,
    -4.125, -3.75, -3.375, -3.0, -2.625, -2.25, -1.875, -1.5, -1.375, -1.25, -1.125, -1.0, -0.875,
    -0.75, -0.625, -0.5, -0.375, -0.25, -0.125, 0.0,
];

/// The lift `h` at each knot, in stops.
pub const LIFT: [f64; 33] = [
    0.0, 0.0044, 0.0299, 0.0461, 0.0306, -0.0078, -0.0605, -0.1373, -0.2248, -0.3224, -0.4384,
    -0.5707, -0.7072, -0.8218, -0.9321, -1.0384, -1.1378, -1.1815, -1.274, -1.3646, -1.4104,
    -1.4165, -1.4199, -1.4188, -1.4096, -1.3848, -1.3315, -1.2425, -1.1329, -0.9404, -0.566,
    -0.1909, 0.1841,
];

/// The lift at log2 value `u`.
pub fn lift(u: f64) -> f64 {
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

/// The scene log `S(x)` of a positive linear value.
pub fn scene_log(x: f64) -> f64 {
    let u = x.log2();
    u + lift(u)
}

/// The positive linear value whose scene log is `z`: `S`'s exact inverse.
pub fn from_scene_log(z: f64) -> f64 {
    let first = KNOTS[0] + LIFT[0];
    let n = KNOTS.len();
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

/// `x` on the linear scene scale, `2^S(x)`; zero and negative values as they are.
pub fn to_scene(x: f64) -> f64 {
    if x > 0.0 { scene_log(x).exp2() } else { x }
}

/// The linear value whose scene-scale value is `s`: [`to_scene`]'s inverse.
pub fn from_scene(s: f64) -> f64 {
    if s > 0.0 { from_scene_log(s.log2()) } else { s }
}

/// `x` moved by `stops` on the scene scale: Basic's Exposure on one channel.
pub fn shift(x: f64, stops: f64) -> f64 {
    if x > 0.0 {
        from_scene_log(scene_log(x) + stops)
    } else {
        x * stops.exp2()
    }
}

/// The slider values at which Temperature's and Tint's stops are frozen.
pub const WHITE_BALANCE_VALUES: [f64; 11] = [
    -100.0, -75.0, -50.0, -25.0, -10.0, 0.0, 10.0, 25.0, 50.0, 75.0, 100.0,
];

/// Temperature's shift of each channel, red, green and blue, in stops on the scene scale, at each
/// of [`WHITE_BALANCE_VALUES`]: fitted per channel to Lightroom Classic's exports of the round 1
/// grey wedge and patch grid at each Temperature value, by the least mean code difference.
pub const TEMPERATURE_STOPS: [[f64; 11]; 3] = [
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

/// Tint's shift of each channel in stops, fitted as [`TEMPERATURE_STOPS`] are.
pub const TINT_STOPS: [[f64; 11]; 3] = [
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

/// One channel's stops at slider `value` by linear interpolation over [`WHITE_BALANCE_VALUES`],
/// the end segment continued outside them.
fn stops_at(table: &[f64; 11], value: f64) -> f64 {
    let v = &WHITE_BALANCE_VALUES;
    let i = v
        .partition_point(|k| *k <= value)
        .saturating_sub(1)
        .min(v.len() - 2);
    table[i] + (value - v[i]) / (v[i + 1] - v[i]) * (table[i + 1] - table[i])
}

/// The stops Temperature and Tint shift each channel by, which add.
pub fn white_balance_stops(temperature: f64, tint: f64) -> [f64; 3] {
    [0, 1, 2].map(|c| stops_at(&TEMPERATURE_STOPS[c], temperature) + stops_at(&TINT_STOPS[c], tint))
}

/// Basic's Temperature, Tint and Exposure on one linear colour: each channel shifted on the scene
/// scale by its white-balance stops plus the Exposure.
pub fn apply(temperature: f64, tint: f64, exposure: f64, rgb: [f64; 3]) -> [f64; 3] {
    let stops = white_balance_stops(temperature, tint);
    [0, 1, 2].map(|c| shift(rgb[c], stops[c] + exposure))
}

/// The Temperature and Tint, unrounded, that make a colour of positive channels neutral: each
/// channel's scene log plus its stops equal, by Newton's method from neutral with a central
/// difference Jacobian, `None` when it does not converge within the slider's range.
pub fn neutral_settings(rgb: [f64; 3]) -> Option<(f64, f64)> {
    if rgb.iter().any(|c| !(c.is_finite() && *c > 0.0)) {
        return None;
    }
    let logs = rgb.map(scene_log);
    let residual = |t: f64, n: f64| {
        let s = white_balance_stops(t, n);
        let z = [0, 1, 2].map(|c| logs[c] + s[c]);
        (z[0] - z[1], z[2] - z[1])
    };
    let (mut t, mut n) = (0.0_f64, 0.0_f64);
    for _ in 0..64 {
        let (a, b) = residual(t, n);
        if a.hypot(b) < 1e-9 {
            return (t.abs() <= 100.0 + 1e-9 && n.abs() <= 100.0 + 1e-9).then_some((t, n));
        }
        let h = 1e-3;
        let (at1, bt1) = residual(t + h, n);
        let (at0, bt0) = residual(t - h, n);
        let (an1, bn1) = residual(t, n + h);
        let (an0, bn0) = residual(t, n - h);
        let j = [
            [(at1 - at0) / (2.0 * h), (an1 - an0) / (2.0 * h)],
            [(bt1 - bt0) / (2.0 * h), (bn1 - bn0) / (2.0 * h)],
        ];
        let det = j[0][0] * j[1][1] - j[0][1] * j[1][0];
        if !det.is_finite() || det.abs() < 1e-15 {
            return None;
        }
        t -= (j[1][1] * a - j[0][1] * b) / det;
        n -= (-j[1][0] * a + j[0][0] * b) / det;
        if !(t.is_finite() && n.is_finite()) || t.abs() > 200.0 || n.abs() > 200.0 {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_is_strictly_increasing_and_inverts() {
        for pair in KNOTS.windows(2).zip(LIFT.windows(2)) {
            let ((k0, k1), (h0, h1)) = ((pair.0[0], pair.0[1]), (pair.1[0], pair.1[1]));
            assert!(k1 + h1 > k0 + h0, "segment at {k0}");
        }
        for i in 0..=4000 {
            let x = 2f64.powf(-12.0 + 15.0 * f64::from(i) / 4000.0);
            let back = from_scene_log(scene_log(x));
            assert!((back / x - 1.0).abs() < 1e-12, "{x} -> {back}");
        }
    }

    #[test]
    fn deep_shadows_and_values_past_white_take_an_exact_gain() {
        for x in [1e-4, 1e-3, 4.0] {
            assert!((shift(x, -1.0) / x - 0.5).abs() < 1e-12, "{x}");
        }
        assert_eq!(shift(0.0, 3.0), 0.0);
        assert_eq!(shift(-0.25, 1.0), -0.5);
        assert_eq!(shift(0.3, 0.0), from_scene_log(scene_log(0.3)));
    }

    #[test]
    fn neutral_white_balance_is_the_identity_and_a_cast_is_neutralized() {
        assert_eq!(white_balance_stops(0.0, 0.0), [0.0; 3]);
        assert_eq!(apply(0.0, 0.0, 0.0, [0.2, 0.4, 0.6])[1], shift(0.4, 0.0));
        // A warm, green cast, made by Temperature 30 and Tint -20 on a grey, is neutralized by a
        // cooler, more magenta setting; the tables are not odd, so not by exactly the negation.
        let cast = apply(30.0, -20.0, 0.0, [0.18; 3]);
        let (t, n) = neutral_settings(cast).expect("a solved cast");
        let back = apply(t, n, 0.0, cast);
        assert!((back[0] - back[1]).abs() < 1e-9 && (back[2] - back[1]).abs() < 1e-9);
        assert!(t < -15.0 && n > 15.0, "{t}, {n}");
    }
}
