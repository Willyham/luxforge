//! HDR merging end to end through the production RAW path: synthetic brackets of real DNGs
//! (`luxforge_testkit::hdr_bracket`, of a synthetic RAW test camera that writing a bracket admits
//! for the process) are read, indexed, grouped, decoded and developed exactly as a person's DNGs
//! are, merged by `merge.plan` and
//! `merge.create` through the real owner and registry with the shared verified development (no
//! synthetic camera), and the result opened, compared with the scene's ground truth and exported.
use super::*;
use crate::{
    ModuleRegistry,
    api::{
        ApiRequest, OwnerHandle,
        owner::{OwnerMessage, catalog::CatalogMessage, library::LibraryMessage},
    },
    artifacts::derived_source::read_derived_source,
    catalog_types::{HdrResolution, MergeProvenance, MergeResolution},
    merge::hdr::{ExposureOrigin, MetadataSource, ReferenceOrigin},
};
use luxforge_test_macros::slow;
use std::{sync::atomic::AtomicBool, thread::JoinHandle};

/// An HDR merge's resolution, inside its kind's tag.
fn resolution(provenance: &MergeProvenance) -> &HdrResolution {
    let MergeResolution::Hdr(resolved) = &provenance.resolved;
    resolved
}

// The generator, compiled into the core's tests once (`crate::merge::hdr::hdr_bracket`). Its
// `Bracket::write`, which every test here calls before reading a frame, admits the RAW test
// cameras through the core's own `luxforge_raw`.
use crate::merge::hdr::hdr_bracket;

use hdr_bracket::{Bracket, BracketSpec, HEIGHT, WIDTH};

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

/// Per-channel statistics of a developed or merged frame against the ground truth, over 8 × 8
/// blocks the truth is flat in.
#[derive(Debug, Default)]
struct Comparison {
    blocks: usize,
    worst_relative: f64,
    worst_at: (u32, u32),
    /// Blocks the reference frame clipped and the result recovered above one.
    recovered: usize,
    /// Block channels outside the tolerance, and the first.
    failures: usize,
    failure: Option<String>,
}

/// One frame of planes, red, green then blue, `width × height`, whose pixel `(i, j)` is the
/// reference's `(x0 + i, y0 + j)`.
struct Planes<'a> {
    values: &'a [f32],
    width: u32,
    height: u32,
    origin: (u32, u32),
}

impl Planes<'_> {
    fn at(&self, channel: usize, x: u32, y: u32) -> f64 {
        let plane = (self.width * self.height) as usize;
        let (i, j) = (x - self.origin.0, y - self.origin.1);
        f64::from(self.values[channel * plane + (j * self.width + i) as usize])
    }

    fn holds(&self, x: u32, y: u32) -> bool {
        x >= self.origin.0
            && y >= self.origin.1
            && x < self.origin.0 + self.width
            && y < self.origin.1 + self.height
    }
}

/// `image`'s viewed pixels as planes, red, green then blue.
fn viewed(image: &crate::LinearImage) -> Vec<f32> {
    let (width, height) = (image.width(), image.height());
    let plane = (width * height) as usize;
    let mut values = vec![0.0; 3 * plane];
    for y in 0..height {
        for x in 0..width {
            let rgb = image.pixel(x, y).expect("a pixel in the view");
            for (channel, value) in rgb.into_iter().enumerate() {
                values[channel * plane + (y * width + x) as usize] = value;
            }
        }
    }
    values
}

const BLOCK: u32 = 8;

