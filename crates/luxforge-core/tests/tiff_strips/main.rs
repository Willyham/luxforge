//! What a TIFF export holds beyond the caller's bands, counted by the allocator: the strips' own
//! source (`src/export/tiff/strips.rs`), compiled into this binary as the core compiles it, writing
//! a frame made before counting starts, in bands of it, to a sink.
//!
//! The counter keeps the calling thread's live bytes and their peak, so this binary's tests may run
//! side by side; each counts only its own thread's work.

// The strips name the crate root's `Error` and `tiles`, which here are the core's own.
use luxforge_core::Error;
mod tiles {
    pub use luxforge_core::tiles::{Depth, PixelRows};
}
#[allow(dead_code)]
#[path = "../../src/export/tiff/strips.rs"]
mod strips;

use luxforge_process::allocations::{self, Counting};
use luxforge_test_macros::slow;
use strips::Strips;
use tiles::{Depth, PixelRows};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const WIDTH: u32 = 1536;
/// Rows a band: the size of a GPU tile row's band.
const BAND: u32 = 256;

/// The bytes the strips allocate at their peak, beyond what the thread held before, for a frame of
/// `height` rows written in bands.
fn held(height: u32, depth: Depth, compress: bool) -> (i64, usize) {
    let samples = WIDTH as usize * height as usize * 4;
    let eight: Vec<u8>;
    let sixteen: Vec<u16>;
    let band_samples = WIDTH as usize * BAND as usize * 4;
    let bands: Vec<PixelRows<'_>> = match depth {
        Depth::Eight => {
            eight = (0..samples).map(|index| (index % 251) as u8).collect();
            eight.chunks(band_samples).map(PixelRows::Eight).collect()
        }
        Depth::Sixteen => {
            sixteen = (0..samples).map(|index| (index % 65_521) as u16).collect();
            sixteen
                .chunks(band_samples)
                .map(PixelRows::Sixteen)
                .collect()
        }
    };
    allocations::reset_peak();
    let before = allocations::thread_live_bytes();
    let mut strips = Strips::new(WIDTH, height, depth, compress, 8);
    let mut out = std::io::sink();
    for band in bands.iter().copied() {
        let rows = (band.len() / (WIDTH as usize * 4)) as u32;
        strips
            .write_rows(&mut out, rows, band, &mut |_| Ok::<(), Error>(()))
            .unwrap();
    }
    assert_eq!(strips.rows(), height);
    let peak = allocations::thread_peak_bytes() - before;
    (peak, strips.tables().0.len())
}

/// The strips hold one RGB row, the Deflate state and its output buffer, and the strip tables:
/// a fixed amount, far below one band, that does not grow with the image but by the tables'
/// 8 bytes a strip.
#[test]
#[slow]
fn the_strips_hold_a_row_the_deflate_state_and_the_tables_never_a_band() {
    for depth in [Depth::Eight, Depth::Sixteen] {
        let band_bytes = WIDTH as i64 * i64::from(BAND) * 4 * depth.bytes() as i64;
        for compress in [false, true] {
            let (short, short_strips) = held(1024, depth, compress);
            let (tall, tall_strips) = held(4096, depth, compress);
            eprintln!(
                "{}-bit, compress {compress}: {short} bytes for 1024 rows, {tall} for 4096, \
                 beside bands of {band_bytes}",
                depth.bytes() * 8
            );
            let tables = 8 * (tall_strips - short_strips) as i64;
            assert!(
                tall - short <= tables + 64,
                "{compress}: the holdings grow with the image beyond the strip tables"
            );
            // A row, the Deflate state and its 64 KiB chunk, and the tables.
            assert!(
                tall < 1 << 20,
                "{compress}: {tall} bytes is more than the row, the Deflate state and the tables"
            );
            assert!(
                tall < band_bytes / 2,
                "{compress}: {tall} bytes against a band"
            );
        }
    }
}
