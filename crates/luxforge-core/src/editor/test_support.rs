//! Helpers the editor's test modules share: temporary catalogs, the JPEG fixture, mutations (the
//! crate's one test mutation, in its `Mutation` and JSON forms), stored strokes and entries, and a
//! test geometry module.
use super::{
    EditorState,
    catalog::{default_artifact_root, insert_entry},
};
use crate::{
    ActionDescriptor, Availability, Component, ComponentMode, EFFECT_FORMAT, EffectDescriptor,
    EffectStage, EntryId, Error, ExactGeometry, HistoryEntry, LayerId, Mask, ModuleDescriptor,
    ModuleRegistry, Mutation, ParameterDescriptor, Processing, Recipe, Snapshot, SnapshotId, Stage,
    ToolModule,
    modules::{ActionInput, ActionPlan, LayerUpdate, NewLayer, StageContext},
};
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use std::{path::Path, sync::Arc};

/// A scratch path no other test uses, and the JPEG fixture: the workspace's one pair, from
/// `luxforge-testbase`.
pub(super) use luxforge_testbase::paths::{jpeg as fixture, temp_path as temp};

/// The crate's one test mutation envelope: `request` at `revision`, by the actor `test`. Every core
/// test module that sends a mutation takes it from here, through `crate::editor`.
pub(crate) fn mutation(revision: u64, request: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "test".into(),
    }
}

/// [`mutation`] as the JSON a client sends in a method's `mutation` field.
pub(crate) fn mutation_json(revision: u64, request: &str) -> Value {
    serde_json::to_value(mutation(revision, request)).expect("a mutation serializes")
}

