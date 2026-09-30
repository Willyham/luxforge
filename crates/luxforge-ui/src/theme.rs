//! Design tokens for the Develop workspace and the styling functions built from them.
//!
//! Every colour, size, spacing and radius a widget uses comes from this module, so the app never
//! writes an ad hoc colour or size. Values are copied from the visual language table in
//! `docs/design/develop-workspace.md`; the unit tests in this module assert the copy is exact.

use iced::font::Weight;
use iced::widget::{button, container, slider, text_input};
use iced::{Background, Border, Color, Font, Padding, Shadow, Theme};

// -- Surfaces ---------------------------------------------------------------------------------

/// The darkest surface; the photograph sits on it.
pub const CANVAS: Color = Color::from_rgb8(0x19, 0x19, 0x1b);
/// Side panels and status bar.
pub const PANEL: Color = Color::from_rgb8(0x20, 0x20, 0x23);
/// Title bar, floating strips, notices.
pub const BAR: Color = Color::from_rgb8(0x23, 0x23, 0x26);
/// Buttons, chips, text fields.
pub const CONTROL: Color = Color::from_rgb8(0x2c, 0x2c, 0x31);
/// All dividers and outlines: 6% white.
pub const BORDER: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.06,
};

// -- Text ---------------------------------------------------------------------------------------

/// Primary text.
pub const TEXT_PRIMARY: Color = Color::from_rgb8(0xe8, 0xe8, 0xea);
/// Secondary text.
pub const TEXT_SECONDARY: Color = Color::from_rgb8(0xa8, 0xa8, 0xae);
/// Tertiary text.
pub const TEXT_TERTIARY: Color = Color::from_rgb8(0x77, 0x77, 0x7f);
/// A slider or field label: a step under primary, so the value on the same line reads first.
pub const TEXT_LABEL: Color = Color::from_rgb8(0xc9, 0xc9, 0xce);
/// Faint text, a step under tertiary: a finished job's duration, which matters less than the dimmed
/// label beside it, as the performance mockup draws it. Opaque for the same reason as [`RULE`].
pub const TEXT_FAINT: Color = Color::from_rgb8(0x55, 0x55, 0x5c);

// -- Slider rail and rules ----------------------------------------------------------------------

/// The empty rail.
pub const RAIL: Color = Color::from_rgb8(0x3a, 0x3a, 0x40);
/// The rail's fill between the zero tick (or the rail's start) and the handle, in every state.
pub const RAIL_FILL: Color = Color::from_rgb8(0xa3, 0xa3, 0xaa);
/// The zero tick across the rail.
pub const ZERO_TICK: Color = Color::from_rgb8(0x5a, 0x5a, 0x62);
/// The resting handle.
pub const THUMB: Color = Color::from_rgb8(0xec, 0xec, 0xee);
/// The dark ring around the handle that separates it from a light or colour rail.
pub const THUMB_OUTLINE: Color = Color::from_rgb8(0x11, 0x11, 0x13);
/// The `temperature` rail hint's stops, blue through a neutral grey to amber. A colour rail is
/// drawn at [`DECORATED_RAIL_OPACITY`] over the panel, so these are the colours that composite to
/// the module references' samples (asserted in the tests below).
pub const TEMPERATURE_RAIL: [Color; 3] = [
    Color::from_rgb8(77, 139, 223),
    Color::from_rgb8(143, 143, 148),
    Color::from_rgb8(226, 179, 107),
];
/// The `tint` rail hint's stops, green through a neutral grey to magenta.
pub const TINT_RAIL: [Color; 3] = [
    Color::from_rgb8(87, 181, 107),
    Color::from_rgb8(141, 144, 147),
    Color::from_rgb8(217, 95, 208),
];
/// A group header's hairline rule. Opaque rather than a white alpha like [`BORDER`]: Iced blends
/// in linear light, which renders a small white alpha far brighter than the references do.
pub const RULE: Color = Color::from_rgb8(0x31, 0x31, 0x34);
/// The 1 px border above each module band, opaque for the same reason as [`RULE`].
pub const BAND_BORDER: Color = Color::from_rgb8(0x2f, 0x2f, 0x32);
/// A value's inset from the right edge of its box, so a typed value does not jump when the field
/// opens for editing.
pub const VALUE_INSET: f32 = 3.0;

// -- Ink and accent -------------------------------------------------------------------------

/// The one warm accent. Used only for: the current history entry, an active canvas mode, a
/// non-neutral module dot, a slider being dragged, a Custom group caption, and Apply. Never for a
/// slider's rail fill, which uses [`RAIL_FILL`] instead (see [`slider_style`]).
pub const ACCENT: Color = Color::from_rgb8(0xe2, 0xb4, 0x6a);
/// Highlight clipping indicator. Reserved for clipping; never reused as a general warning tint.
pub const CLIPPING_HIGHLIGHT: Color = Color::from_rgb8(0xe5, 0x53, 0x4b);
/// Shadow clipping indicator. Reserved for clipping.
pub const CLIPPING_SHADOW: Color = Color::from_rgb8(0x4c, 0x8b, 0xe0);
/// The overlay colour of a cell that holds both endpoints. It is not a third invented colour: it
/// takes its red and green from [`CLIPPING_HIGHLIGHT`] and its blue from [`CLIPPING_SHADOW`], which
/// is exactly what "red and blue at once" means and reads as magenta over a photograph. The
/// composition is asserted in this module's tests rather than written out twice.
pub const CLIPPING_BOTH: Color = Color {
    r: CLIPPING_HIGHLIGHT.r,
    g: CLIPPING_HIGHLIGHT.g,
    b: CLIPPING_SHADOW.b,
    a: 1.0,
};

/// The mask overlay tints, one per name in the core's `MaskOverlayColour`. Reserved for the mask
/// overlay: they say "this is the selection", never "this is clipped".
///
/// They are deliberately not red. Clipping already owns red, blue and the magenta between them on
/// this canvas, and an overlay a person cannot tell apart from a clipping indicator is worse than
/// no overlay at all, so each of these is a long way from all three — the distance is measured in
/// this module's tests rather than claimed. The [masking design](../../../docs/design/masking.md)
/// records red as the default tint, from Lightroom; that default predates the clipping tokens and
/// is the owner's to settle.
pub const MASK_OVERLAY_GREEN: Color = Color::from_rgb8(0x3f, 0xd0, 0x7a);
/// The neutral mask overlay tint, for a scene the green reads into.
pub const MASK_OVERLAY_WHITE: Color = Color::from_rgb8(0xf2, 0xf2, 0xf5);

/// The three histogram channel fills. They are the plain additive primaries rather than tinted
/// versions of them, because the plot's whole job is to say which channel a count belongs to and
/// what their overlap is; the alpha below is what makes the overlap readable.
pub const CHANNEL_RED: Color = Color::from_rgb8(0xff, 0x4d, 0x4d);
pub const CHANNEL_GREEN: Color = Color::from_rgb8(0x4d, 0xff, 0x7a);
pub const CHANNEL_BLUE: Color = Color::from_rgb8(0x4d, 0x9a, 0xff);
/// How opaque one channel fill is. Three overlapping fills at this alpha keep each channel
/// readable on its own and turn a full overlap into the grey the contract describes.
pub const CHANNEL_ALPHA: f32 = 0.55;

/// The colour of a composition guide drawn over the photograph (the thirds overlay, the crop
/// overlay's own thirds). It is [`BORDER`]'s white at the opacity a line needs to stay readable
/// over an image rather than over a panel, which is why it is its own token and not a reuse of a
/// chrome colour.
pub const GUIDE: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.30,
};

// -- Typeface -------------------------------------------------------------------------------

/// The one family every piece of workspace text is set in: Inter, bundled so a real semibold
/// instance exists on every platform. Iced's text engine cannot pick a semibold instance out of
/// the macOS variable system font, so a `Font::DEFAULT` weight request rendered regular.
pub const FONT_FAMILY: &str = "Inter";
/// Regular text; also the application's default font, so a widget that names no font uses it.
pub const FONT: Font = Font::with_name(FONT_FAMILY);
/// Semibold text: module and section titles and sub-group labels.
pub const FONT_SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..FONT
};
/// The bundled static instances of [`FONT_FAMILY`] (Inter 4.1, SIL Open Font License 1.1; see
/// `crates/luxforge-ui/THIRD_PARTY.md`). The application registers them once at startup.
pub const FONT_FILES: [&[u8]; 2] = [
    include_bytes!("../assets/fonts/inter-4.1/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/inter-4.1/Inter-SemiBold.ttf"),
];

// -- Type sizes -----------------------------------------------------------------------------

/// Control text.
pub const SIZE_CONTROL: f32 = 12.0;
/// Semibold module and section titles.
pub const SIZE_TITLE: f32 = 13.0;
/// Captions.
pub const SIZE_CAPTION: f32 = 11.0;
/// Capitalised section labels.
pub const SIZE_SECTION_LABEL: f32 = 10.5;
/// A small caption in sentence case: a metric's unit, a job's elapsed time and detail line, and a
/// disclosure heading's caption. It is the section label's step of the type scale, so a caption
/// beside a section label reads as the same size.
pub const SIZE_SMALL_CAPTION: f32 = SIZE_SECTION_LABEL;
/// A caption's line: a whole number of points, so the rows stacked under a block of captions (the
/// histogram's readout) start on a whole point and their 1 px rules stay sharp.
pub const CAPTION_LINE_HEIGHT: f32 = 14.0;

// -- Grid ------------------------------------------------------------------------------------

/// The 8 pt spacing grid unit.
pub const SPACING: f32 = 8.0;
/// The corner radius used across controls, chips and cards.
pub const RADIUS: f32 = 6.0;
/// The 1 px border width used across dividers and outlines.
pub const BORDER_WIDTH: f32 = 1.0;
/// A fixed width for a right-aligned value field (see [`crate::value_text`]'s tabular-numeral
/// note). A value with a word unit, such as `+0.35 EV`, may run past the box's left edge rather
/// than wrap, so the right edge, where the units digit sits, never moves.
pub const VALUE_WIDTH: f32 = 48.0;
/// A square icon button: title-bar actions and context toggles.
pub const ICON_BUTTON_SIZE: f32 = 26.0;
/// The icon inside an [`ICON_BUTTON_SIZE`] button.
pub const ICON_SIZE: f32 = 16.0;
/// Every vector icon's stroke, in points whatever the icon's size, so a 14 pt reset and a 16 pt
/// mode icon draw the same line.
pub const ICON_STROKE_WIDTH: f32 = 1.2;
/// The histogram plot's height at the top of the tools panel, its outline included, as the Develop
/// workspace boards draw it. The clipping triangles sit inside it; see [`HISTOGRAM_PADDING`].
pub const HISTOGRAM_HEIGHT: f32 = 88.0;

// -- Module panel density ---------------------------------------------------------------------
//
// The tools panel's rows, from the Density table of the Develop workspace design's Module panels
// section, as measured on its module references. Every section, group and control row takes its
// size from here.

/// A module band: the section header row on the Bar surface, the same height expanded or collapsed.
pub const MODULE_HEADER_HEIGHT: f32 = 32.0;
/// The band's inset before its disclosure.
pub const MODULE_HEADER_PADDING_LEFT: f32 = 10.0;
/// The band's inset after its hint or reset.
pub const MODULE_HEADER_PADDING_RIGHT: f32 = 6.0;
/// The accent dot that marks a non-neutral module, or a tab whose group is Custom.
pub const DOT_SIZE: f32 = 6.0;
/// Between the band's disclosure, title and dot.
pub const MODULE_HEADER_SPACING: f32 = 7.0;
/// The expanded section's body: 4 pt top, 12 pt sides, 10 pt bottom.
pub const SECTION_PADDING: Padding = Padding {
    top: 4.0,
    right: 12.0,
    bottom: 10.0,
    left: 12.0,
};
/// Between consecutive rows in a section body: sliders are separated only by this gap.
pub const ROW_SPACING: f32 = 2.0;
/// A sub-group header row.
pub const GROUP_HEADER_HEIGHT: f32 = 24.0;
/// The margin above a sub-group header, on top of [`ROW_SPACING`].
pub const GROUP_MARGIN: f32 = 4.0;
/// Between a group header's disclosure and its label.
pub const GROUP_HEADER_SPACING: f32 = 7.0;
/// Between a group header's label, rule and caption.
pub const GROUP_RULE_SPACING: f32 = 7.0;
/// The rule's extra lead after the label, which has no right side bearing to give it air.
pub const GROUP_RULE_LEAD: f32 = 1.0;
/// Between a group header's caption and its reset.
pub const GROUP_RESET_SPACING: f32 = 6.0;
/// The disclosure chevron in a band or a group header.
pub const DISCLOSURE_SIZE: f32 = 11.0;
/// The reset icon in a band or a group header.
pub const HEADER_ICON_SIZE: f32 = 14.0;
/// The reset button's square hit box in a band or a group header.
pub const HEADER_BUTTON_SIZE: f32 = 20.0;
/// A slider's label line: the label and, right-aligned, its value.
pub const SLIDER_LABEL_HEIGHT: f32 = 14.0;
/// Between a slider's label line and its rail line.
pub const SLIDER_GAP: f32 = 2.0;
/// A slider's rail line: the rail, the zero tick and the handle.
pub const SLIDER_RAIL_HEIGHT: f32 = 12.0;
/// A whole slider row.
pub const SLIDER_ROW_HEIGHT: f32 = SLIDER_LABEL_HEIGHT + SLIDER_GAP + SLIDER_RAIL_HEIGHT;
/// A plain rail's thickness.
pub const RAIL_WIDTH: f32 = 2.0;
/// A colour rail's thickness, a little heavier so its colours read.
pub const DECORATED_RAIL_WIDTH: f32 = 3.0;
/// How strongly a colour rail's declared colours sit over the panel: muted a little, so the thumb
/// stays the brightest mark on the row, as the module references draw every colour rail.
pub const DECORATED_RAIL_OPACITY: f32 = 0.85;
/// The longest piece a colour rail is drawn in, each a two-stop gradient between exactly mixed
/// colours (see [`crate::geometry::rail_pieces`]).
pub const RAIL_PIECE_LENGTH: f32 = 8.0;
/// The handle's radius including its outline ring: a 12 pt handle inside a 1 pt ring.
pub const THUMB_RADIUS: f32 = 7.0;
/// The ring around the handle.
pub const THUMB_OUTLINE_WIDTH: f32 = 1.0;
/// The accent halo around a dragged handle: an 18 pt disc, 3 pt beyond the 12 pt handle.
pub const THUMB_HALO_RADIUS: f32 = 9.0;
/// How strongly the halo's accent sits over the panel and the rail under it.
pub const THUMB_HALO_OPACITY: f32 = 0.25;
/// A range's shoulder grip: an 8 pt disc at the outer end of a shoulder (mask-panels.png, the
/// luminance range's `.sth`), in the tertiary text colour so it reads as secondary to a thumb.
pub const SHOULDER_GRIP_RADIUS: f32 = 4.0;
/// The zero tick's height across the rail.
pub const ZERO_TICK_HEIGHT: f32 = 6.0;
/// The zero tick's width.
pub const ZERO_TICK_WIDTH: f32 = 1.0;
/// The accent mark at a rail's end when the value lies beyond the soft range on that side.
pub const OVER_RANGE_MARK: iced::Size = iced::Size {
    width: 2.0,
    height: 9.0,
};
/// A button on a row of its own in a section: an action (Crop, Apply), or a cell of an icon row.
pub const BUTTON_HEIGHT: f32 = 26.0;
/// A labelled button in a row under a group's sliders that holds a picker.
pub const COMPACT_BUTTON_HEIGHT: f32 = 22.0;
/// A [`BUTTON_HEIGHT`] button's inset on either side of its content.
pub const BUTTON_PADDING: f32 = 10.0;
/// A [`COMPACT_BUTTON_HEIGHT`] button's inset on either side of its content.
pub const COMPACT_BUTTON_PADDING: f32 = 7.0;
/// The inset of a tooltip's text inside its Bar surface.
pub const TOOLTIP_PADDING: f32 = 6.0;
/// Between a labelled button's icon and label.
pub const BUTTON_ICON_SPACING: f32 = 5.0;
/// Between a labelled button's label and its key hint.
pub const BUTTON_HINT_SPACING: f32 = 10.0;
/// A labelled button's icon.
pub const BUTTON_ICON_SIZE: f32 = 14.0;
/// The margin above a button row, on top of [`ROW_SPACING`].
pub const BUTTON_ROW_MARGIN: f32 = 4.0;
/// The margin above a button row that comes straight under a group header, on top of
/// [`ROW_SPACING`].
pub const HEADER_BUTTON_ROW_MARGIN: f32 = 2.0;
/// The margin under a button row, on top of [`ROW_SPACING`], before the group header that follows.
pub const BUTTON_ROW_BOTTOM: f32 = 2.0;
/// Between buttons in one row.
pub const BUTTON_ROW_SPACING: f32 = 6.0;
/// A chip: a ratio preset or a version.
pub const CHIP_HEIGHT: f32 = 22.0;
/// A chip's inset on either side of its label.
pub const CHIP_PADDING: f32 = 8.0;
/// Between a chip's label and its trailing caption.
pub const CHIP_TRAILING_SPACING: f32 = 4.0;
/// Between chips, across a row and between wrapped rows.
pub const CHIP_SPACING: f32 = 4.0;
/// The margin under a row of chips that another row follows, on top of [`ROW_SPACING`].
pub const CHIP_ROW_BOTTOM: f32 = 4.0;
/// An unselected chip's label, a step under the label colour, as the crop reference draws the
/// ratios not chosen.
pub const CHIP_LABEL: Color = Color::from_rgb8(176, 176, 182);
/// A selected chip's fill: the accent laid over the panel at about 16%, opaque so Iced's linear
/// blending does not lighten it, as the crop reference draws the chosen ratio.
pub const SELECTED_FILL: Color = Color::from_rgb8(62, 55, 46);
/// A number field's row: the label, the value box and any unit.
pub const FIELD_ROW_HEIGHT: f32 = 24.0;
/// A field row packed two to a row under a control it belongs to, such as a range's four fields
/// (mask-panels.png, `.fields .fld`): the same 20 pt box on a 22 pt row.
pub const COMPACT_FIELD_ROW_HEIGHT: f32 = 22.0;
/// A number field's value box.
pub const FIELD_WIDTH: f32 = 56.0;
/// A value box's height.
pub const FIELD_HEIGHT: f32 = 20.0;
/// A colour channel's value box, three to a row after the swatch.
pub const CHANNEL_FIELD_WIDTH: f32 = 40.0;
/// Between colour channel boxes.
pub const CHANNEL_FIELD_SPACING: f32 = 4.0;
/// Between a colour row's swatch and its first channel box.
pub const SWATCH_SPACING: f32 = 3.0;
/// A colour swatch in a field row, its ring included.
pub const SWATCH_SIZE: f32 = 16.0;
/// A colour swatch's corner radius.
pub const SWATCH_RADIUS: f32 = 3.0;
/// A value box's text inset from its right edge.
pub const FIELD_INSET: f32 = 6.0;
/// A value box's inset above and below the editing input's line.
pub const FIELD_PADDING_Y: f32 = 2.0;
/// Between a field row's label, its box and a stepper's buttons and rail.
pub const FIELD_UNIT_SPACING: f32 = 6.0;
/// Between a value box and the word unit after it (`px`), as developer-pixel.png draws it.
pub const FIELD_UNIT_GAP: f32 = 8.0;
/// A toggle's row: its label and the switch.
pub const TOGGLE_ROW_HEIGHT: f32 = 26.0;
/// A switch's track.
pub const SWITCH_WIDTH: f32 = 26.0;
/// A switch's track height; its ends are round.
pub const SWITCH_HEIGHT: f32 = 14.0;
/// A switch's knob.
pub const SWITCH_KNOB: f32 = 10.0;
/// The knob's inset from the track's end.
pub const SWITCH_INSET: f32 = 2.0;
/// A readout card's line: a caption on a 16 pt pitch.
pub const READOUT_LINE_HEIGHT: f32 = 16.0;
/// A readout card's inset above its first line and below its last.
pub const READOUT_PADDING_Y: f32 = 5.5;
/// A readout card's inset at either side.
pub const READOUT_PADDING_X: f32 = 10.0;
/// The margin above a readout card, on top of [`ROW_SPACING`].
pub const READOUT_MARGIN: f32 = 4.0;
/// A segmented tab row that stands in for a module's group headers.
pub const TAB_ROW_HEIGHT: f32 = 24.0;
/// The margin above and below a tab row, on top of [`ROW_SPACING`].
pub const TAB_ROW_MARGIN: f32 = 4.0;
/// The inset between a tab row's track and its selected pill.
pub const TAB_INSET: f32 = 2.0;
/// A tab row's track.
pub const TAB_TRACK: Color = Color::from_rgb8(0x28, 0x28, 0x2c);
/// A tab row's selected pill.
pub const TAB_SELECTED: Color = Color::from_rgb8(0x3b, 0x3b, 0x41);
/// The tools panel's scrollbar. It overlays the section padding's right edge, so it is thin
/// enough to clear a band's reset and a slider's value.
pub const PANEL_SCROLLBAR_WIDTH: f32 = 4.0;
/// The scrollbar's inset from the panel's right edge.
pub const PANEL_SCROLLBAR_MARGIN: f32 = 1.0;
/// A history, version or recipe row.
pub const LIST_ROW_HEIGHT: f32 = 26.0;
/// Between consecutive list rows, and between a list's heading and its first row.
pub const LIST_ROW_SPACING: f32 = 2.0;
/// Under a list's heading, before its first row, on top of [`LIST_ROW_SPACING`].
pub const LIST_HEADING_SPACING: f32 = 6.0;
/// A list row's sequence number, right-aligned in this box.
pub const LIST_LEADING_WIDTH: f32 = 14.0;
/// A list row's marker circle.
pub const MARKER_SIZE: f32 = 6.0;
/// A hollow or previewed marker's ring.
pub const MARKER_RING_WIDTH: f32 = 1.0;
/// The current entry's row, tinted a step above the panel, opaque for the same reason as
/// [`RULE`].
pub const LIST_ROW_CURRENT: Color = Color::from_rgb8(47, 47, 50);

