//! The Export sheet's state and model ([design](../../../../docs/design/export-settings.md#the-export-sheet)).
//!
//! The sheet holds one [`ExportSettings`] the person is choosing, the preset it started from and
//! what each typed field reads, and nothing else of its own: every readout, suggested name and
//! notice comes from `export.plan`'s answer ([`SheetPlan`]), and every rule a value must meet is
//! [`ExportSettings::check`]'s. The plan is asked again only when a setting it depends on changes
//! ([`plan_key`]): the quality slider and the metadata level never ask for one. At most one plan is
//! in flight; a change made meanwhile waits as one pending plan of the newest settings
//! ([`PlanState`]).
// The choices the sheet's controls send, which the view names through this model.
use luxforge_core::{
    AssetId, EntryId, ExportFolder, ExportPreset, ExportSettings, FormatSettings, OutputSize,
};
pub(crate) use luxforge_core::{Chroma, ExportFormat, MetadataLevel, tiles::Depth};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The built-in Web preset's identity, which the sheet opens on the first time.
pub(crate) const WEB: &str = "luxforge.web";
/// The long edge and box a size starts from when its fit is chosen with nothing typed.
const DEFAULT_LONG_EDGE: u32 = 2048;
/// A size limit's starting value, in kilobytes.
const DEFAULT_LIMIT_KB: u64 = 500;
/// The density Set print size starts from.
const DEFAULT_DENSITY: u16 = 300;
/// The output above which an optimised or size-limited export warns, as `export.plan` counts it.
const LARGE_IMAGE_PIXELS: u64 = 24_000_000;

/// What the sheet exports: the displayed entry of the open photograph, or the photographs Select's
/// Export N… names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SheetTarget {
    Single {
        asset_id: AssetId,
        entry_id: EntryId,
        /// The photograph's file name, which the title names.
        file_name: String,
        /// Its stem, which every name the sheet writes starts with.
        stem: String,
    },
    Batch {
        count: u32,
        raw: u32,
        jpeg: u32,
        /// The photographs whose rows the desktop holds, which the sheet plans for its notices.
        assets: Vec<AssetId>,
    },
}

impl SheetTarget {
    pub(crate) fn batch(&self) -> bool {
        matches!(self, Self::Batch { .. })
    }
}

/// How the size is bounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fit {
    Full,
    LongEdge,
    Within,
}

impl Fit {
    pub(crate) const ALL: [Self; 3] = [Self::Full, Self::LongEdge, Self::Within];

    fn of(size: OutputSize) -> Self {
        match size {
            OutputSize::Full => Self::Full,
            OutputSize::LongEdge { .. } => Self::LongEdge,
            OutputSize::Within { .. } => Self::Within,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Full => "Full size",
            Self::LongEdge => "Long edge",
            Self::Within => "Fit within",
        }
    }
}

/// A typed field of the sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    LongEdge,
    Width,
    Height,
    Density,
    LimitKb,
    Suffix,
}

/// What each typed field reads, as typed: a field that does not read as a value keeps the settings
/// as they were and says why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Texts {
    pub(crate) long_edge: String,
    pub(crate) width: String,
    pub(crate) height: String,
    pub(crate) density: String,
    pub(crate) limit_kb: String,
    pub(crate) suffix: String,
}

impl Texts {
    fn of(settings: &ExportSettings) -> Self {
        let (long_edge, width, height) = match settings.size {
            OutputSize::Full => (DEFAULT_LONG_EDGE, DEFAULT_LONG_EDGE, DEFAULT_LONG_EDGE),
            OutputSize::LongEdge { pixels } => (pixels, pixels, pixels),
            OutputSize::Within { width, height } => (width.max(height), width, height),
        };
        Self {
            long_edge: long_edge.to_string(),
            width: width.to_string(),
            height: height.to_string(),
            density: settings
                .pixels_per_inch
                .unwrap_or(DEFAULT_DENSITY)
                .to_string(),
            limit_kb: settings
                .limit_bytes()
                .map_or(DEFAULT_LIMIT_KB, |bytes| bytes.div_ceil(1000))
                .to_string(),
            suffix: settings.suffix.clone(),
        }
    }

    fn get(&self, field: Field) -> &str {
        match field {
            Field::LongEdge => &self.long_edge,
            Field::Width => &self.width,
            Field::Height => &self.height,
            Field::Density => &self.density,
            Field::LimitKb => &self.limit_kb,
            Field::Suffix => &self.suffix,
        }
    }

    fn set(&mut self, field: Field, text: String) {
        match field {
            Field::LongEdge => self.long_edge = text,
            Field::Width => self.width = text,
            Field::Height => self.height = text,
            Field::Density => self.density = text,
            Field::LimitKb => self.limit_kb = text,
            Field::Suffix => self.suffix = text,
        }
    }
}

/// The format-specific choices the sheet keeps while another format is chosen, so choosing a
/// format back restores them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Kept {
    quality: u8,
    chroma: Chroma,
    limit_bytes: Option<u64>,
    bit_depth: Depth,
}

