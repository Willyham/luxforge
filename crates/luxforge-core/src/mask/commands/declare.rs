//! What the family publishes: the command table, the panel's controls, the host descriptor and the
//! canvas picks, each generated from the host's kind table where a kind declares it.
use super::{
    ADD_STROKE, COMPONENT, DELETE_STROKE, GeometryMethod, GeometryOp, LIST, MASK, MaskCommand,
    MaskOp, NAME, SAMPLE_INPUT, STROKE, SampleMethod, SampleOp, spoken,
};
use crate::mask::{
    REFINE_DEFAULT, REFINE_MAX, REFINE_MIN, component_band_control, component_parameters,
    component_sample_limit, component_sample_parameters, declared_geometry_kinds, rules,
    sampling_kinds,
};
use crate::{
    ActionDescriptor, CanvasInteraction, ChoiceStyle, Control, Mask, ModuleDescriptor, NumberStyle,
    ParameterDescriptor, ParameterKind,
    mask::{SIZE_MAX, SIZE_MIN},
    model::{COMPONENTS_PER_MASK, MASKS_PER_RECIPE},
    path::{POINTS_PER_STROKE, POSTED_POINTS_PER_STROKE},
};
use std::sync::LazyLock;

/// Every declared command, in the order the design's method table lists them.
pub fn all() -> &'static [MaskCommand] {
    &COMMANDS
}

/// The command one method name declares, or none. The registry's one action lookup
/// ([`crate::ModuleRegistry::resolve_action`]) answers a host action through it, so discovery,
/// dispatch, drafting and the registry's collision check cannot drift.
pub fn find(method: &str) -> Option<&'static MaskCommand> {
    COMMANDS.iter().find(|command| command.method == method)
}

/// The identity of the host descriptor the mask family is published as.
pub(crate) const HOST_MODULE: &str = "luxforge.masks";

/// The mask family as one host descriptor, in the shape a module's descriptor takes: its actions are
/// the commands, its queries the two reads and its controls the panel's widgets. `module.list` lists
/// it under `host`, so an agent discovers a mask command as it discovers a module action. The host
/// owns everything else about it: it declares no effect, is always available and is never
/// registered as a module.
pub(crate) fn descriptor() -> &'static ModuleDescriptor {
    &DESCRIPTOR
}

/// The read one method name declares — `mask.list` or `mask.sample-input` — or none.
pub fn find_query(method: &str) -> Option<&'static ActionDescriptor> {
    DESCRIPTOR.queries.iter().find(|query| query.id == method)
}

static DESCRIPTOR: LazyLock<ModuleDescriptor> = LazyLock::new(|| ModuleDescriptor {
    id: HOST_MODULE.to_owned(),
    title: "Masks".to_owned(),
    hint: Some(
        "Selections the adjustments of Basic, Presence and the colour mixer apply through"
            .to_owned(),
    ),
    actions: COMMANDS
        .iter()
        .map(|command| command.action.clone())
        .collect(),
    queries: vec![
        ActionDescriptor::new(
            LIST,
            "Masks",
            "every mask of one stack with its components, values, amount, invert and the \
                    layers bound to it; read-only, writes no history and emits no event",
        ),
        ActionDescriptor {
            parameters: vec![
                mask_parameter(true),
                ParameterDescriptor::pixel_coordinate("x")
                    .notes("the content column to read, in the stage the masked layer receives"),
                ParameterDescriptor::pixel_coordinate("y")
                    .notes("the content row to read, in the same stage"),
            ],
            ..ActionDescriptor::new(
                SAMPLE_INPUT,
                "Sample input",
                "the pixel the operation this mask modulates receives, at one content position, \
                    as linear-sRGB r, g and b, each an f32 value, with the renderer that read it, \
                    {record: gpu or reference, reason}, the reason naming why the GPU did not; read \
                    off the catalog owner by its tile service. Read-only: it writes no history and \
                    emits no event. It is where a canvas pick gets the colour a colour range's swatch is, because a \
                    range selection is evaluated on the operation's input while the frame a client \
                    can see holds that operation's output — so a colour read from the picture would \
                    be a different colour. The position is a pixel of the stage that operation's \
                    layer receives, and one outside it is refused rather than clamped",
            )
        },
    ],
    controls: CONTROLS.clone(),
    ..ModuleDescriptor::default()
});

