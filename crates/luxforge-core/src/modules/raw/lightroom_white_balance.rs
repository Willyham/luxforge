//! Lightroom's absolute RAW white balance (`crs:Temperature`, `crs:Tint`) converted to Luxforge's
//! own RAW `Temperature`/`Tint`, through the illuminant chromaticity both pairs name.
//!
//! Lightroom's pair is Adobe's `dng_temperature`: a correlated colour temperature read against a
//! fixed table of isotemperature lines on the Planckian locus, and a signed perpendicular offset
//! ("tint") along the nearest line, in CIE 1960 `uv`. [`dng_uv_for_temperature_tint`] is a direct
//! translation of that table and its interpolation. Luxforge's own locus is a different
//! blackbody/daylight approximation ([`super::white_balance`]), so the two pairs' numbers do not
//! mean the same thing; what carries across is the `uv` chromaticity itself, which both pairs
//! define in the same CIE 1960 space. [`lightroom_to_luxforge`] takes Lightroom's pair to that
//! `uv` and then asks Luxforge's own inverse, [`super::white_balance::temperature_tint_from_uv`],
//! which temperature and tint on Luxforge's locus reach the same `uv`.
//!
//! This is a value conversion, not a rendering match: Luxforge turns the answer into sensor gains
//! through LibRaw's camera matrix, never through Adobe's DNG colour pipeline, so an imported
//! preset changes the assumed illuminant, not the rendering algorithm.
//!
//! Provenance: `dng_temperature.cpp` from the Adobe DNG SDK (Copyright 2006 Adobe Systems
//! Incorporated; `$Id: //mondo/dng_sdk_1_4/dng_sdk/source/dng_temperature.cpp#1 $`,
//! `$DateTime: 2012/05/30 13:28:51 $`), read from
//! <https://raw.githubusercontent.com/aizvorski/dng_sdk/master/source/dng_temperature.cpp>, an
//! open mirror of the publicly downloadable SDK. The SDK's accompanying license grants a
//! royalty-free right to use, reproduce, modify and distribute this file's C++, including the
//! `kTempTable` data it reimplements; that grant, and the copyright notice above, are recorded
//! here per the file's own NOTICE, without claiming any formal license audit (see AGENTS.md).
//! [`docs/research/lightroom/presets.md`](../../../../../docs/research/lightroom/presets.md)
//! records the same constants and their check against an independent reference.

use super::white_balance::{LocusMiss, temperature_tint_from_uv};
use crate::Error;

/// Scale between CIE 1960 `uv` distance and Adobe's Tint units: `kTintScale` in
/// `dng_temperature.cpp`. Negative because Adobe's Tint increases toward the green (−`v`) side,
/// the opposite sign convention from Luxforge's own tint.
const DNG_TINT_SCALE: f64 = -3000.0;

/// One row of Adobe's isotemperature table: `r` is `1.0e6 / kelvin` (reciprocal megakelvin), `u`
/// and `v` are the blackbody locus point in CIE 1960 `uv`, and `t` is that isotemperature line's
/// slope `dv/du`. Source: `dng_temperature.cpp`'s `kTempTable`, credited there to Wyszecki &
/// Stiles, *Color Science*, second edition, page 228.
struct RobertsonPoint {
    r: f64,
    u: f64,
    v: f64,
    t: f64,
}

