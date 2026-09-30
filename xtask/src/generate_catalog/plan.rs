//! The shoot plan: one deterministic model of camera bodies, trips and moments that every
//! `generate-catalog` mode draws from, so the image folders, the index rows and the catalog tell
//! the same kind of story.
//!
//! A plan is a sequence of **events**, the ground truth the event rule (P3 in
//! `docs/design/catalog.md`) must find:
//! photographs sorted by capture time with every gap inside an event under 3 hours and every
//! positioned neighbour within 25 km, and consecutive events separated by a gap over 3 hours or,
//! once, by a jump of over 25 km inside an hour on the same day. A trip of several days keeps
//! shooting through the night (a tripod camera's night frames under 3 hours apart), because the
//! gap rule alone would otherwise split it at every night. Inside an event each body shoots
//! **moments** (P5): singles, bursts (3–8 frames under 1 s apart, one exposure) and brackets
//! (3 frames 1 or 2 EV apart, or 5 frames 1 EV apart, 0.3–1.8 s apart, one aperture, ISO and focal
//! length) of three kinds: with the exposure bias recording each step, with the exposure time
//! alone changing, and with nothing in the metadata changing at all, as a drone writes it. One
//! body's consecutive moments are always at least 6 s apart, so no rule merges them. Undated files,
//! with no capture time at all, are an event of their own in one user-named folder.
//!
//! Events are made one at a time, each from its own random stream, so a plan of a million frames
//! never holds more than one event's frames.
use super::clock::{CENTRAL, Date, HOUR, LocalTime, MINUTE, SECOND, Zone};
use super::random::Random;

/// Where a body's files are kept, which decides their folder in every mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    /// Read in place from the camera's card: `DCIM/<folder number><suffix>/`.
    Card { suffix: &'static str },
    /// Copied from its card into a dump folder named after the trip's first day.
    Dump,
    /// Copied into a user-named folder: `<first day> <name>`.
    Named,
    /// Exported from a phone into one folder.
    Phone,
}

/// How a bracket's frames record their exposure steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BracketKind {
    /// The exposure time changes and the exposure bias records each step.
    WithBias,
    /// The exposure time changes and the exposure bias is 0 on every frame.
    WithoutBias,
    /// Nothing in the metadata changes; only the pixels do (the drone case).
    MetadataLess,
}

impl BracketKind {
    /// What a correct grouping reads the bracket from: `metadata` or `previews`.
    pub fn evidence(self) -> &'static str {
        match self {
            BracketKind::WithBias | BracketKind::WithoutBias => "metadata",
            BracketKind::MetadataLess => "previews",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MomentKind {
    Single,
    Burst,
    Bracket(BracketKind),
}

impl MomentKind {
    pub fn name(self) -> &'static str {
        match self {
            MomentKind::Single => "single",
            MomentKind::Burst => "burst",
            MomentKind::Bracket(_) => "bracket",
        }
    }
}

/// One camera body.
pub struct Body {
    pub make: &'static str,
    pub model: &'static str,
    /// `BodySerialNumber`; a phone writes none.
    pub serial: Option<&'static str>,
    pub lens: &'static str,
    /// The file name before its number: `DSC_` names `DSC_0001`.
    pub prefix: &'static str,
    /// Whether the folder number follows the prefix, as Leica's `L1003206` does.
    pub folder_in_name: bool,
    /// The number of the body's first file in the plan.
    pub first_number: u32,
    pub storage: Storage,
    /// Whether it writes `OffsetTimeOriginal`.
    pub writes_offset: bool,
    /// Whether every frame carries a GPS position.
    pub positioned: bool,
    /// Whether its EXIF is little-endian (`II`).
    pub little_endian: bool,
    pub bracket: Option<BracketKind>,
    /// Whether a bracket shoots its base exposure first (`0, −, +`) rather than in order.
    pub zero_first: bool,
    /// The lens's focal lengths in micrometres, its zoom range's ends (equal for a fixed lens).
    pub focal: (u32, u32),
    /// The 35 mm equivalent's factor, in hundredths.
    pub crop: u32,
    /// F-numbers in hundredths.
    pub apertures: &'static [u32],
    pub isos: &'static [u16],
    /// Whether it shoots a trip's night frames from a tripod.
    pub night: bool,
}

impl Body {
    /// `make|model|serial`, the body key the manifest names (an empty serial when none is
    /// written).
    pub fn key(&self) -> String {
        format!("{}|{}|{}", self.make, self.model, self.serial.unwrap_or(""))
    }

