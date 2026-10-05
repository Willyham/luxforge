//! The light link (`docs/design/gpu-preview.md`, "The global estimate"): a global estimate —
//! Dehaze's atmospheric light — computed on the GPU per frame from its whole input stage at full
//! resolution, as the measurement of the per-frame light's input set it (`docs/specs/
//! performance.md`, "The per-frame light's reduction factor").
//!
//! - **What it runs.** A light link ([`GpuLight`]) is a link of steps like any other: the prefix's
//!   colour and masked colour steps, then one spatial step of no apply, the estimating unit's light
//!   passes. Its sequence compiles through the stage's own compile path, on the compile thread,
//!   and its passes are the chain's own pass modules, so `lf_source` runs the colour steps at each
//!   texel the reduction reads, clamped where the byte path clamps, with nothing held between them.
//! - **Tiles.** Its input is the whole content stage, cut from the source the pipeline holds at
//!   full scale ([`super::Derivation::Cut`]), one tile of [`LIGHT_TILE`] pixels at a time into one
//!   texture, row by row; a stage held in two source textures across or down is cut across their
//!   seam as any boundary is. Each tile's reduction writes the stage's 16-pixel blocks it holds,
//!   and no other, into the whole stage's block plane: the tiles' origins are multiples of the
//!   block, so every block is one tile's, read in the order the whole stage would read it, and the
//!   block plane is the same, bit for bit, however the stage is tiled. Then one workgroup selects
//!   the light from the block plane into the slot's light plane ([`spatial::PlaneSize::Light`]).
//! - **Keys.** The light plane keeps the content key of what its light link wrote — the source,
//!   the stage, the steps' words and blocks and the pipeline — and a link whose key did not change
//!   encodes nothing. A step reading the light folds that key into its passes' keys and draws whole
//!   when it changes ([`spatial::GpuSpatial::global`]), the slot evaluating its plan whole then
//!   ([`PhotoPipeline::evaluate_lit`]).
//! - **Bounds.** A link holds the tile texture, the stage's block plane, every tile's words and
//!   cut's words, the blocks and the passes' parameters, charged to the GPU-preview budget before
//!   they are created ([`light_charge`]); the light plane is the slot's pool's, one texel. Nothing
//!   is read back and nothing waits for the GPU: the light is never planned from.
//!
//! - **Where it runs.** A slot evaluating a plan that reads lights ([`super::GpuPlan::lights`]) runs
//!   each light link first, in the same frame, into its pool's light planes
//!   ([`PhotoPipeline::evaluate_lit`]): the main slot's plan and every tile of a picture at rest
//!   alike. A link whose light did not change encodes nothing; a light that changed makes the slot
//!   evaluate the plan whole, every link reading it drawn again.
use super::super::{PhotoPipeline, SurfaceSlots};
use super::{
    BLOCK_CHUNK, BoundaryFormat, Charged, Compiled, GpuFallback, GpuStep, Held, TexelMap,
    blocks::{self, WrittenBlocks},
    buffer_capacity, chain,
    source::Derivation,
    spatial::{self, GpuSpatial, Place, PlaneSize, Pool, PoolTexture, Rect},
    storage_buffer,
};
use std::sync::atomic::Ordering;

/// The side, in stage pixels, of the tiles a light link cuts its stage into: each tile is cut from
/// the source into one texture of this side in turn, so a link holds one tile's texels whatever the
/// stage, 32 MiB of a JPEG's half floats and 64 MiB of a RAW's `f32` at most. A multiple of every
/// block side a reduction reads, so each block of the stage's grid lies in one tile. On a device
/// whose largest texture is smaller, that side rounded down to a multiple of the block.
pub const LIGHT_TILE: u32 = 2048;

/// The format a light link's sequence is compiled to write: its frame is never drawn, and its last
/// pass writes linear values, as a qualification's does, which needs no output encoding.
pub(super) const LIGHT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

/// A light link the surface evaluates: plain data, as a plan is, so this crate names no core type.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuLight {
    /// The content stage it reduces, whole and at full scale: the source's.
    pub stage: (u32, u32),
    /// Its steps: the prefix's colour and masked colour steps, run per texel over the source's
    /// texels, then its own spatial step, which draws nothing: the reduction into the stage's block
    /// plane, a [`PlaneSize::Reduced`] plane of the whole stage, and the selection into the light
    /// plane it declares ([`PlaneSize::Light`]).
    pub steps: Vec<GpuStep>,
}

impl GpuLight {
    /// Its spatial step, the last, when the steps are a light link's: colour and masked colour
    /// steps, then one spatial step of no apply whose first pass writes a reduced plane, its block
    /// plane, and whose last writes the light plane it declares.
    fn step(&self) -> Option<&GpuSpatial> {
        let (last, before) = self.steps.split_last()?;
        let GpuStep::Spatial(spatial) = last else {
            return None;
        };
        let colour = before
            .iter()
            .all(|step| matches!(step, GpuStep::Colour { .. } | GpuStep::Masked(_)));
        let first = spatial.passes.first()?;
        let last = spatial.passes.last()?;
        let blocks = matches!(
            spatial.planes.get(first.output as usize)?.size,
            PlaneSize::Reduced(_)
        );
        let light = matches!(
            spatial.planes.get(last.output as usize)?.size,
            PlaneSize::Light(_)
        );
        (colour && blocks && light && spatial.applies.is_empty() && spatial.passes.len() == 2)
            .then_some(spatial)
    }

