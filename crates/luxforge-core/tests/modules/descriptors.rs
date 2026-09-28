//! The one committed snapshot of what the built-in registry publishes: `module.list` with its
//! `host` array beside the modules, and every method `schema.list` generates from those
//! descriptors, with its parameters, the source kinds it applies to and each superseded field. It
//! is the one place the built-in modules, their actions, queries, effects (with their stage, order,
//! maskable flag and sources) and controls (with their variants) are written down, so registering
//! a module changes this file and no hand-kept list elsewhere, and a change that was meant to keep
//! the descriptors identical proves it byte for byte.
//!
//! Regenerate it after an intentional descriptor change, then review the diff:
//!
//! ```sh
//! cargo test -p luxforge-core --test modules -- --ignored generate_builtin_descriptor_snapshot
//! ```

use luxforge_core::{ModuleRegistry, capabilities::host::TASK_PREFIX};
use luxforge_testkit::{client::Owner, fixtures};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/modules/builtin-descriptors.json")
}

/// `module.list` and the generated part of `schema.list`, as a JSON client reads them from an
/// owner serving [`ModuleRegistry::builtin`], pretty-printed with a trailing newline.
fn published() -> String {
    let catalog = fixtures::temp_catalog("builtin-descriptors");
    let owner =
        Owner::start(&catalog, ModuleRegistry::builtin(), "descriptor-snapshot").expect("an owner");
    let client = owner.client();
    let listed = owner
        .call(client, "module.list", json!({}))
        .expect("module.list");
    let schema = owner
        .call(client, "schema.list", json!({}))
        .expect("schema.list");
    owner.close().expect("the owner stops");
    let _ = std::fs::remove_file(&catalog);

    // `schema.list` repeats both descriptor arrays under the same keys; the snapshot holds them
    // once.
    assert_eq!(schema["modules"], listed["modules"]);
    assert_eq!(schema["host"], listed["host"]);

    // The methods the registry generates from the modules' and the host's descriptors, and not the
    // host's own fixed methods, which their own tests describe.
    let registry = ModuleRegistry::builtin();
    let descriptors = registry
        .descriptors()
        .into_iter()
        .chain(registry.host_descriptors());
    let mut names = Vec::new();
    for descriptor in descriptors {
        names.extend(descriptor.actions.iter().map(|action| {
            registry
                .resolve_action(&action.id)
                .expect("a registered action")
                .method()
        }));
        names.extend(descriptor.queries.iter().map(|query| {
            registry
                .resolve_query(&query.id)
                .expect("a registered query")
                .method()
        }));
        names.extend(
            descriptor
                .tasks
                .iter()
                .map(|task| format!("{TASK_PREFIX}{}", task.id)),
        );
    }
    let methods: Map<String, Value> = names
        .into_iter()
        .map(|name| {
            let schema = schema["methods"]
                .get(&name)
                .unwrap_or_else(|| panic!("schema.list does not list {name}"))
                .clone();
            (name, schema)
        })
        .collect();
    let snapshot = json!({
        "module.list": listed,
        "schema.list": {"methods": methods},
    });
    let mut text = serde_json::to_string_pretty(&snapshot).expect("JSON");
    text.push('\n');
    text
}

#[test]
fn the_built_in_descriptors_match_the_committed_snapshot() {
    let path = snapshot_path();
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is unreadable: {error}", path.display()));
    let published = published();
    if committed == published {
        return;
    }
    let line = committed
        .lines()
        .zip(published.lines())
        .position(|(committed, published)| committed != published)
        .unwrap_or_else(|| committed.lines().count().min(published.lines().count()));
    let around = |text: &str| -> String {
        text.lines()
            .skip(line.saturating_sub(3))
            .take(7)
            .collect::<Vec<_>>()
            .join("\n")
    };
    panic!(
        "the built-in registry no longer publishes {}: first difference at line {}\n\
         committed:\n{}\npublished:\n{}\n\
         If the change is intended, regenerate it with `cargo test -p luxforge-core --test \
         modules -- --ignored generate_builtin_descriptor_snapshot` and review the diff.",
        path.display(),
        line + 1,
        around(&committed),
        around(&published),
    );
}

#[test]
#[ignore = "writes the committed snapshot; run it only after an intended descriptor change"]
fn generate_builtin_descriptor_snapshot() {
    let path = snapshot_path();
    std::fs::create_dir_all(path.parent().expect("a directory")).expect("the fixture directory");
    std::fs::write(&path, published()).expect("the snapshot is written");
}
