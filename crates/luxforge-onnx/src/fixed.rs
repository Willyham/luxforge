//! The fixed-shape copy of a model that the Core ML backend opens, and its record. See the
//! [crate documentation](crate#the-fixed-shape-copy).
//!
//! The copy is `<model>.coreml-fixed.onnx` beside the model and the record
//! `<model>.coreml-fixed.json`, written last. The record names the model's hash and this runtime's
//! version, the free dimensions the copy fixes, the original model's input and output specs, and
//! the copy's length and SHA-256; a record that does not match all of them, or a copy that does not
//! match it, is written again. A model with no free input dimension is recorded with no copy: Core
//! ML opens the model itself.
use crate::{cancel, convert, runtime_error, session};
use luxforge_core::{
    Cancel, Error,
    inference::{ModelFile, TensorSpec},
};
use ort::value::ValueType;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// The suffix of the copy's file name, after the model's own.
pub(crate) const COPY_SUFFIX: &str = ".coreml-fixed.onnx";
/// The suffix of the record's file name, after the model's own.
pub(crate) const RECORD_SUFFIX: &str = ".coreml-fixed.json";
/// The only record format this build reads or writes.
pub(crate) const RECORD_FORMAT: u32 = 1;
/// The largest record read.
const MAX_RECORD_BYTES: u64 = 1 << 20;
/// What every free input dimension is fixed at.
pub(crate) const FIXED_SIDE: u64 = 1;

/// What the first open or an install recorded of a model's fixed-shape copy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub format: u32,
    /// The SHA-256 of the model the copy was written from.
    pub source_sha256: String,
    /// The ONNX Runtime version that wrote it.
    pub runtime_version: String,
    /// The named free input dimensions the copy fixes at [`FIXED_SIDE`], in the order found.
    pub dimensions: Vec<String>,
    /// The original model's inputs and outputs, free dimensions and all.
    pub inputs: Vec<TensorSpec>,
    pub outputs: Vec<TensorSpec>,
    /// The copy, or `None` when the model has no free input dimension.
    pub copy: Option<CopyFile>,
}

/// The written copy: its file name beside the model, its length and its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CopyFile {
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
}

impl Record {
    /// Each input's name and the shape the copy takes it at.
    pub(crate) fn fixed_shapes(&self) -> Vec<(String, Vec<u64>)> {
        self.inputs
            .iter()
            .map(|spec| {
                (
                    spec.name.clone(),
                    spec.shape
                        .iter()
                        .map(|side| side.unwrap_or(FIXED_SIDE))
                        .collect(),
                )
            })
            .collect()
    }

    /// The file Core ML opens: the copy, or the model itself.
    pub(crate) fn opened(&self, model: &Path) -> PathBuf {
        match &self.copy {
            Some(_) => copy_path(model),
            None => model.to_path_buf(),
        }
    }

    /// The derived files beside `model`: the copy, if there is one, and the record.
    pub(crate) fn files(&self, model: &Path) -> Vec<PathBuf> {
        let mut files = Vec::with_capacity(2);
        if self.copy.is_some() {
            files.push(copy_path(model));
        }
        files.push(record_path(model));
        files
    }
}

fn suffixed(model: &Path, suffix: &str) -> PathBuf {
    let mut name = model.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    model.with_file_name(name)
}

pub(crate) fn copy_path(model: &Path) -> PathBuf {
    suffixed(model, COPY_SUFFIX)
}

pub(crate) fn record_path(model: &Path) -> PathBuf {
    suffixed(model, RECORD_SUFFIX)
}

/// The record of `model`'s fixed-shape copy, written now if no matching one is there.
pub(crate) fn ensure(model: &ModelFile, cancel: &Cancel) -> Result<Record, Error> {
    let version = crate::ffi::version();
    if let Some(record) = matching(model, &version) {
        return Ok(record);
    }
    write(model, &version, cancel)
}

/// The record beside `model` when it and its copy match the model and this runtime.
pub(crate) fn matching(model: &ModelFile, version: &str) -> Option<Record> {
    let path = model.path();
    let mut bytes = Vec::new();
    fs::File::open(record_path(path))
        .ok()?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return None;
    }
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    if record.format != RECORD_FORMAT
        || record.source_sha256 != model.identity().sha256
        || record.runtime_version != version
        || record.inputs.is_empty()
    {
        return None;
    }
    if let Some(copy) = &record.copy {
        let copy_path = copy_path(path);
        if copy_path.file_name()?.to_string_lossy() != copy.file
            || fs::metadata(&copy_path).ok()?.len() != copy.bytes
            || sha256(&copy_path).ok()? != copy.sha256
        {
            return None;
        }
    }
    Some(record)
}

