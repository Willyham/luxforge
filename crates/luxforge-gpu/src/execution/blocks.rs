//! What a link's blocks buffer holds, kept by the shared block each of its words came from
//! (`docs/design/gpu-preview.md`, "A tick").
//!
//! Every program's storage block is an `Arc<[u32]>` its caller hands over again each tick, and a
//! link's blocks buffer holds its steps' blocks concatenated in packing order
//! ([`GpuStep::each_block`]). A tick of a drag hands the same blocks — a lens warp's grid, a
//! curve's knots, a committed brush's segments — and changes only the words, so
//! [`WrittenBlocks`] keeps, beside the words the buffer holds, each block it packed and where it
//! starts:
//!
//! - **A block handed again at the same place is neither compared nor copied.** The same allocation
//!   (`Arc::ptr_eq`) at the same start holds the words the buffer already holds there. The block
//!   itself is held, never its address: an allocation freed and reused for other contents could
//!   carry an old address, and holding it keeps it from being freed while it is compared.
//! - **Every other block is compared and copied.** A new block, or one that moved because a block
//!   before it changed length, is compared with what the buffer held there, chunk by chunk of the
//!   buffer, and copied where it differs. So the chunks a tick writes are exactly those
//!   `changed_ranges` finds between the buffer's whole old packing and its new one, and the buffer
//!   holds the same words after every tick; a tick whose every block moved compares and copies
//!   them all, as packing them whole does. A forgotten buffer — a new one, or a link's new input —
//!   writes every chunk, as a first write does.
//! - **Each block is hashed once.** A link's content key reads its blocks through
//!   [`WrittenBlocks::key`]: each block's words hashed the first time a key asks after it is packed,
//!   and kept with it, so an unchanged link's key hashes none of them.
use super::GpuStep;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    sync::Arc,
};

/// A blocks buffer's words as last packed, and the blocks they were packed from.
#[derive(Default)]
pub struct WrittenBlocks {
    /// Every block of the last update, in packing order, at least one word: what the buffer holds
    /// once that update's ranges are written. Empty before the first.
    words: Vec<u32>,
    /// Each block of the last update in packing order, held, with where it starts in `words` and,
    /// once a key asked for it, the hash of its words.
    sources: Vec<Source>,
    /// The buffer does not hold `words` — it is new, or its link's input is — so the next update
    /// writes every chunk, as a first write does.
    forgotten: bool,
    /// The key of the blocks' contents, until a block changes.
    key: Option<u64>,
}

struct Source {
    block: Arc<[u32]>,
    start: usize,
    hash: Option<u64>,
}

/// What one update makes the buffer write, and in tests what it compared and copied.
#[derive(Debug, Default)]
pub struct Update {
    /// The chunks that changed, in order, adjacent ones merged: what the tick writes.
    pub ranges: Vec<Range<usize>>,
    /// Whether the buffer's contents changed: a chunk, or its length.
    pub changed: bool,
    /// Words of the tick's blocks compared with what the buffer held at their place.
    #[cfg(any(test, feature = "qualification"))]
    pub compared: u64,
    /// Words copied into what the buffer holds.
    #[cfg(any(test, feature = "qualification"))]
    pub copied: u64,
}

impl Update {
    /// The words the tick writes.
    pub fn written(&self) -> u64 {
        self.ranges.iter().map(|range| range.len() as u64).sum()
    }
}

/// The words a buffer of `steps`' blocks holds: every block's, at least one, as a buffer of no
/// block words still holds one.
pub fn block_len(steps: &[GpuStep]) -> usize {
    let mut len = 0;
    for step in steps {
        step.each_block(&mut |block| len += block.len());
    }
    len.max(1)
}