    /// The folder number and file stem of the body's `count`th file: numbers run 0001–9999 and
    /// then continue in the next folder, as cameras do.
    pub fn file_stem(&self, count: u32) -> (u32, String) {
        let index = count - 1;
        let folder = 100 + index / 9999;
        let number = index % 9999 + 1;
        let stem = if self.folder_in_name {
            format!("{}{folder}{number:04}", self.prefix)
        } else {
            format!("{}{number:04}", self.prefix)
        };
        (folder, stem)
    }
}

pub const Z8_A: usize = 0;
pub const Z8_B: usize = 1;
pub const LEICA: usize = 2;
pub const FUJI: usize = 3;
pub const DRONE: usize = 4;
pub const PHONE: usize = 5;

const NIKON_APERTURES: &[u32] = &[400, 450, 500, 560, 630, 710, 800, 900, 1000, 1100];
const NIKON_ISOS: &[u16] = &[64, 100, 200, 400, 800, 1600, 3200, 6400];

pub const BODIES: [Body; 6] = [
    Body {
        make: "NIKON CORPORATION",
        model: "NIKON Z 8",
        serial: Some("3012845"),
        lens: "NIKKOR Z 24-120mm f/4 S",
        prefix: "DSC_",
        folder_in_name: false,
        first_number: 1,
        storage: Storage::Card { suffix: "NZ8_1" },
        writes_offset: true,
        positioned: false,
        little_endian: false,
        bracket: Some(BracketKind::WithBias),
        zero_first: true,
        focal: (24_000, 120_000),
        crop: 100,
        apertures: NIKON_APERTURES,
        isos: NIKON_ISOS,
        night: true,
    },
    Body {
        make: "NIKON CORPORATION",
        model: "NIKON Z 8",
        serial: Some("3019377"),
        lens: "NIKKOR Z 24-120mm f/4 S",
        prefix: "DSC_",
        folder_in_name: false,
        first_number: 1,
        storage: Storage::Card { suffix: "NZ8_2" },
        writes_offset: true,
        positioned: false,
        little_endian: false,
        bracket: Some(BracketKind::WithBias),
        zero_first: true,
        focal: (24_000, 120_000),
        crop: 100,
        apertures: NIKON_APERTURES,
        isos: NIKON_ISOS,
        night: true,
    },
    Body {
        make: "LEICA CAMERA AG",
        model: "LEICA Q3",
        serial: Some("5561203"),
        lens: "SUMMILUX 1:1.7/28 ASPH.",
        prefix: "L",
        folder_in_name: true,
        first_number: 3201,
        storage: Storage::Dump,
        writes_offset: false,
        positioned: false,
        little_endian: true,
        bracket: Some(BracketKind::WithoutBias),
        zero_first: false,
        focal: (28_000, 28_000),
        crop: 100,
        apertures: &[170, 200, 280, 400, 560, 800, 1100],
        isos: &[100, 200, 400, 800, 1600, 3200],
        night: true,
    },
    Body {
        make: "FUJIFILM",
        model: "X100VI",
        serial: Some("54A00817"),
        lens: "FUJINON 23mm F2",
        prefix: "DSCF",
        folder_in_name: false,
        first_number: 1,
        storage: Storage::Named,
        writes_offset: false,
        positioned: false,
        little_endian: true,
        bracket: Some(BracketKind::WithBias),
        zero_first: false,
        focal: (23_000, 23_000),
        crop: 152,
        apertures: &[200, 280, 400, 560, 800, 1100],
        isos: &[125, 160, 200, 400, 800, 1600, 3200],
        night: true,
    },
    Body {
        make: "DJI",
        model: "FC3411",
        serial: Some("0K8TH4F0021337"),
        lens: "22mm f/2.8",
        prefix: "DJI_",
        folder_in_name: false,
        first_number: 1,
        storage: Storage::Dump,
        writes_offset: false,
        positioned: true,
        little_endian: true,
        bracket: Some(BracketKind::MetadataLess),
        zero_first: false,
        focal: (8_400, 8_400),
        crop: 262,
        apertures: &[280],
        isos: &[100, 200, 400, 800],
        night: false,
    },
    Body {
        make: "Apple",
        model: "iPhone 15 Pro",
        serial: None,
        lens: "iPhone 15 Pro back triple camera 6.765mm f/1.78",
        prefix: "IMG_",
        folder_in_name: false,
        first_number: 4201,
        storage: Storage::Phone,
        writes_offset: true,
        positioned: true,
        little_endian: false,
        bracket: None,
        zero_first: false,
        focal: (6_765, 6_765),
        crop: 355,
        apertures: &[178],
        isos: &[50, 64, 80, 100, 125, 200, 400, 800],
        night: false,
    },
];

/// A place a trip goes to: its position in microdegrees, its elevation and its zone.
pub struct Place {
    pub name: &'static str,
    pub latitude: i32,
    pub longitude: i32,
    pub elevation: i32,
    pub zone: Zone,
}

const fn place(name: &'static str, latitude: i32, longitude: i32, elevation: i32) -> Place {
    Place {
        name,
        latitude,
        longitude,
        elevation,
        zone: CENTRAL,
    }
}

pub const KONSTANZ: Place = place("Konstanz", 47_660_000, 9_175_000, 405);
pub const REICHENAU: Place = place("Reichenau", 47_689_000, 9_062_000, 398);
pub const ZURICH: Place = place("Zürich", 47_376_900, 8_541_700, 408);
pub const LUZERN: Place = place("Luzern", 47_050_200, 8_309_300, 435);
pub const LINDAU: Place = place("Lindau", 47_546_000, 9_684_000, 400);

/// A moment a spec asks for by name, beside the ones drawn at random.
#[derive(Clone, Copy, Debug)]
pub enum Required {
    Single(usize),
    Burst(usize, u8),
    /// A body, its frame count and its step in EV.
    Bracket(usize, u8, u8),
    /// Two bodies' bursts at once, the second starting 0.37 s after the first: grouping by make
    /// and model alone would merge them.
    Pair(usize, u8, usize, u8),
}

impl Required {
    fn frames(self) -> u32 {
        match self {
            Required::Single(_) => 1,
            Required::Burst(_, n) | Required::Bracket(_, n, _) => u32::from(n),
            Required::Pair(_, a, _, b) => u32::from(a) + u32::from(b),
        }
    }