/// Write `model`'s copy and record, replacing whatever stale ones are there.
fn write(model: &ModelFile, version: &str, cancel: &Cancel) -> Result<Record, Error> {
    let path = model.path();
    let id = &model.identity().id;
    // The original's specs and its free input dimensions' names, from a CPU session of it.
    let builder = session::builder()?;
    let canceler = builder.canceler();
    let mut builder = builder;
    let probe =
        cancel::watching(cancel, canceler, || builder.commit_from_file(path)).map_err(|error| {
            runtime_error(
                &format!("model {id} could not be read"),
                error,
                Some(cancel),
            )
        })?;
    let inputs = convert::specs(probe.inputs())?;
    let outputs = convert::specs(probe.outputs())?;
    let mut dimensions: Vec<String> = Vec::new();
    for input in probe.inputs() {
        let ValueType::Tensor {
            shape,
            dimension_symbols,
            ..
        } = input.dtype()
        else {
            continue;
        };
        for (&side, symbol) in shape.iter().zip(dimension_symbols.iter()) {
            if side >= 0 {
                continue;
            }
            if symbol.is_empty() {
                return Err(Error::incompatible(format!(
                    "model {id}'s input {} has an unnamed free dimension, which no fixed-shape \
                     copy can fix; open it on the cpu backend",
                    input.name()
                )));
            }
            if !dimensions.contains(symbol) {
                dimensions.push(symbol.clone());
            }
        }
    }
    drop(probe);
    if inputs.is_empty() {
        return Err(Error::incompatible(format!("model {id} declares no input")));
    }
    // The stale record goes first, so a failure below never leaves a record naming another copy.
    let record_path = record_path(path);
    let copy_path = copy_path(path);
    let _ = fs::remove_file(&record_path);
    let _ = fs::remove_file(&copy_path);
    let copy = if dimensions.is_empty() {
        None
    } else {
        let partial = path.with_file_name(format!(
            ".{}{COPY_SUFFIX}.{}.partial",
            path.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        let written = write_copy(path, &partial, &dimensions, cancel).and_then(|()| {
            let bytes = fs::metadata(&partial)
                .map_err(|error| file_error(&partial, &error))?
                .len();
            let sha256 = sha256(&partial).map_err(|error| file_error(&partial, &error))?;
            fs::rename(&partial, &copy_path).map_err(|error| file_error(&copy_path, &error))?;
            Ok(CopyFile {
                file: copy_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                bytes,
                sha256,
            })
        });
        if written.is_err() {
            let _ = fs::remove_file(&partial);
        }
        Some(written?)
    };
    let record = Record {
        format: RECORD_FORMAT,
        source_sha256: model.identity().sha256.clone(),
        runtime_version: version.to_owned(),
        dimensions,
        inputs,
        outputs,
        copy,
    };
    let json = serde_json::to_vec_pretty(&record)
        .map_err(|error| Error::internal(format!("fixed-shape record: {error}")))?;
    let staged = record_path.with_file_name(format!(
        ".{}.{}.partial",
        record_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        std::process::id()
    ));
    let published = fs::write(&staged, json)
        .and_then(|()| fs::rename(&staged, &record_path))
        .map_err(|error| file_error(&record_path, &error));
    if published.is_err() {
        let _ = fs::remove_file(&staged);
    }
    published?;
    Ok(record)
}

/// Have ONNX Runtime save `model` with every named free input dimension in `dimensions` fixed at
/// [`FIXED_SIDE`] to `partial`: a CPU session with the overrides, at the basic optimization level,
/// which is provider-independent, saves the graph it optimized, whose shape computations are then
/// constants. Setting the same overrides on the Core ML session instead lets the provider take the
/// graph, but its compiled partition then asks at run time for a `Shape` output nothing feeds.
fn write_copy(
    model: &Path,
    partial: &Path,
    dimensions: &[String],
    cancel: &Cancel,
) -> Result<(), Error> {
    let failed = |error: ort::Error| {
        runtime_error(
            &format!(
                "the fixed-shape copy of {} could not be written",
                model.display()
            ),
            error,
            Some(cancel),
        )
    };
    let mut builder = session::builder()?;
    for dimension in dimensions {
        builder = builder
            .with_dimension_override(dimension, FIXED_SIDE as i64)
            .map_err(|error| failed(error.into()))?;
    }
    let builder = builder
        .with_optimized_model_path(partial)
        .map_err(|error| failed(error.into()))?;
    let canceler = builder.canceler();
    let mut builder = builder;
    let saved =
        cancel::watching(cancel, canceler, || builder.commit_from_file(model)).map_err(failed)?;
    drop(saved);
    cancel.check()
}

fn file_error(path: &Path, error: &std::io::Error) -> Error {
    Error::file_access(format!("{}: {error}", path.display()))
}

/// The SHA-256 of the file at `path`, in lowercase hexadecimal.
pub(crate) fn sha256(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
