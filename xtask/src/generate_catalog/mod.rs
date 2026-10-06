//! `cargo xtask generate-catalog`: deterministic test data for the catalog, drawn from one shoot
//! [plan] of trips, camera bodies and moments, so every lane tests against the same kind of
//! story before the others have landed. The same seed and counts give the same bytes for every
//! image file and the manifest, and the same rows in the catalog and the index (their database
//! files may differ where SQLite's own bytes do).
//!
//! Into a new directory:
//!
//! - `--images N` writes real JPEG files with EXIF and embedded thumbnails under `images/`, with
//!   their ground truth in `images/manifest.json` ([images]).
//! - `--files N` writes `catalog.sqlite`, a format-13 catalog, and its index
//!   `catalog.index/index.sqlite` of N files on a fictional disk ([disk], [files]), some of them
//!   picked and some developed into the catalog already.
//! - `--assets M` writes M developed photographs into `catalog.sqlite` ([assets]): those developed
//!   from the index's files, when `--files` is given too, and the rest from older trips over
//!   several years.
mod assets;
mod clock;
mod disk;
mod files;
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

/// The catalog a run writes, and its seed.
pub struct Run<'a> {
    pub catalog: &'a Path,
    pub catalog_id: &'a str,
    pub seed: u64,
}

/// The catalog's file name inside the output directory.
pub const CATALOG: &str = "catalog.sqlite";

/// The identity of the catalog seeded from `seed`.
pub fn catalog_id(seed: u64) -> String {
    format!("generated-seed-{seed}")
}

/// Generates what `options` asks for into `out`, which must not exist.
pub fn run(out: &Path, options: &Options) -> Result {
    ensure(
        options.files.is_some() || options.assets.is_some() || options.images.is_some(),
        "generate-catalog needs --files N, --assets M or --images N",
    )?;
    ensure(
        options.assets != Some(0),
        "--assets needs at least one photograph",
    )?;
    ensure(!out.exists(), "Generate-catalog output must be new")?;
    // Every plan is made, and a count it cannot hold refused, before anything is written.
    let image_plan = options
        .images
        .map(|count| plan::Plan::images(options.seed, count))
        .transpose()?;
    let file_plan = options
        .files
        .map(|count| plan::Plan::files(options.seed, count))
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
    if options.files.is_none() && options.assets.is_none() {
        return Ok(());
    }
    let catalog = out.join(CATALOG);
    let catalog_id = catalog_id(options.seed);
    let run = Run {
        catalog: &catalog,
        catalog_id: &catalog_id,
        seed: options.seed,
    };
    let mut library = assets::Library::create(&catalog, &catalog_id, options.seed)?;
    let mut inodes = 0;
    let mut developed = 0;
    // Older photographs are from before the index's first trip, or before September.
    let mut before = plan::SEPTEMBER.add_days(-2);
    if let (Some(plan), Some(total)) = (file_plan, options.files) {
        if let Some(first) = plan.first_day() {
            before = first.add_days(-1);
        }
        let most = options.assets.map(u64::from);
        let written = files::write(&run, plan, total, &mut library, most, &mut inodes)?;
        developed = written.developed;
        println!(
            "Wrote an index of {} files in {} events to {}",
            written.files,
            written.events,
            luxforge_core::index_dir(&catalog).display()
        );
    }
    if let Some(total) = options.assets {
        let older = u64::from(total).saturating_sub(developed) as u32;
        if older > 0 {
            let plan = plan::Plan::developed(options.seed, older, before);
            assets::history(&mut library, options.seed, plan, &mut inodes)?;
        }
    }
    let counts = library.finish(options.files.is_some())?;
    println!(
        "Wrote a catalog of {} photographs in {} folders ({} offline, {} missing, {} changed, {} \
         removed) with {} pending picks to {}",
        counts.photographs,
        counts.folders,
        counts.offline,
        counts.missing,
        counts.changed,
        counts.removed,
        counts.picks,
        catalog.display()
    );
    Ok(())
}
