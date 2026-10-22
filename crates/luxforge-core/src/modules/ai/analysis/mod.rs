//! Analysis (`docs/design/ai-editing.md`, "Analysis and the per-photo cache"): the three tasks that
//! prepare a photograph, `task.analyse` (the segmenter's encoder), `task.analyse-scene` (the
//! semantic model, the salient-subject model and the people detector) and `task.analyse-depth`
//! (the depth model), and what they share.
//!
//! Each task reads the repair stage's output through the host-planned reference read
//! ([`crate::capabilities::read`]), a band of whole rows at a time, and reduces it on the module
//! lane in one pass into two inputs: the **analysis input**, at most [`ANALYSIS_SIDE`] on its long
//! side in 8-bit sRGB, which the models see, and the **guide**, the stage's luminance at the
//! selection artifact's resolution (at most [`GUIDE_SIDE`] on its long side, so at most 16 MiB),
//! which a selection is refined against. Both are area averages in linear light, each output pixel
//! the mean of the stage pixels whose index maps into it, quantized to 8-bit sRGB.
//!
//! Everything a task computes goes into the owner's analysis cache
//! ([`crate::capabilities::analysis`]) under the stage identity the owner found when the task was
//! requested: the analysis input and the guide under this module's own kinds, and each model's
//! output under its resource and version. A task over an input the cache already holds answers
//! from it, reading and running nothing.
use super::module::AI_MODULE;
use crate::{
    Error, Region,
    capabilities::{
        analysis::AnalysisKey,
        context::ModuleContext,
        read::{MAX_MODULE_READ_PIXELS, StageIdentity},
    },
    colour::srgb,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) mod models;
mod scene;
#[cfg(test)]
mod tests;

pub(crate) use scene::{analyse_depth, analyse_scene};

/// The analysis input's long side at most, in pixels.
pub(crate) const ANALYSIS_SIDE: u32 = 1024;
/// The guide's long side at most: the selection artifact's.
pub(crate) const GUIDE_SIDE: u32 = 4096;
/// The analysis input's cache kind and format.
const INPUT_KIND: &str = "analysis-input";
/// The guide's cache kind and format.
const GUIDE_KIND: &str = "guide";
const FORMAT: &str = "1";

/// The analysis input: 8-bit sRGB, interleaved RGB, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Raster {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgb: Vec<u8>,
}

impl Raster {
    /// Its codes as `f32`, as a model's input takes them.
    pub(crate) fn codes(&self) -> Vec<f32> {
        self.rgb.iter().map(|&code| f32::from(code)).collect()
    }
}

/// The guide: the stage's luminance in 8-bit sRGB, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Guide {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) codes: Vec<u8>,
}

impl Guide {
    /// Its codes as `[0, 1]` values, as the guided upsampler takes them.
    pub(crate) fn values(&self) -> Vec<f32> {
        self.codes
            .iter()
            .map(|&code| f32::from(code) / 255.0)
            .collect()
    }
}

/// The size of a stage of `size` reduced so its long side is at most `side`: SAM's
/// `get_preprocess_shape`, each side rounding half up and at least one pixel; the stage's own size
/// when it is no larger.
pub(crate) fn reduced_size(size: (u32, u32), side: u32) -> (u32, u32) {
    if size.0.max(size.1) <= side {
        return size;
    }
    crate::modules::ai::segment::long_side_size(size, side)
}

/// The cache key of what this module computed as `kind` at `version` from the stage `identity`
/// names.
pub(crate) fn key(identity: &StageIdentity, kind: &str, version: &str) -> AnalysisKey {
    AnalysisKey {
        module: AI_MODULE.into(),
        asset: identity.asset.clone(),
        source: identity.source.clone(),
        prefix: identity.prefix.clone(),
        kind: kind.into(),
        version: version.into(),
    }
}

/// One output's accumulation: the sums of the current output row and how many stage pixels each
/// output column takes.
struct Target {
    height: u32,
    stage: (u32, u32),
    /// The output column of each stage column.
    columns: Vec<u32>,
    sums: Vec<[f64; 3]>,
    counts: Vec<u32>,
    row: Option<u32>,
    out: Vec<[f64; 3]>,
}

impl Target {
    fn new(stage: (u32, u32), size: (u32, u32)) -> Self {
        let (width, height) = size;
        Self {
            height,
            stage,
            columns: (0..stage.0)
                .map(|x| (u64::from(x) * u64::from(width) / u64::from(stage.0)) as u32)
                .collect(),
            sums: vec![[0.0; 3]; width as usize],
            counts: vec![0; width as usize],
            row: None,
            out: Vec::with_capacity(width as usize * height as usize),
        }
    }

