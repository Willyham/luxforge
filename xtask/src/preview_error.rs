//! `preview-error`: how far one captured frame is from another over the photograph alone, by the
//! independent CIEDE2000 measure in `luxforge_reference::preview_error`, as the
//! [GPU preview design](../../docs/design/gpu-preview.md#the-preview-error-limit) states it: the
//! mean, the worst 16 × 16 block, the p99 and the signed mean ΔL\*, with a verdict against each
//! class's limits. It reads what the editor already produces and launches nothing.
//!
//! Two ways to name the frames. `--candidate PNG --reference PNG --photo-rect L,T,R,B` takes two
//! captures and the photograph's rectangle (physical pixels, right and bottom exclusive).
//! `--evidence DIR --candidate-frame N --reference-frame M` takes frames `frame-N.png` and
//! `frame-M.png` of one evidence run (a smoke run's `app/` directory, or any `--evidence-dir`),
//! checks each frame's provenance as a scenario does, and reads the rectangle the editor recorded
//! for each (`photo_rect`, clipped to the canvas), which must be the same rectangle in both. The
//! candidate is the frame under test, the reference the frame that replaces it; the signed
//! lightness difference is candidate minus reference.
//!
//! The report always carries both classes' verdicts. `--class pointwise|spatial` picks the one the
//! caller is held to: it is repeated as `verdict` and a miss exits non-zero after the report is
//! written. Without it the command only measures, which is what a baseline is.
//! [`corpus`] is the qualification corpus's reader.
use crate::{scenario::Frame, *};
use image::RgbImage;
use luxforge_reference::preview_error::{self as measure, Class, Rgb8, Statistics, Verdict};

pub mod corpus;

/// The one place the measure's own description lives, so a report says what it measured.
const MEASURE: &str = "CIEDE2000, f64, 8-bit sRGB through linear light, XYZ and CIELAB under D65, \
                       over the photograph rectangle only";

pub fn run(mut a: Args) -> Result {
    let class = a
        .value("--class")?
        .map(|value| {
            let value = value.to_string_lossy().into_owned();
            Class::parse(&value)
                .ok_or_else(|| format!("--class is pointwise or spatial, not {value}"))
        })
        .transpose()?;
    let output = a.value("--output")?.map(PathBuf::from);
    if let Some(output) = &output {
        ensure(
            !output.exists(),
            format!("{} exists: use a new file", output.display()),
        )?;
    }
    let evidence = a.value("--evidence")?.map(PathBuf::from);
    let candidate = a.value("--candidate")?.map(PathBuf::from);
    let reference = a.value("--reference")?.map(PathBuf::from);
    let rect = a
        .value("--photo-rect")?
        .map(|value| parse_rect(&value.to_string_lossy()))
        .transpose()?;
    let frames = (
        number(a.value("--candidate-frame")?)?,
        number(a.value("--reference-frame")?)?,
    );
    a.done()?;
    let report = match (evidence, candidate, reference, rect, frames) {
        (Some(evidence), None, None, None, (Some(candidate), Some(reference))) => {
            evidence_report(&evidence, candidate, reference, class)?
        }
        (None, Some(candidate), Some(reference), Some(rect), (None, None)) => {
            file_report(&candidate, &reference, rect, class)?
        }
        _ => {
            return Err(
                "preview-error takes either --candidate PNG --reference PNG --photo-rect \
                        LEFT,TOP,RIGHT,BOTTOM, or --evidence DIR --candidate-frame N \
                        --reference-frame M, with --class pointwise|spatial and --output NEW_FILE \
                        optional"
                    .into(),
            );
        }
    };
    let verdict = report["verdict"].clone();
    let text = format!("{}\n", serde_json::to_string_pretty(&report)?);
    if let Some(output) = &output {
        fs::write(output, &text)?;
    }
    print!("{text}");
    if let Some(class) = class {
        ensure(
            verdict["passed"] == json!(true),
            format!(
                "the frames miss the {} limits: {}",
                class.name(),
                misses(&verdict)
            ),
        )?;
    }
    Ok(())
}

fn number(value: Option<OsString>) -> Result<Option<u64>> {
    value
        .map(|value| {
            let text = value.to_string_lossy().into_owned();
            text.parse::<u64>()
                .map_err(|_| format!("a frame number is a whole number, not {text}").into())
        })
        .transpose()
}

