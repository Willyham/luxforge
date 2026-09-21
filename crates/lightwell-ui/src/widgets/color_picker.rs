//! HSV picker. It maps pointer locations to fractions; the host owns channel values and edits.

use super::curve_editor::invalidate_on_version_change;
use crate::{ColorSwatchModel, ValueEdit, color_swatch, theme};
use iced::{
    Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Cursor},
    widget::{
        canvas::{self, Action, Event, Path, Stroke},
        column, row, text, text_input,
    },
};
use std::{cell::Cell, rc::Rc};

/// Exact 8-bit sRGB to HSV fractions. Hue of grey is defined as zero.
pub fn rgb_to_hsv(rgb: [u8; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|v| f64::from(v) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / delta).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };
    [h, if max == 0.0 { 0.0 } else { delta / max }, max]
}

pub fn hsv_to_rgb(hsv: [f64; 3]) -> [u8; 3] {
    let [h, s, v] = hsv;
    let h = if h.is_finite() {
        h.rem_euclid(1.0) * 6.0
    } else {
        0.0
    };
    let s = if s.is_finite() {
        s.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let v = if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let rgb = match h as u8 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    rgb.map(|v| ((v + m) * 255.0).round() as u8)
}

pub fn rgb_to_hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

pub fn hex_to_rgb(input: &str) -> Option<[u8; 3]> {
    let digits = input.strip_prefix('#').unwrap_or(input);
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&digits[0..2], 16).ok()?,
        u8::from_str_radix(&digits[2..4], 16).ok()?,
        u8::from_str_radix(&digits[4..6], 16).ok()?,
    ])
}

pub fn plane_fraction(point: Point, bounds: Rectangle) -> [f32; 2] {
    [
        ((point.x - bounds.x) / bounds.width).clamp(0.0, 1.0),
        (1.0 - (point.y - bounds.y) / bounds.height).clamp(0.0, 1.0),
    ]
}

pub fn hue_fraction(point: Point, bounds: Rectangle) -> f32 {
    ((point.x - bounds.x) / bounds.width).clamp(0.0, 1.0)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColorPickerModel {
    pub hue: f32,
    pub saturation: f32,
    pub value: f32,
    pub rgb: [u8; 3],
    pub channels: [ValueEdit; 3],
    pub hex: ValueEdit,
    pub dragging: bool,
    pub enabled: bool,
    /// Increment when the plane's hue or any other tessellated content changes.
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ColorPickerEvent {
    Plane([f32; 2]),
    Hue(f32),
    Release,
    Text { field: usize, text: String }, // 0..3 are RGB, 3 is hex
    Submit(usize),
    Reset,
}

/// A picker with one saturation/value plane, a hue rail and four text fields.
pub fn color_picker<'a, M: Clone + 'a>(
    model: &ColorPickerModel,
    on_event: impl Fn(ColorPickerEvent) -> M + 'a,
) -> Element<'a, M> {
    let on_event: Rc<dyn Fn(ColorPickerEvent) -> M + 'a> = Rc::new(on_event);
    let plane = canvas::Canvas::new(PickerCanvas {
        model: model.clone(),
        part: Part::Plane,
        on_event: on_event.clone(),
    })
    .width(Length::Fill)
    .height(Length::Fixed(148.0));
    let hue = canvas::Canvas::new(PickerCanvas {
        model: model.clone(),
        part: Part::Hue,
        on_event: on_event.clone(),
    })
    .width(Length::Fill)
    .height(Length::Fixed(18.0));
    let mut fields = row![].spacing(4.0).align_y(Alignment::Center);
    for (index, label) in ["R", "G", "B", "Hex"].into_iter().enumerate() {
        let edit = if index == 3 {
            &model.hex
        } else {
            &model.channels[index]
        };
        let (value, invalid) = match edit {
            ValueEdit::Display => (
                if index == 3 {
                    rgb_to_hex(model.rgb)
                } else {
                    model.rgb[index].to_string()
                },
                false,
            ),
            ValueEdit::Editing { text, invalid } => (text.clone(), invalid.is_some()),
        };
        let text_callback = on_event.clone();
        let submit_callback = on_event.clone();
        fields = fields.push(text(label).size(theme::SIZE_CAPTION)).push(
            text_input("", &value)
                .size(theme::SIZE_CONTROL)
                .width(Length::FillPortion(if index == 3 { 2 } else { 1 }))
                .style(theme::text_input_style(invalid))
                .on_input_maybe(model.enabled.then_some(move |text| {
                    text_callback(ColorPickerEvent::Text { field: index, text })
                }))
                .on_submit_maybe(
                    model
                        .enabled
                        .then_some(submit_callback(ColorPickerEvent::Submit(index))),
                ),
        );
    }
    let reset = iced::widget::button(text("Reset").size(theme::SIZE_CAPTION))
        .style(theme::button_plain)
        .on_press_maybe(model.enabled.then_some(on_event(ColorPickerEvent::Reset)));
    let swatch = color_swatch(
        &ColorSwatchModel {
            rgb: model.rgb,
            enabled: false,
            open: false,
        },
        on_event(ColorPickerEvent::Reset),
    );
    column![plane, hue, fields, row![swatch, reset].spacing(5.0)]
        .spacing(5.0)
        .into()
}

#[derive(Clone, Copy)]
enum Part {
    Plane,
    Hue,
}

struct PickerCanvas<'a, M> {
    model: ColorPickerModel,
    part: Part,
    on_event: Rc<dyn Fn(ColorPickerEvent) -> M + 'a>,
}