pub(super) fn stored_entry_json(catalog: &Path, entry: &EntryId) -> String {
    Connection::open(catalog)
        .unwrap()
        .query_row(
            "SELECT entry_json FROM entries WHERE id=?1",
            params![entry.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

/// One captured stroke, deterministic in the index so a session builds a different stroke per
/// entry and the same one twice on demand.
///
/// Its brush is small enough to take decimation's two-step floor rather than a share of its radius,
/// so each stroke keeps 67 to 78 of its 100 positions: the stroke the storage measurements in
/// `docs/design/masking.md#stroke-storage` are recorded on.
pub(super) fn stroke(index: usize) -> crate::mask::Stroke {
    let base = 0.05 + (index % 40) as f64 * 0.02;
    let points: Vec<[f64; 2]> = (0..100)
        .map(|step| {
            let t = step as f64 / 99.0;
            [
                base + 0.4 * t,
                0.2 + 0.3 * (t * 6.0 + index as f64).sin().abs(),
            ]
        })
        .collect();
    crate::mask::Stroke::capture(&points, 0.003, 50.0, 100.0, index.is_multiple_of(7))
        .expect("a legal stroke")
}

/// A mask whose one component references these strokes by address, with the strokes themselves
/// in the recipe's table. The component's kind is the one a brush will carry; this build has no
/// provider for it, which is exactly the retention case, and nothing here needs one: the store
/// is the host's and knows nothing about what references it.
pub(super) fn brushed(recipe: &Recipe, strokes: &[crate::mask::Stroke]) -> Recipe {
    let mut table = crate::path::StrokeTable::new("the test session");
    let addresses: Vec<String> = strokes
        .iter()
        .map(|stroke| table.insert(stroke.clone()).to_string())
        .collect();
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("brush");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "brush",
        json!({ "strokes": addresses }),
    ));
    Recipe {
        masks: vec![mask],
        strokes: table,
        ..recipe.clone()
    }
}

/// Write one entry as a commit would, validated and then through the production row write, which
/// is what stores its strokes.
pub(super) fn commit(catalog: &Path, entry: &HistoryEntry) {
    ModuleRegistry::builtin()
        .validate_recipe(&entry.snapshot.recipe)
        .unwrap();
    let mut connection = Connection::open(catalog).unwrap();
    let tx = connection.transaction().unwrap();
    insert_entry(&tx, &default_artifact_root(catalog), entry).unwrap();
    tx.execute(
        "UPDATE asset_state SET current_entry_id=?1, revision=?2 WHERE asset_id=?3",
        params![
            entry.id.as_str(),
            entry.result_revision as i64,
            entry.asset_id.as_str()
        ],
    )
    .unwrap();
    tx.commit().unwrap();
}

/// The next entry of a session, carrying `recipe` whole.
pub(super) fn next_entry(state: &EditorState, recipe: Recipe) -> HistoryEntry {
    HistoryEntry {
        id: EntryId::new(),
        sequence: state.current_entry.sequence + 1,
        label: "Brush 1".into(),
        undo_parent: Some(state.current_entry.id.clone()),
        base_revision: state.revision,
        result_revision: state.revision + 1,
        snapshot: Snapshot {
            id: SnapshotId::new(),
            asset_id: state.asset.id.clone(),
            recipe,
        },
        ..state.current_entry.clone()
    }
}

pub(super) const SHRINK_EFFECT: &str = "test.geometry.shrink";
const SHRINK_TAIL_EFFECT: &str = "test.geometry.tail";
pub(super) const SHRINK_ACTION: &str = "test-shrink";
pub(super) const TAIL_ACTION: &str = "test-shrink-tail";
pub(super) const MISSING_ACTION: &str = "test-shrink-missing";

/// A test-only geometry module that proves the host's in-place update path: `test-shrink`
/// updates its own layer when the stack already has one and appends one otherwise,
/// `test-shrink-tail` always commits a second geometry layer, which the tail carries after the
/// first one, and `test-shrink-missing` plans an update for an identity that is not in the
/// stack.
pub(super) struct ShrinkModule(ModuleDescriptor);

impl ShrinkModule {
    fn new() -> Self {
        let extent = |name: &str| {
            ParameterDescriptor::integer(name, 1, 16383)
                .required(true)
                .unit("px")
                .notes("test")
        };
        let action = |id: &str| ActionDescriptor {
            id: id.into(),
            title: "Shrink".into(),
            notes: "test".into(),
            patch: false,
            parameters: vec![extent("width"), extent("height")],
        };
        let effect = |id: &str| EffectDescriptor {
            id: id.into(),
            format: EFFECT_FORMAT,
            stage: EffectStage::Geometry,
            order: 0,
            maskable: false,
            artifacts: false,
            single: false,
            sources: Vec::new(),
        };
        Self(ModuleDescriptor {
            id: "test.shrink".into(),
            title: "Shrink".into(),
            hint: None,
            effects: vec![effect(SHRINK_EFFECT), effect(SHRINK_TAIL_EFFECT)],
            actions: vec![
                action(SHRINK_ACTION),
                action(TAIL_ACTION),
                action(MISSING_ACTION),
            ],
            queries: Vec::new(),
            controls: Vec::new(),
            reset: None,
            canvas: None,
            developer: false,
            collapsed: false,
            layout: crate::ModuleLayout::Stacked,
            availability: Availability::Available,
            ..ModuleDescriptor::default()
        })
    }

    fn extents(value: &Value) -> Result<(u32, u32), Error> {
        let read = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_u64)
                .filter(|extent| (1..=16383).contains(extent))
                .map(|extent| extent as u32)
                .ok_or_else(|| {
                    Error::validation(format!(
                        "parameter {name} must be an integer within 1..=16383"
                    ))
                })
        };
        Ok((read("width")?, read("height")?))
    }

    /// The registry the update tests use: the developer registry, whose pixel proof they edit with,
    /// plus this module.
    pub(super) fn registry() -> Arc<ModuleRegistry> {
        let mut registry = ModuleRegistry::developer();
        registry.register(Arc::new(Self::new())).unwrap();
        Arc::new(registry)
    }
}

