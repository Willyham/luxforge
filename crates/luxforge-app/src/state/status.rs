//! The status bar model: the last message, who else is connected and what the
//! renderer is doing, and the wording of the sentence that says what last happened.
//!
//! The message is a plain sentence. It names an entry by the sequence number its history row
//! carries and never by its identity, its snapshot or its source hash: those stay with the API.
use crate::state::{ACTOR, Inputs, title};
use luxforge_core::{EditorState, Renderer, RendererRecord, Zoom};
use std::time::Duration;

/// How long the frame on the photo surface took to render, as the preview worker measured it for
/// that frame's own phase ([`luxforge_core::PreviewResult::render_ms`]). It travels with the
/// frame: a zoom that hands a retained frame back to the surface brings that frame's own time with
/// it, so the figure is always the picture on screen and never the time since some earlier request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RenderTime {
    pub(crate) ms: f64,
    /// The frame approximates a drafted RAW white balance on planes developed at another one
    /// ([`luxforge_core::PreviewResult::approximate_white_balance`]).
    pub(crate) approximate: bool,
}

impl RenderTime {
    /// "Exact render · 85 ms" for the reference renderer's exact frame, or its reduction to the
    /// view, and "Approximate render · 12 ms" for a drafted RAW white balance approximated on the
    /// developed planes.
    pub(crate) fn text(self) -> String {
        let kind = if self.approximate {
            "Approximate"
        } else {
            "Exact"
        };
        format!("{kind} render \u{b7} {}", figure(self.ms))
    }
}

/// "GPU render · 2 ms" while the photograph on screen is the GPU's render of the committed stack at
/// rest, and "GPU preview · 2 ms" while it is a gesture's GPU frame, beside the CPU frames'
/// "Approximate render" and "Exact render", with the interface thread's time to prepare that frame,
/// or the picture at rest's tiles (the desktop's `gpu_settle` says why it is that time).
pub(crate) fn gpu_text(ms: f64, at_rest: bool) -> String {
    let kind = if at_rest { "GPU render" } else { "GPU preview" };
    format!("{kind} \u{b7} {}", figure(ms))
}

/// How long a gesture's ticks must have named `compiling` before the status bar says the GPU
/// preview is preparing: a compile that ends sooner is the gesture's first tick or two, which no
/// one needs told about.
pub(crate) const COMPILING_AFTER: Duration = Duration::from_millis(500);

/// What the status bar says of a gesture's CPU path when the reason lasts, or of the reference
/// renderer drawing every frame: a short muted phrase after the render slot, and the sentence its
/// tooltip gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fallback {
    pub(crate) phrase: String,
    pub(crate) tooltip: String,
}

/// When a class of reason is said.
#[derive(Clone, Copy)]
enum Say {
    /// From the session's renderer ([`renderer_notice`]), at rest and during a gesture alike, for
    /// as long as the reference renderer draws the desktop's picture for one of the class's
    /// reasons; never from a gesture's CPU reason, which names the same codes while it lasts.
    Renderer,
    /// For as long as the latest tick took the CPU path for it.
    Lasting,
    /// With `: <layer>` after the phrase, the layer being the one the reason names; a reason that
    /// names none says `unnamed` in its tooltip instead.
    Layer { unnamed: &'static str },
    /// Once the ticks that named it have done so for [`COMPILING_AFTER`].
    Delayed,
    /// At rest, at once, for as long as the photograph is the reference renderer's frame because
    /// the GPU's picture at rest is compiling its programs ([`rest_compiling_notice`]); never from
    /// a gesture's tick.
    Rest,
}

/// One class of reason a gesture, or every frame, is drawn on the CPU path: the codes
/// `state.surface.gpu.plan_fallback` records for it, and its phrase and tooltip.
struct Class {
    codes: &'static [&'static str],
    phrase: &'static str,
    tooltip: &'static str,
    say: Say,
}

