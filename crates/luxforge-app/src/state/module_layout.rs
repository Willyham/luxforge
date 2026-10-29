//! The person's module layout as the desktop draws it
//! ([module manager](../../../../docs/design/module-manager.md#desktop)).
//!
//! The owner holds the layout: `module.list` answers each module's placements and switch, and
//! `module.layout.read` (and every layout write) the orders and the modules turned off. This is
//! what the desktop last read of them, and the one derivation every region draws by: the tools
//! panel's sections ([`super::tools::derive`]), the mode strip ([`super::canvas::derive`]), the
//! palette ([`super::palette::derive`]), the mode letters and the action chords the keyboard binds
//! ([`super::tools::mode_shortcuts`], [`super::host_commands::action_shortcuts`]). Settings ›
//! Modules reads the same [`EffectiveLayout`].
//!
//! The layout is presentation only. Nothing here changes what the owner accepts or how anything
//! renders: an off module stays registered, so its photos render and export, its rows keep their
//! labels and its methods answer. Off is not unavailable: an off module with a non-neutral layer in
//! the displayed photo draws its band, marked Off · edit kept, in the neutral grey.
//!
//! Every function here is bounded by the registered module count.
use crate::state::{
    Inputs,
    number::NumberSpec,
    palette::PaletteAction,
    tools::{labelled_control, with_variants},
};
/// The homes a module's placements and orders name, for the views' messages.
pub(crate) use luxforge_core::Toolset;
use luxforge_core::{
    Control, LayerDescription, ModuleDescriptor, ParameterKind,
    module_layout::{LayoutView, ListedModule, ModuleOrigin, placed_in},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The strip's host entries and their letters, which come before every module's: no module takes
/// them ([design](../../../../docs/design/module-manager.md#desktop), Letters).
pub(crate) const HOST_LETTERS: [(char, &str); 2] = [('V', "Pointer"), ('M', "Mask")];

/// The hint an off module's kept band reads, in the neutral grey: the picture is whole.
pub(crate) const KEPT_HINT: &str = "Off \u{b7} edit kept";

/// What the status bar adds to every layout change: no photo changed.
const UNCHANGED: &str = "rendering unchanged";

/// The person's module layout as the desktop last read it, and what every region derives from it.
///
/// Before `module.layout.read` has answered every module is on and every toolset follows registry
/// order, which is the layout a person who never changed it has; a module `module.list` did not
/// list places by the core's own rule ([`placed_in`]), as a desktop test's registry does.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct EffectiveLayout {
    /// Each listed module's placements, as `module.list` answered them.
    placements: BTreeMap<String, Vec<Toolset>>,
    /// The layout as `module.layout.read`, or the last write of this desktop's, answered it.
    view: Option<LayoutView>,
    /// The registered modules the person turned off.
    off: BTreeSet<String>,
    /// Each registered module that declares a canvas letter, with whether it holds it.
    letters: BTreeMap<String, LetterClaim>,
    /// Each locked module's reason.
    locked: BTreeMap<String, String>,
    /// Each listed module that is not built in, with where it came from: its band carries the
    /// origin badge.
    origins: BTreeMap<String, ModuleOrigin>,
    /// Each external module this launch read that did not load, with its title and the loader's
    /// reason, as `module.package.list` answered at discovery: its band and the stale-preview
    /// notice say *Didn't load* rather than *Not installed*.
    failed: BTreeMap<String, (String, String)>,
    /// A layout write of this desktop's is in flight: the menus offer nothing until it answers.
    pub(crate) pending: bool,
}

/// One module's claim on the canvas letter it declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LetterClaim {
    /// It is the first claimant: the letter enters its mode while it is on.
    Holds(char),
    /// A host entry or an earlier module in registry order holds the letter, named here by title.
    /// The module gets no letter rather than a substitute; the palette still enters its mode.
    Taken { letter: char, owner: String },
}

