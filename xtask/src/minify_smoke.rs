//! The `compare-zone-plate` smoke scenario: the photograph drawn at Fit from a full-resolution
//! texture, judged on a generated zone plate.
//!
//! Before/After keeps the exact raster it displays as its After side, even at Fit, so the photo
//! surface draws a 6000 × 4000 texture a little over a quarter of its size. Without mip levels the
//! sampler skips source texels and folds the detail beyond the display's Nyquist limit into
//! full-contrast replicas of the centre's rings. This scenario opens the generated zone plate at
//! Fit, waits for its exact render, starts the comparison and reveals only After, then reads that
//! capture with [`crate::zone_plate`]'s statistics over the pixels beyond the display's Nyquist
//! limit: the mean and standard deviation of the capture beside those of an independent
//! linear-light box downscale. The display proxy the open drew at Fit is judged the same way as the
//! control, which must read as a box filter does. The state's resident photo-slot bytes must
//! include the After texture's mip levels, and only once it is drawn below its size.
//!
//! `compare-tiled` is the same journey over the generated 60 MP JPEG, 10000 pixels wide, which the
//! device's 8192 pixel textures hold in tiles and so cannot give mip levels. There the comparison
//! draws the display reduction it holds of the exact photograph at Fit, so After alone must read as
//! the display-size photograph did before the comparison, pixel for pixel, with no mip levels
//! resident.
use crate::{
    fixtures,
    scenario::{Checked, Checks, Frame, Plan, Run, Step, plan::only},
    zone_plate::{self, Judgement},
    *,
};
use luxforge_evidence::{self as script, CompareStep};

pub const SCENARIO: &str = "compare-zone-plate";
pub const FIXTURE: &str = "fixtures/generated/zone-plate.jpg";
pub const TILED: &str = "compare-tiled";
pub const TILED_FIXTURE: &str = "fixtures/generated/60mp.jpg";

/// How long the open's frame is left to let the exact render land, which the After side of a
/// comparison is retained from: a display proxy of the stage is not drawn below its own size.
const SETTLE_MS: u64 = 4000;
/// Physical pixels of the photograph's left and right edges the comparison's own chrome covers: the
/// divider's handle and the side's tag.
const CHROME_LEFT: u32 = 80;
const CHROME_RIGHT: u32 = 240;
/// The same for the 60 MP JPEG, whose exact render takes longer.
const TILED_SETTLE_MS: u64 = 10000;

/// The standard deviation a frame may show over the pixels beyond the display's Nyquist limit, as a
/// multiple of the independent box reduction's own there. On this fixture the display proxy reads
/// 1.00 times the reference's, the exact raster drawn without mips 3.55 times (full-contrast replica
/// rings), and with mips 0.69.
const STD_LIMIT: f64 = 2.0;
/// How far the frame's mean there may be from the box reduction's, in codes. The display proxy is
/// 0.00 codes away, the replicas, which average the encoded values rather than the light, 15.3
/// below, and the mipped raster 0.57 above.
const MEAN_LIMIT: f64 = 3.0;

/// Every frame, in order: the open at Fit, the same photograph once its exact render has landed
/// (the control), the comparison started, After alone, Before alone, and the comparison left.
pub fn plan(_: &[PathBuf]) -> Plan {
    plan_settling(SETTLE_MS)
}

pub fn tiled_plan(_: &[PathBuf]) -> Plan {
    plan_settling(TILED_SETTLE_MS)
}

fn plan_settling(settle_ms: u64) -> Plan {
    Plan::new(vec![
        Step::opened("opened").fit(),
        Step::new("settled", script::Step::wait(settle_ms))
            .commits(0)
            .fit(),
        Step::new("compare-slider", CompareStep::Tap)
            .commits(0)
            .fit(),
        Step::new("compare-after", CompareStep::Position(0.0))
            .commits(0)
            .fit(),
        Step::new("compare-before", CompareStep::Position(1.0))
            .commits(0)
            .fit(),
        Step::new(
            "compare-exited",
            script::Step::Key {
                key: script::KEY_ESCAPE.into(),
            },
        )
        .commits(0)
        .fit(),
    ])
}

fn gpu(frame: &Frame, name: &str) -> Result<u64> {
    frame.state()["surface"]["gpu"][name]
        .as_u64()
        .ok_or_else(|| format!("{} lacks surface.gpu.{name}", frame["file"]).into())
}

/// The mip bytes a frame reports resident, or `None`, and a failure naming the frame, when it
/// reports none.
fn mips(frame: &Frame, what: &str, failures: &mut Vec<String>) -> Option<u64> {
    let reported = gpu(frame, "mip_resident_bytes").ok();
    if reported.is_none() {
        failures.push(format!("The {what} frame reports no mip_resident_bytes"));
    }
    reported
}

/// The bytes of every mip level beyond the first of a `width` by `height` texture, as the GPU
/// sizes the chain: each level half the one before, rounded down, to one texel, four bytes a texel.
pub fn mip_chain_bytes((mut width, mut height): (u32, u32)) -> u64 {
    let mut bytes = 0;
    while width > 1 || height > 1 {
        width = (width / 2).max(1);
        height = (height / 2).max(1);
        bytes += u64::from(width) * u64::from(height) * 4;
    }
    bytes
}

