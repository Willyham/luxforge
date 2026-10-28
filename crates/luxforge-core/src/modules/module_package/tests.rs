//! Packages, manifests and the module a manifest's table makes, over fixture packages built here.
use super::*;
use crate::modules::field_patch::{
    Field, FieldControl, FieldPatch, FieldPatchModule, Group, Spec, Values, Wheel,
};
use crate::modules::{
    ActionInput, ActionPlan, ChoiceStyle, ColorOperation, ColorStyle, EffectStage, FixedStage,
    NumberStyle, ParameterDescriptor, Processing, RailDecoration, ToolModule,
};
use crate::{CompileStage, Error, ModuleRegistry, Stage};
use serde_json::json;
use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};
use zip::write::SimpleFileOptions;

/// The smallest component preamble: the WebAssembly magic, version 0x0d and layer 1.
const COMPONENT: &[u8] = b"\0asm\x0d\x00\x01\x00";
const PROGRAM: &str = "fn lf_module(rgb: vec3<f32>) -> vec3<f32> { return rgb; }\n";

/// A split-toning manifest with a field of every kind the table holds, every control style it
/// declares, a rail, a wheel and three groups.
const MANIFEST: &str = r#"
api = "0.1"
id = "org.example.splittoning"
name = "split-toning"
title = "Split toning"
hint = "Tint the highlights and the shadows"
version = "1.2.0"
authors = ["Example Author <author@example.org>"]
license = "MIT OR Apache-2.0"
repository = "https://example.org/split-toning"
toolset = "adjustments"

[effect]
stage = "color"
order = 20
format = 1

[[fields]]
name = "highlight-hue"
kind = "number"
min = 0
max = 360
default = 0
step = 1
precision = 0
unit = "°"
label = "Hue"
history = "Highlight hue"
notes = "the highlights' tint, in degrees"

[[fields]]
name = "highlight-saturation"
kind = "number"
min = 0
max = 100
default = 0
label = "Saturation"
history = "Highlight saturation"

[[fields]]
name = "shadow-hue"
kind = "number"
min = 0
max = 360
default = 240
rail = "hue"
label = "Hue"
history = "Shadow hue"

[[fields]]
name = "shadow-saturation"
kind = "number"
min = 0.0
max = 100.0
default = 0.0
soft_max = 50.0
fine_step = 0.1
label = "Saturation"
history = "Shadow saturation"

[[fields]]
name = "balance"
kind = "number"
min = -100
max = 100
default = 0
zero = 0
style = "field"
label = "Balance"

[[fields]]
name = "steps"
kind = "integer"
min = 1
max = 8
default = 4
style = "stepper"
label = "Steps"

[[fields]]
name = "preserve"
kind = "boolean"
default = true
label = "Preserve luminance"

[[fields]]
name = "mode"
kind = "enum"
options = ["soft", "hard"]
default = "soft"
style = "segmented"
label = "Mode"

[[fields]]
name = "paper"
kind = "color"
default = [255, 250, 240]
style = "picker"
label = "Paper"

[[groups]]
label = "Highlights"
fields = ["highlight-hue", "highlight-saturation"]

[[groups.wheels]]
label = "Highlights"
hue = "highlight-hue"
saturation = "highlight-saturation"

[[groups]]
label = "Shadows"
fields = ["shadow-hue", "shadow-saturation"]

[[groups]]
label = "Blend"
fields = ["balance", "steps", "preserve", "mode", "paper"]
"#;

/// The same table written by hand as a built-in field-patch module would write it.
#[derive(Default)]
struct HandWritten;

