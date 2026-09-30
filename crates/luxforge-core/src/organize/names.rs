//! Event names (P4) and the bundled gazetteer as organizing asks it.
//!
//! An event is named, in this order:
//!
//! 1. **By its place**: what [`PlaceNames`] names at the median position of its positioned frames
//!    (component-wise; across the antimeridian, with western longitudes taken past 180° first).
//!    "Konstanz · 12–13 Sep".
//! 2. **By its folder**: the folder holding more than half of its frames, when a person named it:
//!    not a camera's folder (`100NZ8_1`, `DCIM`) and not only a date. A leading date is dropped when
//!    a name remains: "2026-09-14 Lake" names "Lake · 14 Sep".
//! 3. **By its dates and cameras**: "16 Sep · LEICA Q3, Apple iPhone 15 Pro", the cameras by frame
//!    count, two at most and then how many more ("NIKON Z 8, LEICA Q3 +1"); the dates alone when no
//!    frame records its camera.
//!
//! An Undated event is "Undated · " and its folder's name. Dates are camera-local: "12 Sep",
//! "12–13 Sep", "30 Sep – 2 Oct", and with their years only when the event crosses one ("30 Dec
//! 2025 – 2 Jan 2026").
use super::gazetteer::{self, Nearest};
use crate::catalog_types::{
    BodyIndex, EventGroup, EventId, FolderIndex, FrameFacts, FrameTables, GeoPosition, LocalDay,
    PlaceNames,
};
use std::path::Path;

/// Around a position, the most populous place within this distance may name it: the city rather
/// than the district, arrondissement or suburb the photograph stands in ("Berlin", not "Mitte").
const CITY_RADIUS_KM: f64 = 10.0;
/// …when it has at least this many times the people of the nearest whole place (one that is not a
/// section of another). A district or suburb is far smaller than its city (Paris's 1st
/// arrondissement 15,000 against Paris's 2.1 million; Manhattan 1.5 million against New York City's
/// 8.8 million), while a town beside a bigger neighbour is its own place: Lindau (24,500) beside
/// Bregenz (29,800), Bregenz beside Dornbirn (49,300), Venice (51,300) beside Mestre (147,700) and
/// Versailles beside Saint-Quentin-en-Yvelines, each within 10 km, all of which the most populous
/// place alone would misname.
const CITY_FACTOR: u64 = 3;
/// Otherwise the nearest whole place names a position up to this far away; past it (open country,
/// the sea) a position has no name and the event falls back to its folder or dates.
const NEAREST_PLACE_KM: f64 = 50.0;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// How many cameras an event's name lists before "+N".
const NAMED_CAMERAS: usize = 2;

/// The bundled gazetteer (`gazetteer.rs`, GeoNames `cities15000`) as event names read it: the most
/// populous place within 10 km of a position when it has at least three times the people of the
/// nearest whole place (not a section of another), else that nearest whole place when it is within
/// 50 km, else none. Never an online lookup.
///
/// Its first query parses the table and builds the index, about 10 ms in a release build; every
/// later one takes about a microsecond. A caller on the catalog owner thread warms it first from a
/// worker ([`Self::warm`]), so no client waits for that.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Gazetteer;

impl Gazetteer {
    /// Builds the gazetteer's index now, if no query has; call it from a worker so the first event
    /// list on the owner thread does not pay for the build.
    pub(crate) fn warm() {
        let _ = gazetteer::nearest(0.0, 0.0);
    }
}

impl PlaceNames for Gazetteer {
    fn nearest(&self, position: &GeoPosition) -> Option<String> {
        place_at(position).map(|found| found.place.name.to_owned())
    }
}

/// The place that names `position`, by [`Gazetteer`]'s rule.
fn place_at(position: &GeoPosition) -> Option<Nearest> {
    let (lat, lon) = (position.lat, position.lon);
    let whole = gazetteer::nearest_matching(lat, lon, |place| !place.is_section());
    let around = gazetteer::most_populous_within(lat, lon, CITY_RADIUS_KM, |_| true);
    let people = |found: &Nearest| u64::from(found.place.population);
    match (whole, around) {
        (Some(whole), Some(around)) if people(&around) >= CITY_FACTOR * people(&whole) => {
            Some(around)
        }
        (Some(whole), _) if whole.distance_km <= NEAREST_PLACE_KM => Some(whole),
        // No whole place near enough: only a section of a city can be within the radius.
        (_, around) => around,
    }
}

