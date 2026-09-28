//! RAW manifests and RAW preparation evidence. [`manifest`] is the one reader of a RAW manifest:
//! `raw-corpus`, `verify` and the `raw-editor` smoke scenario all read theirs through it. The
//! corpus tool does not register or qualify an editor decoder.
use crate::*;
use luxforge_raw::RawMode;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::HashSet, fs::File, time::SystemTime};

const MAX_MANIFEST: u64 = 1024 * 1024;
const MAX_SOURCE: u64 = 128 * 1024 * 1024;
const MAX_SOURCES: usize = 256;

/// A RAW manifest, `{"format":1,"sources":[...]}`, each source one `E`. Read only through
/// [`manifest`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest<E> {
    format: u32,
    pub sources: Vec<E>,
    /// The manifest's own directory, which a relative source path is resolved against.
    #[serde(skip)]
    base: PathBuf,
}

/// What every manifest source has, whatever else its kind adds: an id, the path of its file,
/// relative to the manifest's directory unless absolute, and that file's SHA-256.
pub trait Entry: DeserializeOwned {
    fn id(&self) -> &str;
    fn path(&self) -> &Path;
    fn sha256(&self) -> &str;
    /// The kind's own rules over its own fields.
    fn check(&self) -> Result;
}

impl<E: Entry> Manifest<E> {
    /// Each source with the absolute path of its file.
    pub fn located(&self) -> impl Iterator<Item = (&E, PathBuf)> {
        self.sources
            .iter()
            .map(|source| (source, absolute(&self.base, source.path())))
    }
}

/// Read and check a RAW manifest: at most 1 MiB, format 1, 1 to 256 sources with unique ids of
/// ASCII letters, digits and hyphens, each with a lowercase SHA-256 and a path, and each passing its
/// kind's own [`Entry::check`]. Unknown fields anywhere are refused.
pub fn manifest<E: Entry>(path: &Path) -> Result<Manifest<E>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)?;
    ensure(
        bytes.len() as u64 <= MAX_MANIFEST,
        "RAW manifest exceeds 1 MiB",
    )?;
    let mut manifest: Manifest<E> = serde_json::from_slice(&bytes)?;
    ensure(manifest.format == 1, "Unsupported RAW manifest format")?;
    ensure(
        !manifest.sources.is_empty() && manifest.sources.len() <= MAX_SOURCES,
        "RAW manifest needs 1..256 sources",
    )?;
    let mut ids = HashSet::new();
    for source in &manifest.sources {
        let id = source.id();
        ensure(
            !id.is_empty()
                && id.len() <= 96
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            format!("Invalid RAW source id {id:?}"),
        )?;
        ensure(ids.insert(id), format!("Duplicate RAW source id {id}"))?;
        ensure(
            source.sha256().len() == 64
                && source
                    .sha256()
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            format!("RAW source {id} needs a lowercase SHA-256"),
        )?;
        ensure(
            !source.path().as_os_str().is_empty(),
            format!("Empty path for RAW source {id}"),
        )?;
        source.check()?;
    }
    manifest.base = path.parent().ok_or("Manifest has no parent")?.into();
    Ok(manifest)
}

/// A provenance source, `raw-corpus`'s: a public or permitted file, the camera and recording mode
/// it was captured with in words, and where it came from under which licence. It qualifies nothing.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    id: String,
    path: PathBuf,
    sha256: String,
    camera: String,
    mode: String,
    source_url: Option<String>,
    license: String,
    notes: String,
}

impl Entry for Provenance {
    fn id(&self) -> &str {
        &self.id
    }
    fn path(&self) -> &Path {
        &self.path
    }
    fn sha256(&self) -> &str {
        &self.sha256
    }
    fn check(&self) -> Result {
        ensure(
            !self.camera.is_empty() && !self.mode.is_empty(),
            "Camera and declared mode are required",
        )?;
        ensure(
            matches!(self.license.as_str(), "CC0-1.0" | "private-permission"),
            "Declare CC0-1.0 or private-permission for each RAW fixture",
        )?;
        if self.license == "CC0-1.0" {
            ensure(
                self.source_url
                    .as_deref()
                    .is_some_and(|s| s.starts_with("https://")),
                "Public fixture needs an HTTPS provenance URL",
            )?;
        }
        Ok(())
    }
}

