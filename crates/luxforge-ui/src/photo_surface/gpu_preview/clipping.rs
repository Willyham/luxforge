//! Clipping marks: the clipping overlay the GPU stage draws over its own output while a gesture
//! moves (`docs/design/gpu-preview.md`, "Labels and overlays during motion").
//!
//! The CPU's clipping overlay is a cell grid derived on a worker from the CPU frame on screen, so
//! over a GPU frame it would mark another frame's pixels. A plan whose last step is
//! [`GpuStep::Clipping`](super::GpuStep::Clipping) marks its own: in the last pass, after every
//! colour and geometry step, a pixel with a channel whose output code is 0 (shadows) or 255
//! (highlights) — by the CPU quantizer's own thresholds, so a NaN counts as code 0, as the
//! quantizer takes it — is laid over with the overlay's colour for its class at the overlay's
//! opacity. As the surface composites the CPU overlay, the photograph's codes are decoded to
//! linear light and blended there with the colour's decoded codes, then encoded once.
//!
//! The marks are approximate: they are per pixel of the stage the plan draws, a display-size
//! proxy at Fit, where the CPU overlay ORs each display cell over the frame it was derived from.
//! Whole-image clipping counts stay the CPU's exact analysis, and the CPU frame's own overlay
//! replaces the marks when the gesture settles.

/// The clipping overlay one plan's output draws: which classes, the quantizer's thresholds and the
/// overlay's colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipMarks {
    /// Mark pixels with a channel at code 0.
    pub shadows: bool,
    /// Mark pixels with a channel at code 255.
    pub highlights: bool,
    /// The linear value below which a channel's output code is 0, a NaN included.
    pub shadow_below: f32,
    /// The linear value from which a channel's output code is 255.
    pub highlight_from: f32,
    /// The overlay's RGBA8 colours for a shadow, a highlight and both, as the CPU overlay draws
    /// them: sRGB codes and an opacity over 255.
    pub palette: [[u8; 4]; 3],
}

impl ClipMarks {
    /// The step's words: its classes, its two thresholds and its three colours.
    pub(super) const WORDS: usize = 6;

    pub(super) fn words(&self) -> [u32; Self::WORDS] {
        let pack = |rgba: [u8; 4]| u32::from_le_bytes(rgba);
        [
            u32::from(self.shadows) | (u32::from(self.highlights) << 1),
            self.shadow_below.to_bits(),
            self.highlight_from.to_bits(),
            pack(self.palette[0]),
            pack(self.palette[1]),
            pack(self.palette[2]),
        ]
    }
}

/// The marks' function, which the last pass declares beside the output encoding it reads.
pub(super) const SOURCE: &str = "
fn lf_output_mark_colour(packed: u32) -> vec4<f32> {
    return vec4<f32>(
        lf_output_decode(packed & 255u),
        lf_output_decode((packed >> 8u) & 255u),
        lf_output_decode((packed >> 16u) & 255u),
        f32(packed >> 24u) / 255.0,
    );
}

fn lf_output_marks(rgb: vec3<f32>, words: u32) -> vec3<f32> {
    let classes = lf_word(words);
    // Code 0 for a value below the first threshold, NaN included; code 255 from the last.
    let low = any(!(rgb >= vec3<f32>(lf_f32(words + 1u))));
    let high = any(rgb >= vec3<f32>(lf_f32(words + 2u)));
    let shadow = low && (classes & 1u) != 0u;
    let highlight = high && (classes & 2u) != 0u;
    if !(shadow || highlight) {
        return rgb;
    }
    var packed = lf_word(words + 3u);
    if shadow && highlight {
        packed = lf_word(words + 5u);
    } else if highlight {
        packed = lf_word(words + 4u);
    }
    let mark = lf_output_mark_colour(packed);
    return mix(lf_output_requantize(rgb), mark.rgb, mark.a);
}
";

/// The last pass's statement for a marks step whose header is at `base`.
pub(super) fn statement(base: usize) -> String {
    format!("    rgb = lf_output_marks(rgb, lf_words[{base}u]);\n")
}

#[cfg(test)]
mod tests;
