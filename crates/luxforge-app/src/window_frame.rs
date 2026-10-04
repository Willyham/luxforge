//! The window's frame, decided in one place for every platform.
//!
//! On macOS the app's title bar is the window's: the native title bar is transparent, its title is
//! hidden and the content view fills the whole window, so the traffic lights sit inside the app's
//! own 44 pt bar, at its leading edge. The bar leaves them [`TITLE_BAR_LEADING`] and moves the
//! window when its empty area is pressed. Windows and Linux keep their native frames and title
//! bars, so the app's bar starts at the ordinary inset and never drags the window itself.
//!
//! AppKit places the traffic lights for a standard title bar, near the top of the window; they are
//! not centred on the taller bar, and nothing in Iced's window settings moves them.
use luxforge_core::preferences::{WINDOW_SIZE_RANGE, WindowFrame};
use luxforge_ui::theme;

/// The app's title bar is the window's title bar, so the traffic lights sit in it and it drags the
/// window.
pub(crate) const INTEGRATED_TITLE_BAR: bool = cfg!(target_os = "macos");

/// The title bar's leading padding: room for the traffic lights where they sit in the bar, and the
/// bar's ordinary inset where the native frame keeps them.
pub(crate) const TITLE_BAR_LEADING: f32 = if INTEGRATED_TITLE_BAR {
    88.0
} else {
    theme::TITLE_BAR_INSET
};

/// The title bar's leading padding for the window as it is: fullscreen hides the traffic lights,
/// so the bar starts at its ordinary inset there.
pub(crate) fn title_bar_leading(fullscreen: bool) -> f32 {
    if fullscreen {
        theme::TITLE_BAR_INSET
    } else {
        TITLE_BAR_LEADING
    }
}

/// The window's settings: its logical size, whether it is ever shown, and its frame.
///
/// A full-size content view makes the window's content the whole window, so `size` is the whole
/// window on macOS as it is the content area elsewhere, and a hidden evidence launch still captures
/// exactly `size` logical points. A remembered frame opens at its position, in the system's points;
/// without one the system places the window.
pub(crate) fn settings(
    size: (f32, f32),
    position: Option<(f32, f32)>,
    visible: bool,
) -> iced::window::Settings {
    iced::window::Settings {
        size: size.into(),
        position: position.map_or(iced::window::Position::Default, |(x, y)| {
            iced::window::Position::Specific(iced::Point::new(x, y))
        }),
        visible,
        exit_on_close_request: false,
        #[cfg(target_os = "macos")]
        platform_specific: iced::window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        ..iced::window::Settings::default()
    }
}

/// The interface size as a scale over the system's: Iced's logical points are the system's
/// divided by it.
pub(crate) fn interface_scale(interface_size: u16) -> f32 {
    f32::from(interface_size) / 100.0
}

/// The remembered frame a launch opens at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Opening {
    /// The frame as stored, in the system's points.
    pub(crate) frame: WindowFrame,
    /// Iced's logical size for it at the launch's interface size, so the window opens at the
    /// stored size in the system's points whatever the interface size.
    pub(crate) size: (f32, f32),
}

impl Opening {
    pub(crate) fn new(frame: WindowFrame, interface_size: u16) -> Self {
        let scale = interface_scale(interface_size);
        Self {
            frame,
            size: (frame.width / scale, frame.height / scale),
        }
    }

    /// Where the window opens, in the system's points.
    pub(crate) fn position(&self) -> (f32, f32) {
        (self.frame.x, self.frame.y)
    }
}

/// Whether this launch's window frame is remembered, and whether the close has asked for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowMemory {
    /// The close stores the window's frame: a launch that shows its window, outside an evidence
    /// run.
    pub(crate) remember: bool,
    /// The close has asked Iced for the frame, which is stored, or not, once Iced answers.
    pub(crate) asked: bool,
    /// Iced has not answered yet, so a second close request waits for it too.
    pub(crate) waiting: bool,
}

/// What Iced reports of the open window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WindowReport {
    /// The window's size in Iced's logical points.
    pub(crate) size: iced::Size,
    /// Its position in the system's points: Iced converts by the system's factor alone. `None`
    /// where the platform does not report one.
    pub(crate) position: Option<iced::Point>,
    /// The size of the display holding it, in Iced's logical points, or `None` when no display
    /// holds it. Iced reports no display's origin.
    pub(crate) display: Option<iced::Size>,
    pub(crate) fullscreen: bool,
}

