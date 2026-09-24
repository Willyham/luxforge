//! The owner's request table: the first answer of every mutation that carries the `request`
//! envelope, the methods that change nothing with a revision — the preset library, versions, the
//! catalog's import and artifact collection, and module permissions, activation, resources and
//! capability jobs.
//!
//! A retry, the same `request_id` in the same scope with the same method and parameters, is
//! answered with the first answer marked `deduplicated: true`, and its handler does not run, so it
//! changes nothing and emits no event. The same `request_id` with a different input is a
//! `conflict`. A mutation that has a revision — an asset's edits and history, or a module's settings
//! — is deduplicated by the store that holds the revision, durably and in the same write as the
//! change, because a retry after a restart must be answered rather than refused as stale. The
//! methods here have no revision, and every one of them is safe to run again after a restart: it is
//! a no-op, joins the work already done, or fails visibly on the uniqueness it would break. So the
//! table lives with the owner, holds only successful answers, and is bounded.
use crate::{Error, ErrorKind, MutationRequest};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

/// The most answers the table keeps; the oldest is forgotten first.
const REQUESTS: usize = 256;
/// The most bytes of answers the table keeps. An answer larger than this is not kept, so its retry
/// runs again, which every method here allows.
const REQUEST_BYTES: usize = 8 * 1024 * 1024;

/// One request's identity: its scope, its envelope's `request_id` and a hash of its input.
pub(super) struct RequestKey {
    /// The method's family: its name without the last segment, so `preset.create` and
    /// `preset.delete` share `preset`.
    scope: String,
    request_id: String,
    /// SHA-256 of the method name and every parameter, the envelope included.
    input: [u8; 32],
}

impl RequestKey {
    /// The key of one request, or `None` when it carries no well-formed envelope, which its handler
    /// then refuses by name.
    pub(super) fn of(method: &str, params: &Value) -> Option<Self> {
        let mutation = MutationRequest::deserialize(params.get("mutation")?).ok()?;
        let scope = method.rsplit_once('.').map_or(method, |(family, _)| family);
        let mut hash = Sha256::new();
        hash.update(method.as_bytes());
        hash.update([0]);
        hash.update(serde_json::to_vec(params).ok()?);
        Some(Self {
            scope: scope.to_owned(),
            request_id: mutation.request_id,
            input: hash.finalize().into(),
        })
    }

    fn names(&self, other: &Self) -> bool {
        self.scope == other.scope && self.request_id == other.request_id
    }
}

struct Answered {
    key: RequestKey,
    answer: Value,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct RequestTable {
    /// Oldest first.
    answered: VecDeque<Answered>,
    bytes: usize,
}

impl RequestTable {
    /// The first answer to this request, marked `deduplicated`, when it was answered before.
    /// `O(REQUESTS)`, on the owner thread, once per revision-less mutation.
    pub(super) fn answered(&self, key: &RequestKey) -> Result<Option<Value>, Error> {
        let Some(first) = self.answered.iter().find(|entry| entry.key.names(key)) else {
            return Ok(None);
        };
        if first.key.input != key.input {
            return Err(Error::new(
                ErrorKind::Conflict,
                "request_id was already used with different input",
            ));
        }
        let mut answer = first.answer.clone();
        mark(&mut answer, true);
        Ok(Some(answer))
    }

    /// Keep a first answer, marked as not deduplicated, which is how the client receives it too.
    pub(super) fn record(&mut self, key: RequestKey, answer: &mut Value) {
        mark(answer, false);
        let bytes = serde_json::to_vec(&*answer).map_or(usize::MAX, |encoded| encoded.len())
            + key.scope.len()
            + key.request_id.len();
        if bytes > REQUEST_BYTES {
            return;
        }
        self.bytes += bytes;
        self.answered.push_back(Answered {
            key,
            answer: answer.clone(),
            bytes,
        });
        while self.answered.len() > REQUESTS || self.bytes > REQUEST_BYTES {
            if let Some(oldest) = self.answered.pop_front() {
                self.bytes -= oldest.bytes;
            }
        }
    }
}

fn mark(answer: &mut Value, deduplicated: bool) {
    if let Value::Object(fields) = answer {
        fields.insert("deduplicated".into(), Value::Bool(deduplicated));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(method: &str, request_id: &str, name: &str) -> RequestKey {
        RequestKey::of(
            method,
            &json!({"name": name, "mutation": {"request_id": request_id, "actor": "test"}}),
        )
        .expect("a well-formed envelope")
    }

    #[test]
    fn a_retry_gets_the_first_answer_and_other_input_is_a_conflict() {
        let mut table = RequestTable::default();
        let first = key("preset.create", "one", "Soft");
        assert!(table.answered(&first).unwrap().is_none());
        let mut answer = json!({"preset": {"name": "Soft"}});
        table.record(first, &mut answer);
        assert_eq!(answer["deduplicated"], json!(false));
        let retry = table
            .answered(&key("preset.create", "one", "Soft"))
            .unwrap()
            .expect("the retry is answered");
        assert_eq!(
            retry,
            json!({"preset": {"name": "Soft"}, "deduplicated": true})
        );
        let error = table
            .answered(&key("preset.create", "one", "Hard"))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Conflict);
        // Another method of the same family is the same request identity with other input.
        assert_eq!(
            table
                .answered(&key("preset.delete", "one", "Soft"))
                .unwrap_err()
                .kind,
            ErrorKind::Conflict
        );
        // Another family is another scope.
        assert!(
            table
                .answered(&key("version.create", "one", "Soft"))
                .unwrap()
                .is_none()
        );
        assert!(
            RequestKey::of("preset.create", &json!({"name": "Soft"})).is_none(),
            "a request without an envelope has no key; its handler refuses it"
        );
    }

    #[test]
    fn the_table_keeps_the_newest_answers_within_its_bounds() {
        let mut table = RequestTable::default();
        for index in 0..REQUESTS + 8 {
            table.record(
                key("preset.create", &format!("r{index}"), "x"),
                &mut json!({"index": index}),
            );
        }
        assert_eq!(table.answered.len(), REQUESTS);
        assert!(
            table
                .answered(&key("preset.create", "r0", "x"))
                .unwrap()
                .is_none(),
            "the oldest is forgotten"
        );
        assert!(
            table
                .answered(&key("preset.create", &format!("r{}", REQUESTS + 7), "x"))
                .unwrap()
                .is_some()
        );
        let mut huge = json!({"text": "x".repeat(REQUEST_BYTES)});
        table.record(key("preset.import", "huge", "x"), &mut huge);
        assert!(
            table
                .answered(&key("preset.import", "huge", "x"))
                .unwrap()
                .is_none(),
            "an answer larger than the table is not kept"
        );
        assert!(table.bytes <= REQUEST_BYTES);
    }
}
