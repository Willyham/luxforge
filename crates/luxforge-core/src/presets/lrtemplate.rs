//! Legacy Lightroom `.lrtemplate` presets: a bounded recursive-descent reader for exactly the Lua
//! subset Lightroom writes, `s = { … }`.
//!
//! The subset is tables with `key = value` and positional entries (trailing commas and `;` allowed),
//! double-quoted strings with escapes, long strings `[[…]]` and `[==[…]==]`, numbers, `true`,
//! `false`, `ZSTR "…"` localized strings and `--` comments. Anything else is `unsupported-input`
//! naming the byte offset, and nothing is evaluated.
use super::mapping::is_curve;
use super::value::{RawSetting, RawValue};
use crate::Error;
#[cfg(test)]
use crate::ErrorKind;
use std::collections::HashSet;

/// The deepest table nesting accepted, counting the outer `s = { … }` table as the first level.
pub const MAX_TEMPLATE_DEPTH: usize = 16;
/// The most values one template may hold, counting every scalar and every table.
pub const MAX_TEMPLATE_VALUES: usize = 100_000;

#[derive(Clone, Debug, PartialEq)]
enum Lua {
    Str(String),
    /// A number's source text, kept as written for the report.
    Num(String),
    Bool(bool),
    /// Entries in order; a positional entry has no key.
    Table(Vec<(Option<String>, Lua)>),
}

/// What a Develop template holds besides its settings.
#[derive(Debug)]
pub(super) struct Template {
    pub title: Option<String>,
    pub internal_name: Option<String>,
    pub id: Option<String>,
    pub uuid: Option<String>,
    pub settings: Vec<RawSetting>,
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
    values: usize,
}

