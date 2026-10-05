//! The derivation: each token not given by the theme is made from the resolved roles by one rule,
//! a mix of two colours in encoded sRGB at a fixed weight or a role moved in Oklab lightness. The
//! weights and steps are fitted to Luxforge Dark (`docs/design/ui-themes.md`, "Derived tokens"),
//! so a derived theme keeps its hierarchy. Then the surround and rail backdrop are held neutral and
//! every ink to its floor.
use super::report::{AccentNote, InkMove, Neutralised, ReservedColour, ShortenedTier, ThemeReport};
use super::{Mode, Palette, Resolution, ResolvedTheme, Rgba, Roles, TOKEN_COUNT, Token, Tokens};
use crate::{Error, colour::cielab};

/// The background's Oklab lightness at and above which a theme that states no mode is light.
pub const LIGHT_MODE_LIGHTNESS: f64 = 0.5;

/// The derived surround's Oklab lightness step from the background: below it in both modes.
pub const SURROUND_STEP: f64 = -0.031;

/// The derived surface's Oklab lightness step from the background, toward the text.
pub const SURFACE_STEP: f64 = 0.013;

/// The derived control's Oklab lightness step from the background, toward the text: lighter in a
/// dark theme and darker in a light one.
pub const CONTROL_STEP: f64 = 0.050;

/// The most OKLCh chroma the surround and the rail backdrop may carry.
pub const CHROMA_BOUND: f64 = 0.010;

/// How far the derived accent ink is mixed from the accent toward black or white.
pub const ACCENT_INK_WEIGHT: f64 = 0.895;

/// The accent's WCAG luminance at or below which its ink is mixed toward white, not black.
pub const DARK_ACCENT_LUMINANCE: f64 = 0.179;

/// The CIEDE2000 distance from a reserved colour under which the report notes the accent.
pub const ACCENT_NOTE_DISTANCE: f64 = 15.0;

/// The clipping red, which the error ink is unless the theme gives its own.
const CLIPPING_RED: Rgba = ReservedColour::ClippingRed.value();

/// The Oklab lightness step in which an ink is moved toward its floor.
const LIGHTNESS_STEP: f64 = 0.001;

/// The weight step in which a derived tier stops short of its fitted weight.
const WEIGHT_STEP: f64 = 0.005;

/// What a mix is made from.
#[derive(Clone, Copy, Debug)]
pub(super) enum Source {
    Token(Token),
    Black,
    /// White when the text is lighter than the background, black when it is darker: the far end
    /// of the text's direction.
    Pole,
    /// The opposite end: black when the text is lighter than the background.
    AntiPole,
}

/// How a token past the roles is derived.
#[derive(Clone, Copy, Debug)]
pub(super) enum Rule {
    /// The first colour mixed toward the second by the weight, in encoded sRGB.
    Mix(Source, Source, f64),
    /// A text tier: the text mixed toward the control by the weight, or by less where that would
    /// miss the tier's floor.
    Tier(f64),
    /// A colour at an alpha.
    Alpha(Source, u8),
    /// The background held to the chroma bound.
    NeutralBackground,
}

use Rule::{Alpha, Mix, NeutralBackground, Tier};
use Source::{AntiPole, Black, Pole};

const fn of(token: Token) -> Source {
    Source::Token(token)
}

/// The fitted weights of the two derived text roles, toward the control.
pub(super) const SECONDARY_WEIGHT: f64 = 0.335;
pub(super) const TERTIARY_WEIGHT: f64 = 0.59;

