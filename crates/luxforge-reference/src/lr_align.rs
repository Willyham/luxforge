//! Measures and fits for the Lightroom alignment rounds (`docs/design/lightroom-alignment.md`,
//! "Measuring a response, not a look" and "Fitting the translation").
//!
//! Every comparison is of **responses**: the change one setting makes relative to the same
//! editor's rendering of the same picture at the setting's neutral value. Another editor's response
//! is carried onto this editor's neutral rendering in Oklab ([`carried`]), so the two base
//! renderings cancel to first order, and two responses are compared by CIEDE2000 between the
//! colours they reach from that one neutral, which is what the alignment programme's threshold is
//! stated in.
//!
//! The measures read flat patches (transfer curves, linear gain, Oklab chroma ratio and hue shift
//! per hue sector, locality across surrounds), edges and the zone plate (overshoot, halo width,
//! rise and the blur it implies, band gain per spatial frequency, noise deviation), positions
//! (radial and axial profiles and their crossings) and geometry (dot centroids and their matching).
//! The fit ([`fit`], [`fit_by`]) is the level A translation: for each Lightroom value the Luxforge
//! value whose response is closest, then made monotone in the direction that fits better, with its
//! residual, range shortfall and a frozen table of knots.
//!
//! Pure functions of their inputs, with no rendering and no file access: the xtask `lr-align
//! measure` command renders, decodes and writes, and calls these. Each is checked on synthetic
//! cases with known answers in the `lr_align` study.

use super::colour::{self, Oklab};
use super::preview_error::{ciede2000, lab_from_linear};

/// A CIELAB colour `[L*, a*, b*]` under D65.
pub type Lab = [f64; 3];

/// CIELAB of a linear-light sRGB colour.
pub fn lab(rgb: [f64; 3]) -> Lab {
    lab_from_linear(rgb)
}

/// CIE relative luminance `Y` of a linear-light sRGB colour.
pub fn luminance(rgb: [f64; 3]) -> f64 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// The colour `neutral` reaches under another editor's response, the change from `reference` (its
/// neutral rendering) to `adjusted`, applied in Oklab. Linear sRGB in and out; the result may lie
/// outside `[0, 1]` and is kept so, as a difference is still well defined there.
pub fn carried(neutral: [f64; 3], reference: [f64; 3], adjusted: [f64; 3]) -> [f64; 3] {
    let (n, r, a) = (
        colour::to_oklab(neutral),
        colour::to_oklab(reference),
        colour::to_oklab(adjusted),
    );
    colour::from_oklab(Oklab {
        l: n.l + a.l - r.l,
        a: n.a + a.a - r.a,
        b: n.b + a.b - r.b,
    })
}

/// The `q` quantile of `values` by nearest rank, `q` in `(0, 1]`: the `ceil(q n)`th smallest, the
/// definition every figure in the workspace reads. `NaN` for none.
pub fn quantile(values: &[f64], q: f64) -> f64 {
    quantile_in_place(&mut values.to_vec(), q)
}

fn quantile_in_place(values: &mut [f64], q: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let rank = ((q * values.len() as f64).ceil() as usize).clamp(1, values.len()) - 1;
    *values.select_nth_unstable_by(rank, f64::total_cmp).1
}

/// The median by nearest rank.
pub fn median(values: &[f64]) -> f64 {
    quantile(values, 0.5)
}

/// The median, 95th percentile and largest of a set of differences.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spread {
    pub median: f64,
    pub p95: f64,
    pub max: f64,
}

pub fn spread(values: &[f64]) -> Spread {
    Spread {
        median: quantile(values, 0.5),
        p95: quantile(values, 0.95),
        max: values.iter().copied().fold(f64::NAN, f64::max),
    }
}

// -------------------------------------------------------------------------------------------
// Pointwise measures
// -------------------------------------------------------------------------------------------

/// A transfer curve: each sample's neutral CIELAB `L*` against its adjusted `L*`, sorted by the
/// neutral one.
pub fn transfer(neutral: &[[f64; 3]], adjusted: &[[f64; 3]]) -> Vec<[f64; 2]> {
    let mut points: Vec<[f64; 2]> = neutral
        .iter()
        .zip(adjusted)
        .map(|(n, a)| [lab(*n)[0], lab(*a)[0]])
        .collect();
    points.sort_by(|a, b| a[0].total_cmp(&b[0]));
    points
}

/// The gain a change applies to linear luminance: the median of adjusted over neutral `Y` across
/// the samples whose two values both lie strictly inside `(low, high)`, so clipped samples are not
/// read. `None` when no sample qualifies.
pub fn linear_gain(
    neutral: &[[f64; 3]],
    adjusted: &[[f64; 3]],
    low: f64,
    high: f64,
) -> Option<f64> {
    let ratios: Vec<f64> = neutral
        .iter()
        .zip(adjusted)
        .map(|(n, a)| (luminance(*n), luminance(*a)))
        .filter(|(n, a)| *n > low && *n < high && *a > low && *a < high)
        .map(|(n, a)| a / n)
        .collect();
    (!ratios.is_empty()).then(|| median(&ratios))
}

