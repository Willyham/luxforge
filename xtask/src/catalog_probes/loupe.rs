//! Loupe stepping with the look-ahead warm, and a held arrow. The design's targets: "the next frame
//! presented in the frame after the key; a held arrow presents every frame at the key-repeat rate".
//!
//! A launch opens the loupe on the view's first burst and then, per sample, runs one loupe `arrows`
//! step of one press of →. The step waits until the look-ahead is warm — every frame the loupe
//! wants decoded at its size (`loupe_warm`) — so no cold read is timed as warm, then presses. A key
//! is timed from its `loupe_key` (`pressed_ms`, the start of its handling in the editor's update) to
//! the `loupe_presented` of the frame it moved to, under that frame's own position, not a stand-in,
//! carrying the key's own number: presented before any later key. A key whose frame was not held at
//! its size when it was handled (`ready: false`) is cold and left out of the warm figure, counted.
//!
//! The first launch then holds an arrow: one step of as many presses as samples, [`KEY_REPEAT_MS`]
//! apart, the first a press and the rest the key's repeats, starting warm. Each key's own frame
//! presents before the next key or is skipped; the figures are each presented key's latency, the
//! intervals between the frames presented, and the intervals the keys were handled at, with the
//! skipped keys and the keys whose frame was already held named in the scope.
//!
//! The design's figure is in display frames: a warm key's latency counts as `max(1, ⌈ms ÷ the
//! display frame interval⌉)` frames, the interval being the median of the window's frame clock the
//! grid probe observed on the same host and window size, so 1 is a frame presented within one frame
//! interval of the key's handling: in the frame after the key. The milliseconds are kept beside it.
use super::{
    Figure, LOUPE_STEP, Launched, Memory, Prepared, ProbeContext, Source, launch, launch_detail,
    merge, prepare, start, stepped,
};
use crate::*;
use luxforge_evidence::{self as script, ArrowKey, LoupeArrows, LoupeStep, MAX_LOUPE_ARROWS};
use std::collections::BTreeMap;

/// The held arrow's interval: macOS's fastest key repeat in System Settings (`KeyRepeat` 2, in
/// units of 15 ms), the most frames a held key asks the loupe for.
pub(super) const KEY_REPEAT_MS: u64 = 30;
/// Warm presses per launch, so a script stays well inside the evidence run's 60-second deadline.
const WARM_PER_LAUNCH: u32 = 40;

/// What one launch does: open the loupe at `start`, press → `warm` times, each warm, then hold.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Plan {
    start: u32,
    warm: u32,
    hold: Option<(ArrowKey, u32)>,
}

/// The loupe's figures over the generated JPEGs, whose warm stepping is the design's figure, and,
/// when there is one, over the RAW trip beside them; `frame_ms` is the display frame interval
/// observed, which the design's figure is counted in. Each run answers on its own, a failed one
/// with its failed figure; a run without its folder (the JPEGs not generated, no trip) is not run.
pub(super) fn probe(
    root: &Path,
    context: &ProbeContext,
    images: Option<&Source>,
    raw: Option<&Source>,
    frame_ms: Option<f64>,
    memory: &mut Memory,
) -> Vec<Figure> {
    let mut figures = Vec::new();
    for (name, family, source) in [
        ("loupe", "desktop.loupe_step", images),
        ("loupe-raw", "desktop.loupe_step.raw_trip", raw),
    ] {
        let Some(source) = source else {
            continue;
        };
        let taken = Taken {
            name,
            family,
            frame_ms,
        };
        match measure(root, context, source, &taken, memory) {
            Ok(taken) => figures.extend(taken),
            Err(error) => figures.push(Figure::failed(context, name, error)),
        }
    }
    figures
}

/// One run of the probe: its directory's name, the metric family its figures are named in, and
/// the display frame interval observed.
struct Taken<'a> {
    name: &'a str,
    family: &'a str,
    frame_ms: Option<f64>,
}