/// Adobe's 31-row `kTempTable`, transcribed exactly (same row count, same values, same order) from
/// `dng_temperature.cpp`. Rows run from `r = 0` (an infinitely hot blackbody) to `r = 600`
/// (`1_666.67` K), unevenly spaced: every `10` from `0` to `100`, then every `25` from `100` to
/// `600`.
const DNG_TEMP_TABLE: [RobertsonPoint; 31] = [
    RobertsonPoint {
        r: 0.0,
        u: 0.180_06,
        v: 0.263_52,
        t: -0.243_41,
    },
    RobertsonPoint {
        r: 10.0,
        u: 0.180_66,
        v: 0.265_89,
        t: -0.254_79,
    },
    RobertsonPoint {
        r: 20.0,
        u: 0.181_33,
        v: 0.268_46,
        t: -0.268_76,
    },
    RobertsonPoint {
        r: 30.0,
        u: 0.182_08,
        v: 0.271_19,
        t: -0.285_39,
    },
    RobertsonPoint {
        r: 40.0,
        u: 0.182_93,
        v: 0.274_07,
        t: -0.304_70,
    },
    RobertsonPoint {
        r: 50.0,
        u: 0.183_88,
        v: 0.277_09,
        t: -0.326_75,
    },
    RobertsonPoint {
        r: 60.0,
        u: 0.184_94,
        v: 0.280_21,
        t: -0.351_56,
    },
    RobertsonPoint {
        r: 70.0,
        u: 0.186_11,
        v: 0.283_42,
        t: -0.379_15,
    },
    RobertsonPoint {
        r: 80.0,
        u: 0.187_40,
        v: 0.286_68,
        t: -0.409_55,
    },
    RobertsonPoint {
        r: 90.0,
        u: 0.188_80,
        v: 0.289_97,
        t: -0.442_78,
    },
    RobertsonPoint {
        r: 100.0,
        u: 0.190_32,
        v: 0.293_26,
        t: -0.478_88,
    },
    RobertsonPoint {
        r: 125.0,
        u: 0.194_62,
        v: 0.301_41,
        t: -0.582_04,
    },
    RobertsonPoint {
        r: 150.0,
        u: 0.199_62,
        v: 0.309_21,
        t: -0.704_71,
    },
    RobertsonPoint {
        r: 175.0,
        u: 0.205_25,
        v: 0.316_47,
        t: -0.849_01,
    },
    RobertsonPoint {
        r: 200.0,
        u: 0.211_42,
        v: 0.323_12,
        t: -1.018_2,
    },
    RobertsonPoint {
        r: 225.0,
        u: 0.218_07,
        v: 0.329_09,
        t: -1.216_8,
    },
    RobertsonPoint {
        r: 250.0,
        u: 0.225_11,
        v: 0.334_39,
        t: -1.451_2,
    },
    RobertsonPoint {
        r: 275.0,
        u: 0.232_47,
        v: 0.339_04,
        t: -1.729_8,
    },
    RobertsonPoint {
        r: 300.0,
        u: 0.240_10,
        v: 0.343_08,
        t: -2.063_7,
    },
    RobertsonPoint {
        r: 325.0,
        u: 0.247_02,
        v: 0.346_55,
        t: -2.468_1,
    },
    RobertsonPoint {
        r: 350.0,
        u: 0.255_91,
        v: 0.349_51,
        t: -2.964_1,
    },
    RobertsonPoint {
        r: 375.0,
        u: 0.264_00,
        v: 0.352_00,
        t: -3.581_4,
    },
    RobertsonPoint {
        r: 400.0,
        u: 0.272_18,
        v: 0.354_07,
        t: -4.363_3,
    },
    RobertsonPoint {
        r: 425.0,
        u: 0.280_39,
        v: 0.355_77,
        t: -5.376_2,
    },
    RobertsonPoint {
        r: 450.0,
        u: 0.288_63,
        v: 0.357_14,
        t: -6.726_2,
    },
    RobertsonPoint {
        r: 475.0,
        u: 0.296_85,
        v: 0.358_23,
        t: -8.595_5,
    },
    RobertsonPoint {
        r: 500.0,
        u: 0.305_05,
        v: 0.359_07,
        t: -11.324,
    },
    RobertsonPoint {
        r: 525.0,
        u: 0.313_20,
        v: 0.359_68,
        t: -15.628,
    },
    RobertsonPoint {
        r: 550.0,
        u: 0.321_29,
        v: 0.360_11,
        t: -23.325,
    },
    RobertsonPoint {
        r: 575.0,
        u: 0.329_31,
        v: 0.360_38,
        t: -40.770,
    },
    RobertsonPoint {
        r: 600.0,
        u: 0.337_24,
        v: 0.360_51,
        t: -116.45,
    },
];

/// The unit vector along an isotemperature line of slope `t` (`dv/du = t`), oriented the same way
/// `dng_temperature.cpp` always builds it: `(1, t)` normalized, never `(-1, -t)`.
fn unit_slope(t: f64) -> [f64; 2] {
    let length = (1.0 + t * t).sqrt();
    [1.0 / length, t / length]
}

