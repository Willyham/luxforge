//! The `range-corpus` smoke scenario: range selections picked on a real photograph, which they had
//! never been before ([range masks](../../docs/design/range-masks.md#outstanding-work)).
//!
//! One launch over the `--source` photograph. With the experiment on and Mask mode entered, each
//! pick is a new mask of its kind, created from New mask with the eyedropper armed and nothing
//! bound; the pick itself sent through the API (`mask.pick-<kind>`, as a script or an agent sends
//! it, a click without `width` and `height` and a drag with them); the same rectangle asked of
//! `mask.sample-input-area`, whose answer names the renderer that read it; and the mask's own
//! coverage drawn on black, which is the capture of what the pick selected.
//!
//! The picks are declared per photograph of the qualification corpus, by file name, aimed at the
//! skies, skin, foliage and neutrals that photograph holds; any other photograph gets the generic
//! picks, a whole-frame luminance drag and a colour click at the centre. What a photograph does
//! not hold is unavailable, never a pass: the report says which kinds of subject a run covered.
//!
//! It proves that a pick commits one entry that sets the component from the photograph, that the
//! GPU reads the area at the unbound mask's input site, and records the coverage each pick gives.
//! It judges no selection's quality: the captures are for a person to read.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_evidence::{self as script, MaskStep, WorkspaceStep};

pub const SCENARIO: &str = "range-corpus";

const LUMINANCE_RANGE: &str = "luminance-range";
const COLOUR_RANGE: &str = "colour-range";

/// One pick: the subject it is aimed at, the kind it picks into and where, in normalized content
/// coordinates — `[x, y]` for a click, `[x, y, width, height]` for a drag.
struct Pick {
    subject: &'static str,
    kind: &'static str,
    at: &'static [f64],
}

const fn click(subject: &'static str, kind: &'static str, at: &'static [f64]) -> Pick {
    Pick { subject, kind, at }
}

/// The picks of each corpus photograph, by its file name, as `fixtures/preview/corpus.json`'s RAW
/// sources name them in the private manifest.
const PICKS: &[(&str, &[Pick])] = &[
    (
        // A boy on a bamboo bridge under a hazy sky, portrait.
        "nikon_z6.nef",
        &[
            click("sky", COLOUR_RANGE, &[0.55, 0.03, 0.35, 0.15]),
            click("skin", COLOUR_RANGE, &[0.405, 0.48, 0.035, 0.02]),
            click("neutral", COLOUR_RANGE, &[0.43, 0.545]),
            click("foliage", COLOUR_RANGE, &[0.93, 0.415]),
            click("sky", LUMINANCE_RANGE, &[0.7, 0.1]),
            click("whole frame", LUMINANCE_RANGE, &[0.0, 0.0, 1.0, 1.0]),
        ],
    ),
    (
        // Sunflowers against a blurred garden and a grey counter.
        "fujifilm_x100vi.raf",
        &[
            click("petals", COLOUR_RANGE, &[0.70, 0.47]),
            click("foliage", COLOUR_RANGE, &[0.85, 0.375]),
            click("neutral", COLOUR_RANGE, &[0.1, 0.82, 0.3, 0.13]),
            click("flower centre", LUMINANCE_RANGE, &[0.52, 0.47]),
            click("whole frame", COLOUR_RANGE, &[0.0, 0.0, 1.0, 1.0]),
        ],
    ),
    (
        // An aerial view of a lagoon: forest, turquoise shallows, deep water and grey rock.
        "mavic_air_2s.dng",
        &[
            click("foliage", COLOUR_RANGE, &[0.4, 0.02, 0.2, 0.1]),
            click("shallow water", COLOUR_RANGE, &[0.75, 0.375]),
            click("deep water", COLOUR_RANGE, &[0.85, 0.70]),
            click("bright shallows", LUMINANCE_RANGE, &[0.25, 0.62, 0.2, 0.06]),
            click("whole frame", LUMINANCE_RANGE, &[0.0, 0.0, 1.0, 1.0]),
        ],
    ),
];

/// The picks any other photograph gets.
const GENERIC: &[Pick] = &[
    click("whole frame", LUMINANCE_RANGE, &[0.0, 0.0, 1.0, 1.0]),
    click("centre", COLOUR_RANGE, &[0.5, 0.5]),
];

/// The picks declared for `source`, by its file name.
fn picks(source: &Path) -> &'static [Pick] {
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    PICKS
        .iter()
        .find(|(file, _)| *file == name)
        .map_or(GENERIC, |(_, picks)| picks)
}

/// The steps the checks read, by pick: the pick, its area read and its coverage.
fn names(index: usize) -> [String; 3] {
    let n = index + 1;
    [
        format!("pick-{n}"),
        format!("area-{n}"),
        format!("coverage-{n}"),
    ]
}

/// The pick's command parameters, the mask named as the host names it and its one component.
fn parameters(index: usize, pick: &Pick) -> Value {
    let component = if pick.kind == LUMINANCE_RANGE {
        "Luminance range 1"
    } else {
        "Colour range 1"
    };
    let mut params = json!({"mask": {"name": format!("Mask {}", index + 1)},
                            "component": {"name": component},
                            "x": pick.at[0], "y": pick.at[1]});
    if let [_, _, width, height] = pick.at {
        params["width"] = json!(width);
        params["height"] = json!(height);
    }
    params
}

