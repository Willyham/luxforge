//! The Corrections module: its descriptor and its [`ToolModule`] hooks.
use super::{
    CORRECTIONS_EFFECT, actions, labels,
    payload::{MAX_CORRECTIONS, RepairPayload},
    query, table,
};
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, BrushEdit, BrushSource,
    CanvasInteraction, EffectDescriptor, EffectOperations, EffectStage, Error, IdentityKind, Layer,
    LayerReport, ModuleDescriptor, ParameterDescriptor, Processing, ResetAction, StageContext,
    ToolModule,
    path::{POINTS_PER_STROKE, POSTED_POINTS_PER_STROKE},
};
use serde_json::{Map, Value};

pub(crate) const MODULE: &str = "luxforge.corrections";

pub(crate) const ADD: &str = "add-correction";
pub(crate) const UPDATE: &str = "update-correction";
pub(crate) const DELETE: &str = "delete-correction";
pub(crate) const REORDER: &str = "reorder-correction";
pub(crate) const RESET: &str = "reset-corrections";
pub(crate) const ACCEPT: &str = "accept-correction-patch";
pub(crate) const LIST: &str = "corrections";
pub(crate) const SOURCES: &str = "correction-sources";

/// The parameter every operation-bound action and query names its correction by.
pub(crate) const OPERATION: &str = "operation";

/// The most ranked sources one `query.correction-sources` answers.
pub(crate) const MAX_SOURCES: i64 = 8;

/// The Corrections module. Stateless: everything it knows of a photograph is its repair layer.
#[derive(Debug)]
pub(crate) struct CorrectionsModule {
    descriptor: ModuleDescriptor,
}

impl Default for CorrectionsModule {
    fn default() -> Self {
        Self::new()
    }
}

fn points() -> ParameterDescriptor {
    ParameterDescriptor::points("points", 1, POSTED_POINTS_PER_STROKE).notes(format!(
        "the correction's path in the content stage's normalized coordinates (x a fraction of its \
         width, y of its height, the frame 0..1), in drawn order, raw or already decimated; one \
         position is a spot. Its first stored position is the anchor the source is measured from. \
         The stroke it decimates to holds at most {POINTS_PER_STROKE} positions"
    ))
}

fn radius() -> ParameterDescriptor {
    ParameterDescriptor::number(
        "radius",
        super::stroke::RADIUS_MIN,
        super::stroke::RADIUS_MAX,
    )
    .unit("content heights")
    .notes(
        "the brush radius in content heights, so a spot is round at any aspect ratio; a \
             resize keeps the stored positions",
    )
}

fn mode() -> ParameterDescriptor {
    ParameterDescriptor::enumeration("mode", ["heal", "clone"]).notes(
        "heal borrows the source's detail and adapts it to the destination's tone; clone copies \
         the source's pixels",
    )
}

fn feather() -> ParameterDescriptor {
    ParameterDescriptor::integer("feather", 0, 100)
        .notes("the share of the radius over which coverage falls to zero")
}

fn opacity() -> ParameterDescriptor {
    ParameterDescriptor::integer("opacity", 0, 100).notes("how strongly the correction blends")
}

fn source(axis: &str) -> ParameterDescriptor {
    ParameterDescriptor::number(format!("source-{axis}"), -1.0, 2.0).notes(
        "the absolute position the anchor samples from, in the same normalized coordinates as \
         points; send source-x and source-y together",
    )
}

fn operation() -> ParameterDescriptor {
    ParameterDescriptor::identity(OPERATION, IdentityKind::Correction)
        .required(true)
        .notes("the correction, correction-<n>, as add-correction answered it")
}