/// The illuminant chromaticity Lightroom's `Temperature`/`Tint` pair names, in CIE 1960 `uv`: a
/// direct translation of `dng_temperature::Get_xy_coord`, stopping at its `uv` (the function's own
/// `u`, `v` locals just before their final conversion to CIE `xy`) because Luxforge's inverse
/// locus consumes `uv` directly. `temperature_kelvin` and `tint` are Adobe's own units and are not
/// range-checked here: Lightroom's declared range (2,000..50,000 K, ±150) is wider than
/// Luxforge's, and the caller ([`lightroom_to_luxforge`]) is what refuses an out-of-range result,
/// not an out-of-range Lightroom input.
fn dng_uv_for_temperature_tint(temperature_kelvin: f64, tint: f64) -> Result<[f64; 2], Error> {
    if !temperature_kelvin.is_finite() || temperature_kelvin <= 0.0 {
        return Err(Error::validation(
            "Lightroom Temperature must be finite and positive",
        ));
    }
    if !tint.is_finite() {
        return Err(Error::validation("Lightroom Tint must be finite"));
    }

    let r = 1.0e6 / temperature_kelvin;
    let offset = tint / DNG_TINT_SCALE;

    let last = DNG_TEMP_TABLE.len() - 1;
    for index in 0..last {
        if r < DNG_TEMP_TABLE[index + 1].r || index == last - 1 {
            let low = &DNG_TEMP_TABLE[index];
            let high = &DNG_TEMP_TABLE[index + 1];
            let f = (high.r - r) / (high.r - low.r);

            let u = low.u * f + high.u * (1.0 - f);
            let v = low.v * f + high.v * (1.0 - f);

            let [uu1, vv1] = unit_slope(low.t);
            let [uu2, vv2] = unit_slope(high.t);
            let uu3 = uu1 * f + uu2 * (1.0 - f);
            let vv3 = vv1 * f + vv2 * (1.0 - f);
            let length3 = uu3.hypot(vv3);
            let [uu3, vv3] = [uu3 / length3, vv3 / length3];

            let uv = [u + uu3 * offset, v + vv3 * offset];
            if !uv.iter().all(|value| value.is_finite()) {
                return Err(Error::validation(
                    "Lightroom Temperature/Tint uv is non-finite",
                ));
            }
            return Ok(uv);
        }
    }
    unreachable!("the loop always returns by `index == last - 1`");
}

