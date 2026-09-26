//! The owner-side cost of `resources.read`, for the performance record. It is a timing, not a
//! check, so it is ignored; run it in release on a quiet machine:
//!
//! ```sh
//! cargo test --release --locked -p luxforge-core --test resources_cost -- --ignored --nocapture
//! ```
//!
//! It is a test binary of its own so its first half runs in a process that has never touched the
//! GPU, as the headless owner is, before its second half declares a presenter as the desktop does.
use luxforge_core::resources;
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

/// `reads` timed reads, and the same again including the JSON conversion the method handler does.
fn measure(label: &str, reads: usize) {
    summary(
        &format!("{label}, read"),
        (0..reads)
            .map(|_| {
                let start = Instant::now();
                std::hint::black_box(resources::read(context()));
                micros(start)
            })
            .collect(),
    );
    summary(
        &format!("{label}, read + JSON value"),
        (0..reads)
            .map(|_| {
                let start = Instant::now();
                std::hint::black_box(serde_json::to_value(resources::read(context())).unwrap());
                micros(start)
            })
            .collect(),
    );
}

#[test]
#[ignore = "timing for the performance record; prints p50, p95 and max"]
fn resources_read_cost() {
    let start = Instant::now();
    let report = resources::read(context());
    println!("first read (creates the sampler): {:.1} µs", micros(start));
    println!("headless: {}", serde_json::to_string(&report).unwrap());
    measure("headless, no GPU client, so every read walks", 1000);

    resources::declare_gpu_presenter();
    let start = Instant::now();
    let report = resources::read(context());
    println!(
        "first read after declaring a presenter (opens the Metal device, walks): {:.1} µs",
        micros(start)
    );
    measure("presenter, warm client cache", 1000);
    println!(
        "presenter: {}",
        serde_json::to_string(&resources::read(context())).unwrap()
    );
    drop(report);
}

/// The render context whose budgets each read reports.
fn context() -> &'static luxforge_core::RenderContext {
    static CONTEXT: std::sync::OnceLock<luxforge_core::RenderContext> = std::sync::OnceLock::new();
    CONTEXT.get_or_init(luxforge_core::RenderContext::new)
}
