//! A new photograph's Original from its modules ([`crate::ToolModule::original`]): each module's
//! layer where its effect's stage places it, checked by the module and refused by name, never
//! dropped; a JPEG's Original untouched by a RAW-only module; the Original's entry as it always
//! was; the `raw_look` preference reaching the hook at owner start and after `preferences.set`; and
//! a seeded catalog following the same rule.
//!
//! No RAW is checked into the repository, and the decoder refuses a camera mode it does not know,
//! so a Develop of several RAW picks runs `pick.develop`'s commit (`decide`, then `write`) over
//! reads of distinct JPEGs recast as RAW interpretations. The owner tests develop JPEG picks
//! through `pick.develop` itself.
use super::*;
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, BASIC_EFFECT, CompileStage, DETAIL_EFFECT,
    EditorService, LOOK_EFFECT, Layer, LayerReport, ModuleDescriptor, Orientation, OriginalContext,
    OriginalLayer, Processing, RawInterpretation, SourceKind, SourceTag, Stage, StageContext,
    ToolModule,
    catalog_types::{CatalogFolderId, FolderValue},
    editor::{distinct_jpeg, now_ms, original_work, synthetic_raw_metadata},
    library::{
        develop::{Destination, Developed, ReadFile, Refused, decide, read, write},
        journal::Request,
    },
};
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};
use serde_json::Map;
use std::{fs, path::Path, sync::Mutex};

/// What a probe gives a new photograph's Original.
#[derive(Clone)]
enum Contribution {
    /// This payload of the wrapped module's first effect, to a RAW photograph while the look is
    /// Standard, as the look module will; nothing otherwise.
    Raw(Value),
    /// A layer of another module's effect.
    Foreign,
    /// A refusal, whatever the photograph.
    Refuse,
}

/// A linked module with an Original hook: how any module that starts a new photo looks to the
/// host. It records the kind and look of every photograph it was asked about.
struct Probe {
    inner: Arc<dyn ToolModule>,
    contribution: Contribution,
    asked: Mutex<Vec<(SourceTag, RawLook, Option<String>)>>,
}

