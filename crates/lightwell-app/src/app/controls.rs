//! Host mapping for generated controls. Widgets report fractions and events; descriptors own values.

use crate::app::{
    Editor,
    message::{ActionMessage, ControlMessage, Message},
    tasks::call,
};
use crate::state::{
    control_tree::{at_path, walk},
    fields::{self, submit_preset},
    number::{NumberSpec, number_text},
    tools,
};
use iced::{Task, widget::operation};
use lightwell_core::{AssetId, Control, EntryId, ParameterKind, check_value};
use lightwell_ui::{ColorPickerEvent, CurveEditorEvent, hex_to_rgb, hsv_to_rgb, rgb_to_hsv};
use serde_json::{Value, json};

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

impl Editor {
    /// One generated-control or tools-panel section message.
    pub(super) fn control_update(&mut self, message: ControlMessage) -> Task<Message> {
        match message {
            ControlMessage::Field {
                action,
                parameter,
                text,
            } => {
                self.fields.set(&action, &parameter, text);
                // Typing is editing: the field shows what was typed until it is committed.
                self.editing = Some((action, parameter));
            }
            ControlMessage::SliderMoved {
                action,
                parameter,
                value,
            } => {
                // A control whose one field is already a whole request drafts: a patch action's
                // field, or the only parameter its action declares. The move updates the field and
                // the draft's pending value, and the gated tick is the only thing that sends
                // anything. Every other slider keeps its old behaviour, which is to change the text
                // and nothing else until release.
                if tools::drafts(&self.modules, &action, &parameter) {
                    return self.slider_moved(action, parameter, value);
                }
                let text = fields::declared(&self.modules, &action, &parameter)
                    .and_then(NumberSpec::of)
                    .map_or_else(|| number_text(value), |spec| spec.format(value));
                self.fields.set(&action, &parameter, text);
                self.editing = None;
                self.dragging = Some((action, parameter));
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
                let open = self
                    .controls_ui
                    .color_open
                    .entry((action, parameter))
                    .or_default();
                *open = !*open;
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
                    .controls_ui
                    .group_expanded
                    .entry(key)
                    .or_insert(initial);
                *entry = !*entry;
            }
            ControlMessage::SelectTab { module_id, index } => {
                self.controls_ui.selected_tab.insert(module_id, index);
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
                self.editing = Some((action, parameter));
                self.seed_idle_angle();
                return operation::focus(iced::widget::Id::from(id));
            }
            ControlMessage::CancelEdit => self.editing = None,
            ControlMessage::SliderReleased { action, parameter } => {
                // Release ends the gesture: an open draft commits once, and a slider that never
                // drafted submits its own field exactly as Enter in that field does.
                if self.slider_gesture().is_some() {
                    return self.release();
                }
                if tools::drafts(&self.modules, &action, &parameter) {
                    return self.release_without_draft(&action, &parameter);
                }
                return self.dispatch(Message::Control(ControlMessage::Submit {
                    action,
                    parameter: Some(parameter),
                }));
            }
            ControlMessage::Submit { action, parameter } => {
                self.dragging = None;
                if !self.editable() {
                    return Task::none();
                }
                let preset =
                    match submit_preset(&self.modules, &action, parameter.as_deref(), &self.fields)
                    {
                        Ok(preset) => preset,
                        // The field only stops editing once the submit actually runs; a rejected
                        // submit leaves the typed text on screen with its reason rather than
                        // silently reverting to the last committed value.
                        Err(message) => {
                            self.status = message;
                            return Task::none();
                        }
                    };
                // Refused before the field lets go, so the typed text stays with its reason.
                if let Some(reason) = self.action_refusal(&action) {
                    self.status = reason;
                    return Task::none();
                }
                self.editing = None;
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
                self.expanded.insert(module_id, !expanded);
            }
            ControlMessage::ResetModule(module_id) => {
                let Some(reset) = tools::module_of(&self.modules, &module_id)
                    .and_then(|module| module.reset.clone())
                else {
                    self.status = format!("{module_id} declares no reset action");
                    return Task::none();
                };
                return self.dispatch(Message::Action(ActionMessage::Run {
                    action: reset.action,
                    preset: reset.preset,
                }));
            }
            ControlMessage::ResetGroup { module_id, path } => {
                let Some(reset) = tools::module_of(&self.modules, &module_id)
                    .and_then(|module| group_reset(&module.controls, &path))
                else {
                    self.status = format!("{module_id} declares no reset for that group");
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

    /// Ask for one missing displayed curve at a time. The query channel carries no timer and one
    /// in-flight request plus one replaceable pending request at most.
    pub(crate) fn request_visible_curve_samples(&mut self) -> Task<Message> {
        if self.curve_sample_in_flight || self.curve_sample_pending.is_some() {
            return Task::none();
        }
        let mut declared: Vec<(String, Vec<String>)> = Vec::new();
        let Some(entry) = self.displayed_entry() else {
            return Task::none();
        };
        for module in self.modules.iter().filter(|module| module.is_available()) {
            if self.expanded.get(&module.id).copied() == Some(false) {
                continue;
            }
            // A module's only group has no header and is always shown, whatever its declared or
            // recorded disclosure, so its curves are visible whenever the section is.
            let (controls, prefix) = match tools::headerless_group(module) {
                Some(children) => (children, Some(0)),
                None => (&module.controls[..], None),
            };
            let mut controls = walk(controls);
            while let Some(control) = controls.next() {
                match control {
                    Control::Group { collapsed, .. } => {
                        let path: Vec<usize> = prefix.into_iter().chain(controls.path()).collect();
                        let key = tools::group_key(&module.id, &path);
                        let expanded = self.controls_ui.group_expanded.get(&key).copied();
                        if !expanded.unwrap_or(!collapsed) {
                            controls.skip_children();
                        }
                    }
                    Control::Curve {
                        action, channels, ..
                    } => declared.push((
                        action.clone(),
                        channels.iter().map(|c| c.parameter.clone()).collect(),
                    )),
                    _ => {}
                }
            }
        }
        for (action, channels) in declared {
            let Some(first) = channels.first() else {
                continue;
            };
            let channel = self
                .controls_ui
                .curve_channels
                .get(&(action.clone(), first.clone()))
                .copied()
                .unwrap_or(0);
            let Some(parameter) = channels.get(channel) else {
                continue;
            };
            let Some(value) = self.control_field_value(&action, parameter) else {
                continue;
            };
            if self
                .controls_ui
                .curve_samples
                .get(&(action.clone(), parameter.clone()))
                .is_some_and(|samples| {
                    samples.source == value
                        && samples.entry == entry
                        && self
                            .state
                            .as_ref()
                            .is_some_and(|state| samples.asset == state.asset.id)
                })
                || self
                    .curve_sample_requested_source
                    .get(&(action.clone(), parameter.clone()))
                    .is_some_and(|(asset, requested_entry, points)| {
                        self.state
                            .as_ref()
                            .is_some_and(|state| asset == &state.asset.id)
                            && requested_entry == &entry
                            && points == &value
                    })
            {
                continue;
            }
            return self.request_curve_samples(&action, parameter, channel, value);
        }
        Task::none()
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
        self.fields.get_value(action, declared).ok()
    }

    pub(crate) fn set_control_field_value(&mut self, action: &str, parameter: &str, value: &Value) {
        if let Some(declared) =
            tools::declared_action(&self.modules, action).and_then(|a| a.parameter(parameter))
        {
            let _ = self.fields.set_value(action, declared, value);
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
        if !self.editable() {
            return Task::none();
        }
        // A discrete value commits at once: refused under an open draft before the control shows a
        // value that was never sent.
        if !continuous && let Some(reason) = self.action_refusal(&action) {
            self.status = reason;
            return Task::none();
        }
        self.set_control_field_value(&action, &parameter, &value);
        if continuous {
            self.dragging = Some((action, parameter));
            return Task::none();
        }
        self.dragging = None;
        self.editing = None;
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
        if self
            .drafting_control()
            .is_some_and(|(drafting, field)| drafting != action || field != parameter)
        {
            return Task::none();
        }
        let Some(spec) = self.number_spec(&action, &parameter) else {
            return Task::none();
        };
        let current = self
            .control_field_value(&action, &parameter)
            .and_then(|v| v.as_f64())
            .unwrap_or(spec.min);
        let value = spec.value(spec.nudged(current, direction, false, false));
        let drafts = tools::drafts(&self.modules, &action, &parameter);
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
        let Some(spec) = self.number_spec(&action, &parameter) else {
            return Task::none();
        };
        let current = self
            .control_field_value(&action, &parameter)
            .and_then(|v| v.as_f64())
            .unwrap_or(spec.min);
        let value = spec.value(spec.nudged(current, direction, shift, option));
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
            self.status = "Correct the field before stepping it".into();
            return Task::none();
        };
        let next = spec.nudged(current, direction, shift, option);
        let text = if spec.integer {
            (next.round() as i64).to_string()
        } else {
            number_text(next)
        };
        self.fields.set(&action, &parameter, text);
        let id = crate::state::fields::field_id(&action, &parameter, None);
        self.editing = Some((action, parameter));
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
                    || self.dragging.as_ref() == Some(&key)
                {
                    self.control_release(action, parameter)
                } else {
                    Task::none()
                }
            }
            ColorPickerEvent::Text { field, text } => {
                if field == 3 {
                    self.controls_ui.color_hex.insert(key, text);
                } else if field < 3 {
                    self.controls_ui
                        .color_channels
                        .insert((action, parameter, field), text);
                }
                Task::none()
            }
            ColorPickerEvent::Submit(field) => {
                let next = if field == 3 {
                    self.controls_ui
                        .color_hex
                        .get(&key)
                        .and_then(|text| hex_to_rgb(text))
                } else if field < 3 {
                    self.controls_ui
                        .color_channels
                        .get(&(action.clone(), parameter.clone(), field))
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
                        if field == 3 {
                            self.controls_ui.color_hex.remove(&key);
                        } else {
                            self.controls_ui.color_channels.remove(&(
                                action.clone(),
                                parameter.clone(),
                                field,
                            ));
                        }
                        self.control_value(action, parameter, json!(next), false)
                    }
                    None => {
                        self.status =
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
        self.controls_ui
            .picker_hsv
            .get(key)
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
        if !self.editable() {
            return Task::none();
        }
        let next = hsv_to_rgb(hsv);
        self.controls_ui.picker_hsv.insert(
            (action.clone(), parameter.clone()),
            tools::PickerHsv { rgb: next, hsv },
        );
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
        let curve_key = curve_query(&self.modules, &action, &parameter)
            .map(|(_, _, first)| (action.clone(), first))
            .unwrap_or((action.clone(), parameter.clone()));
        let channel = self
            .controls_ui
            .curve_channels
            .get(&curve_key)
            .copied()
            .unwrap_or(0);
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
                self.curve_changed(action, parameter, points, true, channel)
            }
            CurveEditorEvent::Add(position) => {
                if fixed_x.is_some() || points.len() >= max {
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
                self.curve_changed(action, parameter, points, false, channel)
            }
            CurveEditorEvent::Remove(index) => {
                if fixed_x.is_some() || points.len() <= min || index >= points.len() {
                    return Task::none();
                }
                points.remove(index);
                self.curve_changed(action, parameter, points, false, channel)
            }
            CurveEditorEvent::Select(index) => {
                if index >= points.len() {
                    return Task::none();
                }
                self.controls_ui.curve_points.insert(curve_key, index);
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
                self.controls_ui.curve_channels.insert(curve_key, index);
                self.controls_ui
                    .curve_points
                    .remove(&(action.clone(), parameters[0].clone()));
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
                self.controls_ui
                    .curve_edits
                    .insert((action, parameter, index, axis), text);
                Task::none()
            }
            CurveEditorEvent::Submit { index, axis } => {
                let Some(text) = self.controls_ui.curve_edits.get(&(
                    action.clone(),
                    parameter.clone(),
                    index,
                    axis,
                )) else {
                    return Task::none();
                };
                let Ok(value) = text.parse::<f64>() else {
                    self.status = "Curve coordinate must be a number from 0 to 1".into();
                    return Task::none();
                };
                if !(0.0..=1.0).contains(&value) || index >= points.len() || axis > 1 {
                    self.status = "Curve coordinate must be from 0 to 1".into();
                    return Task::none();
                }
                points[index][axis] = value;
                if let Some(declared) = declared.as_ref()
                    && let Err(error) = check_value(declared, &json!(points))
                {
                    self.status = error.to_string();
                    return Task::none();
                }
                self.controls_ui.curve_edits.remove(&(
                    action.clone(),
                    parameter.clone(),
                    index,
                    axis,
                ));
                self.curve_changed(action, parameter, points, false, channel)
            }
        }
    }

    fn curve_changed(
        &mut self,
        action: String,
        parameter: String,
        points: Vec<[f64; 2]>,
        continuous: bool,
        channel: usize,
    ) -> Task<Message> {
        let value = json!(points);
        if !self.editable() {
            return Task::none();
        }
        if self
            .drafting_control()
            .is_some_and(|(drafting, field)| drafting != action || field != parameter)
        {
            return Task::none();
        }
        let Some(declared) =
            tools::declared_action(&self.modules, &action).and_then(|a| a.parameter(&parameter))
        else {
            return Task::none();
        };
        if let Err(error) = check_value(declared, &value) {
            self.status = error.to_string();
            return Task::none();
        }
        self.controls_ui
            .curve_samples
            .remove(&(action.clone(), parameter.clone()));
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
        let Some(state) = &self.state else {
            return Task::none();
        };
        let Some(entry) = self.displayed_entry() else {
            return Task::none();
        };
        self.curve_sample_sequence = self.curve_sample_sequence.wrapping_add(1);
        let identity = CurveSampleIdentity {
            sequence: self.curve_sample_sequence,
            asset: state.asset.id.clone(),
            entry,
            action: action.into(),
            parameter: parameter.into(),
            channel,
            points,
        };
        self.curve_sample_requested
            .insert((action.into(), parameter.into()), identity.sequence);
        self.curve_sample_requested_source.insert(
            (action.into(), parameter.into()),
            (
                identity.asset.clone(),
                identity.entry.clone(),
                identity.points.clone(),
            ),
        );
        let request = CurveSampleRequest { identity, query };
        if self.curve_sample_in_flight {
            if let Some(displaced) = self.curve_sample_pending.replace(request) {
                let key = (displaced.identity.action, displaced.identity.parameter);
                if self.curve_sample_requested.get(&key) == Some(&displaced.identity.sequence) {
                    self.curve_sample_requested.remove(&key);
                    self.curve_sample_requested_source.remove(&key);
                }
            }
            return Task::none();
        }
        self.send_curve_sample_request(request)
    }

    fn send_curve_sample_request(&mut self, request: CurveSampleRequest) -> Task<Message> {
        self.curve_sample_in_flight = true;
        let owner = self.owner.clone();
        let client = self.client;
        let CurveSampleRequest { identity, query } = request;
        let sent = identity.clone();
        Task::perform(
            async move {
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
        self.curve_sample_in_flight = false;
        let valid = self
            .curve_sample_requested
            .get(&(identity.action.clone(), identity.parameter.clone()))
            == Some(&identity.sequence)
            && self
                .state
                .as_ref()
                .is_some_and(|state| state.asset.id == identity.asset)
            && self.displayed_entry() == Some(identity.entry.clone())
            && self.control_field_value(&identity.action, &identity.parameter)
                == Some(identity.points.clone())
            && curve_query(&self.modules, &identity.action, &identity.parameter)
                .and_then(|(_, _, first)| {
                    self.controls_ui
                        .curve_channels
                        .get(&(identity.action.clone(), first))
                        .copied()
                })
                .unwrap_or(0)
                == identity.channel;
        if valid {
            match result {
                Ok(value) => {
                    if let Some(points) = value.get("points").and_then(sampled_from_value) {
                        self.controls_ui.curve_samples.insert(
                            (identity.action, identity.parameter),
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
                        self.status = "Curve sample query returned an invalid point list".into();
                    }
                }
                Err(error) => self.status = error,
            }
        }
        if let Some(next) = self.curve_sample_pending.take() {
            self.send_curve_sample_request(next)
        } else {
            Task::none()
        }
    }
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
    modules: &[lightwell_core::ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<(String, usize, String)> {
    modules.iter().find_map(|module| {
        walk(&module.controls).find_map(|control| match control {
            Control::Curve {
                action: a,
                channels,
                sample_query,
                ..
            } if a == action => {
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
fn curve_channel_parameters(
    modules: &[lightwell_core::ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<Vec<String>> {
    modules.iter().find_map(|module| {
        walk(&module.controls).find_map(|control| match control {
            Control::Curve {
                action: a,
                channels,
                ..
            } if a == action && channels.iter().any(|c| c.parameter == parameter) => {
                Some(channels.iter().map(|c| c.parameter.clone()).collect())
            }
            _ => None,
        })
    })
}

pub(crate) fn initial_group_expanded(
    modules: &[lightwell_core::ModuleDescriptor],
    module_id: &str,
    path: &[usize],
) -> Option<bool> {
    let module = modules.iter().find(|module| module.id == module_id)?;
    match at_path(&module.controls, path)? {
        Control::Group { collapsed, .. } => Some(!collapsed),
        _ => None,
    }
}

pub(super) fn group_reset(
    controls: &[lightwell_core::Control],
    path: &[usize],
) -> Option<lightwell_core::ResetAction> {
    match at_path(controls, path)? {
        Control::Group { reset, .. } => reset.clone(),
        _ => None,
    }
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
