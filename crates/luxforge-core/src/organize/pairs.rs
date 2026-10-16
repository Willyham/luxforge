//! RAW+JPEG pairs: the JPEG a camera records beside a RAW file of the same shot
//! (`docs/design/catalog.md`, "RAW+JPEG pairs").
//!
//! A JPEG file pairs with a RAW file in the same folder whose name is its own but for the
//! extension, ignoring case (`DSC_0007.NEF` and `DSC_0007.JPG`), when both are dated at most
//! [`PAIR_GAP_MS`] apart or either is undated. Each file pairs at most once: a JPEG with the first
//! such RAW file in the frames' order, and a RAW file with the first such JPEG. A view shows a pair
//! as its RAW file's row; grouping, picks and Develop see the RAW file alone.
use crate::{
    SourceTag,
    catalog_types::{FolderIndex, FrameFacts, ViewItem},
};
use std::collections::HashMap;

/// The most a pair's capture times may differ (2 s): a camera writes both from one exposure, and
/// records the same time on both; a RAW and a JPEG of one name further apart are two shots, a
/// counter that wrapped.
pub(crate) const PAIR_GAP_MS: u64 = 2_000;

/// A file's name without its extension, for comparing ignoring case.
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// The RAW+JPEG pairs among `frames`, as `(raw, jpeg)` indices into them, in the RAW files'
/// order. `kind` says what each frame's file is; a frame it answers `None` for (a photograph, a
/// merge's result) pairs with nothing.
pub(crate) fn pairs(
    frames: &[FrameFacts],
    kind: impl Fn(&FrameFacts) -> Option<SourceTag>,
) -> Vec<(usize, usize)> {
    let file = |frame: &FrameFacts| matches!(frame.item, ViewItem::File(_)) && !frame.merged;
    // The RAW files by folder and stem, lowercased: each key once, its first frame.
    let mut raws: HashMap<(FolderIndex, String), usize> = HashMap::new();
    for (at, frame) in frames.iter().enumerate() {
        if file(frame) && kind(frame) == Some(SourceTag::Raw) {
            raws.entry((frame.folder, stem(&frame.name).to_lowercase()))
                .or_insert(at);
        }
    }
    if raws.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for (at, frame) in frames.iter().enumerate() {
        if !file(frame) || kind(frame) != Some(SourceTag::Jpeg) {
            continue;
        }
        let key = (frame.folder, stem(&frame.name).to_lowercase());
        let Some(&raw) = raws.get(&key) else {
            continue;
        };
        let close = match (frames[raw].instant_ms, frame.instant_ms) {
            (Some(a), Some(b)) => a.abs_diff(b) <= PAIR_GAP_MS,
            _ => true,
        };
        if close {
            found.push((raw, at));
            // The RAW file is paired: no other JPEG of its name pairs with it.
            raws.remove(&key);
        }
    }
    found.sort_unstable();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_types::{BodyIndex, Exposure, FileId, FrameTables};

    fn frames(names: &[(&str, &str, Option<i64>)]) -> (Vec<FrameFacts>, Vec<SourceTag>) {
        let mut tables = FrameTables::default();
        let frames = names
            .iter()
            .enumerate()
            .map(|(at, (folder, name, instant_ms))| FrameFacts {
                item: ViewItem::File(FileId(at as i64 + 1)),
                folder: tables.folder((*folder).into()),
                name: (*name).into(),
                instant_ms: *instant_ms,
                local_day: None,
                position: None,
                body: BodyIndex(0),
                exposure: Exposure::default(),
                merged: false,
            })
            .collect();
        let kinds = names
            .iter()
            .map(|(_, name, _)| {
                if name.to_lowercase().ends_with(".jpg") {
                    SourceTag::Jpeg
                } else {
                    SourceTag::Raw
                }
            })
            .collect();
        (frames, kinds)
    }

    /// A RAW file and a JPEG of one name in one folder pair, ignoring case and order; another
    /// folder, another name, two shots of one name minutes apart, a second JPEG or RAW of one
    /// name, and a photograph do not.
    #[test]
    fn organize_a_raw_and_its_jpeg_pair() {
        let t = 1_789_000_000_000;
        let (mut frames, kinds) = frames(&[
            ("/card", "DSC_0007.NEF", Some(t)),
            ("/card", "dsc_0007.jpg", Some(t + 300)),
            ("/card", "DSC_0008.JPG", Some(t + 5000)),
            ("/card", "DSC_0008.NEF", Some(t + 5000)),
            ("/other", "DSC_0009.NEF", Some(t)),
            ("/card", "DSC_0009.JPG", Some(t)),
            ("/card", "DSC_0010.NEF", Some(t)),
            ("/card", "DSC_0010.JPG", Some(t + 600_000)),
            ("/card", "DJI_0001.DNG", None),
            ("/card", "DJI_0001.JPG", Some(t)),
            ("/card", "DJI_0001.jpg", Some(t)),
        ]);
        let kind = |frame: &FrameFacts| match frame.item {
            ViewItem::File(FileId(id)) => Some(kinds[id as usize - 1]),
            _ => None,
        };
        assert_eq!(pairs(&frames, kind), [(0, 1), (3, 2), (8, 9)]);
        frames[0].item = ViewItem::Photo(crate::catalog_types::AssetRowId(1));
        assert_eq!(pairs(&frames, kind), [(3, 2), (8, 9)], "a photograph");
        assert!(pairs(&frames, |_| Some(SourceTag::Jpeg)).is_empty());
    }
}
