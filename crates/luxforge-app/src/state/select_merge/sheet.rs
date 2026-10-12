//! The merge preview sheet ([design](../../../../../docs/design/panorama-merge.md#the-merge-preview);
//! the board `docs/design/merges/hdr-preview.png`), as plain data: what the sheet holds between
//! the owner's answers, and what it draws from them.
//!
//! The sheet opens on Pick merged or Import merged, with that action primary. It shows the
//! frames the plan resolved, the picture the newest `merge-preview` revision drew, with the
//! deghost overlay drawn from the revision's own overlay data, the settings, the folder and the
//! output. While the preview reads and aligns, the picture area says the job's progress, and the
//! settings and the merge buttons stay live. A refusal, from the plan or the preview, replaces the
//! picture and disables both merges; the sheet stays open.
use super::{
    FolderPick, MergeAction, MergeMode, MergeRequest, Toggle, ev_words, plan_refusal,
    refusal_message, words,
};
use luxforge_core::catalog_types::{
    CatalogFolderId, FileId, FolderChoice, MergePlan, MergePreview, MergePreviewDiagnostics,
    MergePreviewId, MergePreviewOverlays,
};
use serde_json::Value;
use std::path::PathBuf;

/// One step of a frame's exposure override: a third of a stop.
pub(crate) const EV_STEP: f64 = 1.0 / 3.0;

/// The words under the sheet's picture while it is a preview, never the saved pixels.
pub(crate) const DISPLAY_NOTE: &str = "Display preview, not the saved pixels";

/// The sheet's footer: how to merge without it.
pub(crate) const LAST_SETTINGS_HINT: &str = "\u{2325}-click Pick merged or Import merged to merge with the last settings, without this preview";

/// What the frames' exposures are and how to correct them.
pub(crate) const EXPOSURE_HINT: &str =
    "Exposure is read from each file; step it by a third of a stop where the metadata is wrong.";

/// What the deghost overlay shows.
pub(crate) const OVERLAY_HINT: &str =
    "Moving regions are tinted and labelled with the exposure they come from.";

/// What the status line says while the preview prepares.
pub(crate) const PREPARING_NOTE: &str = "Settings stay live; merging now continues this work";

/// The view's words for the frames the sheet was opened for, read from their rows: their files,
/// and the first frame's camera and capture time.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SheetContext {
    pub(crate) names: Vec<String>,
    pub(crate) camera: Option<String>,
    pub(crate) capture: Option<String>,
}

/// The open merge preview sheet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreviewSheet {
    /// The frames, the primary action and the settings as the person has them now.
    pub(crate) request: MergeRequest,
    pub(crate) context: SheetContext,
    /// The plan made as the sheet opened: the frames in the stack's order, their exposures, the
    /// reference and the folder proposal.
    pub(crate) plan: Option<MergePlan>,
    /// The catalog's folders, for choosing another, and whether their menu is open.
    pub(crate) folders: Vec<(CatalogFolderId, String)>,
    pub(crate) folder_menu: bool,
    /// The owner's preview, once it is known: every update, merge and close names it.
    pub(crate) preview_id: Option<MergePreviewId>,
    /// The `merge-preview` job preparing or redrawing the picture.
    pub(crate) job: Option<String>,
    /// That job's progress: the fraction done and its words ("Reading 2 of 3").
    pub(crate) fraction: Option<f64>,
    pub(crate) message: Option<String>,
    /// The newest revision the preview answered.
    pub(crate) preview: Option<MergePreview>,
    /// Why these frames cannot merge with these settings: the plan's or the preview's refusal,
    /// or a request the owner refused.
    pub(crate) refusal: Option<String>,
    /// Show deghost overlay: drawn by this client from the revision's data, sending nothing.
    pub(crate) overlay: bool,
    /// The settings changed while no preview could take them: sent once one can.
    pub(crate) stale: bool,
    /// The newest owner task asked for the sheet; an answer to any other is dropped.
    pub(crate) serial: u64,
    /// An owner task for the sheet is in flight.
    pub(crate) asking: bool,
    /// A merge from the sheet is starting.
    pub(crate) merging: Option<MergeMode>,
    /// Every request the sheet sent, in order, for evidence and tests.
    pub(crate) sent: Vec<Value>,
}

