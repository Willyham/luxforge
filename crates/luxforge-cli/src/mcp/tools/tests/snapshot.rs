//! The built-in registry's `schema.list` answer the adapter lists without a session
//! ([`BUILT_IN_SCHEMA`], `src/mcp/builtin-schema.json`), held to what `ModuleRegistry::builtin()`
//! answers. The adapter carries the answer rather than the registry, so it links none of the
//! modules; this test is where the two meet.
//!
//! Regenerate it after an intended registry change, then review the diff:
//!
//! ```sh
//! cargo test -p luxforge-cli --lib -- --ignored generate_builtin_schema_snapshot
//! ```
use super::*;
use luxforge_core::{ModuleRegistry, schemas};
use std::path::PathBuf;

const GENERATOR: &str = "generate_builtin_schema_snapshot";

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/mcp/builtin-schema.json")
}

/// `schema.list` as the built-in registry answers it, pretty-printed with a trailing newline.
fn generated() -> String {
    let mut text =
        serde_json::to_string_pretty(&schemas(&ModuleRegistry::builtin())).expect("JSON");
    text.push('\n');
    text
}

#[test]
fn the_built_in_schema_matches_the_registry() {
    let published = generated();
    if BUILT_IN_SCHEMA == published {
        return;
    }
    let line = BUILT_IN_SCHEMA
        .lines()
        .zip(published.lines())
        .position(|(committed, published)| committed != published)
        .unwrap_or_else(|| {
            BUILT_IN_SCHEMA
                .lines()
                .count()
                .min(published.lines().count())
        });
    let around = |text: &str| -> String {
        text.lines()
            .skip(line.saturating_sub(3))
            .take(7)
            .collect::<Vec<_>>()
            .join("\n")
    };
    panic!(
        "the built-in registry no longer answers {}: first difference at line {}\n\
         committed:\n{}\ngenerated:\n{}\n\
         If the change is intended, regenerate it with `cargo test -p luxforge-cli --lib -- \
         --ignored {GENERATOR}` and review the diff.",
        snapshot_path().display(),
        line + 1,
        around(BUILT_IN_SCHEMA),
        around(&published),
    );
}

#[test]
#[ignore = "rewrites the committed snapshot; run on purpose after an intended registry change"]
fn generate_builtin_schema_snapshot() {
    std::fs::write(snapshot_path(), generated()).expect("the snapshot is written");
}
