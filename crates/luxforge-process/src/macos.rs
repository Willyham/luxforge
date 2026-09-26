//! macOS: `proc_pid_rusage` for CPU time and memory, the IORegistry for GPU time and the system
//! default Metal device for GPU allocations.
//!
//! Two facts decide the sources, both measured on the owner's M4 on 2026-09-23 with a Metal compute
//! probe. `task_power_info_v2`'s GPU utilisation stays 0 on Apple silicon, so it cannot be used.
//! The GPU driver instead publishes, on each user client under an `IOAccelerator`, the process
//! that created it (`IOUserClientCreator`, such as `pid 88026, luxforge`) and an `AppUsage` array
//! with one dictionary per command queue whose `accumulatedGPUTime` is in nanoseconds: 0.80 s
//! against 0.82 s of command-buffer time. `AppUsage` is an undocumented key, so its absence is
//! reported as unavailable with that reason, never as zero.
use crate::{Gpu, Memory, MemoryKind, Unavailable};
use core_foundation::{
    array::CFArray,
    base::{CFAllocatorRef, CFType, CFTypeRef, TCFType, kCFAllocatorDefault},
    dictionary::{CFDictionary, CFDictionaryRef, CFMutableDictionaryRef},
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use objc2::{msg_send, rc::Retained, runtime::AnyObject};
use std::{
    ffi::{c_char, c_int},
    mem::MaybeUninit,
    time::{Duration, Instant},
};

pub(crate) struct State {
    timebase: Result<Timebase, Unavailable>,
    gpu_time: GpuTime,
    allocations: Allocations,
}

impl State {
    pub(crate) fn new() -> Self {
        Self {
            timebase: Timebase::read(),
            gpu_time: GpuTime::new(),
            allocations: Allocations {
                enabled: false,
                device: None,
            },
        }
    }

    pub(crate) fn enable_gpu_allocations(&mut self) {
        self.allocations.enabled = true;
    }

    pub(crate) fn cpu_and_memory(&mut self) -> (Result<u64, Unavailable>, Memory) {
        match rusage() {
            Ok(info) => (
                self.timebase.map(|timebase| {
                    timebase.ns(info.ri_user_time.saturating_add(info.ri_system_time))
                }),
                Memory {
                    kind: MemoryKind::Footprint,
                    bytes: Ok(info.ri_phys_footprint),
                    peak_bytes: Ok(info.ri_lifetime_max_phys_footprint),
                    resident_bytes: Ok(info.ri_resident_size),
                },
            ),
            Err(reason) => (
                Err(reason),
                Memory {
                    kind: MemoryKind::Footprint,
                    bytes: Err(reason),
                    peak_bytes: Err(reason),
                    resident_bytes: Err(reason),
                },
            ),
        }
    }

    pub(crate) fn gpu(&mut self) -> Gpu {
        // Allocations first: a device the sampler opens is a GPU client of this process, which the
        // same read's walk then finds.
        let (allocated_bytes, unified_memory) = self.allocations.read();
        Gpu {
            time_ns: self.gpu_time.read(),
            allocated_bytes,
            unified_memory,
        }
    }
}

// CPU time and memory.

/// `mach_timebase_info_data_t`.
#[repr(C)]
#[derive(Default)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    /// libSystem's ratio between mach absolute-time ticks and nanoseconds. libc declares it too,
    /// deprecated in favour of a crate this workspace does not have, so it is declared here.
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> c_int;
}

/// The ratio that turns mach absolute-time ticks into nanoseconds. `proc_pid_rusage` reports CPU
/// time in ticks, which are nanoseconds on Intel and 125/3 of one on the M4.
#[derive(Clone, Copy, Debug)]
struct Timebase {
    numer: u32,
    denom: u32,
}

impl Timebase {
    fn read() -> Result<Self, Unavailable> {
        let mut info = MachTimebaseInfo::default();
        // SAFETY: the pointer is to a live, writable structure with the layout of
        // mach_timebase_info_data_t, which is all the call writes.
        let status = unsafe { mach_timebase_info(&mut info) };
        if status != 0 || info.numer == 0 || info.denom == 0 {
            return Err(Unavailable("mach_timebase_info failed"));
        }
        Ok(Self {
            numer: info.numer,
            denom: info.denom,
        })
    }

    fn ns(self, ticks: u64) -> u64 {
        let ns = u128::from(ticks) * u128::from(self.numer) / u128::from(self.denom);
        u64::try_from(ns).unwrap_or(u64::MAX)
    }
}

