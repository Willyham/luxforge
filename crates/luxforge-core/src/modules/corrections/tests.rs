//! Corrections through the one action, query and history paths, on a JPEG photograph in a real
//! catalog: payloads, answers, labels, no-ops, refusals, retries, strokes and history.
use super::{CORRECTIONS_EFFECT, payload::RepairPayload, stroke::CorrectionStroke};
use crate::editor::mutation;
use crate::{
    ActionResult, AssetId, EditorService, ErrorKind, MutationOutcome,
    path::{StrokeKind, quantize, steps_to_units},
};
use luxforge_testbase::paths::{jpeg as fixture, temp_path as temp};
use serde_json::{Value, json};
use std::path::PathBuf;

pub(super) struct Session {
    pub(super) service: EditorService,
    pub(super) asset: AssetId,
    pub(super) catalog: PathBuf,
    requests: usize,
}

impl Session {
    pub(super) fn new(name: &str) -> Self {
        let catalog = temp(name);
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        Self {
            service,
            asset,
            catalog,
            requests: 0,
        }
    }

    pub(super) fn revision(&self) -> u64 {
        self.service.state(&self.asset).unwrap().revision
    }

    /// Run `action` with `parameters` at the current revision, under a fresh request identity.
    pub(super) fn run(
        &mut self,
        action: &str,
        parameters: Value,
    ) -> Result<ActionResult, crate::Error> {
        self.requests += 1;
        let request = format!("request-{}", self.requests);
        self.run_as(&request, action, parameters)
    }

    pub(super) fn run_as(
        &mut self,
        request: &str,
        action: &str,
        parameters: Value,
    ) -> Result<ActionResult, crate::Error> {
        let revision = self.revision();
        self.service
            .run_action(&self.asset, mutation(revision, request), action, parameters)
    }

    pub(super) fn ok(&mut self, action: &str, parameters: Value) -> ActionResult {
        self.run(action, parameters)
            .unwrap_or_else(|error| panic!("{action}: {error:?}"))
    }

    pub(super) fn refused(&mut self, action: &str, parameters: Value) -> crate::Error {
        let before = self.revision();
        let error = self.run(action, parameters).unwrap_err();
        assert_eq!(self.revision(), before, "a refusal changes nothing");
        error
    }

    pub(super) fn label(&self) -> String {
        self.service.state(&self.asset).unwrap().current_entry.label
    }

    pub(super) fn labels(&self) -> Vec<String> {
        let page = self.service.history(&self.asset, None, 100).unwrap();
        page.entries.into_iter().map(|row| row.label).collect()
    }

    /// The repair layer's payload in the current entry.
    pub(super) fn payload(&self) -> Option<RepairPayload> {
        let state = self.service.state(&self.asset).unwrap();
        state
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .find(|layer| layer.effect_id == CORRECTIONS_EFFECT)
            .map(|layer| RepairPayload::parse(&layer.payload).unwrap())
    }

    pub(super) fn corrections(&self) -> Value {
        let entry = self.service.state(&self.asset).unwrap().current_entry.id;
        self.service
            .run_query(&self.asset, &entry, "corrections", json!({}))
            .unwrap()
    }
}

/// A spot at `(x, y)` sampled from `(sx, sy)`.
pub(super) fn spot(x: f64, y: f64, sx: f64, sy: f64) -> Value {
    json!({"points": [[x, y]], "radius": 0.02, "source-x": sx, "source-y": sy})
}

/// A grid-exact normalized coordinate.
fn on_grid(value: f64) -> f64 {
    steps_to_units(quantize(value))
}

