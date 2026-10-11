//! The Core ML compile cache: one directory per model hash, runtime version and macOS build, under
//! the application data root. See the [crate documentation](crate#the-core-ml-compile-cache).
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The directory under the runtime's cache root that holds Core ML's compiled models.
const COREML_DIR: &str = "coreml";
/// The file the running macOS names its build in.
const SYSTEM_VERSION: &str = "/System/Library/CoreServices/SystemVersion.plist";
/// The most bytes of it read.
const MAX_SYSTEM_VERSION_BYTES: u64 = 64 * 1024;

/// The compile cache directory of a model whose SHA-256 is `sha256` on this runtime `version`,
/// under `root`, after removing the model's directories for any other runtime version or system
/// build: `None` when the macOS build cannot be read, so Core ML compiles afresh rather than load a
/// compile it cannot tell is current. Nothing is created here; Core ML creates the directory.
pub(crate) fn compile_dir(root: &Path, sha256: &str, version: &str) -> Option<PathBuf> {
    let build = os_build()?;
    let name = key(sha256, version, &build)?;
    let dir = root.join(COREML_DIR);
    prune(&dir, sha256, &name);
    Some(dir.join(name))
}

/// The cache key: `<sha256>-onnxruntime-<version>-<build>`, or `None` when a part would not make a
/// plain directory name.
pub(crate) fn key(sha256: &str, version: &str, build: &str) -> Option<String> {
    let plain = |part: &str| {
        !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
    };
    (plain(sha256) && plain(version) && plain(build))
        .then(|| format!("{sha256}-{}-{version}-{build}", crate::RUNTIME_NAME))
}

/// Remove every compile of the model `sha256` under `dir` but `keep`.
fn prune(dir: &Path, sha256: &str, keep: &str) {
    let prefix = format!("{sha256}-");
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name != keep {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Remove every Core ML compile of the model whose SHA-256 is `sha256` under the runtime's cache
/// root `root`, as a host does when it removes the model. Answers how many it removed.
pub fn remove_compiled(root: &Path, sha256: &str) -> usize {
    let prefix = format!("{sha256}-");
    let mut removed = 0;
    for entry in fs::read_dir(root.join(COREML_DIR))
        .into_iter()
        .flatten()
        .flatten()
    {
        if entry.file_name().to_string_lossy().starts_with(&prefix)
            && fs::remove_dir_all(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// The running macOS build (`25F84`), from the system's version file; `None` elsewhere or when it
/// cannot be read.
pub(crate) fn os_build() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    use std::io::Read;
    let mut text = String::new();
    fs::File::open(SYSTEM_VERSION)
        .ok()?
        .take(MAX_SYSTEM_VERSION_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    build_of(&text)
}

/// The `ProductBuildVersion` string of a `SystemVersion.plist`.
pub(crate) fn build_of(plist: &str) -> Option<String> {
    let after = &plist[plist.find("<key>ProductBuildVersion</key>")?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")? + start;
    let build = after[start..end].trim();
    (!build.is_empty() && build.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| build.to_owned())
}
