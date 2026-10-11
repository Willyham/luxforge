//! The merge preview's numerics over generated brackets: the colour proxy against a direct box
//! mean, the picture of an aligned bracket against the box mean of the full-resolution fusion, the
//! proxy-scale registration against the full-resolution one, the deghost overlay's outlines
//! against their regions, and the display transform and its JPEG.
//!
//! Declared tolerance: an aligned, noise-free bracket's picture against the box mean of its
//! full-resolution fusion, `1e-5 · max|reference| + 1e-9` per channel.

use super::preview::{OUTLINE_TOLERANCE, simplify, trace};
use super::*;
use crate::{Cancel, ErrorKind};

/// The scene's linear radiance at full-resolution pixel `(x, y)` of a `width × height` frame: a
/// ramp, a bright patch the reference and the longer exposure clip, and a dark patch under the
/// reference's shadow level.
fn scene(x: usize, y: usize, width: usize, height: usize) -> [f64; 3] {
    let ramp = 0.02 + 0.6 * x as f64 / width as f64 + 0.25 * y as f64 / height as f64;
    let ripple = 0.05 * ((x as f64 * 0.21).sin() * (y as f64 * 0.17).cos());
    let mut luma = ramp + ripple;
    if (width / 8..width / 4).contains(&x) && (height / 8..height / 3).contains(&y) {
        luma = 3.0;
    }
    if (width / 2..width / 2 + width / 8).contains(&x) && (height / 2..height * 3 / 4).contains(&y)
    {
        luma = 0.004;
    }
    [luma, 0.8 * luma, 0.6 * luma]
}

/// One input of the scene at `offset_ev`, as a sensor with its white at 1 records it: samples
/// clipped at 1, each pixel's level its largest channel.
struct Frame {
    width: usize,
    height: usize,
    rgb: [Vec<f32>; 3],
    levels: Vec<SensorLevel>,
}

impl Frame {
    fn record(
        width: usize,
        height: usize,
        offset_ev: f64,
        scene: &dyn Fn(usize, usize) -> [f64; 3],
    ) -> Self {
        let mut rgb: [Vec<f32>; 3] = std::array::from_fn(|_| Vec::with_capacity(width * height));
        let mut levels = Vec::with_capacity(width * height);
        let gain = offset_ev.exp2();
        for y in 0..height {
            for x in 0..width {
                let radiance = scene(x, y);
                let recorded = radiance.map(|value| value * gain);
                let peak = recorded.iter().copied().fold(0.0, f64::max);
                for (plane, value) in rgb.iter_mut().zip(recorded) {
                    plane.push(value.min(1.0) as f32);
                }
                levels.push(SensorLevel::from_fraction(peak));
            }
        }
        Self {
            width,
            height,
            rgb,
            levels,
        }
    }

    fn rows(&self, top: usize, rows: usize) -> FrameRect<'_> {
        let span = top * self.width..(top + rows) * self.width;
        FrameRect {
            width: self.width,
            height: rows,
            stride: self.width,
            rgb: [
                &self.rgb[0][span.clone()],
                &self.rgb[1][span.clone()],
                &self.rgb[2][span.clone()],
            ],
            levels: &self.levels[span],
        }
    }

    fn view(&self) -> FrameRect<'_> {
        self.rows(0, self.height)
    }

    /// Both proxies, from bands of `band` rows as the spool writes them.
    fn proxies(&self, band: usize) -> (AnalysisProxy, ColourProxy) {
        let (w, h) = (self.width as u32, self.height as u32);
        let mut analysis = ProxyBuilder::new(w, h).unwrap();
        let mut colour = ColourProxyBuilder::new(w, h).unwrap();
        for top in (0..self.height).step_by(band) {
            let rows = self.rows(top, band.min(self.height - top));
            analysis.push_rows(&rows).unwrap();
            colour.push_rows(&rows).unwrap();
        }
        (analysis.finish().unwrap(), colour.finish().unwrap())
    }
}

