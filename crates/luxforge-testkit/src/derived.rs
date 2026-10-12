//! The GPU qualification corpus's synthetic derived source (`fixtures/preview/corpus.json`, kind
//! `synthetic-derived`): the planes an HDR merge's result holds (`docs/design/hdr-merge.md`),
//! generated here, the one definition both the desktop's corpus harness, which stores them as a
//! derived photograph, and `cargo xtask preview-corpus`, which checks their recorded SHA-256,
//! read.
use sha2::{Digest, Sha256};

/// Linear-sRGB planes of `width` × `height`, red values row by row, then green, then blue, as an
/// HDR merge's result holds them: a horizontal ramp in exposure from just below zero, through
/// diffuse white, to four times it; hues turning down the frame; a band of fine texture; hard-edged
/// blocks; a little deterministic noise; and patches out of the sRGB gamut, whose red is negative.
/// Pure arithmetic in `f32`, so every host makes the same bits.
pub fn planes(width: u32, height: u32) -> Vec<f32> {
    let plane = width as usize * height as usize;
    let mut values = vec![0.0_f32; 3 * plane];
    let (w, h) = (width.max(2) as f32 - 1.0, height.max(2) as f32 - 1.0);
    for y in 0..height {
        let v = y as f32 / h;
        // Hues turn down the frame: a chroma direction from a quarter-turn polynomial, no
        // transcendental functions, so the bits do not depend on a platform's maths library.
        let t = v * 4.0;
        let phase = t - t.floor();
        let (a, b) = match t as u32 {
            0 => (1.0 - phase, phase),
            1 => (-phase, 1.0 - phase),
            2 => (-(1.0 - phase), -phase),
            _ => (phase, -(1.0 - phase)),
        };
        for x in 0..width {
            let u = x as f32 / w;
            // Exposure: 2^-7 at the left edge to 4 at the right, by a cubic through the stops,
            // and a pedestal a little below zero in the darkest columns, as a merge's noise floor.
            let level = 0.0078 + 3.9922 * u * u * u - if u < 0.03 { 0.012 } else { 0.0 };
            let texture = if (0.15..0.3).contains(&v) {
                0.12 * (((x * 7 + y * 3) % 5) as f32 - 2.0) / 2.0
            } else {
                0.0
            };
            let block = if ((x / 256) + (y / 256)) % 7 == 0 {
                0.35
            } else {
                0.0
            };
            let hash = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) % 1024;
            let noise = (hash as f32 / 1023.0 - 0.5) * 0.02;
            let base = level * (1.0 + texture + block) + noise * level.min(1.0);
            let mut rgb = [
                base * (1.0 + 0.45 * a),
                base * (1.0 - 0.25 * a + 0.3 * b),
                base * (1.0 - 0.45 * b),
            ];
            if (x / 64) % 23 == 5 && (y / 64) % 9 == 4 {
                rgb = [-0.06 * base - 0.02, 0.55 * base + 0.05, 0.9 * base + 0.02];
            }
            let at = y as usize * width as usize + x as usize;
            for (channel, value) in rgb.into_iter().enumerate() {
                values[channel * plane + at] = value;
            }
        }
    }
    values
}

/// The SHA-256 of `planes` as little-endian binary32 samples, in their order: what the corpus
/// records for a synthetic derived source.
pub fn sha256(planes: &[f32]) -> String {
    let mut digest = Sha256::new();
    for value in planes {
        digest.update(value.to_le_bytes());
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The planes hold values below zero and well above one, in every channel's range, and a
    /// small frame's digest is pinned, so a change to the generator is a change to the corpus.
    #[test]
    fn derived_source_planes_are_signed_with_headroom_and_pinned() {
        let planes = planes(640, 480);
        assert!(planes.iter().all(|value| value.is_finite()));
        assert!(planes.iter().any(|value| *value < 0.0));
        assert!(planes.iter().any(|value| *value > 3.0));
        assert_eq!(
            sha256(&planes),
            "313919d2de36ef6b4c531ad2e3ca3516bb2301ec1fb8dd8722826223bf692d4d"
        );
    }
}