// -- Performance section ------------------------------------------------------------------------
//
// The state panel's Performance block, from the layout table of the performance panel design.
// Its greys are the slider rail's ([`RAIL`], [`RAIL_FILL`], [`THUMB`]) and the group rule
// ([`RULE`]), never the accent: the photograph is the only colour on screen.

/// A disclosure heading's row: the section label, its caption and the chevron, all one button.
pub const DISCLOSURE_HEADING_HEIGHT: f32 = 22.0;
/// A disclosure heading's chevron, a size under a band's [`DISCLOSURE_SIZE`] because it sits
/// beside a 10.5 pt section label rather than a 13 pt title.
pub const DISCLOSURE_CHEVRON_SIZE: f32 = 10.0;
/// A metric row: its label, sparkline and value.
pub const METRIC_ROW_HEIGHT: f32 = 24.0;
/// A metric row's label box.
pub const METRIC_LABEL_WIDTH: f32 = 48.0;
/// A metric row's value box, the value and its unit right-aligned in it, so the last character
/// stays put as the figure changes width (see [`crate::value_text`]'s tabular-numeral note).
pub const METRIC_VALUE_WIDTH: f32 = 56.0;
/// A sparkline's height; its width is whatever its row leaves it.
pub const SPARKLINE_HEIGHT: f32 = 16.0;
/// A sparkline's line.
pub const SPARKLINE_LINE_WIDTH: f32 = 1.25;
/// The dot on a sparkline's newest point. The line's points are inset by this radius on every
/// side, so the dot is never clipped at a window edge, at zero or at the top of the scale.
pub const SPARKLINE_DOT_RADIUS: f32 = 1.75;
/// The area under a sparkline's line: [`RAIL_FILL`] at 16% over [`PANEL`], precomputed opaque
/// because Iced blends in linear light and renders a small alpha much brighter (asserted in the
/// tests below).
pub const SPARKLINE_AREA: Color = Color::from_rgb8(0x35, 0x35, 0x39);
/// A job row's first line: the marker, the label and the elapsed time. The detail line under it is
/// a caption line, [`CAPTION_LINE_HEIGHT`] tall.
pub const JOB_LABEL_HEIGHT: f32 = 16.0;
/// Where a job row's label and detail line start: the [`MARKER_SIZE`] marker and the gap after it.
pub const JOB_LABEL_INSET: f32 = 16.0;
/// Between a job row's detail line and its progress bar, which is a [`RAIL_WIDTH`] rail.
pub const JOB_PROGRESS_GAP: f32 = 2.0;

// -- Canvas chrome ----------------------------------------------------------------------------
//
// The floating chrome over the canvas — the mode strip, the draft bar and the notices — from the
// Canvas section of the Develop workspace design as its boards draw them. The photograph's inset at
// Fit is the desktop's layout (`luxforge-app/src/layout.rs`), whose bottom inset holds the strip.
// The tinted colours are sampled from the boards, precomputed opaque over [`BAR`] for the same
// reason as [`RULE`].

/// How far the floating chrome sits from the canvas edge: the mode strip above the bottom, the draft
/// bar and the notices below the top.
pub const CHROME_INSET: f32 = 12.0;
/// Between the draft bar and a notice stacked under it.
pub const CHROME_STACK_SPACING: f32 = 8.0;
/// The draft bar's and a notice's corner radius.
pub const CHROME_RADIUS: f32 = 9.0;
/// The outline of the floating chrome: 8% white over [`BAR`].
pub const CHROME_BORDER: Color = Color::from_rgb8(0x34, 0x34, 0x37);
/// The soft shadow under the floating chrome, which lifts it off a photograph as the boards draw it.
pub const CHROME_SHADOW: Shadow = Shadow {
    color: Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.35,
    },
    offset: iced::Vector { x: 0.0, y: 4.0 },
    blur_radius: 16.0,
};
/// The mode strip's padding inside its border, the gap between its tools and its radius.
pub const STRIP_PADDING: f32 = 3.0;
pub const STRIP_SPACING: f32 = 2.0;
pub const STRIP_RADIUS: f32 = 10.0;
/// One icon-only tool in the mode strip, and its radius.
pub const STRIP_TOOL_WIDTH: f32 = 34.0;
pub const STRIP_TOOL_HEIGHT: f32 = 30.0;
pub const STRIP_TOOL_RADIUS: f32 = 7.0;
/// A tool's icon at rest; a selected tool's is [`ACCENT`].
pub const STRIP_ICON: Color = Color::from_rgb8(0xb9, 0xb9, 0xbf);
/// A selected tool or an overlay that is on: the accent at 16% over [`BAR`].
pub const STRIP_SELECTED: Color = Color::from_rgb8(0x41, 0x3a, 0x30);
/// The rule between the modes and the view toggles, 1 × [`STRIP_RULE_HEIGHT`] with
/// [`STRIP_RULE_MARGIN`] either side, and the title bar's rule before the panel toggles: 10% white
/// over [`BAR`].
pub const STRIP_RULE: Color = Color::from_rgb8(0x39, 0x39, 0x3c);
pub const STRIP_RULE_HEIGHT: f32 = 16.0;
pub const STRIP_RULE_MARGIN: f32 = 4.0;
/// The strip's whole height: a tool, the padding and the border on both sides.
pub const STRIP_HEIGHT: f32 = STRIP_TOOL_HEIGHT + 2.0 * (STRIP_PADDING + BORDER_WIDTH);
/// The draft bar's height, padding and the gap between its parts.
pub const DRAFT_BAR_HEIGHT: f32 = 34.0;
pub const DRAFT_BAR_PADDING: Padding = Padding {
    top: 0.0,
    right: 6.0,
    bottom: 0.0,
    left: 12.0,
};
pub const DRAFT_BAR_SPACING: f32 = 12.0;
/// The gap between the icon and the name of what a draft bar's gesture edits.
pub const DRAFT_BAR_SUBJECT_SPACING: f32 = 6.0;
/// A notice card's width, padding and the gap between its icon, text and actions.
pub const NOTICE_WIDTH: f32 = 560.0;
pub const NOTICE_PADDING: Padding = Padding {
    top: 10.0,
    right: 10.0,
    bottom: 10.0,
    left: 14.0,
};
pub const NOTICE_SPACING: f32 = 12.0;
/// A notice's title and body sizes.
pub const SIZE_NOTICE_TITLE: f32 = 12.5;
pub const SIZE_NOTICE_BODY: f32 = 11.5;
/// A step brighter than [`TEXT_PRIMARY`]: a notice's title and the title bar's file name, as the
/// boards set them.
pub const TEXT_BRIGHT: Color = Color::from_rgb8(0xf0, 0xf0, 0xf2);
/// A neutral notice's outline: 10% white over [`BAR`], the same as [`STRIP_RULE`].
pub const NOTICE_BORDER: Color = STRIP_RULE;
/// A notice that needs a decision: the accent at 35% over [`BAR`].
pub const NOTICE_WARNING_BORDER: Color = Color::from_rgb8(0x65, 0x55, 0x3d);
/// A notice that reports a failure: [`CLIPPING_HIGHLIGHT`] over [`BAR`], as the components board
/// samples it.
pub const NOTICE_ERROR_BORDER: Color = Color::from_rgb8(0x71, 0x36, 0x34);
/// The primary button's ink on the accent fill.
pub const PRIMARY_INK: Color = Color::from_rgb8(0x1a, 0x14, 0x08);
/// A crop overlay's corner handle, a square with this side and radius centred on the corner.
pub const CROP_CORNER: f32 = 9.0;
pub const CROP_CORNER_RADIUS: f32 = 1.0;
/// A crop overlay's edge handle, a bar this long and thick along the edge at its midpoint.
pub const CROP_EDGE_LENGTH: f32 = 22.0;
pub const CROP_EDGE_THICKNESS: f32 = 5.0;

// -- Histogram inspector ----------------------------------------------------------------------

/// Around the histogram plot at the top of the tools panel.
pub const HISTOGRAM_PADDING: Padding = Padding {
    top: 10.0,
    right: 12.0,
    bottom: 6.0,
    left: 12.0,
};
/// The plot's corner radius.
pub const HISTOGRAM_RADIUS: f32 = 6.0;
/// The plot's 1 px outline: 5% white over [`CANVAS`].
pub const HISTOGRAM_BORDER: Color = Color::from_rgb8(0x24, 0x24, 0x26);
/// A clipping triangle's size inside the plot, and its inset from the plot's side and bottom.
pub const CLIP_TRIANGLE_WIDTH: f32 = 10.0;
pub const CLIP_TRIANGLE_HEIGHT: f32 = 8.0;
pub const CLIP_TRIANGLE_INSET: f32 = 6.0;
/// A clipping triangle whose endpoint has no pixels.
pub const CLIP_TRIANGLE_REST: Color = TEXT_FAINT;
/// The inspector's whole, unconditional height.
pub const HISTOGRAM_INSPECTOR_HEIGHT: f32 =
    HISTOGRAM_PADDING.top + HISTOGRAM_HEIGHT + HISTOGRAM_PADDING.bottom;

// -- Shell ----------------------------------------------------------------------------------------
//
// The title bar, the state panel and the status bar around the canvas and the tools panel, from
// the default board of the Develop workspace design. Every translucent fill the board draws is
// stored opaque, composited over the surface it sits on, for the reason [`RULE`] gives.

/// The rules between the shell's regions: [`BORDER`], 6% white, as the default board composites it
/// over the Bar surface, which is the module band's border.
pub const DIVIDER: Color = BAND_BORDER;
/// The dimensions, format and colour space after the file name.
pub const TEXT_IDENTITY: Color = Color::from_rgb8(0x8a, 0x8a, 0x90);
/// The current history row's label.
pub const TEXT_CURRENT_ROW: Color = Color::from_rgb8(0xf2, 0xf2, 0xf4);
/// The dimensions line's size, between a caption and control text.
pub const SIZE_IDENTITY: f32 = 11.5;
/// A title-bar icon button's fill under the pointer: 6% white over the Bar surface.
pub const ICON_HOVER: Color = Color::from_rgb8(0x30, 0x30, 0x33);
/// A selected icon button's fill: [`ACCENT`] at 14% over the Bar surface.
pub const ICON_SELECTED_FILL: Color = Color::from_rgb8(0x3e, 0x37, 0x2f);
/// The toolbar rule's height and the margin on either side of it.
pub const TOOLBAR_RULE_HEIGHT: f32 = 16.0;
pub const TOOLBAR_RULE_MARGIN: f32 = 6.0;
/// Between the title bar's groups of controls: the identity, the view controls.
pub const TITLE_GROUP_SPACING: f32 = 10.0;
/// Between the title bar's trailing actions.
pub const TITLE_ACTION_SPACING: f32 = 4.0;
/// The title bar's padding at its trailing edge, and at its leading edge where the window keeps its
/// native frame.
pub const TITLE_BAR_INSET: f32 = 12.0;
/// A segmented control's track and its selected segment: the tab row's greys.
pub const SEGMENT_TRACK: Color = TAB_TRACK;
pub const SEGMENT_SELECTED: Color = TAB_SELECTED;
/// A segment's height, its label's horizontal padding, and the track's inset and gap around and
/// between segments.
pub const SEGMENT_HEIGHT: f32 = 24.0;
pub const SEGMENT_PADDING: f32 = 10.0;
pub const SEGMENT_INSET: f32 = 2.0;
/// The track's corner radius; a segment inside it is [`RADIUS`] less its inset, so the two
/// curves stay concentric.
pub const SEGMENT_RADIUS: f32 = 7.0;
/// The state panel's padding: above its first section and below its last, and at its sides. A row
/// adds its own [`SPACING`] inside, so text sits 16 pt from the panel's edge.
pub const PANEL_PADDING_Y: f32 = 12.0;
pub const PANEL_PADDING_X: f32 = 8.0;
/// Between the state panel's sections.
pub const PANEL_SECTION_SPACING: f32 = 12.0;
/// A panel section's heading row: its capitalised label, and a caption or button at its right.
pub const PANEL_HEADING_HEIGHT: f32 = 22.0;
/// Between a panel heading and the chips under it, and between two chips.
pub const VERSION_CHIP_SPACING: f32 = 6.0;
/// A version chip: smaller than a ratio chip, its label at caption size.
pub const VERSION_CHIP_HEIGHT: f32 = 16.0;
pub const VERSION_CHIP_PADDING: f32 = 7.0;
pub const VERSION_CHIP_RADIUS: f32 = 4.0;
/// A list row's corner radius.
pub const LIST_ROW_RADIUS: f32 = 5.0;
/// The status bar's text and the spacing between its trailing facts.
pub const STATUS_SPACING: f32 = 8.0;
pub const STATUS_FACT_SPACING: f32 = 12.0;
/// The dot beside the connected-agents count, and its colour while any other client is connected.
pub const STATUS_DOT_SIZE: f32 = 6.0;
pub const AGENT_CONNECTED: Color = Color::from_rgb8(0x57, 0xb5, 0x6b);

// -- Select: the thumbnail grid ---------------------------------------------------------------

// The Select workspace's grid, cells and moment rows, from the catalog boards' CSS (`.cc`,
// `.mom`, `.dayh`, `.camh`, `.wrap`, and `.cell` for the catalog's larger cells).
//
// A translucent fill over a known surface is stored opaque, composited over it, for the reason
// [`RULE`] gives. A badge sits over the photograph, whose colour is not known, so it stays
// translucent; its alpha is raised so that Iced's linear-light blend over a mid-grey photograph
// lands where the board's sRGB blend does. The offline photograph's opacity is corrected the same
// way over the cell. The tests below recompute each.