fn exposures(offsets: &[f64], reference: usize) -> ResolvedExposures {
    ResolvedExposures {
        reference,
        reference_origin: ReferenceOrigin::Requested,
        inputs: offsets
            .iter()
            .enumerate()
            .map(|(i, &offset_ev)| ResolvedExposure {
                offset_ev,
                scale: (-offset_ev).exp2(),
                origin: if i == reference {
                    ExposureOrigin::Reference
                } else {
                    ExposureOrigin::Metadata {
                        source: MetadataSource::Settings,
                    }
                },
            })
            .collect(),
    }
}

fn registration(width: usize, height: usize, transforms: &[RigidTransform]) -> Registration {
    Registration {
        mode: AlignmentMode::Auto,
        reference: 1,
        width: width as u32,
        height: height as u32,
        inputs: transforms
            .iter()
            .enumerate()
            .map(|(i, transform)| InputAlignment {
                transform: *transform,
                evidence: if i == 1 {
                    AlignmentEvidence::Reference
                } else {
                    AlignmentEvidence::Fitted {
                        via: None,
                        correlation: 0.9,
                        samples: 4096,
                        inlier_fraction: 0.9,
                        residual_ev: 0.02,
                        brightness_ev: 0.0,
                        uncertainty_px: 0.03 * (i + 1) as f64,
                        confidence: 0.9,
                        snapped_px: None,
                    }
                },
            })
            .collect(),
    }
}

#[test]
fn merge_preview_colour_proxy_equals_a_direct_box_mean() {
    // A factor of 3 (2100 / 1024 rounded up), with rows past the last whole box.
    let (width, height) = (2100, 302);
    let bracket = |x: usize, y: usize| scene(x, y, width, height);
    let frame = Frame::record(width, height, 0.0, &bracket);
    let factor = AnalysisProxy::factor_for(width as u32, height as u32) as usize;
    assert_eq!(factor, 3);
    let (_, proxy) = frame.proxies(16);
    let (pw, ph) = proxy.dimensions();
    assert_eq!((pw, ph), (700, 100));
    assert_eq!(proxy.factor(), 3);
    assert_eq!(proxy.frame_dimensions(), (2100, 302));
    assert_eq!(
        ColourProxy::bytes_for(width as u32, height as u32),
        pw * ph * 14
    );
    let view = proxy.view();
    for j in 0..ph {
        for i in 0..pw {
            let mut sums = [0.0_f64; 3];
            let mut peak = SensorLevel(0);
            for y in j * factor..(j + 1) * factor {
                for x in i * factor..(i + 1) * factor {
                    let (rgb, level) = frame.view().at(x, y);
                    for c in 0..3 {
                        sums[c] += f64::from(rgb[c]);
                    }
                    peak = peak.max(level);
                }
            }
            let (rgb, level) = view.at(i, j);
            let area = (factor * factor) as f64;
            for c in 0..3 {
                assert_eq!(
                    rgb[c].to_bits(),
                    ((sums[c] / area) as f32).to_bits(),
                    "proxy pixel ({i}, {j}) channel {c}"
                );
            }
            assert_eq!(level, peak, "proxy pixel ({i}, {j})'s largest level");
        }
    }
    // Any split of the rows into bands builds the same proxy.
    for band in [1, 7, 302] {
        assert_eq!(frame.proxies(band).1, proxy, "{band}-row bands");
    }
    // Refusals: a row of another width, rows past the frame, missing rows, a non-finite sample.
    let mut builder = ColourProxyBuilder::new(width as u32, height as u32).unwrap();
    let narrow = FrameRect {
        width: width - 1,
        ..frame.rows(0, 1)
    };
    assert!(builder.push_rows(&narrow).is_err());
    builder.push_rows(&frame.rows(0, 4)).unwrap();
    assert!(builder.finish().is_err());
    let mut builder = ColourProxyBuilder::new(width as u32, height as u32).unwrap();
    builder.push_rows(&frame.view()).unwrap();
    assert!(builder.push_rows(&frame.rows(0, 1)).is_err());
    let mut bad = Frame::record(width, 3, 0.0, &bracket);
    bad.rgb[1][5] = f32::NAN;
    let mut builder = ColourProxyBuilder::new(width as u32, height as u32).unwrap();
    assert!(builder.push_rows(&bad.view()).is_err());
    assert!(ColourProxyBuilder::new(63, 200).is_err());
}

