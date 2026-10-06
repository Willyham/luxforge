//! The geometry tail and the output encoding: what a plan draws after its content steps
//! (`docs/design/gpu-preview.md`, "A tick").
//!
//! # The geometry tail
//!
//! A [`GpuTail`] step carries the content pass's result to the output stage, as the CPU's exact
//! steps, resamples and warps after the boundary do. A plan with one runs two passes: the content
//! steps write an intermediate texture the boundary's size, and a second pass draws the output
//! stage. For each output pixel `(x, y)` it takes the boundary-stage coordinate `(u, v)` of the
//! pixel's centre `(x + ½, y + ½)`:
//!
//! - through an **affine** matrix, `u = a·x + b·y + c`, `v = d·x + e·y + f`, in `f32`;
//! - through a perspective warp's **homography**, alone or composed with the crop and straightening
//!   around it, `u = (a·x + b·y + c) / w`, `v = (d·x + e·y + f) / w` for `w = g·x + h·y + i`, in
//!   `f32`: the warp's own map at every pixel, with no grid; or
//! - through a lens warp's **coordinate grid**, interpolated bilinearly between its four nodes in
//!   `f32` exactly as the core's `CoordinateGrid::sample` defines it. A sampler's linear filtering
//!   would not do: its weights carry too few bits for the grid's 0.1 px contract.
//!
//! Then it blends the four texels at `floor(u − ½)`, `floor(v − ½)` and the ones after them by the
//! fractions, in linear light, each tap clamped to the rectangle of the boundary stage the CPU's
//! resample reads, whose edge it replicates. The intermediate holds the boundary's window, at the
//! texel map's origin in the boundary stage, so a tap's stage pixel is offset by that origin.
//!
//! The steps after the tail run in the output pass, each at its position map of the output pixel:
//! the vignette after a resample, and any colour operation of the output stage, masked or not.
//!
//! # Quantizing as the CPU does
//!
//! On a JPEG, the CPU writes each segment's output as 8-bit codes, so a stage boundary between the
//! content operations and the tail quantizes their result, and the resample's own output is
//! quantized again before the next segment reads it. A tail that `quantizes` does the same: the
//! content pass writes codes into an 8-bit intermediate, the tail decodes them through the same
//! table the CPU decodes with, and its blend is quantized and decoded again. A developed RAW keeps
//! its values unquantized and unclamped between segments, so its tail does neither, and the
//! intermediate is `rgba32float` to preserve the full linear range.
//!
//! # The output encoding
//!
//! The last pass writes the 8-bit code the CPU's output quantizer gives each channel: clamped to
//! `[0, 1]`, NaN to zero, and the number of the 255 code thresholds at or below the value, each
//! threshold the first `f32` in its code's exact interval ([`OutputEncoding`]). The tables are the
//! core's, which the desktop hands the surface once at start ([`install_output_encoding`]): the
//! widget crate does not reach the core, and a surface with none installed compiles no pass, so
//! its frames stay the CPU's. It writes the
//! code through an `Rgba8Unorm` view of the output texture, and the draw samples the texture
//! through an `Rgba8UnormSrgb` view, exactly as it samples the CPU frame of the same codes. The
//! hardware's own sRGB encoder rounds some values to the neighbouring code, which a GPU frame
//! would carry into the dissolve's end; this does not.
use super::GpuProgram;
use std::sync::{Arc, OnceLock};

/// The entry of the affine tail's mapping.
const AFFINE_ENTRY: &str = "lf_tail_affine";
/// The entry of the projective tail's mapping.
const PROJECTIVE_ENTRY: &str = "lf_tail_projective";
/// The entry of the coordinate grid's mapping.
const GRID_ENTRY: &str = "lf_tail_grid";

/// The tail's words before its mapping's: the output stage's width and height, then the
/// rectangle `x0, y0, x1, y1` of the boundary stage its taps read.
const HEADER_WORDS: usize = 6;

/// `u = a·x + b·y + c`, `v = d·x + e·y + f` of the pixel-edge coordinate `pixel`, with the six
/// coefficients after the tail's header.
const AFFINE_SOURCE: &str = "
fn lf_tail_affine(pixel: vec2<f32>, words: u32, block: u32) -> vec2<f32> {
    return vec2<f32>(
        lf_f32(words + 6u) * pixel.x + lf_f32(words + 7u) * pixel.y + lf_f32(words + 8u),
        lf_f32(words + 9u) * pixel.x + lf_f32(words + 10u) * pixel.y + lf_f32(words + 11u),
    );
}
";

