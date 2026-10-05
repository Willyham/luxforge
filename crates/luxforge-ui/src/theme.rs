//! Design tokens for the Develop workspace and the styling functions built from them.
//!
//! Every colour, size, spacing and radius a widget uses comes from this module, so the app never
//! writes an ad hoc colour or size. Sizes, spacing and type are constants copied from the visual
//! language table in `docs/design/develop-workspace.md`; the unit tests in this module assert the
//! copy is exact.
//!
//! Colours are of two kinds ([UI themes](../../../docs/design/ui-themes.md#rules)). A themed
//! colour is a token of the [`Palette`] the running [`Theme`] holds: Iced hands that theme to every
//! style function and every canvas `draw`, which read it there. A colour whose meaning is its
//! colour is fixed, the same in every theme, and is one of the constants under "Fixed colours"
//! below, each with the reason it is fixed.

mod palette;
mod runtime;

pub use palette::{Ink, Palette, Token};
pub use runtime::{Mode, Theme};

use iced::font::Weight;
use iced::widget::{button, container, slider, text_input};
use iced::{Background, Border, Color, Font, Padding, Shadow};

// -- Fixed colours ------------------------------------------------------------------------------
//
// Never themed: each says something by its colour, so a theme that moved it would change what it
// says. Every other colour a widget draws is a token of the theme's palette.

/// The canvas background's `dark` choice, the one nobody chose. Fixed because the canvas
/// background is the person's own choice of grey around the photograph, the same in every theme.
/// Luxforge Dark's surround has this value.
pub const CANVAS: Color = Color::from_rgb8(0x19, 0x19, 0x1b);
/// The `black` canvas background; fixed for the same reason as [`CANVAS`].
pub const CANVAS_BLACK: Color = Color::from_rgb8(0x00, 0x00, 0x00);
/// The `grey` canvas background: an 18% grey, L* 50, for judging tone the way a print is judged;
/// fixed for the same reason as [`CANVAS`].
pub const CANVAS_GREY: Color = Color::from_rgb8(0x77, 0x77, 0x77);

/// The `temperature` rail hint's stops, blue through a neutral grey to amber. Fixed because a
/// declared rail's colours are the module's meaning. A colour rail is drawn at
/// [`DECORATED_RAIL_OPACITY`] over the theme's rail backdrop, so these are the colours that
/// composite to the module references' samples over Luxforge Dark (asserted in the tests below).
pub const TEMPERATURE_RAIL: [Color; 3] = [
    Color::from_rgb8(77, 139, 223),
    Color::from_rgb8(143, 143, 148),
    Color::from_rgb8(226, 179, 107),
];
/// The `tint` rail hint's stops, green through a neutral grey to magenta; fixed as
/// [`TEMPERATURE_RAIL`] is.
pub const TINT_RAIL: [Color; 3] = [
    Color::from_rgb8(87, 181, 107),
    Color::from_rgb8(141, 144, 147),
    Color::from_rgb8(217, 95, 208),
];

/// Highlight clipping indicator. Fixed and reserved for clipping: the histogram's corners and the
/// overlay say "clipped" by this red in every theme. The theme's error ink starts from it and is
/// never reused for clipping.
pub const CLIPPING_HIGHLIGHT: Color = Color::from_rgb8(0xe5, 0x53, 0x4b);
/// Shadow clipping indicator. Fixed and reserved for clipping, as [`CLIPPING_HIGHLIGHT`] is.
pub const CLIPPING_SHADOW: Color = Color::from_rgb8(0x4c, 0x8b, 0xe0);
/// The overlay colour of a cell that holds both endpoints. It is not a third invented colour: it
/// takes its red and green from [`CLIPPING_HIGHLIGHT`] and its blue from [`CLIPPING_SHADOW`], which
/// is exactly what "red and blue at once" means and reads as magenta over a photograph. The
/// composition is asserted in this module's tests rather than written out twice. Fixed with them.
pub const CLIPPING_BOTH: Color = Color {
    r: CLIPPING_HIGHLIGHT.r,
    g: CLIPPING_HIGHLIGHT.g,
    b: CLIPPING_SHADOW.b,
    a: 1.0,
};

/// The mask overlay tints, one per name in the core's `MaskOverlayColour`. Fixed and reserved for
/// the mask overlay: they say "this is the selection", never "this is clipped", over the
/// photograph's own pixels.
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

/// The three histogram channel fills. Fixed: they are the plain additive primaries rather than
/// tinted versions of them, because the plot's whole job is to say which channel a count belongs
/// to and what their overlap is; the alpha below is what makes the overlap readable.
pub const CHANNEL_RED: Color = Color::from_rgb8(0xff, 0x4d, 0x4d);
pub const CHANNEL_GREEN: Color = Color::from_rgb8(0x4d, 0xff, 0x7a);
pub const CHANNEL_BLUE: Color = Color::from_rgb8(0x4d, 0x9a, 0xff);
/// How opaque one channel fill is. Three overlapping fills at this alpha keep each channel
/// readable on its own and turn a full overlap into the grey the contract describes.
pub const CHANNEL_ALPHA: f32 = 0.55;

/// The colour of a composition guide drawn over the photograph (the thirds overlay, the crop
/// overlay's own thirds): white at the opacity a line needs to stay readable over an image. Fixed
/// because it is drawn over the photograph, not over the theme's surfaces.
pub const GUIDE: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.30,
};

/// The track of the bar along the bottom of the photograph while a long render runs: dark enough
/// to read over a bright image, translucent so the photograph still shows through it. Fixed
/// because it is drawn over the photograph.
pub const RENDER_BAR_TRACK: Color = Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.45,
};
/// That bar's thickness, a little heavier than a rail so it reads over a photograph.
pub const RENDER_BAR_HEIGHT: f32 = 3.0;

