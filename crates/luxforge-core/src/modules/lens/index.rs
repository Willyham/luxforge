//! One immutable offline profile index, loaded on the shared pool rather than the catalog owner.
use super::pinned;
use crate::Error;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Once, OnceLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DistortionModel {
    Poly3,
    Poly5,
    PtLens,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Calibration {
    pub focal_mm: f64,
    pub terms: [f64; 3],
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexMount {
    pub name: String,
    pub compat: Vec<String>,
    pub fixed: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexCamera {
    pub maker: String,
    pub model: String,
    pub mount: String,
    pub crop_factor: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexLens {
    pub key: String,
    pub maker: String,
    pub models: Vec<String>,
    pub mounts: Vec<String>,
    pub crop_factor: f64,
    pub aspect_ratio: f64,
    pub model: DistortionModel,
    pub calibrations: Vec<Calibration>,
    pub record_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LensIndex {
    pub mounts: Vec<IndexMount>,
    pub cameras: Vec<IndexCamera>,
    pub lenses: Vec<IndexLens>,
}

impl LensIndex {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(Error::unsupported_input(
                "Lens profile index exceeds 16 MiB",
            ));
        }
        if format!("{:x}", Sha256::digest(bytes)) != pinned::INDEX_SHA256 {
            return Err(Error::unsupported_input(
                "Lens profile index hash differs from the pinned database",
            ));
        }
        let index: Self = serde_json::from_slice(bytes)
            .map_err(|e| Error::unsupported_input(format!("Malformed lens profile index: {e}")))?;
        Ok(index)
    }
}
static START: Once = Once::new();
static INDEX: OnceLock<Result<Arc<LensIndex>, Error>> = OnceLock::new();
#[cfg(test)]
static PARSER_THREAD: OnceLock<(std::thread::ThreadId, Option<String>)> = OnceLock::new();
#[cfg(test)]
static PARSE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn missing() -> Error {
    Error::not_ready("Lens profile index is unavailable")
        .with_data(serde_json::json!({"missing":["lens-profile-index"]}))
}
fn paths(executable: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(directory) = executable.and_then(Path::parent) {
        paths.push(directory.join("../Resources/lensfun/index.json"));
        paths.push(directory.join("lensfun/index.json"));
    }
    // A development build also finds the checkout's index. A tag build reads only its own payload's,
    // so a missing bundled index is reported, never replaced by the build machine's checkout.
    #[cfg(not(luxforge_release_build))]
    paths.push(PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/lensfun/index.json"
    )));
    paths
}
pub(crate) fn resolve_path() -> Option<PathBuf> {
    paths(std::env::current_exe().ok().as_deref())
        .into_iter()
        .find(|p| fs::File::open(p).is_ok())
}
fn load(path: Option<&Path>) -> Result<Arc<LensIndex>, Error> {
    let path = path.ok_or_else(missing)?;
    let size = fs::metadata(path).map_err(|_| missing())?.len();
    if size > 16 * 1024 * 1024 {
        return Err(Error::unsupported_input(
            "Lens profile index exceeds 16 MiB",
        ));
    }
    let file = fs::File::open(path).map_err(|_| missing())?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| missing())?;
    LensIndex::parse(&bytes).map(Arc::new)
}
/// The thread the one parse runs on: its own, which ends with the parse, rather than the shared
/// Rayon pool, so assembling the module registry starts no pool in a process that never renders,
/// such as `luxforge-mcp` listing the built-in registry.
const PARSER: &str = "luxforge-lens-index";
pub(crate) fn load_in_background() {
    START.call_once(|| {
        let parse = std::thread::Builder::new().name(PARSER.into()).spawn(|| {
            #[cfg(test)]
            {
                let current = std::thread::current();
                let _ = PARSER_THREAD.set((current.id(), current.name().map(Into::into)));
                PARSE_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            let path = resolve_path();
            let _ = INDEX.set(load(path.as_deref()));
        });
        // A thread the system refuses leaves the index unavailable, never a waiter blocked.
        if parse.is_err() {
            let _ = INDEX.set(Err(missing()));
        }
    });
}
/// Block until the one background parse has finished, starting it if nothing has, whether it
/// loaded or failed. For the source worker before a new import completes, never the catalog owner
/// or a UI thread.
pub(crate) fn wait_ready() {
    #[cfg(test)]
    if TEST_INDEX.with(|cell| cell.borrow().is_some()) {
        return;
    }
    load_in_background();
    INDEX.wait();
}
pub(crate) fn shared() -> Result<Arc<LensIndex>, Error> {
    #[cfg(test)]
    if let Some(value) = TEST_INDEX.with(|cell| cell.borrow().clone()) {
        return value;
    }
    INDEX.get().cloned().unwrap_or_else(|| Err(missing()))
}
#[cfg(test)]
thread_local! { static TEST_INDEX: std::cell::RefCell<Option<Result<Arc<LensIndex>, Error>>> = const { std::cell::RefCell::new(None) }; }
/// A caller-local test override does not contaminate concurrent module or export tests.
#[cfg(test)]
pub(crate) fn set_for_test(value: Result<Arc<LensIndex>, Error>) {
    TEST_INDEX.with(|cell| *cell.borrow_mut() = Some(value));
}
#[cfg(test)]
pub(crate) fn clear_for_test() {
    TEST_INDEX.with(|cell| *cell.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn committed() -> Vec<u8> {
        fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/lensfun/index.json"
        ))
        .unwrap()
    }
    #[test]
    fn lens_index_refuses_modified_bytes() {
        let mut bytes = committed();
        bytes.push(b' ');
        assert_eq!(
            LensIndex::parse(&bytes).unwrap_err().kind,
            crate::ErrorKind::UnsupportedInput
        );
    }
    #[test]
    fn only_a_development_build_falls_back_to_the_checkouts_index() {
        let found = paths(Some(Path::new("/opt/Lüx forge/Luxforge/luxforge")));
        let payload = Path::new("/opt/Lüx forge/Luxforge");
        assert_eq!(
            found[..2],
            [
                payload.join("../Resources/lensfun/index.json"),
                payload.join("lensfun/index.json")
            ]
        );
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/lensfun/index.json");
        let development = cfg!(not(luxforge_release_build));
        assert_eq!(found.len(), if development { 3 } else { 2 });
        assert_eq!(found.contains(&checkout), development);
        assert_eq!(
            development,
            option_env!("LUXFORGE_RELEASE_VERSION").is_none()
        );
    }
    #[test]
    fn lens_index_missing_file_answers_not_ready() {
        let error = load(Some(Path::new("/not-present/lens-profile-index"))).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::NotReady);
        assert_eq!(
            error.data.unwrap()["missing"],
            serde_json::json!(["lens-profile-index"])
        );
    }
    #[test]
    #[cfg_attr(
        luxforge_release_build,
        ignore = "reads the committed index through the development build's checkout fallback"
    )]
    fn lens_index_parses_committed_file_off_the_calling_thread() {
        let caller = std::thread::current().id();
        load_in_background();
        luxforge_testbase::wait_for("the one background index parse", || INDEX.get().map(|_| ()));
        load_in_background();
        let (parser, name) = PARSER_THREAD.get().unwrap();
        assert_ne!(caller, *parser);
        assert_eq!(
            name.as_deref(),
            Some(PARSER),
            "its own thread, not the Rayon pool's"
        );
        assert_eq!(PARSE_COUNT.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!INDEX.get().unwrap().as_ref().unwrap().lenses.is_empty());
    }
    #[test]
    fn committed_index_matches_provenance() {
        let bytes = committed();
        let index = LensIndex::parse(&bytes).unwrap();
        let provenance: serde_json::Value = serde_json::from_slice(
            &fs::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/data/lensfun/provenance.json"
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(provenance["index_sha256"], pinned::INDEX_SHA256);
        for model in [
            "NIKKOR Z 24-70mm f/4 S",
            "NIKKOR Z 24-200mm f/4-6.3 VR",
            "FC3411 & compatibles",
            "X100V & compatibles (Standard)",
        ] {
            assert!(
                index
                    .lenses
                    .iter()
                    .any(|lens| lens.models.iter().any(|m| m == model)),
                "{model}"
            );
        }
        for model in ["Nikon Z 6", "FC3411", "ILCE-7RM4"] {
            assert!(index.cameras.iter().any(|c| c.model == model), "{model}");
        }
        assert!(!index.cameras.iter().any(|c| c.model.contains("X100VI")));
        assert!(provenance["excluded"].as_array().unwrap().iter().any(|e| {
            e["reason"] == "too-many-calibrations"
                && e["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m == "FinePix 2800 ZOOM & compatibles (Standard)")
        }));
    }
}
