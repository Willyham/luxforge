//! The object picker's controller ([`crate::object_pick`]): arming a session for a tool, the hover,
//! the click and the loose drag, the pins, the analysis and the model loads on entering, the
//! bounded queue of `task.select-object` requests and the setup hook a tool opens when its models
//! are missing.
//!
//! **One pick in flight.** A hover's pick and a loose stroke's share the desktop's one pick: a
//! move while it runs only replaces the position wanted next, and a stroke released while a hover
//! is answered waits for it. The owner's query worker does the same across every client.
//!
//! **Selections one at a time.** Each chosen object is one `task.select-object` on the module
//! lane, sent once nothing of this desktop's is in flight and the previous selection's commit has
//! landed; the rest wait in a queue of at most [`crate::object_pick::SELECTION_LIMIT`]. What the
//! published artifact becomes is the tool's ([`SelectFor`]): Masks › Object commits it through
//! [`Editor::mask_object_selected`], a model row's Recompute through
//! [`Editor::mask_recomputed`].
//!
//! **Off the loop.** Every owner call runs through [`owner_task`] or [`Task::perform`]; a pick's
//! preview is decoded and highlighted on the blocking pool, so the update loop only wraps the
//! decoded pixels in an image handle.
use super::{
    Editor,
    decoded_handles::image_handle,
    message::{
        Message,
        object_pick::{ObjectPickMessage, ObjectPointer},
    },
    tasks::{self, CallError, owner_task},
};
use crate::{
    brush::ContentMap,
    object_pick::{
        self as pick, ANALYSE, Decision, Found, HoverOverlay, NOTHING_FOUND,
        NOTHING_TO_SELECT, ObjectPick, PICK, Picked, Pin, Pins, PickUse, RUNTIME_LOAD, SELECT,
        SelectFor,
    },
    state::capabilities::Operation,
};
use iced::Task;
use luxforge_core::{
    AssetId, ClientId, DecodedPreview, OwnerHandle, capabilities::resources::ResourceState,
    mask::ObjectPrompt,
};
use serde_json::{Value, json};

/// The most times one request waits through a source preparation.
const PREPARATION_RETRIES: usize = 4;

/// A setup a tool opens when its models are missing, by the id its module declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupId {
    /// Masks › Object: the object finder.
    MaskObject,
}

impl SetupId {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MaskObject => "mask-object",
        }
    }
}

/// Whether a setup's resources are all installed, as the module's status last read them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SetupReady {
    /// Every resource of every part is installed.
    Installed,
    /// This resource of the setup is not installed: the first in declared order.
    Missing { module: String, resource: String },
    /// The module's status has not been read yet.
    Unread { module: String },
    /// No available module declares the setup.
    Undeclared,
}

// ---- owner work ----------------------------------------------------------------------------------

/// Queue `task.analyse` on `asset`, waiting through any source preparation its read asks for: its
/// job. Blocks: run it off the update loop.
pub(crate) fn queue_analysis(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &AssetId,
) -> Result<String, String> {
    queue_task(
        owner,
        client,
        ANALYSE,
        json!({"asset_id": asset, "mutation": tasks::request()}),
    )
}

