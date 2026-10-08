//! Settings pasted between a JPEG and a RAW photograph, and a checked group reset to its defaults,
//! through the catalog owner as a client calls it: `edit.apply-settings` and `batch.apply-settings`
//! apply exactly what the target's kind takes and report the rest as skipped, with the reason the
//! single action would have refused it for. The RAW photographs are seeded (a RAW original is
//! never read for a field patch), so the RAW development layer is the first layer of their stack.

use luxforge_core::{
    AssetId, Layer, OwnerHandle,
    catalog_types::{
        CatalogFolder, CatalogFolderId, FileAvailability, HeaderMetadata, Volume, VolumeId,
    },
    seed::{CatalogSeeder, SeedAsset, SeedKind},
};
use luxforge_testbase::paths;
use luxforge_testkit::client::{self, call, settle};
use serde_json::{Value, json};
use std::{path::PathBuf, thread::JoinHandle};

const ACTOR: &str = "settings-kinds";

/// A seeded catalog of `kinds`, in order, under a running owner.
struct Rig {
    owner: OwnerHandle,
    editor: luxforge_core::ClientId,
    join: Option<JoinHandle<()>>,
    catalog: PathBuf,
    assets: Vec<Value>,
}

impl Rig {
    fn new(name: &str, kinds: &[SeedKind]) -> Self {
        let catalog = paths::temp_catalog(&format!("kinds-{name}"));
        let volume = Volume {
            id: VolumeId::parse("volume-kinds-tests").unwrap(),
            mount_point: "/".into(),
            label: "Test disk".into(),
            removable: false,
            platform_id: None,
            last_seen_ms: 1,
        };
        let folder = CatalogFolder {
            id: CatalogFolderId::new(),
            name: "Kinds".into(),
            parent_id: None,
            created_ms: 1,
            event: None,
            count: 0,
            year: None,
        };
        let seeds: Vec<SeedAsset> = kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| SeedAsset {
                id: AssetId::parse(format!("asset-{:032x}", index + 1)).unwrap(),
                kind: *kind,
                locator: PathBuf::from(format!("/Photos/DSC_{index:04}.JPG")),
                fingerprint: format!("{:064x}", index + 1),
                file_identity: format!("unix:9:{}", index + 1),
                byte_len: 64,
                width: 60,
                height: 40,
                catalog_folder_id: folder.id.clone(),
                volume_id: volume.id.clone(),
                developed_ms: 1_000,
                removed_ms: None,
                availability: FileAvailability::Available,
                checked_ms: 1,
                develop_moment: None,
                header: HeaderMetadata::default(),
                place: None,
            })
            .collect();
        let mut seeder = CatalogSeeder::create(&catalog, "kinds-tests").unwrap();
        seeder.volumes(&[volume]).unwrap();
        seeder.folders(&[folder]).unwrap();
        seeder.assets(&seeds).unwrap();
        seeder.finish().unwrap();
        let (owner, join) = OwnerHandle::start(&catalog).expect("an owner");
        let editor = owner.register();
        Self {
            owner,
            editor,
            join: Some(join),
            catalog,
            assets: seeds.iter().map(|seed| json!(seed.id)).collect(),
        }
    }

    fn call(&self, method: &str, params: Value) -> Value {
        call(&self.owner, self.editor, method, params).unwrap_or_else(|error| panic!("{error}"))
    }

    fn state(&self, asset: &Value) -> Value {
        client::state(&self.owner, self.editor, asset).unwrap()
    }

    fn revision(&self, asset: &Value) -> u64 {
        self.state(asset)["revision"].as_u64().unwrap()
    }

    fn entries(&self, asset: &Value) -> usize {
        self.call("history.list", json!({"asset_id": asset}))["entries"]
            .as_array()
            .unwrap()
            .len()
    }

    fn layers(&self, asset: &Value) -> Vec<Layer> {
        client::recipe(&self.owner, self.editor, asset)
            .unwrap()
            .layers
    }

    /// One edit action on the current revision, answering its result.
    fn edit(&self, method: &str, asset: &Value, tag: &str, fields: Value) -> Value {
        let mut params = json!({
            "asset_id": asset,
            "mutation": client::mutation(self.revision(asset), &client::request_id(tag), ACTOR),
        });
        params
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        self.call(method, params)
    }

    fn envelope(tag: &str) -> Value {
        json!({"request_id": client::request_id(tag), "actor": ACTOR})
    }

    /// `batch.apply-settings` of inline `settings` pasted from `source` onto `targets`, settled.
    fn batch_paste(&self, targets: &[&Value], settings: &Value, source: &str, tag: &str) -> Value {
        let started = self.call(
            "batch.apply-settings",
            json!({
                "targets": {"kind": "assets", "asset_ids": targets},
                "settings": settings,
                "origin": {"kind": "paste", "source": source},
                "mutation": Self::envelope(tag),
            }),
        );
        let settled = settle(&self.owner, self.editor, &started["job_id"]).unwrap();
        assert_eq!(settled["status"], "ready", "{settled}");
        settled
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_file(&self.catalog);
    }
}

