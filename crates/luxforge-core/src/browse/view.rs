//! Evaluating a [`ViewQuery`] into the ordered item list the owner holds for a client
//! ([`View`]), with its group layout and the counts its summary reports. The steps are the
//! module's ([`super`]).

use super::{
    EventCache,
    candidates::{self, Candidates, Needs, Reader},
    caseless, current_stamp,
    filter::Filter,
    stacks::{self, Arranged, StackExpansions},
};
use crate::{
    EditorService, Error,
    catalog_types::{
        FileAvailability, FrameFacts, GroupLayout, Grouping, Moment, MomentProbe, RowCopy, RowPair,
        RowStack, SortKey, Thresholds, ViewFilter, ViewItem, ViewQuery, ViewSelection, ViewSort,
        ViewSource, ViewStamp,
    },
    organize,
};
use std::{
    cmp::{Ordering, Reverse},
    collections::{HashMap, HashSet},
    path::Path,
};

/// One client's evaluated view as the owner holds it: its items in order, 16 bytes each, its
/// group layout, which a window reads moments from, and its selection, which the client's session
/// reports.
#[derive(Clone, Debug)]
pub(crate) struct View {
    pub revision: u64,
    pub items: Vec<ViewItem>,
    pub layout: GroupLayout,
    pub selection: ViewSelection,
    /// A later library change or index revision exists: the view still answers for its items,
    /// and the client evaluates it again.
    pub stale: bool,
    /// The originals collapsed HDR stacks hide and the copies other rows stand for: in no
    /// position, so no selection names them, but items of the view a Develop takes the picks of.
    pub hidden: Vec<ViewItem>,
    /// Each stacked row's badge, by item.
    pub badges: HashMap<ViewItem, RowStack>,
    /// The JPEG each RAW+JPEG pair's RAW row stands for, by the RAW file's item.
    pub pairs: HashMap<ViewItem, RowPair>,
    /// The copies in other folders each row stands for, by the row's item.
    pub copies: HashMap<ViewItem, Vec<RowCopy>>,
}

/// What evaluating a query found.
#[derive(Debug)]
pub(crate) struct Evaluation {
    pub items: Vec<ViewItem>,
    pub layout: GroupLayout,
    /// Picked files; 0 over photographs.
    pub picked: u32,
    /// Files already a developed photograph's original; over photographs, every item.
    pub in_catalog: u32,
    /// Items whose original is not available.
    pub unavailable: u32,
    pub stamp: ViewStamp,
    /// How the view's HDR stacks were arranged.
    pub stacks: Arranged,
    /// The JPEG each RAW+JPEG pair's RAW row stands for, by the RAW file's item; the JPEG is in
    /// no position.
    pub pairs: HashMap<ViewItem, RowPair>,
    /// The copies in other folders each row stands for, by the row's item, and every such copy,
    /// in no position.
    pub copies: HashMap<ViewItem, Vec<RowCopy>>,
    pub copied: Vec<ViewItem>,
}

/// What an evaluation reads with: the service, the event cache an event source resolves through,
/// the preview-brightness bracket check, and the most items a source may hold
/// ([`MAX_VIEW_ITEMS`](crate::catalog_types::MAX_VIEW_ITEMS) on the owner).
pub(crate) struct Context<'a> {
    pub service: &'a EditorService,
    pub events: &'a mut EventCache,
    pub probe: Probe<'a>,
    pub limit: usize,
}

/// The bracket check a view groups files with, for the runs their metadata cannot classify.
#[derive(Clone, Copy)]
pub(crate) enum Probe<'a> {
    /// The preview lane's brightness check over the index ([`crate::previews::bracket_probe`]),
    /// made for each grouping so the index is borrowed only while organizing asks about runs; it
    /// reads a run's fingerprints only when asked about that run. Photographs group without it.
    Previews,
    /// A check of the caller's: the tests' and a model's.
    #[cfg_attr(not(test), allow(dead_code))]
    Given(&'a dyn MomentProbe),
}

