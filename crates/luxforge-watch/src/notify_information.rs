//! The buffer `ReadDirectoryChangesW` fills: a chain of `FILE_NOTIFY_INFORMATION` records, each
//! an entry's path relative to the watched folder and what happened to it. It is plain bytes, so
//! it is parsed and tested on every platform; `windows.rs` reads it.
#![forbid(unsafe_code)]
#![cfg_attr(
    not(windows),
    allow(dead_code, reason = "Windows reads it; its tests run everywhere")
)]
use std::path::{Path, PathBuf};

/// The fixed part of a record: `NextEntryOffset`, `Action` and `FileNameLength`, each a
/// little-endian `u32`.
const HEADER: usize = 12;

/// One record: what happened, and the path relative to the watched folder as UTF-16.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    /// `FILE_ACTION_ADDED` (1), `REMOVED` (2), `MODIFIED` (3), `RENAMED_OLD_NAME` (4) or
    /// `RENAMED_NEW_NAME` (5). Every one is a path to look at again, so none is told apart.
    pub action: u32,
    pub name: Vec<u16>,
}

/// The records in a filled buffer, in order. Each record's `NextEntryOffset` counts from its own
/// start and is 0 on the last; a record that runs past the buffer, or an offset that does not
/// move forward within it, ends the chain.
pub(crate) fn parse(buffer: &[u8]) -> Vec<Record> {
    let mut records = Vec::new();
    let mut at = 0;
    loop {
        let Some(header) = buffer.get(at..at + HEADER) else {
            break;
        };
        let word = |from: usize| {
            u32::from_le_bytes([
                header[from],
                header[from + 1],
                header[from + 2],
                header[from + 3],
            ]) as usize
        };
        let (next, action, length) = (word(0), word(4), word(8));
        let Some(name) = buffer.get(at + HEADER..at + HEADER + length) else {
            break;
        };
        records.push(Record {
            action: action as u32,
            name: name
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect(),
        });
        if next == 0 {
            break;
        }
        at += next;
    }
    records
}

/// The path under `root` a record names. Its components are separated by `\`.
pub(crate) fn path(root: &Path, record: &Record) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        root.join(std::ffi::OsString::from_wide(&record.name))
    }
    #[cfg(not(windows))]
    {
        let mut path = root.to_path_buf();
        path.extend(String::from_utf16_lossy(&record.name).split('\\'));
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A buffer as the system fills it: records aligned to 4 bytes, each pointing at the next.
    fn buffer(records: &[(u32, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (index, (action, name)) in records.iter().enumerate() {
            let name: Vec<u16> = name.encode_utf16().collect();
            let length = HEADER + name.len() * 2;
            let next = if index + 1 == records.len() {
                0
            } else {
                length.next_multiple_of(4)
            };
            bytes.extend_from_slice(&(next as u32).to_le_bytes());
            bytes.extend_from_slice(&action.to_le_bytes());
            bytes.extend_from_slice(&((name.len() * 2) as u32).to_le_bytes());
            for unit in name {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            bytes.resize(bytes.len() + next.saturating_sub(length), 0);
        }
        bytes
    }

    #[test]
    fn the_buffer_parses_into_records_and_their_paths() {
        let bytes = buffer(&[
            (1, "DCIM\\100NIKON\\DSC_0001.NEF"),
            (4, "a.jpg"),
            (5, "Ålesund.jpg"),
        ]);
        let records = parse(&bytes);
        assert_eq!(
            records
                .iter()
                .map(|record| record.action)
                .collect::<Vec<_>>(),
            [1, 4, 5]
        );
        let root = Path::new("/photos");
        let paths: Vec<PathBuf> = records.iter().map(|record| path(root, record)).collect();
        assert_eq!(
            paths,
            [
                root.join("DCIM").join("100NIKON").join("DSC_0001.NEF"),
                root.join("a.jpg"),
                root.join("Ålesund.jpg"),
            ]
        );
    }

    #[test]
    fn a_record_past_the_buffer_ends_the_chain() {
        let bytes = buffer(&[(3, "a.jpg"), (3, "b.jpg")]);
        assert_eq!(parse(&bytes[..bytes.len() - 2]).len(), 1);
        assert!(parse(&bytes[..8]).is_empty());
        assert!(parse(&[]).is_empty());
    }
}
