//! Organizing's tests: the rules case by case here, an independent reference over random frame
//! sets in `reference.rs`, and the generated image folders and index end to end in `generated.rs`;
//! `bracket_timing.rs` is the ignored bench `cargo xtask catalog-measure` times the preview bracket
//! check with.
use super::names::{central_position, dates, user_folder_name};
use super::*;
use crate::catalog_types::{
    BracketEvidence, CameraBody, EventGroup, EventId, FileId, GeoPosition, LocalDay, Moment,
    MomentKind, NoPlaces, NoProbe, ViewItem,
};
use std::{cell::Cell, collections::HashMap};

mod bracket_timing;
mod generated;
mod reference;

pub(super) const SECOND: i64 = 1000;
pub(super) const MINUTE: i64 = 60 * SECOND;
pub(super) const HOUR: i64 = 60 * MINUTE;
pub(super) const DAY: i64 = 24 * HOUR;

/// Midnight starting a date, as an instant read as UTC (the tests' clocks keep no offset).
pub(super) fn midnight(year: i32, month: u32, day: u32) -> i64 {
    i64::from(LocalDay::from_ymd(year, month, day).unwrap().0) * DAY
}

pub(super) fn body(make: &str, model: &str, serial: Option<&str>) -> CameraBody {
    CameraBody {
        make: make.into(),
        model: model.into(),
        serial: serial.map(Into::into),
    }
}

fn z8() -> CameraBody {
    body("NIKON CORPORATION", "NIKON Z 8", Some("3012845"))
}

fn q3() -> CameraBody {
    body("LEICA CAMERA AG", "LEICA Q3", Some("5561203"))
}

fn phone() -> CameraBody {
    body("Apple", "iPhone 15 Pro", None)
}

/// One frame to add: where it is, when and where it was taken, by what and how.
#[derive(Clone, Debug)]
pub(super) struct Shot {
    pub folder: String,
    pub at: Option<i64>,
    pub camera: Option<CameraBody>,
    pub position: Option<GeoPosition>,
    pub exposure: Exposure,
}

pub(super) fn shot(folder: &str, at: Option<i64>) -> Shot {
    Shot {
        folder: folder.into(),
        at,
        camera: None,
        position: None,
        exposure: Exposure::default(),
    }
}

impl Shot {
    pub fn by(mut self, camera: &CameraBody) -> Self {
        self.camera = Some(camera.clone());
        self
    }

    pub fn near(mut self, lat: f64, lon: f64) -> Self {
        self.position = Some(GeoPosition {
            lat,
            lon,
            alt_m: None,
        });
        self
    }

    pub fn exposed(mut self, exposure: Exposure) -> Self {
        self.exposure = exposure;
        self
    }
}

/// Frames and their tables, each frame named `F00001.JPG` … by the order it was added.
#[derive(Default)]
pub(super) struct Frames {
    pub tables: FrameTables,
    pub frames: Vec<FrameFacts>,
}

impl Frames {
    pub fn add(&mut self, shot: Shot) -> ViewItem {
        let id = self.frames.len() as i64 + 1;
        let item = ViewItem::File(FileId(id));
        self.frames.push(FrameFacts {
            item,
            folder: self.tables.folder(shot.folder.into()),
            name: format!("F{id:05}.JPG").into(),
            instant_ms: shot.at,
            local_day: shot.at.map(|at| LocalDay(at.div_euclid(DAY) as i32)),
            position: shot.position,
            body: self.tables.body(shot.camera.as_ref()),
            exposure: shot.exposure,
        });
        item
    }

    pub fn events(&self, thresholds: &Thresholds) -> EventSet {
        events(&self.frames, &self.tables, thresholds, &Gazetteer)
    }

    /// The events' members as item lists, in event order.
    pub fn members(&self, set: &EventSet) -> Vec<Vec<ViewItem>> {
        set.events
            .iter()
            .map(|event| {
                set.order[event.start as usize..(event.start + event.len) as usize]
                    .iter()
                    .map(|index| self.frames[*index as usize].item)
                    .collect()
            })
            .collect()
    }

    /// Orders the frames for a Day › Camera › Moment view and groups them.
    pub fn group(&mut self, thresholds: &Thresholds, probe: &dyn BracketProbe) -> GroupLayout {
        order(
            &mut self.frames,
            &self.tables,
            Grouping::DayCameraMoment,
            false,
        );
        group(
            &self.frames,
            &self.tables,
            Grouping::DayCameraMoment,
            thresholds,
            probe,
        )
    }
}

fn items(range: std::ops::RangeInclusive<i64>) -> Vec<ViewItem> {
    range.map(|id| ViewItem::File(FileId(id))).collect()
}

/// A day's outing: `count` frames from `from`, every `every`, by `camera`, at `place` when given.
fn outing(
    frames: &mut Frames,
    folder: &str,
    camera: &CameraBody,
    place: Option<(f64, f64)>,
    from: i64,
    every: i64,
    count: i64,
) {
    for at in 0..count {
        let shot = shot(folder, Some(from + at * every)).by(camera);
        frames.add(match place {
            Some((lat, lon)) => shot.near(lat, lon),
            None => shot,
        });
    }
}

const KONSTANZ: (f64, f64) = (47.6603, 9.1758);
/// A camera's card folder, which names nothing.
const CARD: &str = "/Volumes/NIKON Z 8/DCIM/100NZ8_1";

