//! The Clone and Heal numerical study (`docs/design/corrections-numerics.md`): which Heal solver
//! and schedule, which colour domain, and what the frozen ones cost and miss.
//!
//! - **Solver.** The frozen cascade ([`repair::SCHEDULE`]) against the converged membrane (the
//!   harmonic interpolation of the same boundary differences, solved by over-relaxation to a
//!   `1e-12` update) and against a normalized-convolution pyramid, the bounded alternative with no
//!   sweep at all. On spots and strokes of several sizes over flat, ramped, textured and
//!   frame-edge inputs, the figures are the largest and RMS difference of the correction in 8-bit
//!   encoded codes.
//! - **Domain.** A textured source moved from a dark area into a bright one, healed additively in
//!   linear light and in the sRGB-encoded domain: how much of the source texture's relative
//!   contrast survives.
//! - **Search.** The candidate set's synthetic answers live in the core's fixtures; here the
//!   weights' effect is shown on textures whose right answer is known.
//!
//! Naturalness is not measured here: these are numerical properties. The photographic review of
//! the frozen contract is recorded in the numerics document.

use luxforge_reference::{
    SplitMix64,
    repair::{self, Class, Compiled, Image, Kind, Level, Operation, Schedule},
    srgb,
};

/// `sum += v`, channel by channel in order.
fn add(sum: &mut [f64; 3], v: [f64; 3]) {
    for (s, x) in sum.iter_mut().zip(v) {
        *s += x;
    }
}

fn image(width: usize, height: usize, f: impl Fn(usize, usize) -> [f64; 3]) -> Image {
    Image::new(
        width,
        height,
        (0..width * height)
            .map(|i| f(i % width, i / width))
            .collect(),
    )
}

/// A texture: a smooth ramp with band-limited noise of amplitude `amp`.
fn texture(width: usize, height: usize, seed: u64, base: f64, amp: f64) -> Image {
    let mut rng = SplitMix64(seed);
    let noise: Vec<f64> = (0..width * height).map(|_| rng.next_noise()).collect();
    image(width, height, |x, y| {
        let n = |dx: usize, dy: usize| {
            noise[(y + dy).min(height - 1) * width + (x + dx).min(width - 1)]
        };
        let v = 0.25 * (n(0, 0) + n(1, 0) + n(0, 1) + n(1, 1));
        let ramp = base * (1.0 + 0.6 * x as f64 / width as f64);
        [
            ramp * (1.0 + amp * v),
            ramp * (1.0 + amp * v) * 0.9,
            ramp * 1.1,
        ]
    })
}

fn spot(x: f64, y: f64, radius: f64, feather: f64, offset: [f64; 2]) -> Operation {
    Operation {
        kind: Kind::Heal,
        paths: vec![vec![[x, y]]],
        radius,
        feather,
        opacity: 100.0,
        offset,
    }
}

/// The converged membrane: over-relaxed Gauss-Seidel on the finest level alone, from the frozen
/// solver's answer, until no update exceeds `1e-12`.
fn converged(finest: &Level) -> Level {
    let mut level = repair::solve(finest.clone(), repair::SCHEDULE);
    let n = level.width.max(level.height) as f64;
    let omega = 2.0 / (1.0 + (std::f64::consts::PI / n).sin());
    for _ in 0..200_000 {
        let mut largest = 0.0f64;
        for parity in [0, 1] {
            for j in 0..level.height {
                for i in 0..level.width {
                    let at = j * level.width + i;
                    if (i + j) % 2 != parity || level.class[at] != Class::Unknown {
                        continue;
                    }
                    let mut sum = [0.0; 3];
                    let mut count = 0.0;
                    for (x, y) in [
                        (i.wrapping_sub(1), j),
                        (i + 1, j),
                        (i, j.wrapping_sub(1)),
                        (i, j + 1),
                    ] {
                        if x >= level.width || y >= level.height {
                            continue;
                        }
                        let k = y * level.width + x;
                        if level.class[k] == Class::Outside {
                            continue;
                        }
                        add(&mut sum, level.f[k]);
                        count += 1.0;
                    }
                    if count > 0.0 {
                        for (value, s) in level.f[at].iter_mut().zip(sum) {
                            let update = omega * (s / count - *value);
                            *value += update;
                            largest = largest.max(update.abs());
                        }
                    }
                }
            }
        }
        if largest < 1e-12 {
            return level;
        }
    }
    panic!("the membrane did not converge");
}

