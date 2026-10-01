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
        detect(&index, &input).reasons,
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
        detect(&index, &input).reasons,
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
/// Every text match that fits the camera, counted independently of `search`'s own filter.
fn compatible_matches(index: &LensIndex, input: &ResolveInput, text: &str) -> usize {
    let needle = normalize(text);
    let cameras = cameras(index, input);
    index
        .lenses
        .iter()
        .filter(|lens| {
            normalize(&lens.maker).contains(&needle)
                || lens.models.iter().any(|m| normalize(m).contains(&needle))
        })
        .filter(|lens| {
            cameras.iter().any(|camera| {
                reasons(index, camera, lens, input)
                    .iter()
                    .all(|r| r.photo_level())
            })
        })
        .count()
}

#[test]
fn empty_search_lists_nothing_and_text_lists_only_compatible_profiles_identity_first() {
    let index = index();
    let input = input("NIKKOR Z 24-70mm f/4 S", 35.0);
    let empty = search(&index, &input, "  ", 0);
    assert!(empty.rows.is_empty());
    assert_eq!((empty.total, empty.pages), (0, 1));
    let page = search(&index, &input, "nikkor z", 0);
    assert!(!page.rows.is_empty());
    assert_eq!(page.total, compatible_matches(&index, &input, "nikkor z"));
    assert_eq!(page.rows[0].matched, Match::LensModel);
    assert_eq!(page.rows[0].title, "NIKKOR Z 24-70mm f/4 S");
    assert!(page.rows[1..].iter().all(|r| r.matched == Match::Search));
    // Lens-level reasons hide a profile; only the photo's own conditions may remain on a row.
    assert!(
        page.rows
            .iter()
            .all(|r| r.reasons.iter().all(|reason| reason.photo_level())),
        "{:?}",
        page.rows
    );
    // Many database profiles name "Canon"; none fits a Z 6 at 35 mm.
    assert!(index.lenses.iter().any(|l| l.maker.contains("Canon")));
    let other_mount = search(&index, &input, "Canon", 0);
    assert_eq!(
        other_mount.total,
        compatible_matches(&index, &input, "Canon")
    );
    assert!(other_mount.rows.iter().all(|row| {
        !row.reasons.contains(&Reason::IncompatibleMount)
            && !row.reasons.contains(&Reason::FocalOutOfRange)
    }));
}

#[test]
fn search_pages_compatible_profiles_without_overlap() {
    let index = index();
    let mut input = input("NIKKOR Z 24-70mm f/4 S", 35.0);
    // Without a focal length nothing is out of range, so many profiles stay compatible.
    input.focal_mm = None;
    input.focal_35mm = None;
    let total = compatible_matches(&index, &input, "nikkor");
    assert!(total > PAGE_ROWS, "{total}");
    let first = search(&index, &input, "nikkor", 0);
    let next = search(&index, &input, "nikkor", 1);
    assert_eq!(first.rows.len(), PAGE_ROWS);
    assert_eq!(first.total, total);
    assert_eq!(first.pages, total.div_ceil(PAGE_ROWS));
    assert!(
        first
            .rows
            .iter()
            .all(|r| !next.rows.iter().any(|n| n.key == r.key))
    );
    // The photo's missing focal length gates selection without hiding a profile.
    assert!(
        first
            .rows
            .iter()
            .all(|r| !r.eligible && r.reasons == [Reason::FocalMissing])
    );
}

#[test]
fn a_camera_outside_the_database_lists_no_search_rows() {
    let index = index();
    let mut input = dji();
    input.make = "FUJIFILM".into();
    input.model = "X100VI".into();
    assert!(search(&index, &input, "X100", 0).rows.is_empty());
    let detection = detect(&index, &input);
    assert!(detection.camera.is_none() && detection.detected.is_none());
    assert_eq!(detection.reasons, [Reason::CameraNotInDatabase]);
}

#[test]
fn detection_names_the_first_eligible_identity_match_or_what_holds_it_back() {
    let index = index();
    let z6 = input("NIKKOR Z 24-70mm f/4 S", 35.0);
    let detection = detect(&index, &z6);
    let detected = detection.detected.unwrap();
    assert!(detected.eligible && detected.matched == Match::LensModel);
    assert_eq!(detected.key, key(&index, &z6));
    assert!(!detection.fixed && detection.reasons.is_empty() && detection.photo.is_empty());

    let fc3411 = detect(&index, &dji());
    assert!(fc3411.fixed);
    let detected = fc3411.detected.unwrap();
    assert_eq!(detected.title, "FC3411 & compatibles");
    assert!(detected.eligible && detected.matched == Match::CameraMount);

    let mut missing = z6.clone();
    missing.focal_mm = None;
    missing.focal_35mm = None;
    let detection = detect(&index, &missing);
    assert_eq!(detection.detected.unwrap().reasons, [Reason::FocalMissing]);
    assert_eq!(detection.photo, [Reason::FocalMissing]);

    let mut beyond = z6.clone();
    beyond.focal_mm = Some(100.0);
    beyond.focal_35mm = Some(100);
    let detection = detect(&index, &beyond);
    assert_eq!(
        detection.detected.unwrap().reasons,
        [Reason::FocalOutOfRange],
        "the detected lens is kept so its focal length can be supplied"
    );

    let mut dx = z6.clone();
    dx.focal_35mm = Some(53);
    let detection = detect(&index, &dx);
    assert_eq!(detection.photo, [Reason::CropModeMismatch]);
    assert!(detection.detected.is_some());

    let mut portrait = z6.clone();
    portrait.stage.height = 6048;
    let detection = detect(&index, &portrait);
    assert!(
        detection.detected.is_none(),
        "an aspect mismatch does not fit"
    );
    assert_eq!(detection.candidates.len(), 1);
    assert_eq!(detection.reasons, [Reason::AspectMismatch]);

    let unknown = input("NIKKOR Z 99mm f/9 Imaginary", 35.0);
    let detection = detect(&index, &unknown);
    assert!(detection.camera.is_some() && detection.candidates.is_empty());
    assert_eq!(detection.reasons, [Reason::LensNotInDatabase]);
}

#[test]
fn selecting_a_profile_search_never_lists_still_refuses_with_its_reasons() {
    let index = index();
    let input = input("NIKKOR Z 24-70mm f/4 S", 35.0);
    // A profile on another mount that covers 35 mm: refused for its mount alone, not its focal.
    let canon = index
        .lenses
        .iter()
        .find(|lens| {
            let reasons = candidate(
                &index,
                lens,
                &input,
                Match::Search,
                &cameras(&index, &input),
            )
            .reasons;
            !fits(&index, lens, &input, &cameras(&index, &input))
                && reasons.contains(&Reason::IncompatibleMount)
                && !reasons.contains(&Reason::FocalOutOfRange)
        })
        .unwrap();
    let error = resolve(&index, &canon.key, &input).unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::UnsupportedInput);
    let data = error.data.unwrap();
    assert!(
        data["reasons"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("incompatible-mount")),
        "{data}"
    );
}
