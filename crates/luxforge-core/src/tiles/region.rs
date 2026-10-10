//! Bounded, reduced regions of a stage, served through the tile provider.
//!
//! A region is one rectangle of the stage one layer receives — or of one layer's output with a
//! payload a module supplies in place of its own — read in linear light and reduced by block
//! means, each reduced pixel the mean of the stage pixels of its `r × r` block that exist. It knows
//! nothing of the feature that reads it: a module's query asks for one through
//! [`crate::StageQuestions::region_before`], as Corrections' automatic source search reads the
//! repair stage's input around a destination.
//!
//! The rectangle is read in bands of whole blocks through the call's one tile session, so what it
//! holds at once is one band of linear values and the reduced sums, never the region at full
//! resolution; the region itself is held to the pixels one evaluated frame may hold and its reduced
//! form to [`MAX_REDUCED_POINTS`]. Every band is drawn by one renderer: a band drawn by another
//! than the first is read again, whole region, from the reference, so an answer never mixes them.
use super::{
    Answered, MaskInputMode, ReadPixels, ReadStage, ReadValues, ReferenceReads, TileReads,
};
use crate::{Cancel, Error, Evaluation, Region, Stage};
use serde_json::Value;

/// The most reduced pixels one region read answers: a 1024-square grid's worth, the bound a
/// sample grid keeps ([`super::GRID_MAX_SIDE`]).
pub const MAX_REDUCED_POINTS: usize = 1024 * 1024;

/// The largest block side a region may be reduced by.
pub const MAX_REDUCTION: u32 = 1 << 14;

/// The most stage pixels one band of a region read holds at once: 1 MP of linear values, 12 MiB.
const BAND_PIXELS: u64 = 1 << 20;

/// Which stage a region is read from.
#[derive(Clone, Debug, PartialEq)]
pub enum RegionStage {
    /// The stage layer `index` receives: the output of the layers before it. `index` may be the
    /// stack's length, whose prefix is the whole stack.
    Before(usize),
    /// The output of layer `layer` with `payload` in place of its own: the stage layer `layer + 1`
    /// receives from the stack with that one payload replaced, its strokes resolved from the
    /// recipe's table. How a module reads its own layer as an earlier state of it, as the repair
    /// layer's operations before one are.
    OutputWith { layer: usize, payload: Value },
}

/// One bounded region read ([`read`]).
#[derive(Clone, Debug, PartialEq)]
pub struct RegionRequest {
    pub stage: RegionStage,
    /// The rectangle wanted, in the stage's pixels. It is widened to whole blocks of the reduction
    /// and clipped to the stage before it is read.
    pub rect: Region,
    /// The block side of the reduction, a power of two in `1..=`[`MAX_REDUCTION`].
    pub reduction: u32,
}

impl RegionRequest {
    /// The layer whose input stage is read: the one [`RegionStage::Before`] names, or the one
    /// after the replaced layer.
    pub(crate) fn layer(&self) -> usize {
        match &self.stage {
            RegionStage::Before(index) => *index,
            RegionStage::OutputWith { layer, .. } => layer + 1,
        }
    }
}

/// A region's reduced linear values.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionRead {
    /// The whole stage the region was read from, at full resolution.
    pub stage: Stage,
    /// The stage rectangle read: the request's, widened to whole blocks and clipped to the stage.
    /// Its origin is a multiple of the reduction, so block `(bx, by)` of the read is block
    /// `(rect.x0 / reduction + bx, rect.y0 / reduction + by)` of the whole stage reduced.
    pub rect: Region,
    pub reduction: u32,
    /// The reduced region's width and height: `ceil` of the rectangle's over the reduction.
    pub size: [u32; 2],
    /// One linear RGB mean per reduced pixel, row by row from the top left: the mean of the stage
    /// pixels of its block that exist, summed row by row, left to right, in `f64`.
    pub linear: Vec<[f64; 3]>,
    /// The renderer that drew every pixel of it.
    pub answered: Answered,
}

impl RegionRead {
    /// The mean at reduced position `(x, y)` of the read, `None` outside it.
    pub fn at(&self, x: u32, y: u32) -> Option<[f64; 3]> {
        (x < self.size[0] && y < self.size[1])
            .then(|| self.linear[y as usize * self.size[0] as usize + x as usize])
    }
}

