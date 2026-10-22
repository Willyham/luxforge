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
//! the update loop into one image the canvas draws over the bounds.
//!
//! A pick of a photograph not analysed as it is now is refused `not-ready` naming `task.analyse`;
//! the desktop then runs that task once for the photograph's revision and asks again when it ends.
//! An analysis that fails is not asked again until the photograph moves.
use super::{
    Editor,
    message::{Message, remove::RemoveMessage},
    remove::decode_base64,
    tasks::{self, CallError, owner_task},
};
use iced::{Task, widget::image::Handle};
use luxforge_core::{AssetId, ClientId, OwnerHandle};
use serde_json::{Value, json};

/// `query.pick-object`.
pub(crate) const PICK: &str = "query.pick-object";
/// `task.analyse`.
pub(crate) const ANALYSE: &str = "task.analyse";
/// The picker's query and the analysis task, without their prefixes.
const PICK_QUERY: &str = "pick-object";
const ANALYSE_TASK: &str = "analyse";
/// The tint of a hovered object: the selection's, at full coverage.
const TINT: [u8; 3] = [255, 69, 69];
const TINT_ALPHA: f32 = 0.38;
/// The most times one analysis waits through a source preparation.
const PREPARATION_RETRIES: usize = 4;

/// The hovered object as the canvas draws it: an image over its bounds in normalized content
/// coordinates, for the photograph and revision it was picked on.
#[derive(Clone, Debug)]
pub(crate) struct HoverOverlay {
    pub(crate) asset: AssetId,
    pub(crate) revision: u64,
    /// `[x0, y0, x1, y1]`.
    pub(crate) bounds: [f64; 4],
    pub(crate) handle: Handle,
}

/// One pick's answer, decoded off the update loop: the overlay of its chosen candidate, or none
/// when it chose nothing.
#[derive(Clone, Debug)]
pub(crate) struct Picked {
    pub(crate) decision: String,
    pub(crate) bounds: Option<[f64; 4]>,
    pub(crate) image: Option<(u32, u32, Vec<u8>)>,
}

/// Hover picking's bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct Hover {
    /// The position wanted next, in normalized content coordinates.
    want: Option<[f64; 2]>,
    /// The position whose pick is in flight.
    pub(crate) asking: Option<[f64; 2]>,
    /// The analysis in flight, by photograph and revision.
    pub(crate) analysing: Option<(AssetId, u64)>,
    /// The photograph and revision whose analysis failed, not asked again until it moves.
    refused: Option<(AssetId, u64)>,
    /// What the canvas draws.
    pub(crate) overlay: Option<HoverOverlay>,
    /// Picks answered, for the evidence log and tests.
    pub(crate) answered: u64,
    /// The last pick sent: what an API client sends for the same hover.
    pub(crate) last_request: Option<Value>,
}

/// The module that offers the picker: one declaring `query.pick-object` whose analysis task is
/// offered under the flags as they are.
pub(crate) fn pick_offered(
    modules: &[luxforge_core::ModuleDescriptor],
    flags: Option<&luxforge_core::flags::FlagList>,
) -> bool {
    modules.iter().filter(|m| m.is_available()).any(|module| {
        module.queries.iter().any(|query| query.id == PICK_QUERY)
            && module
                .task(ANALYSE_TASK)
                .is_some_and(|task| crate::state::settings::task_offered(flags, task))
    })
}

/// The answer of `query.pick-object` as the overlay it draws: the first chosen candidate's bounds
/// and its coverage tinted. `O(preview pixels)`.
pub(crate) fn picked(answer: &Value) -> Result<Picked, String> {
    let decision = answer["decision"].as_str().unwrap_or("nothing").to_owned();
    let chosen = answer["chosen"][0].as_u64().map(|index| index as usize);
    let Some(candidate) = chosen.map(|index| &answer["candidates"][index]) else {
        return Ok(Picked {
            decision,
            bounds: None,
            image: None,
        });
    };
    let bounds = candidate["bounds"].as_array().and_then(|bounds| {
        let values: Vec<f64> = bounds.iter().filter_map(Value::as_f64).collect();
        <[f64; 4]>::try_from(values).ok()
    });
    let preview = &candidate["preview"];
    let (Some(width), Some(height), Some(codes)) = (
        preview["width"].as_u64(),
        preview["height"].as_u64(),
        preview["codes"].as_str(),
    ) else {
        return Ok(Picked {
            decision,
            bounds,
            image: None,
        });
    };
    let codes = decode_base64(codes)?;
    if codes.len() as u64 != width * height {
        return Err(format!(
            "a {width} x {height} preview carried {} codes",
            codes.len()
        ));
    }
    let rgba = codes
        .iter()
        .flat_map(|&code| {
            let alpha = (f32::from(code) * TINT_ALPHA).round() as u8;
            [TINT[0], TINT[1], TINT[2], alpha]
        })
        .collect();
    Ok(Picked {
        decision,
        bounds,
        image: Some((width as u32, height as u32, rgba)),
    })
}

