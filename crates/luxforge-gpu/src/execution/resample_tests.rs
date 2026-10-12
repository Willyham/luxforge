//! The resample sweep's cuts and passes on their own: each output pixel in exactly one cut, every
//! window within its bounds, and the passes' sources valid.
use super::*;

/// The taps of a reduction from `from` to `to` pixels with a support of `reach` stage pixels on
/// either side of each centre, each weight the same: only their placement matters to the cuts.
fn taps(from: u32, to: u32, reach: u32) -> Vec<ResampleTaps> {
    (0..to)
        .map(|index| {
            let centre =
                ((u64::from(index) * 2 + 1) * u64::from(from) / (2 * u64::from(to))) as u32;
            let first = centre.saturating_sub(reach);
            let end = (centre + reach + 1).min(from);
            ResampleTaps {
                first,
                weights: vec![1.0 / (end - first) as f32; (end - first) as usize],
            }
        })
        .collect()
}

/// Every output pixel lies in exactly one cut; cuts go band by band, top to bottom and left to
/// right; a band holds at most [`RESAMPLE_ROWS`] rows; and each window is within the largest side
/// and [`RESAMPLE_SCRATCH_TEXELS`].
#[test]
fn resample_cuts_cover_the_output_once_within_their_bounds() {
    for (stage, output, reach, limit) in [
        ((9504u32, 6336u32), (9504u32, 6336u32), 0u32, 16384u32),
        ((9504, 6336), (2048, 1365), 7, 16384),
        ((6000, 4000), (60, 40), 300, 8192),
        ((6000, 4000), (20, 13), 900, 8192),
        ((3000, 2000), (30, 1), 300, 8192),
        ((1, 1), (1, 1), 0, 8192),
        ((20000, 300), (19000, 299), 3, 16384),
    ] {
        let columns = taps(stage.0, output.0, reach);
        let rows = taps(stage.1, output.1, reach);
        let cuts = resample_cuts(&columns, &rows, limit).unwrap();
        let mut seen = vec![0u8; output.0 as usize * output.1 as usize];
        let mut last = (0u32, 0u32);
        for cut in &cuts {
            assert!(
                (cut.rows.start, cut.columns.start) >= last,
                "{stage:?} -> {output:?}: in order"
            );
            last = (cut.rows.start, cut.columns.start);
            assert!(cut.rows.len() <= RESAMPLE_ROWS as usize);
            let window = ResampleCut {
                columns: &columns[cut.columns.start as usize..cut.columns.end as usize],
                rows: &rows[cut.rows.start as usize..cut.rows.end as usize],
                depth: ResampleDepth::Sixteen,
            }
            .window()
            .unwrap();
            let side = limit.min(RESAMPLE_SIDE);
            assert!(window[2] <= side && window[3] <= side, "{window:?}");
            assert!(
                u64::from(window[2]) * u64::from(window[3]) <= RESAMPLE_SCRATCH_TEXELS,
                "{window:?}"
            );
            for y in cut.rows.clone() {
                for x in cut.columns.clone() {
                    seen[(y * output.0 + x) as usize] += 1;
                }
            }
        }
        assert!(
            seen.iter().all(|&count| count == 1),
            "{stage:?} -> {output:?}"
        );
    }
}

/// An output pixel whose taps reach past the largest side of a window is refused, as is an empty
/// output.
#[test]
fn resample_cuts_refuse_taps_no_window_holds() {
    let columns = taps(20000, 2, 6000);
    let rows = taps(10, 10, 0);
    assert!(matches!(
        resample_cuts(&columns, &rows, 16384),
        Err(GpuFallback::TextureLimit { .. })
    ));
    assert!(matches!(
        resample_cuts(&[], &rows, 16384),
        Err(GpuFallback::TextureLimit { .. })
    ));
}

/// The 16-bit thresholds rise strictly from just past 0 to just below 1, and the passes' sources
/// at both depths are valid WGSL.
#[test]
fn resample_passes_and_thresholds_are_valid() {
    let wide = wide_encoding().expect("the test reference's thresholds");
    assert_eq!(wide.len(), WIDE_THRESHOLDS);
    assert!(wide.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(wide[0] > 0.0 && wide[WIDE_THRESHOLDS - 1] < 1.0);
    super::super::validate(across_source()).unwrap();
    for depth in [ResampleDepth::Eight, ResampleDepth::Sixteen] {
        super::super::validate(&down_source(depth).unwrap()).unwrap();
    }
    let (words, weights) = packed(
        &[
            ResampleTaps {
                first: 10,
                weights: vec![0.25, 0.75],
            },
            ResampleTaps {
                first: 11,
                weights: vec![1.0],
            },
        ],
        10,
    );
    assert_eq!(words, [0, 2, 0, 1, 1, 2]);
    assert_eq!(weights, [0.25, 0.75, 1.0]);
}
