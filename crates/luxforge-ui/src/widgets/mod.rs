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
mod field_grid;
mod floating_bar;
mod focus_control;
mod histogram;
mod icon_button;
mod inline_menu;
mod job_row;
mod list_row;
mod mask_row;
mod menu_choice;
mod metric_row;
mod mode_control;
mod mode_strip;
mod notice_card;
mod number_field;
mod overlay_control;
mod popover;
mod range_slider;
mod readout_card;
mod section_header;
mod segmented;
mod slider;
mod slider_guard;
mod sparkline;
mod stepper;
mod sub_group_header;
mod swatch_slots;
mod tab_row;
mod text;
mod toggle;
mod truncated_text;

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
pub use dropdown::{DropdownButtonModel, MenuEntry, MenuItem, dropdown_button, menu_list};
pub use field_grid::{GridField, field_grid};
pub use floating_bar::{
    DraftBarModel, DraftFinish, DraftSubject, draft_bar, draft_bar_with_controls,
};
pub use focus_control::{ControlKey, ControlKeyEvent, focus_control};
pub use histogram::{
    BINS, ClipTriangleModel, HistogramChannel, HistogramModel, histogram_inspector,
};
pub use icon_button::{
    Icon, IconButtonModel, header_icon_button, icon_button, title_bar_icon_button, with_tooltip,
};
pub use inline_menu::inline_menu;
pub use job_row::{JobRowModel, job_row};
pub use list_row::{ListRowModel, Marker, list_heading, list_row, panel_heading};
pub use mask_row::{
    ComponentRowMessages, ComponentRowModel, DropEdge, MaskRowMessages, MaskRowModel,
    RenameMessages, StrokeRowModel, component_note, component_row, drop_feedback, mask_row,
    rename_input_id, stroke_row,
};
pub use menu_choice::{MenuChoiceModel, menu_choice};
pub use metric_row::{MetricRowModel, metric_row};
pub use mode_control::{CombineMode, ModeControlModel, mode_control};
pub use mode_strip::{ModeEntry, ToggleEntry, mode_strip};
pub use notice_card::{NoticeCardModel, Tone, notice_card};
pub use number_field::{
    NumberFieldModel, ValueEdit, boxed_input, channel_row, compact_number_field, label_line,
    number_field, value_input,
};
pub use overlay_control::{OverlayControlModel, OverlayMode, OverlayTint, overlay_control};
pub use popover::popover;
pub use range_slider::{RangeGrip, RangeSliderModel, RangeValues, range_slider};
pub use readout_card::readout_card;
pub use section_header::{SectionHeaderModel, band_header, module_section};
pub use segmented::{SegmentedModel, segment, segment_track, segmented};
pub use slider::{RailDecoration, SliderModel, slider};
pub use sparkline::SparklineModel;
pub use stepper::{StepperModel, StepperRail, StepperRailMessages, stepper};
pub use sub_group_header::{
    GroupRuleModel, SubGroupHeaderModel, group_rule, sub_group_header,
    sub_group_header_with_actions,
};
pub use swatch_slots::{SwatchSlotsModel, swatch_slots};
pub use tab_row::{Tab, TabRowModel, tab_row};
pub use text::{caption, error_caption, label, section_label, title};
pub use toggle::{ToggleModel, compact_toggle, toggle};
pub use truncated_text::truncated_text;

// Used only inside the crate: by its composed widgets and the components board.
pub(crate) use coverage_thumbnail::coverage_thumbnail;
pub(crate) use double_click::double_click;
pub(crate) use floating_bar::floating_bar;
pub(crate) use histogram::clip_triangle;
pub(crate) use icon_button::icon;
pub(crate) use popover::POPOVER_GAP;
pub(crate) use section_header::section_header;
pub(crate) use text::value_text;
