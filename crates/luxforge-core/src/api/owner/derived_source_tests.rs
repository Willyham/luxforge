//! A derived photograph (an HDR merge's result) through the catalog owner and the JSON methods:
//! opened by `source.prepare` and `job.adopt` with no original file, edited with its relative
//! white balance, sampled, analysed and exported, each answer the reference renderer's frame of the
//! same planes held in memory; and a damaged or missing source failing preparation, sampling and
//! export by name while its history reads.
use super::*;
use crate::{
    LinearImage, LinearSettings, PreviewSource, Raster, analysis::reduce,
    artifacts::derived_source::merged_planes,
};
use luxforge_test_macros::slow;
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};
use std::fs;

/// Two chunks across (a chunk is 1024 × 1024).
const WIDTH: u32 = 1100;
const HEIGHT: u32 = 200;

/// A catalog in its own directory holding a JPEG and a derived photograph of [`planes`], and an
/// owner over it.
struct Opened {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    dir: PathBuf,
    asset: Value,
    values: Vec<f32>,
}

impl Opened {
    fn new(name: &str) -> Self {
        Self::damaged(name, |_, _| {})
    }

    /// [`Self::new`] with `damage` given the artifact root and the source's manifest before the
    /// owner starts.
    fn damaged(name: &str, damage: impl FnOnce(&Path, &Value)) -> Self {
        let dir = temp_dir(&format!("derived-owner-{name}"))
            .canonicalize()
            .unwrap();
        let catalog = dir.join("catalog.sqlite");
        let anchor = dir.join("anchor.jpg");
        fs::copy(fixture(), &anchor).unwrap();
        let values = merged_planes(WIDTH, HEIGHT);
        let asset = {
            let mut service = EditorService::open(&catalog).unwrap();
            service.import(&anchor).unwrap();
            let asset = service
                .publish_planes("merge.hdr", WIDTH, HEIGHT, &values)
                .unwrap();
            let read = service.derived_source_read(&asset).unwrap();
            let root = service.artifact_root().to_path_buf();
            let manifest =
                crate::artifacts::derived_source::source_object_path(&root, read.id.sha256());
            let manifest: Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
            damage(&root, &manifest);
            asset
        };
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        Self {
            owner,
            join: Some(join),
            client,
            dir,
            asset: json!(asset),
            values,
        }
    }