/// This process's `rusage_info_v4`: the oldest flavour with every field read here, the lifetime
/// maximum footprint being the newest of them.
fn rusage() -> Result<libc::rusage_info_v4, Unavailable> {
    let pid = c_int::try_from(std::process::id())
        .map_err(|_| Unavailable("the process id is out of range"))?;
    let mut info = MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: for the V4 flavour the call writes one rusage_info_v4 through the pointer, which is
    // to a live, writable structure of exactly that layout (libc transcribes it from the SDK
    // header). The parameter's `*mut rusage_info_t` type is the C API's `rusage_info_t *`, which
    // means "the buffer", hence the cast.
    let status =
        unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, info.as_mut_ptr().cast()) };
    if status != 0 {
        return Err(Unavailable("proc_pid_rusage failed"));
    }
    // SAFETY: every field is an integer or a byte array, so the zeroed structure was already a
    // valid value, and the call succeeded in filling it.
    Ok(unsafe { info.assume_init() })
}

// GPU time from the IORegistry.

type KernReturn = c_int;
/// `io_object_t`: a mach port name for a kernel IOKit object.
type IoObjectRaw = u32;
const KERN_SUCCESS: KernReturn = 0;
/// `kIOMainPortDefault`, which is `MACH_PORT_NULL`: IOKit uses its default main port.
const MAIN_PORT_DEFAULT: u32 = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    /// Consumes one reference to `matching`, whether or not it succeeds.
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: CFDictionaryRef,
        existing: *mut IoObjectRaw,
    ) -> KernReturn;
    fn IORegistryEntryGetChildIterator(
        entry: IoObjectRaw,
        plane: *const c_char,
        iterator: *mut IoObjectRaw,
    ) -> KernReturn;
    fn IOIteratorNext(iterator: IoObjectRaw) -> IoObjectRaw;
    fn IOIteratorIsValid(iterator: IoObjectRaw) -> c_int;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObjectRaw,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
    fn IORegistryEntryGetRegistryEntryID(entry: IoObjectRaw, id: *mut u64) -> KernReturn;
    fn IOObjectRelease(object: IoObjectRaw) -> KernReturn;
}

/// One reference this process holds to an IOKit object, a registry entry or an iterator,
/// released on drop. A call made with the wrong kind of object is refused by the kernel, not
/// undefined behaviour, so the methods do not distinguish them.
struct IoObject(IoObjectRaw);

impl IoObject {
    /// Own a reference IOKit handed back, or `None` for the null object.
    fn owned(raw: IoObjectRaw) -> Option<Self> {
        (raw != 0).then_some(Self(raw))
    }

    /// The iterator's next object, owned, or `None` at its end.
    fn next(&self) -> Option<Self> {
        // SAFETY: the call takes any port name; for an iterator it returns the next object with a
        // reference the caller owns, and 0 at the end or for anything else.
        Self::owned(unsafe { IOIteratorNext(self.0) })
    }

    /// Whether the registry changed under this iterator, so what it listed may be incomplete.
    fn invalidated(&self) -> bool {
        // SAFETY: the call takes any port name and only reads the iterator's state.
        unsafe { IOIteratorIsValid(self.0) == 0 }
    }

    fn property(&self, key: &CFString) -> Option<CFType> {
        // SAFETY: the key is a live CFString for the whole call, the default allocator and no
        // options are always valid, and the call only reads the entry. A property is returned under
        // the Create rule.
        let value = unsafe {
            IORegistryEntryCreateCFProperty(
                self.0,
                key.as_concrete_TypeRef(),
                kCFAllocatorDefault,
                0,
            )
        };
        // SAFETY: a non-null value is a reference we own, which CFType releases on drop.
        (!value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) })
    }

    /// The entry's registry ID, unique for the boot and never reused, unlike a port name.
    fn registry_id(&self) -> Option<u64> {
        let mut id = 0;
        // SAFETY: the pointer is to a live, writable u64, which is all the call writes.
        let status = unsafe { IORegistryEntryGetRegistryEntryID(self.0, &mut id) };
        (status == KERN_SUCCESS).then_some(id)
    }
}

impl Drop for IoObject {
    fn drop(&mut self) {
        // SAFETY: this value owns exactly one reference to the object and gives it up once, here.
        unsafe { IOObjectRelease(self.0) };
    }
}

