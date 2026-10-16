//! A study's 16-bit output of the whole-frame reference renderer (`docs/design/lightroom-alignment.md`,
//! "The rig"): the same evaluation as [`Render::frame`], with only its terminal 8-bit quantization
//! replaced by a 16-bit sRGB encoding, so a measurement against another editor's 16-bit output
//! reads the reference before display quantization rather than promoted bytes.
//!
//! It is study output, not an export kind and not a display path: the editor never calls it, and no
//! tolerance is declared for it. Every earlier boundary keeps its ordinary width, so the byte
//! domain's intermediate 8-bit quantizations (the source's own bytes, a colour run read by a
//! resample) remain, and [`StudyFrame::narrow_boundaries`] names them.

use super::{
    Byte, Compiled, Evaluation, PixelDomain, Raster, Render, RenderContext, RenderOptions,
    RenderSource, RowScratch, check_source, linear::Linear,
};
use crate::{Error, modules::Region};
use rayon::prelude::*;
use std::borrow::Cow;

/// A whole output frame at 16 bits per channel.
#[derive(Clone, Debug)]
pub struct StudyFrame {
    pub width: u32,
    pub height: u32,
    /// Row-major RGB, each value a 16-bit sRGB code.
    pub rgb: Vec<u16>,
    /// Each segment before the last whose output the next segment reads at 8 bits, with whether
    /// that reader is a resample: the intermediate quantizations this frame keeps. Always empty
    /// for a RAW's linear source, whose boundaries are floating point.
    pub narrow_boundaries: Vec<(usize, bool)>,
}

impl Render<'_> {
    /// The whole frame through the reference renderer with a 16-bit sRGB terminal conversion in
    /// place of the 8-bit one, for a study. Every other boundary, and so every other byte, is the
    /// ordinary render's: where [`Self::frame`] writes code `k`, this writes a code within half an
    /// 8-bit step of `257 k`.
    pub fn study_frame16(&self) -> Result<StudyFrame, Error> {
        match self.source {
            RenderSource::Byte(image) => {
                check_source(image)?;
                study_frame(Byte(image), &self.compiled, &self.options, self.context)
            }
            RenderSource::Linear { image, settings } => study_frame(
                Linear::new(image, settings)?,
                &self.compiled,
                &self.options,
                self.context,
            ),
        }
    }
}

