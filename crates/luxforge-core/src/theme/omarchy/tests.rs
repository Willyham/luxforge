use super::*;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// What Omarchy's resolver gives at [`OMARCHY_COMMIT`] for its 22 built-in themes and for the
/// synthetic themes in `testdata/`, written by `tools/omarchy_theme_parity.py`.
const PARITY: &str = include_str!("parity.json");

fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
        .collect()
}

fn colors(text: &str) -> Result<Palette, ReadError> {
    read(&files(&[(COLORS_TOML, text)]))
}

/// The three files a theme folder holds, as a client sends them.
fn folder(path: &Path) -> BTreeMap<String, String> {
    FILES
        .iter()
        .filter_map(|name| {
            let text = std::fs::read_to_string(path.join(name)).ok()?;
            Some(((*name).to_owned(), text))
        })
        .collect()
}

fn testdata(name: &str) -> Palette {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/theme/omarchy/testdata");
    read(&folder(&path.join(name))).unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn rgb(hex: &str) -> Rgb8 {
    Rgb8::parse_hex(hex).unwrap()
}

/// One theme of the parity fixture.
struct Expected {
    slug: String,
    source: String,
    mode: Mode,
    values: Vec<Option<String>>,
    other: BTreeMap<String, String>,
    palette: Option<String>,
}

fn parity() -> Vec<Expected> {
    let fixture: Json = serde_json::from_str(PARITY).unwrap();
    assert_eq!(fixture["omarchy_commit"], OMARCHY_COMMIT);
    assert_eq!(
        fixture["keys"],
        json!(KEYS.as_slice()),
        "the fixture's keys are the resolver's"
    );
    let themes = fixture["themes"].as_array().unwrap();
    themes
        .iter()
        .map(|theme| Expected {
            slug: theme["slug"].as_str().unwrap().to_owned(),
            source: theme["source"].as_str().unwrap().to_owned(),
            mode: match theme["mode"].as_str().unwrap() {
                "light" => Mode::Light,
                "dark" => Mode::Dark,
                mode => panic!("{mode}"),
            },
            values: serde_json::from_value(theme["values"].clone()).unwrap(),
            other: serde_json::from_value(theme["other"].clone()).unwrap(),
            // A minimal copy of the palette, one `key = "value"` line each, as Omarchy read it.
            palette: theme["palette"].as_array().map(|lines| {
                lines
                    .iter()
                    .map(|line| {
                        let (key, value) = line.as_str().unwrap().split_once('=').unwrap();
                        format!("{key} = \"{value}\"\n")
                    })
                    .collect()
            }),
        })
        .collect()
}

/// Omarchy's 22 built-in themes at [`OMARCHY_COMMIT`], each its slug and the files a client sends
/// for it: the bundled `colors.toml`, or the fixture's copy of the palette.
pub(crate) fn built_in_folders() -> Vec<(String, BTreeMap<String, String>)> {
    parity()
        .into_iter()
        .filter(|expected| expected.source == "omarchy")
        .map(|expected| {
            let text = expected.palette.unwrap_or_else(|| {
                BUNDLED
                    .iter()
                    .find(|bundled| bundled.slug == expected.slug)
                    .expect("a built-in theme is bundled or copied")
                    .colors_toml
                    .to_owned()
            });
            (expected.slug, files(&[(COLORS_TOML, &text)]))
        })
        .collect()
}

/// The synthetic themes in `testdata/`, each its folder's name and files.
pub(crate) fn synthetic_folders() -> Vec<(String, BTreeMap<String, String>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/theme/omarchy/testdata");
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let files = folder(&root.join(&name));
            (name, files)
        })
        .collect()
}

