//! Every field-patch module the built-in registry holds passes the one conformance suite in
//! `tests/conformance/`: discovery, neutral payloads compiling to nothing and sharing the source,
//! one layer per target, drafts, no-ops, deduplication, resets that keep the layer's identity,
//! history, sample equal to render on the byte and linear paths and through a straightened crop,
//! an unavailable provider and reopen. `cargo xtask editor-acceptance` runs the same function in
//! release and records what it returns as evidence.

mod conformance;

use std::{fs, path::Path};

#[test]
fn every_field_patch_module_passes_the_conformance_suite() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
    let out = std::env::temp_dir().join(format!(
        "luxforge-field-patch-conformance-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).expect("a scratch directory");
    let result = conformance::run(&fixture, &out);
    let _ = fs::remove_dir_all(&out);
    if let Err(failure) = result {
        panic!("{failure}");
    }
}
