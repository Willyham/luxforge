//! The core's revision-bound draft: the settings of one gesture, held by one client session until
//! it commits or cancels. A draft writes nothing, emits no event and appears in no history; its
//! effective recipe is computed on demand from the current snapshot and never persisted.
use crate::{
    ActionDescriptor, AssetId, DraftId, Error, IdentityKind, ParameterDescriptor, ParameterKind,
    check_value,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

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
}
