//! Host mapping for generated controls. Widgets report fractions and events; descriptors own values.

use crate::app::Before;
use crate::app::{
    Editor,
    message::{Message, action::ActionMessage, control::ControlMessage},
    tasks::call,
};
use crate::coalesce::Coalesce;
use crate::state::{
    control_tree::{at_path, walk},
    fields::{self, Fields, submit_preset},
    number::{NumberSpec, number_text},
    tools,
};
use iced::{Task, widget::operation};
use luxforge_core::{AssetId, Control, EntryId, ParameterKind, check_value};
use luxforge_ui::{
    ColorPickerEvent, CurveEditorEvent, WheelEvent, hex_to_rgb, hsv_to_rgb, rgb_to_hsv,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A query result can only update the exact curve, entry, channel and points that requested it.
#[derive(Clone, Debug)]
pub(crate) struct CurveSampleIdentity {
    pub(crate) sequence: u64,
    pub(crate) asset: AssetId,
    pub(crate) entry: EntryId,
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) channel: usize,
    pub(crate) points: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct CurveSampleRequest {
    identity: CurveSampleIdentity,
    query: String,
}

/// The generated controls' local state: the text typed into each field, the presentation state
/// the recipe does not hold, which field is typed or dragged, which sections are expanded, and a
/// reset waiting for a gesture's commit. Authoritative values stay in the recipe.
#[derive(Default)]
pub(crate) struct Controls {
    pub(crate) query_choice_slot: Coalesce<super::query_choice::QueryChoiceRequest>,
    /// The text typed into each generated field, by (action id, parameter name).
    pub(crate) fields: Fields,
    /// Local presentation state of generated controls.
    pub(crate) ui: tools::ControlsUi,
    /// The (action, parameter) whose value is being typed.
    pub(crate) editing: Option<(String, String)>,
    /// The (action, parameter) whose slider is being dragged.
    pub(crate) dragging: Option<(String, String)>,
    /// Sections the person collapsed or expanded; every other follows the default.
    pub(crate) expanded: BTreeMap<String, bool>,
    /// A field reset waiting for the gesture commit or request in flight to answer.
    pub(crate) pending_reset: Option<super::slider::PendingReset>,
}

/// The curve sample queries: one in flight with only the newest waiting, and which request each
/// displayed curve last asked, so a late or displaced answer is recognised and dropped.
#[derive(Debug, Default)]
pub(crate) struct CurveSampling {
    /// The last request's sequence number, minted per request.
    pub(crate) sequence: u64,
    /// The sequence each (action, parameter) last asked under.
    pub(crate) requested: BTreeMap<(String, String), u64>,
    /// The asset, entry and points each (action, parameter) last asked about.
    pub(crate) requested_source:
        BTreeMap<(String, String), (luxforge_core::AssetId, luxforge_core::EntryId, Value)>,
    pub(crate) slot: Coalesce<CurveSampleRequest>,
}

impl Editor {
    /// One generated-control or tools-panel section message.
    pub(super) fn control_update(&mut self, message: ControlMessage) -> Task<Message> {
        // The crop frame's fields are the open frame's, not a request of their own: the crop
        // driver turns what their control sends into a change of that frame.
        if message
            .field()
            .is_some_and(|(action, _)| self.is_crop_action(action))
        {
            return self.crop_control(message);
        }
        match message {
            message @ (ControlMessage::QueryChoiceSearch { .. }
            | ControlMessage::QueryChoicePage { .. }
            | ControlMessage::QueryChoiceRetry { .. }
            | ControlMessage::QueryChoiceShared { .. }
            | ControlMessage::QueryChoiceSelect { .. }
            | ControlMessage::QueryChoiceAnswered { .. }
            | ControlMessage::QueryChoiceApply { .. }
            | ControlMessage::QueryChoiceChange { .. }
            | ControlMessage::QueryChoiceReport { .. }
            | ControlMessage::QueryChoiceReportOpened { .. }) => {
                return self.query_choice_update(message);
            }
            ControlMessage::Field {
                action,
                parameter,
                text,
            } => {
                self.controls.fields.set(&action, &parameter, text);
                // Typing is editing: the field shows what was typed until it is committed.
                self.controls.editing = Some((action, parameter));
            }
            ControlMessage::Fraction {
                action,
                parameter,
                fraction,
            } => {
                return self.control_fraction(action, parameter, fraction);
            }
            ControlMessage::Discrete {
                action,
                parameter,
                value,
            } => {
                return self.control_value(action, parameter, value, false);
            }
            ControlMessage::Released { action, parameter } => {
                return self.control_release(action, parameter);
            }
            ControlMessage::Step {
                action,
                parameter,
                direction,
            } => {
                return self.control_step(action, parameter, direction);
            }
            ControlMessage::KeyNudge {
                action,
                parameter,
                direction,
                shift,
                option,
            } => {
                return self.control_key_nudge(action, parameter, direction, shift, option);
            }
            ControlMessage::FieldNudge {
                action,
                parameter,
                direction,
                shift,
                option,
            } => {
                return self.control_field_nudge(action, parameter, direction, shift, option);
            }
            ControlMessage::TogglePicker { action, parameter } => {
                let color = self.controls.ui.color_mut((action, parameter));
                color.open = !color.open;
            }
            ControlMessage::ToggleGroup { module_id, path } => {
                // A module's only group is drawn without a header and is always shown, so there is
                // no disclosure to toggle and no per-client state to record for it.
                if tools::module_of(&self.modules, &module_id)
                    .is_some_and(|module| tools::is_headerless_group(module, &path))
                {
                    return Task::none();
                }
                let key = format!(
                    "{module_id}/{}",
                    path.iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(".")
                );
                let initial =
                    initial_group_expanded(&self.modules, &module_id, &path).unwrap_or(true);
                let entry = self
                    .controls
                    .ui
                    .group_expanded
                    .entry(key)
                    .or_insert(initial);
                *entry = !*entry;
            }
            ControlMessage::SelectView {
                module_id,
                group,
                view,
            } => return self.select_view(module_id, group, view),
            ControlMessage::Wheel { action, hue, event } => {
                return self.control_wheel(action, hue, event);
            }
            ControlMessage::WheelNudge {
                action,
                hue,
                saturation,
                direction,
                shift,
                option,
            } => {
                return self.control_wheel_nudge(action, hue, saturation, direction, shift, option);
            }
            ControlMessage::Picker {
                action,
                parameter,
                event,
            } => {
                return self.control_picker(action, parameter, event);
            }
            ControlMessage::Curve {
                action,
                parameter,
                event,
            } => {
                return self.control_curve(action, parameter, event);
            }
            ControlMessage::CurveSampled { identity, result } => {
                return self.curve_sampled(identity, result);
            }
            ControlMessage::EditValue { action, parameter } => {
                let id = fields::field_id(&action, &parameter, None);
                self.controls.editing = Some((action, parameter));
                return operation::focus(iced::widget::Id::from(id));
            }
            ControlMessage::Submit { action, parameter } => {
                self.controls.dragging = None;
                // Refused before the field lets go, so the typed text stays with its reason.
                if let Some(reason) = self.action_refusal(&action) {
                    self.status.text = reason;
                    return Task::none();
                }
                let preset = match submit_preset(
                    &self.modules,
                    &action,
                    parameter.as_deref(),
                    &self.controls.fields,
                ) {
                    Ok(preset) => preset,
                    // The field only stops editing once the submit actually runs; a rejected
                    // submit leaves the typed text on screen with its reason rather than
                    // silently reverting to the last committed value.
                    Err(message) => {
                        self.status.text = message;
                        return Task::none();
                    }
                };
                self.controls.editing = None;
                return self.dispatch(Message::Action(ActionMessage::Run { action, preset }));
            }
            ControlMessage::ResetField { action, parameter } => {
                return self.reset_field(action, parameter);
            }
            ControlMessage::ToggleSection(module_id) => {
                let expanded = self
                    .workspace
                    .tools
                    .all()
                    .find(|section| section.module_id == module_id)
                    .map(|section| section.expanded)
                    .unwrap_or(true);
                self.controls.expanded.insert(module_id, !expanded);
            }
            ControlMessage::ResetModule(module_id) => {
                let Some(reset) = tools::module_of(&self.modules, &module_id)
                    .and_then(|module| module.reset.clone())
                else {
                    self.status.text = format!("{module_id} declares no reset action");
                    return Task::none();
                };
                return self.dispatch(Message::Action(ActionMessage::Run {
                    action: reset.action,
                    preset: reset.preset,
                }));
            }
            ControlMessage::ResetGroup { module_id, path } => {
                // The reset the group's header shows, exactly as the view model resolved it for
                // the photo and the target: on a RAW photo's global target White balance's reset
                // is the development's As shot.
                let Some(reset) = self.group_reset_of(&module_id, &path) else {
                    self.status.text = format!("{module_id} declares no reset for that group");
                    return Task::none();
                };
                return self.dispatch(Message::Action(ActionMessage::Run {
                    action: reset.action,
                    preset: reset.preset,
                }));
            }
        }
        Task::none()
    }

    /// Show `view` of the tab row of `module_id`'s group `group` (`None` for its own top-level
    /// row), both by id, through `workspace.set` exactly as any client selects one: the session's
    /// answer is what the panel then draws. A row or view the module does not declare sends
    /// nothing, and choosing the view already shown sends nothing either. No recipe field, history
    /// entry, frame or poll follows.
    pub(crate) fn select_view(
        &mut self,
        module_id: String,
        group: Option<String>,
        view: String,
    ) -> Task<Message> {
        let Some(module) = tools::module_of(&self.modules, &module_id) else {
            return Task::none();
        };
        let Some(views) = module.views_at(group.as_deref()) else {
            return Task::none();
        };
        let Some(index) = views.iter().position(|offered| *offered == view) else {
            return Task::none();
        };
        let shown = self
            .session
            .workspace
            .view(&module_id, group.as_deref())
            .and_then(|chosen| views.iter().position(|offered| *offered == chosen))
            .unwrap_or(0);
        if shown == index {
            return Task::none();
        }
        let selection = luxforge_core::ViewSelection {
            module: module_id,
            group,
            view,
        };
        self.event("view_selected", || json!(selection));
        crate::app::tasks::workspace_task(
            self.owner.clone(),
            self.client,
            json!({ "views": [selection] }),
        )
    }

    /// One wheel event: a move drafts the hue and saturation it stands for together, as one patch
    /// through one draft keyed by the hue field; the release commits that draft once; a
    /// double-click runs the wheel's declared reset as one action, after the commit its first
    /// press made has answered ([`Editor::reset_declared`]).
    pub(crate) fn control_wheel(
        &mut self,
        action: String,
        hue: String,
        event: WheelEvent,
    ) -> Task<Message> {
        let Some(wheel) = wheel_of(&self.modules, &action, &hue).cloned() else {
            return Task::none();
        };
        match event {
            WheelEvent::Moved {
                hue: degrees,
                radius,
            } => {
                let (Some(hue_spec), Some(saturation_spec)) = (
                    self.number_spec(&action, &wheel.hue),
                    self.number_spec(&action, &wheel.saturation),
                ) else {
                    return Task::none();
                };
                let values = serde_json::Map::from_iter([
                    (wheel.hue.clone(), hue_spec.snapped(f64::from(degrees))),
                    (
                        wheel.saturation.clone(),
                        saturation_spec.snapped(f64::from(radius) * saturation_spec.max),
                    ),
                ]);
                self.controls_moved(action, wheel.hue, values)
            }
            WheelEvent::Release => self.control_release(action, wheel.hue),
            WheelEvent::Reset => match wheel_reset(&self.modules, &wheel) {
                Some(reset) => self.reset_declared(action, wheel.hue, reset.action, reset.preset),
                None => Task::none(),
            },
        }
    }

    /// An arrow key on a focused wheel: Left and Right turn the hue, wrapping across the seam,
    /// and Up and Down move the saturation, by the field's step, ten with Shift or its fine step
    /// with Option. It drafts like a drag, keyed by the hue field, and the key's release commits.
    pub(crate) fn control_wheel_nudge(
        &mut self,
        action: String,
        hue: String,
        saturation: bool,
        direction: i8,
        shift: bool,
        option: bool,
    ) -> Task<Message> {
        let Some(wheel) = wheel_of(&self.modules, &action, &hue).cloned() else {
            return Task::none();
        };
        let parameter = if saturation {
            wheel.saturation
        } else {
            wheel.hue.clone()
        };
        let Some(value) = self.nudged_value(&action, &parameter, direction, shift, option) else {
            return Task::none();
        };
        // Keyed by the hue whichever field moved, so a hue nudge and a saturation nudge are one
        // gesture and the key's release, which names the hue, commits it.
        let values = serde_json::Map::from_iter([(parameter, value)]);
        self.controls_moved(action, wheel.hue, values)
    }

    /// The reset of the group at `path` in the module's section, as the view model resolved it.
    /// A section that draws no controls holds none, so a group named in one is resolved from the
    /// controls its module would show, built now by the function the derive uses.
    fn group_reset_of(&self, module_id: &str, path: &[usize]) -> Option<tools::ResetRef> {
        let section = self
            .workspace
            .tools
            .all()
            .find(|section| section.module_id == module_id)?;
        section.group_reset(path).cloned().or_else(|| {
            if section.shows_controls() {
                return None;
            }
            let controls = tools::controls_of(section, &self.inputs());
            tools::group_reset_in(&controls, path).cloned()
        })
    }

    /// Ask for one missing displayed curve at a time. The query channel carries no timer and one
    /// in-flight request plus one replaceable pending request at most.
    ///
    /// Which curves are displayed is the derived tools panel's answer
    /// ([`tools::SectionModel::shown_curves`]), so this runs after the screen is derived: a
    /// section's collapse, its tabs, developer filtering, the photo's source kind and each
    /// control's resolved variant are the rules the panel drew it by.
    pub(crate) fn request_visible_curve_samples(&mut self) -> Task<Message> {
        if !self.curve_sampling.slot.idle() {
            return Task::none();
        }
        let (Some(entry), Some(asset)) = (
            self.displayed_entry(),
            self.document.state.as_ref().map(|state| &state.asset.id),
        ) else {
            return Task::none();
        };
        let wanted = self
            .workspace
            .tools
            .all()
            .flat_map(tools::SectionModel::shown_curves)
            .find_map(|curve| {
                let channel = curve.selected_channel;
                let parameter = &curve.channels.get(channel)?.parameter;
                let value = self.control_field_value(&curve.action, parameter)?;
                let key = (curve.action.clone(), parameter.clone());
                let sampled = self
                    .controls
                    .ui
                    .curve(&curve.id)
                    .and_then(|local| local.samples.get(parameter))
                    .is_some_and(|samples| {
                        samples.source == value && samples.entry == entry && &samples.asset == asset
                    });
                let requested = self.curve_sampling.requested_source.get(&key).is_some_and(
                    |(requested_asset, requested_entry, points)| {
                        requested_asset == asset && requested_entry == &entry && points == &value
                    },
                );
                (!sampled && !requested).then_some((key, channel, value))
            });
        match wanted {
            Some(((action, parameter), channel, value)) => {
                self.request_curve_samples(&action, &parameter, channel, value)
            }
            None => Task::none(),
        }
    }
    pub(crate) fn control_release(&mut self, action: String, parameter: String) -> Task<Message> {
        if let Some((drafting, field)) = self.drafting_control() {
            if drafting != action || field != parameter {
                return Task::none();
            }
            return self.release();
        }
        if tools::drafts(&self.modules, &action, &parameter) {
            return self.release_without_draft(&action, &parameter);
        }
        self.dispatch(Message::Control(ControlMessage::Submit {
            action,
            parameter: Some(parameter),
        }))
    }
    pub(crate) fn control_field_value(&self, action: &str, parameter: &str) -> Option<Value> {
        let declared = tools::declared_action(&self.modules, action)?.parameter(parameter)?;
        self.controls.fields.get_value(action, declared).ok()
    }

    pub(crate) fn set_control_field_value(&mut self, action: &str, parameter: &str, value: &Value) {
        if let Some(declared) =
            tools::declared_action(&self.modules, action).and_then(|a| a.parameter(parameter))
        {
            let _ = self.controls.fields.set_value(action, declared, value);
        }
    }
    pub(crate) fn control_fraction(
        &mut self,
        action: String,
        parameter: String,
        fraction: f64,
    ) -> Task<Message> {
        let Some(spec) = self.number_spec(&action, &parameter) else {
            return Task::none();
        };
        self.control_value(action, parameter, spec.at_fraction(fraction), true)
    }

    /// The number spec of one declared number or integer parameter.
    fn number_spec(&self, action: &str, parameter: &str) -> Option<NumberSpec> {
        fields::declared(&self.modules, action, parameter).and_then(NumberSpec::of)
    }

    /// `parameter`'s value one nudge on from what its field shows ([`NumberSpec::nudged`]). The
    /// hue a wheel binds wraps across the seam, since the core declares its 0 and 360 the same
    /// direction.
    fn nudged_value(
        &self,
        action: &str,
        parameter: &str,
        direction: i8,
        shift: bool,
        option: bool,
    ) -> Option<Value> {
        let mut spec = self.number_spec(action, parameter)?;
        if wheel_of(&self.modules, action, parameter).is_some() {
            spec = spec.wrapping();
        }
        let current = self
            .control_field_value(action, parameter)
            .and_then(|v| v.as_f64())
            .unwrap_or(spec.min);
        Some(spec.value(spec.nudged(current, direction, shift, option)))
    }

    pub(crate) fn control_value(
        &mut self,
        action: String,
        parameter: String,
        value: Value,
        continuous: bool,
    ) -> Task<Message> {
        if continuous && tools::drafts(&self.modules, &action, &parameter) {
            return self.control_moved(action, parameter, value);
        }
        // A discrete value commits at once, and a continuous one that does not draft commits on
        // release: either is refused before the control shows a value that was never sent.
        if let Some(reason) = self.control_refusal(&action, &parameter, continuous) {
            self.status.text = reason;
            return Task::none();
        }
        self.set_control_field_value(&action, &parameter, &value);
        if continuous {
            self.controls.dragging = Some((action, parameter));
            return Task::none();
        }
        self.controls.dragging = None;
        self.controls.editing = None;
        self.dispatch(Message::Action(ActionMessage::Run {
            action,
            preset: serde_json::Map::from_iter([(parameter, value)]),
        }))
    }

    pub(crate) fn control_step(
        &mut self,
        action: String,
        parameter: String,
        direction: i8,
    ) -> Task<Message> {
        let drafts = tools::drafts(&self.modules, &action, &parameter);
        // Refused as a whole, so a refused step never releases a gesture it did not open.
        if let Some(reason) = self.control_refusal(&action, &parameter, drafts) {
            self.status.text = reason;
            return Task::none();
        }
        let Some(value) = self.nudged_value(&action, &parameter, direction, false, false) else {
            return Task::none();
        };
        let task = self.control_value(action, parameter, value, drafts);
        if drafts {
            Task::batch([task, self.release()])
        } else {
            task
        }
    }

    pub(crate) fn control_key_nudge(
        &mut self,
        action: String,
        parameter: String,
        direction: i8,
        shift: bool,
        option: bool,
    ) -> Task<Message> {
        let Some(value) = self.nudged_value(&action, &parameter, direction, shift, option) else {
            return Task::none();
        };
        self.control_value(action, parameter, value, true)
    }

    /// A field without a rail edits its text on arrows; only Enter may submit the action.
    pub(crate) fn control_field_nudge(
        &mut self,
        action: String,
        parameter: String,
        direction: i8,
        shift: bool,
        option: bool,
    ) -> Task<Message> {
        let Some(spec) = self.number_spec(&action, &parameter) else {
            return Task::none();
        };
        let Some(current) = self
            .control_field_value(&action, &parameter)
            .and_then(|v| v.as_f64())
        else {
            self.status.text = "Correct the field before stepping it".into();
            return Task::none();
        };
        let next = spec.nudged(current, direction, shift, option);
        let text = if spec.integer {
            (next.round() as i64).to_string()
        } else {
            number_text(next)
        };
        self.controls.fields.set(&action, &parameter, text);
        let id = crate::state::fields::field_id(&action, &parameter, None);
        self.controls.editing = Some((action, parameter));
        iced::widget::operation::focus(iced::widget::Id::from(id))
    }

    pub(crate) fn control_picker(
        &mut self,
        action: String,
        parameter: String,
        event: ColorPickerEvent,
    ) -> Task<Message> {
        let key = (action.clone(), parameter.clone());
        let rgb = self
            .control_field_value(&action, &parameter)
            .and_then(|v| rgb_from_value(&v))
            .unwrap_or([0, 0, 0]);
        match event {
            ColorPickerEvent::Plane([s, v]) => {
                let mut hsv = self.picker_hsv(&key, rgb);
                hsv[1] = picker_fraction(s);
                hsv[2] = picker_fraction(v);
                self.picker_fraction_changed(action, parameter, rgb, hsv)
            }
            ColorPickerEvent::Hue(h) => {
                let mut hsv = self.picker_hsv(&key, rgb);
                hsv[0] = picker_fraction(h);
                self.picker_fraction_changed(action, parameter, rgb, hsv)
            }
            ColorPickerEvent::Release => {
                if self
                    .drafting_control()
                    .is_some_and(|(drafting, field)| drafting == action && field == parameter)
                    || self.controls.dragging.as_ref() == Some(&key)
                {
                    self.control_release(action, parameter)
                } else {
                    Task::none()
                }
            }
            ColorPickerEvent::Text { field, text } => {
                if field == 3 {
                    self.controls.ui.color_mut(key).hex = Some(text);
                } else if field < 3 {
                    self.controls.ui.color_mut(key).channels[field] = Some(text);
                }
                Task::none()
            }
            ColorPickerEvent::Submit(field) => {
                let local = self.controls.ui.color(&key);
                let next = if field == 3 {
                    local
                        .and_then(|local| local.hex.as_deref())
                        .and_then(hex_to_rgb)
                } else if field < 3 {
                    local
                        .and_then(|local| local.channels[field].as_ref())
                        .and_then(|text| text.parse::<u8>().ok())
                        .map(|channel| {
                            let mut next = rgb;
                            next[field] = channel;
                            next
                        })
                } else {
                    None
                };
                match next {
                    Some(next) => {
                        let local = self.controls.ui.color_mut(key);
                        if field == 3 {
                            local.hex = None;
                        } else {
                            local.channels[field] = None;
                        }
                        self.control_value(action, parameter, json!(next), false)
                    }
                    None => {
                        self.status.text =
                            "RGB channels must be 0 to 255 or a six-digit hex colour".into();
                        Task::none()
                    }
                }
            }
            ColorPickerEvent::Reset => {
                self.dispatch(Message::Control(ControlMessage::ResetField {
                    action,
                    parameter,
                }))
            }
        }
    }

    fn picker_hsv(&self, key: &(String, String), rgb: [u8; 3]) -> [f64; 3] {
        self.controls
            .ui
            .color(key)
            .and_then(|local| local.hsv)
            .filter(|picker| picker.rgb == rgb)
            .map(|picker| picker.hsv)
            .unwrap_or_else(|| rgb_to_hsv(rgb))
    }

    fn picker_fraction_changed(
        &mut self,
        action: String,
        parameter: String,
        rgb: [u8; 3],
        hsv: [f64; 3],
    ) -> Task<Message> {
        if let Some(reason) = self.control_refusal(&action, &parameter, true) {
            self.status.text = reason;
            return Task::none();
        }
        let next = hsv_to_rgb(hsv);
        self.controls
            .ui
            .color_mut((action.clone(), parameter.clone()))
            .hsv = Some(tools::PickerHsv { rgb: next, hsv });
        if next == rgb {
            // Gray and black have no representable hue in RGB. Remember the selected fraction
            // without opening a draft or committing an edit that changes nothing.
            Task::none()
        } else {
            self.control_value(action, parameter, json!(next), true)
        }
    }

    pub(crate) fn control_curve(
        &mut self,
        action: String,
        parameter: String,
        event: CurveEditorEvent,
    ) -> Task<Message> {
        let mut points = self
            .control_field_value(&action, &parameter)
            .and_then(|v| curve_from_value(&v))
            .unwrap_or_default();
        let declared = tools::declared_action(&self.modules, &action)
            .and_then(|a| a.parameter(&parameter))
            .cloned();
        let kind = declared.as_ref().map(|p| p.kind.clone());
        let (min, max, fixed_x, monotone) = match kind {
            Some(ParameterKind::Curve {
                points_min,
                points_max,
                fixed_x,
                monotone,
            }) => (points_min, points_max, fixed_x, monotone),
            _ => return Task::none(),
        };
        let curve_key = curve_key(&self.modules, &action, &parameter);
        let channel = self
            .controls
            .ui
            .curve(&curve_key)
            .map_or(0, |local| local.channel);
        match event {
            CurveEditorEvent::Move { index, position } => {
                if index >= points.len() {
                    return Task::none();
                }
                let mut x = f64::from(position[0].clamp(0.0, 1.0));
                if fixed_x.is_some() {
                    x = points[index][0];
                } else {
                    if index > 0 {
                        x = x.max(points[index - 1][0] + f64::EPSILON * 4.0);
                    }
                    if index + 1 < points.len() {
                        x = x.min(points[index + 1][0] - f64::EPSILON * 4.0);
                    }
                }
                let mut y = f64::from(position[1].clamp(0.0, 1.0));
                if monotone {
                    if index > 0 {
                        y = y.max(points[index - 1][1]);
                    }
                    if index + 1 < points.len() {
                        y = y.min(points[index + 1][1]);
                    }
                }
                points[index] = [x, y];
                self.curve_changed(action, parameter, points, true, channel, None)
            }
            CurveEditorEvent::Add(position) => {
                if fixed_x.is_some() {
                    return Task::none();
                }
                if points.len() >= max {
                    let label = curve_label(&self.modules, &action, &parameter);
                    self.status.text = format!("{label} holds at most {max} points");
                    return Task::none();
                }
                let x = f64::from(position[0].clamp(0.0, 1.0));
                let mut y = f64::from(position[1].clamp(0.0, 1.0));
                if monotone {
                    if let Some(lower) = points.iter().rev().find(|p| p[0] < x) {
                        y = y.max(lower[1]);
                    }
                    if let Some(upper) = points.iter().find(|p| p[0] > x) {
                        y = y.min(upper[1]);
                    }
                }
                let p = [x, y];
                if points.iter().any(|current| current[0] == p[0]) {
                    return Task::none();
                }
                points.push(p);
                points.sort_by(|a, b| a[0].total_cmp(&b[0]));
                // The new point is selected, so the next nudge or Delete acts on it.
                let added = points.iter().position(|current| *current == p);
                self.curve_changed(action, parameter, points, false, channel, Some(added))
            }
            // Every desktop removal arrives here: a double-click on a point, Delete or Backspace
            // on the selected point, and a point row's remove button. The API is not limited to
            // interior points; the desktop keeps the end points, which move but are not removed.
            CurveEditorEvent::Remove(index) => {
                if fixed_x.is_some() || index >= points.len() {
                    return Task::none();
                }
                if points.len() <= min {
                    let label = curve_label(&self.modules, &action, &parameter);
                    self.status.text = format!("{label} needs at least {min} points");
                    return Task::none();
                }
                if index == 0 || index + 1 == points.len() {
                    self.status.text = "The end points move but are not removed".into();
                    return Task::none();
                }
                points.remove(index);
                self.curve_changed(action, parameter, points, false, channel, Some(None))
            }
            CurveEditorEvent::Points(open) => {
                let local = self.controls.ui.curve_mut(curve_key);
                local.points_open = open;
                if !open {
                    // Closing the list drops what was typed in it and never committed.
                    local.edits.clear();
                }
                Task::none()
            }
            CurveEditorEvent::Select(index) => {
                if index >= points.len() {
                    return Task::none();
                }
                self.controls.ui.curve_mut(curve_key).point = Some(index);
                Task::none()
            }
            CurveEditorEvent::Channel(index) => {
                let Some(parameters) = curve_channel_parameters(&self.modules, &action, &parameter)
                else {
                    return Task::none();
                };
                let Some(next_parameter) = parameters.get(index) else {
                    return Task::none();
                };
                let next_parameter = next_parameter.clone();
                let local = self.controls.ui.curve_mut(curve_key);
                local.channel = index;
                local.point = None;
                let Some(value) = self.control_field_value(&action, &next_parameter) else {
                    return Task::none();
                };
                self.request_curve_samples(&action, &next_parameter, index, value)
            }
            CurveEditorEvent::Release => self.control_release(action, parameter),
            CurveEditorEvent::Cancel if self.slider_gesture().is_some() => self.discard(),
            CurveEditorEvent::Cancel => Task::none(),
            CurveEditorEvent::Nudge {
                index,
                dx,
                dy,
                shift,
                option,
            } => {
                if index >= points.len() {
                    return Task::none();
                }
                let normal = declared.as_ref().and_then(|p| p.step).unwrap_or(0.01);
                let base = if option {
                    declared
                        .as_ref()
                        .and_then(|p| p.fine_step)
                        .unwrap_or(normal / 10.0)
                } else if shift {
                    normal * 10.0
                } else {
                    normal
                };
                let position = [
                    (points[index][0] + f64::from(dx) * base).clamp(0.0, 1.0) as f32,
                    (points[index][1] + f64::from(dy) * base).clamp(0.0, 1.0) as f32,
                ];
                self.control_curve(
                    action,
                    parameter,
                    CurveEditorEvent::Move { index, position },
                )
            }
            CurveEditorEvent::Text { index, axis, text } => {
                self.controls
                    .ui
                    .curve_mut(curve_key)
                    .edits
                    .insert((parameter, index, axis), text);
                Task::none()
            }
            CurveEditorEvent::Submit { index, axis } => {
                let Some(text) = self
                    .controls
                    .ui
                    .curve(&curve_key)
                    .and_then(|local| local.edits.get(&(parameter.clone(), index, axis)))
                else {
                    return Task::none();
                };
                let Ok(value) = text.parse::<f64>() else {
                    self.status.text = "Curve coordinate must be a number from 0 to 1".into();
                    return Task::none();
                };
                if !(0.0..=1.0).contains(&value) || index >= points.len() || axis > 1 {
                    self.status.text = "Curve coordinate must be from 0 to 1".into();
                    return Task::none();
                }
                points[index][axis] = value;
                if let Some(declared) = declared.as_ref()
                    && let Err(error) = check_value(declared, &json!(points))
                {
                    self.status.text = error.to_string();
                    return Task::none();
                }
                self.controls.ui.curve_mut(curve_key).edits.remove(&(
                    parameter.clone(),
                    index,
                    axis,
                ));
                self.curve_changed(action, parameter, points, false, channel, None)
            }
        }
    }

    /// Send a curve's new points. `select`, when given, is the point selected once the change is
    /// accepted (`Some(None)` clears the selection); a refused change keeps the selection it had.
    fn curve_changed(
        &mut self,
        action: String,
        parameter: String,
        points: Vec<[f64; 2]>,
        continuous: bool,
        channel: usize,
        select: Option<Option<usize>>,
    ) -> Task<Message> {
        let value = json!(points);
        if let Some(reason) = self.control_refusal(&action, &parameter, continuous) {
            self.status.text = reason;
            return Task::none();
        }
        let Some(declared) =
            tools::declared_action(&self.modules, &action).and_then(|a| a.parameter(&parameter))
        else {
            return Task::none();
        };
        if let Err(error) = check_value(declared, &value) {
            self.status.text = error.to_string();
            return Task::none();
        }
        let key = curve_key(&self.modules, &action, &parameter);
        if let Some(point) = select {
            self.controls.ui.curve_mut(key.clone()).point = point;
        }
        self.controls.ui.forget_curve_samples(&key, &parameter);
        let query = self.request_curve_samples(&action, &parameter, channel, value.clone());
        Task::batch([
            self.control_value(action, parameter, value, continuous),
            query,
        ])
    }

    pub(crate) fn request_curve_samples(
        &mut self,
        action: &str,
        parameter: &str,
        channel: usize,
        points: Value,
    ) -> Task<Message> {
        let Some((query, _, _)) = curve_query(&self.modules, action, parameter) else {
            return Task::none();
        };
        let Some(state) = &self.document.state else {
            return Task::none();
        };
        let Some(entry) = self.displayed_entry() else {
            return Task::none();
        };
        self.curve_sampling.sequence = self.curve_sampling.sequence.wrapping_add(1);
        let identity = CurveSampleIdentity {
            sequence: self.curve_sampling.sequence,
            asset: state.asset.id.clone(),
            entry,
            action: action.into(),
            parameter: parameter.into(),
            channel,
            points,
        };
        self.curve_sampling
            .requested
            .insert((action.into(), parameter.into()), identity.sequence);
        self.curve_sampling.requested_source.insert(
            (action.into(), parameter.into()),
            (
                identity.asset.clone(),
                identity.entry.clone(),
                identity.points.clone(),
            ),
        );
        let request = CurveSampleRequest { identity, query };
        if let Some(displaced) = self.curve_sampling.slot.offer(request) {
            let key = (displaced.identity.action, displaced.identity.parameter);
            if self.curve_sampling.requested.get(&key) == Some(&displaced.identity.sequence) {
                self.curve_sampling.requested.remove(&key);
                self.curve_sampling.requested_source.remove(&key);
            }
        }
        self.start_curve_sample()
    }

    /// Send the newest curve sample query waiting, once none is in flight.
    fn start_curve_sample(&mut self) -> Task<Message> {
        let Some(request) = self.curve_sampling.slot.start() else {
            return Task::none();
        };
        let owner = self.owner.clone();
        let client = self.client;
        let CurveSampleRequest { identity, query } = request;
        let sent = identity.clone();
        crate::app::tasks::owner_task(
            move || {
                let mut params = json!({"asset_id":sent.asset,"entry_id":sent.entry});
                params
                    .as_object_mut()
                    .unwrap()
                    .insert(sent.parameter.clone(), sent.points.clone());
                call(&owner, client, &format!("query.{query}"), params).map(|(value, _)| value)
            },
            move |result| {
                Message::Control(ControlMessage::CurveSampled {
                    identity: identity.clone(),
                    result,
                })
            },
        )
    }

    pub(crate) fn curve_sampled(
        &mut self,
        identity: CurveSampleIdentity,
        result: Result<Value, String>,
    ) -> Task<Message> {
        self.curve_sampling.slot.answered();
        let key = curve_key(&self.modules, &identity.action, &identity.parameter);
        let valid = self
            .curve_sampling
            .requested
            .get(&(identity.action.clone(), identity.parameter.clone()))
            == Some(&identity.sequence)
            && self
                .document
                .state
                .as_ref()
                .is_some_and(|state| state.asset.id == identity.asset)
            && self.displayed_entry() == Some(identity.entry.clone())
            && self.control_field_value(&identity.action, &identity.parameter)
                == Some(identity.points.clone())
            && self
                .controls
                .ui
                .curve(&key)
                .map_or(0, |local| local.channel)
                == identity.channel;
        if valid {
            match result {
                Ok(value) => {
                    if let Some(points) = value.get("points").and_then(sampled_from_value) {
                        self.controls.ui.curve_mut(key).samples.insert(
                            identity.parameter,
                            tools::CurveSamples {
                                asset: identity.asset,
                                entry: identity.entry,
                                points: points
                                    .into_iter()
                                    .map(|[x, y]| [x as f32, y as f32])
                                    .collect(),
                                version: identity.sequence,
                                source: identity.points,
                            },
                        );
                    } else {
                        self.status.text =
                            "Curve sample query returned an invalid point list".into();
                    }
                }
                Err(error) => self.status.text = error,
            }
        }
        self.start_curve_sample()
    }
}

/// The wheel a module declares over `hue` of `action`, wherever it sits in its controls.
pub(crate) fn wheel_of<'a>(
    modules: &'a [luxforge_core::ModuleDescriptor],
    action: &str,
    hue: &str,
) -> Option<&'a luxforge_core::WheelControl> {
    modules.iter().find_map(|module| {
        walk(&module.controls).find_map(|control| match control {
            Control::Wheel(wheel) if wheel.action == action && wheel.hue == hue => Some(wheel),
            _ => None,
        })
    })
}