#[derive(Default)]
struct PickerState {
    cache: canvas::Cache,
    version: Cell<Option<u64>>,
    dragging: bool,
}

impl<M: Clone> canvas::Program<M> for PickerCanvas<'_, M> {
    type State = PickerState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<M>> {
        if !self.model.enabled {
            return None;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position_over(bounds)?;
                state.dragging = true;
                Some(
                    Action::publish((self.on_event)(self.position_event(point, bounds)))
                        .and_capture(),
                )
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
                let point = cursor.position()?;
                Some(
                    Action::publish((self.on_event)(self.position_event(point, bounds)))
                        .and_capture(),
                )
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.dragging => {
                state.dragging = false;
                Some(Action::publish((self.on_event)(ColorPickerEvent::Release)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        invalidate_on_version_change(&state.version, self.model.version, || state.cache.clear());
        let part = self.part;
        let hue = self.model.hue;
        let geometry = state.cache.draw(renderer, bounds.size(), |frame| {
            let size = frame.size();
            let cells = if matches!(part, Part::Plane) { 24 } else { 48 };
            let rows = if matches!(part, Part::Plane) { 16 } else { 1 };
            let w = size.width / cells as f32;
            let h = size.height / rows as f32;
            for y in 0..rows {
                for x in 0..cells {
                    let rgb = match part {
                        Part::Plane => hsv_to_rgb([
                            f64::from(hue),
                            (x as f64 + 0.5) / cells as f64,
                            1.0 - (y as f64 + 0.5) / rows as f64,
                        ]),
                        Part::Hue => hsv_to_rgb([(x as f64 + 0.5) / cells as f64, 1.0, 1.0]),
                    };
                    frame.fill_rectangle(
                        Point::new(x as f32 * w, y as f32 * h),
                        Size::new(w + 0.3, h + 0.3),
                        Color::from_rgb8(rgb[0], rgb[1], rgb[2]),
                    );
                }
            }
            let marker = match part {
                Part::Plane => Point::new(
                    self.model.saturation * size.width,
                    (1.0 - self.model.value) * size.height,
                ),
                Part::Hue => Point::new(self.model.hue * size.width, size.height / 2.0),
            };
            frame.stroke(
                &Path::circle(
                    marker,
                    if matches!(part, Part::Plane) {
                        5.0
                    } else {
                        7.0
                    },
                ),
                Stroke::default()
                    .with_color(theme::TEXT_PRIMARY)
                    .with_width(2.0),
            );
        });
        vec![geometry]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if self.model.enabled && cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::None
        }
    }
}

impl<M> PickerCanvas<'_, M> {
    fn position_event(&self, point: Point, bounds: Rectangle) -> ColorPickerEvent {
        match self.part {
            Part::Plane => ColorPickerEvent::Plane(plane_fraction(point, bounds)),
            Part::Hue => ColorPickerEvent::Hue(hue_fraction(point, bounds)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::canvas::Program;

    fn model() -> ColorPickerModel {
        ColorPickerModel {
            hue: 0.2,
            saturation: 0.5,
            value: 0.5,
            rgb: [128, 100, 80],
            channels: std::array::from_fn(|_| ValueEdit::Display),
            hex: ValueEdit::Display,
            dragging: false,
            enabled: true,
            version: 0,
        }
    }
    #[test]
    fn every_rgb_triple_round_trips_through_hsv_and_hex() {
        for r in 0..=255 {
            for g in 0..=255 {
                for b in 0..=255 {
                    let rgb = [r, g, b];
                    assert_eq!(hsv_to_rgb(rgb_to_hsv(rgb)), rgb, "HSV {rgb:?}");
                    assert_eq!(hex_to_rgb(&rgb_to_hex(rgb)), Some(rgb), "hex {rgb:?}");
                }
            }
        }
    }

    #[test]
    fn pointer_fractions_are_clamped_and_vertical_value_is_upward() {
        let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(100.0, 100.0));
        assert_eq!(plane_fraction(Point::new(60.0, 70.0), bounds), [0.5, 0.5]);
        assert_eq!(plane_fraction(Point::new(-9.0, 400.0), bounds), [0.0, 0.0]);
        assert_eq!(hue_fraction(Point::new(110.0, 30.0), bounds), 1.0);
    }

    #[test]
    fn plane_drag_emits_global_position_as_fractions_then_release() {
        let canvas = PickerCanvas {
            model: model(),
            part: Part::Plane,
            on_event: Rc::new(|event| event),
        };
        let mut state = PickerState::default();
        let bounds = Rectangle::new(Point::new(40.0, 70.0), Size::new(100.0, 100.0));
        let cursor = Cursor::Available(Point::new(90.0, 120.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        assert_eq!(
            canvas
                .update(&mut state, &press, bounds, cursor)
                .and_then(|action| action.into_inner().0),
            Some(ColorPickerEvent::Plane([0.5, 0.5]))
        );
        let move_event = Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(140.0, 70.0),
        });
        let move_cursor = Cursor::Available(Point::new(140.0, 70.0));
        assert_eq!(
            canvas
                .update(&mut state, &move_event, bounds, move_cursor)
                .and_then(|action| action.into_inner().0),
            Some(ColorPickerEvent::Plane([1.0, 1.0]))
        );
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        assert_eq!(
            canvas
                .update(&mut state, &release, bounds, Cursor::Unavailable)
                .and_then(|action| action.into_inner().0),
            Some(ColorPickerEvent::Release)
        );
    }
}