impl Probe<'_> {
    /// Run `group` with this check: the preview lane's over the index for a view of files, none
    /// for photographs, or the one given.
    pub(crate) fn with<T>(
        self,
        service: &EditorService,
        over_files: bool,
        group: impl FnOnce(&dyn MomentProbe) -> T,
    ) -> Result<T, Error> {
        match self {
            Self::Given(probe) => Ok(group(probe)),
            Self::Previews if over_files => {
                let index = service.index()?;
                let probe = crate::previews::bracket_probe(index.connection());
                Ok(group(&probe))
            }
            Self::Previews => Ok(group(&crate::catalog_types::NoProbe)),
        }
    }
}

/// Evaluate `query` with every HDR stack collapsed. Refused with `validation` for a query no view
/// can answer and a source that does not exist, and with `resource-limit` for a source past the
/// context's limit.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the owner evaluates with its clients' stacks")
)]
pub(crate) fn evaluate(cx: Context<'_>, query: &ViewQuery) -> Result<Evaluation, Error> {
    evaluate_with(cx, query, &StackExpansions::new())
}

/// Evaluate `query`, showing each HDR stack as `expansions` says (`super::stacks`).
pub(crate) fn evaluate_with(
    cx: Context<'_>,
    query: &ViewQuery,
    expansions: &StackExpansions,
) -> Result<Evaluation, Error> {
    query.validate()?;
    // Stamped before anything is read, so a change made while the view is read marks it stale.
    let stamp = current_stamp(cx.service)?;
    let needs = Needs::of(&query.filter, query.sort.key);
    let mut candidates = prepare(
        &mut Reader {
            service: cx.service,
            events: cx.events,
            index_revision: stamp.index_revision,
            thresholds: &query.thresholds,
            limit: cx.limit,
        },
        &query.source,
        &query.filter,
        needs,
        cx.probe,
    )?;
    let filter = Filter::new(&query.filter, &candidates);
    let mut facts = std::mem::take(&mut candidates.facts);
    // Every stacked item's facts before filtering: Show all brings back the originals the filter
    // leaves out.
    let stacked: HashMap<ViewItem, FrameFacts> = if candidates.stacks.is_empty() {
        HashMap::new()
    } else {
        facts
            .iter()
            .filter(|fact| {
                candidates.stacks.member(fact.item).is_some() || candidates.stacks.leads(fact.item)
            })
            .map(|fact| (fact.item, fact.clone()))
            .collect()
    };
    facts.retain(|fact| filter.failing(fact, candidates.extra(fact.item)) == 0);
    let (copies, copied) = collapse(&mut facts, &candidates);
    let pairs = pair(&mut facts, &candidates);
    order(&mut facts, &candidates, query);
    let (stacks, provenance) = stacks::arrange(
        &mut facts,
        &candidates,
        &stacked,
        expansions,
        query.filter != ViewFilter::default(),
    );
    candidates.push_inputs(provenance);
    let mut layout = cx.probe.with(cx.service, candidates.over_files, |probe| {
        organize::group(
            &facts,
            &candidates.names.tables,
            query.effective_grouping(),
            &query.thresholds,
            probe,
        )
    })?;
    count_picks(&mut layout, |at| candidates.extra(facts[at].item).picked);
    let (mut picked, mut in_catalog, mut unavailable) = (0, 0, 0);
    for fact in &facts {
        let extra = candidates.extra(fact.item);
        match fact.item {
            // Picked counts the files a Develop takes; a photograph is never picked.
            ViewItem::File(_) => picked += u32::from(extra.picked),
            ViewItem::Photo(_) => {}
            // A provenance row is no item of the source.
            ViewItem::Input(_) => continue,
        }
        in_catalog += u32::from(extra.in_catalog);
        unavailable += u32::from(extra.availability != FileAvailability::Available);
    }
    // The originals a collapsed stack hides and the copies a row stands for are the view's files
    // too: a Develop takes their picks.
    for item in stacks.hidden.iter().chain(&copied) {
        if let ViewItem::File(_) = item {
            picked += u32::from(candidates.extra(*item).picked);
        }
    }
    Ok(Evaluation {
        items: facts.iter().map(|fact| fact.item).collect(),
        layout,
        picked,
        in_catalog,
        unavailable,
        stamp,
        stacks,
        pairs,
        copies,
        copied,
    })
}