    /// The slot's light plane it writes, `k` of [`PlaneSize::Light`]; `None` for steps that are
    /// not a light link's.
    pub fn index(&self) -> Option<u32> {
        let spatial = self.step()?;
        let last = spatial.passes.last()?;
        match spatial.planes[last.output as usize].size {
            PlaneSize::Light(k) => Some(k),
            PlaneSize::Reduced(_) | PlaneSize::Fixed { .. } => None,
        }
    }

    /// The side of the blocks its reduction writes.
    fn block(&self) -> Option<u32> {
        let spatial = self.step()?;
        match spatial.planes[spatial.passes[0].output as usize].size {
            PlaneSize::Reduced(s) => Some(s.max(1)),
            PlaneSize::Fixed { .. } | PlaneSize::Light(_) => None,
        }
    }
}

/// The tile side a light link of block side `block` cuts its stage into on a device whose largest
/// texture is `limit`: [`LIGHT_TILE`], or the limit, rounded down to a multiple of the block.
fn tile_side(limit: u32, block: u32) -> u32 {
    (LIGHT_TILE.min(limit) / block * block).max(block)
}

/// The tiles of `stage` at `side`, row by row: each one's rectangle `[x0, y0, x1, y1)`.
fn tiles(stage: (u32, u32), side: u32) -> Vec<[u32; 4]> {
    let mut tiles = Vec::new();
    for y0 in (0..stage.1).step_by(side as usize) {
        for x0 in (0..stage.0).step_by(side as usize) {
            tiles.push([x0, y0, (x0 + side).min(stage.0), (y0 + side).min(stage.1)]);
        }
    }
    tiles
}

/// The bytes a tile's words take in the link's words buffer: a storage binding's offset alignment.
fn region(words: usize) -> u64 {
    ((words * 4) as u64).div_ceil(spatial::PARAMS_STRIDE) * spatial::PARAMS_STRIDE
}

/// What a light link of `light` over a source whose cut is held as `format` takes of the
/// GPU-preview budget on a device whose largest texture is `limit` and whose largest storage
/// binding is `binding`, as it is charged when created: the tile texture, the stage's block plane,
/// every tile's words, the blocks, every tile's cut's words and the passes' parameters. Beside it
/// the slot's pool holds the light plane, one texel ([`spatial::LIGHT_BYTES`]). What the desktop
/// holds a plan's light links to before any exists; it creates nothing. `None` for steps that are
/// not a light link's.
pub fn light_charge(
    light: &GpuLight,
    format: BoundaryFormat,
    limit: u32,
    binding: u64,
) -> Option<u64> {
    let shape = Shape::of(light, format, limit)?;
    Some(shape.texture_bytes() + shape.buffer_bytes(binding).ok()?)
}

/// What a link is created for, which decides every resource it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shape {
    stage: (u32, u32),
    format: BoundaryFormat,
    /// The tile side and the block side.
    side: u32,
    block: u32,
    /// The words of one tile and of the blocks.
    words: usize,
    blocks: usize,
}

impl Shape {
    /// Whether a link made for `self` holds what a light of `other` needs: everything but its
    /// blocks' words, which a stroke on a mask before the light grows every tick and which the
    /// link's blocks buffer grows to in place, as a chain's links grow theirs.
    fn holds(&self, other: &Self) -> bool {
        (self.stage, self.format, self.side, self.block, self.words)
            == (
                other.stage,
                other.format,
                other.side,
                other.block,
                other.words,
            )
    }

    fn of(light: &GpuLight, format: BoundaryFormat, limit: u32) -> Option<Self> {
        let block = light.block()?;
        let mut words = Vec::new();
        chain::pack_words(TexelMap::IDENTITY, (0, 0), &light.steps, &mut words);
        Some(Self {
            stage: light.stage,
            format,
            side: tile_side(limit, block),
            block,
            words: words.len(),
            blocks: blocks::block_len(&light.steps),
        })
    }

    fn tiles(&self) -> Vec<[u32; 4]> {
        tiles(self.stage, self.side)
    }

    /// The tile texture's extent: the side, or the stage where it is smaller.
    fn tile(&self) -> (u32, u32) {
        (self.side.min(self.stage.0), self.side.min(self.stage.1))
    }

    /// The block plane's extent: the stage's blocks.
    fn grid(&self) -> (u32, u32) {
        (
            self.stage.0.div_ceil(self.block),
            self.stage.1.div_ceil(self.block),
        )
    }

    /// The tile texture and the block plane.
    fn texture_bytes(&self) -> u64 {
        let (tile, grid) = (self.tile(), self.grid());
        u64::from(tile.0) * u64::from(tile.1) * self.format.texel_bytes() as u64
            + u64::from(grid.0) * u64::from(grid.1) * spatial::PlaneFormat::Quad.texel_bytes()
    }

    /// Each buffer's bytes at the capacity a device whose largest storage binding is `binding`
    /// gives it: every tile's words, the blocks, each tile's cut's words and the parameters.
    fn buffer_sizes(&self, binding: u64) -> Result<Vec<u64>, GpuFallback> {
        let capacity = |bytes: u64| -> Result<u64, GpuFallback> {
            let limit = binding & !3;
            if bytes > limit {
                return Err(GpuFallback::BufferLimit { bytes, limit });
            }
            Ok(bytes.max(super::MIN_BUFFER).next_power_of_two().min(limit))
        };
        let tiles = self.tiles().len() as u64;
        let mut sizes = vec![
            capacity(tiles * region(self.words))?,
            capacity((self.blocks * 4) as u64)?,
            capacity((tiles + 1) * spatial::PARAMS_STRIDE)?,
        ];
        for _ in 0..tiles {
            sizes.push(capacity(CUT_WORDS * 4)?);
        }
        Ok(sizes)
    }

