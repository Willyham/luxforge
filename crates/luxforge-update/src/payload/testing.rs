//! Fakes for tests of the payload updater, here and in the desktop's port: a payload on disk, a
//! release archive signed with a test key, its feed, and a [`MemoryFetch`] serving them. Nothing
//! here touches the network. Built for this crate's tests and, with the `testing` feature, for
//! other crates' tests.
use super::{ARCHIVE_ROOT, BUILD_FILE, Fetch, FetchRequest};
use crate::{Appcast, AppcastItem, Enclosure, SigningKey, Target};
use flate2::{Compression, write::GzEncoder};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender, channel},
    },
};

/// The executable's name in a test payload.
pub const EXECUTABLE: &str = "luxforge";
/// Where test feeds live.
pub const FEED_BASE: &str = "https://updates.test/v1/updates/";
/// The bytes the fetch hands a sink at a time, as the desktop's transport does.
const PIECE: usize = 16 * 1024;

/// A throwaway key: it signs nothing real.
pub fn test_key() -> SigningKey {
    SigningKey::from_seed(&[42; 32]).expect("a 32-byte seed")
}

/// `build.json` as the release build writes it: the build number as a decimal string and the
/// target as its Rust triple.
pub fn build_json(version: &str, build: u64, target: Target) -> String {
    format!(
        "{{\"schema_version\":1,\"tag\":\"v{version}\",\"version\":\"{version}\",\
         \"build_number\":\"{build}\",\"catalog_format\":1,\"target\":\"{}\",\
         \"profile\":\"release\"}}",
        target.rust_target()
    )
}

/// An installed payload at `dir` (created with its parents): an executable and `build.json`.
/// Answers the executable's path.
pub fn install_payload(dir: &Path, version: &str, build: u64, target: Target) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let executable = dir.join(EXECUTABLE);
    fs::write(&executable, format!("luxforge {version}\n")).unwrap();
    set_mode(&executable, 0o755);
    fs::write(dir.join(BUILD_FILE), build_json(version, build, target)).unwrap();
    executable
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) {}

/// One entry of a hand-built archive.
#[derive(Clone, Debug)]
pub enum Entry {
    Directory(String, u32),
    File(String, Vec<u8>, u32),
    Symlink(String, String),
    HardLink(String, String),
    Fifo(String),
    CharDevice(String),
}