impl LetterClaim {
    /// The letter this module's mode answers to, when it holds one.
    pub(crate) fn held(&self) -> Option<char> {
        match self {
            Self::Holds(letter) => Some(*letter),
            Self::Taken { .. } => None,
        }
    }

    /// What a tool's tooltip says when another holds its letter: `R is Mask's`.
    pub(crate) fn note(&self) -> Option<String> {
        match self {
            Self::Holds(_) => None,
            Self::Taken { letter, owner } => Some(format!("{letter} is {owner}'s")),
        }
    }
}

impl EffectiveLayout {
    /// Adopt `module.list`'s placements and switches for `listed`, and derive the letters and
    /// locks of `modules`, the same listing's descriptors.
    pub(crate) fn adopt_listing(&mut self, listed: &[ListedModule], modules: &[ModuleDescriptor]) {
        self.placements = listed
            .iter()
            .map(|module| {
                (
                    module.descriptor.id.clone(),
                    module
                        .placements
                        .iter()
                        .map(|placement| placement.toolset)
                        .collect(),
                )
            })
            .collect();
        self.origins = listed
            .iter()
            .filter(|module| module.origin != ModuleOrigin::BuiltIn)
            .map(|module| (module.descriptor.id.clone(), module.origin))
            .collect();
        // Until the layout itself is read, the listing's switches say what is off.
        if self.view.is_none() {
            self.off = listed
                .iter()
                .filter(|module| !module.enabled)
                .map(|module| module.descriptor.id.clone())
                .collect();
        }
        self.derive(modules);
    }

    /// Adopt the descriptors of a registry no `module.list` described, as a test hands them in:
    /// every module placed by the core's rule, and every one on.
    #[cfg(test)]
    pub(crate) fn adopt_descriptors(&mut self, modules: &[ModuleDescriptor]) {
        self.placements.clear();
        self.derive(modules);
    }

    /// Adopt the layout `module.layout.read`, or a write's answer, returned.
    pub(crate) fn adopt_view(&mut self, view: LayoutView, modules: &[ModuleDescriptor]) {
        self.off = view.disabled.iter().cloned().collect();
        self.view = Some(view);
        self.derive(modules);
    }

    /// The letters and locks of `modules`, in registry order: host letters first, then the first
    /// module that declares a letter holds it.
    fn derive(&mut self, modules: &[ModuleDescriptor]) {
        let mut holders: BTreeMap<char, String> = HOST_LETTERS
            .iter()
            .map(|(letter, owner)| (*letter, (*owner).to_owned()))
            .collect();
        self.letters.clear();
        for module in modules {
            let Some(canvas) = module.canvas.as_ref() else {
                continue;
            };
            let Some(letter) = canvas
                .shortcut()
                .and_then(|shortcut| shortcut.chars().next())
                .map(|letter| letter.to_ascii_uppercase())
            else {
                continue;
            };
            // The holder is named as its tool is: by its mode's title.
            let claim = match holders.get(&letter) {
                Some(owner) => LetterClaim::Taken {
                    letter,
                    owner: owner.clone(),
                },
                None => {
                    holders.insert(letter, canvas.title().to_owned());
                    LetterClaim::Holds(letter)
                }
            };
            self.letters.insert(module.id.clone(), claim);
        }
        self.locked = modules
            .iter()
            .filter_map(|module| Some((module.id.clone(), module.locked.as_ref()?.reason.clone())))
            .collect();
        // A locked module is always on, whatever an older read said.
        let locked = &self.locked;
        self.off.retain(|id| !locked.contains_key(id));
    }

    /// Adopt `module.package.list`'s answer at discovery: the external modules this launch read
    /// and did not load.
    pub(crate) fn adopt_packages(&mut self, packages: &super::external_modules::PackageList) {
        self.failed = packages
            .modules
            .iter()
            .filter(|row| row.state == super::external_modules::LoadState::Failed)
            .map(|row| {
                let reason = row.reason.clone().unwrap_or_default();
                (row.module.clone(), (row.title(), reason))
            })
            .collect();
    }