fn measure(
    root: &Path,
    context: &ProbeContext,
    source: &Source,
    taken: &Taken,
    memory: &mut Memory,
) -> Result<Vec<Figure>> {
    let prepared = prepare(
        &context.scratch.join(format!("{}-catalog", taken.name)),
        &source.folder,
    )?;
    let plans = plan(prepared.count, &prepared.moments, context.samples)?;
    let run = start(root, &context.scratch.join(taken.name), &context.binary)?;
    let mut figures = Vec::new();
    run.check(|run| {
        let mut launches = Vec::new();
        for (index, plan) in plans.iter().enumerate() {
            let steps = script(&prepared, plan);
            let launched = launch(
                run,
                &format!("launch-{}", index + 1),
                &prepared.catalog,
                &steps,
            )?;
            memory.loupe_run(taken.name, &launched, plan.hold.map(|(_, keys)| keys));
            launches.push(launched);
        }
        figures = report(source, &prepared, &plans, &launches, taken)?;
        Ok(())
    })?;
    Ok(figures)
}

/// A warm key's latency in display frames: `max(1, ⌈ms ÷ frame_ms⌉)`.
fn frames(ms: f64, frame_ms: f64) -> f64 {
    (ms / frame_ms).ceil().max(1.0)
}

/// The launches: each opens the loupe on the view's first burst (or its first frame when it has
/// none, or too few frames after it) and presses → at most [`WARM_PER_LAUNCH`] times; the first
/// then holds an arrow for `samples` presses (at least two), onward when the view has the frames,
/// else back over the frames just stepped.
fn plan(count: u32, moments: &[Value], samples: usize) -> Result<Vec<Plan>> {
    ensure(
        count >= 3,
        format!("The view holds {count} frames; stepping needs 3"),
    )?;
    let start = moments
        .iter()
        .filter(|moment| moment["kind"] == "burst")
        .filter_map(|moment| moment["start"].as_u64())
        .map(|start| start as u32)
        .find(|start| start + 2 < count)
        .unwrap_or(0);
    let room = count - start - 1;
    let mut plans = Vec::new();
    let mut left = u32::try_from(samples)?;
    while left > 0 {
        let warm = left.min(WARM_PER_LAUNCH).min(room);
        plans.push(Plan {
            start,
            warm,
            hold: None,
        });
        left -= warm;
    }
    let keys = u32::try_from(samples)?.clamp(2, MAX_LOUPE_ARROWS);
    let at = start + plans[0].warm;
    plans[0].hold = Some(if at + keys < count {
        (ArrowKey::Right, keys)
    } else if at >= keys {
        (ArrowKey::Left, keys)
    } else if at >= count - 1 - at {
        (ArrowKey::Left, at)
    } else {
        (ArrowKey::Right, count - 1 - at)
    });
    ensure(
        plans[0].hold.is_some_and(|(_, keys)| keys >= 2),
        "The view is too small to hold an arrow",
    )?;
    Ok(plans)
}

/// One launch's script: the loupe opened at its start, its warm presses, then its hold.
fn script(prepared: &Prepared, plan: &Plan) -> Vec<script::Step> {
    let press = |direction, count: u32| {
        script::Step::Loupe(LoupeStep::Arrows(LoupeArrows {
            direction,
            count,
            interval_ms: (count > 1).then_some(KEY_REPEAT_MS),
        }))
    };
    let mut steps = super::open_loupe(prepared, plan.start);
    steps.extend((0..plan.warm).map(|_| press(ArrowKey::Right, 1)));
    steps.extend(plan.hold.map(|(direction, keys)| press(direction, keys)));
    steps
}

/// One key the loupe handled.
#[derive(Clone, Debug, PartialEq)]
struct Key {
    step: u64,
    key: u64,
    from: Option<u64>,
    to: Option<u64>,
    ready: bool,
    pressed_ms: f64,
}

/// One picture the active frame presented.
#[derive(Clone, Debug, PartialEq)]
struct Shown {
    key: u64,
    position: u64,
    stand_in: bool,
    at_ms: f64,
    origin: String,
    size: (u64, u64),
}

