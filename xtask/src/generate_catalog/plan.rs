//! The shoot plan: one deterministic model of camera bodies, trips and moments that every
//! `generate-catalog` mode draws from, so the image folders, the index rows and the catalog tell
//! the same kind of story.
//!
//! A plan is a sequence of **events**, the ground truth the event rule (P3 in
//! `docs/design/catalog.md`) must find: photographs sorted by capture time with every gap inside an
//! event under 3 hours and every positioned neighbour within 25 km, and consecutive events
//! separated by a gap over 3 hours or, once, by a jump of over 25 km inside an hour on the same day.
//! A trip of several days keeps shooting through the night (a tripod camera's night frames under
//! 3 hours apart), because the gap rule alone would otherwise split it at every night. Inside an
//! event each body shoots **moments** (P5): singles, bursts (3–8 frames under 1 s apart, one
//! exposure) and brackets (3 frames 1 or 2 EV apart, or 5 frames 1 EV apart, 0.3–1.8 s apart, one
//! aperture, ISO and focal length) of three kinds: with the exposure bias recording each step, with
//! the exposure time alone changing, and with nothing in the metadata changing at all, as a drone
//! writes it. One body's consecutive moments are always at least 6 s apart, so no rule merges them.
//! Undated files, with no capture time at all, are an event of their own in one user-named folder.
//!
//! **Clocks.** A body that writes `OffsetTimeOriginal` keeps local time; a body that writes none
//! keeps UTC. Every recorded time therefore gives the frame's true instant under the core's reading
//! (`CaptureTime::instant_ms`: the offset when written, the clock read as UTC otherwise), and the
//! ground truth holds under that reading and in real time alike. Bodies without an offset that keep
//! local time instead would skew by the zone's offset, which P3's 3-hour gap cannot absorb.
//!
//! Events are made one at a time, each from its own random stream, so a plan of a million frames
//! never holds more than one event's frames.
use super::clock::{
    CENTRAL, Date, EASTERN, HOUR, ICELAND, JAPAN, LocalTime, MINUTE, SECOND, SOUTH_AFRICA,
    US_EASTERN, WESTERN, Zone,
};
use super::random::Random;

/// Where a body's files are kept in the September trips, which decides their folder.
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
    /// The extension of its RAW files (for the phone, of its only files) in the index and the
    /// catalog; the image folders are JPEG throughout.
    pub raw_extension: &'static str,
    /// Its full-size image, in pixels.
    pub size: (u32, u32),
    /// The typical length of one of its files, in bytes.
    pub bytes: u64,
    pub storage: Storage,
    /// Whether it writes `OffsetTimeOriginal`, and so keeps local time rather than UTC.
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
        raw_extension: "NEF",
        size: (8256, 5504),
        bytes: 52_000_000,
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
        raw_extension: "NEF",
        size: (8256, 5504),
        bytes: 52_000_000,
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
        raw_extension: "DNG",
        size: (9520, 6336),
        bytes: 88_000_000,
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
        raw_extension: "RAF",
        size: (7728, 5152),
        bytes: 42_000_000,
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
        raw_extension: "DNG",
        size: (5472, 3648),
        bytes: 38_000_000,
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
        raw_extension: "JPG",
        size: (4032, 3024),
        bytes: 3_200_000,
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

/// A place a trip goes to: its position in microdegrees, its elevation, its zone and the catalog
/// folder its trips are developed under, when any.
pub struct Place {
    pub name: &'static str,
    /// Its name in ASCII, lower case, for event labels.
    pub slug: &'static str,
    pub latitude: i32,
    pub longitude: i32,
    pub elevation: i32,
    pub zone: Zone,
    pub parent: Option<Parent>,
}

/// The catalog folders some trips' folders are nested in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parent {
    Bodensee,
    Travel,
}

impl Parent {
    pub const ALL: [Parent; 2] = [Parent::Bodensee, Parent::Travel];

    pub fn name(self) -> &'static str {
        match self {
            Parent::Bodensee => "Bodensee",
            Parent::Travel => "Travel",
        }
    }
}

