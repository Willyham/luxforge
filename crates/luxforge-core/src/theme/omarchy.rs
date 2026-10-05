//! Omarchy theme palettes, read as data and resolved as Omarchy resolves them
//! (`docs/design/ui-themes.md#omarchy-themes`).
//!
//! A theme is read from the text of at most three of its files, which the client sends by name:
//! `colors.toml`, `alacritty.toml` (read only when there is no `colors.toml`) and the `light.mode`
//! marker. The core never opens a theme's folder, and nothing a theme ships is run.
//!
//! [`read`] is a port of Omarchy's resolver, `bin/omarchy-theme-color`, at [`OMARCHY_COMMIT`],
//! statement by statement and in its order: the short and ANSI aliases, the fallbacks, the mixes
//! with Omarchy's rounding, and the mode's precedence. A theme with only `alacritty.toml` first
//! gets the palette `bin/omarchy-theme-colors-from-alacritty` writes for it. Where the port differs,
//! it says so: it parses strict TOML where Omarchy reads lines, every colour must be `#rrggbb`, and
//! a mix of a colour the theme does not have leaves the mixed key out, where Omarchy's awk would
//! mix an empty string.
use serde::{Deserialize, Serialize, Serializer};
use std::{collections::BTreeMap, fmt, ops::Range};
use toml_edit::{Document, Item, Table, Value};

/// The Omarchy commit the resolver is ported from and the bundled palettes are copied from: the
/// default branch, `quattro`, on 2026-10-04, whose theme files equal the v4.0.4 release's.
pub const OMARCHY_COMMIT: &str = "035ce29f03bdd97a09af80ef5f2d22d7a98930d6";

pub const COLORS_TOML: &str = "colors.toml";
pub const ALACRITTY_TOML: &str = "alacritty.toml";
/// A marker: its presence makes a theme with no `mode` or `theme_type` key light. Its text is not
/// read.
pub const LIGHT_MODE: &str = "light.mode";
/// The only files [`read`] accepts.
pub const FILES: [&str; 3] = [COLORS_TOML, ALACRITTY_TOML, LIGHT_MODE];
/// The most each file may hold, in bytes.
pub const MAX_FILE_BYTES: usize = 64 * 1024;

/// Every colour key Omarchy's resolver answers, in the order `omarchy-theme-color --all` prints
/// them (`LC_ALL=C sort`). Beside these it answers `mode`, `theme_type` and, verbatim, any key it
/// does not know; this reader lists those as [`Unused`].
pub const KEYS: [&str; 54] = [
    "accent",
    "background",
    "bg",
    "blue",
    "bright_blue",
    "bright_cyan",
    "bright_fg",
    "bright_foreground",
    "bright_green",
    "bright_magenta",
    "bright_purple",
    "bright_red",
    "bright_yellow",
    "brown",
    "color0",
    "color1",
    "color10",
    "color11",
    "color12",
    "color13",
    "color14",
    "color15",
    "color2",
    "color3",
    "color4",
    "color5",
    "color6",
    "color7",
    "color8",
    "color9",
    "cursor",
    "cyan",
    "dark_background",
    "dark_bg",
    "dark_fg",
    "dark_foreground",
    "darker_background",
    "darker_bg",
    "fg",
    "foreground",
    "green",
    "light_fg",
    "light_foreground",
    "lighter_background",
    "lighter_bg",
    "magenta",
    "muted",
    "orange",
    "purple",
    "red",
    "selection",
    "selection_background",
    "selection_foreground",
    "yellow",
];

/// The keys of the Omarchy 3.3 to 3.8 terminal form.
const OMARCHY3_KEYS: [&str; 22] = [
    "accent",
    "cursor",
    "foreground",
    "background",
    "selection_foreground",
    "selection_background",
    "color0",
    "color1",
    "color2",
    "color3",
    "color4",
    "color5",
    "color6",
    "color7",
    "color8",
    "color9",
    "color10",
    "color11",
    "color12",
    "color13",
    "color14",
    "color15",
];

