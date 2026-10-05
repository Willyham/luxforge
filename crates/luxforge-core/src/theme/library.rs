//! The theme library: the themes a person imported, kept in `themes.json` beside
//! `preferences.json` and outside every catalog, and the built-in themes listed beside them
//! (`docs/design/ui-themes.md`, Library and API).
//!
//! The document is `{format: 1, themes: [record, ...]}` through the bounded, format-marked, locked
//! and atomic [`JsonDocument`]: at most [`MAX_THEMES`] records of at most [`MAX_THEME_BYTES`]
//! each, the imported files' text included, in at most [`MAX_LIBRARY_BYTES`]. Each record is
//! kept as the JSON value it is and read on its own, so a record this build cannot read is listed
//! in [`Listing::unrecognized`] with its reason, never makes the others unreadable, and is written
//! back as it was by every later change. Built-in themes are never stored: Luxforge Dark and the
//! six bundled Omarchy themes ([`built_in_themes`]).
//!
//! An Omarchy theme folder's files are read by [`omarchy::read`], and the palette it resolves
//! becomes roles by [`omarchy_roles`]; the bundled themes are read the same way.
//!
//! Everything here is small text work for the catalog owner: one bounded read, or one locked
//! read-modify-write, per method. Nothing reads an asset, a recipe or a catalog.
use super::{
    DerivedReason, LUXFORGE_DARK_NAME, Mode, OmarchyDerived, OmarchyReport, OmarchyRole,
    OmarchyUnused, Resolution, ResolvedTheme, Rgba, Roles, ThemeDocument, Token, Tokens,
    luxforge_dark, omarchy, resolve,
};
use crate::{
    Error, MutationOutcome, capabilities::document::JsonDocument, editor::now_ms,
    presets::safe_file_name,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::LazyLock};

/// The id of Luxforge Dark, the default theme. Bundled themes take `omarchy.<slug>`, and a stored
/// theme `theme-<32 hex digits>`.
pub const LUXFORGE_DARK_ID: &str = "luxforge.dark";
/// The most themes the library stores; importing one more is a `resource-limit` error.
pub const MAX_THEMES: usize = 128;
/// The most bytes one stored record takes as compact JSON, the imported files' text included.
pub const MAX_THEME_BYTES: usize = 8 * 1024;
/// The most bytes `themes.json` takes.
pub const MAX_LIBRARY_BYTES: u64 = 1024 * 1024;
/// The most bytes of one file an import reads.
pub const MAX_THEME_FILE_BYTES: usize = 64 * 1024;
/// The longest theme name, in characters.
pub const MAX_THEME_NAME: usize = 64;
/// The longest theme id a request names.
pub const MAX_THEME_ID: usize = 96;
/// The longest Omarchy theme folder name a request names, in bytes.
pub const MAX_THEME_FOLDER: usize = 128;

/// The format marker of `themes.json`.
const LIBRARY_FORMAT: u32 = 1;
/// The prefix of a stored theme's id.
const STORED_PREFIX: &str = "theme-";
/// The longest actor, in bytes, as for presets and mutations.
const MAX_ACTOR: usize = 128;
/// The extension of an exported Luxforge theme document.
const DOCUMENT_EXTENSION: &str = "lftheme";

/// Where a theme came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ThemeOrigin {
    /// Luxforge Dark. An empty struct rather than a unit variant, because serde ignores unknown
    /// keys after a unit variant's tag and this shape refuses them like the others.
    BuiltIn {},
    /// Read from a Luxforge theme document.
    Luxforge {},
    /// Read from an Omarchy theme folder: its slug, empty when the import named none, and which
    /// palette form it held (`omarchy4`, `omarchy3` or `alacritty`). A bundled theme also names
    /// the Omarchy commit its file came from.
    Omarchy {
        folder: String,
        form: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
    },
}

impl ThemeOrigin {
    /// The rules the theme resolves by: an Omarchy theme's own inks are moved to their floors and
    /// reported; every other theme's author chose each value.
    pub fn resolution(&self) -> Resolution {
        match self {
            Self::Omarchy { .. } => Resolution::Import,
            Self::BuiltIn {} | Self::Luxforge {} => Resolution::Document,
        }
    }
}