#[test]
fn an_add_appends_one_operation_and_answers_its_correction() {
    let mut session = Session::new("corrections-add.sqlite");
    let added = session.ok("add-correction", spot(0.5, 0.5, 0.3, 0.4));
    assert_eq!(added.mutation.outcome, MutationOutcome::Applied);
    assert_eq!(
        added.answer["correction"],
        json!({
            "operation": "correction-1",
            "name": "Heal 1",
            "index": 0,
            "source-x": on_grid(0.3),
            "source-y": on_grid(0.4),
            "source-origin": "set",
        })
    );
    assert_eq!(session.label(), "Add Heal 1");
    let payload = session.payload().unwrap();
    assert_eq!(payload.next, 2);
    let operation = payload.operations[0].brush().unwrap();
    assert_eq!(
        (operation.id, operation.feather, operation.opacity),
        (1, 50, 100)
    );
    assert_eq!(
        operation.offset,
        [quantize(0.3) - quantize(0.5), quantize(0.4) - quantize(0.5)]
    );
    assert_eq!(payload.strokes, vec![operation.stroke.clone()]);
    let stroke = CorrectionStroke::capture(&[[0.5, 0.5]], 0.02).unwrap();
    assert_eq!(operation.stroke, stroke.id());

    let clone = session.ok(
        "add-correction",
        json!({"points": [[0.2, 0.6], [0.25, 0.62]], "radius": 0.01, "mode": "clone",
               "feather": 20, "opacity": 80, "source-x": 0.2, "source-y": 0.3}),
    );
    assert_eq!(clone.answer["correction"]["name"], "Clone 2");
    assert_eq!(clone.answer["correction"]["index"], 1);
    assert_eq!(session.label(), "Add Clone 2");
    // The stored payload is the frozen shape, operations in application order.
    let stored = session.payload().unwrap();
    assert_eq!(stored.operations.len(), 2);
    assert_eq!(stored.next, 3);
}

#[test]
fn a_retried_add_answers_the_same_correction_and_commits_nothing_more() {
    let mut session = Session::new("corrections-retry.sqlite");
    let revision = session.revision();
    let first = session
        .run_as("add", "add-correction", spot(0.5, 0.5, 0.3, 0.4))
        .unwrap();
    // The same request identity at the revision it was sent at.
    let again = session
        .service
        .run_action(
            &session.asset,
            mutation(revision, "add"),
            "add-correction",
            spot(0.5, 0.5, 0.3, 0.4),
        )
        .unwrap();
    assert!(again.mutation.deduplicated);
    assert_eq!(again.answer, first.answer);
    assert_eq!(session.payload().unwrap().operations.len(), 1);
    assert_eq!(session.labels(), vec!["Add Heal 1", "Original"]);
}

#[test]
fn updates_change_one_operation_with_a_label_per_field_and_detect_no_ops() {
    let mut session = Session::new("corrections-update.sqlite");
    session.ok("add-correction", spot(0.5, 0.5, 0.3, 0.4));
    let op = |fields: Value| {
        let mut fields = fields;
        fields["operation"] = json!("correction-1");
        fields
    };
    // Moving the destination keeps the source where it is.
    let moved = session.ok("update-correction", op(json!({"points": [[0.55, 0.45]]})));
    assert_eq!(session.label(), "Move Heal 1");
    assert_eq!(moved.answer["correction"]["source-x"], json!(on_grid(0.3)));
    assert_eq!(moved.answer["correction"]["source-y"], json!(on_grid(0.4)));
    assert_eq!(
        session.payload().unwrap().operations[0]
            .brush()
            .unwrap()
            .offset,
        [
            quantize(0.3) - quantize(0.55),
            quantize(0.4) - quantize(0.45)
        ]
    );
    session.ok(
        "update-correction",
        op(json!({"source-x": 0.7, "source-y": 0.6})),
    );
    assert_eq!(session.label(), "Move Heal 1 source");
    let auto = session.ok(
        "update-correction",
        op(json!({"source-x": 0.7, "source-y": 0.65, "source-origin": "auto"})),
    );
    assert_eq!(session.label(), "New source Heal 1");
    assert_eq!(auto.answer["correction"]["source-origin"], "auto");
    session.ok("update-correction", op(json!({"radius": 0.03})));
    assert_eq!(session.label(), "Resize Heal 1");
    session.ok("update-correction", op(json!({"opacity": 80})));
    assert_eq!(session.label(), "Heal 1 · Opacity 80");
    session.ok("update-correction", op(json!({"feather": 40})));
    assert_eq!(session.label(), "Heal 1 · Feather 40");
    session.ok("update-correction", op(json!({"mode": "clone"})));
    assert_eq!(session.label(), "Change Heal 1 to Clone");
    // Named by its kind before the change.
    session.ok(
        "update-correction",
        op(json!({"mode": "heal", "opacity": 70})),
    );
    assert_eq!(session.label(), "Edit Clone 1");
    let operation = session.payload().unwrap().operations[0]
        .brush()
        .unwrap()
        .clone();
    assert_eq!(
        (operation.feather, operation.opacity, operation.origin),
        (40, 70, super::payload::Origin::Auto)
    );

    // Every sent field equal to what is stored is a no-op with no entry.
    let entries = session.labels().len();
    for same in [
        json!({"opacity": 70}),
        json!({"mode": "heal", "feather": 40}),
        json!({"radius": 0.03}),
        json!({"points": [[0.55, 0.45]]}),
        json!({"source-x": 0.7, "source-y": 0.65, "source-origin": "auto"}),
    ] {
        let result = session.ok("update-correction", op(same.clone()));
        assert_eq!(result.mutation.outcome, MutationOutcome::NoOp, "{same}");
        assert_eq!(result.answer["correction"]["operation"], "correction-1");
    }
    assert_eq!(session.labels().len(), entries);
}