/// The mask a command addresses, as the identity parameter every command that takes one declares.
fn mask_parameter(required: bool) -> ParameterDescriptor {
    ParameterDescriptor::identity(MASK, crate::IdentityKind::Mask)
        .required(required)
        .notes(if required {
            "the mask this command addresses"
        } else {
            "the mask this command addresses; without it the command draws a new one"
        })
}

/// The component inside that mask.
fn component_parameter(required: bool) -> ParameterDescriptor {
    ParameterDescriptor::identity(COMPONENT, crate::IdentityKind::Component)
        .required(required)
        .notes(if required {
            "the component inside that mask"
        } else {
            "the component inside that mask; without it the command puts a new one on the mask"
        })
}

/// The objects a command addresses — whether it takes a mask and whether it takes a component, each
/// required — followed by the values it sets.
fn addressed(
    mask: bool,
    component: bool,
    parameters: Vec<ParameterDescriptor>,
) -> Vec<ParameterDescriptor> {
    mask.then(|| mask_parameter(true))
        .into_iter()
        .chain(component.then(|| component_parameter(true)))
        .chain(parameters)
        .collect()
}

/// The generated sample command one operation on one sampling kind declares, or none when this
/// build does not sample that kind.
pub fn sample(op: SampleOp, kind: &str) -> Option<&'static MaskCommand> {
    COMMANDS.iter().find(|command| {
        matches!(command.op, MaskOp::Sample(samples) if samples.op == op && samples.kind == kind)
    })
}

/// The generated command one operation on one component kind declares, or none when this build does
/// not know the kind.
///
/// A client that has drawn a gradient knows what it did — create, add or patch — and which kind it
/// drew, and needs the method name for that pair. Spelling it out client-side would be a second copy
/// of the generation rule in [`geometry_commands`]; this is the same table, read by the same key.
pub fn geometry(op: GeometryOp, kind: &str) -> Option<&'static MaskCommand> {
    COMMANDS.iter().find(|command| {
        matches!(command.op, MaskOp::Geometry(geometry) if geometry.op == op && geometry.kind == kind)
    })
}

/// The canvas picks the host declares for its own commands: one per sampling kind, generated from
/// the kind table exactly as that kind's two sample methods are.
///
/// This is the host's side of the delivered pick machinery, in the delivered type
/// ([`CanvasInteraction`]) with the delivered meaning, because a mask is a host object and no module
/// declares one — so a pick that fills part of a mask had no way to be expressed until the
/// interaction admitted a host target (proposal P17 of `docs/design/range-study.md`). Each entry runs
/// [`SAMPLE_INPUT`] at the picked content pixel and submits the `r`, `g` and `b` it answers with to
/// `mask.add-<kind>-sample`, whose own declared parameters carry exactly those names — so the
/// delivered "every top-level number field of the result whose name is a parameter of the action"
/// rule needs no exception and the client maps nothing.
///
/// **The colour is never the client's.** It is read by the host from the stage the masked layer
/// receives and travels straight back into a host command; nothing on the way can decode a pixel,
/// because nothing on the way has one. That is the whole of what this declaration buys.
///
/// The mode a pick belongs to is its **action's** method name, which is unique by construction and is
/// what a client sets to enter the mode — there is no second table of mode names. No shortcut is
/// declared: a mask's pick is offered beside the component it fills, the way a module's picker control
/// is, and the canvas mode strip lists no pick mode at all.
pub fn canvas() -> &'static [CanvasInteraction] {
    &CANVAS
}