fn judged(
    source: &image::RgbImage,
    frame: &Frame,
    checks: &mut Checks,
    shows: &str,
) -> Result<Judgement> {
    let rect = frame.photo()?;
    let judgement = zone_plate::judge(
        source,
        frame.image()?,
        rect,
        fixtures::zone_plate_rate(fixtures::ZONE_PLATE),
    );
    checks.note(
        frame,
        shows,
        json!({
            "photo_rect": rect,
            "beyond_nyquist_pixels": judgement.pixels,
            "beyond_nyquist_fraction": judgement.fraction,
            "captured": {"mean": judgement.captured_mean, "std": judgement.captured_std},
            "reference": {"mean": judgement.reference_mean, "std": judgement.reference_std},
            "std_ratio": judgement.captured_std / judgement.reference_std,
            "mean_difference": judgement.captured_mean - judgement.reference_mean,
        }),
    );
    Ok(judgement)
}

/// The claims that hold for a frame whose reduction is a box filter's, as strings that name those
/// that do not.
fn aliasing(what: &str, judgement: Judgement, failures: &mut Vec<String>) {
    let ratio = judgement.captured_std / judgement.reference_std;
    if ratio > STD_LIMIT {
        failures.push(format!(
            "{what}: standard deviation {:.1} codes beyond the Nyquist limit is {ratio:.2} times the reference's {:.1}, over {STD_LIMIT}: full-contrast replica rings",
            judgement.captured_std, judgement.reference_std
        ));
    }
    let off = judgement.captured_mean - judgement.reference_mean;
    if off.abs() > MEAN_LIMIT {
        failures.push(format!(
            "{what}: mean {:.1} codes beyond the Nyquist limit is {off:+.1} from the reference's {:.1}, past {MEAN_LIMIT}",
            judgement.captured_mean, judgement.reference_mean
        ));
    }
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let source = image::open(
        run.sources()
            .first()
            .ok_or("The scenario opened no source")?,
    )?
    .to_rgb8();
    ensure(
        (source.width(), source.height()) == fixtures::ZONE_PLATE,
        "The opened source is not the generated zone plate",
    )?;
    let mut checks = Checks::new();
    let mut failures = Vec::new();

    // The control: the photograph as the open drew it, a display proxy at about its own size.
    let settled = launch.at("settled")?;
    let control = judged(
        &source,
        settled,
        &mut checks,
        "the display-size photograph at Fit, judged as the comparison's After is",
    )?;
    aliasing("Fit without a comparison", control, &mut failures);

    // The comparison's After side was retained from the exact raster, a texture 6000 × 4000.
    let after = launch.at("compare-after")?;
    let [width, height] = fixtures::ZONE_PLATE.into();
    let retained = after.state()["compare_after"].clone();
    if retained != json!([width, height]) {
        failures.push(format!(
            "The After side is {retained}, not the exact {width} × {height} raster the comparison retains once it has landed"
        ));
    }
    if after.state()["comparison"]["position"] != json!(0.0) {
        failures.push("The After-only frame's divider is not at 0".into());
    }
    let drawn = judged(
        &source,
        after,
        &mut checks,
        "After alone at Fit, its exact raster drawn below its size",
    )?;
    aliasing("Compare at Fit, After alone", drawn, &mut failures);

    // Slot accounting: the mip levels belong to the photo slot that holds them, and exist only for
    // a texture drawn below its size. A frame that does not report them says so as a failure after
    // the checks are written, so the figures of the frames that do survive.
    let resident = gpu(after, "full_resident_bytes")?;
    let in_mips = mips(after, "After-only", &mut failures);
    let chain = mip_chain_bytes((width, height));
    let base = u64::from(width) * u64::from(height) * 4;
    checks.note(
        after,
        "the photo slots with the After texture drawn below its size",
        json!({
            "full_resident_bytes": resident,
            "mip_resident_bytes": in_mips,
            "expected_chain_bytes": chain,
            "base_bytes": base,
            "mip_generations": gpu(after, "mip_generations").ok(),
        }),
    );
    if in_mips.is_some_and(|bytes| bytes != chain) {
        failures.push(format!(
            "The After texture holds {} bytes of mip levels, not the {chain} its chain of {width} × {height} has",
            in_mips.unwrap_or_default()
        ));
    }
    if resident < base + chain {
        failures.push(format!(
            "The resident photo slots hold {resident} bytes, fewer than the After texture's {} with its mip levels",
            base + chain
        ));
    }
    let control_mips = mips(settled, "settled", &mut failures);
    if control_mips.is_some_and(|bytes| bytes != 0) {
        failures.push(format!(
            "The display-size photograph holds {} bytes of mip levels, though it is drawn at its own size",
            control_mips.unwrap_or_default()
        ));
    }
    checks.note(
        launch.at("compare-slider")?,
        "the comparison's slot accounting at the divider's first position",
        json!({"mip_resident_bytes": mips(launch.at("compare-slider")?, "compare-slider", &mut failures), "control_mip_resident_bytes": control_mips}),
    );

    // The comparison leaves cleanly and the mips go with the After texture.
    let exited = launch.at("compare-exited")?;
    ensure(
        exited.state()["comparison"].is_null() && exited.state()["compare_after"].is_null(),
        "The comparison did not end",
    )?;
    if let Some(kept) = mips(exited, "compare-exited", &mut failures).filter(|kept| *kept != 0) {
        failures.push(format!(
            "{kept} bytes of mip levels are still resident after the comparison ended"
        ));
    }
    checks.write(
        &launch.evidence,
        SCENARIO,
        json!({
            "fixture": FIXTURE,
            "limits": {"std_ratio": STD_LIMIT, "mean_codes": MEAN_LIMIT},
            "judged_over": "the pixels of the drawn photograph whose source frequency is beyond the display's Nyquist limit",
            "reference": "the decoded source reduced to the drawn size by an area-weighted box average of its linear light, re-encoded",
            "failures": failures,
        }),
    )?;
    ensure(
        failures.is_empty(),
        format!("compare-zone-plate: {}", failures.join("; ")),
    )
}