/// Every class the notice names, in the design's order (`docs/design/gpu-preview.md`, "Labels and
/// overlays during motion"): the one place its wording lives. The first is the reference
/// renderer's, said from the session's renderer; the rest are a gesture's, said from its latest
/// tick. A code in none of them says nothing: the reasons that pass within a tick or two or a job
/// (`boundary-pending`, `source-uploading`, `source-missing` and `surface-pending`, which is also
/// the session's renderer before the photo surface has checked its GPU stage), and the two the
/// table does not name (`unchanged` and `unplannable`). The last is the picture at rest's while its programs compile, said at rest
/// alone.
const CLASSES: [Class; 7] = [
    Class {
        codes: &["no-adapter", "device-lost"],
        phrase: "Reference renderer",
        tooltip: "This graphics device cannot run the GPU renderer, or this launch turned it off, \
                  so every frame is drawn by the reference renderer on the CPU, which is slower.",
        say: Say::Renderer,
    },
    Class {
        codes: &["budget-exceeded", "texture-limit", "buffer-limit"],
        phrase: "GPU memory full",
        tooltip: "This many layers at this zoom need more than the GPU preview holds, so the \
                  preview is drawn on the CPU, which is slower. Fewer masked Presence or Detail \
                  layers, or Fit, draw on the GPU.",
        say: Say::Lasting,
    },
    Class {
        codes: &["budget-reduced"],
        phrase: "Softer while dragging",
        tooltip: "This many layers at this zoom need more than the GPU preview holds, so the drag \
                  is drawn on the GPU at the detail Fit shows, scaled to the view, and is sharp \
                  again when you let go.",
        say: Say::Lasting,
    },
    Class {
        codes: &["pipeline-failed"],
        phrase: "GPU preview unavailable",
        tooltip: "The GPU preview cannot run on this graphics device, so previews are drawn on \
                  the CPU.",
        say: Say::Lasting,
    },
    Class {
        codes: &[
            "pixel-stage",
            "boundary-stage",
            "spatial-unit",
            "spatial-chain",
            "between-resamples",
            "no-program",
            "disabled-program",
            "warp-grid",
            "boundary-size",
            "position-range",
            "region-outside",
            "light-link",
        ],
        phrase: "Not on the GPU",
        tooltip: "The GPU preview cannot draw this layer yet, so this drag is drawn on the CPU.",
        say: Say::Layer {
            unnamed: "The GPU preview cannot draw this stack yet, so this drag is drawn on the \
                      CPU.",
        },
    },
    Class {
        codes: &["compiling"],
        phrase: "Preparing GPU preview",
        tooltip: "The GPU preview is compiling its programs for this stack; drags are drawn on \
                  the CPU until it is ready.",
        say: Say::Delayed,
    },
    Class {
        codes: &["compiling"],
        phrase: "Preparing GPU render",
        tooltip: "The GPU renderer is compiling its programs for this photo, so the reference \
                  renderer draws it on the CPU until they are ready.",
        say: Say::Rest,
    },
];

/// Why a gesture's latest tick took the CPU path while the GPU preview is on, as the desktop's
/// drag recorded it: what the status bar's notice is derived from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CpuReason<'a> {
    /// The reason's code, as evidence records it.
    pub(crate) code: &'a str,
    /// The label the recipe list gives the layer the reason names, when it names one.
    pub(crate) layer: Option<&'a str>,
    /// While the code is `compiling`, how long the ticks that named it had done so at the latest
    /// one. The notice is read from this and never from the clock, so a drag that holds still keeps
    /// what it last said.
    pub(crate) compiling_for: Option<Duration>,
}

impl Fallback {
    /// The notice as a captured frame records it, beside the reason it was derived from.
    pub(crate) fn evidence(notice: Option<&Self>) -> serde_json::Value {
        notice.map_or(
            serde_json::Value::Null,
            |notice| serde_json::json!({"phrase": notice.phrase, "tooltip": notice.tooltip}),
        )
    }
}

impl CpuReason<'_> {
    /// What the notice says of this reason, or `None` when it says nothing: a reason that passes,
    /// one the table does not name, `compiling` before [`COMPILING_AFTER`], or one the reference
    /// renderer's notice says from the session instead ([`renderer_notice`]).
    pub(crate) fn notice(&self) -> Option<Fallback> {
        let class = CLASSES
            .iter()
            .find(|class| class.codes.contains(&self.code))?;
        let (phrase, tooltip) = match (class.say, self.layer) {
            (Say::Renderer | Say::Rest, _) => return None,
            (Say::Lasting, _) => (class.phrase.to_owned(), class.tooltip),
            (Say::Layer { .. }, Some(layer)) => {
                (format!("{}: {layer}", class.phrase), class.tooltip)
            }
            (Say::Layer { unnamed }, None) => (class.phrase.to_owned(), unnamed),
            (Say::Delayed, _) => {
                if self.compiling_for? < COMPILING_AFTER {
                    return None;
                }
                (class.phrase.to_owned(), class.tooltip)
            }
        };
        Some(Fallback {
            phrase,
            tooltip: tooltip.to_owned(),
        })
    }
}

