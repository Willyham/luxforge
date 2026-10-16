//! The plain encode's footprint (see `common`). This binary holds this one test.

mod common;

use luxforge_test_macros::slow;

/// libwebp's YUV 4:2:0 planes (1.5 bytes a pixel), the encoder's state and the bitstream: less
/// than one more RGBA frame (4 bytes a pixel), so the frame is never copied.
#[test]
#[slow]
fn a_plain_encode_holds_less_than_a_copy_of_the_frame() {
    let held = common::held_per_pixel(false);
    assert!(held < 4.0, "{held:.2} bytes a pixel");
}
