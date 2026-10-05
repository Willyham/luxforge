//! The picture at rest on the GPU, process-first (`docs/design/gpu-preview.md`, "The picture at
//! rest"; `docs/design/gpu-first.md`, stage 2): the stack drawn at full resolution in tiles, each
//! tile a region plan of the output stage over its own window of the source the pipeline holds
//! ([`super::GpuBoundary::derived`]), reduced as it is drawn into the view's size by an
//! area-weighted average of its linear light — the reduction the reference frame is held to at Fit
//! and below 100%.
//!
//! - **Tiles a frame.** A surface handed a picture at rest ([`GpuRest`]) draws its tiles in their
//!   order, row by row, [`REST_TILES_PER_FRAME`] each frame, through a slot of its own, the chain a
//!   gesture's frame runs, each tile a fresh evaluation of its own boundary, its pool's records
//!   forgotten, whatever else the frame draws: the desktop hands a picture at rest only while no
//!   draft is open, so the frames a tile shares are at most a released gesture's last. The widget
//!   asks for the next frame while tiles remain, and for nothing after the last, so an idle editor
//!   sleeps once the picture is drawn.
//! - **The reduction.** After each tile one compute pass adds, for every view pixel the tile
//!   reaches, the tile's codes decoded through the output's table and weighted by the reduction's
//!   coverage across and down, into an `f32` accumulator of the view's size. Each view pixel is
//!   one invocation's, and the tiles are added in their fixed order, so the sums are the same every
//!   time. After the last tile one more pass quantizes the accumulator through the output's
//!   thresholds into the rest output, an `rgba8unorm` texture of the view's size the photograph's
//!   draw samples through its sRGB view, as it samples the CPU's frame.
//! - **A tile's codes.** Each tile's codes are in its slot's output, an `rgba8unorm` texture with
//!   texture binding, drawn without the clipping marks, the tile's own pixels at `(0, 0, width,
//!   height)`, its window's halo past them in the boundary only: [`RestSlot::tile_drawn`] encodes a
//!   reduction of the tile, the area average's and any other, before the slot draws the next tile
//!   into the same texture.
//! - **Abandoned, never stale.** A surface handed another picture at rest, or none, lets the one it
//!   draws go and starts over from its first tile, and the rest output is drawn only once its last
//!   tile is in it: in place of the photograph's frame and of a plan's output alike, dissolving in
//!   over the plan's output the frame before ([`RestFigures::dissolving`]).
//! - **The counts.** Every tile's codes are also added into the histogram and clipping counts
//!   ([`super::histogram`]), a tile's own rectangle at a time, so after the last tile the counts are
//!   the whole output stage's, read back without a wait ([`RestCounts`]). A picture at rest with no
//!   reduction to a view ([`GpuRest::reduction`]) is drawn for its counts alone: where the view
//!   draws the stage at its own size or larger — at 100% and above, through a percentage view's
//!   surface too — or while the clipping overlay's marks are the view plan's. It has no
//!   accumulator and no rest output, and draws nothing.
//! - **Bounds.** The accumulator takes sixteen bytes a view pixel, 128 MiB at the 8-megapixel
//!   display bounds ([`REST_VIEW_PIXELS`]); the rest output four; the coverage tables a few words
//!   a view row and column; the tile slot what a 100% region's slot over the tile's window takes;
//!   the counts the surface's reduction's 3,096 bytes and its kernel's table. Each is charged to
//!   the GPU-preview budget before it is created, and leaves through the retirement worker.
use super::super::{PhotoPipeline, Picture, SurfaceSlots, Tile, TileLayout, UNIFORM_SIZE};
use super::histogram::{
    Counts, HistogramError, HistogramReadback, HistogramRect, HistogramReduction,
};
use super::{
    AxisCoverage, Charged, GpuFallback, GpuPlan, Held, OUTPUT_FORMAT, SAMPLED_FORMAT, answered,
    buffer_capacity, storage_buffer, tail::encoding, validate,
};
use std::sync::{Arc, Mutex};

/// How many tiles a surface draws a frame: one, a 2048-pixel tile's chain at most, so a frame
/// that draws one stays a frame.
pub const REST_TILES_PER_FRAME: usize = 1;

/// The most view pixels a picture at rest is reduced to: the display bounds' 8 megapixels, whose
/// accumulator takes 128 MiB.
pub const REST_VIEW_PIXELS: u64 = 1 << 23;

/// The side of the reduction's and the quantization's workgroups.
const GROUP: u32 = 8;

/// The words of the passes' parameters: the tile's rectangle of the output stage, the view pixels
/// it reaches, the view's size and where each coverage table starts.
const PARAMS: usize = 16;

