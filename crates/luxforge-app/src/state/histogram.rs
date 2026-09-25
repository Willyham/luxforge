//! The histogram inspector's model: the plotted bins, the endpoint counters, the two clipping
//! triangles and their tooltips, the pointer readout's wording and the arithmetic that sizes the
//! clipping overlay.
//!
//! Everything here is a pure function of a [`Report`] the preview worker already produced. Nothing
//! reduces a raster, asks the owner for anything or draws: the plot's shared vertical scale, the
//! counter wording, the stale rule and the overlay's cell grid are all decided here so they can be
//! proved without a window, and the widgets are handed numbers they cannot reinterpret.
//!
//! The inspector is the plot and the row of two triangles under it, and nothing else: a fixed
//! height that never depends on the pointer, the analysis status or the counts, so nothing in the
//! tools panel moves while a slider is dragged, the pointer crosses the photograph or a frame is
//! re-analysed. Everything that varies lives where it cannot move a control. The domain is the
//! plot's tooltip; a status with no report behind it is drawn inside the plot's own area
//! ([`HistogramModel::notice`]); the endpoint counts are the triangles' tooltips
//! ([`HistogramModel::shadow_tooltip`], [`HistogramModel::highlight_tooltip`]); and the pointer
//! readout is in the status bar ([`readout_text`]).
//!
//! The described domain is fixed by the [histogram and clipping
//! contract](../../../../docs/design/basic-and-histogram.md#histogram-and-clipping-contract): the
//! rendered SDR sRGB **output** of the whole composition, after crop and edits, before any UI
//! overlay or display scaling. It is never the camera or RAW histogram, which is why the plot says
//! so on hover rather than leaving it to be inferred.
use crate::{
    state::{Inputs, canvas::ZoomView},
    view::{STATE_PANEL_WIDTH, STATUS_BAR_HEIGHT, TITLE_BAR_HEIGHT, TOOLS_PANEL_WIDTH},
};
use luxforge_core::{
    ErrorKind,
    analysis::{AnalysisIdentity, MAX_OVERLAY_CELLS, Report},
};

/// The plot's tooltip. It names the domain in the user's own words so an endpoint count is never
/// read as evidence about the original capture.
pub(crate) const DOMAIN_CAPTION: &str = "Output \u{b7} sRGB \u{b7} after crop";

/// The rule both triangles state on hover, exactly as the contract words it.
pub(crate) const SHADOW_RULE: &str =
    "Any channel at 0 \u{b7} blue; any at 255 \u{b7} red; both endpoints \u{b7} magenta";
pub(crate) const HIGHLIGHT_RULE: &str = SHADOW_RULE;

/// One analysed frame as the desktop holds it: the report its own preview worker reduced, the
/// identity the owner will look that report up under, and the preview generation it arrived with.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Analysis {
    pub(crate) generation: u64,
    pub(crate) identity: AnalysisIdentity,
    pub(crate) report: Report,
}

/// One sampled pixel of the displayed stack, as `render.sample` answered it. An answer that arrives
/// after the canvas moved to another entry is dropped rather than shown against another image. The
/// status bar shows it, in [`readout_text`]'s words.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Readout {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) rgba: [u8; 4],
}

/// What the inspector can say about the displayed frame right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum HistogramStatus {
    /// Nothing has been reduced yet: no photograph, or the first frame is still rendering.
    #[default]
    Pending,
    /// A report is shown and it belongs to the displayed generation.
    Ready,
    /// A report is shown but a newer generation is in flight, so the counts are one frame behind.
    Updating,
    /// The displayed frame could not be rendered, so there is nothing to reduce.
    Unavailable,
}

impl HistogramStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Updating => "updating",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Which render the plotted counts describe, so the plot, the photograph and the overlays are
/// provably the same frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RenderIdentity {
    pub(crate) entry: String,
    /// The revision of the client draft the analysed recipe came from, when one was open.
    pub(crate) draft_revision: Option<u64>,
    /// The preview generation the report arrived under.
    pub(crate) generation: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// One clipping triangle's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Triangle {
    /// This endpoint has pixels in the displayed frame, so the triangle is coloured.
    pub(crate) tinted: bool,
    /// Its overlay is drawn on the photograph.
    pub(crate) active: bool,
    pub(crate) enabled: bool,
}