impl Kept {
    fn of(settings: &ExportSettings) -> Self {
        Self {
            quality: settings.quality().unwrap_or(90),
            chroma: settings.chroma().unwrap_or(Chroma::Full),
            limit_bytes: settings.limit_bytes(),
            bit_depth: match settings.format {
                FormatSettings::Tiff { bit_depth } => bit_depth,
                _ => Depth::Sixteen,
            },
        }
    }

    fn format(self, format: ExportFormat) -> FormatSettings {
        match format {
            ExportFormat::Jpeg => FormatSettings::Jpeg {
                quality: self.quality,
                chroma: self.chroma,
                limit_bytes: self.limit_bytes,
            },
            ExportFormat::Webp => FormatSettings::Webp {
                quality: self.quality,
                limit_bytes: self.limit_bytes,
            },
            ExportFormat::Tiff => FormatSettings::Tiff {
                bit_depth: self.bit_depth,
            },
        }
    }
}

/// One plan's answer: `export.plan`'s for the one photograph, or the batch's notices counted over
/// the plans of the photographs it names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SheetPlan {
    Single(Value),
    Batch(BatchPlan),
}

/// What the plans of a batch's photographs say together.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BatchPlan {
    /// How many photographs were planned.
    pub(crate) planned: u32,
    /// How many carry `large-image`, and the largest estimate among them.
    pub(crate) large: u32,
    pub(crate) largest: u64,
    /// Each refusing notice's code, how many photographs it would leave out and the first one's
    /// words.
    pub(crate) refused: Vec<(String, u32, String)>,
    /// How many photographs the batch will leave out: those with a refusing notice, each once,
    /// and those that could not be planned.
    pub(crate) left_out: u32,
    /// How many could not be planned at all, and the first reason.
    pub(crate) failed: u32,
    pub(crate) failure: Option<String>,
    /// The first photograph's suggested file name.
    pub(crate) first_name: Option<String>,
}

impl BatchPlan {
    /// Count one photograph's plan, or its refusal.
    pub(crate) fn count(&mut self, plan: Result<&Value, &str>) {
        self.planned += 1;
        let plan = match plan {
            Ok(plan) => plan,
            Err(reason) => {
                self.failed += 1;
                self.left_out += 1;
                self.failure.get_or_insert_with(|| reason.to_owned());
                return;
            }
        };
        if self.first_name.is_none()
            && let Some(suggested) = plan["suggested"].as_str()
        {
            self.first_name = Path::new(suggested)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
        }
        let mut refuses = false;
        for notice in plan["notices"].as_array().into_iter().flatten() {
            let code = notice["code"].as_str().unwrap_or_default();
            if notice["refuses"] == true {
                refuses = true;
                match self.refused.iter_mut().find(|(seen, ..)| seen == code) {
                    Some((_, count, _)) => *count += 1,
                    None => self.refused.push((
                        code.to_owned(),
                        1,
                        notice["message"].as_str().unwrap_or_default().to_owned(),
                    )),
                }
            } else if code == "large-image" {
                self.large += 1;
                self.largest = self.largest.max(notice["bytes"].as_u64().unwrap_or(0));
            }
        }
        if refuses {
            self.left_out += 1;
        }
    }
}

/// The sheet's plans: the newest answer and the settings it answered, whether one is in flight and
/// whether a newer change waits for it, and how many the sheet has asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PlanState {
    pub(crate) answer: Option<SheetPlan>,
    pub(crate) answered: Option<ExportSettings>,
    /// The settings of the plan last asked for, answered or in flight.
    pub(crate) asked: Option<ExportSettings>,
    pub(crate) error: Option<String>,
    pub(crate) in_flight: bool,
    pub(crate) pending: bool,
    pub(crate) sent: u32,
}

/// What a name typed in the sheet is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NamingTarget {
    /// Save as preset… or `+`: a new preset of the current settings.
    New,
    /// Rename… of a preset of the person's own.
    Rename(String),
    /// Duplicate of any preset: a new preset of its settings.
    Duplicate(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Naming {
    pub(crate) target: NamingTarget,
    pub(crate) text: String,
}

/// The open sheet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExportSheet {
    pub(crate) target: SheetTarget,
    /// It sends `reference: true`: opened by the palette's Export reference render….
    pub(crate) reference: bool,
    pub(crate) settings: ExportSettings,
    /// The preset card selected, whose settings mark the sheet changed when they differ.
    pub(crate) preset_id: Option<String>,
    pub(crate) texts: Texts,
    /// A typed field that does not read as a value: the field and why.
    pub(crate) invalid: Option<(Field, String)>,
    kept: Kept,
    /// The folder Choose… last chose, kept while Original's folder is selected.
    pub(crate) chosen_folder: Option<PathBuf>,
    pub(crate) plan: PlanState,
    pub(crate) naming: Option<Naming>,
    /// The preset card whose menu is open.
    pub(crate) card_menu: Option<String>,
    /// A preset change is in flight.
    pub(crate) changing: bool,
    /// The last preset change's refusal.
    pub(crate) preset_error: Option<String>,
}

