//! The 100% focus check: `Z`, or a pointer move with the check on, to the inset's region presented.
//! The design's targets: "from a full-size embedded preview, within 50 ms of `Z`; from an
//! on-demand development, reported per camera".
//!
//! A region is timed from what asked for it — `Z`'s `loupe_focus` (`pressed_ms`, the start of its
//! handling) or the pointer move the `pointer` step sent (`loupe_pointer_sent`) — through the
//! region the loupe then asked for (`loupe_region_asked`, by its number) to that region's
//! `loupe_region_presented`: the update whose derived model draws it in the inset. Rows are kept
//! apart by the origin the region reports (`embedded`: cut from a full-size preview or a JPEG
//! original; `developed`: a neutral development made on demand), by camera over the RAW trip, and by
//! which region of its frame it was.
//!
//! - **From a full-size embedded preview**: over the generated JPEGs, each its own full-size
//!   preview, each sample a new frame, `Z` pressed and, once its region has landed, pressed again.
//! - **From a development, per camera**: over the RAW trip, up to `samples` RAW frames of each
//!   camera, each with the loupe opened on it. `Z` asks for the frame's first region since the
//!   loupe opened on it, which includes the frame's development; a pointer move then asks for
//!   another rectangle of the same frame, its second region, timed on its own.
use super::{
    FOCUS_DEVELOPMENT, FOCUS_EMBEDDED, Figure, Launched, Memory, Prepared, ProbeContext, Source,
    launch, launch_detail, merge, prepare, start,
};
use crate::*;
use luxforge_evidence::{self as script, ArrowKey, LoupeStep, SelectStep};
use std::collections::BTreeMap;

/// Samples per launch: each takes three steps over the JPEGs, and up to six, with a RAW
/// development, over the trip, inside the 64-step and 60-second evidence bounds.
const EMBEDDED_PER_LAUNCH: usize = 20;
const DEVELOPED_PER_LAUNCH: usize = 8;
/// Where the pointer goes for a frame's second region: two rectangles far enough apart on any frame
/// larger than the inset, alternated so a frame's second region is never the rectangle its first
/// was asked at.
const POINTERS: [[f32; 2]; 2] = [[0.3, 0.3], [0.7, 0.7]];

/// The figures a frame's developed regions are recorded under: the design's, then the second
/// region beside it.
const DEVELOPED: [&str; 2] = [
    FOCUS_DEVELOPMENT,
    "desktop.focus_check.development_second_region",
];

/// The focus check's figures over the generated JPEGs (when they were generated) and, when there
/// is one, per camera over the RAW trip; without one, the developed figures are skipped, their data
/// absent. Each run answers on its own, a failed one with its failed figure.
pub(super) fn probe(
    root: &Path,
    context: &ProbeContext,
    images: Option<&Source>,
    raw: Option<&Source>,
    memory: &mut Memory,
) -> Vec<Figure> {
    let mut run = |name: &str, source: &Source, per_camera: bool| {
        measure(root, context, name, source, per_camera, memory)
            .unwrap_or_else(|error| vec![Figure::failed(context, name, error)])
    };
    let mut figures = images
        .map(|images| run("focus", images, false))
        .unwrap_or_default();
    match raw {
        Some(raw) => figures.extend(run("focus-raw", raw, true)),
        None => figures.extend(DEVELOPED.map(|metric| {
            Figure::skipped(
                metric,
                "ms",
                "no RAW trip: the RAW corpus is absent, so no frame's region needs a development",
            )
            .target(FOCUS_DEVELOPMENT)
        })),
    }
    figures
}

/// One sample: the frame the loupe is on, how the loupe gets there from the sample before, and
/// where the pointer goes for its second region, if it asks for one.
#[derive(Clone, Debug, PartialEq)]
struct Sample {
    position: u32,
    second: Option<[f32; 2]>,
}

