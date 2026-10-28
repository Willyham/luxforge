//! The host over test components (`components`): a valid one against its native twin, one that
//! imports, traps, overruns its deadline, answers the wrong length or outgrows its memory, and
//! determinism across runs and compiled-code caches.
mod components;

use components::{EFFECT, MANIFEST, Variant, native, package, values};
use luxforge_core::module_package::{ExternalModule, ModuleManifest};
use luxforge_core::{
    Availability, CompileStage, Error, Layer, LayerId, ModuleRegistry, Raster, RegistryOptions,
    SnapshotId, SourceImage, Stage,
};
use luxforge_testbase::paths::temp_dir;
use luxforge_testkit::fixtures::{recipe, render, source_of};
use luxforge_wasm::{Compiled, Limits, WasmHost};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// A 37 × 23 source of every code, in rows of odd width.
fn source() -> SourceImage {
    let pixels: Vec<[u8; 3]> = (0..37 * 23)
        .map(|index: u32| {
            let code = |step: u32| ((index * step) % 256) as u8;
            [code(7), code(13), code(29)]
        })
        .collect();
    source_of(37, 23, &pixels)
}

/// The registry serving `module` as its one external.
fn registry(module: ExternalModule) -> ModuleRegistry {
    ModuleRegistry::assemble(&RegistryOptions {
        external: &[module],
        ..RegistryOptions::default()
    })
    .expect("the registry assembles")
}

/// One frame of a gain-and-lift layer through the host's render entry point.
fn frame(registry: &ModuleRegistry) -> Result<Raster, Error> {
    let layer = Layer {
        id: LayerId::new(),
        effect_id: EFFECT.into(),
        effect_format: 1,
        payload: json!({"gain": 25.0, "lift": 3.0}),
        mask: None,
        artifacts: Vec::new(),
    };
    render(registry, &source(), SnapshotId::new(), &recipe(vec![layer]))
}

fn loaded(host: &WasmHost, variant: Variant) -> ExternalModule {
    let module = host.external(&package(variant)).expect("a module");
    assert_eq!(module.unavailable_reason(), None, "{variant:?} loads");
    module
}

#[test]
fn a_component_renders_the_frame_its_native_twin_renders_byte_for_byte() {
    let host = WasmHost::new(None);
    let adjustment = host.load(&package(Variant::Valid)).expect("it loads");
    let values = values(25.0, 3.0);
    let words = adjustment.coefficients(&values).unwrap();
    assert_eq!(words, native::words(&values));
    // The row's floats, bit for bit, values outside 0..1 among them.
    let mut row: Vec<[f32; 3]> = (0..40)
        .map(|index| {
            let value = index as f32 * 0.037 - 0.3;
            [value, -value * 1.7, value * value]
        })
        .collect();
    let mut twin = row.clone();
    adjustment.apply_row(&words, 9, 5, &mut row).unwrap();
    native::apply_row(native::words(&values), 9, 5, &mut twin);
    let bits =
        |row: &[[f32; 3]]| -> Vec<u32> { row.as_flattened().iter().map(|v| v.to_bits()).collect() };
    assert_eq!(bits(&row), bits(&twin));
    // The frame, through the registry, against the same manifest's module on native units.
    let wasm = frame(&registry(loaded(&host, Variant::Valid))).expect("the frame renders");
    let manifest = ModuleManifest::parse(MANIFEST).unwrap();
    let twin = ExternalModule::loaded(&manifest, Arc::new(native::Units)).unwrap();
    let twin = frame(&registry(twin)).expect("the twin renders");
    assert_eq!(wasm.rgba, twin.rgba);
    let unedited = render(
        &ModuleRegistry::builtin(),
        &source(),
        SnapshotId::new(),
        &recipe(Vec::new()),
    )
    .unwrap();
    assert_ne!(wasm.rgba, unedited.rgba, "the layer changes the frame");
}

#[test]
fn a_component_that_imports_anything_does_not_load_and_its_layers_are_reported() {
    let host = WasmHost::new(None);
    let error = host.load(&package(Variant::Imports)).unwrap_err();
    assert_eq!(
        error.detail,
        "module org.example.gainlift did not load: its component imports log, and an \
         adjustment module imports nothing"
    );
    assert_eq!(error.data.as_ref().unwrap()["failure"], "load");
    let registry = registry(host.external(&package(Variant::Imports)).unwrap());
    let provider = registry.module("org.example.gainlift").expect("listed");
    assert_eq!(
        provider.descriptor().availability,
        Availability::Unavailable {
            reason: "its component imports log, and an adjustment module imports nothing".into()
        }
    );
    let refused = frame(&registry).expect_err("no frame");
    assert_eq!(refused.unavailable_effect_id(), Some(EFFECT));
}