impl ExportSheet {
    /// A sheet over `target` with `settings`, from the preset `preset_id`.
    pub(crate) fn new(
        target: SheetTarget,
        reference: bool,
        settings: ExportSettings,
        preset_id: Option<String>,
    ) -> Self {
        Self {
            target,
            reference,
            texts: Texts::of(&settings),
            kept: Kept::of(&settings),
            chosen_folder: match &settings.folder {
                ExportFolder::Path(path) => Some(path.clone()),
                ExportFolder::Original => None,
            },
            settings,
            preset_id,
            invalid: None,
            plan: PlanState::default(),
            naming: None,
            card_menu: None,
            changing: false,
            preset_error: None,
        }
    }

    /// The settings the sheet would export with, when every typed field reads as a value and they
    /// meet every rule.
    pub(crate) fn valid(&self) -> Option<&ExportSettings> {
        (self.invalid.is_none() && self.settings.check().is_ok()).then_some(&self.settings)
    }

    /// Replace the settings, keeping what the other formats chose.
    fn adopt(&mut self, settings: ExportSettings) {
        self.kept = Kept::of(&settings);
        self.settings = settings;
    }

    /// Choose a preset card: its settings, as listed.
    pub(crate) fn choose(&mut self, preset: &ExportPreset) {
        self.preset_id = Some(preset.preset_id.clone());
        self.adopt(preset.settings.clone());
        self.texts = Texts::of(&self.settings);
        self.invalid = None;
        self.chosen_folder = match &self.settings.folder {
            ExportFolder::Path(path) => Some(path.clone()),
            ExportFolder::Original => self.chosen_folder.take(),
        };
    }

    pub(crate) fn set_format(&mut self, format: ExportFormat) {
        if self.settings.format() != format {
            self.settings.format = self.kept.format(format);
        }
    }

    pub(crate) fn set_fit(&mut self, fit: Fit) {
        if Fit::of(self.settings.size) == fit {
            return;
        }
        let number = |text: &str| text.trim().parse::<u32>().unwrap_or(DEFAULT_LONG_EDGE);
        self.settings.size = match fit {
            Fit::Full => OutputSize::Full,
            Fit::LongEdge => OutputSize::LongEdge {
                pixels: number(&self.texts.long_edge),
            },
            Fit::Within => OutputSize::Within {
                width: number(&self.texts.width),
                height: number(&self.texts.height),
            },
        };
        self.clear_invalid(&[Field::LongEdge, Field::Width, Field::Height]);
    }

    pub(crate) fn set_quality(&mut self, quality: u8) {
        let quality = quality.clamp(1, 100);
        self.kept.quality = quality;
        match &mut self.settings.format {
            FormatSettings::Jpeg { quality: q, .. } | FormatSettings::Webp { quality: q, .. } => {
                *q = quality
            }
            FormatSettings::Tiff { .. } => {}
        }
    }

    pub(crate) fn set_optimise(&mut self, on: bool) {
        self.settings.optimise = on;
    }

    pub(crate) fn set_chroma(&mut self, chroma: Chroma) {
        self.kept.chroma = chroma;
        if let FormatSettings::Jpeg { chroma: c, .. } = &mut self.settings.format {
            *c = chroma;
        }
    }

    pub(crate) fn set_bit_depth(&mut self, depth: Depth) {
        self.kept.bit_depth = depth;
        if let FormatSettings::Tiff { bit_depth } = &mut self.settings.format {
            *bit_depth = depth;
        }
    }

    pub(crate) fn set_limit(&mut self, on: bool) {
        let limit = on.then(|| kilobytes(&self.texts.limit_kb).unwrap_or(DEFAULT_LIMIT_KB) * 1000);
        self.set_limit_bytes(limit);
        if !on {
            self.clear_invalid(&[Field::LimitKb]);
        }
    }

    fn set_limit_bytes(&mut self, limit: Option<u64>) {
        self.kept.limit_bytes = limit;
        match &mut self.settings.format {
            FormatSettings::Jpeg { limit_bytes, .. } | FormatSettings::Webp { limit_bytes, .. } => {
                *limit_bytes = limit
            }
            FormatSettings::Tiff { .. } => {}
        }
    }

    pub(crate) fn set_metadata(&mut self, metadata: MetadataLevel) {
        self.settings.metadata = metadata;
    }

    /// Set print size, or remove it.
    pub(crate) fn set_density(&mut self, on: bool) {
        self.settings.pixels_per_inch =
            on.then(|| self.texts.density.trim().parse().unwrap_or(DEFAULT_DENSITY));
        if !on {
            self.clear_invalid(&[Field::Density]);
        }
    }

    /// Original's folder, or the folder Choose… chose.
    pub(crate) fn set_original_folder(&mut self) {
        self.settings.folder = ExportFolder::Original;
    }

