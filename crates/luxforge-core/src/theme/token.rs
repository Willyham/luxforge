//! The tokens: every colour the widgets draw that a theme decides, by the name the API and the
//! widget crate's palette give it, in the design's table order, with Luxforge Dark's value.
use super::Rgba;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// The token table, written once: each token's variant, its API name and Luxforge Dark's value.
macro_rules! tokens {
    ($($variant:ident = $name:literal, $value:literal;)*) => {
        /// One themed colour. The first ten are the colour roles; the order is the design's
        /// table, which [`Token::ALL`], a [`Palette`](super::Palette) and every listing keep.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Token {
            $($variant,)*
        }

        impl Token {
            /// Every token, in table order.
            pub const ALL: [Self; TOKEN_COUNT] = [$(Self::$variant,)*];

            /// The token's stable snake_case name, as the API and a theme document write it.
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }

        /// Luxforge Dark's value of every token, in table order: the visual language exactly.
        pub(super) const LUXFORGE_DARK: [Rgba; TOKEN_COUNT] = [$(Rgba::hex($value),)*];
    };
}

/// How many tokens there are.
pub const TOKEN_COUNT: usize = 49;

tokens! {
    Surround = "surround", "#19191b";
    Background = "background", "#202023";
    Surface = "surface", "#232326";
    Control = "control", "#2c2c31";
    Text = "text", "#e8e8ea";
    TextSecondary = "text_secondary", "#a8a8ae";
    TextTertiary = "text_tertiary", "#77777f";
    Accent = "accent", "#e2b46a";
    AccentInk = "accent_ink", "#1a1408";
    Error = "error", "#e5534b";
    TextLabel = "text_label", "#c9c9ce";
    TextFaint = "text_faint", "#55555c";
    TextBright = "text_bright", "#f0f0f2";
    TextIdentity = "text_identity", "#8a8a90";
    TextCurrentRow = "text_current_row", "#f2f2f4";
    ChipLabel = "chip_label", "#b0b0b6";
    StripIcon = "strip_icon", "#b9b9bf";
    ModeFixedInk = "mode_fixed_ink", "#99999c";
    Border = "border", "#ffffff0f";
    Scrim = "scrim", "#00000059";
    Rule = "rule", "#313134";
    BandBorder = "band_border", "#2f2f32";
    ChromeBorder = "chrome_border", "#343437";
    StripRule = "strip_rule", "#39393c";
    NoticeWarningBorder = "notice_warning_border", "#65553d";
    NoticeErrorBorder = "notice_error_border", "#713634";
    HistogramBorder = "histogram_border", "#242426";
    ThumbnailBorder = "thumbnail_border", "#212123";
    Rail = "rail", "#3a3a40";
    RailFill = "rail_fill", "#a3a3aa";
    RailBackdrop = "rail_backdrop", "#202023";
    ZeroTick = "zero_tick", "#5a5a62";
    Thumb = "thumb", "#ececee";
    ThumbOutline = "thumb_outline", "#111113";
    SparklineArea = "sparkline_area", "#353539";
    TabTrack = "tab_track", "#28282c";
    TabSelected = "tab_selected", "#3b3b41";
    RowHover = "row_hover", "#29292c";
    ListRowCurrent = "list_row_current", "#2f2f32";
    IconHover = "icon_hover", "#303033";
    SelectedFill = "selected_fill", "#3e372e";
    IconSelectedFill = "icon_selected_fill", "#3e372f";
    StripSelected = "strip_selected", "#413a30";
    MaskRowSelected = "mask_row_selected", "#37322c";
    RevealedRow = "revealed_row", "#5b4932";
    MenuSurface = "menu_surface", "#2a2a2e";
    MenuBorder = "menu_border", "#3f3f43";
    MenuItemHover = "menu_item_hover", "#37373b";
    MenuSeparator = "menu_separator", "#3b3b3f";
}

impl Token {
    /// The colour roles, the first ten tokens: what a theme declares and the rest derive from.
    pub const ROLES: [Self; 10] = [
        Self::Surround,
        Self::Background,
        Self::Surface,
        Self::Control,
        Self::Text,
        Self::TextSecondary,
        Self::TextTertiary,
        Self::Accent,
        Self::AccentInk,
        Self::Error,
    ];

    /// The token named `name`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|token| token.name() == name)
    }

    /// Its place in table order.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Whether the token is a colour role.
    pub const fn is_role(self) -> bool {
        self.index() < Self::ROLES.len()
    }

    /// Whether the token is drawn with alpha, and so may carry it: only these two.
    pub const fn takes_alpha(self) -> bool {
        matches!(self, Self::Border | Self::Scrim)
    }

    /// The WCAG 2 contrast an ink must reach against every surface it is drawn on: the
    /// surround, background, surface and control, or for the accent's ink, the accent. The faint
    /// tier and every token that is not an ink have none.
    pub const fn floor(self) -> Option<f64> {
        match self {
            Self::Text => Some(7.0),
            Self::TextLabel | Self::TextSecondary | Self::AccentInk => Some(4.5),
            Self::TextTertiary | Self::Accent | Self::Error => Some(3.0),
            _ => None,
        }
    }
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl Serialize for Token {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Token {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name).ok_or_else(|| de::Error::custom(format!("unknown token `{name}`")))
    }
}
