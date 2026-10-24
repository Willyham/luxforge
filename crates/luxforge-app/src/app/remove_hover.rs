//! Remove's hover picking (`docs/design/ai-editing.md`, "Picking: hover, click, box and brush"):
//! while Remove is chosen and the `ai.analysis` experiment offers the picker, the object under the
//! pointer is tinted as the selection would be.
//!
//! Every pointer move over the photograph is one `query.pick-object` an API client sends, with the
//! pointer as its `point`. One is in flight at a time: a move while it runs only replaces the
//! position wanted next, which is asked once the answer arrives, so a burst of moves asks for the
//! first and the last. The owner's query worker does the same with requests from every client. The
//! answer's chosen candidate carries its coverage over its bounds, at most
//! `PREVIEW_PIXELS` (65,536) pixels however large the photograph, which is decoded and tinted off
//! the update loop into one image the canvas draws over the bounds: Corrections' overlay treatment,
//! which takes no hue from the photograph, so it reads on an object of any colour — the coverage
//! washed white, inside a white outline over a translucent dark under-stroke.
//!
//! A pick answers about the photograph's current entry, so hover picks only while the current
//! state is on screen, as an edit needs. A pick of a photograph not analysed as it is now is
//! refused `not-ready` naming `task.analyse`; the desktop then runs that task at most once for the
//! photograph's revision (which names its current entry), whatever it answers, and asks again when
//! it ends. A pick refused again for the same revision is shown as refused, not analysed again.
//!
//! The pick, its answer's highlight and the analysis it asks for are the shared object picker's
//! ([`crate::object_pick`], [`super::object_pick`]); this keeps only Remove's own hover state.
use super::{
    Editor,
    decoded_handles::image_handle,
    message::{Message, remove::RemoveMessage},
    object_pick::analyse,
    tasks::{CallError, owner_task},
};
pub(crate) use super::object_pick::pick;
#[cfg(test)]
pub(crate) use super::object_pick::queue_analysis;
#[cfg(test)]
pub(crate) use crate::object_pick::highlight;
use crate::object_pick::needs_analysis;
pub(crate) use crate::object_pick::{
    HoverOverlay, PICK, Picked, pick_offered, point_query as pick_params,
};
use iced::Task;
use luxforge_core::{AssetId, DecodedPreview};
use serde_json::{Value, json};

/// Hover picking's bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct Hover {
    /// The position wanted next, in normalized content coordinates.
    want: Option<[f64; 2]>,
    /// The position whose pick is in flight.
    pub(crate) asking: Option<[f64; 2]>,
    /// The analysis in flight, by photograph and revision.
    pub(crate) analysing: Option<(AssetId, u64)>,
    /// The photograph and revision last analysed for the picker, whatever the analysis answered:
    /// not analysed again until it moves.
    analysed: Option<(AssetId, u64)>,
    /// Analyses run, for the evidence log and tests.
    pub(crate) analyses: u64,
    /// What the canvas draws.
    pub(crate) overlay: Option<HoverOverlay>,
    /// Picks answered, for the evidence log and tests.
    pub(crate) answered: u64,
    /// The last pick sent: what an API client sends for the same hover.
    pub(crate) last_request: Option<Value>,
}


impl Editor {
    /// Whether hover picking is on: Remove is chosen, the picker is offered and the current state,
    /// which a pick answers about, is on screen.
    pub(crate) fn hover_picking(&self) -> bool {
        self.remove_active()
            && pick_offered(&self.modules, self.settings.flags.as_ref())
            && crate::state::at_current(self.document.state.as_ref(), &self.session)
    }

    /// The photograph on screen and its revision.
    fn hover_target(&self) -> Option<(AssetId, u64)> {
        self.document
            .state
            .as_ref()
            .map(|state| (state.asset.id.clone(), state.revision))
    }

    /// What the evidence log records of hover picking: whether it is on, what is in flight, the
    /// overlay drawn and the last pick sent.
    pub(crate) fn hover_record(&self) -> Value {
        let hover = &self.remove_reads.hover;
        json!({
            "picking": self.hover_picking(),
            "asking": hover.asking,
            "analysing": hover.analysing.is_some(),
            "analyses": hover.analyses,
            "answered": hover.answered,
            "overlay": self.hover_overlay().map(|overlay| json!({"bounds": overlay.bounds})),
            "last_request": hover.last_request.as_ref().map(|params| json!({"method": PICK, "params": params})),
        })
    }

    /// Forget everything hover picking holds: Remove was left or the picker withdrawn.
    pub(crate) fn hover_reset(&mut self) {
        let hover = &mut self.remove_reads.hover;
        hover.want = None;
        hover.overlay = None;
    }

