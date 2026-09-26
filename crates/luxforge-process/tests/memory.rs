//! Memory against pages this test touches itself. It is the only test in its binary, so no other
//! test's allocation, release or GPU memory returned in the background lands between its reads.
use luxforge_process::{MemoryKind, Sampler};

const MIB: u64 = 1024 * 1024;

#[test]
fn memory_grows_with_touched_pages_and_peak_is_at_least_current() {
    let mut sampler = Sampler::new();
    let before = sampler.read().memory;
    let expected_kind = if cfg!(target_os = "macos") {
        MemoryKind::Footprint
    } else if cfg!(windows) {
        MemoryKind::Private
    } else {
        MemoryKind::Resident
    };
    assert_eq!(before.kind, expected_kind);
    let base = before.bytes.expect("memory on this platform");
    let mut block = vec![0_u8; 64 * MIB as usize];
    // `vec![0; n]` maps zero pages lazily; writing one byte of every 4 KiB dirties every page on
    // every platform, including the 16 KiB pages of Apple silicon.
    for page in block.chunks_mut(4096) {
        page[0] = 1;
    }
    std::hint::black_box(&block);
    let during = sampler.read().memory;
    let bytes = during.bytes.expect("memory on this platform");
    assert!(
        bytes.saturating_sub(base) >= 48 * MIB,
        "touching 64 MiB moved memory from {base} to {bytes}"
    );
    let peak = during.peak_bytes.expect("peak memory on this platform");
    let resident = during
        .resident_bytes
        .expect("resident memory on this platform");
    assert!(resident > 0);
    // Windows's peak is the peak working set, a resident measure, so it bounds the current working
    // set rather than private bytes; elsewhere it is the peak of `bytes` itself.
    let current = if cfg!(windows) { resident } else { bytes };
    assert!(peak >= current, "peak {peak} is below current {current}");
    drop(block);
}
