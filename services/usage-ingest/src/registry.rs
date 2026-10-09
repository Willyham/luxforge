//! The `feature` and `tool` enums, from `registry.json` beside the manifest, compiled in.
//!
//! The desktop's checked-in copy is `crates/luxforge-core/src/usage/registry.json`; the end-to-end
//! check holds the two equal. The shape is `{"tool": [...], "feature": [...]}` with sorted,
//! kebab-case values.

use serde::Deserialize;
use std::sync::OnceLock;

const SOURCE: &str = include_str!("../registry.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub tool: Vec<String>,
    pub feature: Vec<String>,
}

impl Registry {
    /// Parse and check a registry file.
    pub fn parse(text: &str) -> Result<Registry, String> {
        let registry: Registry = serde_json::from_str(text).map_err(|e| e.to_string())?;
        for (name, values) in [("tool", &registry.tool), ("feature", &registry.feature)] {
            if values.is_empty() {
                return Err(format!("{name} is empty"));
            }
            for value in values {
                if !kebab_case(value) {
                    return Err(format!("{name} value {value:?} is not kebab-case"));
                }
            }
            if values.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(format!("{name} is not sorted and unique"));
            }
        }
        Ok(registry)
    }

    /// The compiled-in registry. A file that fails its own checks stops the process at first use;
    /// the unit tests run the same checks on the same file.
    pub fn get() -> &'static Registry {
        static REGISTRY: OnceLock<Registry> = OnceLock::new();
        REGISTRY.get_or_init(|| Registry::parse(SOURCE).expect("registry.json is invalid"))
    }

    pub fn tool(&self, value: &str) -> Option<&str> {
        find(&self.tool, value)
    }

    pub fn feature(&self, value: &str) -> Option<&str> {
        find(&self.feature, value)
    }
}

fn find<'a>(values: &'a [String], value: &str) -> Option<&'a str> {
    values
        .binary_search_by(|candidate| candidate.as_str().cmp(value))
        .ok()
        .map(|index| values[index].as_str())
}

/// Lowercase words of letters and digits joined by single hyphens.
pub fn kebab_case(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checked_in_file_is_sorted_kebab_case() {
        let registry = Registry::parse(SOURCE).expect("registry.json");
        assert!(!registry.tool.is_empty() && !registry.feature.is_empty());
        let mut sorted = registry.tool.clone();
        sorted.sort();
        assert_eq!(sorted, registry.tool);
        let mut sorted = registry.feature.clone();
        sorted.sort();
        assert_eq!(sorted, registry.feature);
        assert!(
            registry
                .tool
                .iter()
                .chain(&registry.feature)
                .all(|v| kebab_case(v))
        );
    }

    #[test]
    fn the_file_is_exactly_the_two_arrays() {
        let value: serde_json::Value = serde_json::from_str(SOURCE).unwrap();
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["feature", "tool"]);
    }

    #[test]
    fn unsorted_duplicate_and_unkebab_values_are_refused() {
        for bad in [
            r#"{"tool":["b","a"],"feature":["x"]}"#,
            r#"{"tool":["a","a"],"feature":["x"]}"#,
            r#"{"tool":["Basic"],"feature":["x"]}"#,
            r#"{"tool":["a_b"],"feature":["x"]}"#,
            r#"{"tool":["a--b"],"feature":["x"]}"#,
            r#"{"tool":["-a"],"feature":["x"]}"#,
            r#"{"tool":[""],"feature":["x"]}"#,
            r#"{"tool":[],"feature":["x"]}"#,
            r#"{"tool":["a"],"feature":["x"],"extra":[]}"#,
        ] {
            assert!(Registry::parse(bad).is_err(), "{bad}");
        }
        assert!(Registry::parse(r#"{"tool":["1a","a","a-b","b2"],"feature":["x"]}"#).is_ok());
    }

    #[test]
    fn lookup_is_exact() {
        let registry = Registry::get();
        assert_eq!(registry.tool("basic"), Some("basic"));
        assert_eq!(registry.tool("Basic"), None);
        assert_eq!(registry.feature("basic"), None);
    }
}