    pub(crate) fn set_folder(&mut self, folder: PathBuf) {
        self.chosen_folder = Some(folder.clone());
        self.settings.folder = ExportFolder::Path(folder);
    }

    /// One typed field's text: the settings take it when it reads as a value the settings accept,
    /// and keep what they were otherwise, saying why.
    pub(crate) fn type_text(&mut self, field: Field, text: String) {
        self.texts.set(field, text);
        let text = self.texts.get(field).trim().to_owned();
        let mut next = self.settings.clone();
        let read: Result<(), String> = match field {
            Field::LongEdge => pixels(&text).map(|pixels| {
                if matches!(next.size, OutputSize::LongEdge { .. }) {
                    next.size = OutputSize::LongEdge { pixels };
                }
            }),
            Field::Width | Field::Height => pixels(&text).map(|value| {
                if let OutputSize::Within { width, height } = &mut next.size {
                    if field == Field::Width {
                        *width = value;
                    } else {
                        *height = value;
                    }
                }
            }),
            Field::Density => text
                .parse::<u16>()
                .map_err(|_| "a whole number of pixels per inch".to_owned())
                .map(|density| {
                    if next.pixels_per_inch.is_some() {
                        next.pixels_per_inch = Some(density);
                    }
                }),
            Field::LimitKb => kilobytes(&text)
                .ok_or_else(|| "a whole number of kilobytes".to_owned())
                .map(|kb| {
                    if next.limit_bytes().is_some() {
                        let bytes = Some(kb * 1000);
                        match &mut next.format {
                            FormatSettings::Jpeg { limit_bytes, .. }
                            | FormatSettings::Webp { limit_bytes, .. } => *limit_bytes = bytes,
                            FormatSettings::Tiff { .. } => {}
                        }
                    }
                }),
            Field::Suffix => {
                next.suffix = self.texts.suffix.clone();
                Ok(())
            }
        };
        let read = read.and_then(|()| {
            next.check()
                .map_err(|error| error.detail.trim_start_matches("settings.").to_owned())
        });
        match read {
            Ok(()) => {
                let limit = next.limit_bytes();
                self.settings = next;
                if field == Field::LimitKb {
                    self.kept.limit_bytes = limit;
                }
                self.clear_invalid(&[field]);
            }
            Err(reason) => self.invalid = Some((field, reason)),
        }
    }

    fn clear_invalid(&mut self, fields: &[Field]) {
        if self
            .invalid
            .as_ref()
            .is_some_and(|(field, _)| fields.contains(field))
        {
            self.invalid = None;
        }
    }

    /// The preset the sheet started from, from `presets`.
    pub(crate) fn preset<'a>(&self, presets: &'a [ExportPreset]) -> Option<&'a ExportPreset> {
        let id = self.preset_id.as_deref()?;
        presets.iter().find(|preset| preset.preset_id == id)
    }

    /// The settings differ from the selected preset's: its card says *Changed from preset*.
    pub(crate) fn changed(&self, presets: &[ExportPreset]) -> bool {
        self.preset(presets)
            .is_none_or(|preset| preset.settings != self.settings)
    }

    /// The preset the remembered last export names: the selected one while the settings are its
    /// own, none once they changed.
    pub(crate) fn remembered_preset(&self, presets: &[ExportPreset]) -> Option<String> {
        (!self.changed(presets))
            .then(|| self.preset_id.clone())
            .flatten()
    }

    /// Whether the plan should be asked for these settings: they read as valid, and they differ
    /// from the last plan asked for in something the plan depends on.
    pub(crate) fn wants_plan(&self) -> Option<ExportSettings> {
        let settings = self.valid()?;
        let key = plan_key(settings);
        (self.plan.asked.as_ref().map(plan_key) != Some(key)).then(|| settings.clone())
    }
}

/// The settings with everything `export.plan` does not depend on made equal: the quality, which
/// changes no output size, name, estimate or notice, and the metadata level. Two settings with the
/// same key are planned the same.
pub(crate) fn plan_key(settings: &ExportSettings) -> ExportSettings {
    let mut key = settings.clone();
    match &mut key.format {
        FormatSettings::Jpeg { quality, .. } | FormatSettings::Webp { quality, .. } => *quality = 1,
        FormatSettings::Tiff { .. } => {}
    }
    key.metadata = MetadataLevel::None;
    key
}

fn pixels(text: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .map_err(|_| "a whole number of pixels".to_owned())
}

fn kilobytes(text: &str) -> Option<u64> {
    text.trim().parse::<u64>().ok()
}

// -- The model --------------------------------------------------------------------------------------

/// One preset card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardModel {
    pub(crate) preset_id: String,
    pub(crate) name: String,
    /// `JPEG · 2048 px`, or *Changed from preset* on the selected card once the settings differ.
    pub(crate) detail: String,
    pub(crate) builtin: bool,
    pub(crate) selected: bool,
    pub(crate) changed: bool,
    /// Its menu's choices while open: Update with current settings, Rename…, Duplicate and Delete
    /// for the person's own, Duplicate for a built-in one.
    pub(crate) menu: Option<Vec<CardChoice>>,
}