/// A `.tar.gz` of `entries`, in order, with paths written as given (no checks), so tests can make
/// archives a release build never would.
pub fn archive(entries: &[Entry]) -> Vec<u8> {
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        let (path, data, kind, mode, link): (&str, &[u8], _, u32, Option<&str>) = match entry {
            Entry::Directory(path, mode) => (path, &[], tar::EntryType::Directory, *mode, None),
            Entry::File(path, data, mode) => (path, data, tar::EntryType::Regular, *mode, None),
            Entry::Symlink(path, link) => (path, &[], tar::EntryType::Symlink, 0o777, Some(link)),
            Entry::HardLink(path, link) => (path, &[], tar::EntryType::Link, 0o644, Some(link)),
            Entry::Fifo(path) => (path, &[], tar::EntryType::Fifo, 0o644, None),
            Entry::CharDevice(path) => (path, &[], tar::EntryType::Char, 0o644, None),
        };
        header.set_entry_type(kind);
        header.set_mode(mode);
        header.set_size(data.len() as u64);
        header.set_mtime(0);
        // Written raw, so `..` and absolute paths reach the archive.
        write_name(&mut header, path);
        if let Some(link) = link {
            let gnu = header.as_gnu_mut().unwrap();
            gnu.linkname = [0; 100];
            gnu.linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        header.set_cksum();
        tar.append(&header, data).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

fn write_name(header: &mut tar::Header, path: &str) {
    let gnu = header.as_gnu_mut().unwrap();
    gnu.name = [0; 100];
    gnu.name[..path.len()].copy_from_slice(path.as_bytes());
}

/// A release payload's archive, as the release build writes it: `Luxforge/` with the executable,
/// `build.json`, a resource directory and its file.
pub fn release_archive(version: &str, build: u64, target: Target) -> Vec<u8> {
    archive(&release_entries(version, build, target))
}

/// The entries of [`release_archive`], for a test to add to or change.
pub fn release_entries(version: &str, build: u64, target: Target) -> Vec<Entry> {
    vec![
        Entry::Directory(format!("{ARCHIVE_ROOT}/"), 0o755),
        Entry::File(
            format!("{ARCHIVE_ROOT}/{EXECUTABLE}"),
            format!("luxforge {version}\n").into_bytes(),
            0o755,
        ),
        Entry::File(
            format!("{ARCHIVE_ROOT}/{BUILD_FILE}"),
            build_json(version, build, target).into_bytes(),
            0o644,
        ),
        Entry::Directory(format!("{ARCHIVE_ROOT}/lensfun/"), 0o755),
        Entry::File(
            format!("{ARCHIVE_ROOT}/lensfun/lenses.xml"),
            b"<lensdatabase/>\n".to_vec(),
            0o644,
        ),
    ]
}

/// A feed item for `bytes` at `url`, signed by `key`, on `channel` (`None` the default), with
/// `catalog_format`.
pub fn item(
    version: &str,
    build: u64,
    url: &str,
    bytes: &[u8],
    key: &SigningKey,
    channel: Option<&str>,
    catalog_format: Option<u32>,
) -> AppcastItem {
    AppcastItem {
        title: Some(format!("Luxforge {version}")),
        link: Some(format!(
            "https://github.com/Willyham/luxforge/releases/tag/v{version}"
        )),
        pub_date: None,
        build,
        version: version.into(),
        minimum_system_version: None,
        channel: channel.map(str::to_owned),
        catalog_format,
        enclosure: Enclosure {
            url: url.into(),
            length: bytes.len() as u64,
            mime_type: Some(crate::ENCLOSURE_TYPE.into()),
            ed_signature: key.sign(bytes),
        },
    }
}

/// The appcast of `items`.
pub fn feed(items: Vec<AppcastItem>) -> String {
    Appcast {
        title: "Luxforge".into(),
        link: None,
        description: None,
        items,
    }
    .to_xml()
}

/// What a [`MemoryFetch`] answers for a URL.
#[derive(Clone, Debug)]
enum Answer {
    Bytes(Vec<u8>),
    Failure(String),
    /// Write this many of the bytes, say so on the sender, then wait until the request is
    /// cancelled.
    Stall(Vec<u8>, usize, Sender<()>),
}

/// A [`Fetch`] that answers from memory and records every URL asked for.
#[derive(Default)]
pub struct MemoryFetch {
    answers: Mutex<HashMap<String, Answer>>,
    requests: Mutex<Vec<String>>,
}

impl MemoryFetch {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Answer `url` with `bytes`.
    pub fn serve(&self, url: &str, bytes: impl Into<Vec<u8>>) {
        self.answers
            .lock()
            .unwrap()
            .insert(url.into(), Answer::Bytes(bytes.into()));
    }

    /// Answer `url` with a failure.
    pub fn fail(&self, url: &str, message: &str) {
        self.answers
            .lock()
            .unwrap()
            .insert(url.into(), Answer::Failure(message.into()));
    }

    /// Answer `url` with the first `after` of `bytes`, then wait until the request is cancelled.
    /// The receiver hears once each time a request has written those bytes and waits.
    pub fn stall(&self, url: &str, bytes: impl Into<Vec<u8>>, after: usize) -> Receiver<()> {
        let (stalled, heard) = channel();
        self.answers
            .lock()
            .unwrap()
            .insert(url.into(), Answer::Stall(bytes.into(), after, stalled));
        heard
    }

    /// Every URL asked for, in order.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn pieces(bytes: &[u8], sink: &mut dyn Write) -> Result<(), String> {
    for piece in bytes.chunks(PIECE) {
        sink.write_all(piece).map_err(|error| error.to_string())?;
    }
    Ok(())
}

impl Fetch for MemoryFetch {
    fn fetch(&self, request: FetchRequest<'_>, sink: &mut dyn Write) -> Result<(), String> {
        self.requests.lock().unwrap().push(request.url.to_owned());
        let answer = self.answers.lock().unwrap().get(request.url).cloned();
        match answer {
            None => Err(format!("the server answered HTTP 404 for {}", request.url)),
            Some(Answer::Failure(message)) => Err(message),
            Some(Answer::Bytes(bytes)) => pieces(&bytes, sink),
            Some(Answer::Stall(bytes, after, stalled)) => {
                pieces(&bytes[..after.min(bytes.len())], sink)?;
                // Woken by the request's cancel, as a transport's blocked read is.
                let (cancelled, woken) = channel();
                request.cancel.on_cancel(move || {
                    let _ = cancelled.send(());
                });
                let _ = stalled.send(());
                let _ = woken.recv();
                Err("the request was cancelled".into())
            }
        }
    }
}
