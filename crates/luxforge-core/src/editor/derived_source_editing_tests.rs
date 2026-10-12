//! A derived photograph through the editor service: its preparation reads and verifies its stored
//! source once into one shared [`LinearImage`] and reads no file; the cache answers after it; the
//! planes are leased on the source worker's memory gate and released by eviction; cancellation and
//! damage are named failures that keep history readable; and its stacks render exactly, signed and
//! headroom values linear until the SDR boundary, as the same planes held in memory render.
use super::derived_source_tests::{catalog, directory, never};
use super::source::Preparing;
use super::*;
use crate::artifacts::derived_source::{payload_work, source_object_path};
use crate::{LinearImage, LinearSettings, PreviewSource, source::PlaneGate};
use serde_json::json;
use std::{fs, sync::atomic::AtomicBool};

/// Two chunks across (a chunk is 1024 × 1024).
const WIDTH: u32 = 1100;
const HEIGHT: u32 = 300;

/// A merge's result: in each channel a ramp from below zero to four times diffuse white, crossed by
/// a vertical ramp, with a band of fine detail and a column out of the sRGB gamut, whose red is
/// negative.
pub(super) fn scene(width: u32, height: u32) -> Vec<f32> {
    let plane = (width * height) as usize;
    let mut planes = vec![0.0_f32; 3 * plane];
    for y in 0..height {
        for x in 0..width {
            let u = x as f32 / (width - 1) as f32;
            let v = y as f32 / (height - 1).max(1) as f32;
            let detail = if (y / 3) % 2 == 0 {
                0.06 * ((x % 3) as f32 - 1.0)
            } else {
                0.0
            };
            let ramp = -0.2 + 4.2 * u * u + detail;
            let mut rgb = [
                ramp * (0.55 + 0.9 * v),
                ramp * (1.1 - 0.6 * v),
                -0.1 + 1.8 * v * (1.0 - u) + detail,
            ];
            if x % 97 < 6 {
                rgb = [-0.25, 0.7 + v, 0.35];
            }
            let at = (y * width + x) as usize;
            for (channel, value) in rgb.into_iter().enumerate() {
                planes[channel * plane + at] = value;
            }
        }
    }
    planes
}

/// A catalog holding a JPEG and a derived photograph of [`scene`], its writer's hashes taken.
fn published(name: &str) -> (PathBuf, PathBuf, EditorService, AssetId, Vec<f32>) {
    let directory = directory(name);
    let (catalog, mut service, _) = catalog(&directory);
    let values = scene(WIDTH, HEIGHT);
    let asset = service
        .publish_planes("merge.hdr", WIDTH, HEIGHT, &values)
        .unwrap();
    payload_work::take();
    original_work::take();
    (directory, catalog, service, asset, values)
}

fn prepare(service: &mut EditorService, asset: &AssetId) -> Result<EditorState, Error> {
    let needs = service.entry_needs(asset, None)?;
    service.prepare(&needs)
}

/// The derived planes the verified cache holds for `asset`.
fn cached_planes(service: &EditorService, asset: &AssetId) -> LinearImage {
    let state = service.state(asset).unwrap();
    match service
        .verified_prepared(&state.asset, &state.current_entry.snapshot.recipe)
        .unwrap()
    {
        crate::source::PreparedSource::Derived(derived) => derived.linear,
        _ => panic!("a derived photograph prepares a derived source"),
    }
}

fn same_bits(read: &[f32], written: &[f32]) -> bool {
    read.len() == written.len()
        && read
            .iter()
            .zip(written)
            .all(|(read, written)| read.to_bits() == written.to_bits())
}

/// The same planes held in memory, as any linear source is: the independent reference a derived
/// photograph's frames are held to.
fn in_memory(values: &[f32], fingerprint: &str) -> PreviewSource {
    PreviewSource::Raw {
        image: LinearImage::with_fingerprint(WIDTH, HEIGHT, values.to_vec(), fingerprint).unwrap(),
        settings: LinearSettings::default(),
    }
}

