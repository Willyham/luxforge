//! Pure geometry for the slider widget.
//!
//! The slider row draws its own rail line: the plain or decorated rail, the fill from the zero
//! tick (or from the rail's start, for a unipolar slider) to the handle's centre, and the zero
//! tick, all under Iced's handle. Iced places the handle's centre at `radius + (width - 2 *
//! radius) * fraction`; [`rail_geometry`] computes every other position on the same scale, in
//! points from the rail's left edge, so the fill always meets the handle and the tick always sits
//! where the handle rests at zero. These functions have no dependency on a renderer and are
//! tested here directly.

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

/// The colour a declared colour rail shows at `t` of its length: the stops evenly spaced along it
/// and mixed in sRGB, as the module references draw a gradient, rather than in the linear light
/// Iced's own gradient interpolates in (which lightens every midpoint). Channels are sRGB values
/// in `0.0..=1.0`.
pub fn rail_colour_at(stops: &[[f32; 3]], t: f32) -> [f32; 3] {
    match stops {
        [] => [0.0; 3],
        [only] => *only,
        _ => {
            let span = (stops.len() - 1) as f32;
            let position = t.clamp(0.0, 1.0) * span;
            let index = (position.floor() as usize).min(stops.len() - 2);
            let local = position - index as f32;
            let (from, to) = (stops[index], stops[index + 1]);
            std::array::from_fn(|channel| from[channel] + (to[channel] - from[channel]) * local)
        }
    }
}

/// `colour` laid over `background` at `opacity`, mixed in sRGB as the references composite it.
/// Iced would blend a translucent fill in linear light, which renders it brighter, so a widget
/// that needs the reference's result draws this opaque colour instead.
pub fn over(colour: [f32; 3], background: [f32; 3], opacity: f32) -> [f32; 3] {
    std::array::from_fn(|channel| {
        background[channel] + (colour[channel] - background[channel]) * opacity
    })
}

/// The fractions at which a colour rail `width` points long is cut into pieces no longer than
/// `piece` points, both ends included. Each piece is drawn as a two-stop gradient between the
/// exact colours at its ends, so the rail follows [`rail_colour_at`] to well under one 8-bit code
/// while Iced draws only a handful of gradients.
pub fn rail_pieces(width: f32, piece: f32) -> Vec<f32> {
    if width <= 0.0 || piece <= 0.0 {
        return vec![0.0, 1.0];
    }
    let count = (width / piece).ceil().max(1.0) as usize;
    (0..=count).map(|i| i as f32 / count as f32).collect()
}

/// Where Iced draws a slider handle's centre, in points from the rail's left edge, for a rail
/// `width` wide whose round handle has `radius`, at `fraction` of the rail.
pub fn handle_center(width: f32, radius: f32, fraction: f64) -> f32 {
    radius + (width - 2.0 * radius).max(0.0) * fraction.clamp(0.0, 1.0) as f32
}

/// The zero tick's rail fraction for a slider whose fill grows from `zero`, or `None` for a
/// unipolar slider: one with no zero, or a zero at either end of the rail (a fill from the minimum
/// has no midpoint to mark).
pub fn zero_fraction(min: f64, max: f64, zero: Option<f64>) -> Option<f64> {
    zero.filter(|zero| *zero > min && *zero < max)
        .map(|zero| fraction_from_value(min, max, zero))
}

/// What one rail line draws, in points from the rail's left edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RailGeometry {
    /// The handle's centre.
    pub handle: f32,
    /// The filled span `(from, to)`, `from <= to`, or `None` when nothing is filled.
    pub fill: Option<(f32, f32)>,
    /// The zero tick's centre, for a slider with a zero inside its rail.
    pub tick: Option<f32>,
}

/// The rail geometry of a slider whose value sits at `value` of its rail and whose fill grows
/// from `zero` of its rail, or from the rail's start (unipolar, no tick) when `zero` is `None`.
/// Both are rail fractions; the caller maps declared values to them.
pub fn rail_geometry(width: f32, radius: f32, value: f64, zero: Option<f64>) -> RailGeometry {
    let handle = handle_center(width, radius, value);
    let tick = zero.map(|zero| handle_center(width, radius, zero));
    let from = tick.unwrap_or(0.0);
    let fill = if (handle - from).abs() < f32::EPSILON {
        None
    } else {
        Some((from.min(handle), from.max(handle)))
    };
    RailGeometry { handle, fill, tick }
}