/// Lightroom's paired `Temperature` (K) and `Tint` — the absolute RAW white balance,
/// `crs:Temperature`/`crs:Tint` — converted to Luxforge's own RAW `Temperature` and `Tint`,
/// through the illuminant chromaticity both name. Refuses the pair together, with
/// `out-of-range: ...`, when the illuminant is one no temperature and tint on Luxforge's locus
/// reach within [`super::white_balance::INVERSE_TOLERANCE_UV`] — in particular whenever the
/// answer would fall outside 2,000..=12,000 K or ±100 Luxforge tint. This is a value conversion:
/// it does not attempt to reproduce Lightroom's rendering, because Luxforge turns its answer into
/// sensor gains through LibRaw's camera matrix, never Adobe's.
pub(crate) fn lightroom_to_luxforge(temperature_kelvin: f64, tint: f64) -> Result<[f64; 2], Error> {
    let uv = dng_uv_for_temperature_tint(temperature_kelvin, tint)?;
    temperature_tint_from_uv(uv, |miss| match miss {
        LocusMiss::BelowMinimum => {
            "out-of-range: the Lightroom pair needs a temperature below 2000 K".to_owned()
        }
        LocusMiss::AboveMaximum => {
            "out-of-range: the Lightroom pair needs a temperature above 12000 K".to_owned()
        }
        LocusMiss::Tint(tint) => {
            format!("out-of-range: the Lightroom pair needs a tint of {tint:.1}, outside -100..100")
        }
        LocusMiss::NoMatch {
            distance,
            temperature,
        } => format!(
            "out-of-range: no temperature and tint reach the Lightroom pair's white within \
             {tolerance} uv (nearest {distance:.2e} at {temperature:.3} K)",
            tolerance = super::white_balance::INVERSE_TOLERANCE_UV,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    /// An independent reimplementation of `Get_xy_coord`'s search, kept deliberately different in
    /// shape from [`dng_uv_for_temperature_tint`] (it walks the table by index from the end and
    /// keeps `1/r` instead of `r`, rather than sharing any loop or interpolation code) so a bug in
    /// one is unlikely to also be in the other. It returns CIE 1931 `xy`, the SDK's own public
    /// surface, by finishing the conversion `dng_uv_for_temperature_tint` stops short of.
    fn independent_dng_xy(temperature_kelvin: f64, tint: f64) -> [f64; 2] {
        let inverse_megakelvin = 1.0e6 / temperature_kelvin;
        let offset = tint / -3000.0;

        // The largest bracket whose low end the target is at or past: the same bracket the
        // production ascending scan finds, but located by walking down from the high end and
        // stopping at the first hit, rather than up from the low end and stopping at the first
        // miss.
        let mut index = DNG_TEMP_TABLE.len() - 2;
        for candidate in (0..DNG_TEMP_TABLE.len() - 1).rev() {
            if inverse_megakelvin >= DNG_TEMP_TABLE[candidate].r {
                index = candidate;
                break;
            }
        }
        let (r0, r1) = (DNG_TEMP_TABLE[index].r, DNG_TEMP_TABLE[index + 1].r);
        let weight_low = (r1 - inverse_megakelvin) / (r1 - r0);

        let base_u =
            DNG_TEMP_TABLE[index].u * weight_low + DNG_TEMP_TABLE[index + 1].u * (1.0 - weight_low);
        let base_v =
            DNG_TEMP_TABLE[index].v * weight_low + DNG_TEMP_TABLE[index + 1].v * (1.0 - weight_low);

        let slope = |t: f64| {
            let norm = (1.0 + t * t).sqrt();
            (1.0 / norm, t / norm)
        };
        let (du0, dv0) = slope(DNG_TEMP_TABLE[index].t);
        let (du1, dv1) = slope(DNG_TEMP_TABLE[index + 1].t);
        let (mut du, mut dv) = (
            du0 * weight_low + du1 * (1.0 - weight_low),
            dv0 * weight_low + dv1 * (1.0 - weight_low),
        );
        let norm = (du * du + dv * dv).sqrt();
        du /= norm;
        dv /= norm;

        let u = base_u + du * offset;
        let v = base_v + dv * offset;
        [1.5 * u / (u - 4.0 * v + 2.0), v / (u - 4.0 * v + 2.0)]
    }

    fn close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual} vs {expected} (tolerance {tolerance})"
        );
    }

    /// [`dng_uv_for_temperature_tint`] against the independent reimplementation above, over a grid
    /// spanning Lightroom's declared domain, well beyond Luxforge's own range: the two must agree
    /// on the DNG SDK's forward map regardless of what Luxforge later does with it.
    #[test]
    fn matches_the_independent_reference_over_lightrooms_declared_domain() {
        for temperature in [
            2_000.0, 2_856.0, 5_000.0, 5_500.0, 6_500.0, 6_504.0, 10_000.0, 50_000.0,
        ] {
            for tint in [-150.0, -20.0, 0.0, 10.0, 150.0] {
                let uv = dng_uv_for_temperature_tint(temperature, tint).unwrap();
                let [u, v] = uv;
                let x = 1.5 * u / (u - 4.0 * v + 2.0);
                let y = v / (u - 4.0 * v + 2.0);
                let [expected_x, expected_y] = independent_dng_xy(temperature, tint);
                close(x, expected_x, 1.0e-9);
                close(y, expected_y, 1.0e-9);
            }
        }
    }

    /// Anchor points published independently of this conversion: CIE Illuminant A is defined at
    /// `x = 0.4476, y = 0.4074` (Wyszecki & Stiles, the same source the table cites) and sits
    /// essentially on the blackbody locus, so Adobe's own table should reproduce it closely at its
    /// nominal `2856` K with zero tint. D65 (`x = 0.3127, y = 0.3290`) is not on the blackbody
    /// locus, so this is not expected to reproduce D65 to the same tolerance; the point of the
    /// second assertion is only that Adobe's `6504` K, tint 0 lands near D65, as the design intends
    /// the illuminant Lightroom's default Temperature names to be, not that the two are equal.
    #[test]
    fn reproduces_illuminant_a_and_is_close_to_d65() {
        let [u, v] = dng_uv_for_temperature_tint(2_856.0, 0.0).unwrap();
        let x = 1.5 * u / (u - 4.0 * v + 2.0);
        let y = v / (u - 4.0 * v + 2.0);
        close(x, 0.4476, 2.0e-4);
        close(y, 0.4074, 2.0e-4);

        let [u, v] = dng_uv_for_temperature_tint(6_504.0, 0.0).unwrap();
        let x = 1.5 * u / (u - 4.0 * v + 2.0);
        let y = v / (u - 4.0 * v + 2.0);
        close(x, 0.3127, 0.01);
        close(y, 0.3290, 0.01);
    }

    #[test]
    fn non_finite_or_non_positive_temperature_and_non_finite_tint_are_refused() {
        for temperature in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(dng_uv_for_temperature_tint(temperature, 0.0).is_err());
        }
        for tint in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(dng_uv_for_temperature_tint(5_000.0, tint).is_err());
        }
    }

    /// Round trip through Luxforge's own inverse: converting back through
    /// [`crate::modules::gains_from_temperature_tint`] and
    /// [`crate::modules::temperature_tint_from_gains`] with a fixed camera matrix reproduces the
    /// same temperature and tint [`lightroom_to_luxforge`] returned (to the `f32` gain rounding
    /// the payload stores, the same 0.01 K / 1e-3 tint bound `white_balance`'s own round-trip
    /// test uses), and the white that answer selects lands within
    /// [`super::super::white_balance::INVERSE_TOLERANCE_UV`] of the `uv` the Lightroom pair
    /// named.
    #[test]
    fn luxforge_answer_reproduces_the_lightroom_white_through_a_camera_matrix() {
        use super::super::white_balance::{
            gains_from_temperature_tint, temperature_tint_from_gains,
        };

        // LibRaw cam_xyz copied from the supplied Nikon Z6 metadata (same fixture the
        // white_balance tests use).
        let z6_cam_xyz = [
            [0.9943, -0.3269, -0.0839],
            [-0.5323, 1.3269, 0.2259],
            [-0.1198, 0.2083, 0.7557],
            [0.0; 3],
        ];
        for (temperature, tint) in [(5_500.0, 10.0), (3_200.0, 0.0), (6_500.0, -20.0)] {
            let [luxforge_kelvin, luxforge_tint] =
                lightroom_to_luxforge(temperature, tint).unwrap();
            let target_uv = dng_uv_for_temperature_tint(temperature, tint).unwrap();

            let gains =
                gains_from_temperature_tint(luxforge_kelvin, luxforge_tint, z6_cam_xyz).unwrap();
            let [back_kelvin, back_tint] = temperature_tint_from_gains(gains, z6_cam_xyz).unwrap();
            close(back_kelvin, luxforge_kelvin, 0.01);
            close(back_tint, luxforge_tint, 1.0e-3);

            // The white Luxforge's answer selects, read back through its own forward locus, is
            // within tolerance of the `uv` the Lightroom pair named.
            let frame_uv =
                super::super::white_balance::tinted_whitepoint_uv(luxforge_kelvin, luxforge_tint)
                    .unwrap();
            let distance = (frame_uv[0] - target_uv[0]).hypot(frame_uv[1] - target_uv[1]);
            assert!(
                distance <= super::super::white_balance::INVERSE_TOLERANCE_UV,
                "{temperature} K {tint:+} -> {luxforge_kelvin:.3} K {luxforge_tint:+.4}: \
                 {distance:.2e} uv from the Lightroom white"
            );
        }
    }

    /// The out-of-range refusal wording matches the file's style and names the Lightroom pair,
    /// not the gains.
    #[test]
    fn refuses_pairs_whose_luxforge_answer_is_out_of_range() {
        let error = lightroom_to_luxforge(50_000.0, 0.0).expect_err("too warm for Luxforge");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .starts_with("out-of-range: the Lightroom pair needs a temperature"),
            "{}",
            error.detail
        );

        let error = lightroom_to_luxforge(6_500.0, -150.0).expect_err("too much tint");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error.detail.contains("the Lightroom pair needs a tint of"),
            "{}",
            error.detail
        );
    }

    /// Just inside Luxforge's declared ranges, the same construction is answered, not refused.
    #[test]
    fn admits_pairs_whose_luxforge_answer_is_in_range() {
        let [kelvin, tint] = lightroom_to_luxforge(5_500.0, 10.0).unwrap();
        assert!((2_000.0..=12_000.0).contains(&kelvin));
        assert!((-100.0..=100.0).contains(&tint));
    }

    /// Not a gate: prints the sample conversions recorded in the hand-off report. Run with
    /// `--ignored --nocapture`.
    #[test]
    #[ignore = "prints sample conversions for the hand-off report, not a gate"]
    fn print_sample_conversions() {
        for (temperature, tint) in [(5_500.0, 10.0), (3_200.0, 0.0), (7_500.0, -20.0)] {
            match lightroom_to_luxforge(temperature, tint) {
                Ok([kelvin, luxforge_tint]) => println!(
                    "LR {temperature:.0}/{tint:+.0} -> Luxforge {kelvin:.3} K, tint {luxforge_tint:+.4}"
                ),
                Err(error) => println!("LR {temperature:.0}/{tint:+.0} -> refused: {error}"),
            }
        }
    }
}
