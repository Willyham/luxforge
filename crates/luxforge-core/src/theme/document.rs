//! Luxforge's own theme document, `{"format": "luxforge.theme", "version": 1, "name", "roles",
//! "tokens"?}`: what a theme exports as and imports from. Its author chose each value, so it is
//! resolved by [`Resolution::Document`]'s rules, and an unknown field, a missing required role or
//! a missed floor is refused by name.
use super::{Resolution, ResolvedTheme, Roles, Tokens, resolve};
use crate::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The `format` marker of a Luxforge theme document.
pub const THEME_DOCUMENT_FORMAT: &str = "luxforge.theme";
/// The only theme document version this build reads and writes.
pub const THEME_DOCUMENT_VERSION: u64 = 1;

/// A theme as its document holds it: a name, the roles, and the tokens it sets explicitly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeDocument {
    pub name: String,
    pub roles: Roles,
    #[serde(default, skip_serializing_if = "Tokens::is_empty")]
    pub tokens: Tokens,
}

/// The document as read: the marker and version beside the theme's fields.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    #[allow(dead_code, reason = "checked before the document is read")]
    format: String,
    #[allow(dead_code, reason = "checked before the document is read")]
    version: u64,
    name: String,
    roles: Roles,
    #[serde(default)]
    tokens: Tokens,
}

#[derive(Serialize)]
struct Written<'a> {
    format: &'a str,
    version: u64,
    #[serde(flatten)]
    document: &'a ThemeDocument,
}

impl ThemeDocument {
    /// Read a Luxforge theme document and resolve it. Any JSON that does not name the theme format
    /// is not a theme document, any version other than this build's is refused by number, and
    /// the theme it holds must resolve by a document's rules.
    pub fn read(text: &str) -> Result<(Self, ResolvedTheme), Error> {
        let value: Value = serde_json::from_str(text)
            .map_err(|error| Error::unsupported_input(format!("malformed JSON: {error}")))?;
        if value.get("format").and_then(Value::as_str) != Some(THEME_DOCUMENT_FORMAT) {
            return Err(Error::unsupported_input("not a Luxforge theme document"));
        }
        match value.get("version") {
            Some(version) if version.as_u64() == Some(THEME_DOCUMENT_VERSION) => {}
            Some(version) => {
                return Err(Error::unsupported_input(format!(
                    "Luxforge theme document version {version} is not supported; this build \
                     reads version {THEME_DOCUMENT_VERSION}"
                )));
            }
            None => {
                return Err(Error::unsupported_input(
                    "the Luxforge theme document has no version",
                ));
            }
        }
        let read: Read = serde_json::from_value(value).map_err(|error| {
            Error::unsupported_input(format!("malformed Luxforge theme document: {error}"))
        })?;
        let document = Self {
            name: read.name,
            roles: read.roles,
            tokens: read.tokens,
        };
        let resolved = document.resolve(Resolution::Document)?;
        Ok((document, resolved))
    }

    /// The document text, pretty-printed with a final newline: the marker and version, then the
    /// name, the roles in the role table's order and the explicit tokens in table order.
    pub fn write(&self) -> String {
        let mut text = serde_json::to_string_pretty(&Written {
            format: THEME_DOCUMENT_FORMAT,
            version: THEME_DOCUMENT_VERSION,
            document: self,
        })
        .expect("a document of strings and colours always serializes");
        text.push('\n');
        text
    }

    /// The theme resolved by `how`'s rules.
    pub fn resolve(&self, how: Resolution) -> Result<ResolvedTheme, Error> {
        resolve(&self.roles, &self.tokens, how)
    }
}
