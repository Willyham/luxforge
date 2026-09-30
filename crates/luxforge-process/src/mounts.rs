//! The mounted file systems: where each is mounted, what it is called, the platform's identifier of
//! its volume, and whether it is removable, local and meant for a person's files. The catalog's
//! index reads it to tell volumes apart, to find camera cards and to say which folders are offline
//! (`docs/design/catalog.md`, "Browsing the filesystem").
//!
//! - **macOS**: `getfsstat` without waiting on any file system (`MNT_NOWAIT`), so a hung network
//!   mount answers from the kernel's cache; each mount's flags say whether it is removable
//!   (`MNT_REMOVABLE`, which a card and a disk image carry), local, the root, or not meant for
//!   browsing (`MNT_DONTBROWSE`: the system's own volumes). `getattrlist` reads each local
//!   volume's name and UUID, which stays the same across mounts; a network volume's are not read,
//!   so nothing waits on its server.
//! - **Linux**: `/proc/self/mountinfo`; a mount of a block device or a network file system outside
//!   the system's own trees is browsable, and one under `/media` or `/run/media` counts as
//!   removable. No UUID is read yet.
//! - **Windows**: each drive letter whose root exists; no name, UUID or removability is read yet.
use std::path::PathBuf;

/// One mounted file system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    /// Where it is mounted.
    pub mount_point: PathBuf,
    /// The volume's own name, when the platform reports one.
    pub name: Option<String>,
    /// The platform's identifier of the volume, which stays the same when it is mounted again (the
    /// volume UUID on macOS), when it has one.
    pub uuid: Option<[u8; 16]>,
    /// The file system's type as the platform names it (`apfs`, `msdos`, `exfat`, `ext4`).
    pub file_system: String,
    /// On removable media, as the platform reports it: a card, a disk image.
    pub removable: bool,
    /// Stored locally, not on a network server.
    pub local: bool,
    /// Meant to hold a person's files: not one of the system's own, pseudo or hidden mounts.
    pub browsable: bool,
    /// The root file system.
    pub root: bool,
}

/// Every file system mounted now, in the platform's order.
pub fn mounts() -> Result<Vec<Mount>, crate::Unavailable> {
    platform::mounts()
}

#[cfg(target_os = "macos")]
mod platform {
    use super::Mount;
    use crate::Unavailable;
    use std::{
        ffi::{CStr, CString, OsStr, c_char, c_int},
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
    };

    /// Removable media (`sys/mount.h`); `libc` names the other flags but not this one.
    const MNT_REMOVABLE: u32 = 0x0000_0200;
    /// Room for mounts that appear between counting them and reading them.
    const SPARE_ENTRIES: usize = 8;

