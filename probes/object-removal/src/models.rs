//! The pinned model files the probe uses. Nothing is downloaded here: the files are fetched once
//! by hand (see the README) and verified by SHA-256 before any is loaded.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub struct Model {
    pub id: &'static str,
    /// Path relative to the models directory.
    pub file: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

pub const EVIT_L0_ENCODER: Model = Model {
    id: "evit-l0-encoder",
    file: "l0_encoder.onnx",
    bytes: 123_112_917,
    sha256: "7a37ebd16e65c31253416e33897e7e6b07699127765d538e99ecbfaa3ad1e727",
};
pub const EVIT_L0_DECODER: Model = Model {
    id: "evit-l0-decoder",
    file: "l0_decoder.onnx",
    bytes: 16_470_362,
    sha256: "053e72c5703736a8a8a92482c9990c4a26dde9dcb7ff26b05843cf0bc433f42f",
};
pub const SAM21_SMALL_ENCODER: Model = Model {
    id: "sam21-small-encoder",
    file: "mask-object-sam21-small/encoder.onnx",
    bytes: 162_683_048,
    sha256: "82ee8774d6c362de06b35481ff6e6e68bb33ed5973aee8c45c606dfae7300d06",
};
pub const SAM21_SMALL_DECODER: Model = Model {
    id: "sam21-small-decoder",
    file: "mask-object-sam21-small/decoder.onnx",
    bytes: 20_603_850,
    sha256: "606d43f1f16e2c61dafb93d7781f7461c9604baf10aaf372ddf77e3bdddb1e8d",
};
pub const LAMA: Model = Model {
    id: "lama",
    file: "inpainting_lama_2025jan.onnx",
    bytes: 92_591_623,
    sha256: "7df918ac3921d3daf0aae1d219776cf0dc4e4935f035af81841b40adcf74fdf2",
};

/// Big-LaMa at full precision, exported by Carve (`Carve/LaMa-ONNX`, Apache-2.0): the same inputs,
/// scaling and outputs as the OpenCV export, whose weights are quantized in blocks.
pub const LAMA_FP32: Model = Model {
    id: "lama-fp32",
    file: "lama_fp32.onnx",
    bytes: 208_044_816,
    sha256: "1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6",
};

/// Which pinned LaMa export fills a region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillModel {
    /// OpenCV's export (`inpainting_lama_2025jan.onnx`), weights quantized in blocks.
    #[default]
    Lama,
    /// Carve's full-precision Big-LaMa export (`lama_fp32.onnx`).
    LamaFp32,
}

impl FillModel {
    pub fn name(self) -> &'static str {
        self.model().id
    }

    pub fn model(self) -> &'static Model {
        match self {
            FillModel::Lama => &LAMA,
            FillModel::LamaFp32 => &LAMA_FP32,
        }
    }
}

impl std::str::FromStr for FillModel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        [FillModel::Lama, FillModel::LamaFp32]
            .into_iter()
            .find(|m| m.name() == s)
            .ok_or_else(|| format!("unknown fill model {s:?}; expected lama or lama-fp32"))
    }
}

pub const ALL: &[&Model] = &[
    &EVIT_L0_ENCODER,
    &EVIT_L0_DECODER,
    &SAM21_SMALL_ENCODER,
    &SAM21_SMALL_DECODER,
    &LAMA,
];

/// Pinned models fetched only when wanted: `removal-probe models` verifies each one that is present.
pub const OPTIONAL: &[&Model] = &[&LAMA_FP32];

/// The models directory: `--models`, else `LUXFORGE_REMOVAL_MODELS`, else the checkout's ignored
/// `private/models/object-removal/` (a linked worktree uses the main checkout's).
pub fn directory(explicit: Option<PathBuf>) -> PathBuf {
    crate::paths::models_directory(explicit)
}

// The generative candidates (TASK-005), in their own ignored directory
// (`private/models/generative/`, `--gen-models` or `LUXFORGE_GENERATIVE_MODELS`). Each is pinned
// at the publisher's commit named in its source; the README's generative section lists them with
// their licences.

