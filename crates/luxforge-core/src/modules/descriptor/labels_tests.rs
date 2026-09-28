//! Tests of how a label names a value.
use super::*;
use crate::SourceTag;
use serde_json::{Value, json};

#[test]
fn a_value_reads_the_way_a_history_label_names_it() {
    for (case, value, expected) in [
        ("an integer as written", json!(12), "12"),
        ("zero", json!(0), "0"),
        ("a number without trailing zeros", json!(3.5), "3.5"),
        ("a whole number", json!(7.0), "7"),
        ("a negative number", json!(-2.4), "-2.4"),
        ("three channels", json!([1, 2, 3]), "1,2,3"),
        ("an option", json!("rotate-left"), "Rotate left"),
        ("a punctuated option", json!("16:9"), "16:9"),
        ("a one-word option", json!("free"), "Free"),
        ("a boolean", json!(true), "true"),
        ("null", Value::Null, "null"),
    ] {
        assert_eq!(label_value(&value), expected, "{case}");
    }
}

#[test]
fn a_declared_name_reads_as_a_phrase() {
    assert_eq!(title_case("luminance-range"), "Luminance range");
    assert_eq!(title_case("colour_refine"), "Colour refine");
    assert_eq!(title_case("16:9"), "16:9");
    assert_eq!(title_case(""), "");
    assert_eq!(
        not_applicable("RAW", SourceTag::Jpeg),
        "RAW does not apply to a JPEG photo"
    );
}
