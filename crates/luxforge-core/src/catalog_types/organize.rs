//! Organization: the thresholds a view groups with, the per-frame record the organize functions
//! read, and what they answer — events, and the day, camera and moment boundaries of a view.
//!
//! The functions themselves are in `crate::organize`: pure functions of these records,
//! the gazetteer ([`PlaceNames`]) and the thresholds, with the preview-brightness bracket check
//! plugged in through [`BracketProbe`] (the preview lane's). Nothing here is stored; changing a
//! threshold regroups at once.
use super::{
    BodyKey, CameraBody, EventId, Exposure, GeoPosition, LocalDay, Month, VolumeId,
    identity::ViewItem,
};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};

/// The rules events and moments are computed with: the design's P3 and P5, adjustable per view
/// from the Group chip. Every field is optional in JSON and defaults to the recorded value, so a
/// view overrides one field by naming it alone.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thresholds {
    /// A new event starts after a gap longer than this between consecutive photographs sorted by
    /// capture time (3 hours).
    pub event_gap_ms: u64,
    /// …or where consecutive photographs that both carry a position are further apart than this
    /// (25 km).
    pub event_distance_km: f64,
    /// Frames of one body less than this apart are one run (1 s).
    pub run_gap_ms: u64,
    /// Frames of one body less than this apart whose aperture, ISO and focal length match are one
    /// run too (2 s), since bracketing at slower shutter speeds spaces frames further.
    pub matched_run_gap_ms: u64,
    /// A run whose metadata exposure differs by at least this between frames is a bracket (⅓ EV).
    pub metadata_bracket_step_ev: f32,
    /// A run whose previews' measured brightness steps consistently by at least this, with the
    /// framing unchanged, is a bracket (⅔ EV).
    pub preview_bracket_step_ev: f32,
    /// A bracket has at least this many frames (2)…
    pub bracket_min_frames: u32,
    /// …and at most this many (9).
    pub bracket_max_frames: u32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            event_gap_ms: 3 * 60 * 60 * 1000,
            event_distance_km: 25.0,
            run_gap_ms: 1000,
            matched_run_gap_ms: 2000,
            metadata_bracket_step_ev: 1.0 / 3.0,
            preview_bracket_step_ev: 2.0 / 3.0,
            bracket_min_frames: 2,
            bracket_max_frames: 9,
        }
    }
}

impl Thresholds {
    /// The ranges a view may set: an event gap of 1 minute to 7 days, a distance of 0.1 to 1000
    /// km, run gaps of 1 ms to 60 s with the matched gap at least the plain one, bracket steps of
    /// 0.1 to 4 EV, and 2 to 9 bracket frames with the minimum at most the maximum.
    pub fn validate(&self) -> Result<(), Error> {
        let fail = |what: &str| Err(Error::validation(format!("thresholds: {what}")));
        if !(60_000..=7 * 24 * 3_600_000).contains(&self.event_gap_ms) {
            return fail("event_gap_ms must be 1 minute to 7 days");
        }
        if !(0.1..=1000.0).contains(&self.event_distance_km) {
            return fail("event_distance_km must be 0.1 to 1000");
        }
        if !(1..=60_000).contains(&self.run_gap_ms)
            || !(self.run_gap_ms..=60_000).contains(&self.matched_run_gap_ms)
        {
            return fail("run gaps must be 1 ms to 60 s, the matched gap at least the plain one");
        }
        for step in [self.metadata_bracket_step_ev, self.preview_bracket_step_ev] {
            if !(0.1..=4.0).contains(&step) {
                return fail("bracket steps must be 0.1 to 4 EV");
            }
        }
        if !(2..=9).contains(&self.bracket_min_frames)
            || !(self.bracket_min_frames..=9).contains(&self.bracket_max_frames)
        {
            return fail("bracket frames must be 2 to 9, the minimum at most the maximum");
        }
        Ok(())
    }
}

/// How a view of files or photographs is grouped. Grouping applies to a capture-time sort; any other
/// sort shows the view ungrouped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Grouping {
    /// The camera-local day, then the body when a day has more than one, then moments of one body.
    #[default]
    DayCameraMoment,
    /// The camera-local day only.
    Day,
    /// No groups: one run of frames.
    None,
}

impl Grouping {
    pub const ALL: [Self; 3] = [Self::DayCameraMoment, Self::Day, Self::None];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DayCameraMoment => "day-camera-moment",
            Self::Day => "day",
            Self::None => "none",
        }
    }
}

/// What kind of decision a moment is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MomentKind {
    /// One frame on its own.
    Single,
    /// A run of frames whose exposure does not step: choose one.
    Burst,
    /// A run whose exposure steps: usually one photograph to merge.
    Bracket,
}

/// Why a run is called a bracket: its recorded exposure steps, or its previews' measured brightness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BracketEvidence {
    Metadata,
    Previews,
}

