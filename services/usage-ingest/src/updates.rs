//! The update feeds: `GET` and `HEAD /v1/updates/<target>/appcast.xml`, answered as static content
//! from the store directory the operator's publication step fills (`<dir>/<target>/appcast.xml`).
//!
//! A feed is read from disk only when its file changed, and held in memory otherwise, so at most
//! one load and one cached copy of each target's feed (at most 4 MiB) are held whatever the number
//! of requests. Responses share immutable bytes; active responses may retain an older publication.
//! Nothing here reads a request header, a query string or a peer address, and nothing is logged.

use bytes::Bytes;
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
    // Fixed target slots keep single-flight loading independent across targets.
    cache: [Mutex<Option<(Stamp, Bytes)>>; TARGETS.len()],
}

impl Feeds {
    pub fn new(dir: PathBuf) -> Feeds {
        Feeds {
            dir,
            cache: std::array::from_fn(|_| Mutex::new(None)),
        }
    }

    /// The target's feed as it is now, or `None` when there is none: absent, not a regular file,
    /// unreadable, larger than [`MAX_FEED_BYTES`] or changed while it was read. Blocks on the
    /// filesystem; call it off the async workers.
    pub fn read(&self, target: &'static str) -> Option<Bytes> {
        self.read_with(target, |file, bytes| {
            // Reserve one sentinel byte past the observed length, bounded by the feed cap.
            // A concurrent growth is rejected without growing a second maximum-sized buffer.
            read_bounded(file, bytes)?;
            Some(())
        })
    }

    fn read_with(
        &self,
        target: &'static str,
        load: impl FnOnce(&mut File, &mut Vec<u8>) -> Option<()>,
    ) -> Option<Bytes> {
        let slot = TARGETS.iter().position(|candidate| *candidate == target)?;
        // Hold the target's lock from open through publication: a second cold request cannot
        // allocate another buffer, and a later opener cannot publish an older read after it.
        let mut cached = self.cache[slot].lock().unwrap_or_else(|e| e.into_inner());
        let mut file = match File::open(self.dir.join(target).join(FILE)) {
            Ok(file) => file,
            Err(_) => {
                *cached = None;
                return None;
            }
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                *cached = None;
                return None;
            }
        };
        if !metadata.is_file() || metadata.len() > MAX_FEED_BYTES {
            *cached = None;
            return None;
        }
        let stamp = Stamp::of(&metadata);
        if let Some((previous, bytes)) = &*cached
            && *previous == stamp
        {
            return Some(bytes.clone());
        }
        // Drop the old copy before reading the new one, so at most one is held per target.
        *cached = None;
        let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
        load(&mut file, &mut bytes)?;
        // A same-size rewrite must be refused too; verify the opened file's stamp after reading.
        if bytes.len() as u64 != metadata.len() || Stamp::of(&file.metadata().ok()?) != stamp {
            return None;
        }
        let bytes = Bytes::from(bytes);
        *cached = Some((stamp, bytes.clone()));
        Some(bytes)
    }
}

