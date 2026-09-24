//! What the scenarios measure a capture with: where the photograph is drawn and what small patches
//! of it read. Every reader works over a decoded capture, so a frame's capture is decoded once
//! however many readings its checks take; each keeps the arithmetic and iteration order the
//! scenarios' own copies had, so a reading is the same number to the last bit.
use super::Frame;
use crate::*;
use image::RgbImage;

/// Rec. 709 luminance of one 8-bit pixel, in codes.
pub fn luminance(pixel: [u8; 3]) -> f64 {
    0.2126 * f64::from(pixel[0]) + 0.7152 * f64::from(pixel[1]) + 0.0722 * f64::from(pixel[2])
}

/// What a captured frame must show. Defaults describe the fixture at Fit; a crop changes the ratio
/// the displayed image has and, when it is straightened, where its quadrants land.
pub struct Expect {
    /// Which EXIF orientation's quadrant order the fixture was saved with.
    pub orientation: u8,
    /// The displayed ratio, or the fixture's own when `None`.
    pub aspect: Option<f64>,
    /// The physical x range of the editor's photo surface; the whole width without it.
    pub columns: Option<[u32; 2]>,
    /// How far the measured ratio may differ from the expected one.
    pub tolerance: f64,
    /// The smallest fraction of the capture height the image may occupy. A wide crop fills less of
    /// the surface than the fixture does.
    pub min_height: f64,
    /// Whether the image must be centred in the photo surface.
    pub centred: bool,
    /// Whether the four quarter points must show the four quadrant colours. A straightened crop
    /// rotates the quadrant boundaries, so it only requires all four colours to be present.
    pub quadrants: bool,
}

impl Expect {
    pub fn fit(orientation: u8) -> Self {
        Self {
            orientation,
            aspect: None,
            columns: None,
            tolerance: 0.015,
            min_height: 0.5,
            centred: true,
            quadrants: true,
        }
    }
}

/// Check a capture shows the golden quadrant fixture, at Fit unless the expectation says
/// otherwise.
pub fn fixture(img: &RgbImage, expect: &Expect) -> Result<Value> {
    ensure(
        (1..=8).contains(&expect.orientation),
        "Orientation must be 1..8",
    )?;
    let (w, h) = img.dimensions();
    let [surface_left, surface_right] = expect.columns.unwrap_or([0, w]);
    ensure(
        surface_left < surface_right && surface_right <= w,
        "Invalid surface columns",
    )?;
    let surface_width = surface_right - surface_left;
    let colors = crate::fixtures::ORDERS[(expect.orientation - 1) as usize]
        .map(|i| crate::fixtures::COLORS[i]);
    let matches = |p: &[u8], c: [u8; 3]| p.iter().zip(c).all(|(a, b)| a.abs_diff(b) <= 8);
    let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
    let mut counts = [0u32; 4];
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let pixel = &img.get_pixel(x, y).0;
            if let Some(index) = colors.iter().position(|c| matches(pixel, *c)) {
                counts[index] += 1;
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 4);
                bottom = bottom.max(y + 4);
            }
        }
    }
    ensure(
        counts.iter().sum::<u32>() > 0,
        "No fixture pixels: blank or wrong render",
    )?;
    let width = right - left;
    let height = bottom - top;
    ensure(
        left >= surface_left && right <= surface_right,
        "Image outside the photo surface",
    )?;
    ensure(
        width as f64 > surface_width as f64 * 0.2
            && height as f64 > h as f64 * expect.min_height.max(0.0),
        "Fixture too small",
    )?;
    let aspect = expect.aspect.unwrap_or(if expect.orientation >= 5 {
        2.0 / 3.0
    } else {
        3.0 / 2.0
    });
    ensure(aspect.is_finite() && aspect > 0.0, "Invalid aspect")?;
    let measured = width as f64 / height as f64;
    ensure(
        (measured - aspect).abs() < expect.tolerance,
        format!("Incorrect displayed aspect ratio {measured:.4}, expected {aspect:.4}"),
    )?;
    ensure(
        !expect.centred
            || ((left + right) as f64 / 2.0 - (surface_left + surface_right) as f64 / 2.0).abs()
                <= 5.0,
        "Image not centered",
    )?;
    let mut actual = Vec::new();
    if expect.quadrants {
        for ((fx, fy), color) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)]
            .into_iter()
            .zip(colors)
        {
            let x = (left as f64 + fx * width as f64).round() as u32;
            let y = (top as f64 + fy * height as f64).round() as u32;
            ensure(x < w && y < h, "Pixel bounds")?;
            let p = img.get_pixel(x, y).0;
            ensure(matches(&p, color), "Wrong orientation/color")?;
            actual.push(p);
        }
    } else {
        // A straightened crop moves the quadrant boundaries, so prove real content instead: every
        // quadrant colour is still present in quantity.
        ensure(
            counts.iter().all(|count| *count >= 32),
            format!("A quadrant colour is missing from the crop: sampled counts {counts:?}"),
        )?;
    }
    Ok(
        json!({"status":"passed","physical_size":[w,h],"surface_columns":[surface_left,surface_right],"image_bounds":[left,top,right,bottom],"measured_aspect":measured,"expected_aspect":aspect,"aspect_tolerance":expect.tolerance,"quadrant_sample_counts":counts,"corner_rgb":actual,"tolerance_per_channel":8,"scope":"Displayed geometry and sRGB interiors; not monitor calibration or native picker"}),
    )
}