impl CorrectionsModule {
    pub(crate) fn new() -> Self {
        let not_preset = |action: ActionDescriptor| ActionDescriptor {
            preset: false,
            ..action
        };
        let actions = vec![
            not_preset(ActionDescriptor {
                parameters: vec![
                    points().required(true),
                    radius().required(true),
                    mode().default("heal"),
                    feather().default(50),
                    opacity().default(100),
                    source("x"),
                    source("y"),
                ],
                ..ActionDescriptor::new(
                    ADD,
                    "Add correction",
                    "Adds one Clone or Heal operation on top of the photograph's corrections and \
                     answers it as correction {operation, name, index, source-x, source-y, \
                     source-origin}. With source-x and source-y the source is set; without them \
                     the best automatic source is found off the catalog owner against this \
                     revision and stored as an ordinary offset, or the request is refused with \
                     data.reason no-source. A photograph holds at most 100 corrections.",
                )
            }),
            not_preset(ActionDescriptor {
                patch: true,
                parameters: vec![
                    operation(),
                    points(),
                    radius(),
                    mode(),
                    feather(),
                    opacity(),
                    source("x"),
                    source("y"),
                    ParameterDescriptor::enumeration("source-origin", ["auto", "set"]).notes(
                        "where the sent source came from, only with source-x and source-y; set \
                         when omitted",
                    ),
                ],
                ..ActionDescriptor::new(
                    UPDATE,
                    "Edit correction",
                    "Changes the sent fields of one correction. New points move its destination \
                     and keep its source where it is unless a source is sent too; a radius keeps \
                     the stored positions. Answers the correction as add-correction does.",
                )
            }),
            not_preset(ActionDescriptor {
                parameters: vec![operation()],
                ..ActionDescriptor::new(DELETE, "Delete correction", "Removes one correction.")
            }),
            not_preset(ActionDescriptor {
                parameters: vec![
                    operation(),
                    ParameterDescriptor::integer("index", 0, MAX_CORRECTIONS as i64 - 1)
                        .required(true)
                        .notes(
                            "the correction's new place in application order, 0 the bottom, \
                             counted after removing it",
                        ),
                ],
                ..ActionDescriptor::new(
                    REORDER,
                    "Reorder correction",
                    "Moves one correction in the order corrections apply in; it then samples \
                     the corrections below its new place.",
                )
            }),
            not_preset(ActionDescriptor::new(
                RESET,
                "Reset corrections",
                "Removes every correction; numbers already used are not reused.",
            )),
            not_preset(ActionDescriptor {
                accepts_candidate: true,
                parameters: vec![
                    ParameterDescriptor::identity("job_id", IdentityKind::Job)
                        .required(true)
                        .notes("the task job that published the candidate"),
                    ParameterDescriptor::integer("candidate", 0, 3)
                        .required(true)
                        .notes("the candidate's number in its job, as job.read lists it"),
                    opacity()
                        .default(100)
                        .notes("how strongly the patch blends"),
                ],
                ..ActionDescriptor::new(
                    ACCEPT,
                    "Accept patch",
                    "Adds a task's candidate patch on top of the photograph's corrections as a \
                     frozen patch and answers it as correction {operation, name, index}. The \
                     candidate must be live, belong to this photograph and have been generated \
                     against what a new top correction reads now (otherwise conflict, with \
                     data.reasons); its bytes are published as a repair-patch artifact off the \
                     catalog owner before the one entry is committed. A frozen patch keeps \
                     rendering when what it was generated against changes, and is then reported \
                     stale.",
                )
            }),
        ];
        let queries = vec![
            ActionDescriptor::new(
                LIST,
                "Corrections",
                "The photograph's corrections as stored, in application order: {limit, next, \
                 operations: [{operation, name, kind, index, points, radius, feather, opacity, \
                 source-x, source-y, source-origin}]}; a frozen patch is {operation, name, kind: \
                 patch, index, label, artifact, rect, opacity, provenance, stale?}, stale: \
                 {reasons} (source, dimensions, development, earlier-correction) when what it was \
                 generated against changed. Reads no pixel.",
            ),
            ActionDescriptor {
                parameters: vec![
                    ParameterDescriptor::identity(OPERATION, IdentityKind::Correction)
                        .notes("rank sources for this correction, over the corrections below it"),
                    points().notes(
                        "rank sources for a new correction on this path, over every correction",
                    ),
                    radius().notes("the new correction's radius, with points"),
                    ParameterDescriptor::integer("count", 1, MAX_SOURCES)
                        .default(MAX_SOURCES)
                        .notes("how many ranked sources to answer"),
                ],
                ..ActionDescriptor::new(
                    SOURCES,
                    "Correction sources",
                    "Ranks automatic sources without writing anything, exactly as add-correction \
                     ranks them (rank 1 is what an add stores): {outcome: found | no-source, \
                     candidates: [{rank, source-x, source-y, score}]}. Send operation, or points \
                     with radius. Read off the catalog owner.",
                )
            },
        ];
        Self {
            descriptor: ModuleDescriptor {
                id: MODULE.into(),
                title: "Corrections".into(),
                hint: Some("Clone and Heal".into()),
                effects: vec![EffectDescriptor {
                    single: true,
                    artifacts: true,
                    operations: Some(EffectOperations {
                        identity: IdentityKind::Correction,
                        max: MAX_CORRECTIONS as u32,
                    }),
                    ..EffectDescriptor::new(CORRECTIONS_EFFECT, EffectStage::Repair)
                }],
                actions,
                queries,
                controls: Vec::new(),
                reset: Some(ResetAction {
                    action: RESET.into(),
                    preset: Map::new(),
                }),
                canvas: Some(canvas()),
                developer: false,
                collapsed: true,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            },
        }
    }
}