/// Read into the already bounded allocation. `read_to_end` is allowed to reserve spare capacity,
/// so fill a fixed slice instead and retain only the bytes actually read.
fn read_bounded(file: &mut File, bytes: &mut Vec<u8>) -> Option<()> {
    bytes.resize(bytes.capacity(), 0);
    let mut filled = 0;
    while filled < bytes.len() {
        match file.read(&mut bytes[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    bytes.truncate(filled);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, mpsc};

    struct Store(PathBuf);

    impl Store {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "luxforge-ingest-cache-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::create_dir_all(&path).unwrap();
            for target in TARGETS {
                std::fs::create_dir(path.join(target)).unwrap();
                std::fs::write(path.join(target).join(FILE), target).unwrap();
            }
            Self(path)
        }

        fn file(&self, target: &str) -> PathBuf {
            self.0.join(target).join(FILE)
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn cold_reads_share_one_allocation_and_other_targets_stay_independent() {
        let store = Store::new();
        let feeds = Arc::new(Feeds::new(store.0.clone()));
        let (entered, loading) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let leader_feeds = feeds.clone();
        let leader = std::thread::spawn(move || {
            leader_feeds
                .read_with(TARGETS[0], |file, bytes| {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                    file.read_to_end(bytes).ok()?;
                    Some(())
                })
                .unwrap()
        });
        loading.recv().unwrap();
        assert!(feeds.cache[0].try_lock().is_err());
        let follower_feeds = feeds.clone();
        let follower = std::thread::spawn(move || {
            follower_feeds
                .read_with(TARGETS[0], |_, _| {
                    panic!("the follower must reuse the leader's buffer");
                })
                .unwrap()
        });
        // Completing this while the leader remains gated proves target independence.
        assert_eq!(
            feeds.read(TARGETS[1]).unwrap().as_ref(),
            TARGETS[1].as_bytes()
        );
        release.send(()).unwrap();
        let first = leader.join().unwrap();
        let second = follower.join().unwrap();
        assert_eq!(first.as_ptr(), second.as_ptr());
        assert_eq!(feeds.read(TARGETS[0]).unwrap().as_ptr(), first.as_ptr());
    }

    #[cfg(unix)]
    #[test]
    fn a_held_old_read_cannot_overwrite_a_new_publication() {
        let store = Store::new();
        let feeds = Arc::new(Feeds::new(store.0.clone()));
        let (entered, loading) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let leader_feeds = feeds.clone();
        let leader = std::thread::spawn(move || {
            leader_feeds
                .read_with(TARGETS[0], |file, bytes| {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                    file.read_to_end(bytes).ok()?;
                    Some(())
                })
                .unwrap()
        });
        loading.recv().unwrap();
        let staged = store.0.join("new.xml");
        std::fs::write(&staged, b"new publication").unwrap();
        std::fs::rename(staged, store.file(TARGETS[0])).unwrap();
        let follower_feeds = feeds.clone();
        let follower = std::thread::spawn(move || follower_feeds.read(TARGETS[0]).unwrap());
        release.send(()).unwrap();
        assert_eq!(leader.join().unwrap().as_ref(), TARGETS[0].as_bytes());
        let newest = follower.join().unwrap();
        assert_eq!(newest.as_ref(), b"new publication");
        assert_eq!(feeds.read(TARGETS[0]).unwrap().as_ptr(), newest.as_ptr());
    }

    #[test]
    fn replacement_drops_cached_bytes_before_loading_and_rechecks_the_stamp() {
        let store = Store::new();
        let feeds = Feeds::new(store.0.clone());
        let old = feeds.read(TARGETS[0]).unwrap();
        let path = store.file(TARGETS[0]);
        std::fs::write(&path, b"replacement").unwrap();
        let read = feeds.read_with(TARGETS[0], |file, bytes| {
            // The cache owns no old copy while replacement bytes are allocated/read.
            assert!(old.is_unique());
            file.read_to_end(bytes).ok()?;
            let changed = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            let later =
                file.metadata().unwrap().modified().unwrap() + std::time::Duration::from_secs(1);
            changed
                .set_times(std::fs::FileTimes::new().set_modified(later))
                .unwrap();
            Some(())
        });
        assert!(read.is_none());
        assert!(feeds.cache[0].lock().unwrap().is_none());
        assert_eq!(feeds.read(TARGETS[0]).unwrap().as_ref(), b"replacement");
    }

    #[test]
    fn feed_growth_is_bounded_and_refused_without_retaining_old_cache() {
        let store = Store::new();
        let feeds = Feeds::new(store.0.clone());
        feeds.read(TARGETS[0]).unwrap();
        std::fs::write(store.file(TARGETS[0]), b"replacement").unwrap();
        assert!(
            feeds
                .read_with(TARGETS[0], |file, bytes| {
                    std::fs::write(store.file(TARGETS[0]), b"replacement has grown").unwrap();
                    read_bounded(file, bytes)?;
                    assert_eq!(bytes.len(), b"replacement".len() + 1);
                    assert_eq!(bytes.capacity(), b"replacement".len() + 1);
                    Some(())
                })
                .is_none()
        );
        assert!(feeds.cache[0].lock().unwrap().is_none());
        let file = File::create(store.file(TARGETS[0])).unwrap();
        file.set_len(MAX_FEED_BYTES + 1).unwrap();
        assert!(feeds.read(TARGETS[0]).is_none());
    }

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