/// Moebius's denoiser (hustvl/Moebius `ft_places2`), ONNX by simonw/Moebius-ONNX at
/// `5bf1ef5d`, fp32, 512² static.
pub const MOEBIUS_UNET: Model = Model {
    id: "moebius-unet",
    file: "moebius-unet.onnx",
    bytes: 906_698_976,
    sha256: "e3f90f52f72378339b990459fadb29a68d3c7b5c6851545ba42774f489160b08",
};
/// Moebius's VAE encoder (hustvl/PixelHacker `vae/`), same export.
pub const MOEBIUS_VAE_ENCODER: Model = Model {
    id: "moebius-vae-encoder",
    file: "moebius-vae-encoder.onnx",
    bytes: 136_757_093,
    sha256: "b8b81d41e757222a0707665ba9d826703987855e5bed056036b90b988968042f",
};
/// Moebius's VAE decoder, same export.
pub const MOEBIUS_VAE_DECODER: Model = Model {
    id: "moebius-vae-decoder",
    file: "moebius-vae-decoder.onnx",
    bytes: 198_078_671,
    sha256: "d90ef0b7f6c8c8b7234459c8b449d70be0033bf1576c842e8b9991baf3934280",
};
/// FLUX.2 klein 4B's transformer, GGUF Q4_0 by leejet/FLUX.2-klein-4B-GGUF at `3b1f5a9d`.
pub const KLEIN_Q4: Model = Model {
    id: "klein-4b-q4_0",
    file: "flux-2-klein-4b-Q4_0.gguf",
    bytes: 2_460_378_560,
    sha256: "d1023499ef3f2f82ff7c50e6778495195c1b6cc34835741778868428111f9ff4",
};
/// FLUX.2 klein 4B's transformer, GGUF Q8_0, same repository and commit.
pub const KLEIN_Q8: Model = Model {
    id: "klein-4b-q8_0",
    file: "flux-2-klein-4b-Q8_0.gguf",
    bytes: 4_300_629_440,
    sha256: "0bba6951258ec8f92d51114a8fa13e66828297bfff58a738f52729b3ef66fa28",
};
/// FLUX.2 klein 4B's VAE (black-forest-labs/FLUX.2-klein-4B `vae/` at `e7b7dc27`), bf16.
pub const KLEIN_VAE: Model = Model {
    id: "klein-4b-vae",
    file: "flux2-klein-4b-vae.safetensors",
    bytes: 168_120_878,
    sha256: "ca70d2202afe6415bdbcb8793ba8cd99fd159cfe6192381504d6c4d3036e0f04",
};
/// Qwen3-4B, klein's text encoder, GGUF Q4_K_M by unsloth/Qwen3-4B-GGUF at `22c9fc8a`.
pub const QWEN3_Q4: Model = Model {
    id: "qwen3-4b-q4_k_m",
    file: "Qwen3-4B-Q4_K_M.gguf",
    bytes: 2_497_281_312,
    sha256: "f6f851777709861056efcdad3af01da38b31223a3ba26e61a4f8bf3a2195813a",
};
/// Qwen3-4B, GGUF Q8_0, same repository and commit.
pub const QWEN3_Q8: Model = Model {
    id: "qwen3-4b-q8_0",
    file: "Qwen3-4B-Q8_0.gguf",
    bytes: 4_280_405_792,
    sha256: "eed555233267a33c7e8ee31682762cc7751b3f6d224039086e0e846f05fffa5d",
};
/// The Qwen3 tokenizer (black-forest-labs/FLUX.2-klein-4B `tokenizer/tokenizer.json` at
/// `e7b7dc27`), which the text encoder needs: this build bundles no vocabulary tables.
pub const QWEN3_TOKENIZER: Model = Model {
    id: "qwen3-tokenizer",
    file: "qwen3-tokenizer.json",
    bytes: 11_422_654,
    sha256: "aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4",
};
/// fal's object-removal LoRA for klein 4B (fal/flux-2-klein-4B-object-remove-lora at `0e3f5879`,
/// the ComfyUI conversion).
pub const KLEIN_REMOVE_LORA: Model = Model {
    id: "klein-remove-lora",
    file: "klein-object-remove-lora-comfy.safetensors",
    bytes: 76_038_936,
    sha256: "dc197de62e174863f83fc4052465603f4389de7c3a78e17f3d41a1f6f11488ec",
};

