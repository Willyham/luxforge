//! `cargo xtask module-package DIR --output FILE`: an external adjustment module's crate built into
//! its `.lfmodule` package (`docs/design/module-manager.md#authoring`,
//! `docs/engineering/module-authoring.md`).
//!
//! `DIR` holds the module crate (`Cargo.toml`, a `cdylib` on `luxforge-module-sdk`), its
//! `luxforge-module.toml` and, optionally, its `program.wgsl`. The manifest is checked first, so a
//! table the host would refuse fails before anything is built. The crate is built for
//! `wasm32-unknown-unknown` in release with the pinned toolchain, into `target/modules`, and its
//! core module made a component of the `adjustment` world. The package is written with its three
//! entries at the root, Deflate, at a fixed time, read back through the core's package reader, and
//! only then moved over `FILE`.
use crate::*;
use luxforge_core::module_package::{
    COMPONENT_FILE, MANIFEST_FILE, ModuleManifest, ModulePackage, PROGRAM_FILE,
};
use std::io::{Cursor, Write};

/// Where module crates are built, under the repository's target directory.
pub(crate) fn target_dir(root: &Path) -> PathBuf {
    root.join("target/modules")
}

/// The package's bytes: the three files, in a fixed order, each Deflate at the zip epoch, so the
/// same inputs make the same package.
pub(crate) fn archive(
    manifest: &[u8],
    component: &[u8],
    program: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut entries = vec![(MANIFEST_FILE, manifest), (COMPONENT_FILE, component)];
    entries.extend(program.map(|program| (PROGRAM_FILE, program)));
    for (name, bytes) in entries {
        zip.start_file(name, options)?;
        zip.write_all(bytes)?;
    }
    Ok(zip.finish()?.into_inner())
}

/// Build the module crate at `project` and write its package to `output`, answering the package.
pub fn run(root: &Path, project: &Path, output: &Path) -> Result<ModulePackage> {
    let manifest_path = project.join(MANIFEST_FILE);
    let manifest = fs::read(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let text = std::str::from_utf8(&manifest)
        .map_err(|_| format!("{} is not UTF-8 text", manifest_path.display()))?;
    let checked = ModuleManifest::parse(text)
        .map_err(|error| format!("{}: {}", manifest_path.display(), error.detail))?;
    println!(
        "Manifest {} {} ({}), effect {}",
        checked.id, checked.version, checked.title, checked.effect.id
    );
    let program_path = project.join(PROGRAM_FILE);
    let program = program_path
        .is_file()
        .then(|| fs::read(&program_path))
        .transpose()
        .map_err(|error| format!("cannot read {}: {error}", program_path.display()))?;
    let component = luxforge_module_sdk::host::build_component(project, &target_dir(root))?;
    let bytes = archive(&manifest, &component, program.as_deref())?;
    let package = ModulePackage::read(&bytes)
        .map_err(|error| format!("the package the reader refuses: {}", error.detail))?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = output.with_extension("lfmodule.partial");
    fs::write(&partial, &bytes)?;
    fs::rename(&partial, output)?;
    println!(
        "Packaged {} ({} bytes: component {} bytes, {}) {}",
        output.display(),
        bytes.len(),
        package.component.len(),
        if package.program.is_some() {
            "with its GPU program"
        } else {
            "no GPU program"
        },
        package.hash
    );
    Ok(package)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = include_str!("../../examples/modules/split-toning/luxforge-module.toml");
    /// The smallest component preamble, which the reader accepts as a component.
    const COMPONENT: &[u8] = b"\0asm\x0d\x00\x01\x00";

    #[test]
    fn an_archive_is_what_the_core_reader_accepts_and_the_same_each_time() {
        let program = b"fn lf_module(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> { return rgb; }\n";
        let bytes = archive(MANIFEST.as_bytes(), COMPONENT, Some(program)).unwrap();
        let package = ModulePackage::read(&bytes).unwrap();
        assert_eq!(package.manifest.id, "org.example.splittoning");
        assert_eq!(package.component, COMPONENT);
        assert_eq!(
            package.program.as_deref().map(str::as_bytes),
            Some(&program[..])
        );
        assert_eq!(
            archive(MANIFEST.as_bytes(), COMPONENT, Some(program)).unwrap(),
            bytes,
            "the same inputs make the same package"
        );
        let without =
            ModulePackage::read(&archive(MANIFEST.as_bytes(), COMPONENT, None).unwrap()).unwrap();
        assert_eq!(without.program, None);
    }
}
