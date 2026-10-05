//! The colours the catalog boards draw that no token names: the Select workspace's grid, sources,
//! filter bar, sheets, filmstrip and Missing originals.
//!
//! Each is one mix of the running palette's tokens, per channel in encoded sRGB and rounded to the
//! nearest code, as the core derives a theme's tokens ([UI themes](../../../../docs/design/ui-themes.md#derived-tokens)).
//! The weights are fitted to Luxforge Dark, so it draws the board's value; the tests below hold
//! each to its board value, exactly or within the one code named there. They follow every theme
//! the way the tokens they mix do, and a theme cannot set one explicitly.

use super::Palette;
use iced::Color;

/// One derived colour, named where a widget model chooses it before the theme is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Derived {
    /// A Select cell's ground (`.cc`), and the Select title bar and filter bar (`.tb`, `.fbar`): a
    /// step between the surround and the panel.
    CellSurface,
    /// A selected Select cell (`.cc.sel`): halfway from the control to the selected tab.
    CellSelected,
    /// A photograph not decoded yet, and a filter chip at rest: halfway from the panel to the
    /// control.
    CellPlaceholder,
    /// A moment row (`.mom`): between the surround and the control.
    MomentSurface,
    /// A moment row's outline: the text at 7.8% over the row.
    MomentOutline,
    /// A month or group label under a sources heading (`.mon`): the tertiary text 71% toward the
    /// faint.
    SourceMonth,
    /// The rule under the Select bars, and between the facet columns: the text at 6.7% over the
    /// bar.
    SelectBarRule,
    /// A set filter chip: the accent at 16% over the bar.
    FilterChipSet,
    /// A set filter chip's clear glyph: the accent 16.6% toward the bar.
    FilterChipClear,
    /// The rule above a sheet's footer: the text at 7.8% over the panel.
    SheetFooterRule,
    /// The filmstrip's active frame: the panel 84.5% toward the control.
    FrameActive,
    /// The focus inset's outline: the text at 13.2% over the surface.
    FocusInsetBorder,
    /// A Missing originals group (`.grp`): the surround 86% toward the panel.
    ResolveGroup,
    /// A group's outline: the text at 7.8% over the group.
    ResolveGroupOutline,
    /// A group's header rule, and a selected row: the text at 5.5% over the group.
    ResolveHeaderRule,
    /// The rule between a group's rows: the text at 4.5% over the group.
    ResolveRowRule,
    /// A facet column's heading rule: the text at 5.4% over the bar.
    FacetHeadingRule,
    /// A selected facet row: the accent at 12% over the bar.
    FacetRowSelected,
    /// A facet row under the pointer: the text at 4.45% over the bar.
    FacetRowHover,
    /// An organize chip's outline: the tertiary text at 19% over the control.
    OrganizeChipOutline,
    /// A filled organize chip under the pointer: the text at 4.3% over the control.
    RowHoverOnControl,
}

impl Derived {
    /// The colour this resolves to in `palette`.
    pub fn resolve(self, palette: &Palette) -> Color {
        let p = palette;
        match self {
            Derived::CellSurface => mix(p.surround, p.background, 0.6),
            Derived::CellSelected => mix(p.control, p.tab_selected, 0.55),
            Derived::CellPlaceholder => mix(p.background, p.control, 0.5),
            Derived::MomentSurface => mix(p.surround, p.control, 0.39),
            Derived::MomentOutline => mix(Derived::MomentSurface.resolve(p), p.text, 0.078),
            Derived::SourceMonth => mix(p.text_tertiary, p.text_faint, 0.71),
            Derived::SelectBarRule => mix(Derived::CellSurface.resolve(p), p.text, 0.067),
            Derived::FilterChipSet => mix(Derived::CellSurface.resolve(p), p.accent, 0.16),
            Derived::FilterChipClear => mix(p.accent, Derived::CellSurface.resolve(p), 0.166),
            Derived::SheetFooterRule => mix(p.background, p.text, 0.078),
            Derived::FrameActive => mix(p.background, p.control, 0.845),
            Derived::FocusInsetBorder => mix(p.surface, p.text, 0.132),
            Derived::ResolveGroup => mix(p.surround, p.background, 0.86),
            Derived::ResolveGroupOutline => mix(Derived::ResolveGroup.resolve(p), p.text, 0.078),
            Derived::ResolveHeaderRule => mix(Derived::ResolveGroup.resolve(p), p.text, 0.055),
            Derived::ResolveRowRule => mix(Derived::ResolveGroup.resolve(p), p.text, 0.045),
            Derived::FacetHeadingRule => mix(Derived::CellSurface.resolve(p), p.text, 0.054),
            Derived::FacetRowSelected => mix(Derived::CellSurface.resolve(p), p.accent, 0.12),
            Derived::FacetRowHover => mix(Derived::CellSurface.resolve(p), p.text, 0.0445),
            Derived::OrganizeChipOutline => mix(p.control, p.text_tertiary, 0.19),
            Derived::RowHoverOnControl => mix(p.control, p.text, 0.043),
        }
    }
}

