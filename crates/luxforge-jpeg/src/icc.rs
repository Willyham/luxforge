//! The ICC profile's APP2 chunks, both ways (ICC.1, Annex B.4): each `ICC_PROFILE\0`, its
//! sequence number from 1, the chunk count and its part of the profile, at most 255 chunks.

use crate::{JpegError, MAX_SEGMENT_PAYLOAD};

pub(crate) const ICC_HEADER: &[u8] = b"ICC_PROFILE\0";

/// The profile bytes one chunk carries at most: a whole APP2 payload less the chunk header.
pub(crate) const MAX_CHUNK_DATA: usize = MAX_SEGMENT_PAYLOAD - ICC_HEADER.len() - 2;

/// The largest ICC profile reassembled. The caller's profile check parses no more than this, so a
/// larger profile would be refused there too; stopping here bounds the copy.
pub(crate) const MAX_ICC_BYTES: usize = 1024 * 1024;

/// The ICC profile carried by `segments` (APP2 payloads in file order): the `ICC_PROFILE` chunks
/// concatenated in sequence order. `None` without chunks. Every chunk must declare the same count,
/// which must equal the number of chunks, with each number from 1 to that count exactly once.
pub(crate) fn reassemble<'s>(
    segments: impl Iterator<Item = &'s [u8]>,
) -> Result<Option<Vec<u8>>, JpegError> {
    let unreadable = |why: &str| JpegError::Icc(format!("unreadable ICC: {why}"));
    let mut chunks: [Option<&[u8]>; 256] = [None; 256];
    let mut count = None;
    let mut found = 0_usize;
    let mut bytes = 0_usize;
    for segment in segments {
        let Some(chunk) = segment.strip_prefix(ICC_HEADER) else {
            continue;
        };
        let [sequence, total, data @ ..] = chunk else {
            return Err(unreadable("chunk header"));
        };
        if *count.get_or_insert(*total) != *total {
            return Err(unreadable("chunk counts differ"));
        }
        if *sequence == 0 || sequence > total {
            return Err(unreadable("chunk number out of range"));
        }
        let slot = &mut chunks[usize::from(*sequence)];
        if slot.is_some() {
            return Err(unreadable("repeated chunk number"));
        }
        bytes += data.len();
        if bytes > MAX_ICC_BYTES {
            return Err(JpegError::Icc("ICC profile larger than 1 MiB".into()));
        }
        *slot = Some(data);
        found += 1;
    }
    let Some(count) = count else {
        return Ok(None);
    };
    if found != usize::from(count) {
        return Err(unreadable("missing chunk"));
    }
    let mut profile = Vec::with_capacity(bytes);
    for chunk in &chunks[1..=usize::from(count)] {
        profile.extend_from_slice(chunk.expect("every numbered chunk was found"));
    }
    Ok(Some(profile))
}

/// How many chunks `profile` takes, refusing an empty profile and one past 255 chunks.
pub(crate) fn chunk_count(profile: &[u8]) -> Result<u8, JpegError> {
    if profile.is_empty() {
        return Err(JpegError::Internal(
            "jpeg encode: an empty ICC profile".into(),
        ));
    }
    u8::try_from(profile.len().div_ceil(MAX_CHUNK_DATA)).map_err(|_| {
        JpegError::Internal("jpeg encode: the ICC profile does not fit 255 APP2 chunks".into())
    })
}

/// Each APP2 payload of `profile`'s chunks, numbered from 1 of `count`, built in `buffer` one at a
/// time and handed to `write`.
pub(crate) fn write_chunks(
    profile: &[u8],
    count: u8,
    buffer: &mut Vec<u8>,
    mut write: impl FnMut(&[u8]),
) {
    for (index, data) in profile.chunks(MAX_CHUNK_DATA).enumerate() {
        buffer.clear();
        buffer.extend_from_slice(ICC_HEADER);
        // At most 255 chunks, which `chunk_count` checked.
        buffer.extend_from_slice(&[index as u8 + 1, count]);
        buffer.extend_from_slice(data);
        write(buffer);
    }
}
