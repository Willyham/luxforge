//! Developing picks and Develop's filmstrip, as plain data ([develop from
//! folders](../../../../docs/design/develop-from-folders.md#develops-folder)): Develop N's
//! confirmation in Select, made from what `pick.plan` answered and the person's choices, and the
//! filmstrip of the Develop folder Develop moves through. The app holds this state
//! (`app/develop.rs`); the model derived from it is what the confirmation, the title bars, the
//! filmstrip and the status bar draw. Nothing here calls the owner.
use crate::state::select::thousands;
use luxforge_core::{
    AssetId, EntryId, MutationRequest,
    catalog_types::{
        CatalogFolder, CatalogFolderId, CatalogFolders, DevelopInto, DevelopPlan, DevelopReport,
        FolderChoice, LocalDay, MAX_LIBRARY_NAME, PlannedEvent, PreviewOrigin,
    },
};
use serde_json::{Value, json};

/// Develop N's confirmation, as the person has left it: the plan it was opened on, each event's
/// folder, the existing folders it can choose from, and whether a card's picks use their copies.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Confirmation {
    pub(crate) plan: DevelopPlan,
    /// What the picks are developed from, as the header names it: the view's name.
    pub(crate) from: String,
    /// One per event of the plan, in its order.
    pub(crate) events: Vec<EventChoice>,
    /// Every catalog folder, as `folder.list` answered, parents first: what Or add to an existing
    /// folder offers.
    pub(crate) folders: Vec<CatalogFolder>,
    /// A card's picks with copies in indexed folders use them (`use_copies`), confirmed by
    /// fingerprint. Only offered when some have one.
    pub(crate) use_copies: bool,
    /// The event whose Or add to an existing folder menu is open.
    pub(crate) menu: Option<usize>,
}

/// Where one event's picks go: under the place named `name` — "2026 › Konstanz › 12 Sep" — or
/// into an existing folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EventChoice {
    /// The place's name as typed; the plan's proposal to begin with.
    pub(crate) name: String,
    /// An existing folder chosen instead, or proposed by the plan.
    pub(crate) existing: Option<CatalogFolderId>,
    /// The existing folder files each photograph in its capture day's folder inside it, as the
    /// plan proposes for a folder an earlier Develop of the event made. Or add to an existing
    /// folder chooses it without.
    pub(crate) days: bool,
}

impl Confirmation {
    /// The confirmation for `plan`: each event's proposal — its place, selected for typing, or the
    /// folder an earlier Develop from it made — and the copies used. A new folder the plan names
    /// is offered as a place of that name, as the plan proposes no other.
    pub(crate) fn new(plan: DevelopPlan, folders: CatalogFolders, from: String) -> Self {
        let events = plan
            .events
            .iter()
            .map(|event| match &event.folder {
                FolderChoice::Place { name } | FolderChoice::New { name, .. } => EventChoice {
                    name: name.clone(),
                    existing: None,
                    days: false,
                },
                FolderChoice::Existing { folder_id, days } => EventChoice {
                    name: event.name.clone(),
                    existing: Some(folder_id.clone()),
                    days: *days,
                },
            })
            .collect();
        Self {
            plan,
            from,
            events,
            folders: folders.folders,
            use_copies: true,
            menu: None,
        }
    }

    /// The picks on removable volumes, and how many of them have copies, across the plan's events.
    fn removable(&self) -> (u32, u32) {
        self.plan
            .events
            .iter()
            .flat_map(|event| &event.removable)
            .fold((0, 0), |(count, copies), picks| {
                (count + picks.count, copies + picks.with_copy)
            })
    }

    /// Whether the Develop sends `use_copies`: some card picks have copies, and the person left
    /// them to be used.
    pub(crate) fn sends_copies(&self) -> bool {
        self.use_copies && self.removable().1 > 0
    }

    /// Whether the Develop sends `confirm_removable`: some card picks will be developed from the
    /// card itself — those without a copy, or every one when the copies are not used. Pressing
    /// Develop is the person's confirmation, the sentence above it having said so.
    pub(crate) fn confirms_removable(&self) -> bool {
        let (count, copies) = self.removable();
        let covered = if self.sends_copies() { copies } else { 0 };
        count > covered
    }

