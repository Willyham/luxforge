//! The theme model's contract: the token list, Luxforge Dark, the fit of the derivation, golden
//! dark and light themes, the chroma bound, the floors and their moves, and every refusal.
use super::resolve::{RULES, SECONDARY_WEIGHT, TERTIARY_WEIGHT};
use super::*;
use crate::ErrorKind;
use serde_json::json;

/// The design's Tokens table, in its order.
const TABLE: [&str; TOKEN_COUNT] = [
    "surround",
    "background",
    "surface",
    "control",
    "text",
    "text_secondary",
    "text_tertiary",
    "accent",
    "accent_ink",
    "error",
    "text_label",
    "text_faint",
    "text_bright",
    "text_identity",
    "text_current_row",
    "chip_label",
    "strip_icon",
    "mode_fixed_ink",
    "border",
    "scrim",
    "rule",
    "band_border",
    "chrome_border",
    "strip_rule",
    "notice_warning_border",
    "notice_error_border",
    "histogram_border",
    "thumbnail_border",
    "rail",
    "rail_fill",
    "rail_backdrop",
    "zero_tick",
    "thumb",
    "thumb_outline",
    "sparkline_area",
    "tab_track",
    "tab_selected",
    "row_hover",
    "list_row_current",
    "icon_hover",
    "selected_fill",
    "icon_selected_fill",
    "strip_selected",
    "mask_row_selected",
    "revealed_row",
    "menu_surface",
    "menu_border",
    "menu_item_hover",
    "menu_separator",
];

/// The surfaces the inks are drawn on.
const SURFACES: [Token; 4] = [
    Token::Surround,
    Token::Background,
    Token::Surface,
    Token::Control,
];

fn rgb(text: &str) -> Rgba {
    Rgba::parse(text).unwrap()
}

fn import(roles: &Roles) -> ResolvedTheme {
    resolve(roles, &Tokens::new(), Resolution::Import).unwrap()
}

/// The largest difference in any channel, alpha included, in codes.
fn codes(first: Rgba, second: Rgba) -> u8 {
    [
        first.r.abs_diff(second.r),
        first.g.abs_diff(second.g),
        first.b.abs_diff(second.b),
        first.a.abs_diff(second.a),
    ]
    .into_iter()
    .max()
    .unwrap()
}

fn hue(colour: Rgba) -> f64 {
    let [_, a, b] = colour.oklab();
    b.atan2(a).to_degrees()
}

/// Every ink meets its floor, and the surround and rail backdrop the chroma bound.
fn assert_legible(theme: &ResolvedTheme, what: &str) {
    let tokens = &theme.tokens;
    for token in Token::ALL {
        let Some(floor) = token.floor() else {
            continue;
        };
        let against: &[Token] = if token == Token::AccentInk {
            &[Token::Accent]
        } else {
            &SURFACES
        };
        for surface in against {
            let contrast = tokens[token].contrast(tokens[*surface]);
            assert!(
                contrast >= floor,
                "{what}: {token} {} is {contrast:.3}:1 on {surface} {}",
                tokens[token],
                tokens[*surface]
            );
        }
    }
    for token in [Token::Surround, Token::RailBackdrop] {
        let chroma = tokens[token].chroma();
        assert!(chroma <= CHROMA_BOUND, "{what}: {token} carries {chroma}");
    }
}

// The token list.

#[test]
fn the_tokens_are_the_design_table_in_its_order() {
    assert_eq!(Token::ALL.map(Token::name), TABLE);
    for (index, token) in Token::ALL.into_iter().enumerate() {
        assert_eq!(token.index(), index);
        assert_eq!(Token::parse(token.name()), Some(token));
        assert_eq!(token.is_role(), index < 10, "{token}");
        assert!(
            token
                .name()
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
            "{token}"
        );
        assert_eq!(serde_json::to_value(token).unwrap(), json!(token.name()));
    }
    assert_eq!(Token::ROLES, Token::ALL[..10]);
    let alpha: Vec<_> = Token::ALL.into_iter().filter(|t| t.takes_alpha()).collect();
    assert_eq!(alpha, [Token::Border, Token::Scrim]);
    // Each token past the roles has exactly one rule, in table order.
    let ruled: Vec<_> = RULES.iter().map(|(token, _)| *token).collect();
    assert_eq!(ruled, Token::ALL[10..]);
}