/// The formats a theme is imported from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeFormat {
    /// A Luxforge theme document, sent as `content`.
    Luxforge,
    /// An Omarchy theme folder's files, sent as `files`.
    Omarchy,
}

impl ThemeFormat {
    pub const ALL: [Self; 2] = [Self::Luxforge, Self::Omarchy];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Luxforge => "luxforge",
            Self::Omarchy => "omarchy",
        }
    }
}

/// One theme as the library answers it, built in or stored, with every token resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// `None` only for what `theme.inspect` answers, which is not stored.
    pub id: Option<String>,
    pub name: String,
    /// The roles as the theme gave them, before any ink was moved to its floor.
    pub roles: Roles,
    /// The tokens past the roles the theme sets explicitly.
    pub tokens: Tokens,
    pub origin: ThemeOrigin,
    pub built_in: bool,
    /// Every token, the mode and the report, resolved by this build's rules.
    pub resolved: ResolvedTheme,
    /// The imported files' text, verbatim, by file name.
    pub source: BTreeMap<String, String>,
    /// Who imported it, and when; `None` for a built-in theme and for an inspection.
    pub actor: Option<String>,
    pub created_ms: Option<i64>,
    pub updated_ms: Option<i64>,
}

impl Theme {
    pub fn id(&self) -> &str {
        self.id.as_deref().unwrap_or_default()
    }

    /// A `theme.list` row: no roles, tokens or source, five swatches and the report's counts.
    pub fn summary(&self) -> Value {
        let report = &self.resolved.report;
        let tokens = &self.resolved.tokens;
        let neutralised = [&report.surround, &report.rail_backdrop]
            .into_iter()
            .filter(|neutralised| neutralised.before != neutralised.after)
            .count();
        json!({
            "id": self.id,
            "name": self.name,
            "mode": self.resolved.mode,
            "origin": self.origin,
            "built_in": self.built_in,
            "swatches": {
                "surround": tokens[Token::Surround],
                "background": tokens[Token::Background],
                "surface": tokens[Token::Surface],
                "text": tokens[Token::Text],
                "accent": tokens[Token::Accent],
            },
            "report": {
                "derived": report.derived.len(),
                "explicit": report.explicit.len(),
                "moved": report.moved.len(),
                "shortened": report.shortened.len(),
                "neutralised": neutralised,
                "accent_close": report.accent.close,
                "unused": report.omarchy.as_ref().map_or(0, |omarchy| omarchy.unused.len()),
            },
            "adjusted": report.adjusted(),
        })
    }

    /// The record with every resolved token and the full report; with the imported files' text
    /// only for `theme.read`.
    pub fn record(&self, with_source: bool) -> Value {
        let mut record = json!({
            "id": self.id,
            "name": self.name,
            "mode": self.resolved.mode,
            "roles": self.roles,
            "tokens": self.tokens,
            "origin": self.origin,
            "built_in": self.built_in,
            "resolved": self.resolved.tokens,
            "report": self.resolved.report,
            "adjusted": self.resolved.report.adjusted(),
            "actor": self.actor,
            "created_ms": self.created_ms,
            "updated_ms": self.updated_ms,
        });
        if with_source {
            record["source"] = json!(self.source);
        }
        record
    }

    /// The theme as a Luxforge theme document, which imports back to the same tokens. A theme its
    /// author chose every value of is written as it was given. An imported theme's inks may have
    /// been moved to their floors, which a document may not need, so it is written as it resolved:
    /// every role, its mode, and each explicit token's value.
    pub fn document(&self) -> ThemeDocument {
        if self.origin.resolution() == Resolution::Document {
            return ThemeDocument {
                name: self.name.clone(),
                roles: self.roles.clone(),
                tokens: self.tokens.clone(),
            };
        }
        let resolved = &self.resolved.tokens;
        let role = |token| Some(resolved[token]);
        ThemeDocument {
            name: self.name.clone(),
            roles: Roles {
                background: resolved[Token::Background],
                surround: role(Token::Surround),
                surface: role(Token::Surface),
                control: role(Token::Control),
                text: resolved[Token::Text],
                text_secondary: role(Token::TextSecondary),
                text_tertiary: role(Token::TextTertiary),
                accent: resolved[Token::Accent],
                accent_ink: role(Token::AccentInk),
                error: role(Token::Error),
                mode: Some(self.resolved.mode),
            },
            tokens: self
                .tokens
                .keys()
                .map(|token| (*token, resolved[*token]))
                .collect(),
        }
    }
}