/// Compare `planes` with `bracket`'s truth scaled by `gain` over every 8 × 8 block that lies
/// inside them, at least 4 pixels from the frame's edge, in which the truth varies by at most 2%,
/// which `usable` admits, and away from the moving ball; each block mean's channel must be within
/// `tolerance(bx, by, truth)` of the truth's mean, for the block at `(bx, by)`.
fn compare(
    bracket: &Bracket,
    planes: &Planes<'_>,
    gain: f64,
    usable: impl Fn(u32, u32) -> bool,
    tolerance: impl Fn(u32, u32, [f64; 3]) -> [f64; 3],
) -> Comparison {
    let mut found = Comparison::default();
    for by in (4..HEIGHT - BLOCK - 4).step_by(BLOCK as usize) {
        'block: for bx in (4..WIDTH - BLOCK - 4).step_by(BLOCK as usize) {
            if !planes.holds(bx, by) || !planes.holds(bx + BLOCK - 1, by + BLOCK - 1) {
                continue;
            }
            let centre = (f64::from(bx + BLOCK / 2), f64::from(by + BLOCK / 2));
            if bracket.near_subject(centre.0, centre.1, 16.0) {
                continue;
            }
            let mut truth = [0.0; 3];
            let mut low = [f64::INFINITY; 3];
            let mut high = [f64::NEG_INFINITY; 3];
            let mut mean = [0.0; 3];
            let mut clipped = true;
            for y in by..by + BLOCK {
                for x in bx..bx + BLOCK {
                    if !usable(x, y) {
                        continue 'block;
                    }
                    clipped &= bracket.level(0, x, y) >= 1.0;
                    let rgb = bracket.truth(x, y);
                    for channel in 0..3 {
                        let value = gain * rgb[channel];
                        truth[channel] += value;
                        low[channel] = low[channel].min(value);
                        high[channel] = high[channel].max(value);
                        mean[channel] += planes.at(channel, x, y);
                    }
                }
            }
            let count = f64::from(BLOCK * BLOCK);
            if (0..3).any(|channel| high[channel] - low[channel] > 0.02 * truth[channel] / count) {
                continue;
            }
            found.blocks += 1;
            if clipped && (0..3).any(|channel| mean[channel] / count > 1.0) {
                found.recovered += 1;
            }
            let expected = truth.map(|sum| sum / count);
            let allowed = tolerance(bx, by, expected);
            for channel in 0..3 {
                let (expected, got) = (expected[channel], mean[channel] / count);
                let error = (got - expected).abs() / allowed[channel];
                if error > 1.0 {
                    found.failures += 1;
                    found.failure.get_or_insert_with(|| {
                        format!("({bx}, {by}) channel {channel}: {got} for {expected}")
                    });
                }
                if error > found.worst_relative {
                    found.worst_relative = error;
                    found.worst_at = (bx, by);
                }
            }
        }
    }
    found
}