/// A Select cell (`.cc`): 136 × 122 pt at the default size, rounded [`RADIUS`].
pub const CELL_SIZE: iced::Size = iced::Size {
    width: 136.0,
    height: 122.0,
};
/// A Select cell's image box (`.cc .im`, 98 pt tall with 8/8/2 pt padding): the photograph is
/// fitted within [`CELL_IMAGE_MAX`] and centred in this box.
pub const CELL_IMAGE: iced::Rectangle = iced::Rectangle {
    x: 8.0,
    y: 8.0,
    width: 120.0,
    height: 88.0,
};
/// The largest fitted photograph in a Select cell (`.cc .im img`).
pub const CELL_IMAGE_MAX: iced::Size = iced::Size {
    width: 120.0,
    height: 86.0,
};
/// A Select cell's footer (`.cc .ft`): 24 pt under the image box, its label 10.5 pt, 9 pt in.
pub const CELL_FOOTER_TOP: f32 = 98.0;
pub const CELL_FOOTER_HEIGHT: f32 = 24.0;
pub const CELL_FOOTER_INSET: f32 = 9.0;
pub const SIZE_CELL_LABEL: f32 = 10.5;
/// Between a footer's label and the edited dot (the catalog cell's `.ft` gap).
pub const CELL_FOOTER_SPACING: f32 = 6.0;
/// Where a Select cell's badges sit from its corner (`.tk`, `.cnt`: 11 pt from the top and side).
pub const CELL_BADGE_INSET: f32 = 11.0;
/// A catalog cell (`.cell`): 168 × 176 pt, its image box 142 pt tall with 10/10/4 pt padding and a
/// 148 × 124 pt photograph, a 30 pt footer 10 pt in with an 11 pt label, badges 14 pt in. The
/// footer ends 4 pt above the cell's bottom, as the board's cell does.
pub const CATALOG_CELL_SIZE: iced::Size = iced::Size {
    width: 168.0,
    height: 176.0,
};
pub const CATALOG_CELL_IMAGE: iced::Rectangle = iced::Rectangle {
    x: 10.0,
    y: 10.0,
    width: 148.0,
    height: 128.0,
};
pub const CATALOG_CELL_IMAGE_MAX: iced::Size = iced::Size {
    width: 148.0,
    height: 124.0,
};
pub const CATALOG_CELL_FOOTER_TOP: f32 = 142.0;
pub const CATALOG_CELL_FOOTER_HEIGHT: f32 = 30.0;
pub const CATALOG_CELL_FOOTER_INSET: f32 = 10.0;
pub const SIZE_CATALOG_CELL_LABEL: f32 = 11.0;
pub const CATALOG_CELL_BADGE_INSET: f32 = 14.0;
/// The narrowest cell the size slider may ask for; a narrower request draws this.
pub const CELL_MIN_WIDTH: f32 = 64.0;
/// A cell's ground (`.cc`), and a selected or the active cell's (`.cc.sel`, `.cc.act`).
pub const CELL_SURFACE: Color = Color::from_rgb8(0x1d, 0x1d, 0x20);
pub const CELL_SELECTED: Color = Color::from_rgb8(0x34, 0x34, 0x3a);
/// The active cell's inset accent outline (`.cc.act`: 1.5 pt).
pub const CELL_ACTIVE_OUTLINE: f32 = 1.5;
/// The soft shadow under a photograph (`0 1px 3px rgba(0,0,0,.5)`).
pub const CELL_IMAGE_SHADOW: Shadow = Shadow {
    color: Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.5,
    },
    offset: iced::Vector { x: 0.0, y: 1.0 },
    blur_radius: 3.0,
};
/// A photograph whose preview is not decoded yet, or an unreadable file: a flat neutral rectangle
/// at the photograph's shape, a step over the cell, with no spinner.
pub const CELL_PLACEHOLDER: Color = Color::from_rgb8(0x26, 0x26, 0x2a);
/// An offline photograph's opacity (the library board's `.mis` cell draws it at 45%): 0.27 in
/// Iced's linear-light blend lands a mid-grey photograph over [`CELL_SURFACE`] where 45% does in the
/// board's sRGB blend.
pub const CELL_OFFLINE_OPACITY: f32 = 0.27;
/// A pick (`.tk`): an 18 pt [`ACCENT`] disc holding a 10 pt check in [`PRIMARY_INK`], in a 2 pt
/// ring of `rgba(20,20,22,.6)` (alpha corrected for linear light, see the section's note).
pub const CELL_PICK_SIZE: f32 = 18.0;
pub const CELL_CHECK_SIZE: f32 = 10.0;
pub const CELL_PICK_RING_WIDTH: f32 = 2.0;
pub const CELL_PICK_RING: Color = Color {
    r: 20.0 / 255.0,
    g: 20.0 / 255.0,
    b: 22.0 / 255.0,
    a: 0.794,
};
/// A badge over the photograph (`.cnt`, and `.mis` for an unavailable file): 16 pt tall, 5 pt in,
/// rounded 4 pt, a 10 pt icon 3 pt before its 10.5 pt semibold text ("In the catalog" 4 pt), on
/// `rgba(20,20,22,.82)` (alpha corrected for linear light). Badges on one side are 4 pt apart.
pub const CELL_BADGE_HEIGHT: f32 = 16.0;
pub const CELL_BADGE_PADDING: f32 = 5.0;
pub const CELL_BADGE_RADIUS: f32 = 4.0;
pub const CELL_BADGE_ICON_SIZE: f32 = 10.0;
pub const CELL_BADGE_SPACING: f32 = 3.0;
pub const CELL_CATALOG_BADGE_SPACING: f32 = 4.0;
pub const CELL_BADGE_GAP: f32 = 4.0;
pub const SIZE_CELL_BADGE: f32 = 10.5;
pub const CELL_BADGE_SURFACE: Color = Color {
    r: 20.0 / 255.0,
    g: 20.0 / 255.0,
    b: 22.0 / 255.0,
    a: 0.934,
};
/// The unavailable badge's text ("Offline", "Unreadable"), 10 pt semibold in [`CLIPPING_HIGHLIGHT`].
pub const SIZE_CELL_UNAVAILABLE: f32 = 10.0;
/// A moment row (`.mom`): rounded 9 pt on a step between the canvas and the panel, outlined in
/// white at 7% (stored opaque over the row), 6 pt in at the sides and bottom under a 28 pt header.
pub const MOMENT_RADIUS: f32 = 9.0;
pub const MOMENT_SURFACE: Color = Color::from_rgb8(0x20, 0x20, 0x24);
pub const MOMENT_OUTLINE: Color = Color::from_rgb8(48, 48, 51);
pub const MOMENT_PADDING: f32 = 6.0;
pub const MOMENT_HEADER_HEIGHT: f32 = 28.0;
/// A moment header's inset inside the row's padding, the gap between its parts and its 12 pt kind
/// icon (`.mom .mh`); the action button's extra 6 pt margin before it.
pub const MOMENT_HEADER_INSET: f32 = 4.0;
pub const MOMENT_HEADER_SPACING: f32 = 7.0;
pub const MOMENT_ICON_SIZE: f32 = 12.0;
pub const MOMENT_ACTION_MARGIN: f32 = 6.0;
/// A moment's evidence tag (`.tag`): 10 pt text 6 pt in, 14 pt tall, rounded 4 pt, on [`CONTROL`]
/// in [`TEXT_SECONDARY`].
pub const MOMENT_TAG_PADDING: f32 = 6.0;
pub const MOMENT_TAG_HEIGHT: f32 = 14.0;
pub const MOMENT_TAG_RADIUS: f32 = 4.0;
pub const SIZE_MOMENT_TAG: f32 = 10.0;
/// A moment's action button (`.tb` at 22 pt, 8 pt in, 11 pt text).
pub const MOMENT_ACTION_PADDING: f32 = 8.0;
pub const SIZE_MOMENT_ACTION: f32 = 11.0;
/// The grid's flow (`.wrap`): 6 pt between cells, moments and lines; the centre's 16 pt at the
/// sides, 80 pt under the last line for the floating strip, and 10 pt over a first line of cells
/// (a heading brings its own, as the catalog grid's `padding:10px 16px 80px` and the event board
/// draw them).
pub const THUMB_GRID_GAP: f32 = 6.0;
pub const THUMB_GRID_SIDE_INSET: f32 = 16.0;
pub const THUMB_GRID_TOP_INSET: f32 = 10.0;
pub const THUMB_GRID_BOTTOM_INSET: f32 = 80.0;
/// A day heading (`.dayh`): 14 pt over a 16 pt line and 6 pt under it, 2 pt in; its 13 pt semibold
/// title in [`TEXT_BRIGHT`] and 11 pt detail in [`TEXT_TERTIARY`] 10 pt apart on one baseline.
pub const DAY_HEADING_TOP: f32 = 14.0;
pub const DAY_HEADING_LINE: f32 = 16.0;
pub const DAY_HEADING_BOTTOM: f32 = 6.0;
pub const DAY_HEADING_SPACING: f32 = 10.0;
/// A camera heading (`.camh`): 4 pt over a 13 pt line and 8 pt under it, 2 pt in; its capitalised
/// 10.5 pt semibold name and plain count in [`TEXT_TERTIARY`], 8 pt apart.
pub const CAMERA_HEADING_TOP: f32 = 4.0;
pub const CAMERA_HEADING_LINE: f32 = 13.0;
pub const CAMERA_HEADING_BOTTOM: f32 = 8.0;
pub const CAMERA_HEADING_SPACING: f32 = 8.0;
/// Both headings' inset from the grid's content edge.
pub const HEADING_INSET: f32 = 2.0;
/// The grid's scrollbar: the panel scrollbar's width and margin, a scroller never shorter than
/// this, and the strip at the grid's right edge that takes the pointer for it.
pub const THUMB_GRID_SCROLLER_MIN: f32 = 24.0;
pub const THUMB_GRID_SCROLLBAR_HIT: f32 = 10.0;

// -- end Select: the thumbnail grid ------------------------------------------------------------

// -- Masks panel ----------------------------------------------------------------------------------

// The Masks panel's rows and controls, from the mask-panels board of the masking workspace design
// (`docs/design/develop-workspace/html/mask-panels.html`): the mask and component rows, the mode
// and overlay controls, the kind menu, the two-column fields and the colour range's swatches. Each
// translucent fill the board draws is stored opaque, composited over the surface it sits on, for
// the reason [`RULE`] gives; the tests below recompute each composite.

/// A mask row and a component row (`.mrow`, `.crow`): the list row's height.
pub const MASK_ROW_HEIGHT: f32 = LIST_ROW_HEIGHT;
/// A mask row's inset: 8 pt before the thumbnail, 6 pt after the menu.
pub const MASK_ROW_PADDING: Padding = Padding {
    top: 0.0,
    right: 6.0,
    bottom: 0.0,
    left: 8.0,
};
/// Between a mask row's thumbnail, name, dot, amount and buttons.
pub const MASK_ROW_SPACING: f32 = 8.0;
/// A component row's inset: 6 pt before the grip, 4 pt after the menu.
pub const COMPONENT_ROW_PADDING: Padding = Padding {
    top: 0.0,
    right: 4.0,
    bottom: 0.0,
    left: 6.0,
};
/// Between a component row's grip, kind icon, name, mode control and buttons.
pub const COMPONENT_ROW_SPACING: f32 = 6.0;
/// The open mask's row: [`ACCENT`] at 12% over [`PANEL`]. A selected component row is
/// [`LIST_ROW_CURRENT`], white at 7%, as the history's current row is.
pub const MASK_ROW_SELECTED: Color = Color::from_rgb8(55, 50, 44);
/// A mask or component row under the pointer, or the component row whose coverage the overlay is
/// showing: white at 4% over [`PANEL`], a step under a selected row. The board draws no hover, so
/// this is the smallest step that still reads.
pub const ROW_HOVER: Color = Color::from_rgb8(41, 41, 44);
/// A mask's amount readout, right-aligned in this box so the digits keep their place.
pub const MASK_AMOUNT_WIDTH: f32 = 26.0;
/// A coverage thumbnail (`.th`): 28 × 19 pt, rounded 3 pt, a 1 pt border of white at 8% over its
/// near-black ground.
pub const THUMBNAIL_WIDTH: f32 = 28.0;
pub const THUMBNAIL_HEIGHT: f32 = 19.0;
pub const THUMBNAIL_RADIUS: f32 = 3.0;
pub const THUMBNAIL_BACKGROUND: Color = Color::from_rgb8(0x0e, 0x0e, 0x10);
pub const THUMBNAIL_BORDER: Color = Color::from_rgb8(33, 33, 35);
/// A row's small icon button (`.ib.sm`): the header button's square, holding a
/// [`HEADER_ICON_SIZE`] icon (the eye, the menu) or a [`SMALL_ICON_SIZE`] one (the invert).
pub const SMALL_ICON_SIZE: f32 = 12.0;
/// A component row's kind icon column: a [`HEADER_ICON_SIZE`] icon centred in it.
pub const KIND_ICON_WIDTH: f32 = 16.0;
/// A component row's drag handle.
pub const GRIP_SIZE: f32 = 12.0;
/// The mode control (`.mode`): 18 pt segments inset 1 pt on a track rounded 5 pt, each segment
/// rounded 4 pt, the glyph a [`MODE_GLYPH_SIZE`] icon.
pub const MODE_SEGMENT_SIZE: f32 = 18.0;
pub const MODE_INSET: f32 = 1.0;
pub const MODE_RADIUS: f32 = 5.0;
pub const MODE_SEGMENT_RADIUS: f32 = 4.0;
pub const MODE_GLYPH_SIZE: f32 = 10.0;
/// A fixed mode control (a mask's first component) is drawn at the board's 55% opacity. Its track
/// and segment are dark enough that Iced's linear blending matches the board, so they are
/// [`SEGMENT_TRACK`] and [`SEGMENT_SELECTED`] at this alpha over whatever row they sit on; the ink is
/// [`TEXT_BRIGHT`] at 55% over that segment, precomputed opaque because a light alpha would render
/// far brighter.
pub const MODE_FIXED_OPACITY: f32 = 0.55;
pub const MODE_FIXED_INK: Color = Color::from_rgb8(153, 153, 156);
/// The overlay row (`.ov`): 26 pt, its label in a 52 pt box, its parts 6 pt apart, inset 4 pt.
pub const OVERLAY_ROW_HEIGHT: f32 = 26.0;
pub const OVERLAY_LABEL_WIDTH: f32 = 52.0;
pub const OVERLAY_SPACING: f32 = 6.0;
pub const OVERLAY_PADDING: f32 = 4.0;
/// An overlay segment: 20 pt tall, 6 pt either side of its 12 pt glyph.
pub const OVERLAY_SEGMENT_HEIGHT: f32 = 20.0;
pub const OVERLAY_SEGMENT_PADDING: f32 = 6.0;
pub const OVERLAY_GLYPH_SIZE: f32 = 12.0;
/// How strongly the tint glyph shows the tint, as the board draws it at 60%.
pub const OVERLAY_TINT_GLYPH_OPACITY: f32 = 0.6;
/// A tint swatch (`.sw2`): 12 pt, rounded 3 pt, with a 2 pt accent ring outside it when chosen.
pub const OVERLAY_SWATCH_SIZE: f32 = 12.0;
pub const OVERLAY_SWATCH_RING: f32 = 2.0;
/// A swatch that cannot be chosen (the overlay is not a tint) is drawn at this opacity.
pub const SWATCH_DISABLED_OPACITY: f32 = 0.35;
/// The dark outline round every swatch: black at 50%, which reads on any colour.
pub const SWATCH_OUTLINE: Color = Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.5,
};
/// The mask-on-black glyph's outline (`#666`) and the photograph's stand-in colour in the
/// photo-through-selection glyph (`#7a8a4a`), both the board's.
pub const MASK_GLYPH_OUTLINE: Color = Color::from_rgb8(0x66, 0x66, 0x66);
pub const MASK_GLYPH_PHOTO: Color = Color::from_rgb8(0x7a, 0x8a, 0x4a);
/// A dropdown menu (`.menu`): 200 pt wide on its own surface, inset 5 pt, rounded 8 pt, outlined in
/// white at 10% and lifted by a soft shadow.
pub const MENU_WIDTH: f32 = 200.0;
pub const MENU_PADDING: f32 = 5.0;
pub const MENU_RADIUS: f32 = 8.0;
pub const MENU_SURFACE: Color = Color::from_rgb8(0x2a, 0x2a, 0x2e);
pub const MENU_BORDER: Color = Color::from_rgb8(63, 63, 67);
pub const MENU_SHADOW: Shadow = Shadow {
    color: Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.5,
    },
    offset: iced::Vector { x: 0.0, y: 10.0 },
    blur_radius: 30.0,
};
/// A menu item: 26 pt, inset 8 pt, its icon, label and hint 8 pt apart, rounded 5 pt; white at 6%
/// under the pointer (the board draws no hover).
pub const MENU_ITEM_HEIGHT: f32 = 26.0;
pub const MENU_ITEM_PADDING: f32 = 8.0;
pub const MENU_ITEM_SPACING: f32 = 8.0;
pub const MENU_ITEM_RADIUS: f32 = 5.0;
pub const MENU_ITEM_HOVER: Color = Color::from_rgb8(55, 55, 59);
/// A menu's separator: white at 8% over [`MENU_SURFACE`], 4 pt above and below, 6 pt in from the
/// sides.
pub const MENU_SEPARATOR: Color = Color::from_rgb8(59, 59, 63);
pub const MENU_SEPARATOR_MARGIN: Padding = Padding {
    top: 4.0,
    right: 6.0,
    bottom: 4.0,
    left: 6.0,
};
/// A dropdown button (New mask): 24 pt tall, inset 9 pt, a 12 pt plus and a 10 pt chevron 5 pt from
/// its label. A compact one (Add component) is [`COMPACT_BUTTON_HEIGHT`], inset 8 pt.
pub const DROPDOWN_HEIGHT: f32 = 24.0;
pub const DROPDOWN_PADDING: f32 = 9.0;
pub const COMPACT_DROPDOWN_PADDING: f32 = 8.0;
pub const DROPDOWN_ICON_SIZE: f32 = 12.0;
pub const DROPDOWN_CHEVRON_SIZE: f32 = 10.0;
/// What sits under a selected component row — its fields, strokes, swatches and notes — is inset
/// 22 pt, past the grip, so it reads as the row's.
pub const COMPONENT_DETAIL_INDENT: f32 = 22.0;
/// A component's two-column fields (`.fields .fld`): 22 pt rows 2 pt apart, the columns 10 pt
/// apart, 2 pt above the first row and 4 pt under the last; each an 11.5 pt label and a 52 × 18 pt
/// box rounded 4 pt, 8 pt apart.
pub const GRID_ROW_HEIGHT: f32 = 22.0;
pub const GRID_ROW_SPACING: f32 = 2.0;
pub const GRID_COLUMN_SPACING: f32 = 10.0;
pub const GRID_PADDING_TOP: f32 = 2.0;
pub const GRID_PADDING_BOTTOM: f32 = 4.0;
pub const GRID_FIELD_WIDTH: f32 = 52.0;
pub const GRID_FIELD_HEIGHT: f32 = 18.0;
pub const GRID_FIELD_RADIUS: f32 = 4.0;
pub const GRID_LABEL_SPACING: f32 = 8.0;
pub const SIZE_GRID_FIELD: f32 = 11.5;
/// A toggle row in the Masks panel (Invert mask, Erase): 22 pt, its hint 8 pt after its label.
pub const COMPACT_TOGGLE_ROW_HEIGHT: f32 = 22.0;
pub const TOGGLE_HINT_SPACING: f32 = 8.0;
/// A colour range's swatch slot: 22 × 18 pt, rounded 4 pt, 6 pt apart; an empty slot is a dashed
/// [`TEXT_FAINT`] outline.
pub const SWATCH_SLOT_WIDTH: f32 = 22.0;
pub const SWATCH_SLOT_HEIGHT: f32 = 18.0;
pub const SWATCH_SLOT_RADIUS: f32 = 4.0;
pub const SWATCH_SLOT_SPACING: f32 = 6.0;
/// The dashes of an empty slot's outline.
pub const SWATCH_SLOT_DASH: f32 = 2.0;
/// A brush stroke's row: 20 pt, its 11 pt caption after its number right-aligned in 14 pt, and an
/// 11 pt bin.
pub const STROKE_ROW_HEIGHT: f32 = 20.0;
/// The accent line a reorder drag draws at the edge of the row the dragged row will land on.
pub const DROP_INDICATOR_WIDTH: f32 = 2.0;
/// How much of the panel the dragged row is covered with while it is dragged, so it reads as the
/// row in hand without disappearing.
pub const DRAGGED_ROW_DIM: f32 = 0.45;
pub const STROKE_INDEX_WIDTH: f32 = 14.0;
pub const STROKE_ICON_SIZE: f32 = 11.0;
/// Between a stroke row's number and its caption.
pub const STROKE_ROW_SPACING: f32 = 8.0;
/// Between the stroke rows of one brush component.
pub const STROKE_LIST_SPACING: f32 = 2.0;
/// A note under a component row: its kind's own line, in secondary ink, inset past the grip and
/// 4 pt over the next row.
pub const COMPONENT_NOTE_PADDING: Padding = Padding {
    top: 0.0,
    right: 6.0,
    bottom: 4.0,
    left: COMPONENT_DETAIL_INDENT,
};