/// One burst or bracket in a view: the frames `start..start + len` of the view's order. A view's
/// summary lists bursts and brackets only; a frame no moment covers is a single.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Moment {
    pub kind: MomentKind,
    /// Present exactly for a bracket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<BracketEvidence>,
    /// A bracket's per-frame exposure in stops relative to its metered frame (the frame with no
    /// bias, or the median frame), brighter positive, in view order: `[-2, 0, 2]`. Empty for a
    /// burst.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps_ev: Vec<f32>,
    /// From the first frame's capture to the last's, in milliseconds.
    pub span_ms: u64,
    pub start: u32,
    pub len: u32,
    /// How many of its frames are picked: the header's "1 picked". `organize::group` leaves it 0;
    /// a view's evaluation counts it from the frames' picks.
    #[serde(default)]
    pub picked: u32,
}

/// A day of a grouped view: frames `start..start + len`, all captured on `day` on their camera's
/// clock, or undated when `day` is absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DayGroup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day: Option<LocalDay>,
    pub start: u32,
    pub len: u32,
    /// How many of its frames are picked: the heading's "9 picked". `organize::group` leaves it 0;
    /// a view's evaluation counts it from the frames' picks.
    #[serde(default)]
    pub picked: u32,
}

/// One body's frames within a day that has more than one body: frames `start..start + len`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraGroup {
    pub body: BodyKey,
    /// The body as the header names it ([`CameraBody::label`]), or `Unknown camera`.
    pub label: String,
    pub start: u32,
    pub len: u32,
}

/// The group layout of an ordered view: its days, the camera groups inside days with more than one
/// body, and its bursts and brackets. Enough for a virtualized grouped grid to lay itself out
/// without reading a row.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GroupLayout {
    pub days: Vec<DayGroup>,
    pub cameras: Vec<CameraGroup>,
    pub moments: Vec<Moment>,
}

/// An index into [`FrameTables::folders`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FolderIndex(pub u32);

/// An index into [`FrameTables::bodies`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BodyIndex(pub u32);

/// The folders and bodies a list of [`FrameFacts`] names by index, each stored once however many
/// frames share it.
#[derive(Clone, Debug, Default)]
pub struct FrameTables {
    folders: Vec<PathBuf>,
    bodies: Vec<Option<CameraBody>>,
    folder_index: HashMap<PathBuf, FolderIndex>,
    body_index: HashMap<BodyKey, BodyIndex>,
}

impl FrameTables {
    /// The index of `folder`, adding it when it is new.
    pub fn folder(&mut self, folder: PathBuf) -> FolderIndex {
        if let Some(index) = self.folder_index.get(&folder) {
            return *index;
        }
        let index = FolderIndex(self.folders.len() as u32);
        self.folders.push(folder.clone());
        self.folder_index.insert(folder, index);
        index
    }

    /// The index of a frame's body, adding it when it is new; every frame with no camera recorded
    /// shares one.
    pub fn body(&mut self, camera: Option<&CameraBody>) -> BodyIndex {
        let key = BodyKey::of(camera);
        if let Some(index) = self.body_index.get(&key) {
            return *index;
        }
        let index = BodyIndex(self.bodies.len() as u32);
        self.bodies.push(camera.cloned());
        self.body_index.insert(key, index);
        index
    }

    pub fn folder_path(&self, index: FolderIndex) -> &PathBuf {
        &self.folders[index.0 as usize]
    }

    pub fn camera(&self, index: BodyIndex) -> Option<&CameraBody> {
        self.bodies[index.0 as usize].as_ref()
    }

    pub fn body_key(&self, index: BodyIndex) -> BodyKey {
        BodyKey::of(self.camera(index))
    }

    /// The body as a camera group names it.
    pub fn body_label(&self, index: BodyIndex) -> String {
        self.camera(index)
            .map_or_else(|| "Unknown camera".to_owned(), CameraBody::label)
    }
}

/// What the organize functions read of one frame: compact, with its folder and body by index into a
/// [`FrameTables`], so a view of 100,000 frames holds no per-frame strings but the file name.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameFacts {
    pub item: ViewItem,
    pub folder: FolderIndex,
    /// The file's name: what orders undated frames and, with the folder, the path an event's or a
    /// moment's identity hashes.
    pub name: Box<str>,
    /// [`CaptureTime::instant_ms`](super::CaptureTime::instant_ms); none when undated.
    pub instant_ms: Option<i64>,
    /// [`CaptureTime::local_day`](super::CaptureTime::local_day); none when undated.
    pub local_day: Option<LocalDay>,
    pub position: Option<GeoPosition>,
    pub body: BodyIndex,
    pub exposure: Exposure,
}

/// One event: frames `start..start + len` of [`EventSet::order`].
#[derive(Clone, Debug, PartialEq)]
pub struct EventGroup {
    pub id: EventId,
    /// The design's naming order: the nearest place, a user-named folder holding most of it, or
    /// the date and cameras.
    pub name: String,
    /// The name without its dates: the place, the folder or the cameras ("Konstanz", "Lake",
    /// "NIKON Z 8, LEICA Q3 +1"); the whole name for an Undated event ("Undated · From Anna"); empty
    /// when the name is its dates alone.
    pub label: String,
    pub place: Option<String>,
    /// The first and last frames' instants; none for an undated event.
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub first_day: Option<LocalDay>,
    pub last_day: Option<LocalDay>,
    pub bodies: Vec<BodyIndex>,
    pub folders: Vec<FolderIndex>,
    pub start: u32,
    pub len: u32,
}