/// A noise-free frame of a synthetic bracket develops through the shared verified RAW development
/// as a person's DNG does — the catalogued synthetic body and recording mode, its header's
/// settings, bias and capture time, the as-shot white balance from its neutral — into the scene's
/// radiance times the frame's exposure, in every flat block the frame did not clip.
///
/// Tolerance: a block mean's channel within 0.3% of the truth, plus 2⁻¹⁴ of white. With no noise,
/// a flat block's demosaic is exact, so what is left is the 16-bit rounding of each site (half a
/// DN in 15,871), the colour matrix's six-digit rationals and `f32` arithmetic: the 0.3% is room
/// for RCD's interpolation inside blocks that are flat to 2% but not perfectly.
#[test]
#[slow]
fn hdr_merge_end_to_end_bracket_develops_as_a_users_dng() {
    let directory = luxforge_testbase::paths::temp_dir("hdr-merge-end-to-end-develops")
        .canonicalize()
        .unwrap();
    let bracket = Bracket::new(&BracketSpec {
        noise: false,
        ..BracketSpec::default()
    });
    let paths = bracket.write(&directory).unwrap();
    let control = JobControl::new();
    let mut captured = Vec::new();
    for (at, path) in paths.iter().enumerate() {
        let frame = &bracket.frames()[at];
        let developed = SharedDevelopment.develop(path, None, &control).unwrap();
        let metadata = &developed.frame;
        assert_eq!(metadata.make.as_deref(), Some(hdr_bracket::MAKE));
        assert_eq!(metadata.model.as_deref(), Some(hdr_bracket::MODEL));
        assert_eq!(
            metadata.recording,
            Recording::Raw {
                mode: "LuxforgeSyntheticBracketDng16".into(),
                layout: SensorLayout::Bayer,
            }
        );
        assert_eq!((metadata.width, metadata.height), (WIDTH, HEIGHT));
        assert_eq!(metadata.orientation, 1);
        let exposure = &metadata.exposure;
        // The header keeps a shutter time as `f32`.
        let shutter = exposure.shutter_s.unwrap() / frame.exposure_time_s();
        assert!((shutter - 1.0).abs() < 1e-6, "{exposure:?}");
        assert_eq!(exposure.f_number, Some(hdr_bracket::F_NUMBER));
        assert_eq!(exposure.iso, Some(u32::from(hdr_bracket::ISO)));
        assert_eq!(exposure.bias_ev, Some(f64::from(frame.offset_ev)));
        assert_eq!(exposure.focal_mm, Some(hdr_bracket::FOCAL_MM));
        captured.push(metadata.captured_at_ms.expect("a capture time"));
        let development = developed.development.expect("a RAW development");
        let expected = hdr_bracket::NEUTRAL.map(|neutral| 1.0 / neutral);
        for (gain, expected) in development.gains.iter().zip(expected) {
            assert!(
                (f64::from(*gain) - expected).abs() < 1e-3,
                "{:?}",
                development.gains
            );
        }
        let image = &development.image;
        let values = viewed(image);
        let planes = Planes {
            values: &values,
            width: image.width(),
            height: image.height(),
            origin: (0, 0),
        };
        let gain = (f64::from(frame.offset_ev)).exp2();
        let found = compare(
            &bracket,
            &planes,
            gain,
            |x, y| bracket.level(at, x, y) < 0.95,
            |_, _, truth| truth.map(|truth| 0.003 * truth + 1.0 / 16384.0),
        );
        assert!(found.blocks > 100 && found.failures == 0, "{at}: {found:?}");
    }
    let steps: Vec<i64> = captured.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let expected: Vec<i64> = bracket
        .frames()
        .windows(2)
        .map(|pair| i64::from(pair[1].captured_after_ms - pair[0].captured_after_ms))
        .collect();
    assert_eq!(
        steps, expected,
        "capture times in order, to the millisecond"
    );
    let _ = fs::remove_dir_all(&directory);
}

/// A catalog and its owner, with the builtin registry and the merge lane's own development,
/// beside a synthetic bracket written into a folder of its own.
struct Merging {
    directory: PathBuf,
    bracket: Bracket,
    folder: PathBuf,
    paths: Vec<PathBuf>,
    hashes: Vec<String>,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
}

impl Merging {
    fn new(name: &str, spec: &BracketSpec) -> Self {
        let directory = luxforge_testbase::paths::temp_dir(&format!("hdr-merge-end-to-end-{name}"))
            .canonicalize()
            .unwrap();
        let bracket = Bracket::new(spec);
        let folder = directory.join("bracket");
        let paths = bracket.write(&folder).unwrap();
        let hashes = paths.iter().map(|path| sha256(path)).collect();
        let (owner, join) = OwnerHandle::start_with(
            &directory.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
        )
        .unwrap();
        let client = owner.register();
        Self {
            directory,
            bracket,
            folder,
            paths,
            hashes,
            owner,
            join: Some(join),
            client,
        }
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        let response = self
            .owner
            .call(
                self.client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered");
        match response.error {
            None => response.result.expect("a result"),
            Some(error) => panic!("{method}: {error:?}"),
        }
    }

    /// The job once it has ended, which must be ready: its result.
    fn finished(&self, job_id: &Value) -> Value {
        let read = luxforge_testbase::wait_for("the job to end", || {
            let read = self.ok("job.read", json!({"job_id": job_id}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        });
        assert_eq!(read["status"], "ready", "{read}");
        read["result"].clone()
    }

    /// Run `read` on the owner, in order with every call before it.
    fn on_owner<T: Send + 'static>(
        &self,
        read: impl FnOnce(&mut Owner) -> T + Send + 'static,
    ) -> T {
        let (answer, answered) = std::sync::mpsc::channel();
        self.owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Merge(
                LibraryMessage::Run(Box::new(move |owner| {
                    let _ = answer.send(read(owner));
                })),
            )))
            .unwrap();
        answered.recv().unwrap()
    }

