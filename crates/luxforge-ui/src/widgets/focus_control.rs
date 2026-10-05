//! Keyboard focus for generated controls built from Iced widgets without native Tab stops.
//! The wrapper owns only transient focus. The caller maps keys to parameter values and requests.

use super::decorator::{Decoration, decorate};
use crate::theme;
use crate::{Element, Theme};
use iced::{
    Background, Border, Color, Event, Rectangle, Renderer, Shadow,
    advanced::{
        Clipboard, Layout, Shell, renderer,
        widget::{Operation, Tree, operation},
    },
    keyboard::{self, Key, key::Named},
    mouse::{self, Cursor},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKey {
    Space,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Escape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKeyEvent {
    Pressed {
        key: ControlKey,
        shift: bool,
        option: bool,
    },
    Released(ControlKey),
}

pub fn focus_control<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    enabled: bool,
    on_key: impl Fn(ControlKeyEvent) -> Option<M> + 'a,
) -> Element<'a, M> {
    decorate(
        content,
        FocusControl {
            enabled,
            on_key: Box::new(on_key),
        },
    )
}

struct FocusControl<'a, M> {
    enabled: bool,
    on_key: Box<dyn Fn(ControlKeyEvent) -> Option<M> + 'a>,
}

#[derive(Default)]
struct FocusState {
    focused: bool,
}

impl operation::Focusable for FocusState {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
    }
}

fn key_of(key: &Key) -> Option<ControlKey> {
    Some(match key {
        Key::Named(Named::Space) => ControlKey::Space,
        Key::Named(Named::Enter) => ControlKey::Enter,
        Key::Named(Named::ArrowLeft) => ControlKey::Left,
        Key::Named(Named::ArrowRight) => ControlKey::Right,
        Key::Named(Named::ArrowUp) => ControlKey::Up,
        Key::Named(Named::ArrowDown) => ControlKey::Down,
        Key::Named(Named::Escape) => ControlKey::Escape,
        _ => return None,
    })
}

impl<'a, M: Clone> Decoration<'a, M, Theme, Renderer> for FocusControl<'a, M> {
    type State = FocusState;

    fn operate(
        &mut self,
        content: &mut Element<'a, M>,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree.state.downcast_mut::<FocusState>();
        if self.enabled {
            operation.focusable(None, layout.bounds(), state);
        } else {
            state.focused = false;
        }
        operation.traverse(&mut |operation| {
            content
                .as_widget_mut()
                .operate(&mut tree.children[0], layout, renderer, operation)
        });
    }

    fn update(
        &mut self,
        content: &mut Element<'a, M>,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        if !self.enabled {
            tree.state.downcast_mut::<FocusState>().focused = false;
        }
        let focused = self.enabled && tree.state.downcast_ref::<FocusState>().focused;
        if focused {
            let mapped = match event {
                Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => key_of(key)
                    .map(|key| ControlKeyEvent::Pressed {
                        key,
                        shift: modifiers.shift(),
                        option: modifiers.alt(),
                    }),
                Event::Keyboard(keyboard::Event::KeyReleased { key, .. }) => {
                    key_of(key).map(ControlKeyEvent::Released)
                }
                _ => None,
            };
            if let Some(message) = mapped.and_then(|event| (self.on_key)(event)) {
                shell.publish(message);
                shell.capture_event();
                return;
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
        if self.enabled
            && !shell.is_event_captured()
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            )
        {
            tree.state.downcast_mut::<FocusState>().focused = cursor.is_over(layout.bounds());
        }
    }

    fn draw(
        &self,
        content: &Element<'a, M>,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        if tree.state.downcast_ref::<FocusState>().focused {
            use iced::advanced::Renderer as _;
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: Border {
                        color: theme.palette().accent,
                        width: 1.0,
                        radius: theme::RADIUS.into(),
                    },
                    shadow: Shadow::default(),
                    snap: false,
                },
                Background::Color(Color::TRANSPARENT),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_translation_covers_space_and_arrows_but_leaves_tab_to_iced() {
        assert_eq!(key_of(&Key::Named(Named::Space)), Some(ControlKey::Space));
        assert_eq!(
            key_of(&Key::Named(Named::ArrowLeft)),
            Some(ControlKey::Left)
        );
        assert_eq!(key_of(&Key::Named(Named::Tab)), None);
    }
}
