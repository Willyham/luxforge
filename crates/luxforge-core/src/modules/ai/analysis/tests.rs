use super::*;

#[test]
fn reduced_sizes_keep_small_stages_and_scale_the_long_side() {
    assert_eq!(reduced_size((800, 600), ANALYSIS_SIDE), (800, 600));
    assert_eq!(reduced_size((6000, 4000), ANALYSIS_SIDE), (1024, 683));
    assert_eq!(reduced_size((4000, 6000), GUIDE_SIDE), (2731, 4096));
    assert_eq!(reduced_size((5000, 1), ANALYSIS_SIDE), (1024, 1));
}

/// A stage whose every pixel is its column's and row's value, read band by band.
fn reduce_ramp(stage: (u32, u32)) -> (Raster, Guide, Vec<Region>) {
    let mut reads = Vec::new();
    let (raster, guide) = reduce(
        stage,
        &mut |region: Region| {
            reads.push(region);
            Ok((region.y0..region.y0 + region.height)
                .flat_map(|y| {
                    (0..region.width).map(move |x| {
                        let v = if (x / 2 + y / 2) % 2 == 0 { 1.0 } else { 0.0 };
                        [v, v, v]
                    })
                })
                .collect())
        },
        &|| Ok(()),
    )
    .unwrap();
    (raster, guide, reads)
}

#[test]
fn reduction_averages_each_block_in_linear_light() {
    // A 2-pixel checker of white and black averaged over 2 x 2 blocks is solid; over 4 x 4 blocks
    // it is mid-grey in linear light, sRGB code 188.
    let (raster, guide, reads) = reduce_ramp((4096, 2048));
    assert_eq!((raster.width, raster.height), (1024, 512));
    assert_eq!((guide.width, guide.height), (4096, 2048));
    assert!(
        raster.rgb.iter().all(|&code| code == 188),
        "{:?}",
        &raster.rgb[..6]
    );
    // The guide is the stage itself: its luminance, unaveraged.
    assert_eq!(&guide.codes[..4], &[255, 255, 0, 0]);
    // Whole rows, each band within the module read bound, covering the stage once.
    assert!(reads.iter().all(|r| r.x0 == 0 && r.width == 4096));
    assert!(reads.iter().all(|r| r.pixels() <= MAX_MODULE_READ_PIXELS));
    assert_eq!(reads.iter().map(|r| r.height).sum::<u32>(), 2048);
}

#[test]
fn a_short_read_is_refused() {
    let error = reduce((16, 16), &mut |_| Ok(vec![[0.0; 3]; 3]), &|| Ok(())).unwrap_err();
    assert_eq!(error.kind.code(), "internal");
}

#[test]
fn targets_hold_their_codes_and_one_row_of_sums_never_a_float_frame() {
    // The guide at its largest is one byte a pixel, 16 MiB, beside one row of sums.
    let stage = (12_000, 8_000);
    let size = reduced_size((12_000, 12_000), GUIDE_SIDE);
    let guide = Target::new((12_000, 12_000), size, Channels::Luma);
    assert_eq!(guide.out.capacity(), 16 << 20);
    assert_eq!(guide.sums.len(), 4096);
    let input = Target::new(stage, reduced_size(stage, ANALYSIS_SIDE), Channels::Rgb);
    assert_eq!(input.out.capacity(), 1024 * 683 * 3);
    assert_eq!(input.sums.len(), 1024 * 3);
    // Fed a whole stage, the codes fill the allocation made up front and nothing more.
    let mut target = Target::new((64, 48), (16, 12), Channels::Luma);
    for y in 0..48 {
        target.add(y, &[[0.5; 3]; 64]);
    }
    let codes = target.finish();
    assert_eq!((codes.len(), codes.capacity()), (16 * 12, 16 * 12));
}