/// The canonical background and foreground names and their legacy short forms.
const SHORT_NAMES: [(&str, &str); 8] = [
    ("background", "bg"),
    ("dark_background", "dark_bg"),
    ("darker_background", "darker_bg"),
    ("lighter_background", "lighter_bg"),
    ("foreground", "fg"),
    ("dark_foreground", "dark_fg"),
    ("light_foreground", "light_fg"),
    ("bright_foreground", "bright_fg"),
];

/// The named colours and the ANSI slot each falls back to.
const NAMED_ANSI: [(&str, &str); 12] = [
    ("red", "color1"),
    ("green", "color2"),
    ("yellow", "color3"),
    ("blue", "color4"),
    ("magenta", "color5"),
    ("cyan", "color6"),
    ("bright_red", "color9"),
    ("bright_green", "color10"),
    ("bright_yellow", "color11"),
    ("bright_blue", "color12"),
    ("bright_magenta", "color13"),
    ("bright_cyan", "color14"),
];

/// Each ANSI slot and the semantic key it answers when the theme leaves it out.
const ANSI_SEMANTIC: [(&str, &str); 16] = [
    ("color0", "background"),
    ("color1", "red"),
    ("color2", "green"),
    ("color3", "yellow"),
    ("color4", "blue"),
    ("color5", "magenta"),
    ("color6", "cyan"),
    ("color7", "foreground"),
    ("color8", "muted"),
    ("color9", "bright_red"),
    ("color10", "bright_green"),
    ("color11", "bright_yellow"),
    ("color12", "bright_blue"),
    ("color13", "bright_magenta"),
    ("color14", "bright_cyan"),
    ("color15", "bright_foreground"),
];

/// Alacritty's eight colour names, `color0` to `color7` in order.
const ALACRITTY_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

const BLACK: Rgb8 = Rgb8([0, 0, 0]);
const WHITE: Rgb8 = Rgb8([255, 255, 255]);

/// An 8-bit sRGB colour, written and serialized as `#rrggbb`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rgb8(pub [u8; 3]);

impl Rgb8 {
    /// Exactly `#` and six hex digits, in either case.
    pub fn parse_hex(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        Self::parse_digits(digits)
    }

    fn parse_digits(digits: &str) -> Option<Self> {
        if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        Some(Self([channel(0)?, channel(2)?, channel(4)?]))
    }

    /// Omarchy's `mix_color`: each channel `int(a * (1 - t) + b * t + 0.5)`, in awk's doubles.
    fn mix(self, other: Self, amount: f64) -> Self {
        let channel =
            |a: u8, b: u8| (f64::from(a) * (1.0 - amount) + f64::from(b) * amount + 0.5) as u8;
        Self([0, 1, 2].map(|i| channel(self.0[i], other.0[i])))
    }
}

impl fmt::Display for Rgb8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [r, g, b] = self.0;
        write!(f, "#{r:02x}{g:02x}{b:02x}")
    }
}

impl Serialize for Rgb8 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Light,
    Dark,
}

/// How the mode was decided, in Omarchy's precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModeSource {
    /// The `mode` key.
    ModeKey,
    /// The legacy `theme_type` key.
    ThemeTypeKey,
    /// A `light.mode` marker beside the palette.
    LightModeFile,
    /// The background's channels: light when `r + g + b` is above 382, as Omarchy computes it.
    BackgroundBrightness,
}

/// Which of Omarchy's forms the palette was in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Form {
    /// Omarchy 4's semantic keys: any `colors.toml` that is not the Omarchy 3 form.
    Omarchy4,
    /// The Omarchy 3.3 to 3.8 terminal form: a `colors.toml` naming ANSI slots (`color0` to
    /// `color15`), no colour key outside that form (`accent`, `cursor`, `foreground`,
    /// `background`, `selection_foreground`, `selection_background`), and no `mode`.
    Omarchy3,
    /// No `colors.toml`: the palette Omarchy derives from `alacritty.toml`.
    Alacritty,
}