/// Days and cameras: a day with two bodies is split by body, each body in the order of its first
/// frame, and undated frames come last with no camera groups.
#[test]
fn organize_orders_and_groups_days_and_cameras() {
    let mut frames = Frames::default();
    let day = midnight(2026, 9, 12);
    for (at, camera) in [
        (Some(day + 5_000), z8()),
        (Some(day + 1_000), q3()),
        (Some(day + 2_000), z8()),
        (Some(day + 9_000), q3()),
        (None, z8()),
        (Some(day + DAY), z8()),
        (None, q3()),
    ] {
        let folder = if camera == z8() { "/card" } else { "/dump" };
        frames.add(shot(folder, at).by(&camera));
    }
    let layout = frames.group(&Thresholds::default(), &NoProbe);
    let ids: Vec<_> = frames.frames.iter().map(|frame| frame.item).collect();
    let expected: Vec<_> = [2, 4, 3, 1, 6, 5, 7]
        .map(|id| ViewItem::File(FileId(id)))
        .to_vec();
    assert_eq!(ids, expected);
    assert_eq!(
        layout
            .days
            .iter()
            .map(|day| (day.start, day.len))
            .collect::<Vec<_>>(),
        [(0, 4), (4, 1), (5, 2)]
    );
    assert_eq!(layout.days[2].day, None, "undated last");
    assert_eq!(
        layout
            .cameras
            .iter()
            .map(|camera| (camera.label.as_str(), camera.start, camera.len))
            .collect::<Vec<_>>(),
        [("LEICA Q3", 0, 2), ("NIKON Z 8", 2, 2)],
        "no camera groups for undated frames, ordered by folder and name"
    );
    assert!(layout.moments.is_empty(), "every frame is seconds apart");
    let set = frames.events(&Thresholds::default());
    assert_eq!(set.events.len(), 4, "two days and two undated folders");
    assert!(set.events[2].undated() && set.events[3].undated());
    assert_eq!(set.order.len(), frames.frames.len());
}

#[test]
fn organize_metadata_steps_follow_settings_then_bias() {
    let base = Exposure {
        time_s: Some(1.0 / 60.0),
        f_number: Some(8.0),
        iso: Some(100),
        bias_ev: Some(0.0),
        ..Exposure::default()
    };
    let at = |time: f32, bias: f32| Exposure {
        time_s: Some(time),
        bias_ev: Some(bias),
        ..base
    };
    // A shutter bracket with its bias recorded: steps from the settings, around the 0 frame, which
    // the camera shot first.
    let steps = metadata_steps(&[base, at(1.0 / 240.0, -2.0), at(1.0 / 15.0, 2.0)]).unwrap();
    for (step, expected) in steps.iter().zip([0.0, -2.0, 2.0]) {
        assert!((step - expected).abs() < 1e-3, "{steps:?}");
    }
    // Settings unchanged, bias written: the bias says.
    let drone = [
        at(1.0 / 60.0, -1.0),
        at(1.0 / 60.0, 0.0),
        at(1.0 / 60.0, 1.0),
    ];
    assert_eq!(metadata_steps(&drone), Some(vec![-1.0, 0.0, 1.0]));
    // Shutter times alone, 0 bias on every frame: the metered frame is the median, not the first.
    let shutter = [at(1.0 / 120.0, 0.0), base, at(1.0 / 30.0, 0.0)];
    let steps = metadata_steps(&shutter).unwrap();
    for (step, expected) in steps.iter().zip([-1.0, 0.0, 1.0]) {
        assert!((step - expected).abs() < 1e-3, "{steps:?}");
    }
    // Nothing changes: the metadata cannot say.
    assert_eq!(metadata_steps(&[base, base, base]), None);
    assert_eq!(metadata_steps(&[base]), None);
}

// Events.

/// Ground truth: a two-day trip whose phone records positions is one event across its night, with
/// the Nikon's frames (no GPS) in it, named for the place and both days.
#[test]
fn organize_a_positioned_two_day_trip_stays_one_event_across_the_night() {
    let mut frames = Frames::default();
    let first = midnight(2026, 9, 12);
    for day in [first, first + DAY] {
        outing(
            &mut frames,
            "/card/DCIM/100NZ8_1",
            &z8(),
            None,
            day + 8 * HOUR,
            HOUR,
            12,
        );
        outing(
            &mut frames,
            "/iPhone export",
            &phone(),
            Some(KONSTANZ),
            day + 8 * HOUR + 30 * MINUTE,
            2 * HOUR,
            5,
        );
    }
    let set = frames.events(&Thresholds::default());
    assert_eq!(set.events.len(), 1, "{:?}", names(&set));
    let event = &set.events[0];
    assert_eq!(event.name, "Konstanz · 12–13 Sep");
    assert_eq!(event.label, "Konstanz", "the name without its dates");
    assert_eq!(event.place.as_deref(), Some("Konstanz"));
    assert_eq!(event.len as usize, frames.frames.len());
    assert_eq!(
        (event.first_day, event.last_day),
        (
            Some(LocalDay::from_ymd(2026, 9, 12).unwrap()),
            Some(LocalDay::from_ymd(2026, 9, 13).unwrap())
        )
    );
}

/// The same trip with no position anywhere: nothing says it stayed, so each day's outing is its own
/// event, named by its dates and cameras (its card folder is a camera's).
#[test]
fn organize_the_same_trip_without_positions_splits_per_outing() {
    let mut frames = Frames::default();
    let first = midnight(2026, 9, 12);
    for day in [first, first + DAY] {
        outing(
            &mut frames,
            "/card/DCIM/100NZ8_1",
            &z8(),
            None,
            day + 8 * HOUR,
            HOUR,
            12,
        );
        outing(
            &mut frames,
            "/card/DCIM/100NZ8_1",
            &q3(),
            None,
            day + 9 * HOUR,
            2 * HOUR,
            3,
        );
    }
    let set = frames.events(&Thresholds::default());
    assert_eq!(
        names(&set),
        [
            "12 Sep · NIKON Z 8, LEICA Q3",
            "13 Sep · NIKON Z 8, LEICA Q3"
        ]
    );
    assert!(set.events.iter().all(|event| event.place.is_none()));
}

