//! Widget functions and their plain-data models.
//!
//! Every function here takes a model (plain data) and message values or closures, and returns an
//! `Element`. None stores state or validates input; the caller (the app's view layer) supplies
//! values already validated and formatted.

mod button_row;
mod chip;
mod color_picker;
mod color_swatch;
mod curve_editor;
mod double_click;
mod floating_bar;
mod focus_control;
mod histogram;
mod icon_button;
mod inline_menu;
mod list_row;
mod menu_choice;
mod mode_strip;
mod notice_card;
mod number_field;
mod readout_card;
mod section_header;
mod segmented;
mod slider;
mod slider_guard;
mod stepper;
mod sub_group_header;
mod tab_row;
mod text;
mod toggle;
mod truncated_text;

pub use button_row::{
    ButtonSize, ButtonTone, LabelledButtonModel, RowPlacement, button_row, button_row_height,
    equal_button_row, icon_button_row, labelled_button, row_icon_button, text_button,
};
pub use chip::{ChipModel, chip, chip_row, chip_wrap};
pub use color_picker::{
    ColorPickerEvent, ColorPickerModel, color_picker, hex_to_rgb, hsv_to_rgb, hue_fraction,
    plane_fraction, rgb_to_hex, rgb_to_hsv,
};
pub use color_swatch::{ColorSwatchModel, color_swatch};
pub use curve_editor::{
    CurveEditorEvent, CurveEditorModel, CurvePointRow, POINT_HIT_RADIUS, curve_editor, hit_test,
    point_fraction, round_fraction,
};
pub use double_click::double_click;
pub use floating_bar::floating_bar;
pub use focus_control::{ControlKey, ControlKeyEvent, focus_control};
pub use histogram::{
    BINS, ClipTriangleModel, HistogramChannel, HistogramModel, bin_x, clip_triangle, histogram,
    polygon_points,
};
pub use icon_button::{Icon, IconButtonModel, header_icon_button, icon, icon_button};
pub use inline_menu::inline_menu;
pub use list_row::{ListRowModel, Marker, list_heading, list_row};
pub use menu_choice::{MenuChoiceModel, menu_choice};
pub use mode_strip::{ModeEntry, ToggleEntry, mode_strip};
pub use notice_card::{NoticeCardModel, Tone, notice_card};
pub use number_field::{
    NumberFieldModel, ValueEdit, boxed_input, channel_row, label_line, number_field, value_input,
};
pub use readout_card::{readout_card, readout_card_height};
pub use section_header::{
    SectionHeaderModel, collapsed_section_height, expanded_section_height, module_section,
    section_body, section_header,
};
pub use segmented::{SegmentedModel, segmented};
pub use slider::{RailDecoration, SliderModel, slider};
pub use stepper::{StepperModel, stepper};
pub use sub_group_header::{
    SubGroupHeaderModel, sub_group_header, sub_group_header_height, sub_group_header_with_actions,
};
pub use tab_row::{Tab, TabRowModel, tab_row, tab_row_height};
pub use text::{
    caption, control_label, error_caption, group_label, label, section_label, title, value_text,
};
pub use toggle::{ToggleModel, knob_center, toggle};
pub use truncated_text::{ELLIPSIS, Fit, TruncatedText, fit_one_line, truncated_text};
