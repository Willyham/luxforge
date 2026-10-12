//! The resample sweep (`docs/design/export-settings.md`, "Rendering"; `docs/design/gpu-first.md`,
//! "Staged sweeps"): an export's output smaller than its output stage, or encoded at 16 bits, cut
//! from the output stage's linear values held whole on the device and read back band by band.
//!
//! - **The held stage.** Every tile of the stream draws its rectangle's linear values, as
//!   [`super::tiles::TileEnd::Linear`] writes them, into one [`super::staged::StageHolder`] of the
//!   whole output stage in `Rgba32Float` ([`super::tiles::TileOutput::Held`]), held by the tile
//!   runner beside its stage textures ([`super::tiles::TileRunner::hold_output`]).
//! - **A cut.** Each resample cut ([`ResampleCut`]) is a rectangle of the output: the window of the
//!   stage its taps read is copied out of the holder into a scratch texture, texel for texel; one
//!   pass clamps each value to `[0, 1]`, NaN taken to 0, as the core's wide terminal holds it, and
//!   sums each output column's taps across every row of the window into an `f32` buffer, and
//!   a second sums each output row's taps down it, clamps to `[0, 1]` with NaN taken to 0, encodes
//!   once at the output's depth and packs the codes four bytes a pixel, or eight, into a buffer
//!   copied out for the runner to read back. Each sum is taken in ascending order in `f32`; the
//!   taps are the core's own Lanczos-3 taps, handed over and narrowed to `f32`. A side kept
//!   at its size has one tap of weight one, which copies its values exactly.
//! - **Encoding.** 8-bit codes through the installed output encoding's thresholds, as the photo
//!   surface's output quantizer encodes them ([`super::tail::encoding`]); 16-bit codes through the
//!   core's 16-bit thresholds ([`install_wide_encoding`]), searched in a storage buffer.
//! - **Bounds.** A cut's window is at most the device's largest texture side, at most 8,192, either
//!   way, and at most [`RESAMPLE_SCRATCH_TEXELS`] texels, so one output pixel's taps span at most
//!   2,048 × 2,048 stage pixels at an even reduction, about 340 times; a cut holds at most
//!   [`RESAMPLE_ROWS`] output rows ([`resample_cuts`]). Its scratch, its sums, its taps, its codes
//!   and its readback are charged before anything is created ([`resample_charge`]).
use super::GpuFallback;
use std::sync::OnceLock;

/// The most texels a cut's window holds: 4 Mi, 64 MiB at 16 bytes a texel, and as much again for
/// its sums across.
pub const RESAMPLE_SCRATCH_TEXELS: u64 = 1 << 22;

/// The most output rows one cut holds: the most rows one band of a resampled stream holds.
pub const RESAMPLE_ROWS: u32 = 256;

/// The largest side of a cut's window, whatever the device allows past it.
const RESAMPLE_SIDE: u32 = 8192;

/// How many 16-bit code thresholds there are: one between each pair of the 65,536 codes.
pub const WIDE_THRESHOLDS: usize = 65535;

/// The bits per channel a cut encodes its codes at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResampleDepth {
    Eight,
    Sixteen,
}

impl ResampleDepth {
    /// Bytes a pixel of codes takes, read back.
    pub fn pixel_bytes(self) -> u32 {
        match self {
            Self::Eight => 4,
            Self::Sixteen => 8,
        }
    }
}

/// One output column's or row's taps: the stage pixels from `first` on, each weighted.
#[derive(Clone, Debug, PartialEq)]
pub struct ResampleTaps {
    pub first: u32,
    pub weights: Vec<f32>,
}

impl ResampleTaps {
    /// The stage pixel past its last.
    pub fn end(&self) -> u32 {
        self.first + self.weights.len() as u32
    }
}

/// A rectangle of the output, `[columns) × [rows)` in output pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResampleRange {
    pub columns: std::ops::Range<u32>,
    pub rows: std::ops::Range<u32>,
}