/// A choice of a preset card's menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardChoice {
    Update,
    Rename,
    Duplicate,
    Delete,
}

impl CardChoice {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Update => "Update with current settings",
            Self::Rename => "Rename\u{2026}",
            Self::Duplicate => "Duplicate",
            Self::Delete => "Delete",
        }
    }
}

/// Quality, Optimise, Chroma and Limit file size, for JPEG and WebP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QualityModel {
    pub(crate) quality: u8,
    /// The slider is disabled while the limit is on.
    pub(crate) slider_enabled: bool,
    pub(crate) optimise: bool,
    pub(crate) optimise_caption: String,
    /// JPEG's chroma: 0 is Full (4:4:4), 1 Half (4:2:0).
    pub(crate) chroma: Option<usize>,
    pub(crate) limit: bool,
    pub(crate) limit_text: String,
    pub(crate) limit_caption: Option<String>,
}

/// The whole sheet as the view draws it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ExportSheetModel {
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) cards: Vec<CardModel>,
    /// The presets have not been listed yet.
    pub(crate) listing: bool,
    pub(crate) format: usize,
    pub(crate) format_caption: String,
    /// TIFF's bit depth: 0 is 16-bit, 1 8-bit.
    pub(crate) bit_depth: Option<usize>,
    pub(crate) fit: usize,
    pub(crate) long_edge: Option<String>,
    pub(crate) within: Option<(String, String)>,
    pub(crate) readout: String,
    /// Print size, while a density is set: the density typed and the physical size.
    pub(crate) density: Option<(String, String)>,
    pub(crate) quality: Option<QualityModel>,
    /// TIFF's Optimise file size, its lossless compression.
    pub(crate) compress: Option<bool>,
    /// The amber large-image notice under Quality, exactly when the plan carries it.
    pub(crate) large_image: Option<String>,
    pub(crate) metadata: usize,
    pub(crate) metadata_caption: String,
    /// 0 is Original's folder, 1 Chosen folder.
    pub(crate) folder: usize,
    pub(crate) folder_path: Option<String>,
    pub(crate) stem: String,
    pub(crate) suffix: String,
    pub(crate) extension: String,
    /// `→ will-sapa-drone-web.jpg`.
    pub(crate) name: Option<String>,
    /// `X exists there; this writes Y`.
    pub(crate) taken: Option<String>,
    /// Why the export cannot be written, or which photographs it will leave out.
    pub(crate) refusals: Vec<String>,
    /// A typed field that does not read as a value.
    pub(crate) invalid: Option<(Field, String)>,
    pub(crate) summary: String,
    /// Update "Name"… for the person's own preset, changed.
    pub(crate) update: Option<String>,
    pub(crate) save_as: bool,
    pub(crate) naming: Option<(String, String)>,
    pub(crate) preset_error: Option<String>,
    pub(crate) export_label: String,
    pub(crate) can_export: bool,
}

