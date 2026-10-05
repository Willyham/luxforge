//! The theme model: the colours of the interface, chosen by the person and kept outside every
//! catalog (`docs/design/ui-themes.md`). A theme declares roles; every other token derives from them
//! by one fixed rule each unless the theme sets it explicitly; and [`resolve`] turns roles and
//! explicit tokens into a [`Palette`] of all [`TOKEN_COUNT`] tokens with a [`ThemeReport`] of what
//! it derived and adjusted.
//!
//! Resolving holds every theme to two rules. The surround, and the rail backdrop under a declared
//! rail, carry at most [`CHROMA_BOUND`] OKLCh chroma at their own lightness, so the photograph's
//! surround stays neutral. Each ink meets its [`Token::floor`] in WCAG 2 contrast against every
//! surface it is drawn on. A theme imported from elsewhere has its own ink moved in Oklab lightness
//! to its floor and the move reported; a Luxforge theme document's author chose each value, so a
//! document that misses a floor is refused by name.
//!
//! A theme never changes a photograph: nothing here reads an asset, a recipe or a catalog.
mod document;
pub mod omarchy;
mod report;
mod resolve;
mod rgba;
mod token;

#[cfg(test)]
mod tests;

pub use document::{THEME_DOCUMENT_FORMAT, THEME_DOCUMENT_VERSION, ThemeDocument};
pub use report::{AccentNote, InkMove, Neutralised, ReservedColour, ShortenedTier, ThemeReport};
pub use resolve::{
    ACCENT_INK_WEIGHT, ACCENT_NOTE_DISTANCE, CHROMA_BOUND, CONTROL_STEP, DARK_ACCENT_LUMINANCE,
    LIGHT_MODE_LIGHTNESS, SURFACE_STEP, SURROUND_STEP, resolve,
};
pub use rgba::Rgba;
pub use token::{TOKEN_COUNT, Token};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeMap};
use std::collections::BTreeMap;

/// The name of the built-in default theme.
pub const LUXFORGE_DARK_NAME: &str = "Luxforge Dark";

/// Whether a theme is dark or light: the native window's appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Dark,
    Light,
}

/// The tokens a theme sets explicitly past its roles. Each wins over its derivation.
pub type Tokens = BTreeMap<Token, Rgba>;

/// The roles a theme declares. `background`, `text` and `accent` are required; every other colour
/// role derives from them when absent, and `mode` follows the background's Oklab lightness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roles {
    pub background: Rgba,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surround: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Rgba>,
    pub text: Rgba,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_secondary: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_tertiary: Option<Rgba>,
    pub accent: Rgba,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_ink: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
}

impl Roles {
    /// The three required roles, with every other one left to derive.
    pub fn new(background: Rgba, text: Rgba, accent: Rgba) -> Self {
        Self {
            background,
            surround: None,
            surface: None,
            control: None,
            text,
            text_secondary: None,
            text_tertiary: None,
            accent,
            accent_ink: None,
            error: None,
            mode: None,
        }
    }

    /// The colour role `token` as the theme gave it; `None` for a role it left out or a token
    /// that is not a role.
    pub fn get(&self, token: Token) -> Option<Rgba> {
        match token {
            Token::Surround => self.surround,
            Token::Background => Some(self.background),
            Token::Surface => self.surface,
            Token::Control => self.control,
            Token::Text => Some(self.text),
            Token::TextSecondary => self.text_secondary,
            Token::TextTertiary => self.text_tertiary,
            Token::Accent => Some(self.accent),
            Token::AccentInk => self.accent_ink,
            Token::Error => self.error,
            _ => None,
        }
    }
}

/// Every token's resolved value, in table order. It serializes as an object from each token's
/// name to its colour, in that order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette([Rgba; TOKEN_COUNT]);

impl Palette {
    pub fn get(&self, token: Token) -> Rgba {
        self.0[token.index()]
    }

    /// Each token and its value, in table order.
    pub fn iter(&self) -> impl Iterator<Item = (Token, Rgba)> + '_ {
        Token::ALL.into_iter().zip(self.0.iter().copied())
    }

    /// Luxforge Dark's tokens: the visual language exactly.
    pub const fn luxforge_dark() -> Self {
        Self(token::LUXFORGE_DARK)
    }
}

impl std::ops::Index<Token> for Palette {
    type Output = Rgba;

    fn index(&self, token: Token) -> &Rgba {
        &self.0[token.index()]
    }
}

impl Serialize for Palette {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(TOKEN_COUNT))?;
        for (token, value) in self.iter() {
            map.serialize_entry(token.name(), &value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Palette {
    /// Every token, each once; a token missing is refused by name, as is a name that is not one.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = Tokens::deserialize(deserializer)?;
        let mut palette = [Rgba::BLACK; TOKEN_COUNT];
        for token in Token::ALL {
            palette[token.index()] = *values
                .get(&token)
                .ok_or_else(|| de::Error::custom(format!("missing token `{token}`")))?;
        }
        Ok(Self(palette))
    }
}

/// Which rules a theme is resolved by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// A theme imported from another format: its own ink that misses a floor, or a surround past
    /// the chroma bound, is adjusted and the adjustment reported.
    Import,
    /// A Luxforge theme document, or a built-in theme: its author chose each value, so a floor
    /// missed or a surround past the bound is refused by name.
    Document,
}

/// A theme's roles and explicit tokens resolved to every token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedTheme {
    pub mode: Mode,
    pub tokens: Palette,
    pub report: ThemeReport,
}

/// Luxforge Dark, the built-in default: every role and every token set explicitly to the visual
/// language's value.
pub fn luxforge_dark() -> ThemeDocument {
    let palette = Palette::luxforge_dark();
    let role = |token| Some(palette[token]);
    ThemeDocument {
        name: LUXFORGE_DARK_NAME.into(),
        roles: Roles {
            background: palette[Token::Background],
            surround: role(Token::Surround),
            surface: role(Token::Surface),
            control: role(Token::Control),
            text: palette[Token::Text],
            text_secondary: role(Token::TextSecondary),
            text_tertiary: role(Token::TextTertiary),
            accent: palette[Token::Accent],
            accent_ink: role(Token::AccentInk),
            error: role(Token::Error),
            mode: Some(Mode::Dark),
        },
        tokens: palette
            .iter()
            .filter(|(token, _)| !token.is_role())
            .collect(),
    }
}