impl Frame {
    /// [`fixture`] over this frame's capture, inside the photo surface it records.
    pub fn fixture(&self, expect: Expect) -> Result<Value> {
        let columns = self.columns()?;
        fixture(self.image()?, &Expect { columns, ..expect })
    }
}

/// The unedited golden fixture, found inside the photo surface alone: the control rail and picker
/// can hold the quadrant colours in the sidebar. Four interior points must still show the
/// orientation-1 image's own colours.
pub fn identity_photo(frame: &Frame) -> Result<Value> {
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let [surface_left, surface_right] = frame.columns()?.unwrap_or([0, width]);
    ensure(
        surface_left < surface_right && surface_right <= width,
        "Invalid photo surface",
    )?;
    let colours = fixtures::COLORS;
    let matches =
        |pixel: [u8; 3], colour: [u8; 3]| pixel.iter().zip(colour).all(|(a, b)| a.abs_diff(b) <= 8);
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    for y in (0..height).step_by(4) {
        for x in (surface_left..surface_right).step_by(4) {
            if colours
                .iter()
                .any(|colour| matches(image.get_pixel(x, y).0, *colour))
            {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 4);
                bottom = bottom.max(y + 4);
            }
        }
    }
    ensure(
        right > left + 120 && bottom > top + 80,
        "Identity photo is absent or too small",
    )?;
    let aspect = f64::from(right - left) / f64::from(bottom - top);
    ensure(
        (aspect - 1.5).abs() < 0.02,
        format!("Identity photo aspect is {aspect}"),
    )?;
    let mut samples = Vec::new();
    for ((fx, fy), colour) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)]
        .into_iter()
        .zip(colours)
    {
        let x = (f64::from(left) + fx * f64::from(right - left)).round() as u32;
        let y = (f64::from(top) + fy * f64::from(bottom - top)).round() as u32;
        ensure(
            x < width && y < height,
            "Identity photo sample outside capture",
        )?;
        let pixel = image.get_pixel(x, y).0;
        ensure(
            matches(pixel, colour),
            format!("Identity photo colour changed at {x},{y}"),
        )?;
        samples.push(pixel);
    }
    Ok(
        json!({"bounds":[left,top,right,bottom],"aspect":aspect,"corner_rgb":samples,
        "tolerance_per_channel":8,"scope":"Displayed photo surface; control sidebar excluded"}),
    )
}