/// The colours whose meaning is their colour keep their constants and are never tokens.
#[test]
fn the_fixed_colours_are_not_tokens() {
    for fixed in [
        "canvas",
        "canvas_dark",
        "canvas_black",
        "canvas_grey",
        "clipping_highlight",
        "clipping_shadow",
        "clipping_both",
        "clipping_red",
        "clipping_blue",
        "mask_overlay_green",
        "mask_overlay_white",
        "channel_red",
        "channel_green",
        "channel_blue",
        "temperature_rail",
        "tint_rail",
        "guide",
        "render_bar_track",
        "agent_connected",
        "swatch_outline",
        "thumbnail_background",
        "mask_glyph_outline",
        "mask_glyph_photo",
        "chrome_shadow",
        "menu_shadow",
    ] {
        assert_eq!(Token::parse(fixed), None, "{fixed}");
        let document = json!({
            "format": THEME_DOCUMENT_FORMAT,
            "version": 1,
            "name": "Fixed",
            "roles": {"background": "#202023", "text": "#e8e8ea", "accent": "#e2b46a"},
            "tokens": {fixed: "#ff0000"},
        });
        let error = ThemeDocument::read(&document.to_string()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("unknown token `{fixed}`")),
            "{error}"
        );
    }
    // The reserved colours the accent is measured against are fixed too.
    for reserved in ReservedColour::ALL {
        let name = serde_json::to_value(reserved).unwrap();
        assert_eq!(Token::parse(name.as_str().unwrap()), None);
    }
}

#[test]
fn colours_parse_and_write_as_hex() {
    assert_eq!(rgb("#19191b"), Rgba::rgb(0x19, 0x19, 0x1b));
    assert_eq!(rgb("#FFFFFF0F"), Rgba::WHITE.with_alpha(0x0f));
    assert_eq!(rgb("#ffffff0f").to_string(), "#ffffff0f");
    assert_eq!(rgb("#202023ff").to_string(), "#202023");
    for bad in [
        "",
        "#",
        "202023",
        "#20202",
        "#2020233",
        "#20202g",
        "#2020230f0",
        " #202023",
    ] {
        let error = Rgba::parse(bad).unwrap_err();
        assert!(error.contains("#rrggbb or #rrggbbaa"), "{bad}: {error}");
    }
    let value: Rgba = serde_json::from_value(json!("#e2b46a")).unwrap();
    assert_eq!(serde_json::to_value(value).unwrap(), json!("#e2b46a"));
    assert!(serde_json::from_value::<Rgba>(json!("red")).is_err());
}

// Luxforge Dark.

#[test]
fn luxforge_dark_resolves_to_the_visual_language_exactly() {
    let document = luxforge_dark();
    assert_eq!(document.name, "Luxforge Dark");
    assert_eq!(document.tokens.len(), TOKEN_COUNT - 10);
    let resolved = document.resolve(Resolution::Document).unwrap();
    assert_eq!(resolved.mode, Mode::Dark);
    assert_eq!(resolved.tokens, Palette::luxforge_dark());
    for (token, value) in [
        (Token::Surround, "#19191b"),
        (Token::Background, "#202023"),
        (Token::Text, "#e8e8ea"),
        (Token::Accent, "#e2b46a"),
        (Token::AccentInk, "#1a1408"),
        (Token::Border, "#ffffff0f"),
        (Token::Scrim, "#00000059"),
        (Token::RailBackdrop, "#202023"),
        (Token::MenuSeparator, "#3b3b3f"),
    ] {
        assert_eq!(resolved.tokens[token].to_string(), value, "{token}");
    }
    assert_legible(&resolved, "Luxforge Dark");
    let report = &resolved.report;
    assert_eq!(report.given.len(), 11);
    assert!(report.derived.is_empty());
    assert!(report.moved.is_empty() && report.shortened.is_empty());
    assert!(!report.adjusted());
    assert_eq!(report.surround.after, report.surround.before);
    assert_eq!(report.surround.chroma, 0.0039);
    assert_eq!(report.accent.nearest, ReservedColour::MaskWhite);
    assert_eq!(report.accent.distance, 27.14);
    assert!(!report.accent.close);
    // An import of the same colours is the same theme.
    let imported = document.resolve(Resolution::Import).unwrap();
    assert_eq!(imported.tokens, resolved.tokens);
}