    /// Why Develop cannot be pressed: a place's name is empty or too long.
    pub(crate) fn refusal(&self) -> Option<String> {
        self.events.iter().find_map(|choice| {
            if choice.existing.is_some() {
                return None;
            }
            let name = choice.name.trim();
            if name.is_empty() {
                Some("Name the place".to_owned())
            } else if name.chars().count() > MAX_LIBRARY_NAME {
                Some(format!(
                    "A folder name holds at most {MAX_LIBRARY_NAME} characters"
                ))
            } else {
                None
            }
        })
    }

    /// `pick.develop`'s `into`: each event of the plan by its identity, into its chosen folder.
    pub(crate) fn into(&self) -> Vec<DevelopInto> {
        self.plan
            .events
            .iter()
            .zip(&self.events)
            .map(|(event, choice)| DevelopInto {
                event_id: event.event_id.clone(),
                folder: match &choice.existing {
                    Some(folder_id) => FolderChoice::Existing {
                        folder_id: folder_id.clone(),
                        days: choice.days,
                    },
                    None => FolderChoice::Place {
                        name: choice.name.trim().to_owned(),
                    },
                },
            })
            .collect()
    }

    /// The label a folder is listed and shown by: its path from the top level, `Travel › Alps`.
    pub(crate) fn folder_label(&self, folder: &CatalogFolderId) -> Option<String> {
        let mut parts = Vec::new();
        let mut at = Some(folder.clone());
        while let Some(id) = at {
            let held = self.folders.iter().find(|held| held.id == id)?;
            parts.push(held.name.clone());
            at = held.parent_id.clone();
            if parts.len() > self.folders.len() {
                break;
            }
        }
        parts.reverse();
        Some(parts.join(" \u{203a} "))
    }
}

/// `pick.develop` of the picks in the caller's view, as the confirmation stands: each event into
/// its folder, with the removable media answered. The request an agent sends for the same choice.
pub(crate) fn develop_params(confirmation: &Confirmation, mutation: &MutationRequest) -> Value {
    let mut params = json!({
        "into": confirmation.into(),
        "mutation": mutation,
    });
    if confirmation.sends_copies() {
        params["use_copies"] = json!(true);
    }
    if confirmation.confirms_removable() {
        params["confirm_removable"] = json!(true);
    }
    params
}

/// What the confirmation draws.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfirmModel {
    /// "Develop 18 picks".
    pub(crate) title: String,
    /// "from Konstanz".
    pub(crate) from: String,
    /// "Into the catalog folder", or "Into catalog folders" for several events.
    pub(crate) heading: String,
    pub(crate) events: Vec<EventRow>,
    /// What happens to the picks on each removable volume, and to offline ones.
    pub(crate) notes: Vec<String>,
    /// Whether the card's copies are used, when some picks have them.
    pub(crate) copies: Option<bool>,
    /// Why Develop cannot be pressed.
    pub(crate) refusal: Option<String>,
}

/// One event of the confirmation: its place's field or chosen folder, the folders its picks go
/// into, and its menu of existing folders when open.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct EventRow {
    /// The event's own name and how many picks it holds, shown when there are several events:
    /// "Konstanz · Sep 2026 · 12 picks".
    pub(crate) label: Option<String>,
    /// The place's name as typed, or the existing folder's own name, which typing over turns into
    /// a place of that name.
    pub(crate) field: String,
    /// The existing folder chosen, by its label.
    pub(crate) existing: Option<String>,
    /// The folders the picks go into, beneath: "2026 › Konstanz › 12 Sep, 13 Sep", one line per
    /// year, and "Undated › Konstanz" for picks with no capture time; for an existing folder with
    /// day folders, "Travel › Alps › 12 Sep, 13 Sep".
    pub(crate) folders: Vec<String>,
    /// The tag beside it: "place" or "existing folder".
    pub(crate) tag: &'static str,
    /// The open menu: every folder by its label.
    pub(crate) menu: Option<Vec<(CatalogFolderId, String)>>,
}

