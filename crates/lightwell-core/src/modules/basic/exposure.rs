//! The Basic module's exposure unit: the one pointwise equation this slice owns.
use crate::modules::PointwiseColor;

/// Multiply every linear-light channel by `2^EV`.
///
/// The gain is computed once in f64 and applied in f32, which is the working precision the colour
/// contract declares. Nothing is clamped here: the host preserves values outside `[0, 1]` between
/// units and between consecutive colour operations and clamps, encodes and quantizes once at the
/// end of the run, so `+1 EV` followed by `−1 EV` returns the input codes exactly.
#[derive(Debug)]
pub(super) struct Exposure {
    /// The stored parameter value, kept for [`PointwiseColor::describe`] and the finiteness check.
    ev: f64,
    gain: f32,
}

impl Exposure {
    pub(super) fn new(ev: f64) -> Self {
        Self {
            ev,
            gain: ev.exp2() as f32,
        }
    }
}

impl PointwiseColor for Exposure {
    fn apply_row(&self, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            for channel in pixel {
                *channel *= self.gain;
            }
        }
    }

    /// A non-finite stored value, or one whose gain overflows f32, is refused at compilation
    /// before any frame is touched.
    fn is_finite(&self) -> bool {
        self.ev.is_finite() && self.gain.is_finite()
    }

    /// The stored value, at the parameter's declared display precision. The host compares compiled
    /// operations by this string, so two units that describe themselves identically process
    /// identically: the gain is a pure function of `ev`.
    fn describe(&self) -> String {
        format!("exposure({:+.2})", self.ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The independent f64 statement of the same equation, written from the contract.
    fn reference(linear: f64, ev: f64) -> f64 {
        linear * 2.0_f64.powf(ev)
    }

    #[test]
    fn a_row_is_multiplied_by_two_to_the_ev_in_place() {
        for ev in [-5.0, -2.0, -0.5, 0.0, 0.5, 1.0, 2.0, 5.0] {
            let unit = Exposure::new(ev);
            let mut row = [[0.0, 0.25, 1.0], [0.5, -0.125, 2.0]];
            unit.apply_row(&mut row);
            for (pixel, input) in row.iter().zip([[0.0, 0.25, 1.0], [0.5, -0.125, 2.0]]) {
                for (actual, input) in pixel.iter().zip(input) {
                    let expected = reference(input, ev);
                    let tolerance = 1e-6 + 1e-6 * expected.abs();
                    assert!(
                        (f64::from(*actual) - expected).abs() <= tolerance,
                        "ev {ev}: {actual} against {expected}"
                    );
                }
            }
        }
    }

    /// Values outside `[0, 1]` and negative values survive a unit, so the host can clamp once at
    /// the end of the run instead of after every step.
    #[test]
    fn an_inverse_pair_returns_its_input_exactly() {
        let mut row = [[0.0, 0.051_269_46, 1.0], [-0.25, 2.5, 0.215_860_5]];
        let input = row;
        Exposure::new(1.0).apply_row(&mut row);
        Exposure::new(-1.0).apply_row(&mut row);
        assert_eq!(row, input, "powers of two invert exactly in f32");
    }

    #[test]
    fn zero_ev_is_the_identity_gain() {
        let unit = Exposure::new(0.0);
        let mut row = [[0.0, 0.25, 1.0]];
        unit.apply_row(&mut row);
        assert_eq!(row, [[0.0, 0.25, 1.0]]);
        assert_eq!(unit.describe(), "exposure(+0.00)");
    }

    #[test]
    fn finiteness_follows_the_stored_value_and_its_gain() {
        assert!(Exposure::new(5.0).is_finite());
        assert!(Exposure::new(-5.0).is_finite());
        assert!(!Exposure::new(f64::NAN).is_finite());
        assert!(!Exposure::new(f64::INFINITY).is_finite());
        // A value far outside the declared range would overflow the f32 gain; the range check in
        // `validate_payload` refuses it first, and this is the second line of defence.
        assert!(!Exposure::new(1024.0).is_finite());
    }

    #[test]
    fn the_description_carries_the_stored_value_and_its_sign() {
        assert_eq!(Exposure::new(0.5).describe(), "exposure(+0.50)");
        assert_eq!(Exposure::new(-1.25).describe(), "exposure(-1.25)");
        assert_ne!(
            Exposure::new(0.5).describe(),
            Exposure::new(0.51).describe(),
            "two different stored values never describe themselves the same way"
        );
    }
}