    fn moments(self) -> u32 {
        match self {
            Required::Pair(..) => 2,
            _ => 1,
        }
    }
}

/// How an event's first day starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// In the morning of its first date.
    Morning,
    /// 35–55 minutes after the previous event's last frame, the same day.
    AfterPrevious,
}

/// What one event is to hold, before its frames are drawn.
pub struct Spec {
    pub label: String,
    /// Where it happened; `None` for the undated event.
    pub place: Option<&'static Place>,
    /// The user-named folder's name: the undated event's whole name, otherwise the part after
    /// the first date (the place's name when `None`).
    pub folder: Option<&'static str>,
    pub start: Date,
    pub days: u8,
    /// The bodies and their weights for the moments drawn at random.
    pub bodies: Vec<(usize, u32)>,
    pub required: Vec<Required>,
    /// A single by this body opens the event.
    pub first: Option<usize>,
    /// A single by this body closes it.
    pub last: Option<usize>,
    pub begins: Start,
    /// The longest one day's shooting may run.
    pub cap: Option<i64>,
    /// Its share when a total is apportioned.
    pub weight: u32,
    /// Its frame count, set when the plan is apportioned.
    pub budget: u32,
}

/// Night frames between two days of one event.
const NIGHT_FRAMES: u32 = 4;
/// The fewest moments a day of a multi-day event holds, so its gaps stay under 3 hours from
/// morning to evening.
const MOMENTS_A_DAY: u32 = 6;
/// The longest gap between moments inside an event.
const MOST_GAP: i64 = 2 * HOUR + 40 * MINUTE;
/// The shortest gap between moments: well over P5's 2 s run threshold.
const LEAST_GAP: i64 = 6 * SECOND;
/// The fewest files of the undated event.
const UNDATED_FILES: u32 = 3;
/// The most frames the September trips hold: each day's moments stay 6 s apart and inside the
/// day with room to spare.
pub const MOST_IMAGES: u32 = 5_000;

impl Spec {
    fn undated(&self) -> bool {
        self.place.is_none()
    }

    fn edges(&self) -> u32 {
        u32::from(self.first.is_some()) + u32::from(self.last.is_some())
    }

    fn fixed_frames(&self) -> u32 {
        self.required.iter().map(|r| r.frames()).sum::<u32>()
            + self.edges()
            + NIGHT_FRAMES * u32::from(self.days.saturating_sub(1))
    }

    /// The fewest frames that hold every required moment and keep each day's gaps in bounds.
    pub fn minimum(&self) -> u32 {
        if self.undated() {
            return UNDATED_FILES;
        }
        let moments = self.required.iter().map(|r| r.moments()).sum::<u32>() + self.edges();
        let needed = if self.days > 1 {
            MOMENTS_A_DAY * u32::from(self.days)
        } else {
            1
        };
        self.fixed_frames() + needed.saturating_sub(moments)
    }
}

/// A GPS position as written: microdegrees and decimetres above sea level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gps {
    pub latitude: i32,
    pub longitude: i32,
    pub altitude: i32,
}

/// One frame's exposure, as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exposure {
    /// Seconds, as a reduced fraction.
    pub time: (u32, u32),
    /// Hundredths.
    pub f_number: u32,
    pub iso: u16,
    /// Thirds of an EV.
    pub bias: i32,
    /// Micrometres.
    pub focal: u32,
    pub focal_35: u16,
}