/// "3 picks", "1 pick".
fn picks(count: u32) -> String {
    if count == 1 {
        "1 pick".to_owned()
    } else {
        format!("{} picks", thousands(count))
    }
}

/// What the confirmation says about the picks on removable volumes, one sentence per volume, and
/// about offline picks. A card's picks with copies use them once their fingerprints match, unless
/// the person chose the card; the rest are developed from the card, which the catalog then points
/// at.
fn notes(confirmation: &Confirmation) -> Vec<String> {
    let mut volumes: Vec<(String, u32, u32)> = Vec::new();
    for picks in confirmation
        .plan
        .events
        .iter()
        .flat_map(|event| &event.removable)
    {
        match volumes.iter_mut().find(|(label, ..)| *label == picks.label) {
            Some((_, count, copies)) => {
                *count += picks.count;
                *copies += picks.with_copy;
            }
            None => volumes.push((picks.label.clone(), picks.count, picks.with_copy)),
        }
    }
    let copies_used = confirmation.use_copies;
    let mut notes: Vec<String> = volumes
        .into_iter()
        .map(|(label, count, copies)| {
            let on = if count == 1 {
                format!("1 pick is on {label}.")
            } else {
                format!("{} picks are on {label}.", thousands(count))
            };
            if copies == 0 || !copies_used {
                format!("{on} They are developed from it, and the catalog points at it.")
            } else if copies == count {
                let their = if count == 1 {
                    "Its copy"
                } else {
                    "Their copies"
                };
                format!("{on} {their} in indexed folders will be used once the fingerprints match.")
            } else {
                let have = if copies == 1 {
                    "1 has a copy".to_owned()
                } else {
                    format!("{} have copies", thousands(copies))
                };
                let rest = count - copies;
                let other = if rest == 1 {
                    "the other is".to_owned()
                } else {
                    format!("the other {} are", thousands(rest))
                };
                format!(
                    "{on} {have} in indexed folders, used once the fingerprints match; {other} \
                     developed from it, and the catalog points at it."
                )
            }
        })
        .collect();
    let offline = confirmation.plan.offline;
    if offline > 0 {
        notes.push(if offline == 1 {
            "1 pick is offline: it stays picked until its volume is connected.".to_owned()
        } else {
            format!(
                "{} picks are offline: they stay picked until their volume is connected.",
                thousands(offline)
            )
        });
    }
    notes
}

/// The confirmation's model.
pub(crate) fn confirm_model(confirmation: &Confirmation) -> ConfirmModel {
    let several = confirmation.plan.events.len() > 1;
    let events = confirmation
        .plan
        .events
        .iter()
        .zip(&confirmation.events)
        .enumerate()
        .map(|(index, (event, choice))| event_row(confirmation, index, event, choice, several))
        .collect();
    let (_, copies) = confirmation.removable();
    ConfirmModel {
        title: format!("Develop {}", picks(confirmation.plan.count)),
        from: format!("from {}", confirmation.from),
        heading: if several {
            "Into catalog folders".to_owned()
        } else {
            "Into the catalog folder".to_owned()
        },
        events,
        notes: notes(confirmation),
        copies: (copies > 0).then_some(confirmation.use_copies),
        refusal: confirmation.refusal(),
    }
}

