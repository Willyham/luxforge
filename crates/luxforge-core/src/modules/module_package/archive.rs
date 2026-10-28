//! The package's zip, read in memory within its bounds: the end of central directory record is
//! read first, so the archive's entry count and directory size are known before the zip reader
//! parses a directory entry, and each entry's declared size is checked before it is decompressed
//! and its actual size while it is.
use super::{
    COMPONENT_FILE, MANIFEST_FILE, MAX_COMPONENT_BYTES, MAX_MANIFEST_BYTES, MAX_PACKAGE_BYTES,
    MAX_PROGRAM_BYTES, PROGRAM_FILE, PackageRefusal, too_large_package,
};
use crate::Error;
use std::io::{Cursor, Read};
use zip::read::{ArchiveOffset, Config, ZipFileEntry};
use zip::{CompressionMethod, ZipArchive};

/// The most entries a package holds: the manifest, the component and the program.
const MAX_ENTRIES: u16 = 3;
/// The largest central directory three entries need, with room for the usual extra fields.
const MAX_DIRECTORY_BYTES: u32 = 16 * 1024;
/// The end of central directory record's signature and its fixed length before the comment.
const END_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const END_LENGTH: usize = 22;
/// The zip64 end of central directory locator's signature and length, just before the record.
const ZIP64_LOCATOR_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
const ZIP64_LOCATOR_LENGTH: usize = 20;

/// The three files a package may hold, each read whole.
pub(super) struct Entries {
    pub manifest: Vec<u8>,
    pub component: Vec<u8>,
    pub program: Option<Vec<u8>>,
}

/// Which of the package's files an entry is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum File {
    Manifest,
    Component,
    Program,
}

impl File {
    fn name(self) -> &'static str {
        match self {
            Self::Manifest => MANIFEST_FILE,
            Self::Component => COMPONENT_FILE,
            Self::Program => PROGRAM_FILE,
        }
    }

    fn limit(self) -> u64 {
        match self {
            Self::Manifest => MAX_MANIFEST_BYTES,
            Self::Component => MAX_COMPONENT_BYTES,
            Self::Program => MAX_PROGRAM_BYTES,
        }
    }
}

fn archive(detail: impl std::fmt::Display) -> Error {
    PackageRefusal::Archive.error(format!("the package is not a readable zip: {detail}"))
}

/// The entry count the end of central directory record declares, after checking the record
/// describes one archive starting at the first byte, with no zip64 record and a directory within
/// its bound. The record is the last one whose comment runs exactly to the end of the bytes.
fn declared_entries(bytes: &[u8]) -> Result<u16, Error> {
    let le16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let le32 =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let last = bytes
        .len()
        .checked_sub(END_LENGTH)
        .ok_or_else(|| archive("it has no end of central directory record"))?;
    let first = last.saturating_sub(usize::from(u16::MAX));
    let end = (first..=last)
        .rev()
        .find(|&at| {
            bytes[at..at + 4] == END_SIGNATURE
                && at + END_LENGTH + usize::from(le16(at + 20)) == bytes.len()
        })
        .ok_or_else(|| archive("it has no end of central directory record"))?;
    if end >= ZIP64_LOCATOR_LENGTH
        && bytes[end - ZIP64_LOCATOR_LENGTH..end - ZIP64_LOCATOR_LENGTH + 4]
            == ZIP64_LOCATOR_SIGNATURE
    {
        return Err(archive("zip64 archives are not packages"));
    }
    let (disk, directory_disk, on_disk, entries) =
        (le16(end + 4), le16(end + 6), le16(end + 8), le16(end + 10));
    let (size, offset) = (le32(end + 12), le32(end + 16));
    if disk != 0 || directory_disk != 0 || on_disk != entries {
        return Err(archive("split archives are not packages"));
    }
    if entries > MAX_ENTRIES {
        return Err(PackageRefusal::Entries.error(format!(
            "the package has {entries} entries; a package holds at most {MAX_ENTRIES}: \
             {MANIFEST_FILE}, {COMPONENT_FILE} and {PROGRAM_FILE}"
        )));
    }
    if size > MAX_DIRECTORY_BYTES {
        return Err(archive(format!(
            "its central directory is {size} bytes, more than the {MAX_DIRECTORY_BYTES} three \
             entries need"
        )));
    }
    if u64::from(offset) + u64::from(size) != end as u64 {
        return Err(archive(
            "its central directory does not end where its end record begins",
        ));
    }
    Ok(entries)
}