/// The fit of the derivation to Luxforge Dark, the largest channel difference in codes between
/// each derived value and the visual language's: deriving the 39 tokens past the roles from all
/// ten of its roles, and deriving everything from its three required roles alone. Recorded in
/// `docs/design/ui-themes.md`, "Derived tokens".
#[test]
fn luxforge_dark_derives_from_its_roles_within_the_recorded_fit() {
    #[rustfmt::skip]
    const FIT: [(Token, u8, u8); TOKEN_COUNT] = [
        (Token::Surround, 0, 1),
        (Token::Background, 0, 0),
        (Token::Surface, 0, 0),
        (Token::Control, 0, 2),
        (Token::Text, 0, 0),
        (Token::TextSecondary, 0, 3),
        (Token::TextTertiary, 0, 3),
        (Token::Accent, 0, 0),
        (Token::AccentInk, 0, 3),
        (Token::Error, 0, 0),
        (Token::TextLabel, 2, 2),
        (Token::TextFaint, 1, 3),
        (Token::TextBright, 0, 0),
        (Token::TextIdentity, 0, 3),
        (Token::TextCurrentRow, 0, 0),
        (Token::ChipLabel, 0, 3),
        (Token::StripIcon, 1, 3),
        (Token::ModeFixedInk, 0, 1),
        (Token::Border, 0, 0),
        (Token::Scrim, 0, 0),
        (Token::Rule, 0, 0),
        (Token::BandBorder, 0, 0),
        (Token::ChromeBorder, 0, 0),
        (Token::StripRule, 0, 0),
        (Token::NoticeWarningBorder, 0, 0),
        (Token::NoticeErrorBorder, 1, 1),
        (Token::HistogramBorder, 0, 1),
        (Token::ThumbnailBorder, 0, 1),
        (Token::Rail, 0, 2),
        (Token::RailFill, 0, 3),
        (Token::RailBackdrop, 0, 0),
        (Token::ZeroTick, 1, 4),
        (Token::Thumb, 0, 0),
        (Token::ThumbOutline, 0, 0),
        (Token::SparklineArea, 0, 1),
        (Token::TabTrack, 0, 1),
        (Token::TabSelected, 0, 3),
        (Token::RowHover, 0, 0),
        (Token::ListRowCurrent, 0, 0),
        (Token::IconHover, 0, 0),
        (Token::SelectedFill, 0, 0),
        (Token::IconSelectedFill, 1, 1),
        (Token::StripSelected, 1, 1),
        (Token::MaskRowSelected, 0, 0),
        (Token::RevealedRow, 2, 3),
        (Token::MenuSurface, 0, 1),
        (Token::MenuBorder, 0, 1),
        (Token::MenuItemHover, 0, 1),
        (Token::MenuSeparator, 0, 1),
    ];
    let dark = Palette::luxforge_dark();
    let from_roles = import(&luxforge_dark().roles);
    let from_three = import(&Roles::new(
        dark[Token::Background],
        dark[Token::Text],
        dark[Token::Accent],
    ));
    for (index, (token, roles_fit, three_fit)) in FIT.into_iter().enumerate() {
        assert_eq!(token, Token::ALL[index]);
        assert_eq!(
            (
                codes(from_roles.tokens[token], dark[token]),
                codes(from_three.tokens[token], dark[token]),
            ),
            (roles_fit, three_fit),
            "{token}: {} and {} against {}",
            from_roles.tokens[token],
            from_three.tokens[token],
            dark[token]
        );
    }
    // From its roles, 31 of the 39 derived tokens are exact and none is more than two codes off;
    // from three roles, nothing is more than four.
    let exact = FIT[10..].iter().filter(|(_, fit, _)| *fit == 0).count();
    assert_eq!(exact, 31);
    assert_eq!(FIT.iter().map(|(_, fit, _)| *fit).max(), Some(2));
    assert_eq!(FIT.iter().map(|(_, _, fit)| *fit).max(), Some(4));
    // Neither derivation adjusts anything: Luxforge Dark's hierarchy meets every floor.
    for derived in [&from_roles, &from_three] {
        assert!(derived.report.moved.is_empty() && derived.report.shortened.is_empty());
        assert_legible(derived, "Luxforge Dark derived");
    }
    assert_eq!(from_three.report.given, ["background", "text", "accent"]);
}

// Golden themes.

fn assert_golden(theme: &ResolvedTheme, golden: [&str; TOKEN_COUNT]) {
    for ((token, value), expected) in theme.tokens.iter().zip(golden) {
        assert_eq!(value.to_string(), expected, "{token}");
    }
}