/// A trip that moves 100 km a day: the night's gap is not a stay, so each day is an event.
#[test]
fn organize_a_trip_moving_100_km_a_day_splits_per_day() {
    let mut frames = Frames::default();
    let first = midnight(2026, 9, 12);
    // Konstanz, then about 100 km south-west (Luzern is 90 km; this is 0.9° of latitude south).
    for (day, place) in [
        (first, KONSTANZ),
        (first + DAY, (KONSTANZ.0 - 0.9, KONSTANZ.1)),
    ] {
        outing(
            &mut frames,
            "/iPhone export",
            &phone(),
            Some(place),
            day + 8 * HOUR,
            HOUR,
            10,
        );
    }
    let set = frames.events(&Thresholds::default());
    assert_eq!(set.events.len(), 2, "{:?}", names(&set));
    assert_eq!(set.events[0].len, 10);
}

/// A stay is under 24 hours: a gap of exactly a day at the same place splits, a millisecond less
/// does not.
#[test]
fn organize_a_stay_of_a_day_or_more_splits() {
    for (gap, expected) in [(DAY, 2), (DAY - 1, 1), (3 * HOUR + 1, 1), (3 * DAY, 2)] {
        let mut frames = Frames::default();
        let start = midnight(2026, 9, 12) + 18 * HOUR;
        frames.add(
            shot("/p", Some(start))
                .by(&phone())
                .near(KONSTANZ.0, KONSTANZ.1),
        );
        frames.add(
            shot("/p", Some(start + gap))
                .by(&phone())
                .near(KONSTANZ.0 + 0.01, KONSTANZ.1),
        );
        let set = frames.events(&Thresholds::default());
        assert_eq!(set.events.len(), expected, "gap {gap} ms");
    }
}

/// A gap longer than the event gap splits when either side has no position, and the position after
/// it is looked for only up to the next such gap.
#[test]
fn organize_a_stay_needs_a_position_on_both_sides_before_the_next_gap() {
    let start = midnight(2026, 9, 12) + 8 * HOUR;
    // Positioned before the night; the next morning's first outing has no position, and only the
    // outing after it (another long gap later) is back at the same place.
    let mut frames = Frames::default();
    outing(&mut frames, "/p", &phone(), Some(KONSTANZ), start, HOUR, 6);
    outing(&mut frames, "/c", &z8(), None, start + DAY, 10 * MINUTE, 6);
    outing(
        &mut frames,
        "/p",
        &phone(),
        Some(KONSTANZ),
        start + DAY + 5 * HOUR,
        HOUR,
        3,
    );
    let set = frames.events(&Thresholds::default());
    assert_eq!(
        set.events.iter().map(|event| event.len).collect::<Vec<_>>(),
        [6, 6, 3]
    );
    // With a positioned frame in the morning outing, the night is a stay; the 4-hour gap after the
    // outing is a stay too.
    let mut frames = Frames::default();
    outing(&mut frames, "/p", &phone(), Some(KONSTANZ), start, HOUR, 6);
    outing(&mut frames, "/c", &z8(), None, start + DAY, 10 * MINUTE, 6);
    frames.add(
        shot("/p", Some(start + DAY + 55 * MINUTE))
            .by(&phone())
            .near(KONSTANZ.0, KONSTANZ.1),
    );
    outing(
        &mut frames,
        "/p",
        &phone(),
        Some(KONSTANZ),
        start + DAY + 5 * HOUR,
        HOUR,
        3,
    );
    let set = frames.events(&Thresholds::default());
    assert_eq!(set.events.len(), 1, "{:?}", names(&set));
    // Exactly the event gap is not longer than it; a millisecond more is.
    for (gap, expected) in [(3 * HOUR, 1), (3 * HOUR + 1, 2)] {
        let mut frames = Frames::default();
        frames.add(shot("/c", Some(start)).by(&z8()));
        frames.add(shot("/c", Some(start + gap)).by(&z8()));
        assert_eq!(frames.events(&Thresholds::default()).events.len(), expected);
    }
}

/// Zürich, then Luzern within the hour: a jump of over 25 km the same day starts a new event, at
/// the phone's first frame there; the Leica's frames between, with no GPS, stay with Zürich.
#[test]
fn organize_a_same_day_jump_splits_at_the_first_positioned_frame_past_it() {
    let mut frames = Frames::default();
    let start = midnight(2026, 9, 16) + 9 * HOUR;
    let zurich = (47.3769, 8.5417);
    // 30 km south of Zürich.
    let away = (zurich.0 - 0.27, zurich.1);
    frames.add(
        shot("/p", Some(start))
            .by(&phone())
            .near(zurich.0, zurich.1),
    );
    for at in 1..=4 {
        // The Leica shoots on without GPS, interleaved with the phone.
        frames.add(shot("/l", Some(start + at * 10 * MINUTE)).by(&q3()));
    }
    frames.add(
        shot("/p", Some(start + 25 * MINUTE))
            .by(&phone())
            .near(zurich.0, zurich.1 + 0.01),
    );
    frames.add(
        shot("/p", Some(start + 45 * MINUTE))
            .by(&phone())
            .near(away.0, away.1),
    );
    frames.add(shot("/l", Some(start + 50 * MINUTE)).by(&q3()));
    let set = frames.events(&Thresholds::default());
    let members = frames.members(&set);
    assert_eq!(members.len(), 2, "{:?}", names(&set));
    // Zürich: the phone's two frames and the Leica's first four (up to 40 minutes, before the
    // phone's frame at 45).
    assert_eq!(
        members[0],
        [1, 2, 3, 6, 4, 5].map(|id| ViewItem::File(FileId(id)))
    );
    assert_eq!(members[1], items(7..=8));
    assert_eq!(set.events[0].place.as_deref(), Some("Zürich"));
    // 25 km is the default threshold; at 40 km the same frames are one event.
    let set = frames.events(&Thresholds {
        event_distance_km: 40.0,
        ..Thresholds::default()
    });
    assert_eq!(set.events.len(), 1);
}

