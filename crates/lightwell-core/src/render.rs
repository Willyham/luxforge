use crate::{
    Error, ErrorKind, Layer, Recipe, SnapshotId, SourceImage,
    modules::{ExactGeometry, ModuleRegistry, Processing, Resample, Stage},
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, sync::Arc, sync::LazyLock};

const PARALLEL_RENDER_PIXELS: u64 = 1_000_000;

/// The sRGB transfer function over the 256 8-bit channel values: interpolation weights are applied
/// in linear light, so every resampled channel is decoded through this table first.
static SRGB_TO_LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
    let mut table = [0.0; 256];
    for (value, slot) in table.iter_mut().enumerate() {
        let encoded = value as f64 / 255.0;
        *slot = if encoded <= 0.040_45 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        } as f32;
    }
    table
});

/// The sRGB transfer function applied forwards, rounded to the nearest 8-bit value.
fn linear_to_srgb(linear: f64) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

/// One bilinear sample of a frame in linear light, with indices clamped to the frame's edge.
///
/// `fetch` reads one pixel of that frame; the rasterizing path reads a buffer and the point-query
/// path evaluates the previous segment recursively, so both produce identical bytes.
#[inline]
fn bilinear(
    u: f64,
    v: f64,
    width: u32,
    height: u32,
    fetch: impl Fn(u32, u32) -> [u8; 4],
) -> [u8; 4] {
    let table = &*SRGB_TO_LINEAR;
    // The mapped coordinate is a pixel center, so index space starts half a pixel earlier.
    let x = u - 0.5;
    let y = v - 0.5;
    let left = x.floor();
    let top = y.floor();
    let weight_x = x - left;
    let weight_y = y - top;
    let index = |value: f64, limit: u32| -> u32 {
        let last = limit.saturating_sub(1);
        if value <= 0.0 {
            0
        } else if value >= f64::from(last) {
            last
        } else {
            value as u32
        }
    };
    let (left_x, right_x) = (index(left, width), index(left + 1.0, width));
    let (top_y, bottom_y) = (index(top, height), index(top + 1.0, height));
    let corners = [
        (fetch(left_x, top_y), (1.0 - weight_x) * (1.0 - weight_y)),
        (fetch(right_x, top_y), weight_x * (1.0 - weight_y)),
        (fetch(left_x, bottom_y), (1.0 - weight_x) * weight_y),
        (fetch(right_x, bottom_y), weight_x * weight_y),
    ];
    let mut pixel = [0; 4];
    for (channel, slot) in pixel.iter_mut().enumerate().take(3) {
        let linear: f64 = corners
            .iter()
            .map(|(corner, weight)| weight * f64::from(table[corner[channel] as usize]))
            .sum();
        *slot = linear_to_srgb(linear);
    }
    // Alpha has no transfer function; it blends linearly.
    let alpha: f64 = corners
        .iter()
        .map(|(corner, weight)| weight * f64::from(corner[3]))
        .sum();
    pixel[3] = alpha.round().clamp(0.0, 255.0) as u8;
    pixel
}

/// The input pixel one continuous input coordinate falls in, clamped to the frame exactly as the
/// sampler clamps its own indices. A resample maps an output pixel center between input pixels, and
/// this is the corner the bilinear blend weights most, so it is the pixel that output pixel shows.
#[inline]
fn nearest_index(value: f64, limit: u32) -> u32 {
    let last = limit.saturating_sub(1);
    let index = value.floor();
    if index <= 0.0 {
        0
    } else if index >= f64::from(last) {
        last
    } else {
        index as u32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
    pub source_fingerprint: String,
    pub snapshot_id: SnapshotId,
}

impl Raster {
    fn expected_len(width: u32, height: u32) -> Result<usize, Error> {
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| Error::new(ErrorKind::ResourceLimit, "image dimensions overflow"))?;
        if pixels > 512 * 1024 * 1024 {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                "evaluated image exceeds 512 MiB",
            ));
        }
        usize::try_from(pixels).map_err(|_| {
            Error::new(
                ErrorKind::ResourceLimit,
                "image allocation is not addressable",
            )
        })
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = ((u64::from(y) * u64::from(self.width) + u64::from(x)) * 4) as usize;
        self.rgba
            .get(offset..offset + 4)
            .map(|p| [p[0], p[1], p[2], p[3]])
    }
}

/// Composition and mapping of exact geometry belong to the host; modules only declare one step.
impl ExactGeometry {
    pub(crate) fn identity(width: u32, height: u32) -> Self {
        Self {
            a: 1,
            b: 0,
            c: 0,
            d: 1,
            tx: 0,
            ty: 0,
            output_width: width,
            output_height: height,
        }
    }

    /// Compose `self` followed by `next`.
    pub(crate) fn then(self, next: Self) -> Self {
        Self {
            a: next.a * self.a + next.b * self.c,
            b: next.a * self.b + next.b * self.d,
            c: next.c * self.a + next.d * self.c,
            d: next.c * self.b + next.d * self.d,
            tx: next.a * self.tx + next.b * self.ty + next.tx,
            ty: next.c * self.tx + next.d * self.ty + next.ty,
            output_width: next.output_width,
            output_height: next.output_height,
        }
    }

    /// A pure translation that copies one rectangle of its input stage: what a crop with no
    /// straightening declares. Exact, and composable with neighbouring transforms into one pass.
    pub fn crop(x: i64, y: i64, width: u32, height: u32) -> Self {
        Self {
            a: 1,
            b: 0,
            c: 0,
            d: 1,
            tx: -x,
            ty: -y,
            output_width: width,
            output_height: height,
        }
    }

    /// Where an input-stage point lands, or `None` when it falls outside the output stage: a point
    /// replacement cropped away later is simply not visible.
    fn map(self, x: u32, y: u32) -> Option<(u32, u32)> {
        let out_x = self.a * i64::from(x) + self.b * i64::from(y) + self.tx;
        let out_y = self.c * i64::from(x) + self.d * i64::from(y) + self.ty;
        let inside = out_x >= 0
            && out_x < i64::from(self.output_width)
            && out_y >= 0
            && out_y < i64::from(self.output_height);
        inside.then_some((out_x as u32, out_y as u32))
    }

    /// Whether every output pixel of this mapping reads a pixel that exists in the given input
    /// stage. A module declares its own exact step, so the host checks it before any pass reads a
    /// frame through it: the image of the output rectangle is a rectangle, so its corners decide.
    pub(crate) fn reads_inside(self, input_width: u32, input_height: u32) -> bool {
        if self.output_width == 0 || self.output_height == 0 {
            return false;
        }
        let far_x = i64::from(self.output_width) - 1;
        let far_y = i64::from(self.output_height) - 1;
        [(0, 0), (far_x, 0), (0, far_y), (far_x, far_y)]
            .into_iter()
            .all(|(x, y)| {
                let translated_x = x - self.tx;
                let translated_y = y - self.ty;
                let input_x = self.a * translated_x + self.c * translated_y;
                let input_y = self.b * translated_x + self.d * translated_y;
                (0..i64::from(input_width)).contains(&input_x)
                    && (0..i64::from(input_height)).contains(&input_y)
            })
    }

    fn unmap(self, x: u32, y: u32) -> (u32, u32) {
        let translated_x = i64::from(x) - self.tx;
        let translated_y = i64::from(y) - self.ty;
        let input_x = self.a * translated_x + self.c * translated_y;
        let input_y = self.b * translated_x + self.d * translated_y;
        debug_assert!(input_x >= 0 && input_y >= 0);
        (input_x as u32, input_y as u32)
    }

    fn is_identity(self, input_width: u32, input_height: u32) -> bool {
        self.output_width == input_width
            && self.output_height == input_height
            && (self.a, self.b, self.c, self.d, self.tx, self.ty) == (1, 0, 0, 1, 0, 0)
    }
}