/// One cut of an output: its columns' and rows' taps, and the depth it is encoded at.
#[derive(Clone, Copy, Debug)]
pub struct ResampleCut<'a> {
    pub columns: &'a [ResampleTaps],
    pub rows: &'a [ResampleTaps],
    pub depth: ResampleDepth,
}

/// The stage pixels `taps` read, `[first, end)`; `None` for no taps.
fn span(taps: &[ResampleTaps]) -> Option<(u32, u32)> {
    let first = taps.iter().map(|taps| taps.first).min()?;
    let end = taps.iter().map(ResampleTaps::end).max()?;
    Some((first, end))
}

impl ResampleCut<'_> {
    /// The window of the stage its taps read, `[x, y, width, height]`.
    pub fn window(&self) -> Option<[u32; 4]> {
        let (x0, x1) = span(self.columns)?;
        let (y0, y1) = span(self.rows)?;
        Some([x0, y0, x1 - x0, y1 - y0])
    }
}

/// The cuts of an output whose columns' and rows' taps are `columns` and `rows`, on a device whose
/// largest texture side is `limit` ([module documentation](self), "Bounds"): bands of at most
/// [`RESAMPLE_ROWS`] whole output rows, top to bottom, each cut across into windows the scratch
/// holds, left to right. Refused, `texture-limit`, where one output pixel's taps reach past a
/// window's largest side, and for an empty output.
pub fn resample_cuts(
    columns: &[ResampleTaps],
    rows: &[ResampleTaps],
    limit: u32,
) -> Result<Vec<ResampleRange>, GpuFallback> {
    let side = limit.clamp(1, RESAMPLE_SIDE);
    let refused = |width: u32, height: u32| GpuFallback::TextureLimit {
        width,
        height,
        limit: side,
    };
    span(columns).ok_or(refused(0, 0))?;
    span(rows).ok_or(refused(0, 0))?;
    // The rows a band's window may hold: as many as leave room across for the widest one output
    // column's taps, so a band is cut across into windows as narrow as one column needs.
    let across = columns
        .iter()
        .map(|taps| taps.weights.len() as u32)
        .max()
        .unwrap_or(1)
        .clamp(1, side);
    let tallest = (RESAMPLE_SCRATCH_TEXELS / u64::from(across)).min(u64::from(side)) as u32;
    let mut cuts = Vec::new();
    let mut top = 0;
    while top < rows.len() {
        let mut bottom = top + 1;
        let (first, mut end) = (rows[top].first, rows[top].end());
        if end - first > tallest {
            return Err(refused(across, end - first));
        }
        while bottom < rows.len()
            && bottom - top < RESAMPLE_ROWS as usize
            && rows[bottom].end().max(end) - first.min(rows[bottom].first) <= tallest
        {
            end = end.max(rows[bottom].end());
            bottom += 1;
        }
        let (first, end) = span(&rows[top..bottom]).expect("a band of rows");
        let high = end - first;
        let widest = (RESAMPLE_SCRATCH_TEXELS / u64::from(high)).min(u64::from(side)) as u32;
        let mut left = 0;
        while left < columns.len() {
            let (start, mut stop) = (columns[left].first, columns[left].end());
            if stop - start > widest {
                return Err(refused(stop - start, high));
            }
            let mut right = left + 1;
            while right < columns.len()
                && columns[right].end().max(stop) - start.min(columns[right].first) <= widest
            {
                stop = stop.max(columns[right].end());
                right += 1;
            }
            cuts.push(ResampleRange {
                columns: left as u32..right as u32,
                rows: top as u32..bottom as u32,
            });
            left = right;
        }
        top = bottom;
    }
    Ok(cuts)
}

/// The words of `taps` as the passes read them, each `[first − origin, count, offset]` into the
/// weights, and the weights, every tap's in order.
pub(crate) fn packed(taps: &[ResampleTaps], origin: u32) -> (Vec<u32>, Vec<f32>) {
    let mut words = Vec::with_capacity(taps.len() * 3);
    let mut weights = Vec::new();
    for tap in taps {
        words.extend([
            tap.first - origin,
            tap.weights.len() as u32,
            weights.len() as u32,
        ]);
        weights.extend_from_slice(&tap.weights);
    }
    (words, weights)
}

