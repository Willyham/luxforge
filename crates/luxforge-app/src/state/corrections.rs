//! The Corrections workspace's view state and its band model (`docs/design/corrections-workspace.md`).
//!
//! Nothing here names a tool. The module that paints corrections is whichever available module
//! declares a `brush-mask` canvas with an `edit` binding ([`brush_module`]); every action, query and
//! parameter the band and the canvas use is read from that declaration through
//! [`BrushBinding::declared`]. Every value a row shows comes from the module's own list query
//! (`query.<list>`, [`Listing`]); a name is the module's (`Heal 5`), never an identity the desktop
//! spells, and the limit is the one the module answers.
//!
//! Selection, hover, the set source and the New correction settings are this desktop's
//! own view state ([`CorrectionsPanel`]): none of them writes an entry or reaches `session.state`.
use crate::brush::{BrushBinding, BrushSettings, SourceChoice};
use crate::state::Inputs;
use luxforge_core::{AssetId, CanvasInteraction, EntryId, ModuleDescriptor, ParameterKind};
use serde::Deserialize;
use serde_json::Value;

/// The module whose `brush-mask` canvas declares an editable operation list, with its binding: the
/// one module the Corrections workspace drives. The first available one in registry order.
pub(crate) fn brush_module(
    modules: &[ModuleDescriptor],
) -> Option<(&ModuleDescriptor, BrushBinding)> {
    modules
        .iter()
        .filter(|module| module.is_available())
        .filter(|module| matches!(module.canvas, Some(CanvasInteraction::BrushMask { .. })))
        .find_map(|module| {
            let binding = BrushBinding::declared(module)?;
            binding.edit.is_some().then_some((module, binding))
        })
}

/// Whether `mode` is a brush workspace's mode.
pub(crate) fn is_brush_mode(modules: &[ModuleDescriptor], mode: &str) -> bool {
    brush_module(modules).is_some_and(|(module, _)| module.id == mode)
}

/// One stored operation as the module's list query answers it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(crate) struct Listed {
    pub(crate) operation: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) index: usize,
    #[serde(default)]
    pub(crate) points: Vec<[f64; 2]>,
    #[serde(default)]
    pub(crate) radius: f64,
    #[serde(default)]
    pub(crate) feather: f64,
    #[serde(default)]
    pub(crate) opacity: f64,
    #[serde(rename = "source-x", default)]
    pub(crate) source_x: Option<f64>,
    #[serde(rename = "source-y", default)]
    pub(crate) source_y: Option<f64>,
    #[serde(rename = "source-origin", default)]
    pub(crate) origin: Option<String>,
    #[serde(default)]
    pub(crate) stale: Option<Value>,
}

impl Listed {
    /// Where its anchor samples from, when it samples at all.
    pub(crate) fn source(&self) -> Option<[f64; 2]> {
        Some([self.source_x?, self.source_y?])
    }

    /// Its anchor: the first stored point, a spot's centre and a stroke's pin.
    pub(crate) fn anchor(&self) -> Option<[f64; 2]> {
        self.points.first().copied()
    }

    /// A spot is one stored point.
    pub(crate) fn spot(&self) -> bool {
        self.points.len() == 1
    }

    pub(crate) fn held(&self) -> crate::brush::HeldOperation {
        crate::brush::HeldOperation {
            operation: self.operation.clone(),
            points: self.points.clone(),
            radius: self.radius,
            source: self.source(),
        }
    }
}

/// The list query's answer: its limit, the next number and the operations in application order.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(crate) struct ListingBody {
    pub(crate) limit: usize,
    #[serde(default)]
    pub(crate) next: u64,
    pub(crate) operations: Vec<Listed>,
}

/// The list read for one entry of one photograph.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Listing {
    pub(crate) asset: AssetId,
    pub(crate) entry: EntryId,
    pub(crate) body: ListingBody,
}

impl Listing {
    pub(crate) fn find(&self, operation: &str) -> Option<&Listed> {
        self.body
            .operations
            .iter()
            .find(|listed| listed.operation == operation)
    }

    /// The operations top of the stack first, as the band lists them.
    pub(crate) fn top_down(&self) -> impl Iterator<Item = &Listed> {
        self.body.operations.iter().rev()
    }
}

