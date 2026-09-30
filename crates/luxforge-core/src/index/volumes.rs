//! Volumes: which volume a path is on, the volumes mounted now, the camera cards among them, and
//! whether a folder on a known volume is offline.
//!
//! The mount table comes from the platform (`luxforge_process::mounts`), or from a fixed list a
//! test stands in with ([`MountSource::Fixed`]), so cards and offline volumes are tested with
//! scratch directories as mount points. A volume's identity is the platform's volume UUID where
//! it has one (macOS), so a card is the same volume every time it is mounted; otherwise it is the
//! device number, which is not stable across remounts. A volume's label is the platform's volume
//! name, else its mount point's name.
//!
//! On macOS the startup disk is two file systems — the sealed system volume at `/` and the data
//! volume at `/System/Volumes/Data`, joined by firmlinks so `/Users` is on the data volume — and is
//! one volume here: mounted at `/`, named as the system volume, identified by the data volume's
//! UUID, since that is where a person's files are.
//!
//! Reading the mount table ([`MountSource::list`]) waits on no file system, so the catalog owner
//! may read it. Everything else here stats: a mount point's device (the identity of a volume
//! without a UUID, and which volume a path is on) and a card's `DCIM` folder. That runs on the
//! index lane's threads (`super::survey`, `super::query`), never on the owner, and looks only at
//! the mounts it needs: [`volume_in`] stats the mounts that could hold the path, and [`card_of`]
//! the mounts that could carry the volume, so a hung network volume elsewhere is never touched.
use crate::{
    Error,
    atomic_file::file_error,
    catalog_types::{Volume, VolumeId},
};
use luxforge_process::Mount;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// The platform's entry for one mounted file system, as the index's other modules name it, so the
/// platform crate is named in one place.
pub(crate) type PlatformMount = Mount;

/// Where the startup disk's data volume is mounted on macOS.
const MACOS_DATA_VOLUME: &str = "/System/Volumes/Data";
/// The folder that makes a mounted volume a camera card (the DCF standard's image root).
pub(crate) const CARD_FOLDER: &str = "DCIM";

/// Where the mount table comes from.
#[derive(Clone, Debug, Default)]
pub(crate) enum MountSource {
    /// The platform's mount table, read each time it is asked for.
    #[default]
    Platform,
    /// A fixed list, which a test stands in with and may change, as a card is taken out.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "tests stand in with a fixed table")
    )]
    Fixed(Arc<Mutex<Vec<Mount>>>),
}

impl MountSource {
    /// The mounted file systems now, in the platform's order, read without waiting on any of them:
    /// `getfsstat` with `MNT_NOWAIT` on macOS (reading a volume's name and UUID only when it is
    /// local), `/proc/self/mountinfo` on Linux. So the catalog owner may read it. The system's own
    /// mounts are left out, but for the macOS data volume, which names the startup disk: none is a
    /// person's volume, and an automounter's trigger among them can mount, and wait on a network,
    /// when it is looked at. A platform that cannot read its table gives an empty one, in which
    /// every path's volume is found by its device ([`MountTable::volume_of`]).
    pub(crate) fn list(&self) -> Vec<Mount> {
        let mounts = match self {
            Self::Platform => luxforge_process::mounts().unwrap_or_default(),
            Self::Fixed(mounts) => mounts.lock().expect("a fixed mount table").clone(),
        };
        mounts
            .into_iter()
            .filter(|mount| mount.browsable || mount.mount_point == Path::new(MACOS_DATA_VOLUME))
            .collect()
    }

    /// The mount table now, with every mounted volume's identity: a stat of each mount point.
    #[cfg(test)]
    pub(crate) fn read(&self, now_ms: i64) -> MountTable {
        MountTable::of(self.list(), now_ms)
    }
}

/// One mounted volume.
#[derive(Clone, Debug)]
pub(crate) struct Mounted {
    pub volume: Volume,
    /// The startup disk.
    pub startup: bool,
    /// The platform's entry it was learned from, which says whether it is still mounted.
    pub source: Mount,
    /// The path prefixes it serves, each with the device its files carry: its mount point, and for
    /// the macOS startup disk both of its file systems.
    serves: Vec<(PathBuf, Option<u64>)>,
    /// Its `DCIM` folder, when [`MountTable::find_cards`] found one.
    dcim: Option<PathBuf>,
}

impl Mounted {
    /// Its `DCIM` folder when it is a camera card, as [`MountTable::find_cards`] found it.
    pub(crate) fn card_folder(&self) -> Option<PathBuf> {
        self.dcim.clone()
    }