pub(super) fn measure(
    root: &Path,
    context: &ProbeContext,
    name: &str,
    source: &Source,
    per_camera: bool,
    memory: &mut Memory,
) -> Result<Vec<Figure>> {
    let prepared = prepare(
        &context.scratch.join(format!("{name}-catalog")),
        &source.folder,
    )?;
    let launches = if per_camera {
        chunks(
            developed_samples(&prepared, context.samples)?,
            DEVELOPED_PER_LAUNCH,
        )
    } else {
        let count = context.samples.min(prepared.count as usize) as u32;
        chunks(
            (0..count)
                .map(|position| Sample {
                    position,
                    second: None,
                })
                .collect(),
            EMBEDDED_PER_LAUNCH,
        )
    };
    let run = start(root, &context.scratch.join(name), &context.binary)?;
    let mut figures = Vec::new();
    run.check(|run| {
        let mut launched = Vec::new();
        for (index, samples) in launches.iter().enumerate() {
            let steps = script(&prepared, samples);
            let one = launch(
                run,
                &format!("launch-{}", index + 1),
                &prepared.catalog,
                &steps,
            )?;
            memory.loupe_run(name, &one, None);
            launched.push(one);
        }
        figures = report(source, &prepared, &launched, per_camera)?;
        Ok(())
    })?;
    if per_camera
        && !figures
            .iter()
            .any(|figure| DEVELOPED.contains(&figure.metric.as_str()))
    {
        figures.extend(DEVELOPED.map(|metric| {
            Figure::not_measured(
                metric,
                "ms",
                "every RAW frame's region was cut from a full-size embedded preview",
            )
            .target(FOCUS_DEVELOPMENT)
        }));
    }
    Ok(figures)
}

/// Up to `samples` RAW frames of each camera of the trip, the first in the view's order, each with
/// a second region at a pointer the frame's first was not asked at.
fn developed_samples(prepared: &Prepared, samples: usize) -> Result<Vec<Sample>> {
    let mut cameras: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
    for row in &prepared.rows {
        if row["kind"] == "raw"
            && let (Some(camera), Some(position)) =
                (row["camera"].as_str(), row["position"].as_u64())
        {
            let positions = cameras.entry(camera).or_default();
            if positions.len() < samples {
                positions.push(u32::try_from(position)?);
            }
        }
    }
    ensure(
        !cameras.is_empty(),
        "The RAW trip holds no RAW frame with a camera",
    )?;
    let mut positions: Vec<u32> = cameras.into_values().flatten().collect();
    positions.sort_unstable();
    Ok(positions
        .into_iter()
        .map(|position| Sample {
            position,
            second: Some(POINTERS[0]),
        })
        .collect())
}

/// `samples` in launches of at most `per` each.
fn chunks(samples: Vec<Sample>, per: usize) -> Vec<Vec<Sample>> {
    samples.chunks(per).map(<[Sample]>::to_vec).collect()
}

/// One launch's script: the loupe opened on the first sample's frame, then per sample `Z`, the
/// pointer moved for its second region when it asks for one, and `Z` again; between samples → to
/// the next frame when it is the next, else back to the grid, its cell clicked and `E`. The pointer
/// stays where it was moved until the loupe closes, so each second region's pointer is the one the
/// first region was not asked at.
fn script(prepared: &Prepared, samples: &[Sample]) -> Vec<script::Step> {
    let Some(first) = samples.first() else {
        return Vec::new();
    };
    let mut steps = super::open_loupe(prepared, first.position);
    let mut pointer: Option<[f32; 2]> = None;
    for (index, sample) in samples.iter().enumerate() {
        if index > 0 {
            let before = samples[index - 1].position;
            if sample.position == before + 1 {
                steps.push(script::Step::Select(SelectStep::Arrow {
                    direction: ArrowKey::Right,
                    extend: false,
                }));
            } else {
                pointer = None;
                steps.push(script::Step::key(script::KEY_ESCAPE));
                steps.push(script::Step::Select(SelectStep::Click {
                    position: sample.position,
                    shift: false,
                    command: false,
                }));
                steps.push(script::Step::key("e"));
            }
        }
        steps.push(script::Step::key("z"));
        if sample.second.is_some() {
            let next = if pointer == Some(POINTERS[0]) {
                POINTERS[1]
            } else {
                POINTERS[0]
            };
            pointer = Some(next);
            steps.push(script::Step::Loupe(LoupeStep::Pointer(next)));
        }
        steps.push(script::Step::key("z"));
    }
    steps
}

/// Which region of its frame a sample timed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Which {
    /// `Z`: the frame's first region since the loupe opened on it.
    First,
    /// A pointer move with the check on: another rectangle of the same frame.
    Second,
}

