//! The prepared source held on the GPU (`docs/design/gpu-preview.md`, "The GPU source"): the
//! pixels every boundary is derived from, uploaded once per source and shared by every surface of
//! the pipeline, and the two fixed passes that derive a boundary from them.
//!
//! - **What is held.** A JPEG's upright 8-bit codes (`rgba8uint`, 4 bytes a pixel), read by
//!   integer and decoded through the CPU's own 256-entry table, never through the hardware's sRGB
//!   view, whose decode is not the table's to the bit. A developed RAW's three planes of the view's
//!   crop window (`r32float`, 12 bytes a pixel), written straight from the planes the core shares,
//!   with no copy on the CPU; the view's orientation is applied as a boundary is cut, so the
//!   planes are held as they lie.
//! - **Tiles.** A side past the device's largest texture (8192 px on Iced's device) is held in up
//!   to two textures across and two down, each the tile side or what is left; a pass reads them
//!   by coordinate, so a boundary that straddles two is one picture with no seam.
//! - **The upload** is spread as a boundary's is: at most [`UPLOAD_PER_FRAME`](super::UPLOAD_PER_FRAME)
//!   a frame, the rows written straight from the caller's buffer, on the interface thread's
//!   `prepare`, never waited on. It is charged to the GPU-preview budget before anything is
//!   created and retires through the surface's retirement worker.
//! - **Cut** ([`Derivation::Cut`]): a window of the source at full scale, the content stage's pixel
//!   `t + origin` at boundary texel `t`, through the view's orientation; a JPEG's code decoded and
//!   held as the nearest half float, a RAW's value as the `f32` it is: bit for bit the boundary the
//!   CPU renders at the source (`Render::boundary`).
//! - **Reduce** ([`Derivation::Reduce`]): the source's area average at a proxy plan, the proxy
//!   stage's pixel `t + origin` at boundary texel `t`, with the coverage weights the CPU's proxy
//!   build averages with, across each source row and then down; a JPEG's average is quantized
//!   through the output thresholds and decoded again, as the CPU's proxy is. Within a code of the
//!   CPU's proxy: its sums are `f64`, these `f32`.
use super::BoundaryTexture;
use super::{BoundaryFormat, GpuFallback, UPLOAD_CHUNK, spatial::HALF_ROUNDING, tail::encoding};
use std::sync::Arc;

/// What a source is: a JPEG's codes or a developed RAW's planes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Codes,
    Planes,
}

impl SourceKind {
    /// The boundary a source of this kind derives: half floats from codes, `f32` from planes.
    pub fn boundary(self) -> BoundaryFormat {
        match self {
            Self::Codes => BoundaryFormat::Half,
            Self::Planes => BoundaryFormat::Float,
        }
    }

    /// Bytes a held pixel takes on the GPU.
    pub(super) const fn pixel_bytes(self) -> u64 {
        match self {
            Self::Codes => 4,
            Self::Planes => 12,
        }
    }

    /// Textures a tile takes: one of codes, or a plane each.
    const fn textures(self) -> usize {
        match self {
            Self::Codes => 1,
            Self::Planes => 3,
        }
    }
}

/// The pixels a source holds until the GPU does.
#[derive(Clone)]
enum Pixels {
    /// Four bytes a pixel, red, green, blue and alpha, row by row: the whole upright image.
    Codes(Arc<dyn AsRef<[u8]> + Send + Sync>),
    /// Red, then green, then blue, each the base planes' size, row by row.
    Planes(Arc<dyn AsRef<[f32]> + Send + Sync>),
}

/// The prepared source a photograph's every picture on the GPU is derived from, as the caller
/// hands it to a surface ([`PhotoSurface::gpu_source`](super::super::PhotoSurface::gpu_source)):
/// plain data, so this crate still names no core type. Its version changes whenever its pixels
/// do — another photograph, a new development — and never otherwise, so the pipeline uploads it
/// once whichever surfaces hand it.
#[derive(Clone)]
pub struct GpuSource {
    version: u64,
    /// The pixels, until the caller lets them go once the pipeline holds them ([`Self::resident`]).
    pixels: Option<Pixels>,
    kind: SourceKind,
    /// The texels the GPU holds: a JPEG's whole image, a RAW's crop window as its planes lie.
    held: (u32, u32),
    /// A RAW's base planes' size and the crop window's origin in them; the image's size and the
    /// origin for a JPEG.
    base: (u32, u32),
    crop: (u32, u32),
    /// The content stage the source fills: the held texels through the view's orientation.
    stage: (u32, u32),
    /// The rectangle of the content stage whose texels it holds, `[x, y, width, height]`: the
    /// whole stage, or the window [`Self::window`] keeps of it.
    window: [u32; 4],
    /// The map from a content-stage pixel to the held texel it reads: `(a·x + b·y + tx,
    /// c·x + d·y + ty)`, a signed permutation and a translation.
    map: [i32; 6],
}