/// What a count line shows while there is no report to count: a dash, never zeros, which would
/// claim nothing is clipped in a frame nobody has reduced.
const NO_COUNT: &str = "\u{2014}";

/// The ten endpoint counters, both as numbers (for the correlated evidence state) and as the
/// compact lines the triangles' tooltips state in words for a reader who cannot measure the plot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counters {
    pub(crate) r0: u64,
    pub(crate) g0: u64,
    pub(crate) b0: u64,
    pub(crate) r255: u64,
    pub(crate) g255: u64,
    pub(crate) b255: u64,
    pub(crate) any_shadow: u64,
    pub(crate) any_highlight: u64,
    pub(crate) all_shadow: u64,
    pub(crate) all_highlight: u64,
    pub(crate) both: u64,
}

impl Counters {
    fn of(report: &Report) -> Self {
        Self {
            r0: report.r0,
            g0: report.g0,
            b0: report.b0,
            r255: report.r255,
            g255: report.g255,
            b255: report.b255,
            any_shadow: report.any_shadow,
            any_highlight: report.any_highlight,
            all_shadow: report.all_shadow,
            all_highlight: report.all_highlight,
            both: report.both,
        }
    }

    /// The shadow line: the three per-channel code-0 counts, then any and all.
    pub(crate) fn shadow_text(&self) -> String {
        format!(
            "0 \u{b7} R {} G {} B {} \u{b7} any {} \u{b7} all {}",
            self.r0, self.g0, self.b0, self.any_shadow, self.all_shadow
        )
    }

    /// The highlight line, the same shape at code 255.
    pub(crate) fn highlight_text(&self) -> String {
        format!(
            "255 \u{b7} R {} G {} B {} \u{b7} any {} \u{b7} all {}",
            self.r255, self.g255, self.b255, self.any_highlight, self.all_highlight
        )
    }

    /// Pixels at both endpoints at once, which the overlay draws magenta.
    pub(crate) fn both_text(&self) -> String {
        format!("both {}", self.both)
    }
}

/// The whole inspector as plain data.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistogramModel {
    pub(crate) status: HistogramStatus,
    /// Why there is nothing to show, when the status is `Unavailable`.
    pub(crate) reason: Option<String>,
    pub(crate) identity: Option<RenderIdentity>,
    /// The domain the counts describe, stated on hover over the plot.
    pub(crate) caption: &'static str,
    /// The three channels' counts, normalized against one shared linear scale: the largest count in
    /// **any** channel. `None` while there is no report. Raw counts are never changed by this; the
    /// API keeps reporting them exactly as reduced.
    pub(crate) bins: Option<Box<[[f32; 256]; 3]>>,
    /// The count one full-height bin represents, recorded so a frame's plot is checkable.
    pub(crate) plotted_max: u64,
    pub(crate) counters: Counters,
    pub(crate) shadow: Triangle,
    pub(crate) highlight: Triangle,
    /// The shown counts are one generation behind the frame being rendered. The plot is dimmed,
    /// which is its stale label; no text row comes and goes with it.
    pub(crate) stale: bool,
}

/// The default is the empty inspector, not an empty plot: pending, no bins, and the domain already
/// in place, because the domain never depends on what has been analysed.
impl Default for HistogramModel {
    fn default() -> Self {
        Self {
            status: HistogramStatus::default(),
            reason: None,
            identity: None,
            caption: DOMAIN_CAPTION,
            bins: None,
            plotted_max: 0,
            counters: Counters::default(),
            shadow: Triangle::default(),
            highlight: Triangle::default(),
            stale: false,
        }
    }
}

impl HistogramModel {
    /// What the plot says in place of bins, drawn inside the plot's own fixed area: an explicit
    /// state, never a silently empty plot, and never a row of its own that would move the panel.
    ///
    /// Only a status with no report has one. `Updating` keeps the previous report on screen dimmed,
    /// which is its label, so a drag — which is `Updating` for most of its length — flashes no text
    /// on and off over the plot.
    pub(crate) fn notice(&self) -> Option<String> {
        match self.status {
            HistogramStatus::Ready | HistogramStatus::Updating => None,
            HistogramStatus::Pending => Some("No analysis yet".into()),
            HistogramStatus::Unavailable => Some(match &self.reason {
                Some(reason) => format!("Unavailable: {reason}"),
                None => "Unavailable".into(),
            }),
        }
    }

