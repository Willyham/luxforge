//! The free space of the volume a path is on, for a job that preflights the temporary disk it
//! will write: `statvfs` on Unix (the blocks available to an unprivileged process times the
//! fragment size) and `GetDiskFreeSpaceExW` on Windows (the bytes available to the calling user).
//! A platform without either reports `Unsupported`, never a guess.
use std::{io, path::Path};

/// The bytes a process may still write on the volume that holds `path`, which must exist.
pub fn available_bytes(path: &Path) -> io::Result<u64> {
    platform(path)
}

#[cfg(unix)]
fn platform(path: &Path) -> io::Result<u64> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let name = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path holds a NUL byte"))?;
    // SAFETY: `statvfs` is plain old data, for which all zero bytes is a valid value.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is a NUL-terminated string that outlives the call, and `stats` is a writable
    // `statvfs` the call fills in.
    let status = unsafe { libc::statvfs(name.as_ptr(), &raw mut stats) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
    Ok(u64::from(stats.f_bavail as u64).saturating_mul(u64::from(stats.f_frsize as u64)))
}

#[cfg(windows)]
fn platform(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0_u64;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call; `available` is a
    // writable u64, and the two totals the call may also fill are passed as null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(available)
    }
}

#[cfg(not(any(unix, windows)))]
fn platform(_: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "free disk space is not reported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_temporary_directory_has_space_and_a_missing_path_is_an_error() {
        let available = available_bytes(&std::env::temp_dir()).expect("free space");
        assert!(available > 0);
        assert!(available_bytes(Path::new("/luxforge/no/such/path")).is_err());
    }
}