fn event_row(
    confirmation: &Confirmation,
    index: usize,
    event: &PlannedEvent,
    choice: &EventChoice,
    several: bool,
) -> EventRow {
    let existing = choice.existing.as_ref().map(|folder| {
        confirmation
            .folder_label(folder)
            .or_else(|| event.folder_name.clone())
            .unwrap_or_else(|| "a catalog folder".to_owned())
    });
    let menu = (confirmation.menu == Some(index)).then(|| {
        confirmation
            .folders
            .iter()
            .map(|folder| {
                (
                    folder.id.clone(),
                    confirmation
                        .folder_label(&folder.id)
                        .unwrap_or_else(|| folder.name.clone()),
                )
            })
            .collect()
    });
    let folders = match &existing {
        Some(label) if choice.days && !event.days.is_empty() => {
            vec![format!("{label} \u{203a} {}", day_names(&event.days))]
        }
        Some(label) => vec![label.clone()],
        None => place_folders(choice.name.trim(), event),
    };
    let field = match &choice.existing {
        Some(folder) => confirmation
            .folders
            .iter()
            .find(|held| held.id == *folder)
            .map(|held| held.name.clone())
            .or_else(|| event.folder_name.clone())
            .unwrap_or_default(),
        None => choice.name.clone(),
    };
    EventRow {
        label: several.then(|| format!("{} \u{b7} {}", event.name, picks(event.count))),
        field,
        tag: if choice.existing.is_some() {
            "existing folder"
        } else {
            "place"
        },
        existing,
        folders,
        menu,
    }
}

/// English month names as day folders are named: "12 Sep".
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days as their folders are named, joined: "12 Sep, 13 Sep".
fn day_names(days: &[LocalDay]) -> String {
    days.iter()
        .map(|day| {
            let (_, month, day) = day.ymd();
            format!("{day} {}", MONTHS[month as usize - 1])
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The folders a place choice files `event`'s picks in: under each capture year, the place and its
/// days, and "Undated › place" for picks with no capture time.
fn place_folders(place: &str, event: &PlannedEvent) -> Vec<String> {
    let mut lines = Vec::new();
    let mut at = 0;
    while at < event.days.len() {
        let year = event.days[at].ymd().0;
        let end = event.days[at..]
            .iter()
            .position(|day| day.ymd().0 != year)
            .map_or(event.days.len(), |offset| at + offset);
        lines.push(format!(
            "{year:04} \u{203a} {place} \u{203a} {}",
            day_names(&event.days[at..end])
        ));
        at = end;
    }
    if event.undated > 0 {
        lines.push(format!("Undated \u{203a} {place}"));
    }
    lines
}

// -- The filmstrip ---------------------------------------------------------------------------------

/// The most photographs the filmstrip holds of a folder; a larger folder's first ones are held, and
/// its header says so.
pub(crate) const MAX_STRIP_PHOTOS: usize = 100_000;

/// One photograph of the filmstrip: its identity and the name its original has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StripPhoto {
    pub(crate) asset_id: AssetId,
    pub(crate) kind: Option<luxforge_core::SourceTag>,
    pub(crate) name: String,
}

/// The Develop folder as read for the filmstrip: its path from the top level, its photographs with
/// its subfolders', captured oldest first, at most [`MAX_STRIP_PHOTOS`], how many it holds, and the
/// library change it was read at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderRead {
    pub(crate) folder: CatalogFolderId,
    pub(crate) path: Vec<String>,
    pub(crate) photos: Vec<StripPhoto>,
    pub(crate) count: u32,
    pub(crate) library_sequence: u64,
}

/// The filmstrip's header for a folder's path: "2026 › Konstanz", a deep path shortened to its top
/// level and its last two folders, "2026 › … › Konstanz › 12 Sep".
pub(crate) fn path_title(path: &[String]) -> String {
    match path {
        [] => "Catalog folder".to_owned(),
        [top, .., parent, last] if path.len() > 3 => {
            format!("{top} \u{203a} \u{2026} \u{203a} {parent} \u{203a} {last}")
        }
        _ => path.join(" \u{203a} "),
    }
}

/// What Develop's filmstrip holds: the Develop folder's photographs in order, or, until its folder
/// is read, the photographs Develop opened on; the one on screen, outlined, unless it is not in the
/// folder; and the others selected. Held as ids, read a window of rows at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Filmstrip {
    /// Its number, raised for every new strip: the strip's previews are read under it.
    pub(crate) serial: u64,
    /// The catalog folder it holds, once known.
    pub(crate) folder: Option<CatalogFolderId>,
    /// The folder's path from the top level, as last read.
    pub(crate) path: Vec<String>,
    pub(crate) photos: Vec<StripPhoto>,
    /// How many photographs the folder holds, when it holds more than the strip.
    pub(crate) truncated: Option<u32>,
    /// The index of the photograph Develop shows or is switching to; `None` while it is one that
    /// is not in the folder.
    pub(crate) active: Option<usize>,
    /// The first cell the filmstrip draws.
    pub(crate) first: usize,
    /// The folder's photographs are still being read into it.
    pub(crate) reading: bool,
    /// Other selected indices; the active photograph is always selected implicitly.
    pub(crate) selected: std::collections::BTreeSet<usize>,
}

