//! The optimised encode's footprint (see `common`). This binary holds this one test.

mod common;

use luxforge_test_macros::slow;

/// The plain encode's holdings plus the sharp conversion's fixed-point working planes (about 7
/// bytes a pixel, released before the encoder runs): less than libwebp's own sharp path, which
/// converts from an ARGB copy of the frame and so holds 4 bytes a pixel more.
#[test]
#[slow]
fn an_optimised_encode_converts_without_a_copy_of_the_frame() {
    let held = common::held_per_pixel(true);
    assert!(held < 10.0, "{held:.2} bytes a pixel");
}