impl std::fmt::Debug for GpuSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GpuSource")
            .field("version", &self.version)
            .field("kind", &self.kind)
            .field("held", &self.held)
            .field("stage", &self.stage)
            .field("window", &self.window)
            .field("pixels", &self.pixels.is_some())
            .finish()
    }
}

/// The map from a pixel of the content stage a view of `width` × `height` held texels shows under
/// EXIF `orientation` to the held texel it reads, as the core's view maps it: `None` for an
/// orientation outside 1 to 8.
fn orientation_map(orientation: u8, (width, height): (u32, u32)) -> Option<[i32; 6]> {
    let (w, h) = (width as i32 - 1, height as i32 - 1);
    Some(match orientation {
        1 => [1, 0, 0, 0, 1, 0],
        2 => [-1, 0, w, 0, 1, 0],
        3 => [-1, 0, w, 0, -1, h],
        4 => [1, 0, 0, 0, -1, h],
        5 => [0, 1, 0, 1, 0, 0],
        6 => [0, 1, 0, -1, 0, h],
        7 => [0, -1, w, -1, 0, h],
        8 => [0, -1, w, 1, 0, 0],
        _ => return None,
    })
}

impl GpuSource {
    /// A JPEG's upright codes, `width` × `height` pixels of four bytes each in `rgba` as they are,
    /// or `None` when the buffer does not hold exactly that many. Nothing is copied.
    pub fn codes<P: AsRef<[u8]> + Send + Sync + 'static>(
        version: u64,
        rgba: Arc<P>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (width > 0 && height > 0 && (*rgba).as_ref().len() == expected).then(|| Self {
            version,
            pixels: Some(Pixels::Codes(rgba)),
            kind: SourceKind::Codes,
            held: (width, height),
            base: (width, height),
            crop: (0, 0),
            stage: (width, height),
            window: [0, 0, width, height],
            map: [1, 0, 0, 0, 1, 0],
        })
    }

    /// A developed RAW's three planes of `base` values each, red then green then blue, viewed
    /// through the crop window `[x, y, width, height]` of them under EXIF `orientation`, as the
    /// core's view shows them; or `None` when the planes, the window or the orientation do not
    /// fit. Nothing is copied: only the window's rows are uploaded, straight from `planes`.
    pub fn planes<P: AsRef<[f32]> + Send + Sync + 'static>(
        version: u64,
        planes: Arc<P>,
        base: (u32, u32),
        crop: [u32; 4],
        orientation: u8,
    ) -> Option<Self> {
        let [x, y, width, height] = crop;
        let expected = (base.0 as usize)
            .checked_mul(base.1 as usize)?
            .checked_mul(3)?;
        let inside = width > 0
            && height > 0
            && x.checked_add(width)? <= base.0
            && y.checked_add(height)? <= base.1;
        let map = orientation_map(orientation, (width, height))?;
        let stage = if (5..=8).contains(&orientation) {
            (height, width)
        } else {
            (width, height)
        };
        (inside && (*planes).as_ref().len() == expected).then(|| Self {
            version,
            pixels: Some(Pixels::Planes(planes)),
            kind: SourceKind::Planes,
            held: (width, height),
            base,
            crop: (x, y),
            stage,
            window: [0, 0, stage.0, stage.1],
            map,
        })
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn kind(&self) -> SourceKind {
        self.kind
    }

    /// The content stage the source fills, which a cut's origin and a reduction's coverage address.
    pub fn stage(&self) -> (u32, u32) {
        self.stage
    }

    /// The bytes the GPU holds of the source: four a pixel of a JPEG, twelve of a RAW's window.
    pub fn bytes(&self) -> u64 {
        u64::from(self.held.0) * u64::from(self.held.1) * self.kind.pixel_bytes()
    }

    /// This source with its pixels let go: what the caller hands once the pipeline holds them
    /// ([`SourceFigures::ready`]), so it keeps no reference to them. A pipeline that no longer
    /// holds this version cannot upload it again, and a boundary derived from it falls back with
    /// [`GpuFallback::SourceMissing`] until the caller hands the pixels once more.
    pub fn resident(&self) -> Self {
        Self {
            pixels: None,
            ..self.clone()
        }
    }

    /// Whether the source still holds its pixels.
    pub fn holds_pixels(&self) -> bool {
        self.pixels.is_some()
    }

    /// This source holding only `rect`, `[x, y, width, height]` of the content stage: the same
    /// pixels, shared, of which a pipeline uploads and holds only the texels the rectangle reads
    /// through the view's orientation, charged for those alone; or `None` when the rectangle is
    /// empty or leaves the rectangle this source holds. Its stage is still the whole content
    /// stage, so a cut is addressed as it is over the whole source, and is bit for bit the whole
    /// source's wherever the window holds it: a cut that leaves the window, and any reduction,
    /// which reads the whole source, derives nothing from it. Its version is the source's; a
    /// pipeline tells it from the whole source, or another window, by the rectangle it holds.
    pub fn window(&self, rect: [u32; 4]) -> Option<Self> {
        let [x, y, width, height] = rect;
        let inside = width > 0
            && height > 0
            && x.checked_add(width)? <= self.window[0] + self.window[2]
            && y.checked_add(height)? <= self.window[1] + self.window[3]
            && x >= self.window[0]
            && y >= self.window[1];
        if !inside {
            return None;
        }
        // The held texels of the rectangle's corners, as the map reads them: a signed permutation
        // takes a rectangle to a rectangle.
        let [a, b, tx, c, d, ty] = self.map.map(i64::from);
        let held = |px: u32, py: u32| {
            let (px, py) = (i64::from(px), i64::from(py));
            (a * px + b * py + tx, c * px + d * py + ty)
        };
        let corners = [held(x, y), held(x + width - 1, y + height - 1)];
        let (hx0, hx1) = (
            corners[0].0.min(corners[1].0),
            corners[0].0.max(corners[1].0),
        );
        let (hy0, hy1) = (
            corners[0].1.min(corners[1].1),
            corners[0].1.max(corners[1].1),
        );
        if hx0 < 0 || hy0 < 0 || hx1 >= i64::from(self.held.0) || hy1 >= i64::from(self.held.1) {
            return None;
        }
        let (hx0, hy0) = (u32::try_from(hx0).ok()?, u32::try_from(hy0).ok()?);
        let mut map = self.map;
        map[2] -= i32::try_from(hx0).ok()?;
        map[5] -= i32::try_from(hy0).ok()?;
        Some(Self {
            held: (
                u32::try_from(hx1).ok()? - hx0 + 1,
                u32::try_from(hy1).ok()? - hy0 + 1,
            ),
            crop: (self.crop.0 + hx0, self.crop.1 + hy0),
            window: rect,
            map,
            ..self.clone()
        })
    }

    /// The rectangle of the content stage it holds, `[x, y, width, height]`: the whole stage, or
    /// a window's ([`Self::window`]).
    pub fn held_rect(&self) -> [u32; 4] {
        self.window
    }
}