/// Mapping the output of one resample back into its input frame is the host's too.
impl Resample {
    /// The continuous input coordinate one output pixel center samples.
    fn input_at(self, x: u32, y: u32) -> (f64, f64) {
        let [m0, m1, m2, m3, m4, m5] = self.inverse;
        let center_x = f64::from(x) + 0.5;
        let center_y = f64::from(y) + 0.5;
        (
            m0 * center_x + m1 * center_y + m2,
            m3 * center_x + m4 * center_y + m5,
        )
    }
}

/// One exact pass over one input frame: every output pixel copies exactly one input pixel.
fn copy_transformed(
    input: &[u8],
    input_width: u32,
    geometry: ExactGeometry,
) -> Result<Vec<u8>, Error> {
    let width = geometry.output_width;
    let height = geometry.output_height;
    let mut output = vec![0; Raster::expected_len(width, height)?];
    let row_bytes = usize::try_from(u64::from(width) * 4)
        .map_err(|_| Error::new(ErrorKind::ResourceLimit, "image row is not addressable"))?;
    let copy_row = |out_y: usize, row: &mut [u8]| {
        for out_x in 0..width {
            let (input_x, input_y) = geometry.unmap(out_x, out_y as u32);
            let from =
                ((u64::from(input_y) * u64::from(input_width) + u64::from(input_x)) * 4) as usize;
            let to = out_x as usize * 4;
            row[to..to + 4].copy_from_slice(&input[from..from + 4]);
        }
    };
    if u64::from(width) * u64::from(height) >= PARALLEL_RENDER_PIXELS {
        output
            .par_chunks_exact_mut(row_bytes)
            .enumerate()
            .for_each(|(out_y, row)| copy_row(out_y, row));
    } else {
        output
            .chunks_exact_mut(row_bytes)
            .enumerate()
            .for_each(|(out_y, row)| copy_row(out_y, row));
    }
    Ok(output)
}

/// One interpolating pass: the resample reads the frame it was given and writes the next one.
fn resample_frame(
    input: &[u8],
    input_width: u32,
    input_height: u32,
    resample: Resample,
) -> Result<Vec<u8>, Error> {
    let width = resample.output_width;
    let height = resample.output_height;
    let mut output = vec![0; Raster::expected_len(width, height)?];
    let row_bytes = usize::try_from(u64::from(width) * 4)
        .map_err(|_| Error::new(ErrorKind::ResourceLimit, "image row is not addressable"))?;
    let fetch = |x: u32, y: u32| -> [u8; 4] {
        let offset = ((u64::from(y) * u64::from(input_width) + u64::from(x)) * 4) as usize;
        let pixel = &input[offset..offset + 4];
        [pixel[0], pixel[1], pixel[2], pixel[3]]
    };
    let sample_row = |out_y: usize, row: &mut [u8]| {
        for out_x in 0..width {
            let (u, v) = resample.input_at(out_x, out_y as u32);
            let pixel = bilinear(u, v, input_width, input_height, fetch);
            let to = out_x as usize * 4;
            row[to..to + 4].copy_from_slice(&pixel);
        }
    };
    if u64::from(width) * u64::from(height) >= PARALLEL_RENDER_PIXELS {
        output
            .par_chunks_exact_mut(row_bytes)
            .enumerate()
            .for_each(|(out_y, row)| sample_row(out_y, row));
    } else {
        output
            .chunks_exact_mut(row_bytes)
            .enumerate()
            .for_each(|(out_y, row)| sample_row(out_y, row));
    }
    Ok(output)
}

/// One rasterizing pass: the exact operations that share an input frame, their composed geometry and
/// the stage they produce. `entry` is the resample that produces this segment's input frame, so
/// consecutive segments are separated by exactly one resample and the first segment reads the source.
pub(crate) struct Segment {
    pub(crate) entry: Option<Resample>,
    pub(crate) operations: Vec<Processing>,
    pub(crate) geometry: ExactGeometry,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) has_pixels: bool,
}

impl Segment {
    pub(crate) fn new(entry: Option<Resample>, width: u32, height: u32) -> Self {
        Self {
            entry,
            operations: Vec::new(),
            geometry: ExactGeometry::identity(width, height),
            width,
            height,
            has_pixels: false,
        }
    }
}

/// One recipe compiled by the registry: the ordered rasterizing passes and the resamples between
/// them. A recipe without a resample is one segment, which is the M1 and M2 behavior unchanged.
pub(crate) struct Compiled {
    pub(crate) segments: Vec<Segment>,
}

impl Compiled {
    fn last(&self) -> &Segment {
        self.segments
            .last()
            .expect("a compiled recipe always has one segment")
    }

    pub(crate) fn stage(&self) -> Stage {
        Stage {
            width: self.last().width,
            height: self.last().height,
        }
    }
}

/// Where one segment-output pixel comes from: the input-frame pixel it reads and the replacement
/// that wins there.
struct Resolved {
    rgb: Option<[u8; 3]>,
    input_x: u32,
    input_y: u32,
}

impl Segment {
    fn resolve(&self, x: u32, y: u32) -> Option<Resolved> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let (input_x, input_y) = self.geometry.unmap(x, y);
        let mut suffix = ExactGeometry::identity(self.width, self.height);
        for operation in self.operations.iter().rev() {
            match operation {
                Processing::ExactGeometry(step) => suffix = step.then(suffix),
                Processing::PointReplace {
                    x: pixel_x,
                    y: pixel_y,
                    rgb,
                } if suffix.map(*pixel_x, *pixel_y) == Some((x, y)) => {
                    return Some(Resolved {
                        rgb: Some(*rgb),
                        input_x,
                        input_y,
                    });
                }
                // A point replacement mapping outside this stage was cropped away.
                Processing::PointReplace { .. } => {}
                // A resample is the next segment's entry, never one of its operations.
                Processing::Resample(_) => {}
            }
        }
        Some(Resolved {
            rgb: None,
            input_x,
            input_y,
        })
    }
}

/// Apply one segment's point replacements to its rasterized frame. Later replacements win, so the
/// list is walked backwards and the first hit on a coordinate keeps it.
fn replace_points(pixels: &mut [u8], segment: &Segment) {
    if !segment.has_pixels {
        return;
    }
    let mut suffix = ExactGeometry::identity(segment.width, segment.height);
    let mut replaced = HashSet::new();
    for operation in segment.operations.iter().rev() {
        match operation {
            Processing::ExactGeometry(step) => suffix = step.then(suffix),
            Processing::PointReplace {
                x: pixel_x,
                y: pixel_y,
                rgb,
            } => {
                if let Some((x, y)) = suffix.map(*pixel_x, *pixel_y)
                    && replaced.insert((x, y))
                {
                    let offset =
                        ((u64::from(y) * u64::from(segment.width) + u64::from(x)) * 4) as usize;
                    pixels[offset..offset + 3].copy_from_slice(rgb);
                }
            }
            Processing::Resample(_) => {}
        }
    }
}

fn check_source(source: &SourceImage) -> Result<(), Error> {
    if source.rgba.len() != Raster::expected_len(source.width, source.height)? {
        return Err(Error::new(
            ErrorKind::Validation,
            "source pixel buffer has the wrong length",
        ));
    }
    Ok(())
}

fn source_pixel(source: &SourceImage, x: u32, y: u32) -> [u8; 4] {
    let offset = ((u64::from(y) * u64::from(source.width) + u64::from(x)) * 4) as usize;
    let pixel = &source.rgba[offset..offset + 4];
    [pixel[0], pixel[1], pixel[2], pixel[3]]
}

/// One evaluated pixel of a recipe's output stage, with that stage's dimensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub width: u32,
    pub height: u32,
    /// `None` when the coordinate lies outside the output stage.
    pub rgba: Option<[u8; 4]>,
}

/// One located pixel of a recipe's content stage: the source after EXIF orientation, which is the
/// stage the first layer receives and the stage a pixel-stage edit addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPoint {
    pub content_x: u32,
    pub content_y: u32,
    /// The content stage's dimensions.
    pub width: u32,
    pub height: u32,
}