/// The radius a brush starts at when its declaration gives none: a dust spot's size, in content
/// heights (about 100 px across on a 2,400 px-high photograph).
pub(crate) const DEFAULT_RADIUS: f64 = 0.02;

/// A row being dragged to a new place in the stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowDrag {
    pub(crate) operation: String,
    /// The display position (top first) of the row under the pointer.
    pub(crate) over: Option<usize>,
}

/// What a band slider's typed field edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypingTarget {
    /// One New correction slider, by its declared parameter.
    New(String),
    /// One slider of the selected row, by the operation and its declared parameter.
    Row { operation: String, name: String },
}

impl TypingTarget {
    /// The focus identity of the field's input, so beginning to type can focus it.
    pub(crate) fn field_id(&self) -> String {
        match self {
            Self::New(name) => format!("correction-new-{name}"),
            Self::Row { operation, name } => format!("correction-row-{operation}-{name}"),
        }
    }

    /// The target as a captured frame reports it.
    pub(crate) fn summary(&self) -> Value {
        match self {
            Self::New(name) => serde_json::json!({"new": name}),
            Self::Row { operation, name } => {
                serde_json::json!({"row": operation, "field": name})
            }
        }
    }
}

/// The band's one text field: what it edits, the text typed so far and the text it opened with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CorrectionTyping {
    pub(crate) target: TypingTarget,
    pub(crate) text: String,
    /// The value as the slider showed it when typing began: submitting it unchanged writes nothing.
    pub(crate) seed: String,
}

/// A refusal or a held stroke the band and canvas say something about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CorrectionNotice {
    /// The add's automatic source was refused; the stroke stays drafted until an `⌥`-click finishes
    /// it or Cancel discards it. `reason` is the core's words.
    NoSource { reason: String },
    /// The operation limit refused a press before any draft started.
    Limit { text: String, code: String },
}

/// The Corrections workspace's view state. Nothing here is a request until a gesture sends one.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CorrectionsPanel {
    /// The selected operation, by identity.
    pub(crate) selected: Option<String>,
    /// The operation a pin or row is hovered over.
    pub(crate) hovered: Option<String>,
    /// The settings the next press adds with; they persist in the window across photographs.
    pub(crate) settings: Option<BrushSettings>,
    /// Automatic, or the source an `⌥`-click set.
    pub(crate) source: SourceChoice,
    pub(crate) drag: Option<RowDrag>,
    /// The operation `/` last stepped and the rank it asks for next.
    pub(crate) ranking: Option<(String, usize)>,
    /// Where the last path this window placed in this mode ended, for `⇧`-click.
    pub(crate) last_end: Option<[f64; 2]>,
    pub(crate) notice: Option<CorrectionNotice>,
    pub(crate) listing: Option<Listing>,
    /// The operations listed before an add committed: the one the next listing gains is selected.
    pub(crate) select_added: Option<Vec<String>>,
    /// A row slider being dragged: its operation, field and value, drawn until its commit lands.
    pub(crate) row_drag: Option<(String, String, f64)>,
    /// A New correction slider being dragged.
    pub(crate) new_drag: Option<String>,
    /// A slider's value being typed.
    pub(crate) typing: Option<CorrectionTyping>,
}

impl CorrectionsPanel {
    /// The listing, when it describes the entry on screen.
    pub(crate) fn current<'a>(&'a self, displayed: Option<&EntryId>) -> Option<&'a Listing> {
        self.listing
            .as_ref()
            .filter(|listing| Some(&listing.entry) == displayed)
    }

    /// The settings the next press adds with, from the binding's defaults until they are changed.
    /// A radius the declaration gives no default is a dust spot's, [`DEFAULT_RADIUS`].
    pub(crate) fn settings_or(&self, binding: &BrushBinding) -> BrushSettings {
        self.settings.clone().unwrap_or_else(|| {
            let mut settings = binding.settings();
            let declared = binding
                .action
                .parameter(&binding.radius)
                .and_then(|parameter| parameter.default.as_ref())
                .is_some();
            if !declared {
                binding.set(&mut settings, &binding.radius.clone(), DEFAULT_RADIUS);
            }
            settings
        })
    }
}