/// The signed circular difference `to − from` in degrees, in `(−180, 180]`.
pub fn circular_difference(from: f64, to: f64) -> f64 {
    let difference = (to - from).rem_euclid(360.0);
    if difference > 180.0 {
        difference - 360.0
    } else {
        difference
    }
}

/// One hue sector of a colour response, in Oklab: how many samples fell in it and the medians of
/// their chroma ratio, hue shift (degrees) and lightness shift.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sector {
    /// The sector's centre, degrees.
    pub hue: f64,
    pub samples: usize,
    pub chroma_ratio: f64,
    pub hue_shift: f64,
    pub lightness_shift: f64,
}

/// The response per hue sector: samples whose neutral Oklab chroma is at least `min_chroma`,
/// binned by neutral hue into `count` sectors, sector `k` centred on `k · 360 / count` degrees.
/// Empty sectors are left out.
pub fn sectors(
    neutral: &[[f64; 3]],
    adjusted: &[[f64; 3]],
    count: usize,
    min_chroma: f64,
) -> Vec<Sector> {
    let width = 360.0 / count as f64;
    let mut bins: Vec<Vec<(f64, f64, f64)>> = vec![Vec::new(); count];
    for (n, a) in neutral.iter().zip(adjusted) {
        let (n, a) = (colour::to_oklab(*n), colour::to_oklab(*a));
        let chroma = colour::chroma(n);
        if chroma < min_chroma {
            continue;
        }
        let hue = colour::hue_degrees(n).rem_euclid(360.0);
        let bin = (((hue + width / 2.0) / width) as usize) % count;
        let shift = circular_difference(hue, colour::hue_degrees(a).rem_euclid(360.0));
        bins[bin].push((colour::chroma(a) / chroma, shift, a.l - n.l));
    }
    bins.iter()
        .enumerate()
        .filter(|(_, bin)| !bin.is_empty())
        .map(|(index, bin)| {
            let column = |pick: fn(&(f64, f64, f64)) -> f64| -> f64 {
                median(&bin.iter().map(pick).collect::<Vec<_>>())
            };
            Sector {
                hue: index as f64 * width,
                samples: bin.len(),
                chroma_ratio: column(|s| s.0),
                hue_shift: column(|s| s.1),
                lightness_shift: column(|s| s.2),
            }
        })
        .collect()
}

/// How far a response depends on a sample's surroundings: for each group of samples that share an
/// input but sit on different surrounds, each `(neutral, adjusted)`, the largest CIEDE2000 between
/// two members' responses carried onto the group's first neutral; the median over the groups. A
/// pointwise change measures 0.
pub fn locality(groups: &[Vec<([f64; 3], [f64; 3])>]) -> f64 {
    let spreads: Vec<f64> = groups
        .iter()
        .filter(|group| group.len() > 1)
        .map(|group| {
            let base = group[0].0;
            let reached: Vec<Lab> = group
                .iter()
                .map(|(n, a)| lab(carried(base, *n, *a)))
                .collect();
            let mut largest: f64 = 0.0;
            for (i, first) in reached.iter().enumerate() {
                for second in &reached[i + 1..] {
                    largest = largest.max(ciede2000(*first, *second));
                }
            }
            largest
        })
        .collect();
    if spreads.is_empty() {
        0.0
    } else {
        median(&spreads)
    }
}

// -------------------------------------------------------------------------------------------
// Spatial measures
// -------------------------------------------------------------------------------------------

/// An edge's measures, from a profile across it: overshoot above the light plateau and undershoot
/// below the dark one as fractions of the step, the halo's width (the farthest distance from the
/// edge at which either still holds 10% of its peak, pixels), the 10–90% rise (pixels), and the
/// blur `σ` (pixels) the profile's line spread implies.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edge {
    pub overshoot: f64,
    pub undershoot: f64,
    pub halo: f64,
    pub rise: f64,
    pub spread: f64,
}

/// The share of a profile at each end averaged for its plateau.
const PLATEAU: f64 = 0.15;

/// A halo's own floor: an overshoot below this share of the step is no halo.
const HALO_FLOOR: f64 = 0.005;