/// What the notice says of the renderer the session reports, at rest and during a gesture alike:
/// the reference renderer's class while it draws the desktop's picture because the photo surface's
/// GPU stage cannot run on this graphics device, was refused by the launch (both `no-adapter`) or
/// lost its device (`device-lost`). `None` for the GPU, for `surface-pending` (the surface has not
/// checked its stage yet), and for an owner that draws nothing (the reference with no reason). It
/// is the value every API client reads in `session.state`, so the bar and the API cannot disagree.
pub(crate) fn renderer_notice(renderer: Renderer) -> Option<Fallback> {
    if renderer.record() != RendererRecord::Reference {
        return None;
    }
    let code = renderer.reason()?.as_str();
    let class = CLASSES
        .iter()
        .find(|class| matches!(class.say, Say::Renderer) && class.codes.contains(&code))?;
    Some(Fallback {
        phrase: class.phrase.to_owned(),
        tooltip: class.tooltip.to_owned(),
    })
}

/// What the notice says while the photograph at rest is the reference renderer's frame because the
/// GPU's picture at rest, its view plan or its tiles, is still compiling its programs: said at
/// once, since nothing at rest decides it later, and gone with the GPU frame that replaces it.
pub(crate) fn rest_compiling_notice() -> Fallback {
    let class = CLASSES
        .iter()
        .find(|class| matches!(class.say, Say::Rest))
        .expect("the picture at rest's class");
    Fallback {
        phrase: class.phrase.to_owned(),
        tooltip: class.tooltip.to_owned(),
    }
}

/// A time as the render slot gives it. A frame faster than half a millisecond says so rather than
/// claiming zero, and one of a second or more is given in seconds to a tenth (`1.2 s`).
fn figure(ms: f64) -> String {
    if ms < 0.5 {
        "<1 ms".to_owned()
    } else if ms.round() >= 1000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else {
        format!("{} ms", ms.round() as i64)
    }
}

/// What last happened to the open photograph, as the status bar says it once its frame is on
/// screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Happened {
    /// A photograph was opened.
    Opened { file: String },
    /// An action made the entry at `sequence`. `agent` names another client that made it; `None`
    /// is this desktop.
    Applied {
        label: String,
        sequence: u64,
        agent: Option<String>,
    },
    /// Undo moved the current state back past the entry labelled `label`.
    Undid { label: String },
    /// An edit returned its control to where a chain of edits of it began, so auto-collapse hid
    /// the chain's entry, labelled `label`, and the entry at `sequence` is current again.
    Collapsed { label: String, sequence: u64 },
    /// Redo moved the current state forward to an entry that already existed.
    Redid { label: String, sequence: u64 },
    /// A historical preview returned to the current entry.
    Returned { label: String, sequence: u64 },
    /// A composite action changed nothing, because nothing it holds applies to the photo.
    NothingApplied,
}

impl Happened {
    /// What a new state says happened, from the state this desktop held before it. `known` is
    /// whether the new current entry was already among the loaded history rows, which is what
    /// tells a redo from a new entry, and `collapsed` whether this desktop's own edit collapsed the
    /// entry that was current, which tells an edit back to the chain's start from an undo. `None`
    /// when the current entry did not move.
    pub(crate) fn between(
        before: Option<&EditorState>,
        after: &EditorState,
        known: bool,
        collapsed: bool,
    ) -> Option<Self> {
        let entry = &after.current_entry;
        let Some(before) = before.filter(|before| before.asset.id == after.asset.id) else {
            return Some(Self::opened(after));
        };
        let previous = &before.current_entry;
        if previous.id == entry.id {
            return None;
        }
        Some(if entry.sequence < previous.sequence && collapsed {
            Self::Collapsed {
                label: previous.label.clone(),
                sequence: entry.sequence,
            }
        } else if entry.sequence < previous.sequence {
            Self::Undid {
                label: previous.label.clone(),
            }
        } else if known {
            Self::Redid {
                label: entry.label.clone(),
                sequence: entry.sequence,
            }
        } else {
            Self::Applied {
                label: entry.label.clone(),
                sequence: entry.sequence,
                agent: (entry.actor != ACTOR).then(|| entry.actor.clone()),
            }
        })
    }