#[test]
fn delete_reorder_and_reset_keep_numbers_growing() {
    let mut session = Session::new("corrections-order.sqlite");
    for (index, x) in [0.2, 0.4, 0.6].into_iter().enumerate() {
        let added = session.ok("add-correction", spot(x, 0.5, x, 0.2));
        assert_eq!(added.answer["correction"]["index"], index);
    }
    session.ok(
        "reorder-correction",
        json!({"operation": "correction-3", "index": 0}),
    );
    assert_eq!(session.label(), "Reorder Heal 3");
    let order = |session: &Session| -> Vec<u32> {
        session
            .payload()
            .unwrap()
            .operations
            .iter()
            .map(|operation| operation.id())
            .collect()
    };
    assert_eq!(order(&session), vec![3, 1, 2]);
    let same = session.ok(
        "reorder-correction",
        json!({"operation": "correction-3", "index": 0}),
    );
    assert_eq!(same.mutation.outcome, MutationOutcome::NoOp);
    let outside = session.refused(
        "reorder-correction",
        json!({"operation": "correction-3", "index": 3}),
    );
    assert_eq!(outside.kind, ErrorKind::Validation);
    assert_eq!(
        outside.detail,
        "index 3 is outside the 3 corrections of this photograph"
    );
    session.ok("delete-correction", json!({"operation": "correction-1"}));
    assert_eq!(session.label(), "Delete Heal 1");
    assert_eq!(order(&session), vec![3, 2]);
    assert_eq!(
        session.payload().unwrap().strokes.len(),
        2,
        "the reserved strokes follow the operations"
    );
    session.ok("reset-corrections", json!({}));
    assert_eq!(session.label(), "Reset corrections");
    let reset = session.payload().unwrap();
    assert!(reset.operations.is_empty() && reset.strokes.is_empty());
    assert_eq!(reset.next, 4, "a reset keeps next");
    let empty = session.ok("reset-corrections", json!({}));
    assert_eq!(empty.mutation.outcome, MutationOutcome::NoOp);
    let added = session.ok("add-correction", spot(0.5, 0.5, 0.3, 0.4));
    assert_eq!(added.answer["correction"]["operation"], "correction-4");
}

