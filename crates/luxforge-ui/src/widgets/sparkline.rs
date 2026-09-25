//! A sparkline: one resource's recent history as a line over a filled area and a baseline, with a
//! dot on the newest point.
//!
//! Like the histogram, this widget holds no logic: it is handed values that are **already
//! normalised** into `0.0..=1.0` against whatever scale the caller chose, oldest first, and the
//! length of the window they belong to. It never sees a byte count, a percentage or a scale rule,
//! so it cannot disagree with the figure printed beside it about what the line means.

use crate::theme;
use iced::widget::canvas;
use iced::widget::canvas::{LineCap, LineJoin, Path, Stroke, path};
use iced::{Element, Length, Point, Rectangle, Renderer, Size, Theme};
use std::cell::Cell;

/// One sparkline's series.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SparklineModel {
    /// Oldest first, each in `0.0..=1.0`. A value outside that range is clamped and a non-finite
    /// one is drawn as zero when the line is built, so no caller arithmetic can draw outside the
    /// sparkline's box.
    pub values: Vec<f32>,
    /// The window's length in samples, which fixes the horizontal scale: a shorter series starts
    /// part-way across with its newest point at the right edge, and a longer one shows only its
    /// newest `capacity` values.
    pub capacity: usize,
    /// A cheap identity for the series: the caller changes it whenever `values` or `capacity`
    /// change and holds it steady otherwise. The line is tessellated only when it moves, so a
    /// redraw driven by anything else (a hover, another row's sample) reuses the cached geometry.
    /// The widget never inspects what the number means, only whether it moved.
    pub version: u64,
}

/// Where each shown value's point falls in a sparkline of `size`, oldest first.
///
/// The window has `capacity` evenly spaced slots from the left edge to the right edge, inset by
/// [`theme::SPARKLINE_DOT_RADIUS`] so the newest point's dot is never clipped. Sample `i` of the
/// `n` shown sits in slot `capacity - n + i`, so the newest is always in the last slot and a
/// series shorter than the window starts part-way across. Only the newest `capacity` values are
/// shown, so a longer series never compresses the time scale. A value's height runs from the
/// bottom inset (zero) to the top inset (one); it is clamped into `0.0..=1.0`, and a non-finite
/// value is treated as zero. A size smaller than the insets shrinks them rather than placing a
/// point outside the box.
pub fn sparkline_points(values: &[f32], capacity: usize, size: Size) -> Vec<Point> {
    let shown = values.len().min(capacity);
    let values = &values[values.len() - shown..];
    let (width, height) = (size.width.max(0.0), size.height.max(0.0));
    let inset_x = theme::SPARKLINE_DOT_RADIUS.min(width / 2.0);
    let inset_y = theme::SPARKLINE_DOT_RADIUS.min(height / 2.0);
    let (span_x, span_y) = (width - 2.0 * inset_x, height - 2.0 * inset_y);
    let first_slot = capacity - shown;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let slot = first_slot + index;
            let x = if capacity > 1 {
                inset_x + span_x * slot as f32 / (capacity - 1) as f32
            } else {
                width - inset_x
            };
            let value = if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                0.0
            };
            Point::new(x, height - inset_y - value * span_y)
        })
        .collect()
}

/// A sparkline [`theme::SPARKLINE_HEIGHT`] tall that fills the width its row leaves it: a 1 px
/// baseline in the rule colour along the bottom, the area under the line filled opaque, the line
/// in the rail's fill colour and the newest point a dot in the thumb's colour. An empty series
/// draws only the baseline and a single value only the dot.
pub fn sparkline<'a, M: 'a>(model: &SparklineModel) -> Element<'a, M> {
    canvas(Trace {
        values: model.values.clone(),
        capacity: model.capacity,
        version: model.version,
    })
    .width(Length::Fill)
    .height(Length::Fixed(theme::SPARKLINE_HEIGHT))
    .into()
}

/// The canvas program. It owns a copy of the series (at most a window of floats) because a canvas
/// program outlives the view call that built it.
struct Trace {
    values: Vec<f32>,
    capacity: usize,
    version: u64,
}

/// The tessellated geometry and the series version it was built from, which survive across view
/// calls in the widget tree (a fresh [`Trace`] is built every time).
#[derive(Default)]
struct TraceState {
    cache: canvas::Cache,
    version: Cell<Option<u64>>,
}

impl<M> canvas::Program<M> for Trace {
    type State = TraceState;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        // The series only changes when the caller's version moves; `Cache` itself invalidates on a
        // bounds change, so this covers only what the widget cannot infer from `bounds`.
        if state.version.get() != Some(self.version) {
            state.cache.clear();
            state.version.set(Some(self.version));
        }
        let geometry = state.cache.draw(renderer, bounds.size(), |frame| {
            draw_trace(frame, &self.values, self.capacity);
        });
        vec![geometry]
    }
}