fn apply(service: &mut EditorService, asset: &AssetId, action: &str, parameters: Value) {
    let revision = service.state(asset).unwrap().revision;
    service
        .run_action(
            asset,
            mutation(revision, &format!("{action}-{revision}")),
            action,
            parameters,
        )
        .unwrap();
}

/// A cold preparation reads and verifies the manifest and each chunk once, reads no original file,
/// and holds one allocation of planes, bit for bit what was written, which every later reading of
/// the cache shares; a preparation the cache answers, renders and samples read nothing. A new
/// service over the same catalog starts cold again.
#[test]
fn derived_source_preparation_reads_its_source_once_into_shared_planes() {
    let (directory, catalog, mut service, asset, values) = published("prepare-once");
    let state = prepare(&mut service, &asset).unwrap();
    assert_eq!(state.asset.id, asset);
    assert_eq!(
        payload_work::take(),
        3,
        "the manifest and two chunks, each read and hashed once"
    );
    assert_eq!(original_work::take(), (0, 0), "no original file read");
    let first = cached_planes(&service, &asset);
    let second = cached_planes(&service, &asset);
    assert!(
        std::ptr::eq(first.shared_planes().0, second.shared_planes().0),
        "the cache shares one allocation"
    );
    assert!(same_bits(first.shared_planes().0, &values));
    assert_eq!(first.fingerprint(), state.asset.fingerprint);
    // The cache answers: no read, no hash.
    let needs = service.entry_needs(&asset, None).unwrap();
    assert!(matches!(
        service.preparation(&needs).unwrap(),
        Preparing::Ready(_)
    ));
    service.prepare(&needs).unwrap();
    service.render_current(&asset).unwrap();
    let entry = service.state(&asset).unwrap().current_entry.id;
    service.sample_entry(&asset, &entry, 3, 5).unwrap();
    let inspected = service.inspect_source(&asset, None).unwrap();
    assert_eq!(inspected["readiness"], "ready");
    assert_eq!(inspected["source"]["kind"], "derived");
    assert_eq!(payload_work::take(), 0, "the cache answered every reading");
    assert_eq!(original_work::take(), (0, 0));
    drop(service);
    let mut reopened = EditorService::open(&catalog).unwrap();
    assert_eq!(
        reopened.inspect_source(&asset, None).unwrap()["readiness"],
        "preparation-required"
    );
    prepare(&mut reopened, &asset).unwrap();
    assert_eq!(payload_work::take(), 3, "a new service reads it again");
    drop(reopened);
    fs::remove_dir_all(directory).unwrap();
}

