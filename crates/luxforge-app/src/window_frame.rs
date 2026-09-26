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

/// The window's settings: its logical size, whether it is ever shown, and its frame.
///
/// A full-size content view makes the window's content the whole window, so `size` is the whole
/// window on macOS as it is the content area elsewhere, and a hidden evidence launch still captures
/// exactly `size` logical points.
pub(crate) fn settings(size: (f32, f32), visible: bool) -> iced::window::Settings {
    iced::window::Settings {
        size: size.into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_follows_the_platform() {
        let settings = settings((1440.0, 900.0), false);
        assert_eq!(settings.size, iced::Size::new(1440.0, 900.0));
        assert!(!settings.visible);
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
    }
}