/// Queue one module task, waiting through any source preparation its read asks for: its job.
/// Blocks: run it off the update loop.
fn queue_task(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<String, String> {
    for _ in 0..PREPARATION_RETRIES {
        match tasks::call_detailed(owner, client, method, params.clone()) {
            Ok(queued) => {
                return queued["job_id"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{method} named no job"));
            }
            Err(error) if error.code == "preparation-required" => {
                let job = error.job_id.ok_or("a preparation names its job")?;
                tasks::wait_source_job(owner, client, &job).map_err(|e| e.message)?;
            }
            Err(error) => return Err(error.message),
        }
    }
    Err(format!("{method} kept asking for preparation"))
}

/// Await one job's end through `job.wait`: its record's task result when it is ready.
async fn finished(owner: &OwnerHandle, client: ClientId, job: &str) -> Result<Value, String> {
    let mut after = None;
    loop {
        let (change, record) = super::job_reads::wait(owner, client, job, after).await?;
        match record["status"].as_str() {
            Some("ready") => return Ok(record["result"]["result"].clone()),
            Some("queued" | "running") => after = Some(change),
            _ => {
                return Err(record["error"]["message"]
                    .as_str()
                    .unwrap_or("the job did not finish")
                    .to_owned());
            }
        }
    }
}

/// Run `task.analyse` on `asset` and await its job's end: `Ok` when it is ready.
pub(crate) async fn analyse(
    owner: OwnerHandle,
    client: ClientId,
    asset: AssetId,
) -> Result<(), String> {
    let job = tasks::off_loop({
        let owner = owner.clone();
        move || queue_analysis(&owner, client, &asset)
    })
    .await?;
    finished(&owner, client, &job).await.map(|_| ())
}

/// Run one `task.select-object` and await its job's end: the selection it published.
pub(crate) async fn select(
    owner: OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<Value, String> {
    let job = tasks::off_loop({
        let owner = owner.clone();
        move || queue_task(&owner, client, SELECT, params)
    })
    .await?;
    finished(&owner, client, &job).await
}

fn internal(message: String) -> CallError {
    CallError {
        code: "internal".into(),
        message,
        data: None,
        job_id: None,
    }
}

/// Send one point pick and decode its answer. Blocks: run it off the update loop.
pub(crate) fn pick(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<Box<Picked>, CallError> {
    let answer = tasks::call_detailed(owner, client, PICK, params)?;
    pick::picked(&answer).map(Box::new).map_err(internal)
}

/// Send one stroke pick and decode its answer. Blocks: run it off the update loop.
pub(crate) fn find(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<Box<Found>, CallError> {
    let answer = tasks::call_detailed(owner, client, PICK, params)?;
    pick::found(&answer).map(Box::new).map_err(internal)
}

fn handle((width, height, rgba): (u32, u32, Vec<u8>)) -> iced::widget::image::Handle {
    image_handle(DecodedPreview {
        width,
        height,
        rgba,
    })
}

impl Editor {
    // ---- the setup hook --------------------------------------------------------------------------

    /// Whether `setup` is installed, from the status the desktop last read of the module that
    /// declares it.
    pub(crate) fn setup_ready(&self, setup: SetupId) -> SetupReady {
        let Some((module, declared)) = self
            .modules
            .iter()
            .filter(|module| module.is_available())
            .find_map(|module| Some((module, module.setup(setup.as_str())?)))
        else {
            return SetupReady::Undeclared;
        };
        let Some(status) = &self.capabilities.module(&module.id).status else {
            return SetupReady::Unread {
                module: module.id.clone(),
            };
        };
        match declared.resources().find(|resource| {
            !status
                .resources
                .iter()
                .any(|row| row.id == *resource && row.state == ResourceState::Installed)
        }) {
            None => SetupReady::Installed,
            Some(resource) => SetupReady::Missing {
                module: module.id.clone(),
                resource: resource.to_owned(),
            },
        }
    }

    /// The setup hook: open the setup sheet for what `setup` is missing.
    ///
    /// The delivered sheet opens when a download the desktop started is refused for want of
    /// consent and its resource's setup introduces it, so this starts the first missing resource's
    /// install exactly as the capability block's own Download does; Allow on the sheet grants that
    /// scope and retries it. Each further missing resource is set up by choosing the tool again.
    pub(crate) fn open_setup(&mut self, setup: SetupId) -> Task<Message> {
        match self.setup_ready(setup) {
            SetupReady::Missing { module, resource } => {
                self.event(
                    "setup_opened",
                    || json!({"setup": setup.as_str(), "module": module, "resource": resource}),
                );
                self.capability_op(&module, Operation::Install { resource })
            }
            SetupReady::Unread { module } => {
                if self.capabilities.module(&module).pending > 0 {
                    return Task::none();
                }
                self.capability_op(&module, Operation::Refresh)
            }
            SetupReady::Installed | SetupReady::Undeclared => Task::none(),
        }
    }

    // ---- the session -----------------------------------------------------------------------------

    /// The module whose `task.select-object` the run offers now, with the picker beside it.
    pub(crate) fn object_select_module(&self) -> Option<&luxforge_core::ModuleDescriptor> {
        pick::select_module(&self.modules, self.settings.flags.as_ref())
    }

    /// The photograph on screen and its revision.
    fn object_target(&self) -> Option<(AssetId, u64)> {
        self.document
            .state
            .as_ref()
            .map(|state| (state.asset.id.clone(), state.revision))
    }

    /// Arm a picking session for `purpose` on the photograph on screen, at `radius` content
    /// heights for a loose stroke: load the setup's models and analyse the photograph at once, so
    /// hover is ready when the pointer arrives. It changes no mode and sends no edit.
    pub(crate) fn enter_object_pick(
        &mut self,
        purpose: PickUse,
        setup: SetupId,
        radius: f64,
    ) -> Task<Message> {
        let Some((asset, revision)) = self.object_target() else {
            return Task::none();
        };
        let Some(module) = self.object_select_module().map(|module| module.id.clone()) else {
            return Task::none();
        };
        let resources: Vec<String> = self
            .modules
            .iter()
            .find(|descriptor| descriptor.id == module)
            .and_then(|descriptor| descriptor.setup(setup.as_str()))
            .map(|declared| declared.resources().map(str::to_owned).collect())
            .unwrap_or_default();
        let mut session = ObjectPick::new(purpose, asset.clone(), radius);
        session.loads = resources.clone();
        self.object_pick = Some(session);
        self.event("object_pick_entered", || {
            json!({"purpose": purpose.as_str(), "asset_id": asset, "loads": resources})
        });
        let mut tasks: Vec<Task<Message>> = resources
            .into_iter()
            .map(|resource| {
                let (owner, client) = (self.owner.clone(), self.client);
                let params = json!({"module_id": module, "resource_id": resource});
                owner_task(
                    move || {
                        tasks::call_detailed(&owner, client, RUNTIME_LOAD, params)
                            .map_err(|error| error.message)
                    },
                    move |result| {
                        Message::ObjectPick(ObjectPickMessage::Loaded { resource, result })
                    },
                )
            })
            .collect();
        tasks.push(self.object_analyse(asset, revision));
        Task::batch(tasks)
    }

    /// End the session, saying why in the evidence log: a press in progress, its pins and the
    /// selections still waiting for its tool go with it. A selection already running finishes, and
    /// its tool, having left, leaves its artifact unreferenced for retention to collect.
    pub(crate) fn leave_object_pick(&mut self, reason: &'static str) {
        let Some(session) = self.object_pick.take() else {
            return;
        };
        let dropped = self.object_selects.drop_for(session.purpose);
        self.event("object_pick_left", || {
            json!({"purpose": session.purpose.as_str(), "reason": reason, "dropped": dropped})
        });
        match session.purpose {
            PickUse::Mask => self.mask_object_left(),
        }
    }

    /// Why the session has to end after this message, or `None` while it stays.
    fn object_pick_ended(&self, session: &ObjectPick) -> Option<&'static str> {
        if self.document.state.as_ref().map(|state| &state.asset.id) != Some(&session.asset) {
            return Some("photograph");
        }
        // The experiment turned off: as Escape.
        if self.object_select_module().is_none() {
            return Some("flag");
        }
        if self.document.compare_return.is_some()
            || self.compare_key.is_down()
            || self.presentation.compare_after.is_some()
        {
            return Some("compare");
        }
        match session.purpose {
            PickUse::Mask => self.mask_object_ended(),
        }
    }

    /// The session follows the stack it picks on, once per update: it ends when its tool says so,
    /// its content map is asked for again when the displayed entry moves, a pick waiting is sent
    /// and the selection queue moves on.
    pub(crate) fn follow_object_pick(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if let Some(session) = &self.object_pick {
            if let Some(reason) = self.object_pick_ended(session) {
                self.leave_object_pick(reason);
            } else {
                let entry = self.displayed_entry();
                if session.entry != entry || (session.map.is_none() && session.request.is_none())
                {
                    let asset = session.asset.clone();
                    let id = self.next_gesture();
                    let session = self.object_pick.as_mut().expect("the armed session");
                    session.entry = entry.clone();
                    session.request = Some(id);
                    tasks.push(tasks::transform_task_to(
                        self.owner.clone(),
                        self.client,
                        id,
                        asset,
                        entry,
                        |gesture, result| {
                            Message::ObjectPick(ObjectPickMessage::Transform(gesture, result))
                        },
                    ));
                }
                tasks.push(self.object_ask());
            }
        }
        tasks.push(self.flush_selects());
        Task::batch(tasks)
    }

    /// Whether the session picks now: armed, and the current state, which a pick answers about, on
    /// screen.
    pub(crate) fn object_picking(&self) -> bool {
        self.object_pick.is_some()
            && crate::state::at_current(self.document.state.as_ref(), &self.session)
    }

    /// The hovered object the canvas draws now: only for the photograph and revision on screen.
    pub(crate) fn object_overlay(&self) -> Option<&HoverOverlay> {
        let (asset, revision) = self.object_target()?;
        self.object_pick
            .as_ref()?
            .overlay
            .as_ref()
            .filter(|overlay| overlay.asset == asset && overlay.revision == revision)
    }

    /// Send the pick waiting, unless one or an analysis is in flight: a released stroke's first,
    /// then the hover position wanted.
    fn object_ask(&mut self) -> Task<Message> {
        if !self.object_picking() {
            return Task::none();
        }
        let Some((asset, revision)) = self.object_target() else {
            return Task::none();
        };
        let session = self.object_pick.as_mut().expect("the armed session");
        if session.picking() || session.analysing.is_some() {
            return Task::none();
        }
        let (owner, client) = (self.owner.clone(), self.client);
        if let Some((stroke, sent)) = &mut session.finding {
            *sent = true;
            let stroke = stroke.clone();
            let params = pick::stroke_query(&asset, &stroke, session.radius);
            session.last_request = Some(params.clone());
            return owner_task(
                move || find(&owner, client, params),
                move |result| {
                    Message::ObjectPick(ObjectPickMessage::Found {
                        asset,
                        revision,
                        stroke,
                        result,
                    })
                },
            );
        }
        // No hover while a press is down or pins wait for an answer: the outline would move under
        // the pins, and a drag's dashed stroke is what the canvas shows.
        if session.press.is_some() || session.pins.is_some() {
            return Task::none();
        }
        let Some(point) = session.want else {
            return Task::none();
        };
        session.asking = Some(point);
        let params = pick::point_query(&asset, point);
        session.last_request = Some(params.clone());
        owner_task(
            move || pick(&owner, client, params),
            move |result| {
                Message::ObjectPick(ObjectPickMessage::Hovered {
                    asset,
                    revision,
                    point,
                    result,
                })
            },
        )
    }

    /// Analyse the photograph for the picker, at most once per revision whatever it answers.
    fn object_analyse(&mut self, asset: AssetId, revision: u64) -> Task<Message> {
        let key = (asset.clone(), revision);
        let Some(session) = self.object_pick.as_mut() else {
            return Task::none();
        };
        if session.analysed.as_ref() == Some(&key) || session.analysing.is_some() {
            return Task::none();
        }
        session.analysed = Some(key.clone());
        session.analyses += 1;
        session.analysing = Some(key);
        self.event("object_pick_analyse", || json!({"asset_id": asset}));
        let (owner, client) = (self.owner.clone(), self.client);
        Task::perform(analyse(owner, client, asset.clone()), move |result| {
            Message::ObjectPick(ObjectPickMessage::Analysed {
                asset,
                revision,
                result,
            })
        })
    }

    /// Put `prompt` in the selection queue for the session's tool: refused in the status bar when
    /// the queue is full.
    fn object_enqueue(&mut self, prompt: ObjectPrompt) -> bool {
        let Some(session) = &self.object_pick else {
            return false;
        };
        if !self.object_selects.room() {
            self.status.text = format!(
                "At most {} objects are selected at a time",
                pick::SELECTION_LIMIT
            );
            return false;
        }
        let (purpose, asset) = (session.purpose, session.asset.clone());
        self.event("object_pick_queued", || {
            json!({"purpose": purpose.as_str(), "prompt": prompt.request_fields()})
        });
        self.object_selects
            .queued
            .push_back((SelectFor::Pick(purpose), asset, prompt));
        true
    }

    /// Queue a model row's Recompute: its stored prompt selected again on the photograph on screen.
    pub(crate) fn queue_recompute(&mut self, purpose: SelectFor, prompt: ObjectPrompt) -> bool {
        let Some((asset, _)) = self.object_target() else {
            return false;
        };
        if !self.object_selects.room() {
            self.status.text = format!(
                "At most {} objects are selected at a time",
                pick::SELECTION_LIMIT
            );
            return false;
        }
        self.object_selects
            .queued
            .push_back((purpose, asset, prompt));
        true
    }

    /// Move the selection queue on: commit an answered selection once the desktop's request slot is
    /// free, then send the next waiting one once nothing of this desktop's is in flight.
    pub(crate) fn flush_selects(&mut self) -> Task<Message> {
        let free = !self.busy && !self.mask_panel.command_in_flight;
        if !free {
            return Task::none();
        }
        if let Some((purpose, selection)) = self.object_selects.answered.take() {
            return match purpose {
                SelectFor::Pick(PickUse::Mask) => self.mask_object_selected(selection),
                SelectFor::Recompute { mask, component } => {
                    self.mask_recomputed(mask, component, selection)
                }
            };
        }
        if self.object_selects.running.is_some() {
            return Task::none();
        }
        let Some((purpose, asset, prompt)) = self.object_selects.queued.pop_front() else {
            return Task::none();
        };
        // A selection of another photograph than the one on screen is not this view's to make.
        if self.object_target().map(|(on, _)| on) != Some(asset.clone()) {
            return Task::none();
        }
        let params = pick::select_request(&asset, &prompt, json!(tasks::request()));
        self.object_selects.last_request = Some(params.clone());
        self.object_selects.running = Some((purpose.clone(), params.clone()));
        self.object_selects.sent += 1;
        self.event("object_select", || json!({"method": SELECT, "params": params}));
        let (owner, client) = (self.owner.clone(), self.client);
        Task::perform(select(owner, client, params), move |result| {
            Message::ObjectPick(ObjectPickMessage::Selected { purpose, result })
        })
    }

    /// The press in progress, or the stroke a pick is finding, as the canvas draws it.
    fn object_stroke(&self) -> Option<&[[f64; 2]]> {
        let session = self.object_pick.as_ref()?;
        session
            .press
            .as_ref()
            .filter(|press| press.dragging)
            .map(|press| press.points.as_slice())
            .or_else(|| session.finding.as_ref().map(|(stroke, _)| stroke.as_slice()))
            .or(session.nothing.as_deref())
            .or_else(|| session.pins.as_ref().map(|pins| pins.stroke.as_slice()))
    }

    /// What the picking canvas is drawn from, while a session is armed and its map has arrived.
    pub(crate) fn object_pick_canvas(&self) -> Option<crate::view::ObjectPickData<'_>> {
        let session = self.object_pick.as_ref()?;
        let shown = match session.purpose {
            PickUse::Mask => self.mask_mode_active() && self.drawn_mask().is_none(),
        };
        if !shown {
            return None;
        }
        Some(crate::view::ObjectPickData {
            map: session.map.as_ref()?,
            overlay: self.object_overlay(),
            stroke: self.object_stroke(),
            radius: session.radius,
            nothing: session.nothing.is_some(),
            pins: session.pins.as_ref().map(|pins| pins.pins.as_slice()),
        })
    }

    /// What a captured frame records of object picking.
    pub(crate) fn object_pick_record(&self) -> Value {
        json!({
            "session": self.object_pick.as_ref().map(ObjectPick::summary),
            "picking": self.object_picking(),
            "finding_objects": self.object_pick.as_ref().is_some_and(ObjectPick::finding_objects),
            "selects": self.object_selects.summary(),
        })
    }

    /// Nothing object picking started is in flight or waiting: no pick, analysis, load, selection
    /// or commit.
    pub(crate) fn object_pick_idle(&self) -> bool {
        let session = self.object_pick.as_ref().is_none_or(|session| {
            !session.picking()
                && session.analysing.is_none()
                && session.request.is_none()
                && session.loads.is_empty()
                && session.finding.is_none()
                && session.press.is_none()
        });
        session && self.object_selects.idle()
    }

    // ---- messages --------------------------------------------------------------------------------

    pub(crate) fn object_pick_message(&mut self, message: ObjectPickMessage) -> Task<Message> {
        match message {
            ObjectPickMessage::Transform(gesture, result) => {
                if let Some(session) = self
                    .object_pick
                    .as_mut()
                    .filter(|session| session.request == Some(gesture))
                {
                    session.request = None;
                    session.map = match result {
                        Ok(transform) => ContentMap::from_descriptor(transform),
                        Err(error) => {
                            self.status.text = format!("Picking unavailable: {error}");
                            None
                        }
                    };
                }
                Task::none()
            }
            ObjectPickMessage::Hover(point) => {
                let Some(session) = self.object_pick.as_mut() else {
                    return Task::none();
                };
                session.want = point;
                if point.is_none() {
                    session.overlay = None;
                    return Task::none();
                }
                self.object_ask()
            }
            ObjectPickMessage::Hovered {
                asset,
                revision,
                point,
                result,
            } => self.object_hovered(asset, revision, point, result),
            ObjectPickMessage::Analysed {
                asset,
                revision,
                result,
            } => {
                if let Some(session) = self.object_pick.as_mut() {
                    session.analysing = None;
                    session.analysis_error = result.as_ref().err().cloned();
                }
                self.event("object_pick_analysed", || {
                    json!({"asset_id": asset, "revision": revision, "ok": result.is_ok(),
                           "error": result.as_ref().err()})
                });
                if let Err(error) = result {
                    self.status.text = format!("Finding objects failed: {error}");
                    return Task::none();
                }
                self.object_ask()
            }
            ObjectPickMessage::Loaded { resource, result } => {
                if let Some(session) = self.object_pick.as_mut() {
                    session.loads.retain(|held| held != &resource);
                }
                self.event("object_pick_loaded", || {
                    json!({"resource": resource, "ok": result.is_ok(),
                           "state": result.as_ref().ok().map(|answer| answer["state"].clone()),
                           "error": result.as_ref().err()})
                });
                Task::none()
            }
            ObjectPickMessage::Pointer(pointer) => self.object_pointer(pointer),
            ObjectPickMessage::Found {
                asset,
                revision,
                stroke,
                result,
            } => self.object_found(asset, revision, stroke, result),
            ObjectPickMessage::Pin(index) => {
                if let Some(pins) = self
                    .object_pick
                    .as_mut()
                    .and_then(|session| session.pins.as_mut())
                {
                    pins.toggle(index);
                }
                Task::none()
            }
            ObjectPickMessage::Use => self.object_use(),
            ObjectPickMessage::Escape => {
                let Some(session) = self.object_pick.as_mut() else {
                    return Task::none();
                };
                if session.press.take().is_some() {
                    return Task::none();
                }
                if session.pins.take().is_some() || session.nothing.take().is_some() {
                    return Task::none();
                }
                self.leave_object_pick("escape");
                Task::none()
            }
            ObjectPickMessage::Done => {
                if self
                    .object_pick
                    .as_ref()
                    .is_some_and(|session| session.pins.is_some())
                {
                    return self.object_use();
                }
                self.leave_object_pick("done");
                Task::none()
            }
            ObjectPickMessage::Selected { purpose, result } => {
                self.object_selects.running = None;
                self.object_selects.finished += 1;
                match result {
                    Ok(selection) => {
                        self.event("object_selected", || {
                            json!({"for": purpose_name(&purpose), "artifact": selection["artifact"],
                                   "prompt": selection["prompt"]})
                        });
                        // A tool that left while its selection ran has nothing to commit it to.
                        let wanted = match &purpose {
                            SelectFor::Pick(use_) => self
                                .object_pick
                                .as_ref()
                                .is_some_and(|session| session.purpose == *use_),
                            SelectFor::Recompute { .. } => true,
                        };
                        if wanted {
                            self.object_selects.answered = Some((purpose, selection));
                        }
                    }
                    Err(error) => {
                        self.status.text = format!("Selecting the object failed: {error}");
                        self.event("object_select_failed", || json!({"error": error}));
                    }
                }
                self.flush_selects()
            }
        }
    }

    /// A hover's pick answered.
    fn object_hovered(
        &mut self,
        asset: AssetId,
        revision: u64,
        point: [f64; 2],
        result: Result<Box<Picked>, CallError>,
    ) -> Task<Message> {
        let current = self.object_target() == Some((asset.clone(), revision));
        let Some(session) = self.object_pick.as_mut() else {
            return Task::none();
        };
        session.asking = None;
        match result {
            Ok(picked) if current => {
                session.answered += 1;
                session.overlay = match (picked.bounds, picked.image) {
                    (Some(bounds), Some(image)) => Some(HoverOverlay {
                        asset,
                        revision,
                        bounds,
                        handle: handle(image),
                    }),
                    _ => None,
                };
                self.event("object_pick_hovered", || {
                    json!({"point": point, "decision": picked.decision, "bounds": picked.bounds})
                });
            }
            Ok(_) => {}
            Err(error)
                if current
                    && pick::needs_analysis(&error)
                    && session.analysed != Some((asset.clone(), revision)) =>
            {
                return self.object_analyse(asset, revision);
            }
            Err(error) => {
                session.overlay = None;
                if error.code != "cancelled" {
                    self.event(
                        "object_pick_refused",
                        || json!({"code": error.code, "message": error.message}),
                    );
                }
            }
        }
        self.object_ask()
    }

    /// One pointer step of a press on the photograph.
    fn object_pointer(&mut self, pointer: ObjectPointer) -> Task<Message> {
        let Some(session) = self.object_pick.as_mut() else {
            return Task::none();
        };
        match pointer {
            ObjectPointer::Press { view, at } => {
                // A press away from the pins or the Nothing notice dismisses them: the next
                // gesture starts afresh.
                session.pins = None;
                session.nothing = None;
                session.press = Some(pick::Press::pressed(view, at));
                Task::none()
            }
            ObjectPointer::Move { view, at } => {
                if let Some(press) = &mut session.press {
                    press.moved(view, at);
                }
                Task::none()
            }
            ObjectPointer::Release { view, at } => {
                let Some(press) = session.press.take() else {
                    return Task::none();
                };
                match press.released(view, at, session.radius) {
                    pick::Gesture::Click(point) => {
                        if self.object_overlay().is_none() {
                            self.status.text = NOTHING_TO_SELECT.into();
                            self.event("object_pick_click", || json!({"point": point, "outlined": false}));
                            return Task::none();
                        }
                        self.event("object_pick_click", || json!({"point": point, "outlined": true}));
                        self.object_enqueue(pick::click_prompt(point));
                        self.flush_selects()
                    }
                    pick::Gesture::Loose(stroke) => {
                        let session = self.object_pick.as_mut().expect("the armed session");
                        session.overlay = None;
                        session.finding = Some((stroke, false));
                        self.object_ask()
                    }
                }
            }
        }
    }

    /// A loose stroke's pick answered: One is selected as a click would be, Several put up pins
    /// and Nothing keeps the stroke tinted under its notice.
    fn object_found(
        &mut self,
        asset: AssetId,
        revision: u64,
        stroke: Vec<[f64; 2]>,
        result: Result<Box<Found>, CallError>,
    ) -> Task<Message> {
        let current = self.object_target() == Some((asset.clone(), revision));
        let Some(session) = self.object_pick.as_mut() else {
            return Task::none();
        };
        session.finding = None;
        let radius = session.radius;
        match result {
            Ok(found) if current => {
                self.event("object_pick_found", || {
                    json!({"decision": format!("{:?}", found.decision).to_lowercase(),
                           "chosen": found.chosen, "points": stroke.len()})
                });
                match found.decision {
                    Decision::One => {
                        self.object_enqueue(pick::stroke_prompt(&stroke, radius, None));
                        return self.flush_selects();
                    }
                    Decision::Several => {
                        let pins = found
                            .candidates
                            .into_iter()
                            .map(|candidate| Pin {
                                position: candidate.position,
                                bounds: candidate.bounds,
                                chosen: false,
                                handle: candidate.image.map(handle),
                            })
                            .collect();
                        let session = self.object_pick.as_mut().expect("the armed session");
                        session.pins = Some(Pins {
                            stroke,
                            radius,
                            pins,
                        });
                    }
                    Decision::Nothing => {
                        self.status.text = NOTHING_FOUND.into();
                        let session = self.object_pick.as_mut().expect("the armed session");
                        session.nothing = Some(stroke);
                    }
                }
            }
            Ok(_) => {}
            Err(error)
                if current
                    && pick::needs_analysis(&error)
                    && session.analysed != Some((asset.clone(), revision)) =>
            {
                // Asked again once the analysis ends.
                session.finding = Some((stroke, false));
                return self.object_analyse(asset, revision);
            }
            Err(error) => {
                self.status.text = format!("Finding objects failed: {}", error.message);
            }
        }
        self.object_ask()
    }

    /// Return or Use N: select each chosen pin's candidate, one queued selection each.
    fn object_use(&mut self) -> Task<Message> {
        let Some(pins) = self.object_pick.as_ref().and_then(|session| session.pins.as_ref()) else {
            return Task::none();
        };
        let prompts = pins.prompts();
        if prompts.is_empty() {
            self.status.text = "Choose the numbered objects to use first".into();
            return Task::none();
        }
        if let Some(session) = self.object_pick.as_mut() {
            session.pins = None;
        }
        for prompt in prompts {
            if !self.object_enqueue(prompt) {
                break;
            }
        }
        self.flush_selects()
    }
}

fn purpose_name(purpose: &SelectFor) -> &'static str {
    match purpose {
        SelectFor::Pick(use_) => use_.as_str(),
        SelectFor::Recompute { .. } => "recompute",
    }
}
