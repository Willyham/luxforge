//! The split-toning example (`examples/modules/split-toning`) held to its own component: its
//! source compiled here natively, and the same crate built for WebAssembly with the pinned toolchain
//! and run through Wasmtime, over every fixed patch for each value set. The example's own unit
//! tests run here too, as this module's.
#[path = "../../../examples/modules/split-toning/src/lib.rs"]
mod split_toning;

use luxforge_module_sdk::host::{Runner, build_component, componentize, harness};
use luxforge_module_sdk::{Field, Value, Values};
use luxforge_test_macros::slow;
use std::path::Path;

fn values(hh: f64, hs: f64, balance: f64, sh: f64, ss: f64) -> Values {
    Values::new(vec![
        Field::new("highlight-hue", Value::Number(hh)),
        Field::new("highlight-saturation", Value::Number(hs)),
        Field::new("balance", Value::Number(balance)),
        Field::new("shadow-hue", Value::Number(sh)),
        Field::new("shadow-saturation", Value::Number(ss)),
    ])
}

#[test]
#[slow]
fn the_example_gives_identical_buffers_natively_and_through_wasmtime() {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/modules/split-toning");
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("module-sdk");
    let component = build_component(&project, &target).unwrap_or_else(|error| panic!("{error}"));
    let sets = [
        ("defaults", values(45.0, 0.0, 0.0, 220.0, 0.0)),
        ("warm-and-cool", values(35.0, 60.0, 0.0, 215.0, 45.0)),
        ("full", values(0.0, 100.0, 100.0, 360.0, 100.0)),
        ("toward-shadows", values(300.0, 25.0, -100.0, 120.0, 80.0)),
        ("fractional", values(77.25, 33.3, -12.5, 181.75, 66.6)),
    ];
    let report = harness::assert_identical::<split_toning::SplitToning>(&component, &sets);
    assert_eq!(report.value_sets, sets.len());
    // Five patches of 64 × 64 pixels, three channels, for each value set.
    assert_eq!(report.channels, sets.len() * 5 * 64 * 64 * 3);
}

#[test]
fn a_core_module_that_imports_is_not_made_a_component() {
    // A core module of one function type and one import, `env.f`, and nothing else.
    let module = [
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version 1
        0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type section: () -> ()
        0x02, 0x09, 0x01, 0x03, b'e', b'n', b'v', 0x01, b'f', 0x00, 0x00, // import env.f
    ];
    let refusal = componentize(&module).expect_err("an importing module is refused");
    assert!(
        refusal.starts_with("the core module is not an adjustment module"),
        "{refusal}"
    );
}

#[test]
fn a_core_module_is_not_a_component() {
    let refusal = Runner::new(b"\0asm\x01\0\0\0")
        .err()
        .expect("a core module is refused");
    assert!(
        refusal.starts_with("module.wasm is not a component"),
        "{refusal}"
    );
}