/// The tiled journey's claims: After's retained frame is the 60 MP exact raster, its display
/// reduction stands in for it at Fit, no mip level is resident anywhere, and After alone is the
/// display-size photograph the open drew, to within a code.
pub fn verify_tiled(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    let mut failures = Vec::new();
    let settled = launch.at("settled")?;
    let after = launch.at("compare-after")?;
    let retained = after.state()["compare_after"].clone();
    if retained != json!([10000, 6000]) {
        failures.push(format!(
            "The After side is {retained}, not the exact 10000 × 6000 raster the comparison retains once it has landed"
        ));
    }
    let reduced = after.state()["compare_after_reduced_at_fit"].clone();
    if reduced != json!(true) {
        failures.push(format!(
            "The comparison holds no display reduction for Fit ({reduced}), though its exact raster is held in tiles"
        ));
    }
    for (name, frame) in [
        ("settled", settled),
        ("compare-slider", launch.at("compare-slider")?),
        ("compare-after", after),
    ] {
        if mips(frame, name, &mut failures).is_some_and(|bytes| bytes != 0) {
            failures.push(format!(
                "The {name} frame holds mip levels, though the photograph is tiled"
            ));
        }
    }
    let (rect, drawn) = (settled.photo()?, after.photo()?);
    if rect != drawn {
        failures.push(format!(
            "After alone is drawn in {drawn:?}, not where the photograph was, {rect:?}"
        ));
    }
    let (before_image, after_image) = (settled.image()?, after.image()?);
    // Beside the divider's handle at the left edge, and the "After" tag at the top right, which the
    // comparison draws over the photograph's own pixels.
    let [left, top, right, bottom] = rect;
    let (left, right) = (left + CHROME_LEFT, right.saturating_sub(CHROME_RIGHT));
    let mut worst = 0u8;
    for y in top..bottom {
        for x in left..right {
            let (a, b) = (
                before_image.get_pixel(x, y).0,
                after_image.get_pixel(x, y).0,
            );
            worst = worst.max(
                a.iter()
                    .zip(b)
                    .map(|(a, b)| a.abs_diff(b))
                    .max()
                    .unwrap_or(0),
            );
        }
    }
    checks.note(
        after,
        "After alone at Fit over a tiled photograph, against the display-size photograph before the comparison",
        json!({
            "photo_rect": rect,
            "compare_after": retained,
            "compare_after_reduced_at_fit": reduced,
            "largest_channel_difference": worst,
            "full_resident_bytes": gpu(after, "full_resident_bytes").ok(),
            "mip_resident_bytes": gpu(after, "mip_resident_bytes").ok(),
        }),
    );
    if worst > 1 {
        failures.push(format!(
            "After alone differs from the display-size photograph by up to {worst} codes"
        ));
    }
    let exited = launch.at("compare-exited")?;
    ensure(
        exited.state()["comparison"].is_null() && exited.state()["compare_after"].is_null(),
        "The comparison did not end",
    )?;
    checks.write(
        &launch.evidence,
        TILED,
        json!({
            "fixture": TILED_FIXTURE,
            "limits": {"largest_channel_difference": 1},
            "failures": failures,
        }),
    )?;
    ensure(
        failures.is_empty(),
        format!("compare-tiled: {}", failures.join("; ")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mip_chain_of_a_24_mp_texture_is_a_third_of_its_base() {
        let chain = mip_chain_bytes((6000, 4000));
        let base = 6000u64 * 4000 * 4;
        assert!(chain < base / 3 && chain > base / 3 * 98 / 100, "{chain}");
        // Levels of a 5 by 3 texture: 2 by 1, then 1 by 1.
        assert_eq!(mip_chain_bytes((5, 3)), (2 + 1) * 4);
        assert_eq!(mip_chain_bytes((1, 1)), 0);
    }
}