/// The inclusive `(start, end)` of the longest contiguous run of matching positions, or `None` if
/// none matched at all. Used to find a photograph's own drawn extent by its widest row and tallest
/// column: unlike the leftmost-to-rightmost span between any two matches, a contiguous run is never
/// fooled by scattered chrome (title-bar text, an icon) that happens to span a wide gap of
/// unmatched background between its own characters or glyphs.
pub fn longest_run(positions: impl Iterator<Item = (u32, bool)>) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    let mut run_start = None;
    for (position, matched) in positions {
        if matched {
            let start = *run_start.get_or_insert(position);
            if best.is_none_or(|(best_start, best_end)| position - start > best_end - best_start) {
                best = Some((start, position));
            }
        } else {
            run_start = None;
        }
    }
    best
}

/// Which extent of the photograph [`bright_bounds`] measures first. The scenarios' copies of this
/// scan disagreed in order and in one bound, and each keeps its own until one is chosen.
#[derive(Clone, Copy, Debug)]
pub enum Scan {
    /// The tallest bright column first, then the widest bright row among the rows it spans. The
    /// photograph is the tallest bright thing on the surface by a wide margin, while it is not
    /// always the widest: the mode strip is a bright floating bar near the bottom of the canvas that
    /// grows with every mode the host registers. Confining the row scan to the photograph's own rows
    /// reads the photograph whatever the chrome does, and works whether the zoom centres it (Fit) or
    /// anchors it to the photo surface's own top-left corner, which 100% does for a photograph
    /// smaller than the canvas. `last_row` says whether the row scan includes the column's last
    /// bright row (`vignette` and `presets`) or stops before it (the `mask-*` scenarios).
    Tallest { last_row: bool },
    /// The widest bright row of the whole band first, then the tallest bright column of the whole
    /// surface. A scan of every row measures whichever of the photograph and the mode strip happens
    /// to be wider, so this is fooled once the strip outgrows the photograph; `presence` still uses
    /// it.
    Widest,
}

/// How [`bright_bounds`] finds the photograph: a pixel is the photograph's when the mean of its
/// channels is at least `threshold`, and a found rectangle must be larger than `least` (width,
/// height) when that is given.
#[derive(Clone, Copy, Debug)]
pub struct Bright {
    pub threshold: u32,
    pub scan: Scan,
    pub least: Option<(u32, u32)>,
}

