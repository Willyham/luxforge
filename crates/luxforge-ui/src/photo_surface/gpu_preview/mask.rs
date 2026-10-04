//! The masked colour step: a pointwise colour operation blended by the coverage its mask composes,
//! against the operation's own input, as the CPU's colour run blends it
//! (`docs/design/gpu-preview.md`).
//!
//! A [`MaskedColour`] holds the operation's units, which run as colour steps do, and a [`Coverage`]:
//! its components' coverage programs, the algebra that folds them, the whole mask's inversion and
//! amount, where the mask's stage is, the rectangle outside which coverage is exactly zero and
//! whether the thin-feature supersample is asked for. Everything but the programs is data in the
//! step's words, so a person inverting a component or moving the amount writes words and compiles
//! nothing.
//!
//! # Coverage programs
//!
//! A component's program is `fn <entry>(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32)
//! -> f32`: its falloff before its own inversion and the composition. `pos` is the pixel of the
//! stage the mask was compiled against — of the doubled stage under the supersample — and `rgb` is
//! the input of the operation the mask modulates, which a value-based component reads. It follows
//! the convention every program does ([`validate_step`](super::validate_step)).
//!
//! # What the step evaluates
//!
//! For the stage pixel `p` the plan's texel map gives a boundary texel, with `M(p)` the mask's
//! position map:
//!
//! ```text
//! q = M(p)                                  the mask stage's pixel
//! if q is outside the bounds:  coverage = 0
//! fold(q) = m = 0
//!           for each component: c = coverage(q, in); c = 1 - c if it is inverted
//!                               m = max(m, c) | min(m, 1 - c) | min(m, c)   add | subtract | intersect
//!           m = 1 - m if the mask is inverted
//!           scale * m
//! coverage = fold(q), or the mean of fold(2q + (i, j)) over i, j in {0, 1} under the supersample
//! out = in where coverage is exactly 0, else (1 - coverage) * in + coverage * units(in)
//! ```
//!
//! The units are not evaluated where coverage is exactly zero, so no value the CPU never computes
//! reaches the frame, and the blend is per channel in linear light against the operation's own
//! input, as the CPU's is.
//!
//! # The step's words
//!
//! A step's header holds its words base and blocks base and its position map, which for a masked
//! step is the units' map. From its words base the masked step holds, in order: the mask's position
//! map (six `f32`), its bounds `x0, y0, x1, y1` (four `f32`), the supersample and inversion flags,
//! the scale, then for each component its mode, its inversion and its program's two base indices,
//! then each unit's two base indices, then the components' and the units' own words. Its block is
//! the components' blocks then the units'.
use super::{GpuProgram, PositionMap};

/// The words before a masked step's components: the position map, the bounds, the two flags and
/// the scale.
const HEADER_WORDS: usize = PositionMap::WORDS + 4 + 3;
const BOUNDS: usize = PositionMap::WORDS;
const SUPERSAMPLE: usize = BOUNDS + 4;
const INVERT: usize = SUPERSAMPLE + 1;
const SCALE: usize = INVERT + 1;
/// Each component's mode, inversion and two base indices.
const COMPONENT_WORDS: usize = 4;
/// Each unit's two base indices.
const UNIT_WORDS: usize = 2;

/// The prefix of every name the surface generates for a masked step. No entry may start with it.
pub(super) const GENERATED: &str = "lf_surface";

/// How a component folds into the coverage composed so far, as the CPU's mask composes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CoverageMode {
    /// `max(m, c)`.
    Add,
    /// `min(m, 1 − c)`.
    Subtract,
    /// `min(m, c)`.
    Intersect,
}

impl CoverageMode {
    fn word(self) -> u32 {
        match self {
            Self::Add => 0,
            Self::Subtract => 1,
            Self::Intersect => 2,
        }
    }
}

/// One mask component: its coverage program, how it folds and whether it is inverted first.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageComponent {
    pub mode: CoverageMode,
    /// The component's own inversion, `1 − c`, before the fold.
    pub invert: bool,
    /// `fn <entry>(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32`.
    pub program: GpuProgram,
}

/// A mask as the masked step composes it: its components and the data that places and folds them.
#[derive(Clone, Debug, PartialEq)]
pub struct Coverage {
    /// From the stage pixel to the pixel of the stage the mask was compiled against.
    pub position: PositionMap,
    /// `[x0, y0, x1, y1)` of the mask's stage, outside which coverage is exactly zero and nothing
    /// is evaluated.
    pub bounds: [u32; 4],
    /// The thin-feature rule: coverage is the mean of the fold at the four pixels `2q + (i, j)` of
    /// a stage twice the size, which the components' words were compiled against.
    pub supersample: bool,
    pub components: Vec<CoverageComponent>,
    /// The whole mask's inversion, `1 − m`, after the fold.
    pub invert: bool,
    /// `amount / 100`, the final multiply.
    pub scale: f32,
}

