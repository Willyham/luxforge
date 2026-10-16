//! The range picker's controller: arming the eyedropper, following the stack it picks on, and
//! turning a click or a drag on the photograph into one `mask.pick-<kind>` command.
//!
//! The picker is a sub-state of Mask mode ([`crate::range_pick`]). Nothing here changes the
//! workspace mode, the open mask or the sections' target, so the overlay keeps showing the
//! selection while a pick is aimed and an adjustment made between picks edits the open mask's own
//! layer. Each pick is one command through the panel's one request builder
//! ([`Editor::mask_request`]), so the request a pick sends is the one Copy as JSON request shows,
//! and it runs as an owner task like every other mask command: one pick is in flight at a time, and
//! a pick made meanwhile waits, the newest replacing an older waiting one.
use crate::{
    app::{
        Editor,
        gesture::Starting,
        message::{Message, mask::PickPointer},
        outcome::Outcome,
    },
    brush::{ContentMap, Displayed},
    range_pick::{PickArea, PickDrag, RangePicker, place},
};
use iced::Task;
use luxforge_core::{
    ComponentId, MaskId,
    mask::commands::{self, MaskTarget},
};
use serde_json::{Map, Value, json};

impl Editor {
    /// The kind of `component` in `mask`, as the listing reports it.
    fn listed_kind(&self, mask: &MaskId, component: &ComponentId) -> Option<String> {
        self.document
            .masks
            .as_ref()?
            .masks
            .iter()
            .find(|report| &report.id == mask)?
            .components
            .iter()
            .find(|report| &report.id == component)
            .map(|report| report.kind.clone())
    }