/// `u = (a·x + b·y + c) / w`, `v = (d·x + e·y + f) / w` for `w = g·x + h·y + i` of the pixel-edge
/// coordinate `pixel`, with the nine coefficients after the tail's header.
const PROJECTIVE_SOURCE: &str = "
fn lf_tail_projective(pixel: vec2<f32>, words: u32, block: u32) -> vec2<f32> {
    let w = lf_f32(words + 12u) * pixel.x + lf_f32(words + 13u) * pixel.y + lf_f32(words + 14u);
    return vec2<f32>(
        lf_f32(words + 6u) * pixel.x + lf_f32(words + 7u) * pixel.y + lf_f32(words + 8u),
        lf_f32(words + 9u) * pixel.x + lf_f32(words + 10u) * pixel.y + lf_f32(words + 11u),
    ) / w;
}
";

/// The coordinate grid's bilinear interpolation at the pixel-edge coordinate `pixel`, a pixel's
/// centre, as `CoordinateGrid::sample` computes it: the grid's first node's column and row on the
/// stage's lattice of its spacing, the spacing, columns and rows after the tail's header, and its
/// nodes in the block, `(u, v)` pairs row by row. The cell is the stage pixel's, in integers, and
/// the fraction across it the pixel's offset from the cell's node over the spacing: both from the
/// stage's lattice and the pixel alone, never from where the grid starts, so every window of the
/// stage interpolates a pixel to the same bits. A division whose operands depend on the grid's
/// start would not: the shading language may divide by a reciprocal, whose rounding then differs
/// from tile to tile.
const GRID_SOURCE: &str = "
fn lf_tail_grid_node(column: u32, row: u32, columns: u32, block: u32) -> vec2<f32> {
    let at = block + 2u * (row * columns + column);
    return vec2<f32>(lf_block_f32(at), lf_block_f32(at + 1u));
}

fn lf_tail_grid_cell(cell: u32, first: u32, count: u32) -> u32 {
    return min(max(cell, first) - first, count - 2u);
}

fn lf_tail_grid(pixel: vec2<f32>, words: u32, block: u32) -> vec2<f32> {
    let spacing = lf_f32(words + 8u);
    let step = u32(spacing);
    let first = vec2<u32>(lf_word(words + 6u), lf_word(words + 7u));
    let columns = lf_word(words + 9u);
    let rows = lf_word(words + 10u);
    let texel = vec2<u32>(floor(pixel));
    let column = lf_tail_grid_cell(texel.x / step, first.x, columns);
    let row = lf_tail_grid_cell(texel.y / step, first.y, rows);
    let node = vec2<f32>(vec2<u32>((first.x + column) * step, (first.y + row) * step));
    let fx = (pixel.x - node.x) / spacing;
    let fy = (pixel.y - node.y) / spacing;
    let a = lf_tail_grid_node(column, row, columns, block);
    let b = lf_tail_grid_node(column + 1u, row, columns, block);
    let c = lf_tail_grid_node(column, row + 1u, columns, block);
    let d = lf_tail_grid_node(column + 1u, row + 1u, columns, block);
    let top = a + (b - a) * fx;
    let bottom = c + (d - c) * fx;
    return top + (bottom - top) * fy;
}
";

/// The geometry tail of a plan: the output stage it draws, whether it quantizes as a JPEG's
/// segments do, and its mapping as a program of the surface's own whose words and block are the
/// mapping's data.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuTail {
    output: (u32, u32),
    quantize: bool,
    preserve_f32: bool,
    program: GpuProgram,
}

impl GpuTail {
    /// An affine tail onto an output stage of `output`: `matrix` is `[a, b, c, d, e, f]` of
    /// `u = a·x + b·y + c`, `v = d·x + e·y + f` at a pixel's centre; `reads` the `[x0, y0, x1, y1)`
    /// rectangle of the boundary stage the taps are clamped to.
    pub fn affine(output: (u32, u32), reads: [u32; 4], quantize: bool, matrix: [f32; 6]) -> Self {
        let mut words = header(output, reads);
        words.extend(matrix.map(f32::to_bits));
        Self {
            output,
            quantize,
            preserve_f32: false,
            program: GpuProgram {
                words,
                ..GpuProgram::new(AFFINE_ENTRY, AFFINE_SOURCE)
            },
        }
    }

