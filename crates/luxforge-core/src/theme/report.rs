//! What resolving a theme did: which roles it gave and which were derived, each ink moved to its
//! floor, each derived tier that stopped short, the surround and rail backdrop before and after
//! neutralising, and how close the accent sits to a reserved colour. A theme read from an Omarchy
//! folder also says what the folder held and how its palette became roles ([`OmarchyReport`]).
use super::{Mode, Rgba, Token, omarchy};
use serde::{Deserialize, Serialize};

/// A resolved theme's report, returned with it by the API.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeReport {
    /// The roles the theme gave, `mode` included, in the role table's order.
    pub given: Vec<String>,
    /// The roles derived from them, in the same order.
    pub derived: Vec<String>,
    /// The tokens past the roles that the theme set explicitly, in table order.
    pub explicit: Vec<Token>,
    /// Each ink moved in Oklab lightness to meet its floor, in table order.
    pub moved: Vec<InkMove>,
    /// Each derived text tier that stopped short of its fitted weight to meet its floor.
    pub shortened: Vec<ShortenedTier>,
    /// The surround before and after it was held to the chroma bound.
    pub surround: Neutralised,
    /// The rail backdrop before and after it was held to the chroma bound.
    pub rail_backdrop: Neutralised,
    pub accent: AccentNote,
    /// What an Omarchy theme's files held and how they became roles; absent for every other
    /// theme. Resolving never sets it: the reader of the folder does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omarchy: Option<OmarchyReport>,
}

impl ThemeReport {
    /// Whether resolving moved one of the theme's own inks: the Appearance tab's Adjusted badge.
    pub fn adjusted(&self) -> bool {
        self.moved.iter().any(|moved| moved.given)
    }
}

/// One ink moved in Oklab lightness, keeping its hue and chroma, just far enough to meet its
/// floor against every surface it is drawn on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InkMove {
    pub ink: Token,
    /// The WCAG 2 contrast it must reach.
    pub floor: f64,
    pub before: Rgba,
    pub after: Rgba,
    /// The lowest contrast against its surfaces before and after the move, to two decimals.
    pub contrast_before: f64,
    pub contrast_after: f64,
    /// The move's size in CIEDE2000, to two decimals.
    pub difference: f64,
    /// Whether the theme gave the ink. The error ink a theme leaves out is the clipping red, which
    /// is moved too where the theme's surfaces need it.
    pub given: bool,
}

/// A derived text tier that would miss its floor at its fitted weight toward the control, so
/// takes less of the control.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortenedTier {
    pub tier: Token,
    pub floor: f64,
    /// The weight fitted to Luxforge Dark, and the one used.
    pub fitted: f64,
    pub applied: f64,
}

/// A colour held to the chroma bound at its own lightness and hue.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Neutralised {
    pub before: Rgba,
    pub after: Rgba,
    /// Its OKLCh chroma before, to four decimals.
    pub chroma: f64,
}

/// The colours whose meaning is their colour, which the accent is measured against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservedColour {
    /// The highlight-clipping red, `#e5534b`.
    ClippingRed,
    /// The shadow-clipping blue, `#4c8be0`.
    ClippingBlue,
    /// The mask overlay's green, `#3fd07a`.
    MaskGreen,
    /// The mask overlay's white, `#f2f2f5`.
    MaskWhite,
}

impl ReservedColour {
    pub const ALL: [Self; 4] = [
        Self::ClippingRed,
        Self::ClippingBlue,
        Self::MaskGreen,
        Self::MaskWhite,
    ];

    pub const fn value(self) -> Rgba {
        match self {
            Self::ClippingRed => Rgba::hex("#e5534b"),
            Self::ClippingBlue => Rgba::hex("#4c8be0"),
            Self::MaskGreen => Rgba::hex("#3fd07a"),
            Self::MaskWhite => Rgba::hex("#f2f2f5"),
        }
    }
}

/// The accent's distance to the nearest reserved colour. A theme keeps its own accent whatever the
/// distance; one under [`ACCENT_NOTE_DISTANCE`](super::ACCENT_NOTE_DISTANCE) is noted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccentNote {
    pub nearest: ReservedColour,
    pub reserved: Rgba,
    /// CIEDE2000, to two decimals.
    pub distance: f64,
    /// Whether the distance is under the note's threshold.
    pub close: bool,
}

/// How an Omarchy theme's palette became a Luxforge theme's roles
/// (`docs/design/ui-themes.md#what-luxforge-reads`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmarchyReport {
    /// Which of Omarchy's forms the palette was in.
    pub form: omarchy::Form,
    pub mode: Mode,
    /// How Omarchy's precedence decided the mode.
    pub mode_source: omarchy::ModeSource,
    /// Each role an Omarchy key gave, in the role table's order, with the value Omarchy's resolver
    /// gives the key: the role before any move to its floor or to the chroma bound.
    pub roles: Vec<OmarchyRole>,
    /// Each role an Omarchy key could give that was left to derive instead, and why.
    pub derived: Vec<OmarchyDerived>,
    /// Every key and value the theme's files hold that its palette does not use, in the files'
    /// order. The palette's other colours, its named ones among them, are never read.
    pub unused: Vec<OmarchyUnused>,
}

/// One role and the Omarchy key it came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmarchyRole {
    pub role: Token,
    pub key: String,
    pub value: Rgba,
}

/// A role an Omarchy key could give, derived instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmarchyDerived {
    pub role: Token,
    pub key: String,
    pub value: Rgba,
    pub reason: DerivedReason,
}

/// Why an Omarchy key's value was not taken as its role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivedReason {
    /// `lighter_background` equals the background, as it does for every Omarchy 3 palette, so it
    /// is no raised surface: the control derives from the background.
    EqualsBackground,
}

/// A key and value an Omarchy theme's file holds that its palette does not use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmarchyUnused {
    pub file: String,
    /// The key, dotted from the file's root.
    pub key: String,
    /// A string's text, or any other value as the file writes it.
    pub value: String,
    pub reason: omarchy::UnusedReason,
}
