//! Corrections › Object, the band's pointing kind (`docs/design/ai-tools-workspace.md`, "Object"):
//! the object picker armed for removal, the selection of picked objects it holds, Paint shadow and
//! the hand-off of a stroke that found nothing to Remove.
//!
//! The picking itself is the shared picker's ([`crate::object_pick`], [`super::object_pick`]): the
//! hover, the click, the loose drag and its pins. What this adds is what a pick becomes here: one
//! [`SelectedObject`] of the band's selection, tinted at once from the pick's held preview and
//! filled in with its `mask-alpha` artifact when its `task.select-object` answers
//! ([`SelectFor::Object`]). Generate then sends `task.fill-generative` with `object-artifacts`
//! ([`Editor::fill_request`]); everything after it is the delivered Remove group.
//!
//! **The selection.** A click replaces it, `⇧` adds, `⌥`-click on a selected object takes it away
//! by a hit test of the held previews, with no request. At most
//! [`crate::object_pick::SELECTION_LIMIT`] objects. Any change to the objects or the shadow is a
//! new selection: a new serial, a new seed and a new strip, as a new stroke is in Remove; an answer
//! only fills in its object's artifact.
//!
//! **The session** is armed while Object is the chosen kind on screen with its `object` setup
//! installed, a photograph open and nothing compared, and it loads only the picker's models. It
//! ends when any of that stops holding; a selection waiting for its turn goes with it.
use super::{
    Editor,
    message::{Message, object_pick::ObjectPointer},
    object_pick::{SetupId, SetupReady, selection_limit},
    remove::draw_seed,
    tasks,
};
use crate::{
    brush::Modifiers,
    object_pick::{self as pick, Held, NOTHING_TO_SELECT, PICK, PickUse, SELECT, SelectFor},
    state::remove::{EffectStroke, OBJECT_IDLE, RemoveSelection, SelectedObject, objects_selected},
    view::object_pick_canvas::HeldData,
};
use iced::Task;
use luxforge_core::mask::ObjectPrompt;
use serde_json::{Value, json};

impl Editor {
    /// Object is the chosen kind of the Corrections band on screen, and the run offers it.
    pub(crate) fn object_active(&self) -> bool {
        self.remove.object && self.remove_active() && self.object_select_module().is_some()
    }

    /// Choose Remove (`object` false) or Object as the band's kind. The other kind's selection is
    /// cleared; candidates already held stay in the strip.
    pub(crate) fn choose_fill_kind(&mut self, object: bool) -> Task<Message> {
        if object && self.object_select_module().is_none() {
            return Task::none();
        }
        self.remove.chosen = true;
        self.remove.object = object;
        self.clear_other_kind();
        Task::none()
    }

    /// Empty the selection of whatever the chosen kind does not make, keeping its serial so the
    /// strip and its candidates stay: Remove's stroke in Object, the objects and shadow in Remove.
    pub(crate) fn clear_other_kind(&mut self) {
        let object = self.remove.object;
        if !object {
            self.remove.shadow = false;
            self.object_selects.drop_for(PickUse::Object);
        }
        if let Some(selection) = self.remove.selection.as_mut() {
            if object {
                selection.points.clear();
            } else {
                selection.objects.clear();
                selection.effect = None;
            }
        }
        self.remove_reads.painting = None;
    }