impl WindowReport {
    /// The window's frame in the system's points: Iced's logical size times the interface scale.
    /// `None` without a position.
    pub(crate) fn frame(&self, scale: f32) -> Option<WindowFrame> {
        let position = self.position?;
        Some(WindowFrame {
            width: self.size.width * scale,
            height: self.size.height * scale,
            x: position.x,
            y: position.y,
        })
    }

    /// The frame a close stores: not in fullscreen, which keeps the frame stored before, and only
    /// a finite one, its size held within the range `preferences.set` takes.
    pub(crate) fn stored_frame(&self, scale: f32, fullscreen: bool) -> Option<WindowFrame> {
        if fullscreen || self.fullscreen {
            return None;
        }
        let frame = self.frame(scale)?;
        let size = |value: f32| {
            value
                .is_finite()
                .then(|| value.clamp(*WINDOW_SIZE_RANGE.start(), *WINDOW_SIZE_RANGE.end()))
        };
        Some(WindowFrame {
            width: size(frame.width)?,
            height: size(frame.height)?,
            ..frame
        })
        .filter(|frame| frame.x.is_finite() && frame.y.is_finite())
    }

    /// The display's size in the system's points.
    pub(crate) fn display(&self, scale: f32) -> Option<(f32, f32)> {
        self.display
            .map(|display| (display.width * scale, display.height * scale))
    }
}

/// Ask Iced where the window is: its size, position, display and mode, or `None` without a
/// window.
pub(crate) fn report() -> iced::Task<Option<WindowReport>> {
    use iced::{Task, window};
    window::oldest().then(|id| {
        let Some(id) = id else {
            return Task::done(None);
        };
        window::size(id).then(move |size| {
            window::position(id).then(move |position| {
                window::monitor_size(id).then(move |display| {
                    window::mode(id).map(move |mode| {
                        Some(WindowReport {
                            size,
                            position,
                            display,
                            fullscreen: mode == window::Mode::Fullscreen,
                        })
                    })
                })
            })
        })
    })
}

/// What to do with a window opened at its remembered frame once Iced reports where it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Placement {
    /// The frame lies within its display.
    Keep,
    /// No display holds the window, or one away from the main display cannot hold its size: move
    /// it to the main display's origin and check again there.
    ToMain,
    /// Move and resize the window to this frame, in the system's points: centred on the display
    /// at the origin, at the stored size shrunk to fit.
    Fit(WindowFrame),
}