    /// Where `module` came from: built in unless `module.list` said otherwise.
    pub(crate) fn origin(&self, module: &str) -> ModuleOrigin {
        self.origins.get(module).copied().unwrap_or_default()
    }

    /// The title and the loader's reason of an external module this launch read and did not load.
    pub(crate) fn failure(&self, module: &str) -> Option<(&str, &str)> {
        self.failed
            .get(module)
            .map(|(title, reason)| (title.as_str(), reason.as_str()))
    }

    /// The layout as the owner last answered it, or `None` before it has.
    pub(crate) fn view(&self) -> Option<&LayoutView> {
        self.view.as_ref()
    }

    /// Whether the person has `module` on. Every module is, until the layout says otherwise.
    pub(crate) fn enabled(&self, module: &str) -> bool {
        !self.off.contains(module)
    }

    /// Why `module` cannot be turned off, when it is locked.
    pub(crate) fn lock(&self, module: &str) -> Option<&str> {
        self.locked.get(module).map(String::as_str)
    }

    /// Whether `module` has a placement in `toolset`: as `module.list` answered, or by the core's
    /// rule for a module it did not list.
    pub(crate) fn placed(&self, module: &ModuleDescriptor, toolset: Toolset) -> bool {
        match self.placements.get(&module.id) {
            Some(placements) => placements.contains(&toolset),
            None => placed_in(module, toolset),
        }
    }

    /// The person's order of an orderable toolset, or `None` before the layout is read.
    pub(crate) fn order(&self, toolset: Toolset) -> Option<&[String]> {
        self.view.as_ref()?.order(toolset)
    }

    /// Sort `items` into the person's order of `toolset`, stably: an item the order does not name
    /// keeps its place after those it does, and before the layout is read nothing moves.
    pub(crate) fn sort<T>(&self, toolset: Toolset, items: &mut [T], id: impl Fn(&T) -> &str) {
        let Some(order) = self.order(toolset) else {
            return;
        };
        items.sort_by_key(|item| {
            order
                .iter()
                .position(|placed| placed == id(item))
                .unwrap_or(usize::MAX)
        });
    }

    /// `module`'s claim on the canvas letter it declares, when it declares one.
    pub(crate) fn letter(&self, module: &str) -> Option<&LetterClaim> {
        self.letters.get(module)
    }

    /// The registered modules the person turned off, in registry order.
    pub(crate) fn off<'a>(
        &self,
        modules: &'a [ModuleDescriptor],
    ) -> impl Iterator<Item = &'a ModuleDescriptor> {
        modules
            .iter()
            .filter(|module| self.off.contains(&module.id))
    }
}

/// One move a band's menu or a key offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Move {
    Up,
    Down,
    Top,
    Bottom,
}

impl Move {
    pub(crate) const ALL: [Self; 4] = [Self::Up, Self::Down, Self::Top, Self::Bottom];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Up => "Move up",
            Self::Down => "Move down",
            Self::Top => "Move to top",
            Self::Bottom => "Move to bottom",
        }
    }

    /// The key that does the same while the band's menu is open.
    pub(crate) fn key(self) -> Option<&'static str> {
        match self {
            Self::Up => Some("\u{2325}\u{2191}"),
            Self::Down => Some("\u{2325}\u{2193}"),
            Self::Top | Self::Bottom => None,
        }
    }
}

/// The whole order `module.order.set` is sent for moving `module` in `order`, the toolset's
/// complete order, relative to `shown`, the placements drawn now in drawn order: up or down past
/// the drawn neighbour, so a move is never spent passing a module that is off or does not apply
/// to this photo, or to either end of the whole order. `None` when the move changes nothing.
pub(crate) fn moved(
    order: &[String],
    shown: &[&str],
    module: &str,
    to: Move,
) -> Option<Vec<String>> {
    order.iter().position(|id| id == module)?;
    let mut rest: Vec<String> = order.iter().filter(|id| *id != module).cloned().collect();
    let at = shown.iter().position(|id| *id == module);
    let index = match to {
        Move::Top => 0,
        Move::Bottom => rest.len(),
        Move::Up => {
            let before = shown[..at?].last()?;
            rest.iter().position(|id| id == before)?
        }
        Move::Down => {
            let after = shown.get(at? + 1)?;
            rest.iter().position(|id| id == after)? + 1
        }
    };
    rest.insert(index, module.to_owned());
    (rest != order).then_some(rest)
}