/// Which package file an entry named `name` is, refusing a name that leaves the package root and
/// any other entry.
fn entry_file(name: &str) -> Result<File, Error> {
    let outside = name.starts_with(['/', '\\'])
        || name.split(['/', '\\']).any(|part| part == "..")
        || name.contains(':')
        || name.contains('\0');
    if outside {
        return Err(PackageRefusal::OutsideRoot.error(format!(
            "package entry {name:?} is outside the package root"
        )));
    }
    [File::Manifest, File::Component, File::Program]
        .into_iter()
        .find(|file| file.name() == name)
        .ok_or_else(|| {
            PackageRefusal::UnexpectedEntry.error(format!(
                "package entry {name:?} is not part of a module package, which holds only \
                 {MANIFEST_FILE}, {COMPONENT_FILE} and {PROGRAM_FILE} at its root"
            ))
        })
}

/// An entry's name and which package file it is, from its directory record alone: a file of the
/// package, not a link or a directory, unencrypted, stored or Deflate, and no larger than its limit
/// says it is.
fn check_entry(entry: &ZipFileEntry<'_>) -> Result<(&'static str, File), Error> {
    let name = std::str::from_utf8(entry.name_raw())
        .map_err(|_| archive("an entry's name is not UTF-8"))?;
    let file = entry_file(name)?;
    let name = file.name();
    if entry.is_dir() || entry.is_symlink() {
        return Err(PackageRefusal::UnexpectedEntry.error(format!(
            "package entry {name} is a link or a directory, not a file"
        )));
    }
    if entry.encrypted() {
        return Err(archive(format!("{name} is encrypted")));
    }
    let compression = entry.compression();
    if !matches!(
        compression,
        CompressionMethod::Stored | CompressionMethod::Deflated
    ) {
        return Err(archive(format!(
            "{name} is compressed with {compression}, not stored or Deflate"
        )));
    }
    if entry.size() > file.limit() {
        return Err(PackageRefusal::TooLarge.error(format!(
            "{name} is {} bytes, more than the {} it may hold",
            entry.size(),
            file.limit()
        )));
    }
    Ok((name, file))
}

/// The package's files, or the refusal of the first thing wrong with the archive.
pub(super) fn read(bytes: &[u8]) -> Result<Entries, Error> {
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(too_large_package(bytes.len() as u64));
    }
    let declared = declared_entries(bytes)?;
    let config = Config {
        archive_offset: ArchiveOffset::Known(0),
    };
    let mut zip = ZipArchive::with_config(config, Cursor::new(bytes)).map_err(archive)?;
    if zip.len() != usize::from(declared) {
        return Err(
            PackageRefusal::Entries.error("the package names one entry more than once".to_owned())
        );
    }
    let (mut manifest, mut component, mut program) = (None, None, None);
    for index in 0..zip.len() {
        let (name, file) = check_entry(&zip.by_index_data(index).map_err(archive)?)?;
        let limit = file.limit();
        let too_large = |size: u64| {
            PackageRefusal::TooLarge.error(format!(
                "{name} is {size} bytes, more than the {limit} it may hold"
            ))
        };
        let mut reader = zip.by_index(index).map_err(archive)?;
        // The declared size bounds the allocation; the read is bounded again in case it lies,
        // and a whole read checks the entry's CRC-32.
        let mut content = Vec::with_capacity(usize::try_from(reader.size()).unwrap_or(0));
        (&mut reader)
            .take(limit + 1)
            .read_to_end(&mut content)
            .map_err(|error| archive(format!("{name}: {error}")))?;
        if content.len() as u64 > limit {
            return Err(too_large(content.len() as u64));
        }
        let slot = match file {
            File::Manifest => &mut manifest,
            File::Component => &mut component,
            File::Program => &mut program,
        };
        if slot.replace(content).is_some() {
            return Err(
                PackageRefusal::Entries.error(format!("the package names {name} more than once"))
            );
        }
    }
    let missing = |file: File| {
        PackageRefusal::MissingFile.error(format!("the package has no {}", file.name()))
    };
    Ok(Entries {
        manifest: manifest.ok_or_else(|| missing(File::Manifest))?,
        component: component.ok_or_else(|| missing(File::Component))?,
        program,
    })
}