    /// A projective tail onto an output stage of `output`: `matrix` is `[a, b, c, d, e, f, g, h, i]`
    /// of `u = (a·x + b·y + c) / w`, `v = (d·x + e·y + f) / w`, `w = g·x + h·y + i` at a pixel's
    /// centre; `reads` as for [`Self::affine`].
    pub fn projective(
        output: (u32, u32),
        reads: [u32; 4],
        quantize: bool,
        matrix: [f32; 9],
    ) -> Self {
        let mut words = header(output, reads);
        words.extend(matrix.map(f32::to_bits));
        Self {
            output,
            quantize,
            preserve_f32: false,
            program: GpuProgram {
                words,
                ..GpuProgram::new(PROJECTIVE_ENTRY, PROJECTIVE_SOURCE)
            },
        }
    }

    /// A coordinate grid's tail onto an output stage of `output`: node `(i, j)` at the pixel-edge
    /// coordinate `origin + (i, j)·spacing`, `origin` a node of the stage's lattice of that
    /// spacing, `columns` × `rows` of them, `nodes` their `(u, v)` `f32` bits row by row, shared.
    #[allow(clippy::too_many_arguments)]
    pub fn grid(
        output: (u32, u32),
        reads: [u32; 4],
        quantize: bool,
        origin: (u32, u32),
        spacing: u32,
        (columns, rows): (u32, u32),
        nodes: Arc<[u32]>,
    ) -> Self {
        let mut words = header(output, reads);
        // The first node's column and row on the stage's lattice, so the pass needs no division
        // by the spacing that depends on where the grid starts.
        let spacing = spacing.max(1);
        debug_assert!(origin.0 % spacing == 0 && origin.1 % spacing == 0);
        words.extend([
            origin.0 / spacing,
            origin.1 / spacing,
            (spacing as f32).to_bits(),
            columns,
            rows,
        ]);
        Self {
            output,
            quantize,
            preserve_f32: false,
            program: GpuProgram {
                words,
                block: nodes,
                ..GpuProgram::new(GRID_ENTRY, GRID_SOURCE)
            },
        }
    }

    /// The output stage the tail draws.
    pub fn output(&self) -> (u32, u32) {
        self.output
    }

    /// Whether the tail quantizes as a JPEG's segments do.
    pub fn quantizes(&self) -> bool {
        self.quantize
    }

    /// Keep the geometry pass's scene-linear values in `rgba32float`, including values outside
    /// half-float's range. Used for boundaries rendered from the RAW linear path.
    pub fn preserve_f32(mut self) -> Self {
        self.preserve_f32 = true;
        self
    }

    pub(super) fn preserves_f32(&self) -> bool {
        self.preserve_f32
    }

    pub(super) fn program(&self) -> &GpuProgram {
        &self.program
    }

    /// The tail's storage block, shared: a coordinate grid's nodes, empty for an affine or a
    /// projective tail.
    #[cfg(any(test, feature = "qualification"))]
    pub fn block(&self) -> &Arc<[u32]> {
        &self.program.block
    }

    /// Whether the tail takes each output pixel from the intermediate's texel at the same stage
    /// pixel: an affine tail of the identity matrix, which resamples nothing — a stage boundary
    /// before the output stage's operations, quantized as the CPU's is on a JPEG. Its output
    /// changes only where the intermediate does.
    pub(super) fn identity(&self) -> bool {
        // By value: a matrix of the identity may carry negative zeros.
        self.program.entry == AFFINE_ENTRY
            && self
                .program
                .words
                .get(HEADER_WORDS..HEADER_WORDS + 6)
                .is_some_and(|matrix| {
                    matrix
                        .iter()
                        .map(|word| f32::from_bits(*word))
                        .eq([1.0, 0.0, 0.0, 0.0, 1.0, 0.0])
                })
    }

    /// The format of the content pass's result, which the tail reads.
    pub(super) fn intermediate(&self) -> wgpu::TextureFormat {
        intermediate(self.quantize, self.preserve_f32)
    }
}

