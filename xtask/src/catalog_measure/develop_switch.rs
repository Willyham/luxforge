//! Switching photographs in Develop with a cached large preview: **lane D's hook**, filled once the
//! development-set filmstrip (TASK-021) is on the branch. Until then its row is `not_measured`,
//! naming it. It takes the desktop probes' context ([`DesktopContext`]) and returns its rows as
//! they do ([`super::desktop`]).
use super::{desktop::DesktopContext, report::Row};
use crate::*;

pub const METRIC: &str = "desktop.develop_switch.key_to_presented";
pub const TARGET: &str =
    "Switching photographs in Develop with a cached large preview: presented in the frame after the key";

/// The Develop switch's rows.
pub fn develop_switch(context: &DesktopContext) -> Result<Vec<Row>> {
    Ok(vec![
        Row::not_measured(
            METRIC,
            "frames",
            "lane D's Develop switch probe needs the development-set filmstrip (TASK-021); xtask/src/catalog_measure/develop_switch.rs is its hook",
        )
        .target(TARGET)
        .detail(context.record()),
    ])
}