#[test]
fn refusals_name_what_is_wrong_and_change_nothing() {
    let mut session = Session::new("corrections-refusals.sqlite");
    session.ok("add-correction", spot(0.5, 0.5, 0.3, 0.4));
    let cases = [
        (
            "update-correction",
            json!({"operation": "correction-9", "opacity": 10}),
            "no correction correction-9 in this photograph",
        ),
        (
            "delete-correction",
            json!({"operation": "correction-9"}),
            "no correction correction-9 in this photograph",
        ),
        (
            "add-correction",
            spot(0.5, 0.5, 0.5, 0.5),
            "a correction's source must differ from its destination",
        ),
        (
            "add-correction",
            spot(1.8, 0.5, 0.5, 0.5),
            "a correction's destination must meet the photograph's frame",
        ),
        (
            "add-correction",
            spot(0.5, 0.5, -0.5, 0.5),
            "a correction's source must meet the photograph's frame",
        ),
        (
            "add-correction",
            json!({"points": [[0.5, 0.5]], "radius": 0.02, "source-x": 0.3}),
            "send source-x and source-y together, or neither",
        ),
        (
            "update-correction",
            json!({"operation": "correction-1", "source-origin": "auto"}),
            "source-origin applies only with source-x and source-y",
        ),
        (
            "update-correction",
            json!({"operation": "correction-1", "points": [[0.3, 0.4]]}),
            "a correction's source must differ from its destination",
        ),
    ];
    for (action, parameters, expected) in cases {
        let error = session.refused(action, parameters.clone());
        assert_eq!(error.kind, ErrorKind::Validation, "{action} {parameters}");
        assert_eq!(error.detail, expected, "{action} {parameters}");
    }
    // Declared ranges and identities are the generic check's.
    for (action, parameters) in [
        (
            "add-correction",
            json!({"points": [[0.5, 0.5]], "radius": 0.3}),
        ),
        ("delete-correction", json!({"operation": "correction-01"})),
        ("delete-correction", json!({"operation": "mask-1"})),
        (
            "reorder-correction",
            json!({"operation": "correction-1", "index": 100}),
        ),
    ] {
        assert_eq!(
            session.refused(action, parameters).kind,
            ErrorKind::Validation
        );
    }
    assert_eq!(session.payload().unwrap().operations.len(), 1);
    assert_eq!(session.labels(), vec!["Add Heal 1", "Original"]);
}

#[test]
fn the_corrections_query_answers_the_layer_as_stored() {
    let mut session = Session::new("corrections-query.sqlite");
    let empty = session.corrections();
    assert_eq!(empty, json!({"limit": 100, "next": 1, "operations": []}));
    session.ok(
        "add-correction",
        json!({"points": [[0.2, 0.6], [0.3, 0.6]], "radius": 0.01, "mode": "clone",
               "opacity": 80, "source-x": 0.2, "source-y": 0.3}),
    );
    let listed = session.corrections();
    assert_eq!(listed["next"], 2);
    assert_eq!(
        listed["operations"][0],
        json!({
            "operation": "correction-1",
            "name": "Clone 1",
            "kind": "clone",
            "index": 0,
            "points": [[on_grid(0.2), on_grid(0.6)], [on_grid(0.3), on_grid(0.6)]],
            "radius": on_grid(0.01),
            "feather": 50,
            "opacity": 80,
            "source-x": on_grid(0.2),
            "source-y": on_grid(0.3),
            "source-origin": "set",
        })
    );
}

/// Render the current entry, preparing the source it asks for first, as a client does.
fn rendered(service: &mut EditorService, asset: &AssetId) -> Result<crate::Raster, crate::Error> {
    match service.render_current(asset) {
        Err(error) if error.kind == ErrorKind::PreparationRequired => {
            let needs = error
                .needs()
                .expect("a refusal names what it needs")
                .clone();
            service.prepare(&needs)?;
            service.render_current(asset)
        }
        result => result,
    }
}