/// A dark theme of three blue-tinted roles: the surround and rail backdrop are neutralised, the
/// tertiary tier stops short, and the accent sits near the shadow-clipping blue.
#[test]
fn a_dark_theme_derives_its_golden_tokens() {
    let roles = Roles::new(rgb("#1e2030"), rgb("#c0caf5"), rgb("#7aa2f7"));
    let theme = import(&roles);
    #[rustfmt::skip]
    assert_golden(&theme, [
        "#191a1f", "#1e2030", "#212333", "#2a2c3d", "#c0caf5", "#8e95b7", "#6f7592", "#7aa2f7",
        "#0d111a", "#e5534b", "#a8b1d8", "#4c5066", "#d7ddf9", "#767c9a", "#dde2fa", "#949cbf",
        "#9ba3c7", "#8188a8", "#ffffff0f", "#00000059", "#2c2e41", "#2a2d3f", "#2e3144",
        "#323549", "#404f77", "#6f363d", "#22242b", "#202128", "#373a4d", "#8b92b3", "#202126",
        "#545971", "#cbd4f7", "#10111a", "#2f3245", "#262838", "#383b4e", "#252839", "#2a2d3f",
        "#2b2e40", "#2c344f", "#2c344e", "#2e3652", "#293048", "#384664", "#282a3a", "#383b4f",
        "#313447", "#34374b",
    ]);
    assert_eq!(theme.mode, Mode::Dark);
    assert_legible(&theme, "dark");
    let report = &theme.report;
    assert_eq!(report.given, ["background", "text", "accent"]);
    assert_eq!(
        report.derived,
        [
            "surround",
            "surface",
            "control",
            "text_secondary",
            "text_tertiary",
            "accent_ink",
            "error",
            "mode"
        ]
    );
    assert!(report.moved.is_empty());
    assert_eq!(
        report.shortened,
        [ShortenedTier {
            tier: Token::TextTertiary,
            floor: 3.0,
            fitted: TERTIARY_WEIGHT,
            applied: 0.54,
        }]
    );
    assert_eq!(
        report.surround,
        Neutralised {
            before: rgb("#171928"),
            after: rgb("#191a1f"),
            chroma: 0.0295,
        }
    );
    assert_eq!(report.rail_backdrop.after, rgb("#202126"));
    assert_eq!(
        report.accent,
        AccentNote {
            nearest: ReservedColour::ClippingBlue,
            reserved: rgb("#4c8be0"),
            distance: 8.74,
            close: true,
        }
    );
    // It meets every floor as given, so a document of the same roles resolves the same.
    let document = resolve(&roles, &Tokens::new(), Resolution::Document).unwrap();
    assert_eq!(document, theme);
}

/// A light theme whose own text and accent miss their floors on a warm paper background: both
/// are moved, the clipping red is moved for the error ink, two tiers stop short, and the surfaces
/// and the poles step the other way.
#[test]
fn a_light_theme_derives_its_golden_tokens() {
    let roles = Roles::new(rgb("#f4efe6"), rgb("#6b6b6b"), rgb("#e0a030"));
    let theme = import(&roles);
    #[rustfmt::skip]
    assert_golden(&theme, [
        "#e9e5df", "#f4efe6", "#f0ebe2", "#e3ded6", "#464646", "#636261", "#807e7b", "#aa7400",
        "#120c00", "#de4c45", "#5f5e5d", "#c0bcb6", "#2d2d2d", "#82807d", "#262626", "#5f5f5e",
        "#5b5b5a", "#888682", "#0000000f", "#00000059", "#e5e1d8", "#e7e2da", "#e3ded6",
        "#dedad2", "#d8c294", "#e9aba3", "#e0dcd7", "#e2dfd9", "#d0ccc5", "#666563", "#f3efe9",
        "#a7a39e", "#393939", "#f9f6f2", "#ded9d1", "#e9e4dc", "#cfcbc4", "#ece7df", "#e7e2da",
        "#e6e1d9", "#e9dcc2", "#e9ddc4", "#e7dabf", "#ebe0ca", "#d5c198", "#e6e1d9", "#d3cfc7",
        "#dbd7cf", "#d7d3cb",
    ]);
    assert_eq!(theme.mode, Mode::Light);
    assert_legible(&theme, "light");
    let tokens = &theme.tokens;
    // The surround sits below the background; the surface and control step toward the text.
    let lightness = |token| tokens[token].oklab()[0];
    assert!(lightness(Token::Surround) < lightness(Token::Background));
    assert!(lightness(Token::Surface) < lightness(Token::Background));
    assert!(lightness(Token::Control) < lightness(Token::Surface));
    let report = &theme.report;
    let moved: Vec<_> = report
        .moved
        .iter()
        .map(|moved| {
            (
                moved.ink,
                moved.before.to_string(),
                moved.after.to_string(),
                moved.given,
            )
        })
        .collect();
    assert_eq!(
        moved,
        [
            (Token::Text, "#6b6b6b".into(), "#464646".into(), true),
            (Token::Accent, "#e0a030".into(), "#aa7400".into(), true),
            (Token::Error, "#e5534b".into(), "#de4c45".into(), false),
        ]
    );
    assert!(report.adjusted());
    assert_eq!(
        report.shortened,
        [
            ShortenedTier {
                tier: Token::TextSecondary,
                floor: 4.5,
                fitted: SECONDARY_WEIGHT,
                applied: 0.185,
            },
            ShortenedTier {
                tier: Token::TextTertiary,
                floor: 3.0,
                fitted: TERTIARY_WEIGHT,
                applied: 0.37,
            },
        ]
    );
    assert_eq!(report.accent.nearest, ReservedColour::ClippingRed);
    assert!(!report.accent.close);
    // A document of the same roles names the first ink that misses.
    let error = resolve(&roles, &Tokens::new(), Resolution::Document).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(
        error
            .to_string()
            .contains("text #6b6b6b is 3.98:1 against the control #e3ded6"),
        "{error}"
    );
}