fn keys(events: &[Value]) -> Result<Vec<Key>> {
    stepped(events, "loupe_key")
        .into_iter()
        .map(|(step, _, detail)| -> Result<Key> {
            Ok(Key {
                step,
                key: detail["key"].as_u64().ok_or("A loupe_key has no number")?,
                from: detail["from"].as_u64(),
                to: detail["to"].as_u64(),
                ready: detail["ready"] == true,
                pressed_ms: detail["pressed_ms"]
                    .as_f64()
                    .ok_or("A loupe_key has no pressed_ms")?,
            })
        })
        .collect()
}

fn shown(events: &[Value]) -> Result<Vec<Shown>> {
    stepped(events, "loupe_presented")
        .into_iter()
        .map(|(_, at_ms, detail)| -> Result<Shown> {
            Ok(Shown {
                key: detail["key"]
                    .as_u64()
                    .ok_or("A loupe_presented has no key")?,
                position: detail["position"]
                    .as_u64()
                    .ok_or("A loupe_presented has no position")?,
                stand_in: detail["stand_in"] != false,
                at_ms,
                origin: detail["origin"].as_str().unwrap_or_default().to_owned(),
                size: (
                    detail["width"].as_u64().unwrap_or(0),
                    detail["height"].as_u64().unwrap_or(0),
                ),
            })
        })
        .collect()
}

/// The presentation of `key`'s own frame: its position's own picture, not a stand-in, presented
/// after the key and before any later one.
fn own_frame<'a>(key: &Key, shown: &'a [Shown]) -> Option<&'a Shown> {
    shown
        .iter()
        .find(|shown| shown.key == key.key && Some(shown.position) == key.to && !shown.stand_in)
}

/// The script's steps as each `script_step` recorded its request, by step number.
fn requests(events: &[Value]) -> BTreeMap<u64, &Value> {
    events
        .iter()
        .filter(|event| event["event"] == "script_step")
        .filter_map(|event| {
            Some((
                event["detail"]["step"].as_u64()?,
                &event["detail"]["request"],
            ))
        })
        .collect()
}

/// The number of presses a step's request asks for, when it is a loupe `arrows` step.
fn presses(request: &Value) -> Option<u64> {
    request["loupe"]["arrows"]["count"].as_u64()
}

/// The warm presses of one launch.
#[derive(Debug, Default, PartialEq)]
struct Warm {
    samples: Vec<f64>,
    cold: usize,
    not_presented: usize,
    preview: Option<(String, (u64, u64))>,
}

fn warm(events: &[Value]) -> Result<Warm> {
    let requests = requests(events);
    let (keys, shown) = (keys(events)?, shown(events)?);
    let warmed: Vec<u64> = stepped(events, "loupe_warm")
        .iter()
        .map(|(step, ..)| *step)
        .collect();
    let mut warm = Warm::default();
    for (step, request) in &requests {
        if presses(request) != Some(1) {
            continue;
        }
        let pressed: Vec<&Key> = keys.iter().filter(|key| key.step == *step).collect();
        ensure(
            pressed.len() == 1 && warmed.contains(step),
            format!("Warm step {step} pressed {} keys", pressed.len()),
        )?;
        let key = pressed[0];
        ensure(
            key.to.is_some() && key.to != key.from,
            format!("Warm step {step}'s key moved nothing"),
        )?;
        if !key.ready {
            warm.cold += 1;
            continue;
        }
        match own_frame(key, &shown) {
            Some(frame) => {
                warm.samples.push(frame.at_ms - key.pressed_ms);
                warm.preview
                    .get_or_insert_with(|| (frame.origin.clone(), frame.size));
            }
            None => warm.not_presented += 1,
        }
    }
    Ok(warm)
}

/// One launch's held arrow.
#[derive(Debug, Default, PartialEq)]
struct Held {
    keys: usize,
    ready: usize,
    skipped: Vec<u64>,
    stand_ins: usize,
    key_to_presented: Vec<f64>,
    presented_interval: Vec<f64>,
    key_interval: Vec<f64>,
}

