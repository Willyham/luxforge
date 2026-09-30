//! Which volume a path is on.
//!
//! **Placeholder** (catalog contracts): lane A replaces the body of [`volume_of`] with the
//! platform's volume identity — the volume UUID and label on macOS, removability, and mount
//! notifications for cards — keeping its signature. Until then a volume is its device number, its
//! mount point the highest ancestor on the same device, its label that directory's name, and no
//! volume is removable. That is enough for the single-file import to record a volume for every
//! photograph; a device number is not stable across remounts of a card, which is exactly what the
//! real identity fixes.
use crate::{
    Error,
    atomic_file::file_error,
    catalog_types::{Volume, VolumeId},
};
use std::path::Path;

/// The volume the existing file or directory at `path` is on, seen now (`last_seen_ms`).
pub(crate) fn volume_of(path: &Path, now_ms: i64) -> Result<Volume, Error> {
    let canonical = path
        .canonicalize()
        .map_err(|error| file_error("cannot resolve the volume", error.kind()))?;
    let device = device(&canonical)?;
    let mut mount_point = canonical.as_path();
    while let Some(parent) = mount_point.parent() {
        if device_of(parent) != Some(device) {
            break;
        }
        mount_point = parent;
    }
    let label = mount_point.file_name().map_or_else(
        || "Startup disk".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    Ok(Volume {
        id: VolumeId::parse(format!("{}dev{device:016x}", VolumeId::PREFIX))?,
        mount_point: mount_point.to_path_buf(),
        label,
        removable: false,
        platform_id: None,
        last_seen_ms: now_ms,
    })
}

fn device(path: &Path) -> Result<u64, Error> {
    device_of(path).ok_or_else(|| Error::file_access("cannot read the volume of the file"))
}

#[cfg(unix)]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    path.metadata().ok().map(|metadata| metadata.dev())
}

#[cfg(not(unix))]
fn device_of(path: &Path) -> Option<u64> {
    // One volume per path prefix until lane A reads the platform's volume serial number.
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
mod tests {
    use super::*;

    #[test]
    fn a_file_and_its_folder_are_on_one_volume() {
        let dir = luxforge_testbase::paths::temp_dir("volume-of");
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"x").unwrap();
        let of_file = volume_of(&file, 1).unwrap();
        let of_dir = volume_of(&dir, 2).unwrap();
        assert_eq!(of_file.id, of_dir.id);
        assert_eq!(of_file.mount_point, of_dir.mount_point);
        assert!(
            file.canonicalize()
                .unwrap()
                .starts_with(&of_file.mount_point)
        );
        assert!(volume_of(&dir.join("missing.jpg"), 3).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