    /// The overlay the canvas draws now: only for the photograph and revision on screen, and not
    /// while a stroke is painted.
    pub(crate) fn hover_overlay(&self) -> Option<&HoverOverlay> {
        let (asset, revision) = self.hover_target()?;
        self.remove_reads
            .hover
            .overlay
            .as_ref()
            .filter(|overlay| overlay.asset == asset && overlay.revision == revision)
    }

    /// The pointer moved over the photograph (`None`: it left it).
    pub(crate) fn hover_moved(&mut self, point: Option<[f64; 2]>) -> Task<Message> {
        if !self.hover_picking() {
            return Task::none();
        }
        self.remove_reads.hover.want = point;
        if point.is_none() {
            self.remove_reads.hover.overlay = None;
            return Task::none();
        }
        self.hover_ask()
    }

    /// Ask for the wanted position unless a pick or an analysis is in flight.
    fn hover_ask(&mut self) -> Task<Message> {
        let hover = &self.remove_reads.hover;
        if hover.asking.is_some() || hover.analysing.is_some() {
            return Task::none();
        }
        let (Some(point), Some((asset, revision))) = (hover.want, self.hover_target()) else {
            return Task::none();
        };
        self.remove_reads.hover.asking = Some(point);
        let (owner, client) = (self.owner.clone(), self.client);
        let params = pick_params(&asset, point);
        self.remove_reads.hover.last_request = Some(params.clone());
        owner_task(
            move || pick(&owner, client, params),
            move |result| {
                Message::Remove(RemoveMessage::Picked {
                    asset,
                    revision,
                    point,
                    result,
                })
            },
        )
    }

    /// A pick answered.
    pub(crate) fn hover_picked(
        &mut self,
        asset: AssetId,
        revision: u64,
        point: [f64; 2],
        result: Result<Box<Picked>, CallError>,
    ) -> Task<Message> {
        self.remove_reads.hover.asking = None;
        let current = self.hover_target() == Some((asset.clone(), revision));
        match result {
            Ok(picked) if current => {
                self.remove_reads.hover.answered += 1;
                self.event("remove_hover_picked", || {
                    json!({"point": point, "decision": picked.decision, "bounds": picked.bounds})
                });
                self.remove_reads.hover.overlay = match (picked.bounds, picked.image) {
                    (Some(bounds), Some((width, height, rgba))) => Some(HoverOverlay {
                        asset,
                        revision,
                        bounds,
                        handle: image_handle(DecodedPreview {
                            width,
                            height,
                            rgba,
                        }),
                    }),
                    _ => None,
                };
            }
            Ok(_) => {}
            Err(error)
                if current
                    && needs_analysis(&error)
                    && self.remove_reads.hover.analysed != Some((asset.clone(), revision)) =>
            {
                return self.hover_analyse(asset, revision);
            }
            Err(error) => {
                self.remove_reads.hover.overlay = None;
                if error.code != "cancelled" {
                    self.event(
                        "remove_hover_refused",
                        || json!({"code": error.code, "message": error.message}),
                    );
                }
            }
        }
        if self.remove_reads.hover.want != Some(point) && self.hover_picking() {
            return self.hover_ask();
        }
        Task::none()
    }

    /// Analyse the photograph for the picker, at most once per revision whatever it answers.
    fn hover_analyse(&mut self, asset: AssetId, revision: u64) -> Task<Message> {
        let key = (asset.clone(), revision);
        let hover = &mut self.remove_reads.hover;
        if hover.analysed.as_ref() == Some(&key) || hover.analysing.is_some() {
            return Task::none();
        }
        hover.analysed = Some(key.clone());
        hover.analyses += 1;
        hover.analysing = Some(key);
        self.event("remove_hover_analyse", || json!({"asset_id": asset}));
        let (owner, client) = (self.owner.clone(), self.client);
        Task::perform(analyse(owner, client, asset.clone()), move |result| {
            Message::Remove(RemoveMessage::Analysed {
                asset,
                revision,
                result,
            })
        })
    }

    /// The analysis ended.
    pub(crate) fn hover_analysed(
        &mut self,
        asset: AssetId,
        revision: u64,
        result: Result<(), String>,
    ) -> Task<Message> {
        self.remove_reads.hover.analysing = None;
        self.event(
            "remove_hover_analysed",
            || json!({"asset_id": asset, "revision": revision, "ok": result.is_ok(), "error": result.as_ref().err()}),
        );
        if result.is_err() {
            return Task::none();
        }
        if self.hover_picking() {
            return self.hover_ask();
        }
        Task::none()
    }
}