/// A stored record this build cannot read, as `theme.list` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Unrecognized {
    /// The record's `id` and `name` where it holds them as strings.
    pub id: Option<String>,
    pub name: Option<String>,
    pub reason: String,
}

/// Every theme the library holds: the built-in themes, Luxforge Dark first, then the stored ones
/// by name ignoring case; and the stored records this build cannot read, in stored order.
#[derive(Clone, Debug)]
pub struct Listing {
    pub themes: Vec<Theme>,
    pub unrecognized: Vec<Unrecognized>,
}

/// A Luxforge theme document ready to save, as `theme.export` returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ThemeExport {
    /// `<name>.lftheme`, with characters no common file system allows replaced by `_`.
    pub file_name: String,
    pub content: String,
}

/// What an import is asked to read.
#[derive(Clone, Copy, Debug)]
pub struct ThemeInput<'a> {
    pub format: ThemeFormat,
    /// A Luxforge theme document's text.
    pub content: Option<&'a str>,
    /// An Omarchy theme folder's files, by name.
    pub files: Option<&'a BTreeMap<String, String>>,
    /// An Omarchy theme folder's name, its slug, which names the theme unless `name` does.
    pub folder: Option<&'a str>,
    /// Overrides the theme's own name.
    pub name: Option<&'a str>,
}

/// What a format's reader makes of its input, before the library's name rules.
#[derive(Clone, Debug)]
pub(crate) struct Imported {
    pub(crate) name: String,
    pub(crate) roles: Roles,
    pub(crate) tokens: Tokens,
    pub(crate) origin: ThemeOrigin,
    pub(crate) source: BTreeMap<String, String>,
    /// How an Omarchy palette became the roles, for the report.
    pub(crate) omarchy: Option<OmarchyReport>,
}

impl Imported {
    /// The roles and tokens resolved by the origin's rules, with the Omarchy section of the report.
    fn resolve(&self) -> Result<ResolvedTheme, Error> {
        let mut resolved = resolve(&self.roles, &self.tokens, self.origin.resolution())?;
        resolved.report.omarchy = self.omarchy.clone();
        Ok(resolved)
    }
}

/// The themes Luxforge ships: Luxforge Dark, then the bundled Omarchy themes
/// (`docs/design/ui-themes.md#bundled-themes`) in [`omarchy::BUNDLED`]'s order as
/// `omarchy.<slug>`, each read from its vendored `colors.toml` by the same reader and mapping as
/// an import, with an origin naming the pinned commit. They are never stored and never deleted,
/// and their names are taken. Resolved once, on first use.
pub fn built_in_themes() -> &'static [Theme] {
    static BUILT_IN: LazyLock<Vec<Theme>> = LazyLock::new(|| {
        let built_in = |id: String, imported: Imported, resolved| Theme {
            id: Some(id),
            name: imported.name,
            roles: imported.roles,
            tokens: imported.tokens,
            origin: imported.origin,
            built_in: true,
            resolved,
            source: imported.source,
            actor: None,
            created_ms: None,
            updated_ms: None,
        };
        let document = luxforge_dark();
        let dark = Imported {
            name: document.name,
            roles: document.roles,
            tokens: document.tokens,
            origin: ThemeOrigin::BuiltIn {},
            source: BTreeMap::new(),
            omarchy: None,
        };
        let resolved = dark.resolve().expect("Luxforge Dark meets every rule");
        let mut themes = vec![built_in(LUXFORGE_DARK_ID.to_owned(), dark, resolved)];
        for bundled in omarchy::BUNDLED {
            let files = BTreeMap::from([(
                omarchy::COLORS_TOML.to_owned(),
                bundled.colors_toml.to_owned(),
            )]);
            let mut imported = read_omarchy(&files, Some(bundled.slug), None)
                .unwrap_or_else(|error| panic!("{} reads: {error}", bundled.name));
            if let ThemeOrigin::Omarchy { commit, .. } = &mut imported.origin {
                *commit = Some(omarchy::OMARCHY_COMMIT.to_owned());
            }
            let resolved = imported
                .resolve()
                .unwrap_or_else(|error| panic!("{} resolves: {error}", bundled.name));
            themes.push(built_in(bundled.id(), imported, resolved));
        }
        themes
    });
    &BUILT_IN
}