impl Filmstrip {
    pub(crate) fn new(serial: u64, photos: Vec<StripPhoto>, active: usize) -> Self {
        let active = (!photos.is_empty()).then(|| active.min(photos.len() - 1));
        Self {
            serial,
            folder: None,
            path: Vec::new(),
            photos,
            truncated: None,
            active,
            first: active.unwrap_or(0),
            reading: false,
            selected: Default::default(),
        }
    }

    /// Take in the folder as read again: its photographs replace the strip's, and the photograph
    /// `active` names keeps its place when it is still in the folder, with no cell outlined when it
    /// is not. The selection is cleared, as the photographs it named may have moved.
    pub(crate) fn adopt(&mut self, read: FolderRead, active: Option<&AssetId>) {
        let truncated = read.count as usize > read.photos.len();
        self.folder = Some(read.folder);
        self.path = read.path;
        self.photos = read.photos;
        self.truncated = truncated.then_some(read.count);
        self.active = active.and_then(|asset| self.position(asset));
        self.reading = false;
        self.selected.clear();
        if let Some(active) = self.active {
            self.first = active;
        }
    }

    /// Where the photograph `asset` is in the strip.
    pub(crate) fn position(&self, asset: &AssetId) -> Option<usize> {
        self.photos
            .iter()
            .position(|photo| &photo.asset_id == asset)
    }

    pub(crate) fn selected(&self, index: usize) -> bool {
        index < self.photos.len() && (Some(index) == self.active || self.selected.contains(&index))
    }

    pub(crate) fn select(&mut self, index: usize, command: bool, shift: bool) {
        if index >= self.photos.len() {
            return;
        }
        let anchor = self.active.unwrap_or(index);
        if shift {
            self.selected = (anchor.min(index)..=anchor.max(index)).collect();
        } else if command && Some(index) != self.active && !self.selected.remove(&index) {
            self.selected.insert(index);
        }
    }

    /// How many photographs are selected: the others plus the active one, which is always selected.
    pub(crate) fn selected_count(&self) -> usize {
        self.selected.len()
            + usize::from(
                self.active
                    .is_some_and(|active| !self.selected.contains(&active)),
            )
    }

    pub(crate) fn selected_assets(&self) -> Vec<AssetId> {
        self.photos
            .iter()
            .enumerate()
            .filter(|(index, _)| self.selected(*index))
            .map(|(_, photo)| photo.asset_id.clone())
            .collect()
    }

    /// The photograph at `index` of the strip.
    pub(crate) fn photo(&self, index: usize) -> Option<&StripPhoto> {
        self.photos.get(index)
    }

    /// The index `delta` photographs from the active one, when there is one: the strip does not
    /// wrap. From a photograph not in the folder, `→` goes to the first and `←` to the last.
    pub(crate) fn step(&self, delta: isize) -> Option<usize> {
        let index = match self.active {
            Some(active) => active.checked_add_signed(delta)?,
            None if delta > 0 => 0,
            None => self.photos.len().checked_sub(1)?,
        };
        (index < self.photos.len()).then_some(index)
    }

    /// Keep the active photograph's cell in the filmstrip's window of `capacity` cells, moving the
    /// window as little as shows it.
    pub(crate) fn reveal(&mut self, capacity: usize) {
        let capacity = capacity.max(1);
        if let Some(active) = self.active {
            if active < self.first {
                self.first = active;
            } else if active >= self.first + capacity {
                self.first = active + 1 - capacity;
            }
        }
        let last_first = self.photos.len().saturating_sub(capacity);
        self.first = self.first.min(last_first);
    }