    /// Whether the counters describe a real reduction. `Updating` still counts: the previous
    /// generation's report is on screen and is honestly labelled stale. `Pending` and `Unavailable`
    /// have no report at all, and their zeroed [`Counters`] would otherwise read as "nothing is
    /// clipped", which is a claim about a frame nobody has reduced.
    fn counted(&self) -> bool {
        matches!(
            self.status,
            HistogramStatus::Ready | HistogramStatus::Updating
        )
    }

    /// The shadow count line. Always the same shape: with no report the three channels, `any` and
    /// `all` are replaced by one em dash rather than by zeros.
    pub(crate) fn shadow_text(&self) -> String {
        if self.counted() {
            self.counters.shadow_text()
        } else {
            format!("0 \u{b7} {NO_COUNT}")
        }
    }

    /// The highlight count line, the same shape at code 255.
    pub(crate) fn highlight_text(&self) -> String {
        if self.counted() {
            self.counters.highlight_text()
        } else {
            format!("255 \u{b7} {NO_COUNT}")
        }
    }

    /// The both-endpoints count line.
    pub(crate) fn both_text(&self) -> String {
        if self.counted() {
            self.counters.both_text()
        } else {
            format!("both {NO_COUNT}")
        }
    }

    /// The shadow triangle's tooltip: the overlay rule, then the code-0 counts in words.
    pub(crate) fn shadow_tooltip(&self) -> String {
        format!("{SHADOW_RULE}\n{}", self.shadow_text())
    }

    /// The highlight triangle's tooltip: the overlay rule, then the code-255 counts and the pixels
    /// at both endpoints at once, which is the magenta the rule names.
    pub(crate) fn highlight_tooltip(&self) -> String {
        format!(
            "{HIGHLIGHT_RULE}\n{}\n{}",
            self.highlight_text(),
            self.both_text()
        )
    }
}

/// One sampled pixel as the status bar words it: the three output codes and where they came from.
pub(crate) fn readout_text(readout: &Readout) -> String {
    format!(
        "R {} \u{b7} G {} \u{b7} B {} \u{b7} {}, {}",
        readout.rgba[0], readout.rgba[1], readout.rgba[2], readout.x, readout.y
    )
}

/// Normalize three channels against one shared linear scale, and report the scale. The maximum is
/// taken over all three channels together, so the relative height of red against green is the
/// relative count of red against green — a per-channel scale would make three different pictures
/// and destroy exactly the comparison the plot exists for.
pub(crate) fn normalize(report: &Report) -> ([[f32; 256]; 3], u64) {
    let max = report
        .r
        .iter()
        .chain(report.g.iter())
        .chain(report.b.iter())
        .copied()
        .max()
        .unwrap_or(0);
    let mut plotted = [[0.0f32; 256]; 3];
    if max == 0 {
        return (plotted, 0);
    }
    let scale = max as f64;
    for (target, source) in plotted
        .iter_mut()
        .zip([&report.r, &report.g, &report.b].into_iter())
    {
        for (bin, count) in target.iter_mut().zip(source.iter()) {
            *bin = (*count as f64 / scale) as f32;
        }
    }
    (plotted, max)
}