impl Form {
    /// The form's name as it serializes: `omarchy4`, `omarchy3` or `alacritty`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Omarchy4 => "omarchy4",
            Self::Omarchy3 => "omarchy3",
            Self::Alacritty => "alacritty",
        }
    }
}

/// Why a key a theme holds is not used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnusedReason {
    /// A key Omarchy's resolver does not read.
    UnknownKey,
    /// A Hyprland colour or gradient, such as `rgba(33ccffee) rgba(00ff99ee) 45deg`, under a key
    /// the resolver does not read.
    Gradient,
    /// The resolver gave the key another value, as it gives `cursor` the bright foreground and a
    /// short name its canonical name's value.
    Replaced,
    /// An empty value, which Omarchy treats as no value.
    Empty,
    /// An `alacritty.toml` colour Omarchy reads that is not a hex colour, such as
    /// `CellForeground`: Omarchy skips it, and takes its fallback.
    NotAColour,
}

/// One key and value a theme holds that its palette does not use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Unused {
    pub file: &'static str,
    /// The key, dotted from the file's root.
    pub key: String,
    /// A string's text, or any other value as the file writes it.
    pub value: String,
    pub reason: UnusedReason,
}

/// A theme's palette, resolved as Omarchy resolves it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Palette {
    pub form: Form,
    pub mode: Mode,
    pub mode_source: ModeSource,
    pub background: Rgb8,
    /// The background mixed 25% with black when the theme leaves it out.
    pub dark_background: Rgb8,
    /// `color0`, which is the background, when the theme leaves it out.
    pub lighter_background: Rgb8,
    pub foreground: Rgb8,
    /// The terminal's normal blue for an `alacritty.toml` theme.
    pub accent: Rgb8,
    /// Every colour key the resolver answers ([`KEYS`]), the five above included. A key is absent
    /// only when the theme has no colour it falls back to.
    pub colours: BTreeMap<&'static str, Rgb8>,
    /// Every key and value the theme holds that the palette does not use, in the file's order.
    pub unused: Vec<Unused>,
}