/// Each set of copies among `facts` (`organize::copies`) as the index of the copy a view shows and
/// the indices of the others: the copy a RAW+JPEG pair holds, so the pair stays whole, else a
/// picked one, else the first read. A file in an HDR stack is no copy here and keeps its own row.
fn copy_sets(facts: &[FrameFacts], candidates: &Candidates) -> Vec<(usize, Vec<usize>)> {
    if !candidates.over_files {
        return Vec::new();
    }
    let stacked =
        |item: ViewItem| candidates.stacks.member(item).is_some() || candidates.stacks.leads(item);
    let sets = organize::copies(facts, |fact| {
        if stacked(fact.item) {
            None
        } else {
            candidates.extra(fact.item).len
        }
    });
    if sets.is_empty() {
        return Vec::new();
    }
    let paired: HashSet<usize> =
        organize::pairs(facts, |fact| Some(candidates.extra(fact.item).kind))
            .into_iter()
            .flat_map(|(raw, jpeg)| [raw, jpeg])
            .collect();
    sets.into_iter()
        .map(|set| {
            let keep = set
                .iter()
                .copied()
                .max_by_key(|&at| {
                    let picked = candidates.extra(facts[at].item).picked;
                    (paired.contains(&at), picked, Reverse(at))
                })
                .expect("a set of copies holds two files");
            let others = set.into_iter().filter(|&at| at != keep).collect();
            (keep, others)
        })
        .collect()
}

/// Show each set of copies the filter left as one row (`copy_sets`): take the other copies out of
/// `facts` before pairs are found and frames grouped, so a copy never splits a burst, a bracket or
/// a panorama. Answers the copies each row stands for, in path order, and every copy taken out.
fn collapse(
    facts: &mut Vec<FrameFacts>,
    candidates: &Candidates,
) -> (HashMap<ViewItem, Vec<RowCopy>>, Vec<ViewItem>) {
    let sets = copy_sets(facts, candidates);
    if sets.is_empty() {
        return (HashMap::new(), Vec::new());
    }
    let mut copies = HashMap::with_capacity(sets.len());
    let mut copied = Vec::new();
    for (keep, others) in sets {
        let mut rows: Vec<RowCopy> = others
            .iter()
            .filter_map(|&at| {
                let fact = &facts[at];
                let ViewItem::File(file_id) = fact.item else {
                    return None;
                };
                copied.push(fact.item);
                let folder = &candidates.names.folders[fact.folder.0 as usize];
                Some(RowCopy {
                    file_id,
                    path: Path::new(folder).join(&*fact.name),
                })
            })
            .collect();
        rows.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        copies.insert(facts[keep].item, rows);
    }
    let gone: HashSet<ViewItem> = copied.iter().copied().collect();
    facts.retain(|fact| !gone.contains(&fact.item));
    (copies, copied)
}

/// Show each RAW+JPEG pair the filter left whole (`organize::pairs`) as its RAW file's row: take
/// its JPEG out of `facts`, unless it is picked (a pick is never hidden) or in an HDR or panorama
/// stack, and answer the JPEG of each RAW row that stands for one. A filter that leaves the RAW
/// file out, as JPEG only does, leaves the JPEG a row of its own.
fn pair(facts: &mut Vec<FrameFacts>, candidates: &Candidates) -> HashMap<ViewItem, RowPair> {
    if !candidates.over_files {
        return HashMap::new();
    }
    let found = organize::pairs(facts, |fact| Some(candidates.extra(fact.item).kind));
    let mut hidden = HashSet::new();
    let mut pairs = HashMap::new();
    for (raw, jpeg) in found {
        let jpeg = &facts[jpeg];
        let ViewItem::File(file_id) = jpeg.item else {
            continue;
        };
        if candidates.extra(jpeg.item).picked
            || candidates.stacks.member(jpeg.item).is_some()
            || candidates.stacks.leads(jpeg.item)
        {
            continue;
        }
        hidden.insert(jpeg.item);
        pairs.insert(
            facts[raw].item,
            RowPair {
                file_id,
                file_name: jpeg.name.to_string(),
            },
        );
    }
    if !hidden.is_empty() {
        facts.retain(|fact| !hidden.contains(&fact.item));
    }
    pairs
}

/// Fill each day's and each moment's pick count from whether the ordered view's frame at each
/// position is picked. Days partition the view and moments are disjoint, so each frame is asked
/// about at most twice.
pub(super) fn count_picks(layout: &mut GroupLayout, picked: impl Fn(usize) -> bool) {
    let count = |start: u32, len: u32| {
        (start as usize..start as usize + len as usize)
            .filter(|&at| picked(at))
            .count() as u32
    };
    for day in &mut layout.days {
        day.picked = count(day.start, day.len);
    }
    for moment in &mut layout.moments {
        moment.picked = count(moment.start, moment.len);
    }
}