/// The inspector for this derivation. `previous` is the model the last one produced: the plotted
/// bins are reused whenever it describes the same render, because a report never changes within one
/// generation and the normalization is the only part of this that is proportional to anything. Every
/// other field is recomputed, which costs nothing on the derivation a pointer move causes.
pub(crate) fn derive(inputs: &Inputs<'_>, previous: &HistogramModel) -> HistogramModel {
    let flags = (
        inputs.session.workspace.clip_shadows,
        inputs.session.workspace.clip_highlights,
    );
    let mut model = HistogramModel {
        shadow: Triangle {
            tinted: false,
            active: flags.0,
            enabled: inputs.state.is_some(),
        },
        highlight: Triangle {
            tinted: false,
            active: flags.1,
            enabled: inputs.state.is_some(),
        },
        ..HistogramModel::default()
    };
    let Some(analysis) = inputs.analysis else {
        // No report: either the displayed frame failed to render, or none has arrived yet. A
        // failure says so with its reason; anything else is honestly pending, never an empty plot.
        if inputs.state.is_some()
            && let Some((kind, detail)) = inputs.render_error
        {
            model.status = HistogramStatus::Unavailable;
            model.reason = Some(reason(*kind, detail));
        }
        return model;
    };
    let identity = RenderIdentity {
        entry: analysis.identity.entry_id.as_str().to_owned(),
        draft_revision: analysis
            .identity
            .draft
            .as_ref()
            .map(|draft| draft.draft_revision),
        generation: analysis.generation,
        width: analysis.identity.width,
        height: analysis.identity.height,
    };
    let reused = (previous.identity.as_ref() == Some(&identity))
        .then(|| previous.bins.clone())
        .flatten()
        .map(|bins| (bins, previous.plotted_max));
    let (bins, plotted_max) = match reused {
        Some((bins, max)) => (bins, max),
        None => {
            let (bins, max) = normalize(&analysis.report);
            (Box::new(bins), max)
        }
    };
    let counters = Counters::of(&analysis.report);
    // A newer generation is in flight: the previous report stays on screen and is marked stale,
    // rather than blanking the plot for the length of a render.
    let stale = inputs.analysis_updating;
    model.status = if stale {
        HistogramStatus::Updating
    } else {
        HistogramStatus::Ready
    };
    model.stale = stale;
    model.bins = Some(bins);
    model.plotted_max = plotted_max;
    model.shadow.tinted = counters.any_shadow > 0;
    model.highlight.tinted = counters.any_highlight > 0;
    model.counters = counters;
    model.identity = Some(identity);
    model
}

/// A render failure in the words the plot's notice uses. The kind carries the meaning; the detail is
/// already a sentence, so it is shown as it stands.
fn reason(kind: ErrorKind, detail: &str) -> String {
    if detail.trim().is_empty() {
        kind.code().to_owned()
    } else {
        detail.to_owned()
    }
}

// -- The clipping overlay's geometry -------------------------------------------------------------

/// The photo surface's logical size for one window and panel configuration: the window minus the
/// title bar, the status bar, whichever panels are open and the 1 px dividers beside them. It is
/// arithmetic over the layout constants, not a measurement, which is why it belongs here and not in
/// the view: the overlay's cell grid has to be decided before a frame is laid out.
pub(crate) fn photo_surface(
    window: (f32, f32),
    state_panel: bool,
    tools_panel: bool,
) -> (f32, f32) {
    let divider = luxforge_ui::theme::BORDER_WIDTH;
    let mut width = window.0;
    if state_panel {
        width -= STATE_PANEL_WIDTH + divider;
    }
    if tools_panel {
        width -= TOOLS_PANEL_WIDTH + divider;
    }
    // Two horizontal rules, one under the title bar and one over the status bar.
    let height = window.1 - TITLE_BAR_HEIGHT - STATUS_BAR_HEIGHT - 2.0 * divider;
    (width.max(0.0), height.max(0.0))
}

/// The photograph's own size on screen in **physical** pixels, which is what decides how much
/// detail an overlay cell can show.
///
/// At Fit the toolkit contains the image inside the padded surface, so the fitted size is the
/// surface's own scaled by the display factor. At a percentage the zoom is defined against physical
/// pixels — 100% means one physical pixel per source pixel — so the display factor does not enter.
pub(crate) fn displayed_size(
    zoom: ZoomView,
    source: (u32, u32),
    surface: (f32, f32),
    scale_factor: f32,
    padding: f32,
) -> Option<(f32, f32)> {
    let (width, height) = (source.0 as f32, source.1 as f32);
    if !(width > 0.0 && height > 0.0) {
        return None;
    }
    match zoom {
        ZoomView::Fit => {
            let available = (
                (surface.0 - 2.0 * padding).max(0.0),
                (surface.1 - 2.0 * padding).max(0.0),
            );
            if !(available.0 > 0.0 && available.1 > 0.0 && scale_factor > 0.0) {
                return None;
            }
            let scale = (available.0 / width).min(available.1 / height) * scale_factor;
            (scale.is_finite() && scale > 0.0).then_some((width * scale, height * scale))
        }
        ZoomView::Percent(value) => {
            let scale = value / 100.0;
            (scale.is_finite() && scale > 0.0).then_some((width * scale, height * scale))
        }
    }
}