fn draw_trace(frame: &mut canvas::Frame, values: &[f32], capacity: usize) {
    let size = frame.size();
    let baseline_top = size.height - theme::BORDER_WIDTH;
    let points = sparkline_points(values, capacity, size);

    if let [first, .., last] = points.as_slice() {
        // The area closes along the top of the baseline, so the baseline keeps its own colour.
        let mut area = path::Builder::new();
        area.move_to(Point::new(first.x, baseline_top));
        for point in &points {
            area.line_to(*point);
        }
        area.line_to(Point::new(last.x, baseline_top));
        area.close();
        frame.fill(&area.build(), theme::SPARKLINE_AREA);
    }

    frame.fill_rectangle(
        Point::new(0.0, baseline_top),
        Size::new(size.width, theme::BORDER_WIDTH),
        theme::RULE,
    );

    if let [first, rest @ ..] = points.as_slice()
        && !rest.is_empty()
    {
        let mut line = path::Builder::new();
        line.move_to(*first);
        for point in rest {
            line.line_to(*point);
        }
        frame.stroke(
            &line.build(),
            Stroke::default()
                .with_color(theme::RAIL_FILL)
                .with_width(theme::SPARKLINE_LINE_WIDTH)
                .with_line_join(LineJoin::Round)
                .with_line_cap(LineCap::Round),
        );
    }

    if let Some(newest) = points.last() {
        frame.fill(
            &Path::circle(*newest, theme::SPARKLINE_DOT_RADIUS),
            theme::THUMB,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: f32 = theme::SPARKLINE_DOT_RADIUS;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// A full window spans the whole width between the insets, evenly, oldest on the left.
    #[test]
    fn a_full_window_spans_the_width_between_the_dot_insets() {
        let size = Size::new(59.0 + 2.0 * R, 16.0);
        let values = vec![0.5; 60];
        let points = sparkline_points(&values, 60, size);
        assert_eq!(points.len(), 60);
        for (index, point) in points.iter().enumerate() {
            assert!(close(point.x, R + index as f32), "{index}: {point:?}");
        }
        assert!(
            close(points[59].x, size.width - R),
            "the newest at the right"
        );
    }

    /// A series shorter than the window starts part-way across and ends at the right edge: sample
    /// `i` of `n` sits in slot `capacity - n + i`.
    #[test]
    fn a_partial_series_starts_part_way_across_with_its_newest_at_the_right() {
        let size = Size::new(59.0 + 2.0 * R, 16.0);
        let points = sparkline_points(&[0.0, 0.25, 1.0], 60, size);
        assert_eq!(points.len(), 3);
        assert!(close(points[0].x, R + 57.0));
        assert!(close(points[1].x, R + 58.0));
        assert!(close(points[2].x, size.width - R));
    }

    /// A longer series shows its newest window, so the time scale never compresses.
    #[test]
    fn a_series_longer_than_the_window_shows_its_newest_values() {
        let size = Size::new(10.0 + 2.0 * R, 16.0);
        let values: Vec<f32> = (0..15).map(|index| index as f32 / 14.0).collect();
        let points = sparkline_points(&values, 11, size);
        let expected = sparkline_points(&values[4..], 11, size);
        assert_eq!(points, expected);
        assert!(close(points[0].x, R));
    }

    /// Zero sits on the bottom inset, one on the top inset, and a half halfway between them.
    #[test]
    fn values_map_from_the_bottom_inset_to_the_top_inset() {
        let size = Size::new(100.0, 16.0);
        let points = sparkline_points(&[0.0, 0.5, 1.0], 3, size);
        assert!(close(points[0].y, 16.0 - R));
        assert!(close(points[1].y, 8.0));
        assert!(close(points[2].y, R));
        assert!(close(points[0].x, R) && close(points[1].x, 50.0));
    }

    /// The widget clamps rather than trusting: no value a caller passes can draw outside the box.
    #[test]
    fn out_of_range_and_non_finite_values_are_clamped_into_the_box() {
        let size = Size::new(100.0, 16.0);
        let values = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.5, 4.0, 1.0];
        let points = sparkline_points(&values, 6, size);
        let bottom = 16.0 - R;
        assert!(close(points[0].y, bottom), "NaN is zero");
        assert!(close(points[1].y, bottom), "infinity is zero");
        assert!(close(points[2].y, bottom), "negative infinity is zero");
        assert!(close(points[3].y, bottom), "a negative value is zero");
        assert!(close(points[4].y, R), "an over-range value is one");
        assert!(close(points[5].y, R));
        for point in &points {
            assert!(
                (0.0..=100.0).contains(&point.x) && (0.0..=16.0).contains(&point.y),
                "{point:?} left the box"
            );
        }
    }

    #[test]
    fn an_empty_series_has_no_points() {
        assert!(sparkline_points(&[], 60, Size::new(100.0, 16.0)).is_empty());
        assert!(sparkline_points(&[0.5], 0, Size::new(100.0, 16.0)).is_empty());
    }

    /// A single value is one point, in the last slot at the right edge, which draws only the dot.
    #[test]
    fn a_single_value_sits_at_the_right_edge() {
        let size = Size::new(100.0, 16.0);
        for capacity in [1, 2, 60] {
            let points = sparkline_points(&[1.0], capacity, size);
            assert_eq!(
                points,
                vec![Point::new(100.0 - R, R)],
                "capacity {capacity}"
            );
        }
    }

    /// A box smaller than the insets shrinks them: every point stays inside, and a zero-size box
    /// puts every point at its origin.
    #[test]
    fn a_zero_or_tiny_size_keeps_every_point_inside_the_box() {
        let values = [0.0, 0.5, 1.0, f32::NAN];
        let points = sparkline_points(&values, 60, Size::ZERO);
        assert_eq!(points.len(), 4);
        assert!(points.iter().all(|point| *point == Point::ORIGIN));
        let tiny = Size::new(2.0, 1.0);
        for point in sparkline_points(&values, 4, tiny) {
            assert!((0.0..=2.0).contains(&point.x) && (0.0..=1.0).contains(&point.y));
        }
        let negative = sparkline_points(&values, 4, Size::new(-5.0, -5.0));
        assert!(negative.iter().all(|point| *point == Point::ORIGIN));
    }

    #[test]
    fn every_series_shape_builds() {
        for values in [vec![], vec![0.4], vec![0.1, 0.9, f32::NAN]] {
            let _: Element<'_, ()> = sparkline(&SparklineModel {
                values,
                capacity: 60,
                version: 1,
            });
        }
    }
}
