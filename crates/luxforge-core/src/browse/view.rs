//! Evaluating a [`ViewQuery`] into the ordered item list the owner holds for a client
//! ([`View`]), with its group layout and the counts its summary reports. The steps are the
//! module's ([`super`]).

use super::{
    EventCache,
    candidates::{self, Candidates, Needs, Reader},
    caseless, current_stamp,
    filter::Filter,
};
use crate::{
    EditorService, Error,
    catalog_types::{
        BracketProbe, FileAvailability, FrameFacts, GroupLayout, Grouping, Moment, SortKey,
        Thresholds, ViewItem, ViewQuery, ViewSelection, ViewSort, ViewSource, ViewStamp,
    },
    organize,
};
use std::cmp::Ordering;

/// One client's evaluated view as the owner holds it: its items in order, 16 bytes each, its
/// group layout, which a window reads moments from, and its selection, which the client's session
/// reports.
#[derive(Clone, Debug)]
pub(crate) struct View {
    pub revision: u64,
    pub over_files: bool,
    pub items: Vec<ViewItem>,
    pub layout: GroupLayout,
    pub selection: ViewSelection,
    /// A later library change or index revision exists: the view still answers for its items,
    /// and the client evaluates it again.
    pub stale: bool,
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
    Given(&'a dyn BracketProbe),
}

impl Probe<'_> {
    /// Run `group` with this check: the preview lane's over the index for a view of files, none
    /// for photographs, or the one given.
    pub(crate) fn with<T>(
        self,
        service: &EditorService,
        over_files: bool,
        group: impl FnOnce(&dyn BracketProbe) -> T,
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

/// Evaluate `query`. Refused with `validation` for a query no view can answer and a source that
/// does not exist, and with `resource-limit` for a source past the context's limit.
pub(crate) fn evaluate(cx: Context<'_>, query: &ViewQuery) -> Result<Evaluation, Error> {
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
    facts.retain(|fact| filter.failing(fact, candidates.extra(fact.item)) == 0);
    order(&mut facts, &candidates, query);
    let layout = cx.probe.with(cx.service, candidates.over_files, |probe| {
        organize::group(
            &facts,
            &candidates.names.tables,
            query.effective_grouping(),
            &query.thresholds,
            probe,
        )
    })?;
    let (mut picked, mut in_catalog, mut unavailable) = (0, 0, 0);
    for fact in &facts {
        let extra = candidates.extra(fact.item);
        picked += u32::from(extra.picked);
        in_catalog += u32::from(extra.in_catalog);
        unavailable += u32::from(extra.availability != FileAvailability::Available);
    }
    Ok(Evaluation {
        items: facts.iter().map(|fact| fact.item).collect(),
        layout,
        picked,
        in_catalog,
        unavailable,
        stamp,
    })
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
fn decide(candidates: &mut Candidates, thresholds: &Thresholds, probe: &dyn BracketProbe) {
    let mut facts = std::mem::take(&mut candidates.facts);
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
        .map(|fact| candidates.extra(fact.item).picked)
        .collect();
    for (fact, decided) in facts.iter().zip(decided(&picked, &layout.moments)) {
        candidates.extra_mut(fact.item).decided = decided;
    }
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
}