/// `values` as the little-endian bytes a buffer holds them in.
pub(crate) fn float_bytes(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// The bytes of each buffer a cut creates, in the order [`super::tiles::TileRunner`] creates them:
/// the scratch window, the sums across, the columns' and rows' words and weights, the codes and the
/// readback copy. `None` for a cut with no taps.
pub(crate) fn cut_buffers(cut: &ResampleCut<'_>) -> Option<CutBytes> {
    let [_, _, width, height] = cut.window()?;
    let (columns, rows) = (cut.columns.len() as u64, cut.rows.len() as u64);
    let weights = |taps: &[ResampleTaps]| {
        taps.iter()
            .map(|taps| taps.weights.len() as u64)
            .sum::<u64>()
    };
    let codes = columns * rows * u64::from(cut.depth.pixel_bytes());
    Some(CutBytes {
        scratch: u64::from(width) * u64::from(height) * 16,
        across: columns * u64::from(height) * 16,
        taps: (columns + rows) * 12 + (weights(cut.columns) + weights(cut.rows)) * 4,
        codes,
        readback: codes,
    })
}

/// What a cut's buffers take ([`cut_buffers`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CutBytes {
    pub scratch: u64,
    pub across: u64,
    pub taps: u64,
    pub codes: u64,
    pub readback: u64,
}

impl CutBytes {
    /// Everything but its readback copy, which the GPU holds until it is done with the cut.
    pub fn transient(&self) -> u64 {
        self.scratch + self.across + self.taps + self.codes + PARAMS_BYTES
    }

    pub fn total(&self) -> u64 {
        self.transient() + self.readback
    }
}

/// What a cut of `cut` holds on the device at most, beside the held stage: its scratch window, its
/// sums across, its taps, its codes and their readback copy, and the 16-bit thresholds once.
pub fn resample_charge(cut: &ResampleCut<'_>) -> u64 {
    cut_buffers(cut).map_or(0, |bytes| bytes.total()) + WIDE_BYTES
}

/// The bytes of a cut's uniform words.
pub(crate) const PARAMS_BYTES: u64 = 16;

/// The bytes of the 16-bit thresholds' buffer.
pub(crate) const WIDE_BYTES: u64 = (WIDE_THRESHOLDS * 4) as u64;

static WIDE_ENCODING: OnceLock<Box<[f32]>> = OnceLock::new();

/// Hand the resample sweep the core's 16-bit output thresholds, once: the first `f32` of each code's
/// exact interval for codes 1 to 65,535. The first installed hold for the life of the process: the
/// answer is whether `thresholds` is them.
pub fn install_wide_encoding(thresholds: &[f32]) -> bool {
    thresholds.len() == WIDE_THRESHOLDS
        && **WIDE_ENCODING.get_or_init(|| thresholds.into()) == *thresholds
}

/// The installed 16-bit thresholds, if the desktop has installed them. A unit test of this crate
/// installs the shared test reference's.
pub fn wide_encoding() -> Option<&'static [f32]> {
    #[cfg(test)]
    install_wide_encoding(&reference_wide_encoding());
    WIDE_ENCODING.get().map(|thresholds| &**thresholds)
}

/// The 16-bit thresholds from the shared test reference's transfer function, as the core builds
/// its own: `decode((k − 0.5) / 65535)` in `f64` for code `k`, rounded up to the first `f32` in the
/// code's interval.
#[cfg(any(test, feature = "qualification"))]
pub fn reference_wide_encoding() -> Vec<f32> {
    (0..WIDE_THRESHOLDS)
        .map(|index| {
            let threshold =
                luxforge_reference::srgb::decode_encoded((index as f64 + 0.5) / 65535.0);
            let narrowed = threshold as f32;
            if f64::from(narrowed) < threshold {
                narrowed.next_up()
            } else {
                narrowed
            }
        })
        .collect()
}