/// Read a source's items and decide, when the filter asks for Moments without a pick, which of
/// them it leaves out.
pub(super) fn prepare(
    reader: &mut Reader<'_>,
    source: &ViewSource,
    filter: &crate::catalog_types::ViewFilter,
    needs: Needs,
    probe: Probe<'_>,
) -> Result<Candidates, Error> {
    let mut candidates = candidates::read(reader, source, needs)?;
    if filter.without_pick && candidates.over_files {
        let service = reader.service;
        probe.with(service, true, |probe| {
            decide(&mut candidates, reader.thresholds, probe)
        })?;
    }
    Ok(candidates)
}

/// Mark the frames Moments without a pick leaves out: the source's frames are ordered and grouped
/// Day › Camera › Moment under `thresholds`, whatever the view's own sort and grouping, and a frame
/// is decided when it is picked or its moment has a picked frame.
///
/// A bracket whose stack has an active result is decided: its originals count as picked here, so
/// their moment is decided whole. The result, which only an active stack brings into a view over
/// files, is not one of the frames grouped, and is decided with its bracket. A RAW+JPEG pair is
/// one frame here, its RAW file's, picked when either file is; its JPEG is decided with it. A set
/// of copies is one frame too, the copy a view shows, picked when any copy is; the other copies
/// are decided with it.
fn decide(candidates: &mut Candidates, thresholds: &Thresholds, probe: &dyn MomentProbe) {
    let mut facts = std::mem::take(&mut candidates.facts);
    let results: Vec<FrameFacts> = if candidates.stacks.is_empty() {
        Vec::new()
    } else {
        let (files, results) = facts
            .into_iter()
            .partition(|fact| matches!(fact.item, ViewItem::File(_)));
        facts = files;
        results
    };
    // Each set of copies as the copy a view shows; the others are taken out of the frames grouped,
    // and a copy's pick is the shown copy's.
    let sets: Vec<(ViewItem, Vec<ViewItem>)> = copy_sets(&facts, candidates)
        .into_iter()
        .map(|(keep, others)| {
            let others = others.into_iter().map(|at| facts[at].item).collect();
            (facts[keep].item, others)
        })
        .collect();
    let copied: HashSet<ViewItem> = sets
        .iter()
        .flat_map(|(_, others)| others.iter().copied())
        .collect();
    let copy_picked: HashSet<ViewItem> = sets
        .iter()
        .filter(|(_, others)| others.iter().any(|item| candidates.extra(*item).picked))
        .map(|(keep, _)| *keep)
        .collect();
    let picked_any = |item: ViewItem| candidates.extra(item).picked || copy_picked.contains(&item);
    // Each pair's JPEG by its RAW file, taken out of the frames grouped.
    let paired: HashMap<ViewItem, ViewItem> =
        organize::pairs(&facts, |fact| Some(candidates.extra(fact.item).kind))
            .into_iter()
            .map(|(raw, jpeg)| (facts[raw].item, facts[jpeg].item))
            .collect();
    let jpegs: HashSet<ViewItem> = paired.values().copied().collect();
    let (mut partners, files): (Vec<FrameFacts>, Vec<FrameFacts>) = facts
        .into_iter()
        .partition(|fact| jpegs.contains(&fact.item) || copied.contains(&fact.item));
    facts = files;
    organize::order(
        &mut facts,
        &candidates.names.tables,
        Grouping::DayCameraMoment,
        false,
    );
    let layout = organize::group(
        &facts,
        &candidates.names.tables,
        Grouping::DayCameraMoment,
        thresholds,
        probe,
    );
    let picked: Vec<bool> = facts
        .iter()
        .map(|fact| {
            picked_any(fact.item)
                || paired.get(&fact.item).is_some_and(|jpeg| picked_any(*jpeg))
                || candidates.stacks.decided_by_result(fact.item)
        })
        .collect();
    for (fact, decided) in facts.iter().zip(decided(&picked, &layout.moments)) {
        candidates.extra_mut(fact.item).decided = decided;
        if let Some(jpeg) = paired.get(&fact.item) {
            candidates.extra_mut(*jpeg).decided = decided;
        }
    }
    for (keep, others) in &sets {
        let decided = candidates.extra(*keep).decided;
        for item in others {
            candidates.extra_mut(*item).decided = decided;
        }
    }
    for result in &results {
        candidates.extra_mut(result.item).decided = true;
    }
    facts.append(&mut partners);
    facts.extend(results);
    candidates.facts = facts;
}

