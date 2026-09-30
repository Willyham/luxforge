//! Grid scroll over 10,000 files. The design's target: "presented frames p95 within 16 ms at
//! 120 Hz".
//!
//! The editor browses the folder, read into a catalog of its own first, and one `grid_scroll` step
//! then scrolls the grid down at a steady [`PX_PER_FRAME`] logical pixels on each frame of the
//! window's own frame clock, as a steady trackpad scroll does. Presented is the update in which the
//! grid adopts a new offset (`select_scrolled`), whose redraw draws the grid there: the figure is
//! the interval between consecutive presented updates during the scroll, beside the interval
//! between the frames the scroll rode (`grid_scroll_frame`), whose median is the refresh the scope
//! reports as observed. Each frame's cells are counted by what they draw — a decoded preview, the
//! placeholder while one loads, or nothing ever — and summed in the scope.
use super::{
    Launched, Prepared, ProbeContext, Source, launch, launch_scope, measured, prepare, start,
    stepped,
};
use crate::*;
use luxforge_evidence::{self as script, GridScrollStep, MAX_GRID_SCROLL_FRAMES};

/// The scroll's speed: 60 logical pixels a frame, 7,200 a second at 120 Hz, a brisk steady
/// trackpad scroll through a folder.
const PX_PER_FRAME: f32 = 60.0;
/// The fewest frames one scroll runs for: a second at 120 Hz.
const MIN_FRAMES: u32 = 120;

pub(super) fn probe(root: &Path, context: &ProbeContext) -> Result<Vec<Value>> {
    let source = Source {
        label: "the generated folder".into(),
        folder: context.folder_10k.clone(),
    };
    measure(root, context, &source).map_err(|error| {
        format!(
            "The grid probe ({}): {error}",
            context.scratch.join("grid").display()
        )
        .into()
    })
}

fn measure(root: &Path, context: &ProbeContext, source: &Source) -> Result<Vec<Value>> {
    let prepared = prepare(&context.scratch.join("grid-catalog"), &source.folder)?;
    let frames = u32::try_from(context.samples + 1)
        .unwrap_or(MAX_GRID_SCROLL_FRAMES)
        .clamp(MIN_FRAMES, MAX_GRID_SCROLL_FRAMES);
    let steps = script(&prepared, frames);
    let run = start(root, &context.scratch.join("grid"), &context.binary)?;
    let mut rows = Vec::new();
    run.check(|run| {
        let launched = launch(run, "launch-1", &prepared.catalog, &steps)?;
        rows = launched.read(report(source, &prepared, frames, &launched))?;
        Ok(())
    })?;
    Ok(rows)
}

/// `G`, the folder browsed, and the scroll.
fn script(prepared: &Prepared, frames: u32) -> Vec<script::Step> {
    vec![
        script::Step::key("g"),
        script::Step::Select(script::SelectStep::Folder(
            prepared.folder.to_string_lossy().into_owned(),
        )),
        script::Step::GridScroll(GridScrollStep {
            px_per_frame: PX_PER_FRAME,
            frames,
        }),
    ]
}

/// What one scroll did.
#[derive(Debug, Default, PartialEq)]
struct Scroll {
    presented_interval: Vec<f64>,
    frame_interval: Vec<f64>,
    frames: usize,
    moved: usize,
    reached_end: bool,
    cells: u64,
    cached: u64,
    pending: u64,
    unreadable: u64,
}

fn scroll(events: &[Value]) -> Result<Scroll> {
    let step = events
        .iter()
        .find(|event| {
            event["event"] == "script_step" && event["detail"]["request"]["grid_scroll"].is_object()
        })
        .and_then(|event| event["detail"]["step"].as_u64())
        .ok_or("No grid_scroll step ran")?;
    let presented: Vec<f64> = stepped(events, "select_scrolled")
        .into_iter()
        .filter(|(within, ..)| *within == step)
        .map(|(_, at_ms, _)| at_ms)
        .collect();
    let frames: Vec<&Value> = stepped(events, "grid_scroll_frame")
        .into_iter()
        .filter(|(within, ..)| *within == step)
        .map(|(.., detail)| detail)
        .collect();
    ensure(!frames.is_empty(), "The scroll rode no frame")?;
    let times: Vec<f64> = frames
        .iter()
        .filter_map(|frame| frame["frame_ms"].as_f64())
        .collect();
    let sum = |key: &str| {
        frames
            .iter()
            .filter_map(|frame| frame[key].as_u64())
            .sum::<u64>()
    };
    Ok(Scroll {
        presented_interval: presented.windows(2).map(|pair| pair[1] - pair[0]).collect(),
        frame_interval: times.windows(2).map(|pair| pair[1] - pair[0]).collect(),
        frames: frames.len(),
        moved: frames.iter().filter(|frame| frame["moved"] == true).count(),
        reached_end: frames.iter().any(|frame| frame["moved"] == false),
        cells: sum("cells"),
        cached: sum("cached"),
        pending: sum("pending"),
        unreadable: sum("unreadable"),
    })
}

