//! The platform calls a classification and a move make: the volume a path is on (`statfs`, and Disk
//! Arbitration for whether its device is a disk image), whether a folder is writable (`access`),
//! the quarantine attribute (`removexattr`, `getxattr`), Gatekeeper's translocation (Security's
//! `SecTranslocateIsTranslocatedURL` and `SecTranslocateCreateOriginalPathForURL`, which are not
//! public API, so they are looked up with `dlsym` as LetsMove does and missing ones answer "not
//! translocated") and the Trash (Foundation's file manager). This is the only module of the crate
//! that holds `unsafe`.
#![allow(unsafe_code)]

use objc2_core_foundation::CFString;
use objc2_foundation::{NSFileManager, NSString, NSURL};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// The attribute Gatekeeper marks a downloaded file with.
pub(super) const QUARANTINE: &CStr = c"com.apple.quarantine";

/// The volume a path is on, as `statfs` and Disk Arbitration report it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Mounted read-only (`MNT_RDONLY`).
    pub read_only: bool,
    /// The device the volume is mounted from (`f_mntfromname`, such as `/dev/disk4s1`).
    pub device: String,
    /// Where the volume is mounted (`f_mntonname`).
    pub mount_point: PathBuf,
    /// Disk Arbitration describes the device's model as a disk image.
    pub disk_image: bool,
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path holds a NUL byte"))
}

/// A fixed C string field up to its first NUL, or the whole field when it has none.
fn field(field: &[c_char]) -> &[u8] {
    // SAFETY: `c_char` and `u8` have the same size and alignment, and the slice covers exactly the
    // field.
    let raw = unsafe { std::slice::from_raw_parts(field.as_ptr().cast::<u8>(), field.len()) };
    CStr::from_bytes_until_nul(raw).map_or(raw, CStr::to_bytes)
}

/// The volume `path` is on.
pub(super) fn volume(path: &Path) -> io::Result<Volume> {
    let path = c_path(path)?;
    // SAFETY: `statfs` is plain data, and all zeros is a valid value of it.
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a NUL-terminated string for the call, and `stats` is a whole `statfs` to
    // fill.
    if unsafe { libc::statfs(path.as_ptr(), &raw mut stats) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let device = String::from_utf8_lossy(field(&stats.f_mntfromname)).into_owned();
    let mount_point = PathBuf::from(std::ffi::OsStr::from_bytes(field(&stats.f_mntonname)));
    let disk_image = device
        .strip_prefix("/dev/")
        .is_some_and(|name| device_model(name).as_deref() == Some(DISK_IMAGE_MODEL));
    Ok(Volume {
        read_only: stats.f_flags & libc::MNT_RDONLY as u32 != 0,
        device,
        mount_point,
        disk_image,
    })
}

/// The device model Disk Arbitration gives an attached disk image's media.
const DISK_IMAGE_MODEL: &str = "Disk Image";

#[link(name = "DiskArbitration", kind = "framework")]
unsafe extern "C" {
    fn DASessionCreate(allocator: *const c_void) -> *mut c_void;
    fn DADiskCreateFromBSDName(
        allocator: *const c_void,
        session: *mut c_void,
        name: *const c_char,
    ) -> *mut c_void;
    fn DADiskCopyDescription(disk: *mut c_void) -> *const c_void;
    /// The description key of the device's model, a `CFString` constant.
    static kDADiskDescriptionDeviceModelKey: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const c_void);
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFGetTypeID(object: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: *const c_void,
        buffer: *const u8,
        length: isize,
        is_directory: u8,
    ) -> *const c_void;
    fn CFURLGetFileSystemRepresentation(
        url: *const c_void,
        resolve_against_base: u8,
        buffer: *mut u8,
        length: isize,
    ) -> u8;
}

/// Releases a CoreFoundation object it holds when dropped; null holds nothing.
struct Owned(*const c_void);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer is an object this process created or copied (a Create or Copy
            // function's result), released once here.
            unsafe { CFRelease(self.0) };
        }
    }
}

/// The model Disk Arbitration describes the device `name` (`disk4s1`) as, if it answers.
fn device_model(name: &str) -> Option<String> {
    let name = CString::new(name).ok()?;
    // SAFETY: a null allocator is the default one; the call answers a new session or null.
    let session = Owned(unsafe { DASessionCreate(std::ptr::null()) });
    if session.0.is_null() {
        return None;
    }
    // SAFETY: the session is live for the call and `name` is a NUL-terminated BSD name; the call
    // answers a new disk object or null.
    let disk = Owned(unsafe {
        DADiskCreateFromBSDName(std::ptr::null(), session.0.cast_mut(), name.as_ptr())
    });
    if disk.0.is_null() {
        return None;
    }
    // SAFETY: the disk is live for the call, which answers a copied dictionary or null.
    let description = Owned(unsafe { DADiskCopyDescription(disk.0.cast_mut()) });
    if description.0.is_null() {
        return None;
    }
    // SAFETY: the dictionary is live, and the key is the framework's constant; the value is
    // borrowed from the dictionary, which outlives its use below.
    let value = unsafe { CFDictionaryGetValue(description.0, kDADiskDescriptionDeviceModelKey) };
    // SAFETY: a non-null value is a CoreFoundation object, whose type is asked before it is read
    // as a string.
    if value.is_null() || unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return None;
    }
    // SAFETY: `value` is a live `CFString` borrowed for this read.
    let model = unsafe { &*value.cast::<CFString>() };
    Some(model.to_string().trim().to_owned())
}