/// Which frames of an ordered view are decided, given which are picked and its moments: a picked
/// frame, and every frame of a moment with a picked frame.
pub(super) fn decided(picked: &[bool], moments: &[Moment]) -> Vec<bool> {
    let mut decided = picked.to_vec();
    for moment in moments {
        let frames = moment.start as usize..(moment.start + moment.len) as usize;
        if picked[frames.clone()].iter().any(|picked| *picked) {
            decided[frames].fill(true);
        }
    }
    decided
}

/// Put `facts` in the query's order.
fn order(facts: &mut [FrameFacts], candidates: &Candidates, query: &ViewQuery) {
    match query.sort.key {
        SortKey::CaptureTime => organize::order(
            facts,
            &candidates.names.tables,
            query.effective_grouping(),
            query.sort.descending,
        ),
        _ => facts.sort_unstable_by(|a, b| compare(query.sort, candidates, a, b)),
    }
}

/// Two items under a sort other than capture time: its key, reversed when descending, then the
/// declared tie-breaks, always ascending — capture time with undated last, the file name ignoring
/// case, then the path (folder, then name) for files and the row for photographs.
fn compare(sort: ViewSort, candidates: &Candidates, a: &FrameFacts, b: &FrameFacts) -> Ordering {
    let (x, y) = (candidates.extra(a.item), candidates.extra(b.item));
    let key = match sort.key {
        SortKey::FileName => caseless(&a.name, &b.name),
        SortKey::DateDeveloped => x.developed_ms.cmp(&y.developed_ms),
        SortKey::LastEdited => x.last_edited_ms.cmp(&y.last_edited_ms),
        SortKey::CaptureTime => Ordering::Equal,
    };
    let key = if sort.descending { key.reverse() } else { key };
    key.then_with(|| match (a.instant_ms, b.instant_ms) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    })
    .then_with(|| caseless(&a.name, &b.name))
    .then_with(|| {
        if candidates.over_files {
            let tables = &candidates.names.tables;
            tables
                .folder_path(a.folder)
                .cmp(tables.folder_path(b.folder))
                .then_with(|| a.name.cmp(&b.name))
        } else {
            Ordering::Equal
        }
    })
    .then_with(|| a.item.cmp(&b.item))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_types::MomentKind;

    fn moment(start: u32, len: u32) -> Moment {
        Moment {
            kind: MomentKind::Burst,
            evidence: None,
            steps_ev: vec![],
            span_ms: 0,
            start,
            len,
            picked: 0,
        }
    }

    /// A moment with a pick is decided whole; a moment without one and an unpicked single are
    /// left to decide.
    #[test]
    fn browse_a_moment_with_a_pick_is_decided_whole() {
        let picked = [false, false, true, false, false, false, true, false];
        let moments = [moment(0, 4), moment(4, 2)];
        assert_eq!(
            decided(&picked, &moments),
            [true, true, true, true, false, false, true, false]
        );
        assert_eq!(decided(&[], &[]), Vec::<bool>::new());
    }

    /// Each day and moment counts the picked frames it covers, and nothing outside it.
    #[test]
    fn browse_days_and_moments_count_their_picks() {
        let picked = [false, true, true, false, true, false, false, true];
        let day = |start, len| crate::catalog_types::DayGroup {
            day: None,
            start,
            len,
            picked: 99,
        };
        let mut layout = GroupLayout {
            days: vec![day(0, 5), day(5, 3)],
            cameras: Vec::new(),
            moments: vec![moment(0, 3), moment(3, 2), moment(5, 2)],
        };
        count_picks(&mut layout, |at| picked[at]);
        let days: Vec<u32> = layout.days.iter().map(|day| day.picked).collect();
        let moments: Vec<u32> = layout.moments.iter().map(|moment| moment.picked).collect();
        assert_eq!(days, [3, 1]);
        assert_eq!(moments, [2, 1, 0]);
    }
}