impl Exposure {
    /// The exposure time scaled by `2^step`, reduced.
    fn stepped(mut self, step: i8) -> Self {
        let (mut num, mut den) = self.time;
        if step >= 0 {
            num <<= step;
        } else {
            den <<= -step;
        }
        let divisor = gcd(num, den);
        self.time = (num / divisor, den / divisor);
        self
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// What the image of a frame shows: its moment's scene, by the moment's own seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneKey {
    pub seed: u64,
    pub night: bool,
    /// How far the burst's subject has moved: the frame's index in a burst, 0 otherwise.
    pub shift: u8,
}

/// One photograph in the plan.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// An index into [`BODIES`].
    pub body: usize,
    /// The body's running file count, from its first number.
    pub count: u32,
    /// The moment's index within the event.
    pub moment: u32,
    pub kind: MomentKind,
    /// The frame's index within its moment.
    pub index: u8,
    /// Its step in EV within a bracket.
    pub step: Option<i8>,
    /// The camera's clock, `None` when undated.
    pub time: Option<LocalTime>,
    /// The zone's offset at the capture, in minutes (written only by bodies that write one).
    pub offset: i16,
    pub gps: Option<Gps>,
    pub exposure: Exposure,
    pub scene: SceneKey,
}

impl Frame {
    pub fn body(&self) -> &'static Body {
        &BODIES[self.body]
    }

    /// The offset as written, or `None` for a body that writes none.
    pub fn written_offset(&self) -> Option<i16> {
        (self.body().writes_offset && self.time.is_some()).then_some(self.offset)
    }

    /// The capture's UTC milliseconds.
    pub fn utc(&self) -> Option<i64> {
        self.time.map(|time| time.utc(self.offset))
    }
}

/// One ground-truth event with its frames in moment order: moments in capture order, and a moment's
/// frames in order (the two bursts of a pair overlap in time, one after the other here).
pub struct Event {
    pub label: String,
    /// Where it happened, whether or not any frame carries a position; `None` for the undated
    /// event.
    pub place: Option<&'static Place>,
    /// The user-named folder its `Named` bodies' files go into.
    pub folder: String,
    pub start: Date,
    pub frames: Vec<Frame>,
}

impl Event {
    /// Whether any frame carries a position.
    pub fn positioned(&self) -> bool {
        self.frames.iter().any(|frame| frame.gps.is_some())
    }

    /// The camera-local dates it covers.
    pub fn days(&self) -> Vec<Date> {
        let mut days: Vec<Date> = self
            .frames
            .iter()
            .filter_map(|frame| frame.time.map(LocalTime::date))
            .collect();
        days.sort();
        days.dedup();
        days
    }

    pub fn moment_label(&self, moment: u32) -> String {
        format!("{}/{moment:04}", self.label)
    }
}

/// The trips of September 2026 every mode starts from: a two-day Konstanz trip with both Nikon
/// bodies, the Leica and the phone; a day at the lake with no GPS at all; Zürich and then Luzern on
/// one day, over 25 km apart inside the hour; a three-day Lindau trip with the drone; and the
/// undated files in "From Anna".
pub fn september() -> Vec<Spec> {
    let spec = |label: &str, place, start, days, bodies: &[(usize, u32)], weight| Spec {
        label: label.into(),
        place,
        folder: None,
        start,
        days,
        bodies: bodies.to_vec(),
        required: Vec::new(),
        first: None,
        last: None,
        begins: Start::Morning,
        cap: None,
        weight,
        budget: 0,
    };
    vec![
        Spec {
            required: vec![
                Required::Pair(Z8_A, 6, Z8_B, 4),
                Required::Bracket(Z8_A, 3, 2),
                Required::Bracket(LEICA, 3, 1),
                Required::Burst(LEICA, 5),
                Required::Single(PHONE),
            ],
            ..spec(
                "konstanz",
                Some(&KONSTANZ),
                Date::new(2026, 9, 12),
                2,
                &[(Z8_A, 4), (Z8_B, 2), (LEICA, 3), (PHONE, 2)],
                32,
            )
        },
        Spec {
            folder: Some("Lake"),
            required: vec![
                Required::Bracket(FUJI, 5, 1),
                Required::Burst(FUJI, 4),
                Required::Single(Z8_A),
            ],
            ..spec(
                "lake",
                Some(&REICHENAU),
                Date::new(2026, 9, 14),
                1,
                &[(FUJI, 4), (Z8_A, 1)],
                14,
            )
        },
        Spec {
            required: vec![Required::Bracket(LEICA, 3, 2), Required::Burst(PHONE, 3)],
            last: Some(PHONE),
            cap: Some(4 * HOUR + 30 * MINUTE),
            ..spec(
                "zurich",
                Some(&ZURICH),
                Date::new(2026, 9, 16),
                1,
                &[(LEICA, 3), (PHONE, 2)],
                10,
            )
        },
        Spec {
            required: vec![Required::Burst(LEICA, 4)],
            first: Some(PHONE),
            begins: Start::AfterPrevious,
            cap: Some(8 * HOUR),
            ..spec(
                "luzern",
                Some(&LUZERN),
                Date::new(2026, 9, 16),
                1,
                &[(LEICA, 2), (PHONE, 2)],
                12,
            )
        },
        Spec {
            required: vec![
                Required::Bracket(DRONE, 3, 2),
                Required::Bracket(DRONE, 5, 1),
                Required::Burst(Z8_A, 5),
            ],
            ..spec(
                "lindau",
                Some(&LINDAU),
                Date::new(2026, 9, 18),
                3,
                &[(Z8_A, 3), (DRONE, 2), (PHONE, 2)],
                32,
            )
        },
        Spec {
            folder: Some("From Anna"),
            ..spec("undated", None, Date::new(2026, 9, 20), 1, &[(FUJI, 1)], 3)
        },
    ]
}

