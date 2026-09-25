//! Every field-patch module the built-in registry holds passes the one conformance suite in
//! `conformance/`: discovery, neutral payloads compiling to nothing and sharing the source, the
//! field-patch rules (which payloads read, how a patch and a reset plan, labels, descriptions and
//! values), one layer per target, drafts, no-ops, deduplication, resets that keep the layer's
//! identity, history, sample equal to render on the byte and linear paths and through a
//! straightened crop, an unavailable provider and reopen. `cargo xtask editor-acceptance` runs the
//! same function in release and records what it returns as evidence.

use super::conformance;
use lightwell_testkit::fixtures;
use std::fs;

#[test]
fn every_field_patch_module_passes_the_conformance_suite() {
    let out = fixtures::temp_path("field-patch-conformance");
    fs::create_dir_all(&out).expect("a scratch directory");
    let result = conformance::run(&fixtures::jpeg(), &out);
    let _ = fs::remove_dir_all(&out);
    if let Err(failure) = result {
        panic!("{failure}");
    }
}