// The chroma bound.

#[test]
fn no_surround_or_rail_backdrop_passes_the_chroma_bound() {
    let accent = rgb("#e2b46a");
    for (background, text) in [
        ("#002b36", "#eee8d5"),
        ("#3b0a45", "#f0e0f5"),
        ("#031222", "#d0e0f0"),
        ("#0b3d0b", "#e0ffe0"),
        ("#fdf6e3", "#2b2b2b"),
        ("#e6f0ff", "#1a1a2e"),
        ("#ffffff", "#000000"),
        ("#000000", "#ffffff"),
    ] {
        let theme = import(&Roles::new(rgb(background), rgb(text), accent));
        assert_legible(&theme, background);
        for (token, report) in [
            (Token::Surround, &theme.report.surround),
            (Token::RailBackdrop, &theme.report.rail_backdrop),
        ] {
            assert_eq!(report.after, theme.tokens[token]);
            let (before, after) = (report.before.oklab(), report.after.oklab());
            // At its own lightness, within what rounding to a code moves.
            assert!((before[0] - after[0]).abs() < 0.005, "{background} {token}");
            if report.before.chroma() > CHROMA_BOUND {
                assert!(
                    report.after.chroma() > 0.0,
                    "{background} {token} keeps its hue"
                );
                let turn = (hue(report.before) - hue(report.after)).abs();
                assert!(
                    turn.min(360.0 - turn) < 15.0,
                    "{background} {token} turns {turn}°"
                );
            }
        }
    }
    // A surround the theme gives is held too, and the report says from what.
    let mut roles = Roles::new(rgb("#1a1b26"), rgb("#c0caf5"), accent);
    roles.surround = Some(rgb("#031222"));
    let theme = import(&roles);
    assert_eq!(theme.report.surround.before, rgb("#031222"));
    assert_eq!(theme.report.surround.chroma, 0.0402);
    assert_eq!(theme.tokens[Token::Surround], rgb("#0e1215"));
    assert!(theme.tokens[Token::Surround].chroma() <= CHROMA_BOUND);
}

// The floors and their moves.

#[test]
fn an_imported_ink_is_moved_in_lightness_just_far_enough() {
    // Each own ink misses: text and accent too dark on a dark ground, labels set explicitly.
    let mut roles = Roles::new(rgb("#202023"), rgb("#8090a0"), rgb("#6a4a20"));
    roles.text_secondary = Some(rgb("#505060"));
    let theme = import(&roles);
    assert_legible(&theme, "moved");
    let moved = &theme.report.moved;
    assert_eq!(
        moved.iter().map(|moved| moved.ink).collect::<Vec<_>>(),
        [Token::Text, Token::TextSecondary, Token::Accent]
    );
    for moved in moved {
        assert!(moved.given);
        assert!(moved.contrast_before < moved.floor, "{moved:?}");
        assert!(moved.contrast_after >= moved.floor, "{moved:?}");
        // Just far enough: within one Oklab lightness step of the floor, give or take a code.
        assert!(moved.contrast_after < moved.floor * 1.05, "{moved:?}");
        assert!(
            moved.after.oklab()[0] > moved.before.oklab()[0],
            "{moved:?}"
        );
        // Hue kept, and chroma too where the gamut allows.
        let turn = (hue(moved.before) - hue(moved.after)).abs();
        assert!(turn.min(360.0 - turn) < 3.0, "{moved:?}");
        assert!(
            (moved.before.chroma() - moved.after.chroma()).abs() < 0.01,
            "{moved:?}"
        );
        assert!(moved.difference > 0.0);
        assert_eq!(theme.tokens[moved.ink], moved.after);
    }
    assert!(theme.report.adjusted());
}

#[test]
fn an_ink_no_lightness_can_rescue_is_refused_by_name() {
    // A mid-grey ground: no colour reaches 7:1 against it.
    let roles = Roles::new(rgb("#767676"), rgb("#ffffff"), rgb("#000000"));
    let error = resolve(&roles, &Tokens::new(), Resolution::Import).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(
        error.detail.starts_with("text #ffffff cannot reach 7:1"),
        "{error}"
    );
}

#[test]
fn a_derived_tier_stops_short_where_it_would_miss_its_floor() {
    // Text barely past 7:1, so the fitted weights would take the tiers under theirs.
    let theme = import(&Roles::new(rgb("#2a2a2a"), rgb("#cccccc"), rgb("#e2b46a")));
    assert_legible(&theme, "low contrast");
    let shortened = &theme.report.shortened;
    let tiers: Vec<_> = shortened.iter().map(|tier| tier.tier).collect();
    assert_eq!(tiers, [Token::TextSecondary, Token::TextTertiary]);
    for tier in shortened {
        assert!(tier.applied < tier.fitted, "{tier:?}");
        // One weight step more would miss the floor.
        let (text, control) = (theme.tokens[Token::Text], theme.tokens[Token::Control]);
        let further = text.mix(control, tier.applied + 0.005);
        let weakest = SURFACES
            .iter()
            .map(|surface| further.contrast(theme.tokens[*surface]))
            .fold(f64::INFINITY, f64::min);
        assert!(weakest < tier.floor, "{tier:?}");
    }
    // Nothing of the theme's own was moved.
    assert!(theme.report.moved.is_empty());
    assert!(!theme.report.adjusted());
}