impl PreviewSheet {
    pub(crate) fn new(
        request: MergeRequest,
        context: SheetContext,
        folders: Vec<(CatalogFolderId, String)>,
    ) -> Self {
        Self {
            request,
            context,
            plan: None,
            folders,
            folder_menu: false,
            preview_id: None,
            job: None,
            fraction: None,
            message: None,
            preview: None,
            refusal: None,
            overlay: false,
            stale: false,
            serial: 0,
            asking: false,
            merging: None,
            sent: Vec::new(),
        }
    }

    /// The plan the sheet opened with answered: its frames and folder, or the refusal that keeps
    /// the preview from being made.
    pub(crate) fn planned(&mut self, plan: MergePlan) {
        self.refusal = plan_refusal(&plan);
        self.plan = Some(plan);
    }

    /// A preview job was started for the sheet: follow it. Its answer may already name the
    /// preview.
    pub(crate) fn following(&mut self, job: String, preview_id: Option<MergePreviewId>) {
        self.job = Some(job);
        self.fraction = None;
        self.message = None;
        if preview_id.is_some() {
            self.preview_id = preview_id;
        }
    }

    /// Take in a running preview job's record: its progress, and the preview it prepares when the
    /// record names it.
    pub(crate) fn observe(&mut self, record: &Value) {
        let progress = &record["progress"];
        self.fraction = progress["fraction"].as_f64();
        self.message = progress["message"].as_str().map(str::to_owned);
        if let Some(preview_id) = record["result"]
            .get("preview_id")
            .and_then(|id| serde_json::from_value(id.clone()).ok())
        {
            self.preview_id = Some(preview_id);
        }
    }

    /// A revision answered: the picture, its data and output, or its refusal.
    pub(crate) fn adopt(&mut self, preview: MergePreview) {
        self.job = None;
        self.fraction = None;
        self.message = None;
        self.preview_id = Some(preview.preview_id.clone());
        self.refusal = preview
            .refusal
            .as_ref()
            .map(|refusal| refusal_message(Some(refusal), "These frames cannot be merged"));
        self.preview = Some(preview);
    }

    /// The preview job, or a request for it, ended without a revision: why, in the owner's words.
    pub(crate) fn failed(&mut self, reason: String) {
        self.job = None;
        self.fraction = None;
        self.message = None;
        self.refusal = Some(reason);
    }

    /// The resolved offset of input `at` from the reference: the newest revision's, else the
    /// plan's.
    fn resolved(&self, at: usize) -> Option<f64> {
        if let Some(MergePreviewDiagnostics::Hdr(diagnostics)) = self
            .preview
            .as_ref()
            .and_then(|preview| preview.diagnostics.as_ref())
            && let Some(offset) = diagnostics.exposures.get(at)
        {
            return Some(*offset);
        }
        self.plan
            .as_ref()?
            .exposures
            .as_ref()?
            .inputs
            .get(at)
            .map(|exposure| exposure.offset_ev)
    }

    /// The reference's position in the stack: the person's choice, else the newest revision's,
    /// else the plan's.
    fn reference(&self) -> Option<usize> {
        let plan = self.plan.as_ref()?;
        if let Some(path) = &self.request.options.reference {
            return plan.inputs.iter().position(|input| input.path == *path);
        }
        if let Some(MergePreviewDiagnostics::Hdr(diagnostics)) = self
            .preview
            .as_ref()
            .and_then(|preview| preview.diagnostics.as_ref())
        {
            return Some(diagnostics.reference as usize);
        }
        plan.reference.map(|reference| reference as usize)
    }

    /// The frames the newest revision drew: its options are the sheet's settings when it is
    /// current.
    pub(crate) fn current(&self) -> bool {
        self.job.is_none() && !self.stale && !self.asking
    }
}