/// Changing a threshold regroups at once: nothing is stored.
#[test]
fn organize_a_threshold_change_regroups() {
    let mut frames = Frames::default();
    let start = midnight(2026, 9, 12) + 8 * HOUR;
    for at in [0, HOUR + 30 * MINUTE, 4 * HOUR, 5 * HOUR] {
        frames.add(shot("/c", Some(start + at)).by(&z8()));
    }
    assert_eq!(frames.events(&Thresholds::default()).events.len(), 1);
    let hourly = Thresholds {
        event_gap_ms: HOUR as u64,
        ..Thresholds::default()
    };
    let set = frames.events(&hourly);
    assert_eq!(
        set.events.iter().map(|event| event.len).collect::<Vec<_>>(),
        [1, 1, 2]
    );
}

/// Frames with no capture time: an Undated event per folder, each by file name, after every dated
/// event, identified by its folder.
#[test]
fn organize_undated_frames_form_an_event_per_folder_last() {
    let mut frames = Frames::default();
    frames.add(shot("/Pictures/Scans", None));
    frames.add(shot("/Pictures/From Anna", None).by(&q3()));
    frames.add(shot("/Pictures/From Anna", Some(midnight(2026, 9, 20))).by(&q3()));
    frames.add(shot("/Pictures/From Anna", None).by(&q3()));
    frames.add(shot("/", None));
    let set = frames.events(&Thresholds::default());
    assert_eq!(
        names(&set),
        [
            "From Anna · 20 Sep",
            "Undated · /",
            "Undated · From Anna",
            "Undated · Scans"
        ]
    );
    let members = frames.members(&set);
    assert_eq!(
        members[2],
        [ViewItem::File(FileId(2)), ViewItem::File(FileId(4))]
    );
    let labels: Vec<&str> = set
        .events
        .iter()
        .map(|event| event.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "From Anna",
            "Undated · /",
            "Undated · From Anna",
            "Undated · Scans"
        ],
        "a folder-named event's label is the folder; an Undated one's is its whole name"
    );
    let anna = &set.events[2];
    assert!(anna.undated() && anna.first_day.is_none() && anna.end_ms.is_none());
    assert_eq!(
        anna.id,
        EventId::of(std::path::Path::new("/Pictures/From Anna"), 0)
    );
    assert_eq!(anna.bodies.len(), 1);
    // A dated event's identity is its first file and instant.
    let dated = &set.events[0];
    assert_eq!(
        dated.id,
        EventId::of(
            std::path::Path::new("/Pictures/From Anna/F00003.JPG"),
            midnight(2026, 9, 20)
        )
    );
}

// Names.

#[test]
fn organize_names_a_city_not_its_district() {
    for ((lat, lon), name) in [
        ((52.52, 13.405), "Berlin"),
        ((48.8606, 2.3376), "Paris"),
        ((40.758, -73.9855), "New York City"),
        ((-33.8568, 151.2153), "Sydney"),
        ((35.6595, 139.7005), "Tokyo"),
        ((47.3769, 8.5417), "Zürich"),
        ((47.6595, 9.178), "Konstanz"),
        ((40.6501, -73.9496), "New York City"),
        // A town beside a bigger neighbour, within 10 km, is still itself.
        ((47.546, 9.684), "Lindau"),
        ((47.5031, 9.7471), "Bregenz"),
        ((45.4408, 12.3155), "Venice"),
        ((48.8049, 2.1204), "Versailles"),
        // Far from any bigger place, the nearest names it up to 50 km away.
        ((46.0207, 7.7491), "Sierre"),
    ] {
        let found = Gazetteer.nearest(&GeoPosition {
            lat,
            lon,
            alt_m: None,
        });
        assert_eq!(found.as_deref(), Some(name), "({lat}, {lon})");
    }
    // Open sea and empty country name nothing: past 50 km from any place.
    for (lat, lon) in [(0.0, -30.0), (-25.0, 129.0)] {
        assert_eq!(
            Gazetteer.nearest(&GeoPosition {
                lat,
                lon,
                alt_m: None
            }),
            None
        );
    }
}