/// The box mean of a full-resolution plane at `factor`, on the proxy grid.
fn box_mean(plane: &[f32], width: usize, factor: usize, i: usize, j: usize) -> f64 {
    let mut sum = 0.0;
    for y in j * factor..(j + 1) * factor {
        for x in i * factor..(i + 1) * factor {
            sum += f64::from(plane[y * width + x]);
        }
    }
    sum / (factor * factor) as f64
}

#[test]
fn merge_preview_fusion_of_an_aligned_bracket_matches_the_full_merge() {
    let (width, height) = (1100, 700);
    let offsets = [-2.0, 0.0, 2.0];
    let bracket = |x: usize, y: usize| scene(x, y, width, height);
    let frames: Vec<Frame> = offsets
        .iter()
        .map(|&offset| Frame::record(width, height, offset, &bracket))
        .collect();
    let colour: Vec<ColourProxy> = frames.iter().map(|frame| frame.proxies(16).1).collect();
    let exposures = exposures(&offsets, 1);
    let identity = RigidTransform::identity(width as u32, height as u32);
    let registration = registration(width, height, &[identity; 3]);
    let cancel = Cancel::never();
    let coverage = common_coverage(
        width as u32,
        height as u32,
        &registration.transforms(),
        &cancel,
    )
    .unwrap();
    let parameters = FusionParameters::default();
    let picture = fuse_preview(
        &colour,
        &exposures,
        &registration,
        &coverage,
        None,
        &parameters,
        &cancel,
    )
    .unwrap();
    let factor = 2;
    assert_eq!(picture.factor, factor as u32);
    assert_eq!(
        picture.rect,
        PixelRect {
            x: 0,
            y: 0,
            width: 550,
            height: 350
        }
    );
    assert!(picture.highlights_extended(), "{:?}", picture.diagnostics);
    assert!(picture.shadows_extended(), "{:?}", picture.diagnostics);
    assert_eq!(picture.diagnostics.pixels, 550 * 350);
    // The full merge of the same frames, reduced to the picture's grid.
    let pixels = width * height;
    let mut full: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0.0; pixels]);
    let views: Vec<FrameRect<'_>> = frames.iter().map(Frame::view).collect();
    let [red, green, blue] = &mut full;
    fuse_rect(
        &exposures,
        &parameters,
        &views,
        None,
        &mut FusedRect {
            width,
            height,
            stride: width,
            rgb: [red, green, blue],
            limits: None,
        },
    )
    .unwrap();
    let largest = full
        .iter()
        .flatten()
        .fold(0.0_f64, |most, &value| most.max(f64::from(value).abs()));
    let tolerance = 1e-5 * largest + 1e-9;
    for j in 0..picture.height {
        for i in 0..picture.width {
            let got = picture.at(i, j);
            for c in 0..3 {
                assert!(got[c].is_finite());
                let expected = box_mean(&full[c], width, factor, i, j);
                assert!(
                    (f64::from(got[c]) - expected).abs() <= tolerance,
                    "picture ({i}, {j}) channel {c}: {} against {expected}",
                    got[c]
                );
            }
        }
    }
    // The bright patch is recovered above the reference's white and the dark one is above zero.
    let bright = picture.at(width / 6 / factor, height / 5 / factor);
    assert!((f64::from(bright[0]) - 3.0).abs() < 1e-5);
    // A cancelled preview stops.
    let cancelled = Cancel::new();
    cancelled.cancel();
    let error = fuse_preview(
        &colour,
        &exposures,
        &registration,
        &coverage,
        None,
        &parameters,
        &cancelled,
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    // Mismatched inputs are refused.
    assert!(
        fuse_preview(
            &colour[..2],
            &exposures,
            &registration,
            &coverage,
            None,
            &parameters,
            &cancel,
        )
        .is_err()
    );
}