/// Luxforge Dark, the default theme and the fallback for one that cannot be shown.
pub fn luxforge_dark_theme() -> &'static Theme {
    &built_in_themes()[0]
}

/// Read an import's input by its format, refusing a file past [`MAX_THEME_FILE_BYTES`] by name.
pub(crate) fn read_input(input: ThemeInput<'_>) -> Result<Imported, Error> {
    let format = input.format.as_str();
    if let Some(content) = input.content
        && content.len() > MAX_THEME_FILE_BYTES
    {
        return Err(Error::resource_limit(format!(
            "content is {} bytes; a theme file is at most {MAX_THEME_FILE_BYTES}",
            content.len()
        )));
    }
    for (name, text) in input.files.into_iter().flatten() {
        if text.len() > MAX_THEME_FILE_BYTES {
            return Err(Error::resource_limit(format!(
                "file {name:?} is {} bytes; a theme file is at most {MAX_THEME_FILE_BYTES}",
                text.len()
            )));
        }
    }
    match (input.format, input.content, input.files) {
        (ThemeFormat::Luxforge, _, _) if input.folder.is_some() => Err(Error::validation(format!(
            "format {format} takes no folder; folder names an Omarchy theme folder"
        ))),
        (ThemeFormat::Luxforge, Some(content), None) => read_luxforge(content),
        (ThemeFormat::Omarchy, None, Some(files)) => read_omarchy(files, input.folder, input.name),
        (ThemeFormat::Luxforge, _, _) => Err(Error::validation(format!(
            "format {format} takes the document's text as content, and no files"
        ))),
        (ThemeFormat::Omarchy, _, _) => Err(Error::validation(format!(
            "format {format} takes the theme folder's files as files, and no content"
        ))),
    }
}

/// A Luxforge theme document, resolved by a document's rules, whose text is kept under its export
/// file name.
fn read_luxforge(content: &str) -> Result<Imported, Error> {
    let (document, _) = ThemeDocument::read(content)?;
    let file_name = safe_file_name(&document.name, "theme", DOCUMENT_EXTENSION);
    Ok(Imported {
        name: document.name,
        roles: document.roles,
        tokens: document.tokens,
        origin: ThemeOrigin::Luxforge {},
        source: BTreeMap::from([(file_name, content.to_owned())]),
        omarchy: None,
    })
}

/// An Omarchy theme folder's files: `colors.toml`, `alacritty.toml` and `light.mode`, read and
/// resolved as Omarchy resolves them, mapped to roles by [`omarchy_roles`] and resolved by
/// [`Resolution::Import`]. Any other file name is refused by name, as is a theme the reader cannot
/// read; the refusal's data is the reader's coded error. The theme is named `name`, or else after
/// `folder` as Omarchy lists it; with neither it is refused. `source` keeps the text of each file
/// read: `alacritty.toml` beside a `colors.toml` is not read, so not kept.
fn read_omarchy(
    files: &BTreeMap<String, String>,
    folder: Option<&str>,
    name: Option<&str>,
) -> Result<Imported, Error> {
    if let Some(folder) = folder
        && (folder.is_empty()
            || folder.len() > MAX_THEME_FOLDER
            || matches!(folder, "." | "..")
            || folder
                .chars()
                .any(|c| c == '/' || c == '\\' || c.is_control()))
    {
        return Err(Error::validation(format!(
            "folder {folder:?} is not a theme folder's name; folder is the folder's own name, of \
             1..={MAX_THEME_FOLDER} bytes, not a path"
        )));
    }
    let palette = omarchy::read(files).map_err(|error| {
        let data = serde_json::to_value(&error).unwrap_or_default();
        let detail = error.to_string();
        match error {
            omarchy::ReadError::FileNotAllowed { .. } => Error::validation(detail),
            omarchy::ReadError::FileTooLarge { .. } => Error::resource_limit(detail),
            _ => Error::unsupported_input(detail),
        }
        .with_data(data)
    })?;
    let name = name
        .map(str::to_owned)
        .or_else(|| folder.and_then(omarchy::theme_name))
        .ok_or_else(|| {
            Error::validation(match folder {
                Some(folder) => {
                    format!("the folder {folder:?} gives the theme no name; give the theme a name")
                }
                None => "format omarchy takes the theme folder's name as folder, or a name for \
                         the theme"
                    .to_owned(),
            })
        })?;
    let (roles, report) = omarchy_roles(&palette);
    let read = match palette.form {
        omarchy::Form::Alacritty => omarchy::ALACRITTY_TOML,
        omarchy::Form::Omarchy4 | omarchy::Form::Omarchy3 => omarchy::COLORS_TOML,
    };
    let source = files
        .iter()
        .filter(|(file, _)| [read, omarchy::LIGHT_MODE].contains(&file.as_str()))
        .map(|(file, text)| (file.clone(), text.clone()))
        .collect();
    Ok(Imported {
        name,
        roles,
        tokens: Tokens::new(),
        origin: ThemeOrigin::Omarchy {
            folder: folder.unwrap_or_default().to_owned(),
            form: palette.form.as_str().to_owned(),
            commit: None,
        },
        source,
        omarchy: Some(report),
    })
}