/// A row's name while it is renamed in place: a 20 pt input in the name's place.
pub const RENAME_INPUT_HEIGHT: f32 = 20.0;
/// The Masks band's body (`.mod-b` with `padding-top:6px`): the overlay row, the list and New mask.
pub const MASKS_BODY_PADDING: Padding = Padding {
    top: 6.0,
    right: 12.0,
    bottom: 10.0,
    left: 12.0,
};
/// The open mask's body under its group rule (`.mod-b` with `padding-top:0`).
pub const OPEN_MASK_PADDING: Padding = Padding {
    top: 0.0,
    right: 12.0,
    bottom: 10.0,
    left: 12.0,
};
/// The New mask row under the list: 30 pt, its count right-aligned.
pub const NEW_MASK_ROW_HEIGHT: f32 = 30.0;
/// The Add component row under the components: 28 pt, 2 pt under the last row.
pub const ADD_ROW_HEIGHT: f32 = 28.0;
pub const ADD_ROW_MARGIN: f32 = 2.0;
/// Between Invert mask and the first component row.
pub const COMPONENTS_GAP: f32 = 4.0;

// -- Select: sources, filters, title bar, long-running work, loupe and filmstrip ----------------

// The Select workspace's chrome, from the catalog boards' CSS (`.src`, `.fbar`, `.fchip`,
// `.seg`, `.tb`, `.sheet`, `.bar`, `.mstrip`, `.fstrip`). Every translucent fill the boards draw
// over a known surface is stored opaque, composited over that surface, for the reason [`RULE`]
// gives; the tests below recompute each composite. What sits over a photograph (the pick check's
// ring, the pointer's region box) keeps its alpha, since there is no surface to composite over.

/// A source row (`.src`): 24 pt, rounded 5 pt, 8 pt either side plus [`SOURCE_INDENT`] per level,
/// its parts [`SOURCE_ROW_SPACING`] apart.
pub const SOURCE_ROW_HEIGHT: f32 = 24.0;
pub const SOURCE_ROW_PADDING: f32 = 8.0;
pub const SOURCE_INDENT: f32 = 12.0;
pub const SOURCE_ROW_SPACING: f32 = 7.0;
/// A source row's disclosure chevron and its 10 pt column.
pub const SOURCE_CHEVRON_SIZE: f32 = 10.0;
/// A source row's icon column, and the 13 pt icon centred in it, in [`TEXT_IDENTITY`].
pub const SOURCE_ICON_WIDTH: f32 = 14.0;
pub const SOURCE_ICON_SIZE: f32 = 13.0;
/// Between a source's name and its secondary text (an event's dates).
pub const SOURCE_SECONDARY_SPACING: f32 = 6.0;
/// The selected source row: white at 7% over [`PANEL`], the history's current row.
pub const SOURCE_ROW_SELECTED: Color = LIST_ROW_CURRENT;
/// Between the rows of one sources section, and between the panel's sections.
pub const SOURCE_LIST_SPACING: f32 = 1.0;
pub const SOURCE_SECTION_SPACING: f32 = 10.0;
/// A month or a group label under a sources heading (`.mon`): 10.5 pt semibold capitals in this
/// grey, a step under the section label, inset 6 pt above, 8 pt at the sides and 2 pt under.
pub const SOURCE_MONTH: Color = Color::from_rgb8(0x5f, 0x5f, 0x66);
pub const SOURCE_MONTH_PADDING: Padding = Padding {
    top: 6.0,
    right: 8.0,
    bottom: 2.0,
    left: 8.0,
};
/// A neutral tag after a sources heading (`.tag`, "auto"): 10 pt secondary text on [`CONTROL`],
/// 1 pt above and below and 6 pt at the sides, rounded 4 pt, [`SOURCE_TAG_SPACING`] after the
/// label.
pub const SIZE_TAG: f32 = 10.0;
pub const TAG_PADDING: Padding = Padding {
    top: 1.0,
    right: 6.0,
    bottom: 1.0,
    left: 6.0,
};
pub const TAG_RADIUS: f32 = 4.0;
pub const SOURCE_TAG_SPACING: f32 = 8.0;
/// A sources heading (`sec_head`): the state panel's heading row, inset 8 pt.
pub const SOURCE_HEADING_HEIGHT: f32 = PANEL_HEADING_HEIGHT;

/// Filter text: the segments, the chips, the panel's search field and the filter bar's action.
pub const SIZE_FILTER: f32 = 11.5;
/// A search field (`.lsearch`, `.search`): 26 pt on [`CONTROL`], rounded [`RADIUS`], its search
/// icon 8 pt in and 6 pt before the text. The panel's is 11.5 pt with a 12 pt icon; the filter
/// bar's is [`SEARCH_WIDTH`] wide, 12 pt with a 13 pt icon and its key hint at the right.
pub const SEARCH_HEIGHT: f32 = 26.0;
pub const SEARCH_PADDING: f32 = 8.0;
pub const SEARCH_ICON_SPACING: f32 = 6.0;
pub const SEARCH_WIDTH: f32 = 250.0;
pub const SEARCH_ICON_SIZE: f32 = 13.0;
pub const COMPACT_SEARCH_ICON_SIZE: f32 = 12.0;

/// The filter bar (`.fbar`) and the filmstrip (`.fstrip`): a step under the panel, with a 1 pt rule
/// of white at 6% composited over it.
pub const SELECT_BAR: Color = Color::from_rgb8(0x1d, 0x1d, 0x20);
pub const SELECT_BAR_RULE: Color = Color::from_rgb8(43, 43, 45);
/// The filter bar: 40 pt with its rule, 12 pt in from either end, its pieces 6 pt apart and its
/// trailing pieces 10 pt apart.
pub const FILTER_BAR_HEIGHT: f32 = 40.0;
pub const FILTER_BAR_PADDING: f32 = 12.0;
pub const FILTER_BAR_SPACING: f32 = 6.0;
pub const FILTER_BAR_TRAILING_SPACING: f32 = 10.0;
/// A filter segment (`.fseg button`): 22 pt, 9 pt either side, rounded 4 pt on a track rounded
/// 6 pt; its count 7 pt after its label (the button's 4 pt gap and the count's own 3 pt).
pub const FILTER_SEGMENT_HEIGHT: f32 = 22.0;
pub const FILTER_SEGMENT_PADDING: f32 = 9.0;
pub const FILTER_SEGMENT_RADIUS: f32 = 4.0;
pub const FILTER_TRACK_RADIUS: f32 = 6.0;
pub const FILTER_COUNT_SPACING: f32 = 7.0;
/// A filter chip (`.fchip`): 24 pt, 8 pt either side, rounded [`RADIUS`], its icon, label and
/// chevron or clear 5 pt apart, in [`CHIP_LABEL`] on this fill.
pub const FILTER_CHIP_HEIGHT: f32 = 24.0;
pub const FILTER_CHIP_PADDING: f32 = 8.0;
pub const FILTER_CHIP_SPACING: f32 = 5.0;
pub const FILTER_CHIP: Color = Color::from_rgb8(0x26, 0x26, 0x2a);
/// A set condition (`.fchip.on`): the accent at 16% over [`SELECT_BAR`], in accent ink; its clear
/// glyph is the accent at 80% over that.
pub const FILTER_CHIP_SET: Color = Color::from_rgb8(61, 53, 44);
pub const FILTER_CHIP_CLEAR: Color = Color::from_rgb8(193, 155, 94);
pub const FILTER_CHIP_ICON_SIZE: f32 = 11.0;
pub const FILTER_CHIP_GLYPH_SIZE: f32 = 9.0;
/// The filter bar's action (Save as smart collection…): 24 pt on [`CONTROL`], a 12 pt icon 5 pt
/// before its label.
pub const FILTER_ACTION_HEIGHT: f32 = 24.0;

/// A workspace switch segment's key hint (`G`, `D`): 4 pt after its label.
pub const SWITCH_HINT_SPACING: f32 = 4.0;
/// Develop N (`.tb.pri`): 28 pt, 12 pt either side, its count and chevron 6 pt apart. Busy, its
/// label and bar are 8 pt apart on [`CONTROL`], at least [`DEVELOP_BUSY_MIN_WIDTH`] wide, the bar
/// [`DEVELOP_BUSY_BAR_WIDTH`] long. At zero picks its label is in this grey.
pub const DEVELOP_HEIGHT: f32 = 28.0;
pub const DEVELOP_PADDING: f32 = 12.0;
pub const DEVELOP_SPACING: f32 = 6.0;
pub const DEVELOP_BUSY_SPACING: f32 = 8.0;
pub const DEVELOP_BUSY_MIN_WIDTH: f32 = 150.0;
pub const DEVELOP_BUSY_BAR_WIDTH: f32 = 48.0;
pub const DEVELOP_CHEVRON_SIZE: f32 = 11.0;
pub const DEVELOP_DISABLED_INK: Color = Color::from_rgb8(0x5a, 0x5a, 0x62);

/// Long-running work's bar (`.bar`): 4 pt on [`RAIL`], rounded 2 pt, filled with the accent only as
/// far as the work truthfully reports.
pub const WORK_BAR_HEIGHT: f32 = 4.0;
/// The status bar's busiest job: 26 pt, its dot, label, bar and caption 8 pt apart, the bar
/// [`STATUS_JOB_BAR_WIDTH`] long.
pub const STATUS_JOB_HEIGHT: f32 = 26.0;
pub const STATUS_JOB_SPACING: f32 = 8.0;
pub const STATUS_JOB_BAR_WIDTH: f32 = 70.0;
/// A Performance row for work that can be stopped (`jobrow`): 6 pt above and below and 8 pt at the
/// sides, 4 pt between its two lines, the bar line [`WORK_ROW_INDENT`] in, its parts 8 pt apart.
pub const WORK_ROW_PADDING: Padding = Padding {
    top: 6.0,
    right: 8.0,
    bottom: 6.0,
    left: 8.0,
};
pub const WORK_ROW_LINE_SPACING: f32 = 4.0;
pub const WORK_ROW_SPACING: f32 = 8.0;
pub const WORK_ROW_INDENT: f32 = 14.0;
/// The in-view progress sheet (`sheet`): 330 pt on [`BAR`], outlined in white at 10% over it,
/// rounded 12 pt over a soft shadow; its body inset 14 pt above, 16 pt at the sides and 12 pt
/// under, its lines 8 pt apart, its bar 5 pt; its footer on [`PANEL`] under a rule of white at 7%
/// over the panel, inset 10 pt and 12 pt, its buttons 6 pt apart.
pub const SHEET_WIDTH: f32 = 330.0;
pub const SHEET_RADIUS: f32 = 12.0;
pub const SHEET_BORDER: Color = STRIP_RULE;
pub const SHEET_SHADOW: Shadow = Shadow {
    color: Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.45,
    },
    offset: iced::Vector { x: 0.0, y: 12.0 },
    blur_radius: 30.0,
};
pub const SHEET_PADDING: Padding = Padding {
    top: 14.0,
    right: 16.0,
    bottom: 12.0,
    left: 16.0,
};
pub const SHEET_SPACING: f32 = 8.0;
pub const SHEET_BAR_HEIGHT: f32 = 5.0;
pub const SHEET_ICON_SIZE: f32 = 14.0;
pub const SHEET_FOOTER_RULE: Color = Color::from_rgb8(48, 48, 50);
pub const SHEET_FOOTER_PADDING: Padding = Padding {
    top: 10.0,
    right: 12.0,
    bottom: 10.0,
    left: 12.0,
};

/// A moment's frame under the loupe (`.mstrip .fr`): 124 × 84 pt on [`BAR`], rounded 5 pt, the
/// image fitted into 116 × 76; the active one on [`FRAME_ACTIVE`] with a 2 pt inset accent outline;
/// its number 6 pt in and 4 pt up, 10 pt semibold; the frames 6 pt apart.
pub const MOMENT_FRAME_WIDTH: f32 = 124.0;
pub const MOMENT_FRAME_HEIGHT: f32 = 84.0;
pub const MOMENT_FRAME_RADIUS: f32 = 5.0;
pub const MOMENT_IMAGE_WIDTH: f32 = 116.0;
pub const MOMENT_IMAGE_HEIGHT: f32 = 76.0;
pub const MOMENT_ACTIVE_OUTLINE: f32 = 2.0;
pub const MOMENT_STRIP_SPACING: f32 = 6.0;
pub const FRAME_ACTIVE: Color = Color::from_rgb8(0x2a, 0x2a, 0x2f);
pub const SIZE_FRAME_NUMBER: f32 = 10.0;
pub const FRAME_NUMBER_INSET: Padding = Padding {
    top: 0.0,
    right: 0.0,
    bottom: 4.0,
    left: 6.0,
};
/// A frame's pick check is the grid cell's (`CELL_PICK_*`), 5 pt in from the frame's corner.
pub const PICK_CHECK_INSET: f32 = 5.0;
/// The loupe's info bar: the draft bar's 34 pt floating surface, 12 pt in from either end.
pub const LOUPE_INFO_PADDING: f32 = 12.0;
/// The 100% focus check's inset: 308 pt wide on [`BAR`], rounded 8 pt, outlined in white at 12%
/// over it, over a big shadow; the region 209 pt tall over a 24 pt footer inset 8 pt, its parts
/// 8 pt apart.
pub const FOCUS_INSET_WIDTH: f32 = 308.0;
pub const FOCUS_REGION_HEIGHT: f32 = 209.0;
pub const FOCUS_FOOTER_HEIGHT: f32 = 24.0;
pub const FOCUS_INSET_RADIUS: f32 = 8.0;
pub const FOCUS_INSET_BORDER: Color = Color::from_rgb8(61, 61, 64);
pub const FOCUS_INSET_SHADOW: Shadow = Shadow {
    color: Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.5,
    },
    offset: iced::Vector { x: 0.0, y: 10.0 },
    blur_radius: 30.0,
};
/// The pointer's region box over the loupe: a 1.5 pt outline of white at 90%, ringed outside by
/// 1 pt of black at 50% so it reads on a bright sky.
pub const REGION_BOX_WIDTH: f32 = 1.5;
pub const REGION_BOX: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.9,
};
pub const REGION_BOX_RING: Color = Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.5,
};
/// The loupe's key hints (`.hint`): 14 pt apart; each key cap 10.5 pt semibold in [`TEXT_LABEL`] on
/// [`CONTROL`], 1 pt above and below and 5 pt at the sides, rounded 4 pt, 4 pt before its action.
pub const KEY_HINT_SPACING: f32 = 14.0;
pub const KEY_CAP_PADDING: Padding = Padding {
    top: 1.0,
    right: 5.0,
    bottom: 1.0,
    left: 5.0,
};
pub const KEY_CAP_RADIUS: f32 = 4.0;
pub const KEY_CAP_SPACING: f32 = 4.0;
/// Develop's filmstrip (`.fstrip`): 92 pt with its rule on [`SELECT_BAR`]; a 24 pt header inset
/// 10 pt, its parts 8 pt apart; cells 78 × 58 pt on [`BAR`], rounded 4 pt, 4 pt apart, the image
/// fitted into 72 × 52, the active cell on [`FRAME_ACTIVE`] with a 1.5 pt inset accent outline; the
/// cells inset 10 pt at the sides and 6 pt under.
pub const FILMSTRIP_HEIGHT: f32 = 92.0;
pub const FILMSTRIP_HEADER_HEIGHT: f32 = 24.0;
pub const FILMSTRIP_PADDING: f32 = 10.0;
pub const FILMSTRIP_HEADER_SPACING: f32 = 8.0;
pub const FILMSTRIP_CELL_WIDTH: f32 = 78.0;
pub const FILMSTRIP_CELL_HEIGHT: f32 = 58.0;
pub const FILMSTRIP_CELL_RADIUS: f32 = 4.0;
pub const FILMSTRIP_IMAGE_WIDTH: f32 = 72.0;
pub const FILMSTRIP_IMAGE_HEIGHT: f32 = 52.0;
pub const FILMSTRIP_CELL_SPACING: f32 = 4.0;
pub const FILMSTRIP_ACTIVE_OUTLINE: f32 = 1.5;
pub const FILMSTRIP_BOTTOM: f32 = 6.0;