const fn place(
    name: &'static str,
    slug: &'static str,
    (latitude, longitude, elevation): (i32, i32, i32),
    zone: Zone,
    parent: Option<Parent>,
) -> Place {
    Place {
        name,
        slug,
        latitude,
        longitude,
        elevation,
        zone,
        parent,
    }
}

const LAKE: Option<Parent> = Some(Parent::Bodensee);
const AWAY: Option<Parent> = Some(Parent::Travel);

pub const KONSTANZ: Place = place(
    "Konstanz",
    "konstanz",
    (47_660_000, 9_175_000, 405),
    CENTRAL,
    LAKE,
);
pub const REICHENAU: Place = place(
    "Reichenau",
    "reichenau",
    (47_689_000, 9_062_000, 398),
    CENTRAL,
    LAKE,
);
pub const ZURICH: Place = place(
    "Zürich",
    "zurich",
    (47_376_900, 8_541_700, 408),
    CENTRAL,
    None,
);
pub const LUZERN: Place = place(
    "Luzern",
    "luzern",
    (47_050_200, 8_309_300, 435),
    CENTRAL,
    None,
);
pub const LINDAU: Place = place(
    "Lindau",
    "lindau",
    (47_546_000, 9_684_000, 400),
    CENTRAL,
    LAKE,
);

/// Where the older trips go, each with its weight: the lake most often, then home and away.
const HISTORY_PLACES: &[(Place, u32)] = &[
    (KONSTANZ, 6),
    (LINDAU, 4),
    (REICHENAU, 3),
    (
        place(
            "Meersburg",
            "meersburg",
            (47_693_600, 9_271_000, 440),
            CENTRAL,
            LAKE,
        ),
        3,
    ),
    (
        place(
            "Bregenz",
            "bregenz",
            (47_503_100, 9_747_100, 398),
            CENTRAL,
            LAKE,
        ),
        3,
    ),
    (
        place(
            "Überlingen",
            "uberlingen",
            (47_767_600, 9_159_100, 403),
            CENTRAL,
            LAKE,
        ),
        2,
    ),
    (ZURICH, 3),
    (LUZERN, 2),
    (
        place(
            "St. Gallen",
            "st-gallen",
            (47_424_500, 9_376_700, 670),
            CENTRAL,
            None,
        ),
        2,
    ),
    (
        place(
            "Basel",
            "basel",
            (47_559_600, 7_588_600, 260),
            CENTRAL,
            None,
        ),
        1,
    ),
    (
        place("Bern", "bern", (46_948_000, 7_447_400, 540), CENTRAL, None),
        1,
    ),
    (
        place(
            "Zermatt",
            "zermatt",
            (46_020_700, 7_749_100, 1608),
            CENTRAL,
            None,
        ),
        2,
    ),
    (
        place(
            "Grindelwald",
            "grindelwald",
            (46_624_200, 8_041_400, 1034),
            CENTRAL,
            None,
        ),
        2,
    ),
    (
        place(
            "Lugano",
            "lugano",
            (46_003_700, 8_951_100, 273),
            CENTRAL,
            None,
        ),
        1,
    ),
    (
        place(
            "München",
            "munchen",
            (48_135_100, 11_582_000, 519),
            CENTRAL,
            None,
        ),
        1,
    ),
    (
        place(
            "Salzburg",
            "salzburg",
            (47_809_500, 13_055_000, 424),
            CENTRAL,
            None,
        ),
        1,
    ),
    (
        place(
            "Innsbruck",
            "innsbruck",
            (47_269_200, 11_404_100, 574),
            CENTRAL,
            None,
        ),
        1,
    ),
    (
        place(
            "Hallstatt",
            "hallstatt",
            (47_562_200, 13_649_300, 511),
            CENTRAL,
            None,
        ),
        1,
    ),
    (
        place("Wien", "wien", (48_208_200, 16_373_800, 190), CENTRAL, None),
        1,
    ),
    (
        place(
            "Freiburg",
            "freiburg",
            (47_999_000, 7_842_100, 278),
            CENTRAL,
            None,
        ),
        1,
    ),
    (
        place(
            "Annecy",
            "annecy",
            (45_899_200, 6_129_400, 448),
            CENTRAL,
            None,
        ),
        1,
    ),
    (
        place(
            "Chamonix",
            "chamonix",
            (45_923_700, 6_869_400, 1035),
            CENTRAL,
            None,
        ),
        1,
    ),
    (
        place(
            "Venezia",
            "venezia",
            (45_440_800, 12_315_500, 2),
            CENTRAL,
            AWAY,
        ),
        1,
    ),
    (
        place(
            "Firenze",
            "firenze",
            (43_769_600, 11_255_800, 50),
            CENTRAL,
            AWAY,
        ),
        1,
    ),
    (
        place("Paris", "paris", (48_856_600, 2_352_200, 35), CENTRAL, AWAY),
        1,
    ),
    (
        place(
            "Barcelona",
            "barcelona",
            (41_387_400, 2_168_600, 12),
            CENTRAL,
            AWAY,
        ),
        1,
    ),
    (
        place(
            "København",
            "kobenhavn",
            (55_676_100, 12_568_300, 14),
            CENTRAL,
            AWAY,
        ),
        1,
    ),
    (
        place(
            "Lisboa",
            "lisboa",
            (38_722_300, -9_139_300, 50),
            WESTERN,
            AWAY,
        ),
        1,
    ),
    (
        place(
            "London",
            "london",
            (51_507_200, -127_600, 11),
            WESTERN,
            AWAY,
        ),
        1,
    ),
    (
        place(
            "Reykjavík",
            "reykjavik",
            (64_146_600, -21_942_600, 20),
            ICELAND,
            AWAY,
        ),
        1,
    ),
    (
        place(
            "Athína",
            "athina",
            (37_983_800, 23_727_500, 70),
            EASTERN,
            AWAY,
        ),
        1,
    ),
    (
        place(
            "New York",
            "new-york",
            (40_712_800, -74_006_000, 10),
            US_EASTERN,
            AWAY,
        ),
        1,
    ),
    (
        place("Kyoto", "kyoto", (35_011_600, 135_768_100, 50), JAPAN, AWAY),
        1,
    ),
    (
        place(
            "Cape Town",
            "cape-town",
            (-33_924_900, 18_424_100, 10),
            SOUTH_AFRICA,
            AWAY,
        ),
        1,
    ),
];

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