/// The number of strokes the catalog's store holds.
fn stored_strokes(service: &EditorService) -> i64 {
    service
        .connection
        .query_row("SELECT count(*) FROM strokes", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn history_versions_and_reopen_keep_every_operation_and_its_stroke() {
    let mut session = Session::new("corrections-history.sqlite");
    session.ok("add-correction", spot(0.5, 0.5, 0.3, 0.4));
    let one = session
        .service
        .state(&session.asset)
        .unwrap()
        .current_entry
        .id;
    session.ok("add-correction", spot(0.2, 0.2, 0.6, 0.6));
    // Feather and opacity write no stroke; moving writes one more.
    session.ok(
        "update-correction",
        json!({"operation": "correction-1", "opacity": 50}),
    );
    assert_eq!(stored_strokes(&session.service), 2, "each stroke once");
    session.ok(
        "update-correction",
        json!({"operation": "correction-1", "points": [[0.55, 0.5]]}),
    );
    assert_eq!(stored_strokes(&session.service), 3);
    let latest = session.corrections();
    let current = session
        .service
        .state(&session.asset)
        .unwrap()
        .current_entry
        .id;

    // History preview: an earlier entry answers its own operations.
    let earlier = session
        .service
        .run_query(&session.asset, &one, "corrections", json!({}))
        .unwrap();
    assert_eq!(earlier["operations"].as_array().unwrap().len(), 1);
    assert_eq!(earlier["next"], 2);

    // Undo and redo.
    let revision = session.revision();
    session
        .service
        .undo(&session.asset, mutation(revision, "undo"))
        .unwrap();
    assert_eq!(
        session.payload().unwrap().operations[0]
            .brush()
            .unwrap()
            .opacity,
        50
    );
    assert_eq!(
        session.payload().unwrap().operations[0]
            .brush()
            .unwrap()
            .offset,
        [quantize(0.3) - quantize(0.5), quantize(0.4) - quantize(0.5)]
    );
    let revision = session.revision();
    session
        .service
        .redo(&session.asset, mutation(revision, "redo"))
        .unwrap();
    assert_eq!(session.corrections(), latest);

    // A version names the latest entry; restoring the first entry and then the version.
    session
        .service
        .create_version(&session.asset, "Cleaned", None, "test")
        .unwrap();
    let revision = session.revision();
    session
        .service
        .restore(&session.asset, mutation(revision, "restore-one"), &one)
        .unwrap();
    assert_eq!(session.payload().unwrap().operations.len(), 1);
    let revision = session.revision();
    session
        .service
        .restore(
            &session.asset,
            mutation(revision, "restore-version"),
            &current,
        )
        .unwrap();
    assert_eq!(session.corrections(), latest);
    assert_eq!(
        stored_strokes(&session.service),
        3,
        "restores write no stroke"
    );

    // Reopened, every entry reads its strokes back from the store.
    let Session {
        service,
        asset,
        catalog,
        ..
    } = session;
    drop(service);
    let reopened = EditorService::open(&catalog).unwrap();
    let entry = reopened.state(&asset).unwrap().current_entry.id;
    assert_eq!(
        reopened
            .run_query(&asset, &entry, "corrections", json!({}))
            .unwrap(),
        latest
    );
    assert_eq!(
        reopened
            .run_query(&asset, &one, "corrections", json!({}))
            .unwrap(),
        earlier
    );
}

#[test]
fn a_missing_or_corrupt_stroke_is_refused_naming_its_operation_and_history_stays_readable() {
    for corrupt in [false, true] {
        let mut session = Session::new(&format!("corrections-broken-{corrupt}.sqlite"));
        session.ok("add-correction", spot(0.5, 0.5, 0.3, 0.4));
        let stroke = session.payload().unwrap().operations[0]
            .brush()
            .unwrap()
            .stroke
            .clone();
        let Session {
            service,
            asset,
            catalog,
            ..
        } = session;
        let connection = &service.connection;
        connection
            .execute(
                "DELETE FROM strokes WHERE id=?1",
                rusqlite::params![stroke.as_str()],
            )
            .unwrap();
        if corrupt {
            // A mask brush stroke's bytes under this address: the wrong type, and not its content.
            connection
                .execute(
                    "INSERT INTO strokes (id,stroke_json) VALUES (?1,?2)",
                    rusqlite::params![
                        stroke.as_str(),
                        r#"{"points":[[8192,8192]],"size":328,"feather":50,"flow":100,"erase":false}"#
                    ],
                )
                .unwrap();
        }
        drop(service);
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.state(&asset).unwrap();
        assert_eq!(state.current_entry.label, "Add Heal 1");
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 2);
        let entry = state.current_entry.id;
        let refused = service
            .run_query(&asset, &entry, "corrections", json!({}))
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Incompatible, "{refused:?}");
        assert!(
            refused
                .detail
                .contains("referenced by operation correction-1 of Corrections"),
            "{}",
            refused.detail
        );
        let render = rendered(&mut service, &asset).unwrap_err();
        assert_eq!(render.kind, ErrorKind::Incompatible);
        assert!(
            render
                .detail
                .contains("referenced by operation correction-1 of Corrections"),
            "{}",
            render.detail
        );
        let mut session = Session {
            service,
            asset,
            catalog,
            requests: 1,
        };
        let edit = session.refused(
            "update-correction",
            json!({"operation": "correction-1", "opacity": 10}),
        );
        assert_eq!(edit.kind, ErrorKind::Incompatible, "{edit:?}");
    }
}