/// A picture at rest a surface is handed: plain data, as every plan is, so this crate names no core
/// type.
#[derive(Clone, Debug)]
pub struct GpuRest {
    /// Changes whenever the tiles or the reduction do: a picture at rest to draw anew.
    pub version: u64,
    /// The tiles' plans, in the order they are drawn, row by row: each a region plan of the output
    /// stage at full scale, its region the tile, its boundary derived from the source. They cover
    /// the output stage once, which the counts are the whole stage's for.
    pub tiles: Arc<[GpuPlan]>,
    /// The reduction of the tiles to the view's size, which is the picture drawn; `None` draws
    /// the tiles for their counts alone, and nothing on screen.
    pub reduction: Option<RestReduction>,
}

/// A picture at rest's reduction of the output stage to the view's size.
#[derive(Clone, Debug, PartialEq)]
pub struct RestReduction {
    /// The view's size the output stage is reduced to.
    pub view: (u32, u32),
    /// The reduction's coverage of the output stage across and down: for each view column and row,
    /// the output pixels it averages and their weights.
    pub across: AxisCoverage,
    pub down: AxisCoverage,
}

impl GpuRest {
    /// Whether it can be drawn: every tile a region plan and, with a reduction, a view within the
    /// bounds with coverage tables of its size.
    fn valid(&self) -> bool {
        !self.tiles.is_empty()
            && self.tiles.iter().all(|tile| tile.region.is_some())
            && self.reduction.as_ref().is_none_or(RestReduction::valid)
    }
}

impl RestReduction {
    /// A view within the bounds, and coverage tables of the view's size.
    fn valid(&self) -> bool {
        let axis = |coverage: &AxisCoverage, size: u32| {
            coverage.first.len() == size as usize
                && coverage.offsets.len() == size as usize + 1
                && coverage.offsets.windows(2).all(|pair| pair[0] <= pair[1])
                && coverage
                    .offsets
                    .last()
                    .is_some_and(|last| *last as usize == coverage.weights.len())
        };
        self.view.0 > 0
            && self.view.1 > 0
            && u64::from(self.view.0) * u64::from(self.view.1) <= REST_VIEW_PIXELS
            && axis(&self.across, self.view.0)
            && axis(&self.down, self.view.1)
    }

    /// The coverage tables as the passes read them, across then down, each its first output
    /// pixels, its offsets and its weights' `f32` bits, and where each of the six starts.
    fn tables(&self) -> (Vec<u32>, [u32; 6]) {
        let mut words = Vec::new();
        let mut starts = [0; 6];
        for (axis, coverage) in [&self.across, &self.down].into_iter().enumerate() {
            starts[3 * axis] = words.len() as u32;
            words.extend_from_slice(&coverage.first);
            starts[3 * axis + 1] = words.len() as u32;
            words.extend_from_slice(&coverage.offsets);
            starts[3 * axis + 2] = words.len() as u32;
            words.extend(coverage.weights.iter().map(|weight| weight.to_bits()));
        }
        (words, starts)
    }
}

/// The view indices along one axis whose coverage meets the output pixels `[from, to)`: the half-
/// open range a tile of them reaches. `O(view)`.
fn reach(coverage: &AxisCoverage, from: u32, to: u32) -> (u32, u32) {
    let mut reached = None;
    for (index, first) in coverage.first.iter().enumerate() {
        let count = coverage.offsets[index + 1] - coverage.offsets[index];
        if *first < to && first + count > from {
            let (start, _) = reached.get_or_insert((index as u32, index as u32 + 1));
            reached = Some((*start, index as u32 + 1));
        }
    }
    reached.unwrap_or((0, 0))
}

/// What a surface's diagnostics say of its picture at rest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RestFigures {
    pub version: u64,
    pub tiles: u32,
    /// The tiles drawn and reduced so far.
    pub drawn: u32,
    /// Every tile is reduced and the rest output quantized: the photograph can be drawn from it.
    pub done: bool,
    /// The next tile waits for its sequence to compile, its source to upload, or the slots released
    /// before it to retire: no frame is asked for it until then.
    pub waiting: bool,
    /// The rest output, its last tile just in, is dissolving in over the plan's output the surface
    /// drew before it: frames are asked for until the dissolve ends.
    pub dissolving: bool,
    /// The interface thread's time, in microseconds, its frames' `prepare` spent drawing its tiles
    /// and quantizing them: what the status bar names as the picture at rest's render.
    pub prepare_us: u64,
    /// Why a tile, or the rest's own textures, could not be drawn: the picture at rest is then the
    /// caller's to draw otherwise.
    pub fallback: Option<GpuFallback>,
    /// The tiles are drawn for their counts alone ([`GpuRest::reduction`] `None`).
    pub counts_only: bool,
}

