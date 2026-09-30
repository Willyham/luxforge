//! A [`ViewFilter`] as per-item predicates: each condition decided once per distinct body, lens and
//! place of the source, then per item by lookups. [`Filter::failing`] answers which conditions an
//! item fails, so a view keeps the items that fail none and a facet counts the items that fail
//! none but its own.

use super::candidates::{Candidates, Extra};
use crate::{
    SourceTag,
    catalog_types::{BodyIndex, DateRange, FrameFacts, ViewFilter},
};
use std::collections::HashSet;

/// The conditions of a filter, one bit each in what [`Filter::failing`] answers.
pub(super) const TEXT: u16 = 1 << 0;
pub(super) const PICKED: u16 = 1 << 1;
pub(super) const WITHOUT_PICK: u16 = 1 << 2;
pub(super) const CAMERAS: u16 = 1 << 3;
pub(super) const LENSES: u16 = 1 << 4;
pub(super) const KINDS: u16 = 1 << 5;
pub(super) const EDITED: u16 = 1 << 6;
pub(super) const DATES: u16 = 1 << 7;
pub(super) const PLACES: u16 = 1 << 8;

/// A filter compiled against one source's names.
pub(super) struct Filter {
    /// The text, lowercased, with whether each body, lens and place matches it.
    text: Option<Text>,
    picked: Option<bool>,
    without_pick: bool,
    /// Per body index: selected.
    cameras: Option<Vec<bool>>,
    lenses: Option<Vec<bool>>,
    kinds: Option<Vec<SourceTag>>,
    edited: Option<bool>,
    dates: Option<DateRange>,
    places: Option<Vec<bool>>,
}

struct Text {
    needle: String,
    bodies: Vec<bool>,
    lenses: Vec<bool>,
    places: Vec<bool>,
}

impl Filter {
    pub fn new(filter: &ViewFilter, candidates: &Candidates) -> Self {
        let names = &candidates.names;
        let bodies = || (0..names.bodies).map(|index| BodyIndex(index as u32));
        let text = filter.text.as_ref().map(|text| {
            let needle = text.to_lowercase();
            let matches = |value: &str| value.to_lowercase().contains(&needle);
            Text {
                bodies: bodies()
                    .map(|body| {
                        names.tables.camera(body).is_some_and(|camera| {
                            matches(&camera.label())
                                || matches(&camera.make)
                                || matches(&camera.model)
                        })
                    })
                    .collect(),
                lenses: names.lenses.iter().map(|lens| matches(lens)).collect(),
                places: names.places.iter().map(|place| matches(place)).collect(),
                needle,
            }
        });
        let chosen = |values: &[String], names: &[String]| -> Option<Vec<bool>> {
            (!values.is_empty()).then(|| {
                let values: HashSet<&str> = values.iter().map(String::as_str).collect();
                names
                    .iter()
                    .map(|name| values.contains(name.as_str()))
                    .collect()
            })
        };
        Self {
            text,
            picked: filter.picked,
            without_pick: filter.without_pick,
            cameras: (!filter.cameras.is_empty()).then(|| {
                let keys: HashSet<_> = filter.cameras.iter().collect();
                bodies()
                    .map(|body| keys.contains(&names.tables.body_key(body)))
                    .collect()
            }),
            lenses: chosen(&filter.lenses, &names.lenses),
            kinds: (!filter.kinds.is_empty()).then(|| filter.kinds.clone()),
            edited: filter.edited,
            dates: filter.dates,
            places: chosen(&filter.places, &names.places),
        }
    }

    /// The conditions `fact` and `extra` fail, as bits.
    pub fn failing(&self, fact: &FrameFacts, extra: &Extra) -> u16 {
        let mut failing = 0;
        let body = fact.body.0 as usize;
        if let Some(text) = &self.text {
            let found = fact.name.to_lowercase().contains(&text.needle)
                || text.bodies[body]
                || extra.lens.is_some_and(|lens| text.lenses[lens as usize])
                || extra.place.is_some_and(|place| text.places[place as usize]);
            if !found {
                failing |= TEXT;
            }
        }
        if self.picked.is_some_and(|picked| picked != extra.picked) {
            failing |= PICKED;
        }
        if self.without_pick && extra.decided {
            failing |= WITHOUT_PICK;
        }
        if self.cameras.as_ref().is_some_and(|cameras| !cameras[body]) {
            failing |= CAMERAS;
        }
        if self
            .lenses
            .as_ref()
            .is_some_and(|lenses| !extra.lens.is_some_and(|lens| lenses[lens as usize]))
        {
            failing |= LENSES;
        }
        if self
            .kinds
            .as_ref()
            .is_some_and(|kinds| !kinds.contains(&extra.kind))
        {
            failing |= KINDS;
        }
        if self.edited.is_some_and(|edited| edited != extra.edited) {
            failing |= EDITED;
        }
        if let Some(dates) = self.dates
            && !fact
                .local_day
                .is_some_and(|day| dates.from <= day && day <= dates.to)
        {
            failing |= DATES;
        }
        if self
            .places
            .as_ref()
            .is_some_and(|places| !extra.place.is_some_and(|place| places[place as usize]))
        {
            failing |= PLACES;
        }
        failing
    }
}
