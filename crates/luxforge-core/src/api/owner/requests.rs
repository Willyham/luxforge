//! Bounded retry answers for mutations whose persistence lives outside the catalog.
//! Catalog mutations declare `Retries::Durable` and use the independent catalog request table;
//! ordinary asset edits keep their existing transactional asset-scoped request log. Session-only
//! and external-store mutations use this 256-entry, 8-MiB optimization, whose guarantees end on
//! eviction or restart. Keys and canonical input matching are shared by both stores.
#[cfg(test)]
use crate::ErrorKind;
use crate::{Error, Mutation, MutationRequest, capabilities::redact::redacted};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

/// The most answers the table keeps; the oldest is forgotten first.
const REQUESTS: usize = 256;
/// The most bytes of answers the table keeps. An answer larger than this is not kept, so its retry
/// runs again unless its handler supplies separate durable matching.
const REQUEST_BYTES: usize = 8 * 1024 * 1024;

/// One request's identity: its scope, its envelope's `request_id` and a hash of its input.
#[derive(Clone, Debug)]
pub(crate) struct RequestKey {
    /// The method's family: its name without the last segment, so `preset.create` and
    /// `preset.delete` share `preset`.
    pub(crate) scope: String,
    pub(crate) request_id: String,
    /// SHA-256 of the method name and every parameter, the envelope included, with a secret's value
    /// redacted: the table never holds a hash of a secret, so a secret write's retry is matched by
    /// the setting alone.
    pub(crate) input: [u8; 32],
    pub(crate) method: String,
    /// Dispatch's cache policy, carried through deferred handlers; not part of request identity.
    pub(super) cache: bool,
}

impl RequestKey {
    /// The key of one request, or `None` when it carries no well-formed envelope of either kind,
    /// which its handler then refuses by name.
    pub(super) fn of(method: &str, params: &Value) -> Option<Self> {
        let envelope = params.get("mutation")?;
        let request_id = MutationRequest::deserialize(envelope)
            .map(|mutation| mutation.request_id)
            .or_else(|_| Mutation::deserialize(envelope).map(|mutation| mutation.request_id))
            .ok()?;
        let scope = method.rsplit_once('.').map_or(method, |(family, _)| family);
        let mut hash = Sha256::new();
        hash.update(method.as_bytes());
        hash.update([0]);
        hash.update(serde_json::to_vec(&redacted(method, params)).ok()?);
        Some(Self {
            scope: scope.to_owned(),
            method: method.to_owned(),
            request_id,
            input: hash.finalize().into(),
            cache: true,
        })
    }

    pub(super) fn durable(&self) -> crate::editor::catalog_requests::Key {
        crate::editor::catalog_requests::Key {
            scope: self.scope.clone(),
            request_id: self.request_id.clone(),
            input: self.input,
            method: self.method.clone(),
        }
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
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.answered.len()
    }

    /// The first answer to this request, marked `deduplicated`, when it was answered before.
    /// `O(REQUESTS)`, on the owner thread, once per mutation whose retries the owner answers.
    pub(super) fn answered(&self, key: &RequestKey) -> Result<Option<Value>, Error> {
        let Some(first) = self.answered.iter().find(|entry| entry.key.names(key)) else {
            return Ok(None);
        };
        if first.key.input != key.input {
            return Err(Error::conflict(
                "request_id was already used with different input",
            ));
        }
        let mut answer = first.answer.clone();
        mark(&mut answer, true);
        Ok(Some(answer))
    }

    /// Keep a first answer, marked as not deduplicated, which is how the client receives it too —
    /// unless its handler already answered it as a retry: a request a durable store recorded before
    /// this process started (the library journal answers a library change's retry after a
    /// restart), which is a retry however new it is to this table.
    pub(super) fn record(&mut self, key: RequestKey, answer: &mut Value) {
        mark_first(answer);
        if !key.cache {
            return;
        }
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

/// First answers acquire their marker without serializing or cloning an already durable result.
pub(super) fn mark_first(answer: &mut Value) {
    if answer.get("deduplicated") != Some(&Value::Bool(true)) {
        mark(answer, false);
    }
}

pub(super) fn mark(answer: &mut Value, deduplicated: bool) {
    if let Value::Object(fields) = answer {
        fields.insert("deduplicated".into(), Value::Bool(deduplicated));
        if let Some(change) = fields.get_mut("change") {
            mark(change, deduplicated);
        }
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

    /// A handler that answered a request as a retry, from a store that outlives the process, keeps
    /// that answer's `deduplicated: true`; every other first answer is marked not deduplicated.
    #[test]
    fn a_first_answer_its_handler_found_to_be_a_retry_stays_deduplicated() {
        let mut table = RequestTable::default();
        let mut durable = json!({"change": 3, "deduplicated": true});
        table.record(key("pick.set", "before-restart", "a"), &mut durable);
        assert_eq!(durable["deduplicated"], json!(true));
        let retry = table
            .answered(&key("pick.set", "before-restart", "a"))
            .unwrap()
            .expect("the retry is answered");
        assert_eq!(retry["deduplicated"], json!(true));
        let mut fresh = json!({"change": 4, "deduplicated": false});
        table.record(key("pick.set", "new", "a"), &mut fresh);
        assert_eq!(fresh["deduplicated"], json!(false));
    }

    #[test]
    fn a_settings_write_is_keyed_and_its_secret_is_never_part_of_the_input() {
        let secret = |value: &str| {
            json!({
                "module_id": "test.module", "setting": "token", "value": value,
                "mutation": {"expected_revision": 1, "request_id": "one", "actor": "test"},
            })
        };
        let method = "module.settings.set-secret";
        let first = RequestKey::of(method, &secret("first")).expect("the revision envelope");
        let other = RequestKey::of(method, &secret("second")).unwrap();
        assert_eq!(first.scope, "module.settings");
        assert_eq!(first.request_id, "one");
        assert_eq!(first.input, other.input, "matched by the setting alone");
        let set = |mode: &str| {
            json!({
                "module_id": "test.module", "values": {"mode": mode},
                "mutation": {"expected_revision": 1, "request_id": "one", "actor": "test"},
            })
        };
        let fast = RequestKey::of("module.settings.set", &set("fast")).unwrap();
        let exact = RequestKey::of("module.settings.set", &set("exact")).unwrap();
        assert_ne!(fast.input, exact.input);
        assert!(
            RequestKey::of(
                "module.settings.set",
                &json!({"mutation": {"expected_revision": 1, "request_id": "one"}})
            )
            .is_none(),
            "a malformed envelope has no key; its handler refuses it"
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
