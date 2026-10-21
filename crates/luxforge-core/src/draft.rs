//! The core's revision-bound draft: the settings of one gesture, held by one client session until
//! it commits or cancels. A draft writes nothing, emits no event and appears in no history; its
//! effective recipe is computed on demand from the current snapshot and never persisted.
use crate::{
    AcceptedCandidate, ActionDescriptor, AssetId, DraftId, Error, IdentityKind, JobId,
    ParameterDescriptor, ParameterKind, artifacts::PreparedArtifact, check_value,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, sync::Arc};

/// The objects a gesture edits, fixed when it begins: each identity parameter the action declares
/// that the gesture names, by parameter name, with the identity it names — `mask` and `component`
/// for a `mask.*` shape gesture, or the host's one `mask` field of a maskable module action.
///
/// It holds names and identities and nothing about what they identify, so any action that declares
/// identity parameters drafts through it; it is checked where the draft begins by the checks its
/// commit runs (`EditorService::draft_target`), which is where an undeclared name, a value that is
/// not an identity of its kind or a field that is not an identity at all is refused.
pub type DraftTarget = BTreeMap<String, String>;

/// The target a gesture of `action` takes: every identity parameter the action declares, in
/// declaration order, that `identity` answers for. One spelling for every client, so the target is
/// named by what the action declares rather than by a type that knows which objects one family of
/// actions addresses.
pub fn declared_target(
    action: &ActionDescriptor,
    mut identity: impl FnMut(IdentityKind) -> Option<String>,
) -> DraftTarget {
    action
        .parameters
        .iter()
        .filter_map(|parameter| match parameter.kind {
            ParameterKind::Identity { of } => {
                identity(of).map(|value| (parameter.name.clone(), value))
            }
            _ => None,
        })
        .collect()
}

/// One client's open draft of one action on one asset. `fields` holds only what the client set, so
/// a reapply can merge exactly those fields over whatever another client committed meanwhile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub draft_id: DraftId,
    /// The action this draft will run on commit; its parameter descriptors validate every field.
    pub action: String,
    pub asset_id: AssetId,
    /// The asset revision the draft was begun or last reapplied against.
    pub base_revision: u64,
    /// Increases on every accepted `draft.set`, so a preview or a sample can be correlated with the
    /// settings it was evaluated against.
    pub draft_revision: u64,
    pub fields: Map<String, Value>,
    /// The objects the gesture edits, fixed when it begins ([`DraftTarget`]): the mask and
    /// component a `mask.*` gesture drags the handles of, or the mask a masked slider applies
    /// through. The commit sends them as the request fields they are ([`Self::request`]) — a
    /// command's declared identity parameters, a module action's host `mask` field — so the drafted
    /// fields are only the values the gesture moves. Everything else about the lifecycle —
    /// validation, conflict, Discard and Reapply — is unchanged by it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub target: DraftTarget,
    /// Derived: the asset moved under this draft. Recomputed wherever the draft is read, set,
    /// committed or reported, so no notification path is needed.
    pub conflicted: bool,
    /// A candidate draft's patch, held in memory ([`DraftCandidate`]): set only by the catalog
    /// owner, once it has read and verified the candidate the drafted fields name. Never
    /// serialized: a client reads the fields that name it.
    #[serde(skip)]
    pub candidate: Option<Arc<DraftCandidate>>,
}

/// The live candidate a candidate draft of a candidate-accepting action shows
/// (`docs/design/ai-editing.md`, "Candidate drafts"): its job and number, what its accept would
/// commit, and its patch as the prepared artifact publishing will make of it, under the same
/// identity. The draft's plan reads it as an accept's reads the published candidate
/// ([`crate::StageQuestions::accepting`]), and its renders bind the patch from here rather than
/// from the catalog's artifact store.
#[derive(Clone, Debug)]
pub struct DraftCandidate {
    pub(crate) job_id: JobId,
    pub(crate) candidate: u8,
    pub(crate) accepted: Arc<AcceptedCandidate>,
    pub(crate) artifact: Arc<PreparedArtifact>,
}