/// Every generative pin; `removal-probe models --generative` verifies those present.
pub const GENERATIVE: &[&Model] = &[
    &MOEBIUS_UNET,
    &MOEBIUS_VAE_ENCODER,
    &MOEBIUS_VAE_DECODER,
    &KLEIN_Q4,
    &KLEIN_Q8,
    &KLEIN_VAE,
    &QWEN3_Q4,
    &QWEN3_Q8,
    &QWEN3_TOKENIZER,
    &KLEIN_REMOVE_LORA,
];

/// The generative models' directory: `--gen-models`, else `LUXFORGE_GENERATIVE_MODELS`, else
/// the checkout's ignored `private/models/generative/`.
pub fn generative_directory(explicit: Option<PathBuf>) -> PathBuf {
    crate::paths::generative_directory(explicit)
}

pub fn sha256(path: &Path) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

impl Model {
    /// The model's path after checking its size and SHA-256 against the pin. A file that does not
    /// match is refused by name and never reaches a runtime.
    pub fn verified(&self, dir: &Path) -> Result<PathBuf, String> {
        let path = dir.join(self.file);
        let refuse =
            |why: String| format!("refusing model {} ({}): {why}", self.file, path.display());
        let len = std::fs::metadata(&path)
            .map_err(|e| refuse(e.to_string()))?
            .len();
        if len != self.bytes {
            return Err(refuse(format!("{len} bytes, expected {}", self.bytes)));
        }
        let digest = sha256(&path)?;
        if digest != self.sha256 {
            return Err(refuse(format!(
                "SHA-256 {digest}, expected {}",
                self.sha256
            )));
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of the ASCII bytes `abc`, from FIPS 180-2.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn pinned(file: &'static str, bytes: u64, sha256: &'static str) -> Model {
        Model {
            id: "test",
            file,
            bytes,
            sha256,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("luxforge-models-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_file_that_matches_its_pin_is_accepted() {
        let dir = scratch("ok");
        std::fs::write(dir.join("tiny.onnx"), b"abc").unwrap();
        let path = pinned("tiny.onnx", 3, ABC).verified(&dir).unwrap();
        assert_eq!(path, dir.join("tiny.onnx"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_of_the_wrong_size_is_refused_by_name() {
        let dir = scratch("size");
        std::fs::write(dir.join("tiny.onnx"), b"abcd").unwrap();
        let e = pinned("tiny.onnx", 3, ABC).verified(&dir).unwrap_err();
        assert!(e.contains("refusing model tiny.onnx"), "{e}");
        assert!(e.contains("4 bytes, expected 3"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_of_the_right_size_and_the_wrong_hash_is_refused_by_name() {
        let dir = scratch("hash");
        std::fs::write(dir.join("tiny.onnx"), b"abd").unwrap();
        let e = pinned("tiny.onnx", 3, ABC).verified(&dir).unwrap_err();
        assert!(e.contains("refusing model tiny.onnx"), "{e}");
        assert!(e.contains("SHA-256") && e.contains(ABC), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_refused_by_name() {
        let dir = scratch("missing");
        let e = pinned("absent.onnx", 3, ABC).verified(&dir).unwrap_err();
        assert!(e.contains("refusing model absent.onnx"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_pins_are_distinct_files_with_full_digests() {
        for m in ALL.iter().chain(OPTIONAL) {
            assert_eq!(m.sha256.len(), 64, "{}", m.file);
            assert!(
                m.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "{}",
                m.file
            );
        }
        let mut files: Vec<_> = ALL.iter().chain(OPTIONAL).map(|m| m.file).collect();
        files.sort_unstable();
        files.dedup();
        assert_eq!(files.len(), ALL.len() + OPTIONAL.len());
        for m in GENERATIVE {
            assert_eq!(m.sha256.len(), 64, "{}", m.file);
            assert!(
                m.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "{}",
                m.file
            );
            assert!(m.bytes > 0, "{}", m.file);
        }
        let mut generative: Vec<_> = GENERATIVE.iter().map(|m| (m.file, m.sha256)).collect();
        generative.sort_unstable();
        generative.dedup_by(|a, b| a.0 == b.0 || a.1 == b.1);
        assert_eq!(
            generative.len(),
            GENERATIVE.len(),
            "a generative file or digest repeats"
        );
    }
}
