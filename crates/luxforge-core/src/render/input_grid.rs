//! One exact restoration-input grid per overlay worker. It contains values, never a retained
//! source, recipe or development. The downstream pointwise suffix is evaluated per cell.
use super::{
    Byte, Compiled, Entry, Evaluation, PixelDomain, RenderContext, RenderSource, SpatialMode,
    color_runs, linear::Linear, spatial::Tiling,
};
use crate::{
    Cancel, EffectStage, Error, GeometryMap, ModuleRegistry, Recipe, Region,
    analysis::{MaskInputGrid, cell_pixel},
};
use sha2::{Digest, Sha256};
use std::{borrow::Cow, collections::BTreeMap, sync::Arc};

pub const INPUT_GRID_MAX_CELLS: u64 = 8_000_000;

/// The bound layer and the output-stage coordinates sampled by one input grid.
#[derive(Clone, Copy)]
pub(crate) struct GridRequest<'a> {
    pub layer: usize,
    pub transform: &'a GeometryMap,
    pub region: Region,
    pub cells: (u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InputGridKey {
    source: String,
    prefix: [u8; 32],
    wide: bool,
    transform: String,
    region: Region,
    cells: (u32, u32),
}
#[derive(Clone, Debug)]
enum Values {
    Byte(Arc<Vec<[u16; 3]>>),
    Linear(Arc<Vec<[f32; 3]>>),
}
#[derive(Clone, Debug)]
struct InputGrid {
    key: InputGridKey,
    values: Values,
    outside: Arc<Vec<u8>>,
}

/// At most one grid, bounded by `INPUT_GRID_MAX_CELLS`. No cached value owns source buffers.
#[derive(Default)]
pub struct InputGridCache {
    grid: Option<InputGrid>,
}
impl InputGridCache {
    pub fn clear(&mut self) {
        self.grid = None;
    }
    pub fn cells(&self) -> usize {
        self.grid
            .as_ref()
            .map_or(0, |g| (g.key.cells.0 as usize) * (g.key.cells.1 as usize))
    }
    pub fn bytes(&self) -> usize {
        self.grid.as_ref().map_or(0, |g| match &g.values {
            Values::Byte(v) => v.len() * 6 + g.outside.len(),
            Values::Linear(v) => v.len() * 12 + g.outside.len(),
        })
    }
}

pub(crate) struct GridInput {
    grid: InputGrid,
    suffix: Compiled,
    mode: super::MaskInputMode,
}
impl std::fmt::Debug for GridInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GridInput")
            .field("key", &self.grid.key)
            .finish_non_exhaustive()
    }
}
impl MaskInputGrid for GridInput {
    fn linear_cell(&self, cell: usize, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        if self.grid.outside[cell / 8] & (1 << (cell % 8)) != 0 {
            return Ok(None);
        }
        let segment = &self.suffix.segments[0];
        let Some(resolved) = segment.resolve(x, y) else {
            return Ok(None);
        };
        let after = resolved.replacement.map_or(0, |(index, _)| index + 1);
        macro_rules! suffix {
            ($domain:ty, $pixel:expr) => {{
                let mut pixel = $pixel;
                if let Some((_, rgb)) = resolved.replacement {
                    pixel = <$domain>::replace(pixel, rgb);
                }
                if self.mode == super::MaskInputMode::ColourRun {
                    return super::pipeline::colour_input::<$domain>(
                        pixel,
                        color_runs(segment).filter(|run| run.start >= after),
                        x,
                        y,
                        self.grid.key.wide && !segment.has_pixels,
                    )
                    .map(Some);
                }
                if segment.has_color {
                    pixel = <$domain>::colour(
                        pixel,
                        color_runs(segment).filter(|run| run.start >= after),
                        x,
                        y,
                        self.grid.key.wide && !segment.has_pixels,
                    )?;
                }
                let pixel =
                    <$domain>::finish_width(pixel, self.grid.key.wide && !segment.has_pixels)?;
                Ok(Some(<$domain>::linear(pixel)))
            }};
        }
        match &self.grid.values {
            Values::Byte(values) => suffix!(Byte<'_>, values[cell]),
            Values::Linear(values) => suffix!(Linear<'_>, values[cell].map(f64::from)),
        }
    }
}

/// The leading restoration run is the only spatial prefix a value-based overlay may materialize.
/// A later spatial or geometry boundary retains the existing explicit refusal.
pub(crate) fn grid_input(
    registry: &ModuleRegistry,
    source: RenderSource<'_>,
    recipe: &Recipe,
    request: GridRequest<'_>,
    cancel: &Cancel,
    context: &RenderContext,
    cache: &mut InputGridCache,
) -> Result<Option<GridInput>, Error> {
    let GridRequest {
        layer,
        transform,
        region,
        cells,
    } = request;
    let mode = super::MaskInputMode::for_layer(registry, recipe, layer);
    cancel.check()?;
    let prefix = crate::editor::prefix(&recipe.layers, layer)?;
    let mut split = 0;
    let mut restoration = false;
    for (index, held) in prefix.iter().enumerate() {
        match registry.effect_stage(&held.effect_id) {
            Some(EffectStage::Source | EffectStage::Pixel) if !restoration => {}
            Some(EffectStage::Restoration) => {
                restoration = true;
                split = index + 1;
            }
            _ => break,
        }
    }
    if !restoration {
        return Ok(None);
    }
    let count = u64::from(cells.0) * u64::from(cells.1);
    if count > INPUT_GRID_MAX_CELLS {
        return Err(Error::resource_limit(format!(
            "the input grid exceeds INPUT_GRID_MAX_CELLS ({INPUT_GRID_MAX_CELLS})"
        )));
    }
    let (width, height) = source.dimensions();
    let compile = |layers: &[crate::Layer]| {
        registry.compile_layers(
            width,
            height,
            layers,
            &recipe.masks,
            &recipe.strokes,
            &recipe.artifacts,
        )
    };
    let full = compile(&recipe.layers)?;
    let suffix = compile(&prefix[split..])?;
    if suffix.segments.len() != 1 || suffix.segments[0].entry.is_some() {
        return Err(Error::resource_limit(
            "a spatial layer before the masked one means reading the pixel it receives evaluates a spatial tile per grid cell",
        ));
    }
    let restored = compile(&prefix[..split])?;
    let wide = full.prefix_spatial_input_wide(&restored);
    if !restored.evaluates_spatial() {
        return Ok(None);
    }
    // Include only masks the restoration prefix applies through. Editing the inspected mask or
    // a Basic/curve/mixer suffix must reuse the same restoration values.
    let bound_masks: Vec<_> = recipe
        .masks
        .iter()
        .filter(|mask| {
            prefix[..split]
                .iter()
                .any(|layer| layer.mask.as_ref() == Some(&mask.id))
        })
        .collect();
    let hash = Sha256::digest(
        serde_json::to_vec(&(&prefix[..split], bound_masks))
            .map_err(|e| Error::internal(e.to_string()))?,
    );
    let source_key = match source {
        RenderSource::Byte(image) => format!(
            "jpeg:{}:{}:{}:{}",
            image.fingerprint, image.width, image.height, image.orientation
        ),
        RenderSource::Linear { image, settings } => format!(
            "raw:{}:{}:{:?}:{:?}",
            image.fingerprint(),
            image.development(),
            image.view(),
            settings
        ),
    };
    let key = InputGridKey {
        source: source_key,
        prefix: hash.into(),
        wide,
        transform: transform.sha256().to_owned(),
        region,
        cells,
    };
    // Source Debug includes buffer metadata only (fingerprint/development/settings), never pixels.
    if let Some(grid) = &cache.grid
        && grid.key == key
    {
        return Ok(Some(GridInput {
            grid: grid.clone(),
            suffix,
            mode,
        }));
    }
    cancel.check()?;
    // A replacement never retains two grids: release the obsolete values before building.
    cache.grid = None;
    let values = match source {
        RenderSource::Byte(image) => Values::Byte(Arc::new(build(
            Byte(image),
            restored,
            request,
            cancel,
            context,
            wide,
            |p| p,
        )?)),
        RenderSource::Linear { image, settings } => {
            let pixels = build(
                Linear::new(image, settings)?,
                restored,
                request,
                cancel,
                context,
                wide,
                |p| p.map(|v| v as f32),
            )?;
            Values::Linear(Arc::new(pixels))
        }
    };
    let mut outside = vec![0; (count as usize).div_ceil(8)];
    for cell in 0..count as usize {
        if cell % 1024 == 0 {
            cancel.check()?;
        }
        if coordinate(transform, region, cells, cell)?.is_none() {
            outside[cell / 8] |= 1 << (cell % 8);
        }
    }
    cancel.check()?;
    let grid = InputGrid {
        key,
        values,
        outside: Arc::new(outside),
    };
    cache.grid = Some(grid.clone());
    Ok(Some(GridInput { grid, suffix, mode }))
}

fn coordinate(
    transform: &GeometryMap,
    region: Region,
    cells: (u32, u32),
    cell: usize,
) -> Result<Option<(u32, u32)>, Error> {
    let x = region.x0 + cell_pixel(cell as u32 % cells.0, region.width, cells.0);
    let y = region.y0 + cell_pixel(cell as u32 / cells.0, region.height, cells.1);
    let ox = f64::from(x) + 0.5;
    let oy = f64::from(y) + 0.5;
    let (px, py) = transform.to_content(ox, oy).map_err(|e| e.error())?;
    let (px, py) = (px.floor(), py.floor());
    Ok((px.is_finite()
        && py.is_finite()
        && px >= 0.0
        && py >= 0.0
        && px < f64::from(transform.content.width)
        && py < f64::from(transform.content.height))
    .then_some((px as u32, py as u32)))
}

fn build<D: PixelDomain, P: Copy>(
    domain: D,
    compiled: Compiled,
    request: GridRequest<'_>,
    cancel: &Cancel,
    context: &RenderContext,
    wide: bool,
    keep: impl Fn(D::Pixel) -> P,
) -> Result<Vec<P>, Error> {
    let GridRequest {
        transform,
        region,
        cells,
        ..
    } = request;
    let count = cells.0 as usize * cells.1 as usize;
    let tile = compiled
        .segments
        .iter()
        .rev()
        .find_map(|s| match &s.entry {
            Some(Entry::Spatial(entry)) => Some(Tiling::Halo.tile(&entry.operation, s.stage())),
            _ => None,
        })
        .unwrap_or(512);
    let evaluation = Evaluation::new(
        domain,
        Cow::Owned(compiled),
        Tiling::Halo,
        SpatialMode::Point,
        cancel,
        context,
    )?
    .with_input_width(wide);
    let empty = D::spatial_output([0.0; 3], true)?;
    let mut output = vec![keep(empty); count];
    // Group cells by the stage-aligned tile, so a dense grid computes each touched tile once.
    let mut groups: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    for cell in 0..count {
        if cell % 1024 == 0 {
            cancel.check()?;
        }
        if let Some((x, y)) = coordinate(transform, region, cells, cell)? {
            groups
                .entry((y / tile, x / tile))
                .or_default()
                .push(cell as u32);
        }
    }
    let stage = evaluation.stage();
    let halo: u32 = evaluation
        .compiled
        .segments
        .iter()
        .filter_map(|segment| match &segment.entry {
            Some(Entry::Spatial(entry)) => Some(entry.operation.summed_halo(segment.stage())),
            _ => None,
        })
        .sum();
    let window_area = (u64::from(halo) * 2 + 1).pow(2).max(1);
    for (&(ty, tx), group) in &groups {
        cancel.check()?;
        let rect = Region {
            x0: tx * tile,
            y0: ty * tile,
            width: tile.min(stage.width - tx * tile),
            height: tile.min(stage.height - ty * tile),
        };
        if group.len() as u64 >= rect.pixels() / window_area {
            let pixels = evaluation.restoration_region(rect)?;
            for (index, &cell) in group.iter().enumerate() {
                if index % 1024 == 0 {
                    cancel.check()?;
                }
                let (x, y) = coordinate(transform, region, cells, cell as usize)?.unwrap();
                output[cell as usize] =
                    keep(pixels[((y - rect.y0) * rect.width + (x - rect.x0)) as usize]);
            }
        } else {
            for &cell in group {
                cancel.check()?;
                let (x, y) = coordinate(transform, region, cells, cell as usize)?.unwrap();
                output[cell as usize] = keep(
                    evaluation.restoration_region(Region {
                        x0: x,
                        y0: y,
                        width: 1,
                        height: 1,
                    })?[0],
                );
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Layer, LinearImage, LinearSettings, SourceImage, StageSize};
    use serde_json::json;

    fn source() -> SourceImage {
        let (width, height) = (600, 400);
        let rgba = (0..width * height)
            .flat_map(|i| {
                let value = (40 + (i * 37 + i / width * 13) % 150) as u8;
                [
                    value,
                    value.saturating_add((i % 17) as u8),
                    value.saturating_sub((i % 9) as u8),
                    255,
                ]
            })
            .collect::<Vec<_>>();
        SourceImage {
            width,
            height,
            rgba: Arc::new(rgba),
            fingerprint: "input-grid-fixture".into(),
            orientation: 1,
            capture: Default::default(),
        }
    }
    fn identity(width: u32, height: u32) -> GeometryMap {
        GeometryMap::affine(
            StageSize { width, height },
            StageSize { width, height },
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        )
    }
    fn recipe() -> Recipe {
        Recipe {
            layers: vec![
                Layer::new(
                    crate::DETAIL_EFFECT,
                    json!({"luminance":35.0,"colour":25.0}),
                ),
                Layer::new(crate::BASIC_EFFECT, json!({"exposure":0.7})),
            ],
            ..Recipe::default()
        }
    }
    fn exact<'a>(
        registry: &ModuleRegistry,
        source: RenderSource<'a>,
        recipe: &Recipe,
        context: &'a RenderContext,
    ) -> Box<dyn crate::render::StagePixels + 'a> {
        let (width, height) = source.dimensions();
        let compiled = registry.compile(width, height, recipe).unwrap();
        crate::render::prefix_pixels(
            source,
            compiled,
            context,
            &Cancel::never(),
            true,
            crate::render::MaskInputMode::Boundary,
        )
        .unwrap()
    }

    #[test]
    fn detail_input_grid_equals_the_exact_prefix_on_both_domains_at_dense_and_sparse_cells() {
        let registry = ModuleRegistry::builtin();
        let jpeg = source();
        let recipe = recipe();
        let context = RenderContext::new();
        let mut planes = Vec::new();
        for channel in 0..3 {
            planes.extend(
                (0..jpeg.width * jpeg.height)
                    .map(|i| f32::from(jpeg.rgba[i as usize * 4 + channel]) / 255.0 - 0.05),
            );
        }
        let raw = LinearImage::with_fingerprint(jpeg.width, jpeg.height, planes, "input-grid-raw")
            .unwrap();
        for source in [
            RenderSource::Byte(&jpeg),
            RenderSource::Linear {
                image: &raw,
                settings: LinearSettings::default(),
            },
        ] {
            let reference = exact(&registry, source, &recipe, &context);
            for cells in [(3, 2), (40, 30)] {
                let transform = identity(jpeg.width, jpeg.height);
                let region = Region {
                    x0: 0,
                    y0: 0,
                    width: jpeg.width,
                    height: jpeg.height,
                };
                let mut cache = InputGridCache::default();
                let grid = grid_input(
                    &registry,
                    source,
                    &recipe,
                    GridRequest {
                        layer: 2,
                        transform: &transform,
                        region,
                        cells,
                    },
                    &Cancel::never(),
                    &context,
                    &mut cache,
                )
                .unwrap()
                .unwrap();
                for cell in 0..cells.0 as usize * cells.1 as usize {
                    let (x, y) = coordinate(&transform, region, cells, cell)
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        grid.linear_cell(cell, x, y).unwrap(),
                        reference.linear(x, y).unwrap(),
                        "cell {cell}, grid {cells:?}, domain {}",
                        if matches!(source, RenderSource::Byte(_)) {
                            "byte"
                        } else {
                            "linear"
                        }
                    );
                }
                assert_eq!(cache.cells(), cells.0 as usize * cells.1 as usize);
            }
        }
    }

    #[test]
    fn detail_input_grid_tracks_warped_content_cells_and_lens_only_cache_changes() {
        let registry = ModuleRegistry::builtin();
        let jpeg = source();
        let context = RenderContext::new();
        let raw = LinearImage::with_fingerprint(
            jpeg.width,
            jpeg.height,
            (0..3)
                .flat_map(|channel| {
                    let jpeg = &jpeg;
                    (0..jpeg.width * jpeg.height)
                        .map(move |i| f32::from(jpeg.rgba[i as usize * 4 + channel]) / 255.0 - 0.05)
                })
                .collect::<Vec<_>>(),
            "warped-input-grid-raw",
        )
        .unwrap();
        let mut recipe = recipe();
        recipe.layers.extend([
            Layer::new(crate::BASIC_EFFECT, json!({"exposure":0.2})),
            Layer::orientation(crate::Orientation::of(crate::Transform::RotateRight)),
            super::super::testing::frozen_lens(jpeg.height, jpeg.width, 24.0),
            Layer::new(
                crate::PERSPECTIVE_EFFECT,
                json!({"horizontal":35,"vertical":-20}),
            ),
        ]);
        let mut mask = crate::Mask::new("Warped range");
        mask.components.push(crate::Component::new(
            "Range",
            crate::ComponentMode::Add,
            "luminance-range",
            json!({"low":15.0,"high":80.0,"low_feather":5.0,"high_feather":5.0}),
        ));
        recipe.layers[2].mask = Some(mask.id.clone());
        recipe.masks.push(mask);
        for source in [
            RenderSource::Byte(&jpeg),
            RenderSource::Linear {
                image: &raw,
                settings: LinearSettings::default(),
            },
        ] {
            for cells in [(3, 2), (40, 30)] {
                let mut held = recipe.clone();
                let mut cache = InputGridCache::default();
                let mut first = None;
                for focal in [24.0, 35.0] {
                    held.layers[4] =
                        super::super::testing::frozen_lens(jpeg.height, jpeg.width, focal);
                    let full = registry.compile(jpeg.width, jpeg.height, &held).unwrap();
                    let transform =
                        super::super::transform_of(&full, jpeg.width, jpeg.height).unwrap();
                    let prefix = registry
                        .compile_layers(
                            jpeg.width,
                            jpeg.height,
                            &held.layers[..2],
                            &held.masks,
                            &held.strokes,
                            &held.artifacts,
                        )
                        .unwrap();
                    let reference = crate::render::prefix_pixels(
                        source,
                        prefix.clone(),
                        &context,
                        &Cancel::never(),
                        full.prefix_spatial_input_wide(&prefix),
                        super::super::MaskInputMode::ColourRun,
                    )
                    .unwrap();
                    let region = Region {
                        x0: 0,
                        y0: 0,
                        width: transform.output.width,
                        height: transform.output.height,
                    };
                    let request = GridRequest {
                        layer: 2,
                        transform: &transform,
                        region,
                        cells,
                    };
                    let grid = grid_input(
                        &registry,
                        source,
                        &held,
                        request,
                        &Cancel::never(),
                        &context,
                        &mut cache,
                    )
                    .unwrap()
                    .unwrap();
                    for cell in 0..cells.0 as usize * cells.1 as usize {
                        let ox = cell_pixel(cell as u32 % cells.0, region.width, cells.0);
                        let oy = cell_pixel(cell as u32 / cells.0, region.height, cells.1);
                        let (x, y) = transform
                            .to_content(f64::from(ox) + 0.5, f64::from(oy) + 0.5)
                            .unwrap();
                        let (x, y) = (x.floor() as u32, y.floor() as u32);
                        assert_eq!(
                            grid.linear_cell(cell, x, y).unwrap(),
                            reference.linear(x, y).unwrap(),
                            "warped cell {cell}, grid {cells:?}, focal {focal}"
                        );
                    }
                    assert_eq!(grid.grid.key.transform, transform.sha256());
                    if let Some(first) = &first {
                        assert_ne!(grid.grid.key, *first, "a Lens-only edit rebuilds the grid");
                    } else {
                        first = Some(grid.grid.key.clone());
                    }
                    held.masks[0].amount = 0.5;
                    let reused = grid_input(
                        &registry,
                        source,
                        &held,
                        request,
                        &Cancel::never(),
                        &context,
                        &mut cache,
                    )
                    .unwrap()
                    .unwrap();
                    match (&grid.grid.values, &reused.grid.values) {
                        (Values::Byte(a), Values::Byte(b)) => assert!(Arc::ptr_eq(a, b)),
                        (Values::Linear(a), Values::Linear(b)) => assert!(Arc::ptr_eq(a, b)),
                        _ => panic!("the grid's source domain changed"),
                    }
                }
            }
        }
    }

    #[test]
    fn detail_input_grid_before_second_detail_uses_the_first_wide_handoff() {
        let registry = ModuleRegistry::builtin();
        let jpeg = source();
        let context = RenderContext::new();
        let mut recipe = recipe();
        let mut mask = crate::Mask::new("Second Detail");
        mask.components.push(crate::Component::new(
            "Range",
            crate::ComponentMode::Add,
            "luminance-range",
            json!({"low":10.0,"high":80.0,"low_feather":5.0,"high_feather":5.0}),
        ));
        recipe.layers[1] = Layer::new(crate::DETAIL_EFFECT, json!({"sharpening":35.0}));
        recipe.layers[1].mask = Some(mask.id.clone());
        recipe.masks.push(mask);
        let compiled = registry.compile(jpeg.width, jpeg.height, &recipe).unwrap();
        assert!(
            !compiled.prefix_spatial_input_wide(&compiled),
            "the final Detail is terminal and narrow"
        );
        let prefix = registry
            .compile_layers(
                jpeg.width,
                jpeg.height,
                &recipe.layers[..1],
                &recipe.masks,
                &recipe.strokes,
                &recipe.artifacts,
            )
            .unwrap();
        assert!(
            compiled.prefix_spatial_input_wide(&prefix),
            "the first Detail feeds another spatial unit"
        );
        let boundary = prefix.restoration_boundary().unwrap();
        let (frame, stage) = super::super::byte::frames(
            &jpeg,
            &compiled,
            &Cancel::never(),
            Tiling::Halo,
            &context,
            Some(boundary),
            None,
        )
        .unwrap();
        assert!(frame.wide());
        let transform = identity(jpeg.width, jpeg.height);
        let region = Region {
            x0: 0,
            y0: 0,
            width: jpeg.width,
            height: jpeg.height,
        };
        let cells = (4, 3);
        let mut cache = InputGridCache::default();
        let grid = grid_input(
            &registry,
            RenderSource::Byte(&jpeg),
            &recipe,
            GridRequest {
                layer: 1,
                transform: &transform,
                region,
                cells,
            },
            &Cancel::never(),
            &context,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        for cell in 0..cells.0 as usize * cells.1 as usize {
            let (x, y) = coordinate(&transform, region, cells, cell)
                .unwrap()
                .unwrap();
            assert_eq!(
                grid.linear_cell(cell, x, y).unwrap(),
                Some(Byte::linear(frame.pixel(stage, x, y)))
            );
        }
    }

    #[test]
    fn detail_input_grid_uses_the_full_recipe_byte_boundary_width() {
        let registry = ModuleRegistry::developer();
        let jpeg = source();
        let context = RenderContext::new();
        let mut recipe = recipe();
        let transform = identity(jpeg.width, jpeg.height);
        let region = Region {
            x0: 0,
            y0: 0,
            width: jpeg.width,
            height: jpeg.height,
        };
        let cells = (4, 3);
        let mut cache = InputGridCache::default();
        let mut keys = Vec::new();
        for wide in [true, false] {
            if !wide {
                recipe.layers.push(Layer::pixel(0, 0, [12, 34, 56]));
            }
            let compiled = registry.compile(jpeg.width, jpeg.height, &recipe).unwrap();
            assert_eq!(compiled.prefix_spatial_input_wide(&compiled), wide);
            let boundary = compiled.restoration_boundary().unwrap();
            let (frame, stage) = super::super::byte::frames(
                &jpeg,
                &compiled,
                &Cancel::never(),
                Tiling::Halo,
                &context,
                Some(boundary),
                None,
            )
            .unwrap();
            assert_eq!(frame.wide(), wide);
            let grid = grid_input(
                &registry,
                RenderSource::Byte(&jpeg),
                &recipe,
                GridRequest {
                    layer: 1,
                    transform: &transform,
                    region,
                    cells,
                },
                &Cancel::never(),
                &context,
                &mut cache,
            )
            .unwrap()
            .unwrap();
            for cell in 0..cells.0 as usize * cells.1 as usize {
                let (x, y) = coordinate(&transform, region, cells, cell)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    grid.linear_cell(cell, x, y).unwrap(),
                    Some(Byte::linear(frame.pixel(stage, x, y))),
                    "wide={wide}, cell={cell}"
                );
            }
            keys.push(grid.grid.key);
            let prefix = registry
                .compile_layers(
                    jpeg.width,
                    jpeg.height,
                    &recipe.layers[..2],
                    &recipe.masks,
                    &recipe.strokes,
                    &recipe.artifacts,
                )
                .unwrap();
            let point = crate::render::prefix_pixels(
                RenderSource::Byte(&jpeg),
                prefix,
                &context,
                &Cancel::never(),
                wide,
                crate::render::MaskInputMode::Boundary,
            )
            .unwrap();
            let suffix_grid = grid_input(
                &registry,
                RenderSource::Byte(&jpeg),
                &recipe,
                GridRequest {
                    layer: 2,
                    transform: &transform,
                    region,
                    cells,
                },
                &Cancel::never(),
                &context,
                &mut cache,
            )
            .unwrap()
            .unwrap();
            for cell in 0..cells.0 as usize * cells.1 as usize {
                let (x, y) = coordinate(&transform, region, cells, cell)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    suffix_grid.linear_cell(cell, x, y).unwrap(),
                    point.linear(x, y).unwrap(),
                    "suffix point/grid, wide={wide}, cell={cell}"
                );
            }
        }
        assert_ne!(
            keys[0], keys[1],
            "the precision choice invalidates retained grid values"
        );
    }