/// One axis of a proxy's area average, as the CPU's build weighs it: for each output index the
/// first source index it reads, the half-open range of `weights` that belongs to it, and the
/// weights, each narrowed to `f32` once.
#[derive(Clone, Debug, PartialEq)]
pub struct AxisCoverage {
    pub first: Vec<u32>,
    /// One more than the output's length: output `i`'s weights are `offsets[i]..offsets[i + 1]`.
    pub offsets: Vec<u32>,
    pub weights: Vec<f32>,
}

/// A proxy plan's area average of the source: the coverage of the whole proxy stage across and
/// down, and the origin of the window of it a boundary holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Reduction {
    pub origin: (u32, u32),
    pub across: AxisCoverage,
    pub down: AxisCoverage,
}

impl Reduction {
    /// Whether the coverage covers a `size` boundary at its origin, every output's weights in
    /// range: what a pass may read.
    fn valid(&self, size: (u32, u32)) -> bool {
        let axis = |coverage: &AxisCoverage, end: u32| {
            coverage.offsets.len() == coverage.first.len() + 1
                && coverage.first.len() >= end as usize
                && coverage.offsets.windows(2).all(|pair| pair[0] <= pair[1])
                && coverage
                    .offsets
                    .last()
                    .is_some_and(|last| *last as usize == coverage.weights.len())
        };
        axis(&self.across, self.origin.0.saturating_add(size.0))
            && axis(&self.down, self.origin.1.saturating_add(size.1))
    }
}