/// Queue `task.analyse` on `asset`, waiting through any source preparation its read asks for: its
/// job. Blocks: run it off the update loop.
pub(crate) fn queue_analysis(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &AssetId,
) -> Result<String, String> {
    let params = json!({"asset_id": asset, "mutation": tasks::request()});
    for _ in 0..PREPARATION_RETRIES {
        match tasks::call_detailed(owner, client, ANALYSE, params.clone()) {
            Ok(queued) => {
                return queued["job_id"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "task.analyse named no job".to_owned());
            }
            Err(error) if error.code == "preparation-required" => {
                let job = error.job_id.ok_or("a preparation names its job")?;
                tasks::wait_source_job(owner, client, &job).map_err(|e| e.message)?;
            }
            Err(error) => return Err(error.message),
        }
    }
    Err("the analysis kept asking for preparation".into())
}

/// Run `task.analyse` on `asset` and await its job's end through `job.wait`: `Ok` when it is
/// ready.
async fn analyse(owner: OwnerHandle, client: ClientId, asset: AssetId) -> Result<(), String> {
    let job = tasks::off_loop({
        let owner = owner.clone();
        move || queue_analysis(&owner, client, &asset)
    })
    .await?;
    let mut after = None;
    loop {
        let (change, record) = super::job_reads::wait(&owner, client, &job, after).await?;
        match record["status"].as_str() {
            Some("ready") => return Ok(()),
            Some("queued" | "running") => after = Some(change),
            _ => {
                return Err(record["error"]["message"]
                    .as_str()
                    .unwrap_or("the analysis did not finish")
                    .to_owned());
            }
        }
    }
}

/// The `query.pick-object` a hover at `point` sends: what an API client sends for it.
pub(crate) fn pick_params(asset: &AssetId, point: [f64; 2]) -> Value {
    json!({"asset_id": asset, "point": [point]})
}

/// Send one pick and decode its answer. Blocks: run it off the update loop.
pub(crate) fn pick(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<Box<Picked>, CallError> {
    let answer = tasks::call_detailed(owner, client, PICK, params)?;
    picked(&answer).map(Box::new).map_err(|message| CallError {
        code: "internal".into(),
        message,
        data: None,
        job_id: None,
    })
}

/// Whether a refusal names the analysis the pick needs.
fn needs_analysis(error: &CallError) -> bool {
    error.code == "not-ready"
        && error
            .data
            .as_ref()
            .and_then(|data| data["requirements"].as_array())
            .is_some_and(|list| list.iter().any(|r| r["kind"] == "analysis"))
}

impl Editor {
    /// Whether hover picking is on: Remove is chosen and the picker is offered.
    pub(crate) fn hover_picking(&self) -> bool {
        self.remove_active() && pick_offered(&self.modules, self.settings.flags.as_ref())
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
            "refused": hover.refused.is_some(),
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
                        handle: Handle::from_rgba(width, height, rgba),
                    }),
                    _ => None,
                };
            }
            Ok(_) => {}
            Err(error) if current && needs_analysis(&error) => {
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

    /// Analyse the photograph for the picker, once per revision.
    fn hover_analyse(&mut self, asset: AssetId, revision: u64) -> Task<Message> {
        let key = (asset.clone(), revision);
        let hover = &mut self.remove_reads.hover;
        if hover.refused.as_ref() == Some(&key) || hover.analysing.is_some() {
            return Task::none();
        }
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
            || json!({"ok": result.is_ok(), "error": result.as_ref().err()}),
        );
        if result.is_err() {
            self.remove_reads.hover.refused = Some((asset, revision));
            return Task::none();
        }
        if self.hover_picking() {
            return self.hover_ask();
        }
        Task::none()
    }
}