impl ToolModule for Probe {
    fn descriptor(&self) -> &ModuleDescriptor {
        self.inner.descriptor()
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        self.inner.parse(action_id, parameters)
    }
    fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error> {
        self.inner.plan(input, stage)
    }
    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error> {
        self.inner.validate_payload(effect_id, format, payload)
    }
    fn describe(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<LayerReport, Error> {
        self.inner.describe(effect_id, format, payload)
    }
    fn label(&self, action: &ActionDescriptor, input: &ActionInput) -> String {
        self.inner.label(action, input)
    }
    fn planned_label(&self, input: &ActionInput, layers: &[Layer], fallback: &str) -> String {
        self.inner.planned_label(input, layers, fallback)
    }
    fn settings(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<Map<String, Value>, Error> {
        self.inner.settings(effect_id, format, payload)
    }
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        at: CompileStage,
    ) -> Result<Processing, Error> {
        self.inner.compile(effect_id, format, payload, at)
    }
    fn carry(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        input: Stage,
        orientation: Orientation,
    ) -> Result<Option<Value>, Error> {
        self.inner
            .carry(effect_id, format, payload, input, orientation)
    }
    fn original(&self, context: &OriginalContext<'_>) -> Result<Option<OriginalLayer>, Error> {
        self.asked.lock().unwrap().push((
            context.source,
            context.preferences.raw_look,
            context.raw.map(|raw| raw.make.clone()),
        ));
        let raw = context.source == SourceTag::Raw;
        match &self.contribution {
            Contribution::Raw(payload)
                if raw && context.preferences.raw_look == RawLook::Standard =>
            {
                Ok(Some(OriginalLayer {
                    effect_id: self.inner.descriptor().effects[0].id.clone(),
                    payload: payload.clone(),
                }))
            }
            Contribution::Raw(_) => Ok(None),
            Contribution::Foreign if raw => Ok(Some(OriginalLayer {
                effect_id: DETAIL_EFFECT.into(),
                payload: json!({"sharpening": 40}),
            })),
            Contribution::Foreign => Ok(None),
            Contribution::Refuse => Err(Error::not_ready("probe look unavailable")),
        }
    }
}

/// The linked modules, each one `probes` names wrapped in a probe giving its contribution, in
/// registry order.
fn registry(probes: &[(&str, Contribution)]) -> (Vec<Arc<Probe>>, ModuleRegistry) {
    let mut registry = ModuleRegistry::new();
    let mut wrapped = Vec::new();
    for module in crate::modules::linked_modules(false) {
        match probes.iter().find(|(id, _)| module.descriptor().id == *id) {
            Some((_, contribution)) => {
                let probe = Arc::new(Probe {
                    inner: module,
                    contribution: contribution.clone(),
                    asked: Mutex::new(Vec::new()),
                });
                wrapped.push(probe.clone());
                registry.register(probe).unwrap();
            }
            None => registry.register(module).unwrap(),
        }
    }
    assert_eq!(wrapped.len(), probes.len(), "every probe wraps a module");
    (wrapped, registry)
}

const RAW_EFFECT: &str = "luxforge.raw";
const BASIC: &str = "luxforge.basic";
const DETAIL: &str = "luxforge.detail";

/// A distinct JPEG at `dir/name`, read for a Develop as the develop lane reads it, and as a RAW
/// when `raw`: its interpretation a small synthetic one, its size that interpretation's.
fn pick(dir: &Path, name: &str, raw: bool) -> ReadFile {
    let path = distinct_jpeg(&fixture(), &dir.join(name));
    let mut file = read(&path, false, &JobControl::new(), &|_| {}).unwrap();
    if raw {
        file.source = SourceKind::Raw {
            metadata: RawInterpretation::new(synthetic_raw_metadata()).unwrap(),
        };
        (file.width, file.height) = (32, 32);
    }
    file
}

/// `pick.develop`'s commit of `files` as one batch on `service`, into a new catalog folder:
/// the photographs created, in order, and the picks refused.
fn develop(service: &mut EditorService, files: Vec<ReadFile>) -> (Vec<AssetId>, Vec<Refused>) {
    let now = now_ms();
    let developed = files
        .into_iter()
        .map(|file| Developed {
            pick: file.path.clone(),
            used: None,
            file,
            moment: None,
        })
        .collect();
    original_work::take();
    let (decided, refused) = decide(service, developed, now).unwrap();
    assert_eq!(
        original_work::take(),
        (0, 0),
        "an Original reads and decodes nothing"
    );
    if !decided.is_empty() {
        let destination = Destination::New {
            folder: FolderValue {
                id: CatalogFolderId::new(),
                name: format!("Picks {}", NEXT.fetch_add(1, Ordering::Relaxed)),
                parent_id: None,
                created_ms: now,
                event: None,
            },
            events: Vec::new(),
        };
        let request_id = JobId::new().to_string();
        let request = Request {
            method: "pick.develop",
            actor: "test",
            request_id: &request_id,
        };
        let root = service.artifact_root().to_path_buf();
        service
            .library_write(|tx| write(tx, request, &root, &destination, &decided, now))
            .unwrap();
    }
    let created = decided
        .iter()
        .map(|decided| decided.reported().asset_id)
        .collect();
    (created, refused)
}

static NEXT: AtomicU64 = AtomicU64::new(1);

/// The Original's entry as every photograph has always had it: sequence and revision 0, the
/// `original` action labelled Original, by the system.
fn assert_original(entry: &crate::HistoryEntry) {
    assert_eq!(
        (
            entry.sequence,
            entry.result_revision,
            entry.action_id.as_str(),
            entry.label.as_str(),
            entry.actor.as_str(),
        ),
        (0, 0, "original", "Original", "system")
    );
    assert!(entry.undo_parent.is_none() && entry.request_id.is_none());
}

fn effects(recipe: &crate::Recipe) -> Vec<&str> {
    recipe
        .layers
        .iter()
        .map(|layer| layer.effect_id.as_str())
        .collect()
}

#[test]
fn each_new_raw_original_holds_the_contributed_layers_where_their_stages_place_them() {
    let dir = temp_dir("original-hook").canonicalize().unwrap();
    // Basic (colour) is asked before Detail (restoration), whose layer goes ahead of it.
    let (probes, registry) = registry(&[
        (BASIC, Contribution::Raw(json!({"exposure": 0.5}))),
        (DETAIL, Contribution::Raw(json!({"sharpening": 40}))),
    ]);
    let mut service =
        EditorService::open_with(&dir.join("catalog.sqlite"), Arc::new(registry)).unwrap();
    assert_eq!(service.raw_look(), RawLook::Standard, "Standard by default");
    let files = vec![
        pick(&dir, "a/one.NEF", true),
        pick(&dir, "a/two.NEF", true),
        pick(&dir, "a/three.jpg", false),
        pick(&dir, "a/four.NEF", true),
    ];
    let (created, refused) = develop(&mut service, files);
    assert!(refused.is_empty());
    assert_eq!(created.len(), 4);
    for (index, asset) in created.iter().enumerate() {
        let state = service.state(asset).unwrap();
        assert_eq!(state.revision, 0);
        let entry = &state.current_entry;
        assert_original(entry);
        let recipe = &entry.snapshot.recipe;
        if index == 2 {
            assert!(recipe.layers.is_empty(), "a JPEG's Original is untouched");
            continue;
        }
        assert_eq!(
            effects(recipe),
            [RAW_EFFECT, DETAIL_EFFECT, BASIC_EFFECT, LOOK_EFFECT],
            "the source layer stays first, then each by its stage and order: the built-in \
             Standard look after Basic"
        );
        assert_eq!(recipe.layers[1].payload, json!({"sharpening": 40}));
        assert_eq!(recipe.layers[2].payload, json!({"exposure": 0.5}));
        assert!(recipe.layers.iter().all(|layer| layer.mask.is_none()));
        assert_eq!(
            service.history(asset, None, 10).unwrap().entries.len(),
            1,
            "no entry records the contributed layers"
        );
    }
    // Every probe was asked once per photograph, in registry order, with the photograph's kind,
    // the look and its interpretation.
    for probe in &probes {
        let asked = probe.asked.lock().unwrap();
        let kinds: Vec<_> = asked.iter().map(|asked| asked.0).collect();
        assert_eq!(
            kinds,
            [
                SourceTag::Raw,
                SourceTag::Raw,
                SourceTag::Jpeg,
                SourceTag::Raw
            ]
        );
        assert!(asked.iter().all(|asked| asked.1 == RawLook::Standard));
        assert_eq!(asked[0].2.as_deref(), Some("Test"));
        assert_eq!(asked[2].2, None);
    }

    // Neutral: a photograph created from then on starts from the bare development; the photographs
    // already in the catalog keep their Originals.
    service.set_raw_look(RawLook::Neutral);
    let (neutral, refused) = develop(&mut service, vec![pick(&dir, "b/five.NEF", true)]);
    assert!(refused.is_empty());
    let state = service.state(&neutral[0]).unwrap();
    assert_original(&state.current_entry);
    assert_eq!(effects(&state.current_entry.snapshot.recipe), [RAW_EFFECT]);
    assert_eq!(
        probes[0].asked.lock().unwrap().last().unwrap().1,
        RawLook::Neutral
    );
    assert_eq!(
        effects(
            &service
                .state(&created[0])
                .unwrap()
                .current_entry
                .snapshot
                .recipe
        )
        .len(),
        4
    );
    drop(service);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_contribution_that_is_refused_refuses_the_new_photograph_by_the_modules_name() {
    let dir = temp_dir("original-hook-refused").canonicalize().unwrap();
    for (contribution, kind, refusal) in [
        (
            Contribution::Raw(json!({"exposure": "bright"})),
            "validation",
            "module luxforge.basic refused the new photograph's Original: ",
        ),
        (
            Contribution::Foreign,
            "internal",
            "module luxforge.basic refused the new photograph's Original: it gave a layer of \
             luxforge.detail.adjust, which is not one of its effects",
        ),
        (
            Contribution::Refuse,
            "not-ready",
            "module luxforge.basic refused the new photograph's Original: probe look unavailable",
        ),
    ] {
        let (_, registry) = registry(&[(BASIC, contribution.clone())]);
        let catalog = dir.join(format!("catalog-{kind}.sqlite"));
        let mut service = EditorService::open_with(&catalog, Arc::new(registry)).unwrap();
        let raw = pick(&dir, &format!("{kind}/refused.NEF"), true);
        let jpeg = pick(&dir, &format!("{kind}/kept.jpg"), false);
        let (created, refused) = develop(&mut service, vec![raw.clone(), jpeg]);
        // The refusal is the RAW pick's alone, and nothing of it is written; only a probe that
        // refuses whatever the photograph refuses the JPEG too.
        let jpeg_refused = matches!(contribution, Contribution::Refuse);
        assert_eq!(refused.len(), 1 + usize::from(jpeg_refused));
        assert_eq!(created.len(), usize::from(!jpeg_refused));
        let error = &refused[0].error;
        assert_eq!(refused[0].pick, raw.path);
        assert_eq!(error.kind.code(), kind, "{}", error.detail);
        assert!(error.detail.starts_with(refusal), "{}", error.detail);
        let photographs: i64 = service
            .connection
            .query_row("SELECT COUNT(*) FROM assets", [], |row| row.get(0))
            .unwrap();
        assert_eq!(photographs, created.len() as i64);
        drop(service);
    }
    fs::remove_dir_all(dir).unwrap();
}

/// The owner sets the look from the stored preference when it starts and again on each
/// `preferences.set`, and the hook reads it for the photographs a `pick.develop` creates from then
/// on; a JPEG's Original is untouched; a refusal fails its pick.
#[test]
fn an_owner_develop_asks_each_new_photograph_with_the_look_preference() {
    let dir = temp_dir("original-hook-owner").canonicalize().unwrap();
    let config = dir.join("config");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("preferences.json"),
        br#"{"format":1,"raw_look":"neutral"}"#,
    )
    .unwrap();
    let (probes, looking) = registry(&[(BASIC, Contribution::Raw(json!({"exposure": 0.5})))]);
    let (owner, join) = OwnerHandle::start_with_host(
        &dir.join("catalog.sqlite"),
        Arc::new(looking),
        crate::HostConfig {
            preferences_dir: Some(config),
            ..crate::HostConfig::unconfigured()
        },
    )
    .unwrap();
    let client = owner.register();
    let call = |method: &str, params: Value| {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: format!("{method}-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered");
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    };
    let develop = |paths: &[std::path::PathBuf]| {
        let job = call(
            "pick.develop",
            json!({
                "targets": {"kind": "paths", "paths": paths},
                "into": [],
                "confirm_removable": true,
                "mutation": {
                    "request_id": format!("develop-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                    "actor": "test",
                },
            }),
        );
        let record = luxforge_testbase::wait_for("the develop to settle", || {
            let status = call("job.read", json!({"job_id": job["job_id"]}));
            (!matches!(status["status"].as_str(), Some("queued" | "running"))).then_some(status)
        });
        assert_eq!(record["status"], "ready", "{record}");
        record["result"].clone()
    };
    let jpeg = |name: &str| distinct_jpeg(&fixture(), &dir.join("picks").join(name));
    let first = [jpeg("one.jpg"), jpeg("two.jpg"), jpeg("three.jpg")];
    let report = develop(&first);
    let developed = report["developed"].as_array().unwrap();
    assert_eq!(developed.len(), 3, "{report}");
    for photograph in developed {
        let state = call("asset.state", json!({"asset_id": photograph["asset_id"]}));
        let entry = &state["current_entry"];
        assert_eq!(state["revision"], 0);
        assert_eq!(
            (&entry["label"], &entry["actor"], &entry["action_id"]),
            (&json!("Original"), &json!("system"), &json!("original"))
        );
        assert_eq!(entry["snapshot"]["recipe"]["layers"], json!([]));
    }
    let asked = |probes: &[Arc<Probe>]| -> Vec<RawLook> {
        probes[0]
            .asked
            .lock()
            .unwrap()
            .iter()
            .map(|asked| asked.1)
            .collect()
    };
    assert_eq!(asked(&probes), [RawLook::Neutral; 3], "the stored look");

    let set = call("preferences.set", json!({"raw_look": null}));
    assert_eq!(set["raw_look"], "standard");
    develop(&[jpeg("four.jpg")]);
    assert_eq!(asked(&probes)[3], RawLook::Standard, "the look set since");
    owner.stop();
    join.join().unwrap();

    // A refusing module fails each pick by its name and creates nothing.
    let (_, refusing) = registry(&[(BASIC, Contribution::Refuse)]);
    let (owner, join) =
        OwnerHandle::start_with(&dir.join("refusing.sqlite"), Arc::new(refusing)).unwrap();
    let client = owner.register();
    let response = owner
        .call(
            client,
            ApiRequest {
                id: "refused".into(),
                method: "pick.develop".into(),
                params: json!({
                    "targets": {"kind": "paths", "paths": [jpeg("five.jpg")]},
                    "into": [],
                    "confirm_removable": true,
                    "mutation": {"request_id": "develop-refused", "actor": "test"},
                }),
                token: None,
            },
        )
        .unwrap();
    let job = response.result.unwrap()["job_id"].clone();
    let record = luxforge_testbase::wait_for("the refused develop to settle", || {
        let status = owner
            .call(
                client,
                ApiRequest {
                    id: "read".into(),
                    method: "job.read".into(),
                    params: json!({"job_id": job}),
                    token: None,
                },
            )
            .unwrap()
            .result
            .unwrap();
        (!matches!(status["status"].as_str(), Some("queued" | "running"))).then_some(status)
    });
    let result = &record["result"];
    assert_eq!(result["developed"], json!([]), "{record}");
    let failed = &result["failed"][0];
    assert!(
        failed.to_string().contains(
            "module luxforge.basic refused the new photograph's Original: probe look unavailable"
        ),
        "{record}"
    );
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

/// A seeded catalog builds its Originals by the same rule, with identities derived from the
/// photograph's: a seeded RAW holds what a developed one would.
#[test]
fn a_seeded_raw_original_follows_the_same_rule() {
    use crate::seed::{CatalogSeeder, SeedAsset, SeedKind};
    let dir = temp_dir("original-hook-seed").canonicalize().unwrap();
    let catalog = dir.join("catalog.sqlite");
    let (_, registry) = registry(&[
        (BASIC, Contribution::Raw(json!({"exposure": 0.5}))),
        (DETAIL, Contribution::Raw(json!({"sharpening": 40}))),
    ]);
    let registry = Arc::new(registry);
    let mut seeder = CatalogSeeder::create(&catalog, "seeded-originals")
        .unwrap()
        .serving(registry.clone());
    let volume = crate::catalog_types::Volume {
        id: crate::catalog_types::VolumeId::parse("volume-seeded-originals").unwrap(),
        mount_point: dir.clone(),
        label: "Photos".into(),
        removable: false,
        platform_id: None,
        last_seen_ms: 1,
    };
    seeder.volumes(std::slice::from_ref(&volume)).unwrap();
    let folder = crate::catalog_types::CatalogFolder {
        id: CatalogFolderId::new(),
        name: "Seeded".into(),
        parent_id: None,
        created_ms: 1,
        event: None,
        count: 0,
        year: None,
    };
    seeder.folders(std::slice::from_ref(&folder)).unwrap();
    let asset = |index: u32, kind| SeedAsset {
        id: AssetId::new(),
        kind,
        locator: dir.join(format!("{index}.NEF")),
        fingerprint: format!("{index:064x}"),
        file_identity: format!("unix:1:{index}"),
        byte_len: 1,
        width: 64,
        height: 48,
        catalog_folder_id: folder.id.clone(),
        volume_id: volume.id.clone(),
        developed_ms: 1,
        removed_ms: None,
        availability: crate::catalog_types::FileAvailability::Missing,
        checked_ms: 1,
        develop_moment: None,
        header: Default::default(),
        place: None,
    };
    let seeded = [asset(1, SeedKind::Raw), asset(2, SeedKind::Jpeg)];
    seeder.assets(&seeded).unwrap();
    seeder.finish().unwrap();
    let service = EditorService::open_with(&catalog, registry).unwrap();
    let raw = service.state(&seeded[0].id).unwrap();
    assert_original(&raw.current_entry);
    let layers = &raw.current_entry.snapshot.recipe.layers;
    assert_eq!(
        effects(&raw.current_entry.snapshot.recipe),
        [RAW_EFFECT, DETAIL_EFFECT, BASIC_EFFECT, LOOK_EFFECT]
    );
    let suffix = &seeded[0].id.as_str()[AssetId::PREFIX.len()..];
    let ids: Vec<_> = layers.iter().map(|layer| layer.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            format!("layer-{suffix}"),
            format!("layer-{suffix}-4"),
            format!("layer-{suffix}-2"),
            format!("layer-{suffix}-3"),
        ]
    );
    let jpeg = service.state(&seeded[1].id).unwrap();
    assert!(jpeg.current_entry.snapshot.recipe.layers.is_empty());
    drop(service);
    fs::remove_dir_all(dir).unwrap();
}