/// [`Edge`] of `profile`, sample `i` at position `i + 0.5`, rising from dark to light through the
/// edge at position `at`; a falling profile is read reversed. Plateaus are the means of the
/// outermost 15% at each end.
pub fn edge(profile: &[f64], at: f64) -> Edge {
    let n = profile.len();
    let end = ((n as f64 * PLATEAU).ceil() as usize).max(1);
    if n < 4 || end * 2 > n {
        return Edge::default();
    }
    let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
    let (mut dark, mut light) = (mean(&profile[..end]), mean(&profile[n - end..]));
    let (samples, at): (Vec<f64>, f64) = if light >= dark {
        (profile.to_vec(), at)
    } else {
        std::mem::swap(&mut dark, &mut light);
        (profile.iter().rev().copied().collect(), n as f64 - at)
    };
    let step = light - dark;
    if step <= 1e-12 {
        return Edge::default();
    }
    let position = |i: usize| i as f64 + 0.5;
    let (mut over, mut under) = (0.0f64, 0.0f64);
    for (i, value) in samples.iter().enumerate() {
        if position(i) > at {
            over = over.max(value - light);
        } else {
            under = under.max(dark - value);
        }
    }
    let mut halo: f64 = 0.0;
    for (peak, light_side) in [(over, true), (under, false)] {
        if peak < HALO_FLOOR * step {
            continue;
        }
        for (i, value) in samples.iter().enumerate() {
            let x = position(i);
            let excess = if light_side {
                if x <= at {
                    continue;
                }
                value - light
            } else {
                if x > at {
                    continue;
                }
                dark - value
            };
            if excess >= 0.1 * peak {
                halo = halo.max((x - at).abs());
            }
        }
    }
    let crossing = |level: f64| -> Option<f64> {
        // The crossing nearest the edge.
        samples
            .windows(2)
            .enumerate()
            .filter(|(_, pair)| (pair[0] - level) * (pair[1] - level) <= 0.0 && pair[0] != pair[1])
            .map(|(i, pair)| position(i) + (level - pair[0]) / (pair[1] - pair[0]))
            .min_by(|a, b| (a - at).abs().total_cmp(&(b - at).abs()))
    };
    let rise = match (crossing(dark + 0.1 * step), crossing(dark + 0.9 * step)) {
        (Some(low), Some(high)) => (high - low).abs(),
        _ => 0.0,
    };
    // The line spread: the profile's differences, whose variance is the blur's plus a
    // twelfth of a pixel for the one-pixel sampling; a sharpened edge's negative lobes shrink it.
    let lines: Vec<(f64, f64)> = samples
        .windows(2)
        .enumerate()
        .map(|(i, pair)| (i as f64 + 1.0, (pair[1] - pair[0]) / step))
        .collect();
    let mass: f64 = lines.iter().map(|(_, w)| w).sum();
    let centre = lines.iter().map(|(x, w)| x * w).sum::<f64>() / mass;
    let variance = lines
        .iter()
        .map(|(x, w)| w * (x - centre).powi(2))
        .sum::<f64>()
        / mass;
    Edge {
        overshoot: over / step,
        undershoot: under / step,
        halo,
        rise,
        spread: (variance - 1.0 / 12.0).max(0.0).sqrt(),
    }
}

/// One spatial-frequency band of a zone plate and its gain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    /// Cycles per pixel.
    pub low: f64,
    pub high: f64,
    pub gain: f64,
}

/// The RMS of a zone plate's variation about its band mean, per band: the plate's phase is
/// `π r² / (2R)` about `centre`, so its local frequency is `r / (2R)` cycles per pixel, reaching
/// Nyquist at `radius`, split into `bands` equal bands from 0 to 0.5. The plane is row-major,
/// `width` wide. A band holding no pixel is `NaN`.
pub fn band_rms(
    plane: &[f64],
    width: usize,
    centre: (f64, f64),
    radius: f64,
    bands: usize,
) -> Vec<f64> {
    let height = plane.len() / width.max(1);
    let mut sums = vec![[0.0f64; 3]; bands];
    let x0 = (centre.0 - radius).floor().max(0.0) as usize;
    let x1 = ((centre.0 + radius).ceil().max(0.0) as usize).min(width);
    let y0 = (centre.1 - radius).floor().max(0.0) as usize;
    let y1 = ((centre.1 + radius).ceil().max(0.0) as usize).min(height);
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = (x as f64 + 0.5 - centre.0, y as f64 + 0.5 - centre.1);
            let r = dx.hypot(dy);
            if r >= radius {
                continue;
            }
            let band = ((r / (2.0 * radius) / 0.5 * bands as f64) as usize).min(bands - 1);
            let value = plane[y * width + x];
            let s = &mut sums[band];
            s[0] += 1.0;
            s[1] += value;
            s[2] += value * value;
        }
    }
    sums.iter()
        .map(|s| {
            if s[0] > 0.0 {
                (s[2] / s[0] - (s[1] / s[0]).powi(2)).max(0.0).sqrt()
            } else {
                f64::NAN
            }
        })
        .collect()
}

/// The gain per band on a zone plate: [`band_rms`] of the adjusted plane over the neutral's.
pub fn band_gains(
    neutral: &[f64],
    adjusted: &[f64],
    width: usize,
    centre: (f64, f64),
    radius: f64,
    bands: usize,
) -> Vec<Band> {
    gains_of(
        &band_rms(neutral, width, centre, radius, bands),
        &band_rms(adjusted, width, centre, radius, bands),
    )
}