/// What the open gesture shows in the band and the draft bar, as plain data.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DraftingCorrection {
    /// `Heal 7`: what the commit will name it, or the edited operation's name.
    pub(crate) name: String,
    /// The kind's option (`heal`).
    pub(crate) kind: String,
    /// What the gesture does, after the name: `painting`, `moving`, `moving its source`, `resizing`.
    pub(crate) verb: String,
    /// The source phrase: `from the set source 150 px above`, `source picked on release`.
    pub(crate) source: String,
    /// An add rather than an edit of an existing operation.
    pub(crate) adds: bool,
}

/// One slider of the band.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BandSlider {
    /// The declared parameter it sets.
    pub(crate) name: String,
    pub(crate) label: String,
    /// The rail position, 0..1.
    pub(crate) fraction: f64,
    pub(crate) text: String,
    /// What typing into its field edits.
    pub(crate) target: TypingTarget,
    /// The text being typed into its field, as typed, while it is.
    pub(crate) typing: Option<String>,
    /// Why the typed text cannot be applied, when it cannot.
    pub(crate) invalid: Option<String>,
}

/// One kind the band's controls offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KindOption {
    pub(crate) option: String,
    pub(crate) title: String,
}

/// The selected row's own settings.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExpandedRow {
    pub(crate) kind: Option<usize>,
    pub(crate) fields: Vec<BandSlider>,
    /// `Auto · 175 px below`.
    pub(crate) source: String,
}

/// One row of the stack, top first.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CorrectionRow {
    pub(crate) operation: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    /// `spot · auto`, `stroke · set source`.
    pub(crate) description: String,
    pub(crate) opacity: String,
    /// Its place in application order (0 is the bottom).
    pub(crate) index: usize,
    /// Its place in the list, top first.
    pub(crate) position: usize,
    pub(crate) selected: bool,
    pub(crate) hovered: bool,
    pub(crate) menu_open: bool,
    pub(crate) expanded: Option<ExpandedRow>,
}

/// The Corrections band, as the tools panel draws it while the mode is active.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CorrectionsModel {
    /// The mode is on screen, so the band is drawn under the histogram.
    pub(crate) active: bool,
    pub(crate) module: String,
    pub(crate) title: String,
    pub(crate) count: usize,
    pub(crate) hint: String,
    pub(crate) enabled: bool,
    pub(crate) disabled_reason: Option<String>,
    pub(crate) can_reset: bool,
    pub(crate) kinds: Vec<KindOption>,
    pub(crate) new_kind: usize,
    pub(crate) new_fields: Vec<BandSlider>,
    pub(crate) source_set: bool,
    /// The accent part of the source note, when a source is set (`150 px above`).
    pub(crate) source_offset: Option<String>,
    pub(crate) source_note: String,
    pub(crate) rows: Vec<CorrectionRow>,
    /// The drafting add's italic row at the top: its name.
    pub(crate) drafting: Option<(String, String)>,
    pub(crate) footer: String,
    /// The list is waiting for its read of the entry on screen.
    pub(crate) waiting: bool,
    pub(crate) drag_from: Option<usize>,
    pub(crate) drag_over: Option<usize>,
    pub(crate) notice: Option<CorrectionNotice>,
    pub(crate) pins_hidden: bool,
    /// The mode the band's close leaves for: the pointer.
    pub(crate) leave_mode: String,
    /// Remove, the kind control's fill segment, when a module fills ([`crate::state::remove`]).
    pub(crate) remove: Option<crate::state::remove::RemoveModel>,
}

/// The no-source notice's second line: how to finish, and the core's reason when it is not the
/// no-source answer itself (a refused search says why).
pub(crate) fn notice_detail(reason: &str) -> String {
    if reason.starts_with("No automatic source fits") {
        "\u{2325}-click one to finish".to_owned()
    } else {
        format!("\u{2325}-click one to finish \u{b7} {reason}")
    }
}

