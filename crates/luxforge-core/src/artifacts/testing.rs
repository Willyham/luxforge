//! The capability proof module as the artifact tests edit with it: its tint layer is evaluated with
//! an artifact's bytes, so every rendered pixel of the layer proves which bytes were bound.
use super::ArtifactMeta;
use crate::{CapabilitiesProofModule, ModuleRegistry, modules::PROOF_TINT_KIND};
use std::sync::Arc;

/// The developer registry, whose pixel proof the artifact tests edit with, and the capability proof
/// pinned at a loopback endpoint nothing contacts.
pub(crate) fn registry() -> Arc<ModuleRegistry> {
    let mut registry = ModuleRegistry::developer();
    registry
        .register(Arc::new(CapabilitiesProofModule::new("http://127.0.0.1:9")))
        .unwrap();
    Arc::new(registry)
}

/// The artifact bytes of a tint: three little-endian `f32` linear gains.
pub(crate) fn tint_bytes(gains: [f32; 3]) -> Vec<u8> {
    gains.iter().flat_map(|gain| gain.to_le_bytes()).collect()
}

/// What the proof's task records with a tint it publishes.
pub(crate) fn tint_meta() -> ArtifactMeta {
    ArtifactMeta {
        kind: PROOF_TINT_KIND.into(),
        width: None,
        height: None,
        colour: Some("linear-srgb".into()),
    }
}
