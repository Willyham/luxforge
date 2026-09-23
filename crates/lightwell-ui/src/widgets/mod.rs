//! Widget functions and their plain-data models.
//!
//! Every function here takes a model (plain data) and message values or closures, and returns an
//! `Element`. None stores state or validates input; the caller (the app's view layer) supplies
//! values already validated and formatted.

mod badge;
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
mod section_header;
mod segmented;
mod slider;
mod slider_guard;
mod stepper;
mod sub_group_header;
mod text;
mod toggle;

pub use badge::{BadgeModel, badge};
pub use chip::{ChipModel, chip};
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
pub use icon_button::{Icon, IconButtonModel, icon, icon_button};
pub use inline_menu::inline_menu;
pub use list_row::{ListRowModel, Marker, list_row};
pub use menu_choice::{MenuChoiceModel, menu_choice};
pub use mode_strip::{ModeEntry, ToggleEntry, mode_strip};
pub use notice_card::{NoticeCardModel, Tone, notice_card};
pub use number_field::{NumberFieldModel, ValueEdit, number_field, value_input};
pub use section_header::{SectionHeaderModel, section_header};
pub use segmented::{SegmentedModel, segmented};
pub use slider::{RailDecoration, SliderModel, slider};
pub use stepper::{StepperModel, stepper};
pub use sub_group_header::{SubGroupHeaderModel, sub_group_header};
pub use text::{caption, error_caption, label, section_label, title, value_text};
pub use toggle::{ToggleModel, toggle};
