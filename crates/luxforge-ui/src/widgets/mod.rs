//! Widget functions and their plain-data models.
//!
//! Every function here takes a model (plain data) and message values or closures, and returns an
//! `Element`. None stores state or validates input; the caller (the app's view layer) supplies
//! values already validated and formatted.

mod badge;
mod button_row;
mod chip;
mod color_picker;
mod color_swatch;
mod coverage_thumbnail;
mod curve_editor;
mod decorator;
mod disclosure_heading;
mod double_click;
mod dropdown;
mod equal_row;
mod field_grid;
mod floating_bar;
mod focus_control;
mod help_sheet;
mod histogram;
mod hover_panel;
mod icon_button;
mod inline_menu;
mod inline_notice;
mod job_row;
mod key_hint;
mod list_row;
mod mask_row;
mod menu_choice;
mod metric_row;
mod mode_control;
mod mode_strip;
mod notched_slider;
mod notice_card;
mod number_field;
mod overlay_control;
mod pick_bar;
mod popover;
mod range_slider;
mod readout_card;
mod section_header;
mod segmented;
mod setup_sheet;
mod slider;
mod slider_guard;
mod sparkline;
mod steady_scroll;
mod stepper;
mod sub_group_header;
mod swatch_slots;
mod tab_row;
mod text;
mod toggle;
mod truncated_text;
mod wheel;

// -- Select: the thumbnail grid.
mod thumbnail_grid;
// -- end Select: the thumbnail grid.

// -- Select: chrome.
mod filmstrip;
mod filter_bar;
mod long_work;
mod loupe;
mod select_strip;
mod source_list;
mod title_actions;
// -- end Select: chrome.

// -- Select: resolving missing originals.
mod resolve;
// -- end Select: resolving missing originals.

// -- Select: the catalog.
mod catalog;
// -- end Select: the catalog.

pub use badge::{BadgeModel, badge};
pub use button_row::{
    ButtonSize, ButtonTone, LabelledButtonModel, RowPlacement, button_row, equal_button_row,
    icon_button_row, labelled_button, row_icon_button, text_button,
};
pub use chip::{ChipModel, chip, chip_row, chip_wrap, compact_chip};
pub use color_picker::{
    ColorPickerEvent, ColorPickerModel, color_picker, hex_to_rgb, hsv_to_rgb, rgb_to_hex,
    rgb_to_hsv,
};
pub use color_swatch::{ColorSwatchModel, color_swatch};
pub use coverage_thumbnail::CoverageThumbnailModel;
pub use curve_editor::{CurveEditorEvent, CurveEditorModel, CurvePointRow, curve_editor};
pub use disclosure_heading::disclosure_heading;
pub use dropdown::{
    DropdownButtonModel, MenuEntry, MenuItem, dropdown_button, menu_list, menu_list_with_width,
};
pub use equal_row::{EqualRow, equal_row};
pub use field_grid::{GridField, field_grid};
pub use floating_bar::{
    DraftBarModel, DraftFinish, DraftSubject, draft_bar, draft_bar_with_controls,
};
pub use focus_control::{ControlKey, ControlKeyEvent, focus_control};
pub use help_sheet::{
    HelpAlike, HelpRemedy, HelpSheetModel, HelpSheetStep, HintPart, LeadPart, PatchGesture,
    PatchHolds, StepDrawing, help_sheet,
};
pub use histogram::{
    BINS, ClipTriangleModel, HistogramChannel, HistogramModel, histogram_inspector,
};
pub use hover_panel::hover_panel;
pub use icon_button::{
    Icon, IconButtonModel, header_icon_button, icon, icon_button, title_bar_icon_button,
    with_tooltip,
};
pub use inline_menu::inline_menu;
pub use inline_notice::inline_notice;
pub use job_row::{JobRowModel, job_row};
pub use key_hint::{KeyHint, key_cap, key_hints};
pub use list_row::{ListRowModel, Marker, list_heading, list_row, panel_heading};
pub use mask_row::{
    ComponentRowMessages, ComponentRowModel, CorrectionRowMessages, CorrectionRowModel, DropEdge,
    MaskRowMessages, MaskRowModel, RenameMessages, StrokeRowModel, component_note, component_row,
    correction_row, drop_feedback, mask_row, rename_input_id, stroke_row,
};
pub use menu_choice::{MenuChoiceModel, menu_choice};
pub use metric_row::{MetricRowModel, metric_row};
pub use mode_control::{CombineMode, ModeControlModel, mode_control, mode_menu};
pub use mode_strip::{ModeEntry, ToggleEntry, mode_strip};
pub use notched_slider::{NotchedSliderModel, StepKeys, arrow_step, notched_panel, notched_slider};
pub use notice_card::{NoticeCardModel, Tone, notice_card, notice_card_with};
pub use number_field::{
    NumberFieldModel, ValueEdit, boxed_input, channel_row, inline_value, label_line, number_field,
    value_input, values_line,
};
pub use overlay_control::{OverlayControlModel, OverlayMode, OverlayTint, overlay_control};
pub use pick_bar::{
    PickBarModel, PickButtonModel, PickLineModel, pick_bar, pick_button, pick_line,
};
pub use popover::popover;
pub use range_slider::{RangeGrip, RangeSliderModel, RangeValues, range_rail, range_slider};
pub use readout_card::readout_card;
pub use section_header::{SectionHeaderModel, band_header, band_header_with_reset, module_section};
pub use segmented::{
    SegmentedModel, chevron_segment, segment, segment_track, segmented, segmented_fill,
};
pub use setup_sheet::{
    FactTone, SetupFactModel, SetupPicture, SetupSheetModel, SetupStepModel, setup_sheet,
};
pub use slider::{RailDecoration, SliderModel, slider};
pub use sparkline::SparklineModel;
pub use steady_scroll::steady_bottom;
pub use stepper::{StepperModel, StepperRail, StepperRailMessages, stepper};
pub use sub_group_header::{
    GroupRuleModel, SubGroupHeaderModel, group_rule, sub_group_header,
    sub_group_header_with_actions, sub_group_header_with_controls,
};
pub use swatch_slots::{SwatchSlotsModel, swatch_slots};
pub use tab_row::{Tab, TabRowModel, tab_row};
pub use text::{caption, error_caption, label, section_label, title};
pub use toggle::{ToggleModel, compact_toggle, switch, toggle};
pub use truncated_text::truncated_text;
pub use wheel::{
    COMPACT_DIAMETER, Grab, LARGE_DIAMETER, WheelEvent, WheelModel, WheelModifiers, hue_at,
    point_at, wheel,
};

