use super::*;

pub(super) fn source(layout: RawLayout, pixels: Vec<u16>, width: u32, height: u32) -> RawSource {
    let rect = RawRect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let metadata = RawMetadata {
        make: "Test".into(),
        model: "Linear sensor".into(),
        mode: RawMode::from_id("NikonZ6Lossless14").unwrap(),
        layout,
        sensor_width: width,
        sensor_height: height,
        active_area: rect,
        default_crop: rect,
        cfa_width: 0,
        cfa_height: 0,
        cfa: vec![],
        black_cfa: vec![],
        black_base: 128.0,
        black_channels: [0.0; 4],
        black_repeat_width: 0,
        black_repeat_height: 0,
        black_repeat: vec![],
        sensor_white: 1024.0,
        as_shot_gains: [1.0; 3],
        libraw_flip: 0,
        exif_orientation: 1,
        rgb_cam: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ],
        cam_xyz: [[0.0; 3]; 4],
        backend: "synthetic".into(),
        libraw_inset: None,
        format_identity: "synthetic-linear".into(),
        warnings: vec![],
        dng_corrections: None,
    };
    RawSource {
        metadata,
        mosaic: Arc::new(pixels),
        mosaic_corrections: Arc::new(vec![]),
        shape: develop::DemosaicShape::of(&RawSource::blank_native()),
        dng_correction: None,
    }
}

/// A CFA mosaic source of `pixels`, its frame, CFA, black model, white and camera matrix those of
/// the synthetic native metadata `native`.
pub(super) fn mosaic_source(pixels: Vec<u16>, native: &NativeMetadata) -> RawSource {
    let mut raw = source(RawLayout::Mosaic, pixels, native.width, native.height);
    let cfa = (native.cfa_width * native.cfa_height) as usize;
    let repeat = (native.black_repeat_width * native.black_repeat_height) as usize;
    let m = &mut raw.metadata;
    m.cfa_width = native.cfa_width as u8;
    m.cfa_height = native.cfa_height as u8;
    m.cfa = native.cfa[..cfa].to_vec();
    m.black_cfa = native.black_cfa[..cfa].to_vec();
    m.black_base = native.black_base;
    m.black_channels = native.black_channels;
    m.black_repeat_width = native.black_repeat_width as u8;
    m.black_repeat_height = native.black_repeat_height as u8;
    m.black_repeat = native.black_repeat[..repeat].to_vec();
    m.sensor_white = native.white;
    m.rgb_cam = std::array::from_fn(|row| std::array::from_fn(|col| native.rgb_cam[row * 4 + col]));
    raw.shape = develop::DemosaicShape::of(native);
    raw
}

#[test]
fn linear_rgb_preserves_signed_headroom_and_channel_order_without_demosaic() {
    let raw = source(
        RawLayout::LinearRgb,
        vec![0, 576, 1024, 128, 1024, 1472],
        2,
        1,
    );
    let cancel = AtomicBool::new(false);
    let rgb = raw.develop([2.0, 1.0, 0.5], &cancel).unwrap();
    // Planar output from interleaved integers, with black subtraction before WB.
    assert_eq!(rgb.data, vec![-256.0 / 896.0, 0.0, 0.5, 1.0, 0.5, 0.75]);
    let cloned = raw.clone();
    assert!(Arc::ptr_eq(&raw.mosaic, &cloned.mosaic));
    assert_eq!(raw.source_samples(), &[0, 576, 1024, 128, 1024, 1472]);
    assert!(matches!(
        raw.develop([1.0; 3], &AtomicBool::new(true)),
        Err(RawError::Cancelled)
    ));
}

#[test]
fn monochrome_repeats_one_plane_exactly_and_refuses_white_balance_and_picker() {
    let raw = source(RawLayout::Monochrome, vec![0, 128, 576, 1472], 2, 2);
    let rgb = raw.develop([1.0; 3], &AtomicBool::new(false)).unwrap();
    let expected = [-128.0 / 896.0, 0.0, 0.5, 1.5];
    assert_eq!(rgb.data, [expected, expected, expected].concat());
    assert!(matches!(
        raw.develop([1.01, 1.0, 1.0], &AtomicBool::new(false)),
        Err(RawError::InvalidInput(_))
    ));
    assert!(matches!(
        raw.neutral_gains_at(0, 0),
        Err(RawError::NeutralPatch(_))
    ));
}

#[test]
fn linear_picker_reads_all_channels_at_each_site_without_a_frame() {
    let raw = source(
        RawLayout::LinearRgb,
        [384, 640, 256].repeat(32 * 32),
        32,
        32,
    );
    assert_eq!(raw.neutral_gains_at(16, 16).unwrap(), [2.0, 1.0, 4.0]);
    assert!(raw.neutral_gains_at(0, 0).is_err());
    let clipped = source(
        RawLayout::LinearRgb,
        [384, 640, 1024].repeat(32 * 32),
        32,
        32,
    );
    assert!(clipped.neutral_gains_at(16, 16).is_err());
}
