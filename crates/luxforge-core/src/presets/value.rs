//! Lightroom setting values as either format writes them, before the mapping table reads them.
//!
//! Both readers produce the same shapes, so the mapping and the report never know which format a
//! value came from: an XMP attribute and a Lua number are both text as written, an `rdf:Seq` and a
//! positional Lua table are both lists, and a nested `rdf:Description` and a keyed Lua table are
//! both structures.
use serde_json::{Number, Value};

/// The longest report `value`, in characters. A longer one keeps its first 255 characters and ends
/// with an ellipsis.
pub(super) const MAX_REPORT_CHARS: usize = 256;

/// One Lightroom setting: its Camera Raw name and its value as the file wrote it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RawSetting {
    pub name: String,
    pub value: RawValue,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum RawValue {
    /// A scalar as written: an XMP attribute or element text, a Lua string, or a Lua number's
    /// source text.
    Text(String),
    /// A Lua `true` or `false`.
    Bool(bool),
    /// An `rdf:Seq` or `rdf:Bag`, or a positional Lua table, in order. A curve's points are
    /// `"x, y"` texts whichever format wrote them.
    List(Vec<RawValue>),
    /// A nested `rdf:Description` or a keyed Lua table, with its fields in document order.
    Struct(Vec<(String, RawValue)>),
}

impl RawValue {
    pub(super) fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// A structure's field by name.
    pub(super) fn field(&self, name: &str) -> Option<&RawValue> {
        match self {
            Self::Struct(fields) => fields
                .iter()
                .find(|(field, _)| field == name)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// Whether the value holds nothing: blank text, an empty list or an empty structure.
    pub(super) fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.trim().is_empty(),
            Self::Bool(_) => false,
            Self::List(items) => items.is_empty(),
            Self::Struct(fields) => fields.is_empty(),
        }
    }
}

/// A decimal as Lightroom writes it: an optional sign, digits with an optional fraction, and an
/// optional exponent. `inf`, `NaN`, hexadecimal and empty text are not numbers.
fn decimal(text: &str) -> bool {
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(at) => (&unsigned[..at], Some(&unsigned[at + 1..])),
        None => (unsigned, None),
    };
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (mantissa, None),
    };
    let mantissa = digits(whole)
        && fraction.is_none_or(digits)
        && (!whole.is_empty() || fraction.is_some_and(|fraction| !fraction.is_empty()));
    let exponent = exponent.is_none_or(|exponent| {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !exponent.is_empty() && digits(exponent)
    });
    mantissa && exponent
}

/// A number written as text, with or without a leading `+`, as the JSON value a field receives:
/// an integer when the text is a whole number without a fraction or exponent, otherwise a finite
/// `f64`. Anything else is `None`; nothing is rounded or clamped.
pub(super) fn json_number(value: &RawValue) -> Option<Value> {
    let text = value.text()?.trim();
    if !decimal(text) {
        return None;
    }
    let unsigned = text.strip_prefix('+').unwrap_or(text);
    if !unsigned.contains(['.', 'e', 'E'])
        && let Ok(integer) = unsigned.parse::<i64>()
    {
        return Some(Value::from(integer));
    }
    unsigned
        .parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .map(Value::Number)
}

pub(super) fn number(value: &RawValue) -> Option<f64> {
    json_number(value).and_then(|value| value.as_f64())
}