/// The sheet's model, or none while it is closed.
pub(crate) fn model(
    sheet: Option<&ExportSheet>,
    presets: Option<&[ExportPreset]>,
    home: Option<&Path>,
) -> Option<ExportSheetModel> {
    let sheet = sheet?;
    let listed = presets.unwrap_or_default();
    let settings = &sheet.settings;
    let changed = sheet.changed(listed);
    let format = settings.format();
    let (title, subtitle, mut export_label, stem) = match &sheet.target {
        SheetTarget::Single {
            file_name, stem, ..
        } => (
            "Export".to_owned(),
            if sheet.reference {
                format!("{file_name} \u{b7} reference renderer")
            } else {
                file_name.clone()
            },
            "Export".to_owned(),
            stem.clone(),
        ),
        SheetTarget::Batch {
            count, raw, jpeg, ..
        } => {
            let kinds: Vec<String> = [(*raw, "RAW"), (*jpeg, "JPEG")]
                .into_iter()
                .filter(|(n, _)| *n > 0)
                .map(|(n, kind)| format!("{n} {kind}"))
                .chain(["current edits".to_owned()])
                .collect();
            (
                format!("Export {}", photos(*count)),
                kinds.join(" \u{b7} "),
                format!("Export {count}"),
                "original name".to_owned(),
            )
        }
    };
    let cards = listed
        .iter()
        .map(|preset| {
            let selected = sheet.preset_id.as_deref() == Some(preset.preset_id.as_str());
            CardModel {
                preset_id: preset.preset_id.clone(),
                name: preset.name.clone(),
                detail: if selected && changed {
                    "Changed from preset".to_owned()
                } else {
                    card_detail(&preset.settings)
                },
                builtin: preset.builtin,
                selected,
                changed: selected && changed,
                menu: (sheet.card_menu.as_deref() == Some(preset.preset_id.as_str())).then(|| {
                    if preset.builtin {
                        vec![CardChoice::Duplicate]
                    } else {
                        vec![
                            CardChoice::Update,
                            CardChoice::Rename,
                            CardChoice::Duplicate,
                            CardChoice::Delete,
                        ]
                    }
                }),
            }
        })
        .collect();
    let single = match &sheet.plan.answer {
        Some(SheetPlan::Single(plan)) => Some(plan),
        _ => None,
    };
    let batch = match &sheet.plan.answer {
        Some(SheetPlan::Batch(plan)) => Some(plan),
        _ => None,
    };
    // A batch writes every photograph its plans do not leave out.
    let writing = match &sheet.target {
        SheetTarget::Single { .. } => None,
        SheetTarget::Batch { count, .. } => {
            Some(count.saturating_sub(batch.map_or(0, |plan| plan.left_out)))
        }
    };
    if let Some(writing) = writing {
        export_label = format!("Export {writing}");
    }
    let readout = match (&sheet.target, single) {
        (SheetTarget::Batch { .. }, _) => match settings.size {
            OutputSize::Full => "Each photo at its full size".to_owned(),
            OutputSize::LongEdge { pixels } => {
                format!(
                    "Each photo\u{2019}s long edge becomes {pixels} px; a smaller photo keeps its size"
                )
            }
            OutputSize::Within { width, height } => format!(
                "Each photo fits within {width} \u{d7} {height} px; a smaller photo keeps its size"
            ),
        },
        (_, Some(plan)) => size_readout(plan, settings.size),
        (_, None) => "Planning\u{2026}".to_owned(),
    };
    let quality = match settings.format {
        FormatSettings::Jpeg {
            quality,
            chroma,
            limit_bytes,
        } => Some((quality, Some(chroma), limit_bytes)),
        FormatSettings::Webp {
            quality,
            limit_bytes,
        } => Some((quality, None, limit_bytes)),
        FormatSettings::Tiff { .. } => None,
    }
    .map(|(quality, chroma, limit)| QualityModel {
        quality,
        slider_enabled: limit.is_none(),
        optimise: settings.optimise,
        optimise_caption: optimise_caption(format, settings.optimise),
        chroma: chroma.map(|chroma| usize::from(chroma == Chroma::Half)),
        limit: limit.is_some(),
        limit_text: sheet.texts.limit_kb.clone(),
        limit_caption: limit.map(|_| {
            format!(
                "Chooses the highest quality whose file fits, at most {quality}. Takes several \
                 encodes; the result names the quality used."
            )
        }),
    });
    let large_image = match (single, batch) {
        (Some(plan), _) => notice_of(plan, "large-image").map(|message| {
            if format == ExportFormat::Jpeg && settings.optimise && settings.limit_bytes().is_none()
            {
                format!("{message}. Turn Optimise off to write it as it renders.")
            } else {
                format!("{message}.")
            }
        }),
        (_, Some(plan)) if plan.large > 0 => Some(format!(
            "Large images: {} over {} MP hold up to about {} MB each while encoding and are \
             several times slower.",
            photos(plan.large),
            LARGE_IMAGE_PIXELS / 1_000_000,
            plan.largest.div_ceil(1_000_000)
        )),
        _ => None,
    };
    let mut refusals: Vec<String> = Vec::new();
    if let Some(plan) = single {
        refusals.extend(
            plan["notices"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|notice| notice["refuses"] == true)
                .filter_map(|notice| notice["message"].as_str())
                .map(sentence),
        );
    }
    if let Some(plan) = batch {
        refusals.extend(plan.refused.iter().map(|(_, count, message)| {
            format!("{} will be left out: {}", photos(*count), sentence(message))
        }));
        if plan.failed > 0 {
            refusals.push(format!(
                "{} could not be planned: {}",
                photos(plan.failed),
                plan.failure.as_deref().map(sentence).unwrap_or_default()
            ));
        }
    }
    if let (Some(error), None) = (&sheet.plan.error, &sheet.plan.answer) {
        refusals.push(format!("Could not plan the export: {error}"));
    }
    let extension = format!(".{}", format.extension());
    let expected = format!("{stem}{}{extension}", settings.suffix);
    let suggested = single
        .and_then(|plan| plan["suggested"].as_str())
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned());
    let name = match &sheet.target {
        SheetTarget::Single { .. } => suggested.clone().map(|name| format!("\u{2192} {name}")),
        SheetTarget::Batch { .. } => batch
            .and_then(|plan| plan.first_name.clone())
            .map(|name| format!("{name}, \u{2026}")),
    };
    let taken = suggested
        .filter(|name| *name != expected)
        .map(|name| format!("{expected} exists there; this writes {name}"));
    let fresh = sheet
        .plan
        .answered
        .as_ref()
        .is_some_and(|answered| plan_key(answered) == plan_key(settings));
    let refuses = single.is_some_and(|plan| {
        plan["notices"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|notice| notice["refuses"] == true)
            || plan["suggested"].is_null()
    });
    let can_export = sheet.valid().is_some()
        && !sheet.changing
        && fresh
        && match &sheet.target {
            SheetTarget::Single { .. } => !refuses,
            SheetTarget::Batch { .. } => writing.is_some_and(|writing| writing > 0),
        };
    let own = sheet
        .preset(listed)
        .filter(|preset| !preset.builtin && changed)
        .map(|preset| format!("Update \u{201c}{}\u{201d}\u{2026}", preset.name));
    Some(ExportSheetModel {
        title,
        subtitle,
        cards,
        listing: presets.is_none(),
        format: match format {
            ExportFormat::Jpeg => 0,
            ExportFormat::Webp => 1,
            ExportFormat::Tiff => 2,
        },
        format_caption: match format {
            ExportFormat::Jpeg => "8-bit sRGB, profile embedded",
            ExportFormat::Webp => "lossy WebP, sRGB profile embedded",
            ExportFormat::Tiff => "sRGB, profile embedded",
        }
        .to_owned(),
        bit_depth: match settings.format {
            FormatSettings::Tiff { bit_depth } => Some(usize::from(bit_depth == Depth::Eight)),
            _ => None,
        },
        fit: Fit::ALL
            .iter()
            .position(|fit| *fit == Fit::of(settings.size))
            .unwrap_or(0),
        long_edge: matches!(settings.size, OutputSize::LongEdge { .. })
            .then(|| sheet.texts.long_edge.clone()),
        within: matches!(settings.size, OutputSize::Within { .. })
            .then(|| (sheet.texts.width.clone(), sheet.texts.height.clone())),
        readout,
        density: settings.pixels_per_inch.map(|_| {
            let physical = single
                .and_then(|plan| plan.get("physical"))
                .map(physical_text)
                .unwrap_or_default();
            (sheet.texts.density.clone(), physical)
        }),
        quality,
        compress: (format == ExportFormat::Tiff).then_some(settings.optimise),
        large_image,
        metadata: MetadataLevel::ALL
            .iter()
            .position(|level| *level == settings.metadata)
            .unwrap_or(0),
        metadata_caption: metadata_caption(settings.metadata).to_owned(),
        folder: usize::from(matches!(settings.folder, ExportFolder::Path(_))),
        folder_path: sheet
            .chosen_folder
            .as_deref()
            .map(|path| crate::state::select::shown_path(path, home)),
        stem,
        suffix: sheet.texts.suffix.clone(),
        extension,
        name,
        taken,
        refusals,
        invalid: sheet.invalid.clone(),
        summary: summary(sheet, single, writing, batch),
        update: own,
        save_as: true,
        naming: sheet.naming.as_ref().map(|naming| {
            (
                match &naming.target {
                    NamingTarget::New => "Save as preset".to_owned(),
                    NamingTarget::Rename(_) => "Rename preset".to_owned(),
                    NamingTarget::Duplicate(_) => "Duplicate as".to_owned(),
                },
                naming.text.clone(),
            )
        }),
        preset_error: sheet.preset_error.clone(),
        export_label,
        can_export,
    })
}