#[test]
fn a_corrected_photograph_renders_its_table_and_an_emptied_layer_renders_nothing() {
    let mut session = Session::new("corrections-render.sqlite");
    let before = session.service.render_current(&session.asset).unwrap();
    session.ok("add-correction", spot(0.5, 0.5, 0.3, 0.4));
    session.ok(
        "add-correction",
        json!({"points": [[0.2, 0.6], [0.3, 0.62]], "radius": 0.01, "mode": "clone",
               "source-x": 0.6, "source-y": 0.3}),
    );
    // The layer compiles to the host's repair table, one row per operation in order, each with
    // its stroke's stored positions.
    let state = session.service.state(&session.asset).unwrap();
    let recipe = &state.current_entry.snapshot.recipe;
    let layer = recipe
        .layers
        .iter()
        .find(|layer| layer.effect_id == CORRECTIONS_EFFECT)
        .unwrap();
    let compiled = crate::ToolModule::compile_with_strokes(
        &super::CorrectionsModule::new(),
        &layer.effect_id,
        layer.effect_format,
        &layer.payload,
        crate::CompileStage::exact(crate::Stage {
            width: before.width,
            height: before.height,
        }),
        &recipe.strokes,
    )
    .unwrap();
    let crate::Processing::Repair(table) = compiled else {
        panic!("a repair layer compiles to a repair table");
    };
    let rows: Vec<(u32, crate::modules::RepairKind, usize)> = table
        .operations()
        .iter()
        .map(|row| (row.id, row.kind, row.points.len()))
        .collect();
    assert_eq!(
        rows,
        vec![
            (1, crate::modules::RepairKind::Heal, 1),
            (2, crate::modules::RepairKind::Clone, 2)
        ]
    );
    let rendered = session.service.render_current(&session.asset).unwrap();
    assert_eq!(
        (rendered.width, rendered.height),
        (before.width, before.height)
    );
    // An empty repair layer changes nothing and renders the photograph as it was.
    session.ok("reset-corrections", json!({}));
    let after = session.service.render_current(&session.asset).unwrap();
    let differing = before
        .rgba
        .iter()
        .zip(after.rgba.iter())
        .filter(|(a, b)| a != b)
        .count();
    let worst = before
        .rgba
        .iter()
        .zip(after.rgba.iter())
        .map(|(a, b)| a.abs_diff(*b))
        .max();
    assert_eq!(
        (differing, worst, after.width, after.height),
        (0, Some(0), before.width, before.height)
    );
}

#[test]
fn the_hundred_and_first_add_is_refused_before_and_at_commit() {
    let mut session = Session::new("corrections-limit.sqlite");
    for index in 0..100 {
        let x = 0.05 + 0.009 * index as f64;
        session.ok("add-correction", spot(x, 0.5, x, 0.2));
    }
    assert_eq!(session.payload().unwrap().operations.len(), 100);
    let refused = session.refused("add-correction", spot(0.5, 0.8, 0.5, 0.3));
    assert_eq!(refused.kind, ErrorKind::ResourceLimit);
    assert_eq!(refused.detail, "100 corrections is this photograph's limit");
    assert_eq!(refused.data.as_deref(), Some(&json!({"limit": 100})));
    // draft.begin refuses the same before any draft opens; an edit of one still begins.
    let begun = session
        .service
        .draft_target(&session.asset, "add-correction", crate::DraftTarget::new())
        .unwrap_err();
    assert_eq!(begun.detail, refused.detail);
    let target = crate::DraftTarget::from([("operation".to_owned(), "correction-7".to_owned())]);
    session
        .service
        .draft_target(&session.asset, "update-correction", target)
        .unwrap();
    let unknown = crate::DraftTarget::from([("operation".to_owned(), "correction-700".to_owned())]);
    assert_eq!(
        session
            .service
            .draft_target(&session.asset, "update-correction", unknown)
            .unwrap_err()
            .detail,
        "no correction correction-700 in this photograph"
    );
}

