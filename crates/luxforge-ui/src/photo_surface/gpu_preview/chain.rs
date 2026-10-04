//! A plan's steps run as a chain of links, each written whole into a texture the next one reads
//! as its boundary (`docs/design/gpu-preview.md`, "The materialized chain").
//!
//! A plan holding spatial steps is split before each of them: the content steps before the first
//! spatial step form the first link, each spatial step with the colour steps after it forms the
//! next, and the last spatial step's link holds everything after it, the geometry tail, the output
//! steps and the clipping marks among them. Every link but the last writes the linear values of
//! its last step into an intermediate texture of the boundary's size and format, which the next
//! link binds as its own boundary; the last writes the output as a whole plan does.
//!
//! - **Each step runs once a tick.** A spatial step's passes read their input from the texture
//!   the link before wrote, so a pass's `lf_source` is one texel load, never the steps before it
//!   evaluated again at every texel a filter reads. A chain of spatial steps costs the sum of its
//!   links, where recomputing every earlier step at every read made it grow with the square of
//!   their count.
//! - **A link is its own sequence.** Its modules hold only its own steps' programs, so a pass is
//!   the same module wherever its spatial step sits in a chain, and two masked layers of one shape
//!   share every pipeline: a chain compiles each kind of link once.
//! - **A link whose input and words did not change is not run.** Each intermediate keeps the
//!   content key of what it holds — the key of its input and the link's packed words, its blocks'
//!   contents and pipeline — so a tick runs only the links from the first one it changes, and inside
//!   a link only the passes the tick changes ([`spatial::Schedule`]).
//! - **A block handed again is not copied.** Each link keeps the blocks it last wrote by the shared
//!   block each came from ([`WrittenBlocks`]): a tick compares, copies and writes only the blocks
//!   that are not the same allocation at the same place, and its key hashes only those.
//! - **Scratch is the slot's.** A link holds the planes its applies read; its scratch planes are
//!   the slot's pool's, which every link writes in turn ([`spatial::Pool`]), so a link trusts what a
//!   pool texture holds only when it wrote it last.
//! - **Bounds.** Each intermediate is the boundary's size in the boundary's format, charged to the
//!   GPU-preview budget with the slot, beside each link's words, blocks and kept planes and the
//!   pool, once ([`super::chain_charge`]).
use super::{
    Charged, Compiled, GpuStep, MAP_WORDS, STEP_WORDS, TexelMap,
    blocks::{Update, WrittenBlocks},
    encode_pass_over,
    spatial::{Pool, Rect},
};

/// `steps` split into the links the stage runs one after another: every link before the last,
/// each written into an intermediate, and the last, which writes the output. A plan without a
/// spatial step is one link.
pub(super) struct Chain<'a> {
    pub(super) links: Vec<&'a [GpuStep]>,
    pub(super) last: &'a [GpuStep],
}

pub(super) fn chain(steps: &[GpuStep]) -> Chain<'_> {
    let mut links = Vec::new();
    let mut start = 0;
    for (index, step) in steps.iter().enumerate() {
        if matches!(step, GpuStep::Spatial(_)) && index > start {
            links.push(&steps[start..index]);
            start = index;
        }
    }
    Chain {
        links,
        last: &steps[start..],
    }
}

/// The words and blocks of `steps` over a boundary whose texels `texels` maps to the stage, with
/// the output's first pixel at `offset`: their words ([`pack_words`]), and every step's blocks in
/// order ([`GpuStep::each_block`]), at least one word. A slot writes its blocks through
/// [`WrittenBlocks`], which keeps this packing and copies only the blocks a tick changes.
#[cfg(any(test, feature = "qualification"))]
pub(super) fn pack_steps(
    texels: TexelMap,
    offset: (u32, u32),
    steps: &[GpuStep],
    words: &mut Vec<u32>,
    blocks: &mut Vec<u32>,
) {
    pack_words(texels, offset, steps, words);
    blocks.clear();
    for step in steps {
        step.each_block(&mut |block| blocks.extend_from_slice(block));
    }
    if blocks.is_empty() {
        blocks.push(0);
    }
}

