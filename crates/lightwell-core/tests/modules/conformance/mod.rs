//! The field-patch conformance suite: one set of checks every registered field-patch module passes.
//!
//! Basic, the colour mixer, Presence and the vignette share one implementation of everything but
//! their field tables and their compilation (`modules/field_patch.rs`), and the host behaviour they
//! rely on — discovery, drafts, no-ops, request deduplication, resets that keep a layer's identity,
//! one layer per target, history, sample-equals-render on both paths, unavailable providers and
//! reopen — is the host's. So it is proved here once, for every module the registry holds in the
//! field-patch shape ([`shape::field_patches`]), rather than once per module. A module's own
//! numerics against its frozen reference, its placement and its unique behaviour stay in its own
//! tests.
//!
//! The suite is written against the public API only and returns its evidence instead of
//! asserting, so the same code runs as the core's integration test
//! (`tests/modules/field_patch.rs`, in the dev profile) and inside `cargo xtask
//! editor-acceptance` (compiled into `xtask` through a `#[path]` module, in release), which writes
//! what it returns as the acceptance evidence. The two cannot diverge because they are one
//! function. The JSON client it drives the owner with is `lightwell_testkit::client`, the one
//! xtask's acceptance chapters use.
mod journey;
pub mod pixels;
mod rules;
pub mod shape;

pub use lightwell_testkit::client::{Checked, ensure, within};

use lightwell_core::ModuleRegistry;
use lightwell_testkit::client::request_id;
use serde_json::{Value, json};
use std::{fs, path::Path, time::Instant};

/// Who the suite's mutations name.
pub const ACTOR: &str = "field-patch-conformance";

/// The mutation envelope every asset change the suite makes carries, with a request identity new
/// to this process.
pub fn mutation(revision: u64, tag: &str) -> Value {
    lightwell_testkit::client::mutation(revision, &request_id(tag), ACTOR)
}

/// What one module's run showed, step by step.
#[derive(Default)]
pub struct Evidence {
    checks: Vec<Value>,
}

impl Evidence {
    pub fn record(&mut self, shows: &str, detail: Value) {
        self.checks.push(json!({"shows": shows, "detail": detail}));
    }
}

/// The built-in modules the suite must recognise. A new field-patch module needs no entry here to
/// be checked; this list only makes sure a descriptor change can never drop one of these from the
/// suite silently.
pub const KNOWN: [&str; 4] = [
    "lightwell.basic",
    "lightwell.presence",
    "lightwell.mixer",
    "lightwell.vignette",
];

/// Run the suite over every field-patch module of the built-in registry, each against its own new
/// catalog in `out`, on the JPEG `fixture`. Every module is run even when one fails, and the error
/// names each module that failed and the step and property that broke.
pub fn run(fixture: &Path, out: &Path) -> Checked<Value> {
    let started = Instant::now();
    let original = fs::read(fixture)
        .map_err(|error| format!("{} is unreadable: {error}", fixture.display()))?;
    let registry = ModuleRegistry::builtin();
    let modules = shape::field_patches(&registry);
    let found: Vec<&str> = modules.iter().map(|module| module.id.as_str()).collect();
    for known in KNOWN {
        ensure(
            found.contains(&known),
            format!(
                "{known} is no longer recognised as a field-patch module; the suite found {found:?}"
            ),
        )?;
    }
    let sources = pixels::Sources::open(fixture)?;
    // Each module has its own catalog and owner, so the modules run side by side; their results are
    // kept in registration order.
    let outcomes: Vec<Checked<Value>> = std::thread::scope(|scope| {
        let running: Vec<_> = modules
            .iter()
            .map(|module| {
                let (registry, sources) = (&registry, &sources);
                scope.spawn(move || check(registry, module, sources, fixture, out))
            })
            .collect();
        running
            .into_iter()
            .map(|thread| {
                thread
                    .join()
                    .unwrap_or_else(|_| Err("the module's check panicked".to_owned()))
            })
            .collect()
    });
    let mut results = Vec::with_capacity(modules.len());
    let mut failures = Vec::new();
    for (module, outcome) in modules.iter().zip(outcomes) {
        match outcome {
            Ok(evidence) => results.push(evidence),
            Err(error) => failures.push(format!("{}: {error}", module.id)),
        }
    }
    ensure(
        fs::read(fixture).ok().as_deref() == Some(original.as_slice()),
        "the original source changed",
    )?;
    if !failures.is_empty() {
        return Err(format!(
            "{} of {} field-patch modules failed conformance:\n{}",
            failures.len(),
            modules.len(),
            failures.join("\n")
        ));
    }
    Ok(json!({
        "status": "passed",
        "modules": found,
        "results": results,
        "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
    }))
}