#[test]
fn a_draft_previews_a_set_source_and_plans_nothing_for_an_automatic_one() {
    let mut session = Session::new("corrections-draft.sqlite");
    let revision = session.revision();
    let mut draft = crate::Draft::new("add-correction", session.asset.clone(), revision);
    draft.fields = json!({"points": [[0.5, 0.5]], "radius": 0.02})
        .as_object()
        .unwrap()
        .clone();
    let (automatic, _) = session
        .service
        .draft_recipe(&session.asset, &draft)
        .unwrap();
    let current = session.service.state(&session.asset).unwrap().current_entry;
    assert_eq!(automatic, current.snapshot.recipe, "no read, no change");
    draft.fields.insert("source-x".into(), json!(0.3));
    draft.fields.insert("source-y".into(), json!(0.4));
    let (set, _) = session
        .service
        .draft_recipe(&session.asset, &draft)
        .unwrap();
    let committed = session.ok("add-correction", spot(0.5, 0.5, 0.3, 0.4));
    assert_eq!(committed.answer["correction"]["operation"], "correction-1");
    let current = session.service.state(&session.asset).unwrap().current_entry;
    let layers = |recipe: &crate::Recipe| -> Vec<Value> {
        recipe
            .layers
            .iter()
            .map(|layer| layer.payload.clone())
            .collect()
    };
    assert_eq!(
        layers(&set),
        layers(&current.snapshot.recipe),
        "the draft equals its commit"
    );
}

#[test]
fn consecutive_updates_of_one_field_collapse_and_adds_never_do() {
    let mut session = Session::new("corrections-collapse.sqlite");
    session.service.set_auto_collapse(true);
    session.ok("add-correction", spot(0.5, 0.5, 0.3, 0.4));
    session.ok("add-correction", spot(0.2, 0.5, 0.2, 0.3));
    for opacity in [90, 70, 60] {
        session.ok(
            "update-correction",
            json!({"operation": "correction-1", "opacity": opacity}),
        );
    }
    assert_eq!(
        session.labels(),
        vec![
            "Heal 1 · Opacity 60",
            "Add Heal 2",
            "Add Heal 1",
            "Original"
        ]
    );
    // Returning the field to where the run began writes nothing and answers the operation.
    let back = session.ok(
        "update-correction",
        json!({"operation": "correction-1", "opacity": 100}),
    );
    assert_eq!(back.answer["correction"]["operation"], "correction-1");
    assert_eq!(
        session.labels(),
        vec!["Add Heal 2", "Add Heal 1", "Original"]
    );
}

#[test]
fn presets_never_carry_corrections_and_an_ordinary_preset_keeps_them() {
    let mut session = Session::new("corrections-presets.sqlite");
    session.ok("add-correction", spot(0.5, 0.5, 0.3, 0.4));
    let registry = session.service.registry().clone();
    for action in [
        "add-correction",
        "update-correction",
        "delete-correction",
        "reorder-correction",
        "reset-corrections",
    ] {
        assert!(registry.settings_action(action).is_err(), "{action}");
    }
    let refused = session.refused(
        "apply-settings",
        json!({"origin": {"kind": "preset", "name": "Spots"},
               "settings": {"update-correction": {"operation": "correction-1", "opacity": 10}}}),
    );
    assert_eq!(refused.kind, ErrorKind::Validation, "{refused:?}");
    let before = session.payload().unwrap();
    session.ok(
        "apply-settings",
        json!({"origin": {"kind": "preset", "name": "Bright"},
               "settings": {"set-basic": {"exposure": 0.5}}}),
    );
    assert_eq!(session.label(), "Preset: Bright");
    assert_eq!(
        session.payload().unwrap(),
        before,
        "the repair layer is kept as it was"
    );
    assert_eq!(
        session.corrections()["operations"][0]["operation"],
        "correction-1"
    );
}