// -- Select: the thumbnail grid's exports.
pub(crate) use thumbnail_grid::LineKind as GridLineKind;
#[cfg(test)]
pub(crate) use thumbnail_grid::paint_for_tests;
pub use thumbnail_grid::{
    CellAvailability, CellStack, CellView, GridBlock, GridCell, GridContext, GridDirection,
    GridHeading, GridHit, GridLayout, GridMetrics, GridPress, HeaderAction, MomentHeader,
    MomentKind, PendingRow, PressModifiers, ThumbnailGrid, thumbnail_grid,
};
// -- end Select: the thumbnail grid's exports.

// -- Select: chrome's exports.
pub use filmstrip::{FilmstripModel, FilmstripPress, filmstrip, filmstrip_capacity};
pub use filter_bar::{
    ChipEnd, FilterChipModel, FilterOption, FilterSegmentsModel, SearchFieldModel, filter_action,
    filter_bar, filter_chip, filter_segments, search_field,
};
pub use job_row::{WorkRowModel, work_row};
pub use long_work::{ProgressSheetModel, StatusJobModel, WorkProgress, progress_sheet, status_job};
pub use loupe::{
    FocusInsetModel, FrameStripModel, InsetRegion, InsetSource, LoupeInfoModel, MomentFrame,
    focus_box, focus_inset, frame_strip, loupe_info_bar, region_box, visible_window,
};
pub use segmented::{filter_segment, filter_segment_track, keyed_segment};
pub use select_strip::{STRIP_SLIDER_WIDTH, SelectStripModel, select_strip};
pub use source_list::{
    SourceCount, SourceHeadingModel, SourceRowModel, Volume, source_heading, source_month,
    source_row, source_tag,
};
pub use title_actions::{DevelopButtonModel, WorkspaceTab, develop_button, workspace_switch};
// -- end Select: chrome's exports.

// -- Select: resolving missing originals' exports.
pub use resolve::{
    ResolveBarModel, ResolveGroupModel, ResolveResultModel, ResolveRowModel, ResolveTone,
    resolve_action, resolve_bar, resolve_group, resolve_row,
};
// -- end Select: resolving missing originals' exports.

// -- Select: the catalog's exports.
pub use catalog::{
    CatalogSheetModel, FacetColumnModel, FacetRowModel, OrganizeChipModel, OrganizeTone,
    SheetSectionModel, catalog_sheet, facet_column, metadata_browser, organize_chip,
    organize_chips,
};
// -- end Select: the catalog's exports.

// Used only inside the crate: by its composed widgets and the components board.
pub(crate) use coverage_thumbnail::coverage_thumbnail;
pub(crate) use double_click::double_click;
pub(crate) use floating_bar::floating_bar;
pub(crate) use histogram::clip_triangle;
pub(crate) use popover::POPOVER_GAP;
pub(crate) use section_header::section_header;
pub(crate) use text::value_text;
