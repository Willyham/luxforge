//! Where the header parsers read bytes: a whole file in memory, or an open file read under a byte
//! and read budget. The parsers are written once over [`Source`], so the bounded read and the
//! whole-file read run the same code and differ only where the budget runs out.

use super::{CHUNK_BYTES, HEADER_HEAD_BYTES, MAX_HEADER_BYTES, MAX_HEADER_READS};
use std::io::{self, Read, Seek, SeekFrom};

/// Random access to a file's bytes for the header parsers. Every read copies a short range; a
/// range outside the file, or one the budget no longer allows, fills nothing and is `false`.
pub(super) trait Source {
    /// The file's length in bytes.
    fn len(&self) -> u64;

    /// Fill `out` with the bytes at `at`, when all of them lie inside the file and can be read.
    fn read(&mut self, at: u64, out: &mut [u8]) -> bool;

    fn array<const N: usize>(&mut self, at: u64) -> Option<[u8; N]> {
        let mut out = [0; N];
        self.read(at, &mut out).then_some(out)
    }
}

/// A whole file in memory: the reference every bounded read is tested against.
pub(super) struct Whole<'a>(pub(super) &'a [u8]);

impl Source for Whole<'_> {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> bool {
        let Ok(start) = usize::try_from(at) else {
            return false;
        };
        let Some(bytes) = start
            .checked_add(out.len())
            .and_then(|end| self.0.get(start..end))
        else {
            return false;
        };
        out.copy_from_slice(bytes);
        true
    }
}

/// One contiguous range read from the file, kept in [`Bounded::buffer`].
#[derive(Clone, Copy)]
struct Chunk {
    file_at: u64,
    buffer_at: usize,
    len: usize,
}

/// An open file read once at its head, then only in [`CHUNK_BYTES`]-aligned chunks around the
/// ranges the parsers ask for, each kept, until [`MAX_HEADER_BYTES`] or [`MAX_HEADER_READS`] is
/// spent. Past the budget, or after an I/O error, it serves only what it already holds.
pub(super) struct Bounded<'f, R> {
    file: &'f mut R,
    len: u64,
    /// Every byte read, chunk after chunk: never more than [`MAX_HEADER_BYTES`].
    buffer: Vec<u8>,
    chunks: Vec<Chunk>,
    capped: bool,
    error: Option<io::Error>,
}

impl<'f, R: Read + Seek> Bounded<'f, R> {
    /// The file `len` bytes long, its first [`HEADER_HEAD_BYTES`] (or all of it, when shorter)
    /// read at once.
    pub(super) fn new(file: &'f mut R, len: u64) -> Self {
        let head = usize::try_from(len.min(HEADER_HEAD_BYTES as u64)).unwrap_or(HEADER_HEAD_BYTES);
        // Room for the head and the few chunks most files need, so they do not reallocate.
        let typical = if (head as u64) < len {
            head + 4 * CHUNK_BYTES
        } else {
            head
        };
        let mut source = Self {
            file,
            len,
            buffer: Vec::with_capacity(typical),
            chunks: Vec::with_capacity(MAX_HEADER_READS),
            capped: false,
            error: None,
        };
        if head > 0 {
            source.fetch(0, head);
        }
        source
    }

    /// The bytes read, the reads made, whether the budget stopped a read, and the first I/O
    /// error.
    pub(super) fn finish(self) -> (u64, u32, bool, Option<io::Error>) {
        let reads = u32::try_from(self.chunks.len()).unwrap_or(u32::MAX);
        (self.buffer.len() as u64, reads, self.capped, self.error)
    }

    /// Read `len` bytes at `file_at` as a new chunk, when the budget allows it.
    fn fetch(&mut self, file_at: u64, len: usize) -> Option<Chunk> {
        if self.chunks.len() >= MAX_HEADER_READS || self.buffer.len() + len > MAX_HEADER_BYTES {
            self.capped = true;
            return None;
        }
        let buffer_at = self.buffer.len();
        self.buffer.resize(buffer_at + len, 0);
        let read = self
            .file
            .seek(SeekFrom::Start(file_at))
            .and_then(|_| self.file.read_exact(&mut self.buffer[buffer_at..]));
        if let Err(error) = read {
            self.buffer.truncate(buffer_at);
            self.error = Some(error);
            return None;
        }
        let chunk = Chunk {
            file_at,
            buffer_at,
            len,
        };
        self.chunks.push(chunk);
        Some(chunk)
    }
}

impl<R: Read + Seek> Source for Bounded<'_, R> {
    fn len(&self) -> u64 {
        self.len
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> bool {
        let Some(end) = at
            .checked_add(out.len() as u64)
            .filter(|end| *end <= self.len)
        else {
            return false;
        };
        if out.is_empty() {
            return true;
        }
        let held = self
            .chunks
            .iter()
            .copied()
            .find(|chunk| chunk.file_at <= at && end <= chunk.file_at + chunk.len as u64);
        let chunk = match held {
            Some(chunk) => chunk,
            None if self.capped || self.error.is_some() => return false,
            None => {
                let chunk = CHUNK_BYTES as u64;
                let start = at / chunk * chunk;
                let stop = end.div_ceil(chunk).saturating_mul(chunk).min(self.len);
                // At most one IFD table, so far below `usize::MAX`.
                let Ok(len) = usize::try_from(stop - start) else {
                    return false;
                };
                match self.fetch(start, len) {
                    Some(chunk) => chunk,
                    None => return false,
                }
            }
        };
        // Inside the chunk, so both conversions are exact.
        let from = chunk.buffer_at + (at - chunk.file_at) as usize;
        out.copy_from_slice(&self.buffer[from..from + out.len()]);
        true
    }
}
