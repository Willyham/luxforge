//! Whether a canvas tool that draws its own cursor — the mask brush, the Corrections brush — draws
//! it now, or leaves the pointer to the system.
use iced::{Point, Rectangle, mouse::Cursor};

/// Where a brush draws its own cursor, in the canvas's coordinates: at the pointer while it is over
/// the canvas proper and nothing is open over the canvas. `None` otherwise, and the system's cursor
/// is the one shown: outside the canvas; over the palette, a sheet or a menu, each of which claims
/// the pointer, so the canvas is handed a levitating or unavailable cursor; and anywhere while one
/// of them is open (`covered`, [`crate::state::canvas::CanvasModel::covered`]), since a press then
/// closes it rather than painting.
pub(crate) fn brush_cursor(cursor: Cursor, bounds: Rectangle, covered: bool) -> Option<Point> {
    if covered {
        return None;
    }
    cursor.position_in(bounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Size;

    #[test]
    fn the_brush_cursor_is_drawn_only_over_an_uncovered_canvas() {
        let bounds = Rectangle::new(Point::new(100.0, 50.0), Size::new(400.0, 300.0));
        let inside = Point::new(150.0, 80.0);
        // Over the canvas with nothing open: drawn at the pointer, in the canvas's coordinates.
        assert_eq!(
            brush_cursor(Cursor::Available(inside), bounds, false),
            Some(Point::new(50.0, 30.0))
        );
        // Anything open over the canvas: the system's cursor, even with the pointer over the
        // photograph.
        assert_eq!(brush_cursor(Cursor::Available(inside), bounds, true), None);
        // A menu or sheet layered above claims the pointer, and the canvas sees it levitating.
        assert_eq!(
            brush_cursor(Cursor::Levitating(inside), bounds, false),
            None
        );
        // A window overlay over the canvas hands it no cursor at all.
        assert_eq!(brush_cursor(Cursor::Unavailable, bounds, false), None);
        // Off the canvas: the panels, the title bar, the status bar.
        assert_eq!(
            brush_cursor(Cursor::Available(Point::new(20.0, 80.0)), bounds, false),
            None
        );
    }
}
