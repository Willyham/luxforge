//! Developer proof of the model manager. `luxforge.models` declares one model resource of two files,
//! an encoder and a decoder, with a plain trade-off, a use restriction and a machine minimum, the
//! `download-artifact` capability that installs it and the `local-runtime` capability that holds it
//! loaded. It has no task, effect or control: it exists so the Settings › Models tab, the resumable
//! multi-file download and `module.runtime.load`/`unload` can be driven end to end without a real
//! model. The files are a few kilobytes of a fixed pattern behind a magic, which the deterministic
//! fake runtime a developer run serves opens as a session. Like the capability proof it is a test
//! fixture, registered only in developer mode with `--proof-endpoint`, whose loopback
//! `ProofEndpoint` (`luxforge-testbase`, in no binary) serves the files with HTTP ranges. See
//! `docs/design/module-capabilities.md#proof-module`.
use super::{
    ActionInput, ActionPlan, CapabilityModule, LayerReport, ModuleDescriptor, Processing,
    StageContext, ToolModule,
};
use crate::{Error, inference::fake::FAKE_HOST_BACKEND};
use serde_json::{Map, Value, json};
use std::{io::Read, path::Path};

pub const MODELS_PROOF_MODULE: &str = "luxforge.models";
/// The model resource and the capabilities that install and hold it.
pub const PROOF_MODEL_RESOURCE: &str = "proof-segmenter";
pub(crate) const PROOF_MODEL_VERSION: &str = "1";
const PROOF_MODEL_DOWNLOAD: &str = "model";
const PROOF_MODEL_RUNTIME: &str = "segment";
/// Where the proof endpoint serves the two files, under its base URL.
pub const PROOF_ENCODER_PATH: &str = "/proof-model/encoder.bin";
pub const PROOF_DECODER_PATH: &str = "/proof-model/decoder.bin";
/// The further file's name in the install directory; the encoder is named after the resource.
const DECODER_FILE: &str = "decoder";
/// Each file's magic.
const MODEL_MAGIC: [u8; 8] = *b"LFMDL001";
/// The files' lengths and the SHA-256 each is pinned at; a test recomputes both.
const ENCODER_BYTES: usize = 6000;
const DECODER_BYTES: usize = 3000;
const ENCODER_SHA256: &str = "33aa78be2867c4025aa0700efaa2a9c3aecf12f2c2226de3f5ef2176f26c3f41";
const DECODER_SHA256: &str = "f026abf04c5713ecc171cc4b77fdbc44d4dc417d593a3a94a87a4cc0f9d99998";
/// What the fake model is counted at while loaded, and the machine minimum that follows from it:
/// the floor every model's minimum is held to ([`crate::capabilities::descriptor::machine_floor`])
/// is about 1.3 GiB here, so 2 GiB fits every machine the editor runs on.
const RESIDENT_BYTES: u64 = 64 << 20;
const MIN_MEMORY_BYTES: u64 = 2 << 30;

/// The trade-off and use restriction the proof declares, as a real model's would read.
pub const PROOF_MODEL_TRADEOFF: &str = "Small and quick to load; finds nothing, as a proof";
pub const PROOF_MODEL_RESTRICTION: &str = "For testing Luxforge only";

/// One file's bytes: the magic, then a fixed pattern keyed by `seed`.
fn pattern(seed: u64, length: usize) -> Vec<u8> {
    let mut bytes = MODEL_MAGIC.to_vec();
    bytes.extend(
        (0..(length - MODEL_MAGIC.len()) as u64).map(|i| ((i * 31 + seed * 17 + 7) % 251) as u8),
    );
    bytes
}

/// The files the proof endpoint serves, each with the path it serves it at: the encoder, then the
/// decoder.
pub fn proof_model_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (PROOF_ENCODER_PATH, pattern(1, ENCODER_BYTES)),
        (PROOF_DECODER_PATH, pattern(2, DECODER_BYTES)),
    ]
}

/// The developer model manager proof.
pub(crate) struct ModelsProofModule {
    descriptor: ModuleDescriptor,
}