    fn buffer_bytes(&self, binding: u64) -> Result<u64, GpuFallback> {
        Ok(self.buffer_sizes(binding)?.iter().sum())
    }
}

/// The words a cut's pass reads: the tile side, the held size, the content-to-held map and the
/// origin ([`super::source`]).
const CUT_WORDS: u64 = 11;

/// What a light link holds on the GPU, charged to the GPU-preview budget: the tile texture each tile
/// of the source is cut into in turn and the stage's block plane, which only its own passes read,
/// and its buffers; and the content key of the light it last wrote.
pub(in crate::photo_surface) struct LightLink {
    shape: Shape,
    /// One tile of the source at full scale, in the source's boundary format.
    tile: PoolTexture,
    /// The whole stage's block means beside each block's channel minimum, `rgba32float`.
    blocks: PoolTexture,
    /// Every tile's words, a storage binding's offset apart: the link's packed words with the texel
    /// map at the tile.
    words: Charged,
    written_words: Vec<u32>,
    /// The steps' storage blocks, the curve's knots or a brush's grid, written where they change.
    block_words: Charged,
    written_blocks: WrittenBlocks,
    /// Each tile's cut's words, the source's map and the tile's origin.
    cuts: Vec<Charged>,
    /// Each tile's reduction's parameters, then the selection's, a slice each.
    params: Charged,
    written_params: Vec<u32>,
    /// The bytes of its textures and of its buffers, charged.
    texture_bytes: u64,
    buffer_bytes: u64,
}

impl LightLink {
    /// Everything it holds, as charged.
    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn bytes(&self) -> u64 {
        self.texture_bytes + self.buffer_bytes
    }

    /// The block plane, for a readback.
    #[cfg(any(test, feature = "qualification"))]
    fn blocks(&self) -> (&wgpu::Texture, (u32, u32)) {
        (self.blocks.texture(), self.shape.grid())
    }
}

