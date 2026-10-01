//! Independent f64 lens geometry, transcribed from Lensfun v0.3.4 and the fixed-canvas design.
//! Lensfun source: modifier.cpp 203–253, lens.cpp 870–943, auxfun.cpp 441–462,
//! mod-coord.cpp 426–654. No product code is imported.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Model {
    Poly3,
    Poly5,
    PtLens,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    Outside,
    Unconverged,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Refusal {
    Invalid,
    FocalOutOfRange,
}
#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    pub focal: f64,
    pub terms: [f64; 3],
}

pub fn lensfun_norm(
    width: u32,
    height: u32,
    crop_lens: f64,
    crop_camera: f64,
    aspect_lens: f64,
) -> f64 {
    let w = f64::from(width - 1);
    let h = f64::from(height - 1);
    let m = w.min(h);
    let aspect = w.max(h) / m;
    2.0 * (aspect_lens * aspect_lens + 1.0).sqrt() / (aspect * aspect + 1.0).sqrt() * crop_lens
        / crop_camera
        / m
}
pub fn unit_scale(
    width: u32,
    height: u32,
    crop_lens: f64,
    crop_camera: f64,
    aspect_lens: f64,
) -> f64 {
    lensfun_norm(width, height, crop_lens, crop_camera, aspect_lens) * f64::from(width.min(height))
        / 2.0
}
pub fn radial_g(model: Model, t: [f64; 3], r: f64) -> f64 {
    match model {
        Model::Poly3 => 1.0 - t[0] + t[0] * r * r,
        Model::Poly5 => 1.0 + t[0] * r * r + t[1] * r * r * r * r,
        Model::PtLens => t[0] * r * r * r + t[1] * r * r + t[2] * r + 1.0 - t[0] - t[1] - t[2],
    }
}
pub fn radial_f(model: Model, t: [f64; 3], r: f64) -> f64 {
    r * radial_g(model, t, r)
}
pub fn radial_df(model: Model, t: [f64; 3], r: f64) -> f64 {
    match model {
        Model::Poly3 => 1.0 - t[0] + 3.0 * t[0] * r * r,
        Model::Poly5 => 1.0 + 3.0 * t[0] * r * r + 5.0 * t[1] * r * r * r * r,
        Model::PtLens => {
            4.0 * t[0] * r * r * r + 3.0 * t[1] * r * r + 2.0 * t[2] * r + 1.0 - t[0] - t[1] - t[2]
        }
    }
}
fn quadratic(a: f64, b: f64, c: f64) -> Vec<f64> {
    if a == 0.0 {
        return if b == 0.0 { vec![] } else { vec![-c / b] };
    }
    let d = b * b - 4.0 * a * c;
    if d < 0.0 {
        return vec![];
    }
    let q = -0.5 * (b + d.sqrt().copysign(b));
    if q == 0.0 {
        vec![-b / (2.0 * a)]
    } else {
        vec![q / a, c / q]
    }
}
/// Isolate roots with derivative partitions. Used for the reference's compile-time extrema,
/// independently of the product's polynomial solver.
fn cubic_on(a: f64, b: f64, c: f64, d: f64, end: f64) -> Vec<f64> {
    if a == 0.0 {
        return quadratic(b, c, d)
            .into_iter()
            .filter(|r| *r >= 0.0 && *r <= end)
            .collect();
    }
    let f = |r: f64| ((a * r + b) * r + c) * r + d;
    let mut knots = vec![0.0, end];
    knots.extend(
        quadratic(3.0 * a, 2.0 * b, c)
            .into_iter()
            .filter(|r| *r > 0.0 && *r < end),
    );
    knots.sort_by(f64::total_cmp);
    let mut roots = vec![];
    for &r in &knots {
        if f(r).abs() < 1e-14 {
            roots.push(r);
        }
    }
    for pair in knots.windows(2) {
        let (mut lo, mut hi) = (pair[0], pair[1]);
        let sign = f(lo).is_sign_positive();
        if sign == f(hi).is_sign_positive() {
            continue;
        }
        for _ in 0..80 {
            let m = (lo + hi) / 2.0;
            if f(m).is_sign_positive() == sign {
                lo = m;
            } else {
                hi = m;
            }
        }
        roots.push((lo + hi) / 2.0);
    }
    roots
}
pub fn monotone_radius(model: Model, t: [f64; 3], rmax: f64) -> f64 {
    let cap = 4.0 * rmax;
    let roots = match model {
        Model::Poly3 => quadratic(3.0 * t[0], 0.0, 1.0 - t[0]),
        Model::Poly5 => quadratic(5.0 * t[1], 3.0 * t[0], 1.0)
            .into_iter()
            .filter(|x| *x >= 0.0)
            .map(f64::sqrt)
            .collect(),
        Model::PtLens => cubic_on(
            4.0 * t[0],
            3.0 * t[1],
            2.0 * t[2],
            1.0 - t[0] - t[1] - t[2],
            cap,
        ),
    };
    roots.into_iter().filter(|r| *r > 0.0).fold(cap, f64::min)
}
/// Safeguarded Newton on a monotone interval. `epsilon` is in normalized input units.
pub fn inverse_with_epsilon(
    model: Model,
    terms: [f64; 3],
    rd: f64,
    rmono: f64,
    epsilon: f64,
) -> Result<(f64, u32), Outcome> {
    if !rd.is_finite() || rd < 0.0 || rd > radial_f(model, terms, rmono) {
        return Err(Outcome::Outside);
    }
    let (mut lo, mut hi) = (0.0, rmono);
    let mut r = rd.clamp(lo, hi);
    for count in 1..=32 {
        let residual = radial_f(model, terms, r) - rd;
        if residual.abs() <= epsilon {
            return Ok((r, count));
        }
        if residual > 0.0 {
            hi = r;
        } else {
            lo = r;
        }
        let next = r - residual / radial_df(model, terms, r);
        r = if next.is_finite() && next > lo && next < hi {
            next
        } else {
            (lo + hi) / 2.0
        };
    }
    Err(Outcome::Unconverged)
}
pub fn radial_inverse_bracketed(
    model: Model,
    terms: [f64; 3],
    rd: f64,
    rmono: f64,
) -> Result<(f64, u32), Outcome> {
    inverse_with_epsilon(model, terms, rd, rmono, 1e-12)
}
pub fn hermite(y0: Option<f64>, y1: f64, y2: f64, y3: Option<f64>, t: f64) -> f64 {
    let m1 = y0.map_or(y2 - y1, |y| (y2 - y) / 2.0);
    let m2 = y3.map_or(y2 - y1, |y| (y - y1) / 2.0);
    let t2 = t * t;
    let t3 = t2 * t;
    (2.0 * t3 - 3.0 * t2 + 1.0) * y1
        + (t3 - 2.0 * t2 + t) * m1
        + (-2.0 * t3 + 3.0 * t2) * y2
        + (t3 - t2) * m2
}
pub fn lensfun_interpolate(calibrations: &[Calibration], focal: f64) -> Result<[f64; 3], Refusal> {
    if calibrations.is_empty() || !focal.is_finite() || focal <= 0.0 {
        return Err(Refusal::Invalid);
    }
    let mut c = calibrations.to_vec();
    c.sort_by(|a, b| a.focal.total_cmp(&b.focal));
    let low = c[0].focal;
    let high = c.last().unwrap().focal;
    if c.len() == 1 {
        return if (focal / low - 1.0).abs() <= 0.01 {
            Ok(c[0].terms)
        } else {
            Err(Refusal::FocalOutOfRange)
        };
    }
    if focal < low / 1.01 || focal > high * 1.01 {
        return Err(Refusal::FocalOutOfRange);
    }
    let f = focal.clamp(low, high);
    if let Some(v) = c.iter().find(|c| c.focal == f) {
        return Ok(v.terms);
    }
    let above = c.iter().position(|c| c.focal > f).unwrap();
    let below = above - 1;
    let a = c[below];
    let b = c[above];
    let t = (f - a.focal) / (b.focal - a.focal);
    Ok(std::array::from_fn(|i| {
        hermite(
            below.checked_sub(1).map(|j| c[j].terms[i] * c[j].focal),
            a.terms[i] * a.focal,
            b.terms[i] * b.focal,
            c.get(above + 1).map(|c| c.terms[i] * c.focal),
            t,
        ) / f
    }))
}
pub fn g_critical(model: Model, t: [f64; 3]) -> Vec<f64> {
    match model {
        Model::Poly3 => vec![],
        Model::Poly5 => {
            let r2 = -t[0] / (2.0 * t[1]);
            if r2 > 0.0 { vec![r2.sqrt()] } else { vec![] }
        }
        Model::PtLens => quadratic(3.0 * t[0], 2.0 * t[1], t[2]),
    }
}
fn df_critical(model: Model, t: [f64; 3]) -> Vec<f64> {
    match model {
        Model::Poly3 => vec![],
        Model::Poly5 => {
            let r2 = -3.0 * t[0] / (10.0 * t[1]);
            if r2 > 0.0 { vec![r2.sqrt()] } else { vec![] }
        }
        Model::PtLens => quadratic(12.0 * t[0], 6.0 * t[1], 2.0 * t[2]),
    }
}
pub fn cover_scale_lens(model: Model, t: [f64; 3], width: u32, height: u32, ns: f64) -> f64 {
    let rmin = f64::from(width.min(height)) * ns / 2.0;
    let rmax = f64::from(width).hypot(f64::from(height)) * ns / 2.0;
    let mono = monotone_radius(model, t, rmax);
    let lower = radial_inverse_bracketed(model, t, rmin, mono).map_or(rmax, |x| x.0);
    let upper = radial_inverse_bracketed(model, t, rmax, mono).map_or(rmax, |x| x.0.min(rmax));
    if lower > upper {
        return 1.0;
    }
    let mut candidates = vec![lower, upper];
    candidates.extend(
        g_critical(model, t)
            .into_iter()
            .filter(|r| *r >= lower && *r <= upper),
    );
    candidates
        .into_iter()
        .map(|r| radial_g(model, t, r))
        .fold(1.0, f64::max)
}
pub fn local_scale_lens(model: Model, t: [f64; 3], rmax: f64, cover: f64) -> f64 {
    let end = rmax / cover;
    let mut c = vec![0.0, end];
    c.extend(g_critical(model, t));
    c.extend(df_critical(model, t));
    c.into_iter()
        .filter(|r| *r >= 0.0 && *r <= end)
        .map(|r| radial_g(model, t, r).max(radial_df(model, t, r)) / cover)
        .fold(0.0, f64::max)
}
pub fn cover_scale_perspective(width: u32, height: u32, horizontal: i64, vertical: i64) -> f64 {
    1.0 + (horizontal as f64 / 400.0).abs() * f64::from(width) / f64::from(width.max(height))
        + (vertical as f64 / 400.0).abs() * f64::from(height) / f64::from(width.max(height))
}
pub fn perspective_forward(
    q: [f64; 2],
    centre: [f64; 2],
    unit: f64,
    h: f64,
    v: f64,
    cover: f64,
) -> [f64; 2] {
    let u = [(q[0] - centre[0]) / unit, (q[1] - centre[1]) / unit];
    let d = 1.0 + h * u[0] + v * u[1];
    [
        centre[0] + unit * cover * u[0] / d,
        centre[1] + unit * cover * u[1] / d,
    ]
}
pub fn perspective_inverse(
    p: [f64; 2],
    centre: [f64; 2],
    unit: f64,
    h: f64,
    v: f64,
    cover: f64,
) -> [f64; 2] {
    let w = [
        (p[0] - centre[0]) / (unit * cover),
        (p[1] - centre[1]) / (unit * cover),
    ];
    let d = 1.0 - h * w[0] - v * w[1];
    [centre[0] + unit * w[0] / d, centre[1] + unit * w[1] / d]
}
pub fn local_scale_perspective_bound(width: u32, height: u32, h: f64, v: f64, cover: f64) -> f64 {
    let unit = f64::from(width.max(height)) / 2.0;
    let a = f64::from(width) / (2.0 * unit * cover);
    let b = f64::from(height) / (2.0 * unit * cover);
    let d = 1.0 - h.abs() * a - v.abs() * b;
    (d + a.hypot(b) * h.hypot(v)) / (d * d * cover)
}
pub fn radial_input(
    p: [f64; 2],
    centre: [f64; 2],
    ns: f64,
    model: Model,
    t: [f64; 3],
    cover: f64,
) -> [f64; 2] {
    let u = [
        (p[0] - centre[0]) * ns / cover,
        (p[1] - centre[1]) * ns / cover,
    ];
    let g = radial_g(model, t, u[0].hypot(u[1]));
    [centre[0] + u[0] * g / ns, centre[1] + u[1] * g / ns]
}
/// Continuous radial bounds: corners, axis crossings, and g' roots on each edge.
pub fn reads_radial_rect(
    rect: [f64; 4],
    centre: [f64; 2],
    ns: f64,
    model: Model,
    t: [f64; 3],
    cover: f64,
) -> [f64; 4] {
    let [x0, y0, x1, y1] = rect;
    let mut points = vec![[x0, y0], [x0, y1], [x1, y0], [x1, y1]];
    for x in [x0, x1] {
        if centre[1] >= y0 && centre[1] <= y1 {
            points.push([x, centre[1]]);
        }
    }
    for y in [y0, y1] {
        if centre[0] >= x0 && centre[0] <= x1 {
            points.push([centre[0], y]);
        }
    }
    for r in g_critical(model, t).into_iter().filter(|r| *r > 0.0) {
        let radius = r * cover / ns;
        for x in [x0, x1] {
            let d = radius * radius - (x - centre[0]).powi(2);
            if d >= 0.0 {
                for y in [centre[1] - d.sqrt(), centre[1] + d.sqrt()] {
                    if y >= y0 && y <= y1 {
                        points.push([x, y]);
                    }
                }
            }
        }
        for y in [y0, y1] {
            let d = radius * radius - (y - centre[1]).powi(2);
            if d >= 0.0 {
                for x in [centre[0] - d.sqrt(), centre[0] + d.sqrt()] {
                    if x >= x0 && x <= x1 {
                        points.push([x, y]);
                    }
                }
            }
        }
    }
    let mut bound = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for p in points {
        let q = radial_input(p, centre, ns, model, t, cover);
        bound[0] = bound[0].min(q[0]);
        bound[1] = bound[1].min(q[1]);
        bound[2] = bound[2].max(q[0]);
        bound[3] = bound[3].max(q[1]);
    }
    bound
}
pub fn bilinear_linear(frame: &[[f64; 3]], width: u32, height: u32, p: [f64; 2]) -> [f64; 3] {
    let x = p[0] - 0.5;
    let y = p[1] - 0.5;
    let ix = x.floor() as i64;
    let iy = y.floor() as i64;
    let dx = x - x.floor();
    let dy = y - y.floor();
    let at = |x: i64, y: i64| {
        frame[(y.clamp(0, i64::from(height) - 1) * i64::from(width)
            + x.clamp(0, i64::from(width) - 1)) as usize]
    };
    let (a, b, c, d) = (
        at(ix, iy),
        at(ix + 1, iy),
        at(ix, iy + 1),
        at(ix + 1, iy + 1),
    );
    std::array::from_fn(|i| {
        (a[i] * (1.0 - dx) + b[i] * dx) * (1.0 - dy) + (c[i] * (1.0 - dx) + d[i] * dx) * dy
    })
}