static CANVAS: LazyLock<Vec<CanvasInteraction>> = LazyLock::new(|| {
    sampling_kinds()
        .filter_map(|kind| {
            let add = sample(SampleOp::Add, kind)?;
            Some(CanvasInteraction::SampleApply {
                query: SAMPLE_INPUT.to_owned(),
                x: "x".to_owned(),
                y: "y".to_owned(),
                action: add.method.to_owned(),
                title: format!("Pick {}", spoken(kind)),
                shortcut: None,
                icon: None,
            })
        })
        .collect()
});

/// The canvas pick whose mode a client is in, or none when that mode is not one of the host's.
pub fn canvas_pick(mode: &str) -> Option<&'static CanvasInteraction> {
    CANVAS.iter().find(|pick| match pick {
        CanvasInteraction::SampleApply { action, .. } => action == mode,
        CanvasInteraction::PointPick { .. } | CanvasInteraction::CropFrame { .. } => false,
    })
}

/// The panel widgets of the mask commands, over the parameters those commands declare.
///
/// The same [`Control`] vocabulary a module declares, so a client generates a gradient's endpoint
/// fields, the whole-mask amount, the three-way mode selector and the two inversion toggles with the
/// widgets it already has, and no client invents an operation of its own. Layout — which row a
/// control sits in — belongs to the Masks panel and not here.
pub fn controls() -> &'static [Control] {
    &DESCRIPTOR.controls
}

fn modes() -> Vec<String> {
    rules::MODES
        .into_iter()
        .map(|mode| mode.as_str().to_owned())
        .collect()
}

/// One command, doing `op` under `method`, declaring the objects it addresses — a mask, and a
/// component inside it, each required when named — before the values it sets.
fn command(
    op: MaskOp,
    method: &'static str,
    title: &str,
    notes: &str,
    (mask, component): (bool, bool),
    patch: bool,
    parameters: Vec<ParameterDescriptor>,
) -> MaskCommand {
    MaskCommand {
        method,
        op,
        action: ActionDescriptor {
            id: method.to_owned(),
            title: title.to_owned(),
            notes: notes.to_owned(),
            // A mask label names the objects it touched and the mask it belongs to, which only the
            // plan knows; `plan` renders it at commit and the entry stores it, exactly as a
            // module's label is stored.
            patch,
            preset: false,
            analysis: None,
            parameters: addressed(mask, component, parameters),
        },
    }
}

/// The three geometry methods one component kind generates, each declaring exactly that kind's own
/// parameters.
///
/// The method names are leaked for the lifetime of the process, which is what lets a generated
/// command hold the `&'static str` identity every other command holds and a history entry store it
/// as a durable action id. There is one leak per kind per operation, at first use of the table.
fn geometry_commands(kind: &'static str) -> Vec<MaskCommand> {
    let mode = ParameterDescriptor::enumeration("mode", modes())
        .required(true)
        .notes("how this component joins the coverage the components before it composed");
    GeometryOp::all()
        .into_iter()
        .map(|op| {
            let method: &'static str = String::leak(format!("{}-{kind}", op.stem()));
            let patch = op == GeometryOp::Set;
            let mut parameters = match op {
                GeometryOp::Add => vec![mode.clone()],
                _ => Vec::new(),
            };
            parameters.extend(
                component_parameters(kind, !patch).expect("a kind from the host's own table"),
            );
            let (title, notes, addresses) = match op {
                GeometryOp::Create => (
                    format!("New {} mask", spoken(kind)),
                    format!(
                        "a new mask whose first component is an add {} component; a mask never \
                         exists empty, so the initial component is required and its mode is always \
                         add",
                        spoken(kind)
                    ),
                    (false, false),
                ),
                GeometryOp::Add => (
                    format!("Add {}", spoken(kind)),
                    format!(
                        "a second, third, … {} component of a mask, with its mode given explicitly \
                         rather than guessed from a modifier key",
                        spoken(kind)
                    ),
                    (true, false),
                ),
                GeometryOp::Set => (
                    format!("Update {}", spoken(kind)),
                    format!(
                        "a field patch over one {0} component's geometry; the fields the request \
                         names are validated and merged over the stored payload, and a component of \
                         any other kind is refused by name rather than patched with a {0}'s fields",
                        spoken(kind)
                    ),
                    (true, true),
                ),
            };
            command(
                MaskOp::Geometry(GeometryMethod { op, kind }),
                method,
                &title,
                &notes,
                addresses,
                patch,
                parameters,
            )
        })
        .collect()
}

