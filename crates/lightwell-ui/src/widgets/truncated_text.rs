//! One line of text that ends in an ellipsis when it does not fit.
//!
//! Iced's text wraps or clips; neither is right for a label that must stay one line inside a fixed
//! row: a collapsed module band's hint, a history row's label, a recipe row's summary. This widget
//! draws the whole string when it fits and otherwise the longest prefix, on a character boundary,
//! followed by "…". It takes plain data (the string, size, font, colour) and holds only a layout
//! cache.
//!
//! Layout: the widget reports `Fill` width, so a row lays out its other children first and this
//! one takes what is left; its node then hugs the drawn text, so a sibling placed after it (a
//! history row's tag) follows the text, and a container can align it right (a band's hint). The
//! measurement is cached against the content, the available width, the size, the line height and
//! the font, so a layout pass with nothing changed shapes and allocates nothing.

use iced::advanced::text::{self as core_text, Paragraph as _, Renderer as _};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Layout, Widget, layout, mouse, renderer};
use iced::widget::text::{LineHeight, Shaping, Wrapping};
use iced::{Color, Element, Font, Length, Pixels, Rectangle, Renderer, Size, Theme, alignment};

/// The mark appended to a truncated string.
pub const ELLIPSIS: &str = "\u{2026}";

/// How much of a string fits on one line, from [`fit_one_line`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// The whole string fits and is drawn unchanged.
    Whole,
    /// The first `n` bytes (a character boundary) followed by [`ELLIPSIS`] fit. `n` may be 0, in
    /// which case only the ellipsis is drawn.
    Prefix(usize),
    /// Not even the ellipsis fits; nothing is drawn.
    Nothing,
}

/// Finds how much of `content` fits in `available` width on one line.
///
/// `measure` returns the one-line width of a string. It is called on `content` once and then on
/// candidate prefixes with [`ELLIPSIS`] appended, which are built in `scratch` so a caller can
/// reuse one buffer. The search is binary over character boundaries (it assumes a longer prefix is
/// never narrower), and a prefix's trailing whitespace is dropped before the ellipsis.
pub fn fit_one_line(
    content: &str,
    available: f32,
    mut measure: impl FnMut(&str) -> f32,
    scratch: &mut String,
) -> Fit {
    if measure(content) <= available {
        return Fit::Whole;
    }
    let mut candidate = |end: usize, scratch: &mut String| {
        scratch.clear();
        scratch.push_str(&content[..end]);
        scratch.push_str(ELLIPSIS);
        measure(scratch) <= available
    };
    if !candidate(0, scratch) {
        return Fit::Nothing;
    }
    // `lo` is a boundary whose prefix fits; `hi` is one whose prefix does not (the whole string,
    // which does not fit even without the ellipsis). Every step tests a boundary strictly between.
    let (mut lo, mut hi) = (0, content.len());
    while let Some(mid) = boundary_between(content, lo, hi) {
        if candidate(mid, scratch) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Fit::Prefix(content[..lo].trim_end().len())
}

/// A character boundary strictly between `lo` and `hi`, near their midpoint, if there is one.
fn boundary_between(content: &str, lo: usize, hi: usize) -> Option<usize> {
    let middle = lo + (hi - lo) / 2;
    let mut down = middle;
    while down > lo && !content.is_char_boundary(down) {
        down -= 1;
    }
    if down > lo {
        return Some(down);
    }
    let mut up = middle + 1;
    while up < hi && !content.is_char_boundary(up) {
        up += 1;
    }
    (up < hi).then_some(up)
}

/// One line of text truncated with an ellipsis to the width its parent leaves it.
pub struct TruncatedText {
    content: String,
    size: f32,
    font: Font,
    color: Color,
    line_height: LineHeight,
}

/// A one-line label at `size` in `font` and `color`, ending in "…" when it does not fit.
pub fn truncated_text(
    content: impl Into<String>,
    size: f32,
    font: Font,
    color: Color,
) -> TruncatedText {
    TruncatedText {
        content: content.into(),
        size,
        font,
        color,
        line_height: LineHeight::default(),
    }
}

impl TruncatedText {
    /// Sets the line height, which is also the widget's height.
    pub fn line_height(mut self, line_height: LineHeight) -> Self {
        self.line_height = line_height;
        self
    }
}

type Paragraph = <Renderer as core_text::Renderer>::Paragraph;

/// What the last layout measured and the paragraph it produced.
#[derive(Default)]
struct State {
    content: String,
    available: f32,
    size: f32,
    font: Option<Font>,
    line_height: Option<LineHeight>,
    paragraph: Paragraph,
    /// The candidate buffer the search reuses.
    scratch: String,
}

impl State {
    fn is_current(&self, text: &TruncatedText, available: f32) -> bool {
        self.font == Some(text.font)
            && self.line_height == Some(text.line_height)
            && self.size == text.size
            && self.available == available
            && self.content == text.content
    }
}

impl TruncatedText {
    fn paragraph(&self, content: &str) -> Paragraph {
        Paragraph::with_text(core_text::Text {
            content,
            bounds: Size::INFINITE,
            size: Pixels(self.size),
            line_height: self.line_height,
            font: self.font,
            align_x: core_text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: Shaping::default(),
            wrapping: Wrapping::None,
        })
    }

    fn remeasure(&self, state: &mut State, available: f32) {
        let mut scratch = std::mem::take(&mut state.scratch);
        let fit = fit_one_line(
            &self.content,
            available,
            |candidate| self.paragraph(candidate).min_width(),
            &mut scratch,
        );
        state.paragraph = match fit {
            Fit::Whole => self.paragraph(&self.content),
            Fit::Prefix(end) => {
                scratch.clear();
                scratch.push_str(&self.content[..end]);
                scratch.push_str(ELLIPSIS);
                self.paragraph(&scratch)
            }
            Fit::Nothing => self.paragraph(""),
        };
        state.scratch = scratch;
        self.content.clone_into(&mut state.content);
        state.available = available;
        state.size = self.size;
        state.font = Some(self.font);
        state.line_height = Some(self.line_height);
    }
}

impl<M> Widget<M, Theme, Renderer> for TruncatedText {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let available = limits.max().width;
        if !state.is_current(self, available) {
            self.remeasure(state, available);
        }
        let height = self.line_height.to_absolute(Pixels(self.size)).0;
        let width = state.paragraph.min_width().min(available);
        layout::Node::new(limits.resolve(Length::Shrink, Length::Shrink, Size::new(width, height)))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        renderer.fill_paragraph(
            &state.paragraph,
            layout.bounds().position(),
            self.color,
            *viewport,
        );
    }

    fn operate(
        &mut self,
        _tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        // Operations see the whole string, not the truncated one.
        operation.text(None, layout.bounds(), &self.content);
    }
}

