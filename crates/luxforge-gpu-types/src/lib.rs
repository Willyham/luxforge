//! Pure GPU formats, extents and shader calling conventions shared by semantic planning and
//! device execution. No recipe, device, executor or policy budget belongs here.

pub mod layout;

/// How a boundary's texels are held: four little-endian half floats (`rgba16float`), or four
/// little-endian `f32` (`rgba32float`), red, green, blue and an opaque alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoundaryFormat {
    /// A JPEG's byte path.
    Half,
    /// A developed RAW's linear path.
    Float,
}

impl BoundaryFormat {
    /// The format of the boundary a plan of the linear path, or of the byte path, holds.
    pub fn of(linear: bool) -> Self {
        if linear { Self::Float } else { Self::Half }
    }

    /// Bytes per texel.
    pub const fn texel_bytes(self) -> usize {
        match self {
            Self::Half => 8,
            Self::Float => 16,
        }
    }
}

/// What one plane's texels hold: how many channels and whether half precision holds them, and so
/// the texture format the surface keeps it in when it takes a texture of its own.
///
/// A plane may instead take a texture an earlier unit or operation no longer needs, when that
/// texture's format [holds](Self::holds) it: as many channels or more, at its precision or a finer
/// one. Half precision saves memory only for three or four channels, since `rgba16float` is the one
/// half-precision format every adapter stores to: a plane of one or two channels that half
/// precision holds takes `r32float` or `rg32float` when it takes a texture of its own, which costs
/// no more, and an `rgba16float` an earlier unit left free when one is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlaneFormat {
    /// Four channels at half precision, `rgba16float`.
    Colour,
    /// One accumulator, `r32float`.
    Scalar,
    /// Two accumulators read together, such as a mean and a mean square, `rg32float`.
    Pair,
    /// Four accumulators, or a reduced colour with one more channel beside it, `rgba32float`.
    Quad,
    /// One channel half precision holds: `r32float` of its own.
    HalfScalar,
    /// Two channels half precision holds: `rg32float` of its own.
    HalfPair,
}

impl PlaneFormat {
    /// The physical format of a newly allocated texture.
    pub fn kept_as(self) -> Self {
        match self {
            Self::HalfScalar => Self::Scalar,
            Self::HalfPair => Self::Pair,
            other => other,
        }
    }

    /// The WGSL storage texture format.
    pub fn wgsl(self) -> &'static str {
        match self {
            Self::Colour => "rgba16float",
            Self::Scalar | Self::HalfScalar => "r32float",
            Self::Pair | Self::HalfPair => "rg32float",
            Self::Quad => "rgba32float",
        }
    }

    /// The bytes one texel takes.
    pub fn texel_bytes(self) -> u64 {
        match self {
            Self::Scalar | Self::HalfScalar => 4,
            Self::Colour | Self::Pair | Self::HalfPair => 8,
            Self::Quad => 16,
        }
    }

    /// How many channels it holds.
    pub fn channels(self) -> u32 {
        match self {
            Self::Scalar | Self::HalfScalar => 1,
            Self::Pair | Self::HalfPair => 2,
            Self::Colour | Self::Quad => 4,
        }
    }

    /// Whether its texture stores half floats.
    pub fn stores_half(self) -> bool {
        matches!(self, Self::Colour)
    }

    /// Whether half precision holds what it is declared for.
    pub fn half_holds(self) -> bool {
        matches!(self, Self::Colour | Self::HalfScalar | Self::HalfPair)
    }

    /// Whether a texture of this format holds a plane of `plane`'s: as many channels or more, and
    /// full precision unless half precision holds the plane.
    pub fn holds(self, plane: Self) -> bool {
        self.channels() >= plane.channels() && (!self.stores_half() || plane.half_holds())
    }
}

