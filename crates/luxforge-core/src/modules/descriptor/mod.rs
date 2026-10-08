//! Plain-data module descriptors: one serializable source for API discovery, generated controls
//! and every validation limit a module declares.
//!
//! `types` holds the shapes a module declares and a client reads, the control variants and their
//! resolution among them; `validate` the rules a descriptor is registered against; `values` the
//! checks a request's values meet against their declarations, the settings check among them; and
//! `labels` how a label or a refusal names a value.
mod labels;
#[cfg(test)]
mod labels_tests;
#[cfg(test)]
mod testing;
mod types;
#[cfg(test)]
mod types_tests;
mod validate;
#[cfg(test)]
mod validate_tests;
mod values;
#[cfg(test)]
mod values_tests;

pub(crate) use labels::{label_value, not_applicable, title_case};
pub use types::{
    ActionControl, ActionDescriptor, ActionStyle, AnalysisAction, Availability, CanvasInteraction,
    ChoiceStyle, ColorStyle, Control, CurveBackground, CurveChannel, CurveControl,
    EffectDescriptor, EffectStage, GroupControl, ModuleDescriptor, ModuleLayout, NumberControl,
    NumberStyle, ParameterDescriptor, ParameterKind, PickerControl, PresetsControl,
    QueryChoiceControl, RailDecoration, ResetAction, resolve_control, resolve_group_reset,
};
pub use types::{
    ChoiceControl, ColorControl, ControlVariant, IdentityKind, RangeControl, ResolvedControl,
    ResolvedReset, TaskControl, ToggleControl,
};
pub(crate) use types::{MAX_SECRET_LENGTH, PRESET_SETTINGS, SETTINGS_ORIGIN};
pub(crate) use validate::{
    check_declaration, check_parameter_declarations, valid_identity, valid_name,
};
pub(crate) use values::{
    MAX_SETTINGS_ACTIONS, MAX_SETTINGS_FIELDS, check_declared_values, check_settings, check_target,
    decode_parameters, take_parameters,
};
pub use values::{check_parameters, check_value};