/// One region asked for and, when it was, presented.
#[derive(Clone, Debug, PartialEq)]
struct Region {
    which: Which,
    position: Option<u64>,
    from_ms: f64,
    at_ms: Option<f64>,
    origin: Option<String>,
    rect: Value,
    frame: Value,
}

/// Every region a `Z` or a pointer move asked for, in order: each trigger paired with the first
/// region asked after it and before the next trigger, and that region with its presentation.
fn regions(events: &[Value]) -> Result<Vec<Region>> {
    let mut found: Vec<(usize, Region)> = Vec::new();
    let mut position = None;
    for (index, event) in events.iter().enumerate() {
        let detail = &event["detail"];
        let (which, from_ms) = match event["event"].as_str() {
            Some("loupe_focus") if detail["on"] == true => {
                position = detail["position"].as_u64();
                (
                    Which::First,
                    detail["pressed_ms"]
                        .as_f64()
                        .ok_or("A loupe_focus has no pressed_ms")?,
                )
            }
            Some("loupe_pointer_sent") => (
                Which::Second,
                event["elapsed_ms"]
                    .as_f64()
                    .ok_or("A loupe_pointer_sent has no time")?,
            ),
            _ => continue,
        };
        found.push((
            index,
            Region {
                which,
                position,
                from_ms,
                at_ms: None,
                origin: None,
                rect: Value::Null,
                frame: Value::Null,
            },
        ));
    }
    let triggers: Vec<usize> = found.iter().map(|(index, _)| *index).collect();
    for (at, (index, region)) in found.iter_mut().enumerate() {
        let until = triggers.get(at + 1).copied().unwrap_or(events.len());
        let Some(asked) = events[*index..until]
            .iter()
            .find(|event| event["event"] == "loupe_region_asked")
        else {
            continue;
        };
        let serial = &asked["detail"]["serial"];
        region.rect = asked["detail"]["rect"].clone();
        region.frame = asked["detail"]["frame"].clone();
        if let Some(shown) = events[*index..].iter().find(|event| {
            event["event"] == "loupe_region_presented" && &event["detail"]["serial"] == serial
        }) {
            region.at_ms = shown["elapsed_ms"].as_f64();
            region.origin = shown["detail"]["origin"].as_str().map(str::to_owned);
        }
    }
    Ok(found.into_iter().map(|(_, region)| region).collect())
}

/// `rect` or `frame` as `[width, height]`.
fn size(value: &Value) -> Value {
    json!([value["width"], value["height"]])
}

/// The figures: one per origin and region of its frame, and over the trip per camera as well.
fn report(
    source: &Source,
    prepared: &Prepared,
    launched: &[Launched],
    per_camera: bool,
) -> Result<Vec<Figure>> {
    let camera_of = |position: Option<u64>| {
        position
            .and_then(|position| prepared.rows.get(position as usize))
            .and_then(|row| row["camera"].as_str())
            .unwrap_or("unknown")
            .to_owned()
    };
    let mut groups: BTreeMap<(Option<String>, String, Which), Vec<Region>> = BTreeMap::new();
    let mut unanswered: BTreeMap<Which, usize> = BTreeMap::new();
    for launch in launched {
        for region in launch.read(regions(&launch.events))? {
            match region.origin.clone() {
                Some(origin) if region.at_ms.is_some() => {
                    let camera = per_camera.then(|| camera_of(region.position));
                    groups
                        .entry((camera, origin, region.which))
                        .or_default()
                        .push(region);
                }
                _ => *unanswered.entry(region.which).or_default() += 1,
            }
        }
    }
    let base = launch_detail(&launched[0]);
    let mut figures = Vec::new();
    for ((camera, origin, which), regions) in groups {
        let (metric, target) = figure_of(&origin, which);
        let (from, region) = match which {
            Which::First => (
                "Z's handling in the editor's update (loupe_focus pressed_ms)",
                "the frame's first region since the loupe opened on it, under the pointer, which \
                 includes the frame's development when it needs one",
            ),
            Which::Second => (
                "the pointer move sent to the loupe (loupe_pointer_sent)",
                "a second rectangle of the same frame after a pointer move, the check still on",
            ),
        };
        let mut detail = json!({
            "source": source.label,
            "files": prepared.count,
            "camera": camera,
            "origin": origin,
            "rect": size(&regions[0].rect),
            "frame": size(&regions[0].frame),
            "samples": regions.len(),
            "unanswered_in_run": unanswered.get(&which).copied().unwrap_or(0),
            "launches": launched.len(),
        });
        merge(&mut detail, &base);
        let scope = format!(
            "{region}: from {from} to the update whose derived model draws the region in the inset \
             (loupe_region_presented), not scanout; {} regions cut from {} over {}{}",
            regions.len(),
            if origin == "developed" {
                "a Luxforge development made on demand"
            } else {
                "a full-size embedded preview (a JPEG is its own)"
            },
            source.label,
            camera
                .as_ref()
                .map(|camera| format!(", camera {camera}"))
                .unwrap_or_default(),
        );
        let samples = regions
            .iter()
            .filter_map(|region| Some(region.at_ms? - region.from_ms))
            .collect();
        figures.push(
            Figure::measured(metric, "ms", samples)
                .scope(scope)
                .cache(
                    "each frame opened in the loupe just before its Z, its loupe tier read then; \
                     the file cache warm from indexing the folder into a new catalog",
                )
                .target(target)
                .detail(detail),
        );
    }
    ensure(!figures.is_empty(), "No region was presented")?;
    Ok(figures)
}

