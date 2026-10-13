//! The RAW photographs a round copies: a fixed selection from the CC0 sample corpus, read from the
//! corpus tool's local cache, and the owner's supplied files from a RAW manifest. Neither enters
//! the repository; the round holds copies, each with its variant's XMP sidecar, and the originals
//! are only read.
use crate::raw::{EditorSource, manifest};
use crate::*;

/// The corpus photographs every round with `--corpus` copies, chosen across cameras, sensor
/// layouts and sizes: the owner's two bodies first, then three other makers. Each is a qualified,
/// non-DNG file (Lightroom reads a DNG's settings from inside the file, not from a sidecar).
/// Subject matter (skies, skin, foliage, night) was not reviewed when choosing: the photographs are
/// in the corpus mirror, not in the checkout. Changing the selection is a reviewed change to this
/// table.
pub const CORPUS_SELECTION: [(&str, &str); 5] = [
    (
        "pixls-3582",
        "Nikon Z6, the owner's body: 24 MP Bayer, 14-bit lossless",
    ),
    (
        "pixls-7301",
        "Fujifilm X100VI, the owner's body: 40 MP X-Trans, 14-bit lossless",
    ),
    ("pixls-6932", "Sony A7 IV: 33 MP Bayer, 14-bit"),
    ("pixls-4662", "Canon EOS R6: 20 MP Bayer, CR3"),
    ("pixls-5283", "OM System OM-1: 20 MP Four Thirds at 4:3"),
];

/// The corpus manifest, relative to the repository root.
pub const CORPUS_MANIFEST: &str = "fixtures/sample-corpus.json";

/// One RAW photograph a round copies.
#[derive(Clone, Debug)]
pub struct Photo {
    /// The subject id variants are named by: a corpus sample's id, or `own-` and a supplied
    /// source's id.
    pub id: String,
    pub origin: &'static str,
    pub camera: String,
    pub why: String,
    /// The original, which is only read.
    pub path: PathBuf,
    /// The original's extension as written, which every copy keeps.
    pub extension: String,
    pub sha256: String,
    pub bytes: u64,
    /// The developed picture's size, for the export estimate.
    pub dimensions: [u32; 2],
}

/// A photograph a manifest names that the round does not include, and why.
pub type Excluded = (String, String);

/// The selected corpus photographs, each from `cache` (the corpus tool's cache, which names a
/// file by its SHA-256 and lowercase extension), resolved through the corpus manifest
/// `corpus`. Refused, naming the corpus groups to sync, unless every selected file is there.
pub fn corpus(corpus: &Value, cache: &Path) -> Result<Vec<Photo>> {
    let samples = corpus["samples"]
        .as_array()
        .ok_or("the corpus manifest lists no samples")?;
    let devices = corpus["devices"]
        .as_array()
        .ok_or("the corpus manifest lists no devices")?;
    let mut photos = Vec::new();
    let mut missing = Vec::new();
    for (id, why) in CORPUS_SELECTION {
        let sample = samples
            .iter()
            .find(|sample| sample["id"] == id)
            .ok_or(format!("the corpus manifest has no sample {id}"))?;
        let text = |key: &str| -> Result<String> {
            Ok(sample[key]
                .as_str()
                .ok_or(format!("{id} has no {key}"))?
                .to_owned())
        };
        let (sha256, extension) = (text("sha256")?, text("extension")?);
        let device = text("device")?;
        let camera = devices
            .iter()
            .find(|candidate| candidate["id"] == device.as_str())
            .and_then(|found| found["name"].as_str())
            .unwrap_or(&device)
            .to_owned();
        let as_shot = &sample["expectation"]["as_shot"];
        let side = |key: &str| -> Result<u32> {
            Ok(u32::try_from(
                as_shot[key].as_u64().ok_or(format!("{id} has no {key}"))?,
            )?)
        };
        let path = cache.join(format!("{sha256}{}", extension.to_lowercase()));
        if !path.is_file() {
            missing.push(format!(
                "{id} ({camera}, group {})",
                sample["group"].as_str().unwrap_or("?")
            ));
            continue;
        }
        photos.push(Photo {
            id: id.to_owned(),
            origin: "corpus",
            camera,
            why: why.to_owned(),
            path,
            extension: extension.trim_start_matches('.').to_owned(),
            sha256,
            bytes: sample["bytes"]
                .as_u64()
                .ok_or(format!("{id} has no byte count"))?,
            dimensions: [side("width")?, side("height")?],
        });
    }
    if !missing.is_empty() {
        return Err(format!(
            "the corpus cache {} lacks {}; sync those groups with tools/sample_corpus.py sync \
             --group GROUP --qualified-only, or give --corpus a cache that holds them",
            cache.display(),
            missing.join(", ")
        )
        .into());
    }
    Ok(photos)
}

/// The supplied RAW files a RAW manifest names, read through the one RAW manifest reader, and
/// those the round leaves out: a DNG, whose settings Lightroom reads from inside the file.
pub fn supplied(path: &Path) -> Result<(Vec<Photo>, Vec<Excluded>)> {
    let manifest = manifest::<EditorSource>(path)?;
    let mut photos = Vec::new();
    let mut excluded = Vec::new();
    for (source, file) in manifest.located() {
        let extension = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_owned();
        let id = format!("own-{}", source.id);
        if source.dng() || extension.eq_ignore_ascii_case("dng") {
            excluded.push((
                id,
                "a DNG: Lightroom reads a DNG's settings from inside the file, and the rig writes \
                 sidecars only"
                    .to_owned(),
            ));
            continue;
        }
        photos.push(Photo {
            id,
            origin: "supplied",
            camera: format!("{} {}", source.make, source.model),
            why: "supplied by the owner in the RAW manifest".to_owned(),
            bytes: fs::metadata(&file)?.len(),
            path: file,
            extension,
            sha256: source.sha256.clone(),
            dimensions: source.source_dimensions,
        });
    }
    Ok((photos, excluded))
}

/// Refuse a photograph whose file is not the one its manifest pins: its size and SHA-256.
pub fn verify(photo: &Photo) -> Result {
    let metadata = fs::metadata(&photo.path)?;
    ensure(
        metadata.is_file() && metadata.len() == photo.bytes,
        format!("{}: the original is missing or its size changed", photo.id),
    )?;
    ensure(
        hash(&photo.path)? == photo.sha256,
        format!("{}: the original's SHA-256 is not its manifest's", photo.id),
    )
}