/// What the compare and mask canvases draw over the photograph: the compare divider and the ring
/// of its grip, the divider's dark outline, the grip's fill, its arrows and the Before and After
/// labels, and a mask's anchor grip. Fixed because they sit on the photograph's own pixels rather
/// than on the theme's surfaces; their values are the visual language's thumb, thumb ring, Bar,
/// primary text and accent.
pub const PHOTO_HANDLE: Color = Color::from_rgb8(0xec, 0xec, 0xee);
pub const PHOTO_HANDLE_OUTLINE: Color = Color::from_rgb8(0x11, 0x11, 0x13);
pub const PHOTO_GRIP: Color = Color::from_rgb8(0x23, 0x23, 0x26);
pub const PHOTO_LABEL: Color = Color::from_rgb8(0xe8, 0xe8, 0xea);
pub const PHOTO_ANCHOR: Color = Color::from_rgb8(0xe2, 0xb4, 0x6a);

/// The status bar's dot while any other client is connected. Fixed: green says "connected".
pub const AGENT_CONNECTED: Color = Color::from_rgb8(0x57, 0xb5, 0x6b);

/// The dark outline round every colour swatch: black at 50%, which reads on any colour. Fixed
/// with the swatches, whose colours are the photograph's.
pub const SWATCH_OUTLINE: Color = Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.5,
};

/// A coverage thumbnail's near-black ground. Fixed: it is the mask's own zero, which the coverage
/// is drawn over in white.
pub const THUMBNAIL_BACKGROUND: Color = Color::from_rgb8(0x0e, 0x0e, 0x10);

/// The mask-on-black glyph's outline (`#666`) and the photograph's stand-in colour in the
/// photo-through-selection glyph (`#7a8a4a`), both the board's. Fixed: each glyph is a small
/// picture of the overlay it chooses, which draws the same in every theme.
pub const MASK_GLYPH_OUTLINE: Color = Color::from_rgb8(0x66, 0x66, 0x66);
pub const MASK_GLYPH_PHOTO: Color = Color::from_rgb8(0x7a, 0x8a, 0x4a);

/// The soft black shadow under the floating chrome, which lifts it off a photograph as the boards
/// draw it. Fixed because it falls over the photograph.
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
/// The black shadow under a dropdown menu; fixed as [`CHROME_SHADOW`] is.
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

// -- Rules --------------------------------------------------------------------------------------

/// A value's inset from the right edge of its box, so a typed value does not jump when the field
/// opens for editing.
pub const VALUE_INSET: f32 = 3.0;

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

// -- Performance section ------------------------------------------------------------------------
//
// The state panel's Performance block, from the layout table of the performance panel design.
// Its greys are the slider rail's (the palette's `rail`, `rail_fill` and `thumb`) and the group
// rule (`rule`), never the accent: the photograph is the only colour on screen.

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
// The tinted colours are the palette's, sampled from the boards and precomputed opaque over the
// Bar surface for the same reason as the palette's `rule`.

/// How far the floating chrome sits from the canvas edge: the mode strip above the bottom, the draft
/// bar and the notices below the top.
pub const CHROME_INSET: f32 = 12.0;
/// Between the draft bar and a notice stacked under it.
pub const CHROME_STACK_SPACING: f32 = 8.0;
/// The draft bar's and a notice's corner radius.
pub const CHROME_RADIUS: f32 = 9.0;
/// The mode strip's padding inside its border, the gap between its tools and its radius.
pub const STRIP_PADDING: f32 = 3.0;
pub const STRIP_SPACING: f32 = 2.0;
pub const STRIP_RADIUS: f32 = 10.0;
/// One icon-only tool in the mode strip, and its radius.
pub const STRIP_TOOL_WIDTH: f32 = 34.0;
pub const STRIP_TOOL_HEIGHT: f32 = 30.0;
pub const STRIP_TOOL_RADIUS: f32 = 7.0;
/// The rule between the modes and the view toggles, 1 × [`STRIP_RULE_HEIGHT`] with
/// [`STRIP_RULE_MARGIN`] either side, and the title bar's rule before the panel toggles.
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
/// A clipping triangle's size inside the plot, and its inset from the plot's side and bottom.
pub const CLIP_TRIANGLE_WIDTH: f32 = 10.0;
pub const CLIP_TRIANGLE_HEIGHT: f32 = 8.0;
pub const CLIP_TRIANGLE_INSET: f32 = 6.0;
/// The inspector's whole, unconditional height.
pub const HISTOGRAM_INSPECTOR_HEIGHT: f32 =
    HISTOGRAM_PADDING.top + HISTOGRAM_HEIGHT + HISTOGRAM_PADDING.bottom;

// -- Shell ----------------------------------------------------------------------------------------
//
// The title bar, the state panel and the status bar around the canvas and the tools panel, from
// the default board of the Develop workspace design. Every translucent fill the board draws is
// stored opaque, composited over the surface it sits on, for the reason the palette's `rule` gives.

/// The dimensions line's size, between a caption and control text.
pub const SIZE_IDENTITY: f32 = 11.5;
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
/// A segment's height, its label's horizontal padding, and the track's inset and gap around and
/// between segments.
pub const SEGMENT_HEIGHT: f32 = 24.0;
pub const SEGMENT_PADDING: f32 = 10.0;
pub const SEGMENT_INSET: f32 = 2.0;
/// The track's corner radius; a segment inside it is [`RADIUS`] less its inset, so the two
/// curves stay concentric.
pub const SEGMENT_RADIUS: f32 = 7.0;
/// The chevron after a segment's label that has more behind it, and the gap before it.
pub const SEGMENT_CHEVRON_SIZE: f32 = 8.0;
pub const SEGMENT_CHEVRON_SPACING: f32 = 4.0;
/// A notched rail's stops: one cell each, wide enough for `1200%` at caption size, with its notch
/// on the rail line (tall enough for a held thumb's halo) and its label under it.
pub const NOTCH_CELL_WIDTH: f32 = 38.0;
pub const NOTCH_RAIL_HEIGHT: f32 = 2.0 * THUMB_HALO_RADIUS;
pub const NOTCH_LABEL_GAP: f32 = 2.0;
pub const NOTCH_LABEL_HEIGHT: f32 = 14.0;
pub const NOTCH_HEIGHT: f32 = NOTCH_RAIL_HEIGHT + NOTCH_LABEL_GAP + NOTCH_LABEL_HEIGHT + 2.0;
pub const NOTCH_TICK_WIDTH: f32 = 1.0;
pub const NOTCH_TICK_HEIGHT: f32 = 8.0;
/// The zoom stops' panel: the menu surface, inset so the end labels clear its rounded corners.
pub const NOTCH_PANEL_PADDING: Padding = Padding {
    top: 6.0,
    right: 8.0,
    bottom: 4.0,
    left: 8.0,
};
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