/// Where the photograph is drawn, as `[left, top, right, bottom]` with the right and bottom edges
/// inclusive: the bright pixels of the photo surface, clear of the title bar above and the mode
/// strip and status line below, and of the divider that marks each edge of the surface itself.
pub fn bright_bounds(frame: &Frame, bright: Bright) -> Result<[u32; 4]> {
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let [surface_left, surface_right] = frame.columns()?.unwrap_or([0, width]);
    ensure(
        surface_left < surface_right && surface_right <= width,
        "Invalid surface columns",
    )?;
    ensure(
        surface_right - surface_left > 20,
        "Photo surface too narrow to inset from its own edge dividers",
    )?;
    let threshold = bright.threshold;
    let lit = |p: [u8; 3]| (u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) / 3 >= threshold;
    let top_margin = (height / 20).max(20);
    let bottom_margin = height - top_margin;
    let side_inset = 10;
    let (inset_left, inset_right) = (surface_left + side_inset, surface_right - side_inset);
    let column = |x: u32| {
        longest_run((top_margin..bottom_margin).map(|y| (y, lit(image.get_pixel(x, y).0))))
    };
    let row =
        |y: u32| longest_run((inset_left..inset_right).map(|x| (x, lit(image.get_pixel(x, y).0))));
    let tallest = |columns: std::ops::Range<u32>| -> Result<(u32, u32)> {
        let mut tallest: Option<(u32, u32, u32)> = None;
        for x in columns {
            if let Some((top, bottom)) = column(x)
                && tallest.is_none_or(|(h, ..)| bottom - top > h)
            {
                tallest = Some((bottom - top, top, bottom));
            }
        }
        let (_, top, bottom) =
            tallest.ok_or("No photograph in the frame: blank or wrong render")?;
        Ok((top, bottom))
    };
    let widest = |rows: &mut dyn Iterator<Item = u32>| -> Result<(u32, u32)> {
        let mut widest: Option<(u32, u32, u32)> = None;
        for y in rows {
            if let Some((left, right)) = row(y)
                && widest.is_none_or(|(w, ..)| right - left > w)
            {
                widest = Some((right - left, left, right));
            }
        }
        let (_, left, right) = widest.ok_or("No photograph in the frame: blank or wrong render")?;
        Ok((left, right))
    };
    let [left, top, right, bottom] = match bright.scan {
        Scan::Tallest { last_row } => {
            let (top, bottom) = tallest(inset_left..inset_right)?;
            let (left, right) = if last_row {
                widest(&mut (top..=bottom))?
            } else {
                widest(&mut (top..bottom))?
            };
            [left, top, right, bottom]
        }
        Scan::Widest => {
            let (left, right) = widest(&mut (top_margin..bottom_margin))?;
            let (top, bottom) = tallest(inset_left..inset_right)?;
            [left, top, right, bottom]
        }
    };
    if let Some((least_width, least_height)) = bright.least {
        ensure(
            right - left > least_width && bottom - top > least_height,
            format!("Photograph too small to measure: {left}..{right}, {top}..{bottom}"),
        )?;
    }
    Ok([left, top, right, bottom])
}

/// The unrotated golden fixture's drawn rectangle, found from its two top quadrant colours alone
/// and completed from its 480 × 320 aspect: a masked layer that lifts the bottom of the picture
/// cannot move it. A row counts once it holds 32 matching pixels of the photo surface.
pub fn top_quadrant_bounds(frame: &Frame) -> Result<[u32; 4]> {
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let [surface_left, surface_right] = frame.columns()?.unwrap_or([0, width]);
    ensure(
        surface_left < surface_right && surface_right <= width,
        "Invalid surface columns",
    )?;
    // `orientation-1` draws the quadrants in the fixture generator's own order.
    let top = crate::fixtures::ORDERS[0][..2]
        .iter()
        .map(|index| crate::fixtures::COLORS[*index])
        .collect::<Vec<_>>();
    let matches = |pixel: &[u8]| {
        top.iter()
            .any(|colour| pixel.iter().zip(*colour).all(|(a, b)| a.abs_diff(b) <= 8))
    };
    let (mut left, mut right, mut first) = (width, 0u32, None);
    for y in 0..height {
        let row: Vec<u32> = (surface_left..surface_right)
            .filter(|x| matches(&image.get_pixel(*x, y).0))
            .collect();
        if row.len() < 32 {
            continue;
        }
        first.get_or_insert(y);
        left = left.min(*row.first().expect("a matched column"));
        right = right.max(*row.last().expect("a matched column") + 1);
    }
    let top_edge = first.ok_or("No top-quadrant fixture pixels in the capture")?;
    ensure(
        right > left && right - left > 100,
        format!("Photograph too narrow to measure: {left}..{right}"),
    )?;
    // 480 × 320 at Fit: the drawn height follows from the measured width and the source's ratio.
    let drawn = f64::from(right - left) * 320.0 / 480.0;
    let bottom = top_edge + drawn.round() as u32;
    ensure(bottom <= height, "Photograph runs past the capture")?;
    Ok([left, top_edge, right, bottom])
}