/// The two sample methods one sampling kind generates: one adds a colour the canvas picked, one
/// removes the swatch at an index.
///
/// The method names are leaked for the lifetime of the process, exactly as the geometry methods'
/// are, so a generated command holds the `&'static str` identity every other command holds and a
/// history entry stores it as a durable action id.
fn sample_commands(kind: &'static str) -> Vec<MaskCommand> {
    let limit = component_sample_limit(kind).expect("a sampling kind from the host's own table");
    SampleOp::all()
        .into_iter()
        .map(|op| {
            let method: &'static str = String::leak(op.method(kind));
            let (title, notes, parameters) = match op {
                SampleOp::Add => (
                    format!("Sample {}", spoken(kind)),
                    format!(
                        "add one sampled colour to a {0} component, in linear sRGB in the domain of \
                         the operation this mask modulates — the value a pick on the canvas reads \
                         from the stage that operation receives. At most {limit} of them; sampling \
                         a colour the component already holds changes nothing, because the \
                         selection folds its samples by nearest and a duplicate is a no-op",
                        spoken(kind)
                    ),
                    component_sample_parameters(kind)
                        .expect("a sampling kind from the host's own table"),
                ),
                SampleOp::Delete => (
                    format!("Remove {} sample", spoken(kind)),
                    format!(
                        "remove one sampled colour of a {} component by its position in the list, \
                         so a swatch picked by accident is undone on its own rather than by \
                         clearing them all",
                        spoken(kind)
                    ),
                    vec![
                        ParameterDescriptor::integer("index", 0, limit as i64 - 1)
                            .required(true)
                            .notes(
                                "the sample's position in the component's list of sampled colours",
                            ),
                    ],
                ),
            };
            command(
                MaskOp::Sample(SampleMethod { op, kind }),
                method,
                &title,
                &notes,
                (true, true),
                false,
                parameters,
            )
        })
        .collect()
}

