//! An independent reference grouping, written plainly (quadratic where that is simpler), and the
//! implementation compared with it over thousands of random frame sets from fixed seeds: bodies with
//! and without GPS and subsecond clocks, gaps around every threshold, positions around the jump
//! distance and across the antimeridian, bursts, drifting exposures, brackets of nominal
//! third-stop values, consecutive brackets, brackets only the previews show, and undated frames.
use super::*;
use crate::catalog_types::{BodyIndex, FolderIndex, PlaceNames};
use std::collections::BTreeMap;

/// A fixed-seed generator (SplitMix64).
pub(in crate::organize) struct Random(u64);

impl Random {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, count: u64) -> u64 {
        self.next() % count
    }

    pub fn between(&mut self, low: i64, high: i64) -> i64 {
        low + self.below((high - low + 1) as u64) as i64
    }

    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn uniform(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    pub fn chance(&mut self, probability: f64) -> bool {
        self.unit() < probability
    }

    pub fn pick<T: Copy>(&mut self, choices: &[T]) -> T {
        choices[self.below(choices.len() as u64) as usize]
    }
}

/// Nominal third-stop shutter times, as cameras record them (seconds are `1 / value`).
const THIRDS: [f32; 37] = [
    4000.0, 3200.0, 2500.0, 2000.0, 1600.0, 1250.0, 1000.0, 800.0, 640.0, 500.0, 400.0, 320.0,
    250.0, 200.0, 160.0, 125.0, 100.0, 80.0, 60.0, 50.0, 40.0, 30.0, 25.0, 20.0, 15.0, 13.0, 10.0,
    8.0, 6.0, 5.0, 4.0, 3.0, 2.5, 2.0, 1.6, 1.3, 1.0,
];

/// A gazetteer standing in for the real one: a name for every position on a quarter-degree grid in
/// the northern hemisphere, none in the southern, so both naming paths are taken.
struct GridPlaces;

impl PlaceNames for GridPlaces {
    fn nearest(&self, position: &GeoPosition) -> Option<String> {
        (position.lat >= 0.0).then(|| {
            format!(
                "P{}/{}",
                (position.lat * 4.0).round(),
                (position.lon * 4.0).round()
            )
        })
    }
}

/// A preview check standing in for the preview lane's: each frame's measured brightness and its
/// framing, known for some frames; it answers when it knows every frame and they share one framing.
#[derive(Default)]
pub(in crate::organize) struct TableProbe(pub HashMap<ViewItem, (u32, f32)>);

impl BracketProbe for TableProbe {
    fn measure(&self, frames: &[ViewItem]) -> Option<Vec<f32>> {
        let known: Option<Vec<(u32, f32)>> = frames
            .iter()
            .map(|item| self.0.get(item).copied())
            .collect();
        let known = known?;
        known
            .iter()
            .all(|(framing, _)| *framing == known[0].0)
            .then(|| known.iter().map(|(_, value)| value - known[0].1).collect())
    }
}

/// One random frame set: its frames, the thresholds to group them by, and the probe's knowledge.
struct Case {
    frames: Frames,
    thresholds: Thresholds,
    probe: TableProbe,
}

struct Camera {
    body: Option<CameraBody>,
    positioned: bool,
    whole_seconds: bool,
    folder: &'static str,
}

const FOLDERS: [&str; 6] = [
    "/Volumes/Z8/DCIM/100NZ8_1",
    "/Pictures/2026-09-14 Lake",
    "/Pictures/Card dumps/2026-09-12",
    "/Pictures/iPhone export",
    "/Pictures/From Anna",
    "/Pictures/Trips/Iceland",
];

fn random_thresholds(random: &mut Random) -> Thresholds {
    if random.chance(0.6) {
        return Thresholds::default();
    }
    let run_gap_ms = random.between(300, 1500) as u64;
    let bracket_min_frames = random.between(2, 3) as u32;
    let thresholds = Thresholds {
        event_gap_ms: random.between(HOUR, 6 * HOUR) as u64,
        event_distance_km: random.pick(&[5.0, 25.0, 40.0]),
        run_gap_ms,
        matched_run_gap_ms: random.between(run_gap_ms as i64, 3000) as u64,
        metadata_bracket_step_ev: random.pick(&[0.2, 1.0 / 3.0, 0.5, 1.0]),
        preview_bracket_step_ev: random.pick(&[0.5, 2.0 / 3.0, 1.0]),
        bracket_min_frames,
        bracket_max_frames: random.between(i64::from(bracket_min_frames), 9) as u32,
    };
    thresholds.validate().unwrap();
    thresholds
}