#[test]
fn organize_central_position_handles_the_antimeridian() {
    let central = |latitudes: &[f64], longitudes: &[f64]| {
        let position = central_position(&mut latitudes.to_vec(), &mut longitudes.to_vec()).unwrap();
        (position.lat, position.lon)
    };
    assert_eq!(central(&[1.0, 3.0, 2.0], &[10.0, 30.0, 20.0]), (2.0, 20.0));
    assert_eq!(central(&[1.0, 2.0], &[10.0, 20.0]), (1.5, 15.0));
    assert_eq!(
        central(&[0.0, 0.0, 0.0], &[179.0, -179.0, 179.5]),
        (0.0, 179.5)
    );
    assert_eq!(
        central(&[0.0, 0.0, 0.0], &[-179.0, -178.0, 179.0]),
        (0.0, -179.0)
    );
    assert_eq!(central(&[0.0, 0.0], &[178.0, -178.0]), (0.0, 180.0));
    assert!(central_position(&mut [], &mut []).is_none());
    // Fiji's islands either side of the antimeridian name Fiji's side, not the Atlantic's.
    let mut frames = Frames::default();
    let start = midnight(2026, 7, 1) + 9 * HOUR;
    for (at, lon) in [(0, 179.99), (1, -179.99), (2, 179.98)] {
        frames.add(
            shot("/p", Some(start + at * MINUTE))
                .by(&phone())
                .near(-16.8, lon),
        );
    }
    let set = frames.events(&Thresholds::default());
    assert_eq!(set.events.len(), 1);
}

#[test]
fn organize_names_by_a_user_named_folder() {
    let named = |folder: &str| user_folder_name(std::path::Path::new(folder));
    for (folder, name) in [
        ("/Pictures/2026-09-14 Lake", Some("Lake")),
        ("/Pictures/20260914_Lake", Some("Lake")),
        (
            "/Pictures/2026.09.14 - Lake Constance",
            Some("Lake Constance"),
        ),
        ("/Pictures/2026 Iceland", Some("Iceland")),
        ("/Pictures/100 Days", Some("100 Days")),
        ("/Pictures/Lake 2026", Some("Lake 2026")),
        ("/Pictures/iPhone export", Some("iPhone export")),
        ("/Volumes/NIKON Z 8/DCIM/100NZ8_1", None),
        ("/Volumes/SONY/DCIM/100MSDCF", None),
        ("/Volumes/X100VI/DCIM/101_FUJI", None),
        ("/Volumes/X100VI/DCIM", None),
        ("/Pictures/Card dumps/2026-09-12", None),
        ("/Pictures/2026", None),
        ("/Pictures/12.09.2026", None),
        ("/", None),
    ] {
        assert_eq!(named(folder).as_deref(), name, "{folder}");
    }
    // A DCF name is a number from 100 and five characters, eight in all.
    for folder in ["/x/099ABCDE", "/x/100ABCD", "/x/100ABCDEF", "/x/100AB-DE"] {
        assert!(named(folder).is_some(), "{folder}");
    }

    let day = midnight(2026, 9, 14) + 9 * HOUR;
    let lake = |count: i64, other: i64| {
        let mut frames = Frames::default();
        outing(
            &mut frames,
            "/Pictures/2026-09-14 Lake",
            &z8(),
            None,
            day,
            MINUTE,
            count,
        );
        outing(
            &mut frames,
            "/Volumes/Z8/DCIM/100NZ8_1",
            &z8(),
            None,
            day + HOUR,
            MINUTE,
            other,
        );
        frames.events(&Thresholds::default())
    };
    assert_eq!(names(&lake(5, 4)), ["Lake · 14 Sep"]);
    // Half is not more than half.
    assert_eq!(names(&lake(4, 4)), ["14 Sep · NIKON Z 8"]);
    // A place outranks a folder.
    let mut frames = Frames::default();
    outing(
        &mut frames,
        "/Pictures/2026-09-14 Lake",
        &phone(),
        Some(KONSTANZ),
        day,
        MINUTE,
        3,
    );
    assert_eq!(
        names(&frames.events(&Thresholds::default())),
        ["Konstanz · 14 Sep"]
    );
}

#[test]
fn organize_dates_read_as_a_person_writes_them() {
    let day = |year, month, day| LocalDay::from_ymd(year, month, day).unwrap();
    for (first, last, text) in [
        (day(2026, 9, 12), day(2026, 9, 12), "12 Sep"),
        (day(2026, 9, 12), day(2026, 9, 13), "12–13 Sep"),
        (day(2026, 9, 30), day(2026, 10, 2), "30 Sep – 2 Oct"),
        (
            day(2025, 12, 30),
            day(2026, 1, 2),
            "30 Dec 2025 – 2 Jan 2026",
        ),
        (day(2024, 1, 1), day(2024, 12, 31), "1 Jan – 31 Dec"),
    ] {
        assert_eq!(dates(first, last), text);
    }
}

#[test]
fn organize_names_cameras_by_frame_count() {
    let day = midnight(2026, 9, 12) + 9 * HOUR;
    let second_z8 = body("NIKON CORPORATION", "NIKON Z 8", Some("3019377"));
    let mut frames = Frames::default();
    // Two Z 8 bodies count as one label (4 + 3 frames), then the Leica (5), the phone (2) and a
    // frame with no camera, which is not named.
    for (camera, count) in [
        (Some(z8()), 4),
        (Some(second_z8), 3),
        (Some(q3()), 5),
        (Some(phone()), 2),
        (None, 9),
    ] {
        for _ in 0..count {
            let at = day + frames.frames.len() as i64 * MINUTE;
            let shot = shot(CARD, Some(at));
            frames.add(match &camera {
                Some(camera) => shot.by(camera),
                None => shot,
            });
        }
    }
    let set = frames.events(&Thresholds::default());
    assert_eq!(names(&set), ["12 Sep · NIKON Z 8, LEICA Q3 +1"]);
    assert_eq!(set.events[0].label, "NIKON Z 8, LEICA Q3 +1");
    let mut frames = Frames::default();
    frames.add(shot(CARD, Some(day)));
    let set = frames.events(&Thresholds::default());
    assert_eq!(names(&set), ["12 Sep"]);
    assert_eq!(
        set.events[0].label, "",
        "a name of dates alone has no label"
    );
    // Without a gazetteer a positioned event falls back to its folder, then its dates and cameras.
    let mut frames = Frames::default();
    frames.add(
        shot(CARD, Some(day))
            .by(&phone())
            .near(KONSTANZ.0, KONSTANZ.1),
    );
    let set = events(
        &frames.frames,
        &frames.tables,
        &Thresholds::default(),
        &NoPlaces,
    );
    assert_eq!(names(&set), ["12 Sep · Apple iPhone 15 Pro"]);
}