/// Whether `frame`, in the system's points, lies within `display`, the size of the display Iced
/// says holds the window, or `None` when none does.
///
/// Iced reports a display's size but not its origin, and the main display's origin is the
/// system's. So a display is taken to be the main display when `on_main` (the window was just
/// moved there) or when it holds the frame's top-left corner measured from the origin; there the
/// whole frame must lie within it. A display elsewhere, whose origin is unknown, keeps a frame it
/// is large enough for.
pub(crate) fn placement(
    frame: WindowFrame,
    display: Option<(f32, f32)>,
    on_main: bool,
) -> Placement {
    let Some((width, height)) = display else {
        // Moved to the main display and still on none: nothing better to try.
        return if on_main {
            Placement::Keep
        } else {
            Placement::ToMain
        };
    };
    let at_origin = on_main || (0.0..width).contains(&frame.x) && (0.0..height).contains(&frame.y);
    let fits = frame.width <= width && frame.height <= height;
    if !at_origin {
        return if fits {
            Placement::Keep
        } else {
            Placement::ToMain
        };
    }
    let within = frame.x >= 0.0
        && frame.y >= 0.0
        && frame.x + frame.width <= width
        && frame.y + frame.height <= height;
    if within {
        return Placement::Keep;
    }
    let (fit_width, fit_height) = (frame.width.min(width), frame.height.min(height));
    Placement::Fit(WindowFrame {
        width: fit_width,
        height: fit_height,
        x: (width - fit_width) / 2.0,
        y: (height - fit_height) / 2.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_follows_the_platform() {
        let settings = settings((1440.0, 900.0), None, false);
        assert_eq!(settings.size, iced::Size::new(1440.0, 900.0));
        assert!(!settings.visible);
        assert!(matches!(settings.position, iced::window::Position::Default));
        if cfg!(target_os = "macos") {
            assert_eq!(TITLE_BAR_LEADING, 88.0);
            #[cfg(target_os = "macos")]
            {
                let platform = settings.platform_specific;
                assert!(
                    platform.title_hidden
                        && platform.titlebar_transparent
                        && platform.fullsize_content_view
                );
            }
        } else {
            assert_eq!(TITLE_BAR_LEADING, theme::TITLE_BAR_INSET);
        }
        assert_eq!(title_bar_leading(false), TITLE_BAR_LEADING);
        assert_eq!(title_bar_leading(true), theme::TITLE_BAR_INSET);
    }

    fn frame(width: f32, height: f32, x: f32, y: f32) -> WindowFrame {
        WindowFrame {
            width,
            height,
            x,
            y,
        }
    }

    #[test]
    fn a_remembered_window_opens_at_its_frame_in_the_systems_points() {
        let opening = Opening::new(frame(1250.0, 750.0, -1200.5, 40.0), 125);
        assert_eq!(opening.size, (1000.0, 600.0));
        assert_eq!(opening.position(), (-1200.5, 40.0));
        let settings = settings(opening.size, Some(opening.position()), true);
        assert!(matches!(
            settings.position,
            iced::window::Position::Specific(point) if point == iced::Point::new(-1200.5, 40.0)
        ));
        assert_eq!(
            Opening::new(frame(1440.0, 900.0, 0.0, 0.0), 100).size,
            (1440.0, 900.0)
        );
    }

    #[test]
    fn a_remembered_window_frame_outside_its_display_is_centred_and_shrunk_to_fit() {
        let display = Some((1512.0, 982.0));
        // Within the main display, and on a display elsewhere large enough for it.
        assert_eq!(
            placement(frame(1440.0, 900.0, 36.0, 40.0), display, false),
            Placement::Keep
        );
        assert_eq!(
            placement(frame(1440.0, 900.0, -1920.0, 100.0), display, false),
            Placement::Keep
        );
        // No display holds it: to the main display, then checked there.
        assert_eq!(
            placement(frame(1440.0, 900.0, 4000.0, 100.0), None, false),
            Placement::ToMain
        );
        assert_eq!(
            placement(frame(1440.0, 900.0, 0.0, 0.0), None, true),
            Placement::Keep
        );
        // A display elsewhere too small for it: to the main display.
        assert_eq!(
            placement(frame(2400.0, 1300.0, -2560.0, 0.0), display, false),
            Placement::ToMain
        );
        // Partly off the main display: centred there at its size.
        assert_eq!(
            placement(frame(1440.0, 900.0, 600.0, 40.0), display, false),
            Placement::Fit(frame(1440.0, 900.0, 36.0, 41.0))
        );
        // Larger than the main display: centred and shrunk to fit.
        assert_eq!(
            placement(frame(2400.0, 900.0, 0.0, 0.0), display, true),
            Placement::Fit(frame(1512.0, 900.0, 0.0, 41.0))
        );
    }

    #[test]
    fn the_window_frame_stored_at_close_is_in_the_systems_points_and_never_in_fullscreen() {
        let report = WindowReport {
            size: iced::Size::new(1000.0, 600.0),
            position: Some(iced::Point::new(-1200.5, 40.0)),
            display: Some(iced::Size::new(1209.6, 785.6)),
            fullscreen: false,
        };
        // Iced's logical size times the interface size; the position is the system's already.
        assert_eq!(
            report.stored_frame(1.25, false),
            Some(frame(1250.0, 750.0, -1200.5, 40.0))
        );
        assert_eq!(
            report.stored_frame(1.0, false),
            Some(frame(1000.0, 600.0, -1200.5, 40.0))
        );
        let display = report.display(1.25).unwrap();
        assert!((display.0 - 1512.0).abs() < 0.01 && (display.1 - 982.0).abs() < 0.01);
        // Fullscreen, by Iced's mode or the desktop's own state, keeps the frame stored before.
        assert_eq!(report.stored_frame(1.0, true), None);
        let fullscreen = WindowReport {
            fullscreen: true,
            ..report
        };
        assert_eq!(fullscreen.stored_frame(1.0, false), None);
        // No position, nothing to store; a size outside the range is held within it.
        let unplaced = WindowReport {
            position: None,
            ..report
        };
        assert_eq!(unplaced.stored_frame(1.0, false), None);
        let tiny = WindowReport {
            size: iced::Size::new(200.0, 600.0),
            ..report
        };
        assert_eq!(
            tiny.stored_frame(1.0, false),
            Some(frame(320.0, 600.0, -1200.5, 40.0))
        );
    }
}