    #[test]
    fn detail_input_grid_reuses_downstream_changes_and_invalidates_prefix_geometry_and_source() {
        let registry = ModuleRegistry::builtin();
        let jpeg = source();
        let context = RenderContext::new();
        let mut recipe = recipe();
        let transform = identity(jpeg.width, jpeg.height);
        let region = Region {
            x0: 0,
            y0: 0,
            width: jpeg.width,
            height: jpeg.height,
        };
        let mut cache = InputGridCache::default();
        let first = grid_input(
            &registry,
            RenderSource::Byte(&jpeg),
            &recipe,
            GridRequest {
                layer: 2,
                transform: &transform,
                region,
                cells: (4, 3),
            },
            &Cancel::never(),
            &context,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        recipe.layers[1].payload = json!({"exposure":1.1});
        let next = grid_input(
            &registry,
            RenderSource::Byte(&jpeg),
            &recipe,
            GridRequest {
                layer: 2,
                transform: &transform,
                region,
                cells: (4, 3),
            },
            &Cancel::never(),
            &context,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        let (Values::Byte(a), Values::Byte(b)) = (&first.grid.values, &next.grid.values) else {
            panic!()
        };
        assert!(
            Arc::ptr_eq(a, b),
            "colour suffix edits reuse restoration values"
        );
        recipe.layers[0].payload = json!({"luminance":60.0});
        let changed = grid_input(
            &registry,
            RenderSource::Byte(&jpeg),
            &recipe,
            GridRequest {
                layer: 2,
                transform: &transform,
                region,
                cells: (4, 3),
            },
            &Cancel::never(),
            &context,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_ne!(first.grid.key, changed.grid.key);
        let moved = GeometryMap::affine(
            transform.content,
            transform.output,
            [1.0, 0.0, 1.0, 0.0, 1.0, 0.0],
            [1.0, 0.0, -1.0, 0.0, 1.0, 0.0],
        );
        let geometry = grid_input(
            &registry,
            RenderSource::Byte(&jpeg),
            &recipe,
            GridRequest {
                layer: 2,
                transform: &moved,
                region,
                cells: (4, 3),
            },
            &Cancel::never(),
            &context,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_ne!(changed.grid.key, geometry.grid.key);
        let mut replaced = jpeg.clone();
        replaced.fingerprint = "different-source".into();
        let source_change = grid_input(
            &registry,
            RenderSource::Byte(&replaced),
            &recipe,
            GridRequest {
                layer: 2,
                transform: &transform,
                region,
                cells: (4, 3),
            },
            &Cancel::never(),
            &context,
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_ne!(changed.grid.key, source_change.grid.key);
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(
            grid_input(
                &registry,
                RenderSource::Byte(&jpeg),
                &recipe,
                GridRequest {
                    layer: 2,
                    transform: &transform,
                    region,
                    cells: (4, 3)
                },
                &cancel,
                &context,
                &mut cache
            )
            .is_err()
        );
        assert!(
            grid_input(
                &registry,
                RenderSource::Byte(&jpeg),
                &recipe,
                GridRequest {
                    layer: 2,
                    transform: &transform,
                    region,
                    cells: (4000, 4000)
                },
                &Cancel::never(),
                &context,
                &mut cache
            )
            .unwrap_err()
            .detail
            .contains("INPUT_GRID_MAX_CELLS")
        );
        recipe.layers.insert(
            1,
            Layer::new(crate::PRESENCE_EFFECT, json!({"texture":20.0})),
        );
        assert!(
            grid_input(
                &registry,
                RenderSource::Byte(&jpeg),
                &recipe,
                GridRequest {
                    layer: 3,
                    transform: &transform,
                    region,
                    cells: (4, 3)
                },
                &Cancel::never(),
                &context,
                &mut cache
            )
            .unwrap_err()
            .detail
            .contains("spatial tile per grid cell")
        );
    }
}
