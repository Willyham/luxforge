//! The histogram plot and the two clipping triangles under it.
//!
//! Like every widget here, this one holds no logic: it is handed three arrays of 256 heights that
//! are **already normalized** into `0.0..=1.0` against whatever shared scale the caller chose, and
//! three colours to fill them with. It never sees a count, a channel name, an endpoint rule or a
//! render identity, so it cannot disagree with the model about what the plot means — and it cannot
//! normalize one channel differently from another, because it never sees the raw numbers at all.

use crate::theme;
use iced::{
    Color, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    widget::{button, canvas, container, text, tooltip},
};

/// How many bins one channel has: one per 8-bit output code, fixed by the histogram contract.
pub const BINS: usize = 256;

/// One plotted channel: its already-normalized heights and the colour to fill it with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistogramChannel {
    /// One height per output code, each in `0.0..=1.0`. Values outside that range are clamped when
    /// the polygon is built, so a caller's rounding can never draw outside the plot.
    pub bins: [f32; BINS],
    pub color: Color,
}

impl Default for HistogramChannel {
    fn default() -> Self {
        Self {
            bins: [0.0; BINS],
            color: theme::CHANNEL_RED,
        }
    }
}

/// The whole plot: three overlapping channel fills, and whether they are a stale result still on
/// screen while a newer reduction is in flight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistogramModel {
    pub channels: [HistogramChannel; 3],
    /// Dim the whole plot: the counts belong to an older generation than the one being rendered.
    pub stale: bool,
}

impl Default for HistogramModel {
    fn default() -> Self {
        Self {
            channels: [
                HistogramChannel {
                    bins: [0.0; BINS],
                    color: theme::CHANNEL_RED,
                },
                HistogramChannel {
                    bins: [0.0; BINS],
                    color: theme::CHANNEL_GREEN,
                },
                HistogramChannel {
                    bins: [0.0; BINS],
                    color: theme::CHANNEL_BLUE,
                },
            ],
            stale: false,
        }
    }
}

/// How much of its colour a stale plot keeps.
const STALE_ALPHA: f32 = 0.35;

/// Where one bin's column centre falls across a plot `width` wide. Bin 0 sits on the left edge and
/// bin 255 on the right edge, so the plot spans the whole output range with no margin of its own.
pub fn bin_x(index: usize, width: f32) -> f32 {
    if BINS <= 1 {
        return 0.0;
    }
    width * index as f32 / (BINS - 1) as f32
}

/// One channel's filled polygon over a plot of `size`: the baseline's left end, one point per bin,
/// then the baseline's right end, so the path closes along the bottom edge. Heights are clamped
/// into `0.0..=1.0` and a non-finite height is treated as zero, so no caller arithmetic can push a
/// point outside the plot.
pub fn polygon_points(bins: &[f32; BINS], size: Size) -> Vec<Point> {
    let baseline = size.height;
    let mut points = Vec::with_capacity(BINS + 2);
    points.push(Point::new(0.0, baseline));
    for (index, height) in bins.iter().enumerate() {
        let height = if height.is_finite() {
            height.clamp(0.0, 1.0)
        } else {
            0.0
        };
        points.push(Point::new(
            bin_x(index, size.width),
            baseline - height * baseline,
        ));
    }
    points.push(Point::new(size.width, baseline));
    points
}

/// The plot: three filled, overlapping channel polygons drawn over the panel surface at the design's
/// fixed height.
pub fn histogram<'a, M: 'a>(model: &HistogramModel) -> Element<'a, M> {
    container(
        canvas(Plot { model: *model })
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fixed(theme::HISTOGRAM_HEIGHT))
    .style(theme::control_surface)
    .into()
}

/// The canvas program. It owns a copy of the model (three 256-float arrays, 3 KiB) rather than
/// borrowing it, because a canvas program outlives the view call that built it.
struct Plot {
    model: HistogramModel,
}

impl<M> canvas::Program<M> for Plot {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let dim = if self.model.stale { STALE_ALPHA } else { 1.0 };
        for channel in &self.model.channels {
            let points = polygon_points(&channel.bins, bounds.size());
            let mut path = canvas::path::Builder::new();
            let Some(first) = points.first() else {
                continue;
            };
            path.move_to(*first);
            for point in &points[1..] {
                path.line_to(*point);
            }
            path.close();
            frame.fill(
                &path.build(),
                Color {
                    a: channel.color.a * theme::CHANNEL_ALPHA * dim,
                    ..channel.color
                },
            );
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        _bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> iced::mouse::Interaction {
        iced::mouse::Interaction::None
    }
}

/// One clipping triangle: the glyph, whether the plot found any pixel at that endpoint, and whether
/// its overlay is currently drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipTriangleModel {
    pub glyph: String,
    /// The rule this triangle follows, stated in full on hover.
    pub tooltip: String,
    /// The colour this endpoint's overlay uses; the glyph takes it when `tinted`.
    pub tint: Color,
    /// This endpoint has pixels, so the triangle is coloured rather than grey.
    pub tinted: bool,
    /// Its overlay is on, so the triangle is filled.
    pub active: bool,
    pub enabled: bool,
}

