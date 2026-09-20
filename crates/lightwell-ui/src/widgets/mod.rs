//! Widget functions and their plain-data models.
//!
//! Every function here takes a model (plain data) and message values or closures, and returns an
//! `Element`. None stores state or validates input; the caller (the app's view layer) supplies
//! values already validated and formatted.

mod chip;
mod floating_bar;
mod icon_button;
mod inline_menu;
mod list_row;
mod mode_strip;
mod notice_card;
mod section_header;
mod segmented;
mod slider;
mod sub_group_header;
mod text;

pub use chip::{ChipModel, chip};
pub use floating_bar::floating_bar;
pub use icon_button::{IconButtonModel, icon_button};
pub use inline_menu::inline_menu;
pub use list_row::{ListRowModel, Marker, list_row};
pub use mode_strip::{ModeEntry, ToggleEntry, mode_strip};
pub use notice_card::{NoticeCardModel, Tone, notice_card};
pub use section_header::{SectionHeaderModel, section_header};
pub use segmented::{SegmentedModel, segmented};
pub use slider::{SliderModel, ValueEdit, slider};
pub use sub_group_header::{SubGroupHeaderModel, sub_group_header};
pub use text::{caption, error_caption, label, section_label, title, value_text};