impl ModelsProofModule {
    /// The proof whose two files are pinned under `endpoint_base`, the proof endpoint's base URL.
    /// Nothing is contacted: registration validates the URLs.
    pub(crate) fn new(endpoint_base: &str) -> Self {
        let base = endpoint_base.trim().trim_end_matches('/');
        let descriptor = serde_json::from_value(json!({
            "id": MODELS_PROOF_MODULE,
            "title": "Models proof",
            "hint": "A two-file model for the model manager and the Models tab",
            "effects": [],
            "actions": [],
            "controls": [],
            "canvas": null,
            "developer": true,
            "availability": {"kind": "available"},
            "capabilities": [
                {
                    "id": PROOF_MODEL_DOWNLOAD, "kind": "download-artifact",
                    "resource": PROOF_MODEL_RESOURCE,
                    "purpose": "Download the proof segmenter's two files.",
                },
                {
                    "id": PROOF_MODEL_RUNTIME, "kind": "local-runtime",
                    "resource": PROOF_MODEL_RESOURCE, "model": "session",
                    "resident_bytes": RESIDENT_BYTES, "backends": [FAKE_HOST_BACKEND], "precision": "fp32",
                    "purpose": "Hold the proof segmenter loaded between uses.",
                },
            ],
            "resources": [{
                "id": PROOF_MODEL_RESOURCE,
                "title": "Proof segmenter",
                "version": PROOF_MODEL_VERSION,
                "url": format!("{base}{PROOF_ENCODER_PATH}"),
                "bytes": ENCODER_BYTES,
                "sha256": ENCODER_SHA256,
                "files": [{
                    "name": DECODER_FILE,
                    "url": format!("{base}{PROOF_DECODER_PATH}"),
                    "bytes": DECODER_BYTES,
                    "sha256": DECODER_SHA256,
                }],
                "format": "luxforge-proof-model",
                "license": "GPL-3.0-or-later",
                "provenance": "Generated by Luxforge's model manager proof",
                "tradeoff": PROOF_MODEL_TRADEOFF,
                "use_restriction": PROOF_MODEL_RESTRICTION,
                "min_memory_bytes": MIN_MEMORY_BYTES,
            }],
        }))
        .expect("the models proof descriptor's shape is static");
        Self { descriptor }
    }
}

impl ToolModule for ModelsProofModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Err(Error::validation(format!("unknown action {action_id}")))
    }
    fn plan(&self, input: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Err(Error::validation(format!(
            "unknown action {}",
            input.action_id
        )))
    }
    fn validate_payload(&self, effect_id: &str, _: u32, _: &Value) -> Result<(), Error> {
        Err(Error::unavailable_effect(effect_id, &[]))
    }
    fn describe(&self, effect_id: &str, _: u32, _: &Value) -> Result<LayerReport, Error> {
        Err(Error::unavailable_effect(effect_id, &[]))
    }
    fn compile(
        &self,
        effect_id: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
        Err(Error::unavailable_effect(effect_id, &[]))
    }
    fn capabilities(&self) -> Option<&dyn CapabilityModule> {
        Some(self)
    }
}

impl CapabilityModule for ModelsProofModule {
    /// Each staged file starts with the proof's magic; the host has already checked its length and
    /// hash. The file is read, never loaded.
    fn validate_resource(&self, resource_id: &str, path: &Path) -> Result<(), Error> {
        let mut magic = [0u8; MODEL_MAGIC.len()];
        std::fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut magic))
            .map_err(|error| {
                Error::file_access(format!("cannot read {resource_id}: {}", error.kind()))
            })?;
        if magic != MODEL_MAGIC {
            return Err(Error::validation(format!(
                "{} is not a proof model file",
                path.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn the_pins_match_the_files_and_the_descriptor_registers() {
        let files = proof_model_files();
        let hex = |bytes: &[u8]| {
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        assert_eq!(files[0].1.len(), ENCODER_BYTES);
        assert_eq!(files[1].1.len(), DECODER_BYTES);
        assert_eq!(hex(&files[0].1), ENCODER_SHA256);
        assert_eq!(hex(&files[1].1), DECODER_SHA256);
        let module = ModelsProofModule::new("http://127.0.0.1:9/");
        let mut registry = crate::ModuleRegistry::new();
        registry
            .register(std::sync::Arc::new(module))
            .expect("the proof registers");
        let resource = &registry
            .module(MODELS_PROOF_MODULE)
            .unwrap()
            .descriptor()
            .resources[0];
        assert_eq!(resource.url, "http://127.0.0.1:9/proof-model/encoder.bin");
        assert_eq!(
            resource.total_bytes(),
            (ENCODER_BYTES + DECODER_BYTES) as u64
        );
    }

    #[test]
    fn a_file_without_the_magic_is_refused() {
        let directory =
            std::env::temp_dir().join(format!("luxforge-models-proof-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let module = ModelsProofModule::new("http://127.0.0.1:9");
        let good = directory.join("good");
        std::fs::write(&good, &proof_model_files()[1].1).unwrap();
        module
            .validate_resource(PROOF_MODEL_RESOURCE, &good)
            .unwrap();
        let bad = directory.join("bad");
        std::fs::write(&bad, b"not a model at all").unwrap();
        let refused = module
            .validate_resource(PROOF_MODEL_RESOURCE, &bad)
            .unwrap_err();
        assert_eq!(refused.kind, crate::ErrorKind::Validation);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