/// A plane's size against the boundary it is computed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlaneSize {
    /// The stage's blocks of `s × s` pixels, anchored at the stage origin, that the boundary
    /// reaches: `s = 1` is the boundary itself. A boundary at stage origin `o` of width `w` holds
    /// reduced columns `floor(o / s)` to `ceil((o + w) / s)`, so a block a window cuts is held, and
    /// a reduced plane is the CPU's own blocks wherever the window holds them whole.
    Reduced(u32),
    /// A fixed number of texels, whatever the boundary: a global estimate's.
    Fixed { width: u32, height: u32 },
}

impl PlaneSize {
    /// One fixed texel for a semantic global estimate; no runtime resource reference.
    pub const LIGHT: Self = Self::Fixed {
        width: 1,
        height: 1,
    };

    /// Blocks reached by this window, including partial blocks at either edge.
    pub fn extent(self, origin: (u32, u32), size: (u32, u32)) -> (u32, u32) {
        match self {
            Self::Fixed { width, height } => (width, height),
            Self::Reduced(s) => {
                let s = s.max(1);
                let axis = |origin: u32, length: u32| (origin + length).div_ceil(s) - origin / s;
                (axis(origin.0, size.0), axis(origin.1, size.1))
            }
        }
    }
}

/// How a pass runs its kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassShape {
    /// Once for every `span` texels of the output plane, `at` the first of them: `[1, 1]` for a
    /// pointwise kernel, a run along one axis for a running sum reseeded at each run's start.
    Texels { span: [u32; 2] },
    /// One workgroup of [`WORKGROUP_LANES`] lanes, for a reduction of a small plane to a few
    /// values.
    Workgroup,
}

/// How many inputs one pass reads, and how many planes one apply reads.
pub const PASS_INPUTS: usize = 4;
pub const UNIT_APPLY_PLANES: usize = 4;

/// The lanes of a workgroup pass, and the values of `lf_shared`.
pub const WORKGROUP_LANES: u32 = 256;
pub const SHARED_VALUES: u32 = 1024;

/// One side of a texel pass's workgroup.
pub const GROUP_SIDE: u32 = 8;

/// The binding of a pass's output plane in its second group, after every plane it reads.
pub const OUTPUT_BINDING: u32 = 64;

/// The binding of a pass's parameters in its second group.
pub const PARAMS_BINDING: u32 = 65;

/// One pass's slice of the parameter buffer, in bytes and in words: the largest storage-binding
/// offset alignment a device may ask for.
pub const PARAMS_STRIDE: u64 = 256;
pub const PARAMS_WORDS: usize = (PARAMS_STRIDE / 4) as usize;

/// A pass's parameters: its step's header index, its words' offset in the step's words, its span,
/// then the offsets of the applies its source runs, the rectangle of its output it writes,
/// `[x0, y0, x1, y1)`, and last the texel of its output its first invocation starts at, the corner
/// of the rectangle a tick runs it over.
pub const PARAM_STEP: usize = 0;
pub const PARAM_WORDS: usize = 1;
pub const PARAM_SPAN: usize = 2;
pub const PARAM_APPLIES: usize = 4;
pub const PARAM_LIMIT: usize = PARAMS_WORDS - 6;
pub const PARAM_ORIGIN: usize = PARAMS_WORDS - 2;

/// A limit's far edge that no plane reaches, which a `vec2<i32>` holds.
pub const UNLIMITED: u32 = i32::MAX as u32;

/// Apply textures available to one chained spatial operation beside its input and pass inputs.
pub const CHAIN_APPLY_PLANES: usize = 11;
/// Boundary texel map and output origin, before the step headers.
pub const MAP_WORDS: usize = 6;
/// A step's affine position map.
pub const POSITION_WORDS: usize = 6;
/// Each step's two base indices followed by its position map.
pub const STEP_WORDS: usize = 2 + POSITION_WORDS;

