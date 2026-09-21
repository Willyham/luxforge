//! Filter Iced slider events: panel scrolling must never edit, and disabled rails are inert.

use iced::advanced::{
    Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
    widget::{Operation, Tree, tree},
};
use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector,
    keyboard::{self, Key, key::Named},
};

pub(super) struct SliderGuard<'a, M> {
    pub content: Element<'a, M>,
    pub enabled: bool,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub fine_step: f64,
    pub on_fine: Box<dyn Fn(f64) -> M + 'a>,
    pub on_release: M,
}

#[derive(Default)]
struct KeyState {
    modifiers: keyboard::Modifiers,
    active: bool,
}

fn forwards_to_slider(event: &Event, enabled: bool) -> bool {
    enabled && !matches!(event, Event::Mouse(mouse::Event::WheelScrolled { .. }))
}

impl<'a, M: Clone + 'a> Widget<M, Theme, Renderer> for SliderGuard<'a, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<KeyState>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(KeyState::default())
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
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
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
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
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
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.enabled {
            self.content.as_widget().mouse_interaction(
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

impl<'a, M: Clone + 'a> From<SliderGuard<'a, M>> for Element<'a, M> {
    fn from(guard: SliderGuard<'a, M>) -> Self {
        Element::new(guard)
    }
}