/// One compiled recipe bound to its source: the stage it produces and point queries that never
/// allocate a frame. Compiling once serves any number of sampled pixels.
pub(crate) struct Evaluation<'a> {
    source: &'a SourceImage,
    compiled: Compiled,
}

impl<'a> Evaluation<'a> {
    pub(crate) fn new(
        registry: &ModuleRegistry,
        source: &'a SourceImage,
        recipe: &Recipe,
    ) -> Result<Self, Error> {
        check_source(source)?;
        Ok(Self {
            source,
            compiled: registry.compile(source.width, source.height, recipe)?,
        })
    }

    /// One evaluation over an ordered prefix of a stack, for planning a layer against the stage it
    /// will be inserted at. The recipe format is the caller's to check; compiling the prefix costs
    /// `O(layers)` and allocates no frame, so sampling that stage still rasterizes nothing.
    pub(crate) fn over_layers(
        registry: &ModuleRegistry,
        source: &'a SourceImage,
        layers: &[Layer],
    ) -> Result<Self, Error> {
        check_source(source)?;
        Ok(Self {
            source,
            compiled: registry.compile_layers(source.width, source.height, layers)?,
        })
    }

    pub(crate) fn stage(&self) -> Stage {
        self.compiled.stage()
    }

    /// `None` when the coordinate lies outside the output stage.
    pub(crate) fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        self.pixel_in(self.compiled.segments.len() - 1, x, y)
    }

    /// One pixel of one segment's output stage. A resample is evaluated recursively as the bilinear
    /// blend of four pixels of the previous segment, so a point query costs `O(layers · 4^resamples)`
    /// and never allocates a frame; a stack holds at most one crop layer.
    fn pixel_in(&self, index: usize, x: u32, y: u32) -> Option<[u8; 4]> {
        let segment = &self.compiled.segments[index];
        let resolved = segment.resolve(x, y)?;
        let mut rgba = match segment.entry {
            None => source_pixel(self.source, resolved.input_x, resolved.input_y),
            Some(resample) => {
                let previous = &self.compiled.segments[index - 1];
                let (u, v) = resample.input_at(resolved.input_x, resolved.input_y);
                bilinear(u, v, previous.width, previous.height, |x, y| {
                    self.pixel_in(index - 1, x, y)
                        .expect("clamped indices stay inside the previous stage")
                })
            }
        };
        if let Some(rgb) = resolved.rgb {
            rgba[..3].copy_from_slice(&rgb);
        }
        Some(rgba)
    }

    /// The content-stage pixel one output-stage pixel shows. `None` when the coordinate lies outside
    /// the output stage.
    pub(crate) fn locate(&self, x: u32, y: u32) -> Option<(u32, u32)> {
        self.locate_in(self.compiled.segments.len() - 1, x, y)
    }

    /// Walk one segment backwards, the same walk `pixel_in` makes to fetch a color: the composed
    /// exact geometry unmaps to the segment's input frame by its integer inverse, and a resample
    /// takes the nearest pixel of the previous stage to the input coordinate its output pixel center
    /// samples. Cost is linear in the segment count and no frame is allocated.
    fn locate_in(&self, index: usize, x: u32, y: u32) -> Option<(u32, u32)> {
        let segment = &self.compiled.segments[index];
        if x >= segment.width || y >= segment.height {
            return None;
        }
        let (input_x, input_y) = segment.geometry.unmap(x, y);
        let Some(resample) = segment.entry else {
            // The first segment reads the source, which is the content stage.
            return Some((input_x, input_y));
        };
        let previous = &self.compiled.segments[index - 1];
        let (u, v) = resample.input_at(input_x, input_y);
        self.locate_in(
            index - 1,
            nearest_index(u, previous.width),
            nearest_index(v, previous.height),
        )
    }
}

/// Evaluate one output pixel without rasterizing; cost is linear in the layer count and, for a
/// stack with a resample, in the four samples each resample blends.
pub fn sample(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
    x: u32,
    y: u32,
) -> Result<Sample, Error> {
    let evaluation = Evaluation::new(registry, source, recipe)?;
    let stage = evaluation.stage();
    Ok(Sample {
        width: stage.width,
        height: stage.height,
        rgba: evaluation.pixel(x, y),
    })
}

/// Map one pixel of a recipe's output stage back to the content-stage pixel it shows: the source
/// after EXIF orientation, the stage the first layer receives. A point outside the output stage is a
/// validation error naming that stage. Cost is linear in the layer count and no frame is allocated,
/// so the canvas pick and the API query share one implementation.
pub fn locate(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
    x: u32,
    y: u32,
) -> Result<ContentPoint, Error> {
    let evaluation = Evaluation::new(registry, source, recipe)?;
    let stage = evaluation.stage();
    let (content_x, content_y) = evaluation.locate(x, y).ok_or_else(|| {
        Error::new(
            ErrorKind::Validation,
            format!(
                "point ({x}, {y}) is outside the {}x{} rendered image",
                stage.width, stage.height
            ),
        )
    })?;
    Ok(ContentPoint {
        content_x,
        content_y,
        width: source.width,
        height: source.height,
    })
}