/// What resetting `wheel` runs: its fields back to the defaults the module declaring its action
/// gives them ([`luxforge_core::ModuleDescriptor::wheel_reset`]).
pub(crate) fn wheel_reset(
    modules: &[luxforge_core::ModuleDescriptor],
    wheel: &luxforge_core::WheelControl,
) -> Option<luxforge_core::ResetAction> {
    modules
        .iter()
        .find(|module| module.action(&wheel.action).is_some())?
        .wheel_reset(wheel)
}

fn picker_fraction(fraction: f32) -> f64 {
    if fraction.is_finite() {
        f64::from(fraction.clamp(0.0, 1.0))
    } else {
        0.0
    }
}
fn rgb_from_value(value: &Value) -> Option<[u8; 3]> {
    let values = value.as_array()?;
    Some([
        values.first()?.as_u64()? as u8,
        values.get(1)?.as_u64()? as u8,
        values.get(2)?.as_u64()? as u8,
    ])
}
fn curve_from_value(value: &Value) -> Option<Vec<[f64; 2]>> {
    value
        .as_array()?
        .iter()
        .map(|point| {
            let pair = point.as_array()?;
            Some([pair.first()?.as_f64()?, pair.get(1)?.as_f64()?])
        })
        .collect()
}
fn sampled_from_value(value: &Value) -> Option<Vec<[f64; 2]>> {
    let array = value.as_array()?;
    if array.len() < 2 || array.len() > 1024 {
        return None;
    }
    let mut output = Vec::with_capacity(array.len());
    let mut previous_x = None;
    for point in array {
        let pair = point.as_array().filter(|pair| pair.len() == 2)?;
        let x = pair[0].as_f64()?;
        let y = pair[1].as_f64()?;
        if !x.is_finite()
            || !y.is_finite()
            || !(0.0..=1.0).contains(&x)
            || !(0.0..=1.0).contains(&y)
            || previous_x.is_some_and(|px| x <= px)
        {
            return None;
        }
        output.push([x, y]);
        previous_x = Some(x);
    }
    Some(output)
}
fn curve_query(
    modules: &[luxforge_core::ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<(String, usize, String)> {
    modules.iter().find_map(|module| {
        walk(&module.controls).find_map(|control| match control {
            Control::Curve(luxforge_core::CurveControl {
                action: a,
                channels,
                sample_query,
                ..
            }) if a == action => {
                let index = channels
                    .iter()
                    .position(|channel| channel.parameter == parameter)?;
                Some((
                    sample_query.clone(),
                    index,
                    channels.first()?.parameter.clone(),
                ))
            }
            _ => None,
        })
    })
}
/// The key the curve drawing `parameter` of `action` holds its local state under: the action and
/// its first channel ([`tools::ControlKey`]).
fn curve_key(
    modules: &[luxforge_core::ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> tools::ControlKey {
    let first = curve_query(modules, action, parameter).map(|(_, _, first)| first);
    (
        action.to_owned(),
        first.unwrap_or_else(|| parameter.to_owned()),
    )
}
/// The label of the curve control that draws `parameter` of `action`, which its refusals name.
fn curve_label(
    modules: &[luxforge_core::ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> String {
    modules
        .iter()
        .find_map(|module| {
            walk(&module.controls).find_map(|control| match control {
                Control::Curve(curve)
                    if curve.action == action
                        && curve.channels.iter().any(|c| c.parameter == parameter) =>
                {
                    Some(curve.label.clone())
                }
                _ => None,
            })
        })
        .unwrap_or_else(|| parameter.to_owned())
}
fn curve_channel_parameters(
    modules: &[luxforge_core::ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<Vec<String>> {
    modules.iter().find_map(|module| {
        walk(&module.controls).find_map(|control| match control {
            Control::Curve(luxforge_core::CurveControl {
                action: a,
                channels,
                ..
            }) if a == action && channels.iter().any(|c| c.parameter == parameter) => {
                Some(channels.iter().map(|c| c.parameter.clone()).collect())
            }
            _ => None,
        })
    })
}

pub(crate) fn initial_group_expanded(
    modules: &[luxforge_core::ModuleDescriptor],
    module_id: &str,
    path: &[usize],
) -> Option<bool> {
    let module = modules.iter().find(|module| module.id == module_id)?;
    match at_path(&module.controls, path)? {
        Control::Group(luxforge_core::GroupControl { collapsed, .. }) => Some(!collapsed),
        _ => None,
    }
}

/// After every message: a curve's samples describe the entry they were read on, so a change of
/// the displayed entry drops them before the screen is derived.
pub(super) fn after_message(editor: &mut Editor, before: &Before) -> Task<Message> {
    if editor.displayed_entry() != before.entry {
        editor.controls.ui.clear_curve_samples();
        editor.curve_sampling.requested_source.clear();
    }
    // A value typed but not submitted belongs to the mask or component it was typed for. When the
    // fields address another one, the edit is dropped and the fields show the new target's own
    // values, so a later Enter cannot land it there.
    if editor.field_target() != before.field_target {
        editor.controls.editing = None;
        editor.seed_values();
        editor.seed_mask_fields();
    }
    Task::none()
}

/// After the screen is derived: a curve is sampled once it is on screen, which the derived tools
/// panel says ([`Editor::request_visible_curve_samples`]).
pub(super) fn after_derive(editor: &mut Editor) -> Task<Message> {
    Task::batch([
        editor.request_visible_curve_samples(),
        editor.request_visible_query_choices(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampled_curve_is_bounded_and_rejects_bad_fractions() {
        assert_eq!(
            sampled_from_value(&json!([[0.0, 0.0], [1.0, 1.0]]))
                .unwrap()
                .len(),
            2
        );
        assert!(sampled_from_value(&json!([[0.0, 0.0], [0.0, 1.0]])).is_none());
        assert!(sampled_from_value(&json!([[0.0, 0.0], [1.0, 1.1]])).is_none());
        assert!(sampled_from_value(&Value::Array(vec![json!([0.0, 0.0]); 1025])).is_none());
    }
}