const RAW_EFFECT: &str = "luxforge.raw";
const BASIC: &str = "luxforge.basic.adjust";
const PRESENCE: &str = "luxforge.presence.adjust";

/// The payload of the one global layer of `effect` in `asset`'s current stack.
fn payload(rig: &Rig, asset: &Value, effect: &str) -> Value {
    let layers = rig.layers(asset);
    let mut found = layers.iter().filter(|layer| layer.effect_id == effect);
    let layer = found.next().unwrap_or_else(|| panic!("no {effect} layer"));
    assert!(found.next().is_none(), "one {effect} layer");
    layer.payload.clone()
}

/// A stack as its effects and payloads in order, without the identities every commit mints.
fn contents(rig: &Rig, asset: &Value) -> Vec<(String, Value)> {
    rig.layers(asset)
        .into_iter()
        .map(|layer| (layer.effect_id, layer.payload))
        .collect()
}

fn current_label(rig: &Rig, asset: &Value) -> Value {
    rig.state(asset)["current_entry"]["label"].clone()
}

const TEMPERATURE_SKIP: &str =
    "on a RAW photo, Temperature is the source development's: set-raw temperature (K)";
const TINT_SKIP: &str = "on a RAW photo, Tint is the source development's: set-raw tint";

/// A JPEG's Basic white balance pasted onto a RAW photograph, by `edit.apply-settings` and by
/// `batch.apply-settings`: Basic's temperature and tint are the source development's on a RAW
/// photo's global layer (`set-raw`), so they are skipped with the single action's reasons, the
/// fields beside them are applied, and the RAW's own white balance is exactly as it was. A paste of
/// the white balance alone applies nothing, adds no entry and says why.
#[test]
fn a_jpegs_white_balance_pasted_onto_a_raw_photo_is_skipped_and_leaves_the_raw_white_balance() {
    let rig = Rig::new(
        "jpeg-to-raw",
        &[SeedKind::Jpeg, SeedKind::Raw, SeedKind::Raw],
    );
    let (jpeg, single, batched) = (&rig.assets[0], &rig.assets[1], &rig.assets[2]);
    rig.edit(
        "edit.set-basic",
        jpeg,
        "source",
        json!({"temperature": 20, "tint": -8, "exposure": 0.5}),
    );
    let white_balance = rig.call(
        "preset.capture",
        json!({"asset_id": jpeg, "fields": {"set-basic": ["temperature", "tint"]}}),
    )["settings"]
        .clone();
    assert_eq!(
        white_balance,
        json!({"set-basic": {"temperature": 20.0, "tint": -8.0}})
    );
    let with_exposure = json!({"set-basic": {"exposure": 0.25, "temperature": 20.0, "tint": -8.0}});
    let skipped = json!([
        {"action": "set-basic", "parameter": "temperature", "reason": TEMPERATURE_SKIP},
        {"action": "set-basic", "parameter": "tint", "reason": TINT_SKIP},
    ]);
    for raw in [single, batched] {
        rig.edit(
            "edit.set-raw",
            raw,
            "raw-wb",
            json!({"temperature": 5200, "tint": 3}),
        );
    }
    let development = payload(&rig, single, RAW_EFFECT);
    assert_eq!(development["wb_mode"], "custom");
    assert_eq!(development["temperature_kelvin"], json!(5200.0));
    assert_eq!(development["tint"], json!(3.0));
    assert_eq!(payload(&rig, batched, RAW_EFFECT), development);
    let origin = json!({"kind": "paste", "source": "Source.jpg"});

    // Edit: the white balance alone is a no-op that reports both fields.
    let (revision, entries, stack) = (
        rig.revision(single),
        rig.entries(single),
        contents(&rig, single),
    );
    let alone = rig.edit(
        "edit.apply-settings",
        single,
        "wb-only",
        json!({"settings": white_balance, "origin": origin}),
    );
    assert_eq!(alone["outcome"], "no-op");
    assert_eq!(alone["created_entry_id"], Value::Null);
    assert_eq!(alone["skipped"], skipped);
    assert_eq!(rig.revision(single), revision);
    assert_eq!(rig.entries(single), entries);
    assert_eq!(contents(&rig, single), stack);

    // Edit: beside an exposure, only the exposure is applied, in one new entry.
    let pasted = rig.edit(
        "edit.apply-settings",
        single,
        "with-exposure",
        json!({"settings": with_exposure, "origin": origin}),
    );
    assert_eq!(pasted["outcome"], "applied");
    assert_eq!(pasted["skipped"], skipped);
    assert_eq!(rig.revision(single), revision + 1);
    assert_eq!(rig.entries(single), entries + 1);
    assert_eq!(
        current_label(&rig, single),
        json!("Paste settings from Source.jpg")
    );
    assert_eq!(
        payload(&rig, single, BASIC),
        json!({"exposure": 0.25}),
        "no temperature or tint reached Basic"
    );
    assert_eq!(payload(&rig, single, RAW_EFFECT), development);

    // Batch: the white balance alone leaves the photograph out as not applicable.
    let (revision, entries) = (rig.revision(batched), rig.entries(batched));
    let alone = rig.batch_paste(&[batched], &white_balance, "Source.jpg", "batch-wb-only");
    assert_eq!(alone["result"]["done"], json!([]));
    assert_eq!(
        alone["result"]["skipped"],
        json!([{
            "asset_id": batched,
            "code": "not-applicable",
            "reason": format!("none of Source.jpg's settings apply to it: {TEMPERATURE_SKIP}; {TINT_SKIP}"),
        }])
    );
    assert_eq!(rig.revision(batched), revision);
    assert_eq!(rig.entries(batched), entries);

    // Batch: beside an exposure it applies the exposure and reports the same two skips the single
    // call did.
    let mixed = rig.batch_paste(
        &[batched],
        &with_exposure,
        "Source.jpg",
        "batch-with-exposure",
    );
    let report = &mixed["result"];
    assert_eq!(report["done"], json!([batched]));
    assert_eq!(report["skipped"], json!([]));
    assert_eq!(
        report["settings_skipped"],
        json!([{"asset_id": batched, "settings": skipped}])
    );
    assert_eq!(rig.entries(batched), entries + 1);
    assert_eq!(
        current_label(&rig, batched),
        json!("Paste settings from Source.jpg")
    );
    assert_eq!(payload(&rig, batched, BASIC), json!({"exposure": 0.25}));
    assert_eq!(payload(&rig, batched, RAW_EFFECT), development);
    assert_eq!(contents(&rig, batched), contents(&rig, single));
}