// -- Masks panel ----------------------------------------------------------------------------------

// The Masks panel's rows and controls, from the mask-panels board of the masking workspace design
// (`docs/design/develop-workspace/html/mask-panels.html`): the mask and component rows, the mode
// and overlay controls, the kind menu, the two-column fields and the colour range's swatches. Each
// translucent fill the board draws is stored opaque, composited over the surface it sits on, for
// the reason the palette's `rule` gives; the tests below recompute each composite.

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
/// A mask's amount readout, right-aligned in this box so the digits keep their place.
pub const MASK_AMOUNT_WIDTH: f32 = 26.0;
/// A coverage thumbnail (`.th`): 28 × 19 pt, rounded 3 pt, a 1 pt border of white at 8% over its
/// near-black ground.
pub const THUMBNAIL_WIDTH: f32 = 28.0;
pub const THUMBNAIL_HEIGHT: f32 = 19.0;
pub const THUMBNAIL_RADIUS: f32 = 3.0;
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
/// and segment are dark enough that Iced's linear blending matches the board, so they are the
/// palette's `tab_track` and `tab_selected` at this alpha over whatever row they sit on; the ink
/// is its `mode_fixed_ink`, the bright text at 55% over that segment, precomputed opaque because a
/// light alpha would render far brighter.
pub const MODE_FIXED_OPACITY: f32 = 0.55;
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
/// A dropdown menu (`.menu`): 200 pt wide on its own surface, inset 5 pt, rounded 8 pt, outlined in
/// white at 10% and lifted by a soft shadow.
pub const MENU_WIDTH: f32 = 200.0;
pub const MENU_PADDING: f32 = 5.0;
pub const MENU_RADIUS: f32 = 8.0;
/// A menu item: 26 pt, inset 8 pt, its icon, label and hint 8 pt apart, rounded 5 pt; white at 6%
/// under the pointer (the board draws no hover).
pub const MENU_ITEM_HEIGHT: f32 = 26.0;
pub const MENU_ITEM_PADDING: f32 = 8.0;
pub const MENU_ITEM_SPACING: f32 = 8.0;
pub const MENU_ITEM_RADIUS: f32 = 5.0;
/// A menu's separator: 4 pt above and below, 6 pt in from the sides.
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
/// faint-text outline.
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

fn surface(background: Color) -> container::Style {
    container::Style::default().background(background)
}

fn bordered_surface(theme: &Theme, background: Color) -> container::Style {
    surface(background).border(Border {
        color: theme.palette().border,
        width: BORDER_WIDTH,
        radius: RADIUS.into(),
    })
}

/// Text in `ink`, read from the theme the text is drawn in: the themed counterpart of Iced's
/// `text(..).color(..)`.
pub fn ink(ink: impl Into<Ink>) -> impl Fn(&Theme) -> iced::widget::text::Style {
    let ink = ink.into();
    move |theme| iced::widget::text::Style {
        color: Some(ink.resolve(theme.palette())),
    }
}

/// A flat container in `fill`, read from the theme it is drawn in.
pub fn fill(fill: impl Into<Ink>) -> impl Fn(&Theme) -> container::Style {
    let fill = fill.into();
    move |theme| surface(fill.resolve(theme.palette()))
}

/// The canvas surface in the chosen canvas background — [`CANVAS`], [`CANVAS_BLACK`] or
/// [`CANVAS_GREY`], each fixed: flat, no border (the photograph's own edge reads as the boundary).
pub fn canvas_surface(background: Color) -> impl Fn(&Theme) -> container::Style {
    move |_theme| surface(background)
}

/// A side panel or the status bar: flat, no border.
pub fn panel_surface(theme: &Theme) -> container::Style {
    surface(theme.palette().background)
}

/// The title bar: the Bar surface, flat, its rule drawn under it by the shell.
pub fn title_bar_surface(theme: &Theme) -> container::Style {
    surface(theme.palette().surface)
}

/// A rule between the shell's regions: the band border, opaque.
pub fn divider_surface(theme: &Theme) -> container::Style {
    surface(theme.palette().band_border)
}

/// A floating bar or notice: bordered, rounded.
pub fn bar_surface(theme: &Theme) -> container::Style {
    bordered_surface(theme, theme.palette().surface)
}

/// A control surface (chip, menu, card body): bordered, rounded.
pub fn control_surface(theme: &Theme) -> container::Style {
    bordered_surface(theme, theme.palette().control)
}

/// A modal sheet's backdrop: the scrim over everything behind the sheet.
pub fn scrim_surface(theme: &Theme) -> container::Style {
    surface(theme.palette().scrim)
}

/// A piece of floating canvas chrome — the mode strip, the draft bar or a notice — on the Bar
/// surface with `border`, `radius` and the soft [`CHROME_SHADOW`].
pub fn chrome_surface(theme: &Theme, border: Token, radius: f32) -> container::Style {
    surface(theme.palette().surface)
        .border(Border {
            color: theme.palette().get(border),
            width: BORDER_WIDTH,
            radius: radius.into(),
        })
        .shadow(CHROME_SHADOW)
}

