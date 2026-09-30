//! `cargo xtask generate-catalog`: deterministic test data for the catalog, drawn from one shoot
//! [plan] of trips, camera bodies and moments, so every lane tests against the same kind of
//! story before the others have landed. The same seed and counts give the same bytes for every
//! image file and the manifest.
//!
//! `--images N` writes real JPEG files with EXIF and embedded thumbnails under `images/`, with
//! their ground truth in `images/manifest.json` ([images]).
mod clock;
mod images;
mod metadata;
mod plan;
mod random;
mod scene;
#[cfg(test)]
mod tests;

use crate::{Result, ensure};
use std::fs;
use std::path::Path;

/// What to generate.
pub struct Options {
    pub seed: u64,
    pub files: Option<u32>,
    pub assets: Option<u32>,
    pub images: Option<u32>,
}

/// Generates what `options` asks for into `out`, which must not exist.
pub fn run(out: &Path, options: &Options) -> Result {
    ensure(
        options.files.is_some() || options.assets.is_some() || options.images.is_some(),
        "generate-catalog needs --files N, --assets M or --images N",
    )?;
    ensure(
        options.files.is_none() && options.assets.is_none(),
        "--files and --assets are not built yet; only --images is",
    )?;
    ensure(!out.exists(), "Generate-catalog output must be new")?;
    // Every plan is made, and a count too small for it refused, before anything is written.
    let image_plan = options
        .images
        .map(|count| plan::Plan::images(options.seed, count))
        .transpose()?;
    fs::create_dir_all(out)?;
    if let Some(plan) = image_plan {
        let written = images::write(&out.join("images"), options.seed, plan)?;
        println!(
            "Wrote {} images in {} events, and their manifest, to {}",
            written.files,
            written.events,
            out.join("images").display()
        );
    }
    Ok(())
}