    /// Add one stage row.
    fn add(&mut self, y: u32, pixels: &[[f32; 3]]) {
        let row = (u64::from(y) * u64::from(self.height) / u64::from(self.stage.1)) as u32;
        if self.row.is_some_and(|current| current != row) {
            self.flush();
        }
        self.row = Some(row);
        for (pixel, &column) in pixels.iter().zip(&self.columns) {
            let sum = &mut self.sums[column as usize];
            for (s, v) in sum.iter_mut().zip(pixel) {
                *s += f64::from(*v);
            }
            self.counts[column as usize] += 1;
        }
    }

    /// Close the current output row.
    fn flush(&mut self) {
        if self.row.take().is_none() {
            return;
        }
        for (sum, count) in self.sums.iter_mut().zip(&mut self.counts) {
            let n = f64::from((*count).max(1));
            self.out.push(sum.map(|s| s / n));
            *sum = [0.0; 3];
            *count = 0;
        }
    }

    fn finish(mut self) -> Vec<[f64; 3]> {
        self.flush();
        self.out
    }
}

/// Reduce a stage of `stage` size, read through `read` a band of whole rows of at most
/// [`MAX_MODULE_READ_PIXELS`] at a time, to the analysis input and the guide in one pass, calling
/// `checkpoint` between bands. Holds one band and one output row of each target besides the
/// outputs.
pub(crate) fn reduce(
    stage: (u32, u32),
    read: &mut dyn FnMut(Region) -> Result<Vec<[f32; 3]>, Error>,
    checkpoint: &dyn Fn() -> Result<(), Error>,
) -> Result<(Raster, Guide), Error> {
    let (input_size, guide_size) = (
        reduced_size(stage, ANALYSIS_SIDE),
        reduced_size(stage, GUIDE_SIDE),
    );
    let mut input = Target::new(stage, input_size);
    let mut guide = Target::new(stage, guide_size);
    let band = (MAX_MODULE_READ_PIXELS / u64::from(stage.0.max(1))).max(1) as u32;
    let mut y = 0;
    while y < stage.1 {
        checkpoint()?;
        let height = band.min(stage.1 - y);
        let region = Region {
            x0: 0,
            y0: y,
            width: stage.0,
            height,
        };
        let pixels = read(region)?;
        if pixels.len() as u64 != region.pixels() {
            return Err(Error::internal(format!(
                "a stage read of {} pixels answered {}",
                region.pixels(),
                pixels.len()
            )));
        }
        for (row, line) in pixels.chunks_exact(stage.0 as usize).enumerate() {
            input.add(y + row as u32, line);
            guide.add(y + row as u32, line);
        }
        y += height;
    }
    checkpoint()?;
    let quantizer = srgb::quantizer();
    let rgb = input
        .finish()
        .iter()
        .flat_map(|pixel| pixel.map(|channel| quantizer.channel(channel)))
        .collect();
    let codes = guide
        .finish()
        .iter()
        .map(|[r, g, b]| quantizer.channel(0.2126 * r + 0.7152 * g + 0.0722 * b))
        .collect();
    Ok((
        Raster {
            width: input_size.0,
            height: input_size.1,
            rgb,
        },
        Guide {
            width: guide_size.0,
            height: guide_size.1,
            codes,
        },
    ))
}

/// The analysis input and the guide of the context's stage: from the cache, or read and reduced
/// now and stored. Answers whether the stage was read.
pub(crate) fn prepared(context: &ModuleContext) -> Result<(Arc<Raster>, Arc<Guide>, bool), Error> {
    let identity = context.identity()?;
    let cache = context.analysis()?;
    let (input_key, guide_key) = (
        key(identity, INPUT_KIND, FORMAT),
        key(identity, GUIDE_KIND, FORMAT),
    );
    if let (Some(input), Some(guide)) = (
        cache.get::<Raster>(&input_key),
        cache.get::<Guide>(&guide_key),
    ) {
        return Ok((input, guide, false));
    }
    let mut reader = context.stage_reader()?;
    let stage = reader.stage();
    let (input, guide) = reduce(
        (stage.width, stage.height),
        &mut |region| reader.read(region).map(|read| read.linear),
        &|| context.checkpoint(),
    )?;
    drop(reader);
    let (input, guide) = (Arc::new(input), Arc::new(guide));
    cache.insert(input_key, input.clone(), input.rgb.len() as u64)?;
    cache.insert(guide_key, guide.clone(), guide.codes.len() as u64)?;
    Ok((input, guide, true))
}

/// The guide of the context's stage from the cache, without reading; `None` when it is not held.
pub(crate) fn cached_guide(context: &ModuleContext) -> Result<Option<Arc<Guide>>, Error> {
    let identity = context.identity()?;
    Ok(context
        .analysis()?
        .get::<Guide>(&key(identity, GUIDE_KIND, FORMAT)))
}

/// What every analysis result names: the models that computed it and the inputs' sizes.
pub(crate) fn models_value(models: &[&models::SessionModel]) -> Value {
    json!(
        models
            .iter()
            .map(|model| json!({"id": model.file.resource, "version": model.file.version}))
            .collect::<Vec<_>>()
    )
}
