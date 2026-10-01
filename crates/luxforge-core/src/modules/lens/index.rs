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
static PARSER_THREAD: OnceLock<std::thread::ThreadId> = OnceLock::new();
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
pub(crate) fn load_in_background() {
    START.call_once(|| {
        rayon::spawn(|| {
            #[cfg(test)]
            {
                let _ = PARSER_THREAD.set(std::thread::current().id());
                PARSE_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            let path = resolve_path();
            let _ = INDEX.set(load(path.as_deref()));
        })
    });
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
    fn lens_index_missing_file_answers_not_ready() {
        let error = load(Some(Path::new("/not-present/lens-profile-index"))).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::NotReady);
        assert_eq!(
            error.data.unwrap()["missing"],
            serde_json::json!(["lens-profile-index"])
        );
    }
    #[test]
    fn lens_index_parses_committed_file_off_the_calling_thread() {
        let caller = std::thread::current().id();
        load_in_background();
        luxforge_testbase::wait_for("the one background index parse", || INDEX.get().map(|_| ()));
        load_in_background();
        assert_ne!(caller, *PARSER_THREAD.get().unwrap());
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
