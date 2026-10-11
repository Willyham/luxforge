//! `removal-probe paste`: generate a pasted-object ground-truth case from a clean image.
//!
//! The clean image is the truth. The case is the same image with a synthetic object and shadow
//! composited in, and the region that covers every changed pixel. Generation is deterministic.

use std::path::Path;

use serde_json::json;

use crate::args::Args;
use crate::commands::fill::write_json;
use crate::corpus::{Case, Dirs, Manifest};
use crate::image::Rgb8;

pub const USAGE: &str = "  paste (--case ID | --image IMG [--centre FX,FY] [--size FW,FH] [--dilate N]) --out DIR
          [--corpus DIR] [--public DIR]    paste a deterministic object onto a clean image; write pasted.png,
                                           region.png and case.json to DIR. --case takes a corpus.json case;
                                           --centre is the object's centre as fractions of the image (default
                                           0.5,0.5) and --size its width,height as fractions of the shorter
                                           side (default 0.15,0.09). Remove it with
                                           `fill --image DIR/pasted.png --mask DIR/region.png`, then `score`";

pub fn run(mut a: Args) -> Result<(), String> {
    let case_id = a.opt("--case")?;
    let image = a.path("--image")?;
    let centre = a.opt("--centre")?;
    let size = a.opt("--size")?;
    let dilate: Option<u32> = a.parsed("--dilate")?;
    let out = a.path("--out")?.ok_or("--out is required")?;
    let dirs = Dirs::new(a.path("--corpus")?, a.path("--public")?);
    a.done()?;

    let (case, clean_path) = match (case_id, image) {
        (Some(id), None) => {
            if centre.is_some() || size.is_some() || dilate.is_some() {
                return Err(
                    "--case fixes the placement; give --image to place one yourself".into(),
                );
            }
            let manifest = Manifest::embedded();
            let case = manifest.case(&id)?.clone();
            let path = dirs.resolve(&case.image)?;
            (case, path)
        }
        (None, Some(path)) => (
            Case {
                id: "custom".into(),
                image: path.display().to_string(),
                centre: fractions(centre.as_deref().unwrap_or("0.5,0.5"), "--centre")?,
                size: fractions(size.as_deref().unwrap_or("0.15,0.09"), "--size")?,
                dilate: dilate.unwrap_or(6),
            },
            path,
        ),
        _ => return Err("give exactly one of --case and --image".into()),
    };
    generate(&case, &clean_path, &out)
}

/// Writes the case's `pasted.png`, `region.png` and `case.json` to `out`.
pub fn generate(case: &Case, clean_path: &Path, out: &Path) -> Result<(), String> {
    let clean = Rgb8::load(clean_path)?;
    let pasted = case.paste(&clean);
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    pasted.image.save_png(&out.join("pasted.png"))?;
    pasted.region.save_png(&out.join("region.png"))?;
    let (centre, size, dilate) = case.placement((clean.width, clean.height));
    let report = json!({
        "case": case.id,
        "truth": clean_path.display().to_string(),
        "image": [clean.width, clean.height],
        "centre_px": [centre.0, centre.1],
        "size_px": [size.0, size.1],
        "dilate_px": dilate,
        "region_pixels": pasted.region.data.iter().filter(|&&v| v > 0).count(),
        "pasted": out.join("pasted.png").display().to_string(),
        "region": out.join("region.png").display().to_string(),
    });
    write_json(&out.join("case.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report).expect("json"));
    Ok(())
}

fn fractions(v: &str, name: &str) -> Result<[f64; 2], String> {
    let (a, b) = v
        .split_once(',')
        .ok_or_else(|| format!("{name}: expected A,B, got {v:?}"))?;
    let p = |s: &str| {
        s.trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0 && *v < 1.0)
            .ok_or_else(|| format!("{name}: expected fractions between 0 and 1, got {v:?}"))
    };
    Ok([p(a)?, p(b)?])
}