impl PhotoPipeline {
    /// A light link of `shape`, charged to the GPU-preview budget before anything is created: its
    /// textures as the scratch they are, which only its own passes read.
    fn light_link(&self, device: &wgpu::Device, shape: Shape) -> Result<LightLink, GpuFallback> {
        let limit = device.limits().max_texture_dimension_2d;
        let (tile, grid) = (shape.tile(), shape.grid());
        if [tile.0, tile.1, grid.0, grid.1]
            .iter()
            .any(|side| *side > limit)
        {
            return Err(GpuFallback::TextureLimit {
                width: grid.0.max(tile.0),
                height: grid.1.max(tile.1),
                limit,
            });
        }
        let binding = u64::from(device.limits().max_storage_buffer_binding_size);
        let sizes = shape.buffer_sizes(binding)?;
        let (texture_bytes, buffer_bytes) = (shape.texture_bytes(), sizes.iter().sum::<u64>());
        let preview = &self.figures.preview;
        preview.charge(texture_bytes + buffer_bytes)?;
        preview.scratch.fetch_add(texture_bytes, Ordering::AcqRel);
        let texture = |label, (width, height), format, usage| {
            PoolTexture::new(device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            }))
        };
        let charged = |label, bytes| Charged {
            buffer: storage_buffer(device, label, bytes),
            bytes,
        };
        let mut sizes = sizes.into_iter();
        let mut next = || sizes.next().expect("a size for every buffer");
        let words = charged("luxforge.gpu_light.words", next());
        let block_words = charged("luxforge.gpu_light.blocks", next());
        let params = charged("luxforge.gpu_light.params", next());
        let cuts = (0..shape.tiles().len())
            .map(|_| charged("luxforge.gpu_light.cut", next()))
            .collect();
        Ok(LightLink {
            tile: texture(
                "luxforge.gpu_light.tile",
                tile,
                shape.format.texture(),
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            ),
            blocks: texture(
                "luxforge.gpu_light.blocks",
                grid,
                spatial::PlaneFormat::Quad.texture(),
                BLOCK_USAGE,
            ),
            shape,
            words,
            written_words: Vec::new(),
            block_words,
            written_blocks: WrittenBlocks::default(),
            cuts,
            params,
            written_params: Vec::new(),
            texture_bytes,
            buffer_bytes,
        })
    }

    /// Retire `link` through the retirement worker, still charged until the GPU is done with it:
    /// its textures as the scratch they were charged as, and its buffers.
    pub(super) fn retire_light(&self, link: LightLink) {
        let LightLink {
            tile,
            blocks,
            words,
            block_words,
            cuts,
            params,
            texture_bytes,
            ..
        } = link;
        self.retire_preview(Held::Pool(vec![tile, blocks]), texture_bytes);
        for charged in [words, block_words, params].into_iter().chain(cuts) {
            self.retire_preview(Held::Buffer(charged.buffer), charged.bytes);
        }
    }

    /// Encode `light`'s passes on `encoder`, when the light plane it writes does not hold its light
    /// already: each tile of its stage cut from the source the pipeline holds and reduced into the
    /// stage's block plane, then the selection of the light into `pool`'s light plane, whose key it
    /// records. `link` is fitted to the light first, a link of another shape retiring; `pool` must
    /// hold the light plane, as fitting it to a plan whose links read the light, or to the light
    /// itself ([`spatial::PoolKey::with_lights`]), makes it. Answers the light plane's key, the
    /// content key of the light it holds.
    ///
    /// Names why it encodes nothing: the stage cannot run, the light's sequence is compiling
    /// ([`GpuFallback::Compiling`]), the source is still uploading or is not the light's whole
    /// stage, or the steps are not a light link's. Nothing waits for the GPU, and nothing is read
    /// back.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn encode_light(
        &mut self,
        link: &mut Option<LightLink>,
        pool: &mut Pool,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        light: &GpuLight,
    ) -> Result<u64, GpuFallback> {
        if self.gpu.support.is_none() {
            return Err(GpuFallback::NoAdapter);
        }
        if self.gpu.lost.load(Ordering::Acquire) {
            return Err(GpuFallback::DeviceLost);
        }
        if !spatial::supported(&device.limits()) {
            return Err(GpuFallback::NoAdapter);
        }
        let k = light.index().ok_or(GpuFallback::PipelineFailed)?;
        if pool.light_view(k).is_none() {
            return Err(GpuFallback::PipelineFailed);
        }
        let (compiled, pipeline) =
            self.gpu
                .pipeline(device, &light.steps, LIGHT_FORMAT, &self.figures.preview)?;
        let source = self.gpu.source.as_ref().ok_or(GpuFallback::SourceMissing)?;
        if !source.ready() {
            let figures = source.figures();
            return Err(GpuFallback::SourceUploading {
                uploaded: figures.uploaded,
                bytes: figures.bytes,
            });
        }
        let layouts = self
            .gpu
            .layouts
            .as_ref()
            .ok_or(GpuFallback::PipelineFailed)?;
        // The source's cut is the light's input, so it holds the light's whole stage.
        let whole = Derivation::Cut { origin: (0, 0) };
        if source.words(&whole, light.stage).is_none() {
            return Err(GpuFallback::SourceMissing);
        }
        let limit = device.limits().max_texture_dimension_2d;
        let shape =
            Shape::of(light, source.kind().boundary(), limit).ok_or(GpuFallback::PipelineFailed)?;
        if link.as_ref().is_none_or(|held| !held.shape.holds(&shape)) {
            if let Some(old) = link.take() {
                self.retire_light(old);
            }
            *link = Some(self.light_link(device, shape.clone())?);
        }
        let held = link.as_mut().expect("a fitted light link");
        // Blocks past what its buffer holds: a larger buffer, charged before it is created, the
        // old one retiring with its charge, every chunk written again.
        let need = (shape.blocks * 4) as u64;
        if need > held.block_words.bytes {
            let capacity = buffer_capacity(device, need)?;
            self.figures.preview.charge(capacity)?;
            let old = std::mem::replace(
                &mut held.block_words,
                Charged {
                    buffer: storage_buffer(device, "luxforge.gpu_light.blocks", capacity),
                    bytes: capacity,
                },
            );
            held.buffer_bytes = held.buffer_bytes - old.bytes + capacity;
            self.retire_preview(Held::Buffer(old.buffer), old.bytes);
            held.written_blocks.forget();
        }
        held.shape.blocks = shape.blocks;
        // The steps' blocks first, whose key the light's is made of.
        let update =
            held.written_blocks
                .write(queue, &held.block_words.buffer, &light.steps, BLOCK_CHUNK);
        self.figures.preview.blocks_updated(&update);
        let mut template = Vec::new();
        chain::pack_words(TexelMap::IDENTITY, (0, 0), &light.steps, &mut template);
        let key = light_key((
            source.version(),
            light.stage,
            held.shape.side,
            &template,
            held.written_blocks.key(),
            pipeline,
        ));
        if pool.light_key(k) == Some(key) {
            return Ok(key);
        }
        encode_tiles(
            held,
            (device, queue, encoder),
            (
                &compiled,
                self.gpu.support.as_deref().expect("a supported stage"),
            ),
            (source, layouts),
            light,
            pool.light_view(k).expect("the pool holds the light plane"),
        )?;
        pool.set_light_key(k, key);
        Ok(key)
    }
}

impl PhotoPipeline {
    /// Evaluate `plan` into `surface`'s slot as [`PhotoPipeline::evaluate`] does, each light its
    /// steps read written first ([`super::GpuPlan::lights`]): a slot not yet fitted to the plan is
    /// fitted by a first evaluation, which makes its pool hold the light planes; then every light
    /// link is encoded into its plane and submitted, encoding nothing where the plane holds its
    /// light already ([`PhotoPipeline::encode_light`]); and where any light changed, the slot
    /// forgets what it holds, so the plan is evaluated whole and every link reading a light draws
    /// with the new one. The surface keeps one link for each light, light `k` the `k`-th, and lets
    /// the ones past the plan's go. Names why the frame is not the GPU's, as `evaluate` does: a
    /// light's sequence compiling, the source still uploading or not held whole among them.
    pub(super) fn evaluate_lit(
        &mut self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: &super::GpuPlan,
        change: Option<super::GpuChange>,
    ) -> Result<u64, GpuFallback> {
        self.retire_lights(&mut surface.gpu_lights, plan.lights.len());
        if plan.lights.is_empty() {
            return self.evaluate(surface, device, queue, plan, change);
        }
        let count = plan.lights.len() as u32;
        let fitted = surface.gpu.as_ref().is_some_and(|slot| {
            slot.shape == super::Shape::of(plan)
                && slot.boundary_version == Some(plan.boundary.version())
                && (0..count).all(|k| slot.pool.light_view(k).is_some())
        });
        if !fitted {
            self.evaluate(surface, device, queue, plan, None)?;
        }
        let slot = surface.gpu.as_mut().ok_or(GpuFallback::PipelineFailed)?;
        let held: Vec<Option<u64>> = (0..count).map(|k| slot.pool.light_key(k)).collect();
        surface.gpu_lights.resize_with(plan.lights.len(), || None);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.gpu_light.encoder"),
        });
        let mut encoded = Ok(());
        for (light, link) in plan.lights.iter().zip(surface.gpu_lights.iter_mut()) {
            encoded = self
                .encode_light(link, &mut slot.pool, device, queue, &mut encoder, light)
                .map(|_| ());
            if encoded.is_err() {
                break;
            }
        }
        // What was encoded is submitted whatever stopped the rest: each light plane's key names
        // the light its passes write.
        queue.submit([encoder.finish()]);
        encoded?;
        if (0..count).any(|k| slot.pool.light_key(k) != held[k as usize]) {
            slot.forget_evaluation();
        }
        self.evaluate(surface, device, queue, plan, change)
    }

    /// Retire every light link of `links` past the first `keep`, through the retirement worker.
    pub(super) fn retire_lights(&self, links: &mut Vec<Option<LightLink>>, keep: usize) {
        if links.len() > keep {
            for link in links.drain(keep..).flatten() {
                self.retire_light(link);
            }
        }
    }
}

