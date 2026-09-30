//! The loupe's view model ([catalog design](../../../../docs/design/catalog.md#browsing-at-speed),
//! TASK-020): the active frame of Select's view fitted to the screen, its moment's numbered frames
//! under it, the 100% focus check and compare. Like every view model it names no framework type.
//!
//! **Seam.** This file, `app/loupe.rs`, `app/message/loupe.rs` and `view/loupe.rs` are the loupe's
//! own modules. Select hands the loupe what it needs through [`subject`]: the view's revision and
//! count, the active item's position and the moment it sits in. The loupe is entered from Select's
//! grid (`Space` or `E`) and left with `Esc`; the keymap sends the `LoupeMessage`s of
//! `app/message/loupe.rs` for those, and Select's centre draws `view/loupe.rs` in place of the grid
//! while [`LoupeState::open`] is set. Nothing here opens it yet: the loupe's task builds its
//! behaviour on these hooks.
use luxforge_core::catalog_types::{BrowseSession, ViewSummary};

/// The loupe's own state in the desktop: whether it is open over Select's centre. It is this
/// desktop's view state, like the grid's collapsed bursts: no other client sees it. The loupe's
/// task adds what it holds (its decoded frames and look-ahead, the 100% region, compare).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LoupeState {
    pub(crate) open: bool,
}

/// What the loupe shows. The loupe's task adds its regions' models: the info bar, the frame strip,
/// the 100% inset, compare and the key hints.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LoupeModel {
    pub(crate) open: bool,
    /// The frame to show and where it sits; none without a view or an active item.
    pub(crate) subject: Option<LoupeSubject>,
}

/// The loupe's frame and where it sits: the active item's `position` in the view at `revision`
/// (`count` items), and the burst or bracket it belongs to, if any. Positions are the view's,
/// as `browse.rows` and `browse.select` name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LoupeSubject {
    pub(crate) revision: u64,
    pub(crate) position: u32,
    pub(crate) count: u32,
    pub(crate) moment: Option<MomentSpan>,
}

/// A moment of the view: its index in the summary's `groups.moments` and its frames
/// `start..start + len`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MomentSpan {
    pub(crate) index: u32,
    pub(crate) start: u32,
    pub(crate) len: u32,
}

/// The loupe's subject: the session's active item in the view the summary describes, when the
/// session's selection belongs to that view's revision. The moment is found by a binary search of
/// the summary's moments, which are ordered and disjoint.
pub(crate) fn subject(
    summary: Option<&ViewSummary>,
    browse: &BrowseSession,
) -> Option<LoupeSubject> {
    let summary = summary?;
    if browse.revision != summary.revision {
        return None;
    }
    let position = browse.selection.active?;
    if position >= summary.count {
        return None;
    }
    let moments = &summary.groups.moments;
    let after = moments.partition_point(|moment| moment.start <= position);
    let moment = after
        .checked_sub(1)
        .map(|index| (index, &moments[index]))
        .filter(|(_, moment)| position < moment.start + moment.len)
        .map(|(index, moment)| MomentSpan {
            index: index as u32,
            start: moment.start,
            len: moment.len,
        });
    Some(LoupeSubject {
        revision: summary.revision,
        position,
        count: summary.count,
        moment,
    })
}

/// The loupe's model from Select's summary, the session's browse state and the loupe's own state.
pub(crate) fn derive(
    summary: Option<&ViewSummary>,
    browse: &BrowseSession,
    loupe: &LoupeState,
) -> LoupeModel {
    LoupeModel {
        open: loupe.open,
        subject: subject(summary, browse),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::catalog_types::{
        GroupLayout, Moment, MomentKind, ViewQuery, ViewSelection, ViewSource,
    };

    fn summary(revision: u64, count: u32, moments: &[(u32, u32)]) -> ViewSummary {
        ViewSummary {
            revision,
            query: ViewQuery::of(ViewSource::AllPhotographs),
            count,
            picked: 0,
            in_catalog: 0,
            unavailable: 0,
            groups: GroupLayout {
                moments: moments
                    .iter()
                    .map(|&(start, len)| Moment {
                        kind: MomentKind::Burst,
                        evidence: None,
                        steps_ev: Vec::new(),
                        span_ms: 0,
                        start,
                        len,
                    })
                    .collect(),
                ..GroupLayout::default()
            },
            library_sequence: luxforge_core::catalog_types::LibraryChangeSeq(0),
            index_revision: 0,
        }
    }

    fn browse(revision: u64, active: Option<u32>) -> BrowseSession {
        BrowseSession {
            revision,
            count: 12,
            selection: ViewSelection {
                active,
                ..ViewSelection::default()
            },
            ..BrowseSession::default()
        }
    }

    /// The subject is the active item of the view on screen, in its moment when it has one, and
    /// nothing without a view, an active item, or when the session's selection is another view's.
    #[test]
    fn loupe_subject_is_the_active_item_in_its_moment() {
        let view = summary(3, 12, &[(2, 3), (7, 4)]);
        let at = |active| subject(Some(&view), &browse(3, Some(active)));
        assert_eq!(at(0).unwrap().moment, None, "a single before any moment");
        let burst = at(3).unwrap();
        assert_eq!(
            burst.moment,
            Some(MomentSpan {
                index: 0,
                start: 2,
                len: 3
            })
        );
        assert_eq!(at(5).unwrap().moment, None, "a single between moments");
        assert_eq!(at(10).unwrap().moment.unwrap().index, 1);
        assert_eq!(at(11).unwrap().count, 12);
        assert_eq!(subject(Some(&view), &browse(3, None)), None);
        assert_eq!(subject(Some(&view), &browse(2, Some(3))), None);
        assert_eq!(subject(Some(&view), &browse(3, Some(12))), None);
        assert_eq!(subject(None, &browse(3, Some(3))), None);
        let model = derive(Some(&view), &browse(3, Some(3)), &LoupeState { open: true });
        assert!(model.open);
        assert_eq!(model.subject, Some(burst));
    }
}