    /// The header's caption: the folder being read, the selection's count, or, for a folder larger
    /// than the strip, that only its first photographs are held. `None` lets the widget say the
    /// active photograph's place ("3 of 18").
    fn caption(&self) -> Option<String> {
        if self.reading {
            return Some("Reading the folder\u{2026}".into());
        }
        let count = self.selected_count();
        if count > 1 {
            return Some(format!(
                "{} of {} selected",
                thousands(count as u32),
                thousands(self.photos.len() as u32)
            ));
        }
        self.truncated.map(|total| {
            format!(
                "The first {} of {}",
                thousands(self.photos.len() as u32),
                thousands(total)
            )
        })
    }
}

/// The photographs a Develop answered, in order, each once: what it created, linked or relinked.
/// Names come from the files they were developed from.
pub(crate) fn developed_photos(report: &DevelopReport) -> Vec<StripPhoto> {
    let mut photos: Vec<StripPhoto> = Vec::with_capacity(report.developed.len());
    for developed in &report.developed {
        if photos
            .iter()
            .any(|held| held.asset_id == developed.asset_id)
        {
            continue;
        }
        let file = developed.used.as_ref().unwrap_or(&developed.path);
        photos.push(StripPhoto {
            asset_id: developed.asset_id.clone(),
            kind: None,
            name: file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        });
    }
    photos
}

/// What the status bar says once a Develop has ended: how many it developed, and the first thing
/// it could not.
pub(crate) fn developed_sentence(report: &DevelopReport) -> String {
    let developed = developed_photos(report).len() as u32;
    let first = report.failed.first().map(|failure| {
        let name = failure
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{name}: {}", failure.message)
    });
    match (developed, first) {
        (0, Some(first)) => format!("Could not develop the picks \u{b7} {first}"),
        (0, None) => "Nothing was developed".to_owned(),
        (developed, None) => format!("Developed {}", thousands(developed)),
        (developed, Some(first)) => format!(
            "Developed {} \u{b7} {} could not be developed \u{b7} {first}",
            thousands(developed),
            thousands(report.failed.len() as u32)
        ),
    }
}

/// A Develop running on the library lane: its job, once `pick.develop` answered with it, and how
/// many picks it develops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Developing {
    pub(crate) job: Option<String>,
    pub(crate) total: u32,
}

/// The cached large preview drawn in place of the photograph being switched to, until its
/// original is prepared and its exact render replaces it. It is of `asset` at `entry`: never drawn
/// for another photograph or entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShownPreview {
    pub(crate) asset: AssetId,
    pub(crate) entry: Option<EntryId>,
    pub(crate) name: String,
    pub(crate) origin: PreviewOrigin,
    pub(crate) approximate: bool,
    /// The preview's own size.
    pub(crate) size: (u32, u32),
    /// The photo surface's version it was handed over as.
    pub(crate) version: u64,
    /// The key it was read under.
    pub(crate) key: String,
}

impl ShownPreview {
    /// The status bar's render slot: a preview, what kind, and "approximate" when it says so.
    pub(crate) fn render_text(&self) -> String {
        let kind = match self.origin {
            PreviewOrigin::Rendered => "Cached preview",
            _ => "Camera preview",
        };
        if self.approximate {
            format!("{kind} \u{b7} approximate")
        } else {
            kind.to_owned()
        }
    }

    /// The status line while it is on screen.
    pub(crate) fn status_text(&self) -> String {
        format!(
            "Showing {} \u{b7} cached preview while the original prepares",
            self.name
        )
    }
}