/// `5 photos`, `1 photo`.
pub(crate) fn photos(count: u32) -> String {
    if count == 1 {
        "1 photo".to_owned()
    } else {
        format!("{} photos", crate::state::select::thousands(count))
    }
}

/// A preset card's second line: `JPEG · 2048 px`, `JPEG · full · 300 ppi`, `TIFF · 16-bit · full`.
pub(crate) fn card_detail(settings: &ExportSettings) -> String {
    let mut parts = vec![format_label(settings.format()).to_owned()];
    if let FormatSettings::Tiff { bit_depth } = settings.format {
        parts.push(format!("{}-bit", bit_depth.bytes() * 8));
    }
    parts.push(match settings.size {
        OutputSize::Full => "full".to_owned(),
        OutputSize::LongEdge { pixels } => format!("{pixels} px"),
        OutputSize::Within { width, height } => format!("{width} \u{d7} {height}"),
    });
    if let Some(density) = settings.pixels_per_inch {
        parts.push(format!("{density} ppi"));
    }
    parts.join(" \u{b7} ")
}

pub(crate) fn format_label(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Jpeg => "JPEG",
        ExportFormat::Webp => "WebP",
        ExportFormat::Tiff => "TIFF",
    }
}

fn optimise_caption(format: ExportFormat, on: bool) -> String {
    match (format, on) {
        (ExportFormat::Jpeg, true) => "smaller at the same quality; takes longer",
        (ExportFormat::Jpeg, false) => "off: written as it renders, fastest",
        (ExportFormat::Webp, _) => "slowest WebP method, sharper colour edges",
        (ExportFormat::Tiff, _) => "lossless Deflate; slower to write, identical pixels",
    }
    .to_owned()
}