fn random_case(seed: u64) -> Case {
    let mut random = Random::new(seed);
    let thresholds = random_thresholds(&mut random);
    let models = [
        ("NIKON CORPORATION", "NIKON Z 8", Some("3012845")),
        ("NIKON CORPORATION", "NIKON Z 8", Some("3019377")),
        ("LEICA CAMERA AG", "LEICA Q3", None),
        ("DJI", "FC3411", Some("0K8TH4F")),
        ("Apple", "iPhone 15 Pro", None),
    ];
    let cameras: Vec<Camera> = (0..random.between(1, 4))
        .map(|_| {
            let (make, model, serial) = random.pick(&models);
            Camera {
                body: (!random.chance(0.1)).then(|| body(make, model, serial)),
                positioned: random.chance(0.4),
                whole_seconds: random.chance(0.3),
                folder: random.pick(&FOLDERS),
            }
        })
        .collect();
    let mut place = random.pick(&[
        (47.66, 9.17),
        (52.52, 13.4),
        (-16.8, 179.95),
        (-33.9, 151.2),
    ]);
    let mut clock = midnight(
        random.between(2024, 2026) as i32,
        random.between(1, 12) as u32,
        28,
    ) + random.between(6 * HOUR, 10 * HOUR);
    let mut frames = Frames::default();
    let mut probe = TableProbe::default();
    let undated = random.pick(&[0.0, 0.05, 0.3]);
    for moment in 0..random.between(3, 50) {
        clock += match random.below(8) {
            0 => random.between(300, 2500),
            1 => random.between(3 * SECOND, 5 * MINUTE),
            2 => random.between(5 * MINUTE, 150 * MINUTE),
            3 => random.between(170 * MINUTE, 190 * MINUTE),
            4 => random.between(3 * HOUR, 20 * HOUR),
            5 => random.between(DAY - 10 * MINUTE, DAY + 10 * MINUTE),
            6 => random.between(DAY, 5 * DAY),
            _ => random.between(20 * SECOND, 40 * MINUTE),
        };
        if random.chance(0.15) {
            // Move by a distance around the jump threshold, in a random direction.
            let km = random.pick(&[3.0, 20.0, 24.0, 26.0, 30.0, 45.0, 100.0]);
            let bearing = random.uniform(0.0, std::f64::consts::TAU);
            place.0 = (place.0 + km / 111.2 * bearing.cos()).clamp(-80.0, 80.0);
            let lon = place.1 + km / (111.2 * place.0.to_radians().cos()) * bearing.sin();
            place.1 = if lon > 180.0 {
                lon - 360.0
            } else if lon < -180.0 {
                lon + 360.0
            } else {
                lon
            };
        }
        let camera = &cameras[random.below(cameras.len() as u64) as usize];
        let base = Exposure {
            time_s: Some(1.0 / random.pick(&THIRDS[6..30])),
            f_number: Some(random.pick(&[2.8, 4.0, 5.6, 8.0])),
            iso: Some(random.pick(&[64, 100, 400])),
            bias_ev: random.pick(&[None, Some(0.0), Some(-1.0 / 3.0)]),
            focal_mm: Some(random.pick(&[24.0, 50.0])),
            focal_35mm_mm: None,
        };
        // The frames' offsets from the moment's start and their exposures and brightness.
        let mut shots: Vec<(i64, Exposure, Option<f32>)> = Vec::new();
        match random.below(7) {
            // A single.
            0 | 1 => shots.push((0, base, None)),
            // A burst, sometimes with gaps near the run thresholds.
            2 => {
                let mut offset = 0;
                for _ in 0..random.between(2, 12) {
                    shots.push((offset, base, Some(random.uniform(-0.05, 0.05) as f32)));
                    let gaps = [random.between(50, 900), random.between(900, 2100), 1000];
                    offset += random.pick(&gaps);
                }
            }
            // Auto-exposure drifting through a burst, continuously or in nominal steps.
            3 => {
                let mut offset = 0;
                let start = random.below(20) as usize + 6;
                for at in 0..random.between(2, 8) as usize {
                    let time = if random.chance(0.5) {
                        1.0 / (THIRDS[start] * (1.0 + at as f32 * random.uniform(0.01, 0.2) as f32))
                    } else {
                        1.0 / THIRDS[start + random.below(2) as usize]
                    };
                    shots.push((
                        offset,
                        Exposure {
                            time_s: Some(time),
                            ..base
                        },
                        None,
                    ));
                    offset += random.between(100, 700);
                }
            }
            // Brackets of nominal third-stop values, one or several in a row, bias written or not.
            4 | 5 => {
                let count = random.between(2, 5) as usize;
                let thirds = random.pick(&[1, 2, 3, 6]) as isize;
                let pattern: Vec<isize> = match random.below(3) {
                    0 => (0..count as isize)
                        .map(|at| at - count as isize / 2)
                        .collect(),
                    1 => (0..count as isize)
                        .map(|at| if at % 2 == 1 { -((at + 1) / 2) } else { at / 2 })
                        .collect(),
                    _ => (0..count as isize).collect(),
                };
                let middle = 18isize;
                let bias = random.below(3);
                let mut offset = 0;
                for _ in 0..random.pick(&[1, 1, 1, 2, 3]) {
                    for step in &pattern {
                        let index = (middle - step * thirds).clamp(0, THIRDS.len() as isize - 1);
                        let exposure = Exposure {
                            time_s: Some(1.0 / THIRDS[index as usize]),
                            bias_ev: match bias {
                                0 => Some((step * thirds) as f32 / 3.0),
                                1 => Some(0.0),
                                _ => None,
                            },
                            ..base
                        };
                        shots.push((offset, exposure, None));
                        offset += random.between(300, 1900);
                    }
                }
            }
            // A bracket the metadata does not show: the previews do, when the probe knows them.
            _ => {
                let count = random.between(2, 6);
                let step = random.pick(&[0.4, 0.7, 1.0, 2.0]) as f32;
                let mut offset = 0;
                for at in 0..count {
                    shots.push((offset, base, Some(at as f32 * step)));
                    offset += random.between(300, 1900);
                }
            }
        }
        let known = random.chance(0.8);
        for (offset, exposure, brightness) in shots {
            let mut at = clock + offset;
            if camera.whole_seconds {
                at -= at.rem_euclid(SECOND);
            }
            let folder = if random.chance(0.1) {
                random.pick(&FOLDERS)
            } else {
                camera.folder
            };
            let mut shot = shot(folder, (!random.chance(undated)).then_some(at)).exposed(exposure);
            shot.camera = camera.body.clone();
            if camera.positioned && random.chance(0.9) {
                shot = shot.near(
                    place.0 + random.uniform(-0.005, 0.005),
                    place.1 + random.uniform(-0.005, 0.005),
                );
                if let Some(position) = &mut shot.position
                    && position.lon > 180.0
                {
                    position.lon -= 360.0;
                }
            }
            let item = frames.add(shot);
            if let Some(brightness) = brightness.filter(|_| known) {
                probe.0.insert(item, (moment as u32, brightness));
            }
        }
        clock += random.between(0, 2 * SECOND);
    }
    Case {
        frames,
        thresholds,
        probe,
    }
}

