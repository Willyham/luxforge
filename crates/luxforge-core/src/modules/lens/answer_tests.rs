//! The lens-profiles answer in each panel state, from synthetic optics against the committed index.
use super::*;
use crate::{
    OpticalIdentity, ToolModule,
    modules::{Stage, lens::LensModule},
};
use luxforge_raw::{OpticalEntry, OpticalLedger};
use std::sync::OnceLock;

fn index() -> &'static LensIndex {
    static INDEX: OnceLock<LensIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        LensIndex::parse(
            &std::fs::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/data/lensfun/index.json"
            ))
            .unwrap(),
        )
        .unwrap()
    })
}

fn entry(status: OpticalStatus, provenance: &str) -> OpticalEntry {
    OpticalEntry {
        status,
        provenance: provenance.into(),
    }
}

/// The supplied Air 2S DNG's ledger: required warp and gain map, distortion left to a profile.
fn dng_ledger() -> OpticalLedger {
    OpticalLedger {
        distortion: entry(OpticalStatus::KnownUnapplied, "raw-mosaic"),
        lateral_ca: entry(
            OpticalStatus::Applied,
            "dng-opcode:list3:WarpRectilinear:planes=R,B",
        ),
        shading: entry(OpticalStatus::Applied, "dng-opcode:list3:GainMap"),
        interpretation: "fc3411-stage3-active-v1-unclipped-bicubic-a-0.75".into(),
    }
}

struct Photo {
    optics: SourceOptics,
    input: ResolveInput,
}

fn photo(
    make: &str,
    model: &str,
    lens: Option<&str>,
    focal: Option<f64>,
    focal_35mm: Option<u16>,
    ledger: OpticalLedger,
    stage: (u32, u32),
) -> Photo {
    let identity = OpticalIdentity {
        make: Some(make.into()),
        model: Some(model.into()),
        lens_make: None,
        lens_model: lens.map(str::to_owned),
        focal_mm: focal,
        focal_35mm,
    };
    let input = ResolveInput {
        make: make.into(),
        model: model.into(),
        lens_model: identity.lens_model.clone(),
        focal_mm: focal,
        focal_35mm,
        focal_override: None,
        stage: Stage {
            width: stage.0,
            height: stage.1,
        },
    };
    Photo {
        optics: SourceOptics { identity, ledger },
        input,
    }
}

fn air2s() -> Photo {
    photo(
        "DJI",
        "FC3411",
        None,
        Some(8.38),
        Some(22),
        dng_ledger(),
        (5472, 3648),
    )
}

fn z6_jpeg(lens: &str, focal: Option<f64>) -> Photo {
    photo(
        "NIKON CORPORATION",
        "NIKON Z 6",
        Some(lens),
        focal,
        focal.map(|f| f as u16),
        SourceOptics::jpeg_ledger(),
        (6048, 4024),
    )
}

fn ask(photo: &Photo, applied: Option<&payload::Profile>, text: &str, assume: bool) -> Value {
    let answer = answer(
        index(),
        &photo.optics,
        &photo.input,
        applied,
        &Request {
            text,
            page: 0,
            assume,
        },
    );
    // Every state is a valid query-choice answer at the host boundary.
    LensModule::new()
        .descriptor()
        .validate_query_choice_answer("lens-profiles", &answer)
        .unwrap();
    answer
}

#[test]
fn air2s_dng_detects_its_built_in_lens_by_product_name_with_nothing_to_acknowledge() {
    let answer = ask(&air2s(), None, "", false);
    let status = &answer["status"];
    assert_eq!(status["state"], "detected");
    assert_eq!(status["camera_name"], "DJI Air 2S (FC3411)");
    assert_eq!(status["product"], "DJI Air 2S");
    assert_eq!(status["fixed_mount"], true);
    assert_eq!(status["search"], true);
    let card = &status["suggestion"];
    assert_eq!(card["title"], "DJI Air 2S (FC3411) · built-in lens");
    assert_eq!(card["subtitle"], "FC3411 & compatibles · 8.38 mm");
    assert_eq!(card["eligible"], true);
    assert_eq!(card["label"], "Apply");
    assert!(card.get("parameters").is_none() && card.get("note").is_none());
    assert!(status.get("notice").is_none() && status.get("report").is_none());
    assert!(status.get("current").is_none());
    assert_eq!(
        status["summary"],
        "In camera: distortion not corrected · lateral CA corrected · shading corrected"
    );
    assert_eq!(answer["rows"], json!([]));
    assert_eq!(answer["total"], 0);
}

#[test]
fn an_applied_profile_is_one_card_and_hides_the_suggestion() {
    let photo = air2s();
    let detected = resolve::detect(index(), &photo.input).detected.unwrap();
    let resolution = resolve::resolve(index(), &detected.key, &photo.input).unwrap();
    let profile = payload::Profile::from_resolution(resolution, &photo.optics, false).unwrap();
    let answer = ask(&photo, Some(&profile), "", false);
    let status = &answer["status"];
    assert_eq!(status["state"], "applied");
    let card = &status["current"];
    assert_eq!(card["key"], detected.key);
    assert_eq!(card["title"], "DJI Air 2S (FC3411) · built-in lens");
    assert_eq!(card["subtitle"], "FC3411 & compatibles · 8.38 mm");
    assert!(card.get("note").is_none());
    assert!(status.get("suggestion").is_none() && status.get("notice").is_none());
    // Change searches only compatible profiles; nonsense finds none and offers the report.
    let searched = ask(&photo, Some(&profile), "FC3411", false);
    assert_eq!(searched["rows"][0]["key"], detected.key);
    let nothing = ask(&photo, Some(&profile), "Summilux", false);
    assert_eq!(nothing["rows"], json!([]));
    assert_eq!(
        nothing["status"]["notice"],
        json!({"level":"warning","text":"No compatible profile matches “Summilux”."})
    );
    assert_eq!(nothing["status"]["report"]["label"], "Report missing lens");
}