/// The normalized-convolution pyramid: the boundary values and their indicator reduced by a
/// `[1 4 6 4 1] / 16` kernel and halving, then expanded back with the same kernel, each level's
/// expanded estimate filling what the finer level lacks; the correction is the ratio. A bounded
/// alternative with no sweep, for comparison only.
fn pyramid(finest: &Level) -> Level {
    let taps = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    type Plane = (usize, usize, Vec<[f64; 4]>);
    let blur = |(w, h, v): &Plane| -> Plane {
        let mut across = vec![[0.0; 4]; w * h];
        for y in 0..*h {
            for x in 0..*w {
                let mut s = [0.0; 4];
                for (k, t) in taps.iter().enumerate() {
                    let xx = (x as i64 + k as i64 - 2).clamp(0, *w as i64 - 1) as usize;
                    for c in 0..4 {
                        s[c] += t * v[y * w + xx][c];
                    }
                }
                across[y * w + x] = s;
            }
        }
        let mut down = vec![[0.0; 4]; w * h];
        for y in 0..*h {
            for x in 0..*w {
                let mut s = [0.0; 4];
                for (k, t) in taps.iter().enumerate() {
                    let yy = (y as i64 + k as i64 - 2).clamp(0, *h as i64 - 1) as usize;
                    for c in 0..4 {
                        s[c] += t * across[yy * w + x][c];
                    }
                }
                down[y * w + x] = s;
            }
        }
        (*w, *h, down)
    };
    let (w, h) = (finest.width, finest.height);
    let base: Vec<[f64; 4]> = (0..w * h)
        .map(|i| {
            if finest.class[i] == Class::Fixed {
                let f = finest.f[i];
                [f[0], f[1], f[2], 1.0]
            } else {
                [0.0; 4]
            }
        })
        .collect();
    let mut levels: Vec<Plane> = vec![(w, h, base)];
    while levels.last().is_some_and(|l| l.0 > 1 || l.1 > 1) {
        let blurred = blur(levels.last().expect("a level"));
        let (bw, bh) = (blurred.0.div_ceil(2), blurred.1.div_ceil(2));
        let mut v = vec![[0.0; 4]; bw * bh];
        for y in 0..bh {
            for x in 0..bw {
                v[y * bw + x] = blurred.2[(2 * y) * blurred.0 + 2 * x];
            }
        }
        levels.push((bw, bh, v));
    }
    let mut above = levels.pop().expect("the coarsest");
    while let Some(mut level) = levels.pop() {
        let (lw, lh) = (level.0, level.1);
        let mut up = vec![[0.0; 4]; lw * lh];
        for y in 0..lh {
            for x in 0..lw {
                up[y * lw + x] = above.2[(y / 2) * above.0 + x / 2];
            }
        }
        let (_, _, smooth) = blur(&(lw, lh, up));
        for (cell, filler) in level.2.iter_mut().zip(smooth) {
            let weight = cell[3].min(1.0);
            for c in 0..4 {
                cell[c] += (1.0 - weight) * filler[c];
            }
        }
        above = level;
    }
    let mut out = finest.clone();
    for i in 0..w * h {
        if out.class[i] == Class::Unknown {
            let v = above.2[i];
            out.f[i] = if v[3] > 0.0 {
                [v[0] / v[3], v[1] / v[3], v[2] / v[3]]
            } else {
                [0.0; 3]
            };
        }
    }
    out
}

/// The largest and RMS difference of two solutions over the unknowns, in 8-bit encoded codes.
fn difference(a: &Level, b: &Level) -> (f64, f64) {
    let (mut largest, mut sum, mut n) = (0.0f64, 0.0, 0usize);
    for i in 0..a.f.len() {
        if a.class[i] != Class::Unknown {
            continue;
        }
        for c in 0..3 {
            let d = 255.0 * (a.f[i][c] - b.f[i][c]).abs();
            largest = largest.max(d);
            sum += d * d;
            n += 1;
        }
    }
    (largest, (sum / n.max(1) as f64).sqrt())
}

struct Case {
    name: &'static str,
    image: Image,
    op: Operation,
}

fn cases() -> Vec<Case> {
    let (w, h) = (480, 320);
    let tex = texture(w, h, 7, 0.2, 0.4);
    let ramp = image(w, h, |x, y| {
        let t = x as f64 / w as f64;
        let s = y as f64 / h as f64;
        [0.05 + 0.6 * t, 0.1 + 0.5 * s, 0.3 + 0.2 * t * s]
    });
    let contrast = image(w, h, |x, _| if x < w / 2 { [0.02; 3] } else { [0.8; 3] });
    let stroke = |radius: f64, offset: [f64; 2]| Operation {
        kind: Kind::Heal,
        paths: vec![vec![[0.2, 0.3], [0.45, 0.42], [0.7, 0.36], [0.8, 0.6]]],
        radius,
        feather: 30.0,
        opacity: 100.0,
        offset,
    };
    vec![
        Case {
            name: "small spot, texture",
            image: tex.clone(),
            op: spot(0.5, 0.5, 8.0 / 320.0, 50.0, [0.1, 0.0]),
        },
        Case {
            name: "spot r=32 px, texture",
            image: tex.clone(),
            op: spot(0.5, 0.5, 32.0 / 320.0, 50.0, [0.0, 0.25]),
        },
        Case {
            name: "spot r=96 px, ramp",
            image: ramp.clone(),
            op: spot(0.45, 0.5, 96.0 / 320.0, 0.0, [0.3, 0.0]),
        },
        Case {
            name: "spot r=96 px, texture",
            image: tex.clone(),
            op: spot(0.45, 0.5, 96.0 / 320.0, 20.0, [0.3, 0.0]),
        },
        Case {
            name: "spot across a hard edge",
            image: contrast.clone(),
            op: spot(0.5, 0.5, 40.0 / 320.0, 0.0, [0.0, 0.3]),
        },
        Case {
            name: "spot at the frame edge",
            image: tex.clone(),
            op: spot(0.01, 0.5, 30.0 / 320.0, 30.0, [0.15, 0.0]),
        },
        Case {
            name: "thin stroke r=6 px, texture",
            image: tex.clone(),
            op: stroke(6.0 / 320.0, [0.0, 0.15]),
        },
        Case {
            name: "stroke r=20 px, ramp",
            image: ramp,
            op: stroke(20.0 / 320.0, [0.0, 0.2]),
        },
        Case {
            name: "stroke r=20 px, hard edge",
            image: contrast,
            op: stroke(20.0 / 320.0, [0.05, 0.2]),
        },
    ]
}

