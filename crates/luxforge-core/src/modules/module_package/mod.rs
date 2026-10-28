//! External adjustment modules' packages (`docs/design/module-manager.md#contract`): the
//! `.lfmodule` file, its `luxforge-module.toml` manifest, and the field-patch module a manifest's
//! field table describes.
//!
//! A package is a zip of exactly three files at its root: the manifest, `module.wasm` (a
//! WebAssembly component of the `luxforge:module/adjustment@0.1.0` world in the repository's
//! `wit/adjustment.wit`) and an optional `program.wgsl`. [`ModulePackage::read`] reads one from
//! bytes and [`ModulePackage::read_file`] from a path, checking the archive's size, entries and
//! names before anything is decompressed and every entry's size before it is parsed, and refusing
//! with the reason named ([`PackageRefusal`]). Reading writes nothing.
//!
//! The manifest's field table becomes a field-patch [`super::field_patch::Spec`], so an external
//! module has the actions, validation, history, presets, copy settings and generated controls of a
//! built-in field-patch module of the same table; a table the host could not draw is refused when
//! the package is read. The core runs no WebAssembly: [`ModuleManifest::module`] takes the colour
//! arithmetic as an [`AdjustmentUnits`], which the engine crate provides.
mod archive;
mod external;
mod manifest;
#[cfg(test)]
mod tests;

pub use external::{AdjustmentUnits, ExternalModule, FieldEntry, FieldValue, ModuleEngine};
pub use manifest::{ManifestEffect, ModuleManifest};

use crate::Error;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// The component interface version a manifest's `api` names, `luxforge:module/adjustment@0.1.0`.
pub const MODULE_API: &str = "0.1";
/// A package file's extension.
pub const PACKAGE_EXTENSION: &str = "lfmodule";
/// The manifest's name at the package root.
pub const MANIFEST_FILE: &str = "luxforge-module.toml";
/// The component's name at the package root.
pub const COMPONENT_FILE: &str = "module.wasm";
/// The optional GPU program's name at the package root.
pub const PROGRAM_FILE: &str = "program.wgsl";

/// The largest package file, compressed.
pub const MAX_PACKAGE_BYTES: u64 = 24 * 1024 * 1024;
/// The largest manifest, uncompressed.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// The largest component, uncompressed.
pub const MAX_COMPONENT_BYTES: u64 = 16 * 1024 * 1024;
/// The largest GPU program, uncompressed.
pub const MAX_PROGRAM_BYTES: u64 = 256 * 1024;

/// Why a package was refused, as a stable code a client reads from the error's
/// `data.refusal`; the error's detail says what in the package caused it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageRefusal {
    /// Not a zip this reader accepts: unreadable, prefixed or split, zip64, encrypted, or an entry
    /// compressed other than stored or Deflate, or whose bytes do not match its checksum.
    Archive,
    /// The package, or one of its entries, is larger than its limit.
    TooLarge,
    /// More entries than a package holds, or one entry twice.
    Entries,
    /// An entry named with an absolute path or a parent component.
    OutsideRoot,
    /// An entry other than the three a package holds, a directory or a link.
    UnexpectedEntry,
    /// The manifest or the component is missing.
    MissingFile,
    /// `module.wasm` is not a WebAssembly component, or `program.wgsl` is not UTF-8 text.
    Content,
    /// The manifest is not TOML of the declared shape, or its metadata is invalid.
    Manifest,
    /// The manifest targets a component interface this build does not offer.
    Api,
    /// The id is not a reverse-DNS module identity.
    Identity,
    /// The id is in Luxforge's own namespace.
    ReservedIdentity,
    /// The toolset is unknown or not open to external modules.
    Toolset,
    /// The effect's stage is unknown or not colour or finish.
    Stage,
    /// The effect declares itself maskable.
    Maskable,
    /// The effect declares source kinds.
    Sources,
    /// The field table is one the host could not draw.
    Table,
    /// The module engine refused the component: it imports something, or does not export the
    /// `adjust` interface with its three functions' types.
    Component,
}

