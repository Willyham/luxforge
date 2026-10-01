//! The public locate and transform types, and the walks that answer them from a compilation
//! without reading a pixel.

use super::Compiled;
use super::map::{Affine, GeometryMap, MapError, StageSize, WarpStep};
use crate::{Error, ModuleRegistry, Recipe};
use serde::{Deserialize, Serialize};

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

/// Map one pixel of the output stage of a stack compiled against a `width` × `height` content stage
/// back to the content-stage pixel it shows: the source after EXIF orientation, the stage the first
/// layer receives. A point outside the output stage is a validation error naming that stage. Cost is
/// linear in the layer count, reusing the caller's compilation, and no frame is allocated, so the
/// canvas pick and the API query share one implementation. Source pixels are never needed.
pub(crate) fn locate(
    compiled: &Compiled,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
) -> Result<ContentPoint, Error> {
    if compiled
        .segments
        .iter()
        .any(|s| s.entry.as_ref().is_some_and(|e| e.has_warp()))
    {
        let mapping = transform_of(compiled, width, height)?;
        if x >= mapping.output.width || y >= mapping.output.height {
            return Err(Error::validation(format!(
                "point ({x}, {y}) is outside the {}x{} rendered image",
                mapping.output.width, mapping.output.height
            )));
        }
        let (cx, cy) = mapping
            .to_content(f64::from(x) + 0.5, f64::from(y) + 0.5)
            .map_err(|e| e.error())?;
        if cx < 0.0 || cy < 0.0 || cx >= f64::from(width) || cy >= f64::from(height) {
            return Err(MapError::Outside.error());
        }
        return Ok(ContentPoint {
            content_x: cx.floor() as u32,
            content_y: cy.floor() as u32,
            width,
            height,
        });
    }
    /// Walk one segment backwards, the same walk `pixel_in` makes to fetch a colour: the composed
    /// exact geometry unmaps to the segment's input frame by its integer inverse, and the boundary
    /// entering it maps that pixel back to the previous stage ([`super::Entry::locate`]): a
    /// resample takes the nearest pixel to the input coordinate its output pixel centre samples.
    fn walk(compiled: &Compiled, index: usize, x: u32, y: u32) -> Option<(u32, u32)> {
        let segment = &compiled.segments[index];
        if x >= segment.width || y >= segment.height {
            return None;
        }
        let (input_x, input_y) = segment.geometry.unmap(x, y);
        let Some(entry) = &segment.entry else {
            // The first segment reads the source, which is the content stage.
            return Some((input_x, input_y));
        };
        let (x, y) = entry.locate(input_x, input_y, compiled.segments[index - 1].stage());
        walk(compiled, index - 1, x, y)
    }
    let stage = compiled.stage();
    let (content_x, content_y) =
        walk(compiled, compiled.segments.len() - 1, x, y).ok_or_else(|| {
            Error::validation(format!(
                "point ({x}, {y}) is outside the {}x{} rendered image",
                stage.width, stage.height
            ))
        })?;
    Ok(ContentPoint {
        content_x,
        content_y,
        width,
        height,
    })
}

/// The whole geometry tail as one bounded map, both ways, without reading pixels.
/// Affine tails compose to one pair of matrices. Warp tails carry at most the admitted geometry
/// steps, evaluated by the same scalar functions as rendering and point sampling.
pub fn stage_transform(
    registry: &ModuleRegistry,
    width: u32,
    height: u32,
    recipe: &Recipe,
) -> Result<GeometryMap, Error> {
    transform_of(&registry.compile(width, height, recipe)?, width, height)
}

/// [`stage_transform`] of a stack already compiled against a `width` × `height` content stage.
pub(crate) fn transform_of(
    compiled: &Compiled,
    width: u32,
    height: u32,
) -> Result<GeometryMap, Error> {
    let mut steps = Vec::new();
    for segment in &compiled.segments {
        if let Some(entry) = &segment.entry {
            entry.mapping_steps(&mut steps);
        }
        let inverse = Affine::from_exact(segment.geometry).invert()?;
        if inverse != Affine::IDENTITY {
            steps.push(WarpStep::Affine(inverse.0));
        }
    }
    let stage = compiled.stage();
    GeometryMap::from_steps(
        StageSize { width, height },
        StageSize {
            width: stage.width,
            height: stage.height,
        },
        steps,
    )
}