/// How a boundary is derived on the GPU from the source the pipeline holds, rather than uploaded
/// from the CPU.
#[derive(Clone, Debug, PartialEq)]
pub enum Derivation {
    /// A window of the source at full scale: boundary texel `t` is the content stage's pixel
    /// `t + origin`.
    Cut { origin: (u32, u32) },
    /// The source's area average at a proxy plan: boundary texel `t` is the proxy stage's pixel
    /// `t + origin`.
    Reduce(Arc<Reduction>),
}

/// What a surface's diagnostics say of the source the pipeline holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceFigures {
    pub version: u64,
    /// What the GPU holds of it once uploaded, charged to the GPU-preview budget.
    pub bytes: u64,
    /// The bytes uploaded so far.
    pub uploaded: u64,
    /// Every row is uploaded: a boundary can be derived from it.
    pub ready: bool,
}

/// The source the pipeline holds: its textures, tile by tile, how far its upload has got, and what
/// it is charged.
pub(super) struct SourceSlot {
    version: u64,
    kind: SourceKind,
    held: (u32, u32),
    /// The side of every tile but the last across and down.
    tile: u32,
    /// The tiles across and down, at most two each.
    grid: (u32, u32),
    /// Each tile's textures, tile by tile in rows, a tile's planes red, green then blue.
    textures: Vec<wgpu::Texture>,
    /// The textures as a pass binds them: four tiles, the missing ones standing in as the first.
    bindings: wgpu::BindGroup,
    /// The rows written so far.
    rows: u32,
    bytes: u64,
    /// The base planes' size, or the image's, and the held texels' origin in them, for the rows'
    /// offsets in the caller's buffer.
    base: (u32, u32),
    crop: (u32, u32),
    map: [i32; 6],
    /// The rectangle of the content stage it holds ([`GpuSource::held_rect`]).
    window: [u32; 4],
    /// It holds the whole content stage, not a window of it.
    whole: bool,
}

impl SourceSlot {
    pub(super) fn version(&self) -> u64 {
        self.version
    }

    /// Whether it holds `source`'s texels: its version, and the same rectangle of it.
    pub(super) fn holds(&self, source: &GpuSource) -> bool {
        self.version == source.version && self.window == source.window
    }

    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(super) fn kind(&self) -> SourceKind {
        self.kind
    }

    /// Every row is written.
    pub(super) fn ready(&self) -> bool {
        self.rows >= self.held.1
    }

    pub(super) fn figures(&self) -> SourceFigures {
        SourceFigures {
            version: self.version,
            bytes: self.bytes,
            uploaded: u64::from(self.rows.min(self.held.1))
                * u64::from(self.held.0)
                * self.kind.pixel_bytes(),
            ready: self.ready(),
        }
    }