/// The rejected cascade: levels whose fixed cells hold the mean of their fixed children, solved
/// from the coarsest down with `k` red-black iterations a level, each finer level started from its
/// parent's value (piecewise constant). No residual is carried, so a coarse level's moved,
/// averaged boundary is never corrected.
fn cascade(finest: &Level, k: usize) -> Level {
    let coarser = |level: &Level| {
        let mut coarse = level.coarser();
        for j in 0..coarse.height {
            for i in 0..coarse.width {
                let at = j * coarse.width + i;
                if coarse.class[at] != Class::Fixed {
                    continue;
                }
                let (mut sum, mut n) = ([0.0; 3], 0.0);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (x, y) = (2 * i + dx, 2 * j + dy);
                    if x < level.width
                        && y < level.height
                        && level.class[y * level.width + x] == Class::Fixed
                    {
                        add(&mut sum, level.f[y * level.width + x]);
                        n += 1.0;
                    }
                }
                coarse.f[at] = sum.map(|v| v / n);
            }
        }
        coarse
    };
    let mut levels = vec![finest.clone()];
    while levels.last().is_some_and(|l| l.width > 1 || l.height > 1) {
        let next = coarser(levels.last().expect("a level"));
        levels.push(next);
    }
    let mut above: Option<Level> = None;
    while let Some(mut level) = levels.pop() {
        if let Some(coarse) = &above {
            for j in 0..level.height {
                for i in 0..level.width {
                    if level.class[j * level.width + i] == Class::Unknown {
                        level.f[j * level.width + i] = coarse.f[(j / 2) * coarse.width + i / 2];
                    }
                }
            }
        }
        let zero = vec![[0.0; 3]; level.f.len()];
        for _ in 0..k {
            level.sweep(&zero);
        }
        above = Some(level);
    }
    above.expect("the finest level")
}

/// The frozen solver, the other V-cycle schedules measured, the rejected cascade and the
/// convolution pyramid, each against the converged membrane, case by case: the table the numerics
/// document records. Holds the frozen schedule within one code of the membrane everywhere.
#[test]
fn slow_heal_solvers_against_the_converged_membrane() {
    let schedules = [
        ("start, 3 x V(2,2) (frozen)", repair::SCHEDULE),
        (
            "start, 2 x V(2,2)",
            Schedule {
                start: true,
                cycles: 2,
                pre: 2,
                post: 2,
            },
        ),
        (
            "start, 3 x V(1,1)",
            Schedule {
                start: true,
                cycles: 3,
                pre: 1,
                post: 1,
            },
        ),
        (
            "start, 4 x V(1,1)",
            Schedule {
                start: true,
                cycles: 4,
                pre: 1,
                post: 1,
            },
        ),
        (
            "start, 2 x V(3,3)",
            Schedule {
                start: true,
                cycles: 2,
                pre: 3,
                post: 3,
            },
        ),
        (
            "zero, 3 x V(2,2)",
            Schedule {
                start: false,
                cycles: 3,
                pre: 2,
                post: 2,
            },
        ),
        (
            "zero, 4 x V(2,2)",
            Schedule {
                start: false,
                cycles: 4,
                pre: 2,
                post: 2,
            },
        ),
    ];
    let mut worst = vec![0.0f64; schedules.len()];
    println!(
        "case | unknowns | schedules ... | cascade K=4 | pyramid   (max/rms difference from the membrane, 8-bit codes)"
    );
    for case in cases() {
        let compiled = Compiled::new(&case.op, &case.image.stage());
        let problem = repair::problem(&case.image, &compiled).expect("a footprint");
        let exact = converged(&problem.level);
        let unknowns = problem
            .level
            .class
            .iter()
            .filter(|c| **c == Class::Unknown)
            .count();
        let mut row = format!("{} | {unknowns}", case.name);
        for (n, (_, schedule)) in schedules.iter().enumerate() {
            let (m, r) = difference(&repair::solve(problem.level.clone(), *schedule), &exact);
            worst[n] = worst[n].max(m);
            row.push_str(&format!(" | {m:.2}/{r:.3}"));
        }
        let (m, r) = difference(&cascade(&problem.level, 4), &exact);
        row.push_str(&format!(" | {m:.2}/{r:.3}"));
        let (m, r) = difference(&pyramid(&problem.level), &exact);
        row.push_str(&format!(" | {m:.2}/{r:.3}"));
        println!("{row}");
    }
    for ((name, _), worst) in schedules.iter().zip(&worst) {
        println!("{name}: worst {worst:.2} codes");
    }
    assert!(
        worst[0] < 1.0,
        "the frozen schedule is {:.2} codes from the membrane",
        worst[0]
    );
}