impl DraftCandidate {
    /// Whether `fields` name this candidate: its job and number.
    pub(crate) fn names(&self, fields: &Map<String, Value>) -> bool {
        named_candidate(fields)
            .is_some_and(|(job_id, candidate)| job_id == self.job_id && candidate == self.candidate)
    }
}

/// Two drafts hold the same candidate when they name the same one and hold the same bytes.
impl PartialEq for DraftCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.job_id == other.job_id
            && self.candidate == other.candidate
            && self.artifact.id == other.artifact.id
    }
}

/// The candidate drafted fields name, `job_id` and `candidate`, when both are there and well
/// formed.
pub(crate) fn named_candidate(fields: &Map<String, Value>) -> Option<(JobId, u8)> {
    let job_id = serde_json::from_value(fields.get("job_id")?.clone()).ok()?;
    let candidate = u8::try_from(fields.get("candidate")?.as_u64()?).ok()?;
    Some((job_id, candidate))
}

impl Draft {
    pub fn new(action: &str, asset_id: AssetId, base_revision: u64) -> Self {
        Self {
            draft_id: DraftId::new(),
            action: action.to_owned(),
            asset_id,
            base_revision,
            draft_revision: 0,
            fields: Map::new(),
            target: DraftTarget::new(),
            conflicted: false,
            candidate: None,
        }
    }

    /// Validate every named field against the action's declared parameters before changing
    /// anything, so a rejected request leaves the draft exactly as it was.
    pub(crate) fn checked_fields(
        &self,
        parameters: &[ParameterDescriptor],
        fields: &Map<String, Value>,
    ) -> Result<(), Error> {
        for (name, value) in fields {
            let declared = parameters
                .iter()
                .find(|parameter| &parameter.name == name)
                .ok_or_else(|| {
                    Error::validation(format!(
                        "unknown parameter {name} for action {}",
                        self.action
                    ))
                })?;
            check_value(declared, value)?;
        }
        Ok(())
    }

    /// The request this draft commits, and plans its preview from: the drafted fields with the
    /// target's fields over them, so the objects a gesture edits are the ones it began on.
    pub fn request(&self) -> Map<String, Value> {
        let mut request = self.fields.clone();
        for (name, identity) in &self.target {
            request.insert(name.clone(), Value::String(identity.clone()));
        }
        request
    }

    /// Merge accepted fields into the draft. Every caller validates first.
    pub fn merge(&mut self, fields: Map<String, Value>) {
        for (name, value) in fields {
            self.fields.insert(name, value);
        }
        self.draft_revision = self.draft_revision.saturating_add(1);
    }

    /// This draft with accepted fields merged in, exactly as [`Self::merge`] leaves a copy of it,
    /// copying only the fields `fields` leaves as they were. A brush tick replaces its whole path,
    /// which is moved in rather than copied twice. Every caller validates first.
    pub fn merged(&self, fields: Map<String, Value>) -> Self {
        let Self {
            draft_id,
            action,
            asset_id,
            base_revision,
            draft_revision,
            fields: held,
            target,
            conflicted,
            candidate,
        } = self;
        let mut next = Self {
            draft_id: draft_id.clone(),
            action: action.clone(),
            asset_id: asset_id.clone(),
            base_revision: *base_revision,
            draft_revision: *draft_revision,
            fields: held
                .iter()
                .filter(|(name, _)| !fields.contains_key(*name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
            target: target.clone(),
            conflicted: *conflicted,
            candidate: candidate.clone(),
        };
        next.merge(fields);
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merged_is_a_merged_copy() {
        let mut held = Draft::new("mask.add-stroke", AssetId::new(), 3);
        held.target.insert("mask".into(), "m".into());
        held.merge(Map::from_iter([
            ("points".to_owned(), json!([[0.1, 0.2], [0.3, 0.4]])),
            ("size".to_owned(), json!(0.1)),
            ("flow".to_owned(), json!(80.0)),
        ]));
        held.conflicted = true;
        let posted = Map::from_iter([
            (
                "points".to_owned(),
                json!([[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]]),
            ),
            ("feather".to_owned(), json!(40.0)),
        ]);
        let mut copied = held.clone();
        copied.merge(posted.clone());
        assert_eq!(held.merged(posted), copied);
    }
}
