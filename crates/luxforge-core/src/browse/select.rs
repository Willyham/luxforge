//! The selection in a client's view (`browse.select`): disjoint, ascending, merged ranges of
//! positions and the active item, as `session.state` reports them. Changing it costs the ranges
//! involved, never the view — replace, add, remove and toggle are merges of two range lists — except
//! naming items, which finds them in one pass over the view. It is session state: nothing is
//! written. A view evaluated again carries the selection over by item ([`carry_over`]).

use super::{View, json_list};
use crate::{
    EditorService, Error,
    catalog_types::{
        AssetRowId, ItemRef, MAX_LIBRARY_BATCH, PositionRange, SelectionMode, ViewItem,
        ViewSelection,
    },
};
use std::collections::HashMap;

/// A `browse.select` request. Without `items`, `range` or `all` the selection is unchanged and only
/// `active` moves; with any of them, their union is what `mode` applies (`all: false` names
/// nothing, so `{all: false}` selects none).
#[derive(Clone, Debug, Default)]
pub(crate) struct SelectRequest {
    pub mode: SelectionMode,
    pub items: Option<Vec<ItemRef>>,
    pub range: Option<PositionRange>,
    pub all: Option<bool>,
    pub active: Option<u32>,
}

/// Half-open spans of positions, `u64` so no end overflows.
type Span = (u64, u64);

/// Apply `request` to `selection` in `view`. Refused with `validation` for a range or active
/// position past the view's end, an item not in the view, and more than
/// [`MAX_LIBRARY_BATCH`] items; nothing changes when it is refused.
pub(crate) fn select(
    service: &EditorService,
    view: &View,
    selection: &mut ViewSelection,
    request: SelectRequest,
) -> Result<(), Error> {
    let len = view.items.len() as u64;
    if let Some(active) = request.active
        && u64::from(active) >= len
    {
        return Err(Error::validation(format!(
            "active position {active} is past the view's {len} items"
        )));
    }
    let named = request.items.is_some() || request.range.is_some() || request.all.is_some();
    if named {
        let mut given: Vec<Span> = Vec::new();
        if let Some(range) = request.range {
            let end = u64::from(range.start) + u64::from(range.len);
            if end > len {
                return Err(Error::validation(format!(
                    "range {}..{end} is past the view's {len} items",
                    range.start
                )));
            }
            given.push((u64::from(range.start), end));
        }
        if request.all == Some(true) {
            given.push((0, len));
        }
        if let Some(items) = &request.items {
            given.extend(
                positions(service, view, items)?
                    .into_iter()
                    .map(|position| (u64::from(position), u64::from(position) + 1)),
            );
        }
        let given = normalize(given);
        let current = spans(selection);
        let next = match request.mode {
            SelectionMode::Replace => given,
            SelectionMode::Add => union(&current, &given),
            SelectionMode::Remove => difference(&current, &given),
            SelectionMode::Toggle => {
                union(&difference(&current, &given), &difference(&given, &current))
            }
        };
        set(selection, &next);
    }
    if request.active.is_some() {
        selection.active = request.active;
    }
    Ok(())
}

/// The items selected in `view`, in view order.
pub(crate) fn selected_items(view: &View) -> Vec<ViewItem> {
    let mut items = Vec::with_capacity(view.selection.count as usize);
    for range in &view.selection.ranges {
        let start = range.start as usize;
        let end = (start + range.len as usize).min(view.items.len());
        items.extend_from_slice(&view.items[start.min(end)..end]);
    }
    items
}

/// The selection `old` made in `old_items`, carried to `new_items` by item: each selected item
/// still in the view keeps its selection at its new position, and the active item stays active
/// where it now is. One pass over the new view, a binary search each against the selected items.
pub(crate) fn carry_over(
    old_items: &[ViewItem],
    old: &ViewSelection,
    new_items: &[ViewItem],
) -> ViewSelection {
    let mut chosen: Vec<ViewItem> = Vec::with_capacity(old.count as usize);
    for range in &old.ranges {
        let start = (range.start as usize).min(old_items.len());
        let end = (start + range.len as usize).min(old_items.len());
        chosen.extend_from_slice(&old_items[start..end]);
    }
    chosen.sort_unstable();
    let active = old
        .active
        .and_then(|position| old_items.get(position as usize).copied());
    let mut next: Vec<Span> = Vec::new();
    let mut moved_active = None;
    if !chosen.is_empty() || active.is_some() {
        for (position, item) in new_items.iter().enumerate() {
            if Some(*item) == active {
                moved_active = Some(position as u32);
            }
            if chosen.binary_search(item).is_ok() {
                let position = position as u64;
                match next.last_mut() {
                    Some(last) if last.1 == position => last.1 += 1,
                    _ => next.push((position, position + 1)),
                }
            }
        }
    }
    let mut selection = ViewSelection {
        active: moved_active,
        ..ViewSelection::default()
    };
    set(&mut selection, &next);
    selection
}