/// How long a warm cache is trusted before the accelerators' children are walked again, so a GPU
/// client this process opens after the last walk is found within this time even while every
/// cached client still answers.
const REWALK: Duration = Duration::from_secs(10);

/// What one client has contributed to this process's GPU time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tally {
    /// The client's last `AppUsage` sum.
    last_ns: u64,
    /// Whether the client has ever published `AppUsage`. One that never did is expected to go on
    /// without it; one that stops has closed.
    published: bool,
}

impl Tally {
    /// Take the client's latest sum and return the time that has left it. A command queue that is
    /// released takes its entry out of `AppUsage`, so a sum can fall; that time was spent, so the
    /// caller keeps it in a retired total and the counter never goes back.
    fn observe(&mut self, sum_ns: u64) -> u64 {
        let fell = self.last_ns.saturating_sub(sum_ns);
        self.last_ns = sum_ns;
        self.published = true;
        fell
    }
}

struct GpuClient {
    entry: IoObject,
    id: u64,
    tally: Tally,
}

/// GPU time: the sum of `accumulatedGPUTime` over this process's user clients, kept monotonic.
///
/// A full walk reads the creator of every client of every accelerator: 0.35 ms at the median and
/// 1.1 ms at p95 over 84 clients on the owner's machine. So the entries that belong to this
/// process are kept, with a reference each, and a warm read asks only them for `AppUsage`. The
/// walk is repeated every [`REWALK`], whether or not anything is cached, and sooner when a cached
/// client that published stops doing so or a walk found the registry changing under it.
struct GpuTime {
    clients: Vec<GpuClient>,
    /// Time spent by clients that have closed and queues that have gone.
    retired_ns: u64,
    walked: Option<Instant>,
    /// Whether any client of this process has ever published `AppUsage`, after which the counter
    /// is a number even if every client closes.
    published: bool,
    /// Why the last walk found no client.
    empty: Unavailable,
}

const NO_CLIENT: Unavailable = Unavailable("no GPU client in this process");

/// What a walk of the accelerators' children found.
enum Walk {
    /// This process's clients with their registry IDs, each holding a reference.
    Found(Vec<(IoObject, u64)>),
    /// The registry changed during the walk, so a client may have been missed.
    Incomplete,
    Failed(Unavailable),
}

impl GpuTime {
    fn new() -> Self {
        Self {
            clients: Vec::new(),
            retired_ns: 0,
            walked: None,
            published: false,
            empty: NO_CLIENT,
        }
    }

    fn read(&mut self) -> Result<u64, Unavailable> {
        let keys = UsageKeys::new();
        // A process with no client yet walks at most once per interval too: a headless owner has
        // none at all, and walking every read would cost it a third of a millisecond each time.
        let due = self.walked.is_none_or(|at| at.elapsed() >= REWALK);
        let mut walk = due;
        if !due {
            for client in &mut self.clients {
                match keys.sum(&client.entry) {
                    Some(sum) => {
                        self.retired_ns = self.retired_ns.saturating_add(client.tally.observe(sum));
                    }
                    None if client.tally.published => {
                        walk = true;
                        break;
                    }
                    None => {}
                }
            }
        }
        if walk {
            self.walk(&keys);
        }
        if self.clients.iter().any(|client| client.tally.published) {
            self.published = true;
        }
        if self.published {
            Ok(self.clients.iter().fold(self.retired_ns, |total, client| {
                total.saturating_add(client.tally.last_ns)
            }))
        } else if self.clients.is_empty() {
            Err(self.empty)
        } else {
            Err(Unavailable(
                "the GPU driver publishes no AppUsage for this process",
            ))
        }
    }

    fn walk(&mut self, keys: &UsageKeys) {
        self.walked = Some(Instant::now());
        let found = match own_clients() {
            Walk::Found(found) => {
                self.empty = NO_CLIENT;
                found
            }
            // The registry changed during the walk. Keep the cache as it is rather than retire a
            // client the walk merely missed, which would count its time twice when a later walk
            // found it again, and walk again at the next read.
            Walk::Incomplete => {
                self.walked = None;
                return;
            }
            // The same for a walk that failed outright while clients are cached: they go on
            // answering warm reads until the next walk.
            Walk::Failed(_) if !self.clients.is_empty() => return,
            Walk::Failed(reason) => {
                self.empty = reason;
                return;
            }
        };
        let mut previous = std::mem::take(&mut self.clients);
        for (entry, id) in found {
            let tally = previous
                .iter()
                .position(|client| client.id == id)
                .map_or_else(Tally::default, |index| previous.swap_remove(index).tally);
            let mut client = GpuClient { entry, id, tally };
            if let Some(sum) = keys.sum(&client.entry) {
                self.retired_ns = self.retired_ns.saturating_add(client.tally.observe(sum));
            }
            self.clients.push(client);
        }
        // A client the walk no longer finds has closed; what it spent stays counted.
        for gone in previous {
            self.retired_ns = self.retired_ns.saturating_add(gone.tally.last_ns);
        }
    }
}