fn study_frame<D: PixelDomain>(
    domain: D,
    compiled: &Compiled,
    options: &RenderOptions,
    context: &RenderContext,
) -> Result<StudyFrame, Error> {
    let stage = compiled.stage();
    domain.check_output(stage.width, stage.height)?;
    // Bounded as an evaluated frame of its size is, then half as large again for 16 bits.
    let len = Raster::expected_len(stage.width, stage.height)? / 4 * 3;
    options.cancel.check()?;
    let evaluation = Evaluation::study(
        domain,
        Cow::Borrowed(compiled),
        options.tiling,
        &options.cancel,
        context,
    )?;
    let last = compiled.segments.len() - 1;
    let mut rgb = vec![0u16; len];
    let row_len = stage.width as usize * 3;
    rgb.par_chunks_mut(row_len.max(1))
        .enumerate()
        .try_for_each_init(
            || (Vec::new(), RowScratch::default()),
            |(pixels, scratch), (y, row)| -> Result<(), Error> {
                options.cancel.check()?;
                let region = Region {
                    x0: 0,
                    y0: y as u32,
                    width: stage.width,
                    height: 1,
                };
                evaluation.region_in(last, region, pixels, scratch)?;
                for (out, pixel) in row.chunks_exact_mut(3).zip(pixels.iter()) {
                    out.copy_from_slice(&D::terminal16(*pixel)?);
                }
                Ok(())
            },
        )?;
    Ok(StudyFrame {
        width: stage.width,
        height: stage.height,
        rgb,
        narrow_boundaries: evaluation.narrow_boundaries(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::{gradient, registry};
    use crate::{
        BASIC_EFFECT, CropPayload, Layer, LinearImage, RECIPE_FORMAT, Recipe, SnapshotId,
        render::{LinearSettings, render},
    };
    use luxforge_reference::srgb;
    use serde_json::json;

    fn recipe(layers: Vec<Layer>) -> Recipe {
        Recipe {
            format: RECIPE_FORMAT,
            layers,
            ..Recipe::default()
        }
    }

    fn exposure(ev: f64) -> Layer {
        Layer::new(BASIC_EFFECT, json!({"exposure": ev}))
    }

    /// The 16-bit code the reference's encoding gives a linear value.
    fn code16(linear: f64) -> u16 {
        (srgb::encode_clamped(linear) * 65535.0).round() as u16
    }

    /// With nothing to apply, every 16-bit code is its source byte's multiple of 257, and the frame
    /// keeps no intermediate quantization.
    #[test]
    fn without_layers_every_code_is_its_byte_widened() {
        let source = gradient(37, 5);
        let context = RenderContext::new();
        let study = render(
            &registry(),
            &source,
            &recipe(Vec::new()),
            RenderOptions::default(),
            &context,
        )
        .unwrap()
        .study_frame16()
        .unwrap();
        assert_eq!((study.width, study.height), (37, 5));
        for (pixel, byte) in study.rgb.chunks_exact(3).zip(source.rgba.chunks_exact(4)) {
            for channel in 0..3 {
                assert_eq!(pixel[channel], u16::from(byte[channel]) * 257);
            }
        }
        assert!(study.narrow_boundaries.is_empty());
    }

    /// A known gradient under a known gain: each code is the reference encoding of the decoded
    /// byte times `2^ev` at 16 bits, within the colour unit's `f32` precision, and lies within half
    /// an 8-bit step of the 8-bit frame's code, so the study adds precision and changes nothing
    /// else.
    #[test]
    fn a_gain_on_a_gradient_reaches_the_reference_16_bit_codes() {
        let source = gradient(64, 3);
        let context = RenderContext::new();
        let ev = 0.37;
        let compiled = render(
            &registry(),
            &source,
            &recipe(vec![exposure(ev)]),
            RenderOptions::default(),
            &context,
        )
        .unwrap();
        let study = compiled.study_frame16().unwrap();
        let frame = compiled.frame(SnapshotId::new()).unwrap();
        let mut between = 0;
        for ((pixel, byte), shown) in study
            .rgb
            .chunks_exact(3)
            .zip(source.rgba.chunks_exact(4))
            .zip(frame.rgba.chunks_exact(4))
        {
            for channel in 0..3 {
                let expected = code16(luxforge_reference::scene::shift(
                    srgb::decode(byte[channel]),
                    ev,
                ));
                assert!(
                    pixel[channel].abs_diff(expected) <= 2,
                    "{} against {expected}",
                    pixel[channel]
                );
                let widened = i32::from(shown[channel]) * 257;
                assert!((i32::from(pixel[channel]) - widened).abs() <= 129);
                between += usize::from(pixel[channel] % 257 != 0);
            }
        }
        assert!(between > 0, "no code lies between two 8-bit codes");
        assert!(study.narrow_boundaries.is_empty());
    }

    /// A RAW's linear values above 1 take the top code and values below it keep 16-bit precision:
    /// the study's sRGB TIFF-like encoding clips headroom as the display encoding does.
    #[test]
    fn raw_headroom_takes_the_top_code_and_precision_below_it_is_kept() {
        let values = [0.0, 0.003, 0.25, 0.9, 1.0, 1.5, 4.0];
        let planes: Vec<f32> = (0..3).flat_map(|_| values.map(|v| v as f32)).collect();
        let source = LinearImage::new(values.len() as u32, 1, planes).unwrap();
        let context = RenderContext::new();
        let study = render(
            &registry(),
            RenderSource::Linear {
                image: &source,
                settings: LinearSettings::default(),
            },
            &recipe(Vec::new()),
            RenderOptions::default(),
            &context,
        )
        .unwrap()
        .study_frame16()
        .unwrap();
        for (pixel, value) in study.rgb.chunks_exact(3).zip(values) {
            let expected = code16(value);
            assert!(
                pixel.iter().all(|code| code.abs_diff(expected) <= 1),
                "{value}: {pixel:?}"
            );
        }
        assert_eq!(study.rgb[3 * 5], 65535);
        assert!(study.narrow_boundaries.is_empty());
    }

    /// A colour run a crop's resample reads is an 8-bit boundary in the byte domain, which the study
    /// keeps and names.
    #[test]
    fn a_colour_run_read_by_a_resample_is_reported() {
        let source = gradient(40, 30);
        let context = RenderContext::new();
        let crop = CropPayload {
            angle: 3.0,
            x: 0.25,
            y: 0.25,
            width: 0.5,
            height: 0.5,
        };
        let study = render(
            &registry(),
            &source,
            &recipe(vec![exposure(0.5), Layer::crop(crop)]),
            RenderOptions::default(),
            &context,
        )
        .unwrap()
        .study_frame16()
        .unwrap();
        assert_eq!(study.narrow_boundaries, vec![(0, true)]);
    }
}