/// Shares `total` among `specs` by weight, never below a spec's minimum. Refuses a total under the
/// sum of the minimums.
pub fn apportion(specs: &mut [Spec], total: u32) -> Result<(), String> {
    let minimum: u32 = specs.iter().map(Spec::minimum).sum();
    if total < minimum {
        return Err(format!(
            "{total} is too few: the plan needs at least {minimum} frames to hold every event's \
             required moments"
        ));
    }
    let weights: u64 = specs.iter().map(|spec| u64::from(spec.weight)).sum();
    for spec in specs.iter_mut() {
        let share = (u64::from(total) * u64::from(spec.weight) / weights) as u32;
        spec.budget = share.max(spec.minimum());
    }
    let mut sum: u32 = specs.iter().map(|spec| spec.budget).sum();
    // Take any excess from the largest surplus, then give any shortfall to the heaviest spec.
    while sum > total {
        let spec = specs
            .iter_mut()
            .max_by_key(|spec| spec.budget - spec.minimum())
            .expect("a spec");
        let take = (sum - total).min(spec.budget - spec.minimum());
        spec.budget -= take;
        sum -= take;
    }
    if sum < total {
        specs
            .iter_mut()
            .max_by_key(|spec| spec.weight)
            .expect("a spec")
            .budget += total - sum;
    }
    Ok(())
}

/// A plan: its specs, made into events one at a time.
pub struct Plan {
    seed: u64,
    specs: std::vec::IntoIter<Spec>,
    made: u64,
    counters: [u32; BODIES.len()],
    previous_end: Option<LocalTime>,
}

impl Plan {
    pub fn new(seed: u64, specs: Vec<Spec>) -> Self {
        Plan {
            seed,
            specs: specs.into_iter(),
            made: 0,
            counters: BODIES.map(|body| body.first_number - 1),
            previous_end: None,
        }
    }

    /// The September trips with `total` frames between them, at most [`MOST_IMAGES`].
    pub fn images(seed: u64, total: u32) -> Result<Self, String> {
        if total > MOST_IMAGES {
            return Err(format!(
                "{total} is too many: the September trips hold at most {MOST_IMAGES} frames"
            ));
        }
        let mut specs = september();
        apportion(&mut specs, total)?;
        Ok(Plan::new(seed, specs))
    }
}

impl Iterator for Plan {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        let spec = self.specs.next()?;
        let mut maker = Maker {
            random: Random::stream(self.seed, self.made),
            spec: &spec,
            counters: &mut self.counters,
            frames: Vec::with_capacity(spec.budget as usize),
            moment: 0,
        };
        maker.make(self.previous_end);
        let frames = maker.frames;
        self.made += 1;
        if let Some(end) = frames.iter().filter_map(|frame| frame.time).max() {
            self.previous_end = Some(end);
        }
        let folder = match (spec.place, spec.folder) {
            (None, folder) => folder.unwrap_or("Undated").to_owned(),
            (Some(place), folder) => format!("{} {}", spec.start, folder.unwrap_or(place.name)),
        };
        Some(Event {
            label: spec.label,
            place: spec.place,
            folder,
            start: spec.start,
            frames,
        })
    }
}

/// A moment before its time is known.
#[derive(Clone, Copy, Debug)]
enum Draw {
    Single(usize),
    Burst(usize, u8),
    Bracket(usize, u8, u8),
    Pair(usize, u8, usize, u8),
    Night(usize),
}

impl Draw {
    fn frames(self) -> u32 {
        match self {
            Draw::Single(_) | Draw::Night(_) => 1,
            Draw::Burst(_, n) | Draw::Bracket(_, n, _) => u32::from(n),
            Draw::Pair(_, a, _, b) => u32::from(a) + u32::from(b),
        }
    }
}

