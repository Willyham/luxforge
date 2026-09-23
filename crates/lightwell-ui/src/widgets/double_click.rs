//! A transparent wrapper that turns a double-click anywhere over its content into one message,
//! even when the content captures the press itself.
//!
//! Iced's `mouse_area` forwards every event to its content first and only then looks at it, so a
//! child that captures the left press — iced's `slider` does, to start a drag — hides the press
//! from it and a double-click over the child is never seen. This widget does the opposite: it
//! inspects the press first, and when the press is the second click of a double click it publishes
//! its message, captures the event and does **not** forward it, so the content never starts a
//! gesture the reset is about to undo.
//!
//! What that means on a slider rail is worth saying plainly, because it is visible:
//!
//! - A double-click on the rail **away from the handle** is a jump and then a reset. The first
//!   click reaches the slider, which moves the handle to the pointer and commits that value on
//!   release; the second click is swallowed here and resets the field. The committed jump stays in
//!   history as its own entry, and the host holds the reset until that commit has answered, so the
//!   reset names the revision the jump produced.
//! - A double-click **on the handle's own position** is a reset alone: iced's slider publishes no
//!   value when the pointer maps to the value the handle already has, so the first click commits
//!   nothing.
//!
//! Everything that is not a left press is forwarded unchanged, so the content keeps every other
//! event — including the release that ends a drag the first click started.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector};

/// Wraps `content` so that a double-click anywhere over it publishes `on_double_click`.
///
/// The wrapper has no size, layout, styling or interaction of its own: everything is delegated to
/// the content, so wrapping a widget never changes how it looks or how it is measured.
pub fn double_click<'a, M: Clone + 'a>(
    content: impl Into<Element<'a, M>>,
    on_double_click: M,
) -> Element<'a, M> {
    double_click_when(content, on_double_click, true)
}

/// [`double_click`] that only listens while `enabled`.
///
/// The wrapper is there whether or not it is enabled, so the widget tree has the same shape either
/// way and its state — the last press it classified — survives a control being disabled for a
/// moment, such as while a request is in flight. Swapping the wrapper in and out instead would
/// replace that state every time the flag flipped, and a double-click whose two presses straddled
/// the flip would read as two single clicks. While disabled every press goes to the content
/// unclassified and unrecorded, so a press the control ignored never counts as a first click.
pub fn double_click_when<'a, M: Clone + 'a>(
    content: impl Into<Element<'a, M>>,
    on_double_click: M,
    enabled: bool,
) -> Element<'a, M> {
    Element::new(DoubleClick {
        content: content.into(),
        on_double_click,
        enabled,
    })
}

/// Whether one left press over the content is the one that publishes the message.
///
/// Factored out so the rule is provable without a renderer, a layout pass or a window. Only a
/// `Double` counts: a `Triple` is the third click of a run, and publishing again there would reset
/// a field the person is still clicking on.
fn resets(kind: mouse::click::Kind) -> bool {
    matches!(kind, mouse::click::Kind::Double)
}

struct DoubleClick<'a, M, Theme = iced::Theme, Renderer = iced::Renderer> {
    content: Element<'a, M, Theme, Renderer>,
    on_double_click: M,
    /// Presses are classified only while this is set; the wrapper stays in the tree either way.
    enabled: bool,
}

/// The wrapper's own state: the last left press it saw, which is all `mouse::Click` needs to
/// classify the next one.
#[derive(Default)]
struct State {
    previous_click: Option<mouse::Click>,
}

