//! External modules in the one assembly ([`RegistryOptions::external`]): registered after every
//! linked module, unavailable with the loader's reason, refused by name when they collide, and
//! changing nothing the linked modules' order decides.
use super::*;
use crate::catalog_types::HeaderMetadata;
use crate::editor::{RawInterpretation, SourceKind, original_recipe, synthetic_raw_metadata};
use crate::modules::module_package::{AdjustmentUnits, FieldEntry, ModuleManifest};
use crate::modules::{Availability, ColorOperation};
use crate::{CompileStage, EditorService, LayerId, OriginalPreferences, Recipe};
use luxforge_test_macros::slow;
use luxforge_testbase::paths::{jpeg, temp_dir};
use serde_json::Value;

/// A one-field manifest whose module is `id`, named by its last segment.
fn manifest(id: &str) -> ModuleManifest {
    ModuleManifest::parse(&format!(
        r#"
api = "0.1"
id = "{id}"
title = "Glow"
hint = "Glow the picture"
version = "1.0.0"
authors = ["Example"]
license = "MIT"
repository = "https://example.org/glow"
toolset = "adjustments"

[effect]
stage = "color"
format = 1

[[fields]]
name = "amount"
kind = "number"
min = -100
max = 100
default = 0
label = "Amount"

[[groups]]
label = "Glow"
fields = ["amount"]
"#
    ))
    .expect("the manifest reads")
}

/// Units that compile every layer to no unit: what loads, for a registry test.
struct Neutral;

impl AdjustmentUnits for Neutral {
    fn compile(&self, _: &[FieldEntry], _: CompileStage) -> Result<ColorOperation, Error> {
        Ok(ColorOperation::new(Vec::new()))
    }
}

/// A loaded external, an unloaded one, one whose action is a linked module's (`set-vignette`) and
/// one that repeats the first's identity.
fn externals() -> Vec<ExternalModule> {
    vec![
        ExternalModule::loaded(&manifest("org.example.glow"), Arc::new(Neutral)).unwrap(),
        ExternalModule::unavailable(&manifest("org.example.halo"), "the component traps").unwrap(),
        ExternalModule::loaded(&manifest("org.example.vignette"), Arc::new(Neutral)).unwrap(),
        ExternalModule::loaded(&manifest("org.example.glow"), Arc::new(Neutral)).unwrap(),
    ]
}

fn with_externals(external: &[ExternalModule]) -> ModuleRegistry {
    ModuleRegistry::assemble(&RegistryOptions {
        external,
        ..RegistryOptions::default()
    })
    .expect("an assembly never fails because of a package")
}

fn ids(registry: &ModuleRegistry) -> Vec<String> {
    registry
        .descriptors()
        .iter()
        .map(|descriptor| descriptor.id.clone())
        .collect()
}

#[test]
fn registry_assembly_registers_externals_after_the_linked_modules_and_refuses_collisions() {
    let external = externals();
    let registry = with_externals(&external);
    let mut expected = ids(&ModuleRegistry::builtin());
    expected.extend(["org.example.glow".into(), "org.example.halo".into()]);
    assert_eq!(
        ids(&registry),
        expected,
        "linked order first, externals after"
    );
    let (glow, _) = registry
        .action("set-glow")
        .expect("a loaded external's action");
    assert_eq!(glow.descriptor().availability, Availability::Available);
    let halo = registry.module("org.example.halo").expect("listed");
    assert_eq!(
        halo.descriptor().availability,
        Availability::Unavailable {
            reason: "the component traps".into()
        }
    );
    let refused = registry.refused_externals();
    assert_eq!(
        refused
            .iter()
            .map(|refused| refused.id.as_str())
            .collect::<Vec<_>>(),
        ["org.example.vignette", "org.example.glow"]
    );
    assert_eq!(
        refused[0].reason,
        "action set-vignette is already provided by luxforge.vignette"
    );
    assert_eq!(refused[1].reason, "duplicate module org.example.glow");
    // `--disable-module` reaches an external as it does a linked module.
    let disabled = ["org.example.glow".to_owned()];
    let registry = ModuleRegistry::assemble(&RegistryOptions {
        disabled: &disabled,
        external: &external[..1],
        ..RegistryOptions::default()
    })
    .unwrap();
    assert_eq!(
        registry
            .module("org.example.glow")
            .unwrap()
            .descriptor()
            .availability,
        Availability::Unavailable {
            reason: DISABLED_REASON.into()
        }
    );
}

/// The Original's layers of a JPEG and a RAW, with layer identities in building order.
fn originals(registry: &ModuleRegistry) -> Vec<Recipe> {
    let raw = SourceKind::Raw {
        metadata: RawInterpretation::new(synthetic_raw_metadata()).unwrap(),
    };
    [SourceKind::Jpeg, raw]
        .iter()
        .map(|source| {
            let mut next = 0;
            original_recipe(
                registry,
                source,
                &HeaderMetadata::default(),
                OriginalPreferences::default(),
                || {
                    next += 1;
                    LayerId::parse(format!("{}original-{next}", LayerId::PREFIX))
                },
            )
            .expect("the Original builds")
        })
        .collect()
}

#[test]
fn registry_with_externals_builds_the_same_originals() {
    let external = externals();
    assert_eq!(
        originals(&with_externals(&external)),
        originals(&ModuleRegistry::builtin())
    );
}

/// The action and the layers' effects and payloads of every entry an import of the JPEG fixture
/// commits: its Original and any first-open action.
fn first_open(registry: ModuleRegistry, label: &str) -> Vec<(String, Vec<(String, Value)>)> {
    let dir = temp_dir(label);
    let source = dir.join("photo.jpg");
    std::fs::copy(jpeg(), &source).unwrap();
    let mut service = EditorService::open_with(&dir.join("catalog.sqlite"), Arc::new(registry))
        .expect("the catalog opens");
    let state = service.import(&source).expect("the photo imports");
    let entries = service
        .history(&state.asset.id, None, 10)
        .unwrap()
        .entries
        .into_iter()
        .map(|row| {
            let entry = service.entry(&state.asset.id, &row.id).unwrap();
            let layers = entry
                .snapshot
                .recipe
                .layers
                .into_iter()
                .map(|layer| (layer.effect_id, layer.payload))
                .collect();
            (row.action_id, layers)
        })
        .collect();
    drop(service);
    std::fs::remove_dir_all(dir).unwrap();
    entries
}

#[test]
#[slow]
fn registry_with_externals_commits_the_same_first_open_actions() {
    let external = externals();
    assert_eq!(
        first_open(with_externals(&external), "registry-external-first-open"),
        first_open(ModuleRegistry::builtin(), "registry-builtin-first-open")
    );
}