/// What the status bar says after a move: `Moved Tone curve above Presence in your panel`. The
/// neighbour named is the drawn one the module now sits before or after.
pub(crate) fn moved_status(
    titles: &BTreeMap<&str, &str>,
    order: &[String],
    shown: &[&str],
    module: &str,
    place: &str,
) -> String {
    let title = titles.get(module).copied().unwrap_or(module);
    let drawn: Vec<&str> = order
        .iter()
        .map(String::as_str)
        .filter(|id| shown.contains(id))
        .collect();
    let at = drawn.iter().position(|id| *id == module);
    let neighbour = match at {
        Some(at) => match drawn.get(at + 1) {
            Some(next) => format!("above {}", titles.get(next).copied().unwrap_or(next)),
            None => match at.checked_sub(1).and_then(|before| drawn.get(before)) {
                Some(before) => format!("below {}", titles.get(before).copied().unwrap_or(before)),
                None => "to the top".to_owned(),
            },
        },
        None => "in the order".to_owned(),
    };
    format!("Moved {title} {neighbour} in your {place} \u{b7} {UNCHANGED}")
}

/// What the status bar says after a module is turned on or off.
pub(crate) fn switched_status(title: &str, enabled: bool) -> String {
    if enabled {
        format!("Enabled {title} \u{b7} {UNCHANGED}")
    } else {
        format!("Turned off {title} \u{b7} {UNCHANGED}")
    }
}

/// The footer after the last section, naming the modules that are off and offering Customize…:
/// `2 modules off: Perspective, Vignette`. `None` when none is.
pub(crate) fn off_footer(titles: &[&str]) -> Option<String> {
    let count = titles.len();
    (count > 0).then(|| {
        let noun = if count == 1 { "module" } else { "modules" };
        format!("{count} {noun} off: {}", titles.join(", "))
    })
}

/// The values of an off module's non-neutral `rows` as its kept band lists them, read-only: each
/// declared value control's label and value, in declared order, then any value no control labels,
/// by its name. A control declared under a variant is read like any other.
pub(crate) fn kept_values(
    module: &ModuleDescriptor,
    rows: &[&LayerDescription],
) -> Vec<(String, String)> {
    let mut values: Vec<(String, String)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        for control in with_variants(&module.controls) {
            let (action, parameter, label) = match control {
                Control::Number(number) => (&number.action, &number.parameter, &number.label),
                Control::Toggle(toggle) => (&toggle.action, &toggle.parameter, &toggle.label),
                Control::Choice(choice) => (&choice.action, &choice.parameter, &choice.label),
                _ => continue,
            };
            let Some(value) = row.values.get(parameter) else {
                continue;
            };
            if !seen.insert(parameter.clone()) {
                continue;
            }
            values.push((
                label.clone(),
                shown_value(module, control, action, parameter, value),
            ));
        }
        for (name, value) in &row.values {
            if seen.insert(name.clone()) && !value.is_object() && !value.is_array() {
                let label = module
                    .actions
                    .iter()
                    .find_map(|action| labelled_control(&module.controls, &action.id, name))
                    .unwrap_or(name);
                values.push((label.to_owned(), plain_value(value)));
            }
        }
    }
    values
}