/// Bands from two [`band_rms`] results, the adjusted over the neutral.
pub fn gains_of(neutral: &[f64], adjusted: &[f64]) -> Vec<Band> {
    let bands = neutral.len();
    neutral
        .iter()
        .zip(adjusted)
        .enumerate()
        .map(|(index, (n, a))| Band {
            low: index as f64 * 0.5 / bands as f64,
            high: (index + 1) as f64 * 0.5 / bands as f64,
            gain: if *n > 1e-12 { a / n } else { f64::NAN },
        })
        .collect()
}

/// The Gaussian blur `σ` (pixels) band gains imply, `gain(f) = exp(−2π² σ² f²)` at each band's
/// centre frequency, by least squares on the log gains of the bands between 0.05 and 0.98. `None`
/// when no band qualifies.
pub fn blur_from_gains(bands: &[Band]) -> Option<f64> {
    let (mut numerator, mut denominator) = (0.0, 0.0);
    for band in bands {
        if !(band.gain > 0.05 && band.gain < 0.98) {
            continue;
        }
        let f = (band.low + band.high) / 2.0;
        let x = 2.0 * std::f64::consts::PI.powi(2) * f * f;
        numerator += x * -band.gain.ln();
        denominator += x * x;
    }
    (denominator > 0.0).then(|| (numerator / denominator).sqrt())
}

/// The standard deviation of `values` (population form). `NaN` for none.
pub fn deviation(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

// -------------------------------------------------------------------------------------------
// Positional measures
// -------------------------------------------------------------------------------------------

/// A radial profile of a row-major plane: each pixel's normalized elliptical radius
/// `ρ = sqrt((u / ax)² + (v / ay)²)` from `centre`, `(u, v)` its offset turned by `−angle` degrees
/// so the ellipse's axes, `axes` (pixels), lie turned `angle` degrees clockwise on screen (y
/// down); binned into `bins` equal bins over `[0, rho_max]`, each bin's mean value at its centre.
/// Empty bins are left out.
pub fn radial_profile(
    plane: &[f64],
    width: usize,
    centre: (f64, f64),
    axes: (f64, f64),
    angle: f64,
    bins: usize,
    rho_max: f64,
) -> Vec<[f64; 2]> {
    let (sin, cos) = angle.to_radians().sin_cos();
    let mut sums = vec![(0.0f64, 0usize); bins];
    for (index, value) in plane.iter().enumerate() {
        let (x, y) = ((index % width) as f64 + 0.5, (index / width) as f64 + 0.5);
        let (dx, dy) = (x - centre.0, y - centre.1);
        let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
        let rho = (u / axes.0).hypot(v / axes.1);
        if rho >= rho_max {
            continue;
        }
        let bin = ((rho / rho_max * bins as f64) as usize).min(bins - 1);
        sums[bin].0 += value;
        sums[bin].1 += 1;
    }
    sums.iter()
        .enumerate()
        .filter(|(_, (_, count))| *count > 0)
        .map(|(bin, (sum, count))| {
            [
                (bin as f64 + 0.5) * rho_max / bins as f64,
                sum / *count as f64,
            ]
        })
        .collect()
}

/// The orientation of a weighted region's major axis, degrees clockwise on screen (y down) in
/// `(−90, 90]`, from its second moments: each pixel of the row-major plane weighted by its value's
/// magnitude. `None` for no weight.
pub fn orientation(plane: &[f64], width: usize) -> Option<f64> {
    let (mut w, mut sx, mut sy) = (0.0, 0.0, 0.0);
    for (index, value) in plane.iter().enumerate() {
        let m = value.abs();
        w += m;
        sx += m * ((index % width) as f64 + 0.5);
        sy += m * ((index / width) as f64 + 0.5);
    }
    if w <= 0.0 {
        return None;
    }
    let (cx, cy) = (sx / w, sy / w);
    let (mut xx, mut yy, mut xy) = (0.0, 0.0, 0.0);
    for (index, value) in plane.iter().enumerate() {
        let m = value.abs();
        let (dx, dy) = (
            (index % width) as f64 + 0.5 - cx,
            (index / width) as f64 + 0.5 - cy,
        );
        xx += m * dx * dx;
        yy += m * dy * dy;
        xy += m * dx * dy;
    }
    let angle = 0.5 * (2.0 * xy).atan2(xx - yy);
    let degrees = angle.to_degrees();
    Some(if degrees <= -90.0 {
        degrees + 180.0
    } else {
        degrees
    })
}

/// A profile along the axis from `from` to `to` (pixels): each pixel projected onto it as
/// `t = (p − from) · (to − from) / |to − from|²` (0 at `from`, 1 at `to`), binned into `bins` equal
/// bins over `t_range`, each bin's mean at its centre. Empty bins are left out.
pub fn axis_profile(
    plane: &[f64],
    width: usize,
    from: (f64, f64),
    to: (f64, f64),
    bins: usize,
    t_range: (f64, f64),
) -> Vec<[f64; 2]> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length2 = dx * dx + dy * dy;
    let span = t_range.1 - t_range.0;
    let mut sums = vec![(0.0f64, 0usize); bins];
    for (index, value) in plane.iter().enumerate() {
        let (x, y) = ((index % width) as f64 + 0.5, (index / width) as f64 + 0.5);
        let t = ((x - from.0) * dx + (y - from.1) * dy) / length2;
        if t < t_range.0 || t >= t_range.1 {
            continue;
        }
        let bin = (((t - t_range.0) / span * bins as f64) as usize).min(bins - 1);
        sums[bin].0 += value;
        sums[bin].1 += 1;
    }
    sums.iter()
        .enumerate()
        .filter(|(_, (_, count))| *count > 0)
        .map(|(bin, (sum, count))| {
            [
                t_range.0 + (bin as f64 + 0.5) * span / bins as f64,
                sum / *count as f64,
            ]
        })
        .collect()
}