/// What the app holds for developing picks and the filmstrip, as the model reads it. The Develop
/// folder itself is the catalog's view state (`CatalogState::develop_folder`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DevelopState {
    /// Develop N's confirmation, once `pick.plan` answered.
    pub(crate) confirm: Option<Confirmation>,
    /// `pick.plan` is in flight for the confirmation.
    pub(crate) planning: bool,
    /// A Develop this desktop confirmed is running.
    pub(crate) developing: Option<Developing>,
    /// The filmstrip: the Develop folder's photographs, while Develop holds one.
    pub(crate) strip: Option<Filmstrip>,
    /// `D` or the Develop tab asked for Develop, and the Develop folder is being read for it.
    pub(crate) entering: bool,
    /// The filmstrip is collapsed (`Cmd+Option+F`).
    pub(crate) collapsed: bool,
    /// The photograph being switched to, until its exact render is on screen: its name, for the
    /// title bar.
    pub(crate) switching: Option<String>,
    /// The cached preview on screen in place of the photograph.
    pub(crate) preview: Option<ShownPreview>,
    /// How many filmstrip cells fit the strip's width.
    pub(crate) capacity: usize,
}

impl DevelopState {
    /// The filmstrip is drawn under the canvas: Develop holds a folder and it is not collapsed.
    pub(crate) fn strip_shown(&self) -> bool {
        self.strip.is_some() && !self.collapsed
    }
}

/// What the filmstrip draws: the folder's window of photographs, the active one and the caption.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StripModel {
    /// The folder's path: "2026 › Konstanz".
    pub(crate) title: String,
    /// "Reading the folder…" while it is read, the selection's count, or that a large folder's
    /// first photographs are held; otherwise the active photograph's place, which the widget words
    /// itself.
    pub(crate) caption: Option<String>,
    /// The strip's photographs of the window, from `first`.
    pub(crate) cells: Vec<AssetId>,
    pub(crate) first: usize,
    pub(crate) total: usize,
    pub(crate) active: Option<usize>,
    pub(crate) selected: Vec<bool>,
    pub(crate) can_previous: bool,
    pub(crate) can_next: bool,
}

/// What the confirmation, Develop N, the filmstrip and the status bar draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DevelopModel {
    /// The Develop tab and `D` open Develop: a folder is chosen or a photograph is open.
    pub(crate) can_enter: bool,
    pub(crate) confirm: Option<ConfirmModel>,
    /// Develop N reads busy with the Develop's progress, `(done, total)`.
    pub(crate) busy: Option<(usize, usize)>,
    /// Develop N waits for `pick.plan`.
    pub(crate) planning: bool,
    pub(crate) strip: Option<StripModel>,
    /// The render slot's words while a cached preview is on screen.
    pub(crate) render: Option<String>,
    /// The title bar's file name while a photograph is being switched to.
    pub(crate) switching: Option<String>,
}

/// The model, from the state and the Develop's progress as long-running work last read it.
pub(crate) fn derive(state: &DevelopState, progress: Option<f64>) -> DevelopModel {
    let strip = state
        .strip
        .as_ref()
        .filter(|_| !state.collapsed)
        .map(|strip| {
            let end = (strip.first + state.capacity.max(1)).min(strip.photos.len());
            StripModel {
                title: path_title(&strip.path),
                caption: strip.caption(),
                cells: strip.photos[strip.first.min(end)..end]
                    .iter()
                    .map(|photo| photo.asset_id.clone())
                    .collect(),
                first: strip.first,
                total: strip.photos.len(),
                active: strip.active,
                selected: (strip.first.min(end)..end)
                    .map(|index| strip.selected(index))
                    .collect(),
                can_previous: strip.step(-1).is_some(),
                can_next: strip.step(1).is_some(),
            }
        });
    DevelopModel {
        can_enter: state.strip.is_some(),
        confirm: state.confirm.as_ref().map(confirm_model),
        busy: state.developing.as_ref().map(|developing| {
            let total = developing.total as usize;
            let done = progress.map_or(0, |fraction| (fraction * total as f64).round() as usize);
            (done.min(total), total)
        }),
        planning: state.planning,
        strip,
        render: state.preview.as_ref().map(ShownPreview::render_text),
        switching: state.switching.clone(),
    }
}

#[cfg(test)]
#[path = "develop_tests.rs"]
mod filmstrip_tests;