/// Lensfun v0.3.4 data/db/mil-nikon.xml, NIKKOR Z 24-70mm f/4 S.
pub const NIKON_Z_24_70: [Calibration; 5] = [
    Calibration {
        focal: 24.0,
        terms: [0.032, -0.114, 0.073],
    },
    Calibration {
        focal: 28.0,
        terms: [0.022, -0.077, 0.061],
    },
    Calibration {
        focal: 35.0,
        terms: [0.019, -0.056, 0.063],
    },
    Calibration {
        focal: 50.0,
        terms: [0.024, -0.067, 0.096],
    },
    Calibration {
        focal: 70.0,
        terms: [0.012, -0.017, 0.039],
    },
];
#[derive(Clone, Copy, Debug)]
pub struct NamedCalibration {
    pub name: &'static str,
    pub source: &'static str,
    pub model: Model,
    pub terms: [f64; 3],
    pub crop: f64,
    pub aspect: f64,
}
/// Constants copied from the pinned XML, including its full-frame APS-C lens calibrations.
pub const NAMED: [NamedCalibration; 9] = [
    NamedCalibration {
        name: "NIKKOR Z 24-70mm f/4 S at 35",
        source: "mil-nikon.xml:555-570",
        model: Model::PtLens,
        terms: [0.019, -0.056, 0.063],
        crop: 1.0,
        aspect: 1.5,
    },
    NamedCalibration {
        name: "FC3411 at 8.4",
        source: "actioncams.xml:277-283",
        model: Model::PtLens,
        terms: [0.014878, -0.0317787, -0.000777849],
        crop: 2.63,
        aspect: 1.5,
    },
    NamedCalibration {
        name: "X100V at 23",
        source: "compact-fujifilm.xml:730-738",
        model: Model::PtLens,
        terms: [0.012, -0.035, 0.028],
        crop: 1.53,
        aspect: 1.5,
    },
    NamedCalibration {
        name: "Canon PowerShot G12 at 6.1",
        source: "compact-canon.xml:1066-1074",
        model: Model::Poly5,
        terms: [-0.030571633, 0.004658548, 0.0],
        crop: 4.63,
        aspect: 4.0 / 3.0,
    },
    NamedCalibration {
        name: "Olympus M.Zuiko Digital ED 14-42 at 14",
        source: "mil-olympus.xml:276-284",
        model: Model::Poly3,
        terms: [-0.079, 0.0, 0.0],
        crop: 2.0,
        aspect: 4.0 / 3.0,
    },
    NamedCalibration {
        name: "Sony E 10-18 at 10 (full-frame calibration)",
        source: "mil-sony.xml:606-614",
        model: Model::PtLens,
        terms: [0.196017, -0.375554, 0.167273],
        crop: 1.0,
        aspect: 1.5,
    },
    NamedCalibration {
        name: "Sigma 17-50 EX DC HSM at 17",
        source: "slr-sigma.xml:2076-2084",
        model: Model::PtLens,
        terms: [0.235921, -0.485918, 0.275462],
        crop: 1.0,
        aspect: 1.5,
    },
    NamedCalibration {
        name: "Tokina 11-16 at 11 (full-frame calibration)",
        source: "slr-tokina.xml:9-18",
        model: Model::PtLens,
        terms: [-0.024, -0.002, 0.005],
        crop: 1.0,
        aspect: 1.5,
    },
    NamedCalibration {
        name: "identity",
        source: "synthetic exact zero",
        model: Model::Poly3,
        terms: [0.0; 3],
        crop: 1.0,
        aspect: 1.5,
    },
];
