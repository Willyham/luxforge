//! The counters against work this test does itself: CPU spent on a thread and, on macOS, GPU
//! memory allocated and a real Metal dispatch. Memory has its own test binary, because the GPU
//! driver returns released memory to the system in the background, which would land between two
//! of its reads.
mod support;

use luxforge_process::{Sampler, Unavailable};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
const MIB: u64 = 1024 * 1024;

fn spin(duration: Duration) {
    let start = Instant::now();
    let mut turns = 0_u64;
    while start.elapsed() < duration {
        turns = std::hint::black_box(turns.wrapping_add(1));
    }
}

/// Read until `accepted` holds of what `read` answers, through the one hang-bounded wait. A busy
/// GPU or a coarse driver counter can delay a rise past a single sample.
#[cfg(target_os = "macos")]
fn read_until<T>(
    what: &str,
    mut read: impl FnMut() -> T,
    mut accepted: impl FnMut(&T) -> bool,
) -> T {
    luxforge_testbase::wait_for(what, || Some(read()).filter(|value| accepted(value)))
}

/// CPU time the calling thread has spent, in nanoseconds, by the operating system's own
/// per-thread clock: what the thread actually ran, however loaded the host is.
#[cfg(unix)]
fn thread_cpu_ns() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the pointer is to a live, writable timespec, which is all the call writes.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) },
        0
    );
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

#[cfg(windows)]
fn thread_cpu_ns() -> u64 {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentThread, GetThreadTimes},
    };
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: GetCurrentThread returns a pseudo-handle that is always valid for this thread and
    // needs no closing; the four pointers are to live, writable FILETIMEs on this stack frame.
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    assert_ne!(ok, 0, "GetThreadTimes failed");
    let hundreds =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    (hundreds(kernel) + hundreds(user)) * 100
}

/// Another thread spins until its own CPU clock reads 200 ms, and the process's CPU time grows by
/// at least that, less the coarsest counter's rounding. The thread's own clock decides how long it
/// spins, so a loaded host that gives it less of each second makes the test slower, never failed.
#[test]
fn cpu_time_grows_by_the_work_of_another_thread() {
    const WORK: u64 = 200_000_000;
    let mut sampler = Sampler::new();
    let before = sampler
        .read()
        .cpu_time_ns
        .expect("CPU time on this platform");
    let spun = std::thread::spawn(|| {
        let start = thread_cpu_ns();
        let mut turns = 0_u64;
        while thread_cpu_ns() - start < WORK {
            turns = std::hint::black_box(turns.wrapping_add(1));
        }
        thread_cpu_ns() - start
    })
    .join()
    .unwrap();
    let after = sampler
        .read()
        .cpu_time_ns
        .expect("CPU time on this platform");
    let grown = after.saturating_sub(before);
    // Linux reports the sampler's figure in clock ticks, usually 10 ms, truncated for user and
    // system time separately, as `cpu_time_agrees_with_getrusage` allows too.
    let slack = 25_000_000;
    assert!(
        grown + slack >= spun,
        "a thread that ran {spun} ns of CPU time added only {grown} ns to the process's"
    );
    assert!(sampler.read().logical_cpus >= 1);
}

/// An independent reading of the same quantity: `getrusage` counts user and system time of every
/// thread in microseconds, so it must fall between two of the sampler's reads taken around it.
/// This is what catches a unit error, such as mach ticks reported as nanoseconds.
#[cfg(unix)]
#[test]
fn cpu_time_agrees_with_getrusage() {
    fn getrusage_ns() -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: the pointer is to a live, writable rusage, which is all the call writes.
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        // SAFETY: the call succeeded, and a zeroed rusage is a valid value regardless.
        let usage = unsafe { usage.assume_init() };
        let ns =
            |time: libc::timeval| time.tv_sec as u64 * 1_000_000_000 + time.tv_usec as u64 * 1_000;
        ns(usage.ru_utime) + ns(usage.ru_stime)
    }
    let mut sampler = Sampler::new();
    spin(Duration::from_millis(50));
    let before = sampler.read().cpu_time_ns.unwrap();
    let theirs = getrusage_ns();
    let after = sampler.read().cpu_time_ns.unwrap();
    // Linux reports the sampler's figure in clock ticks, usually 10 ms, truncated for user and
    // system time separately; getrusage truncates to microseconds. Nothing else may separate them.
    let slack = 25_000_000;
    assert!(
        before <= theirs + slack && theirs <= after + slack,
        "getrusage {theirs} ns is outside the sampler's {before}..{after} ns"
    );
}

#[test]
fn gpu_allocations_are_unavailable_until_enabled() {
    let mut sampler = Sampler::new();
    let gpu = sampler.read().gpu;
    if cfg!(target_os = "macos") {
        assert_eq!(
            gpu.allocated_bytes,
            Err(Unavailable("no GPU presenter in this process"))
        );
    } else {
        assert!(gpu.allocated_bytes.is_err());
        assert!(gpu.time_ns.is_err());
    }
    assert_eq!(gpu.unified_memory, None);
}

