//! Where a klein model's files are inside its installed resources: the contract between the
//! `luxforge.ai` module's resource descriptors (`crates/luxforge-core/src/modules/ai/models.rs`)
//! and this runtime, which the tests hold to the registered descriptors.
//!
//! - **The generator resource** is the transformer (GGUF) as its own file, with [`VAE`],
//!   [`REMOVAL_LORA`] and [`REMOVAL_CONDITIONING`] beside it.
//! - **The text encoder resource** is the encoder (GGUF) as its own file, with [`TOKENIZER`]
//!   beside it. A model opened from it carries the generator resource as its one companion.
use luxforge_core::{Error, ErrorKind, inference::ModelFile};
use std::path::{Path, PathBuf};

/// The autoencoder, beside the transformer.
pub const VAE: &str = "vae.safetensors";
/// fal's object removal LoRA, beside the transformer.
pub const REMOVAL_LORA: &str = "removal-lora.safetensors";
/// The removal prompt's recorded conditioning (`LFSDCOND` version 1), beside the transformer.
pub const REMOVAL_CONDITIONING: &str = "removal-conditioning.lfsdcond";
/// The text encoder's tokenizer, beside the encoder.
pub const TOKENIZER: &str = "tokenizer.json";

/// The files one klein context opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Files {
    pub transformer: PathBuf,
    pub vae: PathBuf,
    pub lora: PathBuf,
    pub conditioning: PathBuf,
    /// The text encoder and its tokenizer, for a model opened for prompts.
    pub encoder: Option<(PathBuf, PathBuf)>,
}

impl Files {
    /// The files of `model`: a generator resource, or a text encoder resource with the generator
    /// as its one companion. Each must be there; a missing one is `source-unavailable` by name.
    pub fn of(model: &ModelFile) -> Result<Self, Error> {
        let (generator, encoder) = match model.companions() {
            [] => (model, None),
            [generator] => {
                let dir = parent(model.path())?;
                (
                    generator,
                    Some((model.path().to_path_buf(), dir.join(TOKENIZER))),
                )
            }
            more => {
                return Err(Error::incompatible(format!(
                    "a klein model opens with at most one companion, the generator; {} has {}",
                    model.identity().id,
                    more.len()
                )));
            }
        };
        let dir = parent(generator.path())?;
        let files = Self {
            transformer: generator.path().to_path_buf(),
            vae: dir.join(VAE),
            lora: dir.join(REMOVAL_LORA),
            conditioning: dir.join(REMOVAL_CONDITIONING),
            encoder,
        };
        for path in files.paths() {
            if !path.is_file() {
                return Err(Error::new(
                    ErrorKind::SourceUnavailable,
                    format!("model file of {} is missing: {}", model.identity().id, path.display()),
                ));
            }
        }
        Ok(files)
    }

    /// Every path, the encoder's included.
    pub fn paths(&self) -> Vec<&Path> {
        let mut paths = vec![
            self.transformer.as_path(),
            self.vae.as_path(),
            self.lora.as_path(),
            self.conditioning.as_path(),
        ];
        if let Some((encoder, tokenizer)) = &self.encoder {
            paths.push(encoder);
            paths.push(tokenizer);
        }
        paths
    }
}

fn parent(path: &Path) -> Result<&Path, Error> {
    path.parent()
        .ok_or_else(|| Error::validation(format!("{} has no directory", path.display())))
}