/// Where a profile first crosses `level`, walking from its first point, by linear interpolation;
/// `None` when it never does.
pub fn crossing(profile: &[[f64; 2]], level: f64) -> Option<f64> {
    profile.windows(2).find_map(|pair| {
        let ([x0, y0], [x1, y1]) = (pair[0], pair[1]);
        ((y0 - level) * (y1 - level) <= 0.0 && y0 != y1)
            .then(|| x0 + (level - y0) / (y1 - y0) * (x1 - x0))
    })
}

// -------------------------------------------------------------------------------------------
// Geometric measures
// -------------------------------------------------------------------------------------------

/// The centroids of the dark dots on a row-major plane: 8-connected regions of pixels below
/// `threshold` with at least `min_area` pixels, each pixel weighted by how far below the threshold
/// it lies, in pixel-edge coordinates (a pixel's centre is at `i + 0.5`).
pub fn dots(plane: &[f64], width: usize, threshold: f64, min_area: usize) -> Vec<[f64; 2]> {
    let height = plane.len() / width.max(1);
    let mut seen = vec![false; plane.len()];
    let mut centres = Vec::new();
    let mut stack = Vec::new();
    for start in 0..plane.len() {
        if seen[start] || plane[start] >= threshold {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut weight, mut sx, mut sy, mut area) = (0.0, 0.0, 0.0, 0usize);
        while let Some(index) = stack.pop() {
            let (x, y) = (index % width, index / width);
            let w = threshold - plane[index];
            weight += w;
            sx += w * (x as f64 + 0.5);
            sy += w * (y as f64 + 0.5);
            area += 1;
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= width as i64 || ny >= height as i64 {
                        continue;
                    }
                    let next = ny as usize * width + nx as usize;
                    if !seen[next] && plane[next] < threshold {
                        seen[next] = true;
                        stack.push(next);
                    }
                }
            }
        }
        if area >= min_area && weight > 0.0 {
            centres.push([sx / weight, sy / weight]);
        }
    }
    centres
}

/// For each point of `from`, the distance to its nearest point of `to`: the median of those within
/// `limit`, and how many lie beyond it (unmatched). `(NaN, from.len())` when none matches.
pub fn match_points(from: &[[f64; 2]], to: &[[f64; 2]], limit: f64) -> (f64, usize) {
    let distances: Vec<f64> = from
        .iter()
        .map(|p| {
            to.iter()
                .map(|q| (p[0] - q[0]).hypot(p[1] - q[1]))
                .fold(f64::INFINITY, f64::min)
        })
        .collect();
    let matched: Vec<f64> = distances.iter().copied().filter(|d| *d <= limit).collect();
    (median(&matched), distances.len() - matched.len())
}

/// The rotation of a grid of dots, in degrees, positive clockwise on screen (y down): the median
/// direction from each dot to its nearest neighbour lying within 30° of horizontal.
pub fn grid_angle(dots: &[[f64; 2]]) -> Option<f64> {
    let angles: Vec<f64> = dots
        .iter()
        .filter_map(|p| {
            dots.iter()
                .filter(|q| *q != p && q[0] > p[0])
                .map(|q| (q[0] - p[0], q[1] - p[1]))
                .filter(|(dx, dy)| dy.abs() <= dx * 30f64.to_radians().tan())
                .min_by(|a, b| a.0.hypot(a.1).total_cmp(&b.0.hypot(b.1)))
                .map(|(dx, dy)| dy.atan2(dx).to_degrees())
        })
        .collect();
    (!angles.is_empty()).then(|| median(&angles))
}