fn report(
    source: &Source,
    prepared: &Prepared,
    frames: u32,
    launched: &Launched,
) -> Result<Vec<Value>> {
    let scroll = scroll(&launched.events)?;
    let frame_p50 = stats::Distribution::of(scroll.frame_interval.clone()).map(|d| d.p50);
    let mut scope = json!({
        "source": source.label,
        "files": prepared.count,
        "px_per_frame": PX_PER_FRAME,
        "frames_asked": frames,
        "frames_ridden": scroll.frames,
        "frames_moved": scroll.moved,
        "reached_end": scroll.reached_end,
        "refresh_observed_hz": frame_p50.filter(|ms| *ms > 0.0).map(|ms| 1000.0 / ms),
        "cells_drawn": scroll.cells,
        "cells_cached": scroll.cached,
        "cells_pending": scroll.pending,
        "cells_unreadable": scroll.unreadable,
        "target": "presented frames p95 within 16 ms at 120 Hz (provisional)",
        "presented": "the update in which the grid adopts a new offset (select_scrolled), drawn by the redraw it requests; not scanout",
    });
    for (key, value) in launch_scope(launched).as_object().into_iter().flatten() {
        scope[key] = value.clone();
    }
    Ok(vec![
        measured(
            "grid_scroll_presented_interval",
            "ms",
            scroll.presented_interval,
            scope.clone(),
        ),
        measured(
            "grid_scroll_frame_interval",
            "ms",
            scroll.frame_interval,
            scope,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, elapsed_ms: f64, detail: Value) -> Value {
        json!({"event": name, "elapsed_ms": elapsed_ms, "detail": detail})
    }

    fn frame(frame_ms: f64, moved: bool, cached: u64, pending: u64) -> Value {
        event(
            "grid_scroll_frame",
            frame_ms + 1.0,
            json!({"frame_ms": frame_ms, "moved": moved, "cells": cached + pending,
                "cached": cached, "pending": pending, "unreadable": 0}),
        )
    }

    #[test]
    fn a_scroll_is_timed_between_the_updates_that_adopt_its_offsets() {
        let events = vec![
            event(
                "script_step",
                0.0,
                json!({"step": 2, "request": {"select": {"folder": "/f"}}}),
            ),
            event("select_scrolled", 5.0, json!({"scroll": 0.0})),
            event(
                "script_step",
                10.0,
                json!({"step": 3, "request": {"grid_scroll": {"px_per_frame": 60.0, "frames": 4}}}),
            ),
            frame(12.0, true, 20, 4),
            event("select_scrolled", 12.5, json!({"scroll": 60.0})),
            frame(20.3, true, 18, 6),
            event("select_scrolled", 20.9, json!({"scroll": 120.0})),
            frame(28.6, true, 24, 0),
            event("select_scrolled", 45.0, json!({"scroll": 180.0})),
            frame(45.2, false, 24, 0),
        ];
        let scroll = scroll(&events).unwrap();
        assert_eq!(
            scroll.presented_interval.len(),
            2,
            "the folder's own is not the scroll's"
        );
        assert!((scroll.presented_interval[0] - 8.4).abs() < 1e-9);
        assert!((scroll.presented_interval[1] - 24.1).abs() < 1e-9);
        assert_eq!(
            (scroll.frames, scroll.moved, scroll.reached_end),
            (4, 3, true)
        );
        assert_eq!((scroll.cells, scroll.cached, scroll.pending), (96, 86, 10));
        assert_eq!(scroll.frame_interval.len(), 3);
    }

    #[test]
    fn the_script_parses() {
        let prepared = Prepared {
            catalog: "/scratch/catalog.sqlite".into(),
            folder: "/scratch/images".into(),
            count: 10_000,
            moments: vec![],
            rows: vec![],
        };
        let steps = script(&prepared, 240);
        assert_eq!(
            script::parse(&script::write(&steps).to_string()).unwrap(),
            steps
        );
    }
}