fn names(set: &EventSet) -> Vec<&str> {
    set.events
        .iter()
        .map(|event: &EventGroup| event.name.as_str())
        .collect()
}

// Moments.

/// A frame's exposure at `time` seconds, f/8, ISO 100, 50 mm, with `bias` recorded.
pub(super) fn exposure(time: f32, bias: Option<f32>) -> Exposure {
    Exposure {
        time_s: Some(time),
        f_number: Some(8.0),
        iso: Some(100),
        bias_ev: bias,
        focal_mm: Some(50.0),
        focal_35mm_mm: Some(50.0),
    }
}

/// One body's frames at `offsets` (ms from 10:00) with `exposures`, then the Day › Camera › Moment
/// layout's moments.
fn moments_of(
    offsets: &[i64],
    exposures: &[Exposure],
    thresholds: &Thresholds,
    probe: &dyn BracketProbe,
) -> Vec<Moment> {
    let start = midnight(2026, 9, 12) + 10 * HOUR;
    let mut frames = Frames::default();
    for (offset, exposure) in offsets.iter().zip(exposures) {
        frames.add(
            shot("/c", Some(start + offset))
                .by(&z8())
                .exposed(*exposure),
        );
    }
    frames.group(thresholds, probe).moments
}

fn kinds(moments: &[Moment]) -> Vec<(MomentKind, Option<BracketEvidence>, u32, u32)> {
    moments
        .iter()
        .map(|moment| (moment.kind, moment.evidence, moment.start, moment.len))
        .collect()
}

fn assert_steps(moment: &Moment, expected: &[f32]) {
    assert_eq!(moment.steps_ev.len(), expected.len(), "{moment:?}");
    for (step, expected) in moment.steps_ev.iter().zip(expected) {
        assert!(
            (step - expected).abs() < 0.01,
            "{moment:?} against {expected:?}"
        );
    }
}

/// A probe that must not be asked.
struct Unasked;

impl BracketProbe for Unasked {
    fn measure(&self, frames: &[ViewItem]) -> Option<Vec<f32>> {
        panic!("the probe was asked about {frames:?}");
    }
}

/// A probe that answers one measurement and counts how often it was asked.
struct Answer(Option<Vec<f32>>, Cell<u32>);

impl BracketProbe for Answer {
    fn measure(&self, _: &[ViewItem]) -> Option<Vec<f32>> {
        self.1.set(self.1.get() + 1);
        self.0.clone()
    }
}

#[test]
fn organize_bursts_run_by_speed_and_matched_settings() {
    let same = exposure(1.0 / 500.0, Some(0.0));
    let t = Thresholds::default();
    // Six frames in 1.4 s: one burst, with its span.
    let moments = moments_of(&[0, 280, 560, 840, 1120, 1400], &[same; 6], &t, &NoProbe);
    assert_eq!(kinds(&moments), [(MomentKind::Burst, None, 0, 6)]);
    assert_eq!(moments[0].span_ms, 1400);
    // 1.5 s apart joins only with matching aperture, ISO and focal length.
    assert_eq!(
        kinds(&moments_of(&[0, 1500], &[same; 2], &t, &NoProbe)),
        [(MomentKind::Burst, None, 0, 2)]
    );
    let other_iso = Exposure {
        iso: Some(200),
        ..same
    };
    assert!(moments_of(&[0, 1500], &[same, other_iso], &t, &Unasked).is_empty());
    // A gap of exactly the threshold with subsecond times is not less than it.
    assert!(moments_of(&[500, 1500], &[other_iso, same], &t, &Unasked).is_empty());
    // Singles 6 s apart are no moment.
    assert!(moments_of(&[0, 6000, 12_000], &[same; 3], &t, &Unasked).is_empty());
}

/// A camera that records whole seconds only: a 10 fps burst reads as frames 0 or 1000 ms apart,
/// and stays one run.
#[test]
fn organize_second_resolution_bursts_stay_one_run() {
    let same = exposure(1.0 / 1000.0, None);
    let offsets = [0, 0, 0, 0, 1000, 1000, 1000, 1000, 1000, 1000, 2000, 2000];
    let moments = moments_of(&offsets, &[same; 12], &Thresholds::default(), &NoProbe);
    assert_eq!(kinds(&moments), [(MomentKind::Burst, None, 0, 12)]);
    assert_eq!(moments[0].span_ms, 2000);
    // The same gap between subsecond times is a second, not less than one: two frames apart.
    let unmatched = [Exposure::default(); 3];
    let moments = moments_of(
        &[0, 500, 1500],
        &unmatched,
        &Thresholds::default(),
        &NoProbe,
    );
    assert_eq!(kinds(&moments), [(MomentKind::Burst, None, 0, 2)]);
    // And two whole seconds apart is not a second.
    assert!(
        moments_of(
            &[0, 2000],
            &[Exposure::default(); 2],
            &Thresholds::default(),
            &NoProbe
        )
        .is_empty()
    );
}