impl FieldPatch for HandWritten {
    fn spec() -> Spec {
        Spec::named(
            "org.example.splittoning",
            "split-toning",
            "Split toning",
            "Tint the highlights and the shadows",
            "org.example.splittoning.adjust",
            EffectStage::Color,
        )
        .format(1)
        .order(20)
        .fields([
            Field::new(
                ParameterDescriptor::number("highlight-hue", 0.0, 360.0)
                    .default(0)
                    .step(1.0)
                    .precision(0)
                    .unit("°")
                    .notes("the highlights' tint, in degrees"),
                "Hue",
            )
            .history("Highlight hue"),
            Field::new(
                ParameterDescriptor::number("highlight-saturation", 0.0, 100.0).default(0),
                "Saturation",
            )
            .history("Highlight saturation"),
            Field::new(
                ParameterDescriptor::number("shadow-hue", 0.0, 360.0).default(240),
                "Hue",
            )
            .history("Shadow hue")
            .rail(RailDecoration::Hue),
            Field::new(
                ParameterDescriptor::number("shadow-saturation", 0.0, 100.0)
                    .default(0.0)
                    .soft_max(50.0)
                    .fine_step(0.1),
                "Saturation",
            )
            .history("Shadow saturation"),
            Field::new(
                ParameterDescriptor::number("balance", -100.0, 100.0)
                    .default(0)
                    .zero(0.0),
                "Balance",
            )
            .control(FieldControl::Number(NumberStyle::Field)),
            Field::new(
                ParameterDescriptor::integer("steps", 1, 8).default(4),
                "Steps",
            )
            .control(FieldControl::Number(NumberStyle::Stepper)),
            Field::new(
                ParameterDescriptor::boolean("preserve").default(true),
                "Preserve luminance",
            ),
            Field::new(
                ParameterDescriptor::enumeration("mode", ["soft", "hard"]).default("soft"),
                "Mode",
            )
            .control(FieldControl::Choice(ChoiceStyle::Segmented)),
            Field::new(
                ParameterDescriptor::color("paper").default(json!([255, 250, 240])),
                "Paper",
            )
            .control(FieldControl::Color(ColorStyle::Picker)),
        ])
        .group(
            Group::new("Highlights", ["highlight-hue", "highlight-saturation"]).wheel(Wheel::new(
                "Highlights",
                "highlight-hue",
                "highlight-saturation",
            )),
        )
        .group(Group::new("Shadows", ["shadow-hue", "shadow-saturation"]))
        .group(Group::new(
            "Blend",
            ["balance", "steps", "preserve", "mode", "paper"],
        ))
    }

    fn compile(&self, _: &Values<'_>, _: CompileStage) -> Result<Processing, Error> {
        Err(Error::validation("the hand-written table compiles nothing"))
    }
}

/// A zip of these entries, each Deflate-compressed.
fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(*name, SimpleFileOptions::default())
            .expect("an entry");
        zip.write_all(bytes).expect("its bytes");
    }
    zip.finish().expect("the archive").into_inner()
}

/// A package of this manifest, the component and the program.
fn package_of(manifest: &str) -> Vec<u8> {
    zip_of(&[
        (MANIFEST_FILE, manifest.as_bytes()),
        (COMPONENT_FILE, COMPONENT),
        (PROGRAM_FILE, PROGRAM.as_bytes()),
    ])
}

/// The manifest with `from` replaced by `to`, which must occur in it.
fn changed(from: &str, to: &str) -> String {
    assert!(MANIFEST.contains(from), "the manifest holds {from:?}");
    MANIFEST.replacen(from, to, 1)
}

/// The refusal reading `bytes` gives, and its detail.
fn refusal(bytes: &[u8]) -> (PackageRefusal, String) {
    let error = ModulePackage::read(bytes).expect_err("the package is refused");
    let refusal =
        PackageRefusal::of(&error).unwrap_or_else(|| panic!("a package refusal, not {error:?}"));
    (refusal, error.detail)
}

