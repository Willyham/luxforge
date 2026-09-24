//! Helpers the editor's test modules share: temporary catalogs, the JPEG fixture, mutations,
//! stored strokes and entries, and a test geometry module.
use super::{
    EditorState,
    catalog::{default_artifact_root, insert_entry},
};
use crate::{
    ActionDescriptor, Availability, Component, ComponentMode, EFFECT_FORMAT, EffectDescriptor,
    EffectStage, EntryId, Error, ErrorKind, ExactGeometry, HistoryEntry, Layer, LayerId, Mask,
    ModuleDescriptor, ModuleRegistry, Mutation, ParameterDescriptor, ParameterKind, Processing,
    Recipe, Snapshot, SnapshotId, Stage, ToolModule,
    modules::{ActionInput, ActionPlan, StageContext},
};
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT: AtomicU64 = AtomicU64::new(1);
pub(super) fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "lightwell-editor-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}
pub(super) fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}
pub(super) fn mutation(revision: u64, request: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "test".into(),
    }
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
pub(super) fn stroke(index: usize) -> crate::path::Stroke {
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
    crate::path::Stroke::capture(&points, 0.04, 50.0, 100.0, index.is_multiple_of(7))
        .expect("a legal stroke")
}

/// A mask whose one component references these strokes by address, with the strokes themselves
/// in the recipe's table. The component's kind is the one a brush will carry; this build has no
/// provider for it, which is exactly the retention case, and nothing here needs one: the store
/// is the host's and knows nothing about what references it.
pub(super) fn brushed(recipe: &Recipe, strokes: &[crate::path::Stroke]) -> Recipe {
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
        let extent = |name: &str| ParameterDescriptor {
            name: name.into(),
            kind: ParameterKind::Integer { min: 1, max: 16383 },
            required: true,
            default: None,
            unit: Some("px".into()),
            step: None,
            precision: None,
            notes: "test".into(),
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
        };
        let action = |id: &str| ActionDescriptor {
            id: id.into(),
            title: "Shrink".into(),
            notes: "test".into(),
            summary: Some("Shrink {width}x{height}".into()),
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

    fn layer(id: LayerId, width: u32, height: u32) -> Layer {
        Self::layer_of(SHRINK_EFFECT, id, width, height)
    }

    fn layer_of(effect_id: &str, id: LayerId, width: u32, height: u32) -> Layer {
        Layer {
            id,
            effect_id: effect_id.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"width": width, "height": height}),
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn extents(value: &Value) -> Result<(u32, u32), Error> {
        let read = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_u64)
                .filter(|extent| (1..=16383).contains(extent))
                .map(|extent| extent as u32)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Validation,
                        format!("parameter {name} must be an integer within 1..=16383"),
                    )
                })
        };
        Ok((read("width")?, read("height")?))
    }

    /// The registry the update tests use: the built-ins plus this module.
    pub(super) fn registry() -> Arc<ModuleRegistry> {
        let mut registry = ModuleRegistry::builtin();
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
            return Ok(ActionPlan::Update(Self::layer(
                LayerId::new(),
                width,
                height,
            )));
        }
        if input.action_id == TAIL_ACTION {
            return Ok(ActionPlan::Commit(Self::layer_of(
                SHRINK_TAIL_EFFECT,
                LayerId::new(),
                width,
                height,
            )));
        }
        match stage
            .layers
            .iter()
            .find(|layer| layer.effect_id == SHRINK_EFFECT)
        {
            Some(existing) => Ok(ActionPlan::Update(Self::layer(
                existing.id.clone(),
                width,
                height,
            ))),
            None => Ok(ActionPlan::Commit(Self::layer(
                LayerId::new(),
                width,
                height,
            ))),
        }
    }

    fn validate_payload(&self, _: &str, _: u32, payload: &Value) -> Result<(), Error> {
        Self::extents(payload).map(|_| ())
    }

    fn describe_layer(&self, _: &str, _: u32, payload: &Value) -> Result<String, Error> {
        let (width, height) = Self::extents(payload)?;
        Ok(format!("Shrink to {width}x{height}"))
    }

    fn compile(&self, _: &str, _: u32, payload: &Value, stage: Stage) -> Result<Processing, Error> {
        let (width, height) = Self::extents(payload)?;
        if width > stage.width || height > stage.height {
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "shrink {width}x{height} is larger than the {}x{} input stage",
                    stage.width, stage.height
                ),
            ));
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