/// Each role an Omarchy key gives, in the role table's order.
const OMARCHY_ROLES: [(Token, &str); 5] = [
    (Token::Surround, "dark_background"),
    (Token::Background, "background"),
    (Token::Control, "lighter_background"),
    (Token::Text, "foreground"),
    (Token::Accent, "accent"),
];

/// An Omarchy palette's roles (`docs/design/ui-themes.md#what-luxforge-reads`): its mode; its
/// background; `dark_background` as the surround, which [`Resolution::Import`] holds to the chroma
/// bound; `lighter_background` as the control, left to derive when it equals the background;
/// `foreground` as the text; and `accent` as the accent. No other colour is read, because Omarchy's
/// named colours do not always hold the colour they name. Every other role derives.
fn omarchy_roles(palette: &omarchy::Palette) -> (Roles, OmarchyReport) {
    let colour = |omarchy::Rgb8([r, g, b])| Rgba::rgb(r, g, b);
    let mode = match palette.mode {
        omarchy::Mode::Dark => Mode::Dark,
        omarchy::Mode::Light => Mode::Light,
    };
    let mut roles = Roles::new(
        colour(palette.background),
        colour(palette.foreground),
        colour(palette.accent),
    );
    roles.mode = Some(mode);
    roles.surround = Some(colour(palette.dark_background));
    let mut derived = Vec::new();
    let lighter = colour(palette.lighter_background);
    if palette.lighter_background == palette.background {
        derived.push(OmarchyDerived {
            role: Token::Control,
            key: "lighter_background".to_owned(),
            value: lighter,
            reason: DerivedReason::EqualsBackground,
        });
    } else {
        roles.control = Some(lighter);
    }
    let given = OMARCHY_ROLES
        .into_iter()
        .filter_map(|(role, key)| {
            Some(OmarchyRole {
                role,
                key: key.to_owned(),
                value: roles.get(role)?,
            })
        })
        .collect();
    let unused = palette
        .unused
        .iter()
        .map(|unused| OmarchyUnused {
            file: unused.file.to_owned(),
            key: unused.key.clone(),
            value: unused.value.clone(),
            reason: unused.reason,
        })
        .collect();
    let report = OmarchyReport {
        form: palette.form,
        mode,
        mode_source: palette.mode_source,
        roles: given,
        derived,
        unused,
    };
    (roles, report)
}

/// Trimmed text of 1 to [`MAX_THEME_NAME`] characters with no control character.
fn checked_name(name: &str) -> Result<String, Error> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_THEME_NAME
        || name.chars().any(char::is_control)
    {
        return Err(Error::validation(format!(
            "theme name must contain 1..={MAX_THEME_NAME} printable characters"
        )));
    }
    Ok(name.to_owned())
}

fn checked_actor(actor: &str) -> Result<(), Error> {
    if actor.is_empty() || actor.len() > MAX_ACTOR {
        return Err(Error::validation("actor must contain 1..128 characters"));
    }
    Ok(())
}

fn unknown_theme(theme_id: &str) -> Error {
    Error::validation(format!("unknown theme {theme_id}"))
}

