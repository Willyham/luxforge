//! How a history label and a refusal name what they talk about: a parameter's value, a declared
//! name read as a phrase and a module that does not apply to a photo.
use crate::SourceTag;
use serde_json::Value;

/// A parameter value as a history label names it: integers as written, numbers without trailing
/// zeros, three channels as `r,g,b`, enum options title-cased with hyphens as spaces, anything else
/// as its JSON text.
pub(crate) fn label_value(value: &Value) -> String {
    match value {
        // `{}` on an f64 already drops trailing zeros: 3.5, 0, -12.
        Value::Number(number) if number.is_f64() => match number.as_f64() {
            Some(number) => format!("{number}"),
            None => number.to_string(),
        },
        Value::Number(number) => number.to_string(),
        Value::String(text) => title_case(text),
        Value::Array(channels) if channels.iter().all(Value::is_number) => channels
            .iter()
            .map(label_value)
            .collect::<Vec<_>>()
            .join(","),
        other => other.to_string(),
    }
}

/// `rotate-left` reads as `Rotate left`; `16:9` and other punctuated options keep their shape. A
/// mask component's display name is built from its kind the same way, so `luminance-range` reads as
/// `Luminance range 1`.
pub(crate) fn title_case(text: &str) -> String {
    // A declared name reaches this as a kind (`colour-range`) or as a parameter (`colour_refine`),
    // and both read as a phrase, so both separators become a space rather than one of them being
    // shown to a person as it is spelled in a request.
    let spaced = text.replace(['-', '_'], " ");
    let mut characters = spaced.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => spaced,
    }
}

/// What a refusal says when `title`'s module or effect does not apply to a photo of `kind`.
pub(crate) fn not_applicable(title: &str, kind: SourceTag) -> String {
    format!("{title} does not apply to a {} photo", kind.label())
}