/// Whether this process may write into `folder`.
pub(super) fn writable(folder: &Path) -> bool {
    let Ok(path) = c_path(folder) else {
        return false;
    };
    // SAFETY: `path` is a NUL-terminated string for the call.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// Remove the quarantine attribute from `path` itself, not following a symbolic link. Whether it
/// had one: an absent attribute is not an error.
pub(super) fn clear_quarantine(path: &Path) -> io::Result<bool> {
    let path = c_path(path)?;
    // SAFETY: both strings are NUL-terminated for the call.
    let result =
        unsafe { libc::removexattr(path.as_ptr(), QUARANTINE.as_ptr(), libc::XATTR_NOFOLLOW) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOATTR) {
        Ok(false)
    } else {
        Err(error)
    }
}

/// Whether `path` itself carries the quarantine attribute.
pub fn quarantined(path: &Path) -> bool {
    let Ok(path) = c_path(path) else {
        return false;
    };
    // SAFETY: both strings are NUL-terminated; a null buffer of size 0 asks only for the length.
    let length = unsafe {
        libc::getxattr(
            path.as_ptr(),
            QUARANTINE.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    length >= 0
}

/// Mark `path` quarantined as a download is, for tests of the move.
#[cfg(test)]
pub(super) fn quarantine(path: &Path) -> io::Result<()> {
    const VALUE: &[u8] = b"0083;00000000;Luxforge;";
    let path = c_path(path)?;
    // SAFETY: both strings are NUL-terminated and `VALUE` is borrowed for its length.
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            QUARANTINE.as_ptr(),
            VALUE.as_ptr().cast(),
            VALUE.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `SecTranslocateIsTranslocatedURL`.
type IsTranslocated =
    unsafe extern "C" fn(url: *const c_void, translocated: *mut bool, error: *mut c_void) -> u8;
/// `SecTranslocateCreateOriginalPathForURL`.
type OriginalPath = unsafe extern "C" fn(url: *const c_void, error: *mut c_void) -> *const c_void;

struct Translocation {
    is_translocated: Option<IsTranslocated>,
    original_path: Option<OriginalPath>,
}

/// Security's translocation functions, looked up once.
fn translocation() -> &'static Translocation {
    static FUNCTIONS: OnceLock<Translocation> = OnceLock::new();
    FUNCTIONS.get_or_init(|| {
        const SECURITY: &CStr = c"/System/Library/Frameworks/Security.framework/Security";
        // SAFETY: the path is NUL-terminated; the framework is a system one, loaded or already
        // loaded, and never unloaded, so the symbols looked up stay valid for the process.
        let handle = unsafe { libc::dlopen(SECURITY.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return Translocation {
                is_translocated: None,
                original_path: None,
            };
        }
        let symbol = |name: &CStr| {
            // SAFETY: `handle` is a live library handle and `name` is NUL-terminated.
            let found = unsafe { libc::dlsym(handle, name.as_ptr()) };
            (!found.is_null()).then_some(found)
        };
        Translocation {
            // SAFETY: each symbol, when present, is the function of this exact signature
            // (`Security/SecTranslocate.h`).
            is_translocated: symbol(c"SecTranslocateIsTranslocatedURL")
                .map(|found| unsafe { std::mem::transmute::<*mut c_void, IsTranslocated>(found) }),
            // SAFETY: as above.
            original_path: symbol(c"SecTranslocateCreateOriginalPathForURL")
                .map(|found| unsafe { std::mem::transmute::<*mut c_void, OriginalPath>(found) }),
        }
    })
}

/// A file URL for `path`.
fn file_url(path: &Path) -> Owned {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: `bytes` is borrowed for its length; the call answers a new URL or null.
    Owned(unsafe {
        CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            u8::from(path.is_dir()),
        )
    })
}

/// Whether `bundle` runs translocated, and then where the person put it, when the system says.
/// `None` when it is not translocated or the functions are missing.
pub(super) fn translocated(bundle: &Path) -> Option<Option<PathBuf>> {
    let functions = translocation();
    let is_translocated = functions.is_translocated?;
    let url = file_url(bundle);
    if url.0.is_null() {
        return None;
    }
    let mut translocated = false;
    // SAFETY: the URL is live for the call, `translocated` is a valid out-parameter, and a null
    // error pointer asks for no error.
    let answered = unsafe { is_translocated(url.0, &raw mut translocated, std::ptr::null_mut()) };
    if answered == 0 || !translocated {
        return None;
    }
    let original = functions.original_path.and_then(|original_path| {
        // SAFETY: the URL is live for the call; the answer is a new URL or null, released by
        // `Owned`.
        let original = Owned(unsafe { original_path(url.0, std::ptr::null_mut()) });
        if original.0.is_null() {
            return None;
        }
        let mut buffer = vec![0u8; libc::PATH_MAX as usize + 1];
        // SAFETY: the URL is live and `buffer` has room for `PATH_MAX` bytes and the NUL.
        let filled = unsafe {
            CFURLGetFileSystemRepresentation(
                original.0,
                1,
                buffer.as_mut_ptr(),
                buffer.len() as isize,
            )
        };
        (filled != 0).then(|| {
            let end = buffer.iter().position(|&byte| byte == 0).unwrap_or(0);
            PathBuf::from(std::ffi::OsStr::from_bytes(&buffer[..end]))
        })
    });
    Some(original)
}

/// Move `path` to the person's Trash through Foundation's file manager.
pub(super) fn trash(path: &Path) -> io::Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a path is not UTF-8"))?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(text));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|error| io::Error::other(error.localizedDescription().to_string()))
}