/// The bounding box of the photo surface's bright pixels, `[left, top, right, bottom)`, between the
/// notices at the top of the canvas and the floating mode strip at its bottom, for a fixture with no
/// coloured quadrants to match on. The scan starts 4 px inside the surface's own 1 px edge dividers,
/// which are as bright as dark content. Only the photograph is brighter than the canvas surface
/// (`#19191b`) and the bars over it (`#232326`) in that band, provided no notice is drawn.
pub fn band_bounds(frame: &Frame, threshold: u32) -> Result<[u32; 4]> {
    const INSET: u32 = 4;
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let [left_edge, right_edge] = frame.columns()?.unwrap_or([0, width]);
    ensure(
        left_edge + INSET < right_edge.saturating_sub(INSET) && right_edge <= width,
        "Invalid surface columns",
    )?;
    let (band_top, band_bottom) = (height * 3 / 20, height * 22 / 25);
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0u32, 0u32);
    for y in band_top..band_bottom {
        for x in (left_edge + INSET)..(right_edge - INSET) {
            let pixel = image.get_pixel(x, y).0;
            let mean = (u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2])) / 3;
            if mean >= threshold {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    ensure(
        right > left && bottom > top,
        "No photograph in the frame: blank or wrong render",
    )?;
    Ok([left, top, right, bottom])
}

/// The capture position at fractions `at` of `bounds`.
pub fn at(bounds: [u32; 4], at: [f64; 2]) -> (f64, f64) {
    let [left, top, right, bottom] = bounds;
    (
        f64::from(left) + at[0] * f64::from(right - left),
        f64::from(top) + at[1] * f64::from(bottom - top),
    )
}

/// The pixels of the square patch `half` pixels either side of `centre`, row by row, each clamped
/// to the capture.
fn patch(image: &RgbImage, centre: (f64, f64), half: i64) -> impl Iterator<Item = [u8; 3]> + '_ {
    let (width, height) = image.dimensions();
    let (px, py) = centre;
    (-half..=half).flat_map(move |dy| {
        (-half..=half).map(move |dx| {
            let x = (px as i64 + dx).clamp(0, i64::from(width) - 1) as u32;
            let y = (py as i64 + dy).clamp(0, i64::from(height) - 1) as u32;
            image.get_pixel(x, y).0
        })
    })
}

/// Mean Rec. 709 luminance of one small patch.
pub fn mean_luminance(image: &RgbImage, centre: (f64, f64), half: i64) -> Result<f64> {
    let mut total = 0.0;
    let mut count = 0u32;
    for p in patch(image, centre, half) {
        total += luminance(p);
        count += 1;
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(total / f64::from(count))
}

/// Mean Rec. 709 luminance of the patch at fractions `at` of `bounds` in a frame's capture.
pub fn luminance_at(frame: &Frame, bounds: [u32; 4], fraction: [f64; 2], half: i64) -> Result<f64> {
    mean_luminance(frame.image()?, at(bounds, fraction), half)
}

/// Mean 8-bit RGB of one small patch.
pub fn mean_rgb(image: &RgbImage, centre: (f64, f64), half: i64) -> Result<[f64; 3]> {
    let mut totals = [0.0; 3];
    let mut count = 0u32;
    for p in patch(image, centre, half) {
        for (total, channel) in totals.iter_mut().zip(p) {
            *total += f64::from(channel);
        }
        count += 1;
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(totals.map(|total| total / f64::from(count)))
}

/// The darkest Rec. 709 luminance in one small patch: the plain background under a label stroke
/// that a mean would read bright.
pub fn darkest_luminance(image: &RgbImage, centre: (f64, f64), half: i64) -> Result<f64> {
    let mut darkest = f64::INFINITY;
    let mut count = 0u32;
    for p in patch(image, centre, half) {
        darkest = darkest.min(luminance(p));
        count += 1;
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(darkest)
}

/// The lowest and highest per-pixel channel mean in one small patch: the range a texture or
/// contrast gain widens on a neutral fixture.
pub fn grey_range(image: &RgbImage, centre: (f64, f64), half: i64) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in patch(image, centre, half) {
        let mean = (f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])) / 3.0;
        lo = lo.min(mean);
        hi = hi.max(mean);
    }
    (lo, hi)
}