/// A source row: [`SOURCE_ROW_SELECTED`] in every state while selected, else [`ROW_HOVER`] under the
/// pointer, else no surface. The row names its own ink, so the style's text colour is unused.
pub fn source_row(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let pointer = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let background = if selected {
            Some(SOURCE_ROW_SELECTED)
        } else if pointer {
            Some(ROW_HOVER)
        } else {
            None
        };
        button::Style {
            background: background.map(Background::Color),
            text_color: TEXT_LABEL,
            border: Border {
                radius: LIST_ROW_RADIUS.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// A neutral tag or a key cap: [`CONTROL`], rounded `radius`.
pub fn tag_surface(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_theme| {
        surface(CONTROL).border(Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: radius.into(),
        })
    }
}

/// The filter bar's and the filmstrip's surface.
pub fn select_bar_surface(_theme: &Theme) -> container::Style {
    surface(SELECT_BAR)
}

/// The rule under the filter bar and over the filmstrip.
pub fn select_bar_rule(_theme: &Theme) -> container::Style {
    surface(SELECT_BAR_RULE)
}

/// A filter segments' track: [`SEGMENT_TRACK`] rounded [`FILTER_TRACK_RADIUS`].
pub fn filter_track(_theme: &Theme) -> container::Style {
    surface(SEGMENT_TRACK).border(Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: FILTER_TRACK_RADIUS.into(),
    })
}

/// A filter segment: [`segment`]'s fills and inks, rounded [`FILTER_SEGMENT_RADIUS`].
pub fn filter_segment(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let mut style = segment(selected)(theme, status);
        style.border.radius = FILTER_SEGMENT_RADIUS.into();
        style
    }
}