impl ToolModule for ShrinkModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.0
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        let (width, height) = Self::extents(&Value::Object(parameters.clone()))?;
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: json!({"width": width, "height": height})
                .as_object()
                .expect("an object")
                .clone(),
        })
    }

    fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let (width, height) = Self::extents(&Value::Object(input.parameters.clone()))?;
        if input.action_id == MISSING_ACTION {
            return Ok(ActionPlan::Update(LayerUpdate::new(
                LayerId::new(),
                shrink(width, height),
            )));
        }
        if input.action_id == TAIL_ACTION {
            return Ok(ActionPlan::Commit(NewLayer::new(
                SHRINK_TAIL_EFFECT,
                shrink(width, height),
            )));
        }
        match stage
            .layers
            .iter()
            .find(|layer| layer.effect_id == SHRINK_EFFECT)
        {
            Some(existing) => Ok(ActionPlan::Update(LayerUpdate::new(
                existing.id.clone(),
                shrink(width, height),
            ))),
            None => Ok(ActionPlan::Commit(NewLayer::new(
                SHRINK_EFFECT,
                shrink(width, height),
            ))),
        }
    }

    fn validate_payload(&self, _: &str, _: u32, payload: &Value) -> Result<(), Error> {
        Self::extents(payload).map(|_| ())
    }

    fn describe(&self, _: &str, _: u32, payload: &Value) -> Result<crate::LayerReport, Error> {
        let (width, height) = Self::extents(payload)?;
        Ok(crate::LayerReport::new(format!(
            "Shrink to {width}x{height}"
        )))
    }

    fn compile(&self, _: &str, _: u32, payload: &Value, stage: Stage) -> Result<Processing, Error> {
        let (width, height) = Self::extents(payload)?;
        if width > stage.width || height > stage.height {
            return Err(Error::validation(format!(
                "shrink {width}x{height} is larger than the {}x{} input stage",
                stage.width, stage.height
            )));
        }
        Ok(Processing::ExactGeometry(ExactGeometry {
            a: 1,
            b: 0,
            c: 0,
            d: 1,
            tx: 0,
            ty: 0,
            output_width: width,
            output_height: height,
        }))
    }
}

pub(super) fn shrink(width: u32, height: u32) -> Value {
    json!({"width": width, "height": height})
}

/// A small RAW interpretation no decoder produced, valid for [`super::RawInterpretation::new`]:
/// what a test of the stored spelling, or of a rule decided by the photo's source kind, needs
/// without a private RAW fixture.
pub(crate) fn synthetic_raw_metadata() -> luxforge_raw::RawMetadata {
    use luxforge_raw::{RawMetadata, RawMode, RawRect};
    let rect = RawRect {
        x: 0,
        y: 0,
        width: 32,
        height: 32,
    };
    RawMetadata {
        make: "Test".into(),
        model: "Camera".into(),
        mode: RawMode::from_id("NikonZ6Lossless14").unwrap(),
        sensor_width: 32,
        sensor_height: 32,
        active_area: rect,
        default_crop: rect,
        cfa_width: 2,
        cfa_height: 2,
        cfa: vec![0, 1, 1, 2],
        black_cfa: vec![0, 1, 3, 2],
        black_base: 12.125,
        black_channels: [0.1, 0.2, 0.3, 0.4],
        black_repeat_width: 1,
        black_repeat_height: 1,
        black_repeat: vec![0.12345678],
        sensor_white: 16383.0,
        as_shot_gains: [1.2345678, 1.0, 1.8765432],
        libraw_flip: 0,
        rgb_cam: [[0.12345678; 4]; 3],
        cam_xyz: [[0.12345678; 3]; 4],
        backend: "pinned backend".into(),
        exif_orientation: 1,
        libraw_inset: Some(rect),
        format_identity: "test-format".into(),
        warnings: vec![],
        dng_corrections: None,
    }
}

/// Rewrite `asset`'s catalog row to say it is a RAW photo ([`synthetic_raw_metadata`]), for a
/// test of a rule the host decides from the source kind alone, before it reads a stack or a pixel.
/// Its stack stays the JPEG's and its file stays a JPEG, so nothing that plans, renders or prepares
/// may run against it. A service caches heads: open a fresh one on `catalog` afterwards.
pub(crate) fn recast_as_raw(catalog: &Path, asset: &crate::AssetId) {
    let kind = crate::SourceKind::Raw {
        metadata: super::RawInterpretation::new(synthetic_raw_metadata()).unwrap(),
    };
    let changed = Connection::open(catalog)
        .unwrap()
        .execute(
            "UPDATE assets SET source_json=?1 WHERE id=?2",
            params![super::encode(&kind).unwrap(), asset.as_str()],
        )
        .unwrap();
    assert_eq!(changed, 1, "one asset row");
}