/// The histogram and clipping counts of a picture at rest's tiles, or of a gesture's frame, as
/// the surface holds them: still being counted, on their way back from the GPU, or why there are
/// none. Its clones answer alike.
#[derive(Clone, Debug)]
pub(in super::super) enum RestCounts {
    /// Tiles remain to be counted.
    Counting,
    /// The last tile is counted and the counts are read back ([`HistogramReadback`]).
    Reading(HistogramReadback),
    /// No counts: the reduction could not be made or a tile could not be counted.
    Failed(HistogramError),
}

impl RestCounts {
    /// What a caller is told now, read without waiting.
    pub(in super::super) fn outcome(&self) -> CountsOutcome {
        match self {
            Self::Counting => CountsOutcome::Counting,
            Self::Reading(readback) => match readback.poll() {
                None => CountsOutcome::Counting,
                Some(Ok(counts)) => CountsOutcome::Ready(Box::new(counts)),
                Some(Err(error)) => CountsOutcome::Failed(error),
            },
            Self::Failed(error) => CountsOutcome::Failed(error.clone()),
        }
    }

    /// Whether counts are still on their way.
    pub(in super::super) fn pending(&self) -> bool {
        matches!(self.outcome(), CountsOutcome::Counting)
    }
}

/// A gesture's tick counted: the boundary and the revision of the frame it drew, that frame's
/// size, and its counts.
#[derive(Clone, Debug)]
pub(in super::super) struct TickCounted {
    pub(in super::super) boundary: u64,
    pub(in super::super) revision: u64,
    pub(in super::super) size: (u32, u32),
    pub(in super::super) counts: RestCounts,
}

impl TickCounted {
    /// As the desktop reads it, without a wait.
    pub(in super::super) fn read(&self) -> TickCounts {
        TickCounts {
            boundary: self.boundary,
            revision: self.revision,
            size: self.size,
            counts: self.counts.outcome(),
        }
    }
}

/// The counts of the frame a gesture's tick drew, the frame on screen in motion, as the desktop
/// reads them ([`crate::photo_surface::surface_counts`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickCounts {
    /// The boundary the tick's plan was drawn over, and the draft revision it was drawn for.
    pub boundary: u64,
    pub revision: u64,
    /// The frame's size: the displayed-size proxy at Fit and below 100%, the visible region's at
    /// 100% and above.
    pub size: (u32, u32),
    pub counts: CountsOutcome,
}

/// Counts as the desktop reads them ([`crate::photo_surface::surface_counts`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CountsOutcome {
    /// Not counted yet, or on their way back from the GPU.
    Counting,
    /// The counts, exact over the codes counted.
    Ready(Box<Counts>),
    /// Why there are none.
    Failed(HistogramError),
}

/// The reduction's and the quantization's passes, made once, the first time a surface of the
/// pipeline is handed a picture at rest.
pub(in super::super) struct RestPasses {
    reduce_layout: wgpu::BindGroupLayout,
    reduce: wgpu::ComputePipeline,
    quantize_layout: wgpu::BindGroupLayout,
    quantize: wgpu::ComputePipeline,
}