    pub(super) fn mounts() -> Result<Vec<Mount>, Unavailable> {
        // SAFETY: a null buffer of size 0 asks only for the number of mounted file systems.
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        let count = usize::try_from(count).map_err(|_| Unavailable("getfsstat failed"))?;
        let capacity = count + SPARE_ENTRIES;
        let mut entries: Vec<libc::statfs> = Vec::with_capacity(capacity);
        let bytes = c_int::try_from(capacity * size_of::<libc::statfs>())
            .map_err(|_| Unavailable("the mount table is too large"))?;
        // SAFETY: `entries` has room for `capacity` entries and its size in bytes is passed, so
        // getfsstat writes at most that many.
        let filled = unsafe { libc::getfsstat(entries.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
        let filled = usize::try_from(filled).map_err(|_| Unavailable("getfsstat failed"))?;
        // SAFETY: getfsstat initialized the first `filled` entries, never more than `capacity`.
        unsafe { entries.set_len(filled.min(capacity)) };
        Ok(entries.iter().map(mount).collect())
    }

    fn mount(entry: &libc::statfs) -> Mount {
        let flags = entry.f_flags;
        let flag = |mask: c_int| flags & (mask as u32) != 0;
        let mount_point = PathBuf::from(OsStr::from_bytes(bytes(&entry.f_mntonname)));
        let local = flag(libc::MNT_LOCAL);
        // A network volume's attributes are not read: its server may not answer, and nothing
        // here may wait on it.
        let (name, uuid) = if local {
            volume_attributes(&mount_point).unwrap_or_default()
        } else {
            (None, None)
        };
        Mount {
            mount_point,
            name,
            uuid,
            file_system: String::from_utf8_lossy(bytes(&entry.f_fstypename)).into_owned(),
            removable: flags & MNT_REMOVABLE != 0,
            local,
            browsable: !flag(libc::MNT_DONTBROWSE),
            root: flag(libc::MNT_ROOTFS),
        }
    }

    /// A fixed C string field up to its first NUL, or the whole field when it has none.
    fn bytes(field: &[c_char]) -> &[u8] {
        // SAFETY: `c_char` and `u8` have the same size and alignment, and the slice covers
        // exactly the field.
        let raw = unsafe { std::slice::from_raw_parts(field.as_ptr().cast::<u8>(), field.len()) };
        CStr::from_bytes_until_nul(raw).map_or(raw, CStr::to_bytes)
    }

    /// The volume's name and UUID, read with one `getattrlist` on its mount point. A UUID of all
    /// zeros, which a file system without one reports, is none.
    fn volume_attributes(mount_point: &Path) -> Option<(Option<String>, Option<[u8; 16]>)> {
        /// The length word, the name's reference (offset and length) and the UUID, then the name.
        const BUFFER: usize = 4 + 8 + 16 + 1024;
        let path = CString::new(mount_point.as_os_str().as_bytes()).ok()?;
        let mut request = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: 0,
            volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_NAME | libc::ATTR_VOL_UUID,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        };
        let mut buffer = [0_u8; BUFFER];
        // SAFETY: `path` is NUL-terminated, `request` is a valid attribute list, and the buffer's
        // exact size is passed, so the call writes nothing past it.
        let status = unsafe {
            libc::getattrlist(
                path.as_ptr(),
                (&raw mut request).cast(),
                buffer.as_mut_ptr().cast(),
                BUFFER,
                libc::FSOPT_NOFOLLOW,
            )
        };
        if status != 0 {
            return None;
        }
        let word = |at: usize| buffer.get(at..at + 4).map(|b| [b[0], b[1], b[2], b[3]]);
        let written = (u32::from_ne_bytes(word(0)?) as usize).min(BUFFER);
        let offset = i32::from_ne_bytes(word(4)?);
        let length = u32::from_ne_bytes(word(8)?) as usize;
        // The name's offset counts from its reference, at byte 4, and its length includes the NUL.
        let name = usize::try_from(offset)
            .ok()
            .and_then(|offset| buffer.get(4 + offset..(4 + offset + length).min(written)))
            .map(|name| CStr::from_bytes_until_nul(name).map_or(name, CStr::to_bytes))
            .filter(|name| !name.is_empty())
            .map(|name| String::from_utf8_lossy(name).into_owned());
        let uuid: [u8; 16] = buffer.get(12..28)?.try_into().ok()?;
        Some((name, (uuid != [0; 16]).then_some(uuid)))
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::Mount;
    use crate::Unavailable;
    use std::path::{Path, PathBuf};

    /// Network file systems: browsable, never local.
    const NETWORK: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "fuse.sshfs", "9p"];
    /// The system's own trees, never a person's files, except the removable media under
    /// `/run/media`.
    const SYSTEM_TREES: &[&str] = &[
        "/boot", "/dev", "/proc", "/run", "/snap", "/sys", "/var/lib",
    ];

    pub(super) fn mounts() -> Result<Vec<Mount>, Unavailable> {
        let table = std::fs::read_to_string("/proc/self/mountinfo")
            .map_err(|_| Unavailable("/proc/self/mountinfo cannot be read"))?;
        Ok(table.lines().filter_map(mount).collect())
    }

    /// One `mountinfo` line: `id parent major:minor root mount-point options [optional...] -
    /// type source super-options`.
    fn mount(line: &str) -> Option<Mount> {
        let (before, after) = line.split_once(" - ")?;
        let mount_point = PathBuf::from(unescape(before.split(' ').nth(4)?));
        let mut after = after.split(' ');
        let file_system = after.next()?.to_owned();
        let source = after.next().unwrap_or_default();
        let network = NETWORK.contains(&file_system.as_str());
        let removable = ["/media", "/run/media"]
            .iter()
            .any(|tree| mount_point.starts_with(tree));
        let system = SYSTEM_TREES
            .iter()
            .any(|tree| mount_point.starts_with(tree))
            && !removable;
        let root = mount_point == Path::new("/");
        Some(Mount {
            name: (!root)
                .then(|| mount_point.file_name())
                .flatten()
                .map(|name| name.to_string_lossy().into_owned()),
            mount_point,
            uuid: None,
            browsable: (source.starts_with("/dev/") || network) && !system,
            local: !network,
            removable,
            root,
            file_system,
        })
    }

    /// `mountinfo` writes a space, tab, newline and backslash in a path as octal escapes.
    fn unescape(field: &str) -> String {
        let mut out = String::with_capacity(field.len());
        let mut rest = field;
        while let Some(at) = rest.find('\\') {
            out.push_str(&rest[..at]);
            let code = rest.get(at + 1..at + 4);
            match code.and_then(|code| u8::from_str_radix(code, 8).ok()) {
                Some(byte) => {
                    out.push(char::from(byte));
                    rest = &rest[at + 4..];
                }
                None => {
                    out.push('\\');
                    rest = &rest[at + 1..];
                }
            }
        }
        out.push_str(rest);
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_mountinfo_line_reads_its_mount_point_type_and_kind() {
            let card = mount(
                "36 25 179:1 / /run/media/me/NIKON\\040Z\\0408 rw,nosuid - vfat /dev/mmcblk0p1 rw",
            )
            .unwrap();
            assert_eq!(card.mount_point, Path::new("/run/media/me/NIKON Z 8"));
            assert_eq!(card.name.as_deref(), Some("NIKON Z 8"));
            assert!(card.removable && card.browsable && card.local && !card.root);
            let proc = mount("22 1 0:5 / /proc rw - proc proc rw").unwrap();
            assert!(!proc.browsable);
            let root = mount("1 0 259:2 / / rw - ext4 /dev/nvme0n1p2 rw").unwrap();
            assert!(root.root && root.browsable && root.name.is_none());
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::Mount;
    use crate::Unavailable;
    use std::path::PathBuf;

    pub(super) fn mounts() -> Result<Vec<Mount>, Unavailable> {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        Ok((b'A'..=b'Z')
            .map(|letter| format!("{}:\\", char::from(letter)))
            .filter(|root| std::path::Path::new(root).exists())
            .map(|root| Mount {
                root: root.starts_with(&system),
                mount_point: PathBuf::from(root),
                name: None,
                uuid: None,
                file_system: String::new(),
                removable: false,
                local: true,
                browsable: true,
            })
            .collect())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod platform {
    use super::Mount;
    use crate::Unavailable;

    pub(super) fn mounts() -> Result<Vec<Mount>, Unavailable> {
        Err(Unavailable("the mount table is not read on this platform"))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::path::Path;

    /// The root file system is mounted, read-only-sealed or not, and every mount point is
    /// absolute; the startup disk's data volume carries its UUID.
    #[test]
    fn the_mount_table_lists_the_root_and_the_data_volume() {
        let mounts = mounts().unwrap();
        let root = mounts.iter().find(|mount| mount.root).expect("the root");
        assert_eq!(root.mount_point, Path::new("/"));
        assert!(root.browsable && root.local && !root.removable);
        assert!(mounts.iter().all(|mount| mount.mount_point.is_absolute()));
        if let Some(data) = mounts
            .iter()
            .find(|mount| mount.mount_point == Path::new("/System/Volumes/Data"))
        {
            assert!(!data.browsable);
            assert!(data.uuid.is_some(), "the data volume has a UUID");
        }
    }
}