    fn call(&self, method: &str, params: Value) -> ApiResponse {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.owner
            .call(
                self.client,
                ApiRequest {
                    id: format!("{method}-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        let response = self.call(method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn refused(&self, method: &str, params: Value) -> crate::api::ApiFailure {
        self.call(method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to be refused"))
    }

    /// Read a job until it leaves `queued` and `running`, as a client does.
    fn settled(&self, job: &Value) -> Value {
        luxforge_testbase::wait_for("the job to settle", || {
            let read = self.ok("job.read", json!({"job_id": job}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    fn state(&self) -> Value {
        self.ok("asset.state", json!({"asset_id": self.asset}))
    }

    fn edit(&self, method: &str, mut params: Value) {
        let revision = self.state()["revision"].clone();
        params["asset_id"] = self.asset.clone();
        params["mutation"] = json!({
            "expected_revision": revision,
            "request_id": format!("{method}-{revision}"),
            "actor": "test",
        });
        self.ok(method, params);
    }

    /// The reference renderer's frame of the current entry over the same planes held in memory:
    /// the independent frame every answer is held to.
    fn reference(&self) -> Raster {
        let state = self.state();
        let entry: crate::HistoryEntry =
            serde_json::from_value(state["current_entry"].clone()).unwrap();
        let fingerprint = state["asset"]["fingerprint"].as_str().unwrap();
        PreviewSource::Raw {
            image: LinearImage::with_fingerprint(WIDTH, HEIGHT, self.values.clone(), fingerprint)
                .unwrap(),
            settings: LinearSettings::default(),
        }
        .render(
            &ModuleRegistry::builtin(),
            entry.snapshot.id.clone(),
            &entry.snapshot.recipe,
        )
        .unwrap()
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A derived photograph opens through `source.prepare` and `job.adopt` with no original file;
/// edited with a relative white balance, headroom recovered and a mask, its samples, its histogram
/// and its export are each the reference frame of its planes held in memory, and `export.plan`
/// suggests no destination beside a file it does not have.
#[test]
fn a_derived_source_opens_samples_analyses_and_exports_through_the_owner() {
    let opened = Opened::new("journey");
    let queued = opened.ok("source.prepare", json!({"asset_id": opened.asset}));
    let record = opened.settled(&queued["job_id"]);
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(record["kind"], "prepare");
    let adopted = opened.ok("job.adopt", json!({"job_id": queued["job_id"]}));
    assert_eq!(adopted["asset"]["asset"]["source"]["kind"], "derived");
    assert_eq!(adopted["asset"]["revision"], 0, "no first-open correction");
    let inspected = opened.ok("source.inspect", json!({"asset_id": opened.asset}));
    assert_eq!(inspected["readiness"], "ready");
    opened.edit(
        "edit.set-basic",
        json!({"exposure": -1.5, "temperature": 30.0, "tint": -10.0, "contrast": 15.0,
               "highlights": -20.0}),
    );
    opened.edit(
        "mask.create-radial",
        json!({"x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.3, "angle": 0.0,
               "feather": 40.0}),
    );
    let mask = opened.state()["current_entry"]["snapshot"]["recipe"]["masks"][0]["id"].clone();
    opened.edit(
        "edit.set-basic",
        json!({"mask": mask, "exposure": 0.5, "saturation": -30.0}),
    );
    let reference = opened.reference();
    for (x, y) in [
        (0, 0),
        (2, 100),
        (550, 100),
        (1099, 199),
        (900, 10),
        (610, 150),
    ] {
        let sample = opened.ok(
            "render.sample",
            json!({"asset_id": opened.asset, "x": x, "y": y}),
        );
        let at = ((y * reference.width + x) * 4) as usize;
        assert_eq!(
            sample["rgba"],
            json!(&reference.rgba[at..at + 4]),
            "the sample at ({x}, {y})"
        );
    }
    let requested = opened.ok(
        "analysis.request",
        json!({"asset_id": opened.asset, "target": {"kind": "current"}}),
    );
    let analysed = opened.settled(&requested["job_id"]);
    assert_eq!(analysed["status"], "ready", "{analysed}");
    let expected = reduce(
        &reference.rgba,
        reference.width,
        reference.height,
        &crate::Cancel::never(),
    )
    .unwrap();
    assert_eq!(analysed["result"], serde_json::to_value(expected).unwrap());
    let plan = opened.ok(
        "export.plan",
        json!({"asset_id": opened.asset, "settings": crate::export::settings::baseline_json_in(&opened.dir)}),
    );
    // A derived photograph has no original to name the file after.
    assert_eq!(plan["suggested"], Value::Null);
    assert_eq!(plan["output"], json!({"width": WIDTH, "height": HEIGHT}));
    let destination = opened.dir.join("merge-edited.jpg");
    let accepted = opened.ok(
        "export.write",
        json!({"settings": crate::export::settings::baseline_json_in(&opened.dir), "asset_id": opened.asset, "destination": destination,
               "mutation": {"request_id": "derived-export", "actor": "test"}}),
    );
    let exported = opened.settled(&accepted["job_id"]);
    assert_eq!(exported["status"], "ready", "{exported}");
    let mut expected = Vec::new();
    crate::export::encode::encode_jpeg(
        &mut expected,
        &reference,
        &crate::export::encode::BASELINE_90,
        None,
        None,
        &mut |_| {},
        &|| Ok(()),
    )
    .unwrap();
    assert!(
        fs::read(&destination).unwrap() == expected,
        "the export is the encoding of the reference frame"
    );
}

/// A damaged chunk fails the photograph's preparation, its sample's and its export's by the
/// source's name and the chunk's, and a missing manifest refuses them at once; its history and its
/// state read throughout and nothing renders from anything else.
#[test]
#[slow]
fn a_damaged_derived_source_fails_preparation_sampling_and_export_by_name() {
    let opened = Opened::damaged("corrupt", |root, manifest| {
        let chunk = crate::artifacts::derived_source::source_object_path(
            root,
            manifest["chunks"][1]["sha256"].as_str().unwrap(),
        );
        let mut bytes = fs::read(&chunk).unwrap();
        bytes[17] ^= 0x08;
        fs::write(&chunk, bytes).unwrap();
    });
    let source = opened.state()["asset"]["source"]["interpretation"]["source_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let named = format!("derived source {source} chunk 1 is corrupt");
    let queued = opened.ok("source.prepare", json!({"asset_id": opened.asset}));
    let failed = opened.settled(&queued["job_id"]);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error"]["code"], "source-unavailable");
    assert_eq!(failed["error"]["message"], named);
    let destination = opened.dir.join("never.jpg");
    for (method, params) in [
        (
            "render.sample",
            json!({"asset_id": opened.asset, "x": 0, "y": 0}),
        ),
        (
            "export.write",
            json!({"settings": crate::export::settings::baseline_json_in(&opened.dir), "asset_id": opened.asset, "destination": destination,
                   "mutation": {"request_id": "derived-damaged-export", "actor": "test"}}),
        ),
    ] {
        let refused = opened.refused(method, params);
        assert_eq!(refused.code, "preparation-required", "{method}");
        let failed = opened.settled(&json!(refused.job_id.expect("its preparation")));
        assert_eq!(failed["status"], "failed", "{method}");
        assert_eq!(failed["error"]["message"], named, "{method}");
    }
    assert!(!destination.exists());
    let entry = opened.state()["current_entry"]["id"].clone();
    opened.ok(
        "history.inspect",
        json!({"asset_id": opened.asset, "entry_id": entry}),
    );
    opened.ok("history.list", json!({"asset_id": opened.asset}));

    let missing = Opened::damaged("missing", |root, manifest| {
        let _ = manifest;
        for entry in fs::read_dir(root.join(crate::artifacts::derived_source::SOURCES)).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
    });
    for (method, params) in [
        ("source.prepare", json!({"asset_id": missing.asset})),
        (
            "render.sample",
            json!({"asset_id": missing.asset, "x": 0, "y": 0}),
        ),
    ] {
        let refused = missing.refused(method, params);
        assert_eq!(refused.code, "source-unavailable", "{method}");
        assert!(
            refused.message.ends_with("is missing"),
            "{method}: {refused:?}"
        );
    }
    missing.state();
}
