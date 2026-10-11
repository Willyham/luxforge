//! `removal-probe score`: score a removal against its ground truth.
//!
//! The report is described in [`crate::score`]. The command fails when any pixel outside the
//! region changed, after printing the report.

use std::path::PathBuf;

use serde_json::json;

use crate::args::Args;
use crate::commands::fill::write_json;
use crate::corpus::{Dirs, Manifest};
use crate::image::Rgb8;
use crate::mask::Mask;
use crate::score::{self, RING_WIDTH};

pub const USAGE: &str = "  score (--case ID | --input IMG --region MASK.png [--truth IMG]) --result IMG [--ring N]
          [--out FILE.json] [--corpus DIR] [--public DIR]
                                           score a removal: PSNR and SSIM inside the region and in its edge ring
                                           (against the truth), the pixels changed outside the region (must be
                                           0; the command fails otherwise) and LPIPS (skipped, with its reason,
                                           until weights are pinned). --case regenerates a corpus.json
                                           pasted-object case and scores against its clean image; --input and
                                           --region score any removal, and without --truth only the outside
                                           check runs. --ring is the edge ring's width in px (default 8)";

pub fn run(mut a: Args) -> Result<(), String> {
    let case_id = a.opt("--case")?;
    let input_path = a.path("--input")?;
    let region_path = a.path("--region")?;
    let truth_path = a.path("--truth")?;
    let result_path = PathBuf::from(a.required("--result")?);
    let ring_width: u32 = a.parsed("--ring")?.unwrap_or(RING_WIDTH);
    let out = a.path("--out")?;
    let dirs = Dirs::new(a.path("--corpus")?, a.path("--public")?);
    a.done()?;

    let result = Rgb8::load(&result_path)?;
    let (input, region, truth, source) = match (case_id, input_path, region_path) {
        (Some(id), None, None) if truth_path.is_none() => {
            let manifest = Manifest::embedded();
            let case = manifest.case(&id)?;
            let clean_path = dirs.resolve(&case.image)?;
            let clean = Rgb8::load(&clean_path)?;
            let pasted = case.paste(&clean);
            (
                pasted.image,
                pasted.region,
                Some(clean),
                json!({"case": id, "truth": clean_path.display().to_string()}),
            )
        }
        (None, Some(input_path), Some(region_path)) => {
            let truth = truth_path.as_ref().map(|p| Rgb8::load(p)).transpose()?;
            (
                Rgb8::load(&input_path)?,
                Mask::load_png(&region_path)?,
                truth,
                json!({
                    "input": input_path.display().to_string(),
                    "region": region_path.display().to_string(),
                    "truth": truth_path.as_ref().map(|p| p.display().to_string()),
                }),
            )
        }
        _ => {
            return Err(
                "give --case ID, or --input and --region (with --truth for the quality scores)"
                    .into(),
            );
        }
    };

    let score = score::score(&input, &result, &region, truth.as_ref(), ring_width)?;
    let mut report = score.to_json();
    report["source"] = source;
    report["result"] = json!(result_path.display().to_string());
    if let Some(out) = &out {
        write_json(out, &report)?;
    }
    println!("{}", serde_json::to_string_pretty(&report).expect("json"));
    if score.outside_unchanged() {
        Ok(())
    } else {
        Err(format!(
            "{} pixels outside the region differ from the input; a removal must change none",
            score.changed_outside
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::remove;
    use crate::fill::{self, Params};
    use crate::models;
    use crate::runtime::{self, Backend};

    fn evidence(name: &str) -> PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/evidence/task-002")
            .join(name);
        std::fs::create_dir_all(&dir).expect("evidence directory");
        dir
    }

    /// A pasted-object case on a private photograph, removed with the pinned LaMa and scored.
    #[test]
    #[ignore = "needs the pinned models and corpus"]
    fn a_pasted_object_is_removed_and_scored_against_its_clean_image() {
        let manifest = Manifest::embedded();
        let case = manifest.case("bridge-deck").unwrap();
        let clean = Rgb8::load(&Dirs::new(None, None).resolve(&case.image).unwrap()).unwrap();
        let pasted = case.paste(&clean);
        let lama = models::LAMA.verified(&models::directory(None)).unwrap();
        let (mut session, _) = runtime::open(Backend::OrtCpu, &lama).unwrap();
        let (result, _) = fill::remove(
            &pasted.image,
            &pasted.region,
            session.as_mut(),
            &Params::default(),
        )
        .unwrap();
        let s = score::score(
            &pasted.image,
            &result,
            &pasted.region,
            Some(&clean),
            RING_WIDTH,
        )
        .unwrap();
        let report = s.to_json();
        write_json(&evidence("pasted").join("score.json"), &report).unwrap();
        assert!(s.outside_unchanged(), "{report}");
        let (region, ring) = (s.region.unwrap(), s.ring.unwrap());
        assert!(
            region.psnr_db.is_finite() && region.psnr_db > 10.0,
            "{report}"
        );
        assert!(region.ssim > 0.0 && region.ssim <= 1.0, "{report}");
        assert!(ring.ssim > 0.0 && ring.ssim <= 1.0, "{report}");
        assert_eq!(report["lpips"]["status"], "skipped");
    }

    /// The boy on the bridge, removed from a point by the whole chain and scored: no ground truth
    /// exists for it, so the one required check is that nothing outside the region changed.
    #[test]
    #[ignore = "needs the pinned models and corpus"]
    fn the_boy_on_the_bridge_is_removed_with_nothing_changed_outside() {
        let bridge = crate::paths::corpus_directory(None).join("z6-bridge.jpg");
        let out = evidence("bridge");
        let args = vec![
            "--image".to_string(),
            bridge.display().to_string(),
            "--point".to_string(),
            "1690,3550".to_string(),
            "--out".to_string(),
            out.display().to_string(),
        ];
        remove::run(Args::new(args)).unwrap();
        let input = Rgb8::load(&bridge).unwrap();
        let result = Rgb8::load(&out.join("result.png")).unwrap();
        let region = Mask::load_png(&out.join("region.png")).unwrap();
        let s = score::score(&input, &result, &region, None, RING_WIDTH).unwrap();
        let report = s.to_json();
        write_json(&out.join("score.json"), &report).unwrap();
        assert!(s.region_pixels > 100_000, "{report}");
        assert_eq!(s.changed_outside, 0, "{report}");
        assert!(s.region.is_none());
    }
}
