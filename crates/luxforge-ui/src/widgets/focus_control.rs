//! Keyboard focus for generated controls built from Iced widgets without native Tab stops.
//! The wrapper owns only transient focus. The caller maps keys to parameter values and requests.

use crate::theme;
use iced::{
    Background, Border, Color, Element, Event, Length, Rectangle, Renderer, Shadow, Size, Theme,
    Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, overlay, renderer,
        widget::{Operation, Tree, operation, tree},
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
    FocusControl {
        content,
        enabled,
        on_key: Box::new(on_key),
    }
    .into()
}

struct FocusControl<'a, M> {
    content: Element<'a, M>,
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

impl<M: Clone> Widget<M, Theme, Renderer> for FocusControl<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<FocusState>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(FocusState::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
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
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], layout, renderer, operation)
        });
    }
    fn update(
        &mut self,
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
        self.content.as_widget_mut().update(
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
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
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
                        color: theme::ACCENT,
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
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: Clone + 'a> From<FocusControl<'a, M>> for Element<'a, M> {
    fn from(widget: FocusControl<'a, M>) -> Self {
        Element::new(widget)
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