/// The display overlay's cell grid for one displayed raster.
///
/// One uniform scale is chosen for both axes, so the grid always has the source's aspect ratio and
/// the overlay image the canvas stacks over the photograph lands in exactly the same rectangle the
/// photograph does. The scale is the smallest of:
///
/// - `1.0`, because a cell finer than a source pixel would invent detail. When the photograph is
///   drawn larger than it is (Fit on a small image, or any zoom above 100%) the grid is the source
///   itself, which is the most exact overlay there is: one cell per source pixel.
/// - the displayed size, so a photograph drawn smaller than it is gets one cell per physical pixel
///   of the photograph and the buffer stays proportional to what is on screen, not to the image.
/// - [`MAX_OVERLAY_CELLS`] a side, the hard cap the core's reduction enforces.
///
/// A cell ORs the clipping class of every source pixel that falls in it, so reducing the grid never
/// loses an isolated clipped pixel: it only ever makes the cell that holds it bigger.
pub(crate) fn overlay_cells(source: (u32, u32), displayed: (f32, f32)) -> Option<(u32, u32)> {
    let (width, height) = (source.0, source.1);
    if width == 0 || height == 0 || !displayed.0.is_finite() || !displayed.1.is_finite() {
        return None;
    }
    let cap = f32::from(u16::try_from(MAX_OVERLAY_CELLS).ok()?);
    let scale = 1.0f32
        .min(displayed.0 / width as f32)
        .min(displayed.1 / height as f32)
        .min(cap / width as f32)
        .min(cap / height as f32);
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let cells = |extent: u32| {
        ((extent as f32 * scale).round() as u32).clamp(1, MAX_OVERLAY_CELLS.min(extent.max(1)))
    };
    Some((cells(width), cells(height)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::analysis;

    /// A report over a tiny hand-built raster, so every count below is countable by hand.
    fn report(pixels: &[[u8; 4]], width: u32, height: u32) -> Report {
        let rgba: Vec<u8> = pixels.iter().flatten().copied().collect();
        analysis::reduce(&rgba, width, height).expect("a reducible raster")
    }

    #[test]
    fn channels_are_normalized_against_one_shared_maximum() {
        // Four pixels: three share a red code, so red's tallest bin is 3 and green's is 2.
        let report = report(
            &[
                [10, 20, 30, 255],
                [10, 40, 30, 255],
                [10, 60, 90, 255],
                [80, 20, 90, 255],
            ],
            2,
            2,
        );
        let (bins, max) = normalize(&report);
        assert_eq!(max, 3, "red's bin 10 holds three pixels and is the tallest");
        assert!(
            (bins[0][10] - 1.0).abs() < 1e-6,
            "the tallest bin fills the plot"
        );
        // Green's tallest bin holds two pixels, so it reaches two thirds of the same plot: the
        // shared scale is what makes the two channels comparable.
        assert!((bins[1][20] - 2.0 / 3.0).abs() < 1e-6, "{}", bins[1][20]);
        assert!((bins[2][30] - 2.0 / 3.0).abs() < 1e-6);
        assert_eq!(bins[0][80], 1.0 / 3.0);
        assert_eq!(bins[1][99], 0.0, "an empty code plots as empty");
        // Nothing normalizes above the plot, whatever the counts.
        for channel in &bins {
            assert!(channel.iter().all(|bin| (0.0..=1.0).contains(bin)));
        }
    }

    #[test]
    fn an_empty_report_normalizes_to_an_empty_plot_rather_than_dividing_by_zero() {
        let mut empty = report(&[[1, 2, 3, 255]], 1, 1);
        empty.r = [0; 256];
        empty.g = [0; 256];
        empty.b = [0; 256];
        let (bins, max) = normalize(&empty);
        assert_eq!(max, 0);
        assert!(bins.iter().all(|channel| channel.iter().all(|b| *b == 0.0)));
    }

    #[test]
    fn counters_read_out_every_endpoint_the_contract_names() {
        // One all-black pixel, one all-white, one at both endpoints at once, one ordinary.
        let report = report(
            &[
                [0, 0, 0, 255],
                [255, 255, 255, 255],
                [0, 128, 255, 255],
                [12, 34, 56, 255],
            ],
            2,
            2,
        );
        let counters = Counters::of(&report);
        assert_eq!((counters.r0, counters.g0, counters.b0), (2, 1, 1));
        assert_eq!((counters.r255, counters.g255, counters.b255), (1, 1, 2));
        assert_eq!(counters.any_shadow, 2);
        assert_eq!(counters.any_highlight, 2);
        assert_eq!(counters.all_shadow, 1);
        assert_eq!(counters.all_highlight, 1);
        assert_eq!(counters.both, 1, "the mixed pixel is at both endpoints");
        assert_eq!(
            counters.shadow_text(),
            "0 \u{b7} R 2 G 1 B 1 \u{b7} any 2 \u{b7} all 1"
        );
        assert_eq!(
            counters.highlight_text(),
            "255 \u{b7} R 1 G 1 B 2 \u{b7} any 2 \u{b7} all 1"
        );
        assert_eq!(counters.both_text(), "both 1");
    }

    #[test]
    fn the_readout_names_the_three_output_codes_and_the_pixel() {
        assert_eq!(
            readout_text(&Readout {
                x: 12,
                y: 34,
                rgba: [128, 64, 255, 255],
            }),
            "R 128 \u{b7} G 64 \u{b7} B 255 \u{b7} 12, 34"
        );
    }

    /// A report never changes within one generation, so the derivation that follows a pointer move
    /// reuses the plotted bins instead of normalizing 768 counts again. A new generation does not.
    #[test]
    fn the_plotted_bins_are_reused_while_the_render_identity_is_unchanged() {
        let identity = RenderIdentity {
            entry: "entry-1".into(),
            draft_revision: None,
            generation: 4,
            width: 2,
            height: 2,
        };
        let mut bins = [[0.0f32; 256]; 3];
        bins[0][7] = 1.0;
        let previous = HistogramModel {
            status: HistogramStatus::Ready,
            identity: Some(identity.clone()),
            bins: Some(Box::new(bins)),
            plotted_max: 99,
            ..HistogramModel::default()
        };
        // Same identity: the previous plot and its scale are carried over as they stand.
        let same = (previous.identity.as_ref() == Some(&identity))
            .then(|| previous.bins.clone())
            .flatten();
        assert!(same.is_some());
        assert_eq!(same.expect("the reused bins")[0][7], 1.0);
        // A newer generation is a different render, so nothing is carried over.
        let newer = RenderIdentity {
            generation: 5,
            ..identity.clone()
        };
        assert_ne!(previous.identity.as_ref(), Some(&newer));
        // So is the same generation of another entry, which a history preview produces.
        let elsewhere = RenderIdentity {
            entry: "entry-2".into(),
            ..identity
        };
        assert_ne!(previous.identity.as_ref(), Some(&elsewhere));
    }

    #[test]
    fn a_pending_model_says_so_instead_of_plotting_nothing() {
        let model = HistogramModel::default();
        assert_eq!(model.status, HistogramStatus::Pending);
        assert_eq!(model.notice().as_deref(), Some("No analysis yet"));
        assert!(model.bins.is_none());
        // The domain never depends on what has been analysed: the plot states it on hover even
        // before the first report.
        assert_eq!(model.caption, DOMAIN_CAPTION);
        assert_eq!(model.caption, "Output \u{b7} sRGB \u{b7} after crop");
    }

    /// The notice is drawn inside the plot's own area, so it exists only where there is nothing
    /// else to draw there. `Updating` keeps the previous report on screen, dimmed, and says nothing
    /// in words: a drag is `Updating` for most of its length and must not flash text over the plot.
    #[test]
    fn the_plot_notice_appears_only_while_there_is_no_report() {
        for status in [HistogramStatus::Ready, HistogramStatus::Updating] {
            let model = HistogramModel {
                status,
                stale: status == HistogramStatus::Updating,
                ..HistogramModel::default()
            };
            assert_eq!(model.notice(), None, "{status:?}");
        }
        let unavailable = HistogramModel {
            status: HistogramStatus::Unavailable,
            reason: Some("the frame could not be rendered".into()),
            ..HistogramModel::default()
        };
        assert_eq!(
            unavailable.notice().as_deref(),
            Some("Unavailable: the frame could not be rendered")
        );
        // A failure with no detail still names itself rather than going silent.
        assert_eq!(
            HistogramModel {
                status: HistogramStatus::Unavailable,
                ..HistogramModel::default()
            }
            .notice()
            .as_deref(),
            Some("Unavailable")
        );
    }

    /// The counts are the triangles' tooltips: the shadow triangle states its rule and the code-0
    /// line, the highlight triangle its rule, the code-255 line and the both-endpoints line. A
    /// status with a report prints its counts; one without prints a dash, which is honest — a
    /// zeroed counter would claim nothing is clipped in a frame nobody has reduced.
    #[test]
    fn the_triangle_tooltips_carry_the_counts_and_never_invent_zeros() {
        let report = report(
            &[
                [0, 0, 0, 255],
                [255, 255, 255, 255],
                [0, 128, 255, 255],
                [12, 34, 56, 255],
            ],
            2,
            2,
        );
        let counters = Counters::of(&report);
        for status in [HistogramStatus::Ready, HistogramStatus::Updating] {
            let model = HistogramModel {
                status,
                counters: counters.clone(),
                stale: status == HistogramStatus::Updating,
                ..HistogramModel::default()
            };
            assert_eq!(model.shadow_text(), counters.shadow_text(), "{status:?}");
            assert_eq!(
                model.highlight_text(),
                counters.highlight_text(),
                "{status:?}"
            );
            assert_eq!(model.both_text(), counters.both_text(), "{status:?}");
            assert_eq!(
                model.shadow_tooltip(),
                format!("{SHADOW_RULE}\n0 \u{b7} R 2 G 1 B 1 \u{b7} any 2 \u{b7} all 1"),
                "{status:?}"
            );
            assert_eq!(
                model.highlight_tooltip(),
                format!(
                    "{HIGHLIGHT_RULE}\n255 \u{b7} R 1 G 1 B 2 \u{b7} any 2 \u{b7} all 1\nboth 1"
                ),
                "{status:?}"
            );
        }
        for status in [HistogramStatus::Pending, HistogramStatus::Unavailable] {
            // Even holding a stale `Counters`, a status with no report shows no numbers.
            let model = HistogramModel {
                status,
                counters: counters.clone(),
                ..HistogramModel::default()
            };
            assert_eq!(model.shadow_text(), "0 \u{b7} \u{2014}", "{status:?}");
            assert_eq!(model.highlight_text(), "255 \u{b7} \u{2014}", "{status:?}");
            assert_eq!(model.both_text(), "both \u{2014}", "{status:?}");
            assert_eq!(
                model.shadow_tooltip(),
                format!("{SHADOW_RULE}\n0 \u{b7} \u{2014}"),
                "{status:?}"
            );
            assert_eq!(
                model.highlight_tooltip(),
                format!("{HIGHLIGHT_RULE}\n255 \u{b7} \u{2014}\nboth \u{2014}"),
                "{status:?}"
            );
        }
    }

    #[test]
    fn the_surface_shrinks_by_exactly_the_open_panels() {
        let window = (1440.0, 900.0);
        let divider = luxforge_ui::theme::BORDER_WIDTH;
        let both = photo_surface(window, true, true);
        assert_eq!(
            both,
            (
                1440.0 - STATE_PANEL_WIDTH - TOOLS_PANEL_WIDTH - 2.0 * divider,
                900.0 - TITLE_BAR_HEIGHT - STATUS_BAR_HEIGHT - 2.0 * divider
            )
        );
        let none = photo_surface(window, false, false);
        assert_eq!(none.0, 1440.0);
        assert_eq!(none.1, both.1, "the panels never change the height");
        assert!(photo_surface((10.0, 10.0), true, true).0 >= 0.0);
    }

    /// Fit: the contained size scaled by the display factor. 100% and other percentages: the source
    /// scaled by the percentage alone, because the zoom is already defined in physical pixels.
    #[test]
    fn the_displayed_size_follows_the_zoom() {
        let source = (480, 320);
        let surface = (900.0, 800.0);
        let fit = displayed_size(ZoomView::Fit, source, surface, 2.0, 20.0).expect("a fit size");
        // Available 860x760; the width binds at 860/480, and the capture is at 2x.
        let scale = (860.0f32 / 480.0).min(760.0 / 320.0) * 2.0;
        assert!((fit.0 - 480.0 * scale).abs() < 1e-3);
        assert!((fit.1 - 320.0 * scale).abs() < 1e-3);
        let hundred =
            displayed_size(ZoomView::Percent(100.0), source, surface, 2.0, 20.0).expect("100%");
        assert_eq!(
            hundred,
            (480.0, 320.0),
            "one physical pixel per source pixel"
        );
        let half =
            displayed_size(ZoomView::Percent(50.0), source, surface, 2.0, 20.0).expect("50%");
        assert_eq!(half, (240.0, 160.0));
        let double =
            displayed_size(ZoomView::Percent(200.0), source, surface, 2.0, 20.0).expect("200%");
        assert_eq!(double, (960.0, 640.0));
        // Degenerate inputs produce no size rather than a wrong one.
        assert!(displayed_size(ZoomView::Fit, (0, 0), surface, 2.0, 20.0).is_none());
        assert!(displayed_size(ZoomView::Fit, source, (10.0, 10.0), 2.0, 20.0).is_none());
        assert!(displayed_size(ZoomView::Percent(0.0), source, surface, 2.0, 20.0).is_none());
    }

    #[test]
    fn the_cell_grid_never_exceeds_the_source_or_the_cap() {
        // Fit on a small photograph: it is drawn larger than it is, so the grid is the source and
        // an isolated clipped pixel keeps a cell of its own.
        assert_eq!(overlay_cells((480, 320), (1200.0, 800.0)), Some((480, 320)));
        // 100% on the same photograph: exactly one cell per source pixel.
        assert_eq!(overlay_cells((480, 320), (480.0, 320.0)), Some((480, 320)));
        // 200%: still one cell per source pixel, never finer.
        assert_eq!(overlay_cells((480, 320), (960.0, 640.0)), Some((480, 320)));
        // 50%: one cell per physical pixel of the photograph, aspect preserved.
        assert_eq!(overlay_cells((480, 320), (240.0, 160.0)), Some((240, 160)));
        // Fit on a 24 MP photograph in a 1200 pt-wide surface: bounded by what is on screen.
        assert_eq!(
            overlay_cells((6000, 4000), (1200.0, 800.0)),
            Some((1200, 800))
        );
        // 100% on a photograph larger than the cap: the cap binds on both axes at one scale.
        let capped = overlay_cells((10000, 6000), (10000.0, 6000.0)).expect("a capped grid");
        assert!(
            capped.0 <= MAX_OVERLAY_CELLS && capped.1 <= MAX_OVERLAY_CELLS,
            "{capped:?}"
        );
        assert_eq!(capped.0, MAX_OVERLAY_CELLS);
        assert_eq!(capped.1, 2458, "the cap is applied as one uniform scale");
        // The grid keeps the source's aspect ratio, so the overlay lands where the photo does.
        let ratio = capped.0 as f64 / capped.1 as f64;
        assert!((ratio - 10000.0 / 6000.0).abs() < 0.01, "{ratio}");
        // Degenerate inputs produce no grid.
        assert_eq!(overlay_cells((0, 10), (10.0, 10.0)), None);
        assert_eq!(overlay_cells((10, 10), (f32::NAN, 10.0)), None);
        assert_eq!(overlay_cells((10, 10), (0.0, 10.0)), None);
        // A photograph drawn a fraction of a pixel wide still gets one cell, never zero.
        assert_eq!(overlay_cells((10, 10), (0.2, 0.2)), Some((1, 1)));
    }
}