/// An editor source, what the `raw-editor` scenario checks a supported file against and what
/// `verify` builds its RAW components from: the editor's recording mode, the camera, the upright
/// dimensions and orientation the file opens at, and a content point the sensor-neutral picker can
/// sample. A source whose camera profile declares DNG corrections also gives the sensor, active
/// area and default crop geometry the corrections are applied over.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditorSource {
    pub id: String,
    pub path: PathBuf,
    pub sha256: String,
    /// The editor's own recording mode: a mode the camera catalog does not declare is refused as
    /// the manifest is read.
    pub mode: RawMode,
    pub make: String,
    pub model: String,
    /// Full unpacked sensor dimensions, before default crop and orientation.
    pub sensor_dimensions: Option<[u32; 2]>,
    /// Absolute sensor-space rectangle `[x, y, width, height]`.
    pub active_area: Option<[u32; 4]>,
    /// Absolute sensor-space rectangle `[x, y, width, height]`.
    pub default_crop: Option<[u32; 4]>,
    pub source_dimensions: [u32; 2],
    pub orientation: u8,
    /// A fixture-verified unclipped, non-dark upright content point for the sensor picker.
    pub neutral_point: [u32; 2],
}

impl EditorSource {
    /// Whether the file carries DNG corrections: the camera profile's own declaration, not the
    /// mode's name.
    pub fn dng(&self) -> bool {
        self.mode.requires_dng_corrections()
    }

    /// The sensor, active area and default crop a DNG source declares, which its
    /// [`Entry::check`] has found present and consistent.
    pub fn dng_geometry(&self) -> Option<([u32; 2], [u32; 4], [u32; 4])> {
        Some((
            self.sensor_dimensions?,
            self.active_area?,
            self.default_crop?,
        ))
    }
}

impl Entry for EditorSource {
    fn id(&self) -> &str {
        &self.id
    }
    fn path(&self) -> &Path {
        &self.path
    }
    fn sha256(&self) -> &str {
        &self.sha256
    }
    fn check(&self) -> Result {
        let id = &self.id;
        ensure(
            self.source_dimensions.iter().all(|side| *side > 0)
                && (1..=8).contains(&self.orientation)
                && self.neutral_point.iter().zip(self.source_dimensions).all(
                    |(coordinate, side)| {
                        *coordinate >= 6 && coordinate.checked_add(6).is_some_and(|end| end < side)
                    },
                ),
            format!("Invalid geometry for {id}"),
        )?;
        if !self.dng() {
            return Ok(());
        }
        let Some((sensor, active, crop)) = self.dng_geometry() else {
            return Err(format!("DNG source {id} needs sensor, active and crop geometry").into());
        };
        ensure(
            sensor.iter().all(|side| *side > 0)
                && [active, crop].into_iter().all(|rect| {
                    rect[2] > 0
                        && rect[3] > 0
                        && rect[0]
                            .checked_add(rect[2])
                            .is_some_and(|end| end <= sensor[0])
                        && rect[1]
                            .checked_add(rect[3])
                            .is_some_and(|end| end <= sensor[1])
                })
                && crop[0] >= active[0]
                && crop[1] >= active[1]
                && crop[0] + crop[2] <= active[0] + active[2]
                && crop[1] + crop[3] <= active[1] + active[3]
                && (if self.orientation >= 5 {
                    [crop[3], crop[2]]
                } else {
                    [crop[2], crop[3]]
                }) == self.source_dimensions,
            format!("DNG source {id} geometry is inconsistent"),
        )
    }
}

/// An editor manifest's sources as `verify` plans from them: `(id, absolute path)`.
pub fn sources(path: &Path) -> Result<Vec<(String, PathBuf)>> {
    Ok(manifest::<EditorSource>(path)?
        .located()
        .map(|(source, path)| (source.id.clone(), path))
        .collect())
}

#[derive(Debug, PartialEq, Eq)]
struct Signature {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity_change: (u64, u64, i64, i64),
}

fn signature(m: &fs::Metadata) -> Result<Signature> {
    ensure(m.is_file(), "RAW source must be a regular file")?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(Signature {
        len: m.len(),
        modified: m.modified()?,
        #[cfg(unix)]
        identity_change: (m.dev(), m.ino(), m.ctime(), m.ctime_nsec()),
    })
}