/// A filter chip: [`FILTER_CHIP_SET`] in accent ink while its condition is set, else
/// [`FILTER_CHIP`] in [`CHIP_LABEL`], lifting to [`CONTROL`] under the pointer.
pub fn filter_chip(set: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let pointer = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let background = match (set, pointer) {
            (true, _) => FILTER_CHIP_SET,
            (false, true) => CONTROL,
            (false, false) => FILTER_CHIP,
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: if set { ACCENT } else { CHIP_LABEL },
            border: Border {
                radius: RADIUS.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// Develop N while it is busy: [`CONTROL`] behind primary ink in every state, since it reports work
/// rather than offering it.
pub fn develop_busy(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(CONTROL)),
        text_color: TEXT_PRIMARY,
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// Develop N with nothing picked (`.tb.dis`): [`CONTROL`] behind [`DEVELOP_DISABLED_INK`].
pub fn develop_disabled(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        text_color: DEVELOP_DISABLED_INK,
        ..develop_busy(theme, status)
    }
}

/// The status bar's busiest job: no surface at rest, [`ROW_HOVER`] under the pointer.
pub fn status_job(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(Background::Color(ROW_HOVER)),
        button::Status::Active | button::Status::Disabled => None,
    };
    button::Style {
        background,
        text_color: TEXT_SECONDARY,
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A text action inside a row, such as a job's Cancel: no surface, [`TEXT_IDENTITY`] ink lifting to
/// primary under the pointer. Its label names no colour, so it takes this ink.
pub fn quiet_action(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: match status {
            button::Status::Hovered | button::Status::Pressed => TEXT_PRIMARY,
            button::Status::Active => TEXT_IDENTITY,
            button::Status::Disabled => TEXT_TERTIARY,
        },
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// The in-view progress sheet's surface.
pub fn sheet_surface(_theme: &Theme) -> container::Style {
    surface(BAR)
        .border(Border {
            color: SHEET_BORDER,
            width: BORDER_WIDTH,
            radius: SHEET_RADIUS.into(),
        })
        .shadow(SHEET_SHADOW)
}

/// A framed image cell — a moment's frame or a filmstrip cell — rounded `radius`: on [`BAR`], or on
/// [`FRAME_ACTIVE`] with an inset accent outline `outline` wide while active; [`FRAME_ACTIVE`] under
/// the pointer.
pub fn image_cell(
    active: bool,
    radius: f32,
    outline: f32,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let pointer = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if active || pointer {
                FRAME_ACTIVE
            } else {
                BAR
            })),
            text_color: TEXT_PRIMARY,
            border: Border {
                radius: radius.into(),
                width: if active { outline } else { 0.0 },
                color: if active { ACCENT } else { Color::TRANSPARENT },
            },
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// The 100% inset's surface under its region and footer.
pub fn focus_inset_surface(_theme: &Theme) -> container::Style {
    surface(BAR)
        .border(Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: FOCUS_INSET_RADIUS.into(),
        })
        .shadow(FOCUS_INSET_SHADOW)
}

/// The 100% inset's outline, drawn over its region so the photograph never covers it.
pub fn focus_inset_outline(_theme: &Theme) -> container::Style {
    container::Style::default().border(Border {
        color: FOCUS_INSET_BORDER,
        width: BORDER_WIDTH,
        radius: FOCUS_INSET_RADIUS.into(),
    })
}

// -- end Select chrome ---------------------------------------------------------------------------

// -- Select: resolving missing originals ----------------------------------------------------------

// The Missing originals view, from the resolve board's CSS (`.rg`, `.gh`, `.rr`, and the floating
// bar under them). Each translucent fill over a group's surface is stored opaque, composited over
// [`RESOLVE_GROUP`], for the reason [`RULE`] gives; the tests below recompute each.

/// A group (`.rg`): this surface, rounded 9 pt, its 1 pt inset outline white at 7%, 10 pt apart.
pub const RESOLVE_GROUP: Color = Color::from_rgb8(0x1f, 0x1f, 0x22);
pub const RESOLVE_GROUP_OUTLINE: Color = Color::from_rgb8(47, 47, 49);
pub const RESOLVE_GROUP_RADIUS: f32 = 9.0;
pub const RESOLVE_GROUP_SPACING: f32 = 10.0;
/// A group's header (`.gh`): at least 44 pt, inset 6 pt, 10 pt at the right and 12 pt at the left,
/// its parts 10 pt apart and its two lines 2 pt apart, over a rule of white at 5%.
pub const RESOLVE_HEADER_HEIGHT: f32 = 44.0;
pub const RESOLVE_HEADER_PADDING: Padding = Padding {
    top: 6.0,
    right: 10.0,
    bottom: 6.0,
    left: 12.0,
};
pub const RESOLVE_HEADER_SPACING: f32 = 10.0;
pub const RESOLVE_HEADER_LINE_SPACING: f32 = 2.0;
pub const RESOLVE_HEADER_RULE: Color = Color::from_rgb8(42, 42, 45);
/// A header's folder glyph, and a row's catalog folder glyph and result glyph.
pub const RESOLVE_FOLDER_ICON_SIZE: f32 = 14.0;
pub const RESOLVE_ROW_ICON_SIZE: f32 = 11.0;
/// A photograph's row (`.rr`): 44 pt, inset 10 pt at the right and 12 pt at the left, its columns
/// 12 pt apart — the 48 × 32 pt preview in a 52 pt column, the file name and the catalog folder in
/// 150 pt each, the result, and its action in 96 pt — over a rule of white at 4%, and white at 5%
/// behind it while selected.
pub const RESOLVE_ROW_HEIGHT: f32 = 44.0;
pub const RESOLVE_ROW_PADDING: Padding = Padding {
    top: 0.0,
    right: 10.0,
    bottom: 0.0,
    left: 12.0,
};
pub const RESOLVE_ROW_SPACING: f32 = 12.0;
pub const RESOLVE_PREVIEW_COLUMN: f32 = 52.0;
pub const RESOLVE_PREVIEW_WIDTH: f32 = 48.0;
pub const RESOLVE_PREVIEW_HEIGHT: f32 = 32.0;
pub const RESOLVE_NAME_COLUMN: f32 = 150.0;
pub const RESOLVE_ACTION_COLUMN: f32 = 96.0;
pub const RESOLVE_ROW_RULE: Color = Color::from_rgb8(40, 40, 43);
pub const RESOLVE_ROW_SELECTED: Color = Color::from_rgb8(42, 42, 45);
/// A result's glyph 7 pt before its text, and its detail 5 pt after it.
pub const RESOLVE_RESULT_SPACING: f32 = 7.0;
pub const RESOLVE_DETAIL_SPACING: f32 = 5.0;
/// The results' inks: verified in the connected green, a refusal in the clipping red, the rest in
/// the identity grey.
pub const RESOLVE_FOUND: Color = AGENT_CONNECTED;
pub const RESOLVE_REFUSED: Color = CLIPPING_HIGHLIGHT;
/// The view's heading (`252 photographs whose originals…`): inset 14 pt above, 10 pt under and 2 pt
/// at the sides, its note 10 pt after it; the list inset 16 pt at the sides.
pub const RESOLVE_HEADING_PADDING: Padding = Padding {
    top: 14.0,
    right: 2.0,
    bottom: 10.0,
    left: 2.0,
};
pub const RESOLVE_LIST_PADDING: f32 = 16.0;
/// The floating bar (`Relink N`): rounded 10 pt, inset 6 pt with 14 pt at the left, its parts
/// 12 pt apart, 12 pt above the centre's foot.
pub const RESOLVE_BAR_RADIUS: f32 = 10.0;
pub const RESOLVE_BAR_PADDING: Padding = Padding {
    top: 6.0,
    right: 6.0,
    bottom: 6.0,
    left: 14.0,
};
pub const RESOLVE_BAR_SPACING: f32 = 12.0;
pub const RESOLVE_BAR_BOTTOM: f32 = 12.0;

/// A group's surface and outline.
pub fn resolve_group(_theme: &Theme) -> container::Style {
    surface(RESOLVE_GROUP).border(Border {
        color: RESOLVE_GROUP_OUTLINE,
        width: BORDER_WIDTH,
        radius: RESOLVE_GROUP_RADIUS.into(),
    })
}

/// A photograph's row: [`RESOLVE_ROW_SELECTED`] while selected or under the pointer, else no
/// surface. The row names its own ink, so the style's text colour is unused.
pub fn resolve_row(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let pointer = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: (selected || pointer).then_some(Background::Color(RESOLVE_ROW_SELECTED)),
            text_color: TEXT_LABEL,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// A rule inside a group: under its header, or between its rows.
pub fn resolve_rule(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_theme| surface(color)
}

// -- end Select: resolving missing originals ------------------------------------------------------

/// Builds the dark, custom Luxforge theme from the tokens above. There is no light theme yet;
/// see the [visual language](../../../docs/design/develop-workspace.md#visual-language) decision.
pub fn theme() -> Theme {
    Theme::custom(
        "Luxforge".to_string(),
        iced::theme::Palette {
            background: PANEL,
            text: TEXT_PRIMARY,
            primary: ACCENT,
            success: ACCENT,
            warning: ACCENT,
            danger: CLIPPING_HIGHLIGHT,
        },
    )
}

fn surface(background: Color) -> container::Style {
    container::Style::default().background(background)
}

fn bordered_surface(background: Color) -> container::Style {
    surface(background).border(Border {
        color: BORDER,
        width: BORDER_WIDTH,
        radius: RADIUS.into(),
    })
}

/// The canvas surface: flat, no border (the photograph's own edge reads as the boundary).
pub fn canvas_surface(_theme: &Theme) -> container::Style {
    surface(CANVAS)
}

/// A side panel or the status bar: flat, no border.
pub fn panel_surface(_theme: &Theme) -> container::Style {
    surface(PANEL)
}

/// The title bar: the Bar surface, flat, its rule drawn under it by the shell.
pub fn title_bar_surface(_theme: &Theme) -> container::Style {
    surface(BAR)
}

/// A rule between the shell's regions: [`DIVIDER`], opaque.
pub fn divider_surface(_theme: &Theme) -> container::Style {
    surface(DIVIDER)
}

/// A floating bar or notice: bordered, rounded.
pub fn bar_surface(_theme: &Theme) -> container::Style {
    bordered_surface(BAR)
}

/// A control surface (chip, menu, card body): bordered, rounded.
pub fn control_surface(_theme: &Theme) -> container::Style {
    bordered_surface(CONTROL)
}

/// A piece of floating canvas chrome — the mode strip, the draft bar or a notice — on the Bar
/// surface with `border`, `radius` and the soft [`CHROME_SHADOW`].
pub fn chrome_surface(border: Color, radius: f32) -> container::Style {
    surface(BAR)
        .border(Border {
            color: border,
            width: BORDER_WIDTH,
            radius: radius.into(),
        })
        .shadow(CHROME_SHADOW)
}

/// One tool in the mode strip: transparent at rest, [`CONTROL`] under the pointer and
/// [`STRIP_SELECTED`] when it is the active mode or an overlay that is on.
pub fn strip_tool(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = match (selected, status) {
            (true, _) => Some(Background::Color(STRIP_SELECTED)),
            (false, button::Status::Hovered | button::Status::Pressed) => {
                Some(Background::Color(CONTROL))
            }
            (false, button::Status::Active | button::Status::Disabled) => None,
        };
        button::Style {
            background,
            text_color: match (selected, status) {
                (_, button::Status::Disabled) => TEXT_TERTIARY,
                (true, _) => ACCENT,
                (false, _) => STRIP_ICON,
            },
            border: Border {
                radius: STRIP_TOOL_RADIUS.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// The histogram plot: the Canvas surface with its faint outline and rounded corners.
pub fn histogram_surface(_theme: &Theme) -> container::Style {
    surface(CANVAS).border(Border {
        color: HISTOGRAM_BORDER,
        width: BORDER_WIDTH,
        radius: HISTOGRAM_RADIUS.into(),
    })
}

/// A plain, background-free button: list rows, chips and inline menu items.
pub fn button_plain(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(Background::Color(CONTROL)),
        button::Status::Active | button::Status::Disabled => None,
    };

    button::Style {
        background,
        text_color: text_color_for(status),
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// The one accent-filled button per surface: Apply, a selected mode or a primary action.
pub fn button_accent(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Disabled => CONTROL,
        _ => ACCENT,
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color: match status {
            button::Status::Disabled => TEXT_TERTIARY,
            _ => PRIMARY_INK,
        },
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A square icon-glyph button: mode strip entries, header actions, context toggles. No surface at
/// rest, [`ICON_HOVER`] under the pointer.
pub fn button_icon(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(Background::Color(ICON_HOVER)),
        button::Status::Active | button::Status::Disabled => None,
    };
    button::Style {
        background,
        text_color: text_color_for(status),
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A selected icon button: [`ICON_SELECTED_FILL`] behind accent ink, with no outline, as the
/// default board draws the open panels' toggles.
pub fn button_selected(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(ICON_SELECTED_FILL)),
        text_color: if matches!(status, button::Status::Disabled) {
            TEXT_TERTIARY
        } else {
            ACCENT
        },
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// An open colour swatch. The swatch fills its button, so a fill behind it would not show: the
/// open state is the accent outline around it.
pub fn swatch_open(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(Color { a: 0.18, ..ACCENT })),
        text_color: if matches!(status, button::Status::Disabled) {
            TEXT_TERTIARY
        } else {
            ACCENT
        },
        border: Border {
            radius: RADIUS.into(),
            width: BORDER_WIDTH,
            color: ACCENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A segmented control's track: [`SEGMENT_TRACK`], its segments inset by [`SEGMENT_INSET`].
pub fn segment_track(_theme: &Theme) -> container::Style {
    surface(SEGMENT_TRACK).border(Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: SEGMENT_RADIUS.into(),
    })
}

/// One segment. The selected one is raised on [`SEGMENT_SELECTED`] in [`TEXT_BRIGHT`] and never
/// takes the accent, because it states a view rather than an edit; the others are bare, in
/// secondary text.
pub fn segment(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = match (selected, status) {
            (true, _) => Some(Background::Color(SEGMENT_SELECTED)),
            (false, button::Status::Hovered | button::Status::Pressed) => {
                Some(Background::Color(CONTROL))
            }
            (false, _) => None,
        };
        button::Style {
            background,
            text_color: match (selected, status) {
                (_, button::Status::Disabled) => TEXT_TERTIARY,
                (true, _) => TEXT_BRIGHT,
                (false, _) => TEXT_SECONDARY,
            },
            border: Border {
                radius: (SEGMENT_RADIUS - SEGMENT_INSET).into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

fn text_color_for(status: button::Status) -> Color {
    match status {
        button::Status::Disabled => TEXT_TERTIARY,
        _ => TEXT_PRIMARY,
    }
}

/// The value field's text input: transparent until focused or invalid.
pub fn text_input_style(invalid: bool) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |_theme, status| {
        let border_color = if invalid {
            CLIPPING_HIGHLIGHT
        } else {
            match status {
                text_input::Status::Focused { .. } => ACCENT,
                _ => BORDER,
            }
        };

        text_input::Style {
            background: Background::Color(CONTROL),
            border: Border {
                color: border_color,
                width: BORDER_WIDTH,
                radius: RADIUS.into(),
            },
            icon: TEXT_TERTIARY,
            placeholder: TEXT_TERTIARY,
            value: TEXT_PRIMARY,
            selection: Color { a: 0.35, ..ACCENT },
        }
    }
}

/// A field box's text input: the Control surface with no outline at rest, as the module
/// references draw a value box, the accent outline while focused and the clipping red while
/// invalid.
pub fn field_input_style(
    invalid: bool,
) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |theme, status| {
        let mut style = text_input_style(invalid)(theme, status);
        if !invalid && !matches!(status, text_input::Status::Focused { .. }) {
            // No outline at all: a transparent one would still inset the surface by its width.
            style.border.color = Color::TRANSPARENT;
            style.border.width = 0.0;
        }
        style
    }
}

/// The tools panel's thin overlay scrollbar.
pub fn panel_scrollbar() -> iced::widget::scrollable::Direction {
    iced::widget::scrollable::Direction::Vertical(
        iced::widget::scrollable::Scrollbar::new()
            .width(PANEL_SCROLLBAR_WIDTH)
            .scroller_width(PANEL_SCROLLBAR_WIDTH)
            .margin(PANEL_SCROLLBAR_MARGIN),
    )
}

/// A button with no surface in any state, for a disclosure that is read as text.
pub fn button_bare(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: text_color_for(status),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A disclosure heading: no surface in any state, its label tertiary at rest and secondary under
/// the pointer or while pressed. The label takes this text colour, so the style is the one place
/// that decides how the heading answers a hover.
pub fn button_disclosure(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: disclosure_color(matches!(
            status,
            button::Status::Hovered | button::Status::Pressed
        )),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A disclosure heading's ink, label and chevron alike: secondary while hovered, else tertiary.
pub fn disclosure_color(hovered: bool) -> Color {
    if hovered {
        TEXT_SECONDARY
    } else {
        TEXT_TERTIARY
    }
}

/// A module band's surface: the Bar colour, flat, in every state. The band is a disclosure, so
/// it keeps one colour rather than flashing a hover fill across the panel.
pub fn button_band(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(BAR)),
        text_color: text_color_for(status),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A labelled button in a section (a picker or an action): the Control surface, borderless.
pub fn button_control(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Color {
            a: 0.18,
            ..TEXT_PRIMARY
        },
        button::Status::Active | button::Status::Disabled => CONTROL,
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: text_color_for(status),
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// The current entry's list row: tinted, in every state.
pub fn list_row_current(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(LIST_ROW_CURRENT)),
        text_color: TEXT_CURRENT_ROW,
        border: Border {
            radius: LIST_ROW_RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
    .with_disabled(status)
}

/// A mask or component row: `selected` fills it in every state, else [`ROW_HOVER`] shows under the
/// pointer or while `hovered` says the row is the one being shown, else it has no surface.
pub fn mask_row(
    selected: Option<Color>,
    hovered: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let pointer = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let background = match selected {
            Some(fill) => Some(fill),
            None if hovered || pointer => Some(ROW_HOVER),
            None => None,
        };
        button::Style {
            background: background.map(Background::Color),
            text_color: if selected.is_some() {
                TEXT_CURRENT_ROW
            } else {
                TEXT_LABEL
            },
            border: Border {
                radius: LIST_ROW_RADIUS.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            shadow: Shadow::default(),
            snap: false,
        }
        .with_disabled(status)
    }
}

/// The mode control's track: [`SEGMENT_TRACK`] rounded [`MODE_RADIUS`], at
/// [`MODE_FIXED_OPACITY`] when the mode is fixed.
pub fn mode_track(fixed: bool) -> impl Fn(&Theme) -> container::Style {
    move |_theme| {
        let alpha = if fixed { MODE_FIXED_OPACITY } else { 1.0 };
        surface(Color {
            a: alpha,
            ..SEGMENT_TRACK
        })
        .border(Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: MODE_RADIUS.into(),
        })
    }
}

/// One mode segment: raised on [`SEGMENT_SELECTED`] when chosen (at [`MODE_FIXED_OPACITY`] when
/// fixed), [`CONTROL`] under the pointer, else bare. Its glyph carries the mode's colour.
pub fn mode_segment(
    selected: bool,
    fixed: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = match (selected, status) {
            (true, _) => Some(Color {
                a: if fixed { MODE_FIXED_OPACITY } else { 1.0 },
                ..SEGMENT_SELECTED
            }),
            (false, button::Status::Hovered | button::Status::Pressed) => Some(CONTROL),
            (false, _) => None,
        };
        button::Style {
            background: background.map(Background::Color),
            text_color: TEXT_PRIMARY,
            border: Border {
                radius: MODE_SEGMENT_RADIUS.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// A dropdown menu's surface: [`MENU_SURFACE`], outlined in [`MENU_BORDER`], rounded
/// [`MENU_RADIUS`], over [`MENU_SHADOW`].
pub fn menu_surface(_theme: &Theme) -> container::Style {
    surface(MENU_SURFACE)
        .border(Border {
            color: MENU_BORDER,
            width: BORDER_WIDTH,
            radius: MENU_RADIUS.into(),
        })
        .shadow(MENU_SHADOW)
}

/// A menu item: bare at rest, [`MENU_ITEM_HOVER`] under the pointer.
pub fn menu_item(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Some(Background::Color(MENU_ITEM_HOVER))
        }
        button::Status::Active | button::Status::Disabled => None,
    };
    button::Style {
        background,
        text_color: text_color_for(status),
        border: Border {
            radius: MENU_ITEM_RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A menu's separator line.
pub fn menu_separator(_theme: &Theme) -> container::Style {
    surface(MENU_SEPARATOR)
}

/// A selected chip: the accent-tinted fill, borderless.
pub fn chip_selected(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(SELECTED_FILL)),
        text_color: ACCENT,
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
    .with_disabled(status)
}

/// A number field's value box: the Control surface, borderless, a press opens it for typing.
pub fn button_field(theme: &Theme, status: button::Status) -> button::Style {
    button_control(theme, status)
}

/// A readout card: the Canvas surface, rounded, borderless.
pub fn readout_surface(_theme: &Theme) -> container::Style {
    surface(CANVAS).border(Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: RADIUS.into(),
    })
}

trait DisabledStyle {
    fn with_disabled(self, status: button::Status) -> Self;
}

impl DisabledStyle for button::Style {
    fn with_disabled(self, status: button::Status) -> Self {
        match status {
            button::Status::Disabled => button::Style {
                text_color: TEXT_TERTIARY,
                ..self
            },
            _ => self,
        }
    }
}

/// A group header's hairline rule.
pub fn rule_surface(_theme: &Theme) -> container::Style {
    surface(RULE)
}

/// The 1 px border above a module band.
pub fn band_border_surface(_theme: &Theme) -> container::Style {
    surface(BAND_BORDER)
}

/// The slider's handle. The rail, its fill and its zero tick are drawn under it by the slider row
/// itself (see [`crate::geometry::rail_geometry`]), so Iced's own rail is transparent. The handle
/// turns [`ACCENT`] only while dragging, and then drops its dark ring, so the halo the rail line
/// draws under it (see [`THUMB_HALO_RADIUS`]) meets the accent directly, as the references draw it.
pub fn slider_style(dragging: bool) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |_theme, status| {
        let active = dragging || matches!(status, slider::Status::Dragged);

        slider::Style {
            rail: slider::Rail {
                backgrounds: (
                    Background::Color(Color::TRANSPARENT),
                    Background::Color(Color::TRANSPARENT),
                ),
                width: RAIL_WIDTH,
                border: Border::default(),
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle {
                    radius: THUMB_RADIUS,
                },
                background: Background::Color(if active { ACCENT } else { THUMB }),
                border_color: if active {
                    Color::TRANSPARENT
                } else {
                    THUMB_OUTLINE
                },
                border_width: THUMB_OUTLINE_WIDTH,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Select: the thumbnail grid's token tests.

    /// The Select cell is the board's `.cc`: the image box is `.im`'s 98 pt less its 8/8/2 pt
    /// padding, the footer follows it and fills the cell.
    #[test]
    fn select_cell_sizes_are_the_boards_css() {
        assert_eq!((CELL_SIZE.width, CELL_SIZE.height), (136.0, 122.0));
        assert_eq!(CELL_IMAGE.x, 8.0);
        assert_eq!(CELL_IMAGE.width, CELL_SIZE.width - 8.0 - 8.0);
        assert_eq!(CELL_IMAGE.y + CELL_IMAGE.height + 2.0, 98.0);
        assert_eq!(CELL_FOOTER_TOP, 98.0);
        assert_eq!(CELL_FOOTER_TOP + CELL_FOOTER_HEIGHT, CELL_SIZE.height);
        assert_eq!((CELL_IMAGE_MAX.width, CELL_IMAGE_MAX.height), (120.0, 86.0));
        assert_eq!(
            (CELL_FOOTER_INSET, SIZE_CELL_LABEL, CELL_FOOTER_SPACING),
            (9.0, 10.5, 6.0)
        );
        assert_eq!(CELL_BADGE_INSET, 11.0);
        assert_eq!(CELL_SURFACE, Color::from_rgb8(0x1d, 0x1d, 0x20));
        assert_eq!(CELL_SELECTED, Color::from_rgb8(0x34, 0x34, 0x3a));
        assert_eq!(CELL_ACTIVE_OUTLINE, 1.5);
        assert_eq!(CELL_IMAGE_SHADOW.offset, iced::Vector::new(0.0, 1.0));
        assert_eq!(
            (CELL_IMAGE_SHADOW.blur_radius, CELL_IMAGE_SHADOW.color.a),
            (3.0, 0.5)
        );
    }

    /// The catalog cell is the catalog board's `.cell`, which leaves 4 pt under its footer.
    #[test]
    fn catalog_cell_sizes_are_the_boards_css() {
        let size = CATALOG_CELL_SIZE;
        assert_eq!((size.width, size.height), (168.0, 176.0));
        assert_eq!(CATALOG_CELL_IMAGE.width, size.width - 10.0 - 10.0);
        assert_eq!(
            CATALOG_CELL_IMAGE.y + CATALOG_CELL_IMAGE.height + 4.0,
            142.0
        );
        assert_eq!(CATALOG_CELL_FOOTER_TOP, 142.0);
        assert_eq!(
            CATALOG_CELL_FOOTER_TOP + CATALOG_CELL_FOOTER_HEIGHT + 4.0,
            size.height
        );
        assert_eq!(
            (CATALOG_CELL_IMAGE_MAX.width, CATALOG_CELL_IMAGE_MAX.height),
            (148.0, 124.0)
        );
        assert_eq!(
            (
                CATALOG_CELL_FOOTER_INSET,
                SIZE_CATALOG_CELL_LABEL,
                CATALOG_CELL_BADGE_INSET
            ),
            (10.0, 11.0, 14.0)
        );
    }

    /// The badges, the pick, the moment row and the headings are the boards' `.tk`, `.cnt`, `.mom`,
    /// `.tag`, `.tb`, `.wrap`, `.dayh` and `.camh`.
    #[test]
    fn badge_moment_and_heading_sizes_are_the_boards_css() {
        assert_eq!(
            (CELL_PICK_SIZE, CELL_CHECK_SIZE, CELL_PICK_RING_WIDTH),
            (18.0, 10.0, 2.0)
        );
        assert_eq!(
            (
                CELL_BADGE_HEIGHT,
                CELL_BADGE_PADDING,
                CELL_BADGE_RADIUS,
                CELL_BADGE_ICON_SIZE,
                CELL_BADGE_SPACING,
                CELL_CATALOG_BADGE_SPACING
            ),
            (16.0, 5.0, 4.0, 10.0, 3.0, 4.0)
        );
        assert_eq!((SIZE_CELL_BADGE, SIZE_CELL_UNAVAILABLE), (10.5, 10.0));
        assert_eq!(
            (MOMENT_RADIUS, MOMENT_PADDING, MOMENT_HEADER_HEIGHT),
            (9.0, 6.0, 28.0)
        );
        assert_eq!(MOMENT_SURFACE, Color::from_rgb8(0x20, 0x20, 0x24));
        assert_eq!(
            (
                MOMENT_HEADER_INSET,
                MOMENT_HEADER_SPACING,
                MOMENT_ICON_SIZE,
                MOMENT_ACTION_MARGIN
            ),
            (4.0, 7.0, 12.0, 6.0)
        );
        assert_eq!(
            (
                MOMENT_TAG_PADDING,
                MOMENT_TAG_HEIGHT,
                MOMENT_TAG_RADIUS,
                SIZE_MOMENT_TAG
            ),
            (6.0, 14.0, 4.0, 10.0)
        );
        assert_eq!((MOMENT_ACTION_PADDING, SIZE_MOMENT_ACTION), (8.0, 11.0));
        assert_eq!(
            (
                THUMB_GRID_GAP,
                THUMB_GRID_SIDE_INSET,
                THUMB_GRID_TOP_INSET,
                THUMB_GRID_BOTTOM_INSET
            ),
            (6.0, 16.0, 10.0, 80.0)
        );
        assert_eq!(
            (DAY_HEADING_TOP, DAY_HEADING_LINE, DAY_HEADING_BOTTOM),
            (14.0, 16.0, 6.0)
        );
        assert_eq!(
            (
                CAMERA_HEADING_TOP,
                CAMERA_HEADING_LINE,
                CAMERA_HEADING_BOTTOM
            ),
            (4.0, 13.0, 8.0)
        );
        assert_eq!(
            (DAY_HEADING_SPACING, CAMERA_HEADING_SPACING, HEADING_INSET),
            (10.0, 8.0, 2.0)
        );
    }

    /// `colour` at `opacity` over `background` as Iced draws it: blended in linear light, then
    /// encoded, to the 8-bit code a capture samples. The transfer function is the shared
    /// reference's.
    fn linear_composite(colour: Color, background: Color, opacity: f32) -> [u8; 3] {
        use luxforge_reference::srgb;
        [
            (colour.r, background.r),
            (colour.g, background.g),
            (colour.b, background.b),
        ]
        .map(|(c, b)| {
            let (c, b) = (
                srgb::decode_encoded(f64::from(c)),
                srgb::decode_encoded(f64::from(b)),
            );
            srgb::code(b + (c - b) * f64::from(opacity))
        })
    }

    /// The moment row's outline is white at 7% over the row. The badges and the pick's ring keep
    /// their alpha, raised so that Iced's linear-light blend over a mid-grey photograph matches the
    /// board's sRGB blend; the offline photograph's opacity is corrected the same way over the
    /// cell.
    #[test]
    fn select_grid_tints_are_the_boards_composites() {
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1);
        assert!(near(
            code(MOMENT_OUTLINE),
            composite(Color::WHITE, MOMENT_SURFACE, 0.07)
        ));
        let grey = Color::from_rgb8(128, 128, 128);
        let ink = Color::from_rgb8(20, 20, 22);
        assert!(near(
            linear_composite(CELL_BADGE_SURFACE, grey, CELL_BADGE_SURFACE.a),
            composite(ink, grey, 0.82)
        ));
        assert!(near(
            linear_composite(CELL_PICK_RING, grey, CELL_PICK_RING.a),
            composite(ink, grey, 0.6)
        ));
        let [r, g, _] = linear_composite(grey, CELL_SURFACE, CELL_OFFLINE_OPACITY);
        let [br, bg, _] = composite(grey, CELL_SURFACE, 0.45);
        assert!(r.abs_diff(br) <= 1 && g.abs_diff(bg) <= 1, "{r} {br}");
        assert_eq!(
            [CELL_BADGE_SURFACE, CELL_PICK_RING].map(code),
            [[20, 20, 22]; 2]
        );
        assert_eq!(CELL_PLACEHOLDER, Color::from_rgb8(0x26, 0x26, 0x2a));
    }

    // -- end Select: the thumbnail grid's token tests.

    #[test]
    fn surface_tokens_match_the_visual_language_table() {
        assert_eq!(CANVAS, Color::from_rgb8(0x19, 0x19, 0x1b));
        assert_eq!(PANEL, Color::from_rgb8(0x20, 0x20, 0x23));
        assert_eq!(BAR, Color::from_rgb8(0x23, 0x23, 0x26));
        assert_eq!(CONTROL, Color::from_rgb8(0x2c, 0x2c, 0x31));
    }

    #[test]
    fn border_token_is_six_percent_white() {
        assert_eq!(BORDER.r, 1.0);
        assert_eq!(BORDER.g, 1.0);
        assert_eq!(BORDER.b, 1.0);
        assert!((BORDER.a - 0.06).abs() < f32::EPSILON);
    }

    #[test]
    fn text_tokens_match_the_visual_language_table() {
        assert_eq!(TEXT_PRIMARY, Color::from_rgb8(0xe8, 0xe8, 0xea));
        assert_eq!(TEXT_SECONDARY, Color::from_rgb8(0xa8, 0xa8, 0xae));
        assert_eq!(TEXT_TERTIARY, Color::from_rgb8(0x77, 0x77, 0x7f));
    }

    #[test]
    fn accent_and_clipping_tokens_match_the_visual_language_table() {
        assert_eq!(ACCENT, Color::from_rgb8(0xe2, 0xb4, 0x6a));
        assert_eq!(CLIPPING_HIGHLIGHT, Color::from_rgb8(0xe5, 0x53, 0x4b));
        assert_eq!(CLIPPING_SHADOW, Color::from_rgb8(0x4c, 0x8b, 0xe0));
    }

    #[test]
    fn the_guide_token_is_white_at_thirty_percent() {
        assert_eq!((GUIDE.r, GUIDE.g, GUIDE.b), (BORDER.r, BORDER.g, BORDER.b));
        assert!((GUIDE.a - 0.30).abs() < f32::EPSILON);
    }

    /// The both-endpoint overlay colour is composed from the two clipping tokens, never written as
    /// its own literal: a change to either token carries into it.
    #[test]
    fn the_both_endpoint_colour_is_the_two_clipping_tokens_combined() {
        assert_eq!(CLIPPING_BOTH.r, CLIPPING_HIGHLIGHT.r);
        assert_eq!(CLIPPING_BOTH.g, CLIPPING_HIGHLIGHT.g);
        assert_eq!(CLIPPING_BOTH.b, CLIPPING_SHADOW.b);
        assert_eq!(CLIPPING_BOTH.a, 1.0);
        // It is visibly neither of the two it is made from, which is the point of a third class.
        assert_ne!(CLIPPING_BOTH, CLIPPING_HIGHLIGHT);
        assert_ne!(CLIPPING_BOTH, CLIPPING_SHADOW);
    }

    #[test]
    fn mask_overlay_tokens_match_the_visual_language_table() {
        assert_eq!(MASK_OVERLAY_GREEN, Color::from_rgb8(0x3f, 0xd0, 0x7a));
        assert_eq!(MASK_OVERLAY_WHITE, Color::from_rgb8(0xf2, 0xf2, 0xf5));
    }

    /// A mask overlay must never be mistaken for a clipping indicator. The bar is measured, not
    /// asserted by eye: every overlay tint is at least 100 codes away, as a distance over the three
    /// 8-bit channels, from each of the three clipping colours.
    #[test]
    fn every_mask_overlay_tint_is_far_from_every_clipping_colour() {
        fn distance(a: Color, b: Color) -> f32 {
            let channel = |x: f32, y: f32| ((x - y) * 255.0).powi(2);
            (channel(a.r, b.r) + channel(a.g, b.g) + channel(a.b, b.b)).sqrt()
        }
        let mut closest = f32::INFINITY;
        for overlay in [MASK_OVERLAY_GREEN, MASK_OVERLAY_WHITE] {
            for clipping in [CLIPPING_HIGHLIGHT, CLIPPING_SHADOW, CLIPPING_BOTH] {
                let apart = distance(overlay, clipping);
                assert!(
                    apart >= 100.0,
                    "{overlay:?} is only {apart:.0} codes from {clipping:?}"
                );
                closest = closest.min(apart);
            }
        }
        // The two tints are also each other's alternatives, so they must differ as well.
        assert!(distance(MASK_OVERLAY_GREEN, MASK_OVERLAY_WHITE) >= 100.0);
        println!("closest mask overlay tint to a clipping colour: {closest:.0} codes");
    }

    #[test]
    fn channel_fills_are_distinct_and_translucent() {
        for (a, b) in [
            (CHANNEL_RED, CHANNEL_GREEN),
            (CHANNEL_GREEN, CHANNEL_BLUE),
            (CHANNEL_RED, CHANNEL_BLUE),
        ] {
            assert_ne!(a, b);
        }
        assert!((0.0..1.0).contains(&CHANNEL_ALPHA), "the fills overlap");
        assert_eq!(HISTOGRAM_HEIGHT, 88.0);
    }

    #[test]
    fn type_sizes_match_the_visual_language_table() {
        assert_eq!(SIZE_CONTROL, 12.0);
        assert_eq!(SIZE_TITLE, 13.0);
        assert_eq!(SIZE_CAPTION, 11.0);
        assert_eq!(SIZE_SECTION_LABEL, 10.5);
    }

    /// The `OS/2` weight class of a TrueType file and whether it carries an `fvar` table, read
    /// straight from the table directory.
    fn weight_class_and_variation(file: &[u8]) -> (u16, bool) {
        let be16 = |at: usize| u16::from_be_bytes([file[at], file[at + 1]]);
        let be32 = |at: usize| u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        assert_eq!(be32(0), 0x0001_0000, "a TrueType outline file");
        let mut weight = None;
        let mut variable = false;
        for table in 0..usize::from(be16(4)) {
            let record = 12 + table * 16;
            match &file[record..record + 4] {
                b"OS/2" => weight = Some(be16(be32(record + 8) + 4)),
                b"fvar" => variable = true,
                _ => {}
            }
        }
        (weight.expect("an OS/2 table"), variable)
    }

    #[test]
    fn the_bundled_family_is_static_regular_and_semibold_inter() {
        assert_eq!(FONT, Font::with_name("Inter"));
        assert_eq!(FONT_SEMIBOLD.family, FONT.family);
        assert_eq!(FONT_SEMIBOLD.weight, Weight::Semibold);
        assert_eq!(
            FONT_FILES.map(weight_class_and_variation),
            [(400, false), (600, false)]
        );
    }

    #[test]
    fn grid_matches_the_visual_language_table() {
        assert_eq!(SPACING, 8.0);
        assert_eq!(RADIUS, 6.0);
        assert_eq!(BORDER_WIDTH, 1.0);
    }

    #[test]
    fn dragging_handle_turns_accent() {
        let style = slider_style(true)(&theme(), slider::Status::Active);
        assert_eq!(style.handle.background, Background::Color(ACCENT));
        assert_eq!(
            style.handle.border_color,
            Color::TRANSPARENT,
            "the halo, not the dark ring, surrounds a dragged handle"
        );
    }

    #[test]
    fn resting_handle_is_not_accent_and_iced_draws_no_rail() {
        let style = slider_style(false)(&theme(), slider::Status::Active);
        assert_eq!(style.handle.background, Background::Color(THUMB));
        let clear = Background::Color(Color::TRANSPARENT);
        assert_eq!(style.rail.backgrounds, (clear, clear));
    }

    /// The Density table of the Module panels design, pinned: a change here moves every section.
    #[test]
    fn module_panel_density_matches_the_design() {
        assert_eq!(MODULE_HEADER_HEIGHT, 32.0);
        assert_eq!(
            (
                SECTION_PADDING.top,
                SECTION_PADDING.right,
                SECTION_PADDING.bottom,
                SECTION_PADDING.left
            ),
            (4.0, 12.0, 10.0, 12.0)
        );
        assert_eq!((GROUP_HEADER_HEIGHT, GROUP_MARGIN), (24.0, 4.0));
        assert_eq!(
            (SLIDER_LABEL_HEIGHT, SLIDER_GAP, SLIDER_RAIL_HEIGHT),
            (14.0, 2.0, 12.0)
        );
        assert_eq!(SLIDER_ROW_HEIGHT, 28.0);
        assert_eq!(SLIDER_ROW_HEIGHT + ROW_SPACING, 30.0, "the slider pitch");
        assert_eq!(VALUE_WIDTH, 48.0);
        assert_eq!(RAIL_WIDTH, 2.0);
        assert_eq!(ZERO_TICK_HEIGHT, 6.0);
        assert_eq!(
            2.0 * (THUMB_RADIUS - THUMB_OUTLINE_WIDTH),
            12.0,
            "a 12 pt thumb"
        );
        assert_eq!(
            (BUTTON_HEIGHT, COMPACT_BUTTON_HEIGHT, BUTTON_ROW_MARGIN),
            (26.0, 22.0, 4.0)
        );
        assert_eq!(TAB_ROW_HEIGHT, 24.0);
        assert_eq!(LIST_ROW_HEIGHT, 26.0);
        // default.png: history rows on a 28 pt pitch, a 6 pt marker, the current row tinted.
        assert_eq!(LIST_ROW_HEIGHT + LIST_ROW_SPACING, 28.0);
        assert_eq!(MARKER_SIZE, 6.0);
        assert_eq!(LIST_ROW_CURRENT, Color::from_rgb8(47, 47, 50));
    }

    #[test]
    fn rail_tokens_match_the_module_panel_references() {
        assert_eq!(RAIL, Color::from_rgb8(0x3a, 0x3a, 0x40));
        assert_eq!(RAIL_FILL, Color::from_rgb8(0xa3, 0xa3, 0xaa));
        assert_eq!(THUMB, Color::from_rgb8(0xec, 0xec, 0xee));
        assert_eq!(BAND_BORDER, Color::from_rgb8(0x2f, 0x2f, 0x32));
        assert_eq!(RULE, Color::from_rgb8(0x31, 0x31, 0x34));
        assert_eq!(ZERO_TICK, Color::from_rgb8(0x5a, 0x5a, 0x62));
        assert_eq!(THUMB_OUTLINE, Color::from_rgb8(0x11, 0x11, 0x13));
        assert_eq!(TEXT_LABEL, Color::from_rgb8(0xc9, 0xc9, 0xce));
    }

    /// The Performance section's layout table, pinned: the heading, the metric row and its boxes,
    /// the sparkline and the job row's lines.
    #[test]
    fn performance_section_sizes_match_the_design() {
        assert_eq!(DISCLOSURE_HEADING_HEIGHT, 22.0);
        assert_eq!(DISCLOSURE_CHEVRON_SIZE, 10.0);
        assert_eq!(METRIC_ROW_HEIGHT, 24.0);
        assert_eq!((METRIC_LABEL_WIDTH, METRIC_VALUE_WIDTH), (48.0, 56.0));
        assert_eq!(SPACING, 8.0, "the metric row's gaps");
        assert_eq!(SPARKLINE_HEIGHT, 16.0);
        assert_eq!(SPARKLINE_LINE_WIDTH, 1.25);
        assert_eq!(SPARKLINE_DOT_RADIUS, 1.75);
        assert_eq!(BORDER_WIDTH, 1.0, "the sparkline's baseline");
        assert_eq!((JOB_LABEL_HEIGHT, CAPTION_LINE_HEIGHT), (16.0, 14.0));
        assert_eq!(JOB_LABEL_INSET, 16.0);
        const { assert!(JOB_LABEL_INSET > MARKER_SIZE, "the label clears its marker") };
        assert_eq!((RAIL_WIDTH, JOB_PROGRESS_GAP), (2.0, 2.0));
        assert_eq!(SIZE_SMALL_CAPTION, 10.5);
        assert_eq!(SIZE_SMALL_CAPTION, SIZE_SECTION_LABEL);
    }

    /// The sparkline's area is the rail fill at 16% over the panel, stored opaque; its baseline,
    /// line and dot are the rule, the rail fill and the thumb.
    #[test]
    fn the_sparkline_area_is_the_rail_fill_at_sixteen_percent_over_the_panel() {
        let [r, g, b] = crate::geometry::over(
            [RAIL_FILL.r, RAIL_FILL.g, RAIL_FILL.b],
            [PANEL.r, PANEL.g, PANEL.b],
            0.16,
        )
        .map(|channel| (channel * 255.0).round() as u8);
        assert_eq!(SPARKLINE_AREA, Color::from_rgb8(r, g, b));
        assert_eq!(SPARKLINE_AREA, Color::from_rgb8(0x35, 0x35, 0x39));
        assert_eq!(
            SPARKLINE_AREA.a, 1.0,
            "opaque, not an alpha Iced would brighten"
        );
    }

    /// The shell's translucent fills are the default board's alphas composited over the Bar
    /// surface, stored opaque; its sizes are the board's.
    #[test]
    fn shell_tokens_match_the_default_board() {
        let over_bar = |colour: Color, alpha: f32| {
            let [r, g, b] =
                crate::geometry::over([colour.r, colour.g, colour.b], [BAR.r, BAR.g, BAR.b], alpha)
                    .map(|channel| (channel * 255.0).round() as u8);
            Color::from_rgb8(r, g, b)
        };
        // The rules are sampled from the board, where the 6% border lands a code under white over
        // the Bar; they are the module band's border.
        assert_eq!(DIVIDER, BAND_BORDER);
        assert_eq!(ICON_HOVER, over_bar(Color::WHITE, 0.06));
        assert_eq!(STRIP_RULE, over_bar(Color::WHITE, 0.10));
        // Sampled from the board's selected panel toggles, whose blue lands a code under the exact
        // composite.
        assert_eq!(ICON_SELECTED_FILL, Color::from_rgb8(62, 55, 47));
        let composite = over_bar(ACCENT, 0.14);
        for (sampled, exact) in [
            (ICON_SELECTED_FILL.r, composite.r),
            (ICON_SELECTED_FILL.g, composite.g),
            (ICON_SELECTED_FILL.b, composite.b),
        ] {
            assert!((sampled - exact).abs() <= 1.0 / 255.0 + f32::EPSILON);
        }
        assert_eq!(TEXT_IDENTITY, Color::from_rgb8(0x8a, 0x8a, 0x90));
        assert_eq!(TEXT_CURRENT_ROW, Color::from_rgb8(0xf2, 0xf2, 0xf4));
        assert_eq!(AGENT_CONNECTED, Color::from_rgb8(0x57, 0xb5, 0x6b));
        assert_eq!((SEGMENT_TRACK, SEGMENT_SELECTED), (TAB_TRACK, TAB_SELECTED));
        assert_eq!(SEGMENT_TRACK, Color::from_rgb8(0x28, 0x28, 0x2c));
        assert_eq!(SEGMENT_SELECTED, Color::from_rgb8(0x3b, 0x3b, 0x41));
        assert_eq!((ICON_BUTTON_SIZE, ICON_SIZE), (26.0, 16.0));
        assert_eq!(
            (
                SEGMENT_HEIGHT,
                SEGMENT_PADDING,
                SEGMENT_INSET,
                SEGMENT_RADIUS
            ),
            (24.0, 10.0, 2.0, 7.0)
        );
        assert_eq!(
            SEGMENT_HEIGHT + 2.0 * SEGMENT_INSET,
            28.0,
            "the control's height"
        );
        assert_eq!((TITLE_GROUP_SPACING, TITLE_ACTION_SPACING), (10.0, 4.0));
        assert_eq!((TOOLBAR_RULE_HEIGHT, TOOLBAR_RULE_MARGIN), (16.0, 6.0));
        assert_eq!(TITLE_BAR_INSET, 12.0);
        assert_eq!(SIZE_IDENTITY, 11.5);
        // Text sits 16 pt from the state panel's edge: the panel's padding and a row's own.
        assert_eq!(PANEL_PADDING_X + SPACING, 16.0);
        assert_eq!((PANEL_PADDING_Y, PANEL_SECTION_SPACING), (12.0, 12.0));
        assert_eq!(PANEL_HEADING_HEIGHT, 22.0);
        assert_eq!((VERSION_CHIP_HEIGHT, VERSION_CHIP_SPACING), (16.0, 6.0));
        assert_eq!(LIST_ROW_RADIUS, 5.0);
        assert_eq!(
            (STATUS_SPACING, STATUS_FACT_SPACING, STATUS_DOT_SIZE),
            (8.0, 12.0, 6.0)
        );
    }

    /// A selected icon button is the accent tint behind accent ink, with no outline; the segmented
    /// control's selection is the neutral raised fill, never the accent.
    #[test]
    fn selection_styles_match_the_default_board() {
        let selected = button_selected(&theme(), button::Status::Active);
        assert_eq!(
            selected.background,
            Some(Background::Color(ICON_SELECTED_FILL))
        );
        assert_eq!(selected.text_color, ACCENT);
        assert_eq!(selected.border.width, 0.0);
        let segment = segment(true)(&theme(), button::Status::Active);
        assert_eq!(
            segment.background,
            Some(Background::Color(SEGMENT_SELECTED))
        );
        assert_eq!(segment.text_color, TEXT_BRIGHT);
        let resting = super::segment(false)(&theme(), button::Status::Active);
        assert_eq!(resting.background, None);
        assert_eq!(resting.text_color, TEXT_SECONDARY);
        assert_eq!(
            button_icon(&theme(), button::Status::Hovered).background,
            Some(Background::Color(ICON_HOVER))
        );
        assert_eq!(
            button_icon(&theme(), button::Status::Active).background,
            None
        );
    }

    #[test]
    fn faint_text_sits_between_the_panel_and_tertiary_text() {
        assert_eq!(TEXT_FAINT, Color::from_rgb8(0x55, 0x55, 0x5c));
        for (panel, faint, tertiary) in [
            (PANEL.r, TEXT_FAINT.r, TEXT_TERTIARY.r),
            (PANEL.g, TEXT_FAINT.g, TEXT_TERTIARY.g),
            (PANEL.b, TEXT_FAINT.b, TEXT_TERTIARY.b),
        ] {
            assert!(panel < faint && faint < tertiary);
        }
    }

    #[test]
    fn a_disclosure_heading_lifts_to_secondary_under_the_pointer() {
        let colour = |status| button_disclosure(&theme(), status).text_color;
        assert_eq!(colour(button::Status::Active), TEXT_TERTIARY);
        assert_eq!(colour(button::Status::Hovered), TEXT_SECONDARY);
        assert_eq!(colour(button::Status::Pressed), TEXT_SECONDARY);
        assert_eq!(
            button_disclosure(&theme(), button::Status::Hovered).background,
            None
        );
        assert_eq!(disclosure_color(false), TEXT_TERTIARY);
        assert_eq!(disclosure_color(true), TEXT_SECONDARY);
    }

    /// The white-balance rails, drawn at the colour-rail opacity over the panel, land on the
    /// colours sampled from basic.png at their start, middle and end.
    #[test]
    fn white_balance_rails_composite_to_the_basic_reference() {
        let drawn = |colour: Color| {
            let [r, g, b] = crate::geometry::over(
                [colour.r, colour.g, colour.b],
                [PANEL.r, PANEL.g, PANEL.b],
                DECORATED_RAIL_OPACITY,
            );
            [r, g, b].map(|channel| (channel * 255.0).round() as u8)
        };
        assert_eq!(
            TEMPERATURE_RAIL.map(drawn),
            [[0x46, 0x7b, 0xc3], [0x7e, 0x7e, 0x83], [0xc5, 0x9d, 0x60]]
        );
        assert_eq!(
            TINT_RAIL.map(drawn),
            [[0x4f, 0x9f, 0x60], [0x7d, 0x7f, 0x82], [0xbd, 0x56, 0xb6]]
        );
        assert_eq!(DECORATED_RAIL_OPACITY, 0.85);
    }

    /// Composites `colour` at `opacity` over `background` in sRGB, as the boards' CSS does, to the
    /// 8-bit code a board samples.
    fn composite(colour: Color, background: Color, opacity: f32) -> [u8; 3] {
        crate::geometry::over(
            [colour.r, colour.g, colour.b],
            [background.r, background.g, background.b],
            opacity,
        )
        .map(|channel| (channel * 255.0).round() as u8)
    }

    fn code(colour: Color) -> [u8; 3] {
        [colour.r, colour.g, colour.b].map(|channel| (channel * 255.0).round() as u8)
    }

    /// The canvas chrome's sizes are the boards'.
    #[test]
    fn canvas_chrome_sizes_match_the_boards() {
        assert_eq!(CHROME_INSET, 12.0);
        assert_eq!(CHROME_STACK_SPACING, 8.0);
        assert_eq!(CHROME_RADIUS, 9.0);
        assert_eq!(
            (STRIP_PADDING, STRIP_SPACING, STRIP_RADIUS),
            (3.0, 2.0, 10.0)
        );
        assert_eq!((STRIP_TOOL_WIDTH, STRIP_TOOL_HEIGHT), (34.0, 30.0));
        assert_eq!(STRIP_TOOL_RADIUS, 7.0);
        assert_eq!((STRIP_RULE_HEIGHT, STRIP_RULE_MARGIN), (16.0, 4.0));
        assert_eq!(
            STRIP_HEIGHT, 38.0,
            "a tool, 3 pt padding and a 1 pt border each side"
        );
        assert_eq!(DRAFT_BAR_HEIGHT, 34.0);
        assert_eq!(
            (
                DRAFT_BAR_PADDING.top,
                DRAFT_BAR_PADDING.right,
                DRAFT_BAR_PADDING.left
            ),
            (0.0, 6.0, 12.0)
        );
        assert_eq!(DRAFT_BAR_SPACING, 12.0);
        assert_eq!(NOTICE_WIDTH, 560.0);
        assert_eq!(
            (
                NOTICE_PADDING.top,
                NOTICE_PADDING.right,
                NOTICE_PADDING.bottom,
                NOTICE_PADDING.left
            ),
            (10.0, 10.0, 10.0, 14.0)
        );
        assert_eq!(NOTICE_SPACING, 12.0);
        assert_eq!((SIZE_NOTICE_TITLE, SIZE_NOTICE_BODY), (12.5, 11.5));
        assert_eq!((CROP_CORNER, CROP_CORNER_RADIUS), (9.0, 1.0));
        assert_eq!((CROP_EDGE_LENGTH, CROP_EDGE_THICKNESS), (22.0, 5.0));
    }

    /// The chrome's tints are the boards' samples: each is its CSS alpha composited over the Bar
    /// surface in sRGB, to within a code of the board.
    #[test]
    fn canvas_chrome_tints_are_the_boards_composites() {
        let white = Color::WHITE;
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1);
        assert!(near(code(CHROME_BORDER), composite(white, BAR, 0.08)));
        assert!(near(code(STRIP_RULE), composite(white, BAR, 0.10)));
        assert!(near(code(STRIP_SELECTED), composite(ACCENT, BAR, 0.16)));
        assert!(near(
            code(NOTICE_WARNING_BORDER),
            composite(ACCENT, BAR, 0.35)
        ));
        assert!(near(
            code(NOTICE_ERROR_BORDER),
            composite(CLIPPING_HIGHLIGHT, BAR, 0.40)
        ));
        assert!(near(code(HISTOGRAM_BORDER), composite(white, CANVAS, 0.05)));
        assert_eq!(code(STRIP_ICON), [0xb9, 0xb9, 0xbf]);
        assert_eq!(code(TEXT_BRIGHT), [0xf0, 0xf0, 0xf2]);
        assert_eq!(code(PRIMARY_INK), [0x1a, 0x14, 0x08]);
        assert_eq!(code(CLIP_TRIANGLE_REST), [0x55, 0x55, 0x5c]);
    }

    /// The histogram inspector is the plot and its padding and nothing else, so its height is one
    /// constant whatever the analysis says.
    #[test]
    fn the_histogram_inspector_matches_the_boards() {
        assert_eq!(
            (
                HISTOGRAM_PADDING.top,
                HISTOGRAM_PADDING.right,
                HISTOGRAM_PADDING.bottom,
                HISTOGRAM_PADDING.left
            ),
            (10.0, 12.0, 6.0, 12.0)
        );
        assert_eq!(HISTOGRAM_RADIUS, 6.0);
        assert_eq!((CLIP_TRIANGLE_WIDTH, CLIP_TRIANGLE_HEIGHT), (10.0, 8.0));
        assert_eq!(CLIP_TRIANGLE_INSET, 6.0);
        assert_eq!(HISTOGRAM_INSPECTOR_HEIGHT, 104.0);
    }

    /// mask-panels.html: the rows' tints and the menu's lines are the board's CSS alphas composited
    /// over the surfaces they sit on, to within a code.
    #[test]
    fn masks_panel_tints_are_the_boards_composites() {
        let white = Color::WHITE;
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1);
        assert!(near(
            code(MASK_ROW_SELECTED),
            composite(ACCENT, PANEL, 0.12)
        ));
        assert!(near(code(LIST_ROW_CURRENT), composite(white, PANEL, 0.07)));
        assert!(near(code(ROW_HOVER), composite(white, PANEL, 0.04)));
        assert!(near(
            code(THUMBNAIL_BORDER),
            composite(white, THUMBNAIL_BACKGROUND, 0.08)
        ));
        assert!(near(
            code(MENU_BORDER),
            composite(white, MENU_SURFACE, 0.10)
        ));
        assert!(near(
            code(MENU_SEPARATOR),
            composite(white, MENU_SURFACE, 0.08)
        ));
        assert!(near(
            code(MENU_ITEM_HOVER),
            composite(white, MENU_SURFACE, 0.06)
        ));
        let fixed_segment = composite(SEGMENT_SELECTED, PANEL, MODE_FIXED_OPACITY);
        let fixed_segment = Color::from_rgb8(fixed_segment[0], fixed_segment[1], fixed_segment[2]);
        assert!(near(
            code(MODE_FIXED_INK),
            composite(TEXT_BRIGHT, fixed_segment, MODE_FIXED_OPACITY)
        ));
    }

    #[test]
    fn masks_panel_sizes_match_the_board() {
        assert_eq!(MASK_ROW_HEIGHT, 26.0);
        assert_eq!(
            (THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT, THUMBNAIL_RADIUS),
            (28.0, 19.0, 3.0)
        );
        assert_eq!(
            (MODE_SEGMENT_SIZE, MODE_INSET, MODE_RADIUS),
            (18.0, 1.0, 5.0)
        );
        assert_eq!(
            MODE_SEGMENT_SIZE * 3.0 + MODE_INSET * 4.0,
            58.0,
            "three segments on their track"
        );
        assert_eq!((OVERLAY_ROW_HEIGHT, OVERLAY_SEGMENT_HEIGHT), (26.0, 20.0));
        assert_eq!((MENU_WIDTH, MENU_ITEM_HEIGHT), (200.0, 26.0));
        assert_eq!(
            (GRID_ROW_HEIGHT, GRID_FIELD_WIDTH, GRID_FIELD_HEIGHT),
            (22.0, 52.0, 18.0)
        );
        assert_eq!(COMPONENT_DETAIL_INDENT, 22.0);
        assert_eq!(COMPACT_TOGGLE_ROW_HEIGHT, 22.0);
        assert_eq!((SWATCH_SLOT_WIDTH, SWATCH_SLOT_HEIGHT), (22.0, 18.0));
        assert_eq!(STROKE_ROW_HEIGHT, 20.0);
    }

    // -- Select chrome's token tests.

    /// The catalog boards' literal colours, copied exactly.
    #[test]
    fn select_chrome_literals_match_the_catalog_boards() {
        assert_eq!(code(SOURCE_MONTH), [0x5f, 0x5f, 0x66]);
        assert_eq!(code(SELECT_BAR), [0x1d, 0x1d, 0x20]);
        assert_eq!(code(FILTER_CHIP), [0x26, 0x26, 0x2a]);
        assert_eq!(code(DEVELOP_DISABLED_INK), [0x5a, 0x5a, 0x62]);
        assert_eq!(code(FRAME_ACTIVE), [0x2a, 0x2a, 0x2f]);
        // The inks the boards name that the workspace already has.
        assert_eq!(
            code(TEXT_IDENTITY),
            [0x8a, 0x8a, 0x90],
            "`.src .ic`, `.hint`"
        );
        assert_eq!(code(CHIP_LABEL), [0xb0, 0xb0, 0xb6], "`.fchip`");
        assert_eq!(code(TEXT_CURRENT_ROW), [0xf2, 0xf2, 0xf4], "`.src.on`");
        assert_eq!(code(TEXT_FAINT), [0x55, 0x55, 0x5c], "a count's ` / `");
        assert_eq!(code(AGENT_CONNECTED), [0x57, 0xb5, 0x6b], "`.src .vd`");
        assert_eq!(code(RAIL), [0x3a, 0x3a, 0x40], "`.bar`");
        // The region box keeps its alpha: it sits over a photograph.
        assert_eq!((REGION_BOX.a, REGION_BOX_RING.a), (0.9, 0.5));
    }

    /// Every translucent fill the catalog boards draw over a known surface is stored opaque: the
    /// CSS alpha composited over that surface, to within a code.
    #[test]
    fn select_chrome_tints_are_the_boards_composites() {
        let white = Color::WHITE;
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1);
        // `.src.on`: white at 7% over the panel, the history's current row.
        assert!(near(
            code(SOURCE_ROW_SELECTED),
            composite(white, PANEL, 0.07)
        ));
        assert_eq!(SOURCE_ROW_SELECTED, LIST_ROW_CURRENT);
        // `.fbar`'s and `.fstrip`'s rule: white at 6% over their surface.
        assert_eq!(code(SELECT_BAR_RULE), composite(white, SELECT_BAR, 0.06));
        // `.fchip.on`: the accent at 16% over the filter bar; its clear at 80% over that.
        assert_eq!(code(FILTER_CHIP_SET), composite(ACCENT, SELECT_BAR, 0.16));
        assert_eq!(
            code(FILTER_CHIP_CLEAR),
            composite(ACCENT, FILTER_CHIP_SET, 0.8)
        );
        // The sheet: white at 10% over the Bar, and its footer's rule at 7% over the panel.
        assert_eq!(code(SHEET_BORDER), composite(white, BAR, 0.10));
        assert_eq!(code(SHEET_FOOTER_RULE), composite(white, PANEL, 0.07));
        // The 100% inset: white at 12% over the Bar.
        assert_eq!(code(FOCUS_INSET_BORDER), composite(white, BAR, 0.12));
        for colour in [
            SOURCE_ROW_SELECTED,
            SELECT_BAR_RULE,
            FILTER_CHIP_SET,
            FILTER_CHIP_CLEAR,
            SHEET_BORDER,
            SHEET_FOOTER_RULE,
            FOCUS_INSET_BORDER,
        ] {
            assert_eq!(colour.a, 1.0, "opaque, not an alpha Iced would brighten");
        }
    }

    /// The catalog boards' sizes, pinned.
    #[test]
    fn select_chrome_sizes_match_the_catalog_boards() {
        // `.src`, `sec_head`, `.mon`, `.tag`, `.lsearch` and `.search`.
        assert_eq!(
            (SOURCE_ROW_HEIGHT, SOURCE_ROW_PADDING, SOURCE_INDENT),
            (24.0, 8.0, 12.0)
        );
        assert_eq!(
            (SOURCE_ROW_SPACING, SOURCE_ICON_WIDTH, SOURCE_ICON_SIZE),
            (7.0, 14.0, 13.0)
        );
        assert_eq!((SOURCE_CHEVRON_SIZE, SOURCE_SECONDARY_SPACING), (10.0, 6.0));
        assert_eq!(SOURCE_HEADING_HEIGHT, 22.0);
        assert_eq!(
            (
                SOURCE_MONTH_PADDING.top,
                SOURCE_MONTH_PADDING.right,
                SOURCE_MONTH_PADDING.bottom
            ),
            (6.0, 8.0, 2.0)
        );
        assert_eq!((SIZE_TAG, TAG_RADIUS, SOURCE_TAG_SPACING), (10.0, 4.0, 8.0));
        assert_eq!(
            (SEARCH_HEIGHT, SEARCH_WIDTH, SIZE_FILTER),
            (26.0, 250.0, 11.5)
        );
        // `.fbar`, `.fseg`, `.fchip`.
        assert_eq!(
            (FILTER_BAR_HEIGHT, FILTER_BAR_PADDING, FILTER_BAR_SPACING),
            (40.0, 12.0, 6.0)
        );
        assert_eq!((FILTER_SEGMENT_HEIGHT, FILTER_SEGMENT_PADDING), (22.0, 9.0));
        assert_eq!(
            FILTER_SEGMENT_HEIGHT + 2.0 * SEGMENT_INSET,
            26.0,
            "the track"
        );
        assert_eq!((FILTER_SEGMENT_RADIUS, FILTER_TRACK_RADIUS), (4.0, 6.0));
        assert_eq!(
            (FILTER_CHIP_HEIGHT, FILTER_CHIP_PADDING, FILTER_CHIP_SPACING),
            (24.0, 8.0, 5.0)
        );
        // `.tb.pri` and the busy button.
        assert_eq!((DEVELOP_HEIGHT, DEVELOP_PADDING), (28.0, 12.0));
        assert_eq!(
            (DEVELOP_BUSY_MIN_WIDTH, DEVELOP_BUSY_BAR_WIDTH),
            (150.0, 48.0)
        );
        // `.bar`, `statusbit`, `jobrow` and `sheet`.
        assert_eq!(WORK_BAR_HEIGHT, 4.0);
        assert_eq!((STATUS_JOB_HEIGHT, STATUS_JOB_BAR_WIDTH), (26.0, 70.0));
        assert_eq!(WORK_ROW_INDENT, 14.0);
        assert_eq!(
            (SHEET_WIDTH, SHEET_RADIUS, SHEET_BAR_HEIGHT),
            (330.0, 12.0, 5.0)
        );
        // `.mstrip .fr`, `.tk`, the inset and the filmstrip.
        assert_eq!((MOMENT_FRAME_WIDTH, MOMENT_FRAME_HEIGHT), (124.0, 84.0));
        assert_eq!((MOMENT_IMAGE_WIDTH, MOMENT_IMAGE_HEIGHT), (116.0, 76.0));
        assert_eq!(PICK_CHECK_INSET, 5.0);
        assert_eq!(
            (FOCUS_INSET_WIDTH, FOCUS_REGION_HEIGHT, FOCUS_FOOTER_HEIGHT),
            (308.0, 209.0, 24.0)
        );
        assert_eq!((FILMSTRIP_HEIGHT, FILMSTRIP_HEADER_HEIGHT), (92.0, 24.0));
        assert_eq!((FILMSTRIP_CELL_WIDTH, FILMSTRIP_CELL_HEIGHT), (78.0, 58.0));
        assert_eq!(
            (FILMSTRIP_IMAGE_WIDTH, FILMSTRIP_IMAGE_HEIGHT),
            (72.0, 52.0)
        );
        // The strip's cells fit its height: header, the cells and the bottom inset under the rule.
        const {
            assert!(
                FILMSTRIP_HEADER_HEIGHT + FILMSTRIP_CELL_HEIGHT + FILMSTRIP_BOTTOM + BORDER_WIDTH
                    <= FILMSTRIP_HEIGHT
            )
        };
    }

    /// A source row lifts under the pointer and keeps its selection in every state; a set filter
    /// chip is the accent tint behind accent ink.
    #[test]
    fn select_chrome_styles_follow_their_state() {
        let row = |selected, status| source_row(selected)(&theme(), status).background;
        assert_eq!(row(false, button::Status::Active), None);
        assert_eq!(
            row(false, button::Status::Hovered),
            Some(Background::Color(ROW_HOVER))
        );
        assert_eq!(
            row(true, button::Status::Active),
            Some(Background::Color(SOURCE_ROW_SELECTED))
        );
        let chip = filter_chip(true)(&theme(), button::Status::Active);
        assert_eq!(chip.background, Some(Background::Color(FILTER_CHIP_SET)));
        assert_eq!(chip.text_color, ACCENT);
        let chip = filter_chip(false)(&theme(), button::Status::Active);
        assert_eq!(chip.background, Some(Background::Color(FILTER_CHIP)));
        assert_eq!(chip.text_color, CHIP_LABEL);
        let cell = image_cell(true, MOMENT_FRAME_RADIUS, MOMENT_ACTIVE_OUTLINE)(
            &theme(),
            button::Status::Active,
        );
        assert_eq!(cell.background, Some(Background::Color(FRAME_ACTIVE)));
        assert_eq!((cell.border.width, cell.border.color), (2.0, ACCENT));
        let cell = image_cell(false, MOMENT_FRAME_RADIUS, MOMENT_ACTIVE_OUTLINE)(
            &theme(),
            button::Status::Active,
        );
        assert_eq!(cell.background, Some(Background::Color(BAR)));
        assert_eq!(cell.border.width, 0.0);
        assert_eq!(
            develop_disabled(&theme(), button::Status::Disabled).text_color,
            DEVELOP_DISABLED_INK
        );
    }

    // -- end Select chrome's token tests.

    /// The resolve board's composites over a group's surface, recomputed from its CSS.
    #[test]
    fn resolve_composites_match_the_resolve_board() {
        let white = Color::WHITE;
        assert_eq!(code(RESOLVE_GROUP), [0x1f, 0x1f, 0x22], "`.rg` background");
        assert_eq!(
            code(RESOLVE_GROUP_OUTLINE),
            composite(white, RESOLVE_GROUP, 0.07),
            "`.rg` inset outline"
        );
        assert_eq!(
            code(RESOLVE_HEADER_RULE),
            composite(white, RESOLVE_GROUP, 0.05),
            "`.gh` rule"
        );
        assert_eq!(
            code(RESOLVE_ROW_RULE),
            composite(white, RESOLVE_GROUP, 0.04),
            "`.rr` rule"
        );
        assert_eq!(
            code(RESOLVE_ROW_SELECTED),
            composite(white, RESOLVE_GROUP, 0.05),
            "`.rr.on`"
        );
        assert_eq!(code(RESOLVE_FOUND), [0x57, 0xb5, 0x6b], "`.ok`");
        assert_eq!(code(RESOLVE_REFUSED), [0xe5, 0x53, 0x4b], "`.bad`");
    }

    /// The resolve board's sizes, pinned.
    #[test]
    fn resolve_sizes_match_the_resolve_board() {
        assert_eq!(
            (
                RESOLVE_GROUP_RADIUS,
                RESOLVE_GROUP_SPACING,
                RESOLVE_HEADER_HEIGHT
            ),
            (9.0, 10.0, 44.0)
        );
        assert_eq!(
            (
                RESOLVE_ROW_HEIGHT,
                RESOLVE_ROW_SPACING,
                RESOLVE_PREVIEW_COLUMN,
                RESOLVE_NAME_COLUMN,
                RESOLVE_ACTION_COLUMN
            ),
            (44.0, 12.0, 52.0, 150.0, 96.0)
        );
        assert_eq!(
            (RESOLVE_PREVIEW_WIDTH, RESOLVE_PREVIEW_HEIGHT),
            (48.0, 32.0)
        );
        assert_eq!(
            (RESOLVE_BAR_RADIUS, RESOLVE_BAR_SPACING, RESOLVE_BAR_BOTTOM),
            (10.0, 12.0, 12.0)
        );
    }
}
