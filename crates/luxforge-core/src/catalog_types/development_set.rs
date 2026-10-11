//! The development set (`docs/design/catalog.md`, "Develop and the development set";
//! `docs/design/hdr-merge.md`, "Jobs, commands and client state"): the photographs Develop moves
//! through, held by the catalog owner for every client at once, so the desktop's filmstrip and an
//! agent's Pick merged change the same set.
//!
//! A catalog session has at most one **current** set, named by its [`DevelopmentSetId`] and at a
//! [`revision`](DevelopmentSetPage::revision) that every change to it moves on. Its members are
//! photographs in an order of their own, each once. Replacing the set (Develop N, opening a
//! catalog view's photograph, a single open) starts a new set and retires the old; adding and
//! removing members are library changes of `set-member` items, undone item by item. What Develop
//! shows of the set — which photograph, the strip's window, its selection — stays with the client.
use super::{LibraryChangeSeq, MergeId};
use crate::{AssetId, MutationOutcome, SourceTag, model::identifier};
use serde::{Deserialize, Serialize};

identifier!(
    /// A development set, `set-…`: one per replacement, never reused, kept while a library change
    /// names one of its members.
    DevelopmentSetId,
    "set-"
);

/// The most photographs one set holds: the design's catalog of 100,000 photographs, around the
/// photograph a view's set opens on.
pub const MAX_SET_MEMBERS: usize = 100_000;
/// The most members one `development-set.read` answers.
pub const MAX_SET_PAGE: usize = 1000;

/// A `set-member` row's value: the member's place in its set's order. Places only grow, so a
/// member put back by an undo returns between the members it was between.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetMemberValue {
    pub position: u64,
}

/// Where a replacement takes its photographs from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SetSource {
    /// These photographs, in this order, each once: what a Develop answered, or one opened alone.
    Assets { asset_ids: Vec<AssetId> },
    /// The photographs of the caller's view at `revision`, at most [`MAX_SET_MEMBERS`] around
    /// `position`, in the view's order: a catalog view's photograph opened in Develop.
    View { revision: u64, position: u32 },
    /// No set: a file opened alone.
    None,
}

/// One member as `development-set.read` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetMember {
    /// Its index in the set's order, from 0.
    pub index: u32,
    pub asset_id: AssetId,
    pub file_name: String,
    pub kind: SourceTag,
    /// In Removed: still a member, until the person removes it from the set.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    /// The merge it is the result of, for an HDR photograph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_id: Option<MergeId>,
}

/// What `development-set.read` answers: the current set's identity and revision, its size, and
/// members `from..from + members.len()` in order. No set answers no identity and no members.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentSetPage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_id: Option<DevelopmentSetId>,
    /// The catalog's development-set revision, which every replacement and membership change
    /// moves on: a client holding this revision holds the set as it is.
    pub revision: u64,
    pub count: u32,
    pub from: u32,
    pub members: Vec<SetMember>,
}

/// What `development-set.replace` answers: the new set (none for [`SetSource::None`]), its revision
/// and size, and for a view's set the index of the photograph it opened on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentSetReplaced {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_id: Option<DevelopmentSetId>,
    pub revision: u64,
    pub count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<u32>,
    /// A retry answered as the replacement it repeats was.
    #[serde(default)]
    pub deduplicated: bool,
}

/// What `development-set.add` and `development-set.remove` answer: the library change, as every
/// library method does, with the set it changed and the revision it left.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentSetChanged {
    pub outcome: MutationOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<LibraryChangeSeq>,
    pub items: u32,
    pub deduplicated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_id: Option<DevelopmentSetId>,
    pub revision: u64,
    pub count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_set_source_names_its_kind() {
        let asset = AssetId::new();
        assert_eq!(
            serde_json::from_value::<SetSource>(json!({"kind": "assets", "asset_ids": [asset]}))
                .unwrap(),
            SetSource::Assets {
                asset_ids: vec![asset]
            }
        );
        assert_eq!(
            serde_json::from_value::<SetSource>(
                json!({"kind": "view", "revision": 3, "position": 7})
            )
            .unwrap(),
            SetSource::View {
                revision: 3,
                position: 7
            }
        );
        assert_eq!(
            serde_json::from_value::<SetSource>(json!({"kind": "none"})).unwrap(),
            SetSource::None
        );
        assert!(
            serde_json::from_value::<SetSource>(json!({"kind": "view", "revision": 1})).is_err()
        );
        assert!(DevelopmentSetId::new().as_str().starts_with("set-"));
    }
}