/// One tool in the mode strip: transparent at rest, the control surface under the pointer and the
/// strip's selected fill when it is the active mode or an overlay that is on.
pub fn strip_tool(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.palette();
        let background = match (selected, status) {
            (true, _) => Some(Background::Color(palette.strip_selected)),
            (false, button::Status::Hovered | button::Status::Pressed) => {
                Some(Background::Color(palette.control))
            }
            (false, button::Status::Active | button::Status::Disabled) => None,
        };
        button::Style {
            background,
            text_color: match (selected, status) {
                (_, button::Status::Disabled) => palette.text_tertiary,
                (true, _) => palette.accent,
                (false, _) => palette.strip_icon,
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

/// The histogram plot: the surround with its faint outline and rounded corners.
pub fn histogram_surface(theme: &Theme) -> container::Style {
    surface(theme.palette().surround).border(Border {
        color: theme.palette().histogram_border,
        width: BORDER_WIDTH,
        radius: HISTOGRAM_RADIUS.into(),
    })
}

/// A plain, background-free button: list rows, chips and inline menu items.
pub fn button_plain(theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Some(Background::Color(theme.palette().control))
        }
        button::Status::Active | button::Status::Disabled => None,
    };

    button::Style {
        background,
        text_color: text_color_for(theme, status),
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
pub fn button_accent(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.palette();
    let background = match status {
        button::Status::Disabled => palette.control,
        _ => palette.accent,
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color: match status {
            button::Status::Disabled => palette.text_tertiary,
            _ => palette.accent_ink,
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
/// rest, the icon hover fill under the pointer.
pub fn button_icon(theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Some(Background::Color(theme.palette().icon_hover))
        }
        button::Status::Active | button::Status::Disabled => None,
    };
    button::Style {
        background,
        text_color: text_color_for(theme, status),
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A selected icon button: the selected icon fill behind accent ink, with no outline, as the
/// default board draws the open panels' toggles.
pub fn button_selected(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.palette();
    button::Style {
        background: Some(Background::Color(palette.icon_selected_fill)),
        text_color: if matches!(status, button::Status::Disabled) {
            palette.text_tertiary
        } else {
            palette.accent
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
pub fn swatch_open(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.palette();
    button::Style {
        background: Some(Background::Color(Color {
            a: 0.18,
            ..palette.accent
        })),
        text_color: if matches!(status, button::Status::Disabled) {
            palette.text_tertiary
        } else {
            palette.accent
        },
        border: Border {
            radius: RADIUS.into(),
            width: BORDER_WIDTH,
            color: palette.accent,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A segmented control's track: the tab track, its segments inset by [`SEGMENT_INSET`].
pub fn segment_track(theme: &Theme) -> container::Style {
    surface(theme.palette().tab_track).border(Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: SEGMENT_RADIUS.into(),
    })
}

/// One segment. The selected one is raised on the selected tab fill in bright text and never
/// takes the accent, because it states a view rather than an edit; the others are bare, in
/// secondary text.
pub fn segment(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.palette();
        let background = match (selected, status) {
            (true, _) => Some(Background::Color(palette.tab_selected)),
            (false, button::Status::Hovered | button::Status::Pressed) => {
                Some(Background::Color(palette.control))
            }
            (false, _) => None,
        };
        button::Style {
            background,
            text_color: match (selected, status) {
                (_, button::Status::Disabled) => palette.text_tertiary,
                (true, _) => palette.text_bright,
                (false, _) => palette.text_secondary,
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

fn text_color_for(theme: &Theme, status: button::Status) -> Color {
    match status {
        button::Status::Disabled => theme.palette().text_tertiary,
        _ => theme.palette().text,
    }
}

/// The value field's text input: transparent until focused or invalid.
pub fn text_input_style(invalid: bool) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |theme, status| {
        let palette = theme.palette();
        let border_color = if invalid {
            palette.error
        } else {
            match status {
                text_input::Status::Focused { .. } => palette.accent,
                _ => palette.border,
            }
        };

        text_input::Style {
            background: Background::Color(palette.control),
            border: Border {
                color: border_color,
                width: BORDER_WIDTH,
                radius: RADIUS.into(),
            },
            icon: palette.text_tertiary,
            placeholder: palette.text_tertiary,
            value: palette.text,
            selection: Color {
                a: 0.35,
                ..palette.accent
            },
        }
    }
}

/// A field box's text input: the Control surface with no outline at rest, as the module
/// references draw a value box, the accent outline while focused and the error ink while
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
pub fn button_bare(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: text_color_for(theme, status),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A disclosure heading: no surface in any state, its label tertiary at rest and secondary under
/// the pointer or while pressed. The label takes this text colour, so the style is the one place
/// that decides how the heading answers a hover.
pub fn button_disclosure(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: disclosure_color(
            theme,
            matches!(status, button::Status::Hovered | button::Status::Pressed),
        ),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A disclosure heading's ink, label and chevron alike: secondary while hovered, else tertiary.
pub fn disclosure_color(theme: &Theme, hovered: bool) -> Color {
    if hovered {
        theme.palette().text_secondary
    } else {
        theme.palette().text_tertiary
    }
}

/// A module band's surface: the Bar colour, flat, in every state. The band is a disclosure, so
/// it keeps one colour rather than flashing a hover fill across the panel.
pub fn button_band(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(theme.palette().surface)),
        text_color: text_color_for(theme, status),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A labelled button in a section (a picker or an action): the Control surface, borderless.
pub fn button_control(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.palette();
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Color {
            a: 0.18,
            ..palette.text
        },
        button::Status::Active | button::Status::Disabled => palette.control,
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: text_color_for(theme, status),
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
pub fn list_row_current(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(theme.palette().list_row_current)),
        text_color: theme.palette().text_current_row,
        border: Border {
            radius: LIST_ROW_RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
    .with_disabled(theme, status)
}

/// A mask or component row: the `selected` token fills it in every state, else the row hover
/// shows under the pointer or while `hovered` says the row is the one being shown, else it has no
/// surface.
pub fn mask_row(
    selected: Option<Token>,
    hovered: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.palette();
        let pointer = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let background = match selected {
            Some(fill) => Some(palette.get(fill)),
            None if hovered || pointer => Some(palette.row_hover),
            None => None,
        };
        button::Style {
            background: background.map(Background::Color),
            text_color: if selected.is_some() {
                palette.text_current_row
            } else {
                palette.text_label
            },
            border: Border {
                radius: LIST_ROW_RADIUS.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            shadow: Shadow::default(),
            snap: false,
        }
        .with_disabled(theme, status)
    }
}

/// The mode control's track: the tab track rounded [`MODE_RADIUS`], at [`MODE_FIXED_OPACITY`]
/// when the mode is fixed.
pub fn mode_track(fixed: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let alpha = if fixed { MODE_FIXED_OPACITY } else { 1.0 };
        surface(Color {
            a: alpha,
            ..theme.palette().tab_track
        })
        .border(Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: MODE_RADIUS.into(),
        })
    }
}

/// One mode segment: raised on the selected tab fill when chosen (at [`MODE_FIXED_OPACITY`] when
/// fixed), the control surface under the pointer, else bare. Its glyph carries the mode's colour.
pub fn mode_segment(
    selected: bool,
    fixed: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.palette();
        let background = match (selected, status) {
            (true, _) => Some(Color {
                a: if fixed { MODE_FIXED_OPACITY } else { 1.0 },
                ..palette.tab_selected
            }),
            (false, button::Status::Hovered | button::Status::Pressed) => Some(palette.control),
            (false, _) => None,
        };
        button::Style {
            background: background.map(Background::Color),
            text_color: palette.text,
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

/// A dropdown menu's surface: the menu surface, outlined in the menu border, rounded
/// [`MENU_RADIUS`], over [`MENU_SHADOW`].
pub fn menu_surface(theme: &Theme) -> container::Style {
    surface(theme.palette().menu_surface)
        .border(Border {
            color: theme.palette().menu_border,
            width: BORDER_WIDTH,
            radius: MENU_RADIUS.into(),
        })
        .shadow(MENU_SHADOW)
}

/// A menu item: bare at rest, the menu item hover under the pointer.
pub fn menu_item(theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Some(Background::Color(theme.palette().menu_item_hover))
        }
        button::Status::Active | button::Status::Disabled => None,
    };
    button::Style {
        background,
        text_color: text_color_for(theme, status),
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
pub fn menu_separator(theme: &Theme) -> container::Style {
    surface(theme.palette().menu_separator)
}

/// A selected chip: the accent-tinted fill, borderless.
pub fn chip_selected(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(theme.palette().selected_fill)),
        text_color: theme.palette().accent,
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
    .with_disabled(theme, status)
}

/// A number field's value box: the Control surface, borderless, a press opens it for typing.
pub fn button_field(theme: &Theme, status: button::Status) -> button::Style {
    button_control(theme, status)
}

/// A readout card: the surround, rounded, borderless.
pub fn readout_surface(theme: &Theme) -> container::Style {
    surface(theme.palette().surround).border(Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: RADIUS.into(),
    })
}

trait DisabledStyle {
    fn with_disabled(self, theme: &Theme, status: button::Status) -> Self;
}

impl DisabledStyle for button::Style {
    fn with_disabled(self, theme: &Theme, status: button::Status) -> Self {
        match status {
            button::Status::Disabled => button::Style {
                text_color: theme.palette().text_tertiary,
                ..self
            },
            _ => self,
        }
    }
}

/// A row the command palette has just revealed: the revealed row's amber, rounded, for the moment
/// the mark lasts.
pub fn revealed_surface(theme: &Theme) -> container::Style {
    surface(theme.palette().revealed_row).border(Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: RADIUS.into(),
    })
}

/// A group header's hairline rule.
pub fn rule_surface(theme: &Theme) -> container::Style {
    surface(theme.palette().rule)
}

/// The 1 px border above a module band.
pub fn band_border_surface(theme: &Theme) -> container::Style {
    surface(theme.palette().band_border)
}

/// The slider's handle. The rail, its fill and its zero tick are drawn under it by the slider row
/// itself (see [`crate::geometry::rail_geometry`]), so Iced's own rail is transparent. The handle
/// turns the accent only while dragging, and then drops its dark ring, so the halo the rail line
/// draws under it (see [`THUMB_HALO_RADIUS`]) meets the accent directly, as the references draw it.
pub fn slider_style(dragging: bool) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |theme, status| {
        let palette = theme.palette();
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
                background: Background::Color(if active {
                    palette.accent
                } else {
                    palette.thumb
                }),
                border_color: if active {
                    Color::TRANSPARENT
                } else {
                    palette.thumb_outline
                },
                border_width: THUMB_OUTLINE_WIDTH,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Luxforge Dark's palette, whose values the visual language's tests pin.
    const DARK: Palette = Palette::luxforge_dark();

    #[test]
    fn surface_tokens_match_the_visual_language_table() {
        assert_eq!(CANVAS, Color::from_rgb8(0x19, 0x19, 0x1b));
        assert_eq!(CANVAS_BLACK, Color::from_rgb8(0x00, 0x00, 0x00));
        assert_eq!(CANVAS_GREY, Color::from_rgb8(0x77, 0x77, 0x77));
        assert_eq!(
            DARK.surround, CANVAS,
            "Luxforge Dark's surround is the dark canvas"
        );
        assert_eq!(
            DARK.rail_backdrop, DARK.background,
            "a rail lies over the panel"
        );
        assert_eq!(DARK.background, Color::from_rgb8(0x20, 0x20, 0x23));
        assert_eq!(DARK.surface, Color::from_rgb8(0x23, 0x23, 0x26));
        assert_eq!(DARK.control, Color::from_rgb8(0x2c, 0x2c, 0x31));
    }

    /// The canvas surface fills with whichever background it is given, and the grey one is the
    /// 18% grey it claims: L* 50.
    #[test]
    fn canvas_surface_fills_each_canvas_background() {
        let theme = Theme::luxforge_dark();
        for background in [CANVAS, CANVAS_BLACK, CANVAS_GREY] {
            let style = canvas_surface(background)(&theme);
            assert_eq!(style.background, Some(Background::Color(background)));
            assert_eq!(style.border, Border::default());
        }
        let linear = ((CANVAS_GREY.r + 0.055) / 1.055).powf(2.4);
        let lightness = 116.0 * linear.cbrt() - 16.0;
        assert!((linear - 0.18).abs() < 0.01, "{linear}");
        assert!((lightness - 50.0).abs() < 0.5, "{lightness}");
    }

    #[test]
    fn border_token_is_six_percent_white() {
        assert_eq!(DARK.border.r, 1.0);
        assert_eq!(DARK.border.g, 1.0);
        assert_eq!(DARK.border.b, 1.0);
        assert!((DARK.border.a - 0.06).abs() < f32::EPSILON);
    }

    #[test]
    fn text_tokens_match_the_visual_language_table() {
        assert_eq!(DARK.text, Color::from_rgb8(0xe8, 0xe8, 0xea));
        assert_eq!(DARK.text_secondary, Color::from_rgb8(0xa8, 0xa8, 0xae));
        assert_eq!(DARK.text_tertiary, Color::from_rgb8(0x77, 0x77, 0x7f));
    }

    #[test]
    fn accent_and_clipping_tokens_match_the_visual_language_table() {
        assert_eq!(DARK.accent, Color::from_rgb8(0xe2, 0xb4, 0x6a));
        assert_eq!(CLIPPING_HIGHLIGHT, Color::from_rgb8(0xe5, 0x53, 0x4b));
        assert_eq!(CLIPPING_SHADOW, Color::from_rgb8(0x4c, 0x8b, 0xe0));
    }

    #[test]
    fn the_guide_token_is_white_at_thirty_percent() {
        assert_eq!(
            (GUIDE.r, GUIDE.g, GUIDE.b),
            (DARK.border.r, DARK.border.g, DARK.border.b)
        );
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
        let style = slider_style(true)(&Theme::luxforge_dark(), slider::Status::Active);
        assert_eq!(style.handle.background, Background::Color(DARK.accent));
        assert_eq!(
            style.handle.border_color,
            Color::TRANSPARENT,
            "the halo, not the dark ring, surrounds a dragged handle"
        );
    }

    #[test]
    fn resting_handle_is_not_accent_and_iced_draws_no_rail() {
        let style = slider_style(false)(&Theme::luxforge_dark(), slider::Status::Active);
        assert_eq!(style.handle.background, Background::Color(DARK.thumb));
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
        assert_eq!(DARK.list_row_current, Color::from_rgb8(47, 47, 50));
    }

    #[test]
    fn rail_tokens_match_the_module_panel_references() {
        assert_eq!(DARK.rail, Color::from_rgb8(0x3a, 0x3a, 0x40));
        assert_eq!(DARK.rail_fill, Color::from_rgb8(0xa3, 0xa3, 0xaa));
        assert_eq!(DARK.thumb, Color::from_rgb8(0xec, 0xec, 0xee));
        assert_eq!(DARK.band_border, Color::from_rgb8(0x2f, 0x2f, 0x32));
        assert_eq!(DARK.rule, Color::from_rgb8(0x31, 0x31, 0x34));
        assert_eq!(DARK.zero_tick, Color::from_rgb8(0x5a, 0x5a, 0x62));
        assert_eq!(DARK.thumb_outline, Color::from_rgb8(0x11, 0x11, 0x13));
        assert_eq!(DARK.text_label, Color::from_rgb8(0xc9, 0xc9, 0xce));
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
            [DARK.rail_fill.r, DARK.rail_fill.g, DARK.rail_fill.b],
            [DARK.background.r, DARK.background.g, DARK.background.b],
            0.16,
        )
        .map(|channel| (channel * 255.0).round() as u8);
        assert_eq!(DARK.sparkline_area, Color::from_rgb8(r, g, b));
        assert_eq!(DARK.sparkline_area, Color::from_rgb8(0x35, 0x35, 0x39));
        assert_eq!(
            DARK.sparkline_area.a, 1.0,
            "opaque, not an alpha Iced would brighten"
        );
    }

    /// The shell's translucent fills are the default board's alphas composited over the Bar
    /// surface, stored opaque; its sizes are the board's.
    #[test]
    fn shell_tokens_match_the_default_board() {
        let over_bar = |colour: Color, alpha: f32| {
            let [r, g, b] = crate::geometry::over(
                [colour.r, colour.g, colour.b],
                [DARK.surface.r, DARK.surface.g, DARK.surface.b],
                alpha,
            )
            .map(|channel| (channel * 255.0).round() as u8);
            Color::from_rgb8(r, g, b)
        };
        assert_eq!(DARK.icon_hover, over_bar(Color::WHITE, 0.06));
        assert_eq!(DARK.strip_rule, over_bar(Color::WHITE, 0.10));
        // Sampled from the board's selected panel toggles, whose blue lands a code under the exact
        // composite.
        assert_eq!(DARK.icon_selected_fill, Color::from_rgb8(62, 55, 47));
        let composite = over_bar(DARK.accent, 0.14);
        for (sampled, exact) in [
            (DARK.icon_selected_fill.r, composite.r),
            (DARK.icon_selected_fill.g, composite.g),
            (DARK.icon_selected_fill.b, composite.b),
        ] {
            assert!((sampled - exact).abs() <= 1.0 / 255.0 + f32::EPSILON);
        }
        assert_eq!(DARK.text_identity, Color::from_rgb8(0x8a, 0x8a, 0x90));
        assert_eq!(DARK.text_current_row, Color::from_rgb8(0xf2, 0xf2, 0xf4));
        assert_eq!(AGENT_CONNECTED, Color::from_rgb8(0x57, 0xb5, 0x6b));
        assert_eq!(DARK.tab_track, Color::from_rgb8(0x28, 0x28, 0x2c));
        assert_eq!(DARK.tab_selected, Color::from_rgb8(0x3b, 0x3b, 0x41));
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
        let selected = button_selected(&Theme::luxforge_dark(), button::Status::Active);
        assert_eq!(
            selected.background,
            Some(Background::Color(DARK.icon_selected_fill))
        );
        assert_eq!(selected.text_color, DARK.accent);
        assert_eq!(selected.border.width, 0.0);
        let segment = segment(true)(&Theme::luxforge_dark(), button::Status::Active);
        assert_eq!(
            segment.background,
            Some(Background::Color(DARK.tab_selected))
        );
        assert_eq!(segment.text_color, DARK.text_bright);
        let resting = super::segment(false)(&Theme::luxforge_dark(), button::Status::Active);
        assert_eq!(resting.background, None);
        assert_eq!(resting.text_color, DARK.text_secondary);
        assert_eq!(
            button_icon(&Theme::luxforge_dark(), button::Status::Hovered).background,
            Some(Background::Color(DARK.icon_hover))
        );
        assert_eq!(
            button_icon(&Theme::luxforge_dark(), button::Status::Active).background,
            None
        );
    }

    #[test]
    fn faint_text_sits_between_the_panel_and_tertiary_text() {
        assert_eq!(DARK.text_faint, Color::from_rgb8(0x55, 0x55, 0x5c));
        for (panel, faint, tertiary) in [
            (DARK.background.r, DARK.text_faint.r, DARK.text_tertiary.r),
            (DARK.background.g, DARK.text_faint.g, DARK.text_tertiary.g),
            (DARK.background.b, DARK.text_faint.b, DARK.text_tertiary.b),
        ] {
            assert!(panel < faint && faint < tertiary);
        }
    }

    #[test]
    fn a_disclosure_heading_lifts_to_secondary_under_the_pointer() {
        let colour = |status| button_disclosure(&Theme::luxforge_dark(), status).text_color;
        assert_eq!(colour(button::Status::Active), DARK.text_tertiary);
        assert_eq!(colour(button::Status::Hovered), DARK.text_secondary);
        assert_eq!(colour(button::Status::Pressed), DARK.text_secondary);
        assert_eq!(
            button_disclosure(&Theme::luxforge_dark(), button::Status::Hovered).background,
            None
        );
        let theme = Theme::luxforge_dark();
        assert_eq!(disclosure_color(&theme, false), DARK.text_tertiary);
        assert_eq!(disclosure_color(&theme, true), DARK.text_secondary);
    }

    /// The white-balance rails, drawn at the colour-rail opacity over the panel, land on the
    /// colours sampled from basic.png at their start, middle and end.
    #[test]
    fn white_balance_rails_composite_to_the_basic_reference() {
        let drawn = |colour: Color| {
            let [r, g, b] = crate::geometry::over(
                [colour.r, colour.g, colour.b],
                [DARK.background.r, DARK.background.g, DARK.background.b],
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
        assert!(near(
            code(DARK.chrome_border),
            composite(white, DARK.surface, 0.08)
        ));
        assert!(near(
            code(DARK.strip_rule),
            composite(white, DARK.surface, 0.10)
        ));
        assert!(near(
            code(DARK.strip_selected),
            composite(DARK.accent, DARK.surface, 0.16)
        ));
        assert!(near(
            code(DARK.notice_warning_border),
            composite(DARK.accent, DARK.surface, 0.35)
        ));
        assert!(near(
            code(DARK.notice_error_border),
            composite(CLIPPING_HIGHLIGHT, DARK.surface, 0.40)
        ));
        assert!(near(
            code(DARK.histogram_border),
            composite(white, DARK.surround, 0.05)
        ));
        assert_eq!(code(DARK.strip_icon), [0xb9, 0xb9, 0xbf]);
        assert_eq!(code(DARK.text_bright), [0xf0, 0xf0, 0xf2]);
        assert_eq!(code(DARK.accent_ink), [0x1a, 0x14, 0x08]);
        assert_eq!(code(DARK.text_faint), [0x55, 0x55, 0x5c]);
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
            code(DARK.mask_row_selected),
            composite(DARK.accent, DARK.background, 0.12)
        ));
        assert!(near(
            code(DARK.list_row_current),
            composite(white, DARK.background, 0.07)
        ));
        assert!(near(
            code(DARK.row_hover),
            composite(white, DARK.background, 0.04)
        ));
        assert!(near(
            code(DARK.thumbnail_border),
            composite(white, THUMBNAIL_BACKGROUND, 0.08)
        ));
        assert!(near(
            code(DARK.menu_border),
            composite(white, DARK.menu_surface, 0.10)
        ));
        assert!(near(
            code(DARK.menu_separator),
            composite(white, DARK.menu_surface, 0.08)
        ));
        assert!(near(
            code(DARK.menu_item_hover),
            composite(white, DARK.menu_surface, 0.06)
        ));
        let fixed_segment = composite(DARK.tab_selected, DARK.background, MODE_FIXED_OPACITY);
        let fixed_segment = Color::from_rgb8(fixed_segment[0], fixed_segment[1], fixed_segment[2]);
        assert!(near(
            code(DARK.mode_fixed_ink),
            composite(DARK.text_bright, fixed_segment, MODE_FIXED_OPACITY)
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

    /// A light theme whose every token is its own colour, none of them Luxforge Dark's, so a
    /// style that drew the wrong token, or a constant, cannot pass for the right one.
    fn distinct() -> Theme {
        let mut palette = DARK;
        for (index, token) in Token::ALL.into_iter().enumerate() {
            let index = index as u8;
            *palette.token_mut(token.name()).unwrap() =
                Color::from_rgb8(10 + index, 200 - index, 3 * index);
        }
        Theme::new(palette, Mode::Light)
    }

    /// Every region the gallery's widgets draw takes its colour from its own token of the theme
    /// it is drawn in: each style function, under a theme of distinct colours, answers that
    /// theme's token for its surface, outline and ink.
    #[test]
    fn each_region_draws_in_its_own_token() {
        let theme = distinct();
        let p = *theme.palette();
        for token in Token::ALL {
            assert!(
                Token::ALL
                    .into_iter()
                    .filter(|other| p.get(*other) == p.get(token))
                    .count()
                    == 1,
                "{} is distinct",
                token.name()
            );
            assert_ne!(
                p.get(token),
                DARK.get(token),
                "{} is not Dark's",
                token.name()
            );
        }
        let fill = |style: container::Style| style.background;
        let colour = |c: Color| Some(Background::Color(c));
        let active = button::Status::Active;
        let hovered = button::Status::Hovered;
        let disabled = button::Status::Disabled;

        // Surfaces.
        assert_eq!(fill(panel_surface(&theme)), colour(p.background));
        assert_eq!(fill(title_bar_surface(&theme)), colour(p.surface));
        assert_eq!(fill(divider_surface(&theme)), colour(p.band_border));
        assert_eq!(fill(bar_surface(&theme)), colour(p.surface));
        assert_eq!(bar_surface(&theme).border.color, p.border);
        assert_eq!(fill(control_surface(&theme)), colour(p.control));
        assert_eq!(control_surface(&theme).border.color, p.border);
        assert_eq!(fill(scrim_surface(&theme)), colour(p.scrim));
        assert_eq!(fill(histogram_surface(&theme)), colour(p.surround));
        assert_eq!(histogram_surface(&theme).border.color, p.histogram_border);
        assert_eq!(fill(readout_surface(&theme)), colour(p.surround));
        assert_eq!(fill(revealed_surface(&theme)), colour(p.revealed_row));
        assert_eq!(fill(rule_surface(&theme)), colour(p.rule));
        assert_eq!(fill(band_border_surface(&theme)), colour(p.band_border));
        assert_eq!(fill(segment_track(&theme)), colour(p.tab_track));
        assert_eq!(fill(menu_surface(&theme)), colour(p.menu_surface));
        assert_eq!(menu_surface(&theme).border.color, p.menu_border);
        assert_eq!(fill(menu_separator(&theme)), colour(p.menu_separator));
        assert_eq!(fill(mode_track(false)(&theme)), colour(p.tab_track));
        for border in [
            Token::ChromeBorder,
            Token::StripRule,
            Token::NoticeWarningBorder,
            Token::NoticeErrorBorder,
        ] {
            let chrome = chrome_surface(&theme, border, CHROME_RADIUS);
            assert_eq!(fill(chrome), colour(p.surface));
            assert_eq!(chrome.border.color, p.get(border));
        }
        for token in Token::ALL {
            assert_eq!(fill(super::fill(token)(&theme)), colour(p.get(token)));
            assert_eq!(ink(token)(&theme).color, Some(p.get(token)));
        }
        // The fixed canvas backgrounds stay fixed under any theme.
        assert_eq!(fill(canvas_surface(CANVAS)(&theme)), colour(CANVAS));

        // Buttons.
        assert_eq!(button_plain(&theme, hovered).background, colour(p.control));
        assert_eq!(button_plain(&theme, active).text_color, p.text);
        assert_eq!(button_plain(&theme, disabled).text_color, p.text_tertiary);
        assert_eq!(button_accent(&theme, active).background, colour(p.accent));
        assert_eq!(button_accent(&theme, active).text_color, p.accent_ink);
        assert_eq!(
            button_accent(&theme, disabled).background,
            colour(p.control)
        );
        assert_eq!(
            button_icon(&theme, hovered).background,
            colour(p.icon_hover)
        );
        assert_eq!(
            button_selected(&theme, active).background,
            colour(p.icon_selected_fill)
        );
        assert_eq!(button_selected(&theme, active).text_color, p.accent);
        assert_eq!(swatch_open(&theme, active).border.color, p.accent);
        assert_eq!(button_band(&theme, active).background, colour(p.surface));
        assert_eq!(button_control(&theme, active).background, colour(p.control));
        assert_eq!(button_field(&theme, active).background, colour(p.control));
        assert_eq!(
            button_disclosure(&theme, active).text_color,
            p.text_tertiary
        );
        assert_eq!(
            button_disclosure(&theme, hovered).text_color,
            p.text_secondary
        );
        assert_eq!(
            list_row_current(&theme, active).background,
            colour(p.list_row_current)
        );
        assert_eq!(
            list_row_current(&theme, active).text_color,
            p.text_current_row
        );
        assert_eq!(
            chip_selected(&theme, active).background,
            colour(p.selected_fill)
        );
        assert_eq!(chip_selected(&theme, disabled).text_color, p.text_tertiary);
        assert_eq!(
            menu_item(&theme, hovered).background,
            colour(p.menu_item_hover)
        );
        assert_eq!(
            strip_tool(true)(&theme, active).background,
            colour(p.strip_selected)
        );
        assert_eq!(strip_tool(false)(&theme, active).text_color, p.strip_icon);
        assert_eq!(
            segment(true)(&theme, active).background,
            colour(p.tab_selected)
        );
        assert_eq!(segment(true)(&theme, active).text_color, p.text_bright);
        assert_eq!(segment(false)(&theme, active).text_color, p.text_secondary);
        assert_eq!(mode_segment(true, false)(&theme, active).text_color, p.text);
        assert_eq!(
            mode_segment(true, false)(&theme, active).background,
            colour(p.tab_selected)
        );
        assert_eq!(
            mask_row(Some(Token::MaskRowSelected), false)(&theme, active).background,
            colour(p.mask_row_selected)
        );
        assert_eq!(
            mask_row(None, true)(&theme, active).background,
            colour(p.row_hover)
        );
        assert_eq!(
            mask_row(None, false)(&theme, active).text_color,
            p.text_label
        );

        // Fields and the slider's handle.
        let focused = text_input::Status::Focused { is_hovered: false };
        let field = text_input_style(false)(&theme, text_input::Status::Active);
        assert_eq!(field.background, Background::Color(p.control));
        assert_eq!(field.border.color, p.border);
        assert_eq!(field.value, p.text);
        assert_eq!(field.placeholder, p.text_tertiary);
        assert_eq!(
            text_input_style(false)(&theme, focused).border.color,
            p.accent
        );
        assert_eq!(
            text_input_style(true)(&theme, focused).border.color,
            p.error
        );
        let handle = slider_style(false)(&theme, slider::Status::Active).handle;
        assert_eq!(handle.background, Background::Color(p.thumb));
        assert_eq!(handle.border_color, p.thumb_outline);
        let held = slider_style(true)(&theme, slider::Status::Active).handle;
        assert_eq!(held.background, Background::Color(p.accent));
    }
}