/// Heal with the frozen cascade, but on linear-light differences and an additive linear
/// correction: the rejected domain, for comparison.
fn heal_linear(img: &Image, compiled: &Compiled, level: Option<Level>) -> Image {
    let problem = repair::problem(img, compiled).expect("a footprint");
    let mut linear_level = problem.level.clone();
    let source = |x: usize, y: usize| {
        (
            (x as i64 + compiled.delta[0]) as usize,
            (y as i64 + compiled.delta[1]) as usize,
        )
    };
    for j in 0..linear_level.height {
        for i in 0..linear_level.width {
            let at = j * linear_level.width + i;
            if linear_level.class[at] == Class::Fixed {
                let (x, y) = (problem.x0 + i, problem.y0 + j);
                let (sx, sy) = source(x, y);
                let (a, b) = (img.at(x, y), img.at(sx, sy));
                linear_level.f[at] = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
            }
        }
    }
    let solved = level.unwrap_or_else(|| repair::solve(linear_level, repair::SCHEDULE));
    let mut out = img.clone();
    for j in 0..solved.height {
        for i in 0..solved.width {
            let at = j * solved.width + i;
            if solved.class[at] != Class::Unknown {
                continue;
            }
            let (x, y) = (problem.x0 + i, problem.y0 + j);
            let (sx, sy) = source(x, y);
            let s = img.at(sx, sy);
            let a = problem.alpha[at];
            let i_p = img.at(x, y);
            out.pixels[y * img.width + x] =
                [0, 1, 2].map(|c| (1.0 - a) * i_p[c] + a * (s[c] + solved.f[at][c]));
        }
    }
    out
}

/// A Heal whose correction is `level` (the encoded domain), composited as the frozen one is.
fn heal_from(
    img: &Image,
    compiled: &Compiled,
    x0: usize,
    y0: usize,
    alpha: &[f64],
    level: &Level,
) -> Image {
    let mut out = img.clone();
    for j in 0..level.height {
        for i in 0..level.width {
            let at = j * level.width + i;
            if level.class[at] != Class::Unknown {
                continue;
            }
            let (x, y) = (x0 + i, y0 + j);
            let s = img.at(
                (x as i64 + compiled.delta[0]) as usize,
                (y as i64 + compiled.delta[1]) as usize,
            );
            let healed = [0, 1, 2]
                .map(|c| srgb::decode_encoded(srgb::encode_extended(s[c]) + level.f[at][c]));
            let i_p = img.at(x, y);
            out.pixels[y * img.width + x] =
                [0, 1, 2].map(|c| (1.0 - alpha[at]) * i_p[c] + alpha[at] * healed[c]);
        }
    }
    out
}

/// How much of a source texture's relative contrast survives a Heal from a dark area into a bright
/// one, solved additively in linear light against the frozen sRGB-encoded domain.
#[test]
fn heal_domain_keeps_relative_texture_contrast() {
    let (w, h) = (256, 128);
    let mut rng = SplitMix64(11);
    let noise: Vec<f64> = (0..w * h).map(|_| rng.next_noise()).collect();
    // Left half dark (0.03), right half bright (0.5), both with the same relative texture.
    let img = image(w, h, |x, y| {
        let base = if x < w / 2 { 0.03 } else { 0.5 };
        [base * (1.0 + 0.1 * noise[y * w + x]); 3]
    });
    let stage = img.stage();
    // Heal a spot in the bright half from the dark half.
    let op = Operation {
        kind: Kind::Heal,
        paths: vec![vec![[0.75, 0.5]]],
        radius: 20.0 / 128.0,
        feather: 0.0,
        opacity: 100.0,
        offset: [-0.5, 0.0],
    };
    let compiled = Compiled::new(&op, &stage);
    let encoded = repair::heal(&img, &compiled);
    let linear = heal_linear(&img, &compiled, None);
    // Relative contrast: standard deviation over mean, inside the footprint's core.
    let relative = |out: &Image| {
        let (mut sum, mut sq, mut n) = (0.0, 0.0, 0.0);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = stage.pixel_uv(x as u32, y as u32);
                if compiled.distance(u, v) < 0.6 * op.radius {
                    let g = out.at(x, y)[0];
                    sum += g;
                    sq += g * g;
                    n += 1.0;
                }
            }
        }
        let mean = sum / n;
        (sq / n - mean * mean).max(0.0).sqrt() / mean
    };
    let source = relative(&image(w, h, |x, y| {
        img.at((x as i64 + compiled.delta[0]).max(0) as usize, y)
    }));
    let (lin, enc) = (relative(&linear), relative(&encoded));
    println!(
        "relative texture contrast: source {source:.4}, healed linear {lin:.4} ({:.0}%), \
         encoded {enc:.4} ({:.0}%)",
        100.0 * lin / source,
        100.0 * enc / source
    );
    assert!(
        enc > 2.0 * lin,
        "the encoded domain keeps more of the texture"
    );
    assert!(
        encoded
            .pixels
            .iter()
            .all(|p| p.iter().all(|v| v.is_finite()))
    );
}

