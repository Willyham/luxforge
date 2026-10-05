//! Luxforge's own Iced theme: the palette as a runtime value.
//!
//! Iced hands the application's theme to every style function and every canvas program's `draw`,
//! so each reads its colours from the [`Theme`] it is given and no global holds a palette. What
//! Iced styles itself (an unstyled scrollable, a checkbox, a text input's defaults, a pick list and
//! its menu, the window's clear colour) comes from an `iced::Theme` built from the roles and held
//! inside, so Luxforge Dark draws exactly what the fixed `iced::Theme` drew before themes.

use super::Palette;
use iced::theme::{self, Base};
use iced::widget::{
    button, checkbox, container, overlay::menu, pick_list, progress_bar, scrollable, slider, text,
    text_input,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Whether a theme is dark or light. Iced's window follows it: iced_winit sets the native window's
/// appearance from the theme's mode when the window opens and whenever the mode changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    #[default]
    Dark,
    Light,
}

/// The interface's theme: a resolved [`Palette`], its [`Mode`] and a generation number.
///
/// Cloning is one reference count: the application returns its theme to Iced after every update.
/// Every theme built gets a generation no other theme in the process has, so a canvas cache that
/// bakes a colour keys on [`Theme::generation`] and rebuilds once when the theme changes.
#[derive(Debug, Clone)]
pub struct Theme {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    palette: Palette,
    mode: Mode,
    generation: u64,
    /// Iced's own theme, built from the roles, for what Iced styles itself.
    iced: iced::Theme,
}

/// The next generation to hand out. A counter, not a palette: nothing global holds a colour.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

impl Theme {
    /// A theme drawing `palette` in `mode`, with a fresh generation.
    pub fn new(palette: Palette, mode: Mode) -> Self {
        let iced = iced::Theme::custom(
            "Luxforge".to_string(),
            theme::Palette {
                background: palette.background,
                text: palette.text,
                primary: palette.accent,
                success: palette.accent,
                warning: palette.accent,
                danger: palette.error,
            },
        );
        Self {
            inner: Arc::new(Inner {
                palette,
                mode,
                generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
                iced,
            }),
        }
    }

    /// Luxforge Dark, the default theme: the visual language exactly.
    pub fn luxforge_dark() -> Self {
        Self::new(Palette::luxforge_dark(), Mode::Dark)
    }

    /// The resolved colours this theme draws.
    pub fn palette(&self) -> &Palette {
        &self.inner.palette
    }

    /// Whether the theme is dark or light.
    pub fn mode(&self) -> Mode {
        self.inner.mode
    }

    /// This theme's generation, unique in the process; a canvas cache's key holds it.
    pub fn generation(&self) -> u64 {
        self.inner.generation
    }

    /// The `iced::Theme` built from the roles, which Iced's own default styles read.
    pub fn iced(&self) -> &iced::Theme {
        &self.inner.iced
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::luxforge_dark()
    }
}

impl Base for Theme {
    fn default(_preference: theme::Mode) -> Self {
        // Luxforge chooses its own theme rather than following the system's preference.
        Self::luxforge_dark()
    }

    fn mode(&self) -> theme::Mode {
        match self.inner.mode {
            Mode::Dark => theme::Mode::Dark,
            Mode::Light => theme::Mode::Light,
        }
    }

    fn base(&self) -> theme::Style {
        self.inner.iced.base()
    }

    fn palette(&self) -> Option<theme::Palette> {
        Base::palette(&self.inner.iced)
    }

    fn name(&self) -> &str {
        Base::name(&self.inner.iced)
    }
}

impl text::Catalog for Theme {
    type Class<'a> = text::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme: &Theme| text::default(theme.iced()))
    }

    fn style(&self, class: &Self::Class<'_>) -> text::Style {
        class(self)
    }
}

impl container::Catalog for Theme {
    type Class<'a> = container::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(container::transparent)
    }

    fn style(&self, class: &Self::Class<'_>) -> container::Style {
        class(self)
    }
}

impl button::Catalog for Theme {
    type Class<'a> = button::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme: &Theme, status| button::primary(theme.iced(), status))
    }

    fn style(&self, class: &Self::Class<'_>, status: button::Status) -> button::Style {
        class(self, status)
    }
}

