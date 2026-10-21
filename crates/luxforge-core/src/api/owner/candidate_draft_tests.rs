//! Candidate drafts through the catalog owner (`candidate_draft.rs`), driven by the developer
//! producer (`luxforge.candidates`) as a client drives them: the draft renders, on the CPU
//! reference, exactly the entry its accept commits; its `draft.set` parks while the candidate's
//! patch is read off the owner and holds one patch at a time; it is refused what the accept is
//! refused, leaving the draft as it was; and its commit is the accept. The lock a running fill
//! holds is tested with the fill itself (`modules/ai/tests.rs`).
use super::{
    PIXEL_READ_REQUIRED, PreviewRequest,
    accept_tests::{ACCEPT, Harness},
};
use crate::{
    BASIC_EFFECT, DraftId, ErrorKind, api::ApiFailure, api::ApiRequest,
    artifacts::PreparedArtifact, editor::mutation_json, modules::CORRECTIONS_EFFECT,
};
use luxforge_testbase::wait_for;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Weak},
    thread,
};

/// The Corrections module's candidate-accepting action.
const ACTION: &str = "accept-correction-patch";

/// `draft.begin` of the accepting action on the harness's photograph: the draft's identity.
fn begin(harness: &Harness) -> Value {
    harness.ok(
        "draft.begin",
        json!({"asset_id": harness.asset, "action": ACTION}),
    )["draft_id"]
        .clone()
}

fn set(harness: &Harness, draft: &Value, fields: Value) -> Result<Value, ApiFailure> {
    let response = harness.send_as(
        harness.client,
        "draft.set",
        json!({"draft_id": draft, "fields": fields}),
    );
    match response.error {
        None => Ok(response.result.expect("a result")),
        Some(error) => Err(error),
    }
}

fn draft_id(draft: &Value) -> DraftId {
    DraftId::parse(draft.as_str().unwrap()).unwrap()
}

/// The photograph's source prepared, as a client's first read prepares it.
fn prepared(harness: &Harness) {
    let sample = json!({"asset_id": harness.asset, "x": 0, "y": 0});
    for _ in 0..4 {
        let response = harness.send_as(harness.client, "render.sample", sample.clone());
        match response.error {
            None => return,
            Some(error) if error.code == "preparation-required" => {
                let job = json!(error.job_id.expect("a preparation names its job"));
                wait_for("the preparation", || {
                    let read = harness.ok("job.read", json!({"job_id": job}));
                    (read["status"] != "queued" && read["status"] != "running").then_some(())
                });
            }
            Some(error) => panic!("render.sample: {error:?}"),
        }
    }
    panic!("the source kept asking for preparation");
}

/// The reference's whole frame of the current entry, or of `draft`.
fn frame(harness: &Harness, draft: Option<&Value>) -> Result<Vec<u8>, crate::Error> {
    prepared(harness);
    let mut request = PreviewRequest::new(harness.client, harness.asset.clone());
    if let Some(draft) = draft {
        request = request.draft(draft_id(draft));
    }
    let job = harness.owner.preview_job(request)?;
    let raster = job.evaluation.source().render(
        job.evaluation.registry(),
        job.evaluation.entry().snapshot.id.clone(),
        job.evaluation.recipe(),
    )?;
    Ok(raster.rgba.to_vec())
}

/// The patch `draft`'s renders bind, from the draft's own preview.
fn drafted_patch(harness: &Harness, draft: &Value) -> Arc<PreparedArtifact> {
    let job = harness
        .owner
        .preview_job(
            PreviewRequest::new(harness.client, harness.asset.clone()).draft(draft_id(draft)),
        )
        .expect("the draft's preview");
    job.evaluation
        .recipe()
        .artifacts
        .iter()
        .next()
        .expect("the drafted patch")
        .clone()
}

fn draft_read(harness: &Harness, draft: &Value) -> Value {
    harness.ok("draft.read", json!({"draft_id": draft}))
}