/// The palette resolves to every value Omarchy's resolver gives, and lists as unknown exactly the
/// keys it passes through verbatim.
fn assert_parity(palette: &Palette, expected: &Expected) {
    let slug = &expected.slug;
    assert_eq!(palette.mode, expected.mode, "{slug}: mode");
    for (key, value) in KEYS.iter().zip(&expected.values) {
        // Omarchy passes a theme's own value through in its case; the reader writes lower case.
        let value = value.as_ref().map(|value| value.to_ascii_lowercase());
        let resolved = palette.colours.get(key).map(Rgb8::to_string);
        assert_eq!(resolved, value, "{slug}: {key}");
    }
    assert_eq!(
        palette.colours.len(),
        expected.values.iter().flatten().count()
    );
    let unknown: BTreeMap<String, String> = palette
        .unused
        .iter()
        // An `alacritty.toml` theme's unused keys never reach the `colors.toml` Omarchy writes.
        .filter(|unused| {
            unused.file == COLORS_TOML
                && matches!(
                    unused.reason,
                    UnusedReason::UnknownKey | UnusedReason::Gradient
                )
        })
        .map(|unused| (unused.key.clone(), unused.value.clone()))
        .collect();
    assert_eq!(unknown, expected.other, "{slug}: keys passed through");
    let colour = |key| palette.colours[key];
    assert_eq!(palette.background, colour("background"), "{slug}");
    assert_eq!(palette.dark_background, colour("dark_background"), "{slug}");
    assert_eq!(
        palette.lighter_background,
        colour("lighter_background"),
        "{slug}"
    );
    assert_eq!(palette.foreground, colour("foreground"), "{slug}");
    assert_eq!(palette.accent, colour("accent"), "{slug}");
}

#[test]
fn every_built_in_and_synthetic_theme_resolves_as_omarchy_resolves_it() {
    let themes = parity();
    assert_eq!(themes.iter().filter(|t| t.source == "omarchy").count(), 22);
    let mut synthetic = 0;
    for expected in &themes {
        let palette = match (expected.source.as_str(), &expected.palette) {
            ("omarchy", Some(text)) => colors(text),
            ("omarchy", None) => BUNDLED
                .iter()
                .find(|bundled| bundled.slug == expected.slug)
                .unwrap_or_else(|| panic!("{} is neither bundled nor copied", expected.slug))
                .read(),
            _ => {
                synthetic += 1;
                Ok(testdata(&expected.slug))
            }
        };
        let palette = palette.unwrap_or_else(|error| panic!("{}: {error}", expected.slug));
        assert_parity(&palette, expected);
    }
    assert_eq!(synthetic, 9);
}