impl PackageRefusal {
    /// The code a refusal's `data.refusal` carries.
    pub fn code(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::TooLarge => "too-large",
            Self::Entries => "entries",
            Self::OutsideRoot => "outside-root",
            Self::UnexpectedEntry => "unexpected-entry",
            Self::MissingFile => "missing-file",
            Self::Content => "content",
            Self::Manifest => "manifest",
            Self::Api => "api",
            Self::Identity => "identity",
            Self::ReservedIdentity => "reserved-identity",
            Self::Toolset => "toolset",
            Self::Stage => "stage",
            Self::Maskable => "maskable",
            Self::Sources => "sources",
            Self::Table => "table",
            Self::Component => "component",
        }
    }

    /// The refusal a package error carries, if it is one.
    pub fn of(error: &Error) -> Option<Self> {
        let code = error.data.as_ref()?.get("refusal")?.as_str()?;
        ALL.into_iter().find(|refusal| refusal.code() == code)
    }

    /// This refusal as a validation error saying `detail`.
    pub fn error(self, detail: impl Into<String>) -> Error {
        Error::validation(detail).with_data(serde_json::json!({ "refusal": self.code() }))
    }
}

const ALL: [PackageRefusal; 17] = [
    PackageRefusal::Archive,
    PackageRefusal::TooLarge,
    PackageRefusal::Entries,
    PackageRefusal::OutsideRoot,
    PackageRefusal::UnexpectedEntry,
    PackageRefusal::MissingFile,
    PackageRefusal::Content,
    PackageRefusal::Manifest,
    PackageRefusal::Api,
    PackageRefusal::Identity,
    PackageRefusal::ReservedIdentity,
    PackageRefusal::Toolset,
    PackageRefusal::Stage,
    PackageRefusal::Maskable,
    PackageRefusal::Sources,
    PackageRefusal::Table,
    PackageRefusal::Component,
];

/// A package that read and validated: its manifest (with the descriptor its table builds), the
/// component's bytes, the GPU program's text when it ships one, and the package's hash.
#[derive(Clone, Debug, PartialEq)]
pub struct ModulePackage {
    pub manifest: ModuleManifest,
    /// `module.wasm`, a WebAssembly component; the engine checks it beyond its preamble.
    pub component: Vec<u8>,
    /// `program.wgsl`, unvalidated WGSL text; the GPU loader validates it.
    pub program: Option<String>,
    /// `sha256:` and the lowercase hex SHA-256 of the package file's bytes: what a compiled-code or
    /// probe cache is keyed by and what the review sheet shows.
    pub hash: String,
}

impl ModulePackage {
    /// The package these bytes hold, or the refusal of the first thing wrong with them.
    pub fn read(bytes: &[u8]) -> Result<Self, Error> {
        let entries = archive::read(bytes)?;
        let text = std::str::from_utf8(&entries.manifest).map_err(|_| {
            PackageRefusal::Manifest.error(format!("{MANIFEST_FILE} is not UTF-8 text"))
        })?;
        let manifest = ModuleManifest::parse(text)?;
        check_component(&entries.component)?;
        let program = entries
            .program
            .map(|program| {
                String::from_utf8(program).map_err(|_| {
                    PackageRefusal::Content.error(format!("{PROGRAM_FILE} is not UTF-8 text"))
                })
            })
            .transpose()?;
        Ok(Self {
            manifest,
            component: entries.component,
            program,
            hash: format!("sha256:{:x}", Sha256::digest(bytes)),
        })
    }

    /// The package in the file at `path`: a regular file of at most [`MAX_PACKAGE_BYTES`], read
    /// whole and then as [`Self::read`] reads bytes. Nothing is written.
    pub fn read_file(path: &Path) -> Result<Self, Error> {
        Self::read(&read_package_file(path)?)
    }

    /// The module in the unpacked folder at `path`, as a developer loads one: the package's three
    /// files at the folder's root, each a regular file within its limit, read and checked as a
    /// package's entries are. Anything else in the folder, such as the module's sources, is not
    /// read. Its hash is `sha256:` and the hex SHA-256 of each file present, in the package's
    /// order, as its name, a NUL, its length as eight little-endian bytes and its bytes: what keys
    /// the compiled-code cache, so a rebuilt component compiles again. Nothing is written.
    pub fn read_folder(path: &Path) -> Result<Self, Error> {
        let shown = path.display();
        let metadata = std::fs::metadata(path)
            .map_err(|error| Error::file_access(format!("cannot read {shown}: {error}")))?;
        if !metadata.is_dir() {
            return Err(Error::file_access(format!("{shown} is not a folder")));
        }
        let manifest = folder_file(path, MANIFEST_FILE, MAX_MANIFEST_BYTES)?.ok_or_else(|| {
            PackageRefusal::MissingFile.error(format!("the folder holds no {MANIFEST_FILE}"))
        })?;
        let component =
            folder_file(path, COMPONENT_FILE, MAX_COMPONENT_BYTES)?.ok_or_else(|| {
                PackageRefusal::MissingFile.error(format!("the folder holds no {COMPONENT_FILE}"))
            })?;
        let program = folder_file(path, PROGRAM_FILE, MAX_PROGRAM_BYTES)?;
        let mut digest = Sha256::new();
        for (name, bytes) in [
            (MANIFEST_FILE, Some(&manifest)),
            (COMPONENT_FILE, Some(&component)),
            (PROGRAM_FILE, program.as_ref()),
        ] {
            if let Some(bytes) = bytes {
                digest.update(name.as_bytes());
                digest.update([0]);
                digest.update((bytes.len() as u64).to_le_bytes());
                digest.update(bytes);
            }
        }
        let text = std::str::from_utf8(&manifest).map_err(|_| {
            PackageRefusal::Manifest.error(format!("{MANIFEST_FILE} is not UTF-8 text"))
        })?;
        let manifest = ModuleManifest::parse(text)?;
        check_component(&component)?;
        let program = program
            .map(|program| {
                String::from_utf8(program).map_err(|_| {
                    PackageRefusal::Content.error(format!("{PROGRAM_FILE} is not UTF-8 text"))
                })
            })
            .transpose()?;
        Ok(Self {
            manifest,
            component,
            program,
            hash: format!("sha256:{:x}", digest.finalize()),
        })
    }
}

