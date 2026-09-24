//! The one store of the small JSON files the host owns outside every catalog: the settings document,
//! the grants document and each installed resource's `installed.json`. A [`JsonDocument`] is one
//! file of one shape `T`, `{format: <marker>, ...T}`, of at most `max_bytes`. Every call reads the
//! file again, so two processes never act on a stale copy. A file of another format, or one that is
//! not the current shape, is refused with `incompatible` and never rewritten. A change is one
//! locked read-modify-write through the shared durable write, so a failure at any point leaves the
//! previous file. See `docs/design/module-capabilities.md#settings-store`.
use crate::{Error, ErrorKind, atomic_file};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    marker::PhantomData,
    path::{Path, PathBuf},
};

/// The key every document keeps its format marker under, beside the fields of its shape.
const FORMAT: &str = "format";

/// One JSON document in one directory. It holds no state of its own.
pub(crate) struct JsonDocument<T> {
    dir: PathBuf,
    file: &'static str,
    max_bytes: u64,
    format: u32,
    /// What the shape cannot say: a consistency a parsed document must also have.
    check: fn(&T) -> Result<(), String>,
    shape: PhantomData<fn() -> T>,
}

impl<T> Clone for JsonDocument<T> {
    fn clone(&self) -> Self {
        Self {
            dir: self.dir.clone(),
            ..*self
        }
    }
}

impl<T> std::fmt::Debug for JsonDocument<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonDocument")
            .field("path", &self.dir.join(self.file))
            .field("max_bytes", &self.max_bytes)
            .field("format", &self.format)
            .finish()
    }
}

/// The document as written: its marker first, then the fields of its shape.
#[derive(Serialize)]
struct Stored<'a, T> {
    format: u32,
    #[serde(flatten)]
    document: &'a T,
}

impl<T: Serialize + DeserializeOwned> JsonDocument<T> {
    /// `<dir>/<file>`, which nothing creates until the first write. Writers lock `<stem>.lock` and
    /// stage `<file>.tmp` beside it.
    pub(crate) fn new(
        dir: impl Into<PathBuf>,
        file: &'static str,
        max_bytes: u64,
        format: u32,
    ) -> Self {
        Self {
            dir: dir.into(),
            file,
            max_bytes,
            format,
            check: |_| Ok(()),
            shape: PhantomData,
        }
    }

    /// Refuse a document that parses but is not consistent, with `check`'s reason.
    pub(crate) fn checked(self, check: fn(&T) -> Result<(), String>) -> Self {
        Self { check, ..self }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.dir.join(self.file)
    }

    /// What the file is called in a message: `settings` for `settings.json`.
    fn name(&self) -> &'static str {
        self.file
            .split_once('.')
            .map_or(self.file, |(stem, _)| stem)
    }

    fn incompatible(&self, reason: impl std::fmt::Display) -> Error {
        Error::new(
            ErrorKind::Incompatible,
            format!(
                "{}: {reason}; the file is kept unchanged",
                self.path().display()
            ),
        )
    }

    /// The document, or `None` when the file does not exist. It is read without the lock, which the
    /// rename makes safe: a reader sees the previous file or the next one, never a partial one.
    pub(crate) fn read(&self) -> Result<Option<T>, Error> {
        let Some(bytes) = atomic_file::read(&self.path(), self.max_bytes)? else {
            return Ok(None);
        };
        let name = self.name();
        let mut value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| self.incompatible(format!("not valid JSON ({error})")))?;
        match value.get(FORMAT).and_then(Value::as_u64) {
            Some(format) if format == u64::from(self.format) => {}
            Some(format) => {
                return Err(self.incompatible(format!("{name} format {format} is not supported")));
            }
            None => return Err(self.incompatible(format!("no {name} format marker"))),
        }
        if let Value::Object(fields) = &mut value {
            fields.remove(FORMAT);
        }
        let document: T = serde_json::from_value(value)
            .map_err(|error| self.incompatible(format!("not a {name} file ({error})")))?;
        (self.check)(&document).map_err(|reason| self.incompatible(reason))?;
        Ok(Some(document))
    }

    /// Replace the file with `document`. Only a caller that is the file's one writer calls this
    /// directly; everyone else changes a document through [`Self::transact`].
    pub(crate) fn write(&self, document: &T) -> Result<(), Error> {
        let bytes = self.encode(document)?;
        self.save(&bytes)
    }

    fn encode(&self, document: &T) -> Result<Vec<u8>, Error> {
        serde_json::to_vec_pretty(&Stored {
            format: self.format,
            document,
        })
        .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
    }

    fn save(&self, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() as u64 > self.max_bytes {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "{} would be {} bytes; the file is at most {}",
                    self.name(),
                    bytes.len(),
                    self.max_bytes
                ),
            ));
        }
        atomic_file::replace(&self.path(), bytes)
    }
}

impl<T: Serialize + DeserializeOwned + Default> JsonDocument<T> {
    /// One read-modify-write under the writers' lock: read the file again (an absent file is the
    /// empty document), let `apply` change it, and write it back only when it changed. An `apply`
    /// that fails writes nothing, so a refused change leaves the file exactly as it was.
    pub(crate) fn transact<R>(
        &self,
        apply: impl FnOnce(&mut T) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let _lock = atomic_file::lock(&self.dir, &format!("{}.lock", self.name()))?;
        let mut document = self.read()?.unwrap_or_default();
        let before = self.encode(&document)?;
        let answer = apply(&mut document)?;
        let after = self.encode(&document)?;
        if after != before {
            self.save(&after)?;
        }
        Ok(answer)
    }
}