/// Nominal third-stop values: 1/200 → 1/250 s is 0.32 EV, 1/50 → 1/60 s 0.26 and 1/13 → 1/15 s
/// 0.21, each a true ⅓ EV; all make brackets. A sixth of a stop does not.
#[test]
fn organize_third_stop_brackets_count_from_nominal_values() {
    let t = Thresholds::default();
    let offsets = [0, 600, 1200];
    for times in [
        [1.0 / 200.0, 1.0 / 250.0, 1.0 / 160.0],
        [1.0 / 60.0, 1.0 / 80.0, 1.0 / 50.0],
        [1.0 / 15.0, 1.0 / 20.0, 1.0 / 13.0],
    ] {
        let run = times.map(|time| exposure(time, Some(0.0)));
        let moments = moments_of(&offsets, &run, &t, &Unasked);
        assert_eq!(
            kinds(&moments),
            [(MomentKind::Bracket, Some(BracketEvidence::Metadata), 0, 3)],
            "{times:?}"
        );
        // Every bias is 0: the metered frame is the median, the first here.
        let steps = &moments[0].steps_ev;
        assert!(
            steps[0] == 0.0 && steps[1] < -0.2 && steps[2] > 0.2,
            "{steps:?}"
        );
    }
    // 1/250 → 1/280 s is a sixth of a stop: a burst.
    let run = [1.0 / 250.0, 1.0 / 280.0, 1.0 / 315.0].map(|time| exposure(time, Some(0.0)));
    assert_eq!(
        kinds(&moments_of(&offsets, &run, &t, &Unasked)),
        [(MomentKind::Burst, None, 0, 3)]
    );
    // −2, 0, +2 EV shot 0 first, with the bias recorded: steps around the 0 frame.
    let run = [
        exposure(1.0 / 60.0, Some(0.0)),
        exposure(1.0 / 240.0, Some(-2.0)),
        exposure(1.0 / 15.0, Some(2.0)),
    ];
    let moments = moments_of(&[0, 900, 1800], &run, &t, &Unasked);
    assert_eq!(
        kinds(&moments),
        [(MomentKind::Bracket, Some(BracketEvidence::Metadata), 0, 3)]
    );
    assert_steps(&moments[0], &[0.0, -2.0, 2.0]);
    assert_eq!(moments[0].span_ms, 1800);
}

/// Bursts whose exposure moves but does not step: auto-exposure drift and repeated exposures. The
/// metadata says, so the previews are not asked.
#[test]
fn organize_drifting_and_repeated_exposures_are_bursts() {
    let t = Thresholds::default();
    let offsets = [0, 200, 400, 600];
    for times in [
        // Continuous drift, as a phone records it.
        [1.0 / 250.0, 1.0 / 262.0, 1.0 / 270.0, 1.0 / 281.0],
        // Nominal values that repeat.
        [1.0 / 250.0, 1.0 / 250.0, 1.0 / 320.0, 1.0 / 320.0],
        [1.0 / 250.0, 1.0 / 320.0, 1.0 / 320.0, 1.0 / 400.0],
    ] {
        let run = times.map(|time| exposure(time, None));
        assert_eq!(
            kinds(&moments_of(&offsets, &run, &t, &Unasked)),
            [(MomentKind::Burst, None, 0, 4)],
            "{times:?}"
        );
    }
    // Ten frames stepping each time are too many for one bracket and repeat no pattern.
    let run: Vec<_> = (0..10)
        .map(|step| exposure(1.0 / 1000.0 * 2f32.powi(step), None))
        .collect();
    let offsets: Vec<i64> = (0..10).map(|at| at * 300).collect();
    assert_eq!(
        kinds(&moments_of(&offsets, &run, &t, &Unasked)),
        [(MomentKind::Burst, None, 0, 10)]
    );
}

/// Bracket after bracket (an HDR panorama): a run that repeats one bracket's pattern is that many
/// brackets, whether the run is longer than a bracket may be or not.
#[test]
fn organize_consecutive_brackets_split_by_their_period() {
    let t = Thresholds::default();
    let bracket = [(1.0 / 60.0, 0.0), (1.0 / 240.0, -2.0), (1.0 / 15.0, 2.0)];
    for repeats in [2, 3, 4] {
        let run: Vec<_> = (0..repeats)
            .flat_map(|_| bracket.map(|(time, bias)| exposure(time, Some(bias))))
            .collect();
        let offsets: Vec<i64> = (0..run.len() as i64).map(|at| at * 700).collect();
        let moments = moments_of(&offsets, &run, &t, &Unasked);
        let expected: Vec<_> = (0..repeats)
            .map(|at| {
                (
                    MomentKind::Bracket,
                    Some(BracketEvidence::Metadata),
                    at * 3,
                    3,
                )
            })
            .collect();
        assert_eq!(kinds(&moments), expected, "{repeats} brackets");
        for moment in &moments {
            assert_steps(moment, &[0.0, -2.0, 2.0]);
            assert_eq!(moment.span_ms, 1400);
        }
    }
    // Two five-frame brackets: ten frames, over the nine a bracket may have.
    let five = [-2.0f32, -1.0, 0.0, 1.0, 2.0];
    let run: Vec<_> = (0..2)
        .flat_map(|_| five.map(|step| exposure(1.0 / 125.0 * 2f32.powf(step), Some(step))))
        .collect();
    let offsets: Vec<i64> = (0..10).map(|at| at * 500).collect();
    let moments = moments_of(&offsets, &run, &t, &Unasked);
    assert_eq!(
        kinds(&moments),
        [
            (MomentKind::Bracket, Some(BracketEvidence::Metadata), 0, 5),
            (MomentKind::Bracket, Some(BracketEvidence::Metadata), 5, 5)
        ]
    );
    assert_steps(&moments[1], &five);
    // A pattern that does not repeat exactly is one burst.
    let mut broken = run.clone();
    broken[7] = exposure(1.0 / 125.0 * 2f32.powf(0.5), Some(0.5));
    assert_eq!(
        kinds(&moments_of(&offsets, &broken, &t, &Unasked)),
        [(MomentKind::Burst, None, 0, 10)]
    );
}