/// How the picture area is drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Picture {
    /// The preview reads and aligns: the job's words ("Reading 2 of 3") and a bar while its
    /// fraction is known.
    Preparing {
        message: String,
        fraction: Option<f32>,
    },
    /// The newest revision's picture, with the deghost overlay's regions while it is shown.
    Ready {
        path: PathBuf,
        width: u32,
        height: u32,
        regions: Vec<Region>,
        /// A newer revision is being drawn: its job's words.
        updating: Option<String>,
    },
    /// The plan's or the preview's refusal, in place of the picture.
    Refused(String),
}

/// An overlay region's tint: the exposure it is shown from, below the reference, the reference,
/// or above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tint {
    Below,
    Reference,
    Above,
}

/// One moving region of the deghost overlay, in the picture's pixels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Region {
    pub(crate) outline: Vec<[f32; 2]>,
    /// The region's bounds, `[x, y, width, height]`: the label sits at its top left.
    pub(crate) bounds: [f32; 4],
    pub(crate) tint: Tint,
    /// "From 0 EV", or "From −2 EV · fallback".
    pub(crate) label: String,
    /// Shown from another exposure because the reference did not record it usably: hatched.
    pub(crate) fallback: bool,
}

/// One frame of the sheet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FrameRow {
    pub(crate) path: PathBuf,
    /// The file, for its thumbnail from the grid's previews.
    pub(crate) file_id: Option<FileId>,
    pub(crate) name: String,
    /// "−2 EV · 1/1000 s": its resolved exposure and shutter.
    pub(crate) detail: String,
    /// The stepper's words: "Auto", or the override in thirds of a stop ("−1⅓").
    pub(crate) value: String,
    /// The offset the steppers move from: the override, or the resolved one.
    pub(crate) offset: Option<f64>,
    pub(crate) overridden: bool,
    pub(crate) reference: bool,
}

/// The sheet as drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SheetModel {
    /// "Merge HDR · 3 exposures".
    pub(crate) title: String,
    /// "DSC_4088–4090.NEF · Nikon Z 8 · 13 Sep 2026 17:31".
    pub(crate) subtitle: String,
    pub(crate) picture: Picture,
    /// Under the picture: the preview's scope and alignment, the preparing note, or the refusal.
    pub(crate) status: String,
    pub(crate) status_refused: bool,
    /// "Display preview, not the saved pixels", beside a ready picture's status.
    pub(crate) note: Option<&'static str>,
    pub(crate) frames: Vec<FrameRow>,
    /// The frames are not planned yet.
    pub(crate) reading: bool,
    pub(crate) alignment: Toggle,
    pub(crate) deghost: Toggle,
    pub(crate) overlay: bool,
    pub(crate) folder: String,
    /// "catalog folder", "new folder".
    pub(crate) folder_note: Option<&'static str>,
    /// The folders to choose from while the folder menu is open.
    pub(crate) folder_choices: Option<Vec<(String, FolderPick)>>,
    /// "6048 × 4024 · Highlights extended · 279 MB".
    pub(crate) output: Option<String>,
    /// The action that opened the sheet, drawn primary.
    pub(crate) primary: MergeMode,
    /// Pick merged and Import merged work: no refusal, no merge running.
    pub(crate) enabled: bool,
    /// Why they do not, while another merge runs or one is starting from the sheet.
    pub(crate) reason: Option<String>,
}

impl SheetModel {
    /// The action of a merge button, while it works.
    pub(crate) fn merge(&self, mode: MergeMode) -> Option<MergeAction> {
        self.enabled.then_some(MergeAction::SheetMerge(mode))
    }
}