#[test]
fn merge_preview_proxy_transform_is_the_registration_on_the_proxy_grid() {
    for factor in [1_u32, 2, 3, 6] {
        let (width, height) = (6048_u32, 4024_u32);
        let full = RigidTransform::new(width, height, 2.0_f64.to_radians(), 13.5, -7.25);
        let proxy = proxy_transform(&full, factor);
        let f = f64::from(factor);
        let o = (f - 1.0) / 2.0;
        for (u, v) in [(0.0, 0.0), (17.0, 400.0), (1007.0, 670.0), (250.5, 3.25)] {
            let (pu, pv) = proxy.map(u, v);
            let (qx, qy) = full.map(f * u + o, f * v + o);
            assert!((f * pu + o - qx).abs() < 1e-9 && (f * pv + o - qy).abs() < 1e-9);
        }
        assert!(proxy_transform(&RigidTransform::identity(width, height), factor).is_identity());
    }
}

#[test]
fn merge_preview_of_a_moved_bracket_is_finite_inside_its_coverage() {
    let (width, height) = (1100, 700);
    let offsets = [-2.0, 0.0, 2.0];
    let bracket = |x: usize, y: usize| scene(x, y, width, height);
    let frames: Vec<Frame> = offsets
        .iter()
        .map(|&offset| Frame::record(width, height, offset, &bracket))
        .collect();
    let colour: Vec<ColourProxy> = frames.iter().map(|frame| frame.proxies(16).1).collect();
    let (w, h) = (width as u32, height as u32);
    let transforms = [
        RigidTransform::new(w, h, 1.0_f64.to_radians(), 6.0, -4.5),
        RigidTransform::identity(w, h),
        RigidTransform::new(w, h, -0.5_f64.to_radians(), -9.25, 3.0),
    ];
    let registration = registration(width, height, &transforms);
    let cancel = Cancel::never();
    let coverage = common_coverage(w, h, &transforms, &cancel).unwrap();
    let picture = fuse_preview(
        &colour,
        &exposures(&offsets, 1),
        &registration,
        &coverage,
        None,
        &FusionParameters::default(),
        &cancel,
    )
    .unwrap();
    // The picture lies inside the coverage, at most a pixel or two in from it on each side.
    let rect = picture.rect;
    let centre = |u: u32| 2.0 * f64::from(u) + 0.5;
    let cover = coverage.rect;
    assert!(centre(rect.x) >= f64::from(cover.x));
    assert!(centre(rect.y) >= f64::from(cover.y));
    assert!(centre(rect.x + rect.width - 1) <= f64::from(cover.x + cover.width - 1));
    assert!(centre(rect.y + rect.height - 1) <= f64::from(cover.y + cover.height - 1));
    assert!(u64::from(rect.width) * 2 + 8 >= u64::from(cover.width));
    assert!(u64::from(rect.height) * 2 + 8 >= u64::from(cover.height));
    assert!(picture.rgb.iter().flatten().all(|value| value.is_finite()));
    assert_eq!(max_residual_px(&registration), Some(0.09));
}