/// The clip `(x, y, width, height)` a rail line `width` × `height` points draws into, relative to
/// its own origin: the line itself, grown above and below to hold a halo of `halo_radius` centred
/// on it when the halo is taller than the line. Only the drawing reaches past the line; its layout
/// does not.
pub fn rail_clip(width: f32, height: f32, halo_radius: f32) -> (f32, f32, f32, f32) {
    let reach = (halo_radius - height / 2.0).max(0.0);
    (0.0, -reach, width, height + 2.0 * reach)
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
    fn the_handle_travels_inside_its_own_radius() {
        assert_eq!(handle_center(276.0, 7.0, 0.0), 7.0);
        assert_eq!(handle_center(276.0, 7.0, 1.0), 269.0);
        assert_eq!(handle_center(276.0, 7.0, 0.5), 138.0);
        assert_eq!(handle_center(276.0, 7.0, 2.0), 269.0, "clamped to the rail");
    }

    /// The 18 pt halo reaches 3 pt above and below the 12 pt rail line; a line taller than the
    /// halo keeps its own bounds.
    #[test]
    fn the_rail_clip_holds_the_halo_centred_on_the_line() {
        assert_eq!(rail_clip(160.0, 12.0, 9.0), (0.0, -3.0, 160.0, 18.0));
        assert_eq!(rail_clip(160.0, 24.0, 9.0), (0.0, 0.0, 160.0, 24.0));
    }

    #[test]
    fn bipolar_fill_runs_from_the_tick_to_the_handle_on_either_side() {
        let up = rail_geometry(276.0, 7.0, 0.81, Some(0.5));
        assert_eq!(up.tick, Some(138.0));
        assert_eq!(up.fill, Some((138.0, handle_center(276.0, 7.0, 0.81))));
        let down = rail_geometry(276.0, 7.0, 0.3, Some(0.5));
        assert_eq!(down.fill, Some((handle_center(276.0, 7.0, 0.3), 138.0)));
    }

    #[test]
    fn value_at_zero_has_a_tick_and_no_fill() {
        let rail = rail_geometry(276.0, 7.0, 0.5, Some(0.5));
        assert_eq!(rail.fill, None);
        assert_eq!(rail.tick, Some(rail.handle));
    }

    #[test]
    fn only_a_zero_inside_the_rail_draws_a_tick() {
        assert_eq!(zero_fraction(-100.0, 100.0, Some(0.0)), Some(0.5));
        assert_eq!(
            zero_fraction(0.0, 100.0, Some(0.0)),
            None,
            "unipolar from the minimum"
        );
        assert_eq!(zero_fraction(0.0, 100.0, Some(100.0)), None);
        assert_eq!(zero_fraction(0.0, 100.0, None), None);
    }

    #[test]
    fn unipolar_fills_from_the_rail_start_without_a_tick() {
        let rail = rail_geometry(276.0, 7.0, 0.5, None);
        assert_eq!(rail.tick, None);
        assert_eq!(rail.fill, Some((0.0, 138.0)));
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

    fn codes(colour: [f32; 3]) -> [u8; 3] {
        colour.map(|channel| (channel * 255.0).round() as u8)
    }

    fn unit(colour: [u8; 3]) -> [f32; 3] {
        colour.map(|channel| f32::from(channel) / 255.0)
    }

    #[test]
    fn a_colour_rail_mixes_its_evenly_spaced_stops_in_srgb() {
        let stops = [unit([255, 0, 255]), unit([255, 0, 0]), unit([255, 128, 0])];
        assert_eq!(codes(rail_colour_at(&stops, 0.0)), [255, 0, 255]);
        assert_eq!(codes(rail_colour_at(&stops, 0.5)), [255, 0, 0]);
        assert_eq!(codes(rail_colour_at(&stops, 1.0)), [255, 128, 0]);
        // A fifth of the way along the second half: sRGB 0.4 × 128, not linear light's 83.
        assert_eq!(codes(rail_colour_at(&stops, 0.7)), [255, 51, 0]);
        assert_eq!(codes(rail_colour_at(&[], 0.3)), [0, 0, 0]);
        assert_eq!(codes(rail_colour_at(&stops[..1], 0.3)), [255, 0, 255]);
    }

    /// Sampled from colour-mixer.png's Red hue, Red saturation and Red luminance rails: the declared
    /// stops mixed in sRGB and laid over the panel at 85%, to within two 8-bit codes.
    #[test]
    fn colour_rails_match_the_mixer_reference_samples() {
        let panel = unit([0x20, 0x20, 0x23]);
        let drawn = |stops: &[[u8; 3]], t: f32| {
            let stops: Vec<[f32; 3]> = stops.iter().copied().map(unit).collect();
            codes(over(rail_colour_at(&stops, t), panel, 0.85))
        };
        let close = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 2);
        let hue = [[255, 0, 255], [255, 0, 0], [255, 128, 0]];
        assert!(close(drawn(&hue, 0.2), [221, 5, 134]));
        assert!(close(drawn(&hue, 0.8), [222, 69, 5]));
        let saturation = [[128, 128, 128], [255, 0, 0]];
        assert!(close(drawn(&saturation, 0.0), [114, 113, 114]));
        assert!(close(drawn(&saturation, 0.3), [146, 81, 81]));
        let luminance = [[63, 0, 0], [255, 0, 0], [255, 153, 153]];
        assert!(close(drawn(&luminance, 0.2), [125, 5, 5]));
        assert!(close(drawn(&luminance, 0.8), [222, 82, 83]));
    }

    #[test]
    fn a_rail_is_cut_into_pieces_no_longer_than_asked() {
        assert_eq!(rail_pieces(16.0, 8.0), vec![0.0, 0.5, 1.0]);
        let pieces = rail_pieces(276.0, 8.0);
        assert_eq!(pieces.len(), 36);
        assert_eq!((pieces[0], pieces[35]), (0.0, 1.0));
        assert_eq!(rail_pieces(0.0, 8.0), vec![0.0, 1.0]);
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