/// Builds events' names and summaries, reusing its buffers from one event to the next.
pub(super) struct Namer<'a> {
    tables: &'a FrameTables,
    places: &'a dyn PlaceNames,
    bodies: Vec<BodyIndex>,
    folders: Vec<FolderIndex>,
    latitudes: Vec<f64>,
    longitudes: Vec<f64>,
}

impl<'a> Namer<'a> {
    pub(super) fn new(tables: &'a FrameTables, places: &'a dyn PlaceNames) -> Self {
        Self {
            tables,
            places,
            bodies: Vec::new(),
            folders: Vec::new(),
            latitudes: Vec::new(),
            longitudes: Vec::new(),
        }
    }

    /// The event of `members` (indices into `frames`, in event order, all dated or all undated in
    /// one folder), which starts at `start` in the event order.
    pub(super) fn event(
        &mut self,
        frames: &[FrameFacts],
        members: &[u32],
        start: usize,
    ) -> EventGroup {
        self.bodies.clear();
        self.folders.clear();
        self.latitudes.clear();
        self.longitudes.clear();
        let (mut first_day, mut last_day) = (None, None);
        for index in members {
            let frame = &frames[*index as usize];
            self.bodies.push(frame.body);
            self.folders.push(frame.folder);
            if let Some(position) = frame.position {
                self.latitudes.push(position.lat);
                self.longitudes.push(position.lon);
            }
            if let Some(day) = frame.local_day {
                first_day = Some(first_day.map_or(day, |first: LocalDay| first.min(day)));
                last_day = Some(last_day.map_or(day, |last: LocalDay| last.max(day)));
            }
        }
        self.bodies.sort_unstable();
        self.folders.sort_unstable();
        let first = &frames[members[0] as usize];
        let last = &frames[members[members.len() - 1] as usize];
        let first_folder = self.tables.folder_path(first.folder);
        let (id, name, place) = match (first.instant_ms, first_day.zip(last_day)) {
            (Some(instant), Some((first_day, last_day))) => {
                let dates = dates(first_day, last_day);
                let place = central_position(&mut self.latitudes, &mut self.longitudes)
                    .and_then(|position| self.places.nearest(&position));
                let name = match (&place, self.named_folder(members.len())) {
                    (Some(place), _) => format!("{place} · {dates}"),
                    (None, Some(folder)) => format!("{folder} · {dates}"),
                    (None, None) => match self.cameras() {
                        Some(cameras) => format!("{dates} · {cameras}"),
                        None => dates,
                    },
                };
                (
                    EventId::of(&first_folder.join(&*first.name), instant),
                    name,
                    place,
                )
            }
            _ => (
                EventId::of(first_folder, 0),
                format!("Undated · {}", folder_name(first_folder)),
                None,
            ),
        };
        EventGroup {
            id,
            name,
            place,
            start_ms: first.instant_ms,
            end_ms: last.instant_ms,
            first_day,
            last_day,
            bodies: distinct(&self.bodies),
            folders: distinct(&self.folders),
            start: start as u32,
            len: members.len() as u32,
        }
    }

    /// The user-given name of the folder holding more than half of an event of `count` frames, when
    /// there is one.
    fn named_folder(&self, count: usize) -> Option<String> {
        let (folder, held) = runs(&self.folders).max_by_key(|(_, held)| *held)?;
        (held * 2 > count)
            .then(|| user_folder_name(self.tables.folder_path(folder)))
            .flatten()
    }

    /// The event's recorded cameras by frame count (bodies of one label counted together), two at
    /// most and then how many more; none when no frame records its camera.
    fn cameras(&self) -> Option<String> {
        let mut labels: Vec<(String, usize)> = Vec::new();
        for (body, count) in runs(&self.bodies) {
            if self.tables.camera(body).is_none() {
                continue;
            }
            let label = self.tables.body_label(body);
            match labels.iter_mut().find(|(known, _)| *known == label) {
                Some((_, total)) => *total += count,
                None => labels.push((label, count)),
            }
        }
        labels.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let named: Vec<&str> = labels
            .iter()
            .take(NAMED_CAMERAS)
            .map(|(label, _)| label.as_str())
            .collect();
        let mut text = named.join(", ");
        if labels.len() > NAMED_CAMERAS {
            text.push_str(&format!(" +{}", labels.len() - NAMED_CAMERAS));
        }
        (!text.is_empty()).then_some(text)
    }
}