/// The fastest any Apple GPU writes memory, in bytes a second, with headroom: the M2 and M3 Ultra
/// reach 800 GB/s, and the M4 Pro fills the test's buffer at about 200 GB/s.
#[cfg(target_os = "macos")]
const FASTEST_FILL: u64 = 1_000_000_000_000;

/// A 256 MiB private buffer raises the device's allocations by at least its size, and filling it
/// with the blit engine raises GPU time. The GPU time the IORegistry reports is held, as an
/// independent unit check, between the least time the blit's bytes take at the fastest bandwidth
/// an Apple GPU has and a multiple of the command buffer's own GPU start and end times.
#[cfg(target_os = "macos")]
#[test]
fn a_metal_dispatch_raises_gpu_time_and_allocations() {
    let mut sampler = Sampler::new();
    sampler.enable_gpu_allocations();
    let first = sampler.read().gpu;
    let allocated = match first.allocated_bytes {
        Ok(bytes) => bytes,
        Err(reason) => {
            println!("SKIPPED: this machine has no Metal device ({reason})");
            return;
        }
    };
    assert!(first.unified_memory.is_some());
    let buffer = 256 * MIB;
    let gpu = support::Metal::open(buffer as usize).expect("a queue and a buffer on the device");
    // The measured queue may create its driver client lazily. Warm it before a fresh sampler's
    // first walk: a warm sampler discovers new clients only every ten seconds, which this unit
    // check must not confuse with the driver's much shorter counter-retirement delay.
    gpu.dispatch(1);
    sampler = Sampler::new();
    sampler.enable_gpu_allocations();
    // The driver's own allocation accounting can lag a freshly committed private buffer by a
    // beat, the same coarse-update race the GPU time check below waits out.
    let with_buffer = read_until(
        "allocations rising by the buffer",
        || sampler.read().gpu,
        |with_buffer| {
            with_buffer
                .allocated_bytes
                .is_ok_and(|now| now.saturating_sub(allocated) >= buffer)
        },
    );
    let now = with_buffer
        .allocated_bytes
        .expect("allocations once enabled");
    assert!(
        now.saturating_sub(allocated) >= buffer,
        "a {buffer} byte buffer moved allocations from {allocated} to {now}"
    );
    // The device opened above is a GPU client of this process, so on Apple's GPU driver GPU time
    // is a number now, zero or more. A virtual machine's paravirtual GPU need not publish it.
    let before = match with_buffer.time_ns {
        Ok(ns) => Some(ns),
        Err(reason) if support::virtual_machine() => {
            println!("SKIPPED GPU time: this Mac is a virtual machine and reports none ({reason})");
            None
        }
        Err(reason) => {
            panic!("GPU time is unavailable on a native Mac with a GPU client: {reason}")
        }
    };
    let after = before.map(|before| {
        const FILLS: usize = 16;
        let measured = gpu.dispatch(FILLS);
        // AppUsage is this process's active GPU time, while GPUStartTime..GPUEndTime is the
        // command buffer's GPU window, which other processes' work on a loaded host stretches
        // without adding to this process's time. So the window bounds the rise only from above;
        // from below it is bounded by the bytes the blit wrote at a bandwidth no Apple GPU
        // reaches ([`FASTEST_FILL`]), which no load changes. That still catches a unit error: a
        // counter in microseconds read as nanoseconds would rise about a two-hundredth of it.
        let floor = (FILLS as u64 * buffer).saturating_mul(1_000_000_000) / FASTEST_FILL;
        let after = read_until(
            "GPU time rising by the dispatch",
            || sampler.read().gpu.time_ns.expect("GPU time"),
            |after| after.saturating_sub(before) >= floor,
        );
        let grown = after - before;
        println!(
            "GPU time grew {grown} ns; the bytes written need at least {floor} ns; Metal \
             measured the command buffer at {measured} ns"
        );
        assert!(
            grown <= measured.saturating_mul(4).max(1_000_000),
            "GPU time grew {grown} ns for a command buffer Metal measured at {measured} ns"
        );
        after
    });
    // Releasing the queue takes its entry out of AppUsage; the time it spent stays counted.
    drop(gpu);
    // The same lag applies in reverse: a released buffer's bytes can take a beat to leave the
    // driver's own allocation accounting.
    let released = read_until(
        "allocations falling after the release",
        || sampler.read().gpu,
        |released| {
            released
                .allocated_bytes
                .is_ok_and(|allocations| allocations < now)
        },
    );
    let allocations = released.allocated_bytes.unwrap();
    assert!(
        allocations < now,
        "releasing the buffer left allocations at {allocations}"
    );
    if let Some(after) = after {
        let kept = released.time_ns.expect("GPU time");
        assert!(
            kept >= after,
            "GPU time went back from {after} to {kept} ns"
        );
    }
}