#[test]
fn a_module_package_reads_into_its_manifest_component_program_and_hash() {
    let bytes = package_of(MANIFEST);
    let package = ModulePackage::read(&bytes).expect("the package reads");
    let manifest = &package.manifest;
    assert_eq!(
        (
            manifest.api.as_str(),
            manifest.id.as_str(),
            manifest.name.as_str(),
            manifest.title.as_str(),
            manifest.version.as_str(),
            manifest.license.as_str(),
            manifest.repository.as_str(),
            manifest.toolset.as_str(),
        ),
        (
            MODULE_API,
            "org.example.splittoning",
            "split-toning",
            "Split toning",
            "1.2.0",
            "MIT OR Apache-2.0",
            "https://example.org/split-toning",
            "adjustments",
        )
    );
    assert_eq!(manifest.authors, ["Example Author <author@example.org>"]);
    assert_eq!(
        manifest.effect,
        ManifestEffect {
            id: "org.example.splittoning.adjust".into(),
            stage: EffectStage::Color,
            order: 20,
            format: 1,
        }
    );
    assert_eq!(package.component, COMPONENT);
    assert_eq!(package.program.as_deref(), Some(PROGRAM));
    assert_eq!(
        package.hash,
        format!("sha256:{:x}", Sha256::digest(&bytes)),
        "the hash is the package file's"
    );
    // The program is optional.
    let without = zip_of(&[
        (MANIFEST_FILE, MANIFEST.as_bytes()),
        (COMPONENT_FILE, COMPONENT),
    ]);
    assert_eq!(ModulePackage::read(&without).unwrap().program, None);
}

/// The manifest's table builds the descriptor a hand-written field-patch module of the same table
/// builds, action for action and control for control, and the module an engine's units make of
/// it publishes that descriptor and registers.
#[test]
fn a_module_package_table_matches_a_hand_written_field_patch_module() {
    let package = ModulePackage::read(&package_of(MANIFEST)).unwrap();
    let hand = FieldPatchModule::<HandWritten>::new();
    let expected = serde_json::to_value(hand.descriptor()).unwrap();
    assert_eq!(
        serde_json::to_value(package.manifest.descriptor()).unwrap(),
        expected
    );
    let module = package
        .manifest
        .module(Arc::new(Recording::default()))
        .unwrap();
    assert_eq!(serde_json::to_value(module.descriptor()).unwrap(), expected);
    let actions: Vec<&str> = module
        .descriptor()
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert_eq!(actions, ["set-split-toning", "reset-split-toning"]);
    let mut registry = ModuleRegistry::builtin();
    registry.register(module).expect("the module registers");
}

/// The units an engine would supply, recording the values each compile was handed.
#[derive(Default)]
struct Recording {
    compiled: Mutex<Vec<Vec<FieldEntry>>>,
}

impl AdjustmentUnits for Recording {
    fn compile(&self, values: &[FieldEntry], _: CompileStage) -> Result<ColorOperation, Error> {
        self.compiled.lock().unwrap().push(values.to_vec());
        Ok(ColorOperation::new(Vec::new()))
    }
}