/// The triangle's own square, small enough for the two to sit in the plot's bottom corners.
const TRIANGLE_SIZE: f32 = 20.0;

/// One clipping triangle with the rule in its tooltip. It publishes `on_press` and nothing else:
/// which flag that toggles, and what the rule says, are the caller's.
pub fn clip_triangle<'a, M: Clone + 'a>(
    model: &ClipTriangleModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let tint = model.tint;
    let active = model.active;
    let glyph_color = match (model.enabled, model.tinted) {
        (false, _) => theme::TEXT_TERTIARY,
        (true, true) => tint,
        (true, false) => theme::TEXT_TERTIARY,
    };
    let control = button(
        container(
            text(model.glyph.clone())
                .size(theme::SIZE_CONTROL)
                .color(glyph_color),
        )
        .center(Length::Fill),
    )
    .width(Length::Fixed(TRIANGLE_SIZE))
    .height(Length::Fixed(TRIANGLE_SIZE))
    .padding(0.0)
    .style(move |theme: &Theme, status: button::Status| {
        let mut style = theme::button_plain(theme, status);
        if active {
            // An overlay that is on reads as a filled chip in its own clipping colour, which is the
            // same colour the overlay draws on the photograph.
            style.background = Some(iced::Background::Color(Color { a: 0.22, ..tint }));
            style.border = iced::Border {
                color: tint,
                width: theme::BORDER_WIDTH,
                radius: theme::RADIUS.into(),
            };
        }
        style
    })
    .on_press_maybe(if model.enabled { on_press } else { None });
    tooltip(
        control,
        container(
            text(model.tooltip.clone())
                .size(theme::SIZE_CAPTION)
                .color(theme::TEXT_PRIMARY),
        )
        .padding(6.0)
        .style(theme::bar_surface),
        tooltip::Position::Top,
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bins_span_the_whole_plot_width_evenly() {
        let width = 255.0;
        assert_eq!(bin_x(0, width), 0.0);
        assert_eq!(bin_x(BINS - 1, width), width);
        assert_eq!(bin_x(128, width), 128.0);
        // Every step is the same width, so no code is drawn wider than another.
        let step = bin_x(1, width) - bin_x(0, width);
        for index in 1..BINS {
            let measured = bin_x(index, width) - bin_x(index - 1, width);
            assert!((measured - step).abs() < 1e-4, "bin {index} is {measured}");
        }
    }

    #[test]
    fn a_polygon_closes_along_the_baseline_and_rises_with_its_bins() {
        let size = Size::new(255.0, 100.0);
        let mut bins = [0.0f32; BINS];
        bins[0] = 1.0;
        bins[255] = 0.5;
        let points = polygon_points(&bins, size);
        assert_eq!(points.len(), BINS + 2);
        // The first and last points are the baseline's two ends, so the fill has a flat bottom.
        assert_eq!(points[0], Point::new(0.0, 100.0));
        assert_eq!(points[BINS + 1], Point::new(255.0, 100.0));
        // A full bin reaches the top; an empty one stays on the baseline; a half bin is halfway.
        assert_eq!(points[1], Point::new(0.0, 0.0));
        assert_eq!(points[2], Point::new(1.0, 100.0));
        assert_eq!(points[BINS], Point::new(255.0, 50.0));
    }

    /// The widget clamps rather than trusting: no height a caller passes can draw outside the plot,
    /// including a non-finite one.
    #[test]
    fn heights_outside_the_unit_range_are_clamped_into_the_plot() {
        let size = Size::new(255.0, 100.0);
        let mut bins = [0.0f32; BINS];
        bins[0] = 4.0;
        bins[1] = -2.0;
        bins[2] = f32::NAN;
        bins[3] = f32::INFINITY;
        let points = polygon_points(&bins, size);
        for point in &points {
            assert!(
                (0.0..=100.0).contains(&point.y) && (0.0..=255.0).contains(&point.x),
                "{point:?} left the plot"
            );
        }
        assert_eq!(points[1].y, 0.0, "an over-range bin fills the plot");
        assert_eq!(points[2].y, 100.0, "a negative bin is empty");
        assert_eq!(points[3].y, 100.0, "a non-finite bin is empty");
        assert_eq!(points[4].y, 100.0, "an infinite bin is empty");
    }

    #[test]
    fn a_zero_width_plot_produces_a_degenerate_but_valid_polygon() {
        let points = polygon_points(&[0.0; BINS], Size::new(0.0, 0.0));
        assert_eq!(points.len(), BINS + 2);
        assert!(points.iter().all(|p| p.x == 0.0 && p.y == 0.0));
    }
}
