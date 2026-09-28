//! Control variants across modules: the checks only a complete registry can make, the one control
//! resolver the host shares with every client, and the superseded fields both derive.
//!
//! A variant is declared by the module whose control it stands in for (Basic's Temperature names
//! the RAW module's `set-raw.temperature`), so a module's descriptor validates on its own and the
//! variants are checked once every module is registered ([`ModuleRegistry::check_complete`]).
//! Everything here walks descriptors — `O(controls)` — and reads no stack; nothing runs per frame.
use super::ModuleRegistry;
use crate::{
    Error, ErrorKind, MaskId, SourceTag,
    modules::{
        Control, GroupControl, ModuleDescriptor, NumberControl, ResolvedControl, ResolvedReset,
        resolve_control,
    },
};

/// A field one control variant supersedes: parameter `parameter` of action `action`, whose number
/// control `label` has a variant for `source` naming parameter `by_parameter` of action `by_action`
/// of module `module`. On the global target of a photo of `source` the field is refused and its
/// variant is the one path ([`ModuleRegistry::superseded`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Superseded<'r> {
    pub source: SourceTag,
    pub action: &'r str,
    pub parameter: &'r str,
    /// The base control's label, which the refusal names.
    pub label: &'r str,
    /// The module whose control the variant is.
    pub module: &'r str,
    pub by_action: &'r str,
    pub by_parameter: &'r str,
}

impl Superseded<'_> {
    /// `set-raw.temperature`: the variant's field as `schema.list` names it.
    pub fn by(&self) -> String {
        format!("{}.{}", self.by_action, self.by_parameter)
    }
}

/// Every control of `controls`, groups included, depth first in declaration order.
fn walk<'d>(controls: &'d [Control], visit: &mut impl FnMut(&'d Control)) {
    for control in controls {
        visit(control);
        if let Control::Group(GroupControl { controls, .. }) = control {
            walk(controls, visit);
        }
    }
}

impl ModuleRegistry {
    /// The checks a module's own descriptor cannot make, run once every module is registered:
    ///
    /// - every control variant names another registered module that applies to the variant's
    ///   source kind, and its replacement is a control of that module over its own actions or pick
    ///   canvas (or, on a group, a reset of that module), validated as that module's own control;
    /// - every module whose pick canvas none of its own controls reaches is reached by another
    ///   module's picker variant, so every pick mode is still entered from the panel.
    ///
    /// [`ModuleRegistry::builtin`] and every editor service ([`crate::EditorService::open_with`])
    /// run it, so a registry a client serves has passed it. `O(controls)`.
    pub fn check_complete(&self) -> Result<(), Error> {
        let descriptors = self.descriptors();
        let mut reached: Vec<&str> = Vec::new();
        for owner in &descriptors {
            let mut outcome = Ok(());
            walk(&owner.controls, &mut |control| {
                if outcome.is_err() {
                    return;
                }
                for variant in control.variants() {
                    outcome = self.check_variant(owner, control, variant);
                    if outcome.is_err() {
                        return;
                    }
                    if matches!(control, Control::Picker(_)) {
                        reached.push(variant.module.as_str());
                    }
                }
            });
            outcome?;
        }
        for descriptor in &descriptors {
            if descriptor.needs_foreign_picker() && !reached.contains(&descriptor.id.as_str()) {
                return Err(Error::validation(format!(
                    "module {} declares a pick canvas but no picker control reaches it",
                    descriptor.id
                )));
            }
        }
        Ok(())
    }

    fn check_variant(
        &self,
        owner: &ModuleDescriptor,
        control: &Control,
        variant: &crate::modules::ControlVariant,
    ) -> Result<(), Error> {
        let source = variant.source.label();
        let kind = control.kind_name();
        let other = self
            .module(&variant.module)
            .ok_or_else(|| {
                Error::validation(format!(
                    "{source} variant of a {kind} control of module {} names unregistered module {}",
                    owner.id, variant.module
                ))
            })?
            .descriptor();
        if !other.applies_to(variant.source) {
            return Err(Error::validation(format!(
                "{source} variant of a {kind} control of module {} names module {}, which does \
                 not apply to a {source} photo",
                owner.id, other.id
            )));
        }
        let checked = match (&variant.control, &variant.reset) {
            (Some(replacement), _) => other.check_control(replacement, 1),
            (None, reset) => other.check_reset(reset.as_ref()),
        };
        checked.map_err(|error| {
            Error::validation(format!(
                "{source} variant of a {kind} control of module {}: {}",
                owner.id, error.detail
            ))
        })
    }