impl RestPasses {
    /// The passes on `device`, or why there are none: no output encoding installed yet, whose
    /// table and thresholds they decode and quantize through, or a shader the device refused.
    pub(super) fn new(device: &wgpu::Device) -> Result<Self, String> {
        let source = shader(encoding()?);
        validate(&source)?;
        let entry = |binding: u32, ty: wgpu::BindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let uniform = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let storage = |read_only: bool| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let reduce_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("luxforge.gpu_rest.reduce_layout"),
            entries: &[
                entry(0, uniform),
                entry(1, storage(true)),
                entry(2, storage(false)),
                entry(
                    3,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
            ],
        });
        let quantize_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("luxforge.gpu_rest.quantize_layout"),
            // The accumulator as the module declares it, read and written, though this pass reads
            // it only.
            entries: &[
                entry(0, uniform),
                entry(2, storage(false)),
                entry(
                    4,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: OUTPUT_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("luxforge.gpu_rest.shader"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = |layout: &wgpu::BindGroupLayout, entry: &str| {
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("luxforge.gpu_rest.pipeline_layout"),
                bind_group_layouts: &[layout],
                push_constant_ranges: &[],
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("luxforge.gpu_rest.pipeline"),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let reduce = pipeline(&reduce_layout, "lf_rest_reduce");
        let quantize = pipeline(&quantize_layout, "lf_rest_quantize");
        let validation = answered(device.pop_error_scope());
        let internal = answered(device.pop_error_scope());
        match (validation, internal) {
            (Some(None), Some(None)) => Ok(Self {
                reduce_layout,
                reduce,
                quantize_layout,
                quantize,
            }),
            (Some(Some(error)), _) | (_, Some(Some(error))) => Err(error.to_string()),
            _ => Err("the rest passes' error scopes were not answered without waiting".into()),
        }
    }
}

/// The reduction and the quantization over the output `encoding`'s table and thresholds.
fn shader(encoding: &str) -> String {
    format!(
        "{encoding}
@group(0) @binding(0) var<uniform> lf_rest_params: array<vec4<u32>, 4>;
@group(0) @binding(1) var<storage, read> lf_rest_tables: array<u32>;
@group(0) @binding(2) var<storage, read_write> lf_rest_sums: array<vec4<f32>>;
@group(0) @binding(3) var lf_rest_tile: texture_2d<f32>;

// Each view pixel the tile reaches: the tile's codes it covers, decoded and weighted across, then
// down, added to what the tiles before added. One invocation a view pixel, so nothing races.
@compute @workgroup_size({GROUP}, {GROUP}, 1)
fn lf_rest_reduce(@builtin(global_invocation_id) id: vec3<u32>) {{
    let tile = lf_rest_params[0];
    let view = lf_rest_params[1];
    let across = lf_rest_params[2];
    let down = lf_rest_params[3];
    let x = view.x + id.x;
    let y = view.y + id.y;
    if x >= view.z || y >= view.w {{
        return;
    }}
    let column = lf_rest_tables[across.y + x];
    let columns = vec2<u32>(lf_rest_tables[across.z + x], lf_rest_tables[across.z + x + 1u]);
    let row = lf_rest_tables[down.x + y];
    let rows = vec2<u32>(lf_rest_tables[down.y + y], lf_rest_tables[down.y + y + 1u]);
    var sum = vec3<f32>(0.0);
    for (var j = rows.x; j < rows.y; j = j + 1u) {{
        let sy = row + (j - rows.x);
        if sy < tile.y || sy >= tile.w {{
            continue;
        }}
        var along = vec3<f32>(0.0);
        for (var i = columns.x; i < columns.y; i = i + 1u) {{
            let sx = column + (i - columns.x);
            if sx < tile.x || sx >= tile.z {{
                continue;
            }}
            let texel = textureLoad(lf_rest_tile, vec2<i32>(i32(sx - tile.x), i32(sy - tile.y)), 0);
            let code = vec3<u32>(round(texel.rgb * 255.0));
            let linear = vec3<f32>(
                lf_output_decode(code.r),
                lf_output_decode(code.g),
                lf_output_decode(code.b),
            );
            along = along + bitcast<f32>(lf_rest_tables[across.w + i]) * linear;
        }}
        sum = sum + bitcast<f32>(lf_rest_tables[down.z + j]) * along;
    }}
    let at = y * across.x + x;
    lf_rest_sums[at] = lf_rest_sums[at] + vec4<f32>(sum, 0.0);
}}

@group(0) @binding(4) var lf_rest_output: texture_storage_2d<rgba8unorm, write>;

// Every view pixel's average quantized as the output is: its codes over 255, opaque.
@compute @workgroup_size({GROUP}, {GROUP}, 1)
fn lf_rest_quantize(@builtin(global_invocation_id) id: vec3<u32>) {{
    let width = lf_rest_params[2].x;
    let height = lf_rest_params[3].w;
    if id.x >= width || id.y >= height {{
        return;
    }}
    let sum = lf_rest_sums[id.y * width + id.x];
    let codes = vec3<f32>(
        f32(lf_output_code(sum.r)),
        f32(lf_output_code(sum.g)),
        f32(lf_output_code(sum.b)),
    );
    textureStore(lf_rest_output, vec2<i32>(id.xy), vec4<f32>(codes / 255.0, 1.0));
}}
"
    )
}

/// What a picture at rest's reduction to the view holds besides its tile slot, which retires as one.
pub(super) struct RestParts {
    sums: Charged,
    tables: Charged,
    params: Charged,
    output: Picture,
}

/// A picture at rest's reduction to the view: its coverage, its parts and the rest output's
/// storage view.
struct Reduce {
    view: (u32, u32),
    across: AxisCoverage,
    down: AxisCoverage,
    starts: [u32; 6],
    parts: RestParts,
    /// The rest output as the quantization writes it.
    storage: wgpu::TextureView,
}

/// A surface's picture at rest: its tiles, the slot they are drawn through, how far it has got,
/// the reduction to the view with its accumulator and rest output, and the counts.
pub(in super::super) struct RestSlot {
    version: u64,
    tiles: Arc<[GpuPlan]>,
    /// The next tile to draw.
    next: usize,
    /// Why a tile could not be drawn; the rest draws nothing more.
    fallback: Option<GpuFallback>,
    /// The slot every tile is drawn through, apart from the surface's own GPU slot, which a
    /// gesture's plan keeps.
    tile: Box<SurfaceSlots>,
    /// The reduction to the view's size; `None` for the counts' tiles alone.
    reduce: Option<Reduce>,
    /// The bytes of the reduction's parts, charged.
    bytes: u64,
    /// The rest output holds every tile's reduction, quantized, and the counts are read back.
    done: bool,
    /// The last frame found the next tile waiting for its sequence, its source or a retirement.
    waiting: bool,
    /// The interface thread's time its tiles and quantization took so far.
    prepare_us: u64,
    /// The tiles' histogram and clipping counts.
    counts: RestCounts,
}

impl RestSlot {
    /// The rest output, once its last tile is in it; none for the counts' tiles alone.
    pub(in super::super) fn output(&self) -> Option<&Picture> {
        self.reduce
            .as_ref()
            .filter(|_| self.done)
            .map(|reduce| &reduce.parts.output)
    }

    pub(in super::super) fn figures(&self) -> RestFigures {
        RestFigures {
            version: self.version,
            tiles: self.tiles.len() as u32,
            drawn: self.next as u32,
            done: self.done,
            waiting: self.waiting,
            prepare_us: self.prepare_us,
            dissolving: false,
            fallback: self.fallback,
            counts_only: self.reduce.is_none(),
        }
    }

    /// Its version, and its counts as they stand.
    pub(in super::super) fn counts(&self) -> (u64, RestCounts) {
        (self.version, self.counts.clone())
    }

    /// Whether tiles remain to draw, which the widget asks the next frame for.
    pub(in super::super) fn pending(&self) -> bool {
        !self.done && self.fallback.is_none()
    }
}

impl Reduce {
    /// The tile just drawn, `plan`, its codes in `texture` at `(0, 0, width, height)`: encode its
    /// share of the area average into the accumulator.
    fn tile_drawn(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        passes: &RestPasses,
        texture: &wgpu::Texture,
        plan: &GpuPlan,
    ) {
        let Some(region) = plan.region else {
            return;
        };
        let [x0, y0, x1, y1] = region.rect;
        let (vx0, vx1) = reach(&self.across, x0, x1);
        let (vy0, vy1) = reach(&self.down, y0, y1);
        if vx0 >= vx1 || vy0 >= vy1 {
            return;
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.gpu_rest.reduce_bindings"),
            layout: &passes.reduce_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.parts.params.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.parts.tables.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.parts.sums.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("luxforge.gpu_rest.reduce"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&passes.reduce);
        pass.set_bind_group(0, &bindings, &[]);
        pass.dispatch_workgroups((vx1 - vx0).div_ceil(GROUP), (vy1 - vy0).div_ceil(GROUP), 1);
    }

    /// The parameters of a pass over the tile `rect` of the output stage.
    fn params(&self, rect: [u32; 4]) -> [u32; PARAMS] {
        let [x0, y0, x1, y1] = rect;
        let (vx0, vx1) = reach(&self.across, x0, x1);
        let (vy0, vy1) = reach(&self.down, y0, y1);
        let s = self.starts;
        [
            x0,
            y0,
            x1,
            y1,
            vx0,
            vy0,
            vx1,
            vy1,
            self.view.0,
            s[0],
            s[1],
            s[2],
            s[3],
            s[4],
            s[5],
            self.view.1,
        ]
    }

    /// Quantize the accumulator into the rest output, once its last tile is in.
    fn quantize(&self, device: &wgpu::Device, queue: &wgpu::Queue, passes: &RestPasses) {
        queue.write_buffer(
            &self.parts.params.buffer,
            0,
            &le_bytes(&self.params([0; 4])),
        );
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.gpu_rest.quantize_bindings"),
            layout: &passes.quantize_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.parts.params.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.parts.sums.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&self.storage),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.gpu_rest.quantize_encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("luxforge.gpu_rest.quantize"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&passes.quantize);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups(self.view.0.div_ceil(GROUP), self.view.1.div_ceil(GROUP), 1);
        }
        queue.submit([encoder.finish()]);
    }
}

fn le_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// The surface's histogram reduction, made the first time it counts and kept for its life: the
/// kernel compiled once, its counts buffer cleared for each picture at rest. Why there is none.
fn reduction<'a>(
    pipeline: &PhotoPipeline,
    device: &wgpu::Device,
    held: &'a mut Option<HistogramReduction>,
) -> Result<&'a mut HistogramReduction, HistogramError> {
    if held.is_none() {
        *held = Some(HistogramReduction::new(pipeline, device)?);
    }
    Ok(held.as_mut().expect("made above"))
}