#[test]
fn the_accent_ink_mixes_toward_black_or_white_and_falls_back_to_either() {
    let ink = |background: &str, accent: &str| {
        import(&Roles::new(rgb(background), rgb("#f0f0f0"), rgb(accent))).tokens[Token::AccentInk]
    };
    // A light accent mixes toward black, by 89.5%.
    assert_eq!(
        ink("#101010", "#e2b46a"),
        rgb("#e2b46a").mix(Rgba::BLACK, 0.895)
    );
    // A dark accent, at or under 0.179 WCAG luminance, mixes toward white.
    let blue = rgb("#2050c0");
    assert!(blue.luminance() <= DARK_ACCENT_LUMINANCE);
    let light = import(&Roles::new(rgb("#f8f8f8"), rgb("#101010"), blue));
    assert_eq!(light.tokens[Token::AccentInk], blue.mix(Rgba::WHITE, 0.895));
    // Just past the threshold the mix misses 4.5:1, and the ink is pure black.
    let grey = rgb("#767676");
    assert!(grey.luminance() > DARK_ACCENT_LUMINANCE);
    assert!(grey.mix(Rgba::BLACK, ACCENT_INK_WEIGHT).contrast(grey) < 4.5);
    assert_eq!(ink("#101010", "#767676"), Rgba::BLACK);
}

#[test]
fn the_error_ink_is_the_clipping_red_moved_in_lightness_only() {
    // Luxforge Dark's surfaces need no move.
    let dark = import(&Roles::new(rgb("#202023"), rgb("#e8e8ea"), rgb("#e2b46a")));
    assert_eq!(dark.tokens[Token::Error], rgb("#e5534b"));
    // A light grey ground does: the red darkens, keeping its hue.
    let light = import(&Roles::new(rgb("#e8e8e8"), rgb("#101010"), rgb("#2050c0")));
    let error = light.tokens[Token::Error];
    assert_ne!(error, rgb("#e5534b"));
    assert!(error.oklab()[0] < rgb("#e5534b").oklab()[0]);
    assert!((hue(error) - hue(rgb("#e5534b"))).abs() < 2.0);
    let moved = light
        .report
        .moved
        .iter()
        .find(|m| m.ink == Token::Error)
        .unwrap();
    assert!(!moved.given);
    // A derived ink's move is not an adjustment of the theme's own colours.
    assert!(!light.report.adjusted());
    // And a document may leave its error ink to be derived and moved.
    let document = resolve(
        &Roles::new(rgb("#e8e8e8"), rgb("#101010"), rgb("#2050c0")),
        &Tokens::new(),
        Resolution::Document,
    )
    .unwrap();
    assert_eq!(document.tokens[Token::Error], error);
}

#[test]
fn the_mode_follows_the_background_unless_the_theme_states_it() {
    let dark = import(&Roles::new(rgb("#404040"), rgb("#ffffff"), rgb("#e2b46a")));
    assert!(rgb("#404040").oklab()[0] < LIGHT_MODE_LIGHTNESS);
    assert_eq!(dark.mode, Mode::Dark);
    let light = import(&Roles::new(rgb("#b0b0b0"), rgb("#000000"), rgb("#2050c0")));
    assert!(rgb("#b0b0b0").oklab()[0] >= LIGHT_MODE_LIGHTNESS);
    assert_eq!(light.mode, Mode::Light);
    // The threshold lies between two neighbouring greys.
    assert!(rgb("#636363").oklab()[0] < LIGHT_MODE_LIGHTNESS);
    assert!(rgb("#646464").oklab()[0] >= LIGHT_MODE_LIGHTNESS);
    let mut roles = Roles::new(rgb("#202023"), rgb("#e8e8ea"), rgb("#e2b46a"));
    roles.mode = Some(Mode::Light);
    let stated = import(&roles);
    assert_eq!(stated.mode, Mode::Light);
    assert!(stated.report.given.contains(&"mode".to_owned()));
}