impl scrollable::Catalog for Theme {
    type Class<'a> = scrollable::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme: &Theme, status| scrollable::default(theme.iced(), status))
    }

    fn style(&self, class: &Self::Class<'_>, status: scrollable::Status) -> scrollable::Style {
        class(self, status)
    }
}

impl text_input::Catalog for Theme {
    type Class<'a> = text_input::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme: &Theme, status| text_input::default(theme.iced(), status))
    }

    fn style(&self, class: &Self::Class<'_>, status: text_input::Status) -> text_input::Style {
        class(self, status)
    }
}

impl slider::Catalog for Theme {
    type Class<'a> = slider::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme: &Theme, status| slider::default(theme.iced(), status))
    }

    fn style(&self, class: &Self::Class<'_>, status: slider::Status) -> slider::Style {
        class(self, status)
    }
}

impl checkbox::Catalog for Theme {
    type Class<'a> = checkbox::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme: &Theme, status| checkbox::primary(theme.iced(), status))
    }

    fn style(&self, class: &Self::Class<'_>, status: checkbox::Status) -> checkbox::Style {
        class(self, status)
    }
}

impl menu::Catalog for Theme {
    type Class<'a> = menu::StyleFn<'a, Self>;

    fn default<'a>() -> <Self as menu::Catalog>::Class<'a> {
        Box::new(|theme: &Theme| menu::default(theme.iced()))
    }

    fn style(&self, class: &<Self as menu::Catalog>::Class<'_>) -> menu::Style {
        class(self)
    }
}

impl pick_list::Catalog for Theme {
    type Class<'a> = pick_list::StyleFn<'a, Self>;

    fn default<'a>() -> <Self as pick_list::Catalog>::Class<'a> {
        Box::new(|theme: &Theme, status| pick_list::default(theme.iced(), status))
    }

    fn style(
        &self,
        class: &<Self as pick_list::Catalog>::Class<'_>,
        status: pick_list::Status,
    ) -> pick_list::Style {
        class(self, status)
    }
}

impl progress_bar::Catalog for Theme {
    type Class<'a> = progress_bar::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme: &Theme| progress_bar::primary(theme.iced()))
    }

    fn style(&self, class: &Self::Class<'_>) -> progress_bar::Style {
        class(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_built_has_its_own_generation_and_clones_share_it() {
        let first = Theme::luxforge_dark();
        let second = Theme::luxforge_dark();
        assert_ne!(first.generation(), second.generation());
        let clone = first.clone();
        assert_eq!(clone.generation(), first.generation());
        assert!(
            Arc::ptr_eq(&clone.inner, &first.inner),
            "a clone is one count"
        );
    }

    /// The window's clear colour, text colour and mode are what the fixed `iced::Theme` gave
    /// before themes, so Luxforge Dark's window draws as it did.
    #[test]
    fn luxforge_dark_draws_what_the_fixed_iced_theme_drew() {
        let palette = Palette::luxforge_dark();
        let before = iced::Theme::custom(
            "Luxforge".to_string(),
            theme::Palette {
                background: palette.background,
                text: palette.text,
                primary: palette.accent,
                success: palette.accent,
                warning: palette.accent,
                danger: palette.error,
            },
        );
        let theme = Theme::luxforge_dark();
        assert_eq!(theme.base(), before.base());
        assert_eq!(Base::mode(&theme), theme::Mode::Dark);
        assert_eq!(Base::mode(&theme), Base::mode(&before));
        assert_eq!(Base::palette(&theme), Base::palette(&before));
        assert_eq!(Base::name(&theme), "Luxforge");
        let status = scrollable::Status::Active {
            is_horizontal_scrollbar_disabled: false,
            is_vertical_scrollbar_disabled: false,
        };
        let ours = <Theme as scrollable::Catalog>::default();
        assert_eq!(
            format!("{:?}", ours(&theme, status)),
            format!("{:?}", scrollable::default(&before, status))
        );
    }

    #[test]
    fn a_light_theme_reports_light_to_the_window() {
        let theme = Theme::new(Palette::luxforge_dark(), Mode::Light);
        assert_eq!(Base::mode(&theme), theme::Mode::Light);
        assert_eq!(theme.mode(), Mode::Light);
    }
}