fn hash_of(parts: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

impl WrittenBlocks {
    /// The words the buffer holds: every block of the last update's steps, in packing order.
    pub fn words(&self) -> &[u32] {
        &self.words
    }

    /// The buffer no longer holds what was written — new buffers, or a link's new input: the next
    /// update writes every chunk. The words stay readable until then.
    pub fn forget(&mut self) {
        self.forgotten = true;
    }

    /// Make the words `steps`' blocks, packed in order, comparing and copying only the blocks that
    /// are not the ones held at the same start; answers the chunks of `chunk` words that changed —
    /// `changed_ranges` of the old packing and the new, or every chunk once forgotten — and
    /// whether anything did.
    pub fn update(&mut self, steps: &[GpuStep], chunk: usize) -> Update {
        let held = self.words.len();
        let len = block_len(steps);
        self.words.resize(len, 0);
        let mut update = Update::default();
        let (words, sources) = (&mut self.words, &mut self.sources);
        let (mut index, mut start, mut replaced) = (0, 0, false);
        for step in steps {
            step.each_block(&mut |block| {
                // Two empty blocks hold the same words, whichever allocations they are.
                let same = sources.get(index).is_some_and(|source| {
                    source.start == start
                        && (Arc::ptr_eq(&source.block, block)
                            || source.block.is_empty() && block.is_empty())
                });
                if !same {
                    copy_over(words, held, start, block, chunk, &mut update);
                    let source = Source {
                        block: Arc::clone(block),
                        start,
                        hash: None,
                    };
                    match sources.get_mut(index) {
                        Some(kept) => *kept = source,
                        None => sources.push(source),
                    }
                    replaced = true;
                }
                start += block.len();
                index += 1;
            });
        }
        if sources.len() != index {
            sources.truncate(index);
            replaced = true;
        }
        // A buffer of no block words holds one zero word, which it may hold already.
        if start == 0 && (held != 1 || words[0] != 0) {
            copy_over(words, held, 0, &[0], chunk, &mut update);
        }
        if replaced {
            self.key = None;
        }
        if std::mem::take(&mut self.forgotten) {
            update.ranges.clear();
            update.ranges.push(0..len);
        }
        update.changed = !update.ranges.is_empty() || held != len;
        update
    }

    /// [`Self::update`], then the chunks that changed written into `buffer`.
    pub fn write(
        &mut self,
        queue: &wgpu::Queue,
        buffer: &wgpu::Buffer,
        steps: &[GpuStep],
        chunk: usize,
    ) -> Update {
        let update = self.update(steps, chunk);
        for range in &update.ranges {
            queue.write_buffer(
                buffer,
                (range.start * 4) as u64,
                &super::le_bytes(&self.words[range.clone()]),
            );
        }
        update
    }

    /// The key of the blocks' contents: each non-empty block's length and the hash of its words, in
    /// packing order. Blocks of other contents give another key, and the same contents the same
    /// one, whichever allocations hold them; a block's words are hashed once.
    pub fn key(&mut self) -> u64 {
        if let Some(key) = self.key {
            return key;
        }
        let mut hasher = DefaultHasher::new();
        for source in self
            .sources
            .iter_mut()
            .filter(|source| !source.block.is_empty())
        {
            let block = &source.block;
            let hash = *source.hash.get_or_insert_with(|| hash_of(&block[..]));
            (block.len(), hash).hash(&mut hasher);
        }
        let key = hasher.finish();
        self.key = Some(key);
        key
    }
}

/// Compare `block`, packed at `start`, with the first `held` words of `words` — the old packing —
/// chunk by chunk of the buffer, copy it over where it differs, and mark those chunks changed in
/// `update`. Words past the old packing differ.
fn copy_over(
    words: &mut [u32],
    held: usize,
    start: usize,
    block: &[u32],
    chunk: usize,
    update: &mut Update,
) {
    let end = start + block.len();
    let mut at = start;
    while at < end {
        let next = ((at / chunk + 1) * chunk).min(end);
        let new = &block[at - start..next - start];
        let differs = next > held || {
            #[cfg(any(test, feature = "qualification"))]
            {
                update.compared += new.len() as u64;
            }
            words[at..next] != *new
        };
        if differs {
            words[at..next].copy_from_slice(new);
            #[cfg(any(test, feature = "qualification"))]
            {
                update.copied += new.len() as u64;
            }
            let first = at / chunk * chunk;
            let range = first..(first + chunk).min(words.len());
            match update.ranges.last_mut() {
                // Another block in the same chunk already changed it.
                Some(last) if last.end >= range.end => {}
                Some(last) if last.end == range.start => last.end = range.end,
                _ => update.ranges.push(range),
            }
        }
        at = next;
    }
}

#[cfg(test)]
mod tests;