/// `LEFT,TOP,RIGHT,BOTTOM`, four whole numbers.
fn parse_rect(text: &str) -> Result<[u32; 4]> {
    let parts: Vec<u32> = text
        .split(',')
        .map(|part| part.trim().parse::<u32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| format!("--photo-rect is LEFT,TOP,RIGHT,BOTTOM whole numbers, not {text}"))?;
    <[u32; 4]>::try_from(parts).map_err(|_| {
        format!("--photo-rect is LEFT,TOP,RIGHT,BOTTOM whole numbers, not {text}").into()
    })
}

fn frame(image: &RgbImage) -> std::result::Result<Rgb8<'_>, String> {
    Rgb8::new(image.width(), image.height(), image.as_raw())
}

/// The statistics and both verdicts of `candidate` against `reference` over `rect`, as the report
/// holds them. `class`, when given, is the verdict the caller is held to.
pub fn report(
    candidate: &RgbImage,
    reference: &RgbImage,
    rect: [u32; 4],
    class: Option<Class>,
) -> Result<Value> {
    let stats = measure::compare(frame(candidate)?, frame(reference)?, rect)?;
    let [left, top, ..] = rect;
    let verdicts: Vec<(Class, Verdict)> = [Class::Pointwise, Class::Spatial]
        .into_iter()
        .map(|class| (class, measure::verdict(&stats, class)))
        .collect();
    let mut report = json!({
        "measure": MEASURE,
        "capture_size": [candidate.width(), candidate.height()],
        "photograph_rect": rect,
        "statistics": statistics(&stats, [left, top]),
        "definitions": {
            "block": format!(
                "{0} x {0} pixels, tiled from the photograph's top-left in steps of {0}; the last \
                 block in each direction is moved inward to end at the photograph's edge, so no \
                 block is partial; a photograph under {0} across is one block",
                measure::BLOCK
            ),
            "p99": "nearest rank: the value at 1-based rank ceil(0.99 n) of the ascending per-pixel \
                    values, the largest left after discarding the worst floor(n / 100) pixels",
            "signed_mean_delta_l": "mean of L*(candidate) - L*(reference); positive is lighter",
        },
        "verdicts": verdicts
            .iter()
            .map(|(class, verdict)| (class.name().to_owned(), verdict_json(verdict)))
            .collect::<serde_json::Map<_, _>>(),
        "scope": "A measurement of two 8-bit frames; it does not say what produced either",
    });
    if let Some(class) = class {
        report["class"] = json!(class.name());
        report["verdict"] = verdict_json(&measure::verdict(&stats, class));
    } else {
        report["verdict"] = Value::Null;
    }
    Ok(report)
}

/// The four statistics and the diagnostics beside them. The worst block's origin is in capture
/// pixels, so it can be found in the frame.
fn statistics(stats: &Statistics, origin: [u32; 2]) -> Value {
    json!({
        "pixels": stats.pixels,
        "mean_de00": stats.mean,
        "worst_block_mean_de00": stats.worst_block,
        "worst_block_origin": [origin[0] + stats.worst_block_origin[0], origin[1] + stats.worst_block_origin[1]],
        "p99_de00": stats.p99,
        "mean_delta_l": stats.mean_delta_l,
        "max_de00": stats.max,
    })
}

fn verdict_json(verdict: &Verdict) -> Value {
    json!({
        "passed": verdict.passed(),
        "limits": {
            "mean_de00": verdict.limits.mean,
            "worst_block_mean_de00": verdict.limits.worst_block,
            "p99_de00": verdict.limits.p99,
            "mean_delta_l_abs": verdict.limits.mean_delta_l,
        },
        "within": {
            "mean_de00": verdict.mean,
            "worst_block_mean_de00": verdict.worst_block,
            "p99_de00": verdict.p99,
            "mean_delta_l": verdict.mean_delta_l,
        },
    })
}

/// Which statistics a verdict missed, for the refusal's message.
fn misses(verdict: &Value) -> String {
    let missed: Vec<&str> = verdict["within"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, within)| **within == json!(false))
        .map(|(name, _)| name.as_str())
        .collect();
    missed.join(", ")
}

fn file_report(
    candidate: &Path,
    reference: &Path,
    rect: [u32; 4],
    class: Option<Class>,
) -> Result<Value> {
    let (candidate_image, reference_image) = (
        image::open(candidate)?.to_rgb8(),
        image::open(reference)?.to_rgb8(),
    );
    let mut report = report(&candidate_image, &reference_image, rect, class)?;
    report["candidate"] = json!({"file": candidate.display().to_string()});
    report["reference"] = json!({"file": reference.display().to_string()});
    Ok(report)
}