/// Where `items` are in `view`, found in one pass; refused naming the first that is not there.
fn positions(service: &EditorService, view: &View, items: &[ItemRef]) -> Result<Vec<u32>, Error> {
    if items.len() > MAX_LIBRARY_BATCH {
        return Err(Error::validation(format!(
            "{} items exceed the {MAX_LIBRARY_BATCH} one selection request names",
            items.len()
        )));
    }
    let photos: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            ItemRef::Photo { asset_id } => Some(asset_id.as_str()),
            ItemRef::File { .. } => None,
        })
        .collect();
    let mut rows: HashMap<String, i64> = HashMap::new();
    if !photos.is_empty() {
        let mut statement = service.connection.prepare_cached(
            "SELECT id, row_id FROM assets WHERE id IN (SELECT value FROM json_each(?1))",
        )?;
        for row in
            statement.query_map([json_list(&photos)?], |row| Ok((row.get(0)?, row.get(1)?)))?
        {
            let (id, row_id) = row?;
            rows.insert(id, row_id);
        }
    }
    let unknown = |item: &ItemRef| {
        Error::validation(match item {
            ItemRef::File { file_id } => format!("file {} is not in the view", file_id.0),
            ItemRef::Photo { asset_id } => format!("{asset_id} is not in the view"),
        })
    };
    let mut wanted: HashMap<ViewItem, Option<u32>> = HashMap::with_capacity(items.len());
    let mut order = Vec::with_capacity(items.len());
    for item in items {
        let key = match item {
            ItemRef::File { file_id } => ViewItem::File(*file_id),
            ItemRef::Photo { asset_id } => ViewItem::Photo(AssetRowId(
                *rows.get(asset_id.as_str()).ok_or_else(|| unknown(item))?,
            )),
        };
        wanted.insert(key, None);
        order.push((key, item));
    }
    let mut left = wanted.len();
    for (position, item) in view.items.iter().enumerate() {
        if left == 0 {
            break;
        }
        if let Some(slot) = wanted.get_mut(item)
            && slot.is_none()
        {
            *slot = Some(position as u32);
            left -= 1;
        }
    }
    order
        .into_iter()
        .map(|(key, item)| wanted[&key].ok_or_else(|| unknown(item)))
        .collect()
}

fn spans(selection: &ViewSelection) -> Vec<Span> {
    selection
        .ranges
        .iter()
        .map(|range| {
            (
                u64::from(range.start),
                u64::from(range.start) + u64::from(range.len),
            )
        })
        .collect()
}

fn set(selection: &mut ViewSelection, spans: &[Span]) {
    selection.ranges = spans
        .iter()
        .map(|(start, end)| PositionRange {
            start: *start as u32,
            len: (end - start) as u32,
        })
        .collect();
    selection.count = spans.iter().map(|(start, end)| end - start).sum::<u64>() as u32;
}

/// Spans sorted, with empty ones dropped and overlapping or touching ones merged.
fn normalize(mut spans: Vec<Span>) -> Vec<Span> {
    spans.retain(|(start, end)| start < end);
    spans.sort_unstable();
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

fn union(a: &[Span], b: &[Span]) -> Vec<Span> {
    normalize(a.iter().chain(b).copied().collect())
}

/// The positions of `a` not in `b`, both normalized.
fn difference(a: &[Span], b: &[Span]) -> Vec<Span> {
    let mut out = Vec::with_capacity(a.len());
    let mut cut = b.iter().peekable();
    for &(start, end) in a {
        let mut start = start;
        while let Some(&&(cut_start, cut_end)) = cut.peek() {
            if cut_end <= start {
                cut.next();
                continue;
            }
            if cut_start >= end {
                break;
            }
            if cut_start > start {
                out.push((start, cut_start));
            }
            start = start.max(cut_end);
            if cut_end >= end {
                break;
            }
            cut.next();
        }
        if start < end {
            out.push((start, end));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The range algebra against sets of positions.
    #[test]
    fn browse_selection_ranges_follow_set_algebra() {
        let set_of = |spans: &[Span]| -> std::collections::BTreeSet<u64> {
            spans.iter().flat_map(|(start, end)| *start..*end).collect()
        };
        let cases: [(&[Span], &[Span]); 5] = [
            (&[(0, 5), (8, 10)], &[(3, 9)]),
            (&[(2, 4)], &[(0, 2), (4, 6)]),
            (&[], &[(1, 3)]),
            (&[(0, 10)], &[(2, 3), (5, 6), (9, 12)]),
            (&[(1, 2), (3, 4), (5, 6)], &[(0, 7)]),
        ];
        for (a, b) in cases {
            let (sa, sb) = (set_of(a), set_of(b));
            assert_eq!(set_of(&union(a, b)), &sa | &sb, "{a:?} ∪ {b:?}");
            assert_eq!(set_of(&difference(a, b)), &sa - &sb, "{a:?} − {b:?}");
            let merged = union(a, b);
            assert!(
                merged.windows(2).all(|pair| pair[0].1 < pair[1].0),
                "disjoint, ascending and merged: {merged:?}"
            );
        }
        assert_eq!(
            normalize(vec![(4, 6), (0, 2), (2, 3), (5, 5)]),
            [(0, 3), (4, 6)]
        );
    }

    #[test]
    fn browse_a_selection_carries_over_by_item() {
        let item = |id| ViewItem::File(crate::catalog_types::FileId(id));
        let old: Vec<ViewItem> = (1..=6).map(item).collect();
        let selection = ViewSelection {
            count: 3,
            ranges: vec![
                PositionRange { start: 1, len: 2 },
                PositionRange { start: 5, len: 1 },
            ],
            active: Some(2),
        };
        // 2 and 3 selected, 6 selected, 3 active; the new view drops 2 and reverses the rest.
        let new: Vec<ViewItem> = [6, 5, 4, 3, 1].map(item).to_vec();
        let carried = carry_over(&old, &selection, &new);
        assert_eq!(carried.count, 2);
        assert_eq!(
            carried.ranges,
            [
                PositionRange { start: 0, len: 1 },
                PositionRange { start: 3, len: 1 }
            ]
        );
        assert_eq!(carried.active, Some(3));
        let view = View {
            revision: 2,
            over_files: true,
            items: new,
            layout: Default::default(),
            selection: carried.clone(),
            stale: false,
        };
        assert_eq!(selected_items(&view), [item(6), item(3)]);
        let gone = carry_over(&old, &selection, &[item(9)]);
        assert_eq!(gone, ViewSelection::default());
    }
}
