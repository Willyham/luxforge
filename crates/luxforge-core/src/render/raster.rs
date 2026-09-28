//! The rendered byte frame and the helpers every pass allocates and writes one through.

use crate::{Error, SnapshotId};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
    pub source_fingerprint: String,
    pub snapshot_id: SnapshotId,
}

impl Raster {
    pub(crate) fn expected_len(width: u32, height: u32) -> Result<usize, Error> {
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| Error::resource_limit("image dimensions overflow"))?;
        if pixels > luxforge_raw::MAX_FRAME_BYTES {
            return Err(Error::resource_limit("evaluated image exceeds 512 MiB"));
        }
        usize::try_from(pixels)
            .map_err(|_| Error::resource_limit("image allocation is not addressable"))
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = ((u64::from(y) * u64::from(self.width) + u64::from(x)) * 4) as usize;
        self.rgba
            .get(offset..offset + 4)
            .map(|p| [p[0], p[1], p[2], p[3]])
    }
}

/// A zeroed byte frame of `len` bytes, allocated as the `Arc<Vec<u8>>` a [`Raster`] holds so that
/// the frame a pass writes is the frame the render returns, and the pass writes it in place through
/// [`frame_mut`]. `vec![0; len]` asks the allocator for zeroed memory (`calloc`), whose fresh pages
/// are already zero, so no thread fills the frame before the pass: each page is faulted in by
/// whichever worker first writes it.
pub(crate) fn zeroed_frame(len: usize) -> Arc<Vec<u8>> {
    Arc::new(vec![0; len])
}

/// The bytes of a frame a pass is still writing. A frame is not shared until the render returns
/// it, so this never fails, and it never clones one.
pub(crate) fn frame_mut(frame: &mut Arc<Vec<u8>>) -> &mut [u8] {
    #[cfg(test)]
    frame_writes::note(frame);
    Arc::get_mut(frame).expect("a frame is not shared until its render returns it")
}

/// Which frames the passes on this thread were handed to write, for the tests that prove a render
/// returns the frame its last pass wrote rather than a copy of it.
#[cfg(test)]
pub(crate) mod frame_writes {
    use std::cell::RefCell;

    thread_local! {
        /// The data address of every frame handed out while [`record`] runs, oldest first.
        static WRITTEN: RefCell<Option<Vec<usize>>> = const { RefCell::new(None) };
    }

    pub(super) fn note(frame: &[u8]) {
        WRITTEN.with_borrow_mut(|written| {
            if let Some(written) = written {
                written.push(frame.as_ptr() as usize);
            }
        });
    }

    /// `work`'s result, with the address of every frame a pass on this thread was handed to write
    /// while it ran, oldest first.
    pub(crate) fn record<T>(work: impl FnOnce() -> T) -> (T, Vec<usize>) {
        WRITTEN.with_borrow_mut(|written| *written = Some(Vec::new()));
        let result = work();
        let written = WRITTEN.with_borrow_mut(Option::take).unwrap_or_default();
        (result, written)
    }
}