fn held(events: &[Value]) -> Result<Option<Held>> {
    let requests = requests(events);
    let Some(step) = requests
        .iter()
        .find(|(_, request)| presses(request).is_some_and(|count| count > 1))
        .map(|(step, _)| *step)
    else {
        return Ok(None);
    };
    let (keys, shown) = (keys(events)?, shown(events)?);
    let pressed: Vec<&Key> = keys.iter().filter(|key| key.step == step).collect();
    ensure(
        Some(pressed.len() as u64) == presses(requests[&step]),
        format!("The held arrow pressed {} keys", pressed.len()),
    )?;
    ensure(
        pressed
            .iter()
            .all(|key| key.to.is_some() && key.to != key.from),
        "A held key moved nothing",
    )?;
    let serials: Vec<u64> = pressed.iter().map(|key| key.key).collect();
    let mut held = Held {
        keys: pressed.len(),
        ready: pressed.iter().filter(|key| key.ready).count(),
        stand_ins: shown
            .iter()
            .filter(|shown| shown.stand_in && serials.contains(&shown.key))
            .count(),
        key_interval: pressed
            .windows(2)
            .map(|pair| pair[1].pressed_ms - pair[0].pressed_ms)
            .collect(),
        ..Held::default()
    };
    let mut presented = Vec::new();
    for key in &pressed {
        match own_frame(key, &shown) {
            Some(frame) => {
                held.key_to_presented.push(frame.at_ms - key.pressed_ms);
                presented.push(frame.at_ms);
            }
            None => held.skipped.push(key.key),
        }
    }
    held.presented_interval = presented.windows(2).map(|pair| pair[1] - pair[0]).collect();
    Ok(Some(held))
}