/// The passes' work groups: 8 × 8 output pixels.
pub(crate) const GROUP: u32 = 8;

/// The pass across: each output column's taps summed across every row of the window, each value
/// clamped first.
const ACROSS: &str = "
struct LfResample { columns: u32, rows: u32, width: u32, height: u32 }
@group(0) @binding(0) var<uniform> lf_params: LfResample;
@group(0) @binding(1) var lf_window: texture_2d<f32>;
@group(0) @binding(2) var<storage, read> lf_taps: array<u32>;
@group(0) @binding(3) var<storage, read> lf_weights: array<f32>;
@group(0) @binding(4) var<storage, read_write> lf_across: array<vec4<f32>>;

@compute @workgroup_size(8, 8)
fn lf_resample_across(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= lf_params.columns || id.y >= lf_params.height {
        return;
    }
    let first = lf_taps[id.x * 3u];
    let count = lf_taps[id.x * 3u + 1u];
    let offset = lf_taps[id.x * 3u + 2u];
    var sum = vec3<f32>(0.0);
    for (var i = 0u; i < count; i = i + 1u) {
        // Clamped to [0, 1], with NaN taken to 0, as the core's wide terminal holds it.
        let value = textureLoad(lf_window, vec2<u32>(first + i, id.y), 0).rgb;
        let held = min(select(vec3<f32>(0.0), value, value >= vec3<f32>(0.0)), vec3<f32>(1.0));
        sum = sum + lf_weights[offset + i] * held;
    }
    lf_across[id.y * lf_params.columns + id.x] = vec4<f32>(sum, 1.0);
}
";

/// The pass down, before its encoding: each output row's taps summed down the sums across, clamped
/// and encoded, its codes packed.
const DOWN: &str = "
struct LfResample { columns: u32, rows: u32, width: u32, height: u32 }
@group(0) @binding(0) var<uniform> lf_params: LfResample;
@group(0) @binding(1) var<storage, read> lf_taps: array<u32>;
@group(0) @binding(2) var<storage, read> lf_weights: array<f32>;
@group(0) @binding(3) var<storage, read> lf_across: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> lf_codes: array<u32>;
@group(0) @binding(5) var<storage, read> lf_wide: array<f32>;

fn lf_wide_code(value: f32) -> u32 {
    // Clamped to [0, 1], with NaN taken to 0, as the core's 16-bit quantizer clamps it.
    var x = select(0.0, value, value >= 0.0);
    x = min(x, 1.0);
    var code = 0u;
    for (var step = 32768u; step > 0u; step = step >> 1u) {
        if code + step <= 65535u && lf_wide[code + step - 1u] <= x {
            code = code + step;
        }
    }
    return code;
}

fn lf_resample_sum(id: vec3<u32>) -> vec3<f32> {
    let first = lf_taps[id.y * 3u];
    let count = lf_taps[id.y * 3u + 1u];
    let offset = lf_taps[id.y * 3u + 2u];
    var sum = vec3<f32>(0.0);
    for (var i = 0u; i < count; i = i + 1u) {
        sum = sum + lf_weights[offset + i] * lf_across[(first + i) * lf_params.columns + id.x].rgb;
    }
    return sum;
}
";

/// The pass down's entry at 8 bits: four codes a word, red first, alpha 255.
const DOWN_EIGHT: &str = "
@compute @workgroup_size(8, 8)
fn lf_resample_down(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= lf_params.columns || id.y >= lf_params.rows {
        return;
    }
    let sum = lf_resample_sum(id);
    let r = lf_output_code(sum.r);
    let g = lf_output_code(sum.g);
    let b = lf_output_code(sum.b);
    lf_codes[id.y * lf_params.columns + id.x] = r | (g << 8u) | (b << 16u) | (255u << 24u);
}
";