/// The sheet's model; `busy` says why no merge may start while another runs.
pub(crate) fn model(sheet: &PreviewSheet, busy: Option<&str>) -> SheetModel {
    let kind = sheet.request.kind;
    let words = words(kind);
    let plan = sheet.plan.as_ref();
    let options = &sheet.request.options;
    let count = plan.map_or(sheet.request.frames as usize, |plan| plan.inputs.len());
    let title = format!("Merge {} \u{b7} {count} {}", words.noun, words.frames);
    let names: Vec<String> = match plan {
        Some(plan) => plan
            .inputs
            .iter()
            .map(|input| input.file_name.clone())
            .collect(),
        None => sheet.context.names.clone(),
    };
    let subtitle = [
        files_words(&names),
        sheet.context.camera.clone(),
        sheet.context.capture.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" \u{b7} ");

    let reference = sheet.reference();
    let frames = plan.map_or_else(Vec::new, |plan| {
        plan.inputs
            .iter()
            .enumerate()
            .map(|(at, input)| {
                let overridden = options.override_of(&input.path);
                let offset = overridden.or_else(|| sheet.resolved(at));
                let ev = offset.map_or_else(
                    || "EV from the pixels".to_owned(),
                    |offset| {
                        ev_words(offset).map_or_else(|| format!("{offset:+.2} EV"), str::to_owned)
                    },
                );
                let detail = match input.frame.exposure.shutter_s.filter(|time| *time > 0.0) {
                    Some(time) => format!("{ev} \u{b7} {}", shutter_words(time)),
                    None => ev,
                };
                FrameRow {
                    path: input.path.clone(),
                    file_id: input.file_id,
                    name: input.file_name.clone(),
                    detail,
                    value: overridden.map_or_else(|| "Auto".to_owned(), thirds_words),
                    offset,
                    overridden: overridden.is_some(),
                    reference: reference == Some(at),
                }
            })
            .collect()
    });

    let (picture, status, status_refused, note) = picture(sheet);

    let proposed = plan.map(|plan| match (&plan.folder, &plan.suggested_folder) {
        (Some(folder), _) => (folder.name.clone(), Some("catalog folder")),
        (None, Some(FolderChoice::New { name, .. })) => (name.clone(), Some("new folder")),
        _ => (
            plan.folder_reason
                .clone()
                .unwrap_or_else(|| "Choose a folder".to_owned()),
            None,
        ),
    });
    let (folder, folder_note) = match &options.folder {
        FolderPick::Existing { name, .. } => (name.clone(), Some("catalog folder")),
        FolderPick::Proposed => proposed
            .clone()
            .unwrap_or_else(|| ("\u{2026}".to_owned(), None)),
    };
    let folder_choices = sheet.folder_menu.then(|| {
        let mut choices = Vec::with_capacity(sheet.folders.len() + 1);
        if let Some((proposed, _)) = &proposed {
            choices.push((proposed.clone(), FolderPick::Proposed));
        }
        for (folder_id, name) in &sheet.folders {
            choices.push((
                name.clone(),
                FolderPick::Existing {
                    folder_id: folder_id.clone(),
                    name: name.clone(),
                },
            ));
        }
        choices
    });

    let reason = busy.map(str::to_owned).or_else(|| {
        sheet
            .merging
            .map(|_| "Starting the merge\u{2026}".to_owned())
    });
    SheetModel {
        title,
        subtitle,
        picture,
        status,
        status_refused,
        note,
        frames,
        reading: plan.is_none() && sheet.refusal.is_none(),
        alignment: options.alignment,
        deghost: options.deghost,
        overlay: sheet.overlay,
        folder,
        folder_note,
        folder_choices,
        output: output_words(sheet),
        primary: sheet.request.mode,
        enabled: sheet.refusal.is_none() && reason.is_none(),
        reason,
    }
}

/// The picture area and the status line under it.
fn picture(sheet: &PreviewSheet) -> (Picture, String, bool, Option<&'static str>) {
    if let Some(refusal) = &sheet.refusal {
        return (
            Picture::Refused(refusal.clone()),
            refusal.clone(),
            true,
            None,
        );
    }
    let job_words = || {
        sheet
            .message
            .clone()
            .unwrap_or_else(|| "Preparing the preview\u{2026}".to_owned())
    };
    match &sheet.preview {
        Some(MergePreview {
            path: Some(path),
            width,
            height,
            diagnostics,
            overlays,
            ..
        }) => {
            let regions = if sheet.overlay {
                regions(overlays.as_ref())
            } else {
                Vec::new()
            };
            let status = match diagnostics {
                Some(MergePreviewDiagnostics::Hdr(diagnostics)) => {
                    let alignment = match (diagnostics.alignment, diagnostics.max_residual_px) {
                        (luxforge_core::merge::hdr::AlignmentMode::Off, _) => {
                            "not aligned".to_owned()
                        }
                        (_, Some(residual)) => format!("aligned, {residual:.2} px"),
                        (_, None) => "aligned".to_owned(),
                    };
                    format!(
                        "Preview from {}-pixel frames \u{b7} {alignment}",
                        crate::state::select::thousands(diagnostics.proxy_side)
                    )
                }
                None => "Preview".to_owned(),
            };
            (
                Picture::Ready {
                    path: path.clone(),
                    width: *width,
                    height: *height,
                    regions,
                    updating: sheet.job.is_some().then(job_words),
                },
                status,
                false,
                Some(DISPLAY_NOTE),
            )
        }
        _ => (
            Picture::Preparing {
                message: job_words(),
                fraction: sheet.fraction.map(|fraction| fraction as f32),
            },
            PREPARING_NOTE.to_owned(),
            false,
            None,
        ),
    }
}

