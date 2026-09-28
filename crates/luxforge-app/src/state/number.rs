//! How one declared number or integer parameter is shown, stepped, snapped and nudged. The
//! descriptor's hints — range, soft range, step, fine step, precision and zero — are read here
//! once, and the slider, the stepper, the number field, the key and field nudges, the mask panel's
//! fields and every formatted value use the same reading, so a rail, the value it shows and the
//! value it sends cannot disagree.
use luxforge_core::{ParameterDescriptor, ParameterKind};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NumberSpec {
    /// The hard range the host accepts.
    pub(crate) min: f64,
    pub(crate) max: f64,
    /// The range the rail spans: the declared soft bounds, else the hard ones.
    pub(crate) soft_min: f64,
    pub(crate) soft_max: f64,
    /// One increment: the declared step, else 1 for an integer and [`generic_step`] over the
    /// range for a number.
    pub(crate) step: f64,
    /// An Option nudge or a fine drag: the declared fine step, else a tenth of the step.
    pub(crate) fine_step: f64,
    /// The decimals a value is shown with: the declared precision, else the step's own.
    pub(crate) decimals: usize,
    /// The decimals a fine value needs, at least [`Self::decimals`].
    pub(crate) fine_decimals: usize,
    /// Where a bipolar rail's fill grows from: the declared zero, else 0 clamped into the range.
    pub(crate) zero: f64,
    pub(crate) integer: bool,
}

impl NumberSpec {
    /// The spec of a number or integer parameter; `None` for every other kind.
    pub(crate) fn of(parameter: &ParameterDescriptor) -> Option<Self> {
        let (min, max, integer) = match parameter.kind {
            ParameterKind::Number { min, max } => (min, max, false),
            ParameterKind::Integer { min, max } => (min as f64, max as f64, true),
            _ => return None,
        };
        let positive = |step: &f64| step.is_finite() && *step > 0.0;
        let step = parameter
            .step
            .filter(positive)
            .unwrap_or_else(|| if integer { 1.0 } else { generic_step(min, max) });
        let fine_step = parameter.fine_step.filter(positive).unwrap_or(step / 10.0);
        let decimals = if integer {
            0
        } else {
            match parameter.precision {
                Some(precision) => usize::from(precision).min(MAX_DECIMALS),
                None => decimals_of(step),
            }
        };
        let fine_decimals = if integer {
            0
        } else {
            decimals.max(decimals_of(fine_step))
        };
        Some(Self {
            min,
            max,
            soft_min: parameter.soft_min.unwrap_or(min),
            soft_max: parameter.soft_max.unwrap_or(max),
            step,
            fine_step,
            decimals,
            fine_decimals,
            zero: parameter.zero.unwrap_or(0.0_f64.clamp(min, max)),
            integer,
        })
    }

    /// A value as its field shows it: a fixed number of decimals, so a control reads `1.70`,
    /// `0.00` or `-3.50` rather than whatever the last arithmetic left behind.
    ///
    /// A value that rounds to zero is always `0` or `0.00`, never `-0.00`: the sign of a zero is an
    /// artefact of the arithmetic, not something the person did.
    pub(crate) fn format(&self, value: f64) -> String {
        let factor = 10f64.powi(self.decimals as i32);
        let ordinary = (value * factor).round() / factor;
        // A saved sensor value may be fractionally off the visible grid. Only expose the fine
        // digits when they distinguish a real fine nudge rather than a conversion or
        // floating-point residue.
        let fine_quantum = 10f64.powi(-(self.fine_decimals as i32));
        let decimals = if value.is_finite()
            && (value - ordinary).abs() > (fine_quantum * 0.49).max(1e-9 * value.abs().max(1.0))
        {
            self.fine_decimals
        } else {
            self.decimals
        };
        format_decimals(value, decimals)
    }

    /// The value a fraction of the rail stands for: snapped to the fine step, clamped into the
    /// hard range and rounded to the decimals a fine value is shown with.
    pub(crate) fn at_fraction(&self, fraction: f64) -> Value {
        let fraction = fraction.clamp(0.0, 1.0);
        let raw = self.soft_min + fraction * (self.soft_max - self.soft_min);
        self.value(quantize(
            raw,
            self.min,
            self.max,
            self.fine_step,
            self.fine_decimals,
        ))
    }