    /// The photograph `state` holds was opened, whatever this desktop held before: opening the
    /// photograph already open is an open too.
    pub(crate) fn opened(state: &EditorState) -> Self {
        Self::Opened {
            file: state
                .asset
                .locator
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "a photograph".to_owned()),
        }
    }

    pub(crate) fn sentence(&self) -> String {
        match self {
            Self::Opened { file } => format!("Opened {file}"),
            Self::Applied {
                label,
                sequence,
                agent: None,
            } => format!("Applied {label} \u{b7} entry {sequence}"),
            Self::Applied {
                label,
                sequence,
                agent: Some(agent),
            } => format!("Agent {agent} applied {label} \u{b7} entry {sequence}"),
            Self::Undid { label } => format!("Undid {label}"),
            Self::Collapsed { label, sequence } => {
                format!("Back to entry {sequence} \u{b7} collapsed {label}")
            }
            Self::Redid { label, sequence } => format!("Redid {label} \u{b7} entry {sequence}"),
            Self::Returned { label, sequence } => {
                format!("Returned to entry {sequence} \u{b7} {label}")
            }
            Self::NothingApplied => "Nothing applied".to_owned(),
        }
    }
}

/// What the status bar adds when a composite action (a preset) left settings out because they do
/// not apply to the photo: "1 setting does not apply to a RAW photo".
pub(crate) fn skipped(count: usize, kind: luxforge_core::SourceTag) -> String {
    let kind = kind.label();
    match count {
        1 => format!("1 setting does not apply to a {kind} photo"),
        count => format!("{count} settings do not apply to a {kind} photo"),
    }
}

/// What the status bar says while a historical entry is on screen: its sequence number and label
/// as its history row shows them, or that some history is shown when the loaded page does not hold
/// its row.
pub(crate) fn previewing(entry: Option<(u64, &str)>) -> String {
    match entry {
        Some((sequence, label)) => format!("Previewing entry {sequence} \u{b7} {label}"),
        None => "Previewing history".to_owned(),
    }
}

/// The current entry, when nothing this desktop has seen says how it came to be current.
pub(crate) fn showing(sequence: u64, label: &str) -> String {
    format!("Entry {sequence} \u{b7} {label}")
}

/// While Compare holds the Original on screen.
pub(crate) const COMPARING: &str = "Comparing with the original";