/// A patch plans as a built-in's does, and the engine is handed every field in the manifest's
/// order with the stored payload's missing keys at their defaults; a neutral layer never reaches
/// it.
#[test]
fn a_module_package_module_plans_patches_and_hands_its_units_every_value() {
    let package = ModulePackage::read(&package_of(MANIFEST)).unwrap();
    let units = Arc::new(Recording::default());
    let module = package.manifest.module(units.clone()).unwrap();
    let registry = ModuleRegistry::new();
    let stage = FixedStage::new(Stage {
        width: 4,
        height: 4,
    });
    let input = ActionInput {
        action_id: "set-split-toning".into(),
        parameters: json!({"balance": 30, "mode": "hard"})
            .as_object()
            .cloned()
            .unwrap(),
    };
    let ActionPlan::Commit(layer) = module.plan(&input, &stage.context(&[], &registry)).unwrap()
    else {
        panic!("a first non-neutral patch commits a layer");
    };
    assert_eq!(layer.effect_id, "org.example.splittoning.adjust");
    assert_eq!(layer.payload, json!({"balance": 30.0, "mode": "hard"}));
    let at = CompileStage::exact(Stage {
        width: 4,
        height: 4,
    });
    let effect = &package.manifest.effect;
    module
        .compile(&effect.id, effect.format, &json!({}), at)
        .expect("a neutral layer compiles");
    assert!(
        units.compiled.lock().unwrap().is_empty(),
        "neutral never reaches the units"
    );
    module
        .compile(&effect.id, effect.format, &layer.payload, at)
        .expect("the layer compiles");
    let number = |name: &str, value: f64| FieldEntry {
        name: name.into(),
        value: FieldValue::Number(value),
    };
    assert_eq!(
        units.compiled.lock().unwrap().as_slice(),
        [vec![
            number("highlight-hue", 0.0),
            number("highlight-saturation", 0.0),
            number("shadow-hue", 240.0),
            number("shadow-saturation", 0.0),
            number("balance", 30.0),
            FieldEntry {
                name: "steps".into(),
                value: FieldValue::Integer(4),
            },
            FieldEntry {
                name: "preserve".into(),
                value: FieldValue::Boolean(true),
            },
            FieldEntry {
                name: "mode".into(),
                value: FieldValue::Choice("hard".into()),
            },
            FieldEntry {
                name: "paper".into(),
                value: FieldValue::Color([255, 250, 240]),
            },
        ]]
    );
    // A stored layer of another format is refused by its marker, as a built-in's is.
    let Err(error) = module.compile(&effect.id, 2, &layer.payload, at) else {
        panic!("another format is refused");
    };
    assert_eq!(error.detail, "unsupported effect format 2");
}

/// Patch the compression method of the archive's only entry to `method`, in its local header and
/// its central directory record.
fn with_method(mut bytes: Vec<u8>, method: u16) -> Vec<u8> {
    let central = bytes
        .windows(4)
        .position(|window| window == [0x50, 0x4b, 0x01, 0x02])
        .expect("a central directory record");
    bytes[8..10].copy_from_slice(&method.to_le_bytes());
    bytes[central + 10..central + 12].copy_from_slice(&method.to_le_bytes());
    bytes
}