    /// The rail fraction that sends exactly `value`: where the widget's pointer publishes it, the
    /// inverse of [`Self::at_fraction`]. A value outside the soft range or off the fine grid has no
    /// such fraction, since every fraction lands inside the one and on the other; the error is the
    /// value its nearest fraction sends instead.
    pub(crate) fn fraction_of(&self, value: f64) -> Result<f64, Value> {
        let fraction = if self.soft_min.is_finite()
            && self.soft_max.is_finite()
            && self.soft_max > self.soft_min
        {
            ((value - self.soft_min) / (self.soft_max - self.soft_min)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let sent = self.at_fraction(fraction);
        if sent.as_f64() == Some(value) {
            Ok(fraction)
        } else {
            Err(sent)
        }
    }

    /// One nudge from `current` in `direction`: the step, ten steps with Shift, the fine step
    /// with Option, clamped into the hard range.
    pub(crate) fn nudged(&self, current: f64, direction: i8, shift: bool, option: bool) -> f64 {
        let step = if option {
            self.fine_step
        } else if shift {
            self.step * 10.0
        } else {
            self.step
        };
        (current + f64::from(direction.signum()) * step).clamp(self.min, self.max)
    }

    /// A number as the request carries it: an integer parameter's is a whole number.
    pub(crate) fn value(&self, value: f64) -> Value {
        if self.integer {
            Value::from(value.round() as i64)
        } else {
            Value::from(value)
        }
    }
}

/// The generic increment for a number parameter that declares no step: a fraction of its range,
/// rounded to a power of ten. It decides the rail's step, the decimals the field shows and the
/// precision a drag is quantized to, and those three must agree.
pub(crate) fn generic_step(min: f64, max: f64) -> f64 {
    let span = (max - min).abs();
    if !span.is_finite() || span <= 0.0 {
        return 0.01;
    }
    10f64.powf((span / 200.0).log10().round())
}

/// The decimals one increment needs: `0.01` → 2, `0.5` → 1, `10` → 0. Capped at the largest
/// precision a descriptor may declare, so an unrepresentable step cannot ask for endless digits.
pub(crate) fn decimals_of(step: f64) -> usize {
    if !step.is_finite() || step <= 0.0 {
        return 0;
    }
    (0..=MAX_DECIMALS)
        .find(|decimals| {
            let scaled = step * 10f64.powi(*decimals as i32);
            (scaled - scaled.round()).abs() <= 1e-9 * scaled.abs().max(1.0)
        })
        .unwrap_or(MAX_DECIMALS)
}

/// A number as text, with no declared parameter to say how it should read: `0`, `-3.5`, no
/// trailing zeros or exponent noise.
///
/// This is for numbers that are not a declared parameter's value — a range message's limits, the
/// crop draft's own readout. Every number that **is** one goes through [`NumberSpec::format`],
/// which shows the decimals the parameter declares and never leaves `1.7000000000000002` on
/// screen.
pub(crate) fn number_text(value: f64) -> String {
    format!("{value}")
}

/// `value` with exactly `decimals` decimals, and no negative zero.
fn format_decimals(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return number_text(value);
    }
    let text = format!("{value:.decimals$}");
    match text.strip_prefix('-') {
        // "-0", "-0.00": the digits are all zeros, so the sign says nothing.
        Some(rest) if rest.bytes().all(|byte| byte == b'0' || byte == b'.') => rest.to_owned(),
        _ => text,
    }
}

/// The largest number of decimals [`quantize`] will round to. It matches the display precision a
/// module descriptor may declare, so a caller cannot ask for a rounding finer than any control can
/// show.
const MAX_DECIMALS: usize = 6;

/// Cleans up one value after the host maps a rail fraction into the declared range: snapped to
/// `step` from `min`, clamped into `min..=max`, then rounded to `decimals`.
///
/// The widget reports a fraction so the view model can apply the declared range. Calling this
/// after that mapping keeps `1.7000000000000002` off the screen and out of the recipe.
/// Iced's own slider snapping is float arithmetic over `min + n * step`, which can land next to
/// the step; rounding to the declared decimals lands on it exactly.
///
/// A non-positive or non-finite `step` disables snapping, `decimals` above [`MAX_DECIMALS`] is
/// treated as [`MAX_DECIMALS`], a degenerate range answers `min`, and a non-finite `value` answers
/// `min` rather than propagating. The result is never `-0.0`.
fn quantize(value: f64, min: f64, max: f64, step: f64, decimals: usize) -> f64 {
    if !min.is_finite() || !max.is_finite() || max <= min {
        return min;
    }
    if !value.is_finite() {
        return min;
    }
    let snapped = if step.is_finite() && step > 0.0 {
        min + ((value - min) / step).round() * step
    } else {
        value
    };
    // Round inside the range, then clamp again: rounding a value that sits next to an endpoint can
    // step over it when the endpoint itself carries more decimals than the control shows.
    round_to(snapped.clamp(min, max), decimals).clamp(min, max) + 0.0
}

/// `value` rounded to `decimals` decimal places, half away from zero. Returns the value unchanged
/// when it is not finite, so a caller's guard is never the only thing between a NaN and a `.round()`.
fn round_to(value: f64, decimals: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let factor = 10f64.powi(decimals.min(MAX_DECIMALS) as i32);
    let scaled = value * factor;
    if !scaled.is_finite() {
        return value;
    }
    scaled.round() / factor
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One reading of the hints: a declared step and fine step drive the rail, the nudges and the
    /// drag's snapping alike, and a number without a step takes the generic one for all three.
    #[test]
    fn one_spec_steps_snaps_and_nudges_a_declared_parameter() {
        let amount = ParameterDescriptor::number("amount", -10.0, 10.0)
            .soft_min(-5.0)
            .soft_max(5.0)
            .step(0.1)
            .fine_step(0.01)
            .precision(2);
        let spec = NumberSpec::of(&amount).expect("a number");
        assert_eq!((spec.step, spec.fine_step), (0.1, 0.01));
        assert_eq!((spec.decimals, spec.fine_decimals), (2, 2));
        // The rail spans the soft range, and a drag lands on the fine grid.
        assert_eq!(spec.at_fraction(0.0), json!(-5.0));
        assert_eq!(spec.at_fraction(0.5), json!(0.0));
        assert_eq!(spec.at_fraction(0.1234), json!(-3.77));
        assert_eq!(spec.at_fraction(2.0), json!(5.0));
        // Its inverse finds the fraction for a value on the fine grid inside the soft range, and
        // names what any other value would be sent as.
        let fraction = spec.fraction_of(-3.77).expect("on the grid");
        assert_eq!(spec.at_fraction(fraction), json!(-3.77));
        assert_eq!(spec.fraction_of(-3.774), Err(json!(-3.77)));
        assert_eq!(spec.fraction_of(7.0), Err(json!(5.0)));
        // Nudges: the step, ten with Shift, the fine step with Option, clamped to the hard range.
        assert!((spec.nudged(1.0, 1, false, false) - 1.1).abs() < 1e-12);
        assert_eq!(spec.nudged(1.0, -1, true, false), 0.0);
        assert!((spec.nudged(1.0, 1, false, true) - 1.01).abs() < 1e-12);
        assert_eq!(spec.nudged(9.95, 1, true, false), 10.0);

        let bare = ParameterDescriptor::number("angle", -45.0, 45.0);
        let spec = NumberSpec::of(&bare).expect("a number");
        assert_eq!((spec.step, spec.fine_step, spec.decimals), (1.0, 0.1, 0));
        assert_eq!(spec.nudged(0.0, 1, false, false), 1.0);

        let count = ParameterDescriptor::integer("count", 0, 20);
        let spec = NumberSpec::of(&count).expect("an integer");
        assert!(spec.integer);
        assert_eq!((spec.step, spec.decimals, spec.fine_decimals), (1.0, 0, 0));
        assert_eq!(spec.at_fraction(0.52), json!(10));
        assert_eq!(spec.value(3.6), json!(4));
        assert_eq!(spec.format(12.0), "12");

        assert!(NumberSpec::of(&ParameterDescriptor::boolean("on")).is_none());
    }

    /// The exact complaint this exists for: iced's own snapping to a 0.01 step lands next to the
    /// step, and the value reaches the recipe and the field as `1.7000000000000002`.
    #[test]
    fn quantize_lands_exactly_on_the_declared_step() {
        assert_eq!(quantize(1.7000000000000002, -5.0, 5.0, 0.01, 2), 1.7);
        assert_eq!(quantize(1.7049, -5.0, 5.0, 0.01, 2), 1.7);
        assert_eq!(quantize(1.706, -5.0, 5.0, 0.01, 2), 1.71);
        assert_eq!(quantize(-3.499, -5.0, 5.0, 0.01, 2), -3.5);
        // A whole-number step over a whole-number range answers whole numbers.
        assert_eq!(quantize(39.6, -100.0, 100.0, 1.0, 0), 40.0);
        assert_eq!(quantize(6501.2, 2000.0, 12000.0, 10.0, 0), 6500.0);
    }

    /// The snap is measured from `min`, not from zero, so a range whose minimum is not a multiple
    /// of the step still produces values the declared step can reach.
    #[test]
    fn quantize_snaps_from_the_minimum() {
        assert_eq!(quantize(0.5, 0.01, 32.0, 0.01, 2), 0.5);
        assert_eq!(quantize(0.014, 0.01, 32.0, 0.01, 2), 0.01);
        assert_eq!(quantize(0.016, 0.01, 32.0, 0.01, 2), 0.02);
        // 0.3 is 4.0 steps of 0.07 above 0.02, so it snaps to itself, not to a multiple of 0.07.
        assert!((quantize(0.3, 0.02, 1.0, 0.07, 2) - 0.3).abs() < 1e-12);
    }

    #[test]
    fn quantize_clamps_to_the_declared_range() {
        assert_eq!(quantize(12.0, -5.0, 5.0, 0.01, 2), 5.0);
        assert_eq!(quantize(-12.0, -5.0, 5.0, 0.01, 2), -5.0);
        // An endpoint with more decimals than the control shows is still reachable: the rounding
        // happens inside the range and the clamp puts it back on the endpoint.
        assert_eq!(quantize(2.4999, 0.0, 2.4999, 0.0001, 2), 2.4999);
    }

    /// Nothing the widget sends is ever `-0.0`: a field that showed `-0.00` for a value the person
    /// dragged to the centre of a bipolar rail is exactly the noise this removes.
    #[test]
    fn quantize_never_answers_negative_zero() {
        for value in [-0.0, -0.004, -0.0000001] {
            let quantized = quantize(value, -5.0, 5.0, 0.01, 2);
            assert_eq!(quantized, 0.0, "{value}");
            assert!(
                quantized.is_sign_positive(),
                "{value} quantized to a negative zero"
            );
        }
    }

    #[test]
    fn quantize_refuses_degenerate_inputs_rather_than_propagating_them() {
        assert_eq!(quantize(3.0, 5.0, 5.0, 1.0, 0), 5.0);
        assert_eq!(quantize(f64::NAN, -5.0, 5.0, 0.01, 2), -5.0);
        assert_eq!(quantize(f64::INFINITY, -5.0, 5.0, 0.01, 2), -5.0);
        // A step that is not a usable increment disables snapping but still rounds and clamps.
        assert_eq!(quantize(1.234_5, -5.0, 5.0, 0.0, 2), 1.23);
        assert_eq!(quantize(1.234_5, -5.0, 5.0, f64::NAN, 2), 1.23);
        // A precision finer than any control can declare is capped rather than overflowing.
        assert_eq!(quantize(1.5, -5.0, 5.0, 0.0, usize::MAX), 1.5);
    }
}
