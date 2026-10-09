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

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.catalog);
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
    let operation = &payload.operations[0];
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
        session.payload().unwrap().operations[0].offset,
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
    let operation = session.payload().unwrap().operations[0].clone();
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
            .map(|operation| operation.id)
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