    /// Look for its `DCIM` folder: a camera card is a volume other than the startup disk with a
    /// `DCIM` directory at its root. One stat.
    fn find_card(&mut self) {
        let dcim = self.volume.mount_point.join(CARD_FOLDER);
        let card = !self.startup
            && dcim
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.is_dir());
        self.dcim = card.then_some(dcim);
    }
}

/// The volumes mounted at one moment.
#[derive(Clone, Debug)]
pub(crate) struct MountTable {
    mounted: Vec<Mounted>,
    now_ms: i64,
}

impl MountTable {
    /// The table the platform's `mounts` describe.
    pub(crate) fn of(mounts: Vec<Mount>, now_ms: i64) -> Self {
        let data = mounts
            .iter()
            .find(|mount| mount.mount_point == Path::new(MACOS_DATA_VOLUME))
            .cloned();
        let mut mounted = Vec::with_capacity(mounts.len());
        for mount in mounts {
            // The system's own mounts are never a person's volume and are not even looked at: an
            // automounter's trigger can mount, and wait on a network, when it is.
            if !mount.browsable || mount.mount_point == Path::new(MACOS_DATA_VOLUME) {
                continue;
            }
            let source = mount.clone();
            let device = device_of(&mount.mount_point);
            let mut serves = vec![(mount.mount_point.clone(), device)];
            let mut uuid = mount.uuid;
            if mount.root
                && let Some(data) = &data
            {
                // The startup disk: the data volume serves `/` too, and names the volume.
                serves.push((PathBuf::from("/"), device_of(&data.mount_point)));
                serves.push((data.mount_point.clone(), device_of(&data.mount_point)));
                uuid = data.uuid.or(uuid);
            }
            let id = match (uuid, device) {
                (Some(uuid), _) => volume_id_of_uuid(uuid),
                (None, Some(device)) => volume_id_of_device(device),
                (None, None) => continue,
            };
            let label = mount
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .or_else(|| {
                    mount
                        .mount_point
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| STARTUP_LABEL.to_owned());
            mounted.push(Mounted {
                volume: Volume {
                    id,
                    mount_point: mount.mount_point,
                    label,
                    removable: mount.removable,
                    platform_id: uuid.map(|uuid| hex(&uuid)),
                    last_seen_ms: now_ms,
                },
                startup: mount.root,
                source,
                serves,
                dcim: None,
            });
        }
        Self { mounted, now_ms }
    }

    /// Look for each volume's `DCIM` folder, which makes it a camera card: one stat per volume.
    pub(crate) fn find_cards(&mut self) {
        for mounted in &mut self.mounted {
            mounted.find_card();
        }
    }

    /// The volumes a person browses — the startup disk and every browsable mount — in the
    /// table's order, taken out of the table.
    pub(crate) fn into_volumes(self) -> Vec<Mounted> {
        self.mounted
    }

    /// The volume the existing file or directory at `path` is on: the mounted volume whose served
    /// prefix is the longest one of the path on the path's own device. A path on no volume in the
    /// table (the platform's table was unreadable, or a test's fixed one lacks it) is on the volume
    /// of its device, mounted at its highest ancestor on that device and named after it.
    pub(crate) fn volume_of(&self, path: &Path) -> Result<Volume, Error> {
        let canonical = path
            .canonicalize()
            .map_err(|error| file_error("cannot resolve the volume", error.kind()))?;
        let device = device_of(&canonical)
            .ok_or_else(|| Error::file_access("cannot read the volume of the file"))?;
        let found = self
            .mounted
            .iter()
            .flat_map(|mounted| {
                mounted
                    .serves
                    .iter()
                    .filter(|(prefix, served)| {
                        *served == Some(device) && canonical.starts_with(prefix)
                    })
                    .map(move |(prefix, _)| (prefix.as_os_str().len(), mounted))
            })
            .max_by_key(|(length, _)| *length);
        if let Some((_, mounted)) = found {
            return Ok(Volume {
                last_seen_ms: self.now_ms,
                ..mounted.volume.clone()
            });
        }
        let mut mount_point = canonical.as_path();
        while let Some(parent) = mount_point.parent() {
            if device_of(parent) != Some(device) {
                break;
            }
            mount_point = parent;
        }
        let label = mount_point.file_name().map_or_else(
            || STARTUP_LABEL.to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        Ok(Volume {
            id: volume_id_of_device(device),
            mount_point: mount_point.to_path_buf(),
            label,
            removable: false,
            platform_id: None,
            last_seen_ms: self.now_ms,
        })
    }
}

/// What a volume without a name of its own is called when it is the root.
const STARTUP_LABEL: &str = "Startup disk";

/// The volume the existing file or directory at `path` is on, seen now (`last_seen_ms`), by the
/// platform's mount table ([`volume_in`]).
pub(crate) fn volume_of(path: &Path, now_ms: i64) -> Result<Volume, Error> {
    volume_in(&MountSource::Platform.list(), path, now_ms)
}

/// The volume the existing file or directory at `path` is on, by the mounted file systems
/// `mounts` ([`MountTable::volume_of`]), statting only the mounts that could hold it: those
/// mounted at one of its ancestors, and the startup disk's file systems. A volume mounted
/// elsewhere, a hung network volume among them, is not looked at.
pub(crate) fn volume_in(mounts: &[Mount], path: &Path, now_ms: i64) -> Result<Volume, Error> {
    let canonical = path
        .canonicalize()
        .map_err(|error| file_error("cannot resolve the volume", error.kind()))?;
    let candidates = mounts
        .iter()
        .filter(|mount| {
            mount.root
                || mount.mount_point == Path::new(MACOS_DATA_VOLUME)
                || canonical.starts_with(&mount.mount_point)
        })
        .cloned()
        .collect();
    MountTable::of(candidates, now_ms).volume_of(&canonical)
}

/// The volume `id`, when it is mounted, by the mounted file systems `mounts`, statting only the
/// mounts that could carry that identity: the startup disk's file systems, and the mount with that
/// UUID for an identity made from one, or the mounts without a UUID for one made from a device.
pub(crate) fn mounted_in(mounts: &[Mount], id: &VolumeId, now_ms: i64) -> Option<Mounted> {
    let candidates = mounts
        .iter()
        .filter(|mount| {
            mount.root
                || mount.mount_point == Path::new(MACOS_DATA_VOLUME)
                || match mount.uuid {
                    Some(uuid) => volume_id_of_uuid(uuid) == *id,
                    None => id.as_str().starts_with(DEVICE_ID_PREFIX),
                }
        })
        .cloned()
        .collect();
    MountTable::of(candidates, now_ms)
        .mounted
        .into_iter()
        .find(|mounted| mounted.volume.id == *id)
}

/// The mounted volume `id` with its `DCIM` folder, when it is a camera card, by the mounted file
/// systems `mounts`, looking only at what [`mounted_in`] does and at that one `DCIM` folder.
pub(crate) fn card_of(mounts: &[Mount], id: &VolumeId, now_ms: i64) -> Option<(Volume, PathBuf)> {
    let mut mounted = mounted_in(mounts, id, now_ms)?;
    mounted.find_card();
    let dcim = mounted.card_folder()?;
    Some((mounted.volume, dcim))
}

fn volume_id_of_uuid(uuid: [u8; 16]) -> VolumeId {
    VolumeId::parse(format!("{}{}", VolumeId::PREFIX, hex(&uuid)))
        .expect("32 hex digits make a volume identity")
}

/// How an identity made from a device number starts: `volume-dev`.
const DEVICE_ID_PREFIX: &str = "volume-dev";

fn volume_id_of_device(device: u64) -> VolumeId {
    VolumeId::parse(format!("{DEVICE_ID_PREFIX}{device:016x}"))
        .expect("a device number makes a volume identity")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(unix)]
pub(crate) fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    path.symlink_metadata().ok().map(|metadata| metadata.dev())
}

#[cfg(not(unix))]
pub(crate) fn device_of(path: &Path) -> Option<u64> {
    // One volume per path prefix until the platform's volume serial number is read.
    path.components().next().map(|prefix| {
        prefix
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .fold(0_u64, |hash, byte| {
                hash.wrapping_mul(31).wrapping_add(u64::from(*byte))
            })
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    impl MountTable {
        fn volumes(&self) -> impl Iterator<Item = &Mounted> {
            self.mounted.iter()
        }

        fn is_mounted(&self, id: &VolumeId) -> bool {
            self.mounted.iter().any(|mounted| mounted.volume.id == *id)
        }

        /// The folder at `path`, last seen on volume `volume`, is not there and its volume is
        /// not mounted.
        fn offline(&self, path: &Path, volume: &VolumeId) -> bool {
            path.symlink_metadata().is_err() && !self.is_mounted(volume)
        }
    }

    /// A mount entry for a scratch directory standing in as a volume.
    pub(crate) fn mount_at(path: &Path, name: &str, uuid: u8, removable: bool) -> Mount {
        Mount {
            mount_point: path.to_path_buf(),
            name: Some(name.into()),
            uuid: Some([uuid; 16]),
            file_system: "msdos".into(),
            removable,
            local: true,
            browsable: true,
            root: false,
        }
    }

    #[test]
    fn a_file_and_its_folder_are_on_one_volume() {
        let dir = luxforge_testbase::paths::temp_dir("volume-of");
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"x").unwrap();
        let of_file = volume_of(&file, 1).unwrap();
        let of_dir = volume_of(&dir, 2).unwrap();
        assert_eq!(of_file.id, of_dir.id);
        assert_eq!(of_file.mount_point, of_dir.mount_point);
        assert_eq!(of_dir.last_seen_ms, 2);
        assert!(
            file.canonicalize()
                .unwrap()
                .starts_with(&of_file.mount_point)
        );
        assert!(volume_of(&dir.join("missing.jpg"), 3).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// On macOS a person's files are on the startup disk, mounted at `/` and identified by its
    /// UUID, whichever of its two file systems they are on.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_home_folder_is_on_the_startup_disk() {
        let table = MountSource::Platform.read(5);
        let home = volume_of(Path::new(env!("CARGO_MANIFEST_DIR")), 5).unwrap();
        assert_eq!(home.mount_point, Path::new("/"));
        assert!(home.platform_id.is_some(), "identified by its UUID");
        assert_eq!(home.id.as_str().len(), "volume-".len() + 32);
        let startup: Vec<_> = table.volumes().filter(|mounted| mounted.startup).collect();
        assert_eq!(startup.len(), 1, "the startup disk is listed once");
        assert_eq!(startup[0].volume.id, home.id);
        assert!(table.is_mounted(&home.id));
        assert!(startup[0].card_folder().is_none());
    }

    /// A scratch directory standing in as a mounted card: a path inside it is on that volume, with
    /// its name, UUID and removability; its `DCIM` folder makes it a card; a folder on it is
    /// offline only once it is gone and the card is no longer in the table.
    #[test]
    fn a_fixed_table_stands_in_for_a_card() {
        let dir = luxforge_testbase::paths::temp_dir("volume-card")
            .canonicalize()
            .unwrap();
        let card = dir.join("NIKON Z 8");
        let photos = card.join("DCIM/100NZ8_1");
        std::fs::create_dir_all(&photos).unwrap();
        let plain = dir.join("SSD");
        std::fs::create_dir_all(&plain).unwrap();
        let mounts = vec![mount_at(&card, "NIKON Z 8", 7, true), {
            let mut ssd = mount_at(&plain, "SSD", 8, false);
            ssd.uuid = None;
            ssd
        }];
        let mut table = MountSource::Fixed(Arc::new(Mutex::new(mounts.clone()))).read(9);
        assert!(
            table
                .volumes()
                .all(|mounted| mounted.card_folder().is_none()),
            "no card until one is looked for"
        );
        table.find_cards();
        let volume = table.volume_of(&photos).unwrap();
        assert_eq!(volume.label, "NIKON Z 8");
        assert_eq!(volume.mount_point, card);
        assert!(volume.removable);
        assert_eq!(volume.id.as_str(), format!("volume-{}", "07".repeat(16)));
        let cards: Vec<_> = table.volumes().filter_map(Mounted::card_folder).collect();
        assert_eq!(cards, [card.join("DCIM")]);
        let ssd = table.volume_of(&plain).unwrap();
        assert!(ssd.id.as_str().starts_with("volume-dev"), "no UUID");
        assert!(!table.offline(&photos, &volume.id));
        let gone = card.join("DCIM/101NZ8_1");
        assert!(
            !table.offline(&gone, &volume.id),
            "gone from a mounted card"
        );
        let unmounted = MountSource::Fixed(Arc::new(Mutex::new(Vec::new()))).read(10);
        assert!(unmounted.offline(&gone, &volume.id));
        assert!(!unmounted.offline(&photos, &volume.id), "still there");

        // The same answers from only the mounts that could hold the path or carry the identity.
        let within = volume_in(&mounts, &photos, 11).unwrap();
        assert_eq!((within.id, within.last_seen_ms), (volume.id.clone(), 11));
        assert_eq!(volume_in(&mounts, &plain, 11).unwrap().id, ssd.id);
        let (found, dcim) = card_of(&mounts, &volume.id, 12).expect("the card");
        assert_eq!(
            (found.label.as_str(), dcim),
            ("NIKON Z 8", card.join("DCIM"))
        );
        assert!(card_of(&mounts, &ssd.id, 12).is_none(), "no DCIM folder");
        let absent = VolumeId::parse(format!("volume-{}", "09".repeat(16))).unwrap();
        assert!(card_of(&mounts, &absent, 12).is_none(), "not mounted");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