/// The reverse: a RAW photograph's `set-raw` white balance pasted onto a JPEG is skipped as the RAW
/// development does not apply to a JPEG, by the edit and by the batch, and the fields beside it are
/// applied; the JPEG keeps its own white balance.
#[test]
fn a_raw_photos_white_balance_pasted_onto_a_jpeg_is_skipped_and_leaves_the_jpeg_white_balance() {
    let rig = Rig::new(
        "raw-to-jpeg",
        &[SeedKind::Raw, SeedKind::Jpeg, SeedKind::Jpeg],
    );
    let (raw, single, batched) = (&rig.assets[0], &rig.assets[1], &rig.assets[2]);
    rig.edit(
        "edit.set-raw",
        raw,
        "source",
        json!({"temperature": 5200, "tint": 3}),
    );
    let white_balance = rig.call(
        "preset.capture",
        json!({"asset_id": raw, "fields": {"set-raw": ["temperature", "tint"]}}),
    )["settings"]
        .clone();
    assert_eq!(
        white_balance,
        json!({"set-raw": {"temperature": 5200.0, "tint": 3.0}})
    );
    let mut with_exposure = white_balance.clone();
    with_exposure["set-basic"] = json!({"exposure": 0.25});
    let skipped = json!([{"action": "set-raw", "reason": "RAW does not apply to a JPEG photo"}]);
    for jpeg in [single, batched] {
        rig.edit(
            "edit.set-basic",
            jpeg,
            "jpeg-wb",
            json!({"temperature": 12, "tint": -4}),
        );
    }
    assert_eq!(
        payload(&rig, single, BASIC),
        json!({"temperature": 12.0, "tint": -4.0})
    );
    let origin = json!({"kind": "paste", "source": "DSC_4471.NEF"});

    let (revision, entries, stack) = (
        rig.revision(single),
        rig.entries(single),
        contents(&rig, single),
    );
    let alone = rig.edit(
        "edit.apply-settings",
        single,
        "wb-only",
        json!({"settings": white_balance, "origin": origin}),
    );
    assert_eq!(alone["outcome"], "no-op");
    assert_eq!(alone["skipped"], skipped);
    assert_eq!(rig.revision(single), revision);
    assert_eq!(rig.entries(single), entries);
    assert_eq!(contents(&rig, single), stack);

    let pasted = rig.edit(
        "edit.apply-settings",
        single,
        "with-exposure",
        json!({"settings": with_exposure, "origin": origin}),
    );
    assert_eq!(pasted["outcome"], "applied");
    assert_eq!(pasted["skipped"], skipped);
    assert_eq!(rig.entries(single), entries + 1);
    assert_eq!(
        current_label(&rig, single),
        json!("Paste settings from DSC_4471.NEF")
    );
    let expected = json!({"exposure": 0.25, "temperature": 12.0, "tint": -4.0});
    assert_eq!(payload(&rig, single, BASIC), expected);
    assert!(
        rig.layers(single)
            .iter()
            .all(|layer| layer.effect_id != RAW_EFFECT),
        "no RAW development appeared on the JPEG"
    );

    let (revision, entries) = (rig.revision(batched), rig.entries(batched));
    let alone = rig.batch_paste(&[batched], &white_balance, "DSC_4471.NEF", "batch-wb-only");
    assert_eq!(alone["result"]["done"], json!([]));
    assert_eq!(
        alone["result"]["skipped"],
        json!([{
            "asset_id": batched,
            "code": "not-applicable",
            "reason": "none of DSC_4471.NEF's settings apply to it: RAW does not apply to a JPEG photo",
        }])
    );
    assert_eq!(rig.revision(batched), revision);
    assert_eq!(rig.entries(batched), entries);

    let mixed = rig.batch_paste(
        &[batched],
        &with_exposure,
        "DSC_4471.NEF",
        "batch-with-exposure",
    );
    let report = &mixed["result"];
    assert_eq!(report["done"], json!([batched]));
    assert_eq!(
        report["settings_skipped"],
        json!([{"asset_id": batched, "settings": skipped}])
    );
    assert_eq!(rig.entries(batched), entries + 1);
    assert_eq!(payload(&rig, batched, BASIC), expected);
    assert_eq!(contents(&rig, batched), contents(&rig, single));
}