    /// [`resolve_control`] for a control `owner` declares: the one rule every client resolves a
    /// control through, here for a caller that holds the registry. A client that holds only the
    /// descriptors `module.list` returned calls [`resolve_control`] itself with the same answer.
    pub fn resolve_control<'d>(
        &self,
        owner: &'d ModuleDescriptor,
        control: &'d Control,
        kind: Option<SourceTag>,
        mask: Option<&MaskId>,
    ) -> ResolvedControl<'d> {
        resolve_control(&owner.id, control, kind, mask)
    }

    /// [`crate::modules::resolve_group_reset`] for a group `owner` declares.
    pub fn resolve_group_reset<'d>(
        &self,
        owner: &'d ModuleDescriptor,
        group: &'d Control,
        kind: Option<SourceTag>,
        mask: Option<&MaskId>,
    ) -> Option<ResolvedReset<'d>> {
        crate::modules::resolve_group_reset(&owner.id, group, kind, mask)
    }

    /// Every field a control variant supersedes, derived from the variants and never listed by
    /// name: when a number control of action `A` and parameter `P` has a variant for kind `K`, `P`
    /// of `A` is superseded on the global target of a `K` photo, and the variant's own field is its
    /// one path there. The host's refusal, admission and `schema.list` all read this. `O(controls)`.
    pub fn superseded(&self) -> Vec<Superseded<'_>> {
        let mut found = Vec::new();
        for owner in self.descriptors() {
            walk(&owner.controls, &mut |control| {
                let Control::Number(NumberControl {
                    action,
                    parameter,
                    label,
                    variants,
                    ..
                }) = control
                else {
                    return;
                };
                for variant in variants {
                    if let Some(Control::Number(NumberControl {
                        action: by_action,
                        parameter: by_parameter,
                        ..
                    })) = variant.control.as_deref()
                    {
                        found.push(Superseded {
                            source: variant.source,
                            action,
                            parameter,
                            label,
                            module: &variant.module,
                            by_action,
                            by_parameter,
                        });
                    }
                }
            });
        }
        found
    }

    /// The supersession of parameter `parameter` of action `action` on a photo of `kind`, if any.
    pub fn superseded_field(
        &self,
        action: &str,
        parameter: &str,
        kind: SourceTag,
    ) -> Option<Superseded<'_>> {
        self.superseded().into_iter().find(|field| {
            field.source == kind && field.action == action && field.parameter == parameter
        })
    }

    /// What a superseded field is refused with, worded from the declarations: `on a RAW photo,
    /// Temperature is the source development's: set-raw temperature (K)` — the kind, the base
    /// control's label, the variant module's hint (its title without one), and the variant's action,
    /// parameter and declared unit.
    pub fn superseded_refusal(&self, field: &Superseded<'_>) -> String {
        let module = self.module(field.module).map(|module| module.descriptor());
        let owner = module
            .map(|module| module.hint.as_deref().unwrap_or(&module.title))
            .unwrap_or(field.module)
            .to_lowercase();
        let unit = module
            .and_then(|module| module.action(field.by_action))
            .and_then(|action| action.parameter(field.by_parameter))
            .and_then(|parameter| parameter.unit.as_deref())
            .map(|unit| format!(" ({unit})"))
            .unwrap_or_default();
        format!(
            "on a {} photo, {} is the {owner}'s: {} {}{unit}",
            field.source.label(),
            field.label,
            field.by_action,
            field.by_parameter
        )
    }

    /// The refusal of a superseded field, of kind `error`: [`Self::superseded_refusal`]'s message,
    /// and data `{source, field, by}` naming the photo's kind, the refused field and the variant's
    /// field that is its one path there, as `schema.list` names both (`set-raw.temperature`).
    pub(crate) fn superseded_error(&self, error: ErrorKind, field: &Superseded<'_>) -> Error {
        Error::new(error, self.superseded_refusal(field)).with_data(serde_json::json!({
            "source": field.source,
            "field": format!("{}.{}", field.action, field.parameter),
            "by": field.by(),
        }))
    }
}