impl PhotoPipeline {
    /// Draw `surface`'s picture at rest, `rest`: another version than the one it holds starts over,
    /// none lets it go. At most [`REST_TILES_PER_FRAME`] tiles are drawn a frame, each counted, and
    /// the last quantizes the rest output and reads the counts back. A tile the stage cannot draw
    /// yet — its sequence compiling, its source uploading — waits for a later frame; any other
    /// fallback stops the picture at rest, naming why.
    pub(in super::super) fn prepare_rest(
        &mut self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rest: Option<&GpuRest>,
    ) {
        let Some(rest) = rest else {
            self.release_rest(surface);
            return;
        };
        if surface
            .rest
            .as_ref()
            .is_none_or(|slot| slot.version != rest.version)
        {
            self.release_rest(surface);
            surface.rest_refused = None;
            if !rest.valid() {
                surface.rest_refused = Some((rest.version, GpuFallback::PipelineFailed));
                return;
            }
            match self.allocate_rest(device, queue, rest) {
                Ok(mut slot) => {
                    if let Err(error) = reduction(self, device, &mut surface.histogram) {
                        slot.counts = RestCounts::Failed(error);
                    }
                    surface.rest = Some(Box::new(slot));
                }
                Err(fallback) => {
                    surface.rest_refused = Some((rest.version, fallback));
                    return;
                }
            }
        }
        let Some(slot) = surface.rest.as_deref_mut() else {
            return;
        };
        if !slot.pending() {
            return;
        }
        let passes = match &slot.reduce {
            None => None,
            Some(_) => match self.gpu.rest_passes(device) {
                Some(Ok(passes)) => Some(Arc::clone(passes)),
                _ => {
                    slot.fallback = Some(GpuFallback::PipelineFailed);
                    return;
                }
            },
        };
        let started = std::time::Instant::now();
        self.draw_rest(
            slot,
            surface.histogram.as_mut(),
            device,
            queue,
            passes.as_deref(),
        );
        slot.prepare_us += started.elapsed().as_micros() as u64;
    }