/// Every archive-level refusal names its reason: size, entries, names outside the root, other
/// entries, links, duplicates, missing files, compression and content.
#[test]
fn a_module_package_archive_is_refused_with_its_reason() {
    let manifest = MANIFEST.as_bytes();
    let oversized_manifest = format!("{MANIFEST}#{}\n", "x".repeat(MAX_MANIFEST_BYTES as usize));
    let mut symlink = zip::ZipWriter::new(Cursor::new(Vec::new()));
    symlink
        .start_file(MANIFEST_FILE, SimpleFileOptions::default())
        .unwrap();
    symlink.write_all(manifest).unwrap();
    symlink
        .add_symlink(COMPONENT_FILE, "/etc/passwd", SimpleFileOptions::default())
        .unwrap();
    let symlink = symlink.finish().unwrap().into_inner();
    let duplicate = zip_of(&[
        (MANIFEST_FILE, manifest),
        (COMPONENT_FILE, COMPONENT),
        ("module.wasX", COMPONENT),
    ]);
    let duplicate = {
        let mut bytes = duplicate;
        let from = b"module.wasX";
        while let Some(at) = bytes.windows(from.len()).position(|window| window == from) {
            bytes[at..at + from.len()].copy_from_slice(COMPONENT_FILE.as_bytes());
        }
        bytes
    };
    let stored = {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(
            MANIFEST_FILE,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(manifest).unwrap();
        zip.finish().unwrap().into_inner()
    };
    let cases: Vec<(Vec<u8>, PackageRefusal, &str)> = vec![
        (
            vec![0; MAX_PACKAGE_BYTES as usize + 1],
            PackageRefusal::TooLarge,
            "more than the 25165824 a package may hold",
        ),
        (
            b"not a zip".to_vec(),
            PackageRefusal::Archive,
            "it has no end of central directory record",
        ),
        (
            zip_of(&[
                (MANIFEST_FILE, oversized_manifest.as_bytes()),
                (COMPONENT_FILE, COMPONENT),
            ]),
            PackageRefusal::TooLarge,
            "luxforge-module.toml is",
        ),
        (
            zip_of(&[
                (MANIFEST_FILE, manifest),
                (COMPONENT_FILE, COMPONENT),
                (PROGRAM_FILE, PROGRAM.as_bytes()),
                ("README.md", b"read me"),
            ]),
            PackageRefusal::Entries,
            "the package has 4 entries; a package holds at most 3",
        ),
        (
            zip_of(&[(MANIFEST_FILE, manifest), ("../module.wasm", COMPONENT)]),
            PackageRefusal::OutsideRoot,
            "package entry \"../module.wasm\" is outside the package root",
        ),
        (
            zip_of(&[(MANIFEST_FILE, manifest), ("/module.wasm", COMPONENT)]),
            PackageRefusal::OutsideRoot,
            "package entry \"/module.wasm\" is outside the package root",
        ),
        (
            zip_of(&[
                (MANIFEST_FILE, manifest),
                ("lib/../../module.wasm", COMPONENT),
            ]),
            PackageRefusal::OutsideRoot,
            "is outside the package root",
        ),
        (
            zip_of(&[(MANIFEST_FILE, manifest), ("README.md", b"read me")]),
            PackageRefusal::UnexpectedEntry,
            "package entry \"README.md\" is not part of a module package",
        ),
        (
            zip_of(&[(MANIFEST_FILE, manifest), ("lib/module.wasm", COMPONENT)]),
            PackageRefusal::UnexpectedEntry,
            "package entry \"lib/module.wasm\" is not part of a module package",
        ),
        (
            symlink,
            PackageRefusal::UnexpectedEntry,
            "package entry module.wasm is a link or a directory, not a file",
        ),
        (
            duplicate,
            PackageRefusal::Entries,
            "the package names one entry more than once",
        ),
        (
            zip_of(&[(COMPONENT_FILE, COMPONENT)]),
            PackageRefusal::MissingFile,
            "the package has no luxforge-module.toml",
        ),
        (
            zip_of(&[(MANIFEST_FILE, manifest)]),
            PackageRefusal::MissingFile,
            "the package has no module.wasm",
        ),
        (
            with_method(stored, 12),
            PackageRefusal::Archive,
            "not stored or Deflate",
        ),
        (
            zip_of(&[
                (MANIFEST_FILE, manifest),
                (COMPONENT_FILE, b"\0asm\x01\x00\x00\x00"),
            ]),
            PackageRefusal::Content,
            "module.wasm is not a WebAssembly component",
        ),
        (
            zip_of(&[
                (MANIFEST_FILE, manifest),
                (COMPONENT_FILE, COMPONENT),
                (PROGRAM_FILE, b"\xff\xfe"),
            ]),
            PackageRefusal::Content,
            "program.wgsl is not UTF-8 text",
        ),
    ];
    for (bytes, expected, words) in cases {
        let (refused, detail) = refusal(&bytes);
        assert_eq!(refused, expected, "{detail}");
        assert!(detail.contains(words), "{detail:?} says {words:?}");
    }
}

/// Every manifest refusal names its reason: the api, the identity, Luxforge's namespace, the
/// toolset, the stage, maskability, source kinds, metadata and a table the host could not draw.
#[test]
fn a_module_package_manifest_is_refused_with_its_reason() {
    use PackageRefusal::*;
    let cases: Vec<(String, PackageRefusal, &str)> = vec![
        (
            changed("api = \"0.1\"", "api = \"0.2\""),
            Api,
            "api 0.2 is not offered by this build, which offers 0.1",
        ),
        (
            changed(
                "toolset = \"adjustments\"",
                "toolset = \"adjustments\"\ncolour = 1",
            ),
            Manifest,
            "unknown field `colour`",
        ),
        ("api = \"0.1\"\nid = [".into(), Manifest, "not TOML"),
        (
            changed("org.example.splittoning", "example.splittoning"),
            Identity,
            "id \"example.splittoning\" is not a reverse-DNS module identity",
        ),
        (
            changed("org.example.splittoning", "org.example.split-toning"),
            Identity,
            "is not a reverse-DNS module identity",
        ),
        (
            changed("org.example.splittoning", "Org.Example.SplitToning"),
            Identity,
            "is not a reverse-DNS module identity",
        ),
        (
            changed("org.example.splittoning", "luxforge.splittoning.tone"),
            ReservedIdentity,
            "id luxforge.splittoning.tone is in Luxforge's own namespace",
        ),
        (
            changed("org.example.splittoning", "app.luxforge.splittoning"),
            ReservedIdentity,
            "in Luxforge's own namespace",
        ),
        (
            changed("name = \"split-toning\"", "name = \"Split toning\""),
            Identity,
            "name \"Split toning\" is not an action noun",
        ),
        (
            changed("toolset = \"adjustments\"", "toolset = \"corrections\""),
            Toolset,
            "toolset corrections is not open to external modules, only adjustments",
        ),
        (
            changed("toolset = \"adjustments\"", "toolset = \"panel\""),
            Toolset,
            "unknown toolset \"panel\"",
        ),
        (
            changed("stage = \"color\"", "stage = \"spatial\""),
            Stage,
            "an external effect is on the color or finish stage, not spatial",
        ),
        (
            changed("stage = \"color\"", "stage = \"glow\""),
            Stage,
            "unknown stage \"glow\"",
        ),
        (
            changed("format = 1", "format = 1\nmaskable = true"),
            Maskable,
            "an external effect is not maskable",
        ),
        (
            changed("format = 1", "format = 1\nsources = [\"raw\"]"),
            Sources,
            "declares no sources",
        ),
        (
            changed("format = 1", "format = 0"),
            Manifest,
            "a payload format starts at 1",
        ),
        (
            changed("version = \"1.2.0\"", "version = \"1.2\""),
            Manifest,
            "version \"1.2\" is not a SemVer version",
        ),
        (
            changed("license = \"MIT OR Apache-2.0\"", "license = \"GPL 3\""),
            Manifest,
            "license \"GPL 3\" is not an SPDX licence expression",
        ),
        (
            changed("https://example.org", "http://example.org"),
            Manifest,
            "not https",
        ),
        (
            changed("https://example.org", "https://user:secret@example.org"),
            Manifest,
            "it holds credentials",
        ),
        (
            changed(
                "authors = [\"Example Author <author@example.org>\"]",
                "authors = []",
            ),
            Manifest,
            "authors names 0 people",
        ),
        (
            changed("title = \"Split toning\"", "title = \"  \""),
            Manifest,
            "title is empty",
        ),
        // Tables the host could not draw.
        (
            changed(
                "label = \"Preserve luminance\"",
                "label = \"Preserve luminance\"\nrail = \"hue\"",
            ),
            Table,
            "field preserve: its toggle control cannot carry a rail",
        ),
        (
            changed(
                "fields = [\"shadow-hue\", \"shadow-saturation\"]",
                "fields = [\"shadow-hue\", \"shadow-saturaton\"]",
            ),
            Table,
            "group Shadows lists shadow-saturaton, which is not a declared field",
        ),
        (
            changed(
                "fields = [\"shadow-hue\", \"shadow-saturation\"]",
                "fields = [\"shadow-hue\"]",
            ),
            Table,
            "field shadow-saturation is in no group",
        ),
        (
            changed("kind = \"integer\"", "kind = \"curve\""),
            Table,
            "field steps: kind \"curve\" is not one of number, integer, boolean, enum and color",
        ),
        (
            changed("max = 360\ndefault = 0", "max = 180\ndefault = 0"),
            Table,
            "not the 0..=360 degrees a wheel's angle spans",
        ),
        (
            changed("default = \"soft\"", "default = \"medium\""),
            Table,
            "field mode's default is invalid",
        ),
        (
            changed("style = \"segmented\"", "style = \"slider\""),
            Table,
            "field mode: a choice control has no style \"slider\"",
        ),
        (
            changed("default = true", "default = true\nmin = 0"),
            Table,
            "field preserve: a boolean field declares no min",
        ),
        (
            changed("max = 8", "max = 0"),
            Table,
            "field steps's default is invalid",
        ),
        (
            MANIFEST.replace("\"balance\"", "\"Balance\""),
            Table,
            "invalid parameter name Balance of action set-split-toning",
        ),
    ];
    for (manifest, expected, words) in cases {
        let (refused, detail) = refusal(&package_of(&manifest));
        assert_eq!(refused, expected, "{detail}");
        assert!(detail.contains(words), "{detail:?} says {words:?}");
    }
}

/// Reading a package from a file, accepted or refused, writes nothing: the directory it is in
/// holds what it held, byte for byte.
#[test]
fn reading_a_module_package_file_writes_nothing() {
    let directory = luxforge_testbase::paths::temp_dir("module-package");
    let valid = directory.join("split-toning.lfmodule");
    let refused = directory.join("refused.lfmodule");
    std::fs::write(&valid, package_of(MANIFEST)).unwrap();
    std::fs::write(
        &refused,
        zip_of(&[
            (MANIFEST_FILE, MANIFEST.as_bytes()),
            ("../module.wasm", COMPONENT),
        ]),
    )
    .unwrap();
    let listing = || -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut entries: Vec<_> = walk(&directory)
            .into_iter()
            .map(|path| {
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        entries.sort();
        entries
    };
    let before = listing();
    assert!(ModulePackage::read_file(&valid).is_ok());
    assert_eq!(
        PackageRefusal::of(&ModulePackage::read_file(&refused).unwrap_err()),
        Some(PackageRefusal::OutsideRoot)
    );
    assert!(
        ModulePackage::read_file(&directory).is_err(),
        "a directory is no package"
    );
    assert_eq!(listing(), before);
    std::fs::remove_dir_all(&directory).unwrap();
}

/// Every file under `directory`, recursively.
fn walk(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(directory)
        .unwrap()
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

/// The WIT world both the engine and the SDK generate their bindings from declares the version the
/// manifest's `api` names and exports one interface of the three functions, and imports nothing:
/// its types live in that interface, since a type declared in the world itself would be encoded as
/// a type import.
#[test]
fn the_module_package_wit_world_exports_three_functions_and_imports_nothing() {
    let path = luxforge_testbase::paths::repository().join("wit/adjustment.wit");
    let text = std::fs::read_to_string(&path).expect("the WIT file");
    let code: Vec<&str> = text
        .lines()
        .map(|line| line.split("//").next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .collect();
    assert!(code.contains(&format!("package luxforge:module@{MODULE_API}.0;").as_str()));
    let functions: Vec<&str> = code
        .iter()
        .filter_map(|line| line.split_once(": func(").map(|(name, _)| name))
        .collect();
    assert_eq!(functions, ["coefficients", "apply-row", "label"]);
    let world = code
        .iter()
        .position(|line| *line == "world adjustment {")
        .expect("the adjustment world");
    assert_eq!(
        code[world + 1..],
        ["export adjust;", "}"],
        "the world exports adjust alone"
    );
    assert!(code.contains(&"interface adjust {"));
    assert!(
        !code.iter().any(|line| {
            ["import ", "use ", "include "]
                .iter()
                .any(|word| line.starts_with(word))
        }),
        "nothing is imported"
    );
}

#[test]
fn a_module_package_refusal_code_reads_back() {
    for refusal in ALL {
        let error = refusal.error("detail");
        assert_eq!(PackageRefusal::of(&error), Some(refusal));
        assert_eq!(error.kind, crate::ErrorKind::Validation);
    }
    assert_eq!(PackageRefusal::of(&Error::validation("plain")), None);
}