/// The same check on Omarchy's own files: a clone at the pinned commit in `target/omarchy`
/// (`git clone https://github.com/omacom/omarchy target/omarchy && git -C target/omarchy checkout
/// 035ce29f03bdd97a09af80ef5f2d22d7a98930d6`). The test above runs on the bundled files and on
/// the fixture's copies of the other sixteen; this one shows those copies are faithful.
#[test]
#[ignore = "needs an Omarchy clone at the pinned commit in target/omarchy"]
fn omarchy_clone_resolves_as_the_fixture() {
    let clone = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/omarchy");
    let head = std::fs::read_to_string(clone.join(".git/HEAD")).unwrap();
    assert_eq!(head.trim(), OMARCHY_COMMIT);
    let mut slugs: Vec<PathBuf> = std::fs::read_dir(clone.join("themes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    slugs.sort();
    let themes = parity();
    assert_eq!(slugs.len(), 22);
    for path in slugs {
        let slug = path.file_name().unwrap().to_str().unwrap();
        let expected = themes.iter().find(|t| t.slug == slug).unwrap();
        let palette = read(&folder(&path)).unwrap();
        assert_parity(&palette, expected);
        if let Some(copy) = &expected.palette {
            assert_eq!(colors(copy).unwrap().colours, palette.colours, "{slug}");
        }
    }
}

#[test]
fn bundled_files_are_the_pinned_commits() {
    let sha256 = |text: &str| format!("{:x}", Sha256::digest(text.as_bytes()));
    let pinned = [
        (
            "tokyo-night",
            "8b5f53ba35b9305aafa72776dd56ea2029faaf29015e2e42db5e19ef698a8e0e",
        ),
        (
            "catppuccin",
            "a7eddcf3342ee0bee5df5ed7aad71c0e8c07ba5549bba7e040064fc5abc09eea",
        ),
        (
            "catppuccin-latte",
            "071e27fe9b0dd7ee0063c7a3a55288648dcc6ac05a6cf7702caeb51c9ca4398e",
        ),
        (
            "gruvbox",
            "dc30923fe0f19220ff8087f7830c3e7aa5760829b7f2d2f9a10cadc17e02abe0",
        ),
        (
            "nord",
            "62d1739a488db2a66b88f2f054579bdfd08c4ec771d2acf5659b9f9d976c57e2",
        ),
        (
            "everforest",
            "1adc95c8b6ee34d71106bacf23079f14667b82ec7d8f0b7a478ce3f8d57d1c09",
        ),
    ];
    assert_eq!(BUNDLED.map(|b| b.slug), pinned.map(|(slug, _)| slug));
    for (bundled, (_, digest)) in BUNDLED.iter().zip(pinned) {
        assert_eq!(sha256(bundled.colors_toml), digest, "{}", bundled.slug);
    }
    assert_eq!(
        sha256(include_str!("bundled/LICENSE")),
        "717ba1949502290f8e47688ae2e323acd06c8ca47aec9f7596b15f678c1af4a2"
    );
    assert!(include_str!("bundled/NOTICE.md").contains(OMARCHY_COMMIT));
}

#[test]
fn bundled_themes_are_named_as_omarchy_lists_them() {
    for bundled in BUNDLED {
        assert_eq!(theme_name(bundled.slug).as_deref(), Some(bundled.name));
        assert_eq!(bundled.id(), format!("omarchy.{}", bundled.slug));
    }
    let modes = BUNDLED.map(|bundled| bundled.read().unwrap().mode);
    use Mode::{Dark, Light};
    assert_eq!(modes, [Dark, Dark, Light, Dark, Dark, Dark]);
    let tokyo_night = BUNDLED[0].read().unwrap();
    assert_eq!(
        (
            tokyo_night.form,
            tokyo_night.mode_source,
            tokyo_night.background,
            tokyo_night.dark_background,
            tokyo_night.lighter_background,
            tokyo_night.foreground,
            tokyo_night.accent,
        ),
        (
            Form::Omarchy4,
            ModeSource::ModeKey,
            rgb("#1a1b26"),
            rgb("#13141c"),
            rgb("#24283b"),
            rgb("#a9b1d6"),
            rgb("#7aa2f7"),
        )
    );
    assert!(tokyo_night.unused.is_empty());
}

#[test]
fn theme_names_follow_omarchy() {
    for (slug, name) in [
        ("tokyo-night", "Tokyo Night"),
        ("catppuccin-latte", "Catppuccin Latte"),
        ("retro-82", "Retro 82"),
        ("omarchy-ash-theme", "Ash"),
        ("omarchy-theme", "Theme"),
        ("nes-theme", "Nes"),
        ("omarchy-dark-matter", "Dark Matter"),
        ("vhs-80", "Vhs 80"),
        ("A-b", "A B"),
    ] {
        assert_eq!(theme_name(slug).as_deref(), Some(name), "{slug}");
    }
    assert_eq!(theme_name("omarchy-"), None);
}

fn unused(file: &'static str, key: &str, value: &str, reason: UnusedReason) -> Unused {
    Unused {
        file,
        key: key.to_owned(),
        value: value.to_owned(),
        reason,
    }
}

#[test]
fn synthetic_themes_report_their_form_mode_and_unused_keys() {
    use UnusedReason::*;
    let omarchy4 = testdata("omarchy4");
    assert_eq!(
        (omarchy4.form, omarchy4.mode_source),
        (Form::Omarchy4, ModeSource::ModeKey)
    );
    assert_eq!(
        omarchy4.unused,
        [
            unused(
                COLORS_TOML,
                "hyprland_active_border",
                "rgba(5e9ce0ee) rgba(98c379ee) 45deg",
                Gradient
            ),
            unused(
                COLORS_TOML,
                "hyprland_inactive_border",
                "rgb(252a35)",
                Gradient
            ),
            unused(COLORS_TOML, "active_tab_background", "#2b3140", UnknownKey),
        ]
    );
    // Left out, so mixed: the background 25% with black, and `purple` standing for `magenta`.
    assert_eq!(omarchy4.dark_background, rgb("#14171d"));
    assert_eq!(omarchy4.colours["magenta"], rgb("#c678dd"));

    let omarchy3 = testdata("omarchy3");
    assert_eq!(
        (omarchy3.form, omarchy3.mode, omarchy3.mode_source),
        (Form::Omarchy3, Mode::Dark, ModeSource::BackgroundBrightness)
    );
    // `color0` and `color7` become the background and foreground; `lighter_background`, which the
    // form has no key for, is the background.
    assert_eq!(
        omarchy3.unused,
        [
            unused(COLORS_TOML, "color0", "#3b4252", Replaced),
            unused(COLORS_TOML, "color7", "#e5e9f0", Replaced),
        ]
    );
    assert_eq!(omarchy3.lighter_background, omarchy3.background);

    let short = testdata("short-names");
    assert_eq!(
        (short.form, short.mode, short.mode_source),
        (Form::Omarchy4, Mode::Light, ModeSource::ThemeTypeKey)
    );
    assert_eq!(
        short.unused,
        [unused(COLORS_TOML, "fg", "#000000", Replaced)]
    );
    assert_eq!(short.foreground, rgb("#575279"));
    assert_eq!(short.colours["fg"], rgb("#575279"));
    assert_eq!(short.dark_background, rgb("#f2e9e1"));

    let alacritty = testdata("alacritty");
    assert_eq!(
        (alacritty.form, alacritty.mode, alacritty.mode_source),
        (
            Form::Alacritty,
            Mode::Dark,
            ModeSource::BackgroundBrightness
        )
    );
    let file = ALACRITTY_TOML;
    assert_eq!(
        alacritty.unused,
        [
            unused(file, "colors.primary.dim_foreground", "#7f849c", UnknownKey),
            unused(file, "colors.cursor.text", "#1e1e2e", UnknownKey),
            unused(file, "colors.cursor.cursor", "#f5e0dc", UnknownKey),
            unused(file, "colors.selection.text", "CellBackground", UnknownKey),
            unused(
                file,
                "colors.selection.background",
                "CellForeground",
                NotAColour
            ),
        ]
    );
    // The normal blue is the accent; a bright colour left out is the normal one, taken before the
    // primary background replaces `color0`; the selection falls back to the foreground.
    assert_eq!(alacritty.accent, rgb("#89b4fa"));
    assert_eq!(alacritty.background, rgb("#1e1e2e"));
    assert_eq!(alacritty.colours["color8"], rgb("#45475a"));
    assert_eq!(alacritty.colours["color10"], rgb("#89d88b"));
    assert_eq!(alacritty.colours["selection"], alacritty.foreground);

    let marker = testdata("alacritty-light");
    assert_eq!(
        (marker.form, marker.mode, marker.mode_source),
        (Form::Alacritty, Mode::Light, ModeSource::LightModeFile)
    );
    assert!(marker.unused.is_empty());

    for (name, mode, source) in [
        ("light-mode-file", Mode::Light, ModeSource::LightModeFile),
        ("mode-over-marker", Mode::Dark, ModeSource::ModeKey),
        (
            "brightness-382",
            Mode::Dark,
            ModeSource::BackgroundBrightness,
        ),
        (
            "brightness-383",
            Mode::Light,
            ModeSource::BackgroundBrightness,
        ),
    ] {
        let palette = testdata(name);
        assert_eq!(
            (palette.mode, palette.mode_source),
            (mode, source),
            "{name}"
        );
        assert_eq!(palette.form, Form::Omarchy4, "{name}");
    }
}

#[test]
fn mixes_round_as_omarchy_does() {
    // int(a * (1 - t) + b * t + 0.5): halves round up.
    assert_eq!(Rgb8([1, 3, 255]).mix(BLACK, 0.5), Rgb8([1, 2, 128]));
    assert_eq!(Rgb8([255, 254, 0]).mix(BLACK, 0.25), Rgb8([191, 191, 0]));
    assert_eq!(Rgb8([0, 10, 200]).mix(WHITE, 0.2), Rgb8([51, 59, 211]));
}

#[test]
fn background_and_foreground_fall_back_to_ansi_slots() {
    let palette =
        colors("accent = \"#ff0000\"\ncolor0 = \"#101010\"\ncolor7 = \"#e0e0e0\"\n").unwrap();
    assert_eq!(palette.background, rgb("#101010"));
    assert_eq!(palette.foreground, rgb("#e0e0e0"));
    assert_eq!(palette.form, Form::Omarchy3);
}

#[test]
fn a_mix_of_a_missing_colour_is_left_out() {
    let palette = colors(
        "accent = \"#ff0000\"\nbackground = \"#000000\"\nforeground = \"#ffffff\"\nred = \"\"\n",
    )
    .unwrap();
    for key in ["red", "color1", "bright_red", "color9", "orange", "brown"] {
        assert!(!palette.colours.contains_key(key), "{key}");
    }
    assert_eq!(
        palette.unused,
        [unused(COLORS_TOML, "red", "", UnusedReason::Empty)]
    );
}

#[test]
fn keys_under_a_table_are_listed_by_their_path() {
    let palette = colors(
        "accent = \"#ff0000\"\nbackground = \"#000000\"\nforeground = \"#ffffff\"\ntheme_type = \"light\"\nmode = \"dark\"\n[shell]\nborder = \"#ffffff\"\nwidth = 2\n",
    )
    .unwrap();
    assert_eq!(palette.mode_source, ModeSource::ModeKey);
    assert_eq!(
        palette.unused,
        [
            unused(COLORS_TOML, "theme_type", "light", UnusedReason::Replaced),
            unused(
                COLORS_TOML,
                "shell.border",
                "#ffffff",
                UnusedReason::UnknownKey
            ),
            unused(COLORS_TOML, "shell.width", "2", UnusedReason::UnknownKey),
        ]
    );
}

#[test]
fn refusals_name_what_is_wrong() {
    const BASE: &str = "background = \"#000000\"\nforeground = \"#ffffff\"\n";
    let with = |line: &str| format!("{BASE}accent = \"#ff0000\"\n{line}\n");

    assert_eq!(
        read(&files(&[(COLORS_TOML, BASE), ("neovim.lua", "")])),
        Err(ReadError::FileNotAllowed {
            file: "neovim.lua".into()
        })
    );
    assert_eq!(
        read(&files(&[("themes/nord/colors.toml", BASE)])),
        Err(ReadError::FileNotAllowed {
            file: "themes/nord/colors.toml".into()
        })
    );
    assert_eq!(read(&files(&[(LIGHT_MODE, "")])), Err(ReadError::NoPalette));

    // At most 64 KiB a file, each file counted.
    let full = with(&"#".repeat(MAX_FILE_BYTES - with("").len()));
    assert_eq!(full.len(), MAX_FILE_BYTES);
    assert!(colors(&full).is_ok());
    let over = format!("{full} ");
    assert_eq!(
        colors(&over),
        Err(ReadError::FileTooLarge {
            file: COLORS_TOML,
            bytes: MAX_FILE_BYTES + 1,
            limit: MAX_FILE_BYTES
        })
    );
    let marker = " ".repeat(MAX_FILE_BYTES + 1);
    assert!(matches!(
        read(&files(&[(COLORS_TOML, &full), (LIGHT_MODE, &marker)])),
        Err(ReadError::FileTooLarge {
            file: LIGHT_MODE,
            ..
        })
    ));

    assert_eq!(
        colors(&format!("{BASE}accent = #ff0000\n")),
        Err(ReadError::MalformedToml {
            file: COLORS_TOML,
            line: 3,
            column: 10,
            message: "string values must be quoted, expected literal string".into()
        })
    );
    assert!(matches!(
        colors(&with("red = \"#ff0000\"\nred = \"#00ff00\"")),
        Err(ReadError::MalformedToml { line: 5, .. })
    ));

    for (line, key, value) in [
        ("red = \"#ff000\"", "red", "#ff000"),
        ("red = \"ff0000\"", "red", "ff0000"),
        ("red = \"rgb(255,0,0)\"", "red", "rgb(255,0,0)"),
        ("color4 = 0xff0000", "color4", "0xff0000"),
        (
            "selection = \"rgba(33ccffee) rgba(00ff99ee) 45deg\"",
            "selection",
            "rgba(33ccffee) rgba(00ff99ee) 45deg",
        ),
    ] {
        assert_eq!(
            colors(&with(line)),
            Err(ReadError::NotHex {
                file: COLORS_TOML,
                key: key.into(),
                value: value.into()
            }),
            "{line}"
        );
    }
    for (line, key, value) in [
        ("mode = \"auto\"", "mode", "\"auto\""),
        ("theme_type = \"Light\"", "theme_type", "\"Light\""),
        ("mode = true", "mode", "true"),
    ] {
        let error = colors(&with(line)).unwrap_err();
        let shown = value.trim_matches('"');
        assert_eq!(
            error,
            ReadError::InvalidMode {
                file: COLORS_TOML,
                key,
                value: shown.into()
            },
            "{line}"
        );
    }

    let missing = |key: &str| {
        Err(ReadError::MissingKey {
            file: COLORS_TOML,
            key: key.into(),
        })
    };
    assert_eq!(
        colors("foreground = \"#ffffff\"\naccent = \"#ff0000\"\n"),
        missing("background")
    );
    assert_eq!(
        colors("background = \"#000000\"\naccent = \"#ff0000\"\n"),
        missing("foreground")
    );
    // Omarchy's resolver gives no accent of its own, so a palette without one is refused, its blue
    // notwithstanding.
    assert_eq!(
        colors(&format!("{BASE}blue = \"#0000ff\"\n")),
        missing("accent")
    );
    assert_eq!(colors(&format!("{BASE}accent = \"\"\n")), missing("accent"));

    let alacritty = "[colors.normal]\nblack = \"#000000\"\nred = \"#ff0000\"\ngreen = \"#00ff00\"\nyellow = \"#ffff00\"\nmagenta = \"#ff00ff\"\ncyan = \"#00ffff\"\nwhite = \"#ffffff\"\n";
    assert_eq!(
        read(&files(&[(ALACRITTY_TOML, alacritty)])),
        Err(ReadError::MissingKey {
            file: ALACRITTY_TOML,
            key: "colors.normal.blue".into()
        })
    );
    let not_a_colour = alacritty.replace(
        "[colors.normal]\n",
        "[colors.normal]\nblue = \"CellForeground\"\n",
    );
    assert!(matches!(
        read(&files(&[(ALACRITTY_TOML, &not_a_colour)])),
        Err(ReadError::MissingKey { key, .. }) if key == "colors.normal.blue"
    ));
    // Omarchy reads no inline table and nothing outside a header, whole palettes included.
    let complete = alacritty.replace("[colors.normal]\n", "[colors.normal]\nblue = \"#0000ff\"\n");
    assert!(read(&files(&[(ALACRITTY_TOML, &complete)])).is_ok());
    let pairs: Vec<&str> = complete.lines().skip(1).collect();
    let inline = format!("[colors]\nnormal = {{ {} }}\n", pairs.join(", "));
    let rootless: String = pairs
        .iter()
        .map(|pair| format!("colors.normal.{pair}\n"))
        .collect();
    for text in [inline, rootless] {
        assert!(
            matches!(
                read(&files(&[(ALACRITTY_TOML, &text)])),
                Err(ReadError::MissingKey { key, .. }) if key == "colors.normal.black"
            ),
            "{text}"
        );
    }
    // `colors.toml` wins: a broken `alacritty.toml` beside it is never parsed.
    assert!(read(&files(&[(COLORS_TOML, &with("")), (ALACRITTY_TOML, "[")])).is_ok());
}

#[test]
fn reports_serialize_with_hex_colours_and_coded_refusals() {
    let palette = serde_json::to_value(testdata("omarchy3")).unwrap();
    assert_eq!(palette["form"], "omarchy3");
    assert_eq!(palette["mode"], "dark");
    assert_eq!(palette["mode_source"], "background-brightness");
    assert_eq!(palette["accent"], "#d08770");
    assert_eq!(palette["colours"]["bright_blue"], "#8cafd2");
    assert_eq!(palette["unused"][0]["reason"], "replaced");
    let error = ReadError::MissingKey {
        file: COLORS_TOML,
        key: "accent".into(),
    };
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({"code": "missing-key", "file": "colors.toml", "key": "accent"})
    );
    assert_eq!(error.to_string(), "colors.toml has no accent");
}