/// One frame of an evidence run, found by its capture's number and identified as a scenario's
/// frames are.
fn evidence_frame(evidence: &Path, app: &Value, number: u64) -> Result<Frame> {
    let name = format!("frame-{number}.png");
    let record = app["frames"]
        .as_array()
        .ok_or("The run's result.json lists no frames")?
        .iter()
        .find(|record| record["file"] == json!(name))
        .ok_or_else(|| format!("The run holds no {name}"))?;
    Frame::identified(evidence, app, record)
}

/// What a frame's recorded state says about how its picture was produced, so a report is
/// correlated with the editor's own account of each frame.
fn frame_account(frame: &Frame) -> Value {
    let state = frame.state();
    json!({
        "file": frame["file"],
        "step": frame["step"]["request"],
        "committed_revision": state["stack"]["revision"],
        "displayed_draft_revision": state["displayed_draft_revision"],
        "status_render": state["status_bar"]["render"],
        "render_approximate": state["status_bar"]["render_approximate"],
        "render_proxy": state["status_bar"]["render_proxy"],
        "proxy_presented": state["proxy"]["presented"],
        "settled_from_exact": state["proxy"]["settled_from_exact"],
        "drawn_region_quality": state["surface"]["gpu"]["drawn_region_quality"],
        "view": state["surface"]["view"],
    })
}