/// `from` moved `weight` of the way to `to`, per channel in encoded sRGB, rounded to the nearest
/// 8-bit code, and opaque.
pub fn mix(from: Color, to: Color, weight: f32) -> Color {
    let channel = |a: f32, b: f32| ((a + (b - a) * weight) * 255.0).round() / 255.0;
    Color {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
        a: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(colour: Color) -> [u8; 3] {
        [colour.r, colour.g, colour.b].map(|channel| (channel * 255.0).round() as u8)
    }

    /// Luxforge Dark draws each board value: exactly, or within the one code named.
    #[test]
    fn luxforge_dark_draws_the_catalog_boards_values() {
        let dark = Palette::luxforge_dark();
        let exact = [
            (Derived::CellSurface, [0x1d, 0x1d, 0x20]),
            (Derived::CellSelected, [0x34, 0x34, 0x3a]),
            (Derived::CellPlaceholder, [0x26, 0x26, 0x2a]),
            (Derived::MomentSurface, [0x20, 0x20, 0x24]),
            (Derived::MomentOutline, [48, 48, 51]),
            (Derived::SourceMonth, [0x5f, 0x5f, 0x66]),
            (Derived::FilterChipSet, [61, 53, 44]),
            (Derived::FilterChipClear, [193, 155, 94]),
            (Derived::FrameActive, [0x2a, 0x2a, 0x2f]),
            (Derived::FocusInsetBorder, [61, 61, 64]),
            (Derived::ResolveGroup, [0x1f, 0x1f, 0x22]),
            (Derived::ResolveHeaderRule, [42, 42, 45]),
            (Derived::ResolveRowRule, [40, 40, 43]),
            (Derived::FacetHeadingRule, [40, 40, 43]),
            (Derived::FacetRowSelected, [53, 47, 41]),
            (Derived::FacetRowHover, [38, 38, 41]),
            (Derived::OrganizeChipOutline, [0x3a, 0x3a, 0x40]),
            (Derived::RowHoverOnControl, [52, 52, 57]),
        ];
        for (derived, board) in exact {
            assert_eq!(code(derived.resolve(&dark)), board, "{derived:?}");
        }
        // No single weight lands all three channels of these; each is one code off in one channel.
        let near = [
            (Derived::SelectBarRule, [43, 43, 45], [43, 43, 46]),
            (Derived::SheetFooterRule, [48, 48, 50], [48, 48, 51]),
            (Derived::ResolveGroupOutline, [47, 47, 49], [47, 47, 50]),
        ];
        for (derived, board, drawn) in near {
            assert_eq!(code(derived.resolve(&dark)), drawn, "{derived:?}");
            let off = board
                .into_iter()
                .zip(drawn)
                .map(|(a, b): (u8, u8)| a.abs_diff(b))
                .max();
            assert_eq!(off, Some(1), "{derived:?}");
        }
    }

    /// A derived colour follows the tokens it mixes: a palette whose panel and surround change
    /// moves the cell's ground with them.
    #[test]
    fn a_derived_colour_follows_its_tokens() {
        let dark = Palette::luxforge_dark();
        let mut light = dark;
        light.surround = Color::from_rgb8(0xe0, 0xe0, 0xe2);
        light.background = Color::from_rgb8(0xf0, 0xf0, 0xf2);
        assert_eq!(
            code(Derived::CellSurface.resolve(&light)),
            [0xea, 0xea, 0xec]
        );
        assert_ne!(
            Derived::CellSurface.resolve(&light),
            Derived::CellSurface.resolve(&dark)
        );
    }
}