/// The pass down's entry at 16 bits: two codes a word, red first, alpha 65,535.
const DOWN_SIXTEEN: &str = "
@compute @workgroup_size(8, 8)
fn lf_resample_down(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= lf_params.columns || id.y >= lf_params.rows {
        return;
    }
    let sum = lf_resample_sum(id);
    let at = (id.y * lf_params.columns + id.x) * 2u;
    lf_codes[at] = lf_wide_code(sum.r) | (lf_wide_code(sum.g) << 16u);
    lf_codes[at + 1u] = lf_wide_code(sum.b) | (65535u << 16u);
}
";

/// The source of the pass across.
pub(crate) fn across_source() -> &'static str {
    ACROSS
}

/// The source of the pass down at `depth`: the 8-bit one declares the installed output encoding.
pub(crate) fn down_source(depth: ResampleDepth) -> Result<String, String> {
    Ok(match depth {
        ResampleDepth::Eight => format!("{}{DOWN}{DOWN_EIGHT}", super::tail::encoding()?),
        ResampleDepth::Sixteen => format!("{DOWN}{DOWN_SIXTEEN}"),
    })
}

/// The resample sweep's three pipelines and their groups' layouts, made once for a runner.
pub(crate) struct Pipelines {
    pub across: wgpu::ComputePipeline,
    pub across_layout: wgpu::BindGroupLayout,
    pub eight: wgpu::ComputePipeline,
    pub sixteen: wgpu::ComputePipeline,
    pub down_layout: wgpu::BindGroupLayout,
}

impl Pipelines {
    /// The pipelines on `device`, each source checked with `naga` as the stage checks its own and
    /// compiled inside error scopes read without waiting; why not otherwise.
    pub fn new(device: &wgpu::Device) -> Result<Self, String> {
        let buffer = |binding, read_only: Option<bool>| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: match read_only {
                    None => wgpu::BufferBindingType::Uniform,
                    Some(read_only) => wgpu::BufferBindingType::Storage { read_only },
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let made = (|| {
            let across_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("luxforge.resample.across"),
                entries: &[
                    buffer(0, None),
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    buffer(2, Some(true)),
                    buffer(3, Some(true)),
                    buffer(4, Some(false)),
                ],
            });
            let down_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("luxforge.resample.down"),
                entries: &[
                    buffer(0, None),
                    buffer(1, Some(true)),
                    buffer(2, Some(true)),
                    buffer(3, Some(true)),
                    buffer(4, Some(false)),
                    buffer(5, Some(true)),
                ],
            });
            let pipeline = |label, source: &str, entry, layout: &wgpu::BindGroupLayout| {
                let module = super::validate(source)?;
                let pipeline_layout =
                    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(label),
                        bind_group_layouts: &[layout],
                        push_constant_ranges: &[],
                    });
                let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(label),
                    source: wgpu::ShaderSource::Naga(std::borrow::Cow::Owned(module)),
                });
                Ok::<_, String>(
                    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some(label),
                        layout: Some(&pipeline_layout),
                        module: &module,
                        entry_point: Some(entry),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        cache: None,
                    }),
                )
            };
            Ok::<_, String>(Self {
                across: pipeline(
                    "luxforge.resample.across",
                    across_source(),
                    "lf_resample_across",
                    &across_layout,
                )?,
                eight: pipeline(
                    "luxforge.resample.eight",
                    &down_source(ResampleDepth::Eight)?,
                    "lf_resample_down",
                    &down_layout,
                )?,
                sixteen: pipeline(
                    "luxforge.resample.sixteen",
                    &down_source(ResampleDepth::Sixteen)?,
                    "lf_resample_down",
                    &down_layout,
                )?,
                across_layout,
                down_layout,
            })
        })();
        let validation = super::answered(device.pop_error_scope());
        let internal = super::answered(device.pop_error_scope());
        match (made, validation, internal) {
            (Ok(made), Some(None), Some(None)) => Ok(made),
            (Err(error), _, _) => Err(error),
            (_, Some(Some(error)), _) | (_, _, Some(Some(error))) => Err(error.to_string()),
            _ => Err("the resample passes' error scopes were not answered without waiting".into()),
        }
    }
}

#[cfg(test)]
#[path = "resample_tests.rs"]
mod tests;
