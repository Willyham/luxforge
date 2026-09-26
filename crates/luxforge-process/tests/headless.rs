//! A process that never touches the GPU, as the headless JSON owner is: it has no GPU client, so
//! GPU time is unavailable with that reason rather than zero, and allocations are unavailable
//! because no presenter asked for them. It is its own test binary so no other test's device can
//! exist in the process.
mod support;

use luxforge_process::Sampler;
#[cfg(target_os = "macos")]
use luxforge_process::Unavailable;

#[test]
fn a_process_without_a_gpu_client_reports_why_it_has_no_gpu_counters() {
    let mut sampler = Sampler::new();
    let counters = sampler.read();
    assert!(counters.cpu_time_ns.is_ok() || cfg!(not(any(unix, windows))));
    let gpu = counters.gpu;
    assert!(gpu.time_ns.is_err() && gpu.allocated_bytes.is_err());
    #[cfg(target_os = "macos")]
    {
        // A virtual machine may have no Apple GPU accelerator to walk, which is a reason of its
        // own; a native Mac has one, and this process is not among its clients.
        if !support::virtual_machine() {
            assert_eq!(
                gpu.time_ns,
                Err(Unavailable("no GPU client in this process"))
            );
        }
        assert_eq!(
            gpu.allocated_bytes,
            Err(Unavailable("no GPU presenter in this process"))
        );
    }
    assert_eq!(gpu.unified_memory, None);
    // Reading again walks again, finds the same, and still reports no number.
    assert!(sampler.read().gpu.time_ns.is_err());
}