impl From<Required> for Draw {
    fn from(required: Required) -> Draw {
        match required {
            Required::Single(body) => Draw::Single(body),
            Required::Burst(body, n) => Draw::Burst(body, n),
            Required::Bracket(body, n, step) => Draw::Bracket(body, n, step),
            Required::Pair(a, na, b, nb) => Draw::Pair(a, na, b, nb),
        }
    }
}

/// A frame of a moment with its time relative to the moment's start.
struct Draft {
    /// 0, or 1 for the second body of a pair.
    part: u32,
    offset: i64,
    frame: Frame,
}

/// Makes one event's frames.
struct Maker<'a> {
    random: Random,
    spec: &'a Spec,
    counters: &'a mut [u32; BODIES.len()],
    frames: Vec<Frame>,
    moment: u32,
}

const DAY_TIMES: &[(u32, u32)] = &[
    (1, 4000),
    (1, 2000),
    (1, 1000),
    (1, 500),
    (1, 250),
    (1, 125),
    (1, 60),
    (1, 30),
    (1, 15),
];
/// Bracket base times, from which two EV either way stays a real shutter speed.
const BRACKET_TIMES: &[(u32, u32)] = &[(1, 1000), (1, 500), (1, 250), (1, 125), (1, 60)];
const NIGHT_TIMES: &[(u32, u32)] = &[(15, 1), (20, 1), (25, 1), (30, 1)];