/// A pointwise colour operation and the mask it is blended by.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskedColour {
    /// The operation's units in order, each a colour program, chained with nothing clamped between.
    pub units: Vec<GpuProgram>,
    /// From the stage pixel to the `pos` every unit receives.
    pub position: PositionMap,
    pub mask: Coverage,
}

/// Which function a program of a step is, and so its signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    /// `fn(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32>`.
    Colour,
    /// `fn(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32`.
    Coverage,
    /// A spatial program's kernels and applies, which its step checks
    /// ([`super::spatial::validate_spatial`]).
    Spatial,
}

impl MaskedColour {
    /// Every program, components first, with its role.
    pub(super) fn programs(&self) -> impl Iterator<Item = (Role, &GpuProgram)> {
        self.mask
            .components
            .iter()
            .map(|component| (Role::Coverage, &component.program))
            .chain(self.units.iter().map(|unit| (Role::Colour, unit)))
    }

    /// The words this step packs after its header.
    pub(super) fn word_count(&self) -> usize {
        HEADER_WORDS
            + COMPONENT_WORDS * self.mask.components.len()
            + UNIT_WORDS * self.units.len()
            + self
                .programs()
                .map(|(_, program)| program.words.len())
                .sum::<usize>()
    }

    /// The block words this step packs.
    pub(super) fn block_count(&self) -> usize {
        self.programs()
            .map(|(_, program)| program.block.len())
            .sum()
    }

    /// Append this step's words at the base `words.len()` the header recorded for it, its programs'
    /// blocks counted from `block`, the step's own blocks base: exactly [`Self::word_count`] of
    /// them. Its blocks are its programs', in [`Self::programs`]' order.
    pub(super) fn pack_words(&self, words: &mut Vec<u32>, mut block: usize) {
        let mask = &self.mask;
        let base = words.len();
        words.extend(mask.position.words());
        words.extend(mask.bounds.map(|bound| (bound as f32).to_bits()));
        words.extend([
            u32::from(mask.supersample),
            u32::from(mask.invert),
            mask.scale.to_bits(),
        ]);
        let mut word = base
            + HEADER_WORDS
            + COMPONENT_WORDS * mask.components.len()
            + UNIT_WORDS * self.units.len();
        for component in &mask.components {
            words.extend([
                component.mode.word(),
                u32::from(component.invert),
                word as u32,
                block as u32,
            ]);
            word += component.program.words.len();
            block += component.program.block.len();
        }
        for unit in &self.units {
            words.extend([word as u32, block as u32]);
            word += unit.words.len();
            block += unit.block.len();
        }
        for (_, program) in self.programs() {
            words.extend_from_slice(&program.words);
        }
        debug_assert_eq!(words.len(), base + self.word_count());
    }