    /// The tiles `slot` draws this frame, each reduced to the view and counted, and after its last
    /// the quantization and the counts' readback: what one frame's `prepare` spends on a picture at
    /// rest. `passes` are the reduction's, for a picture at rest with one.
    fn draw_rest(
        &mut self,
        slot: &mut RestSlot,
        mut histogram: Option<&mut HistogramReduction>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        passes: Option<&RestPasses>,
    ) {
        for _ in 0..REST_TILES_PER_FRAME {
            let Some(plan) = slot.tiles.get(slot.next).cloned() else {
                break;
            };
            // A fresh evaluation of the tile's own boundary: nothing the slot holds of another
            // tile is reused.
            if let Some(tile) = slot.tile.gpu.as_mut() {
                tile.forget_evaluation();
            }
            slot.waiting = false;
            match self.evaluate(&mut slot.tile, device, queue, &plan, None) {
                Ok(_) => {}
                Err(GpuFallback::Compiling | GpuFallback::SourceUploading { .. }) => {
                    slot.waiting = true;
                    return;
                }
                // The budget holds this tile once what the tiles before it released has retired,
                // their slots refitted to another window's size: it is drawn a frame later, as a
                // tick waits for the slot it replaces.
                Err(GpuFallback::BudgetExceeded { .. })
                    if self
                        .figures
                        .retirement_pending
                        .load(std::sync::atomic::Ordering::Acquire)
                        > 0 =>
                {
                    slot.waiting = true;
                    return;
                }
                Err(fallback) => {
                    slot.fallback = Some(fallback);
                    self.release_gpu(&mut slot.tile);
                    return;
                }
            }
            let Some(texture) = slot
                .tile
                .gpu
                .as_ref()
                .map(|tile| tile.output().tiles[0].texture.clone())
            else {
                slot.fallback = Some(GpuFallback::PipelineFailed);
                return;
            };
            let rect = plan.region.map_or([0; 4], |region| region.rect);
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("luxforge.gpu_rest.tile_encoder"),
            });
            if let (Some(reduce), Some(passes)) = (&slot.reduce, passes) {
                queue.write_buffer(
                    &reduce.parts.params.buffer,
                    0,
                    &le_bytes(&reduce.params(rect)),
                );
                if slot.next == 0 {
                    encoder.clear_buffer(&reduce.parts.sums.buffer, 0, None);
                }
                reduce.tile_drawn(device, &mut encoder, passes, &texture, &plan);
            }
            // The tile's own codes, at the output's first texels, past its halo: counted into the
            // whole stage's counts, which its first tile clears.
            if let (RestCounts::Counting, Some(histogram)) =
                (&slot.counts, histogram.as_deref_mut())
            {
                if slot.next == 0 {
                    histogram.clear(&mut encoder);
                }
                let [x0, y0, x1, y1] = rect;
                let counted = histogram.reduce(
                    &mut encoder,
                    &texture,
                    HistogramRect {
                        x: 0,
                        y: 0,
                        width: x1 - x0,
                        height: y1 - y0,
                    },
                );
                if let Err(error) = counted {
                    slot.counts = RestCounts::Failed(error);
                }
            }
            queue.submit([encoder.finish()]);
            slot.next += 1;
        }
        if slot.next < slot.tiles.len() {
            return;
        }
        // The last tile is in: quantize the accumulator into the rest output, read the counts back
        // and let the tile slot go, which nothing draws again.
        if let (Some(reduce), Some(passes)) = (&mut slot.reduce, passes) {
            reduce.quantize(device, queue, passes);
            reduce.parts.output.version = slot.version;
        }
        if let (RestCounts::Counting, Some(histogram)) = (&slot.counts, histogram) {
            let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("luxforge.gpu_rest.counts_encoder"),
            });
            slot.counts = RestCounts::Reading(histogram.read_back(queue, encoder));
        }
        slot.done = true;
        self.release_gpu(&mut slot.tile);
        super::super::wake_surface();
    }

    /// Retire `surface`'s picture at rest, if it holds one: its tile slot and its own parts.
    pub(in super::super) fn release_rest(&self, surface: &mut SurfaceSlots) {
        let Some(mut slot) = surface.rest.take() else {
            return;
        };
        self.release_gpu(&mut slot.tile);
        let RestSlot { reduce, bytes, .. } = *slot;
        if let Some(reduce) = reduce {
            self.retire_preview(Held::Rest(Box::new(reduce.parts)), bytes);
        }
    }

    /// A picture at rest's slot for `rest`, with its reduction's parts where it has one, charged
    /// before anything is created: the accumulator, the coverage tables, the passes' parameters and
    /// the rest output with its placement.
    fn allocate_rest(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rest: &GpuRest,
    ) -> Result<RestSlot, GpuFallback> {
        let (reduce, bytes) = match &rest.reduction {
            None => (None, 0),
            Some(reduction) => {
                let (reduce, bytes) = self.allocate_reduction(device, queue, reduction)?;
                (Some(reduce), bytes)
            }
        };
        Ok(RestSlot {
            version: rest.version,
            tiles: Arc::clone(&rest.tiles),
            next: 0,
            fallback: None,
            tile: Box::new(self.new_surface()),
            reduce,
            bytes,
            done: false,
            waiting: false,
            prepare_us: 0,
            counts: RestCounts::Counting,
        })
    }

    /// A reduction's parts for `reduction`, and what they are charged, charged before anything is
    /// created.
    fn allocate_reduction(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        reduction: &RestReduction,
    ) -> Result<(Reduce, u64), GpuFallback> {
        let (width, height) = reduction.view;
        let (words, starts) = reduction.tables();
        let sums_bytes = buffer_capacity(device, u64::from(width) * u64::from(height) * 16)?;
        let table_bytes = buffer_capacity(device, (words.len().max(1) * 4) as u64)?;
        let params_bytes = (PARAMS * 4) as u64;
        let output_bytes = u64::from(width) * u64::from(height) * 4;
        let bytes = sums_bytes + table_bytes + params_bytes + output_bytes + UNIFORM_SIZE as u64;
        self.figures.preview.charge(bytes)?;
        let sums = Charged {
            buffer: storage_buffer(device, "luxforge.gpu_rest.sums", sums_bytes),
            bytes: sums_bytes,
        };
        let tables = Charged {
            buffer: storage_buffer(device, "luxforge.gpu_rest.tables", table_bytes),
            bytes: table_bytes,
        };
        queue.write_buffer(&tables.buffer, 0, &le_bytes(&words));
        let params = Charged {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("luxforge.gpu_rest.params"),
                size: params_bytes,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            bytes: params_bytes,
        };
        // Written through its storage view as the codes the quantization computes, sampled through
        // an sRGB-typed view, as the photograph's textures are, and copied out by a headless read.
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("luxforge.gpu_rest.output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUTPUT_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[SAMPLED_FORMAT],
        });
        let storage = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Sampled only: an sRGB view cannot be a storage one.
        let sampled = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(SAMPLED_FORMAT),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            ..wgpu::TextureViewDescriptor::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("luxforge.gpu_rest.uniform"),
            size: UNIFORM_SIZE as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Drawn as a photograph tile is drawn: the same layout, uniform and linear sampler.
        let photo_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.gpu_rest.photo_bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&sampled),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.linear),
                },
            ],
        });
        let layout = TileLayout {
            content: [0, 0, width, height],
            texels: [0, 0, width, height],
        };
        let output = Picture {
            tiles: vec![Tile {
                layout,
                capacity: (width, height),
                texture,
                uniform,
                written_uniform: Mutex::new(None),
                bindings: photo_bindings,
            }],
            width,
            height,
            capacity: (width, height),
            grid: (1, 1),
            limit: device.limits().max_texture_dimension_2d,
            version: 0,
            content_id: None,
            region_key: None,
            allocated_bytes: output_bytes,
            mip_levels: 1,
            mip_bytes: 0,
            mips_current: false,
        };
        Ok((
            Reduce {
                view: reduction.view,
                across: reduction.across.clone(),
                down: reduction.down.clone(),
                starts,
                parts: RestParts {
                    sums,
                    tables,
                    params,
                    output,
                },
                storage,
            },
            bytes,
        ))
    }

    /// Count the frame `surface`'s gesture's plan, `plan`, just drew — the frame on screen in
    /// motion, the displayed-size proxy at Fit and below 100% or the visible region at 100% and
    /// above — once for each tick, and read the counts back, tagged with the tick's boundary and
    /// revision: when the slot evaluated the plan this frame, its output carries no clipping marks
    /// and the surface's last tick's counts are not still on their way.
    pub(in super::super) fn count_tick(
        &mut self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: Option<&GpuPlan>,
    ) {
        let (Some(plan), Some(Ok(boundary)), Some(tag)) =
            (plan, surface.gpu_outcome, surface.gpu_tag)
        else {
            return;
        };
        let key = (boundary, tag);
        if surface.tick_counted == Some(key)
            || matches!(plan.steps.last(), Some(super::GpuStep::Clipping(_)))
            || surface
                .tick_counts
                .as_ref()
                .is_some_and(|tick| tick.counts.pending())
        {
            return;
        }
        let Some(output) = surface.gpu.as_ref().map(super::GpuSlot::output) else {
            return;
        };
        let [tile] = output.tiles.as_slice() else {
            return;
        };
        let (width, height) = plan.region.map_or((output.width, output.height), |region| {
            let [x0, y0, x1, y1] = region.rect;
            (x1 - x0, y1 - y0)
        });
        let texture = tile.texture.clone();
        surface.tick_counted = Some(key);
        let counted = |counts| TickCounted {
            boundary,
            revision: tag,
            size: (width, height),
            counts,
        };
        let histogram = match reduction(self, device, &mut surface.tick_histogram) {
            Ok(histogram) => histogram,
            Err(error) => {
                surface.tick_counts = Some(counted(RestCounts::Failed(error)));
                return;
            }
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.gpu_preview.tick_counts"),
        });
        histogram.clear(&mut encoder);
        let counts = match histogram.reduce(
            &mut encoder,
            &texture,
            HistogramRect {
                x: 0,
                y: 0,
                width,
                height,
            },
        ) {
            Ok(()) => RestCounts::Reading(histogram.read_back(queue, encoder)),
            Err(error) => {
                queue.submit([encoder.finish()]);
                RestCounts::Failed(error)
            }
        };
        surface.tick_counts = Some(counted(counts));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage(first: Vec<u32>, counts: &[u32]) -> AxisCoverage {
        let mut offsets = vec![0];
        for count in counts {
            offsets.push(offsets.last().unwrap() + count);
        }
        let weights = counts
            .iter()
            .flat_map(|count| std::iter::repeat_n(1.0 / *count as f32, *count as usize))
            .collect();
        AxisCoverage {
            first,
            offsets,
            weights,
        }
    }

    /// The view pixels a tile reaches are those whose coverage meets its output pixels: a halved
    /// axis of eight output pixels, two to a view pixel, and tiles of three.
    #[test]
    fn a_tile_reaches_the_view_pixels_its_coverage_meets() {
        let halved = coverage(vec![0, 2, 4, 6], &[2, 2, 2, 2]);
        assert_eq!(reach(&halved, 0, 3), (0, 2));
        assert_eq!(reach(&halved, 3, 6), (1, 3));
        assert_eq!(reach(&halved, 6, 8), (3, 4));
        assert_eq!(reach(&halved, 8, 9), (0, 0));
    }
}