static COMMANDS: LazyLock<Vec<MaskCommand>> = LazyLock::new(|| {
    let mode = |required| {
        ParameterDescriptor::enumeration("mode", modes())
            .required(required)
            .notes("how this component joins the coverage the components before it composed")
    };
    let invert = |notes: &str| {
        ParameterDescriptor::boolean("invert")
            .required(true)
            .notes(notes)
    };
    let index = |limit: i64, notes: &str| {
        ParameterDescriptor::integer("index", 0, limit - 1)
            .required(true)
            .notes(notes)
    };
    let mut commands = vec![
        command(
            MaskOp::Delete,
            "mask.delete",
            "Delete mask",
            "delete a mask and the layers bound to it; destructive, so the history label and the result both name the layers it removed",
            (true, false),
            false,
            Vec::new(),
        ),
        command(
            MaskOp::Rename,
            "mask.rename",
            "Rename mask",
            "set a mask's display name; a name is a person's text and never an identity",
            (true, false),
            false,
            vec![
                ParameterDescriptor::string(NAME, crate::MAX_MASK_NAME)
                    .required(true)
                    .notes("the display name to set: printable, trimmed and not empty"),
            ],
        ),
        command(
            MaskOp::RenameComponent,
            "mask.rename-component",
            "Rename component",
            "set a component's display name; a name is a person's text and never an identity. \
             Unlike a mask's own name, a component's must be unique within its mask, because a \
             history label and the panel's own list read one by its name",
            (true, true),
            false,
            vec![
                ParameterDescriptor::string(NAME, crate::MAX_MASK_NAME)
                    .required(true)
                    .notes("the display name to set: printable, trimmed and not empty"),
            ],
        ),
        command(
            MaskOp::Duplicate,
            "mask.duplicate",
            "Duplicate mask",
            "a copy of a mask, its components and the layers bound to it, with new identities, placed after it; a mask without its adjustments is not a useful copy",
            (true, false),
            false,
            Vec::new(),
        ),
        command(
            MaskOp::SetAmount,
            "mask.set-amount",
            "Amount",
            "the whole-mask amount multiplying the composed coverage",
            (true, false),
            false,
            vec![
                ParameterDescriptor::number("amount", 0.0, Mask::FULL_AMOUNT)
                    .required(true)
                    .notes("0..=100, multiplying the composed coverage")
                    .step(1.0)
                    .precision(0),
            ],
        ),
        command(
            MaskOp::SetInvert,
            "mask.set-invert",
            "Invert mask",
            "invert the composed coverage of a whole mask, before its amount",
            (true, false),
            false,
            vec![invert("invert the composed coverage before the amount")],
        ),
        command(
            MaskOp::Reorder,
            "mask.reorder",
            "Move mask",
            "move a mask in the masks list and, with it, the masked layers of every effect, in one transaction; nothing else moves",
            (true, false),
            false,
            vec![index(
                MASKS_PER_RECIPE as i64,
                "the mask's new position in the masks list",
            )],
        ),
        command(
            MaskOp::SetComponentMode,
            "mask.set-component-mode",
            "Component mode",
            "change a component's role in the composition after the fact; the first component of a mask is always add",
            (true, true),
            false,
            vec![mode(true)],
        ),
        command(
            MaskOp::SetComponentInvert,
            "mask.set-component-invert",
            "Invert component",
            "invert one component's own coverage before it is combined",
            (true, true),
            false,
            vec![invert(
                "invert this component's coverage before it is combined",
            )],
        ),
        command(
            MaskOp::DeleteComponent,
            "mask.delete-component",
            "Delete component",
            "remove one component from a mask; a mask is never empty, so its last component is not deletable",
            (true, true),
            false,
            Vec::new(),
        ),
        command(
            MaskOp::ReorderComponent,
            "mask.reorder-component",
            "Move component",
            "move a component inside its mask; the composition reads the list in order",
            (true, true),
            false,
            vec![index(
                COMPONENTS_PER_MASK as i64,
                "the component's new position in its mask's component list",
            )],
        ),
    ];
    // The brush's own two. They are declared here rather than generated from the kind table because
    // a brush's geometry is drawn: there is no number a `mask.set-brush` could patch, and what these
    // carry is a path and the brush it was drawn with.
    //
    // `mask.add-stroke` is one command over three edits because painting is one gesture: where the
    // brush lands decides whether the stroke drew a mask, put a second brush on one, or added to the
    // brush already there, and the identities it names say which. The history label follows that — `Add brush`,
    // `Add subtract brush`, `Update Brush 1` — so undo walks back one stroke at a time while
    // rendering still sees one component whose strokes are already combined.
    commands.push(MaskCommand {
        method: ADD_STROKE,
        op: MaskOp::AddStroke,
        action: ActionDescriptor {
            id: ADD_STROKE.to_owned(),
            title: "Paint".to_owned(),
            notes: "one brush stroke: with no mask it draws a new one whose first component is an \
                    add brush, with a mask and no component it puts a further brush on that mask in \
                    the mode given, and with both it appends the stroke to that brush. The path is \
                    snapped to the stored grid and decimated there before it is stored, so the same \
                    posted path always produces the same stored stroke"
                .to_owned(),
            patch: false,
            preset: false,
            analysis: None,
            // Where the stroke lands is the two optional identities: neither draws a new mask,
            // a mask alone puts a further brush on it, and both append to that brush.
            parameters: vec![
                mask_parameter(false),
                component_parameter(false),
                // The declared bound is the posted path's, checked before decimation; the stroke
                // capture stores holds the decimated one to its own bound.
                ParameterDescriptor::points("points", 1, POSTED_POINTS_PER_STROKE)
                    .required(true)
                    .notes(format!(
                        "the stroke's path, in the content stage's normalized coordinates, in \
                         drawn order, raw or already decimated; a one-position path is a single \
                         dab. The stroke it decimates to holds at most {POINTS_PER_STROKE} \
                         positions"
                    )),
                // The one legal stroke radius, which capture, the stored-stroke recheck and the
                // brush's compile read too.
                ParameterDescriptor::number("size", SIZE_MIN, SIZE_MAX)
                    .required(true)
                    .notes(
                        "the brush's radius in mask-space units, one unit being the content \
                         stage's height on both axes, so a round brush is round at any aspect ratio",
                    )
                    .step(0.01)
                    .fine_step(0.002)
                    .precision(3),
                ParameterDescriptor::number("feather", 0.0, 100.0)
                    .required(true)
                    .notes(
                        "the ramp's width as a percentage of the radius; 0 is an explicit hard edge",
                    )
                    .step(5.0)
                    .precision(0),
                ParameterDescriptor::number("flow", 0.0, 100.0)
                    .required(true)
                    .notes(
                        "the coverage one pass of this stroke reaches, 0..=100. There is no \
                         density: its meaning depends on a build-up model along a single stroke, \
                         which would make coverage depend on stamp spacing and therefore on \
                         resolution",
                    )
                    .step(5.0)
                    .precision(0),
                ParameterDescriptor::boolean("erase").required(true).notes(
                    "this stroke removes coverage rather than adding it, for the whole of its life",
                ),
                ParameterDescriptor::boolean("limit_to_colour")
                    .required(true)
                    .notes(
                        "limit this stroke to the colour under the brush where it began: the host \
                         reads the pixel the operation this mask modulates receives at the \
                         stroke's first position, stores it with the stroke, and multiplies the \
                         stroke's coverage by the similarity to it. No colour is sent — a request \
                         names the limit, never the colour, so what is stored is always a colour \
                         the photograph has at that position. It is a per-pixel colour test and \
                         not Lightroom's Auto Mask: it knows nothing about edges or connectivity, \
                         so it also paints a matching colour anywhere else the stroke passes over. \
                         A mask no layer is bound to has no operation to read an input from and is \
                         refused by name",
                    )
                    .default(false),
                ParameterDescriptor::number("colour_refine", REFINE_MIN, REFINE_MAX)
                    .required(true)
                    .notes(
                        "how tight the colour limit is, on the colour range's own refine axis and \
                         with the same meaning: a higher refine is always a narrower hold, \
                         geometrically between a whole colour family at 0 and one flat patch at \
                         100. Ignored, and stored nowhere, by a stroke that carries no limit",
                    )
                    .unit("%")
                    .step(1.0)
                    .precision(0)
                    .fine_step(0.1)
                    .zero(REFINE_DEFAULT)
                    .default(REFINE_DEFAULT),
                ParameterDescriptor::enumeration("mode", modes()).notes(
                    "how the brush this stroke creates joins the components before it; only a \
                     stroke that makes a component on an existing mask may carry one, and a mask's \
                     first component is always add",
                ),
            ],
        },
    });
    commands.push(MaskCommand {
        method: DELETE_STROKE,
        op: MaskOp::DeleteStroke,
        action: ActionDescriptor {
            parameters: addressed(
                true,
                true,
                vec![
                    ParameterDescriptor::identity(STROKE, crate::IdentityKind::Stroke)
                        .required(true)
                        .notes(
                            "the stroke's content address, as its component's strokes field lists \
                             it",
                        ),
                ],
            ),
            ..ActionDescriptor::new(
                DELETE_STROKE,
                "Delete stroke",
                "remove one stroke from a brush component. A forward edit and not an undo: it \
                 appends one entry, so a stroke made ten entries ago goes while everything after \
                 it stays. A component's last stroke is not deletable; delete the component",
            )
        },
    });
    // The geometry methods, generated from the host's kind table: registering a kind with declared
    // geometry is what makes it creatable, addable and patchable, and nothing above has to be edited
    // for that to happen. A kind whose geometry is *drawn* declares no parameters and generates none
    // of these: there is no number a `mask.set-brush` could patch, and a create method over an empty
    // parameter list would advertise a component a gesture has to fill in afterwards.
    for kind in declared_geometry_kinds() {
        commands.extend(geometry_commands(kind));
    }
    // The sample methods, generated the same way from the same table: a kind that holds a list of
    // sampled colours becomes samplable by being registered with a sample limit, and nothing above
    // is edited for that to happen.
    for kind in sampling_kinds() {
        commands.extend(sample_commands(kind));
    }
    debug_assert!(
        {
            let mut seen: Vec<&str> = commands.iter().map(|command| command.method).collect();
            seen.sort_unstable();
            let before = seen.len();
            seen.dedup();
            seen.len() == before
        },
        "two mask commands share a method name, so `find` could only ever answer with one of them"
    );
    commands
});