    /// The textures for `source` on `device`, whose largest texture side is `limit`, created
    /// empty with `layouts`' bindings; `charge` is asked for the bytes before anything is created.
    /// `None` with the fallback when the source needs more than two tiles across or down, or
    /// passes the budget.
    pub(super) fn new(
        device: &wgpu::Device,
        source: &GpuSource,
        layouts: &Layouts,
        charge: impl FnOnce(u64) -> Result<(), GpuFallback>,
    ) -> Result<Self, GpuFallback> {
        let limit = device.limits().max_texture_dimension_2d;
        let tile = limit.min(8192);
        let (width, height) = source.held;
        let grid = (width.div_ceil(tile), height.div_ceil(tile));
        if grid.0 > 2 || grid.1 > 2 {
            return Err(GpuFallback::TextureLimit {
                width,
                height,
                limit: 2 * tile,
            });
        }
        let bytes = source.bytes();
        charge(bytes)?;
        let format = match source.kind {
            SourceKind::Codes => wgpu::TextureFormat::Rgba8Uint,
            SourceKind::Planes => wgpu::TextureFormat::R32Float,
        };
        let mut textures = Vec::new();
        for row in 0..grid.1 {
            for column in 0..grid.0 {
                let size = (
                    tile.min(width - column * tile),
                    tile.min(height - row * tile),
                );
                for _ in 0..source.kind.textures() {
                    textures.push(device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("luxforge.gpu_source.tile"),
                        size: wgpu::Extent3d {
                            width: size.0,
                            height: size.1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    }));
                }
            }
        }
        let bindings = layouts.source_bindings(device, source.kind, &textures);
        Ok(Self {
            version: source.version,
            kind: source.kind,
            held: source.held,
            tile,
            grid,
            textures,
            bindings,
            rows: 0,
            bytes,
            base: source.base,
            crop: source.crop,
            map: source.map,
            window: source.window,
            whole: source.window == [0, 0, source.stage.0, source.stage.1],
        })
    }

    /// Write rows of `source`, which must be this slot's, from where the last frame stopped, in
    /// chunks of about [`UPLOAD_CHUNK`] straight from its buffer, until `limit` bytes are written or
    /// every row is: the bytes written. A source whose pixels were let go writes nothing.
    pub(super) fn upload(&mut self, queue: &wgpu::Queue, source: &GpuSource, limit: u64) -> u64 {
        let Some(pixels) = &source.pixels else {
            return 0;
        };
        let (width, height) = self.held;
        let row_bytes = u64::from(width) * self.kind.pixel_bytes();
        let chunk_rows = (UPLOAD_CHUNK / row_bytes).max(1) as u32;
        let mut written = 0;
        while self.rows < height && (written == 0 || written < limit) {
            // A chunk never crosses a tile's lower edge, so it lands in one row of tiles.
            let tile_row = self.rows / self.tile;
            let tile_end = ((tile_row + 1) * self.tile).min(height);
            let rows = chunk_rows
                .min(tile_end - self.rows)
                .min(((limit - written) / row_bytes).clamp(1, u64::from(u32::MAX)) as u32);
            for column in 0..self.grid.0 {
                let x0 = column * self.tile;
                let span = self.tile.min(width - x0);
                let index = (tile_row * self.grid.0 + column) as usize * self.kind.textures();
                let extent = wgpu::Extent3d {
                    width: span,
                    height: rows,
                    depth_or_array_layers: 1,
                };
                match pixels {
                    Pixels::Codes(rgba) => {
                        let mut destination = self.textures[index].as_image_copy();
                        destination.origin.y = self.rows - tile_row * self.tile;
                        queue.write_texture(
                            destination,
                            (**rgba).as_ref(),
                            wgpu::TexelCopyBufferLayout {
                                offset: (u64::from(self.crop.1 + self.rows)
                                    * u64::from(self.base.0)
                                    + u64::from(self.crop.0 + x0))
                                    * 4,
                                bytes_per_row: Some(self.base.0 * 4),
                                rows_per_image: Some(rows),
                            },
                            extent,
                        );
                    }
                    Pixels::Planes(planes) => {
                        let values: &[u8] = bytemuck::cast_slice((**planes).as_ref());
                        let plane = u64::from(self.base.0) * u64::from(self.base.1);
                        for channel in 0..3u64 {
                            let mut destination =
                                self.textures[index + channel as usize].as_image_copy();
                            destination.origin.y = self.rows - tile_row * self.tile;
                            let first = channel * plane
                                + u64::from(self.crop.1 + self.rows) * u64::from(self.base.0)
                                + u64::from(self.crop.0 + x0);
                            queue.write_texture(
                                destination,
                                values,
                                wgpu::TexelCopyBufferLayout {
                                    offset: first * 4,
                                    bytes_per_row: Some(self.base.0 * 4),
                                    rows_per_image: Some(rows),
                                },
                                extent,
                            );
                        }
                    }
                }
            }
            self.rows += rows;
            written += u64::from(rows) * row_bytes;
        }
        written
    }

    /// The words a derivation's pass reads before any coverage table: the tile side, the held
    /// size, the content-to-held map and the origin.
    fn header(&self, origin: (u32, u32)) -> Vec<u32> {
        let mut words = vec![self.tile, self.held.0, self.held.1];
        words.extend(self.map.map(|value| value as u32));
        words.extend([origin.0, origin.1]);
        words
    }

    /// The words `derivation`'s pass reads over a boundary of `size`: the header, and for a
    /// reduction the six table offsets then the tables. `None` when a cut leaves the rectangle of
    /// the content stage the slot holds, or a reduction's coverage does not cover the boundary,
    /// or the slot holds a window, which no reduction reads.
    pub(super) fn words(&self, derivation: &Derivation, size: (u32, u32)) -> Option<Vec<u32>> {
        let [x, y, width, height] = self.window;
        match derivation {
            Derivation::Cut { origin } => {
                let inside = origin.0 >= x
                    && origin.1 >= y
                    && origin.0.checked_add(size.0)? <= x + width
                    && origin.1.checked_add(size.1)? <= y + height;
                inside.then(|| self.header(*origin))
            }
            Derivation::Reduce(reduction) => {
                if !self.whole || !reduction.valid(size) {
                    return None;
                }
                let mut words = self.header(reduction.origin);
                let tables = words.len() + 6;
                let mut at = tables;
                let mut offsets = Vec::with_capacity(6);
                for coverage in [&reduction.across, &reduction.down] {
                    offsets.push(at as u32);
                    at += coverage.first.len();
                    offsets.push(at as u32);
                    at += coverage.offsets.len();
                    offsets.push(at as u32);
                    at += coverage.weights.len();
                }
                words.extend(offsets);
                for coverage in [&reduction.across, &reduction.down] {
                    words.extend(&coverage.first);
                    words.extend(&coverage.offsets);
                    words.extend(coverage.weights.iter().map(|weight| weight.to_bits()));
                }
                Some(words)
            }
        }
    }

    /// Encode `derivation`'s pass into `target`, a boundary of `size` in this source's boundary
    /// format, reading `words` (written by the caller from [`Self::words`]) through `layouts`'
    /// pipelines.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        layouts: &Layouts,
        derivation: &Derivation,
        words: &wgpu::Buffer,
        target: &wgpu::TextureView,
        size: (u32, u32),
    ) {
        let pipeline = layouts.pipeline(self.kind, matches!(derivation, Derivation::Reduce(_)));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.gpu_source.words"),
            layout: &layouts.words,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: words.as_entire_binding(),
            }],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("luxforge.gpu_source.derive"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_viewport(0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.set_bind_group(1, &self.bindings, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// The derivation passes' layouts and pipelines, made once with the stage: a cut and a reduction
/// of codes into half floats, and of planes into `f32`.
pub(super) struct Layouts {
    words: wgpu::BindGroupLayout,
    codes: wgpu::BindGroupLayout,
    planes: wgpu::BindGroupLayout,
    codes_cut: wgpu::RenderPipeline,
    codes_reduce: wgpu::RenderPipeline,
    planes_cut: wgpu::RenderPipeline,
    planes_reduce: wgpu::RenderPipeline,
}

/// The tiles a pass binds: four, the textures of a tile each.
const TILES: usize = 4;

impl Layouts {
    /// The passes on `device`, or why there are none: no output encoding installed yet, whose
    /// table a JPEG's codes are decoded through, or a shader the device refused.
    pub(super) fn new(device: &wgpu::Device) -> Result<Self, String> {
        let encoding = encoding()?;
        let texture = |binding: u32, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let words = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("luxforge.gpu_source.words_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let codes_entries: Vec<_> = (0..TILES as u32)
            .map(|binding| texture(binding, wgpu::TextureSampleType::Uint))
            .collect();
        let codes = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("luxforge.gpu_source.codes_layout"),
            entries: &codes_entries,
        });
        let planes_entries: Vec<_> = (0..(3 * TILES) as u32)
            .map(|binding| {
                texture(
                    binding,
                    wgpu::TextureSampleType::Float { filterable: false },
                )
            })
            .collect();
        let planes = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("luxforge.gpu_source.planes_layout"),
            entries: &planes_entries,
        });
        // Validated as the stage validates every pass before anything reaches the device, and
        // created inside error scopes polled once, so nothing here is an error wgpu's default
        // handler would panic on.
        for kind in [SourceKind::Codes, SourceKind::Planes] {
            super::validate(&shader(kind, encoding))?;
        }
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let build = |kind: SourceKind, layout: &wgpu::BindGroupLayout| {
            let source = shader(kind, encoding);
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("luxforge.gpu_source.shader"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("luxforge.gpu_source.pipeline_layout"),
                bind_group_layouts: &[&words, layout],
                push_constant_ranges: &[],
            });
            let format = kind.boundary().texture();
            let pipeline = |entry: &str| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("luxforge.gpu_source.pipeline"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("lf_source_vertex"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some(entry),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                    cache: None,
                })
            };
            (pipeline("lf_source_cut"), pipeline("lf_source_reduce"))
        };
        let (codes_cut, codes_reduce) = build(SourceKind::Codes, &codes);
        let (planes_cut, planes_reduce) = build(SourceKind::Planes, &planes);
        let validation = super::answered(device.pop_error_scope());
        let internal = super::answered(device.pop_error_scope());
        match (validation, internal) {
            (Some(None), Some(None)) => {}
            (Some(Some(error)), _) | (_, Some(Some(error))) => return Err(error.to_string()),
            _ => {
                return Err(
                    "the derivation pipelines' error scopes were not answered without waiting"
                        .into(),
                );
            }
        }
        Ok(Self {
            words,
            codes,
            planes,
            codes_cut,
            codes_reduce,
            planes_cut,
            planes_reduce,
        })
    }

    fn pipeline(&self, kind: SourceKind, reduce: bool) -> &wgpu::RenderPipeline {
        match (kind, reduce) {
            (SourceKind::Codes, false) => &self.codes_cut,
            (SourceKind::Codes, true) => &self.codes_reduce,
            (SourceKind::Planes, false) => &self.planes_cut,
            (SourceKind::Planes, true) => &self.planes_reduce,
        }
    }

    /// The bindings of a source's `textures`, tile by tile, every missing tile standing in as the
    /// first, which no pass reads.
    fn source_bindings(
        &self,
        device: &wgpu::Device,
        kind: SourceKind,
        textures: &[wgpu::Texture],
    ) -> wgpu::BindGroup {
        let per_tile = kind.textures();
        let views: Vec<wgpu::TextureView> = (0..TILES * per_tile)
            .map(|index| {
                let texture = textures.get(index).unwrap_or(&textures[index % per_tile]);
                texture.create_view(&wgpu::TextureViewDescriptor::default())
            })
            .collect();
        let entries: Vec<wgpu::BindGroupEntry> = views
            .iter()
            .enumerate()
            .map(|(binding, view)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: wgpu::BindingResource::TextureView(view),
            })
            .collect();
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.gpu_source.textures"),
            layout: match kind {
                SourceKind::Codes => &self.codes,
                SourceKind::Planes => &self.planes,
            },
            entries: &entries,
        })
    }
}

