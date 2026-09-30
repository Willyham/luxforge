//! libjpeg's DCT scaling: a frame decoded straight to 1/2, 1/4 or 1/8 of its size on each side by a
//! reduced inverse DCT, rather than decoded in full and resampled.

/// The scale a [`crate::Decoder`] decodes at: libjpeg's `scale_num` of 8, 4, 2 or 1 over a
/// `scale_denom` of 8.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scale {
    Full,
    Half,
    Quarter,
    Eighth,
}

impl Scale {
    /// Every scale, from the least reducing to the most.
    pub const ALL: [Scale; 4] = [Scale::Full, Scale::Half, Scale::Quarter, Scale::Eighth];

    /// libjpeg's `scale_num`, over a `scale_denom` of 8.
    pub fn numerator(self) -> u8 {
        match self {
            Scale::Full => 8,
            Scale::Half => 4,
            Scale::Quarter => 2,
            Scale::Eighth => 1,
        }
    }

    /// The output size of a `width` × `height` frame: each side times the numerator over 8,
    /// rounded up, as libjpeg's `jpeg_core_output_dimensions` (`jdmaster.c`) computes it.
    pub fn output(self, width: u32, height: u32) -> (u32, u32) {
        let side = |length: u32| {
            // At most the side itself, so it fits.
            (u64::from(length) * u64::from(self.numerator())).div_ceil(8) as u32
        };
        (side(width), side(height))
    }

    /// The most reducing scale whose output of `frame` (width, height) still covers `target`
    /// (width, height): both output sides at least the target's, so resampling the decode to the
    /// target only ever shrinks it. `target` is the size the decode becomes, such as the frame
    /// fitted into a tier's bounds, not the bounds themselves. [`Scale::Full`] when even the whole
    /// frame is narrower or shorter than the target.
    pub fn covering(frame: (u32, u32), target: (u32, u32)) -> Scale {
        Scale::ALL
            .into_iter()
            .rev()
            .find(|scale| {
                let (width, height) = scale.output(frame.0, frame.1);
                width >= target.0 && height >= target.1
            })
            .unwrap_or(Scale::Full)
    }
}
