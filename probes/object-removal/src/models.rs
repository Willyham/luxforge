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

/// The models directory: `--models`, else `LUXFORGE_REMOVAL_MODELS`, else the main checkout's
/// ignored `private/models/object-removal`.
pub fn directory(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os("LUXFORGE_REMOVAL_MODELS").map(PathBuf::from))
        .unwrap_or_else(|| {
            PathBuf::from("/Users/will/projects/lightwell/private/models/object-removal")
        })
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
    /// The model's path after checking its size and SHA-256 against the pin.
    pub fn verified(&self, dir: &Path) -> Result<PathBuf, String> {
        let path = dir.join(self.file);
        let len = std::fs::metadata(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .len();
        if len != self.bytes {
            return Err(format!(
                "{}: {len} bytes, expected {}",
                path.display(),
                self.bytes
            ));
        }
        let digest = sha256(&path)?;
        if digest != self.sha256 {
            return Err(format!(
                "{}: SHA-256 {digest}, expected {}",
                path.display(),
                self.sha256
            ));
        }
        Ok(path)
    }
}