/// A kind option's title: its first letter upper-cased.
pub(crate) fn kind_title(option: &str) -> String {
    let mut characters = option.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// The options the binding's `mode` declares.
pub(crate) fn kind_options(binding: &BrushBinding) -> Vec<KindOption> {
    binding
        .mode
        .as_deref()
        .and_then(|name| binding.action.parameter(name))
        .and_then(|declared| match &declared.kind {
            ParameterKind::Enum { options } => Some(options.clone()),
            _ => None,
        })
        .unwrap_or_default()
        .into_iter()
        .map(|option| KindOption {
            title: kind_title(&option),
            option,
        })
        .collect()
}

/// The radius parameter's declared range.
pub(crate) fn radius_range(binding: &BrushBinding) -> (f64, f64) {
    match binding
        .action
        .parameter(&binding.radius)
        .map(|declared| &declared.kind)
    {
        Some(ParameterKind::Number { min, max }) if *min > 0.0 && max > min => (*min, *max),
        _ => (0.0005, 0.25),
    }
}

/// A radius as the Size rail's position: logarithmic over the declared range, so a dust spot and a
/// quarter of the frame are both reachable.
pub(crate) fn size_fraction(binding: &BrushBinding, radius: f64) -> f64 {
    let (min, max) = radius_range(binding);
    ((radius.max(min) / min).ln() / (max / min).ln()).clamp(0.0, 1.0)
}

/// The radius at a Size rail position.
pub(crate) fn size_at(binding: &BrushBinding, fraction: f64) -> f64 {
    let (min, max) = radius_range(binding);
    min * (max / min).powf(fraction.clamp(0.0, 1.0))
}

/// A radius as Size's display: the brush diameter in upright-content pixels, `2 × radius × content
/// height`, so it does not change with zoom or crop. Without a content stage the radius is shown.
pub(crate) fn size_text(radius: f64, content: Option<(u32, u32)>) -> String {
    match content {
        Some((_, height)) => format!("{:.0} px", 2.0 * radius * f64::from(height)),
        None => format!("{radius:.4}"),
    }
}

/// An offset from an anchor to a source in content pixels, said the way the band says it: the
/// distance and its dominant direction, `150 px above`.
pub(crate) fn offset_phrase(
    anchor: [f64; 2],
    source: [f64; 2],
    content: Option<(u32, u32)>,
) -> String {
    let (width, height) = content.map_or((1.0, 1.0), |(w, h)| (f64::from(w), f64::from(h)));
    let dx = (source[0] - anchor[0]) * width;
    let dy = (source[1] - anchor[1]) * height;
    let distance = dx.hypot(dy).round();
    let direction = if dy.abs() >= dx.abs() {
        if dy < 0.0 { "above" } else { "below" }
    } else if dx < 0.0 {
        "left"
    } else {
        "right"
    };
    if content.is_some() {
        format!("{distance:.0} px {direction}")
    } else {
        format!("{:.3} {direction}", dx.hypot(dy))
    }
}

/// A set source as a phrase, measured or not yet: `150 px above` once a press measured it, or
/// `at the ⌥-click` before one did.
pub(crate) fn source_phrase(choice: &SourceChoice, content: Option<(u32, u32)>) -> Option<String> {
    match *choice {
        SourceChoice::Auto => None,
        SourceChoice::Pending(_) => Some("at the \u{2325}-click".to_owned()),
        SourceChoice::Offset([dx, dy]) => Some(offset_phrase([0.0, 0.0], [dx, dy], content)),
    }
}

/// `spot · auto`, `stroke · set source`.
fn description(listed: &Listed) -> String {
    if listed.kind == "patch" {
        return patch_description(listed.stale.as_ref());
    }
    let shape = if listed.spot() { "spot" } else { "stroke" };
    let origin = match listed.origin.as_deref() {
        Some("set") => "set source",
        Some("auto") => "auto",
        Some(other) => other,
        None => "patch",
    };
    format!("{shape} \u{b7} {origin}")
}

/// A frozen patch's row: `frozen patch`, or why it is stale (`docs/design/corrections.md`, "Input
/// identity"): what changed since it was generated, the reasons `query.corrections` names, in
/// order. A stale patch still renders.
fn patch_description(stale: Option<&Value>) -> String {
    let reasons: Vec<&str> = stale
        .and_then(|stale| stale.get("reasons"))
        .and_then(Value::as_array)
        .map(|reasons| reasons.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if reasons.is_empty() {
        return "frozen patch".into();
    }
    let changed: Vec<&str> = reasons.iter().map(|reason| stale_phrase(reason)).collect();
    format!("stale \u{b7} {} since it was generated", changed.join(", "))
}

/// What changed for a frozen patch to be stale, for a stale reason `query.corrections` and
/// `recipe.describe` name: `a correction below it changed`.
pub(crate) fn stale_phrase(reason: &str) -> &str {
    match reason {
        "source" => "the original changed",
        "dimensions" => "the photograph's dimensions changed",
        "development" => "the RAW development changed",
        "earlier-correction" => "a correction below it changed",
        other => other,
    }
}

/// The pin, the band and the status bar's name for the add a press would make now.
pub(crate) fn next_name(listing: Option<&Listing>, kind: &str) -> String {
    match listing {
        Some(listing) => format!("{} {}", kind_title(kind), listing.body.next.max(1)),
        None => kind_title(kind),
    }
}

/// The label the band gives a bound number.
fn field_label<'a>(binding: &BrushBinding, name: &str) -> &'a str {
    if name == binding.radius {
        "Size"
    } else if binding.feather.as_deref() == Some(name) {
        "Feather"
    } else {
        "Opacity"
    }
}

/// The value the text typed into a band slider's field describes, as the declared parameter stores
/// it: Size is the brush diameter in content pixels (a radius when no content stage is known),
/// Feather and Opacity are whole numbers over their declared range. Out of range or not a number
/// says why, in the words the field's caption shows, rather than clamping a guess.
pub(crate) fn typed_value(
    binding: &BrushBinding,
    name: &str,
    text: &str,
    content: Option<(u32, u32)>,
) -> Result<f64, String> {
    let label = field_label(binding, name);
    let digits = text.trim().trim_end_matches("px").trim();
    let Some(typed) = crate::state::masks::parse_number(digits) else {
        return Err("Type a number".to_owned());
    };
    if name == binding.radius {
        let (min, max) = radius_range(binding);
        return match content {
            Some((_, height)) => {
                let height = f64::from(height);
                let radius = typed / (2.0 * height);
                // Half a displayed pixel either side: the field shows whole pixels.
                let slack = 0.25 / height;
                if radius < min - slack || radius > max + slack {
                    Err(format!(
                        "{label} is {:.0} to {:.0} px",
                        (2.0 * min * height).ceil(),
                        (2.0 * max * height).floor()
                    ))
                } else {
                    Ok(radius.clamp(min, max))
                }
            }
            None if typed < min || typed > max => Err(format!("{label} is {min} to {max}")),
            None => Ok(typed),
        };
    }
    let (min, max) = match binding
        .action
        .parameter(name)
        .map(|declared| &declared.kind)
    {
        Some(ParameterKind::Number { min, max }) => (*min, *max),
        Some(ParameterKind::Integer { min, max }) => (*min as f64, *max as f64),
        _ => (0.0, 100.0),
    };
    let value = typed.round();
    if value < min || value > max {
        Err(format!("{label} is {min:.0} to {max:.0}"))
    } else {
        Ok(value)
    }
}

/// The band's sliders for one set of settings. `scope` is the selected row's operation, or `None`
/// for the New correction group; `typing` is the band's field when it edits one of these.
fn sliders(
    binding: &BrushBinding,
    radius: f64,
    feather: Option<f64>,
    opacity: Option<f64>,
    content: Option<(u32, u32)>,
    scope: Option<&str>,
    typing: Option<&CorrectionTyping>,
) -> Vec<BandSlider> {
    let slider = |name: &str, label: &str, fraction: f64, text: String| {
        let target = match scope {
            Some(operation) => TypingTarget::Row {
                operation: operation.to_owned(),
                name: name.to_owned(),
            },
            None => TypingTarget::New(name.to_owned()),
        };
        let typed = typing.filter(|typing| typing.target == target);
        BandSlider {
            name: name.to_owned(),
            label: label.to_owned(),
            fraction,
            text,
            invalid: typed.and_then(|typed| typed_value(binding, name, &typed.text, content).err()),
            typing: typed.map(|typed| typed.text.clone()),
            target,
        }
    };
    let mut fields = vec![slider(
        &binding.radius,
        "Size",
        size_fraction(binding, radius),
        size_text(radius, content),
    )];
    for (name, label, value) in [
        (&binding.feather, "Feather", feather),
        (&binding.opacity, "Opacity", opacity),
    ] {
        if let (Some(name), Some(value)) = (name, value) {
            fields.push(slider(
                name,
                label,
                (value / 100.0).clamp(0.0, 1.0),
                format!("{value:.0}"),
            ));
        }
    }
    fields
}

/// The Corrections band for these inputs.
pub(crate) fn derive(inputs: &Inputs<'_>) -> CorrectionsModel {
    let Some((module, binding)) = brush_module(inputs.modules) else {
        return CorrectionsModel::default();
    };
    let panel = inputs.corrections;
    let active = inputs.session.workspace.mode == module.id && inputs.document.state.is_some();
    let listing = panel.current(inputs.document.display_entry.as_ref());
    let content = inputs.correction_content;
    let operations = listing.map_or(&[][..], |listing| listing.body.operations.as_slice());
    let count = operations.len();
    let disabled_reason = inputs.edit_refusal.clone();
    let enabled = disabled_reason.is_none();
    let kinds = kind_options(&binding);
    let settings = panel.settings_or(&binding);
    let new_kind = settings
        .mode
        .as_deref()
        .and_then(|mode| kinds.iter().position(|kind| kind.option == mode))
        .unwrap_or(0);
    let source_offset = source_phrase(&panel.source, content);
    let source_set = source_offset.is_some();
    let selected = panel
        .selected
        .as_ref()
        .filter(|id| operations.iter().any(|listed| &&listed.operation == id));
    let drag_from = panel.drag.as_ref().and_then(|drag| {
        operations
            .iter()
            .rev()
            .position(|listed| listed.operation == drag.operation)
    });
    let menu = inputs.view_state.menu.as_ref();
    let rows = operations
        .iter()
        .rev()
        .enumerate()
        .map(|(position, listed)| {
            let is_selected = selected == Some(&listed.operation);
            let drag = panel
                .row_drag
                .as_ref()
                .filter(|(operation, _, _)| operation == &listed.operation);
            let value = |name: &Option<String>, stored: f64| {
                Some(match (name, drag) {
                    (Some(name), Some((_, field, value))) if field == name => *value,
                    _ => stored,
                })
            };
            let radius = match drag {
                Some((_, field, value)) if field == &binding.radius => *value,
                _ => listed.radius,
            };
            let feather = value(&binding.feather, listed.feather);
            let opacity = value(&binding.opacity, listed.opacity);
            CorrectionRow {
                operation: listed.operation.clone(),
                name: listed.name.clone(),
                kind: listed.kind.clone(),
                description: description(listed),
                opacity: format!("{:.0}", listed.opacity),
                index: listed.index,
                position,
                selected: is_selected,
                hovered: panel.hovered.as_ref() == Some(&listed.operation),
                menu_open: menu
                    == Some(&crate::state::MenuTarget::Correction(
                        listed.operation.clone(),
                    )),
                expanded: is_selected.then(|| ExpandedRow {
                    kind: kinds.iter().position(|kind| kind.option == listed.kind),
                    fields: sliders(
                        &binding,
                        radius,
                        feather,
                        opacity,
                        content,
                        Some(&listed.operation),
                        panel.typing.as_ref(),
                    ),
                    source: match (listed.anchor(), listed.source()) {
                        (Some(anchor), Some(source)) => format!(
                            "{} \u{b7} {}",
                            match listed.origin.as_deref() {
                                Some("set") => "Set",
                                _ => "Auto",
                            },
                            offset_phrase(anchor, source, content)
                        ),
                        _ => "None".into(),
                    },
                }),
            }
        })
        .collect();
    let drafting = inputs
        .correction_draft
        .as_ref()
        .filter(|draft| draft.adds)
        .map(|draft| (draft.name.clone(), draft.kind.clone()));
    let limit = listing.map(|listing| listing.body.limit);
    CorrectionsModel {
        active,
        module: module.id.clone(),
        title: module.title.clone(),
        count,
        hint: format!(
            "{count} {} \u{b7} Esc leaves",
            if count == 1 {
                "correction"
            } else {
                "corrections"
            }
        ),
        enabled,
        disabled_reason,
        can_reset: enabled && count > 0,
        new_fields: sliders(
            &binding,
            settings.radius,
            settings.feather,
            settings.opacity,
            content,
            None,
            panel.typing.as_ref(),
        ),
        kinds,
        new_kind,
        source_set,
        source_note: if source_set {
            "used until Auto or esc".into()
        } else {
            "Picked from the surroundings on release \u{b7} \u{2325} click sets one".into()
        },
        source_offset,
        rows,
        drafting,
        footer: match limit {
            Some(limit) => format!("{count} of {limit}"),
            None => String::new(),
        },
        waiting: active && listing.is_none(),
        drag_from,
        drag_over: panel.drag.as_ref().and_then(|drag| drag.over),
        notice: panel.notice.clone(),
        pins_hidden: !inputs.session.workspace.mask_handles,
        leave_mode: luxforge_core::POINTER_MODE.to_owned(),
        remove: crate::state::remove::derive(inputs, enabled),
    }
}

impl CorrectionsModel {
    /// What a captured frame records of the band.
    pub(crate) fn summary(&self) -> Value {
        serde_json::json!({
            "active": self.active,
            "count": self.count,
            "footer": self.footer,
            "new_kind": self.kinds.get(self.new_kind).map(|kind| kind.option.clone()),
            "new_fields": self.new_fields.iter().map(|field| serde_json::json!({"name": field.name, "text": field.text, "typing": field.typing, "invalid": field.invalid})).collect::<Vec<_>>(),
            "source_set": self.source_set,
            "source_offset": self.source_offset,
            "rows": self.rows.iter().map(|row| serde_json::json!({
                "operation": row.operation,
                "name": row.name,
                "description": row.description,
                "opacity": row.opacity,
                "index": row.index,
                "selected": row.selected,
                "source": row.expanded.as_ref().map(|expanded| expanded.source.clone()),
                "fields": row.expanded.as_ref().map(|expanded| expanded.fields.iter().map(|field| serde_json::json!({"name": field.name, "text": field.text, "typing": field.typing, "invalid": field.invalid})).collect::<Vec<_>>()),
            })).collect::<Vec<_>>(),
            "drafting": self.drafting.as_ref().map(|(name, _)| name.clone()),
            "notice": self.notice.as_ref().map(|notice| match notice {
                CorrectionNotice::NoSource { reason } => serde_json::json!({"kind": "no-source", "reason": reason}),
                CorrectionNotice::Limit { text, code } => serde_json::json!({"kind": "limit", "text": text, "code": code}),
            }),
            "pins_hidden": self.pins_hidden,
            "waiting": self.waiting,
            "remove": self.remove.as_ref().map(crate::state::remove::RemoveModel::summary),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_said_by_distance_and_dominant_direction() {
        let content = Some((4000, 3000));
        assert_eq!(
            offset_phrase([0.5, 0.5], [0.5, 0.45], content),
            "150 px above"
        );
        assert_eq!(
            offset_phrase([0.5, 0.5], [0.476, 0.5], content),
            "96 px left"
        );
        assert_eq!(
            offset_phrase([0.5, 0.5], [0.5, 0.55], content),
            "150 px below"
        );
        assert_eq!(
            offset_phrase([0.5, 0.5], [0.6, 0.5], content),
            "400 px right"
        );
    }

    #[test]
    fn size_is_the_diameter_in_content_pixels_on_a_logarithmic_rail() {
        assert_eq!(size_text(0.016, Some((6000, 3000))), "96 px");
        let binding = BrushBinding::declared(&crate::brush::testing::descriptor()).unwrap();
        for radius in [0.0005, 0.003, 0.05, 0.25] {
            let back = size_at(&binding, size_fraction(&binding, radius));
            assert!((back - radius).abs() < 1e-9, "{radius} {back}");
        }
        assert_eq!(size_fraction(&binding, 0.0005), 0.0);
        assert_eq!(size_fraction(&binding, 0.25), 1.0);
    }

    #[test]
    fn kinds_are_titled_from_their_options() {
        assert_eq!(kind_title("heal"), "Heal");
        assert_eq!(kind_title("clone"), "Clone");
    }
}

#[cfg(test)]
mod patch_row_tests {
    use super::patch_description;
    use serde_json::json;

    #[test]
    fn a_patch_row_says_why_it_is_stale() {
        assert_eq!(patch_description(None), "frozen patch");
        assert_eq!(
            patch_description(Some(&json!({"reasons": ["development"]}))),
            "stale \u{b7} the RAW development changed since it was generated"
        );
        assert_eq!(
            patch_description(Some(&json!({"reasons": ["source", "earlier-correction"]}))),
            "stale \u{b7} the original changed, a correction below it changed since it was \
             generated"
        );
    }
}