/// A checked group sent at its Original (default) values resets the target's customised group to
/// those defaults in one history entry, by the edit and by the batch, and a field of the same
/// module that the set does not name keeps its value, as do the other modules.
#[test]
fn a_group_pasted_at_its_original_values_resets_the_targets_group_in_one_entry() {
    let rig = Rig::new("group-reset", &[SeedKind::Jpeg, SeedKind::Jpeg]);
    let customised = [
        (
            "set-presence",
            json!({"texture": 20, "clarity": -10, "dehaze": 5}),
        ),
        (
            "set-basic",
            json!({"exposure": 0.5, "contrast": 12, "highlights": -20}),
        ),
    ];
    for photo in &rig.assets {
        for (action, fields) in &customised {
            rig.edit(&format!("edit.{action}"), photo, action, fields.clone());
        }
        assert_eq!(
            payload(&rig, photo, PRESENCE),
            json!({"texture": 20.0, "clarity": -10.0, "dehaze": 5.0})
        );
    }
    let (single, batched) = (&rig.assets[0], &rig.assets[1]);
    let before = payload(&rig, single, BASIC);
    assert_eq!(
        before,
        json!({"exposure": 0.5, "contrast": 12.0, "highlights": -20.0})
    );
    // The whole Presence group at its defaults, and two of Basic's fields at theirs.
    let originals = json!({
        "set-presence": {"texture": 0, "clarity": 0, "dehaze": 0},
        "set-basic": {"exposure": 0, "contrast": 0},
    });
    let entries = rig.entries(single);
    let reset = rig.edit(
        "edit.apply-settings",
        single,
        "reset",
        json!({"settings": originals, "origin": {"kind": "paste", "source": "Original.jpg"}}),
    );
    assert_eq!(reset["outcome"], "applied");
    assert_eq!(rig.entries(single), entries + 1, "one history entry");
    assert_eq!(
        current_label(&rig, single),
        json!("Paste settings from Original.jpg")
    );
    // A field at its default is stored as absent: the group is back to what a fresh layer reads.
    assert_eq!(payload(&rig, single, PRESENCE), json!({}));
    assert_eq!(
        payload(&rig, single, BASIC),
        json!({"highlights": -20.0}),
        "the Basic field the set does not name keeps its value"
    );

    let entries = rig.entries(batched);
    let batch = rig.batch_paste(&[batched], &originals, "Original.jpg", "batch-reset");
    assert_eq!(batch["result"]["done"], json!([batched]));
    assert_eq!(batch["result"]["skipped"], json!([]));
    assert_eq!(rig.entries(batched), entries + 1, "one history entry");
    assert_eq!(contents(&rig, batched), contents(&rig, single));

    // Undo returns the whole group to its customised values in one step.
    let undone = rig.call(
        "history.undo",
        json!({
            "asset_id": single,
            "mutation": client::mutation(rig.revision(single), &client::request_id("undo"), ACTOR),
        }),
    );
    assert_eq!(undone["outcome"], "navigated");
    assert_eq!(
        payload(&rig, single, PRESENCE),
        json!({"texture": 20.0, "clarity": -10.0, "dehaze": 5.0})
    );
    assert_eq!(payload(&rig, single, BASIC), before);
}