    /// The WGSL this step adds: a module-scope function composing its coverage, named after its
    /// step `index`, and the fragment's statements that blend its units by it. `base` is the step's
    /// header in `lf_words`.
    pub(super) fn assemble(&self, index: usize, base: usize) -> (String, String) {
        let mask = &self.mask;
        let w = |offset: usize| format!("lf_surface_words + {offset}u");
        let mut function = format!(
            "
fn lf_surface_mask_{index}(lf_surface_stage: vec2<f32>, lf_surface_rgb: vec3<f32>) -> f32 {{
    let lf_surface_words = lf_words[{base}u];
    let lf_surface_at = vec2<f32>(
        lf_f32({a}) * lf_surface_stage.x + lf_f32({b}) * lf_surface_stage.y + lf_f32({tx}),
        lf_f32({c}) * lf_surface_stage.x + lf_f32({d}) * lf_surface_stage.y + lf_f32({ty}),
    );
    if lf_surface_at.x < lf_f32({x0}) || lf_surface_at.y < lf_f32({y0})
        || lf_surface_at.x >= lf_f32({x1}) || lf_surface_at.y >= lf_f32({y1}) {{
        return 0.0;
    }}
    let lf_surface_fine = lf_word({fine}) != 0u;
    let lf_surface_samples = select(1u, 4u, lf_surface_fine);
    var lf_surface_sum = 0.0;
    for (var lf_surface_sample = 0u; lf_surface_sample < lf_surface_samples;
        lf_surface_sample = lf_surface_sample + 1u) {{
        let lf_surface_offset = vec2<f32>(
            f32(lf_surface_sample & 1u),
            f32(lf_surface_sample >> 1u),
        );
        let lf_surface_pos = select(
            lf_surface_at,
            2.0 * lf_surface_at + lf_surface_offset,
            lf_surface_fine,
        );
        var lf_surface_m = 0.0;
",
            a = w(0),
            b = w(1),
            tx = w(2),
            c = w(3),
            d = w(4),
            ty = w(5),
            x0 = w(BOUNDS),
            y0 = w(BOUNDS + 1),
            x1 = w(BOUNDS + 2),
            y1 = w(BOUNDS + 3),
            fine = w(SUPERSAMPLE),
        );
        for (number, component) in mask.components.iter().enumerate() {
            let at = HEADER_WORDS + COMPONENT_WORDS * number;
            function.push_str(&format!(
                "        lf_surface_m = lf_surface_compose(lf_surface_m, \
                 {}(lf_surface_pos, lf_surface_rgb, lf_word({}), lf_word({})), \
                 lf_word({}), lf_word({}));\n",
                component.program.entry,
                w(at + 2),
                w(at + 3),
                w(at),
                w(at + 1),
            ));
        }
        function.push_str(&format!(
            "        if lf_word({invert}) != 0u {{
            lf_surface_m = 1.0 - lf_surface_m;
        }}
        lf_surface_sum = lf_surface_sum + lf_f32({scale}) * lf_surface_m;
    }}
    return select(lf_surface_sum, lf_surface_sum * 0.25, lf_surface_fine);
}}
",
            invert = w(INVERT),
            scale = w(SCALE),
        ));
        let units_at = HEADER_WORDS + COMPONENT_WORDS * mask.components.len();
        let mut fragment = format!(
            "    {{
        let lf_surface_input = rgb;
        let lf_surface_coverage = lf_surface_mask_{index}(stage, lf_surface_input);
        if lf_surface_coverage != 0.0 {{
            let lf_surface_words = lf_words[{base}u];
            let lf_surface_pos = {};
            var lf_surface_output = lf_surface_input;
",
            PositionMap::wgsl(base + 2, "stage"),
        );
        for (number, unit) in self.units.iter().enumerate() {
            let at = units_at + UNIT_WORDS * number;
            fragment.push_str(&format!(
                "            lf_surface_output = {}(lf_surface_output, lf_surface_pos, \
                 lf_word({}), lf_word({}));\n",
                unit.entry,
                w(at),
                w(at + 1),
            ));
        }
        fragment.push_str(
            "            rgb = (1.0 - lf_surface_coverage) * lf_surface_input
                + lf_surface_coverage * lf_surface_output;
        }
    }
",
        );
        (function, fragment)
    }
}

/// The one fold of a component's coverage into the mask composed so far, which every masked step's
/// function calls, declared once in a shader that has a masked step.
pub(super) const COMPOSE: &str = "
fn lf_surface_compose(lf_surface_m: f32, lf_surface_c: f32, lf_surface_mode: u32,
    lf_surface_invert: u32) -> f32 {
    let lf_surface_own = select(lf_surface_c, 1.0 - lf_surface_c, lf_surface_invert != 0u);
    if lf_surface_mode == 1u {
        return min(lf_surface_m, 1.0 - lf_surface_own);
    }
    if lf_surface_mode == 2u {
        return min(lf_surface_m, lf_surface_own);
    }
    return max(lf_surface_m, lf_surface_own);
}
";

/// The word ranges of `new` that differ from `old`, in chunks of `chunk` words merged where they
/// touch: what a tick writes of the blocks. A buffer's blocks (`blocks::WrittenBlocks`) answer the
/// same ranges without comparing the blocks a tick hands again at the same place. A brush stroke
/// being painted appends its new segments to its block and rewrites the index after them, so a
/// tick writes those and not the strokes' segments it already holds; an unchanged block writes
/// nothing. Words of `old` past the end of `new` are not written: nothing reads past a block's own
/// length.
#[cfg(test)]
pub(super) fn changed_ranges(
    old: &[u32],
    new: &[u32],
    chunk: usize,
) -> Vec<std::ops::Range<usize>> {
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut start = 0;
    while start < new.len() {
        let end = (start + chunk).min(new.len());
        if old.get(start..end) != Some(&new[start..end]) {
            match ranges.last_mut() {
                Some(last) if last.end == start => last.end = end,
                _ => ranges.push(start..end),
            }
        }
        start = end;
    }
    ranges
}