/// What the status bar says while a mask gesture is open in Mask mode: the mode, the mask, the
/// component and how the gesture becomes history. Each drag of a gradient is one draft that its
/// release commits as one entry, as each stroke of a brush is. `painting` is `None` for a shape, and for a brush whether its stroke is down.
pub(crate) fn mask_gesture(
    names: &crate::state::canvas::GestureNames,
    painting: Option<bool>,
) -> String {
    let (mask, component) = (&names.mask, &names.component);
    match painting {
        None => format!("Mask mode \u{b7} {mask} \u{b7} {component} \u{b7} each drag is one entry"),
        Some(true) => {
            format!(
                "Mask mode \u{b7} {mask} \u{b7} {component} painting \u{b7} each stroke is one entry"
            )
        }
        Some(false) => format!(
            "Mask mode \u{b7} {mask} \u{b7} {component} \u{b7} press to paint \u{b7} each stroke is one entry"
        ),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusBarModel {
    pub(crate) message: String,
    /// How many other clients are connected, or why the count is unknown.
    pub(crate) clients: String,
    /// Another client is connected, so the dot beside the count is lit.
    pub(crate) agents_connected: bool,
    /// What the renderer is doing, or what kind of frame is on screen and how long it took.
    pub(crate) render: String,
    /// The GPU frame's figure `render` gives, in microseconds, while it names one.
    pub(crate) gpu_us: Option<u64>,
    /// Why the picture is drawn on the slower CPU path: while the session's renderer is the
    /// reference for a reason of the reference renderer's class, that class, at rest and during a
    /// gesture ([`renderer_notice`]); otherwise why the open gesture is, while the GPU preview is on
    /// and the reason lasts ([`CpuReason::notice`]). The muted phrase after `render`, with its
    /// tooltip. `None` while a GPU frame is on screen, and when the gesture's settle ends.
    pub(crate) fallback: Option<Fallback>,
    /// The zoom mode, its effective percentage and the display scale: `Fit · 18% · 2×`.
    pub(crate) view: String,
}

/// "1 agent connected", "2 agents connected" or "No agents connected"; the desktop is not one of
/// them. `None` when the local server could not start, so nothing can connect.
pub(crate) fn clients_text(clients: Option<usize>) -> String {
    match clients {
        None => "Live API unavailable".into(),
        Some(0) => "No agents connected".into(),
        Some(1) => "1 agent connected".into(),
        Some(count) => format!("{count} agents connected"),
    }
}

/// The display scale as a multiplier, without decimals when it is whole: `2×`, `1.5×`.
pub(crate) fn scale_text(scale: f32) -> String {
    let rounded = (scale * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 {
        format!("{}\u{d7}", rounded as i64)
    } else {
        format!("{rounded}\u{d7}")
    }
}

/// `Fit · 18% · 2×` at Fit, where the effective percentage is what Fit comes to on this window;
/// `50% · 2×` at a percentage, which is its own effective percentage; `Fit · 2×` before a
/// photograph gives Fit a size.
pub(crate) fn view_text(zoom: &Zoom, effective: Option<f32>, scale: f32) -> String {
    let scale = scale_text(scale);
    match (zoom, effective) {
        (Zoom::Fit, Some(effective)) => format!(
            "Fit \u{b7} {} \u{b7} {scale}",
            title::percent_text(effective)
        ),
        (Zoom::Fit, None) => format!("Fit \u{b7} {scale}"),
        (Zoom::Percent { value }, _) => {
            format!("{} \u{b7} {scale}", title::percent_text(*value))
        }
    }
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> StatusBarModel {
    StatusBarModel {
        message: inputs.status.to_owned(),
        clients: clients_text(inputs.clients),
        agents_connected: inputs.clients.is_some_and(|count| count > 0),
        // A GPU frame on screen names itself first: the CPU frame behind it, and any render still
        // running, are not what is shown.
        render: if let Some(us) = inputs.gpu_frame_us {
            gpu_text(us as f64 / 1000.0, inputs.gpu_at_rest)
        } else if let Some(bar) = inputs.render_bar {
            format!("Rendering… {:.0}%", (bar.fraction * 100.0).floor())
        } else if inputs.rendering {
            "Rendering…".into()
        } else {
            match inputs.render {
                Some(time) => time.text(),
                None => "Idle".into(),
            }
        },
        gpu_us: inputs.gpu_frame_us,
        // The notice and a GPU frame never share the bar: the next GPU frame clears it. The
        // reference renderer's notice is the session's, so it is said at rest too, and before any
        // reason a gesture's tick gives.
        fallback: if inputs.gpu_frame_us.is_some() {
            None
        } else {
            renderer_notice(inputs.session.renderer)
                .or_else(|| inputs.cpu_reason.as_ref().and_then(CpuReason::notice))
                .or_else(|| inputs.rest_compiling.then(rest_compiling_notice))
        },
        // The display's own scale: the interface size is a choice of its own, not the display's.
        view: view_text(
            &inputs.session.preview.view.zoom,
            title::effective_percent(inputs),
            inputs.view_state.system_scale_factor,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testing::entry;
    use luxforge_core::{AssetId, AssetRecord, HistoryEntry};
    use std::path::PathBuf;

    /// `photo.jpg` open at `current`.
    fn open_at(asset: &AssetId, current: HistoryEntry) -> EditorState {
        EditorState {
            asset: AssetRecord {
                id: asset.clone(),
                source_root: PathBuf::new(),
                locator: PathBuf::from("photo.jpg"),
                fingerprint: "f".into(),
                file_identity: "i".into(),
                byte_len: 0,
                width: 1,
                height: 1,
                source: luxforge_core::SourceKind::Jpeg,
            },
            revision: current.sequence,
            current_entry: current,
            redo: Vec::new(),
        }
    }

    #[test]
    fn a_skip_is_counted_and_names_the_photos_kind() {
        use luxforge_core::SourceTag;
        assert_eq!(
            skipped(1, SourceTag::Raw),
            "1 setting does not apply to a RAW photo"
        );
        assert_eq!(
            skipped(3, SourceTag::Jpeg),
            "3 settings do not apply to a JPEG photo"
        );
        assert_eq!(Happened::NothingApplied.sentence(), "Nothing applied");
    }

    #[test]
    fn the_render_time_names_the_kind_of_frame_it_describes() {
        let time = |ms: f64, _: bool, approximate: bool| RenderTime { ms, approximate };
        assert_eq!(
            time(12.4, true, true).text(),
            "Approximate render \u{b7} 12 ms"
        );
        assert_eq!(time(85.5, false, false).text(), "Exact render \u{b7} 86 ms");
        // A tiny frame is not "0 ms".
        assert_eq!(
            time(0.2, true, true).text(),
            "Approximate render \u{b7} <1 ms"
        );
        assert_eq!(time(0.5, false, false).text(), "Exact render \u{b7} 1 ms");
        // A second or more reads in seconds, to a tenth.
        assert_eq!(
            time(999.4, false, false).text(),
            "Exact render \u{b7} 999 ms"
        );
        assert_eq!(
            time(1234.0, false, false).text(),
            "Exact render \u{b7} 1.2 s"
        );
        // A drafted RAW white balance approximated on the developed planes says so, at Fit and
        // at 100% alike.
        assert_eq!(
            time(9.2, true, true).text(),
            "Approximate render \u{b7} 9 ms"
        );
        assert_eq!(
            time(140.0, false, true).text(),
            "Approximate render \u{b7} 140 ms"
        );
    }

    /// A GPU frame on screen names itself in the same wording, beside the CPU frames' kinds: a
    /// gesture's as the GPU preview, the committed stack's at rest as the GPU's render.
    #[test]
    fn the_gpu_frame_names_itself_with_its_time() {
        assert_eq!(gpu_text(2.4, false), "GPU preview \u{b7} 2 ms");
        assert_eq!(gpu_text(0.3, false), "GPU preview \u{b7} <1 ms");
        assert_eq!(gpu_text(1500.0, false), "GPU preview \u{b7} 1.5 s");
        assert_eq!(gpu_text(12.4, true), "GPU render \u{b7} 12 ms");
    }

    /// The reason with `code`, naming `layer`, whose `compiling` ticks have run for `compiling`.
    fn reason<'a>(
        code: &'a str,
        layer: Option<&'a str>,
        compiling: Option<Duration>,
    ) -> CpuReason<'a> {
        CpuReason {
            code,
            layer,
            compiling_for: compiling,
        }
    }

    /// The notice's phrase and tooltip for `code` and `layer`, as the status bar gives them.
    fn said(code: &str, layer: Option<&str>) -> Option<(String, String)> {
        reason(code, layer, None)
            .notice()
            .map(|notice| (notice.phrase, notice.tooltip))
    }

    /// Every class of reason names itself with its phrase and its one-sentence tooltip, whichever
    /// of its codes the drag recorded.
    #[test]
    fn each_class_of_reason_says_its_phrase_and_tooltip() {
        let classes: [(&[&str], &str, &str); 3] = [
            (
                &["budget-exceeded", "texture-limit", "buffer-limit"],
                "GPU memory full",
                "This many layers at this zoom need more than the GPU preview holds, so the \
                 preview is drawn on the CPU, which is slower. Fewer masked Presence or Detail \
                 layers, or Fit, draw on the GPU.",
            ),
            (
                &["budget-reduced"],
                "Softer while dragging",
                "This many layers at this zoom need more than the GPU preview holds, so the drag \
                 is drawn on the GPU at the detail Fit shows, scaled to the view, and is sharp \
                 again when you let go.",
            ),
            (
                &["pipeline-failed"],
                "GPU preview unavailable",
                "The GPU preview cannot run on this graphics device, so previews are drawn on \
                 the CPU.",
            ),
        ];
        for (codes, phrase, tooltip) in classes {
            for code in codes {
                // None of these names a layer, whichever the drag names.
                for layer in [None, Some("Presence")] {
                    assert_eq!(
                        said(code, layer),
                        Some((phrase.to_owned(), tooltip.to_owned())),
                        "{code}"
                    );
                }
            }
        }
        // Compiling says it once it has lasted.
        let compiling = reason("compiling", None, Some(Duration::from_secs(2)))
            .notice()
            .expect("a compile that lasted");
        assert_eq!(compiling.phrase, "Preparing GPU preview");
        assert_eq!(
            compiling.tooltip,
            "The GPU preview is compiling its programs for this stack; drags are drawn on the \
             CPU until it is ready."
        );
    }

    /// The reference renderer's class is the session's: said for the reference renderer drawing
    /// for `no-adapter` (a device that cannot run the GPU stage, or a launch that refused it) and
    /// for `device-lost`, and for nothing else the session can report. A gesture's tick that names
    /// either code says nothing of its own, so the notice has one source.
    #[test]
    fn the_reference_renderers_notice_is_the_sessions_alone() {
        use luxforge_core::RendererReason;
        let reference = Some((
            "Reference renderer".to_owned(),
            "This graphics device cannot run the GPU renderer, or this launch turned it off, so \
             every frame is drawn by the reference renderer on the CPU, which is slower."
                .to_owned(),
        ));
        let noticed =
            |renderer| renderer_notice(renderer).map(|notice| (notice.phrase, notice.tooltip));
        for reason in [RendererReason::NoAdapter, RendererReason::DeviceLost] {
            assert_eq!(
                noticed(Renderer::reference(reason)),
                reference,
                "{reason:?}"
            );
            assert_eq!(
                said(reason.as_str(), None),
                None,
                "{reason:?}: a tick says nothing of its own"
            );
        }
        for renderer in [
            Renderer::gpu(),
            Renderer::headless(),
            Renderer::reference(RendererReason::SurfacePending),
        ] {
            assert_eq!(noticed(renderer), None, "{renderer:?}");
        }
    }

    /// The stack's class names the layer the reason names, by the label the recipe list gives it;
    /// a reason that names none says so in its tooltip.
    #[test]
    fn the_stacks_class_names_the_layer_its_reason_names() {
        let codes = [
            "pixel-stage",
            "boundary-stage",
            "spatial-unit",
            "spatial-chain",
            "between-resamples",
            "no-program",
            "disabled-program",
            "warp-grid",
            "boundary-size",
            "position-range",
            "region-outside",
        ];
        for code in codes {
            assert_eq!(
                said(code, Some("Presence")),
                Some((
                    "Not on the GPU: Presence".to_owned(),
                    "The GPU preview cannot draw this layer yet, so this drag is drawn on the CPU."
                        .to_owned()
                )),
                "{code}"
            );
            assert_eq!(
                said(code, None),
                Some((
                    "Not on the GPU".to_owned(),
                    "The GPU preview cannot draw this stack yet, so this drag is drawn on the CPU."
                        .to_owned()
                )),
                "{code}"
            );
        }
    }

    /// A reason that passes within a tick or two or a job, and a code the table does not name, say
    /// nothing.
    #[test]
    fn passing_reasons_and_unnamed_codes_say_nothing() {
        for code in [
            "boundary-pending",
            "source-uploading",
            "source-missing",
            "surface-pending",
            "unchanged",
            "unplannable",
            "a-code-of-another-day",
        ] {
            assert_eq!(said(code, None), None, "{code}");
            assert_eq!(said(code, Some("Presence")), None, "{code}");
        }
    }

    /// `compiling` says its phrase only once the ticks that named it have done so for half a
    /// second: a compile that ends sooner is the gesture's first tick or two.
    #[test]
    fn compiling_says_its_phrase_only_once_it_has_lasted_half_a_second() {
        let ms = Duration::from_millis;
        for (lasted, shown) in [
            (None, false),
            (Some(ms(0)), false),
            (Some(ms(499)), false),
            (Some(ms(500)), true),
            (Some(ms(501)), true),
            (Some(ms(4000)), true),
        ] {
            assert_eq!(
                reason("compiling", None, lasted).notice().is_some(),
                shown,
                "{lasted:?}"
            );
        }
        // The delay is `compiling`'s alone: another reason says itself at once.
        assert!(
            reason("budget-exceeded", None, Some(ms(0)))
                .notice()
                .is_some()
        );
    }

    #[test]
    fn the_clients_caption_counts_agents_and_says_when_none_can_connect() {
        assert_eq!(clients_text(Some(0)), "No agents connected");
        assert_eq!(clients_text(Some(1)), "1 agent connected");
        assert_eq!(clients_text(Some(2)), "2 agents connected");
        assert_eq!(clients_text(None), "Live API unavailable");
    }

    #[test]
    fn the_view_caption_is_the_mode_the_effective_percentage_and_the_scale() {
        assert_eq!(
            view_text(&Zoom::Fit, Some(18.2), 2.0),
            "Fit \u{b7} 18% \u{b7} 2\u{d7}"
        );
        assert_eq!(view_text(&Zoom::Fit, None, 1.0), "Fit \u{b7} 1\u{d7}");
        assert_eq!(
            view_text(&Zoom::Percent { value: 50.0 }, Some(50.0), 1.5),
            "50% \u{b7} 1.5\u{d7}"
        );
        assert_eq!(scale_text(2.0), "2\u{d7}");
        assert_eq!(scale_text(1.25), "1.25\u{d7}");
    }

    /// The sentence names what happened and the entry it made by its sequence number, never by an
    /// identity: an open, this desktop's edit, another client's, an undo, a redo and a return.
    #[test]
    fn the_status_sentence_says_what_last_happened() {
        let asset = AssetId::new();
        let original = entry(&asset, 0, None);
        let opened = open_at(&asset, original.clone());
        let file = "photo.jpg".to_owned();
        assert_eq!(
            Happened::between(None, &opened, false, false),
            Some(Happened::Opened { file: file.clone() })
        );
        assert_eq!(
            Happened::Opened { file: file.clone() }.sentence(),
            format!("Opened {file}")
        );
        // The same current entry read again is not news.
        assert_eq!(Happened::between(Some(&opened), &opened, true, false), None);

        let mut clarity = entry(&asset, 7, Some(&original.id));
        clarity.label = "Clarity +18".into();
        clarity.actor = ACTOR.into();
        let applied = open_at(&asset, clarity);
        let mine = Happened::between(Some(&opened), &applied, false, false).expect("a change");
        assert_eq!(mine.sentence(), "Applied Clarity +18 \u{b7} entry 7");

        let mut theirs = applied.clone();
        theirs.current_entry.actor = "lw-assist".into();
        assert_eq!(
            Happened::between(Some(&opened), &theirs, false, false)
                .expect("a change")
                .sentence(),
            format!(
                "Agent lw-assist applied Clarity +18 \u{b7} entry {}",
                applied.current_entry.sequence
            )
        );

        // Back to the older entry is an undo of the newer one's label; forward to one already
        // loaded is a redo.
        assert_eq!(
            Happened::between(Some(&applied), &opened, true, false)
                .expect("a change")
                .sentence(),
            "Undid Clarity +18"
        );
        assert_eq!(
            Happened::between(Some(&opened), &applied, true, false)
                .expect("a change")
                .sentence(),
            format!(
                "Redid Clarity +18 \u{b7} entry {}",
                applied.current_entry.sequence
            )
        );
        // Back to the older entry because this desktop's edit returned a control to where its
        // chain began is a collapse, not an undo.
        assert_eq!(
            Happened::between(Some(&applied), &opened, true, true)
                .expect("a change")
                .sentence(),
            "Back to entry 0 \u{b7} collapsed Clarity +18"
        );
        assert_eq!(
            Happened::Returned {
                label: "Crop 4:5".into(),
                sequence: 3
            }
            .sentence(),
            "Returned to entry 3 \u{b7} Crop 4:5"
        );
        assert_eq!(
            previewing(Some((3, "Crop 4:5"))),
            "Previewing entry 3 \u{b7} Crop 4:5"
        );
        assert_eq!(previewing(None), "Previewing history");

        // Another photograph is an open, whatever its entries.
        let mut other = applied.clone();
        other.asset.id = luxforge_core::AssetId::new();
        assert!(matches!(
            Happened::between(Some(&opened), &other, false, false),
            Some(Happened::Opened { .. })
        ));
    }

    /// A mask gesture's line names the mode, the mask and the component, and says how the gesture
    /// becomes history: a shape on Apply or Enter, a brush one entry per stroke.
    #[test]
    fn a_mask_gestures_line_names_the_mask_and_how_it_commits() {
        let names = crate::state::canvas::GestureNames {
            mask: "Face".into(),
            component: "Radial 1".into(),
            mode: "Add",
        };
        assert_eq!(
            mask_gesture(&names, None),
            "Mask mode \u{b7} Face \u{b7} Radial 1 \u{b7} each drag is one entry"
        );
        let brush = crate::state::canvas::GestureNames {
            component: "Brush 1".into(),
            ..names
        };
        assert_eq!(
            mask_gesture(&brush, Some(true)),
            "Mask mode \u{b7} Face \u{b7} Brush 1 painting \u{b7} each stroke is one entry"
        );
        assert_eq!(
            mask_gesture(&brush, Some(false)),
            "Mask mode \u{b7} Face \u{b7} Brush 1 \u{b7} press to paint \u{b7} each stroke is one entry"
        );
    }
}
