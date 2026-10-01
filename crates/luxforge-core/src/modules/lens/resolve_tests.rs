use super::*;
use std::fs;
fn index() -> LensIndex {
    LensIndex::parse(
        &fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/lensfun/index.json"
        ))
        .unwrap(),
    )
    .unwrap()
}
fn input(lens: &str, focal: f64) -> ResolveInput {
    ResolveInput {
        make: "NIKON CORPORATION".into(),
        model: "NIKON Z 6".into(),
        lens_model: Some(lens.into()),
        focal_mm: Some(focal),
        focal_35mm: Some(focal as u16),
        focal_override: None,
        stage: Stage {
            width: 6048,
            height: 4024,
        },
    }
}
fn key(index: &LensIndex, input: &ResolveInput) -> String {
    candidates(index, input)
        .into_iter()
        .find(|c| c.eligible)
        .unwrap()
        .key
}
#[test]
fn z6_nef_matches_lensfun_camera_by_exif_strings() {
    let index = index();
    let mut input = input("NIKKOR Z 24-70mm f/4 S", 35.0);
    assert_eq!(candidates(&index, &input).len(), 1);
    input.make = "Nikon".into();
    input.model = "Z 6".into();
    assert!(candidates(&index, &input).is_empty());
    assert_eq!(
        status_reasons(&index, &input),
        vec![Reason::CameraNotInDatabase]
    );
    assert_eq!(normalize("  NIKON\t CORPORATION\n"), "nikon corporation");
    assert_eq!(normalize("Ä  Σ"), "ä σ");
}
#[test]
fn z6_24_200_at_200mm_resolves_exact_calibration() {
    let index = index();
    let input = input("NIKKOR Z 24-200mm f/4-6.3 VR", 200.0);
    let resolution = resolve(&index, &key(&index, &input), &input).unwrap();
    let lens = index
        .lenses
        .iter()
        .find(|l| l.key == resolution.key)
        .unwrap();
    assert_eq!(
        resolution.terms,
        lens.calibrations
            .iter()
            .find(|c| c.focal_mm == 200.0)
            .unwrap()
            .terms
    );
}
fn dji() -> ResolveInput {
    ResolveInput {
        make: "DJI".into(),
        model: "FC3411".into(),
        lens_model: None,
        focal_mm: Some(8.38),
        focal_35mm: Some(22),
        focal_override: None,
        stage: Stage {
            width: 5472,
            height: 3648,
        },
    }
}
#[test]
fn fc3411_candidates_come_from_camera_mount() {
    let index = index();
    let rows = candidates(&index, &dji());
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|r| r.matched == Match::CameraMount));
    assert!(rows.iter().any(|r| r.eligible));
}
#[test]
fn fc3411_focal_8_38_snaps_to_8_4() {
    let index = index();
    let input = dji();
    let key = key(&index, &input);
    let lens = index.lenses.iter().find(|l| l.key == key).unwrap();
    assert_eq!(lens.calibrations[0].focal_mm, 8.4);
    assert_eq!(
        resolve(&index, &key, &input).unwrap().terms,
        lens.calibrations[0].terms
    );
}
#[test]
fn x100vi_is_camera_not_in_database() {
    let index = index();
    let mut input = dji();
    input.make = "FUJIFILM".into();
    input.model = "X100VI".into();
    assert_eq!(
        status_reasons(&index, &input),
        vec![Reason::CameraNotInDatabase]
    );
}
#[test]
fn duplicate_model_records_are_distinct_candidates_filtered_by_crop() {
    let index = index();
    let mut input = input("FE 24-70mm f/4 ZA OSS", 24.0);
    input.make = "SONY".into();
    input.model = "ILCE-7RM4".into();
    let rows = candidates(&index, &input);
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].key, rows[1].key);
    assert!(rows.iter().any(|r| r.crop_factor == 1.0 && r.eligible));
    assert!(
        rows.iter().any(
            |r| r.crop_factor == 1.534 && r.reasons.contains(&Reason::CalibrationSensorSmaller)
        )
    );
}
#[test]
fn dx_crop_mode_metadata_refuses_full_frame_calibration() {
    let index = index();
    let mut input = input("NIKKOR Z 24-70mm f/4 S", 24.0);
    input.focal_35mm = Some(36);
    assert!(
        candidates(&index, &input)[0]
            .reasons
            .contains(&Reason::CropModeMismatch)
    );
}
#[test]
fn aspect_mismatch_is_refused() {
    let index = index();
    let mut input = input("NIKKOR Z 24-70mm f/4 S", 24.0);
    input.stage.height = 6048;
    assert!(
        candidates(&index, &input)[0]
            .reasons
            .contains(&Reason::AspectMismatch)
    );
}
#[test]
fn prime_focal_within_one_percent_snaps_to_calibration() {
    let calibration = [Calibration {
        focal_mm: 8.4,
        terms: [0.01, 0.0, 0.0],
    }];
    assert_eq!(
        interpolate(&calibration, 8.38).unwrap(),
        calibration[0].terms
    );
    assert_eq!(
        interpolate(&calibration, 8.4 * 0.99).unwrap(),
        calibration[0].terms
    );
    assert_eq!(interpolate(&calibration, 8.6), Err(Reason::FocalOutOfRange));
}
#[test]
fn focal_beyond_calibrations_refuses_without_extrapolation() {
    let index = index();
    let mut input = input("NIKKOR Z 24-70mm f/4 S", 24.0);
    let key = key(&index, &input);
    input.focal_mm = Some(100.0);
    input.focal_35mm = Some(100);
    let error = resolve(&index, &key, &input).unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Validation);
    let data = error.data.unwrap();
    assert_eq!(data["reason"], "focal-out-of-range");
    assert_eq!(data["focal"], 100.0);
    assert_eq!(data["min"], 24.0);
    assert_eq!(data["max"], 70.0);
}
#[test]
fn focal_override_is_recorded() {
    let index = index();
    let mut input = input("NIKKOR Z 24-70mm f/4 S", 35.0);
    input.focal_mm = None;
    input.focal_35mm = None;
    input.focal_override = Some(35.0);
    assert_eq!(
        resolve(&index, &key(&index, &input), &input)
            .unwrap()
            .focal_source,
        FocalSource::Override
    );
}
#[test]
fn lensfun_interpolation_matches_reference_vectors() {
    let index = index();
    let input = input("NIKKOR Z 24-70mm f/4 S", 35.0);
    let key = key(&index, &input);
    let lens = index.lenses.iter().find(|l| l.key == key).unwrap();
    let fixture: serde_json::Value = serde_json::from_slice(
        &fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/geometry/lens-perspective.json"
        ))
        .unwrap(),
    )
    .unwrap();
    for vector in fixture["nikon_focals"].as_array().unwrap() {
        let f = vector["focal"].as_f64().unwrap();
        let actual = interpolate(&lens.calibrations, f).unwrap();
        for (i, term) in actual.iter().enumerate() {
            let expected = vector["terms"][i].as_f64().unwrap();
            assert!(
                (term - expected).abs() <= expected.abs() * 1e-12 + 1e-15,
                "{f}: {actual:?}"
            );
        }
    }
}
#[test]
fn unit_scale_reproduces_lensfun_normalization() {
    let index = index();
    let input = input("NIKKOR Z 24-70mm f/4 S", 35.0);
    let resolved = resolve(&index, &key(&index, &input), &input).unwrap();
    let wm = 6047.0_f64;
    let hm = 4023.0_f64;
    let expected = 2.0 * ((1.5_f64.powi(2) + 1.0).sqrt() / ((wm / hm).powi(2) + 1.0).sqrt()) / hm;
    assert!((2.0 * resolved.unit_scale / 4024.0 / expected - 1.0).abs() < 1e-12);
}
#[test]
fn search_pages_candidates_first_then_text_matches() {
    let index = index();
    let input = input("NIKKOR Z 24-70mm f/4 S", 35.0);
    let first = search(&index, &input, "", 0);
    assert_eq!(first.rows.len(), 50);
    assert_eq!(first.rows[0].matched, Match::LensModel);
    assert!(first.rows[1..].iter().all(|r| r.matched == Match::Search));
    let next = search(&index, &input, "", 1);
    assert_eq!(next.rows.len(), 50);
    assert!(next.rows.iter().all(|r| r.matched == Match::Search));
    assert!(
        first
            .rows
            .iter()
            .all(|r| !next.rows.iter().any(|n| n.key == r.key))
    );
}