fn metadata_caption(level: MetadataLevel) -> &'static str {
    match level {
        MetadataLevel::None => "No camera, capture, copyright or GPS fields",
        MetadataLevel::Copyright => {
            "Copyright keeps Artist and Copyright only; no camera, capture or GPS fields"
        }
        MetadataLevel::NoLocation => "Every supported field except the GPS group",
        MetadataLevel::All => "Every supported field, location included",
    }
}

/// A plan notice's words, as the sheet shows them: the message of the notice of `code`.
fn notice_of<'a>(plan: &'a Value, code: &str) -> Option<&'a str> {
    plan["notices"]
        .as_array()?
        .iter()
        .find(|notice| notice["code"] == code)?["message"]
        .as_str()
}

/// A notice's message as a sentence: its first letter capitalised.
fn sentence(message: &str) -> String {
    let mut characters = message.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// `1639 × 2048 px · 3.4 MP · 48% of 3389 × 4236`, or `3389 × 4236 px · 14.4 MP` at full size; a
/// bound the stage is already within says the photograph keeps its size.
fn size_readout(plan: &Value, size: OutputSize) -> String {
    let side = |value: &Value, key: &str| value[key].as_u64().unwrap_or(0);
    let (width, height) = (
        side(&plan["output"], "width"),
        side(&plan["output"], "height"),
    );
    let stage = (
        side(&plan["stage"], "width"),
        side(&plan["stage"], "height"),
    );
    let megapixels = (width * height) as f64 / 1_000_000.0;
    let mut text = format!("{width} \u{d7} {height} px \u{b7} {megapixels:.1} MP");
    if (width, height) != stage && stage.0 > 0 {
        let percent = (width as f64 / stage.0 as f64 * 100.0).round();
        text.push_str(&format!(
            " \u{b7} {percent}% of {} \u{d7} {}",
            stage.0, stage.1
        ));
    } else if size != OutputSize::Full {
        text.push_str(" \u{b7} already within the bound; never enlarged");
    }
    text
}

/// `11.3 × 14.1 in · 28.7 × 35.9 cm`.
fn physical_text(physical: &Value) -> String {
    let pair = |unit: &Value| {
        format!(
            "{:.1} \u{d7} {:.1}",
            unit["width"].as_f64().unwrap_or(0.0),
            unit["height"].as_f64().unwrap_or(0.0)
        )
    };
    format!(
        "{} in \u{b7} {} cm",
        pair(&physical["inches"]),
        pair(&physical["centimetres"])
    )
}

/// The footer's summary: `1639 × 2048 · JPEG 80 · optimised · sRGB`, or for a batch the files it
/// will write and the photographs it leaves out, `4 files · 1 left out · WebP 80 · optimised ·
/// long edge 2048`.
fn summary(
    sheet: &ExportSheet,
    plan: Option<&Value>,
    writing: Option<u32>,
    batch: Option<&BatchPlan>,
) -> String {
    let settings = &sheet.settings;
    let mut parts: Vec<String> = Vec::new();
    match (&sheet.target, writing) {
        (SheetTarget::Single { .. }, _) => {
            if let Some(plan) = plan {
                parts.push(format!(
                    "{} \u{d7} {}",
                    plan["output"]["width"].as_u64().unwrap_or(0),
                    plan["output"]["height"].as_u64().unwrap_or(0)
                ));
            }
        }
        (SheetTarget::Batch { count, .. }, writing) => {
            let writing = writing.unwrap_or(*count);
            parts.push(if writing == 1 {
                "1 file".to_owned()
            } else {
                format!("{writing} files")
            });
            if let Some(left_out) = batch.map(|plan| plan.left_out).filter(|n| *n > 0) {
                parts.push(format!("{left_out} left out"));
            }
        }
    }
    let format = format_label(settings.format());
    parts.push(
        match (settings.quality(), settings.limit_bytes(), settings.format) {
            (_, Some(limit), _) => format!("{format} \u{2264} {} KB", limit / 1000),
            (Some(quality), None, _) => format!("{format} {quality}"),
            (None, _, FormatSettings::Tiff { bit_depth }) => {
                format!("{format} {}-bit", bit_depth.bytes() * 8)
            }
            (None, _, _) => format.to_owned(),
        },
    );
    if settings.optimise {
        parts.push(
            if settings.format() == ExportFormat::Tiff {
                "lossless"
            } else {
                "optimised"
            }
            .to_owned(),
        );
    }
    if sheet.target.batch() {
        match settings.size {
            OutputSize::Full => parts.push("full size".to_owned()),
            OutputSize::LongEdge { pixels } => parts.push(format!("long edge {pixels}")),
            OutputSize::Within { width, height } => {
                parts.push(format!("within {width} \u{d7} {height}"))
            }
        }
    }
    if let Some(density) = settings.pixels_per_inch {
        parts.push(format!("{density} ppi"));
    }
    parts.push("sRGB".to_owned());
    parts.join(" \u{b7} ")
}

#[cfg(test)]
#[path = "export_sheet_tests.rs"]
mod tests;