/// The format of the content pass's result a tail reads: 8-bit codes for one that `quantize`s, as
/// the CPU quantizes a JPEG's segments, `f32` for one that keeps the RAW linear path's values
/// whole, else half floats.
pub(super) fn intermediate(quantize: bool, preserve_f32: bool) -> wgpu::TextureFormat {
    if quantize {
        wgpu::TextureFormat::Rgba8Unorm
    } else if preserve_f32 {
        wgpu::TextureFormat::Rgba32Float
    } else {
        wgpu::TextureFormat::Rgba16Float
    }
}

fn header(output: (u32, u32), reads: [u32; 4]) -> Vec<u32> {
    let mut words = Vec::with_capacity(HEADER_WORDS + 9);
    words.extend([output.0, output.1]);
    words.extend(reads);
    words
}

/// The CPU's output quantizer and decode table, which every pass that encodes or quantizes
/// declares as constants.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputEncoding {
    /// The first `f32` of each code's exact interval, for codes 1 to 255: a value clamped to
    /// `[0, 1]`, NaN taken to 0, has the code of the number of these at or below it.
    pub thresholds: [f32; 255],
    /// Each 8-bit code decoded to linear light: the table the CPU decodes a segment's codes with.
    pub decoded: [f32; 256],
}

static OUTPUT_ENCODING: OnceLock<OutputEncoding> = OnceLock::new();

/// Hand the GPU preview the CPU's output encoding, once, before a surface draws a plan. The first
/// tables installed hold for the life of the process: the answer is whether `encoding` is them.
pub fn install_output_encoding(encoding: OutputEncoding) -> bool {
    *OUTPUT_ENCODING.get_or_init(|| encoding.clone()) == encoding
}

/// The installed output encoding, if the desktop has installed one. A unit test of this crate
/// installs the shared test reference's tables, which the desktop's tests hold to the core's.
pub fn output_encoding() -> Option<&'static OutputEncoding> {
    #[cfg(test)]
    OUTPUT_ENCODING.get_or_init(|| OutputEncoding {
        thresholds: std::array::from_fn(|index| {
            let threshold = luxforge_reference::srgb::decode_encoded((index as f64 + 0.5) / 255.0);
            let narrowed = threshold as f32;
            if f64::from(narrowed) < threshold {
                narrowed.next_up()
            } else {
                narrowed
            }
        }),
        decoded: std::array::from_fn(|code| luxforge_reference::srgb::decode(code as u8) as f32),
    });
    OUTPUT_ENCODING.get()
}

/// An `f32` as a WGSL literal that parses to exactly it.
fn literal(value: f32) -> String {
    format!("{value:?}f")
}

/// The prefixes of every name the surface declares for the tail and the output encoding. No entry
/// may start with them.
pub(super) const GENERATED: [&str; 2] = ["lf_tail", "lf_output"];

/// The output encoding and the decode table every pass that quantizes or encodes declares: the
/// thresholds and codes as constant arrays, `lf_output_code` for one channel's code and
/// `lf_output_encode` for a pixel's codes over 255, `lf_output_decode` for a code's linear value.
/// Built once from the installed [`OutputEncoding`]; with none installed, no pass compiles.
pub(super) fn encoding() -> Result<&'static str, String> {
    static SOURCE: OnceLock<String> = OnceLock::new();
    let installed = output_encoding().ok_or_else(|| {
        String::from("no output encoding is installed: the desktop hands the surface the core's")
    })?;
    Ok(SOURCE.get_or_init(|| encoding_source(installed)))
}

fn encoding_source(installed: &OutputEncoding) -> String {
    let thresholds: Vec<String> = installed.thresholds.iter().copied().map(literal).collect();
    let table: Vec<String> = installed.decoded.iter().copied().map(literal).collect();
    format!(
        "
const lf_output_thresholds = array<f32, 256>({}, 3.0e38f);
const lf_output_decoded = array<f32, 256>({});

fn lf_output_code(value: f32) -> u32 {{
    // Clamped to [0, 1], with NaN taken to 0, as the CPU's quantizer clamps it.
    var x = select(0.0, value, value >= 0.0);
    x = min(x, 1.0);
    var code = 0u;
    for (var step = 128u; step > 0u; step = step >> 1u) {{
        if lf_output_thresholds[code + step - 1u] <= x {{
            code = code + step;
        }}
    }}
    return code;
}}

fn lf_output_encode(rgb: vec3<f32>) -> vec3<f32> {{
    return vec3<f32>(f32(lf_output_code(rgb.r)), f32(lf_output_code(rgb.g)), f32(lf_output_code(rgb.b))) / 255.0;
}}

fn lf_output_decode(code: u32) -> f32 {{
    return lf_output_decoded[min(code, 255u)];
}}

fn lf_output_requantize(rgb: vec3<f32>) -> vec3<f32> {{
    return vec3<f32>(lf_output_decode(lf_output_code(rgb.r)), lf_output_decode(lf_output_code(rgb.g)), lf_output_decode(lf_output_code(rgb.b)));
}}
",
        thresholds.join(", "),
        table.join(", ")
    )
}