impl EventGroup {
    /// Whether the event is a folder's Undated event.
    pub fn undated(&self) -> bool {
        self.start_ms.is_none()
    }
}

/// Every event of a list of frames: `order` is the frames' indices in event order (each event's
/// frames by capture time, an Undated event's by file name) and `events` the events in
/// chronological order, the Undated events last.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventSet {
    pub order: Vec<u32>,
    pub events: Vec<EventGroup>,
}

/// An event as `event.list` answers it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: EventId,
    pub name: String,
    /// [`EventGroup::label`]: the name without its dates, so a client never parses the name.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// Camera-local first and last days; absent for an Undated event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_day: Option<LocalDay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_day: Option<LocalDay>,
    /// The months it is listed under, oldest first.
    pub months: Vec<Month>,
    /// Its bodies' labels.
    pub cameras: Vec<String>,
    pub count: u32,
    pub picked: u32,
    /// Files whose volume is not mounted: browsable from their cached previews, not developable.
    pub offline: u32,
    /// The roots its files are in: indexed folders and cards.
    pub roots: Vec<PathBuf>,
    /// The volumes of those files.
    pub volumes: Vec<VolumeId>,
    pub undated: bool,
}

/// One month of `event.list`: how many events and files it holds, and how many are picked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonthCount {
    pub month: Month,
    pub events: u32,
    pub files: u32,
    pub picked: u32,
}

/// What `event.list` answers: the events asked for, newest first, and every month that has any,
/// newest first, for the sources panel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventList {
    pub events: Vec<Event>,
    pub months: Vec<MonthCount>,
}

/// The gazetteer the event names come from: the nearest populated place to a position, from data
/// bundled with Luxforge and never an online lookup (P4).
pub trait PlaceNames {
    fn nearest(&self, position: &GeoPosition) -> Option<String>;
}

/// No gazetteer: every event is named by its folder or its dates.
pub struct NoPlaces;

impl PlaceNames for NoPlaces {
    fn nearest(&self, _: &GeoPosition) -> Option<String> {
        None
    }
}

/// The preview-brightness bracket check as organizing asks it, for a run the metadata
/// cannot classify: each frame's measured exposure in stops relative to the first frame (brighter
/// positive, the first `0`), one value per frame, and only when the framing is confirmed unchanged;
/// `None` when it cannot tell — previews not decoded yet, or the framing moved. It must answer from
/// previews already decoded, in microseconds, and never read a photograph's file. The preview
/// lane's is `previews::PreviewProbe` (`previews::bracket_probe`), which reads a run's stored
/// fingerprints in one indexed query when it is asked about that run, and nothing for a run it is
/// not asked about.
pub trait BracketProbe {
    fn measure(&self, frames: &[ViewItem]) -> Option<Vec<f32>>;
}

/// No preview check: a run the metadata cannot classify is a burst.
pub struct NoProbe;

impl BracketProbe for NoProbe {
    fn measure(&self, _: &[ViewItem]) -> Option<Vec<f32>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_view_overrides_one_threshold_and_keeps_the_rest() {
        let thresholds: Thresholds =
            serde_json::from_value(json!({"event_gap_ms": 3_600_000})).unwrap();
        assert_eq!(
            thresholds,
            Thresholds {
                event_gap_ms: 3_600_000,
                ..Thresholds::default()
            }
        );
        thresholds.validate().unwrap();
        Thresholds::default().validate().unwrap();
        assert!(serde_json::from_value::<Thresholds>(json!({"gap": 1})).is_err());
        for bad in [
            Thresholds {
                bracket_min_frames: 1,
                ..Thresholds::default()
            },
            Thresholds {
                matched_run_gap_ms: 500,
                ..Thresholds::default()
            },
            Thresholds {
                event_distance_km: f64::NAN,
                ..Thresholds::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn tables_store_each_folder_and_body_once() {
        let mut tables = FrameTables::default();
        let body = CameraBody {
            make: "LEICA CAMERA AG".into(),
            model: "LEICA Q3".into(),
            serial: None,
        };
        let a = tables.folder("/a".into());
        assert_eq!(tables.folder("/a".into()), a);
        assert_ne!(tables.folder("/b".into()), a);
        let leica = tables.body(Some(&body));
        let unknown = tables.body(None);
        assert_eq!(tables.body(Some(&body)), leica);
        assert_ne!(leica, unknown);
        assert_eq!(tables.body_label(unknown), "Unknown camera");
        assert_eq!(tables.body_label(leica), "LEICA Q3");
        assert_eq!(
            serde_json::to_value(Grouping::DayCameraMoment).unwrap(),
            json!("day-camera-moment")
        );
    }
}