impl<'a, M: 'a> From<TruncatedText> for Element<'a, M> {
    fn from(text: TruncatedText) -> Self {
        Element::new(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One unit per character, the ellipsis included.
    fn chars(text: &str) -> f32 {
        text.chars().count() as f32
    }

    fn fitted(content: &str, available: f32) -> String {
        let mut scratch = String::new();
        match fit_one_line(content, available, chars, &mut scratch) {
            Fit::Whole => content.to_string(),
            Fit::Prefix(end) => format!("{}{ELLIPSIS}", &content[..end]),
            Fit::Nothing => String::new(),
        }
    }

    #[test]
    fn a_string_that_fits_is_unchanged() {
        let mut scratch = String::new();
        assert_eq!(fit_one_line("Hue", 3.0, chars, &mut scratch), Fit::Whole);
        assert_eq!(fit_one_line("Hue", 10.0, chars, &mut scratch), Fit::Whole);
    }

    #[test]
    fn truncates_to_the_longest_prefix_that_fits_with_the_ellipsis() {
        assert_eq!(fitted("Blue saturation", 14.0), "Blue saturati\u{2026}");
        assert_eq!(fitted("Blue saturation", 5.0), "Blue\u{2026}");
        // Every width gives the longest fitting prefix: exactly `available` wide.
        // (No spaces, so no trailing whitespace is dropped.)
        let content = "Hue,saturation-and-luminance-by-range";
        for available in 1..content.len() {
            let fitted = fitted(content, available as f32);
            assert_eq!(chars(&fitted), available as f32, "{fitted:?}");
            assert!(fitted.ends_with(ELLIPSIS));
            assert!(content.starts_with(fitted.trim_end_matches(ELLIPSIS)));
        }
    }

    #[test]
    fn trailing_whitespace_is_dropped_before_the_ellipsis() {
        assert_eq!(fitted("Darken or lighten", 8.0), "Darken\u{2026}");
    }

    #[test]
    fn an_empty_string_fits() {
        let mut scratch = String::new();
        assert_eq!(fit_one_line("", 0.0, chars, &mut scratch), Fit::Whole);
    }

    #[test]
    fn a_width_narrower_than_the_ellipsis_draws_nothing() {
        let mut scratch = String::new();
        assert_eq!(
            fit_one_line("Vignette", 0.5, chars, &mut scratch),
            Fit::Nothing
        );
        assert_eq!(fitted("Vignette", 1.0), "\u{2026}");
    }

    #[test]
    fn multi_byte_characters_are_cut_on_boundaries() {
        // Two-, three- and four-byte characters: every prefix tried must be a valid slice.
        let content = "é日本🙂語ü";
        for available in 1..7 {
            let fitted = fitted(content, available as f32);
            assert_eq!(chars(&fitted), available as f32, "{fitted:?}");
        }
        assert_eq!(fitted("🙂🙂🙂", 2.0), "🙂\u{2026}");
    }

    #[test]
    fn the_search_measures_logarithmically_many_candidates() {
        let content = "a".repeat(1024);
        let mut calls = 0;
        let mut scratch = String::new();
        let fit = fit_one_line(
            &content,
            500.0,
            |text| {
                calls += 1;
                chars(text)
            },
            &mut scratch,
        );
        assert_eq!(fit, Fit::Prefix(499));
        // The whole string, the bare ellipsis, then about log2(1024) candidates.
        assert!(calls <= 2 + 11, "{calls} measurements");
    }
}
