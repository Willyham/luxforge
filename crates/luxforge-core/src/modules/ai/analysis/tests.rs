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