/// The area `mask.sample-input-area` reads for a pick: a drag's rectangle, or the one pixel of a
/// click, asked as a rectangle no wider than a pixel of any corpus photograph.
fn area(index: usize, pick: &Pick) -> Value {
    let mut params = json!({"mask": {"name": format!("Mask {}", index + 1)},
                            "x": pick.at[0], "y": pick.at[1]});
    let (width, height) = match pick.at {
        [_, _, width, height] => (*width, *height),
        _ => (1e-5, 1e-5),
    };
    params["width"] = json!(width);
    params["height"] = json!(height);
    params
}

pub fn plan(sources: &[PathBuf]) -> Plan {
    let mut steps = vec![
        Step::opened("opened"),
        crate::mask_range_smoke::range_experiment("range-experiment"),
        Step::new("mask-mode", WorkspaceStep::default().mode("mask"))
            .commits(0)
            .mode("mask"),
    ];
    for (index, pick) in sources
        .first()
        .map_or(GENERIC, |s| picks(s))
        .iter()
        .enumerate()
    {
        let [picked, read, coverage] = names(index);
        steps.extend([
            // A new mask of the kind, which selects its component and arms the eyedropper.
            Step::new(
                format!("mask-{}", index + 1),
                MaskStep::New(pick.kind.into()),
            )
            .commits(1)
            .picker(Some(pick.kind)),
            Step::new(
                picked,
                script::Step::call(format!("mask.pick-{}", pick.kind), parameters(index, pick)),
            )
            .commits(1),
            Step::new(
                read,
                script::Step::call("mask.sample-input-area", area(index, pick)),
            )
            .commits(0),
            Step::new(
                coverage,
                WorkspaceStep::default().mask_overlay("mask-on-black"),
            )
            .commits(0),
        ]);
    }
    Plan::new(steps)
}

/// What a pick stored in its component, as the open row reports it: a band's four fields, or the
/// colours sampled.
fn stored(frame: &Frame, kind: &str) -> Result<Value> {
    let component = frame.component(0)?;
    if kind == LUMINANCE_RANGE {
        let fields = &component["fields"];
        ensure(
            !(fields["low"] == json!(0.0) && fields["high"] == json!(100.0)),
            format!("A luminance pick left the band at the whole range: {fields}"),
        )?;
        Ok(fields.clone())
    } else {
        let samples = component["samples"]
            .as_array()
            .ok_or("The component records no sample list")?;
        ensure(
            samples.len() == 1,
            format!("A colour pick stored {} samples", samples.len()),
        )?;
        Ok(json!({"samples": samples, "refine": component["fields"]["refine"]}))
    }
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let source = run
        .sources()
        .first()
        .cloned()
        .ok_or("The run opened no photograph")?;
    let mut checks = Checks::new();
    let mut rows = Vec::new();
    for (index, pick) in picks(&source).iter().enumerate() {
        let [picked, read, coverage] = names(index);
        let what = format!("pick {}: {} by {}", index + 1, pick.subject, pick.kind);
        let stored =
            stored(launch.at(&picked)?, pick.kind).map_err(|error| format!("{what}: {error}"))?;
        let answer = &launch.at(&read)?["step"]["result"];
        ensure(
            answer["renderer"] == json!({"record": "gpu", "reason": null}),
            format!(
                "{what}: mask.sample-input-area was not the GPU's: {}",
                answer["renderer"]
            ),
        )?;
        let frame = launch.at(&coverage)?;
        let overlay = &frame.state()["mask_overlay"];
        ensure(
            overlay["effective"] == json!("mask-on-black"),
            format!("{what}: the coverage frame's overlay reads {overlay}"),
        )?;
        let row = json!({
            "subject": pick.subject,
            "kind": pick.kind,
            "gesture": if pick.at.len() == 4 { "drag" } else { "click" },
            "at": pick.at,
            "stored": stored,
            "area": {"renderer": answer["renderer"], "stage": answer["stage"],
                     "rect": answer["rect"], "reduction": answer["reduction"],
                     "points": answer["points"].as_array().map_or(0, Vec::len)},
            "coverage_frame": frame["file"].clone(),
        });
        checks.note(frame, &what, row.clone());
        rows.push(row);
    }
    let subjects: Vec<&str> = picks(&source).iter().map(|pick| pick.subject).collect();
    let covered = |subject: &str| subjects.iter().any(|held| held.contains(subject));
    let unavailable: Vec<&str> = ["sky", "skin", "foliage", "neutral"]
        .into_iter()
        .filter(|subject| !covered(subject))
        .collect();
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "source": source,
            "source_sha256": hash(&source)?,
            "picks": rows,
            "unavailable": unavailable,
            "scope": "Range picks sent through the API on one photograph, each into a new mask with nothing bound, read at the mask's input site; the coverage frames are the composed mask drawn on black at Fit, for a person to read. No selection's quality is judged.",
        }),
    )
}
