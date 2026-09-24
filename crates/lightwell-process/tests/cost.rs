//! The sampler's cost with this process's GPU clients holding real `AppUsage` entries: a cold
//! read, which walks every accelerator's children, against a warm one, which asks only the cached
//! clients. It is a timing, not a check, so it is ignored; run it in release on a quiet machine:
//!
//! ```sh
//! cargo test --release --locked -p lightwell-process --test cost -- --ignored --nocapture
//! ```
//!
//! Only macOS has GPU clients to walk, so only there is there anything to time.
#![cfg(target_os = "macos")]

mod support;

use lightwell_process::Sampler;
use std::time::Instant;

fn micros(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e6
}

fn summary(label: &str, mut samples: Vec<f64>) {
    samples.sort_by(f64::total_cmp);
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    println!(
        "{label}: n={} p50={:.1} µs p95={:.1} µs max={:.1} µs",
        samples.len(),
        at(0.5),
        at(0.95),
        samples[samples.len() - 1]
    );
}

#[test]
#[ignore = "timing for the performance record; prints p50, p95 and max"]
fn cold_and_warm_reads() {
    let Some(gpu) = support::Metal::open(64 * 1024 * 1024) else {
        println!("SKIPPED: this machine has no Metal device");
        return;
    };
    let measured = gpu.dispatch(8);
    println!("dispatch: {measured} ns of GPU time by Metal's clock");
    let clients = std::process::Command::new("ioreg")
        .args(["-r", "-c", "IOAccelerator", "-d", "2"])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|line| line.contains("+-o") && line.contains("UserClient"))
                .count()
        })
        .unwrap_or(0);
    println!("user clients under the accelerators: {clients}");
    // A fresh sampler's first read walks; its second is warm.
    let mut cold = Vec::new();
    let mut second = Vec::new();
    for _ in 0..200 {
        let mut sampler = Sampler::new();
        let start = Instant::now();
        let counters = sampler.read();
        cold.push(micros(start));
        assert!(counters.gpu.time_ns.is_ok(), "{counters:?}");
        let start = Instant::now();
        std::hint::black_box(sampler.read());
        second.push(micros(start));
    }
    summary("cold read (walk)", cold);
    summary("second read (warm)", second);
    // The desktop's case: the device is already open, as wgpu opened it, when the first read with
    // allocations enabled looks it up. That read also walks.
    let mut sampler = Sampler::new();
    sampler.enable_gpu_allocations();
    let start = Instant::now();
    let counters = sampler.read();
    println!(
        "first read with allocations, device already open: {:.1} µs",
        micros(start)
    );
    println!("{counters:?}");
    summary(
        "warm read with allocations",
        (0..1000)
            .map(|_| {
                let start = Instant::now();
                std::hint::black_box(sampler.read());
                micros(start)
            })
            .collect(),
    );
}