/// One value as its control would show it: a number at its declared precision and unit, a choice
/// by its label, a toggle as On or Off.
fn shown_value(
    module: &ModuleDescriptor,
    control: &Control,
    action: &str,
    parameter: &str,
    value: &Value,
) -> String {
    let declared = module
        .action(action)
        .and_then(|declared| declared.parameter(parameter));
    match (control, value) {
        (Control::Number(_), Value::Number(number)) => {
            let number = number.as_f64().unwrap_or_default();
            let text = match declared.and_then(NumberSpec::of) {
                Some(spec) => spec.format(number),
                None => crate::state::number::number_text(number),
            };
            match declared.and_then(|declared| declared.unit.as_deref()) {
                Some("deg") => format!("{text}\u{b0}"),
                Some(unit) => format!("{text} {unit}"),
                None => text,
            }
        }
        (Control::Choice(choice), Value::String(option)) => declared
            .and_then(|declared| match &declared.kind {
                ParameterKind::Enum { options } => options.iter().position(|o| o == option),
                _ => None,
            })
            .and_then(|index| choice.labels.get(index).cloned())
            .unwrap_or_else(|| option.clone()),
        _ => plain_value(value),
    }
}

fn plain_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(on) => if *on { "On" } else { "Off" }.to_owned(),
        Value::Number(number) => {
            crate::state::number::number_text(number.as_f64().unwrap_or_default())
        }
        other => other.to_string(),
    }
}

/// One band's menu: Move up, Move down, Move to top and Move to bottom, each offered where it
/// changes the order; Turn off *Title*, absent for a locked module; Customize panel… and Copy as
/// JSON request. With the module manager off it offers only a kept band's Enable *Title*, which
/// repairs what is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BandMenu {
    pub(crate) module_id: String,
    /// Each move and whether it changes anything; none while the module manager is off.
    pub(crate) moves: Vec<(Move, bool)>,
    /// `Turn off Vignette`, or `None` for a locked module, one whose band is kept while off, or
    /// while the module manager is off.
    pub(crate) turn_off: Option<String>,
    /// `Enable Vignette` for a kept band, which is off.
    pub(crate) enable: Option<String>,
    /// Customize panel… and Copy as JSON request: the module manager is on.
    pub(crate) manager: bool,
    /// A layout write is in flight, so every item waits.
    pub(crate) enabled: bool,
}

impl BandMenu {
    /// It has an item to draw.
    pub(crate) fn offers(&self) -> bool {
        self.manager || self.enable.is_some()
    }
}

/// The menu of the band of `module`, drawn now among `shown` in drawn order, with the module
/// manager on or off.
pub(crate) fn band_menu(
    layout: &EffectiveLayout,
    module: &ModuleDescriptor,
    shown: &[&str],
    manager: bool,
) -> BandMenu {
    let order = layout.order(Toolset::Adjustments).unwrap_or_default();
    let on = layout.enabled(&module.id);
    BandMenu {
        module_id: module.id.clone(),
        moves: Move::ALL
            .into_iter()
            .filter(|_| manager)
            .map(|to| (to, moved(order, shown, &module.id, to).is_some()))
            .collect(),
        turn_off: (manager && on && layout.lock(&module.id).is_none())
            .then(|| format!("Turn off {}", module.title)),
        enable: (!on).then(|| format!("Enable {}", module.title)),
        manager,
        enabled: !layout.pending && layout.view().is_some(),
    }
}

/// The requests a band's menu sends, as its Copy as JSON request copies them: each move it offers
/// as the whole order, then Turn off or Enable.
pub(crate) fn band_requests(
    layout: &EffectiveLayout,
    module: &ModuleDescriptor,
    shown: &[&str],
) -> Value {
    let order = layout.order(Toolset::Adjustments).unwrap_or_default();
    let mut requests: Vec<Value> = Move::ALL
        .into_iter()
        .filter_map(|to| moved(order, shown, &module.id, to))
        .map(|order| {
            json!({
                "method": "module.order.set",
                "params": {"toolset": Toolset::Adjustments.as_str(), "order": order},
            })
        })
        .collect();
    if layout.lock(&module.id).is_none() {
        let method = if layout.enabled(&module.id) {
            "module.disable"
        } else {
            "module.enable"
        };
        requests.push(json!({"method": method, "params": {"module": module.id}}));
    }
    Value::Array(requests)
}