    /// List the bracket's folder into the index, plan and create its merge into the folder the plan
    /// proposes — its year, its place (the folder on disk that names its event) and its day, which
    /// the merge makes — and wait for it: the plan, the job's result and the recorded provenance.
    fn merge(&self) -> (Value, Value, MergeProvenance) {
        let listed = self.ok(
            "index.refresh",
            json!({"source": {"kind": "folder", "path": self.folder}}),
        );
        let report = self.finished(&listed["job_id"]);
        assert_eq!(report["files"], json!(self.paths.len()), "{report}");
        assert_eq!(report["unreadable"], 0, "{report}");
        let targets = json!({"kind": "paths", "paths": self.paths});
        let plan = self.ok("merge.plan", json!({"kind": "hdr", "targets": targets}));
        assert_eq!(
            plan["suggested_folder"],
            json!({"kind": "place", "name": "bracket"}),
            "{plan}"
        );
        let job = self.ok(
            "merge.create",
            json!({
                "kind": "hdr",
                "targets": targets,
                "plan_id": plan["plan_id"],
                "mutation": {"request_id": "merge", "actor": "test"},
            }),
        );
        let result = self.finished(&job["job_id"]);
        let folders = self.ok("folder.list", json!({}))["folders"].clone();
        let folder = |id: &Value| {
            folders
                .as_array()
                .unwrap()
                .iter()
                .find(|folder| &folder["id"] == id)
                .unwrap_or_else(|| panic!("no folder {id}: {folders}"))
                .clone()
        };
        let day = folder(&result["catalog_folder_id"]);
        let place = folder(&day["parent_id"]);
        let year = folder(&place["parent_id"]);
        assert_eq!(place["name"], "bracket", "{folders}");
        assert_eq!(year["name"].as_str().unwrap().len(), 4, "{folders}");
        assert!(year.get("parent_id").is_none(), "{folders}");
        let inspected = self.ok("merge.inspect", json!({"merge_id": result["merge_id"]}));
        let provenance = serde_json::from_value(inspected["provenance"].clone()).unwrap();
        (plan, result, provenance)
    }

    /// Open the result through `source.prepare`, then read its saved planes as preparation does.
    fn result_planes(&self, asset: &Value) -> crate::LinearImage {
        let prepared = self.ok("source.prepare", json!({"asset_id": asset}));
        self.finished(&prepared["job_id"]);
        let asset = AssetId::parse(asset.as_str().unwrap().to_owned()).unwrap();
        let read = self
            .on_owner(move |owner| owner.service.derived_source_read(&asset))
            .unwrap();
        read_derived_source(&read, &AtomicBool::new(false)).unwrap()
    }

    /// Export the result as a JPEG; answer its decoded size.
    fn export(&self, asset: &Value) -> (u32, u32) {
        let destination = self.directory.join("exported.jpg");
        let accepted = self.ok(
            "export.jpeg",
            json!({"asset_id": asset, "destination": destination,
                   "mutation": {"request_id": "export", "actor": "test"}}),
        );
        self.finished(&accepted["job_id"]);
        let bytes = fs::read(&destination).unwrap();
        let mut decoder =
            luxforge_jpeg::Decoder::new(&bytes, crate::source::JPEG_LIMITS).expect("a JPEG");
        let (width, height) = (decoder.width(), decoder.height());
        let mut rows = vec![0; width as usize * height as usize * 4];
        decoder
            .read_rows(&mut rows)
            .expect("the whole JPEG decodes");
        decoder.finish().unwrap();
        (width, height)
    }

    fn originals_unchanged(&self) {
        let now: Vec<String> = self.paths.iter().map(|path| sha256(path)).collect();
        assert_eq!(now, self.hashes, "no original's bytes change");
    }
}