#[test]
fn an_add_without_a_source_stores_rank_one_of_the_query_once() {
    let mut session = Session::new("corrections-auto.sqlite");
    let entry = session
        .service
        .state(&session.asset)
        .unwrap()
        .current_entry
        .id;
    let path = json!([[0.5, 0.5], [0.52, 0.51]]);
    let ranked = session
        .service
        .run_query(
            &session.asset,
            &entry,
            "correction-sources",
            json!({"points": path, "radius": 0.02}),
        )
        .unwrap();
    assert_eq!(ranked["outcome"], "found", "{ranked}");
    let candidates = ranked["candidates"].as_array().unwrap();
    assert!(!candidates.is_empty() && candidates.len() <= 8);
    for (index, candidate) in candidates.iter().enumerate() {
        assert_eq!(candidate["rank"], index + 1);
    }
    // The ranking is deterministic, and count takes its head.
    let again = session
        .service
        .run_query(
            &session.asset,
            &entry,
            "correction-sources",
            json!({"points": path, "radius": 0.02, "count": 2}),
        )
        .unwrap();
    assert_eq!(again["candidates"].as_array().unwrap()[..], candidates[..2]);

    let added = session.ok("add-correction", json!({"points": path, "radius": 0.02}));
    let correction = &added.answer["correction"];
    assert_eq!(correction["source-origin"], "auto");
    assert_eq!(correction["source-x"], candidates[0]["source-x"]);
    assert_eq!(correction["source-y"], candidates[0]["source-y"]);
    let payload = session.payload().unwrap();
    let operation = payload.operations[0].brush().unwrap();
    assert_eq!(operation.origin, super::payload::Origin::Auto);
    // The answer is exactly on the grid, so it is the stored offset from the anchor.
    let x = correction["source-x"].as_f64().unwrap();
    assert_eq!(quantize(x) - quantize(0.5), operation.offset[0]);

    // The same operation ranks over the stage below it, which is the stage the add read.
    let entry = session
        .service
        .state(&session.asset)
        .unwrap()
        .current_entry
        .id;
    let by_operation = session
        .service
        .run_query(
            &session.asset,
            &entry,
            "correction-sources",
            json!({"operation": "correction-1"}),
        )
        .unwrap();
    assert_eq!(by_operation["candidates"], ranked["candidates"]);
    // `/`: the next rank, sent as an automatic source.
    session.ok(
        "update-correction",
        json!({"operation": "correction-1", "source-x": candidates[1]["source-x"],
               "source-y": candidates[1]["source-y"], "source-origin": "auto"}),
    );
    assert_eq!(session.label(), "New source Heal 1");
}

#[test]
fn no_automatic_source_is_an_explicit_refusal_that_commits_nothing() {
    let mut session = Session::new("corrections-no-source.sqlite");
    // A quarter-height spot at the centre: every candidate ring leaves the frame.
    let entry = session
        .service
        .state(&session.asset)
        .unwrap()
        .current_entry
        .id;
    let ranked = session
        .service
        .run_query(
            &session.asset,
            &entry,
            "correction-sources",
            json!({"points": [[0.5, 0.5]], "radius": 0.25}),
        )
        .unwrap();
    assert_eq!(ranked, json!({"outcome": "no-source", "candidates": []}));
    let refused = session.refused(
        "add-correction",
        json!({"points": [[0.5, 0.5]], "radius": 0.25}),
    );
    assert_eq!(refused.kind, ErrorKind::Validation);
    assert_eq!(
        refused.detail,
        "No automatic source fits; send source-x and source-y"
    );
    assert_eq!(
        refused.data.as_deref(),
        Some(&json!({"reason": "no-source"}))
    );
    assert!(session.payload().is_none());
    // Exactly one of operation, or points with radius.
    for parameters in [
        json!({}),
        json!({"points": [[0.5, 0.5]]}),
        json!({"operation": "correction-1", "points": [[0.5, 0.5]], "radius": 0.02}),
    ] {
        let error = session
            .service
            .run_query(&session.asset, &entry, "correction-sources", parameters)
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
    }
}
