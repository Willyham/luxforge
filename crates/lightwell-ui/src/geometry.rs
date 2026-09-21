//! Pure geometry for the slider widget.
//!
//! Iced's slider draws its rail as exactly two quads, split at the handle. To grow a fill from a
//! zero tick that generally sits somewhere other than the handle, one of those two quads needs a
//! second, internal split. These functions compute that geometry as plain fractions with no
//! dependency on layout, pixels or a renderer, so they are tested here directly; [`crate::theme`]
//! turns the result into the [`iced::widget::slider::Style`] the widget actually draws.

/// Whether a rail segment is empty (no fill) or filled, at a point along the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    /// No fill: the plain rail colour.
    Empty,
    /// Filled: the portion between the zero tick and the current value.
    Filled,
}

/// The fill of one rail segment (the half of the rail to one side of the handle).
///
/// `Split`'s `at` is a fraction local to the segment: `0.0` at the end of the segment nearest the
/// rail's minimum, `1.0` at the end nearest its maximum. `before` fills `0.0..at`, `after` fills
/// `at..1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    /// The whole segment has one fill.
    Solid(Fill),
    /// The segment has a hard edge at `at`.
    Split { at: f64, before: Fill, after: Fill },
}

/// The rail's two background segments, split at the handle, exactly as
/// [`iced::widget::slider`] draws `Rail::backgrounds`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillStops {
    /// The segment from the rail's minimum end to the handle.
    pub left: Segment,
    /// The segment from the handle to the rail's maximum end.
    pub right: Segment,
}

/// The side of a soft rail that contains a valid hard-range value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Low,
    High,
}

/// Position of a hard-range value relative to the soft rail.
pub fn over_range_side(soft_min: f64, soft_max: f64, value: f64) -> Option<Side> {
    if value < soft_min {
        Some(Side::Low)
    } else if value > soft_max {
        Some(Side::High)
    } else {
        None
    }
}