/// The tail's sampling: four taps of the content pass's result, each clamped to the rectangle of
/// the boundary stage the CPU reads and offset by the window the boundary holds, blended in linear
/// light. A quantizing tail decodes the 8-bit codes the content pass wrote.
pub(super) fn sampling(quantize: bool) -> String {
    let load = if quantize {
        "    let c = textureLoad(lf_boundary, texel, 0).rgb;
    return vec3<f32>(
        lf_output_decode(u32(round(c.r * 255.0))),
        lf_output_decode(u32(round(c.g * 255.0))),
        lf_output_decode(u32(round(c.b * 255.0))),
    );"
    } else {
        "    return textureLoad(lf_boundary, texel, 0).rgb;"
    };
    format!(
        "
fn lf_tail_tap(x: i32, y: i32, words: u32) -> vec3<f32> {{
    let low = vec2<i32>(i32(lf_word(words + 2u)), i32(lf_word(words + 3u)));
    let high = vec2<i32>(i32(lf_word(words + 4u)), i32(lf_word(words + 5u))) - vec2<i32>(1);
    let at = clamp(vec2<i32>(x, y), low, high);
    let origin = vec2<i32>(i32(lf_f32(0u)), i32(lf_f32(1u)));
    let last = vec2<i32>(textureDimensions(lf_boundary)) - vec2<i32>(1);
    let texel = clamp(at - origin, vec2<i32>(0), last);
{load}
}}

fn lf_tail_sample(uv: vec2<f32>, words: u32) -> vec3<f32> {{
    let p = uv - vec2<f32>(0.5);
    let corner = floor(p);
    let w = p - corner;
    let x = i32(corner.x);
    let y = i32(corner.y);
    return lf_tail_tap(x, y, words) * ((1.0 - w.x) * (1.0 - w.y))
        + lf_tail_tap(x + 1, y, words) * (w.x * (1.0 - w.y))
        + lf_tail_tap(x, y + 1, words) * ((1.0 - w.x) * w.y)
        + lf_tail_tap(x + 1, y + 1, words) * (w.x * w.y);
}}
"
    )
}

/// The opening of the tail pass's fragment stage, for a tail whose header is at `base`: the output
/// pixel, its edge column and row repeated past the frame and offset by the region's origin, its coordinate in the boundary
/// stage through the mapping, and the blend there, quantized as the CPU's resample output is when
/// the tail quantizes. `stage` is the output pixel the steps after the tail address.
pub(super) fn fragment(tail: &GpuTail, base: usize) -> String {
    let requantize = if tail.quantize {
        "    rgb = lf_output_requantize(rgb);\n"
    } else {
        ""
    };
    format!(
        "
@fragment
fn lf_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {{
    let lf_tail_words = lf_words[{base}u];
    let lf_tail_block = lf_words[{block}u];
    let lf_tail_last = vec2<u32>(lf_word(lf_tail_words), lf_word(lf_tail_words + 1u)) - vec2<u32>(1u);
    // The drawn pixel, its edge column and row repeated, then offset to the output stage: a
    // region's origin at a percentage zoom, zero for a whole frame.
    let texel = min(vec2<u32>(position.xy), lf_tail_last) + vec2<u32>(lf_word(4u), lf_word(5u));
    let stage = vec2<f32>(texel);
    let uv = {entry}(stage + vec2<f32>(0.5), lf_tail_words, lf_tail_block);
    var rgb = lf_tail_sample(uv, lf_tail_words);
{requantize}",
        block = base + 1,
        entry = tail.program.entry,
    )
}
