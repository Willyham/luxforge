//! What one encode holds beyond the caller's frame, read from the process's peak footprint (macOS
//! physical footprint, Linux peak resident set, Windows peak working set), which counts libwebp's
//! own allocations as well as Rust's.
//!
//! The platform keeps one peak for the life of the process, and memory an encode frees may stay in
//! the footprint, so each measured encode is the one test of a binary of its own
//! (`footprint_plain`, `footprint_optimised`): it makes the frame before the first read, and its
//! figure is the peak after the encode less the level before it.

use luxforge_process::Sampler;
use luxforge_webp::{Settings, encode};

pub const WIDTH: u32 = 4096;
pub const HEIGHT: u32 = 3072;

/// A photograph-like frame: smooth gradients with a fine, low-contrast texture, touched so it is
/// in the footprint before the first read.
fn frame() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(WIDTH as usize * HEIGHT as usize * 4);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let texture = ((x * 7 + y * 13) % 16) as u8;
            let r = (x * 200 / WIDTH) as u8 + texture;
            let g = (y * 200 / HEIGHT) as u8 + texture / 2;
            let b = ((x + y) * 100 / (WIDTH + HEIGHT)) as u8 + 60;
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    rgba
}

/// The bytes a pixel one encode of the frame adds to the footprint at its peak.
pub fn held_per_pixel(optimise: bool) -> f64 {
    let rgba = frame();
    let mut sampler = Sampler::new();
    let memory = sampler.read().memory;
    let before = memory.bytes.expect("the platform reports the footprint");
    let peak_before = memory.peak_bytes.expect("the platform reports its peak");
    let file = encode(
        WIDTH,
        HEIGHT,
        &rgba,
        &Settings {
            quality: 80,
            optimise,
            icc: None,
            exif: None,
        },
        &mut |_| true,
    )
    .expect("encodes");
    let peak = sampler.read().memory.peak_bytes.unwrap();
    assert!(
        peak > peak_before,
        "the encode did not raise the process's peak, so the figure would not be its own"
    );
    let pixels = f64::from(WIDTH) * f64::from(HEIGHT);
    let held = (peak - before) as f64 / pixels;
    eprintln!(
        "optimise {optimise}: {held:.2} bytes a pixel held beyond the {:.1} MB frame, for a \
         {:.2} MB file",
        pixels * 4.0 / 1e6,
        file.len() as f64 / 1e6
    );
    held
}