// The reference events.

#[derive(Debug, PartialEq)]
struct RefEvent {
    members: Vec<ViewItem>,
    id: EventId,
    name: String,
    place: Option<String>,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    first_day: Option<LocalDay>,
    last_day: Option<LocalDay>,
    bodies: Vec<BodyIndex>,
    folders: Vec<FolderIndex>,
}

fn reference_events(
    frames: &Frames,
    thresholds: &Thresholds,
    places: &dyn PlaceNames,
) -> Vec<RefEvent> {
    let all = &frames.frames;
    let mut dated: Vec<&FrameFacts> = all.iter().filter(|f| f.instant_ms.is_some()).collect();
    dated.sort_by_key(|f| (f.instant_ms, f.name.to_lowercase(), f.item));
    let mut groups: Vec<Vec<&FrameFacts>> = Vec::new();
    let mut current: Vec<&FrameFacts> = Vec::new();
    for (at, frame) in dated.iter().enumerate() {
        if !current.is_empty() {
            let last_position = current.iter().rev().find_map(|f| f.position);
            let gap = frame.instant_ms.unwrap() - dated[at - 1].instant_ms.unwrap();
            let long = gap as u64 > thresholds.event_gap_ms;
            let mut after = None;
            for next in at..dated.len() {
                if next > at
                    && (dated[next].instant_ms.unwrap() - dated[next - 1].instant_ms.unwrap())
                        as u64
                        > thresholds.event_gap_ms
                {
                    break;
                }
                if dated[next].position.is_some() {
                    after = dated[next].position;
                    break;
                }
            }
            let stay = long
                && gap < DAY
                && matches!((last_position, after), (Some(a), Some(b))
                    if a.distance_km(&b) <= thresholds.event_distance_km);
            let jump = matches!((last_position, frame.position), (Some(a), Some(b))
                if a.distance_km(&b) > thresholds.event_distance_km);
            if (long && !stay) || jump {
                groups.push(std::mem::take(&mut current));
            }
        }
        current.push(frame);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    let mut undated: BTreeMap<std::path::PathBuf, Vec<&FrameFacts>> = BTreeMap::new();
    for frame in all.iter().filter(|f| f.instant_ms.is_none()) {
        undated
            .entry(frames.tables.folder_path(frame.folder).clone())
            .or_default()
            .push(frame);
    }
    for group in undated.values_mut() {
        group.sort_by_key(|f| (f.name.to_lowercase(), f.item));
    }
    groups.extend(undated.into_values());
    groups
        .into_iter()
        .map(|group| reference_event(frames, &group, places))
        .collect()
}

fn reference_event(frames: &Frames, group: &[&FrameFacts], places: &dyn PlaceNames) -> RefEvent {
    let tables = &frames.tables;
    let first = group[0];
    let last = group[group.len() - 1];
    let mut bodies: Vec<BodyIndex> = group.iter().map(|f| f.body).collect();
    bodies.sort();
    bodies.dedup();
    let mut folders: Vec<FolderIndex> = group.iter().map(|f| f.folder).collect();
    folders.sort();
    folders.dedup();
    let first_day = group.iter().filter_map(|f| f.local_day).min();
    let last_day = group.iter().filter_map(|f| f.local_day).max();
    let folder = tables.folder_path(first.folder);
    let (id, name, place) = match first.instant_ms {
        None => (
            EventId::of(folder, 0),
            format!(
                "Undated · {}",
                folder
                    .file_name()
                    .map_or(folder.to_string_lossy(), |name| name.to_string_lossy())
            ),
            None,
        ),
        Some(instant) => {
            let dates = reference_dates(first_day.unwrap(), last_day.unwrap());
            let positions: Vec<GeoPosition> = group.iter().filter_map(|f| f.position).collect();
            let place = (!positions.is_empty())
                .then(|| places.nearest(&reference_central(&positions)))
                .flatten();
            let mut per_folder: HashMap<FolderIndex, usize> = HashMap::new();
            for frame in group {
                *per_folder.entry(frame.folder).or_default() += 1;
            }
            let folder_name = per_folder
                .iter()
                .find(|(_, count)| **count * 2 > group.len())
                .and_then(|(folder, _)| reference_folder_name(tables.folder_path(*folder)));
            let mut per_label: HashMap<String, usize> = HashMap::new();
            for frame in group {
                if tables.camera(frame.body).is_some() {
                    *per_label.entry(tables.body_label(frame.body)).or_default() += 1;
                }
            }
            let mut labels: Vec<(String, usize)> = per_label.into_iter().collect();
            labels.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let cameras = match labels.len() {
                0 => None,
                1 => Some(labels[0].0.clone()),
                2 => Some(format!("{}, {}", labels[0].0, labels[1].0)),
                more => Some(format!("{}, {} +{}", labels[0].0, labels[1].0, more - 2)),
            };
            let name = if let Some(place) = &place {
                format!("{place} · {dates}")
            } else if let Some(folder) = folder_name {
                format!("{folder} · {dates}")
            } else if let Some(cameras) = cameras {
                format!("{dates} · {cameras}")
            } else {
                dates
            };
            (
                EventId::of(&folder.join(&*first.name), instant),
                name,
                place,
            )
        }
    };
    RefEvent {
        members: group.iter().map(|f| f.item).collect(),
        id,
        name,
        place,
        start_ms: first.instant_ms,
        end_ms: last.instant_ms,
        first_day,
        last_day,
        bodies,
        folders,
    }
}

fn reference_central(positions: &[GeoPosition]) -> GeoPosition {
    fn middle(mut values: Vec<f64>) -> f64 {
        values.sort_by(f64::total_cmp);
        let n = values.len();
        if n % 2 == 1 {
            values[n / 2]
        } else {
            (values[n / 2 - 1] + values[n / 2]) / 2.0
        }
    }
    let lons: Vec<f64> = positions.iter().map(|p| p.lon).collect();
    let west = lons.iter().copied().fold(f64::INFINITY, f64::min);
    let east = lons.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let lon = if east - west > 180.0 {
        let shifted = middle(
            lons.iter()
                .map(|l| if *l < 0.0 { l + 360.0 } else { *l })
                .collect(),
        );
        if shifted > 180.0 {
            shifted - 360.0
        } else {
            shifted
        }
    } else {
        middle(lons)
    };
    GeoPosition {
        lat: middle(positions.iter().map(|p| p.lat).collect()),
        lon,
        alt_m: None,
    }
}

fn reference_dates(first: LocalDay, last: LocalDay) -> String {
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let ((y1, m1, d1), (y2, m2, d2)) = (first.ymd(), last.ymd());
    let (m1, m2) = (months[m1 as usize - 1], months[m2 as usize - 1]);
    if (y1, m1, d1) == (y2, m2, d2) {
        format!("{d1} {m1}")
    } else if y1 != y2 {
        format!("{d1} {m1} {y1} – {d2} {m2} {y2}")
    } else if m1 != m2 {
        format!("{d1} {m1} – {d2} {m2}")
    } else {
        format!("{d1}–{d2} {m1}")
    }
}

fn reference_folder_name(folder: &std::path::Path) -> Option<String> {
    let name = folder.file_name()?.to_string_lossy().trim().to_owned();
    let dateish = |c: char| c.is_ascii_digit() || "-_. ".contains(c);
    let chars: Vec<char> = name.chars().collect();
    let dcf = chars.len() == 8
        && ('1'..='9').contains(&chars[0])
        && chars[1..3].iter().all(char::is_ascii_digit)
        && chars[3..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == '_');
    if dcf || name.to_uppercase() == "DCIM" || name.chars().all(dateish) {
        return None;
    }
    let prefix: String = name.chars().take_while(|c| dateish(*c)).collect();
    let rest = name[prefix.len()..].trim().to_owned();
    if prefix.chars().filter(char::is_ascii_digit).count() >= 4 && !rest.is_empty() {
        Some(rest)
    } else {
        Some(name)
    }
}

fn compare_events(case: &Case, seed: impl std::fmt::Display, places: &dyn PlaceNames) {
    let frames = &case.frames;
    let set = events(&frames.frames, &frames.tables, &case.thresholds, places);
    let expected = reference_events(frames, &case.thresholds, places);
    let got: Vec<RefEvent> = set
        .events
        .iter()
        .zip(frames.members(&set))
        .map(|(event, members)| RefEvent {
            members,
            id: event.id.clone(),
            name: event.name.clone(),
            place: event.place.clone(),
            start_ms: event.start_ms,
            end_ms: event.end_ms,
            first_day: event.first_day,
            last_day: event.last_day,
            bodies: event.bodies.clone(),
            folders: event.folders.clone(),
        })
        .collect();
    assert_eq!(got.len(), expected.len(), "seed {seed}: event count");
    for (at, (got, expected)) in got.iter().zip(&expected).enumerate() {
        assert_eq!(got, expected, "seed {seed}: event {at}");
    }
    let mut starts: Vec<u32> = set.events.iter().map(|event| event.start).collect();
    starts.dedup();
    assert_eq!(
        starts.len(),
        set.events.len(),
        "seed {seed}: events are disjoint"
    );
    assert_eq!(
        set.events
            .iter()
            .map(|event| event.len as usize)
            .sum::<usize>(),
        frames.frames.len(),
        "seed {seed}: every frame is in one event"
    );
}

// The reference moments.

/// The view order of `frames` under Day › Camera › Moment, by a plain key.
fn reference_order(frames: &Frames, descending: bool) -> Vec<ViewItem> {
    let all = &frames.frames;
    let mut first_of: HashMap<(Option<LocalDay>, BodyIndex), i64> = HashMap::new();
    for frame in all.iter().filter(|f| f.instant_ms.is_some()) {
        let first = first_of
            .entry((frame.local_day, frame.body))
            .or_insert(i64::MAX);
        *first = (*first).min(frame.instant_ms.unwrap());
    }
    let mut dated: Vec<&FrameFacts> = all.iter().filter(|f| f.instant_ms.is_some()).collect();
    dated.sort_by_key(|f| {
        let first = first_of[&(f.local_day, f.body)];
        (
            f.local_day,
            first,
            f.body,
            f.instant_ms,
            f.name.to_lowercase(),
            f.item,
        )
    });
    if descending {
        dated.reverse();
    }
    let mut undated: Vec<&FrameFacts> = all.iter().filter(|f| f.instant_ms.is_none()).collect();
    undated.sort_by_key(|f| {
        (
            frames.tables.folder_path(f.folder).clone(),
            f.name.to_lowercase(),
            f.item,
        )
    });
    dated.into_iter().chain(undated).map(|f| f.item).collect()
}

fn reference_steps(run: &[Exposure]) -> Option<Vec<f32>> {
    let varies = |values: &[f32]| values.iter().any(|v| (v - values[0]).abs() > 1e-3);
    let brightness: Option<Vec<f32>> = run.iter().map(|e| e.ev().map(|ev| -ev)).collect();
    let bias: Option<Vec<f32>> = run.iter().map(|e| e.bias_ev).collect();
    let values = if brightness.as_deref().is_some_and(varies) {
        brightness.unwrap()
    } else if bias.as_deref().is_some_and(varies) {
        bias.clone().unwrap()
    } else {
        return None;
    };
    let zero: Vec<usize> = match &bias {
        Some(bias) => (0..bias.len())
            .filter(|at| bias[*at].abs() <= 1e-3)
            .collect(),
        None => Vec::new(),
    };
    let metered = if zero.len() == 1 {
        zero[0]
    } else {
        reference_middle_ranked(&values)
    };
    Some(values.iter().map(|v| v - values[metered]).collect())
}

fn reference_middle_ranked(values: &[f32]) -> usize {
    let mut ranked: Vec<(f32, usize)> = values.iter().copied().zip(0..).collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    ranked[(values.len() - 1) / 2].1
}

fn reference_apart(values: &[f32], apart: f32) -> bool {
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    values.iter().all(|v| v.is_finite())
        && (1..sorted.len()).all(|at| sorted[at] - sorted[at - 1] >= apart)
}

fn reference_moments(
    view: &[&FrameFacts],
    thresholds: &Thresholds,
    probe: &dyn BracketProbe,
) -> Vec<Moment> {
    let mut moments = Vec::new();
    let joined = |a: &FrameFacts, b: &FrameFacts| {
        if a.local_day.is_none() || a.local_day != b.local_day || a.body != b.body {
            return false;
        }
        let (x, y) = (a.instant_ms.unwrap(), b.instant_ms.unwrap());
        let gap = (x - y).unsigned_abs();
        let whole = x % 1000 == 0 && y % 1000 == 0;
        let under = |limit: u64| gap < limit || (whole && gap == limit);
        under(thresholds.run_gap_ms)
            || (under(thresholds.matched_run_gap_ms) && a.exposure.settings_match(&b.exposure))
    };
    let (low, high) = (
        thresholds.bracket_min_frames as usize,
        thresholds.bracket_max_frames as usize,
    );
    let metadata_same = 0.13f32.min(thresholds.metadata_bracket_step_ev / 2.0);
    let metadata_apart = thresholds.metadata_bracket_step_ev - metadata_same;
    let preview_same = 0.25f32.min(thresholds.preview_bracket_step_ev / 2.0);
    let preview_apart = thresholds.preview_bracket_step_ev - preview_same;
    let span = |run: &[&FrameFacts]| {
        (run[0].instant_ms.unwrap() - run[run.len() - 1].instant_ms.unwrap()).unsigned_abs()
    };
    let make = |kind, evidence, steps_ev, run: &[&FrameFacts], start: usize| Moment {
        kind,
        evidence,
        steps_ev,
        span_ms: span(run),
        start: start as u32,
        len: run.len() as u32,
        picked: 0,
    };
    let mut start = 0;
    while start < view.len() {
        let mut end = start + 1;
        while end < view.len() && joined(view[end - 1], view[end]) {
            end += 1;
        }
        let run = &view[start..end];
        let n = run.len();
        if n >= 2 {
            let exposures: Vec<Exposure> = run.iter().map(|f| f.exposure).collect();
            let fits = (low..=high).contains(&n);
            if let Some(steps) = reference_steps(&exposures) {
                let period = (low..=high).find(|p| {
                    *p < n
                        && n.is_multiple_of(*p)
                        && (0..n - p).all(|at| (steps[at] - steps[at + p]).abs() <= metadata_same)
                        && (0..n / p)
                            .all(|k| reference_apart(&steps[k * p..(k + 1) * p], metadata_apart))
                });
                if fits && reference_apart(&steps, metadata_apart) {
                    moments.push(make(
                        MomentKind::Bracket,
                        Some(BracketEvidence::Metadata),
                        steps,
                        run,
                        start,
                    ));
                } else if let Some(p) = period {
                    for k in 0..n / p {
                        let chunk = &run[k * p..(k + 1) * p];
                        let chunk_steps = reference_steps(&exposures[k * p..(k + 1) * p])
                            .unwrap_or_else(|| {
                                let values = &steps[k * p..(k + 1) * p];
                                let middle = values[reference_middle_ranked(values)];
                                values.iter().map(|v| v - middle).collect()
                            });
                        moments.push(make(
                            MomentKind::Bracket,
                            Some(BracketEvidence::Metadata),
                            chunk_steps,
                            chunk,
                            start + k * p,
                        ));
                    }
                } else {
                    moments.push(make(MomentKind::Burst, None, Vec::new(), run, start));
                }
            } else {
                let items: Vec<ViewItem> = run.iter().map(|f| f.item).collect();
                let measured = if fits { probe.measure(&items) } else { None };
                match measured {
                    Some(values)
                        if values.len() == n && reference_apart(&values, preview_apart) =>
                    {
                        let middle = values[reference_middle_ranked(&values)];
                        let steps = values.iter().map(|v| v - middle).collect();
                        moments.push(make(
                            MomentKind::Bracket,
                            Some(BracketEvidence::Previews),
                            steps,
                            run,
                            start,
                        ));
                    }
                    _ => moments.push(make(MomentKind::Burst, None, Vec::new(), run, start)),
                }
            }
        }
        start = end;
    }
    moments
}

fn reference_layout(
    frames: &Frames,
    view: &[ViewItem],
    thresholds: &Thresholds,
    probe: &dyn BracketProbe,
) -> GroupLayout {
    let by_item: HashMap<ViewItem, &FrameFacts> =
        frames.frames.iter().map(|f| (f.item, f)).collect();
    let view: Vec<&FrameFacts> = view.iter().map(|item| by_item[item]).collect();
    let mut layout = GroupLayout::default();
    let mut start = 0;
    while start < view.len() {
        let day = view[start].local_day;
        let end = (start..view.len())
            .find(|at| view[*at].local_day != day)
            .unwrap_or(view.len());
        layout.days.push(DayGroup {
            day,
            start: start as u32,
            len: (end - start) as u32,
            picked: 0,
        });
        let bodies: Vec<BodyIndex> = view[start..end].iter().map(|f| f.body).collect();
        if day.is_some() && bodies.iter().any(|b| *b != bodies[0]) {
            let mut at = start;
            while at < end {
                let body = view[at].body;
                let run_end = (at..end).find(|k| view[*k].body != body).unwrap_or(end);
                layout.cameras.push(CameraGroup {
                    body: frames.tables.body_key(body),
                    label: frames.tables.body_label(body),
                    start: at as u32,
                    len: (run_end - at) as u32,
                });
                at = run_end;
            }
        }
        start = end;
    }
    layout.moments = reference_moments(&view, thresholds, probe);
    layout
}

fn compare_layouts(case: &mut Case, seed: impl std::fmt::Display) {
    for descending in [false, true] {
        let frames = &mut case.frames;
        let expected_view = reference_order(frames, descending);
        order(
            &mut frames.frames,
            &frames.tables,
            Grouping::DayCameraMoment,
            descending,
        );
        let view: Vec<ViewItem> = frames.frames.iter().map(|f| f.item).collect();
        assert_eq!(
            view, expected_view,
            "seed {seed}: order (descending {descending})"
        );
        let layout = group(
            &frames.frames,
            &frames.tables,
            Grouping::DayCameraMoment,
            &case.thresholds,
            &case.probe,
        );
        let expected = reference_layout(frames, &view, &case.thresholds, &case.probe);
        assert_eq!(
            layout.days, expected.days,
            "seed {seed}: days (descending {descending})"
        );
        assert_eq!(
            layout.cameras, expected.cameras,
            "seed {seed}: cameras (descending {descending})"
        );
        assert_eq!(
            layout.moments.len(),
            expected.moments.len(),
            "seed {seed}: moments (descending {descending}): {:#?}\n{:#?}",
            layout.moments,
            expected.moments
        );
        for (got, expected) in layout.moments.iter().zip(&expected.moments) {
            assert_eq!(
                got, expected,
                "seed {seed}: moment (descending {descending})"
            );
        }
    }
}

/// Checks the events, days, cameras and moments of `frames` against the reference at the default
/// thresholds with no preview check, and answers the frames (in a view's order).
pub(super) fn check_against_reference(frames: Frames, what: &str) -> Frames {
    let mut case = Case {
        frames,
        thresholds: Thresholds::default(),
        probe: TableProbe::default(),
    };
    compare_events(&case, what, &Gazetteer);
    compare_layouts(&mut case, what);
    case.frames
}

/// Events, days, cameras and moments of 4,000 random frame sets equal the reference's.
#[test]
fn organize_equals_an_independent_grouping_of_random_frames() {
    let mut tallies = [0usize; 6];
    for seed in 0..4_000 {
        let mut case = random_case(seed);
        compare_events(&case, seed, &GridPlaces);
        compare_layouts(&mut case, seed);
        let layout = group(
            &case.frames.frames,
            &case.frames.tables,
            Grouping::DayCameraMoment,
            &case.thresholds,
            &case.probe,
        );
        let set = events(
            &case.frames.frames,
            &case.frames.tables,
            &case.thresholds,
            &GridPlaces,
        );
        tallies[0] += case.frames.frames.len();
        tallies[1] += set.events.len();
        for moment in &layout.moments {
            let at = match (moment.kind, moment.evidence) {
                (MomentKind::Burst, _) => 2,
                (_, Some(BracketEvidence::Metadata)) => 3,
                _ => 4,
            };
            tallies[at] += 1;
        }
        tallies[5] += set
            .events
            .iter()
            .filter(|event| event.place.is_some())
            .count();
    }
    // The cases reach every kind of result, so the comparison is not vacuous.
    let [frames, events, bursts, metadata, previews, placed] = tallies;
    println!(
        "{frames} frames, {events} events ({placed} placed), {bursts} bursts, {metadata} metadata and {previews} preview brackets"
    );
    assert!(frames > 100_000 && events > 10_000 && placed > 1_000);
    assert!(bursts > 5_000 && metadata > 2_000 && previews > 1_000);
}