impl Drop for Merging {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Where a block may be compared with the truth after a merge: some frame recorded every one of
/// its pixels below the fusion's roll-off (sensor level 0.8), so the result there is measured, not
/// the all-invalid fallback.
fn recorded(bracket: &Bracket, x: u32, y: u32) -> bool {
    (0..bracket.frames().len()).any(|at| bracket.level(at, x, y) < 0.8)
}

/// The tolerance a merged block's channels are held to: 0.5% of the truth plus 2⁻¹⁴ of white for
/// what is not noise, and five standard deviations of the noise of the block's mean as the frame
/// that recorded the whole block longest under the roll-off has it.
///
/// The fixed part: demosaicing, the as-shot white balance and the colour matrix are within 0.3%
/// on a flat block (the noise-free development test above), and one bilinear resample of a
/// handheld frame moves a block flat to 2% by a fraction of that. The noise: the fusion weights
/// each frame by its exposure under the roll-off, the inverse of its shot-noise variance, so the
/// fused mean is no noisier than that frame's alone. A red or blue channel the shortest
/// exposure alone recorded rests on 16 sites of a few hundred electrons, carried through a
/// colour matrix that amplifies it: about 0.5% of the truth a standard deviation, against which
/// a fixed 1.5% would fail one block channel in a few hundred. Five deviations leave a chance
/// failure under one in a million block channels, of about 20,000 compared.
fn merged_tolerance(bracket: &Bracket, bx: u32, by: u32, truth: [f64; 3]) -> [f64; 3] {
    let longest = (0..bracket.frames().len())
        .filter(|&at| {
            (by..by + BLOCK).all(|y| (bx..bx + BLOCK).all(|x| bracket.level(at, x, y) < 0.8))
        })
        .max_by_key(|&at| bracket.frames()[at].offset_ev)
        .unwrap_or_else(|| {
            (0..bracket.frames().len())
                .min_by_key(|&at| bracket.frames()[at].offset_ev)
                .unwrap()
        });
    let sigma = bracket.block_sigma(longest, truth, BLOCK * BLOCK);
    std::array::from_fn(|channel| 0.005 * truth[channel] + 1.0 / 16384.0 + 5.0 * sigma[channel])
}

/// The registration's declared tolerance through a 2× analysis proxy (hdr-merge.md, alignment
/// evidence), in pixels at every frame corner.
const REGISTERED_PX: f64 = 0.25;

/// The registration recovered each frame's motion: exactly the identity for a still frame, whose
/// fit is within its own uncertainty of no motion, and within [`REGISTERED_PX`] at every frame
/// corner of a handheld frame's generated motion.
fn assert_registered(bracket: &Bracket, provenance: &MergeProvenance) {
    let registration = &resolution(provenance).registration;
    for (at, frame) in bracket.frames().iter().enumerate() {
        let transform = &registration.inputs[at].transform;
        if frame.motion == hdr_bracket::Rigid::IDENTITY {
            assert!(
                transform.is_identity(),
                "input {at} is still: {:?}",
                registration.inputs[at]
            );
            continue;
        }
        let generated = crate::merge::hdr::RigidTransform::new(
            WIDTH,
            HEIGHT,
            frame.motion.rotation_rad,
            frame.motion.shift.0,
            frame.motion.shift.1,
        );
        let error = transform.corner_distance(&generated, WIDTH, HEIGHT);
        assert!(
            error < REGISTERED_PX,
            "input {at} is {error} pixels from its motion: {:?}",
            registration.inputs[at]
        );
    }
}

/// The merged result, opened through `source.prepare`, against the scene's ground truth over its
/// coverage, in the merged tolerance.
fn compare_result(merging: &Merging, result: &Value, provenance: &MergeProvenance) -> Comparison {
    let coverage = &resolution(provenance).coverage;
    let image = merging.result_planes(&result["result_asset_id"]);
    assert_eq!(
        (image.width(), image.height()),
        (coverage.rect.width, coverage.rect.height)
    );
    let values = viewed(&image);
    let planes = Planes {
        values: &values,
        width: coverage.rect.width,
        height: coverage.rect.height,
        origin: (coverage.rect.x, coverage.rect.y),
    };
    let bracket = &merging.bracket;
    compare(
        bracket,
        &planes,
        1.0,
        |x, y| recorded(bracket, x, y),
        |bx, by, truth| merged_tolerance(bracket, bx, by, truth),
    )
}

/// A bracket of three DNGs, 0, −2 and +2 EV, listed into the index, grouped as one bracket from
/// its metadata, planned and merged through the registry with the production development: the
/// provenance has the generated exposures from metadata with the metered frame the reference,
/// identity registration and full coverage; the result opens through `source.prepare` and holds
/// the scene's radiance wherever some frame recorded it, above one where the metered frame
/// clipped; it exports as a JPEG; the originals' bytes are unchanged.
#[test]
#[slow]
fn hdr_merge_end_to_end_merges_a_still_bracket_to_its_ground_truth() {
    let merging = Merging::new("still", &BracketSpec::default());
    let (plan, result, provenance) = merging.merge();

    // The index read every header: one body, the frames' settings, so the plan resolves the
    // exposures from metadata with the zero-bias frame the reference.
    assert_eq!(plan["suitable"], true, "{plan}");
    assert_eq!(plan["needs_fit"], false, "{plan}");
    assert_eq!(plan["reference"], 0, "{plan}");

    // Once merged, the folder shows the result alone, its three frames collapsed beneath it.
    let view = merging.ok(
        "browse.view",
        json!({"source": {"kind": "folder", "path": merging.folder}}),
    );
    assert_eq!(
        (&view["count"], &view["stacks"], &view["collapsed"]),
        (&json!(1), &json!(1), &json!(3)),
        "{view}"
    );
    // Expanded, Select groups the frames as one bracket, from their metadata.
    merging.ok(
        "stack.set-expanded",
        json!({"merge_id": result["merge_id"], "expansion": "expanded"}),
    );
    let view = merging.ok(
        "browse.view",
        json!({"source": {"kind": "folder", "path": merging.folder}}),
    );
    let moments = view["groups"]["moments"].as_array().unwrap();
    assert_eq!(moments.len(), 1, "{view}");
    assert_eq!(moments[0]["kind"], "bracket", "{view}");
    assert_eq!(moments[0]["evidence"], "metadata", "{view}");
    assert_eq!(moments[0]["steps_ev"], json!([0.0, -2.0, 2.0]), "{view}");
    assert_eq!(moments[0]["len"], 3);

    // The provenance: every input as decoded, the generated offsets from the settings, the
    // metered frame the reference, nothing moved and nothing lost.
    let bracket = &merging.bracket;
    assert_eq!(result["file_name"], job::result_name(&merging.paths[0]));
    // Offsets from `f32` shutter times: within a millionth of a stop of the generated ones.
    let range: Vec<f64> = serde_json::from_value(result["exposure_range_ev"].clone()).unwrap();
    assert!(
        (range[0] + 2.0).abs() < 1e-6 && (range[1] - 2.0).abs() < 1e-6,
        "{range:?}"
    );
    let resolved = resolution(&provenance);
    assert_eq!(resolved.exposures.reference, 0);
    assert_eq!(
        resolved.exposures.reference_origin,
        ReferenceOrigin::ZeroBias
    );
    for (at, frame) in bracket.frames().iter().enumerate() {
        let input = &provenance.inputs[at];
        assert_eq!(input.path, merging.paths[at]);
        assert_eq!(input.frame.make.as_deref(), Some(hdr_bracket::MAKE));
        assert_eq!(input.frame.model.as_deref(), Some(hdr_bracket::MODEL));
        assert_eq!(
            input.frame.exposure.bias_ev,
            Some(f64::from(frame.offset_ev))
        );
        let exposure = &resolved.exposures.inputs[at];
        assert!(
            (exposure.offset_ev - f64::from(frame.offset_ev)).abs() < 1e-6,
            "{exposure:?}"
        );
        let origin = if at == 0 {
            ExposureOrigin::Reference
        } else {
            ExposureOrigin::Metadata {
                source: MetadataSource::Settings,
            }
        };
        assert_eq!(exposure.origin, origin);
    }
    assert_registered(bracket, &provenance);
    let gains = resolved.development.white_balance;
    for (gain, neutral) in gains.iter().zip(hdr_bracket::NEUTRAL) {
        assert!((f64::from(*gain) * neutral - 1.0).abs() < 1e-3, "{gains:?}");
    }
    // Every input is fitted, within its uncertainty of no motion, and so the identity: the whole
    // frame is covered.
    let coverage = &resolved.coverage;
    assert_eq!(coverage.pixels_lost, 0, "{coverage:?}");
    assert_eq!((coverage.rect.width, coverage.rect.height), (WIDTH, HEIGHT));
    let fusion = &resolved.fusion;
    assert!(fusion.highlight_extended > 50_000, "{fusion:?}");
    assert!(fusion.shadow_extended > 5_000, "{fusion:?}");
    // The sun and its nearest glow, which no exposure records.
    assert!(
        fusion.all_invalid > 0 && fusion.all_invalid < 20_000,
        "{fusion:?}"
    );
    assert_eq!(resolved.motion.regions, 0, "{:?}", resolved.motion);

    // The result opens, and holds the scene's radiance.
    let found = compare_result(&merging, &result, &provenance);
    assert!(found.blocks > 500 && found.failures == 0, "{found:?}");
    assert!(found.recovered > 50, "{found:?}");

    assert_eq!(
        merging.export(&result["result_asset_id"]),
        (coverage.rect.width, coverage.rect.height)
    );
    merging.originals_unchanged();
}

/// A handheld bracket — each frame after the metered one turned by a fraction of a degree and
/// shifted a few pixels — whose brightest frame caught a ball in another place: the registration
/// recovers each frame's motion, the coverage loses the edges no frame shares, deghosting finds
/// the ball and shows the reference's, and the covered result holds the scene's radiance.
#[test]
#[slow]
fn hdr_merge_end_to_end_aligns_and_deghosts_a_handheld_bracket() {
    let spec = BracketSpec {
        handheld: true,
        moving_subject: Some(2),
        seed: 7,
        ..BracketSpec::default()
    };
    let merging = Merging::new("handheld", &spec);
    let (_, result, provenance) = merging.merge();
    let bracket = &merging.bracket;
    let resolved = resolution(&provenance);
    assert_eq!(resolved.exposures.reference, 0);
    assert_registered(bracket, &provenance);
    let coverage = &resolved.coverage;
    assert!(coverage.pixels_lost > 0, "{coverage:?}");
    assert!(
        coverage.rect.width > WIDTH - 32 && coverage.rect.height > HEIGHT - 32,
        "{coverage:?}"
    );
    let motion = &resolved.motion;
    assert!(motion.regions >= 1, "{motion:?}");
    assert!(motion.reference_regions >= 1, "{motion:?}");

    let image = merging.result_planes(&result["result_asset_id"]);
    assert_eq!(
        (image.width(), image.height()),
        (coverage.rect.width, coverage.rect.height)
    );
    let values = viewed(&image);
    let planes = Planes {
        values: &values,
        width: coverage.rect.width,
        height: coverage.rect.height,
        origin: (coverage.rect.x, coverage.rect.y),
    };
    let found = compare(
        bracket,
        &planes,
        1.0,
        |x, y| recorded(bracket, x, y),
        |bx, by, truth| merged_tolerance(bracket, bx, by, truth),
    );
    assert!(found.blocks > 500 && found.failures == 0, "{found:?}");
    assert!(found.recovered > 50, "{found:?}");
    // One ball, where the reference saw it, and the scene where the brightest frame did: within
    // 5% of the truth, against a ghost that would be the ball's red over the field or half of
    // each.
    let reference_ball = bracket.frames()[0].subject;
    let moved_ball = bracket.frames()[2].subject;
    assert_ne!(reference_ball, moved_ball);
    for (x, y) in [reference_ball, moved_ball] {
        let (bx, by) = (x as u32 - BLOCK / 2, y as u32 - BLOCK / 2);
        let (mut got, mut want) = ([0.0; 3], [0.0; 3]);
        for y in by..by + BLOCK {
            for x in bx..bx + BLOCK {
                let truth = bracket.truth(x, y);
                for channel in 0..3 {
                    got[channel] += planes.at(channel, x, y);
                    want[channel] += truth[channel];
                }
            }
        }
        for channel in 0..3 {
            let (got, want) = (got[channel], want[channel]);
            assert!(
                (got - want).abs() <= 0.05 * want,
                "({x}, {y}) channel {channel}: {got} for {want}"
            );
        }
    }
    merging.originals_unchanged();
}

/// Merge a bracket wider or finer than the default through the production path: every frame
/// registered (the extremes, usable on a few percent of the frame, searched on a finer level or
/// chained through a neighbour), the exposures from the settings, the coverage whole when still,
/// and the result within the merged tolerance of the ground truth, recovering highlights.
fn merges_a_bracket(name: &str, spec: &BracketSpec) {
    let merging = Merging::new(name, spec);
    let (plan, result, provenance) = merging.merge();
    assert_eq!(plan["suitable"], true, "{plan}");
    assert_eq!(plan["needs_fit"], false, "{plan}");
    let bracket = &merging.bracket;
    let resolved = resolution(&provenance);
    assert_eq!(resolved.exposures.reference, 0);
    for (at, frame) in bracket.frames().iter().enumerate() {
        let exposure = &resolved.exposures.inputs[at];
        assert!(
            (exposure.offset_ev - f64::from(frame.offset_ev)).abs() < 1e-6,
            "{exposure:?}"
        );
    }
    assert_registered(bracket, &provenance);
    let coverage = &resolved.coverage;
    if spec.handheld {
        assert!(
            coverage.rect.width > WIDTH - 32 && coverage.rect.height > HEIGHT - 32,
            "{coverage:?}"
        );
    } else {
        assert_eq!(coverage.pixels_lost, 0, "{coverage:?}");
    }
    let found = compare_result(&merging, &result, &provenance);
    assert!(found.blocks > 500 && found.failures == 0, "{found:?}");
    assert!(found.recovered > 50, "{found:?}");
    merging.originals_unchanged();
}

/// Five frames two stops apart, 0, −2, +2, −4 and +4 EV, as a camera's common automatic
/// bracketing takes them: the +4 EV frame records only the shadows, a few percent of the frame.
#[test]
#[slow]
fn hdr_merge_end_to_end_merges_a_five_frame_two_stop_still_bracket() {
    merges_a_bracket(
        "five-by-two-still",
        &BracketSpec {
            frames: 5,
            step_ev: 2,
            ..BracketSpec::default()
        },
    );
}

/// The same five-frame bracket handheld: each frame turned and shifted, the extremes too.
#[test]
#[slow]
fn hdr_merge_end_to_end_merges_a_five_frame_two_stop_handheld_bracket() {
    merges_a_bracket(
        "five-by-two-handheld",
        &BracketSpec {
            frames: 5,
            step_ev: 2,
            handheld: true,
            seed: 3,
            ..BracketSpec::default()
        },
    );
}

/// Seven frames a stop apart, ±3 EV.
#[test]
#[slow]
fn hdr_merge_end_to_end_merges_a_seven_frame_one_stop_bracket() {
    merges_a_bracket(
        "seven-by-one",
        &BracketSpec {
            frames: 7,
            step_ev: 1,
            ..BracketSpec::default()
        },
    );
}

/// Three frames three stops apart, ±3 EV.
#[test]
#[slow]
fn hdr_merge_end_to_end_merges_a_three_frame_three_stop_bracket() {
    merges_a_bracket(
        "three-by-three",
        &BracketSpec {
            step_ev: 3,
            ..BracketSpec::default()
        },
    );
}