    /// Why the Object session has to end, or `None` while it stays: the kind is no longer the
    /// one on screen, or its setup is no longer installed.
    pub(crate) fn object_tool_ended(&self) -> Option<&'static str> {
        if !self.object_active() {
            return Some("tool");
        }
        if self.setup_ready(SetupId::Object) != SetupReady::Installed {
            return Some("setup");
        }
        None
    }

    /// The session ended: an object whose selection had not started goes with it, since nothing
    /// will publish its artifact.
    pub(crate) fn object_tool_left(&mut self) {
        let running = match &self.object_selects.running {
            Some((SelectFor::Object { ticket }, _)) => Some(*ticket),
            _ => None,
        };
        let stranded =
            |object: &SelectedObject| object.artifact.is_none() && Some(object.ticket) != running;
        if self
            .remove
            .selection
            .as_ref()
            .is_some_and(|selection| selection.objects.iter().any(stranded))
        {
            self.object_renew(|selection| selection.objects.retain(|object| !stranded(object)));
        }
    }

    /// Once per update, before the shared session follows its stack: arm the Object session when
    /// the kind is on screen with its setup installed, and keep a loose stroke's radius at the
    /// band's Size. Another photograph re-arms it there at once.
    pub(crate) fn follow_object_tool(&mut self) -> Task<Message> {
        if !self.object_active() {
            if !self.remove.chosen || !self.remove.object {
                self.remove.shadow = false;
            }
            return Task::none();
        }
        match self.object_pick.as_ref() {
            Some(session) if session.purpose == PickUse::Object => {
                match self.object_pick_ended(session) {
                    Some(reason) => self.leave_object_pick(reason),
                    None => {
                        let radius = self.remove_radius();
                        if let Some(session) = self.object_pick.as_mut() {
                            session.radius = radius;
                        }
                        return Task::none();
                    }
                }
            }
            Some(_) => self.leave_object_pick("tool"),
            None => {}
        }
        if self.document.state.is_none()
            || self.object_comparing()
            || self.setup_ready(SetupId::Object) != SetupReady::Installed
        {
            return Task::none();
        }
        let radius = self.remove_radius();
        // The picker's models are the object finder's: the fill loads when Generate runs.
        self.enter_object_pick(PickUse::Object, SetupId::MaskObject, radius)
    }

    /// The object finder is ready: a status bar still saying Finding objects… says what Object
    /// does now instead, since no frame arrives to say it.
    pub(crate) fn object_tool_found(&mut self) {
        let finding = self
            .object_pick
            .as_ref()
            .is_none_or(|session| session.purpose != PickUse::Object || session.finding_objects());
        if finding || !self.status.text.starts_with(pick::FINDING_OBJECTS) {
            return;
        }
        let count = self.selected_objects().len();
        self.status.text = if count == 0 {
            OBJECT_IDLE.to_owned()
        } else {
            crate::state::remove::object_sentence(count, &self.remove.prompt)
        };
    }

    /// Why a press in Object does nothing yet, while no session is armed: its setup's words, or
    /// the reason the session waits.
    pub(crate) fn object_unarmed_reason(&self) -> String {
        self.workspace
            .corrections
            .remove
            .as_ref()
            .and_then(|model| model.object.as_ref()?.setup.as_ref())
            .map_or_else(
                || "Point at an object to remove it".to_owned(),
                |setup| setup.reason.clone(),
            )
    }

    /// Change the selection on the photograph on screen and make it a new one: a new serial, seed
    /// and strip, the candidate shown over the old one put away. With no object left there is no
    /// selection, and the shadow goes with them.
    fn object_renew(&mut self, change: impl FnOnce(&mut RemoveSelection)) {
        let Some(asset) = self
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone())
        else {
            return;
        };
        let radius = self.remove_radius();
        let mut selection = self
            .remove
            .selection
            .take()
            .filter(|selection| selection.asset == asset)
            .unwrap_or_else(|| RemoveSelection::empty(asset, 0, 0, radius));
        selection.points.clear();
        change(&mut selection);
        self.remove.clear_draft();
        if selection.objects.is_empty() {
            return;
        }
        self.remove.serial += 1;
        selection.serial = self.remove.serial;
        selection.seed = draw_seed(selection.serial);
        self.remove.selection = Some(selection);
    }

    /// The selected objects on the photograph on screen.
    fn selected_objects(&self) -> &[SelectedObject] {
        self.document
            .state
            .as_ref()
            .and_then(|state| self.remove.selection_on(&state.asset.id))
            .map_or(&[], |selection| selection.objects.as_slice())
    }

    /// The selected object under a content position, the last picked first: a hit test of each
    /// object's held preview over its bounds.
    fn object_hit(&self, at: [f64; 2]) -> Option<usize> {
        self.selected_objects().iter().rposition(|object| {
            object
                .held
                .as_ref()
                .is_some_and(|held| held.coverage.covers(held.bounds, at))
        })
    }

    /// What the status bar says once the selection changed.
    fn say_objects(&mut self) {
        let count = self.selected_objects().len();
        self.status.text = if count == 0 {
            "Selection cleared".into()
        } else {
            crate::state::remove::object_sentence(count, &self.remove.prompt)
        };
    }

    /// A click in Object: with an object outlined it is the whole selection, or is added to it with
    /// `⇧`; `⌥` takes the selected object under the pointer away. With nothing outlined it says so.
    pub(crate) fn object_tool_click(
        &mut self,
        point: [f64; 2],
        modifiers: Modifiers,
    ) -> Task<Message> {
        if let Some(reason) = self.generation_lock() {
            self.status.text = reason;
            return Task::none();
        }
        if modifiers.alt {
            return self.object_take(point);
        }
        let Some(kept) = self.object_overlay().map(|overlay| overlay.held.clone()) else {
            self.status.text = NOTHING_TO_SELECT.into();
            self.event(
                "object_pick_click",
                || json!({"point": point, "outlined": false}),
            );
            return Task::none();
        };
        if modifiers.shift && self.object_hit(point).is_some() {
            self.status.text = "That object is already selected".into();
            return Task::none();
        }
        self.event(
            "object_pick_click",
            || json!({"point": point, "outlined": true, "adds": modifiers.shift}),
        );
        self.object_tool_pick(vec![(pick::click_prompt(point), kept)], modifiers.shift)
    }

    /// `⌥`-click: the selected object under the pointer leaves the selection, by a hit test of the
    /// held previews; nothing is sent.
    fn object_take(&mut self, point: [f64; 2]) -> Task<Message> {
        let Some(index) = self.object_hit(point) else {
            self.status.text = "No selected object here".into();
            return Task::none();
        };
        let ticket = self.selected_objects()[index].ticket;
        self.object_selects.drop_object(ticket);
        self.event(
            "object_taken",
            || json!({"number": index + 1, "ticket": ticket}),
        );
        self.object_renew(|selection| {
            selection.objects.remove(index);
        });
        self.say_objects();
        Task::none()
    }

    /// Picks that select: each a new object of the selection, replacing it unless `adds`, and one
    /// `task.select-object` queued for it. The ninth object is refused in the status bar.
    pub(crate) fn object_tool_pick(
        &mut self,
        picks: Vec<(ObjectPrompt, Option<Held>)>,
        adds: bool,
    ) -> Task<Message> {
        if picks.is_empty() {
            return Task::none();
        }
        if let Some(reason) = self.generation_lock() {
            self.status.text = reason;
            return Task::none();
        }
        let Some(asset) = self
            .object_pick
            .as_ref()
            .map(|session| session.asset.clone())
        else {
            return Task::none();
        };
        let kept = if adds {
            self.selected_objects().len()
        } else {
            0
        };
        if kept + picks.len() > pick::SELECTION_LIMIT {
            self.status.text = selection_limit();
            return Task::none();
        }
        if !adds {
            self.object_selects.drop_for(PickUse::Object);
        }
        let mut picked = Vec::with_capacity(picks.len());
        for (prompt, held) in picks {
            let ticket = self.remove.tickets + 1;
            if !self.queue_select(SelectFor::Object { ticket }, asset.clone(), prompt.clone()) {
                break;
            }
            self.remove.tickets = ticket;
            picked.push(SelectedObject {
                ticket,
                prompt,
                held,
                artifact: None,
            });
        }
        if picked.is_empty() {
            return Task::none();
        }
        self.object_renew(|selection| {
            if !adds {
                selection.objects.clear();
                selection.effect = None;
            }
            selection.objects.extend(picked);
        });
        self.say_objects();
        self.flush_selects()
    }

    /// The selection of `ticket` is still held, so its answer has an object to fill in.
    pub(crate) fn object_tool_holds(&self, ticket: u64) -> bool {
        self.remove.selection.as_ref().is_some_and(|selection| {
            selection
                .objects
                .iter()
                .any(|object| object.ticket == ticket)
        })
    }

    /// An object's `task.select-object` answered: its artifact is what Generate names. The same
    /// object picked twice is kept once.
    pub(crate) fn object_tool_selected(&mut self, ticket: u64, selection: Value) -> Task<Message> {
        let Some(artifact) = selection["artifact"].as_str().map(str::to_owned) else {
            self.object_tool_failed(ticket);
            return Task::none();
        };
        let Some(held) = self.remove.selection.as_mut() else {
            return Task::none();
        };
        if held
            .objects
            .iter()
            .any(|object| object.artifact.as_deref() == Some(artifact.as_str()))
        {
            self.status.text = "That object is already selected".into();
            self.object_renew(|selection| {
                selection.objects.retain(|object| object.ticket != ticket);
            });
            return Task::none();
        }
        if let Some(object) = held
            .objects
            .iter_mut()
            .find(|object| object.ticket == ticket)
        {
            object.artifact = Some(artifact);
        }
        Task::none()
    }

    /// An object's selection failed, which the status bar has said: it leaves the selection.
    pub(crate) fn object_tool_failed(&mut self, ticket: u64) {
        if self.object_tool_holds(ticket) {
            self.object_renew(|selection| {
                selection.objects.retain(|object| object.ticket != ticket);
            });
        }
    }

    /// Paint shadow pressed: the brush is armed for one stroke, or put down again.
    pub(crate) fn object_shadow(&mut self) {
        if !self.object_active() {
            return;
        }
        if self.remove.shadow {
            self.remove.shadow = false;
            self.status.text = "Paint shadow put down".into();
            return;
        }
        if self.selected_objects().is_empty() {
            self.status.text = "Point at an object first".into();
            return;
        }
        if let Some(reason) = self.generation_lock() {
            self.status.text = reason;
            return;
        }
        self.remove.shadow = true;
        self.status.text =
            "Paint the shadow \u{b7} one stroke \u{b7} Esc puts the brush down".into();
    }

    /// The shadow stroke is painted: it is the selection's effect region, and the brush is put
    /// down.
    pub(crate) fn object_shadow_painted(&mut self, points: Vec<[f64; 2]>, radius: f64) {
        self.event(
            "object_shadow",
            || json!({"points": points.len(), "radius": radius}),
        );
        self.object_renew(|selection| {
            selection.effect = Some(EffectStroke { points, radius });
        });
        self.remove.shadow = false;
        self.status.text = "Shadow painted \u{b7} Generate removes it with the objects".into();
    }

    /// A pointer step in Object that is not a pick: Paint shadow's stroke, as Remove paints. A
    /// pick press while a generation runs is refused by name. `None` when the picker takes it.
    pub(crate) fn object_tool_pointer(&mut self, pointer: ObjectPointer) -> Option<Task<Message>> {
        let painting = self.remove_reads.painting.is_some();
        if !self.remove.shadow && !painting {
            if let ObjectPointer::Press { .. } = pointer
                && let Some(reason) = self.generation_lock()
            {
                self.status.text = reason;
                return Some(Task::none());
            }
            return None;
        }
        Some(match pointer {
            ObjectPointer::Press {
                view,
                at,
                modifiers,
            } if !painting => self.remove_press(view, at, modifiers),
            ObjectPointer::Press { .. } => Task::none(),
            ObjectPointer::Move { view, at } => self.remove_drag(view, at),
            ObjectPointer::Release { view, at } => self.remove_release(view, at),
        })
    }

    /// Return in Object with no pins up: a stroke that found nothing becomes Remove's selection,
    /// and otherwise a selection ready to fill is generated.
    pub(crate) fn object_tool_done(&mut self) -> Task<Message> {
        let nothing = self
            .object_pick
            .as_mut()
            .and_then(|session| session.nothing.take());
        if let Some(stroke) = nothing {
            return self.object_hand_off(stroke);
        }
        let ready = self
            .workspace
            .corrections
            .remove
            .as_ref()
            .is_some_and(|model| model.selection && model.generate_reason.is_none());
        if ready {
            return self.dispatch(Message::Remove(
                super::message::remove::RemoveMessage::Generate,
            ));
        }
        Task::none()
    }

    /// Nothing found: the kind is Remove, with the stroke as its painted selection.
    fn object_hand_off(&mut self, stroke: Vec<[f64; 2]>) -> Task<Message> {
        let Some(asset) = self
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone())
        else {
            return Task::none();
        };
        let radius = self
            .object_pick
            .as_ref()
            .map_or_else(|| self.remove_radius(), |session| session.radius);
        self.event(
            "object_hand_off",
            || json!({"points": stroke.len(), "radius": radius}),
        );
        let task = self.choose_fill_kind(false);
        self.remove.clear_draft();
        self.remove.serial += 1;
        let serial = self.remove.serial;
        self.remove.selection = Some(RemoveSelection {
            points: stroke,
            ..RemoveSelection::empty(asset, serial, draw_seed(serial), radius)
        });
        self.status.text =
            "Painted area selected \u{b7} Generate fills it \u{b7} Esc clears it".into();
        task
    }

    /// Escape in Object, one level at a time: a press in progress, a shadow stroke being painted,
    /// a running generation (cancelled), Paint shadow, the pins or the Nothing notice, then the
    /// selection, then the mode, as the Corrections band's own Escape order goes on.
    pub(crate) fn object_tool_escape(&mut self) -> Task<Message> {
        if let Some(session) = self.object_pick.as_mut()
            && session.press.take().is_some()
        {
            return Task::none();
        }
        if self.remove_reads.painting.take().is_some() {
            self.status.text = "Stroke cancelled".into();
            return Task::none();
        }
        if self.generation_lock().is_some() {
            return self.remove_cancel();
        }
        if std::mem::take(&mut self.remove.shadow) {
            self.status.text = "Paint shadow put down".into();
            return Task::none();
        }
        if let Some(session) = self.object_pick.as_mut()
            && (session.pins.take().is_some() || session.nothing.take().is_some())
        {
            return Task::none();
        }
        self.correction_escape()
    }

    /// What the picking canvas draws of Object's selection.
    pub(crate) fn object_tool_canvas(&self) -> HeldData<'_> {
        let selection = self
            .document
            .state
            .as_ref()
            .and_then(|state| self.remove.selection_on(&state.asset.id));
        let painting = self.remove_reads.painting.as_ref();
        let progress = self
            .document
            .state
            .as_ref()
            .and_then(|state| self.remove.running_on(&state.asset.id))
            .and_then(|job| {
                let objects = selection?.objects.iter().filter_map(|o| o.held.as_ref());
                let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
                let mut any = false;
                for held in objects {
                    let [x0, y0, x1, y1] = held.bounds;
                    lo = [lo[0].min(x0), lo[1].min(y0)];
                    hi = [hi[0].max(x1), hi[1].max(y1)];
                    any = true;
                }
                any.then(|| (job.fraction, [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0]))
            });
        HeldData {
            objects: selection.map_or(&[], |selection| selection.objects.as_slice()),
            effect: selection.and_then(|selection| selection.effect.as_ref()),
            live: painting.map(|(_, points)| points.as_slice()),
            brush: (self.remove.shadow || painting.is_some()).then(|| self.remove_radius()),
            progress,
            shown: self
                .core_gesture()
                .is_some_and(|gesture| gesture.candidate().is_some() && gesture.draft.drafted()),
        }
    }

    /// Copy as JSON for the Object group's picks: the last `query.pick-object` the session sent.
    pub(crate) fn object_pick_copy(&self) -> Result<Value, String> {
        self.object_pick
            .as_ref()
            .filter(|session| session.purpose == PickUse::Object)
            .and_then(|session| session.last_request.clone())
            .map(|params| json!({"method": PICK, "params": params}))
            .ok_or_else(|| "Point at an object first".to_owned())
    }

    /// Copy as JSON for the Object group's selects: the `task.select-object` each selected object
    /// was picked with, built as the queue builds it.
    pub(crate) fn object_select_copy(&self) -> Result<Value, String> {
        let asset = &self
            .document
            .state
            .as_ref()
            .ok_or(crate::state::NO_PHOTOGRAPH)?
            .asset
            .id;
        let requests: Vec<Value> = self
            .selected_objects()
            .iter()
            .map(|object| {
                let params = pick::select_request(asset, &object.prompt, json!(tasks::request()));
                json!({"method": SELECT, "params": params})
            })
            .collect();
        match requests.len() {
            0 => Err("Point at an object first".into()),
            1 => Ok(requests.into_iter().next().unwrap_or_default()),
            _ => Ok(Value::Array(requests)),
        }
    }

    /// What a captured frame records of Object's selection.
    pub(crate) fn object_tool_summary(&self) -> Value {
        json!({
            "kind": if self.remove.object { "object" } else { "remove" },
            "active": self.object_active(),
            "shadow_armed": self.remove.shadow,
            "objects": self.selected_objects().iter().enumerate().map(|(index, object)| json!({
                "number": index + 1,
                "ticket": object.ticket,
                "prompt": object.prompt.request_fields(),
                "artifact": object.artifact,
                "bounds": object.held.as_ref().map(|held| held.bounds),
            })).collect::<Vec<_>>(),
            "effect": self.document.state.as_ref()
                .and_then(|state| self.remove.selection_on(&state.asset.id))
                .and_then(|selection| selection.effect.as_ref())
                .map(|effect| json!({"points": effect.points.len(), "radius": effect.radius})),
            "selection": objects_selected(self.selected_objects().len()),
        })
    }
}