/// The words of `steps` over a boundary whose texels `texels` maps to the stage, with the output's
/// first pixel at `offset`: the header — the texel map, the offset, each step's base indices and
/// position map — then every step's words. The bases index the blocks packed in step order.
pub(super) fn pack_words(
    texels: TexelMap,
    offset: (u32, u32),
    steps: &[GpuStep],
    words: &mut Vec<u32>,
) {
    words.clear();
    words.extend(
        [
            texels.origin[0],
            texels.origin[1],
            texels.step[0],
            texels.step[1],
        ]
        .map(f32::to_bits),
    );
    words.extend([offset.0, offset.1]);
    let header = MAP_WORDS + STEP_WORDS * steps.len();
    let (mut word, mut block) = (header, 0);
    for step in steps {
        words.extend([word as u32, block as u32]);
        words.extend(step.position().words());
        word += step.word_count();
        block += step.block_count();
    }
    let mut block = 0;
    for step in steps {
        match step {
            GpuStep::Colour { program, .. } => words.extend_from_slice(&program.words),
            GpuStep::Masked(masked) => masked.pack_words(words, block),
            GpuStep::Geometry(tail) => words.extend_from_slice(&tail.program().words),
            GpuStep::Spatial(spatial) => spatial.pack_words(words, block),
            GpuStep::Clipping(marks) => words.extend(marks.words()),
        }
        block += step.block_count();
    }
}

/// The content key of a link's output: the key of its input, its packed words and blocks and the
/// pipeline that ran it.
pub(super) fn link_key(input: u64, words: &[u32], blocks: &[u32], pipeline: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    (input, words, blocks, pipeline).hash(&mut hasher);
    hasher.finish()
}

/// [`link_key`] with the link's blocks named by the key of their contents
/// ([`WrittenBlocks::key`]), which the link keeps with the blocks it wrote, so a tick that hands it
/// the same blocks hashes none of their words. Blocks of other contents give another key.
pub(super) fn written_key(input: u64, words: &[u32], blocks: u64, pipeline: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    (input, words, blocks, pipeline).hash(&mut hasher);
    hasher.finish()
}

/// The key of a boundary's texels, which the first link reads.
pub(super) fn boundary_key(version: u64) -> u64 {
    link_key(version, &[], &[], u64::MAX)
}

/// One link of a slot's chain before its last: the intermediate it writes, which the next link
/// reads as its boundary; its own words, blocks and kept spatial planes; and what the intermediate
/// holds.
pub(super) struct LinkSlot {
    pub(super) texture: wgpu::Texture,
    target: wgpu::TextureView,
    pub(super) words: Charged,
    pub(super) blocks: Charged,
    /// Group 0: the link's words and blocks, and the texture it reads.
    pub(super) bindings: wgpu::BindGroup,
    written_words: Vec<u32>,
    /// The blocks last written, by the shared block each came from.
    written_blocks: WrittenBlocks,
    pub(super) spatial: Option<Box<super::SpatialSlot>>,
    /// The content key of what `texture` holds; `None` before it is first written.
    key: Option<u64>,
    /// The pipeline that last wrote it.
    pipeline: Option<u64>,
    texture_bytes: u64,
}

impl LinkSlot {
    pub(super) fn new(
        texture: wgpu::Texture,
        words: Charged,
        blocks: Charged,
        bindings: wgpu::BindGroup,
        texture_bytes: u64,
    ) -> Self {
        let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            target,
            words,
            blocks,
            bindings,
            written_words: Vec::new(),
            written_blocks: WrittenBlocks::default(),
            spatial: None,
            key: None,
            pipeline: None,
            texture_bytes,
        }
    }

    /// Everything the link holds, as charged: the pool's textures are the slot's.
    pub(super) fn bytes(&self) -> u64 {
        self.texture_bytes
            + self.words.bytes
            + self.blocks.bytes
            + self
                .spatial
                .as_ref()
                .map_or(0, |spatial| spatial.planes.bytes)
    }

    /// Forget what the link's buffers and intermediate hold: new buffers or a new input. Its
    /// planes' schedule takes a new holder from `pool`.
    pub(super) fn forget(&mut self, pool: &mut Pool) {
        self.written_words.clear();
        self.written_blocks.forget();
        self.key = None;
        if let Some(spatial) = self.spatial.as_mut() {
            spatial.forget(pool);
        }
    }

    /// Write the tick's `words` and the chunks of `steps`' blocks that changed, comparing only the
    /// blocks it does not already hold at the same place ([`WrittenBlocks::write`]); answers what
    /// the blocks' update wrote.
    pub(super) fn write(
        &mut self,
        queue: &wgpu::Queue,
        words: &[u32],
        steps: &[GpuStep],
    ) -> Update {
        if self.written_words != words {
            queue.write_buffer(&self.words.buffer, 0, &super::le_bytes(words));
            self.written_words.clear();
            self.written_words.extend_from_slice(words);
        }
        self.written_blocks
            .write(queue, &self.blocks.buffer, steps, super::BLOCK_CHUNK)
    }

    /// The blocks the link last wrote.
    #[cfg(test)]
    pub(super) fn written_blocks(&self) -> &WrittenBlocks {
        &self.written_blocks
    }

    /// Run the link into its intermediate when what it holds is not the key of `input`, its tick's
    /// `words`, the blocks it last wrote and `pipeline`: its spatial step's passes this tick
    /// changes, then its frame's pass over the boundary's `size`. On an incremental tick (`dirty`,
    /// the rectangle its input and its own steps changed in since it last ran, when it last ran the
    /// same pipeline) only over the rectangle that change reaches, keeping the rest of what the
    /// intermediate holds. Its scratch planes are `pool`'s. Answers whether it encoded anything,
    /// how many passes it dispatched, and on an incremental tick the rectangle its output changed
    /// in.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        (compiled, pipeline): (&Compiled, u64),
        pool: &mut Pool,
        steps: &[GpuStep],
        words: &[u32],
        input: u64,
        (texels, size): (TexelMap, (u32, u32)),
        dirty: Option<Rect>,
    ) -> (bool, u64, Option<Rect>) {
        let key = written_key(input, words, self.written_blocks.key(), pipeline);
        if self.key == Some(key) {
            return (false, 0, dirty.map(|_| Rect::whole((0, 0))));
        }
        // Only what the same pipeline last wrote is kept around a change.
        let dirty = dirty.filter(|_| self.key.is_some() && self.pipeline == Some(pipeline));
        let mut dispatched = 0;
        let mut reached = dirty;
        if let Some(spatial) = self.spatial.as_mut() {
            let (ran, over) = spatial.tick(
                device,
                queue,
                encoder,
                (&compiled.spatial, pipeline),
                &self.bindings,
                pool,
                steps,
                (words, self.written_blocks.words()),
                input,
                (texels, size),
                dirty,
            );
            dispatched = ran;
            reached = dirty.zip(over).map(|(dirty, over)| dirty.union(&over));
        }
        let planes = self
            .spatial
            .as_ref()
            .and_then(|spatial| spatial.groups())
            .and_then(|groups| groups.fragment.as_ref());
        encode_pass_over(
            encoder,
            &self.target,
            &compiled.render,
            (&self.bindings, planes),
            (size.0 as f32, size.1 as f32),
            reached,
        );
        self.key = Some(key);
        self.pipeline = Some(pipeline);
        (true, dispatched, reached)
    }

    /// The content key of what the intermediate holds.
    pub(super) fn key(&self) -> Option<u64> {
        self.key
    }
}