fn name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn error_at(&self, at: usize, detail: &str) -> Error {
        Error::unsupported_input(format!("malformed .lrtemplate: {detail} at byte {at}"))
    }

    fn error(&self, detail: &str) -> Error {
        self.error_at(self.at, detail)
    }

    fn limit(&self, detail: String) -> Error {
        Error::resource_limit(format!("{detail} at byte {}", self.at))
    }

    /// The level of a long bracket `[`, `=`×level, `[` opening at the cursor, without moving.
    fn long_bracket(&self) -> Option<usize> {
        let rest = self.bytes.get(self.at..)?;
        if rest.first() != Some(&b'[') {
            return None;
        }
        let level = rest[1..].iter().take_while(|byte| **byte == b'=').count();
        (rest.get(level + 1) == Some(&b'[')).then_some(level)
    }

    /// A long string's body. A newline straight after the opening bracket is not part of it.
    fn long_string(&mut self, level: usize) -> Result<&'a str, Error> {
        let open = self.at;
        self.at += level + 2;
        for newline in ["\r\n", "\n\r", "\n", "\r"] {
            if self.text[self.at..].starts_with(newline) {
                self.at += newline.len();
                break;
            }
        }
        let close = format!("]{}]", "=".repeat(level));
        let Some(length) = self.text[self.at..].find(&close) else {
            return Err(self.error_at(open, "unterminated long string"));
        };
        let body = &self.text[self.at..self.at + length];
        self.at += length + close.len();
        Ok(body)
    }

    /// Whitespace and comments, `--` to the end of the line or `--[[…]]`.
    fn space(&mut self) -> Result<(), Error> {
        loop {
            while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
                self.at += 1;
            }
            if !self.bytes[self.at..].starts_with(b"--") {
                return Ok(());
            }
            self.at += 2;
            if let Some(level) = self.long_bracket() {
                self.long_string(level)?;
            } else {
                self.at = self.text[self.at..]
                    .find('\n')
                    .map_or(self.bytes.len(), |line| self.at + line);
            }
        }
    }

    fn name(&mut self) -> &'a str {
        let start = self.at;
        while self.peek().is_some_and(name_byte) {
            self.at += 1;
        }
        &self.text[start..self.at]
    }

    fn quoted(&mut self) -> Result<String, Error> {
        let open = self.at;
        self.at += 1;
        let mut out = Vec::new();
        loop {
            let rest = &self.bytes[self.at..];
            let run = rest
                .iter()
                .position(|byte| matches!(byte, b'"' | b'\\' | b'\n' | b'\r'))
                .ok_or_else(|| self.error_at(open, "unterminated string"))?;
            out.extend_from_slice(&rest[..run]);
            self.at += run;
            match self.bytes[self.at] {
                b'"' => {
                    self.at += 1;
                    break;
                }
                b'\\' => self.escape(&mut out, open)?,
                _ => return Err(self.error_at(open, "unterminated string")),
            }
        }
        String::from_utf8(out).map_err(|_| self.error_at(open, "string is not UTF-8"))
    }

    fn escape(&mut self, out: &mut Vec<u8>, open: usize) -> Result<(), Error> {
        let escape = self.at;
        self.at += 1;
        let Some(byte) = self.peek() else {
            return Err(self.error_at(open, "unterminated string"));
        };
        self.at += 1;
        let simple = match byte {
            b'n' | b'\n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'v' => 0x0b,
            b'\\' | b'"' | b'\'' => byte,
            b'0'..=b'9' => {
                let mut code = u32::from(byte - b'0');
                for _ in 0..2 {
                    match self.peek() {
                        Some(digit @ b'0'..=b'9') => {
                            code = code * 10 + u32::from(digit - b'0');
                            self.at += 1;
                        }
                        _ => break,
                    }
                }
                u8::try_from(code).map_err(|_| self.error_at(escape, "escape out of range"))?
            }
            _ => return Err(self.error_at(escape, "invalid escape")),
        };
        out.push(simple);
        Ok(())
    }

    fn number(&mut self) -> Result<&'a str, Error> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        let digits = |parser: &mut Self| {
            let from = parser.at;
            while parser.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                parser.at += 1;
            }
            parser.at - from
        };
        let mut mantissa = digits(self);
        if self.peek() == Some(b'.') {
            self.at += 1;
            mantissa += digits(self);
        }
        if mantissa == 0 {
            return Err(self.error_at(start, "malformed number"));
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if digits(self) == 0 {
                return Err(self.error_at(start, "malformed number"));
            }
        }
        if self
            .peek()
            .is_some_and(|byte| name_byte(byte) || byte == b'.')
        {
            return Err(self.error_at(start, "malformed number"));
        }
        Ok(&self.text[start..self.at])
    }

    fn value(&mut self, depth: usize) -> Result<Lua, Error> {
        self.values += 1;
        if self.values > MAX_TEMPLATE_VALUES {
            return Err(self.limit(format!(
                "the template holds more than {MAX_TEMPLATE_VALUES} values"
            )));
        }
        match self.peek() {
            Some(b'{') => self.table(depth + 1),
            Some(b'"') => self.quoted().map(Lua::Str),
            Some(b'[') => match self.long_bracket() {
                Some(level) => self
                    .long_string(level)
                    .map(|body| Lua::Str(body.to_owned())),
                None => Err(self.error("unsupported value")),
            },
            Some(b'-' | b'.' | b'0'..=b'9') => self.number().map(|text| Lua::Num(text.to_owned())),
            Some(byte) if name_start(byte) => {
                let start = self.at;
                match self.name() {
                    "true" => Ok(Lua::Bool(true)),
                    "false" => Ok(Lua::Bool(false)),
                    "ZSTR" => {
                        self.space()?;
                        let text = match (self.peek(), self.long_bracket()) {
                            (Some(b'"'), _) => self.quoted()?,
                            (_, Some(level)) => self.long_string(level)?.to_owned(),
                            _ => return Err(self.error("ZSTR needs a string")),
                        };
                        Ok(Lua::Str(localized(text)))
                    }
                    name => Err(self.error_at(start, &format!("unsupported value {name}"))),
                }
            }
            Some(_) => Err(self.error("unsupported value")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn table(&mut self, depth: usize) -> Result<Lua, Error> {
        if depth > MAX_TEMPLATE_DEPTH {
            return Err(self.limit(format!(
                "the template nests tables deeper than {MAX_TEMPLATE_DEPTH} levels"
            )));
        }
        let open = self.at;
        self.at += 1;
        let mut entries = Vec::new();
        let mut keys = HashSet::new();
        loop {
            self.space()?;
            if self.peek() == Some(b'}') {
                self.at += 1;
                return Ok(Lua::Table(entries));
            }
            let start = self.at;
            let mut key = None;
            if self.peek().is_some_and(name_start) {
                let name = self.name();
                self.space()?;
                if self.peek() == Some(b'=') && self.bytes.get(self.at + 1) != Some(&b'=') {
                    self.at += 1;
                    self.space()?;
                    if !keys.insert(name) {
                        return Err(self.error_at(start, &format!("duplicate key {name}")));
                    }
                    key = Some(name.to_owned());
                } else {
                    self.at = start;
                }
            }
            let value = self.value(depth)?;
            entries.push((key, value));
            self.space()?;
            match self.peek() {
                Some(b',' | b';') => self.at += 1,
                Some(b'}') => {}
                Some(_) => return Err(self.error("expected , or }")),
                None => return Err(self.error_at(open, "unterminated table")),
            }
        }
    }

    /// `s = { … }` and nothing after it but whitespace and comments.
    fn chunk(&mut self) -> Result<Vec<(Option<String>, Lua)>, Error> {
        self.space()?;
        if self.name() != "s" {
            return Err(self.error("expected s = {"));
        }
        self.space()?;
        if self.peek() != Some(b'=') {
            return Err(self.error("expected s = {"));
        }
        self.at += 1;
        self.space()?;
        if self.peek() != Some(b'{') {
            return Err(self.error("expected s = {"));
        }
        let Lua::Table(entries) = self.value(0)? else {
            unreachable!("a value starting with {{ is a table");
        };
        self.space()?;
        if self.at != self.bytes.len() {
            return Err(self.error("unexpected text after the preset table"));
        }
        Ok(entries)
    }
}

/// The default text of a `$$$/Key=Text` localized string; any other string as it is.
fn localized(text: String) -> String {
    match text
        .strip_prefix("$$$/")
        .and_then(|key| key.split_once('='))
    {
        Some((_, default)) => default.to_owned(),
        None => text,
    }
}

fn get<'t>(entries: &'t [(Option<String>, Lua)], key: &str) -> Option<&'t Lua> {
    entries
        .iter()
        .find(|(name, _)| name.as_deref() == Some(key))
        .map(|(_, value)| value)
}