// ---- Photographic review --------------------------------------------------------------------

/// The owner's photograph for the review, read from `LUXFORGE_REPAIR_PHOTO` or the main
/// checkout's local fixture; `None` (and the review skipped) when neither is there.
fn photo() -> Option<image::RgbImage> {
    let path = std::env::var("LUXFORGE_REPAIR_PHOTO")
        .unwrap_or_else(|_| "../../fixtures/jpg/will-sapa-drone.jpg".into());
    image::open(&path).ok().map(|image| image.to_rgb8())
}

/// A crop of `photo` decoded to linear light.
fn crop(photo: &image::RgbImage, x0: u32, y0: u32, w: u32, h: u32) -> Image {
    image(w as usize, h as usize, |x, y| {
        photo
            .get_pixel(x0 + x as u32, y0 + y as u32)
            .0
            .map(srgb::decode)
    })
}

fn write(img: &Image, name: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/repair-study");
    std::fs::create_dir_all(&dir).expect("the review directory");
    let mut out = image::RgbImage::new(img.width as u32, img.height as u32);
    for (i, p) in img.pixels.iter().enumerate() {
        out.put_pixel(
            (i % img.width) as u32,
            (i / img.width) as u32,
            image::Rgb(p.map(srgb::code)),
        );
    }
    out.save(dir.join(name)).expect("a review image");
}

/// `|a - b|` in codes, times `gain`, as a grey image.
fn difference_image(a: &Image, b: &Image, gain: f64) -> Image {
    image(a.width, a.height, |x, y| {
        let (p, q) = (a.at(x, y), b.at(x, y));
        let d = (0..3)
            .map(|c| (srgb::encode_extended(p[c]) - srgb::encode_extended(q[c])).abs())
            .fold(0.0, f64::max);
        [srgb::decode_encoded((d * gain).min(1.0)); 3]
    })
}

/// Writes the review crops under `artifacts/repair-study`: each case's input, Clone, the frozen
/// Heal, the converged membrane, the linear-domain Heal and the frozen Heal's difference from the
/// membrane at 10x. Run on request: `cargo test -p luxforge-reference --release --test studies
/// repair::photographic_review -- --ignored --nocapture`.
#[test]
#[ignore = "writes review images from a local photograph"]
fn photographic_review() {
    let Some(photo) = photo() else {
        println!("photographic_review: no photograph; skipped");
        return;
    };
    struct Review {
        name: &'static str,
        at: [u32; 4],
        paths: Vec<Vec<[f64; 2]>>,
        radius_px: f64,
        feather: f64,
        source_px: [f64; 2],
    }
    let reviews = [
        Review {
            name: "road",
            at: [1525, 1990, 1186, 594],
            paths: vec![vec![
                [0.0, 268.0],
                [60.0, 200.0],
                [150.0, 166.0],
                [250.0, 166.0],
                [380.0, 205.0],
                [500.0, 214.0],
                [650.0, 210.0],
                [720.0, 220.0],
                [800.0, 246.0],
                [900.0, 296.0],
                [1000.0, 356.0],
                [1100.0, 430.0],
                [1186.0, 486.0],
            ]],
            radius_px: 40.0,
            feather: 30.0,
            source_px: [0.0, -80.0],
        },
        Review {
            name: "blotches",
            at: [424, 2542, 847, 677],
            paths: vec![vec![[250.0, 420.0]]],
            radius_px: 40.0,
            feather: 50.0,
            source_px: [110.0, 0.0],
        },
    ];
    for review in reviews {
        let [x0, y0, w, h] = review.at;
        let img = crop(&photo, x0, y0, w, h);
        let (wf, hf) = (f64::from(w), f64::from(h));
        let op = Operation {
            kind: Kind::Heal,
            paths: review
                .paths
                .iter()
                .map(|path| path.iter().map(|[x, y]| [x / wf, y / hf]).collect())
                .collect(),
            radius: review.radius_px / hf,
            feather: review.feather,
            opacity: 100.0,
            offset: [review.source_px[0] / wf, review.source_px[1] / hf],
        };
        let stage = img.stage();
        let compiled = Compiled::new(&op, &stage);
        let started = std::time::Instant::now();
        let healed = repair::heal(&img, &compiled);
        let heal_time = started.elapsed();
        let cloned = repair::clone(
            &img,
            &Compiled::new(
                &Operation {
                    kind: Kind::Clone,
                    ..op.clone()
                },
                &stage,
            ),
        );
        let problem = repair::problem(&img, &compiled).expect("a footprint");
        let exact = converged(&problem.level);
        let membrane = heal_from(
            &img,
            &compiled,
            problem.x0,
            problem.y0,
            &problem.alpha,
            &exact,
        );
        let linear = heal_linear(&img, &compiled, None);
        let (m, r) = difference(
            &repair::solve(problem.level.clone(), repair::SCHEDULE),
            &exact,
        );
        println!(
            "{}: {} unknowns, frozen Heal {heal_time:?} (f64 reference), against the membrane max {m:.2} rms {r:.3} codes",
            review.name,
            problem
                .level
                .class
                .iter()
                .filter(|c| **c == Class::Unknown)
                .count()
        );
        write(&img, &format!("{}-input.png", review.name));
        write(&cloned, &format!("{}-clone.png", review.name));
        write(&healed, &format!("{}-heal.png", review.name));
        write(&membrane, &format!("{}-membrane.png", review.name));
        write(&linear, &format!("{}-heal-linear.png", review.name));
        write(
            &difference_image(&healed, &membrane, 10.0),
            &format!("{}-heal-vs-membrane-x10.png", review.name),
        );
    }
}