/// Every token past the roles and its rule, in table order. The weights are fitted to Luxforge
/// Dark: the test `luxforge_dark_derives_from_its_roles_within_the_recorded_fit` holds each
/// derived value to its token within the recorded number of codes.
pub(super) const RULES: [(Token, Rule); TOKEN_COUNT - 10] = [
    (Token::TextLabel, Tier(0.16)),
    (
        Token::TextFaint,
        Mix(of(Token::Text), of(Token::Control), 0.775),
    ),
    (Token::TextBright, Mix(of(Token::Text), Pole, 0.36)),
    (
        Token::TextIdentity,
        Mix(of(Token::TextSecondary), of(Token::Control), 0.24),
    ),
    (Token::TextCurrentRow, Mix(of(Token::Text), Pole, 0.455)),
    (
        Token::ChipLabel,
        Mix(of(Token::TextSecondary), of(Token::Text), 0.125),
    ),
    (
        Token::StripIcon,
        Mix(of(Token::TextSecondary), of(Token::Text), 0.265),
    ),
    (
        Token::ModeFixedInk,
        Mix(of(Token::Text), of(Token::Control), 0.42),
    ),
    (Token::Border, Alpha(Pole, 0x0f)),
    (Token::Scrim, Alpha(Black, 0x59)),
    (
        Token::Rule,
        Mix(of(Token::Background), of(Token::Text), 0.085),
    ),
    (
        Token::BandBorder,
        Mix(of(Token::Background), of(Token::Text), 0.075),
    ),
    (
        Token::ChromeBorder,
        Mix(of(Token::Background), of(Token::Text), 0.10),
    ),
    (
        Token::StripRule,
        Mix(of(Token::Background), of(Token::Text), 0.125),
    ),
    (
        Token::NoticeWarningBorder,
        Mix(of(Token::Surface), of(Token::Accent), 0.345),
    ),
    (
        Token::NoticeErrorBorder,
        Mix(of(Token::Surface), of(Token::Error), 0.40),
    ),
    (
        Token::HistogramBorder,
        Mix(of(Token::Surround), of(Token::Text), 0.055),
    ),
    (
        Token::ThumbnailBorder,
        Mix(of(Token::Surround), of(Token::Text), 0.04),
    ),
    (
        Token::Rail,
        Mix(of(Token::Control), of(Token::TextTertiary), 0.19),
    ),
    (
        Token::RailFill,
        Mix(of(Token::TextSecondary), of(Token::TextTertiary), 0.095),
    ),
    (Token::RailBackdrop, NeutralBackground),
    (
        Token::ZeroTick,
        Mix(of(Token::Control), of(Token::TextTertiary), 0.61),
    ),
    (Token::Thumb, Mix(of(Token::Text), Pole, 0.18)),
    (
        Token::ThumbOutline,
        Mix(of(Token::Background), AntiPole, 0.46),
    ),
    (
        Token::SparklineArea,
        Mix(of(Token::Background), of(Token::TextSecondary), 0.155),
    ),
    (
        Token::TabTrack,
        Mix(of(Token::Background), of(Token::Control), 0.63),
    ),
    (
        Token::TabSelected,
        Mix(of(Token::Control), of(Token::TextTertiary), 0.20),
    ),
    (
        Token::RowHover,
        Mix(of(Token::Background), of(Token::Text), 0.045),
    ),
    (
        Token::ListRowCurrent,
        Mix(of(Token::Background), of(Token::Text), 0.075),
    ),
    (
        Token::IconHover,
        Mix(of(Token::Background), of(Token::Text), 0.08),
    ),
    (
        Token::SelectedFill,
        Mix(of(Token::Background), of(Token::Accent), 0.155),
    ),
    (
        Token::IconSelectedFill,
        Mix(of(Token::Background), of(Token::Accent), 0.15),
    ),
    (
        Token::StripSelected,
        Mix(of(Token::Background), of(Token::Accent), 0.17),
    ),
    (
        Token::MaskRowSelected,
        Mix(of(Token::Background), of(Token::Accent), 0.12),
    ),
    (
        Token::RevealedRow,
        Mix(of(Token::Surround), of(Token::Accent), 0.32),
    ),
    (
        Token::MenuSurface,
        Mix(of(Token::Background), of(Token::Control), 0.80),
    ),
    (
        Token::MenuBorder,
        Mix(of(Token::Background), of(Token::TextSecondary), 0.23),
    ),
    (
        Token::MenuItemHover,
        Mix(of(Token::Background), of(Token::TextSecondary), 0.17),
    ),
    (
        Token::MenuSeparator,
        Mix(of(Token::Background), of(Token::TextSecondary), 0.20),
    ),
];

/// A number rounded to `places` decimals, for a report that reads the same on every platform.
fn rounded(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

/// The surfaces every ink but the accent's is drawn on.
const SURFACES: [Token; 4] = [
    Token::Surround,
    Token::Background,
    Token::Surface,
    Token::Control,
];

/// The resolution in progress: the palette filled so far, and the report.
struct Resolver<'a> {
    roles: &'a Roles,
    tokens: &'a Tokens,
    how: Resolution,
    values: [Option<Rgba>; TOKEN_COUNT],
    pole: Rgba,
    moved: Vec<InkMove>,
    shortened: Vec<ShortenedTier>,
}

