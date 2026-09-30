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
    Launched, Prepared, ProbeContext, Source, launch, launch_scope, measured, not_measured,
    prepare, start,
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

/// The metrics the RAW trip's developed regions would have been recorded under.
const DEVELOPED: [&str; 2] = [
    "focus_check_developed_first_region",
    "focus_check_developed_second_region",
];

/// The focus check's rows over the generated JPEGs and, when there is one, per camera over the
/// RAW trip; without one, the developed rows are not measured.
pub(super) fn probe(
    root: &Path,
    context: &ProbeContext,
    images: &Source,
    raw: Option<&Source>,
) -> Result<Vec<Value>> {
    let named = |name: &str, result: Result<Vec<Value>>| -> Result<Vec<Value>> {
        result.map_err(|error| {
            format!(
                "The {name} probe ({}): {error}",
                context.scratch.join(name).display()
            )
            .into()
        })
    };
    let mut rows = named("focus", measure(root, context, "focus", images, false))?;
    match raw {
        Some(raw) => rows.extend(named(
            "focus-raw",
            measure(root, context, "focus-raw", raw, true),
        )?),
        None => rows.extend(DEVELOPED.map(|metric| {
            not_measured(
                metric,
                "no RAW trip was given, so no frame's region needs a development",
            )
        })),
    }
    Ok(rows)
}

/// One sample: the frame the loupe is on, how the loupe gets there from the sample before, and
/// where the pointer goes for its second region, if it asks for one.
#[derive(Clone, Debug, PartialEq)]
struct Sample {
    position: u32,
    second: Option<[f32; 2]>,
}

fn measure(
    root: &Path,
    context: &ProbeContext,
    name: &str,
    source: &Source,
    per_camera: bool,
) -> Result<Vec<Value>> {
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
    let mut rows = Vec::new();
    run.check(|run| {
        let mut launched = Vec::new();
        for (index, samples) in launches.iter().enumerate() {
            let steps = script(&prepared, samples);
            launched.push(launch(
                run,
                &format!("launch-{}", index + 1),
                &prepared.catalog,
                &steps,
            )?);
        }
        rows = report(source, &prepared, &launched, per_camera)?;
        Ok(())
    })?;
    if per_camera
        && !rows
            .iter()
            .any(|row| DEVELOPED.contains(&row["metric"].as_str().unwrap_or("")))
    {
        rows.extend(DEVELOPED.map(|metric| {
            not_measured(
                metric,
                "every RAW frame's region was cut from a full-size embedded preview",
            )
        }));
    }
    Ok(rows)
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

/// The rows: one per origin and region of its frame, and over the trip per camera as well.
fn report(
    source: &Source,
    prepared: &Prepared,
    launched: &[Launched],
    per_camera: bool,
) -> Result<Vec<Value>> {
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
    let base = launch_scope(&launched[0]);
    let mut rows = Vec::new();
    for ((camera, origin, which), regions) in groups {
        let (metric, from, region) = match which {
            Which::First => (
                format!("focus_check_{origin}_first_region"),
                "Z's handling in the editor's update (loupe_focus pressed_ms)",
                "the frame's first region since the loupe opened on it, under the pointer",
            ),
            Which::Second => (
                format!("focus_check_{origin}_second_region"),
                "the pointer move sent to the loupe (loupe_pointer_sent)",
                "a second rectangle of the same frame after a pointer move, the check still on",
            ),
        };
        let mut scope = json!({
            "source": source.label,
            "files": prepared.count,
            "camera": camera,
            "origin": origin,
            "region": region,
            "rect": size(&regions[0].rect),
            "frame": size(&regions[0].frame),
            "samples": regions.len(),
            "unanswered_in_run": unanswered.get(&which).copied().unwrap_or(0),
            "from": from,
            "presented": "the update whose derived model draws the region in the inset (loupe_region_presented), not scanout",
            "launches": launched.len(),
        });
        if origin == "embedded" && which == Which::First {
            scope["target"] =
                json!("within 50 ms of Z from a full-size embedded preview (provisional)");
        }
        for (key, value) in base.as_object().into_iter().flatten() {
            scope[key] = value.clone();
        }
        let samples = regions
            .iter()
            .filter_map(|region| Some(region.at_ms? - region.from_ms))
            .collect();
        rows.push(measured(&metric, "ms", samples, scope));
    }
    ensure(!rows.is_empty(), "No region was presented")?;
    Ok(rows)
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