// ---- The automatic source on textures whose right answer is known ---------------------------

fn search_of(img: &Image, op: &Operation) -> repair::Search {
    let stage = img.stage();
    let r = repair::reduction(repair::extent(op, &stage), &stage);
    let reduced = repair::reduced(img, r);
    let window = [0, 0, reduced.width as i64, reduced.height as i64];
    repair::search(&reduced, op, &stage, r, window)
}

fn ranked(search: repair::Search) -> Vec<repair::Candidate> {
    match search {
        repair::Search::Ranked(ranked) => ranked,
        repair::Search::NoSource => panic!("no source"),
    }
}

fn spot_px(img: &Image, x: f64, y: f64, radius_px: f64) -> Operation {
    let (w, h) = (img.width as f64, img.height as f64);
    Operation {
        kind: Kind::Heal,
        paths: vec![vec![[x / w, y / h]]],
        radius: radius_px / h,
        feather: 50.0,
        opacity: 100.0,
        offset: [0.0, 0.0],
    }
}

/// On a flat field every surviving candidate scores its distance penalty alone: a spot's nearest
/// ring (1.5 E) always overlaps its own footprint, so the best is the 2.25 E ring's first direction,
/// and the ranking runs ring by ring, direction by direction.
#[test]
fn search_flat_field_takes_the_nearest_ring_and_the_smallest_angle() {
    let img = image(400, 300, |_, _| [0.18; 3]);
    let ranked = ranked(search_of(&img, &spot_px(&img, 200.0, 150.0, 20.0)));
    assert!(ranked.iter().all(|c| c.ring > 0), "1.5 E overlaps a spot");
    assert_eq!((ranked[0].ring, ranked[0].direction), (1, 0));
    assert_eq!(ranked[0].score, repair::DISTANCE_WEIGHT * repair::RINGS[1]);
    let order: Vec<(usize, usize)> = ranked.iter().map(|c| (c.ring, c.direction)).collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);
}

/// A vertical gradient matches its ring exactly only along the horizontal: directions 0 and 12 tie
/// on the ring and texture, and the tie goes to the smaller angle.
#[test]
fn search_gradient_matches_along_its_level_lines() {
    let img = image(400, 300, |_, y| [0.05 + 0.6 * y as f64 / 300.0; 3]);
    let ranked = ranked(search_of(&img, &spot_px(&img, 200.0, 150.0, 20.0)));
    assert_eq!((ranked[0].ring, ranked[0].direction), (1, 0));
    assert_eq!((ranked[1].ring, ranked[1].direction), (1, 12));
    assert_eq!(ranked[0].score, ranked[1].score);
}

/// A horizontal stroke's 1.5 E ring survives where it moves the stroke off itself: at 15 degrees
/// the two footprints still touch, at 30 degrees they part, so on a flat field the best is ring 0,
/// direction 2.
#[test]
fn search_stroke_uses_the_nearest_ring_off_its_own_axis() {
    let img = image(400, 300, |_, _| [0.3; 3]);
    let (w, h) = (400.0, 300.0);
    let op = Operation {
        kind: Kind::Clone,
        paths: vec![vec![[170.0 / w, 150.0 / h], [230.0 / w, 150.0 / h]]],
        radius: 6.0 / h,
        feather: 0.0,
        opacity: 100.0,
        offset: [0.0, 0.0],
    };
    let ranked = ranked(search_of(&img, &op));
    assert_eq!((ranked[0].ring, ranked[0].direction), (0, 2));
    assert!(
        !ranked
            .iter()
            .any(|c| c.ring == 0 && [0, 1, 11, 12, 13, 23].contains(&c.direction))
    );
}

/// A spot by the left edge has no candidate that reads past it, and a spot too large for its frame
/// has none at all.
#[test]
fn search_rejects_the_frame_and_answers_no_source() {
    let img = image(400, 300, |_, _| [0.3; 3]);
    let ranked = ranked(search_of(&img, &spot_px(&img, 30.0, 150.0, 15.0)));
    assert!(ranked.iter().all(|c| {
        let [ux, _] = repair::direction(c.direction);
        ux > -0.5
    }));
    let small = image(100, 100, |_, _| [0.3; 3]);
    assert_eq!(
        search_of(&small, &spot_px(&small, 50.0, 50.0, 30.0)),
        repair::Search::NoSource
    );
}