/// The figures of every launch over `source`: the warm keys in display frames and in ms, then the
/// held arrow's.
fn report(
    source: &Source,
    prepared: &Prepared,
    plans: &[Plan],
    launches: &[Launched],
    taken: &Taken,
) -> Result<Vec<Figure>> {
    let mut samples = Vec::new();
    let (mut cold, mut not_presented, mut preview) = (0, 0, None);
    for launched in launches {
        let warm = launched.read(warm(&launched.events))?;
        samples.extend(warm.samples);
        cold += warm.cold;
        not_presented += warm.not_presented;
        preview = preview.or(warm.preview);
    }
    let base = launch_detail(&launches[0]);
    let preview = preview.map(
        |(origin, (width, height))| json!({"origin": origin, "width": width, "height": height}),
    );
    let rule = "frames = max(1, ceil(ms / frame_interval_ms)): 1 is a frame presented within one \
                display frame interval of the key's handling, in the frame after the key. The \
                interval is the median of the window's frame clock the grid probe observed while \
                scrolling, on the same host and window size";
    let mut detail = json!({
        "source": source.label,
        "files": prepared.count,
        "start_position": plans[0].start,
        "samples": samples.len(),
        "cold_excluded": cold,
        "not_presented": not_presented,
        "preview": preview,
        "launches": launches.len(),
        "frame_interval_ms": taken.frame_ms,
        "rule": rule,
        "samples_ms": samples,
    });
    merge(&mut detail, &base);
    let scope = format!(
        "Warm key to its frame presented, over {} presses of → from the first burst of {} ({} \
         files), each once the look-ahead was warm: from the key's handling in the editor's update \
         (loupe_key pressed_ms) to the update whose derived model draws that frame's own picture \
         (loupe_presented), not scanout; a stand-in is never counted",
        samples.len(),
        source.label,
        prepared.count
    );
    let cache = "the look-ahead warm before each key: every frame it wants decoded at its size \
                 (loupe_warm), from loupe tiers read in the run; a key whose frame was not held when \
                 it was handled is counted cold and left out";
    let in_frames = |family: &str| format!("{family}.key_to_presented");
    let frames_figure = match taken.frame_ms {
        Some(frame_ms) => Figure::measured(
            in_frames(taken.family),
            "frames",
            samples.iter().map(|ms| frames(*ms, frame_ms)).collect(),
        ),
        None => Figure::not_measured(
            in_frames(taken.family),
            "frames",
            "no display frame interval was observed to count the keys' latency in",
        ),
    };
    let mut figures = vec![
        frames_figure
            .scope(format!("{scope}; in display frames by the rule in detail"))
            .cache(cache)
            .target(LOUPE_STEP)
            .detail(detail.clone()),
        Figure::measured(
            format!("{}.key_to_presented_ms", taken.family),
            "ms",
            samples,
        )
        .scope(scope)
        .cache(cache)
        .target(LOUPE_STEP)
        .detail(detail),
    ];
    let held = launches[0]
        .read(held(&launches[0].events))?
        .ok_or("The first launch held no arrow")?;
    let (direction, keys) = plans[0].hold.ok_or("The first launch plans no hold")?;
    let mut detail = json!({
        "source": source.label,
        "files": prepared.count,
        "key_repeat_interval_ms": KEY_REPEAT_MS,
        "key_repeat": "macOS's fastest System Settings key repeat (KeyRepeat 2 x 15 ms); the first press, then repeats",
        "direction": format!("{direction:?}").to_lowercase(),
        "keys": keys,
        "presented_before_next_key": held.key_to_presented.len(),
        "skipped_keys": held.skipped,
        "ready_at_key": held.ready,
        "stand_ins_presented": held.stand_ins,
    });
    merge(&mut detail, &base);
    let scope = |what: &str| {
        format!(
            "{what}, while → is held for {keys} keys at {KEY_REPEAT_MS} ms over {} ({} files), \
             each key's own frame presented (loupe_presented) before the next key or counted \
             skipped in detail; presented is the update that draws it, not scanout",
            source.label, prepared.count
        )
    };
    let cache = "the look-ahead warm before the first press only (loupe_warm), then as it keeps up";
    for (metric, what, samples) in [
        (
            "held_key_to_presented_ms",
            "Each key to its own frame presented",
            held.key_to_presented,
        ),
        (
            "held_presented_interval",
            "Intervals between the keys' frames presented",
            held.presented_interval,
        ),
        (
            "held_key_interval",
            "Intervals between the keys as the editor handled them",
            held.key_interval,
        ),
    ] {
        figures.push(
            Figure::measured(format!("{}.{metric}", taken.family), "ms", samples)
                .scope(scope(what))
                .cache(cache)
                .target(LOUPE_STEP)
                .detail(detail.clone()),
        );
    }
    Ok(figures)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, elapsed_ms: f64, detail: Value) -> Value {
        json!({"event": name, "elapsed_ms": elapsed_ms, "detail": detail})
    }

    fn step(step: u64, count: u64) -> Value {
        let request = if count == 0 {
            json!({"key": {"key": "e"}})
        } else {
            json!({"loupe": {"arrows": {"direction": "right", "count": count}}})
        };
        event(
            "script_step",
            0.0,
            json!({"step": step, "request": request}),
        )
    }

    fn key(serial: u64, from: u64, ready: bool, pressed_ms: f64) -> Value {
        event(
            "loupe_key",
            pressed_ms + 0.5,
            json!({"key": serial, "from": from, "to": from + 1, "ready": ready,
                "pressed_ms": pressed_ms}),
        )
    }

    fn presented(serial: u64, position: u64, stand_in: bool, at_ms: f64) -> Value {
        event(
            "loupe_presented",
            at_ms,
            json!({"key": serial, "position": position, "stand_in": stand_in,
                "origin": "embedded", "width": 640, "height": 427}),
        )
    }

    /// Two warm presses, one of them cold, then a hold of three keys whose second frame is
    /// presented only after the third key: skipped.
    fn events() -> Vec<Value> {
        vec![
            step(4, 0),
            presented(0, 3, false, 90.0),
            step(5, 1),
            event("loupe_warm", 100.0, json!({"ahead": 4, "ready": 4})),
            key(1, 3, true, 101.0),
            presented(1, 4, false, 104.0),
            step(6, 1),
            event("loupe_warm", 200.0, json!({"ahead": 4, "ready": 4})),
            key(2, 4, false, 201.0),
            presented(2, 5, true, 203.0),
            presented(2, 5, false, 240.0),
            step(7, 3),
            event("loupe_warm", 300.0, json!({"ahead": 4, "ready": 4})),
            key(3, 5, true, 301.0),
            presented(3, 6, false, 303.0),
            key(4, 6, false, 331.0),
            presented(4, 7, true, 332.0),
            key(5, 7, true, 361.0),
            presented(5, 8, false, 364.0),
            presented(5, 7, false, 370.0),
        ]
    }

    #[test]
    fn a_warm_press_is_timed_to_its_own_frame_and_a_cold_one_is_left_out() {
        let warm = warm(&events()).unwrap();
        assert_eq!(warm.samples, vec![3.0]);
        assert_eq!((warm.cold, warm.not_presented), (1, 0));
        assert_eq!(warm.preview, Some(("embedded".into(), (640, 427))));
    }

    #[test]
    fn a_held_key_whose_frame_misses_the_next_key_is_skipped() {
        let held = held(&events()).unwrap().expect("a hold");
        assert_eq!((held.keys, held.ready, held.stand_ins), (3, 2, 1));
        assert_eq!(held.skipped, vec![4], "frame 7 presented only after key 5");
        assert_eq!(held.key_to_presented, vec![2.0, 3.0]);
        assert_eq!(held.presented_interval, vec![61.0]);
        assert_eq!(held.key_interval, vec![30.0, 30.0]);
    }

    #[test]
    fn a_warm_key_counts_the_display_frames_it_spans_and_at_least_one() {
        let interval = 1000.0 / 120.0;
        assert_eq!(frames(0.0, interval), 1.0);
        assert_eq!(frames(1.2, interval), 1.0);
        assert_eq!(frames(interval, interval), 1.0);
        assert_eq!(frames(9.0, interval), 2.0);
        assert_eq!(frames(21.2, interval), 3.0);
    }

    #[test]
    fn a_warm_step_that_pressed_nothing_fails() {
        let mut events = events();
        events.retain(|event| event["detail"]["key"] != 1 || event["event"] != "loupe_key");
        assert!(warm(&events).is_err());
    }

    #[test]
    fn the_plan_starts_at_a_burst_and_holds_where_the_view_has_room() {
        let moments = [
            json!({"kind": "single", "start": 0, "len": 1}),
            json!({"kind": "burst", "start": 3, "len": 5}),
        ];
        assert_eq!(
            plan(120, &moments, 5).unwrap(),
            vec![Plan {
                start: 3,
                warm: 5,
                hold: Some((ArrowKey::Right, 5))
            }]
        );
        let plans = plan(120, &moments, 100).unwrap();
        assert_eq!(
            plans.iter().map(|plan| plan.warm).sum::<u32>(),
            100,
            "{plans:?}"
        );
        assert!(plans.iter().all(|plan| plan.warm <= WARM_PER_LAUNCH));
        assert_eq!(
            plans[0].hold,
            Some((ArrowKey::Right, 76)),
            "neither way holds 100 keys from position 43 of 120: the longer run"
        );
        assert_eq!(
            plan(50, &moments, 30).unwrap()[0].hold,
            Some((ArrowKey::Left, 30)),
            "back over the frames just stepped"
        );
        assert!(plans[1..].iter().all(|plan| plan.hold.is_none()));
        assert!(plan(2, &moments, 5).is_err());
    }

    #[test]
    fn every_script_parses_and_fits_the_step_bound() {
        let prepared = Prepared {
            catalog: "/scratch/catalog.sqlite".into(),
            folder: "/scratch/images".into(),
            count: 120,
            moments: vec![],
            rows: vec![],
        };
        for plan in plan(120, &[], 200).unwrap() {
            let steps = script(&prepared, &plan);
            assert!(steps.len() <= script::MAX_SCRIPT_STEPS);
            assert_eq!(
                script::parse(&script::write(&steps).to_string()).unwrap(),
                steps
            );
        }
    }
}