#[test]
fn an_explicit_token_wins_over_its_derivation() {
    let roles = Roles::new(rgb("#1e2030"), rgb("#c0caf5"), rgb("#7aa2f7"));
    let tokens = Tokens::from([
        (Token::Rule, rgb("#ff00ff")),
        (Token::Border, rgb("#ff00ff40")),
        (Token::TextLabel, rgb("#b0b8e0")),
    ]);
    let theme = resolve(&roles, &tokens, Resolution::Document).unwrap();
    assert_eq!(theme.tokens[Token::Rule], rgb("#ff00ff"));
    assert_eq!(theme.tokens[Token::Border], rgb("#ff00ff40"));
    assert_eq!(theme.tokens[Token::TextLabel], rgb("#b0b8e0"));
    assert_eq!(
        theme.report.explicit,
        [Token::TextLabel, Token::Border, Token::Rule]
    );
    // Everything else is what the roles alone derive.
    let derived = import(&roles);
    for (token, value) in theme.tokens.iter() {
        if !tokens.contains_key(&token) {
            assert_eq!(value, derived.tokens[token], "{token}");
        }
    }
}

// The document and its refusals.

fn document(roles: serde_json::Value, extra: serde_json::Value) -> String {
    let mut document = json!({
        "format": THEME_DOCUMENT_FORMAT,
        "version": THEME_DOCUMENT_VERSION,
        "name": "Test",
        "roles": roles,
    });
    if let (Some(fields), serde_json::Value::Object(extra)) = (document.as_object_mut(), extra) {
        fields.extend(extra);
    }
    document.to_string()
}

fn refusal(text: &str) -> (ErrorKind, String) {
    let error = ThemeDocument::read(text).unwrap_err();
    (error.kind, error.to_string())
}

#[test]
fn luxforge_dark_round_trips_through_its_document() {
    let written = luxforge_dark().write();
    let (read, resolved) = ThemeDocument::read(&written).unwrap();
    assert_eq!(read, luxforge_dark());
    assert_eq!(resolved.tokens, Palette::luxforge_dark());
    let at = |key: &str| written.find(&format!("\n  \"{key}\":")).unwrap();
    let keys = ["format", "version", "name", "roles", "tokens"].map(at);
    assert!(keys.is_sorted(), "{written}");
    let value: serde_json::Value = serde_json::from_str(&written).unwrap();
    assert_eq!(value["format"], "luxforge.theme");
    assert_eq!(value["version"], 1);
    assert_eq!(value["roles"]["mode"], "dark");
    assert_eq!(value["roles"]["accent"], "#e2b46a");
    assert_eq!(value["tokens"]["border"], "#ffffff0f");
    assert!(written.ends_with("}\n"));
    // Three roles are a whole document.
    let minimal = document(
        json!({"background": "#1e2030", "text": "#c0caf5", "accent": "#7aa2f7"}),
        json!({}),
    );
    let (read, _) = ThemeDocument::read(&minimal).unwrap();
    assert!(read.tokens.is_empty());
    assert!(!read.write().contains("tokens"));
}