fn verify(path: &Path, expected: &str) -> Result<Value> {
    let before = signature(&fs::metadata(path)?)?;
    ensure(before.len <= MAX_SOURCE, "RAW source exceeds 128 MiB")?;
    let file = File::open(path)?;
    ensure(
        signature(&file.metadata()?)? == before,
        "RAW source changed before reading",
    )?;
    let mut stream = file.take(MAX_SOURCE + 1);
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ensure(total <= MAX_SOURCE, "RAW source grew beyond 128 MiB")?;
        hash.update(&buffer[..n]);
    }
    ensure(
        total == before.len
            && signature(&stream.get_ref().metadata()?)? == before
            && signature(&fs::metadata(path)?)? == before,
        "RAW source changed while hashing",
    )?;
    let actual = format!("{:x}", hash.finalize());
    ensure(actual == expected, "RAW source SHA-256 mismatch")?;
    Ok(json!({"bytes":total,"sha256":actual,"verified_file":true,"decoder_qualified":false}))
}

pub fn corpus(manifest_path: &Path, out: &Path) -> Result {
    let manifest = manifest::<Provenance>(manifest_path)?;
    // create_dir, not create_dir_all: an existing evidence directory must never be reused.
    fs::create_dir(out)?;
    let mut results = Vec::new();
    let mut passed = true;
    for (source, path) in manifest.located() {
        let result = match verify(&path, &source.sha256) {
            Ok(value) => json!({"id":source.id,"verification":value}),
            Err(error) => {
                passed = false;
                json!({"id":source.id,"verification":{"verified_file":false,"decoder_qualified":false,"error":error.to_string()}})
            }
        };
        results.push(result);
    }
    write_json(
        &out.join("result.json"),
        &json!({"format":1,"passed":passed,
        "scope":"Read-only file integrity and declared provenance; no decoder or camera-mode qualification",
        "manifest":manifest,"results":results}),
    )?;
    ensure(passed, "RAW corpus verification failed; see result.json")?;
    println!(
        "PASS RAW file integrity ({} samples); decoder support remains unqualified",
        results.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest(path: &Path, digest: &str) -> Value {
        json!({"format":1,"sources":[{"id":"sample","path":path,"sha256":digest,
            "camera":"Synthetic","mode":"file-integrity only","source_url":null,
            "license":"private-permission","notes":"not a RAW decoder fixture"}]})
    }
    #[test]
    fn corpus_preserves_readonly_sources_and_refuses_reused_output() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("source.NEF");
        fs::write(&input, b"synthetic integrity fixture").unwrap();
        let expected = hash(&input).unwrap();
        let mut permissions = fs::metadata(&input).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&input, permissions).unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        write_json(
            &manifest_path,
            &manifest(Path::new("source.NEF"), &expected),
        )
        .unwrap();
        let out = tmp.path().join("result");
        corpus(&manifest_path, &out).unwrap();
        let before = fs::read(out.join("result.json")).unwrap();
        assert!(corpus(&manifest_path, &out).is_err());
        assert_eq!(before, fs::read(out.join("result.json")).unwrap());
        assert_eq!(hash(&input).unwrap(), expected);
        let result = read_json(&out.join("result.json")).unwrap();
        assert_eq!(
            result["results"][0]["verification"]["decoder_qualified"],
            false
        );
        // The manifest is recorded as it was written: its relative path, not the resolved one.
        assert_eq!(result["manifest"]["sources"][0]["path"], "source.NEF");
    }
    #[test]
    fn missing_and_changed_sources_are_failures_with_retained_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        let input = tmp.path().join("source.RAF");
        write_json(
            &manifest_path,
            &manifest(Path::new("source.RAF"), &"0".repeat(64)),
        )
        .unwrap();
        let missing = tmp.path().join("missing");
        assert!(corpus(&manifest_path, &missing).is_err());
        assert_eq!(
            read_json(&missing.join("result.json")).unwrap()["passed"],
            false
        );
        fs::write(&input, b"wrong bytes").unwrap();
        let changed = tmp.path().join("changed");
        assert!(corpus(&manifest_path, &changed).is_err());
        assert!(
            read_json(&changed.join("result.json")).unwrap()["results"][0]["verification"]["error"]
                .as_str()
                .unwrap()
                .contains("mismatch")
        );
    }
    #[test]
    fn unknown_fields_duplicate_ids_invalid_hashes_and_limits_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manifest.json");
        let load = |path: &Path| super::manifest::<Provenance>(path);
        let original = manifest(Path::new("source.NEF"), &"a".repeat(64));
        write_json(&path, &original).unwrap();
        assert!(load(&path).is_ok());
        for bad in [
            json!({"format":2,"sources":[]}),
            json!({"format":1,"sources":[]}),
            // The one shape: a list under any other name is an unknown field.
            json!({"format":1,"samples":original["sources"]}),
        ] {
            write_json(&path, &bad).unwrap();
            assert!(load(&path).is_err());
        }
        let mut bad = original.clone();
        bad["unknown"] = json!(true);
        write_json(&path, &bad).unwrap();
        assert!(load(&path).is_err());
        let mut bad = original.clone();
        bad["sources"] = json!([bad["sources"][0], bad["sources"][0]]);
        write_json(&path, &bad).unwrap();
        assert!(load(&path).is_err());
        let mut bad = original.clone();
        bad["sources"][0]["sha256"] = json!("invalid");
        write_json(&path, &bad).unwrap();
        assert!(load(&path).is_err());
        let mut bad = original;
        bad["sources"][0]["id"] = json!("under_score");
        write_json(&path, &bad).unwrap();
        assert!(load(&path).is_err());
        fs::write(&path, vec![b' '; MAX_MANIFEST as usize + 1]).unwrap();
        assert!(load(&path).is_err());
        let source = tmp.path().join("large.NEF");
        File::create(&source)
            .unwrap()
            .set_len(MAX_SOURCE + 1)
            .unwrap();
        assert!(
            verify(&source, &"0".repeat(64))
                .unwrap_err()
                .to_string()
                .contains("128 MiB")
        );
    }

    /// An editor manifest names its modes in the editor's own vocabulary, resolves each path
    /// against its own directory, and asks for DNG geometry exactly where the camera profile
    /// declares DNG corrections.
    #[test]
    fn editor_sources_are_typed_and_dng_geometry_follows_the_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manifest.json");
        let z6 = json!({"id":"nikon-z6","path":"raw/z6.NEF","sha256":"a".repeat(64),
            "mode":"NikonZ6Lossless14","make":"Nikon","model":"Z 6",
            "source_dimensions":[4024,6048],"orientation":8,"neutral_point":[1609,2419]});
        let dji = json!({"id":"dji-air2s","path":"/raw/air2s.DNG","sha256":"b".repeat(64),
            "mode":"DjiAir2sDng16","make":"DJI","model":"FC3411",
            "sensor_dimensions":[5568,3648],"active_area":[96,0,5472,3648],
            "default_crop":[100,4,5464,3640],"source_dimensions":[5464,3640],"orientation":1,
            "neutral_point":[2185,1456]});
        let load = |sources: Value| {
            write_json(&path, &json!({"format":1,"sources":sources})).unwrap();
            super::manifest::<EditorSource>(&path)
        };
        let manifest = load(json!([z6, dji])).unwrap();
        let located: Vec<_> = manifest.located().collect();
        assert_eq!(located[0].1, tmp.path().join("raw/z6.NEF"));
        assert_eq!(located[1].1, Path::new("/raw/air2s.DNG"));
        assert!(!located[0].0.dng() && located[1].0.dng());
        assert_eq!(
            sources(&path).unwrap(),
            [
                ("nikon-z6".to_owned(), tmp.path().join("raw/z6.NEF")),
                ("dji-air2s".to_owned(), PathBuf::from("/raw/air2s.DNG"))
            ]
        );
        // A mode the camera catalog does not declare is refused as the manifest is read.
        let mut unknown = z6.clone();
        unknown["mode"] = json!("NikonZ6Lossless13");
        assert!(load(json!([unknown])).is_err());
        // The DNG profile's source needs its geometry, and it must agree with the upright size.
        let mut bare = dji.clone();
        bare.as_object_mut().unwrap().remove("active_area");
        assert!(
            load(json!([bare]))
                .unwrap_err()
                .to_string()
                .contains("needs sensor, active and crop geometry")
        );
        let mut wrong = dji.clone();
        wrong["source_dimensions"] = json!([5464, 3641]);
        assert!(
            load(json!([wrong]))
                .unwrap_err()
                .to_string()
                .contains("geometry is inconsistent")
        );
        // A neutral point needs six pixels of margin inside the upright image.
        let mut edge = z6;
        edge["neutral_point"] = json!([5, 100]);
        assert!(load(json!([edge])).is_err());
    }
}