/// The figure a region of `origin` is recorded under, and the design's figure whose target it
/// answers: `Z`'s region from a full-size embedded preview, or a development's, per camera; a
/// second region beside each.
fn figure_of(origin: &str, which: Which) -> (String, &'static str) {
    match (origin, which) {
        ("embedded", Which::First) => (FOCUS_EMBEDDED.into(), FOCUS_EMBEDDED),
        ("developed", Which::First) => (FOCUS_DEVELOPMENT.into(), FOCUS_DEVELOPMENT),
        ("developed", Which::Second) => (DEVELOPED[1].into(), FOCUS_DEVELOPMENT),
        (origin, Which::First) => (format!("desktop.focus_check.{origin}"), FOCUS_EMBEDDED),
        (origin, Which::Second) => (
            format!("desktop.focus_check.{origin}_second_region"),
            FOCUS_EMBEDDED,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, elapsed_ms: f64, detail: Value) -> Value {
        json!({"event": name, "elapsed_ms": elapsed_ms, "detail": detail})
    }

    fn asked(serial: u64, at: f64) -> Value {
        event(
            "loupe_region_asked",
            at,
            json!({"serial": serial, "rect": {"x": 1, "y": 2, "width": 616, "height": 418},
                "frame": {"width": 6000, "height": 4000}}),
        )
    }

    fn shown(serial: u64, at: f64, origin: &str) -> Value {
        event(
            "loupe_region_presented",
            at,
            json!({"serial": serial, "origin": origin}),
        )
    }

    /// `Z` on position 4 with its region presented, its second region after a pointer move, a
    /// `Z` whose region never landed, and a region presented late under an older number.
    fn events() -> Vec<Value> {
        vec![
            event(
                "loupe_focus",
                10.5,
                json!({"on": true, "position": 4, "pressed_ms": 10.0}),
            ),
            asked(1, 10.6),
            shown(1, 900.0, "developed"),
            event("loupe_pointer_sent", 1000.0, json!({"x": 0.3, "y": 0.3})),
            asked(2, 1000.2),
            shown(2, 1040.0, "developed"),
            event(
                "loupe_focus",
                2000.5,
                json!({"on": false, "position": 4, "pressed_ms": 2000.0}),
            ),
            event(
                "loupe_focus",
                3000.5,
                json!({"on": true, "position": 5, "pressed_ms": 3000.0}),
            ),
            asked(3, 3000.6),
        ]
    }

    #[test]
    fn each_region_is_timed_from_what_asked_for_it_to_its_own_presentation() {
        let regions = regions(&events()).unwrap();
        assert_eq!(regions.len(), 3);
        assert_eq!(
            (regions[0].which, regions[0].position, regions[0].at_ms),
            (Which::First, Some(4), Some(900.0))
        );
        assert_eq!(regions[0].at_ms.unwrap() - regions[0].from_ms, 890.0);
        assert_eq!(
            (regions[1].which, regions[1].position),
            (Which::Second, Some(4))
        );
        assert_eq!(regions[1].at_ms.unwrap() - regions[1].from_ms, 40.0);
        assert_eq!(regions[1].origin.as_deref(), Some("developed"));
        assert_eq!(size(&regions[1].rect), json!([616, 418]));
        assert_eq!(
            (regions[2].position, regions[2].at_ms),
            (Some(5), None),
            "never presented"
        );
    }

    #[test]
    fn a_trigger_that_asked_for_nothing_is_not_paired_with_the_next_ones_region() {
        let events = vec![
            event("loupe_pointer_sent", 5.0, json!({})),
            event(
                "loupe_focus",
                10.5,
                json!({"on": true, "position": 1, "pressed_ms": 10.0}),
            ),
            asked(1, 10.6),
            shown(1, 20.0, "embedded"),
        ];
        let regions = regions(&events).unwrap();
        assert_eq!(regions[0].at_ms, None);
        assert_eq!(regions[1].at_ms, Some(20.0));
    }

    #[test]
    fn each_script_parses_and_moves_the_pointer_off_the_first_regions_rectangle() {
        let prepared = Prepared {
            catalog: "/scratch/catalog.sqlite".into(),
            folder: "/scratch/raw".into(),
            count: 50,
            moments: vec![],
            rows: vec![],
        };
        let samples: Vec<Sample> = [3, 4, 9]
            .into_iter()
            .map(|position| Sample {
                position,
                second: Some(POINTERS[0]),
            })
            .collect();
        let steps = script(&prepared, &samples);
        let pointers: Vec<[f32; 2]> = steps
            .iter()
            .filter_map(|step| match step {
                script::Step::Loupe(LoupeStep::Pointer(at)) => Some(*at),
                _ => None,
            })
            .collect();
        assert_eq!(
            pointers,
            vec![POINTERS[0], POINTERS[1], POINTERS[0]],
            "frame 4 is reached by →, so its first region is asked at frame 3's pointer"
        );
        // Open (4), frame 3 (3), → and frame 4 (4), Esc, click, E and frame 9 (6).
        assert_eq!(steps.len(), 17);
        assert!(steps.len() <= script::MAX_SCRIPT_STEPS);
        assert_eq!(
            script::parse(&script::write(&steps).to_string()).unwrap(),
            steps
        );
        let full: Vec<Sample> = (0..DEVELOPED_PER_LAUNCH as u32)
            .map(|at| Sample {
                position: at * 2,
                second: Some(POINTERS[0]),
            })
            .collect();
        assert!(script(&prepared, &full).len() <= script::MAX_SCRIPT_STEPS);
        let embedded: Vec<Sample> = (0..EMBEDDED_PER_LAUNCH as u32)
            .map(|position| Sample {
                position,
                second: None,
            })
            .collect();
        assert!(script(&prepared, &embedded).len() <= script::MAX_SCRIPT_STEPS);
    }

    #[test]
    fn a_region_is_recorded_under_the_designs_figure_for_its_origin() {
        assert_eq!(
            figure_of("embedded", Which::First),
            (FOCUS_EMBEDDED.to_owned(), FOCUS_EMBEDDED)
        );
        assert_eq!(
            figure_of("developed", Which::First),
            (FOCUS_DEVELOPMENT.to_owned(), FOCUS_DEVELOPMENT)
        );
        assert_eq!(
            figure_of("developed", Which::Second),
            (
                "desktop.focus_check.development_second_region".to_owned(),
                FOCUS_DEVELOPMENT
            )
        );
        assert_eq!(
            figure_of("embedded", Which::Second).0,
            "desktop.focus_check.embedded_second_region"
        );
    }

    #[test]
    fn a_trips_samples_are_the_first_raw_frames_of_each_camera() {
        let row = |position: u64, kind: &str, camera: &str| json!({"position": position, "kind": kind, "camera": camera});
        let prepared = Prepared {
            catalog: "/scratch/catalog.sqlite".into(),
            folder: "/scratch/raw".into(),
            count: 6,
            moments: vec![],
            rows: vec![
                row(0, "raw", "Z 6"),
                row(1, "jpeg", "Z 6"),
                row(2, "raw", "X100VI"),
                row(3, "raw", "Z 6"),
                row(4, "raw", "Z 6"),
                row(5, "raw", "X100VI"),
            ],
        };
        let positions: Vec<u32> = developed_samples(&prepared, 2)
            .unwrap()
            .iter()
            .map(|sample| sample.position)
            .collect();
        assert_eq!(positions, vec![0, 2, 3, 5]);
    }
}