/// The content key of a light: its source's version, its stage, its tile side, the steps' words,
/// their blocks' key and its pipeline.
fn light_key(parts: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::hash::DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

/// Encode a light's passes over every tile of its stage, then its selection into `light`, the light
/// plane's view: what [`PhotoPipeline::encode_light`] runs once it knows the light changed.
fn encode_tiles(
    held: &mut LightLink,
    (device, queue, encoder): (&wgpu::Device, &wgpu::Queue, &mut wgpu::CommandEncoder),
    (compiled, support): (&Compiled, &super::Support),
    (source, layouts): (&super::SourceSlot, &super::SourceLayouts),
    light: &GpuLight,
    light_view: &wgpu::TextureView,
) -> Result<(), GpuFallback> {
    let [reduce, select] = &compiled.spatial.passes[..] else {
        return Err(GpuFallback::PipelineFailed);
    };
    let shape = held.shape.clone();
    let tiles = shape.tiles();
    if tiles.is_empty() {
        return Err(GpuFallback::PipelineFailed);
    }
    let block = shape.block;
    // Every tile's words, its texel map at the tile, a binding's offset apart; and every tile's
    // reduction's parameters over its blocks, then the selection's.
    let stride = region(shape.words);
    let mut words = vec![0u32; (tiles.len() as u64 * stride / 4) as usize];
    let mut packed = Vec::new();
    let mut params = Vec::new();
    let selection = Place {
        origin: [0, 0],
        limit: Rect::whole((spatial::UNLIMITED, spatial::UNLIMITED)),
        dispatch: [1, 1, 1],
    };
    let mut places = Vec::with_capacity(tiles.len());
    for (index, &[x0, y0, x1, y1]) in tiles.iter().enumerate() {
        let texels = TexelMap {
            origin: [x0 as f32, y0 as f32],
            step: [1.0, 1.0],
        };
        chain::pack_words(texels, (0, 0), &light.steps, &mut packed);
        let at = (index as u64 * stride / 4) as usize;
        words[at..at + packed.len()].copy_from_slice(&packed);
        // The blocks whose first pixel the tile holds, which are every block it holds whole.
        let covered = Rect {
            x0: x0 / block,
            y0: y0 / block,
            x1: x1.div_ceil(block),
            y1: y1.div_ceil(block),
        };
        let place = Place {
            origin: [covered.x0, covered.y0],
            limit: covered,
            dispatch: [
                (covered.x1 - covered.x0).div_ceil(spatial::GROUP_SIDE),
                (covered.y1 - covered.y0).div_ceil(spatial::GROUP_SIDE),
                1,
            ],
        };
        let slices = spatial::parameters(&light.steps, &[place, selection]);
        params.extend_from_slice(&slices[..slices.len() / 2]);
        places.push(place);
    }
    let slices = spatial::parameters(&light.steps, &[places[0], selection]);
    params.extend_from_slice(&slices[slices.len() / 2..]);
    if held.written_words != words {
        queue.write_buffer(&held.words.buffer, 0, &super::le_bytes(&words));
        held.written_words = words;
    }
    if held.written_params != params {
        queue.write_buffer(&held.params.buffer, 0, &super::le_bytes(&params));
        held.written_params = params;
    }
    let word_bytes = std::num::NonZeroU64::new((shape.words * 4) as u64);
    let params_slice = |number: usize| {
        wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: &held.params.buffer,
            offset: spatial::PARAMS_STRIDE * number as u64,
            size: std::num::NonZeroU64::new(spatial::PARAMS_STRIDE),
        })
    };
    let programs = |index: usize| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.gpu_light.bindings"),
            layout: &support.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &held.words.buffer,
                        offset: index as u64 * stride,
                        size: word_bytes,
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: held.block_words.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(held.tile.view()),
                },
            ],
        })
    };
    // The reduction reads no plane: its second group is its output, the block plane, and its
    // parameters. The selection reads the block plane and writes the light.
    if !reduce.slots().is_empty() || select.slots().len() != 1 {
        return Err(GpuFallback::PipelineFailed);
    }
    let output = |layout: &wgpu::BindGroupLayout,
                  plane: Option<&wgpu::TextureView>,
                  view: &wgpu::TextureView,
                  number: usize| {
        let mut entries = Vec::with_capacity(3);
        if let Some(plane) = plane {
            entries.push(wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(plane),
            });
        }
        entries.push(wgpu::BindGroupEntry {
            binding: spatial::OUTPUT_BINDING,
            resource: wgpu::BindingResource::TextureView(view),
        });
        entries.push(wgpu::BindGroupEntry {
            binding: spatial::PARAMS_BINDING,
            resource: params_slice(number),
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.gpu_light.planes"),
            layout,
            entries: &entries,
        })
    };
    for (index, (&[x0, y0, x1, y1], place)) in tiles.iter().zip(&places).enumerate() {
        let size = (x1 - x0, y1 - y0);
        let cut = Derivation::Cut { origin: (x0, y0) };
        let words = source.words(&cut, size).ok_or(GpuFallback::SourceMissing)?;
        queue.write_buffer(&held.cuts[index].buffer, 0, &super::le_bytes(&words));
        source.encode(
            device,
            encoder,
            layouts,
            &cut,
            &held.cuts[index].buffer,
            held.tile.view(),
            size,
        );
        let bindings = programs(index);
        let planes = output(reduce.layout(), None, held.blocks.view(), index);
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("luxforge.gpu_light.reduce"),
            timestamp_writes: None,
        });
        pass.set_pipeline(reduce.pipeline());
        pass.set_bind_group(0, &bindings, &[]);
        pass.set_bind_group(1, &planes, &[]);
        pass.dispatch_workgroups(place.dispatch[0], place.dispatch[1], place.dispatch[2]);
    }
    let bindings = programs(0);
    let planes = output(
        select.layout(),
        Some(held.blocks.view()),
        light_view,
        tiles.len(),
    );
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("luxforge.gpu_light.select"),
        timestamp_writes: None,
    });
    pass.set_pipeline(select.pipeline());
    pass.set_bind_group(0, &bindings, &[]);
    pass.set_bind_group(1, &planes, &[]);
    pass.dispatch_workgroups(1, 1, 1);
    Ok(())
}