/// When the metadata cannot say, the previews may: through the probe, relative to the median
/// frame, only for a run of bracket size, and only for a step of about ⅔ EV or more.
#[test]
fn organize_brackets_from_previews_come_through_the_probe() {
    let t = Thresholds::default();
    let drone = exposure(1.0 / 250.0, Some(0.0));
    let offsets = [0, 800, 1600];
    let probe = Answer(Some(vec![0.0, 1.0, 2.0]), Cell::new(0));
    let moments = moments_of(&offsets, &[drone; 3], &t, &probe);
    assert_eq!(
        kinds(&moments),
        [(MomentKind::Bracket, Some(BracketEvidence::Previews), 0, 3)]
    );
    assert_steps(&moments[0], &[-1.0, 0.0, 1.0]);
    assert_eq!(probe.1.get(), 1);
    // A measured step of 0.55 EV counts as about ⅔; 0.3 does not.
    let probe = Answer(Some(vec![0.0, -0.55, 0.55]), Cell::new(0));
    assert_steps(
        &moments_of(&offsets, &[drone; 3], &t, &probe)[0],
        &[0.0, -0.55, 0.55],
    );
    for answer in [
        Some(vec![0.0, 0.3, 0.6]),
        Some(vec![0.0, 1.0]),
        Some(vec![0.0, f32::NAN, 1.0]),
        None,
    ] {
        let probe = Answer(answer.clone(), Cell::new(0));
        assert_eq!(
            kinds(&moments_of(&offsets, &[drone; 3], &t, &probe)),
            [(MomentKind::Burst, None, 0, 3)],
            "{answer:?}"
        );
    }
    assert_eq!(
        kinds(&moments_of(&offsets, &[drone; 3], &t, &NoProbe)),
        [(MomentKind::Burst, None, 0, 3)]
    );
    // Ten identical frames are not asked about: no bracket is that long.
    let offsets: Vec<i64> = (0..10).map(|at| at * 100).collect();
    let probe = Answer(Some((0..10).map(|at| at as f32).collect()), Cell::new(0));
    assert_eq!(
        kinds(&moments_of(&offsets, &[drone; 10], &t, &probe)),
        [(MomentKind::Burst, None, 0, 10)]
    );
    assert_eq!(probe.1.get(), 0);
}

/// A view sorted newest first has the same moments, their frames and steps in its own order.
#[test]
fn organize_moments_follow_a_descending_view() {
    let start = midnight(2026, 9, 12) + 10 * HOUR;
    let mut frames = Frames::default();
    let bracket = [(1.0 / 60.0, 0.0), (1.0 / 240.0, -2.0), (1.0 / 15.0, 2.0)];
    for (at, (time, bias)) in bracket.iter().enumerate() {
        frames.add(
            shot("/c", Some(start + at as i64 * 700))
                .by(&z8())
                .exposed(exposure(*time, Some(*bias))),
        );
    }
    for at in 0..4 {
        frames.add(
            shot("/c", Some(start + 10_000 + at * 300))
                .by(&z8())
                .exposed(exposure(1.0 / 500.0, None)),
        );
    }
    order(
        &mut frames.frames,
        &frames.tables,
        Grouping::DayCameraMoment,
        true,
    );
    let layout = group(
        &frames.frames,
        &frames.tables,
        Grouping::DayCameraMoment,
        &Thresholds::default(),
        &NoProbe,
    );
    assert_eq!(
        kinds(&layout.moments),
        [
            (MomentKind::Burst, None, 0, 4),
            (MomentKind::Bracket, Some(BracketEvidence::Metadata), 4, 3)
        ]
    );
    assert_steps(&layout.moments[1], &[2.0, -2.0, 0.0]);
    assert_eq!(
        (layout.moments[0].span_ms, layout.moments[1].span_ms),
        (900, 1400)
    );
}

/// Moments are per body: two bodies' simultaneous bursts are two moments, and a Day-only or
/// ungrouped view has none.
#[test]
fn organize_moments_are_per_body_and_only_in_the_moment_grouping() {
    let start = midnight(2026, 9, 12) + 10 * HOUR;
    let mut frames = Frames::default();
    let second_z8 = body("NIKON CORPORATION", "NIKON Z 8", Some("3019377"));
    for at in 0..6 {
        frames.add(shot("/a", Some(start + at * 150)).by(&z8()));
    }
    for at in 0..4 {
        frames.add(shot("/b", Some(start + 370 + at * 200)).by(&second_z8));
    }
    let layout = frames.group(&Thresholds::default(), &NoProbe);
    assert_eq!(
        kinds(&layout.moments),
        [
            (MomentKind::Burst, None, 0, 6),
            (MomentKind::Burst, None, 6, 4)
        ]
    );
    assert_eq!(layout.cameras.len(), 2);
    for grouping in [Grouping::Day, Grouping::None] {
        order(&mut frames.frames, &frames.tables, grouping, false);
        let layout = group(
            &frames.frames,
            &frames.tables,
            grouping,
            &Thresholds::default(),
            &Unasked,
        );
        assert!(layout.moments.is_empty() && layout.cameras.is_empty());
        assert_eq!(layout.days.len(), usize::from(grouping == Grouping::Day));
    }
}