/// One row of the mode strip's ⋯ menu: a strip tool, its letter, and its check or lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StripTool {
    /// The module, or `None` for the host's Mask.
    pub(crate) module_id: Option<String>,
    pub(crate) title: String,
    pub(crate) letter: Option<char>,
    pub(crate) on: bool,
    /// Always on: the host's Mask, and a locked module.
    pub(crate) locked: bool,
}

/// The mode strip's ⋯ menu: Mask, then every module tool in the person's order, off ones
/// unchecked, then Customize tools….
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StripMenu {
    /// The strip draws its ⋯: the module manager is on.
    pub(crate) available: bool,
    pub(crate) open: bool,
    pub(crate) tools: Vec<StripTool>,
    /// A layout write is in flight, so no check can be pressed.
    pub(crate) enabled: bool,
}

/// The ⋯ menu's rows for `modules` under `layout`. Developer tools are listed only when the run
/// shows them, as the strip does. The menu is the module manager's: with it off the strip draws no
/// ⋯ and the menu is never open.
pub(crate) fn strip_menu(
    layout: &EffectiveLayout,
    modules: &[ModuleDescriptor],
    developer: bool,
    open: bool,
    manager: bool,
) -> StripMenu {
    if !manager {
        return StripMenu::default();
    }
    let mut tools = vec![StripTool {
        module_id: None,
        title: "Mask".to_owned(),
        letter: Some('M'),
        on: true,
        locked: true,
    }];
    let mut listed: Vec<&ModuleDescriptor> = modules
        .iter()
        .filter(|module| layout.placed(module, Toolset::Tools))
        .filter(|module| !module.developer || developer)
        .collect();
    // Before the layout is read, the strip's own default: brush workspaces before crop frames.
    listed.sort_by_key(|module| {
        !matches!(
            module.canvas,
            Some(luxforge_core::CanvasInteraction::BrushMask { .. })
        )
    });
    layout.sort(Toolset::Tools, &mut listed, |module| &module.id);
    tools.extend(listed.into_iter().map(|module| {
        StripTool {
            module_id: Some(module.id.clone()),
            title: module
                .canvas
                .as_ref()
                .map_or(module.title.as_str(), |canvas| canvas.title())
                .to_owned(),
            letter: layout.letter(&module.id).and_then(LetterClaim::held),
            on: layout.enabled(&module.id),
            locked: layout.lock(&module.id).is_some(),
        }
    }));
    StripMenu {
        available: true,
        open,
        tools,
        enabled: !layout.pending,
    }
}

/// The palette's layout entries: Customize panel…, and Turn off *Title* or Enable *Title* for every
/// module that is not locked, developer ones only when the run shows them. None while the module
/// manager is off.
pub(crate) fn palette_entries(inputs: &Inputs<'_>) -> Vec<(String, String, PaletteAction)> {
    if !crate::state::settings::manager_on(inputs.settings.flags.as_ref()) {
        return Vec::new();
    }
    let layout = inputs.layout;
    let mut entries = vec![(
        "Customize panel\u{2026}".to_owned(),
        "module.layout.read \u{b7} module.order.set".to_owned(),
        PaletteAction::Customize(Toolset::Adjustments),
    )];
    for module in inputs
        .modules
        .iter()
        .filter(|module| layout.lock(&module.id).is_none())
        .filter(|module| !module.developer || inputs.developer)
    {
        let on = layout.enabled(&module.id);
        entries.push(if on {
            (
                format!("Turn off {}", module.title),
                "module.disable".to_owned(),
                PaletteAction::SetEnabled {
                    module: module.id.clone(),
                    enabled: false,
                },
            )
        } else {
            (
                format!("Enable {}", module.title),
                "module.enable".to_owned(),
                PaletteAction::SetEnabled {
                    module: module.id.clone(),
                    enabled: true,
                },
            )
        });
    }
    entries
}

#[cfg(test)]
mod tests;