impl Maker<'_> {
    fn make(&mut self, previous_end: Option<LocalTime>) {
        if self.spec.undated() {
            let body = self.spec.bodies[0].0;
            for _ in 0..self.spec.budget {
                let drafts = self.realize(Draw::Single(body), false);
                self.place(drafts, None);
            }
            return;
        }
        let days = self.draws();
        let mut start = match self.spec.begins {
            Start::AfterPrevious => {
                previous_end.expect("an event that follows another")
                    + self.random.between(35 * MINUTE, 55 * MINUTE)
            }
            Start::Morning if self.spec.days > 1 => {
                self.spec.start.at(7, 30) + self.random.between(0, 10 * MINUTE)
            }
            Start::Morning => self.spec.start.at(8, 0) + self.random.between(0, 2 * HOUR),
        };
        let day_count = days.len();
        for (day, draws) in days.into_iter().enumerate() {
            let end = self.day(start, &draws);
            if day + 1 < day_count {
                let next = self.spec.start.add_days(day as i64 + 1).at(7, 30)
                    + self.random.between(0, 10 * MINUTE);
                self.night(end, next);
                start = next;
            }
        }
    }

    /// The event's moments, split into its days.
    fn draws(&mut self) -> Vec<Vec<Draw>> {
        let spec = self.spec;
        let mut remaining = spec.budget - spec.fixed_frames();
        let mut draws = Vec::new();
        while remaining > 0 {
            let draw = self.extra(remaining);
            remaining -= draw.frames();
            draws.push(draw);
        }
        let required: u32 = spec.required.iter().map(|r| r.moments()).sum::<u32>() + spec.edges();
        let needed = if spec.days > 1 {
            MOMENTS_A_DAY * u32::from(spec.days)
        } else {
            1
        };
        // A day of a multi-day event needs enough moments to span it: break the largest drawn
        // moments into singles until there are.
        while (draws.len() as u32) + required < needed {
            let (at, largest) = draws
                .iter()
                .enumerate()
                .max_by_key(|(_, draw)| draw.frames())
                .map(|(at, draw)| (at, *draw))
                .expect("the minimum leaves room for enough moments");
            let body = match largest {
                Draw::Burst(body, _) | Draw::Bracket(body, ..) => body,
                _ => unreachable!("the minimum leaves room for enough moments"),
            };
            draws.splice(at..=at, (0..largest.frames()).map(|_| Draw::Single(body)));
        }
        for &required in &spec.required {
            let at = self.random.between(0, draws.len() as i64) as usize;
            draws.insert(at, required.into());
        }
        if let Some(body) = spec.first {
            draws.insert(0, Draw::Single(body));
        }
        if let Some(body) = spec.last {
            draws.push(Draw::Single(body));
        }
        let days = usize::from(spec.days.max(1));
        let mut split = Vec::with_capacity(days);
        let mut rest = draws.into_iter();
        let total = rest.len();
        for day in 0..days {
            let count = total / days + usize::from(day < total % days);
            split.push(rest.by_ref().take(count).collect());
        }
        split
    }

    /// One moment drawn at random, of at most `remaining` frames.
    fn extra(&mut self, remaining: u32) -> Draw {
        let weights: Vec<u32> = self.spec.bodies.iter().map(|&(_, w)| w).collect();
        let body = self.spec.bodies[self.random.weighted(&weights)].0;
        let (bracket, burst) = match BODIES[body].bracket {
            None => (0.0, 0.2),
            Some(BracketKind::MetadataLess) => (0.35, 0.0),
            Some(_) => (0.13, 0.25),
        };
        let roll = self.random.unit();
        if roll < bracket && remaining >= 3 {
            if remaining >= 5 && self.random.chance(0.3) {
                Draw::Bracket(body, 5, 1)
            } else {
                Draw::Bracket(body, 3, if self.random.chance(0.5) { 1 } else { 2 })
            }
        } else if roll < bracket + burst && remaining >= 3 {
            Draw::Burst(
                body,
                self.random.between(3, 8.min(i64::from(remaining))) as u8,
            )
        } else {
            Draw::Single(body)
        }
    }

    /// Lays out one day's moments from `start` and returns the time of its last frame.
    fn day(&mut self, start: LocalTime, draws: &[Draw]) -> LocalTime {
        let moments: Vec<Vec<Draft>> = draws
            .iter()
            .map(|&draw| self.realize(draw, false))
            .collect();
        let durations: Vec<i64> = moments
            .iter()
            .map(|drafts| drafts.iter().map(|d| d.offset).max().unwrap_or(0))
            .collect();
        let gaps = moments.len().saturating_sub(1);
        let window = if self.spec.days > 1 {
            13 * HOUR
        } else {
            (gaps as i64 * 80 * MINUTE).min(self.spec.cap.unwrap_or(12 * HOUR))
        };
        let free = (window - durations.iter().sum::<i64>()).max(0);
        let weights: Vec<f64> = (0..gaps).map(|_| self.random.uniform(0.3, 1.7)).collect();
        let gaps = spread(free, &weights, MOST_GAP);
        let mut time = start;
        let mut end = start;
        for (index, drafts) in moments.into_iter().enumerate() {
            end = time + durations[index];
            self.place(drafts, Some(time));
            if let Some(&gap) = gaps.get(index) {
                time = end + gap.max(LEAST_GAP);
            }
        }
        end
    }

    /// Four night frames spaced evenly between `end` and `next`.
    fn night(&mut self, end: LocalTime, next: LocalTime) {
        let body = self
            .spec
            .bodies
            .iter()
            .map(|&(body, _)| body)
            .find(|&body| BODIES[body].night)
            .unwrap_or(self.spec.bodies[0].0);
        let span = next.0 - end.0;
        for index in 1..=i64::from(NIGHT_FRAMES) {
            let drafts = self.realize(Draw::Night(body), true);
            let at = end
                + span * index / (i64::from(NIGHT_FRAMES) + 1)
                + self.random.between(-2 * MINUTE, 2 * MINUTE);
            self.place(drafts, Some(at));
        }
    }

    /// Gives a moment's drafts their times and file numbers and appends them.
    fn place(&mut self, drafts: Vec<Draft>, start: Option<LocalTime>) {
        let parts = drafts.iter().map(|d| d.part).max().unwrap_or(0) + 1;
        for draft in drafts {
            let mut frame = draft.frame;
            frame.moment = self.moment + draft.part;
            frame.time = start.map(|start| start + draft.offset);
            if let (Some(time), Some(place)) = (frame.time, self.spec.place) {
                frame.offset = place.zone.offset(time.date());
            }
            self.counters[frame.body] += 1;
            frame.count = self.counters[frame.body];
            self.frames.push(frame);
        }
        self.moment += parts;
    }

    fn settings(&mut self, body: usize, night: bool) -> Exposure {
        let b = &BODIES[body];
        let focal = if b.focal.0 == b.focal.1 {
            b.focal.0
        } else {
            self.random
                .between(i64::from(b.focal.0 / 1000), i64::from(b.focal.1 / 1000))
                as u32
                * 1000
        };
        let focal_35 = ((u64::from(focal) * u64::from(b.crop) + 50_000) / 100_000) as u16;
        if night {
            return Exposure {
                time: *self.random.pick(NIGHT_TIMES),
                f_number: b.apertures[0],
                iso: *self.random.pick(&b.isos[b.isos.len() - 3..]),
                bias: 0,
                focal,
                focal_35,
            };
        }
        let bias = if self.random.chance(0.2) {
            *self.random.pick(&[-2, -1, 1, 2])
        } else {
            0
        };
        Exposure {
            time: *self.random.pick(DAY_TIMES),
            f_number: *self.random.pick(b.apertures),
            iso: *self.random.pick(&b.isos[..b.isos.len().div_ceil(2)]),
            bias,
            focal,
            focal_35,
        }
    }

    fn gps(&mut self, body: usize) -> Option<Gps> {
        let place = self.spec.place?;
        if !BODIES[body].positioned {
            return None;
        }
        let lift = if body == DRONE {
            self.random.between(400, 1200)
        } else {
            self.random.between(0, 250)
        };
        Some(Gps {
            latitude: place.latitude + self.random.between(-18_000, 18_000) as i32,
            longitude: place.longitude + self.random.between(-25_000, 25_000) as i32,
            altitude: place.elevation * 10 + lift as i32,
        })
    }

    /// A moment's frames, timed from its start.
    fn realize(&mut self, draw: Draw, night: bool) -> Vec<Draft> {
        let frame = |body: usize, kind, index: u8, exposure, scene, gps| Frame {
            body,
            count: 0,
            moment: 0,
            kind,
            index,
            step: None,
            time: None,
            offset: 0,
            gps,
            exposure,
            scene,
        };
        let mut drafts = Vec::new();
        match draw {
            Draw::Single(body) | Draw::Night(body) => {
                let exposure = self.settings(body, night);
                let scene = self.scene(night);
                let gps = self.gps(body);
                drafts.push(Draft {
                    part: 0,
                    offset: 0,
                    frame: frame(body, MomentKind::Single, 0, exposure, scene, gps),
                });
            }
            Draw::Burst(body, count) => {
                drafts = self.burst(body, count, 0, 0, (80, 400));
            }
            Draw::Pair(a, count_a, b, count_b) => {
                drafts = self.burst(a, count_a, 0, 0, (100, 200));
                drafts.extend(self.burst(b, count_b, 1, 370, (120, 300)));
            }
            Draw::Bracket(body, count, step) => {
                let b = &BODIES[body];
                let kind = b.bracket.expect("a bracketing body");
                let mut base = self.settings(body, false);
                base.time = *self.random.pick(BRACKET_TIMES);
                base.bias = 0;
                let scene = self.scene(false);
                let gps = self.gps(body);
                let steps: &[i8] = match (count, b.zero_first) {
                    (5, true) => &[0, -1, 1, -2, 2],
                    (5, false) => &[-2, -1, 0, 1, 2],
                    (_, true) => &[0, -1, 1],
                    (_, false) => &[-1, 0, 1],
                };
                let mut offset = 0;
                for (index, &unit) in steps.iter().enumerate() {
                    let ev = if count == 5 { unit } else { unit * step as i8 };
                    let exposure = match kind {
                        BracketKind::WithBias => Exposure {
                            bias: i32::from(ev) * 3,
                            ..base.stepped(ev)
                        },
                        BracketKind::WithoutBias => base.stepped(ev),
                        BracketKind::MetadataLess => base,
                    };
                    let mut draft = frame(
                        body,
                        MomentKind::Bracket(kind),
                        index as u8,
                        exposure,
                        scene,
                        gps,
                    );
                    draft.step = Some(ev);
                    drafts.push(Draft {
                        part: 0,
                        offset,
                        frame: draft,
                    });
                    offset += self.random.between(300, 1800);
                }
            }
        }
        drafts
    }

    fn burst(
        &mut self,
        body: usize,
        count: u8,
        part: u32,
        start: i64,
        interval: (i64, i64),
    ) -> Vec<Draft> {
        let exposure = self.settings(body, false);
        let scene = self.scene(false);
        let gps = self.gps(body);
        let mut offset = start;
        (0..count)
            .map(|index| {
                let draft = Draft {
                    part,
                    offset,
                    frame: Frame {
                        body,
                        count: 0,
                        moment: 0,
                        kind: MomentKind::Burst,
                        index,
                        step: None,
                        time: None,
                        offset: 0,
                        gps,
                        exposure,
                        scene: SceneKey {
                            shift: index,
                            ..scene
                        },
                    },
                };
                offset += if self.random.chance(0.1) {
                    self.random.between(interval.1, 950)
                } else {
                    self.random.between(interval.0, interval.1)
                };
                draft
            })
            .collect()
    }

    fn scene(&mut self, night: bool) -> SceneKey {
        SceneKey {
            seed: self.random.next_u64(),
            night,
            shift: 0,
        }
    }
}

/// Shares `total` among gaps in proportion to `weights`, none over `most`, pouring what a full gap
/// cannot take into the others.
fn spread(total: i64, weights: &[f64], most: i64) -> Vec<i64> {
    let mut gaps = vec![0; weights.len()];
    let mut open: Vec<usize> = (0..weights.len()).collect();
    let mut left = total;
    while left > 0 && !open.is_empty() {
        let sum: f64 = open.iter().map(|&i| weights[i]).sum();
        let mut still = Vec::with_capacity(open.len());
        let mut used = 0;
        for &i in &open {
            let share = (left as f64 * weights[i] / sum) as i64;
            let room = most - gaps[i];
            if share >= room {
                gaps[i] = most;
                used += room;
            } else {
                gaps[i] += share;
                used += share;
                still.push(i);
            }
        }
        left -= used;
        if still.len() == open.len() {
            break;
        }
        open = still;
    }
    gaps
}