#[test]
fn a_candidate_draft_renders_exactly_the_entry_its_accept_commits() {
    let harness = Harness::new("draft-exact");
    harness.ok(
        "edit.set-basic",
        json!({"asset_id": harness.asset, "mutation": mutation_json(harness.revision(), "basic"),
               "exposure": 0.6, "contrast": 25}),
    );
    let job = harness.generate(2, 4);
    let draft = begin(&harness);
    let unfilled = frame(&harness, None).unwrap();
    let answered = set(
        &harness,
        &draft,
        json!({"job_id": job, "candidate": 1, "opacity": 80}),
    )
    .unwrap();
    assert_eq!(answered["draft_revision"], 1);
    assert_eq!(answered["fields"]["candidate"], 1);
    let drafted = frame(&harness, Some(&draft)).unwrap();
    assert_ne!(drafted, unfilled, "the draft shows the candidate");
    let held: Weak<PreparedArtifact> = Arc::downgrade(&drafted_patch(&harness, &draft));
    let revision = harness.revision();
    let committed = harness.ok(
        "draft.commit",
        json!({"draft_id": draft, "mutation": mutation_json(revision, "commit")}),
    );
    assert_eq!(committed["outcome"], "applied", "{committed}");
    assert_eq!(committed["revision"], revision + 1);
    assert_eq!(
        committed["correction"],
        json!({"operation": "correction-1", "name": "Patch 1", "index": 0})
    );
    assert_eq!(harness.states(&job), ["live", "accepted"]);
    let history = harness.ok(
        "history.list",
        json!({"asset_id": harness.asset, "limit": 1}),
    );
    assert_eq!(history["entries"][0]["label"], "Add Patch 1");
    assert!(harness.ok("session.state", json!({}))["draft"].is_null());
    assert!(
        held.upgrade().is_none(),
        "the closed draft let its patch go"
    );
    // Buffer for buffer, the committed entry is the drafted one, its patch under Basic.
    assert_eq!(frame(&harness, None).unwrap(), drafted);
    let current = harness
        .owner
        .preview_job(PreviewRequest::new(harness.client, harness.asset.clone()))
        .unwrap();
    let effects: Vec<&str> = current
        .evaluation
        .recipe()
        .layers
        .iter()
        .map(|layer| layer.effect_id.as_str())
        .collect();
    let at = |effect: &str| effects.iter().position(|id| *id == effect).unwrap();
    assert!(at(CORRECTIONS_EFFECT) < at(BASIC_EFFECT), "{effects:?}");
}

#[test]
fn a_candidate_draft_set_parks_while_its_patch_is_read_and_holds_one_patch() {
    let harness = Harness::new("draft-parks");
    let job = harness.generate(2, 5);
    let draft = begin(&harness);
    // A caller that may not wait is told so, and nothing changes.
    let unparked = harness
        .owner
        .call_unparked(
            harness.client,
            ApiRequest {
                id: "unparked".into(),
                method: "draft.set".into(),
                params: json!({"draft_id": draft, "fields": {"job_id": job, "candidate": 0}}),
                token: None,
            },
        )
        .unwrap()
        .error
        .unwrap();
    assert_eq!(unparked.code, "not-ready");
    assert_eq!(unparked.data, Some(json!({"reason": PIXEL_READ_REQUIRED})));
    // The set waits while the source worker reads the patch, and the draft stays as it was.
    harness.hold();
    let call = {
        let (owner, client) = (harness.owner.clone(), harness.client);
        let params = json!({"draft_id": draft, "fields": {"job_id": job, "candidate": 0}});
        thread::spawn(move || {
            owner.call(
                client,
                ApiRequest {
                    id: "held-set".into(),
                    method: "draft.set".into(),
                    params,
                    token: None,
                },
            )
        })
    };
    wait_for("the patch to be read on the source worker", || {
        let list = harness.ok("activity.list", json!({}));
        list["active"]
            .as_array()
            .and_then(|active| {
                active.iter().find(|entry| {
                    entry["kind"] == "artifacts.read" && entry["label"] == "Reading candidate"
                })
            })
            .map(drop)
    });
    wait_for("the read to wait at the gate", || {
        Some(()).filter(|_| harness.gate.holding())
    });
    let waiting = draft_read(&harness, &draft);
    assert_eq!(
        (waiting["draft_revision"].clone(), waiting["fields"].clone()),
        (json!(0), json!({}))
    );
    harness.gate.open();
    let answered = call.join().unwrap().unwrap();
    assert!(answered.error.is_none(), "{:?}", answered.error);
    assert_eq!(answered.result.unwrap()["draft_revision"], 1);
    prepared(&harness);
    let first = Arc::downgrade(&drafted_patch(&harness, &draft));
    // Another candidate replaces the held patch; a field of the same one keeps it.
    set(&harness, &draft, json!({"candidate": 1})).unwrap();
    let second = Arc::downgrade(&drafted_patch(&harness, &draft));
    assert!(first.upgrade().is_none(), "one patch at a time");
    set(&harness, &draft, json!({"opacity": 50})).unwrap();
    assert!(Arc::ptr_eq(
        &second.upgrade().unwrap(),
        &drafted_patch(&harness, &draft)
    ));
    harness.ok("draft.cancel", json!({"draft_id": draft}));
    assert!(
        second.upgrade().is_none(),
        "a cancelled draft holds nothing"
    );
}