/// The preparation is the source worker's: its work decodes, so it waits on the memory gate, and
/// its planes hold a lease there until the cache lets them go; preparing another photograph evicts
/// them first, and the derived source is read again when it is next opened.
#[test]
fn derived_source_planes_are_leased_on_the_gate_and_evicted_before_another_source() {
    let (directory, _, mut service, asset, _) = published("gate");
    let gate = Arc::new(PlaneGate::default());
    let needs = service.entry_needs(&asset, None).unwrap();
    let Preparing::Work(work, reads) = service.preparation(&needs).unwrap() else {
        panic!("a cold derived source is work");
    };
    assert!(
        work.decodes(),
        "it allocates planes, so it waits on the gate"
    );
    let mut prepared = work.run(&reads, &never()).unwrap();
    assert_eq!(payload_work::take(), 3);
    prepared
        .linear_mut()
        .expect("derived planes")
        .hold(gate.lease());
    assert_eq!(gate.counted(), 1);
    service.complete_preparation(prepared).unwrap();
    service.render_current(&asset).unwrap();
    assert_eq!(gate.counted(), 1, "the cache holds the leased planes");
    service.evict_development(false);
    assert_eq!(gate.counted(), 0, "evicted before another source allocates");
    let state = service.state(&asset).unwrap();
    assert_eq!(
        service
            .verified_prepared(&state.asset, &state.current_entry.snapshot.recipe)
            .unwrap_err()
            .kind,
        ErrorKind::PreparationRequired
    );
    prepare(&mut service, &asset).unwrap();
    assert_eq!(payload_work::take(), 3);
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

/// A cancelled preparation is `cancelled` and keeps nothing; the photograph's history reads and a
/// later preparation completes.
#[test]
fn a_cancelled_derived_source_preparation_keeps_nothing() {
    let (directory, _, mut service, asset, values) = published("cancel");
    let needs = service.entry_needs(&asset, None).unwrap();
    let Preparing::Work(work, reads) = service.preparation(&needs).unwrap() else {
        panic!("a cold derived source is work");
    };
    let error = work.run(&reads, &AtomicBool::new(true)).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(payload_work::take(), 0, "nothing was read");
    let state = service.state(&asset).unwrap();
    assert!(
        service
            .verified_prepared(&state.asset, &state.current_entry.snapshot.recipe)
            .is_err()
    );
    service.history(&asset, None, 10).unwrap();
    prepare(&mut service, &asset).unwrap();
    assert!(same_bits(
        cached_planes(&service, &asset).shared_planes().0,
        &values
    ));
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

/// A damaged chunk fails the preparation by the source's name and the chunk's, a missing manifest
/// fails it before any worker runs, and a prepared source whose manifest has gone refuses a render
/// rather than answering from what it held; history reads throughout, nothing stored is changed,
/// and the photograph prepares again once its source is whole.
#[test]
fn a_damaged_derived_source_fails_preparation_and_rendering_by_name() {
    let (directory, _, mut service, asset, values) = published("damaged");
    let state = service.state(&asset).unwrap();
    let source = state.asset.source.derived().unwrap().source_id.clone();
    let root = service.artifact_root().to_path_buf();
    let manifest = source_object_path(&root, source.sha256());
    let chunk = source_object_path(&root, &read_chunk(&manifest, 1));
    let whole = fs::read(&chunk).unwrap();
    let mut damaged = whole.clone();
    damaged[9] ^= 0x10;
    fs::write(&chunk, &damaged).unwrap();
    let error = prepare(&mut service, &asset).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(
        error.detail,
        format!("derived source {source} chunk 1 is corrupt")
    );
    assert_eq!(
        service.render_current(&asset).unwrap_err().kind,
        ErrorKind::PreparationRequired,
        "no frame without its verified source"
    );
    service.history(&asset, None, 10).unwrap();
    assert_eq!(fs::read(&chunk).unwrap(), damaged, "kept as it is");
    fs::write(&chunk, &whole).unwrap();
    prepare(&mut service, &asset).unwrap();
    assert!(same_bits(
        cached_planes(&service, &asset).shared_planes().0,
        &values
    ));
    // The cache's signature is the stored manifest: once it has gone the prepared planes answer
    // nothing, and a new preparation names it missing.
    let kept = fs::read(&manifest).unwrap();
    fs::remove_file(&manifest).unwrap();
    let error = service.render_current(&asset).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(error.detail, format!("derived source {source} is missing"));
    let error = prepare(&mut service, &asset).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert!(error.detail.contains("is missing"), "{error}");
    service.state(&asset).unwrap();
    service.history(&asset, None, 10).unwrap();
    fs::write(&manifest, kept).unwrap();
    prepare(&mut service, &asset).unwrap();
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

/// The SHA-256 of chunk `ordinal` the manifest at `path` lists.
fn read_chunk(path: &Path, ordinal: usize) -> String {
    let manifest: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    manifest["chunks"][ordinal]["sha256"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The Original of a derived photograph is no edit: each output code is the stored linear value's
/// sRGB code, negative values black and values above one white, worked out independently by the
/// reference crate. Exposure down two stops brings the headroom back: a value of four is white and
/// one of two is the code of a half, so nothing was clipped before the SDR boundary.
#[test]
fn a_derived_source_stays_linear_until_the_sdr_boundary() {
    let (directory, _, mut service, asset, values) = published("linear");
    let state = prepare(&mut service, &asset).unwrap();
    assert!(state.current_entry.snapshot.recipe.layers.is_empty());
    let plane = (WIDTH * HEIGHT) as usize;
    let codes = |frame: &crate::Raster, scale: f64| {
        let mut mismatched = 0;
        for at in 0..plane {
            for channel in 0..3 {
                let expected =
                    luxforge_reference::srgb::code(f64::from(values[channel * plane + at]) * scale);
                if frame.rgba[at * 4 + channel] != expected {
                    mismatched += 1;
                }
            }
        }
        mismatched
    };
    let neutral = service.render_current(&asset).unwrap();
    assert_eq!((neutral.width, neutral.height), (WIDTH, HEIGHT));
    assert_eq!(codes(&neutral, 1.0), 0, "the stored values' codes exactly");
    assert!(values.iter().any(|value| *value < 0.0) && values.iter().any(|value| *value > 3.0));
    apply(&mut service, &asset, "set-basic", json!({"exposure": -2.0}));
    let darker = service.render_current(&asset).unwrap();
    assert_eq!(
        codes(&darker, 0.25),
        0,
        "two stops down is a quarter of each stored value, its headroom intact"
    );
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

/// Every module a derived photograph offers, as a person stacks them — Basic with a relative white
/// balance and headroom, the Tone curve, the colour mixer with grading, a masked Basic, Detail,
/// Presence, a straightened crop, Perspective and the vignette — renders on the reference renderer
/// exactly as the same planes held in memory render the same stack, at every stack prefix, all
/// from its one preparation.
#[test]
fn slow_a_derived_source_renders_every_stack_as_its_planes_in_memory_do() {
    let (directory, _, mut service, asset, values) = published("stacks");
    let state = prepare(&mut service, &asset).unwrap();
    assert_eq!(payload_work::take(), 3);
    let reference = in_memory(&values, &state.asset.fingerprint);
    let steps: [(&str, Value); 9] = [
        (
            "set-basic",
            json!({"exposure": -1.2, "contrast": 20.0, "highlights": -40.0, "shadows": 30.0,
                   "whites": -10.0, "blacks": 10.0, "temperature": 25.0, "tint": -10.0,
                   "vibrance": 20.0, "saturation": 10.0}),
        ),
        (
            "set-curve",
            json!({"luminance": [[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]]}),
        ),
        (
            "set-mixer",
            json!({"red-hue": 20, "green-saturation": 30, "grade-shadows-hue": 200,
                   "grade-shadows-saturation": 40}),
        ),
        (
            "mask.create-linear",
            json!({"x0": 0.5, "y0": 0.1, "x1": 0.5, "y1": 0.9}),
        ),
        ("set-detail", json!({"sharpening": 50.0, "luminance": 30.0})),
        ("set-presence", json!({"clarity": 40.0, "texture": 30.0})),
        ("crop-fit", json!({"aspect": "16:9", "angle": 4.0})),
        (
            "set-perspective",
            json!({"horizontal": 15, "vertical": -10}),
        ),
        ("set-vignette", json!({"amount": -40, "midpoint": 40})),
    ];
    let registry = ModuleRegistry::builtin();
    for (action, parameters) in steps {
        if action == "mask.create-linear" {
            apply(&mut service, &asset, action, parameters);
            let mask = service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .masks[0]
                .id
                .clone();
            apply(
                &mut service,
                &asset,
                "set-basic",
                json!({"mask": mask, "exposure": 0.6, "saturation": -20.0}),
            );
        } else {
            apply(&mut service, &asset, action, parameters);
        }
        let entry = service.state(&asset).unwrap().current_entry;
        let frame = service.render_current(&asset).unwrap();
        let expected = reference
            .render(&registry, entry.snapshot.id.clone(), &entry.snapshot.recipe)
            .unwrap();
        assert_eq!(
            (frame.width, frame.height),
            (expected.width, expected.height),
            "{action}"
        );
        assert!(
            frame.rgba == expected.rgba,
            "{action}: the derived photograph's frame is its planes' frame"
        );
        assert_eq!(frame.source_fingerprint, state.asset.fingerprint);
    }
    assert_eq!(
        payload_work::take(),
        0,
        "every frame from the one preparation"
    );
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}
