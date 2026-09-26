//! The inputs integration tests build their checks from: repository fixtures, unique scratch paths,
//! exact synthetic sources, single-layer stacks, mutation envelopes, frames and samples through the
//! one render entry point, and the one tolerance rule a rendered byte is held to against an f64
//! reference.
//!
//! These speak `luxforge-core` types, so they serve the core's integration tests, other crates'
//! tests and xtask, not the core's own unit tests (see the crate documentation).
use luxforge_core::{
    EFFECT_FORMAT, Error, Layer, LayerId, LinearImage, LinearSettings, ModuleRegistry, Mutation,
    RECIPE_FORMAT, Raster, Recipe, RenderContext, RenderOptions, RenderSource, Sample, SnapshotId,
    SourceImage,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// The workspace root.
pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A file under the repository's `fixtures/`.
pub fn fixture(name: &str) -> PathBuf {
    repository().join("fixtures").join(name)
}

/// The 480x320 synthetic quadrant JPEG most journeys import.
pub fn jpeg() -> PathBuf {
    fixture("s0/orientation-1.jpg")
}

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

/// A path in the system temporary directory no other test of any process uses: the process, a
/// per-process counter and `name`, which ends in whatever extension the file needs. Whatever a
/// previous run left there is removed first.
pub fn temp_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "luxforge-{}-{}-{name}",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&path);
    path
}

/// A new, empty scratch directory at a [`temp_path`].
pub fn temp_dir(name: &str) -> PathBuf {
    let path = temp_path(name);
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
}

/// A new catalog path, as [`temp_path`] makes one.
pub fn temp_catalog(label: &str) -> PathBuf {
    temp_path(&format!("{label}.sqlite"))
}

/// A fingerprint that names exactly these contents, as a real source's does. The host keys what it
/// caches for a source (Dehaze's atmospheric light among it) by the fingerprint, so two different
/// synthetic sources must never share one.
fn content_fingerprint(kind: &str, width: u32, height: u32, bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update(width.to_le_bytes());
    digest.update(height.to_le_bytes());
    digest.update(bytes);
    let hex: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// A synthetic opaque byte source in row order: exact 8-bit codes, so a rendered byte can be
/// compared with an f64 reference without a decoder's own rounding in the way.
pub fn source_of(width: u32, height: u32, pixels: &[[u8; 3]]) -> SourceImage {
    assert_eq!(
        pixels.len() as u64,
        u64::from(width) * u64::from(height),
        "a {width}x{height} source needs that many pixels"
    );
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        rgba.extend_from_slice(pixel);
        rgba.push(255);
    }
    SourceImage {
        width,
        height,
        fingerprint: content_fingerprint("byte", width, height, &rgba),
        rgba: rgba.into(),
        orientation: 1,
    }
}

/// A planar linear-sRGB source in row order, as a RAW development hands the linear path: the R
/// plane, then G, then B.
pub fn linear_source_of(width: u32, height: u32, pixels: &[[f64; 3]]) -> LinearImage {
    assert_eq!(
        pixels.len() as u64,
        u64::from(width) * u64::from(height),
        "a {width}x{height} source needs that many pixels"
    );
    let mut planes = Vec::with_capacity(pixels.len() * 3);
    for channel in 0..3 {
        planes.extend(pixels.iter().map(|pixel| pixel[channel] as f32));
    }
    let bytes: Vec<u8> = planes
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let fingerprint = content_fingerprint("linear", width, height, &bytes);
    LinearImage::with_fingerprint(width, height, planes, fingerprint)
        .expect("a linear source of matching planes")
}

/// A global layer of `effect` holding `payload`, in the current effect format.
pub fn layer(effect: &str, payload: Value) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: effect.into(),
        effect_format: EFFECT_FORMAT,
        payload,
        mask: None,
        artifacts: Vec::new(),
    }
}

/// A current-format recipe of these layers and no masks.
pub fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers,
        masks: Vec::new(),
        ..Recipe::default()
    }
}

/// The mutation envelope an in-process [`luxforge_core::EditorService`] call carries.
pub fn mutation(revision: u64, request: &str, actor: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: actor.into(),
    }
}

/// A rendered code against the f64 reference's: exact, unless the reference's linear value (clamped
/// to the output range) sits within `relative + relative · |threshold|` of the linear threshold
/// between the two codes, where one code of difference is permitted because production evaluates
/// in f32. Each numerical contract declares its own `relative`.
pub fn assert_code_near_threshold(
    actual: u8,
    expected: u8,
    linear: f64,
    relative: f64,
    case: &str,
) {
    if actual == expected {
        return;
    }
    let difference = i32::from(actual) - i32::from(expected);
    assert!(
        difference.abs() <= 1,
        "{case}: rendered {actual}, reference {expected}"
    );
    let crossed = actual.max(expected);
    assert!(
        crossed >= 1,
        "{case}: code 0 has no lower threshold to sit on"
    );
    let threshold = luxforge_reference::code_threshold(crossed);
    let tolerance = relative + relative * threshold.abs();
    assert!(
        (linear.clamp(0.0, 1.0) - threshold).abs() <= tolerance,
        "{case}: rendered {actual} against {expected}, but the reference value {linear} is not \
         within {tolerance} of the code threshold {threshold}"
    );
}

/// One exact frame of `recipe` over a byte source, through the render entry point in a context of
/// its own.
pub fn render(
    registry: &ModuleRegistry,
    source: &SourceImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
) -> Result<Raster, Error> {
    frame(registry, source.into(), snapshot_id, recipe)
}

/// One exact pixel of `recipe` over a byte source, through the render entry point in a context of
/// its own. `rgba` is `None` outside the output stage.
pub fn sample(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
    x: u32,
    y: u32,
) -> Result<Sample, Error> {
    point(registry, source.into(), recipe, x, y)
}

/// [`render`] over a linear source with the settings its recipe asks of it.
pub fn render_linear(
    registry: &ModuleRegistry,
    image: &LinearImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    settings: LinearSettings,
) -> Result<Raster, Error> {
    frame(
        registry,
        RenderSource::Linear { image, settings },
        snapshot_id,
        recipe,
    )
}

/// [`sample`] over a linear source with the settings its recipe asks of it.
pub fn sample_linear(
    registry: &ModuleRegistry,
    image: &LinearImage,
    recipe: &Recipe,
    settings: LinearSettings,
    x: u32,
    y: u32,
) -> Result<Sample, Error> {
    point(
        registry,
        RenderSource::Linear { image, settings },
        recipe,
        x,
        y,
    )
}

fn frame(
    registry: &ModuleRegistry,
    source: RenderSource<'_>,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
) -> Result<Raster, Error> {
    let context = RenderContext::new();
    luxforge_core::render(registry, source, recipe, RenderOptions::default(), &context)?
        .frame(snapshot_id)
}

fn point(
    registry: &ModuleRegistry,
    source: RenderSource<'_>,
    recipe: &Recipe,
    x: u32,
    y: u32,
) -> Result<Sample, Error> {
    let context = RenderContext::new();
    luxforge_core::render(registry, source, recipe, RenderOptions::default(), &context)?
        .sample(x, y)
}