/// The WGSL every assembled GPU-preview shader starts with; see the [module documentation](self).
/// A program's own WGSL, appended to this, must validate on its own.
pub const PRELUDE: &str = "\
// Luxforge GPU-preview prelude: the bindings and helpers every program may use.
@group(0) @binding(0) var<storage, read> lf_words: array<u32>;
@group(0) @binding(1) var<storage, read> lf_blocks: array<u32>;

fn lf_word(i: u32) -> u32 {
    return lf_words[i];
}

fn lf_f32(i: u32) -> f32 {
    return bitcast<f32>(lf_words[i]);
}

fn lf_block_word(i: u32) -> u32 {
    return lf_blocks[i];
}

fn lf_block_f32(i: u32) -> f32 {
    return bitcast<f32>(lf_blocks[i]);
}
";

/// The spatial convention's declarations as stubs, which a program alone is validated against.
pub const SPATIAL_PRELUDE: &str = "
// Luxforge GPU-preview spatial declarations.
var<workgroup> lf_shared: array<f32, 1024>;
fn lf_plane(slot: u32, at: vec2<i32>) -> vec4<f32> { return vec4<f32>(f32(slot), vec2<f32>(at), 1.0); }
fn lf_plane_size(slot: u32) -> vec2<i32> { return vec2<i32>(i32(slot) + 1); }
fn lf_source(at: vec2<i32>) -> vec3<f32> { return vec3<f32>(vec2<f32>(at), 0.5); }
fn lf_origin() -> vec2<i32> { return vec2<i32>(0); }
fn lf_size() -> vec2<i32> { return vec2<i32>(1); }
fn lf_store(at: vec2<i32>, value: vec4<f32>) {}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_hold_partial_reduction_blocks() {
        for (origin, size, reduction, expected) in [
            ((0, 0), (7, 9), 4, (2, 3)),
            ((3, 7), (2, 2), 4, (2, 2)),
            ((4, 8), (4, 4), 4, (1, 1)),
            ((3, 5), (7, 9), 0, (7, 9)),
        ] {
            assert_eq!(PlaneSize::Reduced(reduction).extent(origin, size), expected);
        }
        assert_eq!(PlaneSize::LIGHT.extent((31, 17), (200, 300)), (1, 1));
    }

    #[test]
    fn texture_reuse_keeps_channels_and_precision() {
        assert!(PlaneFormat::Quad.holds(PlaneFormat::Colour));
        assert!(PlaneFormat::Quad.holds(PlaneFormat::Pair));
        assert!(!PlaneFormat::Colour.holds(PlaneFormat::Scalar));
        assert!(PlaneFormat::Colour.holds(PlaneFormat::HalfPair));
        assert!(!PlaneFormat::Scalar.holds(PlaneFormat::Pair));
        assert_eq!(PlaneFormat::HalfPair.kept_as(), PlaneFormat::Pair);
        assert_eq!(PlaneFormat::HalfScalar.kept_as(), PlaneFormat::Scalar);
        assert_eq!(PlaneFormat::Quad.texel_bytes(), 16);
        assert_eq!(BoundaryFormat::Half.texel_bytes(), 8);
        assert_eq!(BoundaryFormat::Float.texel_bytes(), 16);
    }

    #[test]
    fn headers_and_pass_parameters_use_the_production_layout() {
        assert_eq!(MAP_WORDS, 6);
        assert_eq!(STEP_WORDS, 8);
        assert_eq!(
            [PARAM_STEP, PARAM_WORDS, PARAM_SPAN, PARAM_APPLIES],
            [0, 1, 2, 4]
        );
        assert_eq!([PARAM_LIMIT, PARAM_ORIGIN, PARAMS_WORDS], [58, 62, 64]);
        assert_eq!(PARAMS_STRIDE, 256);
        assert_eq!([OUTPUT_BINDING, PARAMS_BINDING], [64, 65]);
        assert_eq!(PASS_INPUTS, 4);
        assert_eq!(UNIT_APPLY_PLANES, 4);
        assert_eq!(CHAIN_APPLY_PLANES, 11);
    }
}
