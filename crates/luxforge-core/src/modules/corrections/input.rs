//! A frozen patch's input identity and why a patch is stale (`docs/design/corrections.md`, "Input
//! identity"): one core function computes `{source, dimensions, upstream, earlier}` for an
//! operation of the repair layer, and the comparison of a stored identity with the current one names
//! each component that changed.
//!
//! - `source`: the SHA-256 of the verified original's fingerprint, its kind and the decoder's
//!   interpretation of it (a RAW's camera interpretation), and its decoded dimensions. Never the
//!   locator, so a same-byte Locate changes nothing.
//! - `dimensions`: the content stage the repair layer receives, `[width, height]`.
//! - `upstream`: [`crate::render::spatial::prefix_hash`] of the layers before the repair layer and
//!   the masks they read — the RAW development, white balance included.
//! - `earlier`: the SHA-256 of the canonical JSON of the repair layer's operations before this one,
//!   stroke and artifact identities included.
//!
//! Later operations, layers after the repair layer, masks no earlier layer reads and the original's
//! location are not part of it, so none of them stales a patch. Everything here reads the recipe
//! and the asset's catalog row; no pixel, file or prepared source.
use super::payload::Operation;
use crate::{
    AssetRecord, Error, Layer, Mask, Stage, mask_field::MaskSampling, render::spatial::prefix_hash,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What a frozen patch was generated against: the four components of its input identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairInput {
    /// Hex SHA-256 of the original's interpretation ([module](self) docs).
    pub source: String,
    /// The content stage the repair layer receives.
    pub dimensions: [u32; 2],
    /// Hex SHA-256 of the layers before the repair layer and the masks they read.
    pub upstream: String,
    /// Hex SHA-256 of the canonical JSON of the operations before this one.
    pub earlier: String,
}

/// One reason a patch is stale: a component of its input identity that changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StaleReason {
    /// The original's interpretation: another file, or another decode of it.
    Source,
    /// The content stage's size.
    Dimensions,
    /// The source development before the repair layer: a RAW's white balance or development.
    Development,
    /// An operation below it in the repair layer.
    EarlierCorrection,
}

impl StaleReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Dimensions => "dimensions",
            Self::Development => "development",
            Self::EarlierCorrection => "earlier-correction",
        }
    }

    /// What changed, in the words a status line says it: `the RAW development changed`.
    pub fn sentence(self) -> &'static str {
        match self {
            Self::Source => "the original changed",
            Self::Dimensions => "the photograph's dimensions changed",
            Self::Development => "the RAW development changed",
            Self::EarlierCorrection => "a correction below it changed",
        }
    }
}

fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl RepairInput {
    /// Every component in its stored form: three lowercase hex SHA-256 digests and positive
    /// dimensions.
    pub(crate) fn check(&self) -> Result<(), Error> {
        if !is_digest(&self.source) || !is_digest(&self.upstream) || !is_digest(&self.earlier) {
            return Err(Error::validation(
                "a patch's input names its source, upstream and earlier operations by SHA-256",
            ));
        }
        if self.dimensions.contains(&0) {
            return Err(Error::validation(
                "a patch's input dimensions must be positive",
            ));
        }
        Ok(())
    }

    /// The components of `self`, a stored identity, that differ from `current`, in the order the
    /// contract lists them; empty when the patch is current.
    pub fn reasons(&self, current: &Self) -> Vec<StaleReason> {
        let mut reasons = Vec::new();
        if self.source != current.source {
            reasons.push(StaleReason::Source);
        }
        if self.dimensions != current.dimensions {
            reasons.push(StaleReason::Dimensions);
        }
        if self.upstream != current.upstream {
            reasons.push(StaleReason::Development);
        }
        if self.earlier != current.earlier {
            reasons.push(StaleReason::EarlierCorrection);
        }
        reasons
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The repair stage's **output** identity a model selection records as its `prefix_hash`: the
/// SHA-256 of the content stage, the layers before the repair layer and every repair operation,
/// which is exactly what a new top correction's input identity holds (`dimensions`, `upstream` and
/// `earlier` over all operations). A colour edit, a crop or a lens correction after the repair
/// layer changes none of it, so an ordinary edit never stales a selection; a removal does.
/// `O(1)` over an identity already computed.
pub(crate) fn selection_prefix(top: &RepairInput) -> String {
    let canonical = serde_json::to_vec(&(top.dimensions, &top.upstream, &top.earlier))
        .expect("an input identity is serializable");
    digest(&canonical)
}

/// The `source` component of an asset: its fingerprint, kind and interpretation and its decoded
/// dimensions, never its locator or file identity. `O(1)`; reads the catalog row only.
pub(crate) fn source_identity(asset: &AssetRecord) -> String {
    let canonical =
        serde_json::to_vec(&(&asset.fingerprint, &asset.source, asset.width, asset.height))
            .expect("an asset's interpretation is serializable");
    digest(&canonical)
}

/// The input identity of an operation at `position` of the repair layer whose operations are
/// `operations`, the layer at `index` of `layers` receiving `content`: the one core function. A
/// new top operation is `position == operations.len()`; a stack without a repair layer passes the
/// index one would take and no operations. `O(layers + operations)` serialization.
pub(crate) fn repair_input(
    source: &str,
    content: Stage,
    layers: &[Layer],
    masks: &[Mask],
    index: usize,
    operations: &[Operation],
    position: usize,
) -> Result<RepairInput, Error> {
    let before = &layers[..index.min(layers.len())];
    let upstream = prefix_hash(before, masks, MaskSampling::Point)?;
    let earlier = serde_json::to_vec(&operations[..position.min(operations.len())])
        .map_err(|error| Error::internal(format!("operations could not be hashed: {error}")))?;
    Ok(RepairInput {
        source: source.to_owned(),
        dimensions: [content.width, content.height],
        upstream,
        earlier: digest(&earlier),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> RepairInput {
        RepairInput {
            source: "a".repeat(64),
            dimensions: [40, 30],
            upstream: "b".repeat(64),
            earlier: "c".repeat(64),
        }
    }

    #[test]
    fn each_changed_component_is_one_reason_in_contract_order() {
        let stored = input();
        assert!(stored.reasons(&input()).is_empty());
        let current = RepairInput {
            earlier: "d".repeat(64),
            upstream: "e".repeat(64),
            ..input()
        };
        assert_eq!(
            stored.reasons(&current),
            [StaleReason::Development, StaleReason::EarlierCorrection]
        );
        let moved = RepairInput {
            source: "f".repeat(64),
            dimensions: [30, 40],
            ..input()
        };
        assert_eq!(
            stored.reasons(&moved),
            [StaleReason::Source, StaleReason::Dimensions]
        );
        assert_eq!(
            serde_json::to_value(StaleReason::EarlierCorrection).unwrap(),
            "earlier-correction"
        );
        assert!(input().check().is_ok());
        assert!(
            RepairInput {
                source: "A".repeat(64),
                ..input()
            }
            .check()
            .is_err()
        );
        assert!(
            RepairInput {
                dimensions: [0, 3],
                ..input()
            }
            .check()
            .is_err()
        );
    }
}
