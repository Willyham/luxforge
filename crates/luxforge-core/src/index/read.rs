//! One file's header, read on an index worker: the file is opened, its signature taken from the open
//! handle, and its header read within the header reader's byte and read budget
//! (`export::metadata::header`). No image data is read and nothing is hashed. A file of a supported
//! extension whose container the reader does not recognize, or whose read fails, is listed as
//! unreadable with the reason.
use crate::{
    SourceTag,
    catalog_types::{FileRecord, FileSignature, HeaderState, VolumeId},
    export::metadata::header::{Container, read_header},
};
use std::{fs::File, io::ErrorKind, path::PathBuf};

/// One file whose header is to be read.
#[derive(Clone, Debug)]
pub(crate) struct FileTask {
    pub path: PathBuf,
    pub folder: PathBuf,
    pub name: String,
    pub kind: SourceTag,
    pub volume_id: VolumeId,
    /// The listing's time, which the record carries as when it was last seen.
    pub seen_ms: i64,
}

impl FileTask {
    /// The record of this file before its header is read.
    pub(crate) fn pending(&self, signature: FileSignature) -> FileRecord {
        self.record(signature, HeaderState::Pending)
    }

    fn record(&self, signature: FileSignature, header: HeaderState) -> FileRecord {
        FileRecord {
            path: self.path.clone(),
            folder: self.folder.clone(),
            name: self.name.clone(),
            volume_id: self.volume_id.clone(),
            signature,
            kind: self.kind,
            header,
            last_seen_ms: self.seen_ms,
        }
    }
}

/// What reading one file's header found.
#[derive(Debug)]
pub(crate) enum HeaderOutcome {
    /// The file's record, its header read or unreadable.
    Read(Box<FileRecord>),
    /// The file is gone since it was listed.
    Vanished(PathBuf),
}

/// Read the header of `task`'s file.
pub(crate) fn read_file(task: &FileTask) -> HeaderOutcome {
    let mut file = match File::open(&task.path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return HeaderOutcome::Vanished(task.path.clone());
        }
        Err(error) => return unreadable(task, None, &format!("cannot open it: {}", error.kind())),
    };
    let signature = match file.metadata() {
        Ok(metadata) => FileSignature::of(&metadata),
        Err(error) => return unreadable(task, None, &format!("cannot read it: {}", error.kind())),
    };
    let header = match read_header(&mut file, signature.len) {
        Ok(read) if read.header.container == Container::Unknown => {
            return unreadable(
                task,
                Some(signature),
                "not a JPEG or a RAW container Luxforge reads",
            );
        }
        Ok(read) => read.header.metadata(),
        Err(error) => {
            return unreadable(
                task,
                Some(signature),
                &format!("cannot read its header: {}", error.kind()),
            );
        }
    };
    HeaderOutcome::Read(Box::new(
        task.record(signature, HeaderState::Ok(Box::new(header))),
    ))
}

fn unreadable(task: &FileTask, signature: Option<FileSignature>, reason: &str) -> HeaderOutcome {
    let signature = signature.unwrap_or(FileSignature {
        len: 0,
        modified_ns: 0,
        identity: None,
    });
    HeaderOutcome::Read(Box::new(
        task.record(signature, HeaderState::Unreadable(reason.to_owned())),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(path: PathBuf) -> FileTask {
        FileTask {
            folder: path.parent().unwrap().to_path_buf(),
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            path,
            kind: SourceTag::Jpeg,
            volume_id: VolumeId::parse("volume-0123456789").unwrap(),
            seen_ms: 7,
        }
    }

    #[test]
    fn a_header_is_read_an_unknown_container_is_unreadable_and_a_gone_file_vanished() {
        let dir = luxforge_testbase::paths::temp_dir("index-read");
        let jpeg = dir.join("a.jpg");
        std::fs::copy(luxforge_testbase::paths::jpeg(), &jpeg).unwrap();
        let HeaderOutcome::Read(record) = read_file(&task(jpeg.clone())) else {
            panic!("read");
        };
        assert!(matches!(record.header, HeaderState::Ok(_)), "{record:?}");
        assert_eq!(
            record.signature.len,
            std::fs::metadata(&jpeg).unwrap().len()
        );
        assert_eq!(record.last_seen_ms, 7);

        let junk = dir.join("b.nef");
        std::fs::write(&junk, b"not a raw file").unwrap();
        let HeaderOutcome::Read(record) = read_file(&task(junk)) else {
            panic!("read");
        };
        assert!(
            matches!(&record.header, HeaderState::Unreadable(reason) if reason.contains("container")),
            "{record:?}"
        );
        assert_eq!(record.signature.len, 14);

        assert!(matches!(
            read_file(&task(dir.join("gone.jpg"))),
            HeaderOutcome::Vanished(_)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