/// The centred window of the photo surface: 40% of its width and 30% of the capture's height about
/// its centre, which lies inside a fitted photograph at either orientation and touches neither the
/// notice cards at the top of the canvas nor the mode strip at its bottom.
fn surface_window(frame: &Frame) -> Result<(&RgbImage, [u32; 4])> {
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let [left, right] = frame.columns()?.unwrap_or([0, width]);
    ensure(left < right && right <= width, "Invalid surface columns")?;
    let surface = right - left;
    let (cx, cy) = ((left + right) / 2, height / 2);
    let (half_w, half_h) = (surface / 5, height * 3 / 20);
    ensure(
        half_w > 10 && half_h > 10 && cx > half_w && cy > half_h,
        "Photo surface too small to sample",
    )?;
    Ok((image, [cx - half_w, cy - half_h, cx + half_w, cy + half_h]))
}

/// Mean Rec. 709 luminance of the photo surface's centred window.
pub fn window_luminance(frame: &Frame) -> Result<f64> {
    let (image, [x0, y0, x1, y1]) = surface_window(frame)?;
    let mut total = 0.0;
    let mut count = 0u32;
    for y in y0..y1 {
        for x in x0..x1 {
            total += luminance(image.get_pixel(x, y).0);
            count += 1;
        }
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(total / f64::from(count))
}

/// Mean per-channel value of the same window.
pub fn window_rgb(frame: &Frame) -> Result<[f64; 3]> {
    let (image, [x0, y0, x1, y1]) = surface_window(frame)?;
    let mut totals = [0.0; 3];
    let mut count = 0u32;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = image.get_pixel(x, y).0;
            for channel in 0..3 {
                totals[channel] += f64::from(p[channel]);
            }
            count += 1;
        }
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(totals.map(|total| total / f64::from(count)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_capture_and_a_bad_surface_fail_the_fixture_check() {
        let blank = RgbImage::new(960, 640);
        assert!(
            fixture(&blank, &Expect::fit(6))
                .unwrap_err()
                .to_string()
                .contains("blank")
        );
        assert!(
            fixture(
                &blank,
                &Expect {
                    columns: Some([10, 5]),
                    ..Expect::fit(6)
                }
            )
            .unwrap_err()
            .to_string()
            .contains("surface columns")
        );
    }

    #[test]
    fn the_longest_run_is_contiguous_and_inclusive() {
        let runs = |matched: &[bool]| {
            longest_run(
                matched
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(i, m)| (i as u32, m)),
            )
        };
        assert_eq!(runs(&[false, true, true, false, true]), Some((1, 2)));
        assert_eq!(runs(&[true, false, true, true, true]), Some((2, 4)));
        assert_eq!(runs(&[false, false]), None);
    }

    #[test]
    fn a_patch_is_clamped_to_the_capture_and_read_row_by_row() {
        let mut image = RgbImage::from_pixel(4, 4, image::Rgb([0, 0, 0]));
        image.put_pixel(0, 0, image::Rgb([255, 255, 255]));
        // Half 1 about the corner reads the corner pixel four times: at (0,0) and three clamps.
        let read: Vec<[u8; 3]> = patch(&image, (0.4, 0.0), 1).collect();
        assert_eq!(read.len(), 9);
        assert_eq!(read.iter().filter(|p| p[0] == 255).count(), 4);
        assert_eq!(
            mean_rgb(&image, (0.0, 0.0), 1).unwrap(),
            [255.0 * 4.0 / 9.0; 3]
        );
        assert_eq!(darkest_luminance(&image, (0.0, 0.0), 1).unwrap(), 0.0);
        assert_eq!(grey_range(&image, (0.0, 0.0), 1), (0.0, 255.0));
        assert!((mean_luminance(&image, (0.0, 0.0), 0).unwrap() - 255.0).abs() < 1e-9);
        assert_eq!(at([10, 20, 30, 60], [0.5, 0.25]), (20.0, 30.0));
    }
}