#[test]
fn a_document_is_refused_by_name() {
    let roles = json!({"background": "#202023", "text": "#e8e8ea", "accent": "#e2b46a"});
    let unsupported = ErrorKind::UnsupportedInput;
    let validation = ErrorKind::Validation;
    for (text, kind, names) in [
        ("{".to_owned(), unsupported, "malformed JSON"),
        (
            json!({"format": "luxforge.preset", "version": 1}).to_string(),
            unsupported,
            "not a Luxforge theme document",
        ),
        (
            document(roles.clone(), json!({"version": 2})),
            unsupported,
            "version 2 is not supported",
        ),
        (
            json!({"format": THEME_DOCUMENT_FORMAT, "name": "x", "roles": roles}).to_string(),
            unsupported,
            "has no version",
        ),
        // A missing required role.
        (
            document(
                json!({"background": "#202023", "text": "#e8e8ea"}),
                json!({}),
            ),
            unsupported,
            "missing field `accent`",
        ),
        (
            document(json!({"text": "#e8e8ea", "accent": "#e2b46a"}), json!({})),
            unsupported,
            "missing field `background`",
        ),
        // Unknown fields, at the top and among the roles.
        (
            document(roles.clone(), json!({"author": "me"})),
            unsupported,
            "unknown field `author`",
        ),
        (
            document(
                json!({"background": "#202023", "text": "#e8e8ea", "accent": "#e2b46a",
                       "highlight": "#ffffff"}),
                json!({}),
            ),
            unsupported,
            "unknown field `highlight`",
        ),
        (
            document(roles.clone(), json!({"tokens": {"sidebar": "#000000"}})),
            unsupported,
            "unknown token `sidebar`",
        ),
        (
            document(
                json!({"background": "#202023", "text": "white", "accent": "#e2b46a"}),
                json!({}),
            ),
            unsupported,
            "found \"white\"",
        ),
        (
            document(
                json!({"background": "#202023", "text": "#e8e8ea", "accent": "#e2b46a",
                       "mode": "dim"}),
                json!({}),
            ),
            unsupported,
            "unknown variant `dim`",
        ),
        // A role among the tokens, and alpha where a token takes none.
        (
            document(roles.clone(), json!({"tokens": {"surround": "#000000"}})),
            validation,
            "surround is a role",
        ),
        (
            document(roles.clone(), json!({"tokens": {"rule": "#31313480"}})),
            validation,
            "token rule #31313480 has alpha",
        ),
        (
            document(
                json!({"background": "#202023", "text": "#e8e8ea80", "accent": "#e2b46a"}),
                json!({}),
            ),
            validation,
            "role text #e8e8ea80 has alpha",
        ),
        // Each floor a document's own ink can miss.
        (
            document(
                json!({"background": "#202023", "text": "#a0a0a0", "accent": "#e2b46a"}),
                json!({}),
            ),
            validation,
            "text #a0a0a0 is 5.32:1 against the control #2c2c2f; its floor is 7:1",
        ),
        (
            document(
                json!({"background": "#202023", "text": "#e8e8ea", "accent": "#e2b46a",
                       "text_secondary": "#707070"}),
                json!({}),
            ),
            validation,
            "text_secondary #707070 is 2.81:1 against the control #2c2c2f; its floor is 4.5:1",
        ),
        (
            document(roles.clone(), json!({"tokens": {"text_label": "#808080"}})),
            validation,
            "text_label #808080 is 3.53:1 against the control #2c2c2f; its floor is 4.5:1",
        ),
        (
            document(
                json!({"background": "#202023", "text": "#e8e8ea", "accent": "#e2b46a",
                       "text_tertiary": "#505050"}),
                json!({}),
            ),
            validation,
            "text_tertiary #505050 is 1.73:1 against the control #2c2c2f; its floor is 3:1",
        ),
        (
            document(
                json!({"background": "#202023", "text": "#e8e8ea", "accent": "#5a4020"}),
                json!({}),
            ),
            validation,
            "accent #5a4020 is 1.45:1 against the control #2c2c2f; its floor is 3:1",
        ),
        (
            document(
                json!({"background": "#202023", "text": "#e8e8ea", "accent": "#e2b46a",
                       "error": "#802020"}),
                json!({}),
            ),
            validation,
            "error #802020 is 1.42:1 against the control #2c2c2f; its floor is 3:1",
        ),
        (
            document(
                json!({"background": "#202023", "text": "#e8e8ea", "accent": "#e2b46a",
                       "accent_ink": "#a08050"}),
                json!({}),
            ),
            validation,
            "accent_ink #a08050 is 1.93:1 against the accent #e2b46a; its floor is 4.5:1",
        ),
        // A surround or rail backdrop of its own past the chroma bound.
        (
            document(
                json!({"background": "#202023", "text": "#e8e8ea", "accent": "#e2b46a",
                       "surround": "#031222"}),
                json!({}),
            ),
            validation,
            "surround #031222 carries 0.0402 OKLCh chroma; it is held to 0.01",
        ),
        (
            document(
                roles.clone(),
                json!({"tokens": {"rail_backdrop": "#1e2030"}}),
            ),
            validation,
            "rail_backdrop #1e2030 carries 0.0305 OKLCh chroma",
        ),
    ] {
        let (actual, message) = refusal(&text);
        assert_eq!(actual, kind, "{message}");
        assert!(
            message.contains(names),
            "{message} does not contain {names}"
        );
    }
}

#[test]
fn the_report_and_resolved_theme_serialize_for_the_api() {
    let theme = import(&Roles::new(rgb("#f4efe6"), rgb("#6b6b6b"), rgb("#e0a030")));
    let value = serde_json::to_value(&theme).unwrap();
    assert_eq!(value["mode"], "light");
    // The tokens in table order, every one of them.
    let text = serde_json::to_string(&theme.tokens).unwrap();
    let places = TABLE.map(|name| text.find(&format!("\"{name}\":")).unwrap());
    assert!(places.is_sorted(), "{text}");
    assert_eq!(value["tokens"].as_object().unwrap().len(), TOKEN_COUNT);
    assert_eq!(value["report"]["moved"][0]["ink"], "text");
    assert_eq!(value["report"]["moved"][0]["before"], "#6b6b6b");
    assert_eq!(value["report"]["accent"]["nearest"], "clipping_red");
    let back: ResolvedTheme = serde_json::from_value(value).unwrap();
    assert_eq!(back, theme);
    // A palette missing a token is refused by its name.
    let mut partial = serde_json::to_value(theme.tokens).unwrap();
    partial.as_object_mut().unwrap().remove("thumb");
    let error = serde_json::from_value::<Palette>(partial).unwrap_err();
    assert!(
        error.to_string().contains("missing token `thumb`"),
        "{error}"
    );
}