/// Clamp a numeric value to the rail and return its fraction. The caller owns the numeric range.
pub fn fraction_from_value(min: f64, max: f64, value: f64) -> f64 {
    if !min.is_finite() || !max.is_finite() || max <= min {
        return 0.0;
    }
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

/// Evenly spaced colour-stop positions, including both endpoints.
pub fn rail_stop_positions(count: usize) -> Vec<f32> {
    match count {
        0 => Vec::new(),
        1 => vec![0.0],
        count => (0..count).map(|i| i as f32 / (count - 1) as f32).collect(),
    }
}

/// Computes the rail fill geometry for a slider spanning `min..=max`, currently at `value`, whose
/// fill grows from `zero` (or from `min` when `zero` is `None`, for a unipolar slider).
///
/// Values outside `min..=max` are clamped, matching the slider widget's own clamping of `value`
/// and `on_change` results.
pub fn fill_stops(min: f64, max: f64, zero: Option<f64>, value: f64) -> FillStops {
    if max <= min {
        return FillStops {
            left: Segment::Solid(Fill::Empty),
            right: Segment::Solid(Fill::Empty),
        };
    }

    let fraction = |v: f64| (v.clamp(min, max) - min) / (max - min);
    let value_frac = fraction(value);
    let zero_frac = zero.map_or(0.0, fraction);

    if value_frac >= zero_frac {
        // The fill spans [zero, value], entirely inside the left segment (min..handle).
        let left = if value_frac <= 0.0 {
            Segment::Solid(Fill::Empty)
        } else {
            split_or_solid(zero_frac / value_frac, Fill::Empty, Fill::Filled)
        };
        FillStops {
            left,
            right: Segment::Solid(Fill::Empty),
        }
    } else {
        // The fill spans [value, zero], entirely inside the right segment (handle..max).
        let span = 1.0 - value_frac;
        let at = if span <= 0.0 {
            1.0
        } else {
            (zero_frac - value_frac) / span
        };
        let right = split_or_solid(at, Fill::Filled, Fill::Empty);
        FillStops {
            left: Segment::Solid(Fill::Empty),
            right,
        }
    }
}

/// Builds a [`Segment`], collapsing to `Solid` when the split falls at or beyond either end.
fn split_or_solid(at: f64, before: Fill, after: Fill) -> Segment {
    let at = at.clamp(0.0, 1.0);
    if at <= 0.0 {
        Segment::Solid(after)
    } else if at >= 1.0 {
        Segment::Solid(before)
    } else {
        Segment::Split { at, before, after }
    }
}

/// Maps a pointer fraction along the rail (`0.0` at `min`, `1.0` at `max`) to a value, snapped to
/// `step` and clamped to `min..=max`. A non-positive `step` disables snapping.
///
/// This mirrors the mapping [`iced::widget::slider`] performs internally from a cursor position,
/// exposed as a pure function so the mapping can be tested without a renderer or a layout pass.
pub fn value_from_fraction(min: f64, max: f64, step: f64, fraction: f64) -> f64 {
    if max <= min {
        return min;
    }

    let fraction = fraction.clamp(0.0, 1.0);
    let raw = min + fraction * (max - min);

    if step <= 0.0 {
        return raw.clamp(min, max);
    }

    let steps = ((raw - min) / step).round();
    (min + steps * step).clamp(min, max)
}

/// The largest number of decimals [`quantize`] will round to. It matches the display precision a
/// module descriptor may declare, so a caller cannot ask for a rounding finer than any control can
/// show.
pub const MAX_DECIMALS: usize = 6;

/// Cleans up one value after the host maps a rail fraction into the declared range: snapped to
/// `step` from `min`, clamped into `min..=max`, then rounded to `decimals`.
///
/// The widget reports a fraction so the host can apply its declared range. Calling this pure
/// helper after that mapping keeps `1.7000000000000002` off the screen and out of the recipe.
/// Iced's own slider snapping is float arithmetic over `min + n * step`, which can land next to
/// the step; rounding to the declared decimals lands on it exactly.
///
/// A non-positive or non-finite `step` disables snapping, `decimals` above [`MAX_DECIMALS`] is
/// treated as [`MAX_DECIMALS`], a degenerate range answers `min`, and a non-finite `value` answers
/// `min` rather than propagating. The result is never `-0.0`.
pub fn quantize(value: f64, min: f64, max: f64, step: f64, decimals: usize) -> f64 {
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

    #[test]
    fn bipolar_positive_fills_left_segment_from_zero() {
        let stops = fill_stops(-100.0, 100.0, Some(0.0), 62.0);
        assert_eq!(stops.right, Segment::Solid(Fill::Empty));
        match stops.left {
            Segment::Split { at, before, after } => {
                assert!((at - (0.5 / 0.81)).abs() < 1e-9);
                assert_eq!(before, Fill::Empty);
                assert_eq!(after, Fill::Filled);
            }
            other => panic!("expected a split left segment, got {other:?}"),
        }
    }

    #[test]
    fn bipolar_negative_fills_right_segment_up_to_zero() {
        let stops = fill_stops(-100.0, 100.0, Some(0.0), -40.0);
        assert_eq!(stops.left, Segment::Solid(Fill::Empty));
        match stops.right {
            Segment::Split { at, before, after } => {
                assert!((at - (0.2 / 0.7)).abs() < 1e-9);
                assert_eq!(before, Fill::Filled);
                assert_eq!(after, Fill::Empty);
            }
            other => panic!("expected a split right segment, got {other:?}"),
        }
    }

    #[test]
    fn value_at_zero_has_no_fill() {
        let stops = fill_stops(-100.0, 100.0, Some(0.0), 0.0);
        assert_eq!(stops.left, Segment::Solid(Fill::Empty));
        assert_eq!(stops.right, Segment::Solid(Fill::Empty));
    }

    #[test]
    fn unipolar_fills_from_the_minimum() {
        let stops = fill_stops(0.0, 100.0, None, 40.0);
        assert_eq!(stops.left, Segment::Solid(Fill::Filled));
        assert_eq!(stops.right, Segment::Solid(Fill::Empty));
    }

    #[test]
    fn unipolar_at_minimum_has_no_fill() {
        let stops = fill_stops(0.0, 100.0, None, 0.0);
        assert_eq!(stops.left, Segment::Solid(Fill::Empty));
        assert_eq!(stops.right, Segment::Solid(Fill::Empty));
    }

    #[test]
    fn zero_outside_range_clamps_to_the_nearest_end() {
        // A zero tick above the range behaves like a unipolar slider filling from the minimum.
        let stops = fill_stops(10.0, 100.0, Some(0.0), 40.0);
        assert_eq!(stops.left, Segment::Solid(Fill::Filled));
    }

    #[test]
    fn degenerate_range_is_a_solid_empty_rail() {
        let stops = fill_stops(5.0, 5.0, Some(0.0), 5.0);
        assert_eq!(stops.left, Segment::Solid(Fill::Empty));
        assert_eq!(stops.right, Segment::Solid(Fill::Empty));
    }

    #[test]
    fn value_from_fraction_maps_the_ends() {
        assert_eq!(value_from_fraction(-100.0, 100.0, 1.0, 0.0), -100.0);
        assert_eq!(value_from_fraction(-100.0, 100.0, 1.0, 1.0), 100.0);
        assert_eq!(value_from_fraction(0.0, 100.0, 1.0, 0.5), 50.0);
    }

    #[test]
    fn value_from_fraction_snaps_to_step() {
        assert_eq!(value_from_fraction(0.0, 100.0, 10.0, 0.24), 20.0);
    }

    #[test]
    fn value_from_fraction_clamps_out_of_range_fractions() {
        assert_eq!(value_from_fraction(0.0, 100.0, 1.0, -5.0), 0.0);
        assert_eq!(value_from_fraction(0.0, 100.0, 1.0, 5.0), 100.0);
    }

    #[test]
    fn soft_range_marks_only_the_excess_side() {
        assert_eq!(over_range_side(-2.0, 2.0, -3.0), Some(Side::Low));
        assert_eq!(over_range_side(-2.0, 2.0, 3.0), Some(Side::High));
        assert_eq!(over_range_side(-2.0, 2.0, 1.0), None);
        assert_eq!(fraction_from_value(-2.0, 2.0, 3.0), 1.0);
    }

    #[test]
    fn rail_stops_span_the_full_rail() {
        assert_eq!(rail_stop_positions(0), Vec::<f32>::new());
        assert_eq!(rail_stop_positions(1), vec![0.0]);
        assert_eq!(rail_stop_positions(4), vec![0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]);
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
