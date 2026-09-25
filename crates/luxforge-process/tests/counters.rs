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

#[test]
fn cpu_time_grows_by_the_work_of_another_thread() {
    let mut sampler = Sampler::new();
    let before = sampler
        .read()
        .cpu_time_ns
        .expect("CPU time on this platform");
    std::thread::spawn(|| spin(Duration::from_millis(200)))
        .join()
        .unwrap();
    let after = sampler
        .read()
        .cpu_time_ns
        .expect("CPU time on this platform");
    let grown = after.saturating_sub(before);
    assert!(
        grown >= 150_000_000,
        "200 ms of spinning added only {grown} ns of CPU time"
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

/// A 256 MiB private buffer raises the device's allocations by at least its size, and filling it
/// with the blit engine raises GPU time. The GPU time the IORegistry reports is compared, loosely,
/// with the command buffer's own GPU start and end times as an independent unit check.
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
    let with_buffer = sampler.read().gpu;
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
        let measured = gpu.dispatch(16);
        // AppUsage is active GPU time while GPUStartTime..GPUEndTime is the command buffer's GPU
        // window; the blit can occupy a fraction of that window on Apple silicon. Wait for a
        // meaningful fraction rather than treating the command-buffer interval as equal GPU work.
        let deadline = Instant::now() + Duration::from_secs(2);
        let after = loop {
            let after = sampler.read().gpu.time_ns.expect("GPU time");
            if after.saturating_sub(before) >= (measured / 8).max(1) || Instant::now() > deadline {
                break after;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            after > before,
            "GPU time stayed at {before} ns after a dispatch"
        );
        let grown = after - before;
        println!("GPU time grew {grown} ns; Metal measured the command buffer at {measured} ns");
        assert!(
            grown >= measured / 8 && grown <= measured.saturating_mul(4).max(1_000_000),
            "GPU time grew {grown} ns for a command buffer Metal measured at {measured} ns"
        );
        after
    });
    // Releasing the queue takes its entry out of AppUsage; the time it spent stays counted.
    drop(gpu);
    let released = sampler.read().gpu;
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