#[cfg(test)]
mod tests {
    use super::super::{GpuProgram, GpuSpatial};
    use super::*;

    fn colour(entry: &'static str) -> GpuStep {
        GpuStep::colour(GpuProgram::new(entry, ""))
    }

    fn spatial(entry: &'static str) -> GpuStep {
        GpuStep::Spatial(Box::new(GpuSpatial {
            program: GpuProgram::new(entry, ""),
            planes: Vec::new(),
            passes: Vec::new(),
            applies: Vec::new(),
            clamps: false,
            mask: None,
            halos: Vec::new(),
        }))
    }

    fn entries(steps: &[GpuStep]) -> Vec<String> {
        steps
            .iter()
            .map(|step| match step {
                GpuStep::Colour { program, .. } => program.entry.to_string(),
                GpuStep::Spatial(spatial) => spatial.program.entry.to_string(),
                _ => String::new(),
            })
            .collect()
    }

    /// A plan is split before each spatial step that is not its first step: the content steps,
    /// then each spatial step with the colour steps after it; the last link holds the rest.
    #[test]
    fn a_plan_is_split_before_each_spatial_step() {
        let plain = [colour("a"), colour("b")];
        let split = chain(&plain);
        assert!(split.links.is_empty());
        assert_eq!(entries(split.last), ["a", "b"]);

        let leading = [spatial("s"), colour("a")];
        let split = chain(&leading);
        assert!(split.links.is_empty());
        assert_eq!(entries(split.last), ["s", "a"]);

        let steps = [
            colour("a"),
            colour("b"),
            spatial("s1"),
            colour("c"),
            spatial("s2"),
            spatial("s3"),
            colour("d"),
        ];
        let split = chain(&steps);
        let links: Vec<Vec<String>> = split.links.iter().map(|link| entries(link)).collect();
        assert_eq!(links, [vec!["a", "b"], vec!["s1", "c"], vec!["s2"]]);
        assert_eq!(entries(split.last), ["s3", "d"]);
    }

    /// The key a link's output holds changes with its input, its words, its blocks and its
    /// pipeline, and with nothing else.
    #[test]
    fn a_links_key_follows_everything_it_reads() {
        let key = link_key(1, &[1, 2], &[3], 4);
        assert_eq!(key, link_key(1, &[1, 2], &[3], 4));
        assert_ne!(key, link_key(2, &[1, 2], &[3], 4));
        assert_ne!(key, link_key(1, &[1, 3], &[3], 4));
        assert_ne!(key, link_key(1, &[1, 2], &[4], 4));
        assert_ne!(key, link_key(1, &[1, 2], &[3], 5));
        assert_ne!(boundary_key(1), boundary_key(2));
    }
}
