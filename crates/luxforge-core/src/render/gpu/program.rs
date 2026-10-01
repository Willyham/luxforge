//! What a module's GPU program is: WGSL text the module owns beside its CPU unit, the kind of
//! function it declares, the uniform words it reads and whether it is qualified to run.
//!
//! The core names no GPU crate. A program is plain text and plain words, which the photo surface
//! assembles into one pipeline per program sequence and feeds on every frame. The calling convention
//! is the surface's: a prelude it prepends declares the bindings and the helpers a program reads its
//! data through, so a program's text declares only functions and constants.
//!
//! ```wgsl
//! fn lf_word(i: u32) -> u32          // the plan's uniform words, every program's concatenated
//! fn lf_f32(i: u32) -> f32           // bitcast<f32>(lf_word(i))
//! fn lf_block_word(i: u32) -> u32    // the plan's storage blocks, concatenated
//! fn lf_block_f32(i: u32) -> f32
//! ```
//!
//! Every name a program declares starts with its entry function's name, `lf_<module>_<unit>`, so
//! programs concatenated into one module never collide. The entry function's signature is its
//! [`GpuProgramKind`]'s.
use std::sync::Arc;

/// Which function a program's entry is, and therefore its signature. `words` and `block` are the
/// program's own base indices into the concatenated uniform words and storage blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuProgramKind {
    /// A pointwise colour unit, in the content space of its operation or in the output space after
    /// the geometry tail, as the plan places it:
    /// `fn <entry>(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32>`.
    /// `rgb` is scene-linear sRGB and unclamped; `pos` is the integer coordinate, as `f32`, that the
    /// CPU unit's `apply_row(y, x0, ..)` addresses for this pixel.
    Colour,
    /// One mask component's coverage before its own inversion and the composition:
    /// `fn <entry>(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32`. `pos` is the
    /// pixel of the stage the mask was compiled against; `rgb` is the input of the operation the
    /// mask modulates.
    Coverage,
}

/// One WGSL program, owned by the module whose CPU unit it mirrors and kept in a `.wgsl` file
/// beside that unit. Programs are `static`, so a description refers to one by address and a plan
/// carries no copy of the text.
#[derive(Debug, PartialEq, Eq)]
pub struct GpuProgram {
    /// The entry function's name. Every function and constant the text declares starts with it.
    pub entry: &'static str,
    /// The WGSL text: functions and constants only, no bindings, entry points or attributes.
    pub source: &'static str,
    pub kind: GpuProgramKind,
    /// How many uniform words every description of this program carries.
    pub words: usize,
    /// Whether the program met its class's error limits on the corpus. A disabled program still
    /// ships and is validated, but a stack that needs it takes the CPU path with the program named
    /// ([`super::GpuFallback::DisabledProgram`]) until it qualifies.
    pub enabled: bool,
}

/// One unit's program and the data it reads: what [`crate::PointwiseColor::gpu`] and a mask
/// component answer. A unit with no description takes the CPU path.
///
/// The words are a pure function of the unit's coefficients, so two units whose
/// [`crate::PointwiseColor::describe`] strings are equal produce equal words: the description is
/// already the host's identity for a compiled unit, and the plan's uniforms follow it.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuDescription {
    pub program: &'static GpuProgram,
    /// The uniform block: `program.words` 32-bit words, each an `f32`'s bits or an integer.
    pub words: Vec<u32>,
    /// The storage block, for data too large or too variable for uniforms: a curve's knots, a
    /// brush's segment grid. Shared, so a plan clones an `Arc` and not the block.
    pub block: Option<Arc<[u32]>>,
}

impl GpuDescription {
    /// A description of `program` with these uniform words and no storage block.
    pub fn new(program: &'static GpuProgram, words: Vec<u32>) -> Self {
        Self {
            program,
            words,
            block: None,
        }
    }

    /// The same description with a storage block.
    pub fn with_block(mut self, block: Arc<[u32]>) -> Self {
        self.block = Some(block);
        self
    }

    /// Whether the words this description carries are the count its program declares. A plan
    /// refuses a description that disagrees, as an internal error, rather than feed a program words
    /// it does not read or leave some it does unset.
    pub(crate) fn well_formed(&self) -> bool {
        self.words.len() == self.program.words
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! What a test that gives a unit a program checks about its descriptions, and the programs the
    //! render tests' own units carry. These are validated as WGSL like every shipped program, and
    //! the plan tests' reference executor runs a Rust twin of each.
    use super::{GpuProgram, GpuProgramKind};
    use crate::PointwiseColor;
    use std::collections::HashMap;

    /// The test exposure unit's program: every channel times the gain in word 0.
    pub(crate) static EXPOSURE: GpuProgram = GpuProgram {
        entry: "lf_test_exposure",
        source: "fn lf_test_exposure(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
                 -> vec3<f32> {\n    return rgb * lf_f32(words);\n}\n",
        kind: GpuProgramKind::Colour,
        words: 1,
        enabled: true,
    };

    /// The test positional unit's program: `x / width` added to red and `y / height` to green, the
    /// stage's width and height in words 0 and 1.
    pub(crate) static POSITIONAL: GpuProgram = GpuProgram {
        entry: "lf_test_positional",
        source: "fn lf_test_positional(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
                 -> vec3<f32> {\n    return rgb + vec3<f32>(pos.x / lf_f32(words), \
                 pos.y / lf_f32(words + 1u), 0.0);\n}\n",
        kind: GpuProgramKind::Colour,
        words: 2,
        enabled: true,
    };

    /// A test mask component's program: a horizontal ramp, `(x + ½) / width` clamped to `[0, 1]`,
    /// the width in word 0 and a storage block holding one offset added to `x`.
    pub(crate) static RAMP: GpuProgram = GpuProgram {
        entry: "lf_test_ramp",
        source: "const lf_test_ramp_half: f32 = 0.5;\n\
                 fn lf_test_ramp(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {\n    \
                 return clamp((pos.x + lf_block_f32(block) + lf_test_ramp_half) / lf_f32(words), \
                 0.0, 1.0);\n}\n",
        kind: GpuProgramKind::Coverage,
        words: 1,
        enabled: true,
    };

    /// Every test-only program, for the WGSL validation.
    pub(crate) static PROGRAMS: &[&GpuProgram] = &[&EXPOSURE, &POSITIONAL, &RAMP];

    /// Two units that describe themselves identically produce identical descriptions, and every
    /// description carries the words its program declares. A unit with no description is allowed
    /// here; the plan answers it as the CPU path.
    pub(crate) fn assert_uniforms_follow_descriptions(units: &[&dyn PointwiseColor]) {
        let mut seen: HashMap<String, Option<super::GpuDescription>> = HashMap::new();
        for unit in units {
            let description = unit.gpu();
            if let Some(description) = &description {
                assert!(
                    description.well_formed(),
                    "{}: {} words for a program that reads {}",
                    unit.describe(),
                    description.words.len(),
                    description.program.words
                );
            }
            match seen.get(&unit.describe()) {
                Some(earlier) => assert_eq!(
                    earlier,
                    &description,
                    "two units described as {} have different uniforms",
                    unit.describe()
                ),
                None => {
                    seen.insert(unit.describe(), description);
                }
            }
        }
    }
}