fn evidence_report(
    evidence: &Path,
    candidate: u64,
    reference: u64,
    class: Option<Class>,
) -> Result<Value> {
    let app = read_json(&evidence.join("result.json"))?;
    ensure(
        app["status"] == "captured",
        "The evidence run's result is not a captured run",
    )?;
    let (candidate_frame, reference_frame) = (
        evidence_frame(evidence, &app, candidate)?,
        evidence_frame(evidence, &app, reference)?,
    );
    // The part of the photograph on screen, as the scenarios locate it; it must be the same part
    // of the capture in both, or the frames show different views.
    let rect = candidate_frame.visible_photo()?;
    ensure(
        reference_frame.visible_photo()? == rect,
        format!(
            "The frames record different photograph rectangles: {rect:?} and {:?}",
            reference_frame.visible_photo()?
        ),
    )?;
    let mut report = report(
        candidate_frame.image()?,
        reference_frame.image()?,
        rect,
        class,
    )?;
    report["candidate"] = frame_account(&candidate_frame);
    report["reference"] = frame_account(&reference_frame);
    report["evidence"] = json!(evidence.display().to_string());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    const BASE: [u8; 3] = [120, 120, 120];
    const OFF: [u8; 3] = [150, 120, 120];

    fn image_with(patch: Option<[u32; 4]>, canvas: [u8; 3]) -> RgbImage {
        // A 96 x 64 capture whose photograph is [16, 8, 80, 56]; the canvas around it is `canvas`.
        RgbImage::from_fn(96, 64, |x, y| {
            let inside = (16..80).contains(&x) && (8..56).contains(&y);
            let patched =
                patch.is_some_and(|p| (p[0]..p[2]).contains(&x) && (p[1]..p[3]).contains(&y));
            Rgb(if patched {
                OFF
            } else if inside {
                BASE
            } else {
                canvas
            })
        })
    }

    #[test]
    fn preview_error_reports_the_four_statistics_and_both_verdicts() {
        let reference = image_with(None, [25, 25, 27]);
        // A candidate that differs on a 16 x 16 patch, one block of the photograph's grid (which starts
        // at its top-left, [16, 8]), and, wildly, on the canvas.
        let candidate = image_with(Some([32, 24, 48, 40]), [255, 0, 255]);
        let report = report(&candidate, &reference, [16, 8, 80, 56], None).unwrap();
        let d = luxforge_reference::preview_error::delta_e00_srgb8(BASE, OFF);
        let stats = &report["statistics"];
        assert_eq!(stats["pixels"], 64 * 48);
        let close = |value: &Value, expected: f64| {
            assert!(
                (value.as_f64().unwrap() - expected).abs() < 1e-12,
                "{value} against {expected}"
            );
        };
        close(&stats["mean_de00"], d * 256.0 / 3072.0);
        close(&stats["worst_block_mean_de00"], d);
        assert_eq!(stats["worst_block_origin"], json!([32, 24]));
        close(&stats["p99_de00"], d);
        close(&stats["max_de00"], d);
        assert!(
            stats["mean_delta_l"].as_f64().unwrap() > 0.0,
            "the candidate is lighter"
        );
        // The canvas's wild difference changed nothing, and the patch misses both classes' block
        // limits (d is about 11).
        for class in ["pointwise", "spatial"] {
            assert_eq!(report["verdicts"][class]["passed"], false, "{class}");
            assert_eq!(
                report["verdicts"][class]["within"]["worst_block_mean_de00"],
                false
            );
        }
        assert_eq!(report["verdict"], Value::Null);
        assert_eq!(report["photograph_rect"], json!([16, 8, 80, 56]));
        assert_eq!(report["capture_size"], json!([96, 64]));
        assert!(
            report["definitions"]["block"]
                .as_str()
                .unwrap()
                .contains("moved inward")
        );

        // A selected class repeats its own verdict.
        let report = super::report(
            &candidate,
            &reference,
            [16, 8, 80, 56],
            Some(Class::Spatial),
        )
        .unwrap();
        assert_eq!(report["class"], "spatial");
        assert_eq!(report["verdict"], report["verdicts"]["spatial"]);
        assert_eq!(report["verdict"]["limits"]["p99_de00"], 5.0);

        // Frames that agree inside the photograph pass whatever the canvas holds.
        let report = super::report(
            &image_with(None, [255, 0, 255]),
            &reference,
            [16, 8, 80, 56],
            Some(Class::Pointwise),
        )
        .unwrap();
        assert_eq!(report["statistics"]["mean_de00"], 0.0);
        assert_eq!(report["verdict"]["passed"], true);
    }

    #[test]
    fn preview_error_refuses_what_it_cannot_measure() {
        let frame = image_with(None, [25, 25, 27]);
        let small = RgbImage::new(8, 8);
        assert!(
            report(&frame, &small, [0, 0, 8, 8], None)
                .unwrap_err()
                .to_string()
                .contains("differ in size")
        );
        assert!(
            report(&frame, &frame, [0, 0, 97, 64], None)
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
        assert!(
            parse_rect("1,2,3").is_err()
                && parse_rect("1,2,3,x").is_err()
                && parse_rect("1,2,3,4,5").is_err()
        );
        assert_eq!(parse_rect("16, 8,80,56").unwrap(), [16, 8, 80, 56]);
        assert!(number(Some("x".into())).is_err());
        assert_eq!(number(Some("7".into())).unwrap(), Some(7));
    }

    fn args(values: &[&str]) -> Args {
        Args(values.iter().map(OsString::from).collect())
    }

    #[test]
    fn preview_error_command_writes_json_and_holds_a_class_to_its_limits() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let reference = dir.join("reference.png");
        let identical = dir.join("identical.png");
        let patched = dir.join("patched.png");
        image_with(None, [25, 25, 27]).save(&reference).unwrap();
        image_with(None, [255, 0, 255]).save(&identical).unwrap();
        image_with(Some([32, 24, 48, 40]), [25, 25, 27])
            .save(&patched)
            .unwrap();
        let path = |p: &Path| p.display().to_string();

        let out = dir.join("identical.json");
        run(args(&[
            "--candidate",
            &path(&identical),
            "--reference",
            &path(&reference),
            "--photo-rect",
            "16,8,80,56",
            "--class",
            "pointwise",
            "--output",
            &path(&out),
        ]))
        .unwrap();
        let written = read_json(&out).unwrap();
        assert_eq!(written["statistics"]["mean_de00"], 0.0);
        assert_eq!(written["verdict"]["passed"], true);
        assert_eq!(written["candidate"]["file"], path(&identical));
        // A report is never overwritten.
        let again = run(args(&[
            "--candidate",
            &path(&identical),
            "--reference",
            &path(&reference),
            "--photo-rect",
            "16,8,80,56",
            "--output",
            &path(&out),
        ]));
        assert!(again.unwrap_err().to_string().contains("exists"));

        // A miss writes the report and then fails, naming the statistics that missed.
        let missed = dir.join("patched.json");
        let error = run(args(&[
            "--candidate",
            &path(&patched),
            "--reference",
            &path(&reference),
            "--photo-rect",
            "16,8,80,56",
            "--class",
            "pointwise",
            "--output",
            &path(&missed),
        ]))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("pointwise") && error.contains("worst_block_mean_de00"),
            "{error}"
        );
        assert_eq!(read_json(&missed).unwrap()["verdict"]["passed"], false);
        // Without a class it only measures.
        run(args(&[
            "--candidate",
            &path(&patched),
            "--reference",
            &path(&reference),
            "--photo-rect",
            "16,8,80,56",
        ]))
        .unwrap();

        // Mixed or incomplete ways of naming the frames, an unknown class and an unknown argument.
        for bad in [
            vec!["--candidate", "a.png"],
            vec!["--evidence", "dir", "--candidate-frame", "1"],
            vec![
                "--evidence",
                "dir",
                "--candidate",
                "a.png",
                "--reference",
                "b.png",
                "--photo-rect",
                "0,0,1,1",
            ],
            vec!["--class", "other"],
            vec!["--surprise"],
        ] {
            assert!(run(args(&bad)).is_err(), "{bad:?}");
        }
    }

    /// An evidence run of `frames` captures, each `(image, photo_rect)`, laid out as the editor
    /// writes one: `result.json`, `frame-N.png` and the `state-N.json` beside each, equal to its
    /// record.
    fn run_dir(dir: &Path, frames: &[(RgbImage, [u32; 4])]) {
        let records: Vec<Value> = frames
            .iter()
            .enumerate()
            .map(|(index, (image, rect))| {
                let number = index + 1;
                image.save(dir.join(format!("frame-{number}.png"))).unwrap();
                let record = json!({
                    "file": format!("frame-{number}.png"),
                    "capture_provenance": "window-renderer-readback",
                    "photo_rect": rect,
                    "canvas_rect": [0, 0, image.width(), image.height()],
                    "step": {"request": {"wait": {"ms": 1}}, "status": "sent", "step": number},
                    "state": {
                        "run_id": "run-1",
                        "backend": {"backend": "Metal", "adapter": "test"},
                        "stack": {"revision": 3},
                        "status_bar": {"render": "Approximate render · 9 ms", "render_approximate": true, "render_proxy": true},
                        "proxy": {"presented": true, "settled_from_exact": false},
                        "surface": {"gpu": {"drawn_region_quality": null}, "view": {"zoom": {"mode": "fit"}}},
                    },
                });
                write_json(&dir.join(format!("state-{number}.json")), &record).unwrap();
                record
            })
            .collect();
        write_json(
            &dir.join("result.json"),
            &json!({"status": "captured", "run_id": "run-1", "frames": records}),
        )
        .unwrap();
    }

    #[test]
    fn preview_error_reads_the_photograph_rectangle_from_an_evidence_run() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let rect = [16, 8, 80, 56];
        run_dir(
            dir,
            &[
                (image_with(Some([32, 24, 48, 40]), [255, 0, 255]), rect),
                (image_with(None, [25, 25, 27]), rect),
                (image_with(None, [25, 25, 27]), [16, 8, 79, 56]),
            ],
        );
        let report = evidence_report(dir, 1, 2, Some(Class::Spatial)).unwrap();
        let d = luxforge_reference::preview_error::delta_e00_srgb8(BASE, OFF);
        assert!(
            (report["statistics"]["worst_block_mean_de00"]
                .as_f64()
                .unwrap()
                - d)
                .abs()
                < 1e-12
        );
        assert_eq!(report["photograph_rect"], json!(rect));
        // Each frame's own account of how it was produced travels with the figures.
        assert_eq!(report["candidate"]["file"], "frame-1.png");
        assert_eq!(report["candidate"]["render_proxy"], true);
        assert_eq!(
            report["reference"]["status_render"],
            "Approximate render · 9 ms"
        );
        assert_eq!(
            report["reference"]["view"],
            json!({"zoom": {"mode": "fit"}})
        );

        let error = |candidate, reference| {
            evidence_report(dir, candidate, reference, None)
                .unwrap_err()
                .to_string()
        };
        assert!(error(1, 3).contains("different photograph rectangles"));
        assert!(error(1, 9).contains("holds no frame-9.png"));
        // A capture the run did not record, or one that no longer matches its state file.
        write_json(&dir.join("state-2.json"), &json!({"file": "frame-2.png"})).unwrap();
        assert!(error(1, 2).contains("state file"));
        // A run that never finished is not measured.
        write_json(
            &dir.join("result.json"),
            &json!({"status": "failed", "frames": []}),
        )
        .unwrap();
        assert!(error(1, 2).contains("not a captured run"));
    }
}
