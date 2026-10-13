//! The guided upsampler against its independent `f64` reference
//! (`luxforge_reference::guided`), bit for bit: the same window and the same codes from the same
//! prior, guide and settings, at the two output sizes a selection takes, with the guide at the
//! output's own resolution. And what the filter is for: an edge the prior placed only to within a
//! cell lands where the guide's edge is.

use luxforge_core::{
    Cancel,
    mask::guided::{self, Params, Plane},
};
use luxforge_reference::guided as reference;
use luxforge_test_macros::slow;

/// A deterministic synthetic case: a prior at `side / scale` that is a soft disc with a logistic
/// edge a few cells wide, and a guide at `side` whose disc is the same one with a sharp edge and a
/// fine texture over both sides of it.
fn case(side: u32, scale: u32) -> (Vec<f32>, u32, Vec<f32>) {
    let cells = side / scale;
    let centre = f64::from(side) * 0.45;
    let radius = f64::from(side) * 0.3;
    let prior: Vec<f32> = (0..cells * cells)
        .map(|k| {
            let (i, j) = (k % cells, k / cells);
            let x = (f64::from(i) + 0.5) * f64::from(scale);
            let y = (f64::from(j) + 0.5) * f64::from(scale);
            let d = ((x - centre).powi(2) + (y - centre * 1.1).powi(2)).sqrt() - radius;
            // A ramp two cells wide that saturates to exactly 0 and 1 away from the edge, as a
            // decided model's probabilities do, displaced half a cell from the true edge.
            (0.5 - (d + 0.5 * f64::from(scale)) / (2.0 * f64::from(scale))).clamp(0.0, 1.0) as f32
        })
        .collect();
    let guide: Vec<f32> = (0..side * side)
        .map(|k| {
            let (x, y) = (f64::from(k % side), f64::from(k / side));
            let d = ((x + 0.5 - centre).powi(2) + (y + 0.5 - centre * 1.1).powi(2)).sqrt() - radius;
            let base = if d < 0.0 { 0.8 } else { 0.25 };
            let texture = 0.03 * ((x * 0.37).sin() * (y * 0.23).cos());
            (base + texture) as f32
        })
        .collect();
    (prior, cells, guide)
}

fn both(
    side: u32,
    scale: u32,
    radius: Option<u32>,
) -> (guided::Upsampled, reference::Window, Vec<u8>) {
    let (prior, cells, guide) = case(side, scale);
    let mut params = Params::for_sizes((cells, cells), (side, side));
    if let Some(radius) = radius {
        params.radius = radius;
    }
    let production = guided::upsample(
        Plane {
            width: cells,
            height: cells,
            values: &prior,
        },
        Plane {
            width: side,
            height: side,
            values: &guide,
        },
        params,
        &Cancel::never(),
    )
    .expect("a well-formed case")
    .expect("a non-zero prior");
    let (window, codes) = reference::upsample(
        &reference::Plane {
            width: cells,
            height: cells,
            values: &prior,
        },
        &reference::Plane {
            width: side,
            height: side,
            values: &guide,
        },
        params.radius,
        params.eps,
    )
    .expect("a non-zero prior");
    (production, window, codes)
}

fn assert_identical(side: u32, scale: u32) {
    let (production, window, codes) = both(side, scale, None);
    assert_eq!(
        (
            production.window.x,
            production.window.y,
            production.window.width,
            production.window.height
        ),
        (window.x, window.y, window.width, window.height),
        "{side} px: the window"
    );
    if let Some(at) = production
        .codes
        .iter()
        .zip(&codes)
        .position(|(ours, theirs)| ours != theirs)
    {
        panic!(
            "{side} px: code {at} is {} against the reference's {}",
            production.codes[at], codes[at]
        );
    }
    assert_eq!(production.codes.len(), codes.len());
    // Not vacuous: the case has a soft edge, a full inside and a zero outside.
    assert!(codes.iter().any(|&code| code == 255));
    assert!(codes.iter().any(|&code| code == 0));
    assert!(codes.iter().any(|&code| code > 0 && code < 255));
}

#[test]
fn the_upsampler_is_bit_identical_to_its_reference_at_1024_px() {
    assert_identical(1024, 4);
}

#[test]
#[slow]
fn the_upsampler_is_bit_identical_to_its_reference_at_4096_px() {
    assert_identical(4096, 4);
}

/// The prior's edge is a ramp two cells wide, half a cell off the true edge; the guide's is one
/// pixel. Along the disc's horizontal diameter the upsampled coverage crosses half within two pixels
/// of the guide's edge, and falls there in one step at least four times the steepest step of the
/// prior's own enlarged ramp. The filter sharpens an edge whose ramp its box spans, so the radius
/// here is four cells.
#[test]
fn the_edge_follows_the_guide_rather_than_the_prior() {
    let side = 1024u32;
    let scale = 4u32;
    let (production, ..) = both(side, scale, Some(4 * scale));
    let raster = production.into_raster(side, side);
    let centre = f64::from(side) * 0.45;
    let radius = f64::from(side) * 0.3;
    let row = (centre * 1.1) as u32;
    let edge = centre + radius;
    let line = &raster[(row * side) as usize..((row + 1) * side) as usize];
    let crossing = (centre as usize..side as usize)
        .find(|&x| line[x] < 128)
        .expect("the coverage falls past the edge") as f64;
    assert!(
        (crossing - edge).abs() <= 2.0,
        "half coverage at {crossing}, the guide's edge at {edge}"
    );
    let steepest = |codes: &[u8]| {
        codes
            .windows(2)
            .skip(centre as usize)
            .map(|pair| i32::from(pair[0]) - i32::from(pair[1]))
            .max()
            .unwrap_or(0)
    };
    // The prior alone, enlarged bilinearly to the same row.
    let (prior, cells, guide) = case(side, scale);
    let prior_plane = reference::Plane {
        width: cells,
        height: cells,
        values: &prior,
    };
    let guide_plane = reference::Plane {
        width: side,
        height: side,
        values: &guide,
    };
    let enlarged: Vec<u8> = (0..side)
        .map(|x| {
            let p = reference::prior_sample(
                &prior_plane,
                &guide_plane,
                f64::from(x) + 0.5,
                f64::from(row) + 0.5,
            );
            (p * 255.0).round() as u8
        })
        .collect();
    let (guided, prior) = (steepest(line), steepest(&enlarged));
    assert!(
        guided >= 4 * prior,
        "the guided edge falls {guided} codes in one pixel against the prior's {prior}"
    );
}