/// The deghost overlay's regions from a revision's overlay data.
fn regions(overlays: Option<&MergePreviewOverlays>) -> Vec<Region> {
    let Some(MergePreviewOverlays::Hdr(overlays)) = overlays else {
        return Vec::new();
    };
    overlays
        .motion
        .iter()
        .map(|motion| {
            let tint = if motion.offset_ev.abs() < 1e-6 {
                Tint::Reference
            } else if motion.offset_ev < 0.0 {
                Tint::Below
            } else {
                Tint::Above
            };
            let ev = ev_words(motion.offset_ev)
                .map_or_else(|| format!("{:+.2} EV", motion.offset_ev), str::to_owned);
            Region {
                outline: motion
                    .outline
                    .iter()
                    .map(|[x, y]| [*x as f32, *y as f32])
                    .collect(),
                bounds: [
                    motion.bounds.x as f32,
                    motion.bounds.y as f32,
                    motion.bounds.width as f32,
                    motion.bounds.height as f32,
                ],
                tint,
                label: if motion.fallback {
                    format!("From {ev} \u{b7} fallback")
                } else {
                    format!("From {ev}")
                },
                fallback: motion.fallback,
            }
        })
        .collect()
}

/// "6048 × 4024 · Highlights extended · 279 MB": the newest revision's output, else the plan's
/// estimate.
fn output_words(sheet: &PreviewSheet) -> Option<String> {
    let preview = sheet.preview.as_ref();
    let (width, height, bytes) = match preview.and_then(|preview| preview.output.as_ref()) {
        Some(output) => (output.width, output.height, Some(output.source_bytes)),
        None => {
            let estimates = &sheet.plan.as_ref()?.estimates;
            (estimates.width?, estimates.height?, estimates.source_bytes)
        }
    };
    let mut parts = vec![format!("{width} \u{d7} {height}")];
    if let Some(MergePreviewDiagnostics::Hdr(diagnostics)) =
        preview.and_then(|preview| preview.diagnostics.as_ref())
    {
        match (
            diagnostics.highlights_extended,
            diagnostics.shadows_extended,
        ) {
            (true, true) => parts.push("Highlights and shadows extended".to_owned()),
            (true, false) => parts.push("Highlights extended".to_owned()),
            (false, true) => parts.push("Shadows extended".to_owned()),
            (false, false) => {}
        }
    }
    if let Some(bytes) = bytes {
        parts.push(size_words(bytes));
    }
    Some(parts.join(" \u{b7} "))
}

/// A source's bytes as the output line says them: "279 MB", "1.2 GB".
pub(crate) fn size_words(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let megabytes = bytes as f64 / MB;
    if megabytes >= 1024.0 {
        format!("{:.1} GB", megabytes / 1024.0)
    } else {
        format!("{} MB", megabytes.round().max(1.0) as u64)
    }
}

/// A shutter time: "1/1000 s", "2 s", "0.8 s".
pub(crate) fn shutter_words(seconds: f64) -> String {
    if seconds < 1.0 {
        format!("1/{} s", (1.0 / seconds).round() as u64)
    } else if seconds.fract() == 0.0 {
        format!("{} s", seconds as u64)
    } else {
        format!("{seconds:.1} s")
    }
}