/// The derivation shader for a source of `kind`, over the output `encoding`: the tiles, the
/// content-to-held map, one pixel's linear value, and the cut's and the reduction's entries.
fn shader(kind: SourceKind, encoding: &str) -> String {
    let (bindings, texel, store, settle) = match kind {
        SourceKind::Codes => (
            (0..TILES)
                .map(|tile| {
                    format!("@group(1) @binding({tile}) var lf_source_t{tile}: texture_2d<u32>;\n")
                })
                .collect::<String>(),
            "
fn lf_source_texel(at: vec2<u32>) -> vec3<f32> {
    let tile = lf_source_word(0u);
    let q = vec2<i32>(at % vec2<u32>(tile));
    var codes = vec4<u32>(0u);
    switch (at.y / tile) * 2u + at.x / tile {
        case 0u: { codes = textureLoad(lf_source_t0, q, 0); }
        case 1u: { codes = textureLoad(lf_source_t1, q, 0); }
        case 2u: { codes = textureLoad(lf_source_t2, q, 0); }
        default: { codes = textureLoad(lf_source_t3, q, 0); }
    }
    return vec3<f32>(lf_output_decode(codes.r), lf_output_decode(codes.g), lf_output_decode(codes.b));
}
"
            .to_owned(),
            "    return lf_surface_half(vec4<f32>(rgb, 1.0));",
            "    return lf_output_requantize(rgb);",
        ),
        SourceKind::Planes => (
            (0..TILES)
                .flat_map(|tile| {
                    (0..3).map(move |plane| {
                        format!(
                            "@group(1) @binding({}) var lf_source_t{tile}_{plane}: texture_2d<f32>;\n",
                            tile * 3 + plane
                        )
                    })
                })
                .collect::<String>(),
            {
                let load = |tile: usize| {
                    format!(
                        "rgb = vec3<f32>(textureLoad(lf_source_t{tile}_0, q, 0).r, \
                         textureLoad(lf_source_t{tile}_1, q, 0).r, \
                         textureLoad(lf_source_t{tile}_2, q, 0).r);"
                    )
                };
                format!(
                    "
fn lf_source_texel(at: vec2<u32>) -> vec3<f32> {{
    let tile = lf_source_word(0u);
    let q = vec2<i32>(at % vec2<u32>(tile));
    var rgb = vec3<f32>(0.0);
    switch (at.y / tile) * 2u + at.x / tile {{
        case 0u: {{ {} }}
        case 1u: {{ {} }}
        case 2u: {{ {} }}
        default: {{ {} }}
    }}
    return rgb;
}}
",
                    load(0),
                    load(1),
                    load(2),
                    load(3)
                )
            },
            "    return vec4<f32>(rgb, 1.0);",
            "    return rgb;",
        ),
    };
    format!(
        "
@group(0) @binding(0) var<storage, read> lf_source_words: array<u32>;
{bindings}
{encoding}
{HALF_ROUNDING}

fn lf_source_word(i: u32) -> u32 {{
    return lf_source_words[i];
}}

fn lf_source_f32(i: u32) -> f32 {{
    return bitcast<f32>(lf_source_words[i]);
}}

{texel}

// The held texel the content stage's pixel `p` reads: the view's orientation, a signed
// permutation and a translation.
fn lf_source_held(p: vec2<u32>) -> vec2<u32> {{
    let x = i32(p.x);
    let y = i32(p.y);
    let a = bitcast<i32>(lf_source_word(3u));
    let b = bitcast<i32>(lf_source_word(4u));
    let tx = bitcast<i32>(lf_source_word(5u));
    let c = bitcast<i32>(lf_source_word(6u));
    let d = bitcast<i32>(lf_source_word(7u));
    let ty = bitcast<i32>(lf_source_word(8u));
    return vec2<u32>(u32(a * x + b * y + tx), u32(c * x + d * y + ty));
}}

fn lf_source_pixel(p: vec2<u32>) -> vec3<f32> {{
    return lf_source_texel(lf_source_held(p));
}}

fn lf_source_store(rgb: vec3<f32>) -> vec4<f32> {{
{store}
}}

fn lf_source_settle(rgb: vec3<f32>) -> vec3<f32> {{
{settle}
}}

@vertex
fn lf_source_vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {{
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}}

@fragment
fn lf_source_cut(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {{
    let p = vec2<u32>(position.xy) + vec2<u32>(lf_source_word(9u), lf_source_word(10u));
    return lf_source_store(lf_source_pixel(p));
}}

@fragment
fn lf_source_reduce(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {{
    let q = vec2<u32>(position.xy) + vec2<u32>(lf_source_word(9u), lf_source_word(10u));
    let across = lf_source_word(11u);
    let across_offsets = lf_source_word(12u);
    let across_weights = lf_source_word(13u);
    let down = lf_source_word(14u);
    let down_offsets = lf_source_word(15u);
    let down_weights = lf_source_word(16u);
    let first_x = lf_source_word(across + q.x);
    let x0 = lf_source_word(across_offsets + q.x);
    let x1 = lf_source_word(across_offsets + q.x + 1u);
    let first_y = lf_source_word(down + q.y);
    let y0 = lf_source_word(down_offsets + q.y);
    let y1 = lf_source_word(down_offsets + q.y + 1u);
    // Across each source row first, then down the rows, as the CPU's build averages.
    var sum = vec3<f32>(0.0);
    for (var j = y0; j < y1; j = j + 1u) {{
        let y = first_y + (j - y0);
        var row = vec3<f32>(0.0);
        for (var i = x0; i < x1; i = i + 1u) {{
            row = row + lf_source_pixel(vec2<u32>(first_x + (i - x0), y))
                * lf_source_f32(across_weights + i);
        }}
        sum = sum + row * lf_source_f32(down_weights + j);
    }}
    return lf_source_store(lf_source_settle(sum));
}}
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each orientation's map takes the content stage onto the held texels as the core's view
    /// does, corner for corner.
    #[test]
    fn the_orientation_map_is_the_views() {
        let (width, height) = (5u32, 3u32);
        for orientation in 1..=8u8 {
            let map = orientation_map(orientation, (width, height)).expect("1 to 8");
            let stage = if orientation >= 5 {
                (height, width)
            } else {
                (width, height)
            };
            let view = |x: i32, y: i32| -> (i32, i32) {
                let (w, h) = (width as i32 - 1, height as i32 - 1);
                match orientation {
                    1 => (x, y),
                    2 => (w - x, y),
                    3 => (w - x, h - y),
                    4 => (x, h - y),
                    5 => (y, x),
                    6 => (y, h - x),
                    7 => (w - y, h - x),
                    _ => (w - y, x),
                }
            };
            for y in 0..stage.1 as i32 {
                for x in 0..stage.0 as i32 {
                    let [a, b, tx, c, d, ty] = map;
                    assert_eq!(
                        (a * x + b * y + tx, c * x + d * y + ty),
                        view(x, y),
                        "orientation {orientation} at ({x}, {y})"
                    );
                }
            }
        }
        assert!(orientation_map(9, (width, height)).is_none());
    }

    /// The shaders of both kinds validate with the `naga` wgpu uses.
    #[test]
    fn the_derivation_shaders_validate() {
        let encoding = encoding().expect("the test tables");
        for kind in [SourceKind::Codes, SourceKind::Planes] {
            let source = shader(kind, encoding);
            let module = wgpu::naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|error| panic!("{kind:?}: {}", error.emit_to_string(&source)));
            wgpu::naga::valid::Validator::new(
                wgpu::naga::valid::ValidationFlags::all(),
                wgpu::naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .unwrap_or_else(|error| panic!("{kind:?}: {error:?}"));
        }
    }
}