/// Read `request` of `evaluation`'s stack through `reads` under `cancel`.
///
/// Validation first, reading nothing: a reduction that is not a power of two in range, a layer
/// outside the stack, a replaced layer the stack does not hold, and a region whose reduced form
/// exceeds [`MAX_REDUCED_POINTS`] or whose stage rectangle exceeds what one evaluated frame may
/// hold are refused by name. A region that misses the stage answers an empty read. Costs one
/// prefix compile, `O(layers)`, and the bands' reads; holds one band and the reduced sums.
pub fn read(
    evaluation: &Evaluation,
    request: &RegionRequest,
    reads: &dyn TileReads,
    cancel: &Cancel,
) -> Result<RegionRead, Error> {
    cancel.check()?;
    let reduction = request.reduction;
    if !reduction.is_power_of_two() || reduction > MAX_REDUCTION {
        return Err(Error::validation(format!(
            "a region's reduction must be a power of two within 1..={MAX_REDUCTION}"
        )));
    }
    let replaced = match &request.stage {
        RegionStage::Before(_) => None,
        RegionStage::OutputWith { layer, payload } => {
            let mut recipe = evaluation.recipe().clone();
            let target = recipe.layers.get_mut(*layer).ok_or_else(|| {
                Error::validation(format!(
                    "layer index {layer} is outside the {} layers of the stack",
                    evaluation.recipe().layers.len()
                ))
            })?;
            target.payload = payload.clone();
            Some(
                Evaluation::new(
                    evaluation.registry().clone(),
                    evaluation.context().clone(),
                    evaluation.source().clone(),
                    evaluation.entry().clone(),
                    recipe,
                    None,
                )
                .with_sensor(evaluation.sensor_arc()),
            )
        }
    };
    let evaluation = replaced.as_ref().unwrap_or(evaluation);
    let layer = request.layer();
    let recipe = evaluation.recipe();
    let prefix = recipe.layers.get(..layer).ok_or_else(|| {
        Error::validation(format!(
            "layer index {layer} is outside the {} layers of the stack",
            recipe.layers.len()
        ))
    })?;
    let (width, height) = evaluation.source().dimensions();
    let stage = evaluation
        .registry()
        .compile_layers(
            width,
            height,
            prefix,
            &recipe.masks,
            &recipe.strokes,
            &recipe.artifacts,
        )?
        .stage();
    let rect = aligned(request.rect, reduction, stage);
    let size = [
        rect.width.div_ceil(reduction),
        rect.height.div_ceil(reduction),
    ];
    let points = size[0] as usize * size[1] as usize;
    if points > MAX_REDUCED_POINTS {
        return Err(Error::resource_limit(format!(
            "a region of {} x {} reduced pixels exceeds the {MAX_REDUCED_POINTS} one read answers",
            size[0], size[1]
        )));
    }
    let empty = |answered| RegionRead {
        stage,
        rect,
        reduction,
        size,
        linear: Vec::new(),
        answered,
    };
    if rect.width == 0 || rect.height == 0 {
        return Ok(empty(Answered::reference(None)));
    }
    let mode = crate::render::MaskInputMode::for_layer(evaluation.registry(), recipe, layer);
    let stage_read = ReadStage::Before {
        layer,
        mode: MaskInputMode::from(mode),
    };
    // Before allocation: the reduced sums and one band of linear values.
    let band_rows = band_rows(rect.width, reduction).min(rect.height);
    let _scratch = evaluation
        .context()
        .scratch()
        .reserve(points * size_of::<[f64; 3]>() + band_rows as usize * rect.width as usize * 12);
    let read_with = |reads: &dyn TileReads| -> Result<Option<RegionRead>, Error> {
        let mut session = reads.session(evaluation, cancel);
        let mut sums = vec![[0.0_f64; 3]; points];
        let mut answered: Option<Answered> = None;
        let mut y = rect.y0;
        while y < rect.y0 + rect.height {
            cancel.check()?;
            let rows = band_rows.min(rect.y0 + rect.height - y);
            let band = Region {
                x0: rect.x0,
                y0: y,
                width: rect.width,
                height: rows,
            };
            let answer = session.read(stage_read, band, ReadValues::Linear)?;
            match &answered {
                None => answered = Some(answer.answered.clone()),
                // A band drawn by another renderer than the first: the whole region is read
                // again from the reference.
                Some(first) if first.record != answer.answered.record => return Ok(None),
                Some(_) => {}
            }
            let ReadPixels::Linear(values) = &answer.pixels else {
                return Err(Error::internal("a linear region read answered codes"));
            };
            if answer.rect != band {
                return Err(Error::internal("a region band was answered clipped"));
            }
            for row in 0..rows {
                let block_row = ((y + row - rect.y0) / reduction) as usize * size[0] as usize;
                let values = &values[row as usize * rect.width as usize..][..rect.width as usize];
                for (column, value) in values.iter().enumerate() {
                    let sum = &mut sums[block_row + column / reduction as usize];
                    for channel in 0..3 {
                        sum[channel] += f64::from(value[channel]);
                    }
                }
            }
            y += rows;
        }
        let linear = sums
            .into_iter()
            .enumerate()
            .map(|(index, sum)| {
                let (bx, by) = (index as u32 % size[0], index as u32 / size[0]);
                let across = reduction.min(rect.width - bx * reduction);
                let down = reduction.min(rect.height - by * reduction);
                let count = f64::from(across) * f64::from(down);
                sum.map(|channel| channel / count)
            })
            .collect();
        Ok(Some(RegionRead {
            linear,
            answered: answered.unwrap_or_else(|| Answered::reference(None)),
            ..empty(Answered::reference(None))
        }))
    };
    match read_with(reads)? {
        Some(read) => Ok(read),
        None => read_with(&ReferenceReads)?
            .ok_or_else(|| Error::internal("the reference answered a region in two renderers")),
    }
}

