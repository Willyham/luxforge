//! Filter Iced slider events: panel scrolling must never edit, and disabled rails are inert.

use super::decorator::Decoration;
use crate::{Element, Theme};
use iced::advanced::{Clipboard, Layout, Shell, mouse, widget::Tree};
use iced::{
    Event, Rectangle, Renderer,
    keyboard::{self, Key, key::Named},
};

pub(super) struct SliderGuard<'a, M> {
    pub enabled: bool,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub fine_step: f64,
    pub on_fine: Box<dyn Fn(f64) -> M + 'a>,
    pub on_release: M,
}

#[derive(Default)]
pub(super) struct KeyState {
    modifiers: keyboard::Modifiers,
    active: bool,
}

fn forwards_to_slider(event: &Event, enabled: bool) -> bool {
    enabled && !matches!(event, Event::Mouse(mouse::Event::WheelScrolled { .. }))
}

impl<'a, M: Clone + 'a> Decoration<'a, M, Theme, Renderer> for SliderGuard<'a, M> {
    type State = KeyState;

    fn update(
        &mut self,
        content: &mut Element<'a, M>,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        // Leave wheel events uncaptured so the containing panel may scroll. Iced's slider
        // normally treats Ctrl+wheel as an edit, even when no drag is active.
        if !forwards_to_slider(event, self.enabled) {
            return;
        }
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) = event {
            tree.state.downcast_mut::<KeyState>().modifiers = *modifiers;
        }
        let key_state = tree.state.downcast_mut::<KeyState>();
        if matches!(
            event,
            Event::Keyboard(keyboard::Event::KeyReleased {
                key: Key::Named(Named::ArrowUp | Named::ArrowDown),
                ..
            })
        ) && key_state.active
        {
            key_state.active = false;
            shell.publish(self.on_release.clone());
            shell.capture_event();
            return;
        }
        if matches!(
            event,
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(Named::ArrowUp | Named::ArrowDown),
                ..
            })
        ) && cursor.is_over(layout.bounds())
        {
            key_state.active = true;
        }
        let modifiers = key_state.modifiers;
        if modifiers.alt() && cursor.is_over(layout.bounds()) {
            match event {
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(Named::ArrowUp),
                    ..
                }) => {
                    shell.publish((self.on_fine)(
                        (self.value + self.fine_step).clamp(self.min, self.max),
                    ));
                    shell.capture_event();
                    return;
                }
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(Named::ArrowDown),
                    ..
                }) => {
                    shell.publish((self.on_fine)(
                        (self.value - self.fine_step).clamp(self.min, self.max),
                    ));
                    shell.capture_event();
                    return;
                }
                _ => {}
            }
        }
        content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        content: &Element<'a, M>,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.enabled {
            content.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
        } else {
            mouse::Interaction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wheel_never_reaches_iced_slider_and_disabled_rail_is_inert() {
        let wheel = Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        });
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        assert!(!forwards_to_slider(&wheel, true));
        assert!(!forwards_to_slider(&wheel, false));
        assert!(!forwards_to_slider(&press, false));
        assert!(forwards_to_slider(&press, true));
    }
}