/// Move a keyed value out of its table, so the settings are converted without a copy.
fn take(entries: &mut [(Option<String>, Lua)], key: &str) -> Option<Lua> {
    entries
        .iter_mut()
        .find(|(name, _)| name.as_deref() == Some(key))
        .map(|(_, value)| std::mem::replace(value, Lua::Bool(false)))
}

fn string(value: Option<&Lua>) -> Option<String> {
    match value {
        Some(Lua::Str(text)) => Some(text.clone()),
        _ => None,
    }
}

/// A Lua value as a setting value. A flat numeric array under a curve's name is read as that
/// curve's `"x, y"` points.
fn raw(name: &str, value: Lua) -> RawValue {
    match value {
        Lua::Str(text) | Lua::Num(text) => RawValue::Text(text),
        Lua::Bool(value) => RawValue::Bool(value),
        Lua::Table(entries) if entries.iter().all(|(key, _)| key.is_none()) => {
            let flat_curve = is_curve(name)
                && entries.len() % 2 == 0
                && entries
                    .iter()
                    .all(|(_, value)| matches!(value, Lua::Num(_)));
            if flat_curve {
                RawValue::List(
                    entries
                        .chunks_exact(2)
                        .map(|pair| match pair {
                            [(_, Lua::Num(x)), (_, Lua::Num(y))] => {
                                RawValue::Text(format!("{x}, {y}"))
                            }
                            _ => unreachable!("every entry is a number"),
                        })
                        .collect(),
                )
            } else {
                RawValue::List(
                    entries
                        .into_iter()
                        .map(|(_, value)| raw("", value))
                        .collect(),
                )
            }
        }
        Lua::Table(entries) => {
            let mut position = 0;
            RawValue::Struct(
                entries
                    .into_iter()
                    .map(|(key, value)| {
                        let key = key.unwrap_or_else(|| {
                            position += 1;
                            format!("[{position}]")
                        });
                        let value = raw(&key, value);
                        (key, value)
                    })
                    .collect(),
            )
        }
    }
}

