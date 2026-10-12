//! The update feeds: `GET` and `HEAD /v1/updates/<target>/appcast.xml`, answered as static content
//! from the store directory the operator's publication step fills (`<dir>/<target>/appcast.xml`).
//!
//! A feed is read from disk only when its file changed, and held in memory otherwise, so at most
//! one copy of each target's feed (at most 4 MiB) is kept whatever the number of requests. Nothing
//! here reads a request header, a query string or a peer address, and nothing is logged.

use bytes::Bytes;
use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

/// The feed route's prefix.
pub const PREFIX: &str = "/v1/updates/";
/// Each target's one file.
pub const FILE: &str = "appcast.xml";
/// The targets a feed may be served for.
pub const TARGETS: [&str; 3] = ["macos-arm64", "linux-x86_64", "windows-x86_64"];
/// The largest feed served, as the clients read at most this much.
pub const MAX_FEED_BYTES: u64 = 4 * 1024 * 1024;
/// Lets a new release reach clients within an hour, through any cache on the way.
pub const CACHE_CONTROL: &str = "public, max-age=900";
pub const CONTENT_TYPE: &str = "application/rss+xml; charset=utf-8";

/// The target a request path names, when it is exactly a feed path.
pub fn target(path: &str) -> Option<&'static str> {
    let (target, file) = path.strip_prefix(PREFIX)?.split_once('/')?;
    if file != FILE {
        return None;
    }
    TARGETS.iter().copied().find(|t| *t == target)
}

/// What identifies one version of a feed file: a publication writes a new file in a new
/// directory, so its inode and modification time change.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    inode: u64,
}

impl Stamp {
    fn of(metadata: &Metadata) -> Stamp {
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(metadata);
        #[cfg(not(unix))]
        let inode = 0;
        Stamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            inode,
        }
    }
}

/// The store, and the last bytes read for each target.
pub struct Feeds {
    dir: PathBuf,
    cache: Mutex<HashMap<&'static str, (Stamp, Bytes)>>,
}

impl Feeds {
    pub fn new(dir: PathBuf) -> Feeds {
        Feeds {
            dir,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The target's feed as it is now, or `None` when there is none: absent, not a regular file,
    /// unreadable, larger than [`MAX_FEED_BYTES`] or changed while it was read. Blocks on the
    /// filesystem; call it off the async workers.
    pub fn read(&self, target: &'static str) -> Option<Bytes> {
        let file = File::open(self.dir.join(target).join(FILE)).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > MAX_FEED_BYTES {
            return None;
        }
        let stamp = Stamp::of(&metadata);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((cached, bytes)) = cache.get(target)
            && *cached == stamp
        {
            return Some(bytes.clone());
        }
        // Drop the old copy before reading the new one, so at most one is held per target.
        cache.remove(target);
        drop(cache);
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        (&file)
            .take(MAX_FEED_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 != metadata.len() {
            return None;
        }
        let bytes = Bytes::from(bytes);
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(target, (stamp, bytes.clone()));
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_feed_paths_name_a_target() {
        assert_eq!(
            target("/v1/updates/macos-arm64/appcast.xml"),
            Some("macos-arm64")
        );
        assert_eq!(
            target("/v1/updates/linux-x86_64/appcast.xml"),
            Some("linux-x86_64")
        );
        for path in [
            "/v1/updates/",
            "/v1/updates/linux-x86_64",
            "/v1/updates/linux-x86_64/",
            "/v1/updates/linux-x86_64/appcast.xml/",
            "/v1/updates/linux-x86_64/other.xml",
            "/v1/updates/linux-arm64/appcast.xml",
            "/v1/updates/../linux-x86_64/appcast.xml",
            "/v1/updates/linux-x86_64/../appcast.xml",
            "/v1/updates//linux-x86_64/appcast.xml",
            "/v1/updates/LINUX-X86_64/appcast.xml",
            "/v2/updates/linux-x86_64/appcast.xml",
        ] {
            assert_eq!(target(path), None, "{path}");
        }
    }
}