/// A destination in a band of fine texture takes a source inside the band, though one partly in
/// the flat field around it matches the ring more closely (two independent textures differ by
/// more than a texture and a flat field do): the texture term decides it.
#[test]
fn search_texture_term_keeps_texture_with_texture() {
    let mut rng = SplitMix64(5);
    let noise: Vec<f64> = (0..200 * 120).map(|_| rng.next_noise()).collect();
    let img = image(200, 120, |x, y| {
        let textured = (40..80).contains(&y);
        let n = if textured { noise[y * 200 + x] } else { 0.0 };
        [srgb::decode_encoded(0.5 + 0.04 * n); 3]
    });
    let op = spot_px(&img, 100.0, 60.0, 10.0);
    let best = ranked(search_of(&img, &op))[0];
    let source_y = 60.0 + best.delta[1] as f64;
    assert!(
        source_y - 10.0 >= 40.0 && source_y + 10.0 < 80.0,
        "the source stays in the texture: {best:?}"
    );
}

// ---- The automatic source on the quadrant fixture --------------------------------------------

/// The ring term `A` and texture term `T` of one candidate, recomputed beside the reference's
/// score so a review can say which term decided it.
fn terms(reduced: &Image, sets: &repair::Sets, delta: [i64; 2]) -> (f64, f64) {
    let inside = |p: [i64; 2]| {
        p[0] >= 0
            && p[1] >= 0
            && (p[0] as usize) < reduced.width
            && (p[1] as usize) < reduced.height
    };
    let ring: Vec<[i64; 2]> = sets.ring.iter().copied().filter(|p| inside(*p)).collect();
    let energy = |set: &[[i64; 2]], shift: [i64; 2]| {
        let member = |p: [i64; 2]| {
            set.binary_search_by(|q| (q[1], q[0]).cmp(&(p[1], p[0])))
                .is_ok()
        };
        let (mut sum, mut n) = (0.0, 0usize);
        for &[x, y] in set {
            for next in [[x + 1, y], [x, y + 1]] {
                if member(next) {
                    let a = reduced.at((x + shift[0]) as usize, (y + shift[1]) as usize);
                    let b =
                        reduced.at((next[0] + shift[0]) as usize, (next[1] + shift[1]) as usize);
                    for c in 0..3 {
                        sum += (a[c] - b[c]) * (a[c] - b[c]);
                    }
                    n += 1;
                }
            }
        }
        if n == 0 {
            0.0
        } else {
            (sum / (3 * n) as f64).sqrt()
        }
    };
    let mut sum = 0.0;
    for q in &ring {
        let a = reduced.at(q[0] as usize, q[1] as usize);
        let b = reduced.at((q[0] + delta[0]) as usize, (q[1] + delta[1]) as usize);
        for c in 0..3 {
            sum += (a[c] - b[c]) * (a[c] - b[c]);
        }
    }
    let a = if ring.is_empty() {
        0.0
    } else {
        (sum / (3 * ring.len()) as f64).sqrt()
    };
    let t = (energy(&sets.interior, delta) - energy(&ring, [0, 0])).abs();
    (a, t)
}

/// The share of an operation's full-resolution footprint, moved by `delta`, that is near-white
/// (every channel's code above 200): on the quadrant fixture, its labels and arrow.
fn white_share(img: &Image, op: &Operation, delta: [i64; 2]) -> f64 {
    let stage = img.stage();
    let compiled = Compiled::new(op, &stage);
    let (mut white, mut n) = (0usize, 0usize);
    for y in 0..img.height {
        for x in 0..img.width {
            if compiled.coverage(&stage, x, y) <= 0.0 {
                continue;
            }
            let (sx, sy) = (x as i64 + delta[0], y as i64 + delta[1]);
            if sx < 0 || sy < 0 || sx as usize >= img.width || sy as usize >= img.height {
                continue;
            }
            n += 1;
            if img
                .at(sx as usize, sy as usize)
                .iter()
                .all(|v| srgb::code(*v) > 200)
            {
                white += 1;
            }
        }
    }
    if n == 0 { 0.0 } else { white as f64 / n as f64 }
}

