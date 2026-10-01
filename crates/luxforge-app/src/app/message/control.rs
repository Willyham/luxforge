//! Generated controls.
use crate::app::controls::CurveSampleIdentity;
use luxforge_ui::{ColorPickerEvent, CurveEditorEvent};
use serde_json::Value;

/// A generated control or a tools-panel section changed. Handled in `app/controls.rs`.
#[derive(Clone, Debug)]
pub(crate) enum ControlMessage {
    QueryChoiceSearch {
        action: String,
        text: String,
    },
    QueryChoicePage {
        action: String,
        page: u32,
    },
    QueryChoiceRetry {
        action: String,
    },
    QueryChoiceShared {
        action: String,
        parameter: String,
        text: String,
    },
    QueryChoiceSelect {
        action: String,
        key: String,
    },
    QueryChoiceAnswered {
        identity: crate::state::query_choice::QueryChoiceIdentity,
        result: Result<Value, String>,
    },
    /// The suggestion card's Apply.
    QueryChoiceApply {
        action: String,
    },
    /// Open or close Change, which reveals the search under a card.
    QueryChoiceChange {
        action: String,
        open: bool,
    },
    /// Open the answer's report page in the default browser.
    QueryChoiceReport {
        action: String,
    },
    /// The platform's answer to opening a report page.
    QueryChoiceReportOpened {
        result: Result<(), String>,
    },
    /// A generated field changed: the text the user typed for one declared parameter.
    Field {
        action: String,
        parameter: String,
        text: String,
    },
    /// Enter in a generated field runs that field's action when it is runnable. `parameter` names
    /// the field the key was pressed in, which is the only field a patch action submits.
    Submit {
        action: String,
        parameter: Option<String>,
    },
    /// A slider rail position in 0..=1; the host maps it through the descriptor's soft range.
    Fraction {
        action: String,
        parameter: String,
        fraction: f64,
    },
    /// A discrete control sends one field of its declared action once.
    Discrete {
        action: String,
        parameter: String,
        value: Value,
    },
    /// The end of a continuous control gesture: it commits the open draft of the control drafting,
    /// and a control that does not draft runs its action once, exactly as Enter in the field does.
    Released {
        action: String,
        parameter: String,
    },
    /// One explicit stepper button press or keyboard step.
    Step {
        action: String,
        parameter: String,
        direction: i8,
    },
    KeyNudge {
        action: String,
        parameter: String,
        direction: i8,
        shift: bool,
        option: bool,
    },
    FieldNudge {
        action: String,
        parameter: String,
        direction: i8,
        shift: bool,
        option: bool,
    },
    TogglePicker {
        action: String,
        parameter: String,
    },
    ToggleGroup {
        module_id: String,
        path: Vec<usize>,
    },
    /// Selects a tab in a module whose descriptor declares `layout: tabs`. Per-client view state
    /// exactly like `ToggleGroup`: it changes no recipe and sends no request.
    SelectTab {
        module_id: String,
        index: usize,
    },
    Picker {
        action: String,
        parameter: String,
        event: ColorPickerEvent,
    },
    Curve {
        action: String,
        parameter: String,
        event: CurveEditorEvent,
    },
    /// Sampled curve geometry from the module's declared read-only query.
    CurveSampled {
        identity: CurveSampleIdentity,
        result: Result<Value, String>,
    },
    /// Return one generated field to its declared default. On a patch action that is one action
    /// submitting that field alone; otherwise it only refills the text, as it always has.
    ResetField {
        action: String,
        parameter: String,
    },
    /// A value is being typed, so the field shows the text rather than the formatted value.
    EditValue {
        action: String,
        parameter: String,
    },
    /// Collapse or expand one module's section.
    ToggleSection(String),
    /// Return one module to its neutral state through its declared reset action.
    ResetModule(String),
    /// Return one control group to its neutral values through the group's declared reset action.
    ResetGroup {
        module_id: String,
        path: Vec<usize>,
    },
}

impl ControlMessage {
    /// The one declared field a message names, as `(action, parameter)`: every message a number,
    /// colour or curve control sends. Section, group and tab messages name none.
    pub(crate) fn field(&self) -> Option<(&str, &str)> {
        match self {
            Self::Field {
                action, parameter, ..
            }
            | Self::Fraction {
                action, parameter, ..
            }
            | Self::Discrete {
                action, parameter, ..
            }
            | Self::Released { action, parameter }
            | Self::Step {
                action, parameter, ..
            }
            | Self::KeyNudge {
                action, parameter, ..
            }
            | Self::FieldNudge {
                action, parameter, ..
            }
            | Self::TogglePicker { action, parameter }
            | Self::Picker {
                action, parameter, ..
            }
            | Self::Curve {
                action, parameter, ..
            }
            | Self::ResetField { action, parameter }
            | Self::EditValue { action, parameter } => Some((action, parameter)),
            Self::Submit {
                action,
                parameter: Some(parameter),
            } => Some((action, parameter)),
            Self::Submit {
                parameter: None, ..
            }
            | Self::ToggleGroup { .. }
            | Self::SelectTab { .. }
            | Self::CurveSampled { .. }
            | Self::QueryChoiceSearch { .. }
            | Self::QueryChoicePage { .. }
            | Self::QueryChoiceRetry { .. }
            | Self::QueryChoiceShared { .. }
            | Self::QueryChoiceSelect { .. }
            | Self::QueryChoiceAnswered { .. }
            | Self::QueryChoiceApply { .. }
            | Self::QueryChoiceChange { .. }
            | Self::QueryChoiceReport { .. }
            | Self::QueryChoiceReportOpened { .. }
            | Self::ToggleSection(_)
            | Self::ResetModule(_)
            | Self::ResetGroup { .. } => None,
        }
    }
}