/// A frame of `variant` through `host` fails with `failure` and a detail starting `detail`, which
/// names the module, and there is no frame.
fn fails(host: &WasmHost, variant: Variant, failure: &str, detail: &str) {
    let registry = registry(loaded(host, variant));
    let error = frame(&registry).expect_err("no frame");
    assert!(
        error.detail.starts_with(detail),
        "{variant:?}: {}",
        error.detail
    );
    assert_eq!(
        error
            .data
            .as_ref()
            .map(|data| (&data["module"], &data["failure"])),
        Some((&json!("org.example.gainlift"), &json!(failure))),
        "{variant:?}"
    );
}

#[test]
fn a_component_that_traps_or_answers_the_wrong_length_fails_the_render_by_name() {
    let host = WasmHost::new(None);
    let trapped =
        "module org.example.gainlift trapped in apply-row: wasm `unreachable` instruction executed";
    fails(&host, Variant::Traps, "trap", trapped);
    // Growing past the memory limit fails the growth, and the component traps.
    fails(&host, Variant::Grows, "trap", trapped);
    fails(
        &host,
        Variant::Short,
        "length",
        "module org.example.gainlift answered 108 values for a row of 111",
    );
}

#[test]
fn a_component_that_overruns_its_deadline_fails_the_render_by_name() {
    let host = WasmHost::new(None).with_limits(Limits {
        deadline: Duration::from_millis(30),
        ..Limits::DEFAULT
    });
    fails(
        &host,
        Variant::Spins,
        "deadline",
        "module org.example.gainlift overran its 30 ms deadline in apply-row",
    );
}

#[test]
fn a_component_refusal_and_label_are_the_component_own() {
    let adjustment = WasmHost::new(None).load(&package(Variant::Valid)).unwrap();
    let error = adjustment.coefficients(&values(1.0, 2.0)[..1]).unwrap_err();
    assert_eq!(
        (error.kind, error.detail.as_str()),
        (
            luxforge_core::ErrorKind::Validation,
            "module org.example.gainlift refused the layer: expected two fields"
        )
    );
    assert_eq!(
        adjustment.label(&values(1.0, 2.0)).unwrap().as_deref(),
        Some("Gain")
    );
}

#[test]
fn renders_are_identical_across_runs_and_with_cached_or_freshly_compiled_code() {
    let cache = temp_dir("wasm-compiled-cache");
    let package = package(Variant::Valid);
    let uncached = WasmHost::new(None);
    let writing = WasmHost::new(Some(cache.clone()));
    let reading = WasmHost::new(Some(cache.clone()));
    assert_eq!(
        uncached.load(&package).unwrap().compiled(),
        Compiled::Uncached
    );
    assert_eq!(
        writing.load(&package).unwrap().compiled(),
        Compiled::Compiled
    );
    assert_eq!(reading.load(&package).unwrap().compiled(), Compiled::Cached);
    let first = frame(&registry(loaded(&uncached, Variant::Valid))).unwrap();
    let again = frame(&registry(loaded(&uncached, Variant::Valid))).unwrap();
    let cached = frame(&registry(loaded(&reading, Variant::Valid))).unwrap();
    assert_eq!(first.rgba, again.rgba);
    assert_eq!(first.rgba, cached.rgba);
    // A corrupted entry is compiled again, not run.
    for entry in std::fs::read_dir(&cache).unwrap() {
        let path = entry.unwrap().path();
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
    }
    assert_eq!(
        reading.load(&package).unwrap().compiled(),
        Compiled::Compiled
    );
    std::fs::remove_dir_all(cache).unwrap();
}

#[test]
fn instances_are_created_on_first_use_and_kept_for_reuse() {
    let adjustment = WasmHost::new(None).load(&package(Variant::Valid)).unwrap();
    assert_eq!(
        adjustment.idle_instances(),
        0,
        "loading creates no instance"
    );
    let words = adjustment.coefficients(&values(10.0, 0.0)).unwrap();
    let mut row = [[0.5; 3]; 4];
    adjustment.apply_row(&words, 0, 0, &mut row).unwrap();
    assert_eq!(adjustment.idle_instances(), 1, "one caller, one instance");
}

#[test]
fn equal_values_compile_to_equal_operations_and_the_words_are_the_identity() {
    let adjustment = WasmHost::new(None).load(&package(Variant::Valid)).unwrap();
    let units = adjustment.units();
    let at = CompileStage::exact(Stage {
        width: 4,
        height: 4,
    });
    let a = units.compile(&values(10.0, 1.0), at).unwrap();
    assert_eq!(a, units.compile(&values(10.0, 1.0), at).unwrap());
    assert_ne!(a, units.compile(&values(10.0, 2.0), at).unwrap());
}
