//! The history labels of the Corrections actions (`docs/design/corrections.md`, "History
//! labels"): an operation is named by its kind's title and its number, an update by the operation's
//! kind before the change and the one field it changed.
use super::{
    actions::addressed,
    module::{ACCEPT, ADD, DELETE, REORDER, RESET, UPDATE, repair_layer},
    payload::OperationKind,
};
use crate::{ActionInput, Layer};
use serde_json::Value;

/// The label of `input`, planned on `before` into `after`, or `None` to take the action's title.
pub(super) fn label(input: &ActionInput, before: &[Layer], after: &[Layer]) -> Option<String> {
    let parameters = &input.parameters;
    match input.action_id.as_str() {
        ADD => {
            let (_, _, payload) = repair_layer(after).ok()??;
            let kind = parameters
                .get("mode")
                .and_then(Value::as_str)
                .and_then(OperationKind::parse)
                .unwrap_or(OperationKind::Heal);
            Some(format!("Add {} {}", kind.title(), payload.next - 1))
        }
        RESET => Some("Reset corrections".into()),
        ACCEPT => {
            let (_, _, payload) = repair_layer(after).ok()??;
            let (_, operation) = payload.find(u32::try_from(payload.next - 1).ok()?).ok()?;
            Some(format!("Add {}", operation.name()))
        }
        UPDATE | DELETE | REORDER => {
            let (_, _, payload) = repair_layer(before).ok()??;
            let (_, operation) = payload.find(addressed(parameters).ok()?).ok()?;
            let name = operation.name();
            Some(match input.action_id.as_str() {
                DELETE => format!("Delete {name}"),
                REORDER => format!("Reorder {name}"),
                _ => update_label(input, &name),
            })
        }
        _ => None,
    }
}

/// An update's label, by the fields it sent besides the operation.
fn update_label(input: &ActionInput, name: &str) -> String {
    let parameters = &input.parameters;
    let mut sent: Vec<&str> = parameters
        .keys()
        .map(String::as_str)
        .filter(|field| *field != super::module::OPERATION)
        .collect();
    sent.sort_unstable();
    let value = |field: &str| parameters.get(field).map(crate::modules::label_value);
    match sent.as_slice() {
        ["points"] => format!("Move {name}"),
        ["source-x", "source-y"] => format!("Move {name} source"),
        ["source-origin", "source-x", "source-y"] => {
            match parameters.get("source-origin").and_then(Value::as_str) {
                Some("auto") => format!("New source {name}"),
                _ => format!("Move {name} source"),
            }
        }
        ["radius"] => format!("Resize {name}"),
        ["opacity"] => format!("{name} · Opacity {}", value("opacity").unwrap_or_default()),
        ["feather"] => format!("{name} · Feather {}", value("feather").unwrap_or_default()),
        ["mode"] => format!("Change {name} to {}", value("mode").unwrap_or_default()),
        _ => format!("Edit {name}"),
    }
}