/// Which drive an older trip's folder is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drive {
    /// The Mac's own disk, under the pictures folder.
    Internal,
    /// The external "Photos SSD", under its archive folder.
    External,
}

/// How an event's files are filed on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filing {
    /// Where each body's [`Storage`] puts them: the September trips.
    BySource,
    /// Every body's files in one trip folder, `<year>/<first day> <title>`, on the drive: the
    /// older trips.
    Trip(Drive),
}

/// What one event is to hold, before its frames are drawn.
pub struct Spec {
    pub label: String,
    /// Where it happened; `None` for an undated event.
    pub place: Option<&'static Place>,
    /// The user-named folder's name: an undated event's whole name, otherwise the part after the
    /// first date (the place's name when `None`).
    pub folder: Option<&'static str>,
    pub filing: Filing,
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
    /// Developed photographs rather than every frame shot: its moments are singles and brackets,
    /// never bursts, whose one developed frame is a single.
    pub developed: bool,
    /// Its cameras wrote JPEG rather than RAW (in the index and the catalog).
    pub jpeg: bool,
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
        self.fixed_frames() + needed_moments(self.days).saturating_sub(moments)
    }
}

/// The fewest moments an event of `days` days holds.
fn needed_moments(days: u8) -> u32 {
    if days > 1 {
        MOMENTS_A_DAY * u32::from(days)
    } else {
        1
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
    /// The camera's clock, `None` when undated: local time for a body that writes its offset,
    /// UTC for one that does not.
    pub time: Option<LocalTime>,
    /// How far the clock is ahead of UTC, in minutes: the zone's offset for a body that writes
    /// it, 0 for one that does not.
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
    /// Where it happened, whether or not any frame carries a position; `None` for an undated
    /// event.
    pub place: Option<&'static Place>,
    /// What the person calls it: the user-named folder's name after its date, or the place's.
    pub title: String,
    /// The user-named folder its `Named` bodies' files (or, filed by trip, all its files) go into:
    /// `<first day> <title>`, or the title alone when undated.
    pub folder: String,
    pub filing: Filing,
    /// Its cameras wrote JPEG rather than RAW.
    pub jpeg: bool,
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

    /// Where a September trip's frame is kept: whether on the card, and its folder there or under
    /// the pictures folder.
    pub fn source_folder(&self, frame: &Frame) -> (bool, String) {
        let body = frame.body();
        match body.storage {
            Storage::Card { suffix } => (
                true,
                format!("DCIM/{}{suffix}", body.file_stem(frame.count).0),
            ),
            Storage::Dump => (false, format!("Card dumps/{}", self.start)),
            Storage::Named => (false, self.folder.clone()),
            Storage::Phone => (false, "iPhone export".into()),
        }
    }

    /// A frame's file name in the index and the catalog: its RAW extension, or `JPG` for the
    /// phone, on a trip shot in JPEG and for undated files, which are exports.
    pub fn file_name(&self, frame: &Frame) -> String {
        let body = frame.body();
        let extension = if self.place.is_none() || self.jpeg {
            "JPG"
        } else {
            body.raw_extension
        };
        format!("{}.{extension}", body.file_stem(frame.count).1)
    }
}

fn spec(
    label: &str,
    place: Option<&'static Place>,
    start: Date,
    days: u8,
    bodies: &[(usize, u32)],
    weight: u32,
) -> Spec {
    Spec {
        label: label.into(),
        place,
        folder: None,
        filing: Filing::BySource,
        start,
        days,
        bodies: bodies.to_vec(),
        required: Vec::new(),
        first: None,
        last: None,
        begins: Start::Morning,
        cap: None,
        developed: false,
        jpeg: false,
        weight,
        budget: 0,
    }
}

/// The trips of September 2026 every mode starts from: a two-day Konstanz trip with both Nikon
/// bodies, the Leica and the phone; a day at the lake with no GPS at all; Zürich and then Luzern on
/// one day, over 25 km apart inside the hour; a three-day Lindau trip with the drone; and the
/// undated files in "From Anna".
pub fn september() -> Vec<Spec> {
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

/// The first day of the September trips.
pub const SEPTEMBER: Date = Date::new(2026, 9, 12);

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

/// How a run of older trips is drawn.
pub struct History {
    /// The frames between them.
    pub total: u32,
    /// The mean frames of one trip.
    pub mean: u32,
    /// The whole days between one trip's last day and the next one's first.
    pub gaps: (i64, i64),
    /// The last trip ends at least a day before this date.
    pub before: Date,
    /// Developed photographs rather than every frame shot (see [`Spec::developed`]).
    pub developed: bool,
    /// Which trips' folders are on the external drive.
    pub external: External,
    /// Of `total`, the undated files that close the run, in "Scans" on the internal disk.
    pub undated: u32,
    /// The random stream its own draws come from, apart from every event's and another
    /// history's.
    pub stream: u64,
}

/// Which older trips are on the external drive; the rest are on the internal disk.
#[derive(Clone, Copy, Debug)]
pub enum External {
    /// The older half.
    OlderHalf,
    /// Those that start in these years.
    Years(i32, i32),
}

/// Older trips, in time order, that end before `history.before`: each at a place drawn from
/// [`HISTORY_PLACES`], with a main camera, sometimes a second, often the phone and now and then
/// the drone, lasting a day to three by its size.
pub fn history(seed: u64, history: &History) -> Vec<Spec> {
    let mut random = Random::stream(seed, history.stream);
    let dated = history.total - history.undated.min(history.total);
    let mut sizes = Vec::new();
    let mut left = dated;
    while left > 0 {
        let size = ((f64::from(history.mean) * random.uniform(0.4, 1.6)).round() as u32).max(3);
        let size = if left - size.min(left) < 3 {
            left
        } else {
            size
        };
        sizes.push(size);
        left -= size;
    }
    let weights: Vec<u32> = HISTORY_PLACES.iter().map(|&(_, w)| w).collect();
    let mut specs = Vec::with_capacity(sizes.len() + 1);
    let mut before = history.before;
    // Drawn from the latest trip back, then turned into time order.
    let count = sizes.len();
    for (back, &size) in sizes.iter().rev().enumerate() {
        let days: u8 = match size {
            40.. => [1, 1, 1, 1, 1, 1, 1, 2, 2, 3][random.between(0, 9) as usize],
            24.. => [1, 1, 1, 2][random.between(0, 3) as usize],
            _ => 1,
        };
        let gap = random.between(history.gaps.0, history.gaps.1);
        let start = before.add_days(-gap - i64::from(days));
        before = start;
        let place = &HISTORY_PLACES[random.weighted(&weights)].0;
        let main = *random.pick(&[Z8_A, Z8_A, LEICA, FUJI]);
        let mut bodies = vec![(main, 4)];
        if random.chance(0.25) {
            let second = *random.pick(&[Z8_A, LEICA, FUJI]);
            if second != main {
                bodies.push((second, 2));
            }
        }
        if random.chance(0.6) {
            bodies.push((PHONE, 2));
        }
        if random.chance(0.12) {
            bodies.push((DRONE, 1));
        }
        let external = match history.external {
            External::OlderHalf => back >= count / 2,
            External::Years(first, last) => (first..=last).contains(&start.year),
        };
        specs.push(Spec {
            filing: Filing::Trip(if external {
                Drive::External
            } else {
                Drive::Internal
            }),
            jpeg: random.chance(0.35),
            developed: history.developed,
            budget: size,
            ..spec(
                &format!("{}-{start}", place.slug),
                Some(place),
                start,
                days,
                &bodies,
                0,
            )
        });
    }
    specs.reverse();
    if history.undated > 0 {
        specs.push(Spec {
            folder: Some("Scans"),
            filing: Filing::Trip(Drive::Internal),
            budget: history.undated.min(history.total),
            ..spec("scans", None, history.before, 1, &[(FUJI, 1)], 0)
        });
    }
    specs
}

/// A plan: its specs, made into events one at a time.
pub struct Plan {
    seed: u64,
    /// Where this plan's events' random streams start, so two plans of one run draw apart.
    stream: u64,
    specs: std::vec::IntoIter<Spec>,
    made: u64,
    counters: [u32; BODIES.len()],
    previous_end: Option<LocalTime>,
}

impl Plan {
    pub fn new(seed: u64, stream: u64, specs: Vec<Spec>) -> Self {
        Plan {
            seed,
            stream,
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
        Ok(Plan::new(seed, 0, specs))
    }

    /// What an index of `total` files holds: older trips, the later half on the internal disk and
    /// the older half on the external drive, then the September trips with a third of the files,
    /// at most 3,000.
    pub fn files(seed: u64, total: u32) -> Result<Self, String> {
        let mut specs = september();
        let minimum: u32 = specs.iter().map(Spec::minimum).sum();
        let recent = (total / 3).clamp(minimum, 3_000).min(total);
        apportion(&mut specs, recent)?;
        let mut all = history(
            seed,
            &History {
                total: total - recent,
                mean: 250,
                gaps: (5, 40),
                before: SEPTEMBER.add_days(-2),
                developed: false,
                external: External::OlderHalf,
                undated: 0,
                stream: 0x4849_5354_0000_0001,
            },
        );
        all.extend(specs);
        Ok(Plan::new(seed, 0, all))
    }

    /// `total` developed photographs from older trips over several years ending before `before`,
    /// and a few undated scans: trips of up to 400 photographs, spaced so a million spans about
    /// twenty years; those of 2020 to 2022 on the external drive.
    pub fn developed(seed: u64, total: u32, before: Date) -> Self {
        let mean = (total / 400).clamp(40, 400);
        let trips = i64::from(total / mean).max(1);
        let spacing = (2 * 3650 / trips).max(2);
        let undated = if total >= 100 {
            (total / 500).max(3)
        } else {
            0
        };
        let history = History {
            total,
            mean,
            gaps: (1, spacing),
            before,
            developed: true,
            external: External::Years(2020, 2022),
            undated,
            stream: 0x4849_5354_0000_0002,
        };
        Plan::new(seed, 1 << 40, self::history(seed, &history))
    }

    /// The first day of the plan's next event.
    pub fn first_day(&self) -> Option<Date> {
        self.specs.as_slice().first().map(|spec| spec.start)
    }
}

impl Iterator for Plan {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        let spec = self.specs.next()?;
        let mut maker = Maker {
            random: Random::stream(self.seed, self.stream + self.made),
            spec: &spec,
            zone: spec.place.map_or(0, |place| place.zone.offset(spec.start)),
            counters: &mut self.counters,
            frames: Vec::with_capacity(spec.budget as usize),
            moment: 0,
        };
        if let Some(end) = maker.make(self.previous_end) {
            self.previous_end = Some(end);
        }
        let frames = maker.frames;
        self.made += 1;
        let (title, folder) = match (spec.place, spec.folder) {
            (None, folder) => {
                let name = folder.unwrap_or("Undated").to_owned();
                (name.clone(), name)
            }
            (Some(place), folder) => {
                let title = folder.unwrap_or(place.name).to_owned();
                let folder = format!("{} {title}", spec.start);
                (title, folder)
            }
        };
        Some(Event {
            label: spec.label,
            place: spec.place,
            title,
            folder,
            filing: spec.filing,
            jpeg: spec.jpeg,
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
    /// The zone's offset in minutes over the whole event, from its first day.
    zone: i16,
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
    /// Lays out the event on the local wall clock of its place ([`Self::place`] turns each time
    /// into its body's clock) and answers when, on that clock, its last frame was taken.
    fn make(&mut self, previous_end: Option<LocalTime>) -> Option<LocalTime> {
        if self.spec.undated() {
            let body = self.spec.bodies[0].0;
            for _ in 0..self.spec.budget {
                let drafts = self.realize(Draw::Single(body), false);
                self.place(drafts, None);
            }
            return None;
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
        let mut end = start;
        for (day, draws) in days.into_iter().enumerate() {
            end = self.day(start, &draws);
            if day + 1 < day_count {
                let next = self.spec.start.add_days(day as i64 + 1).at(7, 30)
                    + self.random.between(0, 10 * MINUTE);
                self.night(end, next);
                start = next;
            }
        }
        Some(end)
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
        // A day of a multi-day event needs enough moments to span it: break the largest drawn
        // moments into singles until there are.
        while (draws.len() as u32) + required < needed_moments(spec.days) {
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
        let bodies = &self.spec.bodies;
        let total: u64 = bodies.iter().map(|&(_, w)| u64::from(w)).sum();
        let mut target = self.random.next_u64() % total;
        let mut body = bodies[0].0;
        for &(candidate, weight) in bodies {
            if target < u64::from(weight) {
                body = candidate;
                break;
            }
            target -= u64::from(weight);
        }
        let (bracket, burst) = match BODIES[body].bracket {
            None => (0.0, 0.2),
            Some(BracketKind::MetadataLess) => (0.35, 0.0),
            Some(_) => (0.13, 0.25),
        };
        let burst = if self.spec.developed { 0.0 } else { burst };
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

    /// Gives a moment's drafts their times (`start` is on the place's local wall clock) and file
    /// numbers, and appends them.
    fn place(&mut self, drafts: Vec<Draft>, start: Option<LocalTime>) {
        let parts = drafts.iter().map(|d| d.part).max().unwrap_or(0) + 1;
        for draft in drafts {
            let mut frame = draft.frame;
            frame.moment = self.moment + draft.part;
            frame.offset = if frame.body().writes_offset {
                self.zone
            } else {
                0
            };
            let behind = i64::from(self.zone - frame.offset) * MINUTE;
            frame.time = start.map(|start| start + (draft.offset - behind));
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
