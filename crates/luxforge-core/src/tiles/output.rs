//! What an export asks its renderer for (`docs/design/export-settings.md`, rendering): the output's
//! pixel size, which is the output stage's or smaller, never larger, and the bits per channel of its
//! encoding. The GPU's bands and the reference's frame are both of this shape, and the encoders read
//! it, so a 16-bit output is encoded once at 16 bits rather than widened from 8.

/// Bits per channel of an export's output encoding: 8 for every format, 16 for TIFF only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Depth {
    #[default]
    Eight,
    Sixteen,
}

impl Depth {
    /// Bytes per channel sample.
    pub const fn bytes(self) -> usize {
        match self {
            Self::Eight => 1,
            Self::Sixteen => 2,
        }
    }
}

/// The pixels an export renders: `width` × `height`, at most the output stage's size on both sides
/// and scaled from it by one factor, encoded at `depth`, then sharpened when `sharpen` says so
/// ([`super::sharpen`]). A `width` and `height` equal to the stage's are the full-size output, which
/// is rendered without resampling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OutputSpec {
    pub width: u32,
    pub height: u32,
    pub depth: Depth,
    /// Output sharpening of the encoded codes, at the output's size, after resizing.
    pub sharpen: Option<super::sharpen::OutputSharpen>,
}

impl OutputSpec {
    /// The full-size 8-bit output of a `width` × `height` stage, unsharpened: today's export.
    pub const fn full(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            depth: Depth::Eight,
            sharpen: None,
        }
    }

    /// Whether it is the stage's own size at 8 bits, unsharpened: the exact frame itself, and on
    /// the GPU its tiles' own codes, neither held nor resampled.
    pub const fn is_exact_frame(&self, stage_width: u32, stage_height: u32) -> bool {
        self.is_full(stage_width, stage_height)
            && matches!(self.depth, Depth::Eight)
            && self.sharpen.is_none()
    }

    /// Whether this output is the stage's own size, so nothing is resampled.
    pub const fn is_full(&self, stage_width: u32, stage_height: u32) -> bool {
        self.width == stage_width && self.height == stage_height
    }
}

/// Opaque RGBA rows of an output, row by row from the left: 8-bit sRGB codes, or 16-bit sRGB codes
/// (0–65,535, alpha 65,535) in native byte order. Each row is `width × 4` samples.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pixels {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}

impl Pixels {
    pub fn depth(&self) -> Depth {
        match self {
            Self::Eight(_) => Depth::Eight,
            Self::Sixteen(_) => Depth::Sixteen,
        }
    }

    /// Borrowed rows, as an encoder reads them.
    pub fn rows(&self) -> PixelRows<'_> {
        match self {
            Self::Eight(samples) => PixelRows::Eight(samples),
            Self::Sixteen(samples) => PixelRows::Sixteen(samples),
        }
    }
}