/// What the block plane is created with: written by the reductions, read by the selection, and in
/// a build with a readback copied out by a test.
#[cfg(not(any(test, feature = "qualification")))]
const BLOCK_USAGE: wgpu::TextureUsages =
    wgpu::TextureUsages::TEXTURE_BINDING.union(wgpu::TextureUsages::STORAGE_BINDING);
#[cfg(any(test, feature = "qualification"))]
const BLOCK_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
    .union(wgpu::TextureUsages::STORAGE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC);

#[cfg(any(test, feature = "qualification"))]
pub use bench::{LightBench, Lit};

/// Qualification only: a light link and a plan reading its light drawn on a headless device and
/// read back, through the photo surface's own slot, as a test or the corpus harness holds them to
/// the CPU's. Everything here blocks the calling thread on the GPU, which no surface of the desktop
/// does.
#[cfg(any(test, feature = "qualification"))]
mod bench {
    use super::super::super::{PhotoPipeline, SurfaceSlots};
    use super::super::{GpuChange, GpuFallback, GpuPlan, GpuSource, Held, spatial};
    use super::{GpuLight, LightLink};
    use std::sync::Arc;

    /// A light read back: its value, `[r, g, b, 1]`, and the stage's block means beside each
    /// block's channel minimum, row by row, `grid` of them.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Lit {
        pub light: [f32; 4],
        pub blocks: Vec<[f32; 4]>,
        pub grid: (u32, u32),
        /// The light plane's content key, as its light link recorded it.
        pub key: u64,
    }

    /// One surface of a pipeline of its own on a headless device, its slot drawing a plan as the
    /// desktop's does, its light links run before its chain, and a light link of its own beside it
    /// that computes a light alone ([`PhotoPipeline::encode_light`]).
    pub struct LightBench {
        device: wgpu::Device,
        queue: wgpu::Queue,
        pipeline: PhotoPipeline,
        surface: SurfaceSlots,
        /// The pool a light is drawn into when no plan reads it.
        pool: spatial::Pool,
        link: Option<LightLink>,
        poisoned: bool,
    }

    impl LightBench {
        pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
            let pipeline = PhotoPipeline::with_figures(
                device,
                queue,
                wgpu::TextureFormat::Bgra8UnormSrgb,
                Arc::default(),
            );
            let surface = pipeline.new_surface();
            Self {
                device: device.clone(),
                queue: queue.clone(),
                pipeline,
                surface,
                pool: spatial::Pool::default(),
                link: None,
                poisoned: false,
            }
        }

        /// A bench on a device of this host's default adapter, its largest texture `texture_limit`
        /// pixels when given, as a device past whose limit a stage is held in tiles; and the
        /// adapter, its backend and its driver, for the report. `None`, having printed that `test`
        /// was skipped, without one: a run without one is not GPU evidence.
        pub fn headless(test: &str, texture_limit: Option<u32>) -> Option<(Self, String)> {
            let limits = wgpu::Limits {
                max_texture_dimension_2d: texture_limit
                    .unwrap_or(wgpu::Limits::default().max_texture_dimension_2d),
                ..wgpu::Limits::default()
            };
            let (device, queue, info) = super::super::headless::device(test, limits)?;
            let adapter = format!(
                "{} ({:?}, {:?}, driver {:?} {:?})",
                info.name, info.backend, info.device_type, info.driver, info.driver_info
            );
            Some((Self::new(&device, &queue), adapter))
        }

        /// Another bench on this one's device: a pipeline and a surface of its own, its figures its
        /// own.
        pub fn another(&self) -> Self {
            Self::new(&self.device, &self.queue)
        }

        /// While `poisoned`, every later draw starts each link's passes from NaN in every scratch
        /// texture of the slot's pool, every record forgotten ([`spatial::Pool::poison`]).
        pub fn set_poison(&mut self, poisoned: bool) {
            self.poisoned = poisoned;
            #[cfg(test)]
            self.pipeline.set_scratch_poison(poisoned);
            if let Some(slot) = self.surface.gpu.as_mut() {
                slot.pool.set_poisoned(poisoned);
            }
        }

        /// The source handed to the pipeline for one frame, a frame's rows of it written.
        fn hand(&mut self, source: &GpuSource) {
            self.pipeline
                .fit_source(&self.device, &self.queue, Some(source));
        }

        /// The end of a frame: a source no surface handed retires.
        fn trim(&mut self) {
            self.pipeline.trim_source();
        }

        /// `light` over `source`, encoded into `pool`'s light plane and submitted, waiting out a
        /// compile and the upload; its key. The fallback that stops it, or the one that kept it
        /// waiting past the test base's hang bound.
        fn encode(
            &mut self,
            source: &GpuSource,
            light: &GpuLight,
            into_slot: bool,
        ) -> Result<u64, GpuFallback> {
            let mut waited = GpuFallback::Compiling;
            // One frame a look, through the one hang-bounded wait, while the light's sequence
            // compiles or its source's rows are written.
            luxforge_testbase::try_wait_for("a light link's passes", || {
                self.hand(source);
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("luxforge.gpu_light.bench"),
                        });
                let pool = match (into_slot, self.surface.gpu.as_mut()) {
                    (true, Some(slot)) => &mut slot.pool,
                    (true, None) => return Some(Err(GpuFallback::PipelineFailed)),
                    (false, _) => &mut self.pool,
                };
                let outcome = self.pipeline.encode_light(
                    &mut self.link,
                    pool,
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    light,
                );
                self.queue.submit([encoder.finish()]);
                self.trim();
                match outcome {
                    Err(
                        waiting @ (GpuFallback::Compiling | GpuFallback::SourceUploading { .. }),
                    ) => {
                        waited = waiting;
                        None
                    }
                    other => Some(other),
                }
            })
            .unwrap_or(Err(waited))
        }

        /// `light` over `source` alone, into a pool of the bench's own holding its light plane,
        /// read back: the light, the stage's block means and the light's key.
        pub fn light(&mut self, source: &GpuSource, light: &GpuLight) -> Result<Lit, GpuFallback> {
            let k = light.index().ok_or(GpuFallback::PipelineFailed)?;
            let key = spatial::PoolKey::of(std::iter::empty(), (0, 0), (0, 0)).with_lights(k + 1);
            let preview = &self.pipeline.figures.preview;
            self.pool.fit(
                &self.device,
                &key,
                &mut |bytes| {
                    preview.charge(bytes)?;
                    preview
                        .scratch
                        .fetch_add(bytes, std::sync::atomic::Ordering::AcqRel);
                    Ok(())
                },
                &mut |_, _| {},
            )?;
            let key = self.encode(source, light, false)?;
            let plane = self
                .pool
                .light_texture(k)
                .ok_or(GpuFallback::PipelineFailed)?;
            let light_texels = read(&self.device, &self.queue, plane, (1, 1), 16)?;
            let (blocks, grid) = self
                .link
                .as_ref()
                .map(LightLink::blocks)
                .ok_or(GpuFallback::PipelineFailed)?;
            let block_texels = read(&self.device, &self.queue, blocks, grid, 16)?;
            Ok(Lit {
                light: floats(&light_texels)[0],
                blocks: floats(&block_texels),
                grid,
                key,
            })
        }

        /// `plan` drawn through the surface's own slot over `source`, waiting out a compile and the
        /// upload, as the slot draws it with `change`: its output's codes, RGBA row by row, the
        /// boundary's size or its region's.
        fn evaluate(
            &mut self,
            source: &GpuSource,
            plan: &GpuPlan,
            change: Option<GpuChange>,
        ) -> Result<(), GpuFallback> {
            let mut waited = GpuFallback::Compiling;
            // One frame a look, through the one hang-bounded wait, while its sequences compile or
            // its source's rows are written.
            luxforge_testbase::try_wait_for("a plan's evaluation", || {
                self.hand(source);
                let outcome = self.pipeline.evaluate_lit(
                    &mut self.surface,
                    &self.device,
                    &self.queue,
                    plan,
                    change,
                );
                if let Some(slot) = self.surface.gpu.as_mut() {
                    slot.pool.set_poisoned(self.poisoned);
                }
                self.trim();
                match outcome {
                    Ok(_) => Some(Ok(())),
                    Err(
                        waiting @ (GpuFallback::Compiling | GpuFallback::SourceUploading { .. }),
                    ) => {
                        waited = waiting;
                        None
                    }
                    Err(fallback) => Some(Err(fallback)),
                }
            })
            .unwrap_or(Err(waited))
        }

        /// `plan` drawn over `source` through the surface's own slot with `change`, reading
        /// `light`, when given, in place of the light links it carries, as the slot runs them
        /// before its chain ([`PhotoPipeline::evaluate_lit`]). Its output's codes, RGBA row by row.
        pub fn draw(
            &mut self,
            source: &GpuSource,
            light: Option<&GpuLight>,
            plan: &GpuPlan,
            change: Option<GpuChange>,
        ) -> Result<Vec<[u8; 4]>, GpuFallback> {
            let plan = match light {
                Some(light) => GpuPlan {
                    lights: vec![light.clone()],
                    ..plan.clone()
                },
                None => plan.clone(),
            };
            self.evaluate(source, &plan, change)?;
            let slot = self
                .surface
                .gpu
                .as_ref()
                .ok_or(GpuFallback::PipelineFailed)?;
            let output = slot.output();
            let size = (output.width, output.height);
            let bytes = read(&self.device, &self.queue, &output.tiles[0].texture, size, 4)?;
            Ok(bytes
                .chunks_exact(4)
                .map(|texel| [texel[0], texel[1], texel[2], texel[3]])
                .collect())
        }

        /// `plan` drawn over `source` as the desktop's surface draws a frame of it
        /// ([`PhotoPipeline::prepare_gpu`]): the slot running the light links the plan carries
        /// ([`super::super::GpuPlan::lights`]) before its chain, as each frame does, waiting out a
        /// compile and the upload, with `change`. Its output's codes, RGBA row by row.
        pub fn prepare(
            &mut self,
            source: &GpuSource,
            plan: &GpuPlan,
            change: Option<GpuChange>,
        ) -> Result<Vec<[u8; 4]>, GpuFallback> {
            let mut waited = GpuFallback::Compiling;
            luxforge_testbase::try_wait_for("a frame's lights and chain", || {
                self.hand(source);
                self.pipeline.prepare_gpu(
                    &mut self.surface,
                    &self.device,
                    &self.queue,
                    Some(plan),
                    None,
                    change,
                );
                self.trim();
                match self.surface.gpu_outcome {
                    Some(Ok(_)) => Some(Ok(())),
                    Some(Err(
                        waiting @ (GpuFallback::Compiling
                        | GpuFallback::SourceUploading { .. }
                        | GpuFallback::BoundaryUploading { .. }),
                    )) => {
                        waited = waiting;
                        None
                    }
                    Some(Err(fallback)) => Some(Err(fallback)),
                    None => Some(Err(GpuFallback::PipelineFailed)),
                }
            })
            .unwrap_or(Err(waited))?;
            let slot = self
                .surface
                .gpu
                .as_ref()
                .ok_or(GpuFallback::PipelineFailed)?;
            let output = slot.output();
            let size = (output.width, output.height);
            let bytes = read(&self.device, &self.queue, &output.tiles[0].texture, size, 4)?;
            Ok(bytes
                .chunks_exact(4)
                .map(|texel| [texel[0], texel[1], texel[2], texel[3]])
                .collect())
        }

        /// How many light links the surface holds for its slot, and what they hold, as charged.
        pub fn surface_lights(&self) -> (usize, u64) {
            let links = self.surface.gpu_lights.iter().flatten();
            (links.clone().count(), links.map(LightLink::bytes).sum())
        }

        /// How many spatial passes the slot has dispatched, over every draw.
        pub fn spatial_passes(&self) -> u64 {
            self.pipeline.figures.preview.spatial_passes()
        }

        /// What the GPU-preview budget holds charged, and of it the scratch.
        pub fn charged(&self) -> (u64, u64) {
            let preview = &self.pipeline.figures.preview;
            (preview.in_use(), preview.scratch())
        }

        /// What the bench's light link holds, as charged.
        pub fn light_bytes(&self) -> Option<u64> {
            self.link.as_ref().map(LightLink::bytes)
        }

        /// Let everything go — the slot, the light link, the bench's pool's light plane and the
        /// source, which no frame hands again — through the retirement worker, and wait until it
        /// has retired them.
        pub fn release(&mut self) {
            self.pipeline.release_gpu(&mut self.surface);
            if let Some(link) = self.link.take() {
                self.pipeline.retire_light(link);
            }
            self.trim();
            // The bench's pool holds light planes alone, charged as the slot's pool's are: fitted
            // to none, it hands them back to retire.
            let mut retired = Vec::new();
            let _ = self.pool.fit(
                &self.device,
                &spatial::PoolKey::of(std::iter::empty(), (0, 0), (0, 0)),
                &mut |_| Ok::<(), GpuFallback>(()),
                &mut |textures, bytes| retired.push((textures, bytes)),
            );
            for (textures, bytes) in retired {
                self.pipeline.retire_preview(Held::Pool(textures), bytes);
            }
            let figures = &self.pipeline.figures;
            luxforge_testbase::wait_until("the bench's retirements", || {
                figures
                    .retirement_pending
                    .load(std::sync::atomic::Ordering::Acquire)
                    == 0
            });
        }
    }

    /// `texels` of `texture` at its origin, `bytes` a texel, read back unpadded: the submission
    /// waited for.
    fn read(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        (width, height): (u32, u32),
        bytes: u32,
    ) -> Result<Vec<u8>, GpuFallback> {
        let row = width * bytes;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("luxforge.gpu_light.readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.gpu_light.read"),
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let index = queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .map_err(|_| GpuFallback::DeviceLost)?;
        match receiver.try_recv() {
            Ok(Ok(())) => {}
            _ => return Err(GpuFallback::DeviceLost),
        }
        let mapped = readback.slice(..).get_mapped_range();
        let mut texels = Vec::with_capacity((row * height) as usize);
        for line in mapped.chunks_exact(padded as usize) {
            texels.extend_from_slice(&line[..row as usize]);
        }
        drop(mapped);
        readback.unmap();
        Ok(texels)
    }

    /// `rgba32float` texels' bytes as values.
    fn floats(bytes: &[u8]) -> Vec<[f32; 4]> {
        bytes
            .chunks_exact(16)
            .map(|texel| {
                std::array::from_fn(|channel| {
                    f32::from_le_bytes(
                        texel[channel * 4..channel * 4 + 4]
                            .try_into()
                            .expect("four bytes"),
                    )
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "light_tests.rs"]
mod tests;
