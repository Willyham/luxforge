//! The status bar model: the last message, the pointer readout, who else is connected and what the
//! renderer is doing, and the wording of the sentence that says what last happened.
//!
//! The message is a plain sentence. It names an entry by the sequence number its history row
//! carries and never by its identity, its snapshot or its source hash: those stay with the API.
use crate::state::{ACTOR, Inputs, histogram, title};
use luxforge_core::{EditorState, Zoom};

/// How long the frame on the photo surface took to render, as the preview worker measured it for
/// that frame's own phase ([`luxforge_core::PreviewResult::render_ms`]). It travels with the
/// frame: a zoom that hands a retained frame back to the surface brings that frame's own time with
/// it, so the figure is always the picture on screen and never the time since some earlier request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RenderTime {
    pub(crate) ms: f64,
    /// The frame is the display-size proxy rather than the exact full-resolution render.
    pub(crate) proxy: bool,
    /// The frame approximates a drafted RAW white balance on planes developed at another one
    /// ([`luxforge_core::PreviewResult::approximate_white_balance`]).
    pub(crate) approximate: bool,
}

impl RenderTime {
    /// "Exact render · 85 ms" for the exact full-resolution render, and "Approximate render · 12
    /// ms" for anything else on screen: the display-size proxy, or a drafted RAW white balance
    /// approximated on the developed planes. A frame faster than half a millisecond says so rather
    /// than claiming zero, and one of a second or more is given in seconds to a tenth (`1.2 s`).
    pub(crate) fn text(self) -> String {
        let figure = if self.ms < 0.5 {
            "<1 ms".to_owned()
        } else if self.ms.round() >= 1000.0 {
            format!("{:.1} s", self.ms / 1000.0)
        } else {
            format!("{} ms", self.ms.round() as i64)
        };
        let kind = if self.proxy || self.approximate {
            "Approximate"
        } else {
            "Exact"
        };
        format!("{kind} render \u{b7} {figure}")
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
    /// Redo moved the current state forward to an entry that already existed.
    Redid { label: String, sequence: u64 },
    /// A historical preview returned to the current entry.
    Returned { label: String, sequence: u64 },
}

impl Happened {
    /// What a new state says happened, from the state this desktop held before it. `known` is
    /// whether the new current entry was already among the loaded history rows, which is what
    /// tells a redo from a new entry. `None` when the current entry did not move.
    pub(crate) fn between(
        before: Option<&EditorState>,
        after: &EditorState,
        known: bool,
    ) -> Option<Self> {
        let entry = &after.current_entry;
        let Some(before) = before.filter(|before| before.asset.id == after.asset.id) else {
            return Some(Self::opened(after));
        };
        let previous = &before.current_entry;
        if previous.id == entry.id {
            return None;
        }
        Some(if entry.sequence < previous.sequence {
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
            Self::Redid { label, sequence } => format!("Redid {label} \u{b7} entry {sequence}"),
            Self::Returned { label, sequence } => {
                format!("Returned to entry {sequence} \u{b7} {label}")
            }
        }
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusBarModel {
    pub(crate) message: String,
    /// The three output codes under the pointer and their pixel, while the pointer is over the
    /// photograph; `None` otherwise. The view keeps a fixed slot for it either way, so nothing else
    /// in the bar moves as it comes and goes.
    pub(crate) readout: Option<String>,
    /// How many other clients are connected, or why the count is unknown.
    pub(crate) clients: String,
    /// Another client is connected, so the dot beside the count is lit.
    pub(crate) agents_connected: bool,
    /// What the renderer is doing, or what kind of frame is on screen and how long it took.
    pub(crate) render: String,
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
        readout: inputs.readout.map(histogram::readout_text),
        clients: clients_text(inputs.clients),
        agents_connected: inputs.clients.is_some_and(|count| count > 0),
        render: if inputs.rendering {
            "Rendering…".into()
        } else {
            match inputs.render {
                Some(time) => time.text(),
                None => "Idle".into(),
            }
        },
        view: view_text(
            &inputs.session.preview.view.zoom,
            title::effective_percent(inputs),
            inputs.scale_factor,
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
    fn the_render_time_names_the_kind_of_frame_it_describes() {
        let time = |ms: f64, proxy: bool, approximate: bool| RenderTime {
            ms,
            proxy,
            approximate,
        };
        assert_eq!(
            time(12.4, true, false).text(),
            "Approximate render \u{b7} 12 ms"
        );
        assert_eq!(time(85.5, false, false).text(), "Exact render \u{b7} 86 ms");
        // A tiny frame is not "0 ms".
        assert_eq!(
            time(0.2, true, false).text(),
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
            Happened::between(None, &opened, false),
            Some(Happened::Opened { file: file.clone() })
        );
        assert_eq!(
            Happened::Opened { file: file.clone() }.sentence(),
            format!("Opened {file}")
        );
        // The same current entry read again is not news.
        assert_eq!(Happened::between(Some(&opened), &opened, true), None);

        let mut clarity = entry(&asset, 7, Some(&original.id));
        clarity.label = "Clarity +18".into();
        clarity.actor = ACTOR.into();
        let applied = open_at(&asset, clarity);
        let mine = Happened::between(Some(&opened), &applied, false).expect("a change");
        assert_eq!(mine.sentence(), "Applied Clarity +18 \u{b7} entry 7");

        let mut theirs = applied.clone();
        theirs.current_entry.actor = "lw-assist".into();
        assert_eq!(
            Happened::between(Some(&opened), &theirs, false)
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
            Happened::between(Some(&applied), &opened, true)
                .expect("a change")
                .sentence(),
            "Undid Clarity +18"
        );
        assert_eq!(
            Happened::between(Some(&opened), &applied, true)
                .expect("a change")
                .sentence(),
            format!(
                "Redid Clarity +18 \u{b7} entry {}",
                applied.current_entry.sequence
            )
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
            Happened::between(Some(&opened), &other, false),
            Some(Happened::Opened { .. })
        ));
    }
}
