//! Numerical proofs independent of the product renderer. Committed outputs are an oracle,
//! not photographs or a claim that a camera/lens is photographically qualified.
use luxforge_reference::SplitMix64;
use luxforge_reference::geometry::*;
use serde_json::{Value, json};
use std::path::PathBuf;
const ASPECTS: [(u32, u32); 5] = [
    (6048, 4024),
    (6000, 4500),
    (6400, 3600),
    (4000, 4000),
    (4032, 6048),
];
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/geometry/lens-perspective.json")
}
fn close(a: f64, b: f64, t: f64) {
    assert!((a - b).abs() <= t, "{a} != {b} (tolerance {t})");
}
fn stage(c: NamedCalibration, w: u32, h: u32) -> (f64, f64, f64) {
    let ns = lensfun_norm(w, h, c.crop, c.crop, c.aspect);
    let s = cover_scale_lens(c.model, c.terms, w, h, ns);
    let r = f64::from(w).hypot(f64::from(h)) * ns / 2.0;
    (ns, s, r)
}
#[test]
fn lensfun_interpolation_matches_reference_vectors() {
    for (f, expected) in [
        (26.0, [0.026132, -0.093154, 0.065238]),
        (30.0, [0.020039, -0.067526, 0.058313]),
        (42.0, [0.022247, -0.064513, 0.084468]),
        (60.0, [0.017932, -0.041531, 0.067609]),
    ] {
        let got = lensfun_interpolate(&NIKON_Z_24_70, f).unwrap();
        for i in 0..3 {
            close(got[i], expected[i], 1e-6);
        }
    }
    for c in NIKON_Z_24_70 {
        assert_eq!(
            lensfun_interpolate(&NIKON_Z_24_70, c.focal).unwrap(),
            c.terms
        );
    }
}
#[test]
fn focal_beyond_calibrations_refuses_without_extrapolation() {
    for f in [1.0, 23.0, 72.0] {
        assert_eq!(
            lensfun_interpolate(&NIKON_Z_24_70, f),
            Err(Refusal::FocalOutOfRange)
        );
    }
}
#[test]
fn prime_focal_within_one_percent_snaps_to_calibration() {
    let c = [Calibration {
        focal: 8.4,
        terms: NAMED[1].terms,
    }];
    assert_eq!(lensfun_interpolate(&c, 8.38).unwrap(), c[0].terms);
    assert_eq!(lensfun_interpolate(&c, 8.3), Err(Refusal::FocalOutOfRange));
}
#[test]
fn lens_cover_scale_closed_form_matches_dense_sampling() {
    for c in NAMED {
        for (w, h) in ASPECTS[..4].iter().copied() {
            let (ns, s, rmax) = stage(c, w, h);
            let centre = [f64::from(w) / 2.0, f64::from(h) / 2.0];
            let mono = monotone_radius(c.model, c.terms, rmax);
            let mut dense: f64 = 1.0;
            for n in 0..=4096 {
                let t = f64::from(n) / 4096.0;
                for p in [
                    [f64::from(w) * t, 0.0],
                    [f64::from(w) * t, f64::from(h)],
                    [0.0, f64::from(h) * t],
                    [f64::from(w), f64::from(h) * t],
                ] {
                    let rho = (p[0] - centre[0]).hypot(p[1] - centre[1]) * ns;
                    if let Ok((r, _)) = radial_inverse_bracketed(c.model, c.terms, rho, mono) {
                        dense = dense.max(rho / r);
                    }
                }
            }
            // Dense boundary is an independent lower bound on the tight analytic cover scale.
            assert!(s + 1e-9 >= dense, "{} {w}x{h}: {s} < {dense}", c.name);
            assert!(s - dense < 1e-9, "{}: {s} - {dense}", c.name);
            for y in 0..=256 {
                for x in 0..=256 {
                    let q = radial_input(
                        [
                            f64::from(w) * f64::from(x) / 256.0,
                            f64::from(h) * f64::from(y) / 256.0,
                        ],
                        centre,
                        ns,
                        c.model,
                        c.terms,
                        s * (1.0 + 1e-12),
                    );
                    assert!(
                        q[0] >= -1e-8
                            && q[1] >= -1e-8
                            && q[0] <= f64::from(w) + 1e-8
                            && q[1] <= f64::from(h) + 1e-8,
                        "{} {q:?}",
                        c.name
                    );
                }
            }
        }
    }
}
#[test]
fn barrel_profile_certifies_at_unit_scale() {
    assert_eq!(
        cover_scale_lens(Model::Poly3, [-0.05, 0.0, 0.0], 6000, 4000, 2.0 / 4000.0),
        1.0
    );
}
#[test]
fn perspective_cover_scale_is_tight_at_extremes() {
    for (w, h) in ASPECTS {
        for horizontal in [-100, 0, 100] {
            for vertical in [-100, 0, 100] {
                let s = cover_scale_perspective(w, h, horizontal, vertical);
                let c = [f64::from(w) / 2.0, f64::from(h) / 2.0];
                let u = f64::from(w.max(h)) / 2.0;
                let inside = |scale| {
                    [
                        [0.0, 0.0],
                        [0.0, f64::from(h)],
                        [f64::from(w), 0.0],
                        [f64::from(w), f64::from(h)],
                    ]
                    .into_iter()
                    .all(|p| {
                        let q = perspective_inverse(
                            p,
                            c,
                            u,
                            horizontal as f64 / 400.0,
                            vertical as f64 / 400.0,
                            scale,
                        );
                        q[0] >= -1e-9
                            && q[1] >= -1e-9
                            && q[0] <= f64::from(w) + 1e-9
                            && q[1] <= f64::from(h) + 1e-9
                    })
                };
                assert!(inside(s));
                if horizontal != 0 || vertical != 0 {
                    assert!(!inside(s * (1.0 - 1e-6)));
                }
            }
        }
    }
}
fn oriented(mut p: [f64; 2], mirror: bool, turns: usize) -> [f64; 2] {
    if mirror {
        p[0] = -p[0];
    }
    for _ in 0..turns {
        p = [-p[1], p[0]];
    }
    p
}
#[test]
fn perspective_carry_conjugates_all_eight_orientations() {
    for mirror in [false, true] {
        for turns in 0..4 {
            let n = oriented([0.1, -0.0625], mirror, turns);
            for i in 0..10000 {
                let p = [(i % 100) as f64 / 99.0 - 0.5, (i / 100) as f64 / 99.0 - 0.5];
                let lhs = oriented(
                    perspective_forward(p, [0.0; 2], 1.0, 0.1, -0.0625, 1.15),
                    mirror,
                    turns,
                );
                let rhs = perspective_forward(
                    oriented(p, mirror, turns),
                    [0.0; 2],
                    1.0,
                    n[0],
                    n[1],
                    1.15,
                );
                for j in 0..2 {
                    close(lhs[j], rhs[j], 1e-9);
                }
            }
        }
    }
}
#[test]
fn lens_map_is_orientation_invariant() {
    let c = NAMED[0];
    for mirror in [false, true] {
        for turns in 0..4 {
            for i in 0..10000 {
                let p = [(i % 100) as f64 / 50.0 - 1.0, (i / 100) as f64 / 50.0 - 1.0];
                let lhs = oriented(
                    radial_input(p, [0.0; 2], 1.0, c.model, c.terms, 1.02),
                    mirror,
                    turns,
                );
                let rhs = radial_input(
                    oriented(p, mirror, turns),
                    [0.0; 2],
                    1.0,
                    c.model,
                    c.terms,
                    1.02,
                );
                for j in 0..2 {
                    close(lhs[j], rhs[j], 1e-9);
                }
            }
        }
    }
}
#[test]
fn lens_forward_reports_outside_beyond_visible_radius() {
    let c = NAMED[4];
    let mono = monotone_radius(c.model, c.terms, 2.0);
    let edge = radial_f(c.model, c.terms, mono);
    assert_eq!(
        radial_inverse_bracketed(c.model, c.terms, edge + 0.01, mono),
        Err(Outcome::Outside)
    );
}
#[test]
fn lens_inverse_converges_on_visible_points() {
    for c in NAMED {
        let (ns, s, rmax) = stage(c, 6048, 4024);
        let mono = monotone_radius(c.model, c.terms, rmax);
        for i in 0..=10000 {
            let r = rmax / s * f64::from(i) / 10000.0;
            let rd = radial_f(c.model, c.terms, r);
            let (got, n) = inverse_with_epsilon(c.model, c.terms, rd, mono, ns * 1e-4).unwrap();
            assert!(n <= 32);
            close(radial_f(c.model, c.terms, got), rd, ns * 1e-4);
        }
    }
}
#[test]
fn warp_reads_bound_never_underestimates_dense_reference() {
    let mut rng = SplitMix64(93827);
    for c in NAMED {
        let (ns, s, _) = stage(c, 6048, 4024);
        for _ in 0..3000 {
            let x0 = rng.next_range(0.0, 1.0) * 6048.0;
            let y0 = rng.next_range(0.0, 1.0) * 4024.0;
            let x1 = x0 + rng.next_range(0.0, 1.0) * (6048.0 - x0);
            let y1 = y0 + rng.next_range(0.0, 1.0) * (4024.0 - y0);
            let b = reads_radial_rect([x0, y0, x1, y1], [3024.0, 2012.0], ns, c.model, c.terms, s);
            for i in 0..=32 {
                let t = f64::from(i) / 32.0;
                for p in [
                    [x0 + (x1 - x0) * t, y0],
                    [x0 + (x1 - x0) * t, y1],
                    [x0, y0 + (y1 - y0) * t],
                    [x1, y0 + (y1 - y0) * t],
                ] {
                    let q = radial_input(p, [3024.0, 2012.0], ns, c.model, c.terms, s);
                    assert!(
                        q[0] >= b[0] - 1e-8
                            && q[1] >= b[1] - 1e-8
                            && q[0] <= b[2] + 1e-8
                            && q[1] <= b[3] + 1e-8,
                        "{} {b:?} {q:?}",
                        c.name
                    );
                }
            }
        }
    }
}
fn singular(a: f64, b: f64, c: f64, d: f64) -> f64 {
    let sum = a * a + b * b + c * c + d * d;
    let det = a * d - b * c;
    ((sum + (sum * sum - 4.0 * det * det).max(0.0).sqrt()) / 2.0).sqrt()
}
#[test]
fn local_minification_bounds_hold_over_dense_sampling() {
    for c in NAMED {
        let (_, s, r) = stage(c, 6048, 4024);
        let bound = local_scale_lens(c.model, c.terms, r, s);
        for i in 0..=10000 {
            let at = r / s * f64::from(i) / 10000.0;
            assert!(radial_df(c.model, c.terms, at) / s <= bound + 1e-12);
            assert!(radial_g(c.model, c.terms, at) / s <= bound + 1e-12);
        }
    }
    for (w, h) in ASPECTS {
        for hval in [-0.25, 0.0, 0.25] {
            for vval in [-0.25, 0.0, 0.25] {
                let s = cover_scale_perspective(w, h, (hval * 400.0) as i64, (vval * 400.0) as i64);
                let bound = local_scale_perspective_bound(w, h, hval, vval, s);
                for y in 0..=100 {
                    for x in 0..=100 {
                        let a = (f64::from(x) / 100.0 - 0.5) * 2.0 * f64::from(w)
                            / f64::from(w.max(h))
                            / s;
                        let b = (f64::from(y) / 100.0 - 0.5) * 2.0 * f64::from(h)
                            / f64::from(w.max(h))
                            / s;
                        let d = 1.0 - hval * a - vval * b;
                        let sv = singular(
                            (d + a * hval) / (d * d * s),
                            a * vval / (d * d * s),
                            b * hval / (d * d * s),
                            (d + b * vval) / (d * d * s),
                        );
                        assert!(sv <= bound + 1e-12);
                    }
                }
            }
        }
    }
    close(
        local_scale_perspective_bound(4000, 4000, 0.25, 0.25, 1.5),
        1.5,
        1e-12,
    );
}
// RMS error against a 16x16 footprint-integrated continuous test signal. The checkerboard
// exercises Nyquist detail; the zone plate sweeps spatial frequencies. This is a quality study,
// not a decision to change the owner-selected admission bound.
fn alias_energy(scale: f64, zone: bool) -> f64 {
    let signal = |x: f64, y: f64| {
        if zone {
            0.5 + 0.5 * (0.006 * (x * x + y * y)).sin()
        } else {
            0.5 + 0.5 * (std::f64::consts::PI * x).cos() * (std::f64::consts::PI * y).cos()
        }
    };
    let mut sum = 0.0;
    for y in 0..32 {
        for x in 0..32 {
            let px = (f64::from(x) + 0.5) * scale + 0.13;
            let py = (f64::from(y) + 0.5) * scale + 0.27;
            let ix = px.floor();
            let iy = py.floor();
            let dx = px - ix;
            let dy = py - iy;
            let filtered = (signal(ix, iy) * (1.0 - dx) + signal(ix + 1.0, iy) * dx) * (1.0 - dy)
                + (signal(ix, iy + 1.0) * (1.0 - dx) + signal(ix + 1.0, iy + 1.0) * dx) * dy;
            let mut ideal = 0.0;
            for by in 0..16 {
                for bx in 0..16 {
                    ideal += signal(
                        px + (f64::from(bx) + 0.5 - 8.0) * scale / 16.0,
                        py + (f64::from(by) + 0.5 - 8.0) * scale / 16.0,
                    ) / 256.0;
                }
            }
            sum += (filtered - ideal).powi(2);
        }
    }
    (sum / 1024.0).sqrt()
}
fn fixture() -> Value {
    let mappings:Vec<_>=NAMED.into_iter().map(|c|{let (ns,tight_cover,rmax)=stage(c,6048,4024);let cover=tight_cover*(1.0+1e-12);json!({"name":c.name,"source":c.source,"model":format!("{:?}",c.model),"terms":c.terms,"normalization":ns,"unit_scale":unit_scale(6048,4024,c.crop,c.crop,c.aspect),"tight_cover":tight_cover,"cover":cover,"local_scale_max":local_scale_lens(c.model,c.terms,rmax,cover),"read_windows":[reads_radial_rect([0.0,0.0,6048.0,4024.0],[3024.0,2012.0],ns,c.model,c.terms,cover),reads_radial_rect([100.0,700.0,2000.0,2500.0],[3024.0,2012.0],ns,c.model,c.terms,cover)],"point":radial_input([1200.5,3400.5],[3024.0,2012.0],ns,c.model,c.terms,cover)})}).collect();
    let nikon:Vec<_>=[24.0,26.0,30.0,35.0,42.0,50.0,60.0,70.0].into_iter().map(|f|{let terms=lensfun_interpolate(&NIKON_Z_24_70,f).unwrap();let ns=lensfun_norm(6048,4024,1.0,1.0,1.5);json!({"focal":f,"terms":terms,"cover":cover_scale_lens(Model::PtLens,terms,6048,4024,ns)*(1.0+1e-12)})}).collect();
    let perspectives:Vec<_>=ASPECTS.into_iter().map(|(w,h)|{let s=cover_scale_perspective(w,h,100,100)*(1.0+1e-12);json!({"width":w,"height":h,"horizontal":100,"vertical":100,"cover":s,"local_scale_max":local_scale_perspective_bound(w,h,0.25,0.25,s)})}).collect();
    let orientations:Vec<_>=[false,true].into_iter().flat_map(|mirror|(0..4).map(move|turns|json!({"mirror":mirror,"turns":turns,"carried":oriented([40.0,-25.0],mirror,turns)}))).collect();
    let alias:Vec<_>=(10..=26).map(|n|{let scale=f64::from(n)/10.0;json!({"local_scale":scale,"checkerboard_rms":alias_energy(scale,false),"zone_plate_rms":alias_energy(scale,true)})}).collect();
    json!({"interpretation":"lensfun-v1-distortion-edge-1","upstream_commit":"101c745e847a5de4a1e569a94368ce2027198598","stage":{"width":6048,"height":4024},"mappings":mappings,"nikon_focals":nikon,"perspective":perspectives,"orientations":orientations,"alias_energy":alias,"alias_method":"RMS against 16x16 continuous footprint integration; 32x32 output; linear-light signals"})
}
fn compare(a: &Value, b: &Value) {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            close(a, b, 1e-12 + 1e-12 * b.abs());
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b) {
                compare(a, b);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(a.len(), b.len());
            for (k, a) in a {
                compare(a, &b[k]);
            }
        }
        _ => assert_eq!(a, b),
    }
}
#[test]
fn committed_geometry_fixtures_are_current() {
    let committed: Value = serde_json::from_slice(&std::fs::read(fixture_path()).unwrap()).unwrap();
    compare(&committed, &fixture());
}
#[test]
#[ignore = "regenerates committed numerical fixtures"]
fn regenerate_committed_geometry_fixtures() {
    std::fs::create_dir_all(fixture_path().parent().unwrap()).unwrap();
    std::fs::write(
        fixture_path(),
        serde_json::to_string_pretty(&fixture()).unwrap() + "\n",
    )
    .unwrap();
}

#[test]
fn corner_only_reads_underestimate_wide_barrel_profile() {
    let c = NAMED[4];
    let (ns, s, _) = stage(c, 6048, 4024);
    let centre = [3024.0, 2012.0];
    let corners = [[0.0, 0.0], [6048.0, 0.0], [0.0, 4024.0], [6048.0, 4024.0]];
    let max_corner = corners
        .into_iter()
        .map(|p| radial_input(p, centre, ns, c.model, c.terms, s)[0])
        .fold(0.0, f64::max);
    let b = reads_radial_rect([0.0, 0.0, 6048.0, 4024.0], centre, ns, c.model, c.terms, s);
    assert!(b[2] - max_corner > 30.0);
}
#[test]
fn identity_bilinear_centres_are_exact_and_preserve_signed_headroom() {
    let frame = [
        [-0.2, 1.5, 0.5],
        [2.0, -1.0, 0.0],
        [0.1, 0.2, 0.3],
        [0.7, 0.8, 0.9],
    ];
    for y in 0..2 {
        for x in 0..2 {
            assert_eq!(
                bilinear_linear(&frame, 2, 2, [f64::from(x) + 0.5, f64::from(y) + 0.5]),
                frame[(y * 2 + x) as usize]
            );
        }
    }
}