/// A keystone: the ratio of the median dot spacing along the first row (or column, `vertical`
/// false: along the left column) to that along the last, rows and columns found by clustering the
/// dots' positions across them at half the median spacing. 1 for a grid seen square on.
pub fn keystone(dots: &[[f64; 2]], vertical: bool) -> Option<f64> {
    // Across the rows (y for rows, x for columns) and along them.
    let (across, along) = if vertical { (1, 0) } else { (0, 1) };
    let mut sorted: Vec<[f64; 2]> = dots.to_vec();
    sorted.sort_by(|a, b| a[across].total_cmp(&b[across]));
    let gaps: Vec<f64> = sorted
        .windows(2)
        .map(|pair| pair[1][across] - pair[0][across])
        .filter(|gap| *gap > 1.0)
        .collect();
    let spacing = median(&gaps);
    if !spacing.is_finite() {
        return None;
    }
    let mut lines: Vec<Vec<f64>> = Vec::new();
    let mut last = f64::NEG_INFINITY;
    for dot in &sorted {
        if dot[across] - last > spacing / 2.0 || lines.is_empty() {
            lines.push(Vec::new());
        }
        last = dot[across];
        lines.last_mut().expect("a line").push(dot[along]);
    }
    let pitch = |line: &Vec<f64>| -> Option<f64> {
        let mut positions = line.clone();
        positions.sort_by(f64::total_cmp);
        let steps: Vec<f64> = positions.windows(2).map(|pair| pair[1] - pair[0]).collect();
        (steps.len() >= 2).then(|| median(&steps))
    };
    let full: Vec<&Vec<f64>> = lines.iter().filter(|line| line.len() >= 3).collect();
    let (first, last) = (full.first()?, full.last()?);
    Some(pitch(first)? / pitch(last)?)
}

// -------------------------------------------------------------------------------------------
// Fitting
// -------------------------------------------------------------------------------------------

/// The CIEDE2000 from neutral at and above which a sample counts as affected by a setting.
pub const AFFECTED: f64 = 0.5;

/// How close, in the fit's distance, a candidate must be to the best to be indistinguishable from
/// it: the fit's resolution on a round, reported as each point's span.
pub const TIE: f64 = 0.1;

/// The residual below which a best value at the end of Luxforge's range is a match, not a range
/// shortfall: a quarter of the level C threshold of 2.
pub const SHORTFALL_RESIDUAL: f64 = 0.5;

/// The knots of a frozen map: 21 across the Lightroom range.
pub const KNOTS: usize = 21;

/// How finely the search refines between the sampled values around the best one.
const REFINE: usize = 16;

/// One Lightroom sample a colour fit matches: the colours it reaches from Luxforge's neutral
/// ([`carried`]), as CIELAB, and the samples it is judged on: those it moves at least
/// [`AFFECTED`] from neutral, or every sample when it moves none, so a setting that reaches only
/// the shadows is judged on the shadows.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub labs: Vec<Lab>,
    pub samples: Vec<usize>,
}

impl Target {
    pub fn new(neutral: &[Lab], labs: Vec<Lab>) -> Self {
        let affected: Vec<usize> = (0..labs.len().min(neutral.len()))
            .filter(|&i| ciede2000(neutral[i], labs[i]) >= AFFECTED)
            .collect();
        let samples = if affected.is_empty() {
            (0..labs.len().min(neutral.len())).collect()
        } else {
            affected
        };
        Self { labs, samples }
    }

    /// The median CIEDE2000 between `candidate`'s colours and this target's over its samples.
    pub fn distance(&self, candidate: &[Lab]) -> f64 {
        let mut values: Vec<f64> = self
            .samples
            .iter()
            .map(|&i| ciede2000(candidate[i], self.labs[i]))
            .collect();
        quantile_in_place(&mut values, 0.5)
    }

    /// The response's own size: the median CIEDE2000 from `neutral` over its samples.
    pub fn size(&self, neutral: &[Lab]) -> f64 {
        self.distance(neutral)
    }
}

/// Linear interpolation between two sampled colour sets.
pub fn lerp_labs(a: &[Lab], b: &[Lab], t: f64) -> Vec<Lab> {
    a.iter()
        .zip(b)
        .map(|(a, b)| [0, 1, 2].map(|c| a[c] + t * (b[c] - a[c])))
        .collect()
}

/// Which way a fitted map runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Increasing,
    Decreasing,
}

/// One fitted point: a Lightroom value, the Luxforge value with the closest response once the map
/// is monotone, the distance left there, the distance at the identity conversion (the same number,
/// `None` outside Luxforge's sampled range), whether the best value lies at the end of Luxforge's
/// range still improving towards it and further than [`SHORTFALL_RESIDUAL`] away (a range
/// shortfall, reported and never clamped), and the sampled Luxforge values within [`TIE`] of the
/// best.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitPoint {
    pub lightroom: f64,
    pub luxforge: f64,
    pub residual: f64,
    pub identity: Option<f64>,
    pub shortfall: bool,
    pub span: [f64; 2],
}