/// The two property keys a warm read uses, made once per read.
struct UsageKeys {
    usage: CFString,
    accumulated: CFString,
}

impl UsageKeys {
    fn new() -> Self {
        Self {
            usage: CFString::from_static_string("AppUsage"),
            accumulated: CFString::from_static_string("accumulatedGPUTime"),
        }
    }

    /// The sum of `accumulatedGPUTime` over a client's `AppUsage` entries, or `None` when it
    /// publishes no `AppUsage` array. An empty array is a client with no work yet: zero.
    fn sum(&self, entry: &IoObject) -> Option<u64> {
        let usage = entry.property(&self.usage)?.downcast::<CFArray>()?;
        let mut total: u64 = 0;
        for item in usage.iter() {
            let item = *item;
            if item.is_null() {
                continue;
            }
            // SAFETY: a CFArray's elements are CF objects, alive while the array is; wrapping under
            // the Get rule retains this one for as long as `item` lives.
            let item = unsafe { CFType::wrap_under_get_rule(item) };
            let Some(queue) = item.downcast::<CFDictionary>() else {
                continue;
            };
            let Some(value) = queue.find(self.accumulated.as_CFTypeRef()) else {
                continue;
            };
            let value = *value;
            if value.is_null() {
                continue;
            }
            // SAFETY: a CFDictionary's values are CF objects, alive while the dictionary is;
            // wrapping under the Get rule retains this one for as long as `value` lives.
            let value = unsafe { CFType::wrap_under_get_rule(value) };
            let Some(ns) = value
                .downcast::<CFNumber>()
                .and_then(|number| number.to_i64())
                .and_then(|ns| u64::try_from(ns).ok())
            else {
                continue;
            };
            total = total.saturating_add(ns);
        }
        Some(total)
    }
}

/// Walk every `IOAccelerator`'s children in the service plane and keep those whose
/// `IOUserClientCreator` names this process. Matching the user clients directly as services does
/// not work: they are neither registered nor matched.
fn own_clients() -> Walk {
    // SAFETY: the name is a NUL-terminated C string literal. The dictionary is returned under the
    // Create rule, and ownership passes to IOServiceGetMatchingServices below.
    let matching = unsafe { IOServiceMatching(c"IOAccelerator".as_ptr()) };
    if matching.is_null() {
        return Walk::Failed(Unavailable("IOKit could not match GPU accelerators"));
    }
    let mut raw = 0;
    // SAFETY: the dictionary is the one reference IOServiceMatching returned, which this call
    // consumes whatever it returns, and the pointer is to a live, writable io_iterator_t.
    let status =
        unsafe { IOServiceGetMatchingServices(MAIN_PORT_DEFAULT, matching.cast_const(), &mut raw) };
    let Some(accelerators) = (status == KERN_SUCCESS)
        .then(|| IoObject::owned(raw))
        .flatten()
    else {
        return Walk::Failed(Unavailable("the IORegistry lists no GPU accelerator"));
    };
    let prefix = format!("pid {},", std::process::id());
    let creator = CFString::from_static_string("IOUserClientCreator");
    let mut found = Vec::new();
    let mut complete = true;
    while let Some(accelerator) = accelerators.next() {
        let mut raw = 0;
        // SAFETY: the plane name is a NUL-terminated C string literal and the pointer is to a
        // live, writable io_iterator_t; the iterator is returned with a reference we own.
        let status = unsafe {
            IORegistryEntryGetChildIterator(accelerator.0, c"IOService".as_ptr(), &mut raw)
        };
        let Some(children) = (status == KERN_SUCCESS)
            .then(|| IoObject::owned(raw))
            .flatten()
        else {
            continue;
        };
        while let Some(child) = children.next() {
            let ours = child
                .property(&creator)
                .and_then(|value| value.downcast::<CFString>())
                .is_some_and(|name| name.to_string().starts_with(&prefix));
            if ours && let Some(id) = child.registry_id() {
                found.push((child, id));
            }
        }
        complete &= !children.invalidated();
    }
    complete &= !accelerators.invalidated();
    if complete {
        Walk::Found(found)
    } else {
        Walk::Incomplete
    }
}