impl<M, Theme, Renderer> Widget<M, Theme, Renderer> for DoubleClick<'_, M, Theme, Renderer>
where
    Renderer: renderer::Renderer,
    M: Clone,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
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

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
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
        // The press is classified before the content sees it, which is the whole point: a content
        // that captures the press (iced's slider does) would otherwise hide every double click.
        if self.enabled
            && let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event
            && let Some(position) = cursor.position_over(layout.bounds())
            && self.record(tree, position)
        {
            shell.publish(self.on_double_click.clone());
            shell.capture_event();
            return;
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
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
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

impl<M, Theme, Renderer> DoubleClick<'_, M, Theme, Renderer> {
    /// Records one left press and answers whether it completes a double click.
    fn record(&self, tree: &mut Tree, position: Point) -> bool {
        let state: &mut State = tree.state.downcast_mut();
        let click = mouse::Click::new(position, mouse::Button::Left, state.previous_click);
        state.previous_click = Some(click);
        resets(click.kind())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouse::{Button, Click, click::Kind};

    /// The rule the wrapper applies to each press it classifies. Iced decides which kind a press
    /// is, from the previous one's position and time; this is the whole of what this widget adds,
    /// over every kind there is. A test that clicked twice and asserted `Double` would be a test
    /// of the wall clock: `mouse::Click` treats two presses at the same `Instant` as unrelated.
    #[test]
    fn only_a_double_click_resets() {
        assert!(!resets(Kind::Single), "the first click starts the gesture");
        assert!(resets(Kind::Double), "the second click resets the field");
        assert!(
            !resets(Kind::Triple),
            "a third click does not reset a second time"
        );
    }

    /// The wrapper over an empty 40 × 12 content, with the unit renderer: enough to drive `update`
    /// and the tree reconciliation exactly as the runtime does, without a window or a GPU.
    type Wrapper = DoubleClick<'static, u8, iced::Theme, ()>;

    fn wrapper(enabled: bool) -> Wrapper {
        DoubleClick {
            content: Element::new(iced::widget::Space::new().width(40).height(12)),
            on_double_click: 7,
            enabled,
        }
    }

    /// One left press in the middle of the content, and what it published.
    fn press(widget: &mut Wrapper, tree: &mut Tree) -> Vec<u8> {
        let node = layout::Node::new(Size::new(40.0, 12.0));
        let mut published = Vec::new();
        let mut shell = Shell::new(&mut published);
        widget.update(
            tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            mouse::Cursor::Available(Point::new(20.0, 6.0)),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &Rectangle::new(Point::ORIGIN, Size::new(40.0, 12.0)),
        );
        published
    }

    /// Rebuild the widget as the next view would and reconcile the retained tree against it.
    fn rebuild(tree: &mut Tree, widget: &Wrapper) {
        tree.diff(widget as &dyn Widget<u8, iced::Theme, ()>);
    }

    /// A control that is disabled between the two presses of a double-click — which is what a
    /// request in flight does to a whole section — keeps the wrapper and its first press, so the
    /// second press still resets. A press while disabled publishes nothing and is not counted.
    #[test]
    fn a_disabled_moment_between_the_presses_keeps_the_first_press() {
        let mut widget = wrapper(true);
        let mut tree = Tree::new(&widget as &dyn Widget<u8, iced::Theme, ()>);
        assert!(press(&mut widget, &mut tree).is_empty(), "the first press");

        let mut disabled = wrapper(false);
        rebuild(&mut tree, &disabled);
        assert_eq!(
            tree.tag,
            tree::Tag::of::<State>(),
            "the wrapper is still in the tree"
        );
        assert!(
            press(&mut disabled, &mut tree).is_empty(),
            "a disabled wrapper publishes nothing"
        );

        let mut enabled = wrapper(true);
        rebuild(&mut tree, &enabled);
        // `mouse::Click` needs the second press strictly later than the first.
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(
            press(&mut enabled, &mut tree),
            vec![7],
            "the second press completes the double-click begun before the control was disabled"
        );
    }

    /// Why the wrapper must stay in the tree: reconciling it against a different widget, as the
    /// slider did by dropping the wrapper while its rail was disabled, replaces the retained state,
    /// so the first press is forgotten and the second is a single click.
    #[test]
    fn swapping_the_wrapper_out_forgets_the_first_press() {
        let mut widget = wrapper(true);
        let mut tree = Tree::new(&widget as &dyn Widget<u8, iced::Theme, ()>);
        assert!(press(&mut widget, &mut tree).is_empty());

        let bare: Element<'static, u8, iced::Theme, ()> =
            Element::new(iced::widget::Space::new().width(40).height(12));
        tree.diff(bare.as_widget());
        let mut again = wrapper(true);
        rebuild(&mut tree, &again);
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(
            press(&mut again, &mut tree).is_empty(),
            "the retained click history went with the swapped-out wrapper"
        );
    }

    /// A press far from the previous one starts a new run, so dragging the handle and pressing
    /// again elsewhere on the rail never reads as a double click. Distance alone decides this, so
    /// it holds however fast or slowly the two presses arrive.
    #[test]
    fn a_press_elsewhere_starts_a_new_run() {
        let first = Click::new(Point::new(20.0, 8.0), Button::Left, None);
        assert_eq!(first.kind(), Kind::Single, "there is nothing before it");
        let elsewhere = Click::new(Point::new(180.0, 8.0), Button::Left, Some(first));
        assert_eq!(elsewhere.kind(), Kind::Single);
        assert!(!resets(elsewhere.kind()));
    }
}
