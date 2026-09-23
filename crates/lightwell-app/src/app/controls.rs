//! Host mapping for generated controls. Widgets report fractions and events; descriptors own values.

use crate::app::{Editor, message::Message, tasks::call};
use crate::state::tools;
use iced::Task;
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
    /// Ask for one missing displayed curve at a time. The query channel carries no timer and one
    /// in-flight request plus one replaceable pending request at most.
    pub(crate) fn request_visible_curve_samples(&mut self) -> Task<Message> {
        if self.curve_sample_in_flight || self.curve_sample_pending.is_some() {
            return Task::none();
        }
        fn visit(
            module_id: &str,
            controls: &[Control],
            path: &mut Vec<usize>,
            ui: &tools::ControlsUi,
            output: &mut Vec<(String, Vec<String>)>,
        ) {
            for (index, control) in controls.iter().enumerate() {
                path.push(index);
                match control {
                    Control::Group {
                        controls,
                        collapsed,
                        ..
                    } => {
                        let key = tools::group_key(module_id, path);
                        if ui.group_expanded.get(&key).copied().unwrap_or(!collapsed) {
                            visit(module_id, controls, path, ui, output);
                        }
                    }
                    Control::Curve {
                        action, channels, ..
                    } => output.push((
                        action.clone(),
                        channels.iter().map(|c| c.parameter.clone()).collect(),
                    )),
                    _ => {}
                }
                path.pop();
            }
        }
        let mut declared = Vec::new();
        let Some(entry) = self.displayed_entry() else {
            return Task::none();
        };
        for module in self.modules.iter().filter(|module| module.is_available()) {
            if self.expanded.get(&module.id).copied() == Some(false) {
                continue;
            }
            // A module's only group has no header and is always shown, whatever its declared or
            // recorded disclosure, so its curves are visible whenever the section is.
            let (controls, mut path) = match tools::headerless_group(module) {
                Some(children) => (children, vec![0]),
                None => (&module.controls[..], Vec::new()),
            };
            visit(
                &module.id,
                controls,
                &mut path,
                &self.controls_ui,
                &mut declared,
            );
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
        if let Some(draft) = &self.slider_draft {
            if draft.action != action || draft.parameter != parameter {
                return Task::none();
            }
            self.slider_commit()
        } else {
            self.dispatch(Message::Submit {
                action,
                parameter: Some(parameter),
            })
        }
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
        let Some(declared) = tools::declared_action(&self.modules, &action)
            .and_then(|action| action.parameter(&parameter))
        else {
            return Task::none();
        };
        let fraction = fraction.clamp(0.0, 1.0);
        let (min, max, integer) = match declared.kind {
            ParameterKind::Number { .. } => (
                declared
                    .soft_min
                    .unwrap_or_else(|| hard_min(&declared.kind)),
                declared
                    .soft_max
                    .unwrap_or_else(|| hard_max(&declared.kind)),
                false,
            ),
            ParameterKind::Integer { .. } => (
                declared
                    .soft_min
                    .unwrap_or_else(|| hard_min(&declared.kind)),
                declared
                    .soft_max
                    .unwrap_or_else(|| hard_max(&declared.kind)),
                true,
            ),
            _ => return Task::none(),
        };
        let raw = min + fraction * (max - min);
        let step = declared.step.unwrap_or(if integer {
            1.0
        } else {
            tools::generic_step(min, max)
        });
        let fine_step = declared.fine_step.unwrap_or(step / 10.0);
        let value = lightwell_ui::geometry::quantize(
            raw,
            hard_min(&declared.kind),
            hard_max(&declared.kind),
            fine_step,
            crate::app::fields::fine_decimals_for(declared),
        );
        let value = if integer {
            Value::from(value.round() as i64)
        } else {
            Value::from(value)
        };
        self.control_value(action, parameter, value, true)
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
        self.set_control_field_value(&action, &parameter, &value);
        if continuous {
            self.dragging = Some((action, parameter));
            return Task::none();
        }
        self.dragging = None;
        self.editing = None;
        self.dispatch(Message::RunAction {
            action,
            preset: serde_json::Map::from_iter([(parameter, value)]),
        })
    }

    pub(crate) fn control_step(
        &mut self,
        action: String,
        parameter: String,
        direction: i8,
    ) -> Task<Message> {
        if self
            .slider_draft
            .as_ref()
            .is_some_and(|draft| draft.action != action || draft.parameter != parameter)
        {
            return Task::none();
        }
        let Some(declared) = tools::declared_action(&self.modules, &action)
            .and_then(|action| action.parameter(&parameter))
        else {
            return Task::none();
        };
        let (min, max, integer) = match declared.kind {
            ParameterKind::Number { .. } => {
                (hard_min(&declared.kind), hard_max(&declared.kind), false)
            }
            ParameterKind::Integer { .. } => {
                (hard_min(&declared.kind), hard_max(&declared.kind), true)
            }
            _ => return Task::none(),
        };
        let current = self
            .control_field_value(&action, &parameter)
            .and_then(|v| v.as_f64())
            .unwrap_or(min);
        let step = declared.step.unwrap_or(if integer { 1.0 } else { 0.01 });
        let next = (current + f64::from(direction.signum()) * step).clamp(min, max);
        let value = if integer {
            Value::from(next.round() as i64)
        } else {
            Value::from(next)
        };
        let drafts = tools::drafts(&self.modules, &action, &parameter);
        let task = self.control_value(action, parameter, value, drafts);
        if drafts {
            Task::batch([task, self.slider_commit()])
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
        let Some(declared) =
            tools::declared_action(&self.modules, &action).and_then(|a| a.parameter(&parameter))
        else {
            return Task::none();
        };
        let (min, max, integer) = match declared.kind {
            ParameterKind::Number { .. } => {
                (hard_min(&declared.kind), hard_max(&declared.kind), false)
            }
            ParameterKind::Integer { .. } => {
                (hard_min(&declared.kind), hard_max(&declared.kind), true)
            }
            _ => return Task::none(),
        };
        let normal = declared.step.unwrap_or(if integer { 1.0 } else { 0.01 });
        let step = if option {
            declared.fine_step.unwrap_or(normal / 10.0)
        } else if shift {
            normal * 10.0
        } else {
            normal
        };
        let current = self
            .control_field_value(&action, &parameter)
            .and_then(|v| v.as_f64())
            .unwrap_or(min);
        let next = (current + f64::from(direction.signum()) * step).clamp(min, max);
        let value = if integer {
            Value::from(next.round() as i64)
        } else {
            Value::from(next)
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
        let Some(declared) =
            tools::declared_action(&self.modules, &action).and_then(|a| a.parameter(&parameter))
        else {
            return Task::none();
        };
        let (min, max, integer) = match declared.kind {
            ParameterKind::Number { .. } => {
                (hard_min(&declared.kind), hard_max(&declared.kind), false)
            }
            ParameterKind::Integer { .. } => {
                (hard_min(&declared.kind), hard_max(&declared.kind), true)
            }
            _ => return Task::none(),
        };
        let normal = declared.step.unwrap_or(if integer { 1.0 } else { 0.01 });
        let step = if option {
            declared.fine_step.unwrap_or(normal / 10.0)
        } else if shift {
            normal * 10.0
        } else {
            normal
        };
        let Some(current) = self
            .control_field_value(&action, &parameter)
            .and_then(|v| v.as_f64())
        else {
            self.status = "Correct the field before stepping it".into();
            return Task::none();
        };
        let next = (current + f64::from(direction.signum()) * step).clamp(min, max);
        let text = if integer {
            (next.round() as i64).to_string()
        } else {
            crate::app::fields::number_text(next)
        };
        self.fields.set(&action, &parameter, text);
        let id = crate::app::fields::field_id(&action, &parameter, None);
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
                    .slider_draft
                    .as_ref()
                    .is_some_and(|draft| draft.action == action && draft.parameter == parameter)
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
            ColorPickerEvent::Reset => self.dispatch(Message::ResetField { action, parameter }),
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
            CurveEditorEvent::Cancel => self.slider_cancel(),
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
            .slider_draft
            .as_ref()
            .is_some_and(|draft| draft.action != action || draft.parameter != parameter)
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
            move |result| Message::CurveSampled {
                identity: identity.clone(),
                result,
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

fn hard_min(kind: &ParameterKind) -> f64 {
    match kind {
        ParameterKind::Number { min, .. } => *min,
        ParameterKind::Integer { min, .. } => *min as f64,
        _ => 0.0,
    }
}
fn picker_fraction(fraction: f32) -> f64 {
    if fraction.is_finite() {
        f64::from(fraction.clamp(0.0, 1.0))
    } else {
        0.0
    }
}
fn hard_max(kind: &ParameterKind) -> f64 {
    match kind {
        ParameterKind::Number { max, .. } => *max,
        ParameterKind::Integer { max, .. } => *max as f64,
        _ => 1.0,
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
    fn visit(
        controls: &[Control],
        action: &str,
        parameter: &str,
    ) -> Option<(String, usize, String)> {
        for control in controls {
            match control {
                Control::Group { controls, .. } => {
                    if let Some(found) = visit(controls, action, parameter) {
                        return Some(found);
                    }
                }
                Control::Curve {
                    action: a,
                    channels,
                    sample_query,
                    ..
                } if a == action => {
                    if let Some(index) = channels
                        .iter()
                        .position(|channel| channel.parameter == parameter)
                    {
                        return Some((
                            sample_query.clone(),
                            index,
                            channels.first()?.parameter.clone(),
                        ));
                    }
                }
                _ => {}
            }
        }
        None
    }
    modules
        .iter()
        .find_map(|module| visit(&module.controls, action, parameter))
}
fn curve_channel_parameters(
    modules: &[lightwell_core::ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<Vec<String>> {
    fn visit(controls: &[Control], action: &str, parameter: &str) -> Option<Vec<String>> {
        for control in controls {
            match control {
                Control::Group { controls, .. } => {
                    if let Some(found) = visit(controls, action, parameter) {
                        return Some(found);
                    }
                }
                Control::Curve {
                    action: a,
                    channels,
                    ..
                } if a == action && channels.iter().any(|c| c.parameter == parameter) => {
                    return Some(channels.iter().map(|c| c.parameter.clone()).collect());
                }
                _ => {}
            }
        }
        None
    }
    modules
        .iter()
        .find_map(|module| visit(&module.controls, action, parameter))
}

pub(crate) fn initial_group_expanded(
    modules: &[lightwell_core::ModuleDescriptor],
    module_id: &str,
    path: &[usize],
) -> Option<bool> {
    let module = modules.iter().find(|module| module.id == module_id)?;
    let mut controls = &module.controls[..];
    for (depth, index) in path.iter().copied().enumerate() {
        let Control::Group {
            controls: children,
            collapsed,
            ..
        } = controls.get(index)?
        else {
            return None;
        };
        if depth + 1 == path.len() {
            return Some(!collapsed);
        }
        controls = children;
    }
    None
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