static CONTROLS: LazyLock<Vec<Control>> = LazyLock::new(|| {
    let mut controls: Vec<Control> = vec![
        Control::number("mask.set-amount", "amount", "Amount").into(),
        Control::toggle("mask.set-invert", "invert", "Invert mask").into(),
        Control::choice("mask.set-component-mode", "mode", "Mode")
            .choice_style(ChoiceStyle::Segmented)
            .into(),
        Control::toggle("mask.set-component-invert", "invert", "Invert component").into(),
    ];
    // Every number a kind declares has a number field, generated from the same declarations the
    // patch method declares. The control's **action** names the kind it belongs to — a radius is a
    // `mask.set-radial` control and a gradient endpoint a `mask.set-linear` one — so a panel selects
    // the controls of the component it has open without a second table saying which are which, and a
    // kind registered later brings its own fields with it.
    for kind in declared_geometry_kinds() {
        let action = COMMANDS
            .iter()
            .find(|command| {
                command.op
                    == MaskOp::Geometry(GeometryMethod {
                        op: GeometryOp::Set,
                        kind,
                    })
            })
            .expect("every kind generates its patch method")
            .method;
        // A band's own two-thumb control comes first, over the same patch method, and its number
        // fields follow it: a typed value is exact, and the fields are what an agent reads too.
        controls.extend(component_band_control(kind, action));
        controls.extend(geometry_controls(
            action,
            component_parameters(kind, false).expect("a kind from the host's own table"),
        ));
    }
    controls
});

/// The number fields one kind's patch method `action` gets from its declared `parameters`: one per
/// parameter whose declared kind is a number, and none for any other.
///
/// A number field is the one widget this path generates, so it is bound only where it can edit what
/// the parameter declares. A geometry parameter of another kind — a polygon's `points` vertex list
/// — gets no control from here rather than a number field that could never hold its value; its
/// editor is the kind's own canvas gesture or a control of its own kind.
pub(super) fn geometry_controls(
    action: &'static str,
    parameters: Vec<ParameterDescriptor>,
) -> impl Iterator<Item = Control> {
    parameters
        .into_iter()
        .filter(|parameter| matches!(parameter.kind, ParameterKind::Number { .. }))
        .map(move |parameter| {
            Control::number(action, &parameter.name, control_label(&parameter.name))
                .number_style(NumberStyle::Field)
                .into()
        })
}

/// A stored field's name as a control shows it: `x0` is `X0`, `radius_x` is `Radius X`. Short
/// segments stay upper case because they are axis names, not words.
fn control_label(field: &str) -> String {
    field
        .split('_')
        .map(|word| {
            if word.len() <= 2 {
                word.to_uppercase()
            } else {
                let mut characters = word.chars();
                match characters.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
