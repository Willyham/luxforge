//! Copies: one file found in more than one folder, as a backup or a second import leaves it
//! (`docs/design/catalog.md`, "Copies").
//!
//! Two files are copies when both are dated at the same instant by the same body, with the same
//! name ignoring case and the same length. A view shows each set of copies as one row, so a copy
//! never splits a burst, a bracket or a panorama.
use crate::catalog_types::{BodyIndex, FrameFacts, ViewItem};
use std::collections::HashMap;

/// The sets of copies among `frames`, each two or more indices into them in the frames' order, the
/// sets in the order of their first frames. `len` answers each frame's file length; a frame it
/// answers `None` for, an undated frame, a photograph and a merge's result are copies of nothing.
pub(crate) fn copies(
    frames: &[FrameFacts],
    len: impl Fn(&FrameFacts) -> Option<u64>,
) -> Vec<Vec<usize>> {
    let mut sets: HashMap<(BodyIndex, i64, u64, String), Vec<usize>> = HashMap::new();
    for (at, frame) in frames.iter().enumerate() {
        if !matches!(frame.item, ViewItem::File(_)) || frame.merged {
            continue;
        }
        let (Some(instant), Some(len)) = (frame.instant_ms, len(frame)) else {
            continue;
        };
        sets.entry((frame.body, instant, len, frame.name.to_lowercase()))
            .or_default()
            .push(at);
    }
    let mut found: Vec<Vec<usize>> = sets.into_values().filter(|set| set.len() > 1).collect();
    found.sort_unstable();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SourceTag,
        catalog_types::{
            CameraBody, Exposure, FileId, FrameTables, Grouping, LocalDay, MomentEvidence,
            MomentKind, NoProbe, Thresholds,
        },
        organize::{group, order, pairs},
    };

    /// A DJI Air 2S's frame `name` in `folder` at `second` past a minute, exposed for `time_s` with
    /// `bias_ev`.
    fn drone(
        tables: &mut FrameTables,
        id: i64,
        (folder, name): (&str, &str),
        second: i64,
        (time_s, bias_ev): (f32, f32),
    ) -> FrameFacts {
        let body = CameraBody {
            make: "DJI".into(),
            model: "FC3411".into(),
            serial: Some("42UQJ1W13A02NY".into()),
        };
        let instant = 1_697_610_600_000 + second * 1000;
        FrameFacts {
            merged: false,
            item: ViewItem::File(FileId(id)),
            folder: tables.folder(folder.into()),
            name: name.into(),
            instant_ms: Some(instant),
            local_day: Some(LocalDay(instant.div_euclid(86_400_000) as i32)),
            position: None,
            body: tables.body(Some(&body)),
            exposure: Exposure {
                time_s: Some(time_s),
                f_number: Some(2.8),
                iso: Some(100),
                bias_ev: Some(bias_ev),
                focal_mm: Some(8.38),
                focal_35mm_mm: Some(22.0),
            },
        }
    }

    /// Files of one name, length, instant and body in two folders are copies, ignoring case; a
    /// second body, another length, another instant, an undated file and a photograph are not.
    #[test]
    fn organize_finds_a_files_copies_in_other_folders() {
        let mut tables = FrameTables::default();
        let exposed = (1.0 / 15.0, 0.0);
        let mut frames = vec![
            drone(&mut tables, 1, ("/a", "DJI_0197.DNG"), 4, exposed),
            drone(&mut tables, 2, ("/b", "dji_0197.dng"), 4, exposed),
            drone(&mut tables, 3, ("/c", "DJI_0197.DNG"), 4, exposed),
            drone(&mut tables, 4, ("/d", "DJI_0197.DNG"), 5, exposed),
            drone(&mut tables, 5, ("/e", "DJI_0197.DNG"), 4, exposed),
            drone(&mut tables, 6, ("/f", "DJI_0197.DNG"), 4, exposed),
            drone(&mut tables, 7, ("/g", "DJI_0197.DNG"), 4, exposed),
        ];
        frames[4].body = tables.body(None);
        frames[5].instant_ms = None;
        frames[6].item = ViewItem::Photo(crate::catalog_types::AssetRowId(1));
        let len = |frame: &FrameFacts| match frame.item {
            ViewItem::File(FileId(2)) => Some(40),
            ViewItem::File(FileId(3)) => Some(41),
            _ => Some(40),
        };
        assert_eq!(copies(&frames, len), [vec![0, 1]]);
        assert!(copies(&frames, |_| None).is_empty());
    }

    /// A drone's RAW+JPEG bracket in one folder with a copy of its RAW files in another groups as a
    /// burst of six; shown as its pairs with each set of copies as one frame it is the bracket.
    #[test]
    fn organize_a_paired_bracket_with_copied_raw_files_stays_a_bracket() {
        let mut tables = FrameTables::default();
        let shots = [
            ("DJI_0197", 4, (1.0 / 15.0, 0.0)),
            ("DJI_0198", 5, (1.0 / 25.0, -0.7)),
            ("DJI_0199", 5, (1.0 / 10.0, 0.7)),
        ];
        let mut frames = Vec::new();
        let mut kinds = Vec::new();
        for (folder, extensions) in [
            ("/Bali/Drone", &["DNG", "JPG"][..]),
            ("/Bali/Ubud/Drone", &["DNG"]),
        ] {
            for (stem, second, exposure) in shots {
                for extension in extensions {
                    let id = frames.len() as i64 + 1;
                    let name = format!("{stem}.{extension}");
                    frames.push(drone(&mut tables, id, (folder, &name), second, exposure));
                    kinds.push(if *extension == "JPG" {
                        SourceTag::Jpeg
                    } else {
                        SourceTag::Raw
                    });
                }
            }
        }
        let grouped = |mut frames: Vec<FrameFacts>| {
            order(&mut frames, &tables, Grouping::DayCameraMoment, false);
            let layout = group(
                &frames,
                &tables,
                Grouping::DayCameraMoment,
                &Thresholds::default(),
                &NoProbe,
            );
            layout
                .moments
                .iter()
                .map(|moment| (moment.kind, moment.evidence, moment.len))
                .collect::<Vec<_>>()
        };
        let kind = |frame: &FrameFacts| match frame.item {
            ViewItem::File(FileId(id)) => Some(kinds[id as usize - 1]),
            _ => None,
        };
        let jpegs: Vec<usize> = pairs(&frames, kind).iter().map(|(_, jpeg)| *jpeg).collect();
        assert_eq!(jpegs.len(), 3, "each RAW file in /Bali/Drone pairs");
        let mut shown: Vec<FrameFacts> = frames
            .iter()
            .enumerate()
            .filter(|(at, _)| !jpegs.contains(at))
            .map(|(_, frame)| frame.clone())
            .collect();
        assert_eq!(
            grouped(shown.clone()),
            [(MomentKind::Burst, None, 6)],
            "every RAW file twice"
        );
        let sets = copies(&shown, |_| Some(41_058_716));
        assert_eq!(sets.len(), 3);
        let copied: Vec<usize> = sets.iter().flat_map(|set| set[1..].to_vec()).collect();
        shown = shown
            .into_iter()
            .enumerate()
            .filter(|(at, _)| !copied.contains(at))
            .map(|(_, frame)| frame)
            .collect();
        assert_eq!(
            grouped(shown),
            [(MomentKind::Bracket, Some(MomentEvidence::Metadata), 3)]
        );
    }
}