/// Whether `point` lies inside the closed polygon `outline` (even-odd).
fn encloses(outline: &[[f64; 2]], point: [f64; 2]) -> bool {
    let mut inside = false;
    for k in 0..outline.len() {
        let (a, b) = (outline[k], outline[(k + 1) % outline.len()]);
        if (a[1] > point[1]) != (b[1] > point[1]) {
            let x = a[0] + (point[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if point[0] < x {
                inside = !inside;
            }
        }
    }
    inside
}

/// The distance from `point` to the polygon's boundary.
fn boundary_distance(outline: &[[f64; 2]], point: [f64; 2]) -> f64 {
    (0..outline.len())
        .map(|k| {
            let (a, b) = (outline[k], outline[(k + 1) % outline.len()]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let length = dx * dx + dy * dy;
            let t = if length == 0.0 {
                0.0
            } else {
                (((point[0] - a[0]) * dx + (point[1] - a[1]) * dy) / length).clamp(0.0, 1.0)
            };
            (point[0] - a[0] - t * dx).hypot(point[1] - a[1] - t * dy)
        })
        .fold(f64::INFINITY, f64::min)
}

/// A shape from rows of `#` and `.`.
fn shape(rows: &[&str]) -> (Vec<bool>, usize, usize) {
    let (w, h) = (rows[0].len(), rows.len());
    let cells = rows
        .iter()
        .flat_map(|row| row.bytes().map(|cell| cell == b'#'))
        .collect();
    (cells, w, h)
}

#[test]
fn merge_preview_overlay_outlines_enclose_their_regions() {
    let comb: Vec<String> = (0..6)
        .map(|y| {
            (0..400)
                .map(|x| if y < 2 || x % 2 == 0 { '#' } else { '.' })
                .collect()
        })
        .collect();
    let comb: Vec<&str> = comb.iter().map(String::as_str).collect();
    let shapes: Vec<(&str, Vec<&str>, bool)> = vec![
        ("one pixel", vec!["...", ".#.", "..."], false),
        ("an L", vec!["#...", "#...", "####"], false),
        ("a diagonal", vec!["#...", ".#..", "..#.", "...#"], false),
        (
            "touching corners",
            vec!["##..", "##..", "..##", "..##"],
            false,
        ),
        ("a ring", vec!["#####", "#...#", "#...#", "#####"], true),
        (
            "a blob",
            vec!["..##..", ".####.", "######", ".#..#.", "##..##"],
            false,
        ),
        ("a comb", comb, false),
    ];
    for (name, rows, holes) in shapes {
        let (cells, w, h) = shape(&rows);
        let inside = |x: i64, y: i64| {
            x >= 0
                && y >= 0
                && (x as usize) < w
                && (y as usize) < h
                && cells[y as usize * w + x as usize]
        };
        let first = cells.iter().position(|&cell| cell).unwrap();
        let contour = trace(&inside, (first % w) as i64, (first / w) as i64, w, h).unwrap();
        let exact: Vec<[f64; 2]> = contour.iter().map(|&(x, y)| [x as f64, y as f64]).collect();
        let outline = simplify(&contour, OUTLINE_TOLERANCE, preview::MAX_OUTLINE_VERTICES);
        assert!(outline.len() >= 3, "{name}");
        // Where the vertex cap doubled the tolerance (the comb's 400 teeth), a tooth four rows
        // high may be cut away; elsewhere every pixel is held within the tolerance.
        let reach = if name == "a comb" {
            4.0 + OUTLINE_TOLERANCE
        } else {
            OUTLINE_TOLERANCE
        };
        if name == "one pixel" {
            assert_eq!(
                outline,
                vec![[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]]
            );
        }
        assert!(
            outline.len() <= preview::MAX_OUTLINE_VERTICES,
            "{name}: {}",
            outline.len()
        );
        for y in 0..h {
            for x in 0..w {
                let centre = [x as f64 + 0.5, y as f64 + 0.5];
                if cells[y * w + x] {
                    // The traced boundary encloses every pixel; the simplified one each within its
                    // tolerance.
                    assert!(encloses(&exact, centre), "{name}: ({x}, {y})");
                    assert!(
                        encloses(&outline, centre) || boundary_distance(&outline, centre) <= reach,
                        "{name}: ({x}, {y})"
                    );
                } else if !holes {
                    assert!(!encloses(&exact, centre), "{name}: ({x}, {y}) is outside");
                }
            }
        }
    }
}

#[test]
fn merge_preview_overlays_name_each_moving_region() {
    let (width, height) = (1100, 700);
    let offsets = [-2.0, 0.0, 2.0];
    // A dark subject in one place in the first two exposures and in another in the third.
    let subject = |x: usize, y: usize, at: (usize, usize)| {
        (at.0..at.0 + 80).contains(&x) && (at.1..at.1 + 60).contains(&y)
    };
    let frames: Vec<Frame> = offsets
        .iter()
        .enumerate()
        .map(|(input, &offset)| {
            let at = if input == 2 { (700, 420) } else { (300, 380) };
            let moved = move |x: usize, y: usize| {
                let ramp = 0.05 + 0.15 * x as f64 / width as f64 + 0.05 * y as f64 / height as f64;
                let texture = 0.02 * ((x as f64 * 0.3).sin() + (y as f64 * 0.23).cos());
                let luma = if subject(x, y, at) {
                    0.03
                } else {
                    ramp + texture
                };
                [luma, 0.9 * luma, 0.7 * luma]
            };
            Frame::record(width, height, offset, &moved)
        })
        .collect();
    let (analysis, colour): (Vec<AnalysisProxy>, Vec<ColourProxy>) =
        frames.iter().map(|frame| frame.proxies(16)).unzip();
    let exposures = exposures(&offsets, 1);
    let identity = RigidTransform::identity(width as u32, height as u32);
    let registration = registration(width, height, &[identity; 3]);
    let cancel = Cancel::never();
    let aligned = AlignedProxies::new(&registration, &analysis, &cancel).unwrap();
    let motion = detect_motion(&aligned, &exposures, DeghostMode::Auto, &cancel)
        .unwrap()
        .expect("deghosting on");
    assert!(!motion.is_empty());
    let coverage = common_coverage(
        width as u32,
        height as u32,
        &registration.transforms(),
        &cancel,
    )
    .unwrap();
    let picture = fuse_preview(
        &colour,
        &exposures,
        &registration,
        &coverage,
        Some(&motion),
        &FusionParameters::default(),
        &cancel,
    )
    .unwrap();
    assert!(picture.diagnostics.motion > 0);
    assert!(picture.rgb.iter().flatten().all(|value| value.is_finite()));
    let overlays = motion_overlays(&motion, &exposures, picture.rect, &cancel).unwrap();
    assert!(!overlays.is_empty());
    let (w, h) = (picture.width, picture.height);
    let rect = picture.rect;
    for y in 0..h {
        for x in 0..w {
            let label = motion.label(rect.x as usize + x, rect.y as usize + y);
            if label == 0 {
                continue;
            }
            let region = &motion.regions()[label as usize - 1];
            let centre = [x as f64 + 0.5, y as f64 + 0.5];
            let holder = overlays.iter().find(|overlay| {
                let b = overlay.bounds;
                (b.x as usize..(b.x + b.width) as usize).contains(&x)
                    && (b.y as usize..(b.y + b.height) as usize).contains(&y)
                    && (encloses(&overlay.outline, centre)
                        || boundary_distance(&overlay.outline, centre) <= OUTLINE_TOLERANCE)
            });
            let holder = holder.unwrap_or_else(|| panic!("no outline holds ({x}, {y})"));
            assert_eq!(holder.input, region.selected);
            assert_eq!(holder.fallback, region.fallback);
            assert_eq!(holder.offset_ev, offsets[region.selected]);
        }
    }
    for overlay in &overlays {
        assert!(overlay.outline.len() >= 4);
        assert!(overlay.outline.len() <= preview::MAX_OUTLINE_VERTICES);
        let b = overlay.bounds;
        for point in &overlay.outline {
            assert!(point[0] >= f64::from(b.x) && point[0] <= f64::from(b.x + b.width));
            assert!(point[1] >= f64::from(b.y) && point[1] <= f64::from(b.y + b.height));
        }
    }
    // A picture outside the map's grid is refused.
    let outside = PixelRect {
        x: 1,
        ..picture.rect
    };
    assert!(motion_overlays(&motion, &exposures, outside, &cancel).is_err());
}

#[test]
fn merge_preview_display_transform_is_monotone_and_maps_zero_to_zero() {
    assert_eq!(display_pixel([0.0; 3]), [0, 0, 0]);
    assert_eq!(display_pixel([-0.5, -1.0, -0.01]), [0, 0, 0]);
    let mut previous = 0_u8;
    let mut grey = 0.0_f32;
    while grey <= 64.0 {
        let code = display_pixel([grey; 3]);
        assert!(code[0] == code[1] && code[1] == code[2]);
        assert!(code[0] >= previous, "{grey}");
        previous = code[0];
        grey += 1.0 / 512.0;
    }
    // Below the shoulder it is the plain sRGB encoding.
    let quantizer = crate::colour::srgb::quantizer();
    for value in [0.001_f32, 0.18, 0.5, 0.69] {
        assert_eq!(
            display_pixel([value; 3])[0],
            quantizer.channel(f64::from(value))
        );
    }
    // Extended highlights stay distinguishable rather than clipping at one.
    let codes: Vec<u8> = [1.0_f32, 2.0, 4.0]
        .map(|v| display_pixel([v; 3])[0])
        .to_vec();
    assert!(
        codes[0] < codes[1] && codes[1] < codes[2] && codes[2] < 255,
        "{codes:?}"
    );
    // Hue is kept: the channels' order survives the shoulder.
    let [r, g, b] = display_pixel([4.0, 2.0, 1.0]);
    assert!(r > g && g > b && r < 255);
}

#[test]
fn merge_preview_jpeg_encodes_the_display_picture() {
    let (width, height) = (40, 24);
    let mut rgb: [Vec<f32>; 3] = std::array::from_fn(|_| Vec::new());
    for y in 0..height {
        for x in 0..width {
            // Smooth in the display domain, from the shadows into the shoulder.
            let value = 0.1 + (x + y) as f32 / 40.0;
            rgb[0].push(value);
            rgb[1].push(0.5 * value);
            rgb[2].push(0.25);
        }
    }
    let picture = PreviewPicture {
        rect: PixelRect {
            x: 0,
            y: 0,
            width: width as u32,
            height: height as u32,
        },
        factor: 1,
        width,
        height,
        rgb,
        diagnostics: FusionDiagnostics::default(),
    };
    let bytes = encode_preview_jpeg(&picture, &Cancel::never()).unwrap();
    let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
    assert_eq!(decoded.dimensions(), (width as u32, height as u32));
    let mut error = 0_u64;
    for (x, y, pixel) in decoded.enumerate_pixels() {
        let expected = display_pixel(picture.at(x as usize, y as usize));
        for c in 0..3 {
            let difference = pixel.0[c].abs_diff(expected[c]);
            error += u64::from(difference);
            assert!(
                difference <= 12,
                "({x}, {y}): {:?} against {expected:?}",
                pixel.0
            );
        }
    }
    // On average within a code or two a channel.
    assert!(error as f64 / (width * height * 3) as f64 <= 2.0, "{error}");
    let cancelled = Cancel::new();
    cancelled.cancel();
    assert!(encode_preview_jpeg(&picture, &cancelled).is_err());
}

#[test]
fn merge_preview_helpers_name_the_proxy_side_and_residual() {
    assert_eq!(preview_proxy_side(6048, 4024), 1008);
    assert_eq!(preview_proxy_side(4024, 6048), 1008);
    assert_eq!(preview_proxy_side(800, 600), 800);
    let identity = RigidTransform::identity(6048, 4024);
    let mut fitted = registration(6048, 4024, &[identity; 3]);
    assert_eq!(max_residual_px(&fitted), Some(0.09));
    for input in &mut fitted.inputs {
        if !matches!(input.evidence, AlignmentEvidence::Reference) {
            input.evidence = AlignmentEvidence::Framing {
                shift_px: 0.0,
                correlation: None,
                in_place: None,
            };
        }
    }
    assert_eq!(max_residual_px(&fitted), None);
    // The picture covers the proxy pixels whose centres lie in the coverage.
    let coverage = Coverage {
        frame_width: 6048,
        frame_height: 4024,
        rect: PixelRect {
            x: 10,
            y: 4,
            width: 6000,
            height: 4000,
        },
        pixels_lost: 0,
    };
    let rect = preview::preview_rect(&coverage, 6, 1008, 670, &[identity; 3]).unwrap();
    // Centres 6·u + 2.5: columns from u = 2 (at or after 10) to 1001 (at or before 6009), rows
    // from 1 (at or after 4) to 666 (at or before 4003).
    assert_eq!(
        rect,
        PixelRect {
            x: 2,
            y: 1,
            width: 1000,
            height: 666
        }
    );
    assert_eq!(PreviewPicture::bytes_for(6048, 4024), 1008 * 670 * 12);
    assert_eq!(
        preview_scratch_bytes(9, 1024),
        9 * 64 * 1024 * 14 + 64 * 1024 * 5
    );
}
