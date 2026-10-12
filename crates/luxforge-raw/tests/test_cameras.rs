//! The test cameras (`data/test-cameras.json`) are admitted only once the process calls
//! `admit_test_cameras`. Admission is process-wide and never revoked, so this binary is its own
//! process and holds one test that checks the refusal before it admits them: nothing else here
//! admits them (the bracket generator's own tests, compiled in with it, only make bytes).
use luxforge_raw::{RawError, RawMode, RawSource, admit_test_cameras, test_cameras_admitted};
use luxforge_test_macros::slow;
use std::sync::atomic::AtomicBool;

// The synthetic bracket generator, compiled in from its one source, as luxforge-core's HDR tests
// do: only `Bracket::write` admits the test cameras, and this test asks for a frame's bytes alone.
#[path = "../../luxforge-testkit/src/hdr_bracket.rs"]
#[allow(dead_code)]
mod hdr_bracket;

const MODE: &str = "LuxforgeSyntheticBracketDng16";

#[test]
#[slow]
fn synthetic_body_is_refused_until_test_cameras_are_admitted() {
    let bracket = hdr_bracket::Bracket::new(&hdr_bracket::BracketSpec::default());
    let dng = bracket.dng(0);
    let cancel = AtomicBool::new(false);

    // Before: the native table refuses the body at identify, before any unpack, and no lookup
    // knows its mode.
    assert!(!test_cameras_admitted());
    assert_eq!(
        RawSource::decode(&dng, &cancel).err(),
        Some(RawError::UnsupportedMode(
            "camera model is outside the RAW catalog".into()
        ))
    );
    assert_eq!(RawMode::from_id(MODE), None);
    assert!(serde_json::from_value::<RawMode>(serde_json::json!(MODE)).is_err());

    // After, and idempotently: the body decodes as any catalogued DNG camera's file does.
    admit_test_cameras();
    admit_test_cameras();
    assert!(test_cameras_admitted());
    let source = RawSource::decode(&dng, &cancel).expect("an admitted test camera decodes");
    let metadata = source.metadata();
    assert_eq!(
        (metadata.make.as_str(), metadata.model.as_str()),
        (hdr_bracket::MAKE, hdr_bracket::MODEL)
    );
    assert_eq!(metadata.mode.id(), MODE);
    assert_eq!(
        (metadata.sensor_width, metadata.sensor_height),
        (hdr_bracket::SENSOR_WIDTH, hdr_bracket::SENSOR_HEIGHT)
    );
    let mode = RawMode::from_id(MODE).expect("an admitted test camera's mode");
    assert!(mode.requires_dng_corrections());
    assert!(!mode.is_monochrome());
    assert_eq!(
        serde_json::from_value::<RawMode>(serde_json::json!(MODE)).unwrap(),
        mode
    );
}
