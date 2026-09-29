//! The one store of the small format-marked JSON files kept outside every catalog: the settings
//! document, the grants document, each installed resource's `installed.json` and the artifact
//! root's `manifest.json`. A [`JsonDocument`] is one file of one shape `T`,
//! `{format: <marker>, ...T}`, of at most `max_bytes`. Every call reads the file again, so two
//! processes never act on a stale copy. A file of another format, or one that is not the current shape, is refused with
//! `incompatible` and never rewritten. A change is one locked read-modify-write through the shared
//! durable write, so a failure at any point leaves the previous file. See
//! `docs/design/module-capabilities.md#settings-store`.
use crate::{Error, atomic_file};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{marker::PhantomData, path::PathBuf};

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

    #[cfg(test)]
    pub(crate) fn dir(&self) -> &std::path::Path {
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
        Error::incompatible(format!(
            "{}: {reason}; the file is kept unchanged",
            self.path().display()
        ))
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

    /// The bytes [`Self::write`] would write, for a writer that places them itself.
    pub(crate) fn encode(&self, document: &T) -> Result<Vec<u8>, Error> {
        serde_json::to_vec_pretty(&Stored {
            format: self.format,
            document,
        })
        .map_err(|error| Error::internal(error.to_string()))
    }

    fn save(&self, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() as u64 > self.max_bytes {
            return Err(Error::resource_limit(format!(
                "{} would be {} bytes; the file is at most {}",
                self.name(),
                bytes.len(),
                self.max_bytes
            )));
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

/// The document's own contract, once for its three callers: settings, grants and installed
/// resources test only what each adds.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorKind, capabilities::testing::temp};
    use serde::Deserialize;
    use serde_json::json;
    use std::{fs, path::Path};

    /// A small shape whose check refuses an empty note.
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Notes {
        notes: Vec<String>,
    }

    const FILE: &str = "notes.json";
    const MAX: u64 = 256;

    fn document(dir: &Path, max_bytes: u64) -> JsonDocument<Notes> {
        JsonDocument::new(dir, FILE, max_bytes, 3).checked(|document| {
            if document.notes.iter().any(String::is_empty) {
                Err("an empty note".into())
            } else {
                Ok(())
            }
        })
    }

    fn notes(items: &[&str]) -> Notes {
        Notes {
            notes: items.iter().map(|item| item.to_string()).collect(),
        }
    }

    /// A directory of its own, removed at the end.
    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_absent_file_reads_as_none_and_a_write_reads_back_under_its_marker() {
        let dir = Dir(temp("document-round-trip"));
        let document = document(&dir.0, MAX);
        assert_eq!(document.read().unwrap(), None);
        assert!(!dir.0.exists(), "a read creates nothing");
        // A direct write is its one writer's, which owns the directory.
        fs::create_dir_all(&dir.0).unwrap();
        document.write(&notes(&["a", "b"])).unwrap();
        assert_eq!(document.read().unwrap(), Some(notes(&["a", "b"])));
        let text = fs::read_to_string(document.path()).unwrap();
        assert_eq!(
            text.as_bytes(),
            document.encode(&notes(&["a", "b"])).unwrap()
        );
        assert!(
            text.find("\"format\"").unwrap() < text.find("\"notes\"").unwrap(),
            "the marker comes first: {text}"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"format": 3, "notes": ["a", "b"]})
        );
    }

    #[test]
    fn a_file_of_another_format_shape_or_consistency_is_incompatible_and_kept() {
        let dir = Dir(temp("document-refused"));
        fs::create_dir_all(&dir.0).unwrap();
        let document = document(&dir.0, MAX);
        for (contents, reason) in [
            (
                json!({"format": 4, "notes": []}).to_string(),
                "notes format 4 is not supported",
            ),
            (json!({"notes": []}).to_string(), "no notes format marker"),
            ("{\"format\": 3, \"notes\": [".to_owned(), "not valid JSON"),
            (
                json!({"format": 3, "notes": [], "extra": true}).to_string(),
                "not a notes file",
            ),
            (
                json!({"format": 3, "notes": [""]}).to_string(),
                "an empty note",
            ),
        ] {
            fs::write(document.path(), &contents).unwrap();
            let changed = document.transact(|document| {
                document.notes.push("new".into());
                Ok(())
            });
            for error in [document.read().unwrap_err(), changed.unwrap_err()] {
                assert_eq!(error.kind, ErrorKind::Incompatible, "{contents}: {error}");
                assert!(
                    error.detail.contains(reason),
                    "{contents}: {}",
                    error.detail
                );
                assert!(
                    error.detail.ends_with("; the file is kept unchanged"),
                    "{}",
                    error.detail
                );
            }
            assert_eq!(fs::read_to_string(document.path()).unwrap(), contents);
        }
    }

    #[test]
    fn the_file_is_bounded_when_it_is_read_and_when_it_is_written() {
        let dir = Dir(temp("document-bounded"));
        fs::create_dir_all(&dir.0).unwrap();
        let document = document(&dir.0, MAX);
        document.write(&notes(&["kept"])).unwrap();
        let kept = fs::read(document.path()).unwrap();
        // A write that would pass the bound is refused and leaves the file, and no staged copy.
        let long = "x".repeat(MAX as usize);
        let grown = document.transact(|document| {
            document.notes.push(long.clone());
            Ok(())
        });
        for error in [
            document.write(&notes(&[&long])).unwrap_err(),
            grown.unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::ResourceLimit, "{error}");
        }
        assert_eq!(fs::read(document.path()).unwrap(), kept);
        assert!(!dir.0.join(format!("{FILE}.tmp")).exists());
        // A file at the bound is read; one past it is refused without being read whole, and kept.
        let mut at = kept.clone();
        at.resize(MAX as usize, b' ');
        fs::write(document.path(), &at).unwrap();
        assert_eq!(document.read().unwrap(), Some(notes(&["kept"])));
        let mut over = at;
        over.push(b' ');
        fs::write(document.path(), &over).unwrap();
        assert_eq!(document.read().unwrap_err().kind, ErrorKind::ResourceLimit);
        assert_eq!(fs::read(document.path()).unwrap(), over);
    }

    #[test]
    fn a_transaction_writes_only_a_change_and_nothing_when_it_is_refused() {
        let dir = Dir(temp("document-transact"));
        let document = document(&dir.0, MAX);
        // An absent file is the empty document, and leaving it empty writes nothing.
        let read = document.transact(|document| Ok(document.notes.len()));
        assert_eq!(read.unwrap(), 0);
        assert!(!document.path().exists());
        // Written compactly by hand, so a rewrite would show: a transaction that ends where it
        // began leaves these very bytes, and so does one that is refused.
        let compact = json!({"format": 3, "notes": ["a"]}).to_string();
        fs::write(document.path(), &compact).unwrap();
        document
            .transact(|document| {
                document.notes.push("b".into());
                document.notes.pop();
                Ok(())
            })
            .unwrap();
        assert_eq!(fs::read_to_string(document.path()).unwrap(), compact);
        let refused = document.transact(|document| {
            document.notes.clear();
            Err::<(), _>(Error::validation("refused"))
        });
        assert_eq!(refused.unwrap_err().detail, "refused");
        assert_eq!(fs::read_to_string(document.path()).unwrap(), compact);
        document
            .transact(|document| {
                document.notes.push("b".into());
                Ok(())
            })
            .unwrap();
        assert_eq!(document.read().unwrap(), Some(notes(&["a", "b"])));
    }

    /// Each writer has its own instance, as a second process would; the writers' lock makes each
    /// read-modify-write whole, so no update is lost.
    #[test]
    fn two_writers_in_parallel_lose_no_update() {
        let dir = Dir(temp("document-parallel"));
        let writers: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|writer| {
                let document = document(&dir.0, 1 << 16);
                std::thread::spawn(move || {
                    for index in 0..20 {
                        document
                            .transact(|document| {
                                document.notes.push(format!("{writer}{index}"));
                                Ok(())
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let written = document(&dir.0, 1 << 16).read().unwrap().unwrap().notes;
        assert_eq!(written.len(), 40, "every update was kept");
        for writer in ["a", "b"] {
            let own: Vec<&String> = written
                .iter()
                .filter(|note| note.starts_with(writer))
                .collect();
            let expected: Vec<String> = (0..20).map(|index| format!("{writer}{index}")).collect();
            assert_eq!(
                own,
                expected.iter().collect::<Vec<_>>(),
                "{writer} in order"
            );
        }
    }
}
