//! The interface's themed colours: one field per token of the UI themes design's Tokens table
//! (`docs/design/ui-themes.md`), in its order and under its names.
//!
//! The core keeps the same list for the API, and the two crates cannot see each other, so the
//! names are also listed in [`Palette::TOKEN_NAMES`], which a desktop test holds equal to the
//! core's. Luxforge Dark's values are the visual language's, exactly, alpha included.

use iced::Color;

/// Declares [`Palette`], [`Token`], the token names, the name-based reads and Luxforge Dark's
/// values from one list, so a token is named once.
macro_rules! palette {
    ($($(#[doc = $doc:literal])* $variant:ident $name:ident = $value:expr,)*) => {
        /// The resolved colour of every themed token. Copy, so a style function reads a field and
        /// a widget can keep the few it draws with.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Palette {
            $($(#[doc = $doc])* pub $name: Color,)*
        }

        /// One token of the [`Palette`], named where a colour is chosen before the theme that
        /// resolves it is known: a widget model's ink, read from the theme when the widget draws.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Token {
            $($(#[doc = $doc])* $variant,)*
        }

        impl Palette {
            /// Every token's name, in the design's table order. The API's names are these.
            pub const TOKEN_NAMES: [&'static str; 49] = [$(stringify!($name),)*];

            /// Luxforge Dark: the visual language's values, as the constants held them before
            /// themes, alpha included.
            pub const fn luxforge_dark() -> Self {
                Self { $($name: $value,)* }
            }

            /// The colour `token` resolves to.
            pub const fn get(&self, token: Token) -> Color {
                match token {
                    $(Token::$variant => self.$name,)*
                }
            }

            /// The token named `name`, or `None` for a name the palette does not hold.
            pub fn token(&self, name: &str) -> Option<Color> {
                Token::from_name(name).map(|token| self.get(token))
            }

            /// The token named `name`, to set from a resolved theme, or `None` for a name the
            /// palette does not hold.
            pub fn token_mut(&mut self, name: &str) -> Option<&mut Color> {
                match Token::from_name(name)? {
                    $(Token::$variant => Some(&mut self.$name),)*
                }
            }
        }

        impl Token {
            /// Every token, in the design's table order.
            pub const ALL: [Token; 49] = [$(Token::$variant,)*];

            /// The token's name, as the API and [`Palette::TOKEN_NAMES`] spell it.
            pub const fn name(self) -> &'static str {
                match self {
                    $(Token::$variant => stringify!($name),)*
                }
            }

            /// The token named `name`.
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $(stringify!($name) => Some(Token::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

const fn rgb(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

palette! {
    /// Behind the histogram plot and a readout card; the darkest surface, held near-neutral.
    Surround surround = rgb(0x19191b),
    /// Side panels, the status bar and the Settings sheet.
    Background background = rgb(0x202023),
    /// The Bar surface: the title bar, floating strips, notices and module bands.
    Surface surface = rgb(0x232326),
    /// Buttons, chips and text fields.
    Control control = rgb(0x2c2c31),
    /// Primary text.
    Text text = rgb(0xe8e8ea),
    /// Secondary text.
    TextSecondary text_secondary = rgb(0xa8a8ae),
    /// Tertiary text and disabled controls.
    TextTertiary text_tertiary = rgb(0x77777f),
    /// The one accent: the current entry, the active mode, the edited dot, a dragged thumb, Apply
    /// and the render bar.
    Accent accent = rgb(0xe2b46a),
    /// Text and icons on the accent.
    AccentInk accent_ink = rgb(0x1a1408),
    /// Invalid values, unavailable reasons and failure outlines.
    Error error = rgb(0xe5534b),
    /// A slider or field label: a step under primary, so the value on the same line reads first.
    TextLabel text_label = rgb(0xc9c9ce),
    /// A step under tertiary: a finished job's duration, and an empty clipping triangle.
    TextFaint text_faint = rgb(0x55555c),
    /// A step over primary: a notice's title and the title bar's file name.
    TextBright text_bright = rgb(0xf0f0f2),
    /// The title bar's dimensions, format and colour space.
    TextIdentity text_identity = rgb(0x8a8a90),
    /// The current history row's label.
    TextCurrentRow text_current_row = rgb(0xf2f2f4),
    /// An unselected chip's label.
    ChipLabel chip_label = rgb(0xb0b0b6),
    /// A mode-strip tool's icon at rest.
    StripIcon strip_icon = rgb(0xb9b9bf),
    /// A fixed mode control's glyph: the bright text at 55% over its segment, stored opaque.
    ModeFixedInk mode_fixed_ink = rgb(0x99999c),
    /// Outlines: 6% white.
    Border border = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.06 },
    /// Behind a modal sheet (the command palette, Settings): 35% black.
    Scrim scrim = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.35 },
    /// A group header's hairline rule. Opaque rather than a white alpha like `border`: Iced blends
    /// in linear light, which renders a small white alpha far brighter than the references do.
    Rule rule = rgb(0x313134),
    /// The 1 px border above each module band, and the shell's dividers.
    BandBorder band_border = rgb(0x2f2f32),
    /// The floating chrome's outline: 8% white over the surface.
    ChromeBorder chrome_border = rgb(0x343437),
    /// The mode strip's and the title bar's rule, and a neutral notice's outline: 10% white over
    /// the surface.
    StripRule strip_rule = rgb(0x39393c),
    /// A notice that needs a decision: the accent at 35% over the surface.
    NoticeWarningBorder notice_warning_border = rgb(0x65553d),
    /// A notice that reports a failure: the error at 40% over the surface.
    NoticeErrorBorder notice_error_border = rgb(0x713634),
    /// The histogram plot's outline: 5% white over the surround.
    HistogramBorder histogram_border = rgb(0x242426),
    /// A coverage thumbnail's outline: 8% white over its black ground.
    ThumbnailBorder thumbnail_border = rgb(0x212123),
    /// The empty slider rail.
    Rail rail = rgb(0x3a3a40),
    /// The rail's fill between the zero tick (or the rail's start) and the handle.
    RailFill rail_fill = rgb(0xa3a3aa),
    /// What a declared colour rail is laid over at 85%: the panel, held to the surround's chroma
    /// bound so a tinted panel cannot tint a temperature or hue rail.
    RailBackdrop rail_backdrop = rgb(0x202023),
    /// The zero tick across the rail.
    ZeroTick zero_tick = rgb(0x5a5a62),
    /// The resting handle.
    Thumb thumb = rgb(0xececee),
    /// The dark ring around the handle that separates it from a light or colour rail.
    ThumbOutline thumb_outline = rgb(0x111113),
    /// The area under a sparkline's line: the rail fill at 16% over the panel, stored opaque.
    SparklineArea sparkline_area = rgb(0x353539),
    /// A tab row's and a segmented control's track.
    TabTrack tab_track = rgb(0x28282c),
    /// The selected tab or segment.
    TabSelected tab_selected = rgb(0x3b3b41),
    /// A row under the pointer: white at 4% over the panel.
    RowHover row_hover = rgb(0x29292c),
    /// The current row: white at 7% over the panel.
    ListRowCurrent list_row_current = rgb(0x2f2f32),
    /// An icon button under the pointer: 6% white over the surface.
    IconHover icon_hover = rgb(0x303033),
    /// A selected chip: the accent at about 16% over the panel.
    SelectedFill selected_fill = rgb(0x3e372e),
    /// A selected icon button: the accent at 14% over the surface.
    IconSelectedFill icon_selected_fill = rgb(0x3e372f),
    /// A selected mode-strip tool or an overlay that is on: the accent at 16% over the surface.
    StripSelected strip_selected = rgb(0x413a30),
    /// The open mask's row: the accent at 12% over the panel.
    MaskRowSelected mask_row_selected = rgb(0x37322c),
    /// A row the command palette has just revealed.
    RevealedRow revealed_row = rgb(0x5b4932),
    /// A menu's surface.
    MenuSurface menu_surface = rgb(0x2a2a2e),
    /// A menu's outline: white at 10% over the menu.
    MenuBorder menu_border = rgb(0x3f3f43),
    /// A menu item under the pointer: white at 6% over the menu.
    MenuItemHover menu_item_hover = rgb(0x37373b),
    /// A menu's separator: white at 8% over the menu.
    MenuSeparator menu_separator = rgb(0x3b3b3f),
}

impl Default for Palette {
    fn default() -> Self {
        Self::luxforge_dark()
    }
}

/// The colour a widget model asks for: a token of the running theme's palette, resolved when the
/// widget draws, or a fixed colour, the same in every theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ink {
    Token(Token),
    Fixed(Color),
}

impl Ink {
    /// The colour this ink draws in `palette`.
    pub const fn resolve(self, palette: &Palette) -> Color {
        match self {
            Ink::Token(token) => palette.get(token),
            Ink::Fixed(colour) => colour,
        }
    }
}

impl From<Token> for Ink {
    fn from(token: Token) -> Self {
        Ink::Token(token)
    }
}

impl From<Color> for Ink {
    fn from(colour: Color) -> Self {
        Ink::Fixed(colour)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_is_named_once_in_the_design_order() {
        assert_eq!(Palette::TOKEN_NAMES[0], "surround");
        assert_eq!(Palette::TOKEN_NAMES[48], "menu_separator");
        for (index, token) in Token::ALL.into_iter().enumerate() {
            assert_eq!(token.name(), Palette::TOKEN_NAMES[index]);
            assert_eq!(Token::from_name(token.name()), Some(token));
        }
        let mut names = Palette::TOKEN_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 49, "no name twice");
        assert_eq!(Token::from_name("canvas"), None);
    }

    /// Each token reads and writes its own field: a palette whose every token is set by name to a
    /// distinct colour reads each back by name and by token.
    #[test]
    fn a_token_reads_and_writes_its_own_field() {
        let mut palette = Palette::luxforge_dark();
        for (index, name) in Palette::TOKEN_NAMES.into_iter().enumerate() {
            *palette.token_mut(name).unwrap() = Color::from_rgb8(index as u8, 1, 2);
        }
        for (index, token) in Token::ALL.into_iter().enumerate() {
            let expected = Color::from_rgb8(index as u8, 1, 2);
            assert_eq!(palette.get(token), expected);
            assert_eq!(palette.token(token.name()), Some(expected));
        }
        assert!(palette.token_mut("canvas").is_none());
    }

    /// Luxforge Dark is the design's Tokens table, alpha included.
    #[test]
    fn luxforge_dark_is_the_tokens_table() {
        let dark = Palette::luxforge_dark();
        let hex = |colour: Color| {
            let [r, g, b, a] = [colour.r, colour.g, colour.b, colour.a]
                .map(|channel| (channel * 255.0).round() as u8);
            if a == 255 {
                format!("#{r:02x}{g:02x}{b:02x}")
            } else {
                format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
            }
        };
        let table = [
            ("surround", "#19191b"),
            ("background", "#202023"),
            ("surface", "#232326"),
            ("control", "#2c2c31"),
            ("text", "#e8e8ea"),
            ("text_secondary", "#a8a8ae"),
            ("text_tertiary", "#77777f"),
            ("accent", "#e2b46a"),
            ("accent_ink", "#1a1408"),
            ("error", "#e5534b"),
            ("text_label", "#c9c9ce"),
            ("text_faint", "#55555c"),
            ("text_bright", "#f0f0f2"),
            ("text_identity", "#8a8a90"),
            ("text_current_row", "#f2f2f4"),
            ("chip_label", "#b0b0b6"),
            ("strip_icon", "#b9b9bf"),
            ("mode_fixed_ink", "#99999c"),
            ("border", "#ffffff0f"),
            ("scrim", "#00000059"),
            ("rule", "#313134"),
            ("band_border", "#2f2f32"),
            ("chrome_border", "#343437"),
            ("strip_rule", "#39393c"),
            ("notice_warning_border", "#65553d"),
            ("notice_error_border", "#713634"),
            ("histogram_border", "#242426"),
            ("thumbnail_border", "#212123"),
            ("rail", "#3a3a40"),
            ("rail_fill", "#a3a3aa"),
            ("rail_backdrop", "#202023"),
            ("zero_tick", "#5a5a62"),
            ("thumb", "#ececee"),
            ("thumb_outline", "#111113"),
            ("sparkline_area", "#353539"),
            ("tab_track", "#28282c"),
            ("tab_selected", "#3b3b41"),
            ("row_hover", "#29292c"),
            ("list_row_current", "#2f2f32"),
            ("icon_hover", "#303033"),
            ("selected_fill", "#3e372e"),
            ("icon_selected_fill", "#3e372f"),
            ("strip_selected", "#413a30"),
            ("mask_row_selected", "#37322c"),
            ("revealed_row", "#5b4932"),
            ("menu_surface", "#2a2a2e"),
            ("menu_border", "#3f3f43"),
            ("menu_item_hover", "#37373b"),
            ("menu_separator", "#3b3b3f"),
        ];
        assert_eq!(table.map(|(name, _)| name), Palette::TOKEN_NAMES);
        for (name, value) in table {
            assert_eq!(hex(dark.token(name).unwrap()), value, "{name}");
        }
        // The two alphas are exactly the former constants' 0.06 and 0.35, not their 8-bit codes.
        assert_eq!(
            dark.border,
            Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.06
            }
        );
        assert_eq!(
            dark.scrim,
            Color {
                a: 0.35,
                ..Color::BLACK
            }
        );
    }
}