/// The distinct values of a sorted list and how many times each occurs.
fn runs<T: Copy + PartialEq>(sorted: &[T]) -> impl Iterator<Item = (T, usize)> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        let value = *sorted.get(at)?;
        let count = sorted[at..]
            .iter()
            .take_while(|next| **next == value)
            .count();
        at += count;
        Some((value, count))
    })
}

fn distinct<T: Copy + PartialEq>(sorted: &[T]) -> Vec<T> {
    runs(sorted).map(|(value, _)| value).collect()
}

/// The component-wise median of positions given as latitudes and longitudes (reordered in place),
/// or none when there are none. When the longitudes span more than half the globe, the event
/// straddles the antimeridian: western longitudes are taken past 180° for the median, which is
/// then wrapped back.
pub(super) fn central_position(
    latitudes: &mut [f64],
    longitudes: &mut [f64],
) -> Option<GeoPosition> {
    if latitudes.is_empty() {
        return None;
    }
    let (west, east) = longitudes
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(west, east), lon| {
            (west.min(*lon), east.max(*lon))
        });
    let lon = if east - west > 180.0 {
        for lon in longitudes.iter_mut().filter(|lon| **lon < 0.0) {
            *lon += 360.0;
        }
        let lon = middle(longitudes);
        if lon > 180.0 { lon - 360.0 } else { lon }
    } else {
        middle(longitudes)
    };
    Some(GeoPosition {
        lat: middle(latitudes),
        lon,
        alt_m: None,
    })
}

/// The median of `values` (sorted in place), the mean of the middle two for an even count.
fn middle(values: &mut [f64]) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle]
    } else {
        (values[middle - 1] + values[middle]) / 2.0
    }
}

/// A day range as an event's name shows it: "12 Sep", "12–13 Sep", "30 Sep – 2 Oct", "30 Dec 2025
/// – 2 Jan 2026".
pub(super) fn dates(first: LocalDay, last: LocalDay) -> String {
    let (first_year, first_month, first_day) = first.ymd();
    let (last_year, last_month, last_day) = last.ymd();
    let month = |month: u32| MONTHS[month as usize - 1];
    if first_year != last_year {
        format!(
            "{first_day} {} {first_year} – {last_day} {} {last_year}",
            month(first_month),
            month(last_month)
        )
    } else if first_month != last_month {
        format!(
            "{first_day} {} – {last_day} {}",
            month(first_month),
            month(last_month)
        )
    } else if first_day != last_day {
        format!("{first_day}–{last_day} {}", month(first_month))
    } else {
        format!("{first_day} {}", month(first_month))
    }
}

/// A folder's own name, or its whole path when it has none (a volume's root).
fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .unwrap_or(folder.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// A folder's name when a person gave it, without a leading date; none for a camera's folder
/// (`DCIM`, or DCF's three digits and five characters, `100NZ8_1`) or a name that is only a date.
pub(super) fn user_folder_name(folder: &Path) -> Option<String> {
    let name = folder.file_name()?.to_string_lossy();
    let name = name.trim();
    if name.eq_ignore_ascii_case("DCIM") || is_dcf_folder(name) || name.chars().all(date_char) {
        return None;
    }
    Some(without_leading_date(name).to_owned())
}

/// A DCF directory name (Design rule for Camera File system 2.0, 3.2.1): a number from 100 to 999
/// and five free characters (digits, Latin letters and underscores).
fn is_dcf_folder(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 8
        && (b'1'..=b'9').contains(&bytes[0])
        && bytes[1..3].iter().all(u8::is_ascii_digit)
        && bytes[3..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

/// A character of a written date: a digit or a separator.
fn date_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '-' | '_' | '.' | ' ')
}

/// `name` without its leading date, when it starts with one and a name remains: a leading run of
/// digits and separators holding at least four digits (a year, or a day and month), so "2026-09-14
/// Lake" is "Lake" but "100 Days" stays whole.
fn without_leading_date(name: &str) -> &str {
    let date_len = name.find(|c: char| !date_char(c)).unwrap_or(name.len());
    let digits = name[..date_len].bytes().filter(u8::is_ascii_digit).count();
    let rest = name[date_len..].trim();
    if digits >= 4 && !rest.is_empty() {
        rest
    } else {
        name
    }
}