#[test]
fn a_candidate_draft_is_refused_what_its_accept_is_refused_and_stays_as_it_was() {
    let harness = Harness::new("draft-refusals");
    let job = harness.generate(3, 6);
    let draft = begin(&harness);
    let unchanged = |harness: &Harness, revision: u64, candidate: Value| {
        let read = draft_read(harness, &draft);
        assert_eq!(read["draft_revision"], revision, "{read}");
        assert_eq!(read["fields"]["candidate"], candidate, "{read}");
    };
    let refused = |fields: Value| set(&harness, &draft, fields).unwrap_err();
    let never = refused(json!({"job_id": job, "candidate": 3}));
    assert_eq!(never.code, "validation", "{never:?}");
    let unknown = refused(json!({"job_id": crate::JobId::new(), "candidate": 0}));
    assert_eq!(unknown.code, "validation", "{unknown:?}");
    unchanged(&harness, 0, Value::Null);
    set(&harness, &draft, json!({"job_id": job, "candidate": 2})).unwrap();
    // A released candidate the draft does not hold.
    harness.ok(
        "job.release-candidates",
        json!({"job_id": job, "candidates": [1], "mutation": {"request_id": "release", "actor": "test"}}),
    );
    let released = refused(json!({"candidate": 1}));
    assert_eq!(released.code, "conflict", "{released:?}");
    unchanged(&harness, 1, json!(2));
    // Another photograph's draft of this photograph's candidate.
    let other = harness.owner.register();
    let elsewhere = harness.send_as(
        other,
        "draft.begin",
        json!({"asset_id": harness.other, "action": ACTION}),
    );
    let elsewhere_draft = elsewhere.result.unwrap()["draft_id"].clone();
    let elsewhere = harness
        .send_as(
            other,
            "draft.set",
            json!({"draft_id": elsewhere_draft, "fields": {"job_id": job, "candidate": 0}}),
        )
        .error
        .unwrap();
    assert_eq!(elsewhere.code, "validation", "{elsewhere:?}");
    assert!(
        elsewhere.message.contains("another photograph"),
        "{elsewhere:?}"
    );
    // Candidate 0 accepted by an API client: candidate 2 was generated against the stack without
    // it, which a new top correction no longer reads. Its draft's renders and reapply are refused
    // as its accept is, with the accept's reasons.
    harness.ok(
        ACCEPT,
        harness.accept_params(&job, 0, harness.revision(), "accept-0"),
    );
    let render = frame(&harness, Some(&draft)).unwrap_err();
    assert_eq!(render.kind, ErrorKind::Conflict, "{render:?}");
    assert_eq!(
        render.data,
        Some(Box::new(json!({"reasons": ["earlier-correction"]})))
    );
    let reapply = harness.refused("draft.reapply", json!({"draft_id": draft}));
    assert_eq!(reapply.code, "conflict", "{reapply:?}");
    assert_eq!(
        reapply.data,
        Some(json!({"reasons": ["earlier-correction"]}))
    );
    unchanged(&harness, 1, json!(2));
}

#[test]
fn a_candidate_draft_commit_is_the_accept() {
    let harness = Harness::new("draft-commit");
    let job = harness.generate(2, 7);
    let draft = begin(&harness);
    set(&harness, &draft, json!({"job_id": job, "candidate": 0})).unwrap();
    let revision = harness.revision();
    let commit =
        |request: &str| json!({"draft_id": draft, "mutation": mutation_json(revision, request)});
    // A caller that may not wait.
    let unparked = harness
        .owner
        .call_unparked(
            harness.client,
            ApiRequest {
                id: "unparked".into(),
                method: "draft.commit".into(),
                params: commit("unparked"),
                token: None,
            },
        )
        .unwrap()
        .error
        .unwrap();
    assert_eq!(unparked.code, "not-ready");
    assert_eq!(unparked.data, Some(json!({"reason": "accept-publishes"})));
    // Released after its patch was read: the draft still renders it, and its commit is refused
    // as the accept is, keeping the draft.
    harness.ok(
        "job.release-candidates",
        json!({"job_id": job, "candidates": [0], "mutation": {"request_id": "release", "actor": "test"}}),
    );
    let unfilled = frame(&harness, None).unwrap();
    assert_ne!(frame(&harness, Some(&draft)).unwrap(), unfilled);
    let refused = harness.refused("draft.commit", commit("released"));
    assert_eq!(refused.code, "conflict", "{refused:?}");
    assert_eq!(draft_read(&harness, &draft)["fields"]["candidate"], 0);
    assert_eq!(harness.revision(), revision);
    // The other candidate commits, once: a retry is the first answer.
    set(&harness, &draft, json!({"candidate": 1})).unwrap();
    let committed = harness.ok("draft.commit", commit("commit"));
    assert_eq!(committed["outcome"], "applied", "{committed}");
    assert_eq!(harness.states(&job), ["released", "accepted"]);
    assert!(harness.ok("session.state", json!({}))["draft"].is_null());
    let retried = harness.ok("draft.commit", commit("commit"));
    assert_eq!(retried["deduplicated"], true);
    assert_eq!(retried["correction"], committed["correction"]);
    assert_eq!(harness.revision(), revision + 1);
    // An accept from a client with no candidate draft is unchanged.
    let job = harness.generate(1, 8);
    let accepted = harness.ok(
        ACCEPT,
        harness.accept_params(&job, 0, harness.revision(), "plain"),
    );
    assert_eq!(accepted["outcome"], "applied");
}
