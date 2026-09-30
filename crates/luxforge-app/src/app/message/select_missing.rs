//! Missing originals (TASK-023), carried inside the Select workspace's message as
//! [`SelectMessage::Missing`](super::select::SelectMessage::Missing), and Locate original… in
//! Develop's Original not found notice, which shares its Locate.
use crate::state::select_missing::{LocateFrom, MissingFilter, PhotoFacts};
use luxforge_core::{
    AssetId,
    catalog_types::{MissingOriginals, VolumeState},
};
use serde_json::Value;
use std::path::PathBuf;

/// What Missing originals' gestures and owner answers are, handled in `app/select_missing.rs`.
/// Every gesture that changes or searches anything sends the request an API client would.
#[derive(Clone, Debug)]
pub(crate) enum MissingMessage {
    /// `source.missing` and `volume.list` answered, for the Select shell's evaluation `serial`.
    Listed {
        serial: u64,
        result: Result<Box<(MissingOriginals, Vec<VolumeState>)>, String>,
    },
    /// Find in a folder… on the group developed from this folder: the native folder dialog.
    Find(PathBuf),
    /// The folder the dialog chose, or none: `source.find` of the group in it.
    FindIn {
        group: PathBuf,
        root: Option<PathBuf>,
    },
    /// `source.find` answered with its job, or refused.
    Finding {
        group: PathBuf,
        result: Result<String, String>,
    },
    /// Stop search: `job.cancel` of the running search.
    Stop,
    /// `job.cancel` answered.
    Stopped(Result<Value, String>),
    /// The timer that exists while a search or a Locate runs.
    Poll,
    /// `job.read` of the running search and the running Locate answered.
    Polled {
        search: Option<(String, Result<Value, String>)>,
        locate: Option<(String, Result<Value, String>)>,
    },
    /// A filter segment.
    Filter(MissingFilter),
    /// A row pressed: the Info panel describes it.
    Row(AssetId),
    /// `asset.state` and `history.list` answered for the selected row.
    Facts {
        asset: AssetId,
        result: Result<PhotoFacts, String>,
    },
    /// Open a row's Choose… menu, or close the one open.
    Menu(Option<AssetId>),
    /// One of the identical files chosen for a photograph.
    Choose { asset: AssetId, path: PathBuf },
    /// Relink N: `source.relink` of every verified pair.
    Relink,
    /// `source.relink` answered: the pairs it was sent and its answer.
    Relinked {
        pairs: Vec<AssetId>,
        result: Result<Value, String>,
    },
    /// Locate… on a row, or the Info panel's Locate a different file…: the native file dialog.
    Locate(AssetId),
    /// Locate original… in Develop's notice, for the open photograph: the native file dialog.
    LocateOriginal,
    /// The file the dialog chose, or none: `source.locate` of that photograph and file.
    LocatePicked {
        asset: AssetId,
        file_name: String,
        from: LocateFrom,
        path: Option<PathBuf>,
    },
    /// `source.locate` answered with its job, or refused.
    Locating(Result<String, String>),
}