#[test]
fn a_jpeg_offers_its_detected_lens_with_the_acknowledgement_its_apply_sends() {
    let photo = z6_jpeg("NIKKOR Z 24-70mm f/4 S", Some(35.0));
    let answer = ask(&photo, None, "", false);
    let status = &answer["status"];
    assert_eq!(status["state"], "detected");
    let card = &status["suggestion"];
    assert_eq!(card["title"], "NIKKOR Z 24-70mm f/4 S");
    assert_eq!(card["subtitle"], "Nikon Z 6 · 35 mm");
    assert_eq!(card["eligible"], true);
    assert_eq!(card["parameters"], json!({"assume-uncorrected": true}));
    assert!(
        card["note"]
            .as_str()
            .unwrap()
            .contains("assumes it did not")
    );
    assert!(
        status["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("assume-uncorrected-required"))
    );
    assert_eq!(status["visible_shared"], json!([]));
    let acknowledged = ask(&photo, None, "", true);
    assert_eq!(acknowledged["status"]["reasons"], json!([]));
    let searched = ask(&photo, None, "NIKKOR Z 24-70", false);
    let rows = searched["rows"].as_array().unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| {
        row["eligible"] == true && row["parameters"] == json!({"assume-uncorrected": true})
    }));
}

#[test]
fn a_camera_outside_the_database_warns_and_reports_without_a_search() {
    let photo = photo(
        "FUJIFILM",
        "X100VI",
        None,
        Some(23.0),
        Some(35),
        SourceOptics::jpeg_ledger(),
        (7728, 5152),
    );
    for text in ["", "X100"] {
        let answer = ask(&photo, None, text, false);
        let status = &answer["status"];
        assert_eq!(status["state"], "camera-not-found");
        assert_eq!(status["search"], false);
        assert_eq!(
            status["notice"],
            json!({"level":"warning","text":"FUJIFILM X100VI is not in the lens database."})
        );
        assert!(
            status["report"]["url"]
                .as_str()
                .unwrap()
                .starts_with(report::NEW_ISSUE_URL)
        );
        assert!(status.get("suggestion").is_none());
        assert_eq!(status["visible_shared"], json!([]));
        assert_eq!(answer["rows"], json!([]));
    }
}

#[test]
fn an_unmatched_lens_warns_offers_search_and_reports() {
    let answer = ask(
        &z6_jpeg("NIKKOR Z 99mm f/9 Imaginary", Some(35.0)),
        None,
        "",
        false,
    );
    let status = &answer["status"];
    assert_eq!(status["state"], "lens-not-found");
    assert_eq!(status["search"], true);
    assert_eq!(
        status["notice"]["text"],
        "No profile matches NIKKOR Z 99mm f/9 Imaginary."
    );
    assert!(status["report"]["url"].is_string());
    assert!(
        status["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("lens-not-in-database"))
    );
}

#[test]
fn an_already_corrected_source_says_so_and_offers_nothing() {
    let mut ledger = dng_ledger();
    ledger.distortion = entry(OpticalStatus::Applied, "dng-opcode:list3:WarpRectilinear");
    let mut photo = air2s();
    photo.optics.ledger = ledger;
    let answer = ask(&photo, None, "FC3411", false);
    let status = &answer["status"];
    assert_eq!(status["state"], "already-corrected");
    assert_eq!(status["search"], false);
    assert!(
        status["notice"]["text"]
            .as_str()
            .unwrap()
            .contains("already corrects lens distortion")
    );
    assert!(status.get("report").is_none() && status.get("suggestion").is_none());
    // An agent's search still sees why a compatible row cannot be selected.
    let rows = answer["rows"].as_array().unwrap();
    assert!(rows.iter().all(|row| {
        row["eligible"] == false
            && row["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("embedded-distortion-applied"))
    }));
}

#[test]
fn a_missing_or_out_of_range_focal_length_asks_for_one() {
    let missing = ask(&z6_jpeg("NIKKOR Z 24-70mm f/4 S", None), None, "", false);
    let status = &missing["status"];
    assert_eq!(status["state"], "detected");
    assert_eq!(status["visible_shared"], json!(["focal"]));
    assert_eq!(status["suggestion"]["eligible"], false);
    assert_eq!(status["suggestion"]["reasons"], json!(["focal-missing"]));
    assert!(
        status["notice"]["text"]
            .as_str()
            .unwrap()
            .contains("does not record its focal length")
    );
    let beyond = ask(
        &z6_jpeg("NIKKOR Z 24-70mm f/4 S", Some(100.0)),
        None,
        "",
        false,
    );
    let status = &beyond["status"];
    assert_eq!(status["visible_shared"], json!(["focal"]));
    assert_eq!(
        status["suggestion"]["reasons"],
        json!(["focal-out-of-range"])
    );
    assert!(
        status["notice"]["text"]
            .as_str()
            .unwrap()
            .contains("outside the profile's calibrated 24–70 mm")
    );
}

#[test]
fn a_crop_mode_photo_says_so_and_offers_no_search() {
    let mut photo = z6_jpeg("NIKKOR Z 24-70mm f/4 S", Some(35.0));
    photo.input.focal_35mm = Some(53);
    photo.optics.identity.focal_35mm = Some(53);
    let answer = ask(&photo, None, "", false);
    let status = &answer["status"];
    assert_eq!(status["search"], false);
    assert_eq!(status["suggestion"]["eligible"], false);
    assert!(
        status["notice"]["text"]
            .as_str()
            .unwrap()
            .contains("crop mode")
    );
}