/// A theme refused, by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "kebab-case")]
pub enum ReadError {
    /// A file other than `colors.toml`, `alacritty.toml` and `light.mode`.
    FileNotAllowed { file: String },
    FileTooLarge {
        file: &'static str,
        bytes: usize,
        limit: usize,
    },
    /// Neither `colors.toml` nor `alacritty.toml`.
    NoPalette,
    /// Not TOML. `line` and `column` count from 1.
    MalformedToml {
        file: &'static str,
        line: usize,
        column: usize,
        message: String,
    },
    /// A colour key whose value is not `#rrggbb`.
    NotHex {
        file: &'static str,
        key: String,
        value: String,
    },
    /// A `mode` or `theme_type` other than `light` or `dark`.
    InvalidMode {
        file: &'static str,
        key: &'static str,
        value: String,
    },
    /// A key the palette needs and no fallback gives: the background, the foreground or the accent
    /// of a `colors.toml`, or one of an `alacritty.toml`'s eight normal colours.
    MissingKey { file: &'static str, key: String },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileNotAllowed { file } => write!(
                f,
                "{file:?} is not read; an Omarchy theme is read from colors.toml, alacritty.toml and light.mode only"
            ),
            Self::FileTooLarge { file, bytes, limit } => {
                write!(f, "{file} holds {bytes} bytes, more than {limit}")
            }
            Self::NoPalette => write!(f, "the theme has neither colors.toml nor alacritty.toml"),
            Self::MalformedToml {
                file,
                line,
                column,
                message,
            } => write!(
                f,
                "{file} is not TOML at line {line}, column {column}: {message}"
            ),
            Self::NotHex { file, key, value } => {
                write!(f, "{file}: {key} is {value:?}, not a #rrggbb colour")
            }
            Self::InvalidMode { file, key, value } => {
                write!(f, "{file}: {key} is {value:?}, not \"light\" or \"dark\"")
            }
            Self::MissingKey { file, key } => write!(f, "{file} has no {key}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// Reads an Omarchy theme from its files' text, keyed by file name.
pub fn read(files: &BTreeMap<String, String>) -> Result<Palette, ReadError> {
    for (name, text) in files {
        let Some(&file) = FILES.iter().find(|file| **file == name.as_str()) else {
            return Err(ReadError::FileNotAllowed { file: name.clone() });
        };
        if text.len() > MAX_FILE_BYTES {
            return Err(ReadError::FileTooLarge {
                file,
                bytes: text.len(),
                limit: MAX_FILE_BYTES,
            });
        }
    }
    let light_mode = files.contains_key(LIGHT_MODE);
    if let Some(text) = files.get(COLORS_TOML) {
        let file = read_colors(text)?;
        resolve(file, light_mode)
    } else if let Some(text) = files.get(ALACRITTY_TOML) {
        resolve(colors_from_alacritty(text)?, light_mode)
    } else {
        Err(ReadError::NoPalette)
    }
}

/// A theme's name from its folder's slug, as `omarchy-theme-list` writes it: a letter at the start
/// or after a hyphen upper-cased, and each hyphen a space. A cloned repository's `omarchy-` prefix
/// and `-theme` suffix are removed first, as `omarchy-theme-install` names its folder. `None` when
/// nothing is left.
pub fn theme_name(slug: &str) -> Option<String> {
    let slug = slug.strip_prefix("omarchy-").unwrap_or(slug);
    let slug = slug.strip_suffix("-theme").unwrap_or(slug);
    if slug.is_empty() {
        return None;
    }
    let mut name = String::with_capacity(slug.len());
    let mut word_start = true;
    for c in slug.chars() {
        match c {
            '-' => name.push(' '),
            c if word_start => name.push(c.to_ascii_uppercase()),
            c => name.push(c),
        }
        word_start = c == '-';
    }
    Some(name)
}

/// One of the Omarchy themes Luxforge bundles: Omarchy's own `colors.toml`, copied unmodified
/// from [`OMARCHY_COMMIT`] (`omarchy/bundled/NOTICE.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bundled {
    /// Omarchy's folder name for the theme.
    pub slug: &'static str,
    /// [`theme_name`] of the slug.
    pub name: &'static str,
    pub colors_toml: &'static str,
}

impl Bundled {
    /// `omarchy.<slug>`.
    pub fn id(&self) -> String {
        format!("omarchy.{}", self.slug)
    }