/// The `corrections` smoke's automatic Heals over the quadrant fixture's flat green quadrant (its
/// painted stroke and its `⇧`-click straight stroke, radius 0.04 content heights), every surviving
/// candidate with its ring term, texture term and the near-white share of its source, and the
/// Heal the best candidate makes written under `artifacts/repair-study/`. Run on request:
/// `cargo test -p luxforge-reference --release --test studies repair::search_review_quadrants --
/// --ignored --nocapture`.
#[test]
#[ignore = "a review of the automatic source on the quadrant fixture; prints and writes images"]
fn search_review_quadrants() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0/orientation-1.jpg");
    let photo = image::open(&path).expect("the quadrant fixture").to_rgb8();
    let (w, h) = photo.dimensions();
    let img = crop(&photo, 0, 0, w, h);
    let stage = img.stage();
    let cases: [(&str, Vec<[f64; 2]>); 2] = [
        ("stroke", vec![[0.62, 0.30], [0.72, 0.30], [0.82, 0.30]]),
        ("straight", vec![[0.82, 0.30], [0.82, 0.40]]),
    ];
    for (name, path) in cases {
        let op = Operation {
            kind: Kind::Heal,
            paths: vec![path],
            radius: 0.04,
            feather: 50.0,
            opacity: 100.0,
            offset: [0.0, 0.0],
        };
        let e = repair::extent(&op, &stage);
        let r = repair::reduction(e, &stage);
        let reduced = repair::reduced(&img, r);
        let sets = repair::sets(&op, &stage, r);
        let window = [0, 0, reduced.width as i64, reduced.height as i64];
        let ranked = ranked(repair::search(&reduced, &op, &stage, r, window));
        println!(
            "{name}: extent {:.1} px, reduction {r}, {} candidates survive of 96",
            e * f64::from(stage.height),
            ranked.len()
        );
        println!("rank ring dir   delta(px)        A        T    score  white");
        for (rank, c) in ranked.iter().enumerate() {
            let (a, t) = terms(&reduced, &sets, c.delta);
            let full = repair::delta(c.offset, &stage);
            println!(
                "{:>4} {:>4} {:>3} {:>5},{:>5} {:>8.4} {:>8.4} {:>8.4} {:>6.3}",
                rank + 1,
                repair::RINGS[c.ring],
                c.direction * 15,
                full[0],
                full[1],
                a,
                t,
                c.score,
                white_share(&img, &op, full)
            );
        }
        // A candidate set this review proposes beside the frozen one: in each direction, offsets
        // that are multiples of the least shift that clears the destination's grown footprint
        // (its width across that direction, not the stroke's half-diagonal), scored by the same
        // terms. Not the frozen contract: evidence for a proposed change.
        let grown = &sets.grown;
        let member = |p: [i64; 2]| {
            grown
                .binary_search_by(|q| (q[1], q[0]).cmp(&(p[1], p[0])))
                .is_ok()
        };
        let (mut bx0, mut by0, mut bx1, mut by1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        for p in grown.iter().chain(
            sets.ring
                .iter()
                .filter(|p| p[0] >= 0 && p[1] >= 0 && p[0] < window[2] && p[1] < window[3]),
        ) {
            bx0 = bx0.min(p[0]);
            by0 = by0.min(p[1]);
            bx1 = bx1.max(p[0]);
            by1 = by1.max(p[1]);
        }
        let mut proposed = Vec::new();
        for j in 0..repair::DIRECTIONS {
            let [cu, cv] = repair::direction(j);
            let mut clear = None;
            for step in 1..4096 {
                let t = step as f64 * 0.5;
                let d = [(t * cu).round() as i64, (t * cv).round() as i64];
                if !grown.iter().any(|p| member([p[0] + d[0], p[1] + d[1]])) {
                    clear = Some(t);
                    break;
                }
            }
            let Some(clear) = clear else { continue };
            for m in [1.0, 1.5, 2.25, 3.25] {
                let t = clear * m;
                let d = [(t * cu).round() as i64, (t * cv).round() as i64];
                if bx0 + d[0] < 0
                    || by0 + d[1] < 0
                    || bx1 + d[0] >= window[2]
                    || by1 + d[1] >= window[3]
                {
                    continue;
                }
                if grown.iter().any(|p| member([p[0] + d[0], p[1] + d[1]])) {
                    continue;
                }
                let (a, t_term) = terms(&reduced, &sets, d);
                let full = [d[0] * r as i64, d[1] * r as i64];
                proposed.push((
                    a + t_term + repair::DISTANCE_WEIGHT * m,
                    m,
                    j,
                    full,
                    a,
                    t_term,
                ));
            }
        }
        proposed.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then(a.1.total_cmp(&b.1))
                .then(a.2.cmp(&b.2))
        });
        println!(
            "{name}, proposed clearing-distance candidates: {} survive",
            proposed.len()
        );
        for (rank, (score, m, j, full, a, t)) in proposed.iter().take(8).enumerate() {
            println!(
                "{:>4} {:>4} {:>3} {:>5},{:>5} {:>8.4} {:>8.4} {:>8.4} {:>6.3}",
                rank + 1,
                m,
                j * 15,
                full[0],
                full[1],
                a,
                t,
                score,
                white_share(&img, &op, *full)
            );
        }
        if let Some((_, _, _, full, _, _)) = proposed.first() {
            let offset = [
                full[0] as f64 / f64::from(stage.width),
                full[1] as f64 / f64::from(stage.height),
            ];
            let healed = repair::heal(
                &img,
                &Compiled::new(
                    &Operation {
                        offset,
                        ..op.clone()
                    },
                    &stage,
                ),
            );
            write(&healed, &format!("quadrants-{name}-heal-proposed.png"));
        }
        let best = ranked[0];
        let healed = repair::heal(
            &img,
            &Compiled::new(
                &Operation {
                    offset: best.offset,
                    ..op.clone()
                },
                &stage,
            ),
        );
        write(&healed, &format!("quadrants-{name}-heal.png"));
    }
}
