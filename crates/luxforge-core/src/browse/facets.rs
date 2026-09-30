//! Facet counts (`browse.facets`), the metadata browser's columns: for each facet, every value with
//! how many items have it, most frequent first.
//!
//! Every condition of a filter is a predicate of one item (Moments without a pick included, since
//! it is decided over the whole source first), so the view a facet value predicts — the query with
//! that facet's own condition replaced by that one value — holds exactly the items that fail no
//! other condition and have that value. One pass over the source counts every facet asked.

use super::{
    Context,
    candidates::{Needs, Reader},
    current_stamp,
    filter::{self, Filter},
    view::prepare,
};
use crate::{
    Error,
    catalog_types::{
        Facet, FacetValue, Facets, LocalDay, SortKey, Thresholds, ViewFilter, ViewQuery, ViewSource,
    },
};
use std::collections::{BTreeMap, HashMap};

/// The condition a facet replaces.
fn condition(facet: Facet) -> u16 {
    match facet {
        Facet::Date => filter::DATES,
        Facet::Place => filter::PLACES,
        Facet::Camera => filter::CAMERAS,
        Facet::Lens => filter::LENSES,
        Facet::Kind => filter::KINDS,
        Facet::Pick => filter::PICKED,
    }
}

/// Count `asked` over `source` narrowed by `filter`, under the default thresholds. Refused with
/// `validation` for a filter its source's items cannot have and for the pick facet over
/// photographs, and with `resource-limit` for a source past the context's limit. The value absent
/// counts the items that record none (undated, no place, no lens, no camera).
pub(crate) fn facets(
    cx: Context<'_>,
    source: &ViewSource,
    filter: &ViewFilter,
    asked: &[Facet],
) -> Result<Facets, Error> {
    let query = ViewQuery {
        filter: filter.clone(),
        ..ViewQuery::of(source.clone())
    };
    query.validate()?;
    if !source.over_files() && asked.contains(&Facet::Pick) {
        return Err(Error::validation(
            "the pick facet counts files, not photographs",
        ));
    }
    let stamp = current_stamp(cx.service)?;
    let thresholds = Thresholds::default();
    let candidates = prepare(
        &mut Reader {
            service: cx.service,
            events: cx.events,
            index_revision: stamp.index_revision,
            thresholds: &thresholds,
            limit: cx.limit,
        },
        source,
        filter,
        Needs::of(filter, SortKey::CaptureTime),
        cx.probe,
    )?;
    let compiled = Filter::new(filter, &candidates);
    let mut asked: Vec<Facet> = asked.to_vec();
    asked.sort_unstable();
    asked.dedup();
    let mut counts: Vec<HashMap<Option<i64>, u32>> = vec![HashMap::new(); asked.len()];
    for fact in &candidates.facts {
        let extra = candidates.extra(fact.item);
        let failing = compiled.failing(fact, extra);
        for (facet, counts) in asked.iter().zip(&mut counts) {
            if failing & !condition(*facet) != 0 {
                continue;
            }
            let key = match facet {
                Facet::Date => fact.local_day.map(|day| i64::from(day.0)),
                Facet::Place => extra.place.map(i64::from),
                Facet::Camera => candidates
                    .names
                    .tables
                    .camera(fact.body)
                    .map(|_| i64::from(fact.body.0)),
                Facet::Lens => extra.lens.map(i64::from),
                Facet::Kind => Some(extra.kind as i64),
                Facet::Pick => Some(i64::from(extra.picked)),
            };
            *counts.entry(key).or_default() += 1;
        }
    }
    let names = &candidates.names;
    let mut answer = BTreeMap::new();
    for (facet, counts) in asked.into_iter().zip(counts) {
        let mut values: Vec<(Option<i64>, u32)> = counts.into_iter().collect();
        let value = |key: Option<i64>| -> (Option<String>, Option<String>) {
            let Some(key) = key else {
                let label = (facet == Facet::Camera).then(|| "Unknown camera".to_owned());
                return (None, label);
            };
            match facet {
                Facet::Date => (Some(LocalDay(key as i32).to_string()), None),
                Facet::Place => (Some(names.places[key as usize].clone()), None),
                Facet::Camera => {
                    let body = crate::catalog_types::BodyIndex(key as u32);
                    (
                        Some(names.tables.body_key(body).0),
                        Some(names.tables.body_label(body)),
                    )
                }
                Facet::Lens => (Some(names.lenses[key as usize].clone()), None),
                Facet::Kind => {
                    let kind = crate::SourceTag::ALL[key as usize];
                    (
                        Some(kind.as_str().to_owned()),
                        Some(kind.label().to_owned()),
                    )
                }
                Facet::Pick if key == 1 => (Some("picked".into()), Some("Picked".into())),
                Facet::Pick => (Some("not-picked".into()), Some("Not picked".into())),
            }
        };
        let mut listed: Vec<FacetValue> = values
            .drain(..)
            .map(|(key, count)| {
                let (value, label) = value(key);
                FacetValue {
                    value,
                    label,
                    count,
                }
            })
            .collect();
        // Most frequent first; then by value, the value absent last.
        listed.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.value.is_none().cmp(&b.value.is_none()))
                .then_with(|| a.value.cmp(&b.value))
        });
        answer.insert(facet, listed);
    }
    Ok(Facets { counts: answer })
}