/// One knot of a map: a Lightroom value and the Luxforge value the map gives it, `None` outside
/// the measured span, and whether it is a measured value (a sample or the neutral anchor) rather
/// than interpolated between two.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Knot {
    pub lightroom: f64,
    pub luxforge: Option<f64>,
    pub measured: bool,
}

/// A fitted translation.
#[derive(Clone, Debug, PartialEq)]
pub struct Fit {
    pub points: Vec<FitPoint>,
    pub direction: Direction,
    pub knots: Vec<Knot>,
}

/// One sample's search: the best value and the distance at every sampled value.
struct Search {
    best: f64,
    row: Vec<f64>,
}

/// The level A fit of colour responses: [`fit_by`] over CIELAB sets with [`Target::distance`].
pub fn fit(
    values: &[f64],
    luxforge: &[Vec<Lab>],
    lightroom: &[(f64, Target)],
    anchor: Option<(f64, f64)>,
    range: (f64, f64),
) -> Fit {
    fit_by(
        values,
        luxforge,
        lightroom,
        |target: &Target, labs: &Vec<Lab>| target.distance(labs),
        Some(|a: &Vec<Lab>, b: &Vec<Lab>, t: f64| lerp_labs(a, b, t)),
        anchor,
        range,
    )
}

/// The level A fit: for each Lightroom sample the Luxforge value whose response is closest by
/// `distance`, searched at every sampled value and then refined between the two around the best
/// through `lerp` (sampled values only without it); then made monotone, through the neutral
/// `anchor` (Lightroom's default, Luxforge's default) when the family holds nothing else away from
/// neutral, by pooling adjacent violators, in whichever
/// direction leaves the smaller total residual, each residual re-measured at its pooled value; and
/// tabulated as [`KNOTS`] knots across `range`, Lightroom's.
///
/// `values` are Luxforge's sampled values, strictly increasing, and `luxforge` its response at
/// each. The searches run in parallel, one per sample.
pub fn fit_by<R, T, D, L>(
    values: &[f64],
    luxforge: &[R],
    lightroom: &[(f64, T)],
    distance: D,
    lerp: Option<L>,
    anchor: Option<(f64, f64)>,
    range: (f64, f64),
) -> Fit
where
    R: Sync,
    T: Sync,
    D: Fn(&T, &R) -> f64 + Sync,
    L: Fn(&R, &R, f64) -> R + Sync,
{
    assert_eq!(values.len(), luxforge.len());
    assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
    let (low, high) = (values[0], values[values.len() - 1]);
    // The distance at any value inside the sampled span: the sampled response itself, or with
    // `lerp` the blend of the two around it; infinite outside it, or between samples without lerp.
    let at = |target: &T, value: f64| -> f64 {
        if value < low - 1e-9 || value > high + 1e-9 {
            return f64::INFINITY;
        }
        let upper = values
            .partition_point(|v| *v < value - 1e-9)
            .min(values.len() - 1);
        if (values[upper] - value).abs() <= 1e-9 {
            return distance(target, &luxforge[upper]);
        }
        match &lerp {
            Some(lerp) if upper > 0 => {
                let t = (value - values[upper - 1]) / (values[upper] - values[upper - 1]);
                distance(target, &lerp(&luxforge[upper - 1], &luxforge[upper], t))
            }
            _ => f64::INFINITY,
        }
    };
    let search = |target: &T| -> Search {
        let row: Vec<f64> = luxforge.iter().map(|r| distance(target, r)).collect();
        let index = (0..row.len())
            .min_by(|a, b| row[*a].total_cmp(&row[*b]))
            .unwrap_or(0);
        let mut best = (values[index], row[index]);
        if lerp.is_some() {
            let from = values[index.saturating_sub(1)];
            let to = values[(index + 1).min(values.len() - 1)];
            for step in 0..=REFINE {
                let value = from + (to - from) * step as f64 / REFINE as f64;
                let d = at(target, value);
                if d < best.1 {
                    best = (value, d);
                }
            }
        }
        Search { best: best.0, row }
    };
    let searches: Vec<Search> = parallel_map(lightroom, |(_, target)| search(target));
    // Pool through the anchor, which every block holding it is pinned to.
    let pool = |direction: Direction| -> Vec<f64> {
        let sign = if direction == Direction::Increasing {
            1.0
        } else {
            -1.0
        };
        let mut items: Vec<(f64, f64, f64, Option<usize>)> = lightroom
            .iter()
            .zip(&searches)
            .enumerate()
            .map(|(i, ((value, _), search))| (*value, sign * search.best, 1.0, Some(i)))
            .collect();
        if let Some(anchor) = anchor {
            items.push((anchor.0, sign * anchor.1, 1e9, None));
        }
        items.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut blocks: Vec<(f64, f64, Vec<Option<usize>>)> = Vec::new();
        for (_, value, weight, index) in items {
            blocks.push((value, weight, vec![index]));
            while blocks.len() > 1 {
                let last = blocks.len() - 1;
                if blocks[last - 1].0 <= blocks[last].0 {
                    break;
                }
                let (value, weight, members) = blocks.pop().expect("a block");
                let previous = blocks.last_mut().expect("a block");
                previous.0 = (previous.0 * previous.1 + value * weight) / (previous.1 + weight);
                previous.1 += weight;
                previous.2.extend(members);
            }
        }
        let mut pooled = vec![0.0; lightroom.len()];
        for (value, _, members) in blocks {
            for index in members.into_iter().flatten() {
                pooled[index] = (sign * value).clamp(low, high);
            }
        }
        pooled
    };
    let residuals = |pooled: &[f64]| -> Vec<f64> {
        let at = &at;
        parallel_map(
            &lightroom.iter().zip(pooled).collect::<Vec<_>>(),
            |((_, target), value)| at(target, **value),
        )
    };
    let increasing = pool(Direction::Increasing);
    let decreasing = pool(Direction::Decreasing);
    let up = residuals(&increasing);
    let down = residuals(&decreasing);
    let total = |r: &[f64]| r.iter().copied().filter(|d| d.is_finite()).sum::<f64>();
    let (direction, pooled, residual) = if total(&down) < total(&up) {
        (Direction::Decreasing, decreasing, down)
    } else {
        (Direction::Increasing, increasing, up)
    };
    let inward_step = values
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .fold(f64::INFINITY, f64::min)
        / 4.0;
    let points: Vec<FitPoint> = lightroom
        .iter()
        .zip(&searches)
        .zip(pooled.iter().zip(&residual))
        .map(|(((value, target), search), (luxforge_value, residual))| {
            let at_end =
                (*luxforge_value - low).abs() < 1e-9 || (*luxforge_value - high).abs() < 1e-9;
            let inward = if (*luxforge_value - high).abs() < 1e-9 {
                luxforge_value - inward_step
            } else {
                luxforge_value + inward_step
            };
            let shortfall = at_end
                && *residual > SHORTFALL_RESIDUAL
                && (lerp.is_none() || at(target, inward) > *residual);
            let nearest = search.row.iter().copied().fold(f64::INFINITY, f64::min);
            let within: Vec<f64> = search
                .row
                .iter()
                .zip(values)
                .filter(|(d, _)| **d <= nearest + TIE)
                .map(|(_, v)| *v)
                .collect();
            FitPoint {
                lightroom: *value,
                luxforge: *luxforge_value,
                residual: *residual,
                identity: (*value >= low - 1e-9 && *value <= high + 1e-9)
                    .then(|| at(target, *value))
                    .filter(|d| d.is_finite()),
                shortfall,
                span: [
                    within.iter().copied().fold(*luxforge_value, f64::min),
                    within.iter().copied().fold(*luxforge_value, f64::max),
                ],
            }
        })
        .collect();
    let knots = knots(&points, anchor, range);
    Fit {
        points,
        direction,
        knots,
    }
}