/// Every check for one module: the payload rules in process, then the whole editing journey
/// through the owner, the same catalog with the module unavailable, and the catalog reopened.
fn check(
    registry: &ModuleRegistry,
    module: &shape::FieldPatch,
    sources: &pixels::Sources,
    fixture: &Path,
    out: &Path,
) -> Checked<Value> {
    let started = Instant::now();
    let mut evidence = Evidence::default();
    let payloads = within("payloads", || pixels::payloads(registry, module, sources))?;
    evidence.record(
        "every neutral spelling compiles to nothing, is reported neutral and Neutral, renders the shared source allocation and changes no byte on the linear path; each field alone and each whole payload has exactly the consequences of the module's own neutrality rule",
        payloads,
    );
    let rules = within("the field-patch rules", || {
        rules::rules(registry, module, sources.stage())
    })?;
    evidence.record(
        "every field reads at both ends of its range and in an integer spelling while a value just outside it, a string, a non-object, another effect and an undeclared format are refused by name; a patch commits only a non-neutral layer holding the canonical payload, merges over the stored layer in place, is a no-op when it changes nothing however spelled, drops a field set back to its default, and a reset keeps the layer and stores {}; two layers for one target refuse planning by name; labels and descriptions follow the declared rules and values fill every default",
        rules,
    );
    let refusals = within("stored payloads", || {
        pixels::stored_refusals(registry, module, sources)
    })?;
    evidence.record(
        "a stored layer of an undeclared format is incompatible and is not rendered, an unknown field, an out-of-range value and a non-object payload are refused by name, and nothing is rewritten",
        refusals,
    );
    let ambiguous = within("two global layers", || {
        let first = pixels::layer(module, &module.full_high());
        let second = pixels::layer(module, &module.full_low());
        pixels::refuses_as_ambiguous(
            registry,
            module,
            sources,
            &pixels::stack(vec![first, second]),
        )
    })?;
    evidence.record(
        "a stack holding two global layers of the effect is refused by rendering and sampling with ambiguous <title> layers and stays readable",
        ambiguous,
    );

    let catalog = out.join(format!("{}-conformance.sqlite", module.id));
    ensure(
        !catalog.exists(),
        format!(
            "{} already exists; the suite needs a new catalog",
            catalog.display()
        ),
    )?;
    let state = journey::journey(module, sources, fixture, &catalog, &mut evidence)?;
    let unavailable = within("an unavailable provider", || {
        journey::unavailable(module, &catalog, &state)
    })?;
    evidence.record(
        "the same catalog served with the module unavailable keeps every layer stored and readable, lists the module unavailable, and refuses sampling, analysis and a new edit by name",
        unavailable,
    );
    let reopened = within("reopen", || journey::reopen(&catalog, &state))?;
    evidence.record(
        "the catalog reopened returns the same revision, entry, layer and mask identities, rows, pixels and analysis identity",
        reopened,
    );
    Ok(json!({
        "module": module.id,
        "effect": module.effect.id,
        "stage": module.effect.stage,
        "order": module.effect.order,
        "maskable": module.effect.maskable,
        "set": module.set,
        "reset": module.reset,
        "fields": module.fields.iter().map(|field| field.name.clone()).collect::<Vec<_>>(),
        "catalog": catalog.file_name().map(|name| name.to_string_lossy().into_owned()),
        "checks": evidence.checks,
        "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
    }))
}
