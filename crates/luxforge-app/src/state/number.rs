//! How one declared number or integer parameter is shown, stepped, snapped and nudged. The
//! descriptor's hints — range, soft range, step, fine step, precision and zero — are read here
//! once, and the slider, the stepper, the number field, the key and field nudges, the mask panel's
//! fields and every formatted value use the same reading, so a rail, the value it shows and the
//! value it sends cannot disagree.
use luxforge_core::{ParameterDescriptor, ParameterKind};
use luxforge_ui::geometry::{MAX_DECIMALS, quantize};
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
}
