//! The compiled-code cache: a component compiled once per package and engine, under the data root.
//!
//! An entry is named by the package hash and the engine's compatibility hash (Wasmtime's version,
//! its configuration and the host's CPU features), so a package, an engine build or a machine
//! never reads another's code. Its file is the SHA-256 of the serialized code followed by the
//! code, written to a temporary name and renamed into place; a read checks the digest before
//! Wasmtime loads the code, so a torn or corrupted entry is compiled again rather than run. The
//! cache is disposable: nothing else reads it, and deleting it costs one compilation per package.
use crate::Compiled;
use sha2::{Digest, Sha256};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use wasmtime::Engine;
use wasmtime::component::Component;

/// The largest cache entry read back. Compiled code is a small multiple of the component, which a
/// package holds at most 16 MiB of.
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const DIGEST_BYTES: usize = 32;

/// `package`'s component, compiled, and where its code came from. With a cache directory, an entry
/// is read when it is valid and written after a compilation otherwise; a cache that cannot be read
/// or written costs a compilation, never the load.
pub(crate) fn component(
    engine: &Engine,
    cache: Option<&Path>,
    hash: &str,
    bytes: &[u8],
) -> Result<(Component, Compiled), String> {
    let entry = cache.map(|dir| dir.join(entry_name(engine, hash)));
    if let Some(component) = entry.as_deref().and_then(|path| read(engine, path)) {
        return Ok((component, Compiled::Cached));
    }
    let component = Component::new(engine, bytes)
        .map_err(|error| format!("its component does not compile: {error:#}"))?;
    let stored = entry
        .as_deref()
        .is_some_and(|path| write(&component, path).is_ok());
    Ok((
        component,
        if stored {
            Compiled::Compiled
        } else {
            Compiled::Uncached
        },
    ))
}

/// The entry of the package with this `sha256:` hash under this engine.
fn entry_name(engine: &Engine, hash: &str) -> String {
    // A package's hash is `sha256:` and 64 hex digits; anything else a caller built is hashed, so
    // an entry's name never holds a separator.
    let package = match hash.strip_prefix("sha256:") {
        Some(hex) if hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            hex.to_ascii_lowercase()
        }
        _ => format!("{:x}", Sha256::digest(hash.as_bytes())),
    };
    let mut compatibility = Sha256Hasher(Sha256::new());
    engine
        .precompile_compatibility_hash()
        .hash(&mut compatibility);
    let engine = compatibility.0.finalize();
    format!("{package}-{:x}.cwasm", Hex(&engine[..8]))
}

/// The entry at `path` as a component, or `None` when it is missing, unreadable, larger than an
/// entry may be, fails its digest, or is code this engine refuses.
fn read(engine: &Engine, path: &Path) -> Option<Component> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_ENTRY_BYTES || bytes.len() < DIGEST_BYTES {
        return None;
    }
    let (digest, code) = bytes.split_at(DIGEST_BYTES);
    if Sha256::digest(code).as_slice() != digest {
        return None;
    }
    load(engine, code)
}

/// Load compiled code this cache wrote.
#[allow(unsafe_code)]
fn load(engine: &Engine, code: &[u8]) -> Option<Component> {
    // SAFETY: Wasmtime requires that the bytes are what `Component::serialize` produced; they
    // become machine code. These are: only `write` below creates an entry, in the application's
    // own data directory, from `Component::serialize`, and `read` has checked the SHA-256 written
    // with them, so a torn, truncated or corrupted entry never reaches here. Wasmtime itself
    // refuses code of another version, configuration or CPU, which the entry's name already
    // separates.
    unsafe { Component::deserialize(engine, code) }.ok()
}

/// Write `component`'s code to `path`: to a temporary name beside it, then renamed into place, so
/// a reader sees a whole entry or none. The write is not flushed to the drive: a torn entry fails
/// its digest and is compiled again.
fn write(component: &Component, path: &Path) -> std::io::Result<()> {
    let code = component.serialize().map_err(std::io::Error::other)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temporary = temporary(path);
    let mut bytes = Vec::with_capacity(DIGEST_BYTES + code.len());
    bytes.extend_from_slice(&Sha256::digest(&code));
    bytes.extend_from_slice(&code);
    let written =
        std::fs::write(&temporary, &bytes).and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// A temporary name beside `path`, unique to this process and moment.
fn temporary(path: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}-{nanos}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// A [`Hasher`] over SHA-256, so the engine's compatibility hash names an entry the same way in
/// every process and build of the same engine.
struct Sha256Hasher(Sha256);

impl Hasher for Sha256Hasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(&self) -> u64 {
        let digest = self.0.clone().finalize();
        u64::from_le_bytes(digest[..8].try_into().unwrap_or_default())
    }
}

struct Hex<'a>(&'a [u8]);

impl std::fmt::LowerHex for Hex<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}