/// The brush that paints corrections ([`CanvasInteraction::BrushMask`]): a new path drafts
/// `add-correction`; pins, source handles and the size handle draft `update-correction` on one
/// correction; `query.corrections` lists what to draw and `query.correction-sources` ranks sources.
fn canvas() -> CanvasInteraction {
    CanvasInteraction::BrushMask {
        action: ADD.into(),
        points: "points".into(),
        radius: "radius".into(),
        feather: Some("feather".into()),
        opacity: Some("opacity".into()),
        mode: Some("mode".into()),
        source: Some(BrushSource {
            x: "source-x".into(),
            y: "source-y".into(),
            query: SOURCES.into(),
        }),
        edit: Some(BrushEdit {
            action: UPDATE.into(),
            operation: OPERATION.into(),
            list: LIST.into(),
        }),
        title: "Corrections".into(),
        shortcut: Some("Q".into()),
        icon: Some("heal".into()),
    }
}

/// The stored payload of a repair layer, refused by name when it is not this effect's or format's.
pub(crate) fn stored(
    effect_id: &str,
    format: u32,
    payload: &Value,
) -> Result<RepairPayload, Error> {
    if effect_id != CORRECTIONS_EFFECT {
        return Err(Error::unavailable_effect(effect_id, &[]));
    }
    if format != crate::EFFECT_FORMAT {
        return Err(Error::incompatible(format!(
            "unsupported Corrections format {format}"
        )));
    }
    RepairPayload::parse(payload)
}

/// The repair layer of `layers`, read: its index and payload, or `None` when the stack holds none.
pub(crate) fn repair_layer(
    layers: &[Layer],
) -> Result<Option<(usize, &Layer, RepairPayload)>, Error> {
    layers
        .iter()
        .enumerate()
        .find(|(_, layer)| layer.effect_id == CORRECTIONS_EFFECT)
        .map(|(index, layer)| {
            stored(&layer.effect_id, layer.effect_format, &layer.payload)
                .map(|payload| (index, layer, payload))
        })
        .transpose()
}

impl ToolModule for CorrectionsModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        actions::parse(action_id, parameters)
    }

    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        actions::plan(input, context)
    }

    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error> {
        stored(effect_id, format, payload).map(drop)
    }

    fn describe(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<LayerReport, Error> {
        let payload = stored(effect_id, format, payload)?;
        let summary = match payload.operations.len() {
            0 => "No corrections".to_owned(),
            1 => "1 correction".to_owned(),
            count => format!("{count} corrections"),
        };
        Ok(LayerReport {
            summary,
            values: Map::new(),
            neutral: payload.operations.is_empty(),
        })
    }

    fn planned_label(
        &self,
        input: &ActionInput,
        before: &[Layer],
        layers: &[Layer],
        fallback: &str,
    ) -> String {
        labels::label(input, before, layers).unwrap_or_else(|| fallback.to_owned())
    }

    fn answer(
        &self,
        input: &ActionInput,
        layers: &[Layer],
        strokes: &crate::path::StrokeTable,
    ) -> Result<Map<String, Value>, Error> {
        actions::answer(input, layers, strokes)
    }

    fn collapses(&self, input: &ActionInput) -> bool {
        // Planned on the base, an add would mint another operation (and, without a source, read
        // pixels again): it never collapses.
        input.action_id != ADD
    }

    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        context: &StageContext<'_>,
    ) -> Result<Value, Error> {
        match query_id {
            LIST => query::corrections(context),
            SOURCES => query::sources(parameters, context),
            _ => Err(Error::validation(format!("unknown query {query_id}"))),
        }
    }

    fn neutral_payload(&self, _effect_id: &str) -> Value {
        RepairPayload::default().stored()
    }

    fn compile_with_strokes(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        at: crate::CompileStage,
        strokes: &crate::path::StrokeTable,
    ) -> Result<Processing, Error> {
        table::compile(&stored(effect_id, format, payload)?, strokes, &[], at)
    }

    /// A repair layer's rows are its strokes' positions, so it compiles through
    /// [`ToolModule::compile_with_strokes`]; asked without the strokes, only the empty layer can
    /// compile, and a layer holding operations is refused rather than drawn without them.
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        at: crate::CompileStage,
    ) -> Result<Processing, Error> {
        table::compile(
            &stored(effect_id, format, payload)?,
            &crate::path::StrokeTable::default(),
            &[],
            at,
        )
    }

    fn capabilities(&self) -> Option<&dyn crate::CapabilityModule> {
        Some(self)
    }
}

/// A repair layer that holds frozen patches lists their artifacts, so the host compiles it with
/// their verified bytes; it never calls a producer.
impl crate::CapabilityModule for CorrectionsModule {
    fn compile_bound(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        at: crate::CompileStage,
        artifacts: &[std::sync::Arc<crate::artifacts::PreparedArtifact>],
        strokes: &crate::path::StrokeTable,
    ) -> Result<Processing, Error> {
        table::compile(&stored(effect_id, format, payload)?, strokes, artifacts, at)
    }
}