/// The manifest's text in the package these bytes hold, read as [`ModulePackage::read`] reads it:
/// what the module store keeps beside an installed package, so a package that no longer reads
/// still names its module.
pub(crate) fn manifest_text(bytes: &[u8]) -> Result<String, Error> {
    String::from_utf8(archive::read(bytes)?.manifest)
        .map_err(|_| PackageRefusal::Manifest.error(format!("{MANIFEST_FILE} is not UTF-8 text")))
}

/// The bytes of the package file at `path`:a regular file of at most [`MAX_PACKAGE_BYTES`], read
/// whole, for a caller that keeps the bytes beside the package they hold ([`ModulePackage::read`]),
/// as an install copies exactly the bytes it validated. Nothing is written.
pub fn read_package_file(path: &Path) -> Result<Vec<u8>, Error> {
    let shown = path.display();
    let file = std::fs::File::open(path)
        .map_err(|error| Error::file_access(format!("cannot open {shown}: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| Error::file_access(format!("cannot read {shown}: {error}")))?;
    if !metadata.is_file() {
        return Err(Error::file_access(format!("{shown} is not a file")));
    }
    if metadata.len() > MAX_PACKAGE_BYTES {
        return Err(too_large_package(metadata.len()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(MAX_PACKAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Error::file_access(format!("cannot read {shown}: {error}")))?;
    Ok(bytes)
}

/// The file `name` at the root of the folder `folder`, or `None` when there is none: a regular
/// file, not a link, of at most `limit` bytes, refused as too large past it.
fn folder_file(folder: &Path, name: &str, limit: u64) -> Result<Option<Vec<u8>>, Error> {
    let path = folder.join(name);
    let shown = path.display();
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::file_access(format!("cannot read {shown}: {error}"))),
    };
    if !metadata.is_file() {
        return Err(PackageRefusal::UnexpectedEntry
            .error(format!("{name} in the folder is not a regular file")));
    }
    let too_large = |bytes: u64| {
        PackageRefusal::TooLarge.error(format!(
            "{name} is {bytes} bytes, more than the {limit} it may hold"
        ))
    };
    if metadata.len() > limit {
        return Err(too_large(metadata.len()));
    }
    let file = std::fs::File::open(&path)
        .map_err(|error| Error::file_access(format!("cannot open {shown}: {error}")))?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Error::file_access(format!("cannot read {shown}: {error}")))?;
    if bytes.len() as u64 > limit {
        return Err(too_large(bytes.len() as u64));
    }
    Ok(Some(bytes))
}

fn too_large_package(bytes: u64) -> Error {
    PackageRefusal::TooLarge.error(format!(
        "the package is {bytes} bytes, more than the {MAX_PACKAGE_BYTES} a package may hold"
    ))
}

/// The component's preamble: the WebAssembly magic and the component layer, which a core module's
/// preamble does not carry. The engine validates the rest.
fn check_component(bytes: &[u8]) -> Result<(), Error> {
    const MAGIC: [u8; 4] = *b"\0asm";
    const COMPONENT_LAYER: [u8; 2] = [1, 0];
    let component = bytes.len() >= 8 && bytes[..4] == MAGIC && bytes[6..8] == COMPONENT_LAYER;
    if component {
        Ok(())
    } else {
        Err(PackageRefusal::Content
            .error(format!("{COMPONENT_FILE} is not a WebAssembly component")))
    }
}