pub fn render(
    registry: &ModuleRegistry,
    source: &SourceImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
) -> Result<Raster, Error> {
    check_source(source)?;
    let compiled = registry.compile(source.width, source.height, recipe)?;
    let first = &compiled.segments[0];
    let mut width = first.width;
    let mut height = first.height;
    // An identity pass with nothing to write shares the source allocation instead of copying it.
    let mut frame = if first.geometry.is_identity(source.width, source.height) {
        first.has_pixels.then(|| source.rgba.as_ref().to_vec())
    } else {
        Some(copy_transformed(
            source.rgba.as_ref(),
            source.width,
            first.geometry,
        )?)
    };
    if let Some(pixels) = frame.as_mut() {
        replace_points(pixels, first);
    }

    for segment in &compiled.segments[1..] {
        let resample = segment
            .entry
            .expect("every segment after the first enters through a resample");
        let previous = frame.take();
        let input = previous.as_deref().unwrap_or(source.rgba.as_ref());
        let mut next = resample_frame(input, width, height, resample)?;
        // The frame the resample read is released before the next pass, so two frames is the peak.
        drop(previous);
        width = resample.output_width;
        height = resample.output_height;
        if !segment.geometry.is_identity(width, height) {
            let transformed = copy_transformed(&next, width, segment.geometry)?;
            next = transformed;
            width = segment.width;
            height = segment.height;
        }
        replace_points(&mut next, segment);
        frame = Some(next);
    }

    Ok(Raster {
        width,
        height,
        rgba: frame.map_or_else(|| source.rgba.clone(), Into::into),
        source_fingerprint: source.fingerprint.clone(),
        snapshot_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetId, EFFECT_FORMAT, Layer, LayerId, ORIENTATION_EFFECT, Orientation, PIXEL_EFFECT,
        PixelReplace, Recipe, Snapshot, Transform,
        modules::{
            ActionInput, ActionPlan, Availability, BoxRect, CropPayload, CropStage,
            EffectDescriptor, EffectStage, ModuleDescriptor, StageContext, ToolModule,
        },
    };
    use serde_json::{Map, Value, json};

    fn registry() -> ModuleRegistry {
        ModuleRegistry::builtin()
    }
    fn source(width: u32, height: u32) -> SourceImage {
        let mut rgba = Vec::new();
        for i in 0..width * height {
            rgba.extend([i as u8, (i + 20) as u8, (i + 40) as u8, 255]);
        }
        SourceImage {
            width,
            height,
            rgba: rgba.into(),
            fingerprint: "sha256:test".into(),
            orientation: 1,
        }
    }
    fn red(raster: &Raster) -> Vec<u8> {
        raster.rgba.chunks_exact(4).map(|p| p[0]).collect()
    }
    /// One single-action orientation layer: what a transform commits when the stack does not end
    /// in an orientation layer. A sequence of these is the stepwise form every proof below uses.
    fn turn(transform: Transform) -> Layer {
        Layer::orientation(Orientation::of(transform))
    }

    /// The elementary steps one orientation payload means: the mirror, then the quarter turns. The
    /// stepwise references apply these one at a time, independently of the composed mapping.
    fn steps(payload: &Value) -> Vec<Transform> {
        let orientation: Orientation = serde_json::from_value(payload.clone()).unwrap();
        let mut steps = Vec::new();
        if orientation.mirror {
            steps.push(Transform::MirrorHorizontal);
        }
        for _ in 0..orientation.turns {
            steps.push(Transform::RotateRight);
        }
        steps
    }

    fn rendered(source: &SourceImage, layers: Vec<Layer>) -> Raster {
        render(
            &registry(),
            source,
            SnapshotId::new(),
            &Recipe { format: 1, layers },
        )
        .unwrap()
    }

    fn reference(source: &SourceImage, layers: &[Layer]) -> (u32, u32, Vec<u8>) {
        let mut width = source.width;
        let mut height = source.height;
        let mut rgba = source.rgba.as_ref().to_vec();
        for layer in layers {
            match layer.effect_id.as_str() {
                PIXEL_EFFECT => {
                    let pixel: PixelReplace =
                        serde_json::from_value(layer.payload.clone()).unwrap();
                    let offset = ((pixel.y * width + pixel.x) * 4) as usize;
                    rgba[offset..offset + 3].copy_from_slice(&pixel.rgb);
                }
                ORIENTATION_EFFECT => {
                    for transform in steps(&layer.payload) {
                        let (next_width, next_height) = match transform {
                            Transform::RotateLeft | Transform::RotateRight => (height, width),
                            Transform::MirrorHorizontal | Transform::FlipVertical => {
                                (width, height)
                            }
                        };
                        let mut next = vec![0; (next_width * next_height * 4) as usize];
                        for y in 0..height {
                            for x in 0..width {
                                let (next_x, next_y) = match transform {
                                    Transform::RotateRight => (height - 1 - y, x),
                                    Transform::RotateLeft => (y, width - 1 - x),
                                    Transform::MirrorHorizontal => (width - 1 - x, y),
                                    Transform::FlipVertical => (x, height - 1 - y),
                                };
                                let from = ((y * width + x) * 4) as usize;
                                let to = ((next_y * next_width + next_x) * 4) as usize;
                                next[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
                            }
                        }
                        width = next_width;
                        height = next_height;
                        rgba = next;
                    }
                }
                TEST_CROP_EFFECT => {
                    let crop: CropPayload = serde_json::from_value(layer.payload.clone()).unwrap();
                    assert_eq!(crop.angle, 0.0, "the stepwise reference never straightens");
                    let (x, y) = (
                        (crop.x * f64::from(width)).round() as u32,
                        (crop.y * f64::from(height)).round() as u32,
                    );
                    let next_width = (crop.width * f64::from(width)).round().max(1.0) as u32;
                    let next_height = (crop.height * f64::from(height)).round().max(1.0) as u32;
                    let mut next = vec![0; (next_width * next_height * 4) as usize];
                    for row in 0..next_height {
                        for column in 0..next_width {
                            let from = (((row + y) * width + column + x) * 4) as usize;
                            let to = ((row * next_width + column) * 4) as usize;
                            next[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
                        }
                    }
                    width = next_width;
                    height = next_height;
                    rgba = next;
                }
                other => panic!("unexpected test effect {other}"),
            }
        }
        (width, height, rgba)
    }

    /// Where one pixel of a stage lands after the exact layers that follow it, evaluated one layer
    /// at a time and independently of the renderer: the direction `locate` walks backwards. `None`
    /// when a crop discards it. Pixel layers move nothing, so they are skipped.
    fn forward(width: u32, height: u32, layers: &[Layer], x: u32, y: u32) -> Option<(u32, u32)> {
        let (mut width, mut height, mut x, mut y) = (width, height, x, y);
        for layer in layers {
            match layer.effect_id.as_str() {
                PIXEL_EFFECT => {}
                ORIENTATION_EFFECT => {
                    for transform in steps(&layer.payload) {
                        (x, y) = match transform {
                            Transform::RotateRight => (height - 1 - y, x),
                            Transform::RotateLeft => (y, width - 1 - x),
                            Transform::MirrorHorizontal => (width - 1 - x, y),
                            Transform::FlipVertical => (x, height - 1 - y),
                        };
                        (width, height) = match transform {
                            Transform::RotateLeft | Transform::RotateRight => (height, width),
                            Transform::MirrorHorizontal | Transform::FlipVertical => {
                                (width, height)
                            }
                        };
                    }
                }
                TEST_CROP_EFFECT => {
                    let crop: CropPayload = serde_json::from_value(layer.payload.clone()).unwrap();
                    assert_eq!(crop.angle, 0.0, "the stepwise reference never straightens");
                    let (origin_x, origin_y) = (
                        (crop.x * f64::from(width)).round() as u32,
                        (crop.y * f64::from(height)).round() as u32,
                    );
                    width = (crop.width * f64::from(width)).round().max(1.0) as u32;
                    height = (crop.height * f64::from(height)).round().max(1.0) as u32;
                    if x < origin_x
                        || y < origin_y
                        || x >= origin_x + width
                        || y >= origin_y + height
                    {
                        return None;
                    }
                    (x, y) = (x - origin_x, y - origin_y);
                }
                other => panic!("unexpected test effect {other}"),
            }
        }
        Some((x, y))
    }

    /// The pixel index a continuous index-space position is nearest to, clamped to the frame.
    fn nearest(position: f64, limit: u32) -> u32 {
        position.round().clamp(0.0, f64::from(limit - 1)) as u32
    }

    /// A test-only module that compiles crop payloads and plain scaling into the host's primitives.
    /// The real crop module is a separate deliverable; this one exists so the host's resample
    /// segmentation can be tested without it.
    struct GeometryTestModule(ModuleDescriptor);

    const TEST_CROP_EFFECT: &str = "test.crop";
    const TEST_SCALE_EFFECT: &str = "test.scale";
    const TEST_OFFSET_EFFECT: &str = "test.offset";

    impl GeometryTestModule {
        fn shared() -> Arc<dyn ToolModule> {
            Arc::new(Self(ModuleDescriptor {
                id: "test.geometry".into(),
                title: "Test geometry".into(),
                hint: None,
                effects: [TEST_CROP_EFFECT, TEST_SCALE_EFFECT, TEST_OFFSET_EFFECT]
                    .into_iter()
                    .map(|id| EffectDescriptor {
                        id: id.into(),
                        format: EFFECT_FORMAT,
                        stage: EffectStage::Geometry,
                    })
                    .collect(),
                actions: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                availability: Availability::Available,
            }))
        }
    }

    impl ToolModule for GeometryTestModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(&self, _: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Err(Error::new(ErrorKind::Internal, "no actions"))
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::NoOp)
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, effect_id: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok(format!("test geometry {effect_id}"))
        }
        fn compile(
            &self,
            effect_id: &str,
            _: u32,
            payload: &Value,
            stage: Stage,
        ) -> Result<Processing, Error> {
            if effect_id == TEST_OFFSET_EFFECT {
                // A raw translation with a smaller output, including mappings the host must reject.
                return Ok(Processing::ExactGeometry(ExactGeometry::crop(
                    payload["x"].as_i64().expect("test offset"),
                    payload["y"].as_i64().expect("test offset"),
                    payload["width"].as_u64().expect("test offset") as u32,
                    payload["height"].as_u64().expect("test offset") as u32,
                )));
            }
            if effect_id == TEST_SCALE_EFFECT {
                let scale = payload["scale"].as_f64().expect("test scale");
                return Ok(Processing::Resample(Resample {
                    inverse: [1.0 / scale, 0.0, 0.0, 0.0, 1.0 / scale, 0.0],
                    output_width: (f64::from(stage.width) * scale) as u32,
                    output_height: (f64::from(stage.height) * scale) as u32,
                }));
            }
            let crop: CropPayload = serde_json::from_value(payload.clone())
                .map_err(|error| Error::new(ErrorKind::Validation, error.to_string()))?;
            let crop_stage = CropStage {
                width: stage.width,
                height: stage.height,
                angle: crop.angle,
            };
            let rect = crop.output_rect(&crop_stage)?;
            if crop.angle == 0.0 {
                return Ok(Processing::ExactGeometry(ExactGeometry::crop(
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                )));
            }
            Ok(Processing::Resample(Resample {
                inverse: crop_stage.inverse_map((rect.x as f64, rect.y as f64)),
                output_width: rect.width,
                output_height: rect.height,
            }))
        }
    }

    /// The payload a crop draft would commit: the requested fraction of the rotated box, fitted onto
    /// the source and normalized, so every case in these tests is a rectangle the contract accepts.
    fn fitted_crop(width: u32, height: u32, angle: f64, rect: [f64; 4]) -> CropPayload {
        let stage = CropStage {
            width,
            height,
            angle,
        };
        let (box_width, box_height) = stage.bounding_box();
        let fitted = stage.fit_about_center(BoxRect {
            x: rect[0] * box_width,
            y: rect[1] * box_height,
            width: rect[2] * box_width,
            height: rect[3] * box_height,
        });
        fitted.normalized(&stage)
    }

    fn crop_layer(crop: CropPayload) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: TEST_CROP_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: serde_json::to_value(crop).unwrap(),
        }
    }

    fn offset_layer(x: i64, y: i64, width: u32, height: u32) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: TEST_OFFSET_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"x": x, "y": y, "width": width, "height": height}),
        }
    }

    fn scale_layer(scale: f64) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: TEST_SCALE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"scale": scale}),
        }
    }

    fn geometry_registry() -> ModuleRegistry {
        let mut registry = ModuleRegistry::builtin();
        registry.register(GeometryTestModule::shared()).unwrap();
        registry
    }

    /// An asymmetric gradient with a varying alpha, so a wrong axis, a wrong weight or a dropped
    /// alpha channel all show up.
    fn gradient(width: u32, height: u32) -> SourceImage {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                rgba.extend([
                    (x * 251 / width.max(1)) as u8,
                    (y * 241 / height.max(1)) as u8,
                    ((x * 7 + y * 3) % 256) as u8,
                    (200 + (x + y) % 56) as u8,
                ]);
            }
        }
        SourceImage {
            width,
            height,
            rgba: rgba.into(),
            fingerprint: "sha256:gradient".into(),
            orientation: 1,
        }
    }

    /// An independent f64 evaluation of the crop contract: the rotated box, the rounded output
    /// rectangle and one bilinear sample in linear light. It shares no code with the renderer.
    struct CropReference {
        box_width: f64,
        box_height: f64,
        cos: f64,
        sin: f64,
        origin: (f64, f64),
        width: u32,
        height: u32,
    }

    impl CropReference {
        fn new(source: &SourceImage, crop: CropPayload) -> Self {
            let rect = [crop.x, crop.y, crop.width, crop.height];
            let radians = crop.angle * std::f64::consts::PI / 180.0;
            let (cos, sin) = (radians.cos(), radians.sin());
            let width = f64::from(source.width);
            let height = f64::from(source.height);
            let box_width = width * cos.abs() + height * sin.abs();
            let box_height = width * sin.abs() + height * cos.abs();
            Self {
                box_width,
                box_height,
                cos,
                sin,
                origin: (
                    (rect[0] * box_width).round(),
                    (rect[1] * box_height).round(),
                ),
                width: (rect[2] * box_width).round().max(1.0) as u32,
                height: (rect[3] * box_height).round().max(1.0) as u32,
            }
        }

        /// The continuous source position, in index space, that one output pixel center samples.
        fn position(&self, source: &SourceImage, i: u32, j: u32) -> (f64, f64) {
            let x = self.origin.0 + f64::from(i) + 0.5 - self.box_width / 2.0;
            let y = self.origin.1 + f64::from(j) + 0.5 - self.box_height / 2.0;
            (
                self.cos * x + self.sin * y + f64::from(source.width) / 2.0 - 0.5,
                -self.sin * x + self.cos * y + f64::from(source.height) / 2.0 - 0.5,
            )
        }

        fn pixel(&self, source: &SourceImage, i: u32, j: u32) -> [u8; 4] {
            let decode = |value: u8| -> f64 {
                let encoded = f64::from(value) / 255.0;
                if encoded <= 0.04045 {
                    encoded / 12.92
                } else {
                    ((encoded + 0.055) / 1.055).powf(2.4)
                }
            };
            let encode = |linear: f64| -> u8 {
                let linear = linear.clamp(0.0, 1.0);
                let encoded = if linear <= 0.0031308 {
                    12.92 * linear
                } else {
                    1.055 * linear.powf(1.0 / 2.4) - 0.055
                };
                (encoded * 255.0).round() as u8
            };
            let (u, v) = self.position(source, i, j);
            let (left, top) = (u.floor(), v.floor());
            let (fraction_x, fraction_y) = (u - left, v - top);
            let at = |x: f64, y: f64| -> [u8; 4] {
                let x = (x.max(0.0) as u32).min(source.width - 1);
                let y = (y.max(0.0) as u32).min(source.height - 1);
                let offset = ((y * source.width + x) * 4) as usize;
                [
                    source.rgba[offset],
                    source.rgba[offset + 1],
                    source.rgba[offset + 2],
                    source.rgba[offset + 3],
                ]
            };
            let samples = [
                (at(left, top), (1.0 - fraction_x) * (1.0 - fraction_y)),
                (at(left + 1.0, top), fraction_x * (1.0 - fraction_y)),
                (at(left, top + 1.0), (1.0 - fraction_x) * fraction_y),
                (at(left + 1.0, top + 1.0), fraction_x * fraction_y),
            ];
            let mut pixel = [0; 4];
            for (channel, slot) in pixel.iter_mut().enumerate().take(3) {
                *slot = encode(
                    samples
                        .iter()
                        .map(|(sample, weight)| weight * decode(sample[channel]))
                        .sum(),
                );
            }
            pixel[3] = samples
                .iter()
                .map(|(sample, weight)| weight * f64::from(sample[3]))
                .sum::<f64>()
                .round() as u8;
            pixel
        }
    }

    #[test]
    #[ignore = "measurement, run explicitly in release"]
    fn measure_resample_on_photo_sized_frames() {
        let registry = geometry_registry();
        for (width, height) in [(6000_u32, 4000_u32), (9504, 6336)] {
            let source = gradient(width, height);
            for (label, layers) in [
                ("exact rotate", vec![turn(Transform::RotateRight)]),
                (
                    "crop 0 deg",
                    vec![crop_layer(fitted_crop(
                        width,
                        height,
                        0.0,
                        [0.1, 0.1, 0.8, 0.8],
                    ))],
                ),
                (
                    "crop 10 deg",
                    vec![crop_layer(fitted_crop(
                        width,
                        height,
                        10.0,
                        [0.1, 0.1, 0.8, 0.8],
                    ))],
                ),
                (
                    "crop 10 deg then rotate",
                    vec![
                        crop_layer(fitted_crop(width, height, 10.0, [0.1, 0.1, 0.8, 0.8])),
                        turn(Transform::RotateRight),
                    ],
                ),
            ] {
                let recipe = Recipe { format: 1, layers };
                let mut best = f64::INFINITY;
                let mut raster = None;
                for _ in 0..5 {
                    let start = std::time::Instant::now();
                    raster = Some(render(&registry, &source, SnapshotId::new(), &recipe).unwrap());
                    best = best.min(start.elapsed().as_secs_f64() * 1000.0);
                }
                let raster = raster.unwrap();
                println!(
                    "{width}x{height} {label}: {best:.1} ms -> {}x{}",
                    raster.width, raster.height
                );
            }
        }
    }

    #[test]
    fn rotated_crops_match_an_independent_reference_sampler() {
        let registry = geometry_registry();
        // The last input is over a megapixel on both sides of the resample, so the parallel row path
        // runs; every pixel of every case is compared against the reference.
        for (width, height, angle, rect) in [
            (64_u32, 48_u32, 7.5_f64, [0.12, 0.1, 0.7, 0.75]),
            (64, 48, -30.0, [0.25, 0.2, 0.5, 0.55]),
            (64, 48, 45.0, [0.3, 0.3, 0.4, 0.4]),
            (1500, 1100, 10.0, [0.1, 0.1, 0.8, 0.8]),
        ] {
            let source = gradient(width, height);
            let crop = fitted_crop(width, height, angle, rect);
            let recipe = Recipe {
                format: 1,
                layers: vec![crop_layer(crop)],
            };
            let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
            let reference = CropReference::new(&source, crop);
            let case = format!("{width}x{height} at {angle}");
            assert_eq!(
                (raster.width, raster.height),
                (reference.width, reference.height),
                "{case}: output dimensions"
            );
            assert!(raster.width > 1 && raster.height > 1, "{case}");
            let mut worst = 0_i32;
            for j in 0..raster.height {
                for i in 0..raster.width {
                    let expected = reference.pixel(&source, i, j);
                    let actual = raster.pixel(i, j).expect("inside the output stage");
                    for channel in 0..4 {
                        let difference = i32::from(actual[channel]) - i32::from(expected[channel]);
                        worst = worst.max(difference.abs());
                        assert!(
                            difference.abs() <= 1,
                            "{case}: pixel ({i}, {j}) channel {channel}: {actual:?} against {expected:?}"
                        );
                    }
                }
            }
            assert!(
                u64::from(raster.width) * u64::from(raster.height) > PARALLEL_RENDER_PIXELS
                    || width == 64,
                "{case}: {}x{} does not reach the parallel row path",
                raster.width,
                raster.height
            );
            println!("{case}: worst channel difference against the f64 reference is {worst}");
        }
    }

    #[test]
    fn angle_zero_crops_are_exact_copies_composed_into_one_pass() {
        let registry = geometry_registry();
        let source = source(7, 5);
        let crops = [
            CropPayload::NEUTRAL,
            CropPayload {
                angle: 0.0,
                x: 2.0 / 7.0,
                y: 1.0 / 5.0,
                width: 4.0 / 7.0,
                height: 3.0 / 5.0,
            },
            CropPayload {
                angle: 0.0,
                x: 0.0,
                y: 4.0 / 5.0,
                width: 1.0,
                height: 1.0 / 5.0,
            },
        ];
        for crop in crops {
            for stack in [
                vec![],
                vec![Transform::RotateRight],
                vec![Transform::MirrorHorizontal],
            ] {
                for after in [
                    vec![],
                    vec![Transform::RotateLeft],
                    vec![Transform::FlipVertical],
                ] {
                    let mut layers: Vec<Layer> = stack.iter().copied().map(turn).collect();
                    layers.push(crop_layer(crop));
                    layers.extend(after.iter().copied().map(turn));
                    let recipe = Recipe {
                        format: 1,
                        layers: layers.clone(),
                    };
                    let compiled = registry
                        .compile(source.width, source.height, &recipe)
                        .unwrap();
                    assert_eq!(
                        compiled.segments.len(),
                        1,
                        "an exact crop never starts a new rasterizing pass"
                    );
                    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
                    let expected = reference(&source, &layers);
                    assert_eq!(
                        (raster.width, raster.height),
                        (expected.0, expected.1),
                        "{layers:?}"
                    );
                    assert_eq!(
                        raster.rgba.as_ref(),
                        expected.2,
                        "{crop:?} {stack:?} {after:?}"
                    );
                }
            }
        }
        // A neutral crop of the whole stage still shares the source allocation.
        let shared = render(
            &registry,
            &source,
            SnapshotId::new(),
            &Recipe {
                format: 1,
                layers: vec![crop_layer(CropPayload::NEUTRAL)],
            },
        )
        .unwrap();
        assert!(Arc::ptr_eq(&shared.rgba, &source.rgba));
    }

    #[test]
    fn locating_maps_exact_geometry_back_to_the_content_pixel() {
        let registry = geometry_registry();
        let source = source(7, 5);
        let crops = [
            CropPayload::NEUTRAL,
            CropPayload {
                angle: 0.0,
                x: 2.0 / 7.0,
                y: 1.0 / 5.0,
                width: 4.0 / 7.0,
                height: 3.0 / 5.0,
            },
            CropPayload {
                angle: 0.0,
                x: 0.0,
                y: 4.0 / 5.0,
                width: 1.0,
                height: 1.0 / 5.0,
            },
        ];
        for crop in crops {
            for before in [
                vec![],
                vec![Transform::RotateRight],
                vec![Transform::MirrorHorizontal],
                vec![Transform::RotateLeft, Transform::FlipVertical],
            ] {
                for after in [
                    vec![],
                    vec![Transform::RotateLeft],
                    vec![Transform::FlipVertical],
                    vec![Transform::MirrorHorizontal, Transform::RotateRight],
                ] {
                    let mut layers: Vec<Layer> = before.iter().copied().map(turn).collect();
                    layers.push(crop_layer(crop));
                    layers.extend(after.iter().copied().map(turn));
                    let recipe = Recipe {
                        format: 1,
                        layers: layers.clone(),
                    };
                    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
                    // Every output pixel names a content pixel that the stepwise forward map puts
                    // back where it was found, and the rendered bytes are that content pixel's.
                    for y in 0..raster.height {
                        for x in 0..raster.width {
                            let located = locate(&registry, &source, &recipe, x, y).unwrap();
                            let content = (located.content_x, located.content_y);
                            assert_eq!(
                                (located.width, located.height),
                                (source.width, source.height),
                                "{layers:?}"
                            );
                            assert_eq!(
                                forward(source.width, source.height, &layers, content.0, content.1),
                                Some((x, y)),
                                "({x}, {y}) of {layers:?}"
                            );
                            assert_eq!(
                                raster.pixel(x, y),
                                Some(source_pixel(&source, content.0, content.1)),
                                "({x}, {y}) of {layers:?}"
                            );
                        }
                    }
                    // And every content pixel the stack keeps is located from where it lands.
                    for y in 0..source.height {
                        for x in 0..source.width {
                            let Some((out_x, out_y)) =
                                forward(source.width, source.height, &layers, x, y)
                            else {
                                continue;
                            };
                            let located =
                                locate(&registry, &source, &recipe, out_x, out_y).unwrap();
                            assert_eq!(
                                (located.content_x, located.content_y),
                                (x, y),
                                "({x}, {y}) of {layers:?}"
                            );
                        }
                    }
                    // A point outside the output stage is refused, not clamped.
                    for (x, y) in [(raster.width, 0), (0, raster.height)] {
                        let error = locate(&registry, &source, &recipe, x, y)
                            .expect_err(&format!("({x}, {y}) is outside {layers:?}"));
                        assert_eq!(error.kind, ErrorKind::Validation);
                        assert!(
                            error
                                .detail
                                .contains(&format!("{}x{}", raster.width, raster.height)),
                            "{error}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_rotated_crop_locates_the_nearest_pixel_its_sampler_read() {
        let registry = geometry_registry();
        for (width, height, angle, rect, after) in [
            (40_u32, 24_u32, 12.0_f64, [0.2, 0.15, 0.6, 0.65], vec![]),
            (
                28,
                36,
                -30.0,
                [0.25, 0.2, 0.5, 0.55],
                vec![Transform::RotateRight],
            ),
            (
                32,
                24,
                7.5,
                [0.3, 0.25, 0.45, 0.5],
                vec![Transform::MirrorHorizontal, Transform::FlipVertical],
            ),
        ] {
            let source = gradient(width, height);
            let crop = fitted_crop(width, height, angle, rect);
            let tail: Vec<Layer> = after.iter().copied().map(turn).collect();
            // A pixel layer before the crop moves nothing; the walk back is geometry only.
            let mut layers = vec![Layer::pixel(2, 3, [250, 1, 2]), crop_layer(crop)];
            layers.extend(tail.iter().cloned());
            let recipe = Recipe {
                format: 1,
                layers: layers.clone(),
            };
            let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
            let reference = CropReference::new(&source, crop);
            let case = format!("{width}x{height} at {angle}");
            assert_eq!(
                u64::from(raster.width) * u64::from(raster.height),
                u64::from(reference.width) * u64::from(reference.height),
                "{case}: output pixel count"
            );
            for j in 0..reference.height {
                for i in 0..reference.width {
                    let (x, y) = forward(reference.width, reference.height, &tail, i, j)
                        .expect("exact transforms keep every pixel");
                    let located = locate(&registry, &source, &recipe, x, y).unwrap();
                    let (u, v) = reference.position(&source, i, j);
                    assert_eq!(
                        (located.content_x, located.content_y),
                        (nearest(u, width), nearest(v, height)),
                        "{case}: ({i}, {j}) of the crop's output"
                    );
                    assert_eq!((located.width, located.height), (width, height), "{case}");
                }
            }
        }
    }

    #[test]
    fn a_translation_with_a_smaller_output_composes_and_is_bounds_checked() {
        let registry = geometry_registry();
        let source = source(7, 5);
        // Two translations and a quarter turn compose into one mapping over the source.
        let layers = vec![
            offset_layer(1, 1, 5, 4),
            turn(Transform::RotateRight),
            offset_layer(1, 2, 2, 3),
        ];
        let recipe = Recipe {
            format: 1,
            layers: layers.clone(),
        };
        let compiled = registry
            .compile(source.width, source.height, &recipe)
            .unwrap();
        assert_eq!(compiled.segments.len(), 1);
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        assert_eq!((raster.width, raster.height), (2, 3));
        // The same stack applied one step at a time, through the composed mapping's own definition.
        let mut expected = Vec::new();
        for y in 0..raster.height {
            for x in 0..raster.width {
                let (turned_x, turned_y) = (x + 1, y + 2);
                let (offset_x, offset_y) = (turned_y, 4 - 1 - turned_x);
                expected.extend(source_pixel(&source, offset_x + 1, offset_y + 1));
            }
        }
        assert_eq!(raster.rgba.as_ref(), expected);
        for (x, y) in [(0, 0), (raster.width - 1, raster.height - 1)] {
            assert_eq!(
                sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                raster.pixel(x, y)
            );
        }
        // A mapping that would read outside its input frame is refused, not rasterized.
        for layers in [
            vec![offset_layer(3, 0, 5, 5)],
            vec![offset_layer(-1, 0, 4, 4)],
            vec![offset_layer(0, 0, 8, 5)],
            vec![offset_layer(1, 1, 5, 4), offset_layer(1, 0, 5, 4)],
        ] {
            let recipe = Recipe {
                format: 1,
                layers: layers.clone(),
            };
            let error = render(&registry, &source, SnapshotId::new(), &recipe)
                .expect_err(&format!("{layers:?} reads outside the stage"));
            assert_eq!(error.kind, ErrorKind::Validation);
            assert!(error.detail.contains("reads outside"), "{error}");
            assert_eq!(
                sample(&registry, &source, &recipe, 0, 0).unwrap_err().kind,
                ErrorKind::Validation
            );
        }
    }

    #[test]
    fn samples_match_rendered_pixels_through_a_resample() {
        let registry = geometry_registry();
        let source = gradient(40, 24);
        for layers in [
            vec![crop_layer(fitted_crop(
                40,
                24,
                12.0,
                [0.2, 0.15, 0.6, 0.65],
            ))],
            vec![
                Layer::pixel(3, 4, [250, 1, 2]),
                crop_layer(fitted_crop(40, 24, -20.0, [0.25, 0.25, 0.5, 0.5])),
                Layer::pixel(1, 1, [3, 251, 4]),
                turn(Transform::RotateRight),
                Layer::pixel(0, 2, [5, 6, 252]),
            ],
            // Two resamples: a point query blends four recursively evaluated blends.
            vec![
                turn(Transform::MirrorHorizontal),
                crop_layer(fitted_crop(40, 24, 45.0, [0.3, 0.3, 0.4, 0.4])),
                scale_layer(1.5),
                Layer::pixel(0, 0, [7, 8, 253]),
            ],
            vec![scale_layer(2.0), Layer::pixel(5, 5, [254, 9, 10])],
        ] {
            let recipe = Recipe {
                format: 1,
                layers: layers.clone(),
            };
            let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
            for y in 0..raster.height {
                for x in 0..raster.width {
                    let sampled = sample(&registry, &source, &recipe, x, y).unwrap();
                    assert_eq!(
                        (sampled.width, sampled.height),
                        (raster.width, raster.height),
                        "{layers:?}"
                    );
                    assert_eq!(sampled.rgba, raster.pixel(x, y), "({x}, {y}) of {layers:?}");
                }
            }
            assert_eq!(
                sample(&registry, &source, &recipe, raster.width, 0)
                    .unwrap()
                    .rgba,
                None
            );
        }
    }

    #[test]
    fn a_point_replacement_cropped_away_simply_disappears() {
        let registry = geometry_registry();
        let source = source(8, 6);
        let inside = Layer::pixel(4, 3, [250, 1, 2]);
        let outside = Layer::pixel(0, 0, [3, 251, 4]);
        for crop in [
            crop_layer(CropPayload {
                angle: 0.0,
                x: 0.25,
                y: 1.0 / 3.0,
                width: 0.5,
                height: 0.5,
            }),
            crop_layer(fitted_crop(8, 6, 15.0, [0.25, 0.25, 0.5, 0.5])),
        ] {
            let with_outside = Recipe {
                format: 1,
                layers: vec![inside.clone(), outside.clone(), crop.clone()],
            };
            let without = Recipe {
                format: 1,
                layers: vec![inside.clone(), crop.clone()],
            };
            let rendered = render(&registry, &source, SnapshotId::new(), &with_outside).unwrap();
            let expected = render(&registry, &source, SnapshotId::new(), &without).unwrap();
            assert_eq!(rendered.rgba, expected.rgba, "{crop:?}");
            for y in 0..rendered.height {
                for x in 0..rendered.width {
                    assert_eq!(
                        sample(&registry, &source, &with_outside, x, y)
                            .unwrap()
                            .rgba,
                        rendered.pixel(x, y),
                        "({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn the_frame_limit_applies_to_a_resample_and_point_queries_still_answer() {
        let registry = geometry_registry();
        let source = source(3, 2);
        let recipe = Recipe {
            format: 1,
            layers: vec![scale_layer(10_000.0)],
        };
        let error = render(&registry, &source, SnapshotId::new(), &recipe)
            .expect_err("30000x20000 is over the frame limit");
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert!(error.detail.contains("512 MiB"), "{error}");
        // The same stack answers a point query, because no frame is allocated for one pixel.
        let sampled = sample(&registry, &source, &recipe, 15_000, 10_000).unwrap();
        assert_eq!((sampled.width, sampled.height), (30_000, 20_000));
        assert!(sampled.rgba.is_some());
        // A resample must declare a stage the host can address at all.
        let empty = Recipe {
            format: 1,
            layers: vec![scale_layer(0.0)],
        };
        let error = sample(&registry, &source, &empty, 0, 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
    }

    #[test]
    fn pixel_layers_are_ordered_exact_and_source_is_immutable() {
        let source = source(3, 2);
        let original = source.clone();
        let a = Layer::pixel(1, 0, [200, 201, 202]);
        let first = rendered(&source, vec![a.clone()]);
        let second = rendered(&source, vec![a, Layer::pixel(1, 0, [9, 8, 7])]);
        assert_eq!(first.pixel(1, 0), Some([200, 201, 202, 255]));
        assert_eq!(second.pixel(1, 0), Some([9, 8, 7, 255]));
        assert_eq!(first.pixel(0, 0), Some([0, 20, 40, 255]));
        assert_eq!(source, original);
        assert!(rendered(&source, vec![]).pixel(1, 0) != second.pixel(1, 0));
    }

    #[test]
    fn exact_transform_coordinate_tables_for_asymmetric_input() {
        let source = source(3, 2);
        assert_eq!(
            red(&rendered(&source, vec![turn(Transform::RotateRight)])),
            vec![3, 0, 4, 1, 5, 2]
        );
        assert_eq!(
            red(&rendered(&source, vec![turn(Transform::RotateLeft)])),
            vec![2, 5, 1, 4, 0, 3]
        );
        assert_eq!(
            red(&rendered(&source, vec![turn(Transform::MirrorHorizontal)])),
            vec![2, 1, 0, 5, 4, 3]
        );
        assert_eq!(
            red(&rendered(&source, vec![turn(Transform::FlipVertical)])),
            vec![3, 4, 5, 0, 1, 2]
        );
    }

    #[test]
    fn transform_identities_and_operation_order_hold() {
        let source = source(5, 3);
        for (transform, count) in [
            (Transform::RotateRight, 4),
            (Transform::RotateLeft, 4),
            (Transform::MirrorHorizontal, 2),
            (Transform::FlipVertical, 2),
        ] {
            let layers = (0..count).map(|_| turn(transform)).collect();
            assert_eq!(rendered(&source, layers).rgba, source.rgba);
            // The same actions composed into one layer reach the neutral orientation, whose
            // identity mapping shares the source buffer instead of copying it.
            let mut composed = Orientation::NEUTRAL;
            for _ in 0..count {
                composed = composed.then(transform);
            }
            assert_eq!(composed, Orientation::NEUTRAL, "{transform:?}");
            let collapsed = rendered(&source, vec![Layer::orientation(composed)]);
            assert_eq!(collapsed.rgba, source.rgba);
            assert!(Arc::ptr_eq(&collapsed.rgba, &source.rgba));
        }
        let before = rendered(
            &source,
            vec![
                Layer::pixel(0, 0, [250, 0, 0]),
                turn(Transform::RotateRight),
            ],
        );
        assert_eq!(before.pixel(2, 0), Some([250, 0, 0, 255]));
        let after = rendered(
            &source,
            vec![
                turn(Transform::RotateRight),
                Layer::pixel(0, 0, [250, 0, 0]),
            ],
        );
        assert_eq!(after.pixel(0, 0), Some([250, 0, 0, 255]));
        assert_ne!(before.rgba, after.rgba);
    }

    #[test]
    fn compiled_recipes_match_stepwise_evaluation_for_interleaved_operations() {
        let source = source(5, 3);
        let transforms = [
            Transform::RotateLeft,
            Transform::RotateRight,
            Transform::MirrorHorizontal,
            Transform::FlipVertical,
        ];
        for first in transforms {
            for second in transforms {
                for third in transforms {
                    let layers = vec![
                        Layer::pixel(1, 1, [201, 1, 2]),
                        turn(first),
                        Layer::pixel(0, 0, [3, 202, 4]),
                        turn(second),
                        Layer::pixel(1, 1, [5, 6, 203]),
                        turn(third),
                        Layer::pixel(0, 0, [204, 8, 9]),
                    ];
                    let expected = reference(&source, &layers);
                    let actual = rendered(&source, layers);
                    assert_eq!((actual.width, actual.height), (expected.0, expected.1));
                    assert_eq!(actual.rgba.as_ref(), expected.2);
                }
            }
        }
    }

    /// One orientation layer holding the composed state renders exactly what the same actions
    /// render as separate single-action layers, and both match the stepwise reference. Every one
    /// of the eight orientations against every action, and every sequence of three actions, on a
    /// non-square source so a wrongly composed quarter turn changes the dimensions.
    #[test]
    fn a_composed_orientation_renders_what_its_separate_action_layers_render() {
        let source = source(5, 3);
        let transforms = [
            Transform::RotateLeft,
            Transform::RotateRight,
            Transform::MirrorHorizontal,
            Transform::FlipVertical,
        ];
        let orientations = [false, true]
            .into_iter()
            .flat_map(|mirror| (0..4).map(move |turns| Orientation { mirror, turns }));
        for state in orientations {
            for transform in transforms {
                let separate = vec![Layer::orientation(state), turn(transform)];
                let expected = reference(&source, &separate);
                let collapsed = rendered(&source, vec![Layer::orientation(state.then(transform))]);
                let stepwise = rendered(&source, separate);
                for raster in [&collapsed, &stepwise] {
                    assert_eq!(
                        (raster.width, raster.height),
                        (expected.0, expected.1),
                        "{state:?} then {transform:?}"
                    );
                    assert_eq!(
                        raster.rgba.as_ref(),
                        expected.2,
                        "{state:?} then {transform:?}"
                    );
                }
            }
        }
        for first in transforms {
            for second in transforms {
                for third in transforms {
                    let separate = vec![turn(first), turn(second), turn(third)];
                    let composed = Orientation::of(first).then(second).then(third);
                    let expected = reference(&source, &separate);
                    let collapsed = rendered(&source, vec![Layer::orientation(composed)]);
                    assert_eq!(
                        (collapsed.width, collapsed.height),
                        (expected.0, expected.1),
                        "{first:?} {second:?} {third:?}"
                    );
                    assert_eq!(
                        collapsed.rgba.as_ref(),
                        expected.2,
                        "{first:?} {second:?} {third:?}"
                    );
                    assert_eq!(rendered(&source, separate).rgba, collapsed.rgba);
                }
            }
        }
    }

    #[test]
    fn samples_match_rendered_pixels_and_keep_source_alpha() {
        let registry = registry();
        let mut source = source(5, 3);
        let rgba: Vec<u8> = source
            .rgba
            .iter()
            .enumerate()
            .map(|(i, v)| if i % 4 == 3 { (i / 4) as u8 + 100 } else { *v })
            .collect();
        source.rgba = rgba.into();
        let recipe = Recipe {
            format: 1,
            layers: vec![
                Layer::pixel(1, 1, [201, 1, 2]),
                turn(Transform::RotateLeft),
                Layer::pixel(0, 0, [3, 202, 4]),
                turn(Transform::MirrorHorizontal),
                Layer::pixel(0, 0, [204, 8, 9]),
                Layer::pixel(0, 0, [205, 10, 11]),
            ],
        };
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        for y in 0..raster.height {
            for x in 0..raster.width {
                let sampled = sample(&registry, &source, &recipe, x, y).unwrap();
                assert_eq!(
                    (sampled.width, sampled.height),
                    (raster.width, raster.height)
                );
                assert_eq!(sampled.rgba, raster.pixel(x, y), "({x}, {y})");
            }
        }
        assert_eq!(
            sample(&registry, &source, &recipe, raster.width, 0)
                .unwrap()
                .rgba,
            None
        );
        assert_eq!(
            sample(&registry, &source, &recipe, 0, raster.height)
                .unwrap()
                .rgba,
            None
        );
        let invalid = Recipe {
            format: 1,
            layers: vec![Layer::pixel(9, 9, [0, 0, 0])],
        };
        assert!(sample(&registry, &source, &invalid, 0, 0).is_err());
    }

    #[test]
    fn invalid_coordinates_and_buffers_fail_without_panicking() {
        let registry = registry();
        let source = source(3, 2);
        let snapshot = Snapshot::original(AssetId::new());
        assert!(
            render(
                &registry,
                &source,
                snapshot.id.clone(),
                &Recipe {
                    format: 1,
                    layers: vec![Layer::pixel(3, 0, [0, 0, 0])]
                }
            )
            .is_err()
        );
        let malformed = SourceImage {
            rgba: vec![0].into(),
            ..source
        };
        assert!(render(&registry, &malformed, snapshot.id, &Recipe::default()).is_err());
    }
}
