//! The rules a descriptor is registered against: identities, declared parameters and their hints,
//! controls and their bindings, resets, canvas interactions and the presets control, for a module
//! and, with the host's three differences, for the host's own descriptors.
use super::types::{
    ActionControl, ActionDescriptor, CanvasInteraction, ChoiceControl, ColorControl, Control,
    CurveControl, EffectStage, GroupControl, MAX_SECRET_LENGTH, ModuleDescriptor, ModuleLayout,
    NumberControl, PRESET_ID, PRESET_NAME, PRESET_SETTINGS, ParameterDescriptor, ParameterKind,
    PickerControl, PresetsControl, RailDecoration, RangeControl, ResetAction, TaskControl,
    ToggleControl,
};
use super::values::check_value;
use crate::Error;
use std::collections::HashSet;

/// Groups nest for layout only; a descriptor deeper than this is rejected rather than walked.
const MAX_CONTROL_DEPTH: usize = 8;

/// The longest text a `string` parameter may declare, in characters.
const MAX_STRING_LENGTH: usize = 256;

fn valid_segments(value: &str, separator: char, alphabetic_segments: bool) -> bool {
    !value.is_empty()
        && value.split(separator).enumerate().all(|(index, segment)| {
            let mut bytes = segment.bytes();
            let Some(first) = bytes.next() else {
                return false;
            };
            let starts = first.is_ascii_lowercase()
                || (!alphabetic_segments && index > 0 && first.is_ascii_digit());
            starts && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// Module and effect identity: lowercase words separated by dots, e.g. `luxforge.pixel.replace`.
pub(crate) fn valid_identity(value: &str) -> bool {
    valid_segments(value, '.', true)
}

/// Action and parameter identity: lowercase words separated by hyphens, e.g. `set-pixel`.
pub(crate) fn valid_name(value: &str) -> bool {
    valid_segments(value, '-', false)
}

impl ModuleDescriptor {
    /// Reject every descriptor a client could not render or validate against.
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_as(Declarer::Module)
    }

    /// [`Self::validate`] for a descriptor the **host** publishes for its own objects
    /// ([`crate::ModuleRegistry::host_descriptors`]), which meets every rule a module's does except
    /// the three that describe what only the host may declare:
    ///
    /// - an action or query identity is the method a client calls, one lowercase family word, a dot
    ///   and a hyphenated name (`mask.create-linear`, `mask.list`): the host's commands live in its
    ///   own method namespace, as `history.*` does, and a module identity may carry no dot, which is
    ///   what keeps the two namespaces apart;
    /// - a parameter name may join its words with `_` as well as `-`, because a host command's
    ///   geometry parameters are its stored payload's field names (`radius_x`);
    /// - a parameter may be of a host-only kind ([`ParameterKind::host_only`]), which names one of
    ///   the host's own objects.
    ///
    /// It adds one rule a module does not have: **a host control declares no variants**. A variant
    /// applies only on the global target, and a host control always addresses one of the host's
    /// objects — a mask control is always masked — so a variant on one could never apply.
    pub(crate) fn validate_host(&self) -> Result<(), Error> {
        self.validate_as(Declarer::Host)
    }

    fn validate_as(&self, declarer: Declarer) -> Result<(), Error> {
        if !valid_identity(&self.id) {
            return Err(Error::validation(format!(
                "invalid module identity {}",
                self.id
            )));
        }
        if self.title.trim().is_empty() {
            return Err(Error::validation(format!(
                "module {} has no title",
                self.id
            )));
        }
        let mut effects = HashSet::with_capacity(self.effects.len());
        for effect in &self.effects {
            if !valid_identity(&effect.id) {
                return Err(Error::validation(format!(
                    "invalid effect identity {}",
                    effect.id
                )));
            }
            if !effects.insert(effect.id.as_str()) {
                return Err(Error::validation(format!("duplicate effect {}", effect.id)));
            }
            if let Some(repeated) = effect
                .sources
                .iter()
                .enumerate()
                .find_map(|(at, kind)| effect.sources[..at].contains(kind).then_some(kind))
            {
                return Err(Error::validation(format!(
                    "effect {} lists source kind {} twice",
                    effect.id,
                    repeated.label()
                )));
            }
            // A mask is stored in content-stage coordinates, so an effect whose input is not that
            // content stage has nothing to read one in: a geometry effect changes the stage and a
            // finish effect is defined in the output coordinates the geometry tail produced.
            if effect.maskable
                && matches!(effect.stage, EffectStage::Geometry | EffectStage::Finish)
            {
                return Err(Error::validation(format!(
                    "effect {} declares maskable at the {} stage, which a mask stored in \
                     content-stage coordinates cannot reach",
                    effect.id,
                    effect.stage.as_str()
                )));
            }
        }
        let mut actions = HashSet::with_capacity(self.actions.len());
        for action in &self.actions {
            check_declared(declarer, action, "action", &mut actions)?;
        }
        // A query declares and validates exactly like an action, in its own identity namespace.
        let mut queries = HashSet::with_capacity(self.queries.len());
        for query in &self.queries {
            check_declared(declarer, query, "query", &mut queries)?;
        }
        // Settings, capabilities, resources and tasks refer to each other and to the
        // actions above, so they are checked together once those are known to be sound.
        crate::capabilities::descriptor::validate(self)?;
        for control in &self.controls {
            self.check_control(control, 1)?;
        }
        if declarer == Declarer::Host
            && let Some(control) = with_variants(&self.controls)
        {
            return Err(Error::validation(format!(
                "{} control of host descriptor {} declares variants, which apply only on the \
                 global target a host control never addresses",
                control.kind_name(),
                self.id
            )));
        }
        // One picker stands for one pick mode, so two would be two ways into the same mode and a
        // panel could not say which is selected.
        let pickers = Self::count(&self.controls, &|control| {
            matches!(control, Control::Picker(_))
        });
        if pickers > 1 {
            return Err(Error::validation(format!(
                "module {} declares {pickers} picker controls; a module declares at most one",
                self.id
            )));
        }
        // One library per module: two would show the same presets twice with no way to say which
        // one a client should render.
        let presets = Self::count(&self.controls, &|control| {
            matches!(control, Control::Presets(_))
        });
        if presets > 1 {
            return Err(Error::validation(format!(
                "module {} declares {presets} presets controls; a module declares at most one",
                self.id
            )));
        }
        self.check_reset(self.reset.as_ref())?;
        if self.layout == ModuleLayout::Tabs {
            let all_groups = self
                .controls
                .iter()
                .all(|control| matches!(control, Control::Group(_)));
            if self.controls.len() < 2 || !all_groups {
                return Err(Error::validation(format!(
                    "module {} declares layout: tabs but needs at least two top-level groups",
                    self.id
                )));
            }
        }
        match &self.canvas {
            Some(CanvasInteraction::PointPick {
                action,
                x,
                y,
                title,
                shortcut,
                icon,
                commit,
            }) => {
                self.check_canvas_mode(title, shortcut.as_deref(), icon.as_deref())?;
                let declared = self.declared_action(action)?;
                for name in [x, y] {
                    let parameter = self.declared_parameter(declared, name)?;
                    if !matches!(parameter.kind, ParameterKind::Integer { .. }) {
                        return Err(Error::validation(format!(
                            "canvas parameter {name} of action {action} is not an integer"
                        )));
                    }
                }
                // A committing pick sends the two coordinates and nothing else, so they must be
                // the whole request its action takes.
                if *commit
                    && let Some(other) = declared
                        .parameters
                        .iter()
                        .find(|parameter| parameter.name != *x && parameter.name != *y)
                {
                    return Err(Error::validation(format!(
                        "canvas action {action} commits on a pick but declares parameter {}",
                        other.name
                    )));
                }
            }
            Some(CanvasInteraction::SampleApply {
                query,
                x,
                y,
                action,
                title,
                shortcut,
                icon,
            }) => {
                self.check_canvas_mode(title, shortcut.as_deref(), icon.as_deref())?;
                // The query answers the pick and the action receives its result, so both identities
                // and both coordinate parameters must be declared here before a client sees them.
                let declared = self.declared_query(query)?;
                for name in [x, y] {
                    let parameter = self.declared_parameter(declared, name)?;
                    if !matches!(parameter.kind, ParameterKind::Integer { .. }) {
                        return Err(Error::validation(format!(
                            "canvas parameter {name} of query {query} is not an integer"
                        )));
                    }
                }
                self.declared_action(action)?;
            }
            Some(CanvasInteraction::CropFrame {
                action,
                angle,
                x,
                y,
                width,
                height,
                fit_action,
                aspect,
                title,
                shortcut,
                icon,
            }) => {
                self.check_canvas_mode(title, shortcut.as_deref(), icon.as_deref())?;
                let declared = self.declared_action(action)?;
                for name in [angle, x, y, width, height] {
                    self.canvas_number(declared, name)?;
                }
                let fit = self.declared_action(fit_action)?;
                let chosen = self.declared_parameter(fit, aspect)?;
                if !matches!(chosen.kind, ParameterKind::Enum { .. }) {
                    return Err(Error::validation(format!(
                        "canvas parameter {aspect} of action {fit_action} is not an enum"
                    )));
                }
                // Fitting a ratio needs the draft angle, so the fit action declares one too.
                self.canvas_number(fit, angle)?;
            }
            None => {}
        }
        Ok(())
    }

    /// A canvas mode needs a name for the mode strip, its optional shortcut is exactly one
    /// uppercase ASCII letter so a keymap can hold it without parsing, and its optional icon is a
    /// name from the vocabulary an action control's `icon` uses.
    fn check_canvas_mode(
        &self,
        title: &str,
        shortcut: Option<&str>,
        icon: Option<&str>,
    ) -> Result<(), Error> {
        if title.trim().is_empty() {
            return Err(Error::validation(format!(
                "module {} declares a canvas interaction without a title",
                self.id
            )));
        }
        match shortcut {
            Some(letter)
                if letter.len() != 1 || !letter.starts_with(|c: char| c.is_ascii_uppercase()) =>
            {
                return Err(Error::validation(format!(
                    "canvas shortcut {letter} of module {} must be one uppercase ASCII letter",
                    self.id
                )));
            }
            _ => {}
        }
        match icon {
            Some(icon) if !valid_name(icon) => Err(Error::validation(format!(
                "canvas mode of module {} has invalid icon name {icon}",
                self.id
            ))),
            _ => Ok(()),
        }
    }

    /// Whether this module declares a pick canvas (`point-pick` or `sample-apply`) that none of its
    /// own controls reaches. A pick mode is entered from the panel, so such a module is valid only
    /// when another module's picker variant reaches it, which only the complete registry can say
    /// ([`crate::ModuleRegistry::check_complete`]). Without that a pick would be reachable only by
    /// its letter.
    pub(crate) fn needs_foreign_picker(&self) -> bool {
        matches!(
            self.canvas,
            Some(CanvasInteraction::PointPick { .. } | CanvasInteraction::SampleApply { .. })
        ) && Self::count(&self.controls, &|control| {
            matches!(control, Control::Picker(_))
        }) == 0
    }

    /// A reset is validated exactly like an action control: the action must be declared here and
    /// every preset value must be in its parameter's range.
    pub(crate) fn check_reset(&self, reset: Option<&ResetAction>) -> Result<(), Error> {
        let Some(reset) = reset else {
            return Ok(());
        };
        let declared = self.declared_action(&reset.action)?;
        for (name, value) in &reset.preset {
            check_value(self.declared_parameter(declared, name)?, value)?;
        }
        Ok(())
    }

    fn canvas_number(&self, action: &ActionDescriptor, name: &str) -> Result<(), Error> {
        let parameter = self.declared_parameter(action, name)?;
        if matches!(parameter.kind, ParameterKind::Number { .. }) {
            Ok(())
        } else {
            Err(Error::validation(format!(
                "canvas parameter {name} of action {} is not a number",
                action.id
            )))
        }
    }

    fn declared_action(&self, id: &str) -> Result<&ActionDescriptor, Error> {
        self.action(id).ok_or_else(|| {
            Error::validation(format!(
                "module {} references undeclared action {id}",
                self.id
            ))
        })
    }

    fn declared_query(&self, id: &str) -> Result<&ActionDescriptor, Error> {
        self.query(id).ok_or_else(|| {
            Error::validation(format!(
                "module {} references undeclared query {id}",
                self.id
            ))
        })
    }

    fn declared_parameter<'a>(
        &self,
        action: &'a ActionDescriptor,
        name: &str,
    ) -> Result<&'a ParameterDescriptor, Error> {
        action.parameter(name).ok_or_else(|| {
            Error::validation(format!("action {} has no parameter {name}", action.id))
        })
    }

    /// What one control's variants must be on their own, before the registry is complete: another
    /// module's valid identity, at most one per source kind, and the right replacement — a `reset`
    /// on a group that has a reset of its own, and on a number, action or picker a `control` of the
    /// same kind that carries no variants itself. Whether the named module exists, applies to the
    /// kind and declares what the replacement names is [`crate::ModuleRegistry::check_complete`]'s.
    fn check_variant_shapes(&self, control: &Control) -> Result<(), Error> {
        let variants = control.variants();
        let kind = control.kind_name();
        for (at, variant) in variants.iter().enumerate() {
            let source = variant.source.label();
            if variants[..at]
                .iter()
                .any(|earlier| earlier.source == variant.source)
            {
                return Err(Error::validation(format!(
                    "{kind} control of module {} declares two variants for {source}",
                    self.id
                )));
            }
            if !valid_identity(&variant.module) || variant.module == self.id {
                return Err(Error::validation(format!(
                    "{source} variant of a {kind} control of module {} must name another module, \
                     not {}",
                    self.id, variant.module
                )));
            }
            match (control, &variant.control, &variant.reset) {
                (Control::Group(GroupControl { reset, .. }), None, Some(_)) => {
                    if reset.is_none() {
                        return Err(Error::validation(format!(
                            "group of module {} declares a {source} reset variant but no reset",
                            self.id
                        )));
                    }
                }
                (Control::Group(_), _, _) => {
                    return Err(Error::validation(format!(
                        "{source} variant of a group of module {} needs a reset and no control",
                        self.id
                    )));
                }
                (_, Some(replacement), None) => {
                    if replacement.kind_name() != kind {
                        return Err(Error::validation(format!(
                            "{source} variant of a {kind} control of module {} is a {} control",
                            self.id,
                            replacement.kind_name()
                        )));
                    }
                    if !replacement.variants().is_empty() {
                        return Err(Error::validation(format!(
                            "{source} variant of a {kind} control of module {} declares variants \
                             of its own",
                            self.id
                        )));
                    }
                }
                _ => {
                    return Err(Error::validation(format!(
                        "{source} variant of a {kind} control of module {} needs a control and no \
                         reset",
                        self.id
                    )));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn check_control(&self, control: &Control, depth: usize) -> Result<(), Error> {
        if depth > MAX_CONTROL_DEPTH {
            return Err(Error::validation(format!(
                "module {} nests controls deeper than {MAX_CONTROL_DEPTH} levels",
                self.id
            )));
        }
        self.check_variant_shapes(control)?;
        match control {
            Control::Group(GroupControl {
                label,
                controls,
                reset,
                ..
            }) => {
                if label.trim().is_empty() {
                    return Err(Error::validation(format!(
                        "module {} has an unlabelled group",
                        self.id
                    )));
                }
                self.check_reset(reset.as_ref())?;
                for child in controls {
                    self.check_control(child, depth + 1)?;
                }
            }
            Control::Number(NumberControl {
                action,
                parameter,
                rail,
                reset,
                ..
            }) => {
                self.check_reset(reset.as_ref())?;
                let declared = self.declared_action(action)?;
                let declared = self.declared_parameter(declared, parameter)?;
                if !matches!(
                    declared.kind,
                    ParameterKind::Integer { .. } | ParameterKind::Number { .. }
                ) {
                    return Err(Error::validation(format!(
                        "number control for {parameter} of action {action} is not an integer or a number"
                    )));
                }
                if rail.is_some() && !matches!(declared.kind, ParameterKind::Number { .. }) {
                    return Err(Error::validation(format!(
                        "number control for {parameter} of action {action} declares a rail hint on a non-number parameter"
                    )));
                }
                if let Some(RailDecoration::Gradient { stops }) = rail
                    && !(2..=8).contains(&stops.len())
                {
                    return Err(Error::validation(format!(
                        "number control for {parameter} of action {action} needs 2..=8 gradient stops"
                    )));
                }
            }
            Control::Toggle(ToggleControl {
                action, parameter, ..
            }) => {
                let declared = self.declared_parameter(self.declared_action(action)?, parameter)?;
                if !matches!(declared.kind, ParameterKind::Boolean) {
                    return Err(Error::validation(format!(
                        "toggle control for {parameter} of action {action} is not a boolean"
                    )));
                }
            }
            Control::Choice(ChoiceControl {
                action, parameter, ..
            }) => {
                let declared = self.declared_parameter(self.declared_action(action)?, parameter)?;
                if !matches!(declared.kind, ParameterKind::Enum { .. }) {
                    return Err(Error::validation(format!(
                        "choice control for {parameter} of action {action} is not an enum"
                    )));
                }
            }
            Control::Color(ColorControl {
                action, parameter, ..
            }) => {
                let declared = self.declared_action(action)?;
                let declared = self.declared_parameter(declared, parameter)?;
                if !matches!(declared.kind, ParameterKind::Color) {
                    return Err(Error::validation(format!(
                        "color control for {parameter} of action {action} is not a color"
                    )));
                }
            }
            Control::Curve(CurveControl {
                action,
                channels,
                sample_query,
                ..
            }) => {
                self.declared_action(action)?;
                let query = self.declared_query(sample_query)?;
                if channels.is_empty() || channels.len() > 8 {
                    return Err(Error::validation(format!(
                        "curve control of action {action} needs 1..=8 channels"
                    )));
                }
                let mut seen = HashSet::with_capacity(channels.len());
                for channel in channels {
                    let parameter = &channel.parameter;
                    let declared =
                        self.declared_parameter(self.declared_action(action)?, parameter)?;
                    if !matches!(declared.kind, ParameterKind::Curve { .. }) {
                        return Err(Error::validation(format!(
                            "curve control for {parameter} of action {action} is not a curve"
                        )));
                    }
                    let query_parameter = self.declared_parameter(query, parameter)?;
                    if query_parameter.kind != declared.kind {
                        return Err(Error::validation(format!(
                            "curve control for {parameter} of action {action} has a mismatched sample query {sample_query}"
                        )));
                    }
                    if query_parameter.required || query_parameter.default.is_some() {
                        return Err(Error::validation(format!(
                            "curve control for {parameter} of action {action} needs an optional sample query field without a default"
                        )));
                    }
                    if channel.label.trim().is_empty() || !seen.insert(parameter) {
                        return Err(Error::validation(format!(
                            "curve control for {parameter} of action {action} needs distinct labelled channels"
                        )));
                    }
                }
                if query
                    .parameters
                    .iter()
                    .any(|parameter| parameter.required && !seen.contains(&parameter.name))
                {
                    return Err(Error::validation(format!(
                        "curve control of action {action} has sample query {sample_query} with unrelated required parameters"
                    )));
                }
            }
            Control::Range(RangeControl {
                action,
                low,
                high,
                low_feather,
                high_feather,
                label,
                rail,
            }) => {
                if label.trim().is_empty() {
                    return Err(Error::validation(format!(
                        "range control of action {action} has no label"
                    )));
                }
                let declared = self.declared_action(action)?;
                let names: Vec<&String> = [Some(low), Some(high)]
                    .into_iter()
                    .chain([low_feather.as_ref(), high_feather.as_ref()])
                    .flatten()
                    .collect();
                for (at, name) in names.iter().enumerate() {
                    let parameter = self.declared_parameter(declared, name)?;
                    if !matches!(parameter.kind, ParameterKind::Number { .. }) {
                        return Err(Error::validation(format!(
                            "range control for {name} of action {action} is not a number"
                        )));
                    }
                    if names[..at].contains(name) {
                        return Err(Error::validation(format!(
                            "range control of action {action} binds {name} twice"
                        )));
                    }
                }
                // The two edges are two positions on the one axis the rail spans, so they declare
                // the same range; a shoulder is a width on that axis and declares its own.
                let edge = |name: &str| declared.parameter(name).map(|p| &p.kind);
                if edge(low) != edge(high) {
                    return Err(Error::validation(format!(
                        "range control of action {action} binds {low} and {high}, which declare \
                         different ranges"
                    )));
                }
                if let Some(RailDecoration::Gradient { stops }) = rail
                    && !(2..=8).contains(&stops.len())
                {
                    return Err(Error::validation(format!(
                        "range control of action {action} needs 2..=8 gradient stops"
                    )));
                }
            }
            Control::Action(ActionControl {
                action,
                preset,
                icon,
                ..
            }) => {
                let declared = self.declared_action(action)?;
                // A button on a patch sends the fields it names and nothing else, so it names at
                // least one; a group's worth, such as As shot's temperature and tint, is allowed.
                if declared.patch && preset.is_empty() {
                    return Err(Error::validation(format!(
                        "action control for patch action {action} needs at least one preset field"
                    )));
                }
                for (name, value) in preset {
                    check_value(self.declared_parameter(declared, name)?, value)?;
                }
                if let Some(icon) = icon
                    && !valid_name(icon)
                {
                    return Err(Error::validation(format!(
                        "action control {action} has invalid icon name {icon}"
                    )));
                }
            }
            // A picker is the panel's way into this module's own pick mode, so the module must
            // declare one. A crop frame takes the whole canvas and has its own controls; it is not
            // a pick and a picker cannot stand for it.
            Control::Picker(PickerControl { label, .. }) => {
                if label.trim().is_empty() {
                    return Err(Error::validation(format!(
                        "module {} has an unlabelled picker",
                        self.id
                    )));
                }
                match &self.canvas {
                    Some(CanvasInteraction::PointPick { .. })
                    | Some(CanvasInteraction::SampleApply { .. }) => {}
                    Some(CanvasInteraction::CropFrame { .. }) | None => {
                        return Err(Error::validation(format!(
                            "module {} declares a picker control without a point-pick or sample-apply canvas",
                            self.id
                        )));
                    }
                }
            }
            Control::Task(TaskControl { task, label }) => {
                if label.trim().is_empty() {
                    return Err(Error::validation(format!(
                        "task control for {task} of module {} has no label",
                        self.id
                    )));
                }
                if self.task(task).is_none() {
                    return Err(Error::validation(format!(
                        "task control of module {} names undeclared task {task}",
                        self.id
                    )));
                }
            }
            Control::Presets(PresetsControl { action }) => {
                let declared = self.declared_action(action)?;
                self.check_presets_action(declared)?;
            }
        }
        Ok(())
    }

    /// A presets control submits its action with a preset's settings set, name and library
    /// identity, and nothing else, so the action declares exactly those parameters with the kinds
    /// that carry them. A field patch makes every parameter optional, so it cannot require them.
    fn check_presets_action(&self, action: &ActionDescriptor) -> Result<(), Error> {
        let id = &action.id;
        if action.patch {
            return Err(Error::validation(format!(
                "presets control action {id} is a field patch, so it cannot require its settings and name"
            )));
        }
        let required = |name: &str, kind: &str, matches: fn(&ParameterKind) -> bool| {
            let parameter = self.declared_parameter(action, name)?;
            if !matches(&parameter.kind) || !parameter.required || parameter.default.is_some() {
                return Err(Error::validation(format!(
                    "presets control action {id} needs a required {kind} parameter {name}"
                )));
            }
            Ok(())
        };
        required(PRESET_SETTINGS, "settings", |kind| {
            matches!(kind, ParameterKind::Settings)
        })?;
        required(PRESET_NAME, "string", |kind| {
            matches!(kind, ParameterKind::String { .. })
        })?;
        if let Some(parameter) = action.parameter(PRESET_ID)
            && (!matches!(parameter.kind, ParameterKind::String { .. }) || parameter.required)
        {
            return Err(Error::validation(format!(
                "presets control action {id} may declare only an optional string parameter {PRESET_ID}"
            )));
        }
        if let Some(extra) = action.parameters.iter().find(|parameter| {
            ![PRESET_SETTINGS, PRESET_NAME, PRESET_ID].contains(&&*parameter.name)
        }) {
            return Err(Error::validation(format!(
                "presets control action {id} declares parameter {} beyond {PRESET_SETTINGS}, {PRESET_NAME} and {PRESET_ID}",
                extra.name
            )));
        }
        Ok(())
    }

    /// How many controls of one kind this module declares, at any depth.
    fn count(controls: &[Control], kind: &dyn Fn(&Control) -> bool) -> usize {
        controls
            .iter()
            .map(|control| match control {
                Control::Group(GroupControl { controls, .. }) => Self::count(controls, kind),
                control => usize::from(kind(control)),
            })
            .sum()
    }
}

/// One declared action or query: a valid, unique identity, a title, and parameters whose names,
/// ranges, hints and defaults a caller can be validated against. Actions and queries are checked by
/// the same rules because a client calls them the same way; only the method prefix differs.
fn check_declared<'a>(
    declarer: Declarer,
    declared: &'a ActionDescriptor,
    kind: &str,
    seen: &mut HashSet<&'a str>,
) -> Result<(), Error> {
    let valid = match declarer {
        Declarer::Module => valid_name(&declared.id),
        Declarer::Host => valid_host_method(&declared.id),
    };
    if !valid {
        return Err(Error::validation(format!(
            "invalid {kind} identity {}",
            declared.id
        )));
    }
    if !seen.insert(declared.id.as_str()) {
        return Err(Error::validation(format!(
            "duplicate {kind} {}",
            declared.id
        )));
    }
    if declared.title.trim().is_empty() {
        return Err(Error::validation(format!(
            "{kind} {} has no title",
            declared.id
        )));
    }
    declared_parameters(declarer, kind, &declared.id, &declared.parameters)
}

/// Who declares a descriptor, which decides the three things only the host may declare
/// ([`ModuleDescriptor::validate_host`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Declarer {
    Module,
    Host,
}

/// A host action or query identity: the method a client calls, one lowercase family word, a dot and
/// a hyphenated name, e.g. `mask.create-linear`.
fn valid_host_method(value: &str) -> bool {
    value
        .split_once('.')
        .is_some_and(|(family, name)| valid_segments(family, '.', true) && valid_name(name))
}

/// The first control, at any depth, that declares variants.
fn with_variants(controls: &[Control]) -> Option<&Control> {
    controls.iter().find_map(|control| {
        if !control.variants().is_empty() {
            return Some(control);
        }
        match control {
            Control::Group(GroupControl { controls, .. }) => with_variants(controls),
            _ => None,
        }
    })
}

/// The parameters one action, query or task declares: valid, unique names, sound kinds, hints and
/// defaults, so every caller can be validated against them the same way.
pub(crate) fn check_parameter_declarations(
    kind: &str,
    id: &str,
    declared: &[ParameterDescriptor],
) -> Result<(), Error> {
    declared_parameters(Declarer::Module, kind, id, declared)
}

fn declared_parameters(
    declarer: Declarer,
    kind: &str,
    id: &str,
    declared: &[ParameterDescriptor],
) -> Result<(), Error> {
    let host = declarer == Declarer::Host;
    let mut parameters = HashSet::with_capacity(declared.len());
    for parameter in declared {
        let valid =
            valid_name(&parameter.name) || (host && valid_segments(&parameter.name, '_', false));
        if !valid {
            return Err(Error::validation(format!(
                "invalid parameter name {} of {kind} {id}",
                parameter.name
            )));
        }
        if !parameters.insert(parameter.name.as_str()) {
            return Err(Error::validation(format!(
                "duplicate parameter {} of {kind} {id}",
                parameter.name
            )));
        }
        if parameter.kind.setting_only() {
            return Err(Error::validation(format!(
                "parameter {} of {kind} {id} declares kind {}, which only a module setting declares",
                parameter.name,
                parameter.kind.name()
            )));
        }
        // An identity names one of the host's own objects, which only the host's commands address;
        // a module edits through the host's `mask` target and never names a mask itself.
        if !host && parameter.kind.host_only() {
            return Err(Error::validation(format!(
                "parameter {} of {kind} {id} declares kind {}, which only a host command declares",
                parameter.name,
                parameter.kind.name()
            )));
        }
        check_declaration(parameter)?;
    }
    Ok(())
}

/// One declared parameter's kind, hints and default, wherever it is declared: an action, a query, a
/// task or a module setting.
pub(crate) fn check_declaration(parameter: &ParameterDescriptor) -> Result<(), Error> {
    match &parameter.kind {
        ParameterKind::Integer { min, max } if min > max => {
            return Err(Error::validation(format!(
                "parameter {} declares an empty range {min}..={max}",
                parameter.name
            )));
        }
        ParameterKind::Number { min, max } if !min.is_finite() || !max.is_finite() || min > max => {
            return Err(Error::validation(format!(
                "parameter {} declares an empty range {min}..={max}",
                parameter.name
            )));
        }
        ParameterKind::Enum { options } => {
            if options.is_empty() {
                return Err(Error::validation(format!(
                    "parameter {} declares no options",
                    parameter.name
                )));
            }
            let mut seen = HashSet::with_capacity(options.len());
            if let Some(option) = options
                .iter()
                .find(|option| option.is_empty() || !seen.insert(option.as_str()))
            {
                return Err(Error::validation(format!(
                    "parameter {} declares an empty or duplicate option {option:?}",
                    parameter.name
                )));
            }
        }
        ParameterKind::String { max_length }
            if *max_length == 0 || *max_length > MAX_STRING_LENGTH =>
        {
            return Err(Error::validation(format!(
                "parameter {} declares a max_length {max_length} outside 1..={MAX_STRING_LENGTH}",
                parameter.name
            )));
        }
        ParameterKind::Text { max_bytes: 0 } => {
            return Err(Error::validation(format!(
                "parameter {} declares a max_bytes of 0",
                parameter.name
            )));
        }
        ParameterKind::Points {
            points_min,
            points_max,
        } if *points_min < 1
            || *points_max > crate::path::POSTED_POINTS_PER_STROKE
            || points_min > points_max =>
        {
            return Err(Error::validation(format!(
                "parameter {} declares invalid path point bounds; 1..={} is the limit",
                parameter.name,
                crate::path::POSTED_POINTS_PER_STROKE
            )));
        }
        ParameterKind::Curve {
            points_min,
            points_max,
            fixed_x,
            ..
        } => {
            if *points_min < 2 || *points_max > 32 || points_min > points_max {
                return Err(Error::validation(format!(
                    "parameter {} declares invalid curve point bounds",
                    parameter.name
                )));
            }
            if let Some(xs) = fixed_x
                && (xs.len() < *points_min
                    || xs.len() > *points_max
                    || xs
                        .iter()
                        .any(|x| !x.is_finite() || !(0.0..=1.0).contains(x))
                    || xs.windows(2).any(|pair| pair[0] >= pair[1]))
            {
                return Err(Error::validation(format!(
                    "parameter {} declares invalid fixed_x curve points",
                    parameter.name
                )));
            }
        }
        ParameterKind::Secret { max_length }
            if *max_length == 0 || *max_length > MAX_SECRET_LENGTH =>
        {
            return Err(Error::validation(format!(
                "parameter {} declares a max_length {max_length} outside 1..={MAX_SECRET_LENGTH}",
                parameter.name
            )));
        }
        ParameterKind::Endpoint { classes } => {
            if classes.is_empty() {
                return Err(Error::validation(format!(
                    "endpoint parameter {} declares no class",
                    parameter.name
                )));
            }
            let mut seen = HashSet::with_capacity(classes.len());
            if !classes.iter().all(|class| seen.insert(class)) {
                return Err(Error::validation(format!(
                    "endpoint parameter {} declares a class twice",
                    parameter.name
                )));
            }
        }
        _ => {}
    }
    check_hints(parameter)?;
    let Some(default) = &parameter.default else {
        return Ok(());
    };
    match parameter.kind {
        // A secret is never plain data, and a destination is a person's choice, never a
        // module's.
        ParameterKind::Secret { .. } => Err(Error::validation(format!(
            "secret parameter {} declares a default; a secret never has one",
            parameter.name
        ))),
        ParameterKind::Endpoint { .. } => Err(Error::validation(format!(
            "endpoint parameter {} declares a default; a destination is the person's choice",
            parameter.name
        ))),
        _ => check_value(parameter, default),
    }
}

/// The largest number of decimals a client is asked to display. Beyond this a slider's text is
/// noise rather than information, so a descriptor declaring more is rejected at registration.
const MAX_PRECISION: u8 = 6;

/// Steps and precision describe numeric and curve controls. They remain client hints: requests
/// are validated against the hard kind and are never rounded to a hint.
fn check_hints(parameter: &ParameterDescriptor) -> Result<(), Error> {
    let name = &parameter.name;
    if !matches!(
        parameter.kind,
        ParameterKind::Integer { .. } | ParameterKind::Number { .. } | ParameterKind::Curve { .. }
    ) && (parameter.step.is_some() || parameter.precision.is_some())
    {
        return Err(Error::validation(format!(
            "parameter {name} declares a step or precision but is not numeric or a curve"
        )));
    }
    let bounds = match parameter.kind {
        ParameterKind::Integer { min, max } => Some((min as f64, max as f64)),
        ParameterKind::Number { min, max } => Some((min, max)),
        _ => None,
    };
    if bounds.is_none()
        && [parameter.soft_min, parameter.soft_max, parameter.zero]
            .iter()
            .any(Option::is_some)
    {
        return Err(Error::validation(format!(
            "parameter {name} declares numeric hints but is not a number"
        )));
    }
    if bounds.is_none()
        && !matches!(parameter.kind, ParameterKind::Curve { .. })
        && parameter.fine_step.is_some()
    {
        return Err(Error::validation(format!(
            "parameter {name} declares a fine step but is not numeric or a curve"
        )));
    }
    if let Some((min, max)) = bounds {
        let soft_min = parameter.soft_min.unwrap_or(min);
        let soft_max = parameter.soft_max.unwrap_or(max);
        if !soft_min.is_finite()
            || !soft_max.is_finite()
            || soft_min < min
            || soft_max > max
            || (min < max && soft_min >= soft_max)
        {
            return Err(Error::validation(format!(
                "parameter {name} declares a soft range outside {min}..={max}"
            )));
        }
        if let Some(fine_step) = parameter.fine_step
            && (!fine_step.is_finite() || fine_step <= 0.0)
        {
            return Err(Error::validation(format!(
                "parameter {name} declares a fine step that is not finite and positive"
            )));
        }
        if let Some(zero) = parameter.zero
            && (!zero.is_finite() || zero < min || zero > max)
        {
            return Err(Error::validation(format!(
                "parameter {name} declares a zero outside {min}..={max}"
            )));
        }
    }
    if let Some(fine_step) = parameter.fine_step
        && (!fine_step.is_finite() || fine_step <= 0.0)
    {
        return Err(Error::validation(format!(
            "parameter {name} declares a fine step that is not finite and positive"
        )));
    }
    if let Some(step) = parameter.step
        && (!step.is_finite() || step <= 0.0)
    {
        return Err(Error::validation(format!(
            "parameter {name} declares a step that is not finite and positive"
        )));
    }
    if let Some(precision) = parameter.precision
        && precision > MAX_PRECISION
    {
        return Err(Error::validation(format!(
            "parameter {name} declares a precision above {MAX_PRECISION}"
        )));
    }
    Ok(())
}