/// Whether `id` is a stored theme's: `theme-` and hex digits, as [`new_id`] makes them.
fn stored_id(id: &str) -> bool {
    id.strip_prefix(STORED_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && id.len() <= MAX_THEME_ID
            && rest.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

fn new_id() -> String {
    format!("{STORED_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

/// `themes.json` as written: each record as the JSON value it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Library {
    #[serde(default)]
    themes: Vec<Value>,
}

/// One stored record, as this build writes and reads it.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    id: String,
    name: String,
    mode: Mode,
    roles: Roles,
    tokens: Tokens,
    origin: ThemeOrigin,
    /// The report the import made. The answers give this build's resolution's report, which is
    /// the same until the rules change, with this one's Omarchy section.
    report: super::ThemeReport,
    source: BTreeMap<String, String>,
    actor: String,
    created_ms: i64,
    updated_ms: i64,
}

impl Stored {
    fn of(theme: &Theme) -> Self {
        Self {
            id: theme.id().to_owned(),
            name: theme.name.clone(),
            mode: theme.resolved.mode,
            roles: theme.roles.clone(),
            tokens: theme.tokens.clone(),
            origin: theme.origin.clone(),
            report: theme.resolved.report.clone(),
            source: theme.source.clone(),
            actor: theme.actor.clone().unwrap_or_default(),
            created_ms: theme.created_ms.unwrap_or_default(),
            updated_ms: theme.updated_ms.unwrap_or_default(),
        }
    }

    /// The record as a value, and its size as compact JSON, refused past [`MAX_THEME_BYTES`].
    fn encode(&self) -> Result<Value, Error> {
        let value =
            serde_json::to_value(self).map_err(|error| Error::internal(error.to_string()))?;
        let bytes = compact_len(&value);
        if bytes > MAX_THEME_BYTES {
            return Err(Error::resource_limit(format!(
                "the theme would take {bytes} bytes; a theme is at most {MAX_THEME_BYTES}, the \
                 imported files' text included"
            )));
        }
        Ok(value)
    }
}

fn compact_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

/// One stored record read, or the reason it cannot be.
fn read_record(value: &Value) -> Result<Theme, String> {
    let bytes = compact_len(value);
    if bytes > MAX_THEME_BYTES {
        return Err(format!(
            "the record takes {bytes} bytes; a theme is at most {MAX_THEME_BYTES}"
        ));
    }
    let stored: Stored = serde_json::from_value(value.clone())
        .map_err(|error| format!("not a theme record ({error})"))?;
    if !stored_id(&stored.id) {
        return Err(format!("{:?} is not a stored theme's id", stored.id));
    }
    if stored.origin == (ThemeOrigin::BuiltIn {}) {
        return Err("a stored theme cannot be built in".into());
    }
    match checked_name(&stored.name) {
        Ok(name) if name == stored.name => {}
        _ => {
            return Err(format!(
                "the name {:?} is not 1..={MAX_THEME_NAME} printable characters, trimmed",
                stored.name
            ));
        }
    }
    let mut resolved = resolve(&stored.roles, &stored.tokens, stored.origin.resolution())
        .map_err(|error| format!("it does not resolve: {}", error.detail))?;
    // What the import read of an Omarchy folder, which resolving the roles cannot say again.
    resolved.report.omarchy = stored.report.omarchy;
    Ok(Theme {
        id: Some(stored.id),
        name: stored.name,
        roles: stored.roles,
        tokens: stored.tokens,
        origin: stored.origin,
        built_in: false,
        resolved,
        source: stored.source,
        actor: Some(stored.actor),
        created_ms: Some(stored.created_ms),
        updated_ms: Some(stored.updated_ms),
    })
}

/// A field of a record that may not be readable, where it is a string.
fn text_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

impl Library {
    /// Every theme, and every record this build cannot read. A record whose id or name another
    /// theme already holds, ignoring case for the name, cannot be read either.
    fn listing(&self) -> Listing {
        let mut themes: Vec<Theme> = built_in_themes().to_vec();
        let mut stored = Vec::new();
        let mut unrecognized = Vec::new();
        for value in &self.themes {
            let read = read_record(value).and_then(|theme| {
                let taken = |other: &Theme| {
                    other.id == theme.id || other.name.to_lowercase() == theme.name.to_lowercase()
                };
                if themes.iter().chain(&stored).any(taken) {
                    Err(format!(
                        "another theme already holds its id {} or its name {:?}",
                        theme.id(),
                        theme.name
                    ))
                } else {
                    Ok(theme)
                }
            });
            match read {
                Ok(theme) => stored.push(theme),
                Err(reason) => unrecognized.push(Unrecognized {
                    id: text_field(value, "id"),
                    name: text_field(value, "name"),
                    reason,
                }),
            }
        }
        stored.sort_by_cached_key(|theme| theme.name.to_lowercase());
        themes.extend(stored);
        Listing {
            themes,
            unrecognized,
        }
    }

    /// Refuse a name any theme holds, ignoring case across Unicode: a built-in theme's, a stored
    /// one's, or one an unreadable record carries.
    fn ensure_unique(&self, listing: &Listing, name: &str) -> Result<(), Error> {
        let key = name.to_lowercase();
        let holder = listing
            .themes
            .iter()
            .map(|theme| theme.name.clone())
            .chain(
                self.themes
                    .iter()
                    .filter_map(|value| text_field(value, "name")),
            )
            .find(|held| held.to_lowercase() == key);
        match holder {
            Some(held) => Err(Error::conflict(format!(
                "a theme named {held:?} already exists; choose another name"
            ))),
            None => Ok(()),
        }
    }
}

impl Listing {
    /// The theme `theme_id` names. A stored record this build cannot read is `incompatible` with
    /// its reason; an id nothing holds is refused as unknown.
    pub fn theme(&self, theme_id: &str) -> Result<&Theme, Error> {
        if let Some(theme) = self.themes.iter().find(|theme| theme.id() == theme_id) {
            return Ok(theme);
        }
        match self
            .unrecognized
            .iter()
            .find(|record| record.id.as_deref() == Some(theme_id))
        {
            Some(record) => Err(Error::incompatible(format!(
                "theme {theme_id} cannot be read: {}; it is kept unchanged",
                record.reason
            ))),
            None => Err(unknown_theme(theme_id)),
        }
    }
}

/// The library over one configuration directory. It holds no state of its own: every call reads
/// `themes.json` again, so two processes never act on a stale copy.
pub(crate) struct ThemeStore(Option<JsonDocument<Library>>);

impl ThemeStore {
    /// `<dir>/themes.json`, which nothing creates until the first import. With no directory only
    /// the built-in themes are held, and an import is `not-ready`.
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self(
            dir.map(|dir| JsonDocument::new(dir, "themes.json", MAX_LIBRARY_BYTES, LIBRARY_FORMAT)),
        )
    }

    fn document(&self) -> Result<&JsonDocument<Library>, Error> {
        self.0
            .as_ref()
            .ok_or_else(|| Error::not_ready("no user preference directory is configured"))
    }

    /// Every theme the library holds, read now.
    pub(crate) fn list(&self) -> Result<Listing, Error> {
        let library = match &self.0 {
            Some(document) => document.read()?.unwrap_or_default(),
            None => Library::default(),
        };
        Ok(library.listing())
    }

    /// One theme, built in or stored.
    pub(crate) fn theme(&self, theme_id: &str) -> Result<Theme, Error> {
        if let Some(theme) = built_in_themes()
            .iter()
            .find(|theme| theme.id() == theme_id)
        {
            return Ok(theme.clone());
        }
        self.list()?.theme(theme_id).cloned()
    }

    /// `theme.inspect`: the theme an import of `input` would store, with no id, actor or time, and
    /// nothing stored. The name rules apply; whether another theme holds the name is the import's
    /// to say.
    pub(crate) fn inspect(&self, input: ThemeInput<'_>) -> Result<Theme, Error> {
        let imported = read_input(input)?;
        let name = checked_name(input.name.unwrap_or(&imported.name))?;
        let resolved = imported.resolve()?;
        Ok(Theme {
            id: None,
            name,
            roles: imported.roles,
            tokens: imported.tokens,
            origin: imported.origin,
            built_in: false,
            resolved,
            source: imported.source,
            actor: None,
            created_ms: None,
            updated_ms: None,
        })
    }

    /// `theme.import`: store what [`Self::inspect`] answers, with a new id, `actor` and the time.
    /// A name another theme holds is a `conflict`, and a full library or a record past
    /// [`MAX_THEME_BYTES`] a `resource-limit` error; nothing is renamed, replaced or stored then.
    pub(crate) fn import(&self, input: ThemeInput<'_>, actor: &str) -> Result<Theme, Error> {
        checked_actor(actor)?;
        let document = self.document()?;
        let mut theme = self.inspect(input)?;
        let now = now_ms();
        theme.id = Some(new_id());
        theme.actor = Some(actor.to_owned());
        theme.created_ms = Some(now);
        theme.updated_ms = Some(now);
        let record = Stored::of(&theme).encode()?;
        document.transact(|library| {
            if library.themes.len() >= MAX_THEMES {
                return Err(Error::resource_limit(format!(
                    "the theme library holds at most {MAX_THEMES} themes; delete one first"
                )));
            }
            library.ensure_unique(&library.listing(), &theme.name)?;
            library.themes.push(record);
            Ok(())
        })?;
        Ok(theme)
    }

    /// `theme.delete`: remove a stored theme, or a stored record this build cannot read, by id:
    /// `Applied` when it was there and `NoOp` when it was not. A built-in theme and `active`, the
    /// theme the preferences choose, are refused by name.
    pub(crate) fn delete(&self, theme_id: &str, active: &str) -> Result<MutationOutcome, Error> {
        if let Some(theme) = built_in_themes()
            .iter()
            .find(|theme| theme.id() == theme_id)
        {
            return Err(Error::validation(format!(
                "{} is built in and cannot be deleted",
                theme.name
            )));
        }
        if theme_id == active {
            return Err(Error::conflict(format!(
                "theme {theme_id} is the active theme; choose another before deleting it"
            )));
        }
        let Some(document) = &self.0 else {
            return Ok(MutationOutcome::NoOp);
        };
        document.transact(|library| {
            let before = library.themes.len();
            library
                .themes
                .retain(|value| text_field(value, "id").as_deref() != Some(theme_id));
            Ok(if library.themes.len() == before {
                MutationOutcome::NoOp
            } else {
                MutationOutcome::Applied
            })
        })
    }

    /// `theme.export`: the theme as a Luxforge theme document named `<name>.lftheme`.
    pub(crate) fn export(&self, theme_id: &str) -> Result<ThemeExport, Error> {
        let theme = self.theme(theme_id)?;
        Ok(ThemeExport {
            file_name: safe_file_name(&theme.name, "theme", DOCUMENT_EXTENSION),
            content: theme.document().write(),
        })
    }
}

/// The theme the desktop draws from its first frame, read with its launch preferences.
#[derive(Clone, Debug)]
pub struct LaunchTheme {
    /// The theme drawn: the chosen one, or Luxforge Dark when it cannot be shown.
    pub id: String,
    pub name: String,
    pub resolved: ResolvedTheme,
    /// Why the chosen theme is not drawn, naming it: missing, unreadable, or a library that cannot
    /// be read. The stored choice is not changed.
    pub problem: Option<Error>,
}

impl LaunchTheme {
    /// The theme `chosen` names, from `themes.json` under `dir`; Luxforge Dark for `None`.
    pub(crate) fn read(dir: Option<PathBuf>, chosen: Option<&str>) -> Self {
        let fallback = luxforge_dark_theme();
        let chosen = chosen.unwrap_or(LUXFORGE_DARK_ID);
        let (theme, problem) = match ThemeStore::new(dir).theme(chosen) {
            Ok(theme) => (theme, None),
            Err(error) => {
                let problem = Error::new(
                    error.kind,
                    format!(
                        "the theme {chosen} cannot be shown, so {LUXFORGE_DARK_NAME} is: {}",
                        error.detail
                    ),
                );
                (fallback.clone(), Some(problem))
            }
        };
        Self {
            id: theme.id().to_owned(),
            name: theme.name,
            resolved: theme.resolved,
            problem,
        }
    }

    /// Luxforge Dark, with no problem.
    pub fn luxforge_dark() -> Self {
        let theme = luxforge_dark_theme();
        Self {
            id: LUXFORGE_DARK_ID.to_owned(),
            name: theme.name.clone(),
            resolved: theme.resolved.clone(),
            problem: None,
        }
    }
}