/// [`KNOTS`] knots across `range`, each the map's value there by linear interpolation between the
/// fitted points and the anchor, if any, `None` outside their span.
pub fn knots(points: &[FitPoint], anchor: Option<(f64, f64)>, range: (f64, f64)) -> Vec<Knot> {
    let mut known: Vec<(f64, f64)> = points.iter().map(|p| (p.lightroom, p.luxforge)).collect();
    known.extend(anchor);
    known.sort_by(|a, b| a.0.total_cmp(&b.0));
    known.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
    (0..KNOTS)
        .map(|k| {
            let lightroom = range.0 + (range.1 - range.0) * k as f64 / (KNOTS - 1) as f64;
            let measured = known.iter().any(|(v, _)| (v - lightroom).abs() < 1e-9);
            let luxforge = known.windows(2).find_map(|pair| {
                let ((v0, u0), (v1, u1)) = (pair[0], pair[1]);
                (lightroom >= v0 - 1e-9 && lightroom <= v1 + 1e-9)
                    .then(|| u0 + (lightroom - v0) / (v1 - v0) * (u1 - u0))
            });
            Knot {
                lightroom,
                luxforge: luxforge.or_else(|| {
                    known
                        .iter()
                        .find(|(v, _)| (v - lightroom).abs() < 1e-9)
                        .map(|(_, u)| *u)
                }),
                measured,
            }
        })
        .collect()
}

/// `work` over every item on a bounded pool of scoped threads, the results in item order.
fn parallel_map<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(items.len())
        .max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<Option<R>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            return done;
                        };
                        done.push((index, work(item)));
                    }
                })
            })
            .collect();
        for handle in handles {
            for (index, result) in handle.join().expect("a fit worker panicked") {
                results[index] = Some(result);
            }
        }
    });
    results
        .into_iter()
        .map(|result| result.expect("every item was taken"))
        .collect()
}