/// A boolean as either format writes it: `True` and `False` in any case, `1` and `0`, or a Lua
/// boolean.
pub(super) fn boolean(value: &RawValue) -> Option<bool> {
    match value {
        RawValue::Bool(value) => Some(*value),
        RawValue::Text(text) => {
            let text = text.trim();
            if text.eq_ignore_ascii_case("true") || text == "1" {
                Some(true)
            } else if text.eq_ignore_ascii_case("false") || text == "0" {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether a curve maps every point to itself: a non-empty list of `"x, y"` points with `x == y`.
pub(super) fn identity_curve(value: &RawValue) -> bool {
    let RawValue::List(points) = value else {
        return false;
    };
    !points.is_empty()
        && points.iter().all(|point| {
            point
                .text()
                .and_then(|point| point.split_once(','))
                .and_then(|(x, y)| {
                    let x = number(&RawValue::Text(x.to_owned()))?;
                    let y = number(&RawValue::Text(y.to_owned()))?;
                    Some(x == y)
                })
                .unwrap_or(false)
        })
}

/// Text that stops growing past the report limit, so a 1 MiB list is never joined in full.
#[derive(Default)]
pub(super) struct Bounded {
    text: String,
    chars: usize,
}

impl Bounded {
    pub(super) fn full(&self) -> bool {
        self.chars > MAX_REPORT_CHARS
    }

    pub(super) fn push(&mut self, text: &str) {
        for character in text.chars() {
            if self.full() {
                return;
            }
            self.text.push(character);
            self.chars += 1;
        }
    }

    pub(super) fn finish(mut self) -> String {
        if self.full()
            && let Some((cut, _)) = self.text.char_indices().nth(MAX_REPORT_CHARS - 1)
        {
            self.text.truncate(cut);
            self.text.push('…');
        }
        self.text
    }
}

/// The report's `value`: the text as written, a list's items joined with `; `, and a structure as
/// its `Key=Value` fields joined with `, `, with a container inside a container reduced to its
/// size. At most [`MAX_REPORT_CHARS`] characters.
pub(super) fn report_text(value: &RawValue) -> String {
    let mut out = Bounded::default();
    write(&mut out, value, true);
    out.finish()
}

fn write(out: &mut Bounded, value: &RawValue, top: bool) {
    match value {
        RawValue::Text(text) => out.push(text),
        RawValue::Bool(value) => out.push(if *value { "true" } else { "false" }),
        RawValue::List(items) if top => {
            for (index, item) in items.iter().enumerate() {
                if out.full() {
                    return;
                }
                if index > 0 {
                    out.push("; ");
                }
                write(out, item, false);
            }
        }
        RawValue::List(items) => out.push(&format!("({} items)", items.len())),
        RawValue::Struct(fields) => {
            for (index, (name, field)) in fields.iter().enumerate() {
                if out.full() {
                    return;
                }
                if index > 0 {
                    out.push(", ");
                }
                out.push(name);
                out.push("=");
                match field {
                    RawValue::List(items) => out.push(&format!("({} items)", items.len())),
                    RawValue::Struct(fields) => out.push(&format!("({} fields)", fields.len())),
                    scalar => write(out, scalar, false),
                }
            }
        }
    }
}

/// A plain string held to the report limit.
pub(super) fn bounded(text: &str) -> String {
    let mut out = Bounded::default();
    out.push(text);
    out.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(value: &str) -> RawValue {
        RawValue::Text(value.to_owned())
    }

    #[test]
    fn numbers_parse_as_lightroom_writes_them_and_nothing_else() {
        for (written, expected) in [
            ("0.5", json!(0.5)),
            ("+0.50", json!(0.5)),
            ("-12", json!(-12)),
            ("+25", json!(25)),
            ("30", json!(30)),
            (" 7 ", json!(7)),
            (".5", json!(0.5)),
            ("5.", json!(5.0)),
            ("-0", json!(0)),
            ("1e-3", json!(0.001)),
            ("6.0", json!(6.0)),
        ] {
            assert_eq!(json_number(&text(written)), Some(expected), "{written}");
        }
        for written in [
            "", "+", "-", ".", "+-5", "--5", "inf", "NaN", "0x10", "1e", "1e+", "1.2.3", "12abc",
            "1 000", "True",
        ] {
            assert_eq!(json_number(&text(written)), None, "{written}");
        }
        assert_eq!(json_number(&RawValue::Bool(true)), None);
        // A whole number too large for i64 is still a finite number.
        assert_eq!(
            json_number(&text("99999999999999999999")),
            Some(json!(1e20))
        );
        assert_eq!(json_number(&text("1e999")), None);
    }

    #[test]
    fn booleans_parse_in_every_form_lightroom_writes() {
        for (written, expected) in [
            (text("True"), Some(true)),
            (text("TRUE"), Some(true)),
            (text("true"), Some(true)),
            (text("False"), Some(false)),
            (text("fAlSe"), Some(false)),
            (text("1"), Some(true)),
            (text("0"), Some(false)),
            (RawValue::Bool(true), Some(true)),
            (RawValue::Bool(false), Some(false)),
            (text("yes"), None),
            (text("2"), None),
            (RawValue::List(Vec::new()), None),
        ] {
            assert_eq!(boolean(&written), expected, "{written:?}");
        }
    }

    #[test]
    fn a_curve_is_identity_only_when_every_point_lies_on_the_diagonal() {
        let curve =
            |points: &[&str]| RawValue::List(points.iter().map(|point| text(point)).collect());
        assert!(identity_curve(&curve(&["0, 0", "128,128", "255, 255"])));
        assert!(!identity_curve(&curve(&["0, 0", "64, 56", "255, 255"])));
        assert!(!identity_curve(&curve(&[])));
        assert!(!identity_curve(&curve(&["0, 0", "oops"])));
        assert!(!identity_curve(&text("0, 0")));
    }

    #[test]
    fn report_text_joins_lists_summarizes_structures_and_stops_at_the_limit() {
        let list = RawValue::List(vec![text("0, 0"), text("255, 255")]);
        assert_eq!(report_text(&list), "0, 0; 255, 255");
        let look = RawValue::Struct(vec![
            ("Name".into(), text("Adobe Color")),
            ("Amount".into(), text("1")),
            ("Group".into(), RawValue::List(vec![text("a")])),
            ("Inner".into(), RawValue::Struct(Vec::new())),
            ("Stubbed".into(), RawValue::Bool(true)),
        ]);
        assert_eq!(
            report_text(&look),
            "Name=Adobe Color, Amount=1, Group=(1 items), Inner=(0 fields), Stubbed=true"
        );
        let masks = RawValue::List(vec![
            RawValue::Struct(vec![("What".into(), text("Mask"))]),
            RawValue::List(vec![text("x")]),
        ]);
        assert_eq!(report_text(&masks), "What=Mask; (1 items)");
        let long = RawValue::List((0..1000).map(|_| text("0, 0")).collect());
        let reported = report_text(&long);
        assert_eq!(reported.chars().count(), MAX_REPORT_CHARS);
        assert!(reported.ends_with('…'));
        let exact = "é".repeat(MAX_REPORT_CHARS);
        assert_eq!(bounded(&exact), exact);
        let over = "é".repeat(MAX_REPORT_CHARS + 1);
        assert_eq!(
            bounded(&over),
            format!("{}…", "é".repeat(MAX_REPORT_CHARS - 1))
        );
    }
}
