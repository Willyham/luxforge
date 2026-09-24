//! Real GPU work for the counter tests, through the system default Metal device: the one object
//! the sampler reads allocations from and, in the editor, the one wgpu presents through.
#![cfg(target_os = "macos")]
#![allow(dead_code)]

use objc2::{
    Encode, Encoding, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::AnyObject,
};

/// `NSRange`, for `fillBuffer:range:value:`.
#[repr(C)]
struct NSRange {
    location: usize,
    length: usize,
}

// SAFETY: the layout is NSRange's, two NSUIntegers, and the name is the one the Objective-C runtime
// records for it, so objc2's encoding check compares like with like.
unsafe impl Encode for NSRange {
    const ENCODING: Encoding = Encoding::Struct("_NSRange", &[usize::ENCODING, usize::ENCODING]);
}

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    fn MTLCreateSystemDefaultDevice() -> *mut AnyObject;
}

/// Whether this Mac is a virtual machine, such as a hosted CI runner. Its paravirtual GPU is a
/// Metal device, but it need not publish the Apple GPU driver's `AppUsage`, so a GPU time check
/// there is a functional check at most, never native GPU evidence.
pub fn virtual_machine() -> bool {
    let mut present: libc::c_int = 0;
    let mut size = size_of::<libc::c_int>();
    // SAFETY: the name is a NUL-terminated C string literal, the old value is a live, writable int
    // whose size is passed in `size`, and nothing is written to the kernel.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.hv_vmm_present".as_ptr(),
            (&raw mut present).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    status == 0 && present != 0
}

/// `MTLResourceStorageModePrivate`: GPU-only memory, which the device counts and the CPU never
/// maps.
const STORAGE_MODE_PRIVATE: usize = 2 << 4;

pub struct Metal {
    queue: Retained<AnyObject>,
    buffer: Retained<AnyObject>,
    bytes: usize,
    _device: Retained<AnyObject>,
}

impl Metal {
    /// Open the default device, a command queue and a private buffer of `bytes`, or `None` when
    /// this machine has no Metal device.
    pub fn open(bytes: usize) -> Option<Self> {
        // SAFETY: no arguments; the result is an owned reference or nil, which Retained takes over.
        let device = unsafe { Retained::from_raw(MTLCreateSystemDefaultDevice()) }?;
        // SAFETY: newCommandQueue takes no arguments and returns an owned queue or nil.
        let queue: Option<Retained<AnyObject>> = unsafe { msg_send![&*device, newCommandQueue] };
        // SAFETY: newBufferWithLength:options: takes two NSUIntegers and returns an owned buffer
        // or nil.
        let buffer: Option<Retained<AnyObject>> = unsafe {
            msg_send![&*device, newBufferWithLength: bytes, options: STORAGE_MODE_PRIVATE]
        };
        Some(Self {
            queue: queue?,
            buffer: buffer?,
            bytes,
            _device: device,
        })
    }

    /// Fill the whole buffer `fills` times with the blit engine in one command buffer, wait for it
    /// and return its GPU time in nanoseconds as Metal measured it.
    pub fn dispatch(&self, fills: usize) -> u64 {
        autoreleasepool(|_| {
            // SAFETY: every message below is an MTLCommandQueue, MTLCommandBuffer or
            // MTLBlitCommandEncoder method sent to a live object of that kind with arguments of
            // the declared types; the autoreleased command buffer and encoder are retained by
            // objc2 and outlive their use inside this pool.
            unsafe {
                let command: Retained<AnyObject> = msg_send![&*self.queue, commandBuffer];
                let blit: Retained<AnyObject> = msg_send![&*command, blitCommandEncoder];
                for fill in 0..fills {
                    let range = NSRange {
                        location: 0,
                        length: self.bytes,
                    };
                    let value = fill as u8;
                    let _: () =
                        msg_send![&*blit, fillBuffer: &*self.buffer, range: range, value: value];
                }
                let _: () = msg_send![&*blit, endEncoding];
                let _: () = msg_send![&*command, commit];
                let _: () = msg_send![&*command, waitUntilCompleted];
                let start: f64 = msg_send![&*command, GPUStartTime];
                let end: f64 = msg_send![&*command, GPUEndTime];
                ((end - start).max(0.0) * 1e9) as u64
            }
        })
    }
}