    /// The palette, through the same reader as an import.
    pub fn read(&self) -> Result<Palette, ReadError> {
        read(&BTreeMap::from([(
            COLORS_TOML.to_owned(),
            self.colors_toml.to_owned(),
        )]))
    }
}

/// The bundled themes, in the order they are listed.
pub const BUNDLED: [Bundled; 6] = [
    Bundled {
        slug: "tokyo-night",
        name: "Tokyo Night",
        colors_toml: include_str!("omarchy/bundled/tokyo-night/colors.toml"),
    },
    Bundled {
        slug: "catppuccin",
        name: "Catppuccin",
        colors_toml: include_str!("omarchy/bundled/catppuccin/colors.toml"),
    },
    Bundled {
        slug: "catppuccin-latte",
        name: "Catppuccin Latte",
        colors_toml: include_str!("omarchy/bundled/catppuccin-latte/colors.toml"),
    },
    Bundled {
        slug: "gruvbox",
        name: "Gruvbox",
        colors_toml: include_str!("omarchy/bundled/gruvbox/colors.toml"),
    },
    Bundled {
        slug: "nord",
        name: "Nord",
        colors_toml: include_str!("omarchy/bundled/nord/colors.toml"),
    },
    Bundled {
        slug: "everforest",
        name: "Everforest",
        colors_toml: include_str!("omarchy/bundled/everforest/colors.toml"),
    },
];

/// One key and value of a theme's file, in the file's order.
struct Entry {
    key: String,
    value: String,
    kind: EntryKind,
}

enum EntryKind {
    Colour(&'static str, Rgb8),
    /// `mode` or `theme_type`.
    Mode(&'static str, Mode),
    Unused(UnusedReason),
}

/// A palette as the resolver receives it: the colours and mode keys a `colors.toml` holds, or the
/// ones Omarchy writes for an `alacritty.toml`.
struct PaletteFile {
    file: &'static str,
    form: Form,
    entries: Vec<Entry>,
}

fn parse<'a>(file: &'static str, text: &'a str) -> Result<Document<&'a str>, ReadError> {
    Document::parse(text).map_err(|error| {
        let start = error.span().map_or(0, |span| span.start);
        let before = &text[..start.min(text.len())];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        ReadError::MalformedToml {
            file,
            line,
            column,
            message: error.message().to_owned(),
        }
    })
}

/// A value as its file writes it, for a report.
fn raw(text: &str, span: Option<Range<usize>>) -> String {
    span.and_then(|span| text.get(span))
        .map_or_else(String::new, |raw| raw.trim().to_owned())
}

fn value_text(text: &str, value: &Value) -> String {
    match value {
        Value::String(string) => string.value().clone(),
        value => raw(text, value.span()),
    }
}

/// Every leaf under `item`, dotted from the root, as an unused entry.
fn unused_leaves(text: &str, key: String, item: &Item, reason: UnusedReason, out: &mut Vec<Entry>) {
    match item {
        Item::None => {}
        Item::Value(value) => out.push(Entry {
            key,
            value: value_text(text, value),
            kind: EntryKind::Unused(reason),
        }),
        Item::Table(table) => {
            for (name, item) in table.iter() {
                unused_leaves(text, format!("{key}.{name}"), item, reason, out);
            }
        }
        Item::ArrayOfTables(tables) => {
            for (i, table) in tables.iter().enumerate() {
                for (name, item) in table.iter() {
                    unused_leaves(text, format!("{key}[{i}].{name}"), item, reason, out);
                }
            }
        }
    }
}

/// A Hyprland colour or gradient: `rgb(…)` and `rgba(…)` stops, then an optional angle.
fn is_hyprland_colour(value: &str) -> bool {
    let mut words = value.split_whitespace().peekable();
    let mut stops = 0;
    while let Some(word) = words.next() {
        let stop = ["rgb(", "rgba("]
            .iter()
            .any(|open| word.starts_with(open) && word.ends_with(')'));
        let angle = words.peek().is_none()
            && stops > 0
            && word
                .strip_suffix("deg")
                .is_some_and(|n| n.parse::<f64>().is_ok());
        if !stop && !angle {
            return false;
        }
        stops += usize::from(stop);
    }
    stops > 0
}

fn read_colors(text: &str) -> Result<PaletteFile, ReadError> {
    let file = COLORS_TOML;
    let document = parse(file, text)?;
    let mut entries = Vec::new();
    for (key, item) in document.iter() {
        let Item::Value(value) = item else {
            unused_leaves(
                text,
                key.to_owned(),
                item,
                UnusedReason::UnknownKey,
                &mut entries,
            );
            continue;
        };
        let string = value.as_str();
        let shown = value_text(text, value);
        let kind = if string == Some("") {
            EntryKind::Unused(UnusedReason::Empty)
        } else if let Some(&name) = KEYS.iter().find(|name| **name == key) {
            let colour = string.and_then(Rgb8::parse_hex).ok_or(ReadError::NotHex {
                file,
                key: key.to_owned(),
                value: shown.clone(),
            })?;
            EntryKind::Colour(name, colour)
        } else if let Some(name) = ["mode", "theme_type"].into_iter().find(|name| *name == key) {
            let mode = match string {
                Some("light") => Mode::Light,
                Some("dark") => Mode::Dark,
                _ => {
                    return Err(ReadError::InvalidMode {
                        file,
                        key: name,
                        value: shown,
                    });
                }
            };
            EntryKind::Mode(name, mode)
        } else if string.is_some_and(is_hyprland_colour) {
            EntryKind::Unused(UnusedReason::Gradient)
        } else {
            EntryKind::Unused(UnusedReason::UnknownKey)
        };
        entries.push(Entry {
            key: key.to_owned(),
            value: shown,
            kind,
        });
    }
    let mut has_ansi = false;
    let mut omarchy3 = true;
    for entry in &entries {
        match entry.kind {
            EntryKind::Colour(name, _) => {
                has_ansi |= name.starts_with("color");
                omarchy3 &= OMARCHY3_KEYS.contains(&name);
            }
            EntryKind::Mode("mode", _) => omarchy3 = false,
            _ => {}
        }
    }
    let form = if has_ansi && omarchy3 {
        Form::Omarchy3
    } else {
        Form::Omarchy4
    };
    Ok(PaletteFile {
        file,
        form,
        entries,
    })
}

/// A colour as `omarchy-theme-colors-from-alacritty` accepts one: six hex digits after an optional
/// `0x` or `#`, quoted, or bare with no `#`. A bare value is a TOML integer, so its text is read.
fn alacritty_colour(text: &str, value: &Value) -> Option<Rgb8> {
    let without_0x = |s: &str| {
        let digits = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
        Rgb8::parse_digits(digits.unwrap_or(s))
    };
    match value {
        Value::String(string) => {
            let string = string.value();
            string
                .strip_prefix('#')
                .map_or_else(|| without_0x(string), Rgb8::parse_digits)
        }
        Value::Integer(_) => without_0x(&raw(text, value.span())),
        _ => None,
    }
}

/// The palette `omarchy-theme-colors-from-alacritty` writes for a theme with only
/// `alacritty.toml`: its eight normal colours, required; its bright ones, each falling back to the
/// normal one; its primary background and foreground, falling back to normal black and white; its
/// selection background, falling back to the foreground; and its normal blue as the accent.
///
/// Omarchy reads `key = value` lines under a `[colors.<group>]` header, or dotted
/// (`normal.black = …`) under `[colors]`, as this does; it does not read inline tables. Every other
/// key under `colors` is listed as unused; the rest of the file is the terminal's configuration and
/// is not.
fn colors_from_alacritty(text: &str) -> Result<PaletteFile, ReadError> {
    let file = ALACRITTY_TOML;
    let document = parse(file, text)?;
    let mut read = BTreeMap::<String, Rgb8>::new();
    let mut entries = Vec::new();
    let wanted = |group: &str, name: &str| match group {
        "normal" | "bright" => ALACRITTY_NAMES.contains(&name),
        "primary" => ["background", "foreground"].contains(&name),
        "selection" => name == "background",
        _ => false,
    };
    match document.as_table().get("colors") {
        // A `colors` table dotted from the root sits under no header, where Omarchy reads nothing.
        Some(Item::Table(colors)) if !colors.is_dotted() => {
            for (group, item) in colors.iter() {
                let key = format!("colors.{group}");
                match item {
                    Item::Table(table) => {
                        read_alacritty_group(text, &key, table, &wanted, &mut read, &mut entries)
                    }
                    item => unused_leaves(text, key, item, UnusedReason::UnknownKey, &mut entries),
                }
            }
        }
        Some(item) => unused_leaves(
            text,
            "colors".to_owned(),
            item,
            UnusedReason::UnknownKey,
            &mut entries,
        ),
        None => {}
    }
    let colour = |key: &str| read.get(key).copied();
    let mut ansi = [None; 16];
    for (i, name) in ALACRITTY_NAMES.into_iter().enumerate() {
        let key = format!("colors.normal.{name}");
        ansi[i] = Some(colour(&key).ok_or(ReadError::MissingKey { file, key })?);
    }
    for (i, name) in ALACRITTY_NAMES.into_iter().enumerate() {
        ansi[i + 8] = colour(&format!("colors.bright.{name}")).or(ansi[i]);
    }
    let background = colour("colors.primary.background").or(ansi[0]);
    let foreground = colour("colors.primary.foreground").or(ansi[7]);
    ansi[0] = background;
    ansi[7] = foreground;
    let selection = colour("colors.selection.background").or(foreground);
    let written = [
        ("accent", ansi[4]),
        ("selection", selection),
        ("background", background),
        ("foreground", foreground),
    ];
    let ansi_keys = ANSI_SEMANTIC.map(|(key, _)| key);
    for (name, colour) in written.into_iter().chain(ansi_keys.into_iter().zip(ansi)) {
        let colour = colour.expect("every normal colour was read");
        entries.push(Entry {
            key: name.to_owned(),
            value: colour.to_string(),
            kind: EntryKind::Colour(name, colour),
        });
    }
    Ok(PaletteFile {
        file,
        form: Form::Alacritty,
        entries,
    })
}

fn read_alacritty_group(
    text: &str,
    prefix: &str,
    table: &Table,
    wanted: &dyn Fn(&str, &str) -> bool,
    read: &mut BTreeMap<String, Rgb8>,
    entries: &mut Vec<Entry>,
) {
    let group = &prefix["colors.".len()..];
    for (name, item) in table.iter() {
        let key = format!("{prefix}.{name}");
        match item {
            Item::Value(value) if wanted(group, name) => match alacritty_colour(text, value) {
                Some(colour) => {
                    read.insert(key, colour);
                }
                None => entries.push(Entry {
                    key,
                    value: value_text(text, value),
                    kind: EntryKind::Unused(UnusedReason::NotAColour),
                }),
            },
            item => unused_leaves(text, key, item, UnusedReason::UnknownKey, entries),
        }
    }
}

/// The resolver's colours. A key is set when the map holds it, as a Bash value is set when it is
/// not empty.
struct Colours(BTreeMap<&'static str, Rgb8>);

impl Colours {
    fn get(&self, key: &str) -> Option<Rgb8> {
        self.0.get(key).copied()
    }

    /// `[[ ${key} ]] || key="${a:-${b:-…}}"`, and with one fallback `alias_theme_color key a`.
    fn fill(&mut self, key: &'static str, fallbacks: &[&str]) {
        if !self.0.contains_key(key)
            && let Some(colour) = fallbacks.iter().find_map(|fallback| self.get(fallback))
        {
            self.0.insert(key, colour);
        }
    }

    /// `[[ ${key} ]] || key=$(mix_color "${from}" with percent%)`. A colour the theme does not have
    /// is not mixed, where Omarchy's awk would read the empty string as a colour of its own.
    fn fill_mix(&mut self, key: &'static str, from: &str, with: Rgb8, percent: f64) {
        if !self.0.contains_key(key)
            && let Some(colour) = self.get(from)
        {
            self.0.insert(key, colour.mix(with, percent / 100.0));
        }
    }

    /// `key="${from}"` when `from` is set.
    fn copy(&mut self, key: &'static str, from: &str) {
        if let Some(colour) = self.get(from) {
            self.0.insert(key, colour);
        }
    }
}

/// `resolve_theme_colors` and `resolve_theme_mode`, statement by statement in their order.
fn resolve(file: PaletteFile, light_mode: bool) -> Result<Palette, ReadError> {
    let mut t = Colours(BTreeMap::new());
    let (mut mode_key, mut theme_type) = (None, None);
    for entry in &file.entries {
        match entry.kind {
            EntryKind::Colour(name, colour) => {
                t.0.insert(name, colour);
            }
            EntryKind::Mode("mode", mode) => mode_key = Some(mode),
            EntryKind::Mode(_, mode) => theme_type = Some(mode),
            EntryKind::Unused(_) => {}
        }
    }

    // The complete short-name palette, before the ANSI fallbacks; a canonical name wins.
    for (key, short) in SHORT_NAMES {
        t.fill(key, &[short]);
    }
    // Themes from before the semantic palette may only name ANSI slots.
    t.fill("background", &["color0"]);
    t.fill("foreground", &["color7"]);
    t.copy("color0", "background");
    t.copy("color7", "foreground");
    for (key, ansi) in NAMED_ANSI {
        t.fill(key, &[ansi]);
    }
    t.fill("magenta", &["purple"]);
    t.fill("bright_magenta", &["bright_purple"]);

    t.fill("light_foreground", &["color7", "foreground"]);
    t.fill("bright_foreground", &["color15", "foreground"]);
    // The cursor is always the bright foreground, whatever the theme says.
    t.0.remove("cursor");
    t.copy("cursor", "bright_foreground");
    t.fill("lighter_background", &["color0", "background"]);
    t.fill("dark_foreground", &["color8", "foreground"]);
    t.fill("muted", &["color8", "dark_foreground"]);
    let selection = ["selection_background", "color8", "color0", "background"];
    t.fill("selection", &selection);
    t.fill("selection_background", &["selection"]);
    t.fill("selection_foreground", &["bright_foreground"]);
    t.fill("orange", &["yellow"]);
    t.fill_mix("brown", "orange", BLACK, 50.0);

    // Shades derived from the base colours when the theme gives none.
    t.fill_mix("dark_background", "background", BLACK, 25.0);
    t.fill_mix("darker_background", "background", BLACK, 50.0);
    for (bright, base) in [
        ("bright_red", "red"),
        ("bright_yellow", "yellow"),
        ("bright_green", "green"),
        ("bright_cyan", "cyan"),
        ("bright_blue", "blue"),
        ("bright_magenta", "magenta"),
    ] {
        t.fill_mix(bright, base, WHITE, 20.0);
    }
    t.fill("purple", &["magenta"]);
    t.fill("bright_purple", &["bright_magenta"]);

    // The ANSI names, for consumers that still read them.
    for (ansi, key) in ANSI_SEMANTIC {
        t.fill(ansi, &[key]);
    }
    // And the short names, from their canonical names.
    for (key, short) in SHORT_NAMES {
        t.copy(short, key);
    }
    let Colours(t) = t;

    let required = |key: &str| {
        t.get(key).copied().ok_or(ReadError::MissingKey {
            file: COLORS_TOML,
            key: key.to_owned(),
        })
    };
    let background = required("background")?;
    let foreground = required("foreground")?;
    let accent = required("accent")?;

    let (mode, mode_source) = if let Some(mode) = mode_key {
        (mode, ModeSource::ModeKey)
    } else if let Some(mode) = theme_type {
        (mode, ModeSource::ThemeTypeKey)
    } else if light_mode {
        (Mode::Light, ModeSource::LightModeFile)
    } else {
        let sum: u32 = background.0.iter().map(|&c| u32::from(c)).sum();
        let mode = if sum > 382 { Mode::Light } else { Mode::Dark };
        (mode, ModeSource::BackgroundBrightness)
    };

    let unused = file
        .entries
        .into_iter()
        .filter_map(|entry| {
            let reason = match entry.kind {
                EntryKind::Unused(reason) => reason,
                // Omarchy writes an `alacritty.toml` theme's palette itself.
                _ if file.form == Form::Alacritty => return None,
                EntryKind::Colour(name, colour) if t.get(name) != Some(&colour) => {
                    UnusedReason::Replaced
                }
                EntryKind::Mode("theme_type", theme_type) if theme_type != mode => {
                    UnusedReason::Replaced
                }
                _ => return None,
            };
            Some(Unused {
                file: file.file,
                key: entry.key,
                value: entry.value,
                reason,
            })
        })
        .collect();

    Ok(Palette {
        form: file.form,
        mode,
        mode_source,
        background,
        dark_background: t["dark_background"],
        lighter_background: t["lighter_background"],
        foreground,
        accent,
        colours: t,
        unused,
    })
}

#[cfg(test)]
pub(crate) mod tests;
