//! The ingress against the desktop's own copies: the event registry is byte-for-byte the desktop's,
//! and every synthetic batch the desktop's core accepts or refuses
//! (`crates/luxforge-core/tests/fixtures/usage/`, decoded there by `usage::decode_batch` and
//! re-encoded by the client's encoder) is accepted or refused here alike. Read from the repository
//! checkout at test time, so the container build, which sees only this package, needs none of it.

use luxforge_usage_ingest::wire;
use serde_json::Value;
use std::{fs, path::PathBuf};

fn repository(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn read(path: &str) -> String {
    fs::read_to_string(repository(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
}

const FIXTURES: &str = "crates/luxforge-core/tests/fixtures/usage";

#[test]
fn the_registry_is_the_desktops_byte_for_byte() {
    assert_eq!(
        fs::read(repository("services/usage-ingest/registry.json")).unwrap(),
        fs::read(repository("crates/luxforge-core/src/usage/registry.json")).unwrap(),
        "services/usage-ingest/registry.json differs from the desktop's crates/luxforge-core/src/usage/registry.json"
    );
}

#[test]
fn every_batch_the_desktop_accepts_is_accepted() {
    for name in ["valid-design-example.json", "valid-every-event.json"] {
        let body = read(&format!("{FIXTURES}/{name}"));
        assert!(
            wire::parse(body.as_bytes()).is_ok(),
            "{name}: the desktop's core accepts it, the ingress refuses it"
        );
    }
}

#[test]
fn every_batch_the_desktop_refuses_is_refused() {
    let cases: Vec<Value> =
        serde_json::from_str(&read(&format!("{FIXTURES}/invalid-batches.json"))).unwrap();
    assert!(cases.len() > 100, "only {} invalid cases", cases.len());
    let accepted: Vec<&str> = cases
        .iter()
        .filter(|case| {
            let body = case["body"].as_str().expect("a case has a body");
            wire::parse(body.as_bytes()).is_ok()
        })
        .map(|case| case["case"].as_str().unwrap_or("?"))
        .collect();
    assert!(
        accepted.is_empty(),
        "the desktop's core refuses these, the ingress accepts them: {accepted:?}"
    );
}