    /// Arm the eyedropper on `component` of `mask`, when the host declares a pick for its kind.
    /// Arming is a tool start, so an overlay that is Off by default shows Tint until the person
    /// chooses otherwise. It sends nothing; the content map a press is placed through is asked for
    /// after this message ([`Editor::follow_range_picker`]).
    pub(crate) fn arm_picker(&mut self, mask: MaskId, component: ComponentId) -> bool {
        let Some(asset) = self
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone())
        else {
            return false;
        };
        let Some(kind) = self.listed_kind(&mask, &component) else {
            return false;
        };
        let Some(picker) = RangePicker::arm(asset, mask, component, &kind) else {
            return false;
        };
        self.mask_panel.overlay_manual = false;
        self.event("range_picker_armed", || {
            json!({"mask": picker.mask.as_str(), "component": picker.component.as_str(),
                   "kind": picker.kind, "action": picker.action})
        });
        self.mask_panel.picker = Some(picker);
        true
    }

    /// Put the eyedropper down, saying why in the evidence log. A drag in progress goes with it
    /// and sends nothing, and so does a pick still waiting for its turn.
    pub(crate) fn put_picker_down(&mut self, reason: &'static str) {
        if let Some(picker) = self.mask_panel.picker.take() {
            self.event("range_picker_ended", || {
                json!({"component": picker.component.as_str(), "reason": reason,
                       "dropped": picker.waiting.is_some()})
            });
        }
    }

    /// The row's Pick: arm the eyedropper on the selected component, or put it down when it is
    /// armed there already.
    pub(crate) fn toggle_picker(&mut self) -> Task<Message> {
        let (Some(mask), Some(component)) = (
            self.mask_panel.selected_mask.clone(),
            self.mask_panel.selected_component.clone(),
        ) else {
            self.status.text = "Select the component to pick into".into();
            return Task::none();
        };
        if self
            .mask_panel
            .picker
            .as_ref()
            .is_some_and(|picker| picker.mask == mask && picker.component == component)
        {
            self.put_picker_down("pick");
            return Task::none();
        }
        if !self.mask_mode_active() {
            self.status.text = "Picking from the photograph is part of Mask mode".into();
            return Task::none();
        }
        // Arming is view state, like entering a mode: an open draft is finished first, and a brush
        // in hand is put down, since one press cannot both paint and pick.
        if let Some(reason) = self.gesture_refusal(Starting::Mode) {
            self.status.text = reason;
            return Task::none();
        }
        self.put_brush_down();
        if !self.arm_picker(mask, component) {
            let kind = self
                .selected_component_kind()
                .unwrap_or_else(|| "unknown".to_owned());
            self.status.text = format!("A {kind} component is not picked from the photograph");
        }
        Task::none()
    }

    /// Why the armed eyedropper has to be put down after this message, or `None` while it stays.
    fn picker_ended(&self, picker: &RangePicker) -> Option<&'static str> {
        if self.document.state.as_ref().map(|state| &state.asset.id) != Some(&picker.asset) {
            return Some("photograph");
        }
        if !self.mask_mode_active() {
            return Some("mode");
        }
        if self.document.compare_return.is_some()
            || self.compare_key.is_down()
            || self.presentation.compare_after.is_some()
        {
            return Some("compare");
        }
        // Another mask gesture — a brush in hand, a new shape, a handle drag — owns the press.
        if self.mask_shape().is_some() {
            return Some("gesture");
        }
        if self.mask_panel.selected_mask.as_ref() != Some(&picker.mask)
            || self.mask_panel.selected_component.as_ref() != Some(&picker.component)
        {
            return Some("selection");
        }
        // Deleted or undone away, here or by another client.
        if self.listed_kind(&picker.mask, &picker.component).is_none() {
            return Some("gone");
        }
        None
    }

    /// The armed eyedropper follows the stack it picks on, once per update: it is put down when
    /// what it picks into is no longer what the panel is looking at, its content map is asked for
    /// again when the displayed entry moves, and a pick that was waiting is sent once nothing else
    /// is in flight and the map is the stack's on screen.
    pub(crate) fn follow_range_picker(&mut self) -> Task<Message> {
        // The remembered pick goes with its component, armed or not.
        if let Some((mask, component, _)) = &self.mask_panel.last_pick
            && self.listed_kind(mask, component).is_none()
        {
            self.mask_panel.last_pick = None;
        }
        let Some(picker) = &self.mask_panel.picker else {
            return Task::none();
        };
        if let Some(reason) = self.picker_ended(picker) {
            self.put_picker_down(reason);
            return Task::none();
        }
        let entry = self.displayed_entry();
        let mut tasks = Vec::new();
        if picker.entry != entry {
            let asset = picker.asset.clone();
            let id = self.next_gesture();
            let picker = self.mask_panel.picker.as_mut().expect("the armed picker");
            picker.entry = entry.clone();
            picker.request = Some(id);
            tasks.push(crate::app::tasks::transform_task(
                self.owner.clone(),
                self.client,
                id,
                asset,
                entry,
                None,
            ));
        }
        tasks.push(self.flush_pick());
        Task::batch(tasks)
    }

    /// One pointer step on the photograph while the eyedropper is armed. A press and its moves are
    /// held as the drag the canvas draws; the release makes one pick — a click where it went down,
    /// or the rectangle it was dragged over — which waits its turn ([`Editor::flush_pick`]). A
    /// release whose drag Escape cancelled makes none.
    pub(crate) fn pick_pointer(&mut self, pointer: PickPointer) -> Task<Message> {
        let Some(picker) = self.mask_panel.picker.as_mut() else {
            return Task::none();
        };
        match pointer {
            PickPointer::Press { view, at } => {
                picker.drag = Some(PickDrag::pressed(view, at));
                Task::none()
            }
            PickPointer::Move { view, at } => {
                if let Some(drag) = &mut picker.drag {
                    drag.moved(view, at);
                }
                Task::none()
            }
            PickPointer::Release { view, at, shift } => {
                let Some(drag) = picker.drag.take() else {
                    return Task::none();
                };
                // Shift adds a colour to those held; a kind that holds none has nothing to add to,
                // so it reads no Shift.
                let gesture = drag.released(view, at, shift && picker.adds);
                if picker.waiting.replace(gesture).is_some() {
                    self.event("range_pick_replaced", || json!({}));
                }
                self.flush_pick()
            }
        }
    }

    /// Send the waiting pick, if there is one and it can go: nothing of this desktop's is in flight
    /// and the content map is the one of the stack on screen. Otherwise it keeps waiting, and the
    /// next update tries again.
    fn flush_pick(&mut self) -> Task<Message> {
        let Some(picker) = &self.mask_panel.picker else {
            return Task::none();
        };
        let Some(gesture) = picker.waiting else {
            return Task::none();
        };
        if self.busy {
            return Task::none();
        }
        let Some(entry) = self.displayed_entry() else {
            return Task::none();
        };
        let displayed = Displayed {
            entry: &entry,
            draft: None,
        };
        let placed = match ContentMap::for_press(picker.map.as_ref(), &displayed) {
            Ok(map) => place(map, &gesture),
            // The fresh map is on its way, and the pick waits for it.
            Err(_) if picker.request.is_some() => return Task::none(),
            Err(refusal) => Err(refusal),
        };
        let picker = self.mask_panel.picker.as_mut().expect("the armed picker");
        picker.waiting = None;
        match placed {
            Ok(area) => {
                let (mask, component) = (picker.mask.clone(), picker.component.clone());
                self.mask_panel.last_pick = Some((mask, component, area));
                self.send_pick(area)
            }
            Err(refusal) => {
                self.status.text = refusal.sentence().to_owned();
                self.event("range_pick", || json!({"error": refusal.token()}));
                self.outcome(Outcome::PickEnded);
                Task::none()
            }
        }
    }

    /// Send one pick as its command, through the panel's one mask-command path.
    fn send_pick(&mut self, area: PickArea) -> Task<Message> {
        let Some((method, target, fields)) =
            self.mask_panel.picker.as_ref().and_then(|picker| {
                pick_command(&picker.mask, &picker.component, &picker.kind, &area)
            })
        else {
            return Task::none();
        };
        self.event(
            "range_pick",
            || json!({"method": method, "fields": Value::Object(fields.clone())}),
        );
        let sent = self.mask_command(method, target, fields);
        // The command family's one refusal, when it refused, is in the status bar already.
        self.outcome(if self.mask_panel.command_in_flight {
            Outcome::PickCommitting
        } else {
            Outcome::PickEnded
        });
        sent
    }

    /// What the armed eyedropper's canvas is drawn from, while it is armed in Mask mode, its map has
    /// arrived and no mask gesture's handles hold the canvas.
    pub(crate) fn range_pick_canvas(&self) -> Option<crate::view::RangePickData<'_>> {
        let picker = self.mask_panel.picker.as_ref()?;
        if !self.mask_mode_active() || self.drawn_mask().is_some() {
            return None;
        }
        Some(crate::view::RangePickData {
            map: picker.map.as_ref()?,
            rectangle: picker.rectangle(),
        })
    }

    /// The pick request the selected component's Copy as JSON request shows: the last pick the
    /// eyedropper sent on it, or a click at the centre of the frame before any.
    pub(crate) fn copied_pick(&self) -> Option<(&'static str, MaskTarget, Map<String, Value>)> {
        let mask = self.mask_panel.selected_mask.as_ref()?;
        let component = self.mask_panel.selected_component.as_ref()?;
        let kind = self.listed_kind(mask, component)?;
        let area = self
            .mask_panel
            .last_pick
            .as_ref()
            .filter(|(last_mask, last_component, _)| {
                last_mask == mask && last_component == component
            })
            .map_or(PickArea::CENTRE, |(_, _, area)| *area);
        pick_command(mask, component, &kind, &area)
    }
}

/// The command one pick into `component` of `mask` sends: the kind's generated pick, addressed to
/// the two, with the pick's own fields.
pub(crate) fn pick_command(
    mask: &MaskId,
    component: &ComponentId,
    kind: &str,
    area: &PickArea,
) -> Option<(&'static str, MaskTarget, Map<String, Value>)> {
    let command = commands::pick(kind)?;
    let adds = command.action.parameter("add").is_some();
    Some((
        command.method,
        MaskTarget {
            mask: Some(mask.clone()),
            component: Some(component.clone()),
            ..MaskTarget::default()
        },
        area.fields(adds),
    ))
}