/// An override in thirds of a stop: "0", "+⅔", "−1⅓"; an offset off the grid to a tenth.
pub(crate) fn thirds_words(offset: f64) -> String {
    let thirds = (offset * 3.0).round();
    if (offset * 3.0 - thirds).abs() > 1e-6 {
        return crate::state::select::ev_step(offset as f32);
    }
    let thirds = thirds as i64;
    if thirds == 0 {
        return "0".to_owned();
    }
    let sign = if thirds < 0 { '\u{2212}' } else { '+' };
    let (whole, part) = (thirds.abs() / 3, thirds.abs() % 3);
    let fraction = match part {
        1 => "\u{2153}",
        2 => "\u{2154}",
        _ => "",
    };
    if whole == 0 {
        format!("{sign}{fraction}")
    } else {
        format!("{sign}{whole}{fraction}")
    }
}

/// The step of the override of a frame at `offset` by `by` thirds, on the thirds grid.
pub(crate) fn stepped(offset: f64, by: i32) -> f64 {
    ((offset / EV_STEP).round() + f64::from(by)) * EV_STEP
}

/// The frames' files in a few words: "DSC_4088–4090.NEF" for a run of one name and extension,
/// "a.NEF – b.CR3" otherwise.
pub(crate) fn files_words(names: &[String]) -> Option<String> {
    let (first, last) = (names.first()?, names.last()?);
    if names.len() == 1 || first == last {
        return Some(first.clone());
    }
    let split = |name: &str| match name.rsplit_once('.') {
        Some((stem, extension)) => (stem.to_owned(), Some(extension.to_owned())),
        None => (name.to_owned(), None),
    };
    let ((first_stem, extension), (last_stem, last_extension)) = (split(first), split(last));
    if extension == last_extension && first_stem.len() == last_stem.len() {
        let mut common = first_stem
            .bytes()
            .zip(last_stem.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        // The run's numbers are said whole: back to where the last name's number starts.
        while common > 0 && last_stem.as_bytes()[common - 1].is_ascii_digit() {
            common -= 1;
        }
        if last_stem[common..]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            && last_stem.is_char_boundary(common)
        {
            let tail = &last_stem[common..];
            return Some(match extension {
                Some(extension) => format!("{first_stem}\u{2013}{tail}.{extension}"),
                None => format!("{first_stem}\u{2013}{tail}"),
            });
        }
    }
    Some(format!("{first} \u{2013} {last}"))
}

/// Where a picture of `size` sits when fitted whole and centred in `area`: its top left and
/// scale.
pub(crate) fn fit(size: (f32, f32), area: (f32, f32)) -> ([f32; 2], f32) {
    if size.0 <= 0.0 || size.1 <= 0.0 {
        return ([0.0, 0.0], 0.0);
    }
    let scale = (area.0 / size.0).min(area.1 / size.1).max(0.0);
    (
        [
            (area.0 - size.0 * scale) / 2.0,
            (area.1 - size.1 * scale) / 2.0,
        ],
        scale,
    )
}

/// A fallback region's hatching: the segments of 45° lines `spacing` apart, from top right to
/// bottom left, that lie inside the closed polygon `outline` (even-odd).
pub(crate) fn hatch(outline: &[[f32; 2]], spacing: f32) -> Vec<([f32; 2], [f32; 2])> {
    if outline.len() < 3 || spacing <= 0.0 {
        return Vec::new();
    }
    // Each line is x + y = c.
    let sums = outline.iter().map(|[x, y]| x + y);
    let (low, high) = sums.fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), sum| {
        (low.min(sum), high.max(sum))
    });
    let mut segments = Vec::new();
    let mut c = (low / spacing).floor() * spacing + spacing / 2.0;
    while c < high {
        let mut crossings = Vec::new();
        for (at, a) in outline.iter().enumerate() {
            let b = outline[(at + 1) % outline.len()];
            let (sa, sb) = (a[0] + a[1] - c, b[0] + b[1] - c);
            if (sa < 0.0) != (sb < 0.0) {
                let t = sa / (sa - sb);
                crossings.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            }
        }
        crossings.sort_by(|a, b| a[0].total_cmp(&b[0]));
        for pair in crossings.chunks_exact(2) {
            segments.push((pair[0], pair[1]));
        }
        c += spacing;
    }
    segments
}
