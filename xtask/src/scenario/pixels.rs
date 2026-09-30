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
pub struct Fixture {
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

impl Fixture {
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
pub fn fixture(img: &RgbImage, expect: &Fixture) -> Result<Value> {
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

/// The canvas surface's own colour, `#19191b`, which is all the canvas shows where no photograph is
/// drawn.
pub const CANVAS: [u8; 3] = [0x19, 0x19, 0x1b];

impl Frame {
    /// [`fixture`] over this frame's capture, inside the photo surface it records.
    pub fn fixture(&self, expect: Fixture) -> Result<Value> {
        let columns = self.columns()?;
        fixture(self.image()?, &Fixture { columns, ..expect })
    }

    /// Where the editor drew the photograph in this frame, `[left, top, right, bottom]` physical
    /// pixels with the right and bottom edges exclusive, as it records it per frame (`photo_rect`):
    /// the rectangle the photo surface lays the photograph out in, snapped as it draws it. A
    /// percentage zoom can carry it past the capture's edges.
    pub fn photo_rect(&self) -> Result<[i64; 4]> {
        serde_json::from_value(self["photo_rect"].clone()).map_err(|_| {
            format!(
                "The frame records no photograph rectangle: {}",
                self["photo_rect"]
            )
            .into()
        })
    }

    /// The one photo locator: [`Frame::photo_rect`], which must lie inside the capture and hold a
    /// drawn picture — at least a quarter of an 8 × 8 grid of samples over it must read something
    /// other than the canvas surface, so a blank or missing render fails here rather than being
    /// measured.
    pub fn photo(&self) -> Result<[u32; 4]> {
        let rect = self.photo_rect()?;
        let (width, height) = self.image()?.dimensions();
        let [left, top, right, bottom] = rect;
        ensure(
            0 <= left
                && left < right
                && right <= i64::from(width)
                && 0 <= top
                && top < bottom
                && bottom <= i64::from(height),
            format!(
                "The photograph's rectangle {rect:?} is not inside the {width} × {height} capture"
            ),
        )?;
        self.drawn(rect.map(|edge| edge as u32))
    }

    /// The part of the photograph on screen: [`Frame::photo_rect`] clipped to the canvas region the
    /// frame records (`canvas_rect`), which a percentage zoom can carry the photograph past, holding
    /// a drawn picture as [`Frame::photo`] requires.
    pub fn visible_photo(&self) -> Result<[u32; 4]> {
        let rect = self.photo_rect()?;
        let canvas: [u32; 4] = serde_json::from_value(self["canvas_rect"].clone())
            .map_err(|_| "The frame records no canvas rectangle")?;
        let [left, top, right, bottom] = [
            rect[0].max(i64::from(canvas[0])),
            rect[1].max(i64::from(canvas[1])),
            rect[2].min(i64::from(canvas[2])),
            rect[3].min(i64::from(canvas[3])),
        ];
        ensure(
            left < right && top < bottom,
            format!("The photograph's rectangle {rect:?} is off the canvas {canvas:?}"),
        )?;
        self.drawn([left, top, right, bottom].map(|edge| edge as u32))
    }

    /// `bounds`, once at least a quarter of an 8 × 8 grid of samples over it reads something other
    /// than the canvas surface.
    fn drawn(&self, bounds: [u32; 4]) -> Result<[u32; 4]> {
        let image = self.image()?;
        let drawn = (0..8)
            .flat_map(|row| (0..8).map(move |column| (column, row)))
            .filter(|(column, row)| {
                let (x, y) = at(
                    bounds,
                    [
                        (f64::from(*column) + 0.5) / 8.0,
                        (f64::from(*row) + 0.5) / 8.0,
                    ],
                );
                let pixel = image.get_pixel(x as u32, y as u32).0;
                pixel.iter().zip(CANVAS).any(|(a, b)| a.abs_diff(b) > 3)
            })
            .count();
        ensure(
            drawn >= 16,
            format!("No photograph drawn in {bounds:?}: blank or wrong render"),
        )?;
        Ok(bounds)
    }

    /// The photograph's recorded edges are where its drawn pixels end: at the middle of each edge,
    /// the pixel just inside is `lit` and the one just outside is the canvas surface. For a frame
    /// with nothing drawn over the photograph's edges, which is what makes a placement claim about
    /// the rectangle a claim about the pixels too.
    pub fn photo_edges(&self, lit: impl Fn([u8; 3]) -> bool) -> Result<[u32; 4]> {
        let [left, top, right, bottom] = self.photo()?;
        let image = self.image()?;
        let (mid_x, mid_y) = ((left + right) / 2, (top + bottom) / 2);
        for (name, inside, outside) in [
            (
                "left",
                (left, mid_y),
                left.checked_sub(1).map(|x| (x, mid_y)),
            ),
            ("right", (right - 1, mid_y), Some((right, mid_y))),
            ("top", (mid_x, top), top.checked_sub(1).map(|y| (mid_x, y))),
            ("bottom", (mid_x, bottom - 1), Some((mid_x, bottom))),
        ] {
            let pixel = |(x, y): (u32, u32)| image.get_pixel(x, y).0;
            let background = outside
                .filter(|(x, y)| *x < image.width() && *y < image.height())
                .map(pixel);
            ensure(
                lit(pixel(inside)) && background.is_some_and(|p| p == CANVAS),
                format!(
                    "The photograph's {name} edge is not where its pixels end: {:?} inside, {background:?} outside",
                    pixel(inside)
                ),
            )?;
        }
        Ok([left, top, right, bottom])
    }

    /// The capture position at fractions `fraction` of the located photograph.
    pub fn photo_at(&self, fraction: [f64; 2]) -> Result<(f64, f64)> {
        Ok(at(self.photo()?, fraction))
    }

    /// Mean Rec. 709 luminance of the patch `half` pixels either side of fractions `fraction` of
    /// the located photograph.
    pub fn luminance_at(&self, fraction: [f64; 2], half: i64) -> Result<f64> {
        mean_luminance(self.image()?, self.photo_at(fraction)?, half)
    }

    /// Mean 8-bit RGB of that patch.
    pub fn rgb_at(&self, fraction: [f64; 2], half: i64) -> Result<[f64; 3]> {
        mean_rgb(self.image()?, self.photo_at(fraction)?, half)
    }

    /// The darkest Rec. 709 luminance in that patch.
    pub fn darkest_at(&self, fraction: [f64; 2], half: i64) -> Result<f64> {
        darkest_luminance(self.image()?, self.photo_at(fraction)?, half)
    }
}

/// The unedited golden fixture where the editor records drawing it, its edges where its pixels
/// end: at least 120 × 80, at the fixture's 3:2, and with four interior points showing the
/// orientation-1 image's own colours.
pub fn identity_photo(frame: &Frame) -> Result<Value> {
    let bounds = frame.photo_edges(|pixel| pixel != CANVAS)?;
    let [left, top, right, bottom] = bounds;
    ensure(
        right > left + 120 && bottom > top + 80,
        "Identity photo is absent or too small",
    )?;
    let aspect = f64::from(right - left) / f64::from(bottom - top);
    ensure(
        (aspect - 1.5).abs() < 0.02,
        format!("Identity photo aspect is {aspect}"),
    )?;
    let image = frame.image()?;
    let matches =
        |pixel: [u8; 3], colour: [u8; 3]| pixel.iter().zip(colour).all(|(a, b)| a.abs_diff(b) <= 8);
    let mut samples = Vec::new();
    for (fraction, colour) in [[0.25, 0.25], [0.75, 0.25], [0.25, 0.75], [0.75, 0.75]]
        .into_iter()
        .zip(fixtures::COLORS)
    {
        let (x, y) = at(bounds, fraction);
        let (x, y) = (x.round() as u32, y.round() as u32);
        let pixel = image.get_pixel(x, y).0;
        ensure(
            matches(pixel, colour),
            format!("Identity photo colour changed at {x},{y}"),
        )?;
        samples.push(pixel);
    }
    Ok(json!({"bounds":bounds,"aspect":aspect,"corner_rgb":samples,
        "tolerance_per_channel":8,"scope":"The photograph the editor records drawing, its edges checked against the capture"}))
}

/// How two readings of a capture must relate, with the threshold the scenario states for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tolerance {
    /// Within this much of each other, inclusive: the same picture, or a patch left where it was.
    Within(f64),
    /// Less than this apart, strictly.
    Under(f64),
    /// At least this far apart, either way: a patch that moved.
    Apart(f64),
    /// More than this far apart, either way, strictly: the negation of `Within`.
    Beyond(f64),
    /// The first above the second by more than this: brighter, lifted.
    Above(f64),
    /// The first no more than this above the second, inclusive: a reading bounded by another.
    AtMost(f64),
}

/// Reading `a` against reading `b` under `tolerance`; the error names `what`, both readings and the
/// threshold. The one comparison every scenario's pixel claims go through.
pub fn compare(what: &str, a: f64, b: f64, tolerance: Tolerance) -> Result {
    match tolerance {
        Tolerance::Within(most) => ensure(
            (a - b).abs() <= most,
            format!("{what}: {a:.2} and {b:.2} differ by more than {most}"),
        ),
        Tolerance::Under(most) => ensure(
            (a - b).abs() < most,
            format!("{what}: {a:.2} and {b:.2} differ by {most} or more"),
        ),
        Tolerance::Apart(least) => ensure(
            (a - b).abs() >= least,
            format!("{what}: {a:.2} and {b:.2} differ by less than {least}"),
        ),
        Tolerance::Beyond(least) => ensure(
            (a - b).abs() > least,
            format!("{what}: {a:.2} and {b:.2} differ by {least} or less"),
        ),
        Tolerance::Above(margin) => ensure(
            a > b + margin,
            format!("{what}: {a:.2} is not above {b:.2} by more than {margin}"),
        ),
        Tolerance::AtMost(margin) => ensure(
            a <= b + margin,
            format!("{what}: {a:.2} is more than {margin} above {b:.2}"),
        ),
    }
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

impl Frame {
    /// The central window of the photograph on screen: the middle 40% of its width and half its
    /// height, inside the photograph at every zoom and clear of its edges.
    fn window(&self) -> Result<[u32; 4]> {
        let [left, top, right, bottom] = self.visible_photo()?;
        let (width, height) = (right - left, bottom - top);
        let window = [
            left + width * 3 / 10,
            top + height / 4,
            right - width * 3 / 10,
            bottom - height / 4,
        ];
        ensure(
            window[0] < window[2] && window[1] < window[3],
            format!(
                "The photograph {:?} is too small to sample",
                [left, top, right, bottom]
            ),
        )?;
        Ok(window)
    }

    /// Mean Rec. 709 luminance of the photograph's central window.
    pub fn window_luminance(&self) -> Result<f64> {
        let (image, [x0, y0, x1, y1]) = (self.image()?, self.window()?);
        let mut total = 0.0;
        let mut count = 0u32;
        for y in y0..y1 {
            for x in x0..x1 {
                total += luminance(image.get_pixel(x, y).0);
                count += 1;
            }
        }
        Ok(total / f64::from(count))
    }

    /// Mean per-channel value of the same window.
    pub fn window_rgb(&self) -> Result<[f64; 3]> {
        let (image, [x0, y0, x1, y1]) = (self.image()?, self.window()?);
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
        Ok(totals.map(|total| total / f64::from(count)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_capture_and_a_bad_surface_fail_the_fixture_check() {
        let blank = RgbImage::new(960, 640);
        assert!(
            fixture(&blank, &Fixture::fit(6))
                .unwrap_err()
                .to_string()
                .contains("blank")
        );
        assert!(
            fixture(
                &blank,
                &Fixture {
                    columns: Some([10, 5]),
                    ..Fixture::fit(6)
                }
            )
            .unwrap_err()
            .to_string()
            .contains("surface columns")
        );
    }

    #[test]
    fn a_comparison_holds_its_threshold_at_the_boundary() {
        assert!(compare("same", 10.0, 11.0, Tolerance::Within(1.0)).is_ok());
        assert!(compare("same", 10.0, 11.5, Tolerance::Within(1.0)).is_err());
        assert!(compare("moved", 10.0, 2.0, Tolerance::Apart(8.0)).is_ok());
        assert!(compare("moved", 2.0, 10.0, Tolerance::Apart(8.0)).is_ok());
        assert!(compare("moved", 10.0, 3.0, Tolerance::Apart(8.0)).is_err());
        assert!(compare("lifted", 23.0, 10.0, Tolerance::Above(12.0)).is_ok());
        // Above is strict, and one-sided.
        assert!(compare("lifted", 22.0, 10.0, Tolerance::Above(12.0)).is_err());
        assert!(compare("lifted", 10.0, 23.0, Tolerance::Above(12.0)).is_err());
        // Under and Beyond are the strict forms of Within and Apart.
        assert!(compare("same", 10.0, 10.5, Tolerance::Under(1.0)).is_ok());
        assert!(compare("same", 10.0, 11.0, Tolerance::Under(1.0)).is_err());
        assert!(compare("moved", 10.0, 11.5, Tolerance::Beyond(1.0)).is_ok());
        assert!(compare("moved", 10.0, 11.0, Tolerance::Beyond(1.0)).is_err());
        let error = compare("the drag", 5.0, 7.25, Tolerance::Within(1.5))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "the drag: 5.00 and 7.25 differ by more than 1.5");
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