/// Read a Develop template whose `s = {` starts at byte `start` of `text`. Byte offsets in errors
/// count from the start of `text`.
pub(super) fn read(text: &str, start: usize) -> Result<Template, Error> {
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        at: start,
        values: 0,
    };
    let mut top = parser.chunk()?;
    match get(&top, "type") {
        Some(Lua::Str(kind)) if kind == "Develop" => {}
        Some(Lua::Str(kind)) => {
            return Err(Error::unsupported_input(format!(
                "a {kind} template is not a Develop preset"
            )));
        }
        _ => return Err(Error::unsupported_input("the template has no Develop type")),
    }
    let Some(Lua::Table(mut value)) = take(&mut top, "value") else {
        return Err(Error::unsupported_input("the template has no value table"));
    };
    let Some(Lua::Table(entries)) = take(&mut value, "settings") else {
        return Err(Error::unsupported_input(
            "the template has no value.settings table",
        ));
    };
    let mut settings = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let Some(name) = key else {
            return Err(Error::unsupported_input(
                "value.settings holds an entry without a name",
            ));
        };
        let value = raw(&name, value);
        settings.push(RawSetting { name, value });
    }
    Ok(Template {
        title: string(get(&top, "title")),
        internal_name: string(get(&top, "internalName")),
        id: string(get(&top, "id")),
        uuid: string(get(&value, "uuid")),
        settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Vec<(Option<String>, Lua)>, Error> {
        Parser {
            text,
            bytes: text.as_bytes(),
            at: 0,
            values: 0,
        }
        .chunk()
    }

    fn str(text: &str) -> Lua {
        Lua::Str(text.to_owned())
    }

    fn num(text: &str) -> Lua {
        Lua::Num(text.to_owned())
    }

    fn key(name: &str, value: Lua) -> (Option<String>, Lua) {
        (Some(name.to_owned()), value)
    }

    #[test]
    fn the_subset_reads_as_written() {
        let text = "-- leading comment\n s={ a = \"q\\\"\\n\\65\\\\\", b = -1.5e2, c = { 1, 2, }; \
                    d = true, e = false, f = [[\nlong ] still]], g = [==[x]]y]==], --[[ gone ]]\n\
                    h = ZSTR \"$$$/K/L=Text = more\", i = ZSTR [[plain]], j = .5, }\n-- end";
        assert_eq!(
            parse(text).unwrap(),
            vec![
                key("a", str("q\"\nA\\")),
                key("b", num("-1.5e2")),
                key("c", Lua::Table(vec![(None, num("1")), (None, num("2"))])),
                key("d", Lua::Bool(true)),
                key("e", Lua::Bool(false)),
                key("f", str("long ] still")),
                key("g", str("x]]y")),
                key("h", str("Text = more")),
                key("i", str("plain")),
                key("j", num(".5")),
            ]
        );
    }

    #[test]
    fn malformed_input_names_its_byte_offset() {
        for (text, detail) in [
            ("s = { a = @ }", "unsupported value at byte 10"),
            ("s = { a = \"abc }", "unterminated string at byte 10"),
            ("s = { a = \"a\nb\" }", "unterminated string at byte 10"),
            ("s = { a = \"\\q\" }", "invalid escape at byte 11"),
            ("s = { a = \"\\300\" }", "escape out of range at byte 11"),
            ("s = { a = [[x }", "unterminated long string at byte 10"),
            ("s = { a = 1 b = 2 }", "expected , or } at byte 12"),
            ("s = { a = 1, a = 2 }", "duplicate key a at byte 13"),
            ("s = { a = nil }", "unsupported value nil at byte 10"),
            ("s = { a = 0x10 }", "malformed number at byte 10"),
            ("s = { a = 1.2.3 }", "malformed number at byte 10"),
            ("s = { a = - 1 }", "malformed number at byte 10"),
            ("s = { a = ZSTR 5 }", "ZSTR needs a string at byte 15"),
            (
                "s = { a = 1 } x",
                "unexpected text after the preset table at byte 14",
            ),
            ("s = { a = { 1 }", "unterminated table at byte 4"),
            ("t = {}", "expected s = { at byte 1"),
            ("s = { --[[ open", "unterminated long string at byte 8"),
            ("s = { a = \"\\", "unterminated string at byte 10"),
        ] {
            let error = parse(text).unwrap_err();
            assert_eq!(error.kind, ErrorKind::UnsupportedInput, "{text}");
            assert!(error.detail.ends_with(detail), "{text}: {}", error.detail);
        }
    }

    #[test]
    fn depth_and_value_limits_are_resource_limits() {
        let nested = |levels: usize| {
            format!(
                "s = {}{}",
                "{ a = ".repeat(levels - 1) + "{",
                "}".repeat(levels)
            )
        };
        assert!(parse(&nested(MAX_TEMPLATE_DEPTH)).is_ok());
        let error = parse(&nested(MAX_TEMPLATE_DEPTH + 1)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        // The outer table is one value, so it holds MAX - 1 entries at the limit.
        let values = |count: usize| format!("s = {{ {} }}", "0,".repeat(count));
        assert!(parse(&values(MAX_TEMPLATE_VALUES - 1)).is_ok());
        let error = parse(&values(MAX_TEMPLATE_VALUES)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
    }

    #[test]
    fn a_flat_array_is_a_curve_only_under_a_curve_name() {
        let array = Lua::Table(vec![
            (None, num("0")),
            (None, num("10")),
            (None, num("255")),
            (None, num("250")),
        ]);
        let text = |value: &str| RawValue::Text(value.to_owned());
        assert_eq!(
            raw("ToneCurvePV2012", array.clone()),
            RawValue::List(vec![text("0, 10"), text("255, 250")])
        );
        assert_eq!(
            raw("Other", array),
            RawValue::List(vec![text("0"), text("10"), text("255"), text("250")])
        );
        let odd = Lua::Table(vec![(None, num("0")), (None, num("1")), (None, num("2"))]);
        assert_eq!(
            raw("ToneCurve", odd),
            RawValue::List(vec![text("0"), text("1"), text("2")])
        );
        let mixed = Lua::Table(vec![key("Name", str("n")), (None, num("1"))]);
        assert_eq!(
            raw("Look", mixed),
            RawValue::Struct(vec![("Name".into(), text("n")), ("[1]".into(), text("1"))])
        );
    }
}