// GPU allocations from the Metal device.

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    /// The system's default GPU device under the Create rule, or nil. Within one process it is the
    /// same object every time, and the one wgpu presents through.
    fn MTLCreateSystemDefaultDevice() -> *mut AnyObject;
}

struct Allocations {
    enabled: bool,
    /// Opened at the first read after enabling, and kept, including a failure to open one.
    device: Option<Result<MetalDevice, Unavailable>>,
}

impl Allocations {
    fn read(&mut self) -> (Result<u64, Unavailable>, Option<bool>) {
        if !self.enabled {
            return (Err(Unavailable("no GPU presenter in this process")), None);
        }
        match self.device.get_or_insert_with(MetalDevice::system_default) {
            Ok(device) => (Ok(device.allocated_bytes()), Some(device.unified)),
            Err(reason) => (Err(*reason), None),
        }
    }
}

struct MetalDevice {
    device: Retained<AnyObject>,
    /// `hasUnifiedMemory`, read once: it is a property of the hardware.
    unified: bool,
}

// SAFETY: Apple documents MTLDevice as safe to use from any thread, and objc2-metal declares the
// protocol Send + Sync for that reason. The sampler only sends it two read-only property getters
// and releases it on drop, and Objective-C reference counting is atomic, so moving the owning
// reference to another thread is sound. The sampler is only ever used behind `&mut`, so there is
// no concurrent use to consider either.
unsafe impl Send for MetalDevice {}

impl MetalDevice {
    fn system_default() -> Result<Self, Unavailable> {
        // SAFETY: the function takes no arguments and returns an owned reference or nil.
        let raw = unsafe { MTLCreateSystemDefaultDevice() };
        // SAFETY: a non-nil result is a +1 reference under the Create rule, which Retained takes
        // over and releases on drop.
        let device = unsafe { Retained::from_raw(raw) }
            .ok_or(Unavailable("this Mac has no Metal device"))?;
        // SAFETY: hasUnifiedMemory is an MTLDevice property (macOS 10.15) that takes no arguments
        // and returns BOOL, which objc2 converts to bool; the receiver is a live device.
        let unified: bool = unsafe { msg_send![&*device, hasUnifiedMemory] };
        Ok(Self { device, unified })
    }

    fn allocated_bytes(&self) -> u64 {
        // SAFETY: currentAllocatedSize is an MTLDevice property (macOS 10.13) that takes no
        // arguments and returns NSUInteger; the receiver is a live device.
        let bytes: usize = unsafe { msg_send![&*self.device, currentAllocatedSize] };
        bytes as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_convert_through_the_timebase_without_overflow() {
        let m4 = Timebase {
            numer: 125,
            denom: 3,
        };
        assert_eq!(m4.ns(24_000_000), 1_000_000_000, "one second of M4 ticks");
        let intel = Timebase { numer: 1, denom: 1 };
        assert_eq!(intel.ns(123), 123);
        assert_eq!(m4.ns(u64::MAX), u64::MAX);
    }

    /// The fold that keeps GPU time monotonic: a sum that falls keeps what left it, a client that
    /// closes keeps what it spent, and nothing is counted twice.
    #[test]
    fn a_falling_sum_and_a_closed_client_never_take_time_back() {
        let mut retired = 0;
        let mut a = Tally::default();
        let mut b = Tally::default();
        let total = |retired: u64, clients: &[Tally]| {
            clients
                .iter()
                .fold(retired, |total, client| total + client.last_ns)
        };
        retired += a.observe(100);
        retired += b.observe(0);
        assert_eq!(total(retired, &[a, b]), 100);
        retired += a.observe(250);
        retired += b.observe(40);
        assert_eq!(total(retired, &[a, b]), 290);
        // One of a's queues is released with 200 ns on it while another spends 30 more.
        retired += a.observe(80);
        assert_eq!(total(retired, &[a, b]), 290, "the fall is kept");
        retired += a.observe(120);
        assert_eq!(total(retired, &[a, b]), 330, "and later growth is counted");
        // b closes.
        retired += b.last_ns;
        assert_eq!(total(retired, &[a]), 330);
        assert!(a.published && b.published);
        assert!(!Tally::default().published);
    }
}