/// `rect` widened to whole blocks of `reduction` on the stage's block grid and clipped to the
/// stage: the origin rounds down to a multiple of the reduction and the far edges round up, then
/// both are held inside the stage. Empty when `rect` misses the stage.
fn aligned(rect: Region, reduction: u32, stage: Stage) -> Region {
    let x1 = rect.x0.saturating_add(rect.width).min(stage.width);
    let y1 = rect.y0.saturating_add(rect.height).min(stage.height);
    let x0 = rect.x0.min(stage.width) / reduction * reduction;
    let y0 = rect.y0.min(stage.height) / reduction * reduction;
    let x1 = x1
        .div_ceil(reduction)
        .saturating_mul(reduction)
        .min(stage.width);
    let y1 = y1
        .div_ceil(reduction)
        .saturating_mul(reduction)
        .min(stage.height);
    if x1 <= x0 || y1 <= y0 {
        return Region {
            x0,
            y0,
            width: 0,
            height: 0,
        };
    }
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

/// How many stage rows one band holds: whole blocks, as many as keep the band within
/// [`BAND_PIXELS`], and at least one block.
fn band_rows(width: u32, reduction: u32) -> u32 {
    let blocks = (BAND_PIXELS / (u64::from(width) * u64::from(reduction)).max(1)).max(1);
    u32::try_from(blocks.saturating_mul(u64::from(reduction))).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(width: u32, height: u32) -> Stage {
        Stage { width, height }
    }

    #[test]
    fn a_region_widens_to_whole_blocks_and_stays_inside_the_stage() {
        let region = |x0, y0, width, height| Region {
            x0,
            y0,
            width,
            height,
        };
        assert_eq!(
            aligned(region(5, 9, 10, 3), 4, stage(100, 50)),
            region(4, 8, 12, 4)
        );
        // The far edge rounds up past the stage and is clipped back to it.
        assert_eq!(
            aligned(region(90, 40, 20, 20), 8, stage(100, 50)),
            region(88, 40, 12, 10)
        );
        assert_eq!(aligned(region(120, 0, 5, 5), 1, stage(100, 50)).width, 0);
    }

    #[test]
    fn a_band_holds_whole_blocks_within_its_pixel_bound() {
        assert_eq!(band_rows(1024, 1), 1024);
        assert_eq!(band_rows(1024, 16) % 16, 0);
        assert!(u64::from(band_rows(6000, 4)) * 6000 <= BAND_PIXELS);
        // A block wider than the bound still reads one block row.
        assert_eq!(band_rows(1 << 20, 64), 64);
    }
}
