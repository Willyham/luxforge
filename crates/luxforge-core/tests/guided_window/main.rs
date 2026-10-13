//! What the guided upsampler allocates: its window and its coefficient grid, never a plane of the
//! whole output. A small object's selection on a 4096 px output costs a few hundred kilobytes; a
//! frame-filling one stays inside the bound the grid's cap sets.
//!
//! The counter is the calling thread's own, and the upsampler runs on the calling thread, so the
//! harness's other threads cannot disturb the figure.
use luxforge_core::{
    Cancel,
    mask::guided::{self, GRID_MAX, Params, Plane},
};
use luxforge_process::allocations::{self, Counting};
use luxforge_test_macros::slow;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const SIDE: u32 = 4096;
const CELLS: u32 = 1024;

/// The bytes the calling thread held at its peak while `run` ran, beyond what it held before.
fn peak_of(run: impl FnOnce() -> guided::Upsampled) -> (i64, guided::Upsampled) {
    allocations::reset_peak();
    let before = allocations::thread_live_bytes();
    let upsampled = run();
    (allocations::thread_peak_bytes() - before, upsampled)
}

fn upsample(prior: &[f32], guide: &[f32]) -> guided::Upsampled {
    guided::upsample(
        Plane {
            width: CELLS,
            height: CELLS,
            values: prior,
        },
        Plane {
            width: SIDE,
            height: SIDE,
            values: guide,
        },
        Params::for_sizes((CELLS, CELLS), (SIDE, SIDE)),
        &Cancel::never(),
    )
    .expect("a well-formed case")
    .expect("a non-zero prior")
}

#[test]
#[slow]
fn the_upsampler_allocates_only_its_window() {
    let guide: Vec<f32> = (0..SIDE * SIDE)
        .map(|k| ((k % SIDE) as f32 / SIDE as f32) * 0.5 + 0.25)
        .collect();

    // A small object: 24 cells, about 96 px, of a 4096 px output.
    let mut prior = vec![0.0f32; (CELLS * CELLS) as usize];
    for j in 500..524 {
        for i in 300..324 {
            prior[(j * CELLS + i) as usize] = 0.9;
        }
    }
    let (peak, upsampled) = peak_of(|| upsample(&prior, &guide));
    let window = u64::from(upsampled.window.width) * u64::from(upsampled.window.height);
    assert!(
        window < 200 * 200,
        "the window is {}x{}",
        upsampled.window.width,
        upsampled.window.height
    );
    // The codes, ten f64 planes of the window's coefficient grid and their integral, and the two
    // axis tables: nothing near the 16 MiB a plane of the output would be.
    let bound = window + 12 * 8 * window + 4096;
    assert!(
        (peak as u64) < bound,
        "the small object's upsample peaked at {peak} bytes against {bound}"
    );
    assert!(peak < 1 << 20, "{peak} bytes for a small object");

    // A frame-filling one: its window is the whole output, and the grid's cap bounds the planes.
    let full = vec![0.75f32; (CELLS * CELLS) as usize];
    let (peak, upsampled) = peak_of(|| upsample(&full, &guide));
    assert_eq!(
        (upsampled.window.width, upsampled.window.height),
        (SIDE, SIDE)
    );
    let bound = u64::from(SIDE) * u64::from(SIDE) + 11 * 8 * GRID_MAX + (1 << 20);
    assert!(
        (peak as u64) < bound,
        "the frame-filling upsample peaked at {peak} bytes against {bound}"
    );
}