/// [`Pixels`] borrowed: what an encoder is handed, a band or a whole frame, without a copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelRows<'a> {
    Eight(&'a [u8]),
    Sixteen(&'a [u16]),
}

impl PixelRows<'_> {
    pub fn depth(&self) -> Depth {
        match self {
            Self::Eight(_) => Depth::Eight,
            Self::Sixteen(_) => Depth::Sixteen,
        }
    }

    /// Channel samples held, four per pixel.
    pub fn len(&self) -> usize {
        match self {
            Self::Eight(samples) => samples.len(),
            Self::Sixteen(samples) => samples.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl OutputSpec {
    /// Refuse, with `validation`, an output with an empty side or larger than its
    /// `stage_width` × `stage_height` stage on either side: an export is never enlarged.
    pub fn check(&self, stage_width: u32, stage_height: u32) -> Result<(), crate::Error> {
        if self.width == 0
            || self.height == 0
            || self.width > stage_width
            || self.height > stage_height
        {
            return Err(crate::Error::validation(format!(
                "a {}x{} output cannot be rendered from the {stage_width}x{stage_height} output \
                 stage: it is never larger than the stage, nor empty",
                self.width, self.height
            )));
        }
        Ok(())
    }
}

/// The whole of an export's output as the reference renderer renders it ([`reference_output`]):
/// `width` × `height` opaque RGBA pixels at the output's depth. A full-size 8-bit output is the
/// reference's own frame, shared rather than copied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputFrame {
    pub width: u32,
    pub height: u32,
    samples: FrameSamples,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FrameSamples {
    /// The reference's own 8-bit frame, as `Render::frame` answered it.
    Shared(std::sync::Arc<Vec<u8>>),
    Owned(Pixels),
}

impl OutputFrame {
    /// The bits per channel it holds.
    pub fn depth(&self) -> Depth {
        match &self.samples {
            FrameSamples::Shared(_) => Depth::Eight,
            FrameSamples::Owned(pixels) => pixels.depth(),
        }
    }

    /// Its rows, as an encoder reads them, without a copy.
    pub fn rows(&self) -> PixelRows<'_> {
        match &self.samples {
            FrameSamples::Shared(rgba) => PixelRows::Eight(rgba),
            FrameSamples::Owned(pixels) => pixels.rows(),
        }
    }

    /// Its pixels, owned: a full-size 8-bit frame is copied only while another holds it, as the
    /// prepared source does for a stack that changes nothing.
    pub fn into_pixels(self) -> Pixels {
        match self.samples {
            FrameSamples::Shared(rgba) => Pixels::Eight(
                std::sync::Arc::try_unwrap(rgba).unwrap_or_else(|rgba| (*rgba).clone()),
            ),
            FrameSamples::Owned(pixels) => pixels,
        }
    }

    /// The reference's 8-bit frame it shares, for a test that proves it was not copied.
    #[cfg(test)]
    pub(crate) fn shared(&self) -> Option<&std::sync::Arc<Vec<u8>>> {
        match &self.samples {
            FrameSamples::Shared(rgba) => Some(rgba),
            FrameSamples::Owned(_) => None,
        }
    }
}

/// `evaluation`'s output for an export, `output`, from the reference renderer under `cancel`: what
/// the export lane renders in place of `evaluation.exact(..).frame(..)`
/// (`docs/design/export-settings.md`, "Rendering").
///
/// - **Full size, 8 bits.** The exact frame itself, stamped with `snapshot_id`, byte for byte and
///   shared rather than copied.
/// - **Full size, 16 bits.** The frame's terminal values encoded once at 16 bits instead of 8
///   (`Render::wide_frame`), opaque.
/// - **Smaller.** Those 16-bit values resampled in linear light by the separable Lanczos-3 filter
///   (`render::export_output`), clamped and encoded once at the output's depth.
/// - **Sharpened.** Any of these, then sharpened in place on its codes ([`super::sharpen`]): a
///   full-size 8-bit frame is first taken from the render, copied only while another holds it.
///
/// An output with an empty side or past the stage is refused with `validation`; every frame is
/// held to the evaluated-frame limit at its depth, the 16-bit terminal at six bytes a pixel and the
/// output beside it. A cancellation answers `cancelled`, never a partial frame.
pub fn reference_output(
    evaluation: &crate::Evaluation,
    output: OutputSpec,
    snapshot_id: crate::SnapshotId,
    cancel: &crate::Cancel,
) -> Result<OutputFrame, crate::Error> {
    reference_output_phased(evaluation, output, snapshot_id, cancel, &mut || Ok(()))
}

/// [`reference_output`], calling `resampling` once the exact render is done and before a smaller
/// output is resampled from it or any output is sharpened, so an export reports the phase; its
/// error stops the export there.
pub(crate) fn reference_output_phased(
    evaluation: &crate::Evaluation,
    output: OutputSpec,
    snapshot_id: crate::SnapshotId,
    cancel: &crate::Cancel,
    resampling: &mut dyn FnMut() -> Result<(), crate::Error>,
) -> Result<OutputFrame, crate::Error> {
    render_output(
        evaluation.exact(cancel)?,
        output,
        snapshot_id,
        cancel,
        resampling,
    )
}

/// [`reference_output_phased`] of an exact `render` of the whole frame: the one place the
/// whole-frame reference's export output is made, which the banded reference's output of a paged
/// source is held to exactly (`render::bands_output`).
pub(crate) fn render_output(
    render: crate::Render<'_>,
    output: OutputSpec,
    snapshot_id: crate::SnapshotId,
    cancel: &crate::Cancel,
    resampling: &mut dyn FnMut() -> Result<(), crate::Error>,
) -> Result<OutputFrame, crate::Error> {
    let (width, height) = render.stage();
    output.check(width, height)?;
    let full = output.is_full(width, height);
    let frame = if full && output.depth == Depth::Eight {
        let frame = render.frame(snapshot_id)?;
        OutputFrame {
            width: frame.width,
            height: frame.height,
            samples: FrameSamples::Shared(frame.rgba),
        }
    } else {
        let terminal = render.wide_frame()?;
        drop(render);
        let pixels = if full {
            Pixels::Sixteen(terminal.into_rgba16(cancel)?)
        } else {
            resampling()?;
            crate::render::export_output::resample(&terminal, output, cancel)?
        };
        OutputFrame {
            width: output.width,
            height: output.height,
            samples: FrameSamples::Owned(pixels),
        }
    };
    let Some(sharpen) = output.sharpen else {
        return Ok(frame);
    };
    if full {
        resampling()?;
    }
    let (width, height) = (frame.width, frame.height);
    let mut pixels = frame.into_pixels();
    super::sharpen::sharpen(&mut pixels, (width, height), &sharpen.kernel(), cancel)?;
    Ok(OutputFrame {
        width,
        height,
        samples: FrameSamples::Owned(pixels),
    })
}