impl Resolver<'_> {
    /// The theme's own value of `token`: the role it gave or the token it set.
    fn explicit(&self, token: Token) -> Option<Rgba> {
        self.roles
            .get(token)
            .or_else(|| self.tokens.get(&token).copied())
    }

    /// A token already resolved. The order of resolution makes every rule's sources resolved
    /// before it.
    fn value(&self, token: Token) -> Rgba {
        self.values[token.index()].expect("a rule's sources resolve before it")
    }

    fn set(&mut self, token: Token, value: Rgba) {
        self.values[token.index()] = Some(value);
    }

    fn source(&self, source: Source) -> Rgba {
        match source {
            Source::Token(token) => self.value(token),
            Source::Black => Rgba::BLACK,
            Source::Pole => self.pole,
            Source::AntiPole => Rgba::rgb(255 - self.pole.r, 255 - self.pole.g, 255 - self.pole.b),
        }
    }

    /// What `ink` is drawn on.
    fn backdrops(&self, ink: Token) -> Vec<(Token, Rgba)> {
        let tokens: &[Token] = if ink == Token::AccentInk {
            &[Token::Accent]
        } else {
            &SURFACES
        };
        tokens
            .iter()
            .map(|token| (*token, self.value(*token)))
            .collect()
    }

    /// The lowest contrast of `value` against `backdrops`, and the backdrop it is against.
    fn weakest(value: Rgba, backdrops: &[(Token, Rgba)]) -> (f64, Token, Rgba) {
        backdrops
            .iter()
            .map(|(token, backdrop)| (value.contrast(*backdrop), *token, *backdrop))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .expect("an ink is drawn on something")
    }

    /// `value` as the ink `token`: unchanged where it meets its floor; otherwise moved in Oklab
    /// lightness, keeping its hue and chroma, just far enough, unless the theme gave it in a
    /// document, which is refused. An ink no lightness can bring to its floor is refused.
    fn ink(&mut self, token: Token, value: Rgba, given: bool) -> Result<Rgba, Error> {
        let floor = token.floor().expect("an ink has a floor");
        let backdrops = self.backdrops(token);
        let (contrast, against, backdrop) = Self::weakest(value, &backdrops);
        if contrast >= floor {
            return Ok(value);
        }
        if given && self.how == Resolution::Document {
            return Err(Error::validation(format!(
                "{token} {value} is {contrast:.2}:1 against the {against} {backdrop}; its floor \
                 is {floor}:1"
            )));
        }
        let [lightness, a, b] = value.oklab();
        // Away from the background first where both directions take the same step.
        let away = if lightness >= self.value(Token::Background).oklab()[0] {
            1.0
        } else {
            -1.0
        };
        let steps = (1.0 / LIGHTNESS_STEP).round() as u32;
        let after = (1..=steps)
            .flat_map(|step| {
                [away, -away].map(|sign| lightness + sign * f64::from(step) * LIGHTNESS_STEP)
            })
            .filter(|moved| (0.0..=1.0).contains(moved))
            .chain([1.0, 0.0])
            .map(|moved| Rgba::from_oklab([moved, a, b]))
            .find(|moved| Self::weakest(*moved, &backdrops).0 >= floor)
            .ok_or_else(|| {
                Error::validation(format!(
                    "{token} {value} cannot reach {floor}:1 against the {} at any lightness",
                    backdrops
                        .iter()
                        .map(|(token, colour)| format!("{token} {colour}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        self.moved.push(InkMove {
            ink: token,
            floor,
            before: value,
            after,
            contrast_before: rounded(contrast, 2),
            contrast_after: rounded(Self::weakest(after, &backdrops).0, 2),
            difference: rounded(cielab::ciede2000(value.cielab(), after.cielab()), 2),
            given,
        });
        Ok(after)
    }

    /// `value` held to the chroma bound at its own lightness and hue. A document's own value past
    /// the bound is refused.
    fn neutral(
        &self,
        token: Token,
        value: Rgba,
        given: bool,
    ) -> Result<(Rgba, Neutralised), Error> {
        let chroma = value.chroma();
        let report = |after| Neutralised {
            before: value,
            after,
            chroma: rounded(chroma, 4),
        };
        if chroma <= CHROMA_BOUND {
            return Ok((value, report(value)));
        }
        if given && self.how == Resolution::Document {
            return Err(Error::validation(format!(
                "{token} {value} carries {chroma:.4} OKLCh chroma; it is held to {CHROMA_BOUND}"
            )));
        }
        let [lightness, a, b] = value.oklab();
        // The bound itself first, then a little less where rounding to a code overshoots it.
        let after = (0..=20)
            .map(|step| (1.0 - f64::from(step) / 20.0) * CHROMA_BOUND / chroma)
            .map(|scale| Rgba::from_oklab([lightness, a * scale, b * scale]))
            .find(|candidate| candidate.chroma() <= CHROMA_BOUND)
            .unwrap_or_else(|| Rgba::from_oklab([lightness, 0.0, 0.0]));
        Ok((after, report(after)))
    }

    /// The derived tier `token`: the text toward the control by `weight`, or by the largest
    /// smaller multiple of the weight step that meets the tier's floor.
    fn tier(&mut self, token: Token, weight: f64) -> Rgba {
        let floor = token.floor().expect("a checked tier has a floor");
        let (text, control) = (self.value(Token::Text), self.value(Token::Control));
        let backdrops = self.backdrops(token);
        let meets = |weight| {
            let value = text.mix(control, weight);
            (Self::weakest(value, &backdrops).0 >= floor).then_some(value)
        };
        if let Some(value) = meets(weight) {
            return value;
        }
        // Every multiple of the step below the weight, the largest first.
        let below = (weight / WEIGHT_STEP).round() as u32;
        let (applied, value) = (0..below)
            .rev()
            .map(|step| f64::from(step) * WEIGHT_STEP)
            .find_map(|applied| meets(applied).map(|value| (applied, value)))
            // The text itself meets the highest floor, so the weight zero always does.
            .unwrap_or((0.0, text));
        self.shortened.push(ShortenedTier {
            tier: token,
            floor,
            fitted: weight,
            applied: rounded(applied, 3),
        });
        value
    }

    /// A text tier: the theme's own, held to its floor, or derived.
    fn text_tier(&mut self, token: Token, weight: f64) -> Result<Rgba, Error> {
        match self.explicit(token) {
            Some(value) => self.ink(token, value, true),
            None => Ok(self.tier(token, weight)),
        }
    }
}

/// The accent's distance to the nearest reserved colour.
fn accent_note(accent: Rgba) -> AccentNote {
    let lab = accent.cielab();
    let (nearest, distance) = ReservedColour::ALL
        .into_iter()
        .map(|reserved| (reserved, cielab::ciede2000(lab, reserved.value().cielab())))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("four reserved colours");
    AccentNote {
        nearest,
        reserved: nearest.value(),
        distance: rounded(distance, 2),
        close: distance < ACCENT_NOTE_DISTANCE,
    }
}

/// Refuse what no resolution can use: a role in `tokens`, or alpha on a token not drawn with it.
fn check_shape(roles: &Roles, tokens: &Tokens) -> Result<(), Error> {
    for role in Token::ROLES {
        if let Some(value) = roles.get(role)
            && !value.is_opaque()
        {
            return Err(Error::validation(format!(
                "role {role} {value} has alpha; a role is opaque"
            )));
        }
    }
    for (token, value) in tokens {
        if token.is_role() {
            return Err(Error::validation(format!(
                "{token} is a role; a theme gives it under roles, not tokens"
            )));
        }
        if !value.is_opaque() && !token.takes_alpha() {
            return Err(Error::validation(format!(
                "token {token} {value} has alpha; only border and scrim take it"
            )));
        }
    }
    Ok(())
}

/// Resolve a theme's roles and explicit tokens to every token, by `how`'s rules. Refused by name:
/// a role in `tokens` or alpha where a token takes none; an ink no lightness brings to its floor;
/// and for a [`Resolution::Document`], a given ink that misses its floor or a given surround or
/// rail backdrop past the chroma bound.
pub fn resolve(roles: &Roles, tokens: &Tokens, how: Resolution) -> Result<ResolvedTheme, Error> {
    check_shape(roles, tokens)?;
    let background = roles.background;
    let background_lightness = background.oklab()[0];
    let mode = roles
        .mode
        .unwrap_or(if background_lightness < LIGHT_MODE_LIGHTNESS {
            Mode::Dark
        } else {
            Mode::Light
        });
    let text_lightness = roles.text.oklab()[0];
    let toward_text = if text_lightness == background_lightness {
        if mode == Mode::Dark { 1.0 } else { -1.0 }
    } else {
        (text_lightness - background_lightness).signum()
    };
    let mut resolver = Resolver {
        roles,
        tokens,
        how,
        values: [None; TOKEN_COUNT],
        pole: if toward_text > 0.0 {
            Rgba::WHITE
        } else {
            Rgba::BLACK
        },
        moved: Vec::new(),
        shortened: Vec::new(),
    };

    // The surfaces.
    resolver.set(Token::Background, background);
    let surround_given = resolver.explicit(Token::Surround);
    let surround_before = surround_given
        .unwrap_or_else(|| background.at_lightness(background_lightness + SURROUND_STEP));
    let (surround, surround_report) =
        resolver.neutral(Token::Surround, surround_before, surround_given.is_some())?;
    resolver.set(Token::Surround, surround);
    for (token, step) in [
        (Token::Surface, SURFACE_STEP),
        (Token::Control, CONTROL_STEP),
    ] {
        let value = resolver
            .explicit(token)
            .unwrap_or_else(|| background.at_lightness(background_lightness + toward_text * step));
        resolver.set(token, value);
    }

    // The inks on them.
    let text = resolver.ink(Token::Text, roles.text, true)?;
    resolver.set(Token::Text, text);
    let accent = resolver.ink(Token::Accent, roles.accent, true)?;
    resolver.set(Token::Accent, accent);
    let error = match resolver.explicit(Token::Error) {
        Some(error) => resolver.ink(Token::Error, error, true)?,
        None => resolver.ink(Token::Error, CLIPPING_RED, false)?,
    };
    resolver.set(Token::Error, error);
    for (token, weight) in [
        (Token::TextSecondary, SECONDARY_WEIGHT),
        (Token::TextTertiary, TERTIARY_WEIGHT),
    ] {
        let value = resolver.text_tier(token, weight)?;
        resolver.set(token, value);
    }
    let accent_ink = match resolver.explicit(Token::AccentInk) {
        Some(ink) => resolver.ink(Token::AccentInk, ink, true)?,
        None => {
            let toward = if accent.luminance() > DARK_ACCENT_LUMINANCE {
                Rgba::BLACK
            } else {
                Rgba::WHITE
            };
            let mixed = accent.mix(toward, ACCENT_INK_WEIGHT);
            let floor = Token::AccentInk
                .floor()
                .expect("the accent ink has a floor");
            if mixed.contrast(accent) >= floor {
                mixed
            } else {
                toward
            }
        }
    };
    resolver.set(Token::AccentInk, accent_ink);

    // Every other token, in table order: each rule's sources are roles.
    let mut rail_backdrop = None;
    for (token, rule) in RULES {
        let given = resolver.explicit(token);
        let value = match (rule, given) {
            (NeutralBackground, given) => {
                let (value, report) =
                    resolver.neutral(token, given.unwrap_or(background), given.is_some())?;
                rail_backdrop = Some(report);
                value
            }
            (Tier(weight), _) => resolver.text_tier(token, weight)?,
            (_, Some(value)) => value,
            (Mix(from, to, weight), None) => resolver.source(from).mix(resolver.source(to), weight),
            (Alpha(source, alpha), None) => resolver.source(source).with_alpha(alpha),
        };
        resolver.set(token, value);
    }

    let names = |given: bool| {
        Token::ROLES
            .into_iter()
            .filter(|role| roles.get(*role).is_some() == given)
            .map(|role| role.name().to_owned())
            .chain((roles.mode.is_some() == given).then(|| "mode".to_owned()))
            .collect()
    };
    let mut moved = resolver.moved;
    moved.sort_by_key(|moved| moved.ink);
    let report = ThemeReport {
        given: names(true),
        derived: names(false),
        explicit: tokens.keys().copied().collect(),
        moved,
        shortened: resolver.shortened,
        surround: surround_report,
        rail_backdrop: rail_backdrop.expect("the rail backdrop has a rule"),
        accent: accent_note(accent),
        omarchy: None,
    };
    let values = resolver
        .values
        .map(|value| value.expect("every token resolves"));
    Ok(ResolvedTheme {
        mode,
        tokens: Palette(values),
        report,
    })
}
