//! The figures carry the measuring command's names and units; and, ignored, the probes end to end
//! at a tiny scale through the command's desktop step: they run and answer well-formed rows. That
//! proves the probes, not the editor's speed: its figures are not a measurement.
use super::*;
use crate::catalog_measure::desktop::{self, DesktopContext};

/// Each figure the design names is one the measuring command lists, in its unit.
#[test]
fn the_designs_figures_are_named_as_the_measuring_command_names_them() {
    for (metric, unit) in [
        (GRID_FRAME_TIME, "ms"),
        (LOUPE_STEP, "frames"),
        (FOCUS_EMBEDDED, "ms"),
        (FOCUS_DEVELOPMENT, "ms"),
        (MEMORY_GRID, "MiB"),
        (MEMORY_LOUPE, "MiB"),
    ] {
        assert!(
            PROBES
                .iter()
                .any(|probe| probe.0 == metric && probe.1 == unit),
            "{metric} in {unit}"
        );
        let figure = Figure::measured(metric, "ms", vec![1.0]).target(metric);
        assert!(figure.target.is_some_and(|target| !target.is_empty()));
    }
}

/// The memory figures: every sample the grid's decoded bytes were read at, and each run's loupe
/// high-water mark, one sample a run.
#[test]
fn the_memory_figures_hold_every_grid_sample_and_each_runs_loupe_peak() {
    let memory = Memory {
        grid: vec![10.0, 40.0, 25.0],
        grid_budget: Some(192.0),
        loupe: vec![
            json!({"run": "loupe", "peak_mib": 30.0, "budget_mib": 256.0, "held_arrow_keys": 4}),
            json!({"run": "focus", "peak_mib": 12.0, "budget_mib": 256.0}),
        ],
    };
    let figures = memory.figures();
    assert_eq!(figures[0].metric, MEMORY_GRID);
    assert_eq!(
        figures[0].outcome,
        Outcome::Measured(vec![10.0, 40.0, 25.0])
    );
    assert_eq!(figures[0].detail["peak_mib"], 40.0);
    assert_eq!(figures[1].metric, MEMORY_LOUPE);
    assert_eq!(figures[1].outcome, Outcome::Measured(vec![30.0, 12.0]));
    assert_eq!(figures[1].detail["runs"][0]["held_arrow_keys"], 4);
}

/// Each probe run answers on its own: with no editor to launch and no folder for the grid or the
/// trip, every run fails, each as one failed row of its own naming its run directory, and the rows
/// of the others — the memory figures here — still stand. One probe's error never loses another's
/// rows.
#[test]
fn a_probe_run_that_fails_is_its_own_failed_row_beside_the_others() {
    let out = std::env::temp_dir().join(format!(
        "luxforge-catalog-probes-failing-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&out);
    let context = DesktopContext {
        binary: out.join("no-editor"),
        scratch: out.join("probes"),
        samples: 1,
        folder_10k: out.join("no-folder"),
        raw_trip: Some(out.join("no-trip")),
    };
    let rows: Vec<Value> = desktop::desktop_probes(&context)
        .unwrap()
        .into_iter()
        .map(|row| row.value())
        .collect();
    let failed: Vec<&str> = rows
        .iter()
        .filter(|row| row["status"] == "failed")
        .filter_map(|row| row["metric"].as_str())
        .collect();
    assert_eq!(
        failed,
        [
            "desktop.probe.grid",
            "desktop.probe.loupe",
            "desktop.probe.loupe-raw",
            "desktop.probe.focus",
            "desktop.probe.focus-raw",
        ],
        "{rows:#?}"
    );
    for row in rows.iter().filter(|row| row["status"] == "failed") {
        let run = row["detail"]["run"].as_str().unwrap();
        let dir = context.scratch.join(run);
        assert!(
            row["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains(&dir.display().to_string())),
            "{row}"
        );
        assert_eq!(row["detail"]["dir"], json!(dir));
    }
    for metric in [MEMORY_GRID, MEMORY_LOUPE] {
        assert!(
            rows.iter()
                .any(|row| row["metric"] == metric && row["status"] == "not_measured"),
            "{metric}: {rows:#?}"
        );
    }
    fs::remove_dir_all(&out).unwrap();
}

/// The release editor the ignored runs launch in the background (`CATALOG_PROBES_BINARY`, by
/// default the workspace's `target/release/luxforge`, which they do not build: run
/// `cargo xtask build --release` first), and the new directory they write into
/// (`CATALOG_PROBES_OUT`, by default one under the system's temporary directory named `name`).
fn editor_and_out(name: &str) -> (PathBuf, PathBuf) {
    let root = root().unwrap();
    let binary = std::env::var_os("CATALOG_PROBES_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| binary(&root).unwrap());
    assert!(binary.is_file(), "no editor at {}", binary.display());
    let out = std::env::var_os("CATALOG_PROBES_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("luxforge-{name}-{}", std::process::id()))
        });
    assert!(!out.exists(), "{} must be new", out.display());
    (binary, out)
}

/// The RAW trip an ignored run browses: the folder `CATALOG_PROBES_RAW_TRIP` names, else one
/// written under `out` from the RAW corpus `CATALOG_PROBES_RAW_CORPUS` names, as `catalog-measure`
/// writes its own — `CATALOG_PROBES_TRIP_FRAMES` frames (1,000 by default, the design's scale),
/// copies under new names whose EXIF dates are rewritten, the corpus only read — else none.
fn raw_trip(out: &Path) -> Option<PathBuf> {
    if let Some(trip) = std::env::var_os("CATALOG_PROBES_RAW_TRIP") {
        return Some(trip.into());
    }
    let corpus = std::env::var_os("CATALOG_PROBES_RAW_CORPUS").map(PathBuf::from)?;
    let frames = std::env::var("CATALOG_PROBES_TRIP_FRAMES")
        .map_or(1_000, |frames| frames.parse().expect("a frame count"));
    let sources = crate::catalog_measure::data::corpus(Some(&corpus)).unwrap();
    let trip =
        crate::catalog_measure::data::write_trip(&sources, &out.join("trip"), frames).unwrap();
    Some(trip.dir)
}

/// Every desktop probe run once through the measuring command's desktop step, with four samples,
/// over the 120 JPEGs the probes generate, a folder of 500 generated JPEGs standing in for the
/// 10,000-file folder, and the RAW trip [`raw_trip`] names, if any. Every row must be there, none
/// failed, each holding a distribution of at least one sample or saying why it has none.
#[test]
#[ignore = "launches the release editor in the background; build it first"]
fn the_desktop_probes_run_at_a_tiny_scale() {
    let (binary, out) = editor_and_out("catalog-probes");
    let grid = out.join("grid-folder");
    generate_catalog::run(
        &grid,
        &generate_catalog::Options {
            seed: 2,
            files: None,
            assets: None,
            images: Some(500),
        },
    )
    .unwrap();
    let context = DesktopContext {
        binary,
        scratch: out.join("probes"),
        samples: 4,
        folder_10k: grid.join("images"),
        raw_trip: raw_trip(&out),
    };
    let rows: Vec<Value> = desktop::desktop_probes(&context)
        .unwrap()
        .into_iter()
        .map(|row| row.value())
        .collect();
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
    for (metric, unit, _) in PROBES {
        assert!(
            rows.iter()
                .any(|row| row["metric"] == metric && row["unit"] == unit),
            "no {metric} row in {unit}"
        );
    }
    for row in &rows {
        assert!(row["metric"].as_str().is_some(), "{row}");
        assert_ne!(row["status"], "failed", "{row}");
        if row["status"] == "measured" {
            assert!(row["distribution"]["count"].as_u64() > Some(0), "{row}");
            assert!(
                row["scope"].as_str().is_some_and(|scope| !scope.is_empty()),
                "{row}"
            );
        } else {
            assert!(
                row["reason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty()),
                "{row}"
            );
        }
    }
}

/// The `focus-raw` probe alone over a RAW trip like the measurement's ([`raw_trip`]), with
/// `CATALOG_PROBES_SAMPLES` samples a camera (30 by default, `catalog-measure`'s): every one of its
/// launches must settle every step — the loupe opened on frames anywhere in the trip, including
/// past the grid's first block of rows, and on RAWs whose loupe tier needs a development or is
/// refused — and the run must answer its figures. It proves the probe settles, not the editor's
/// speed: its figures are not a measurement.
#[test]
#[ignore = "launches the release editor in the background over a RAW trip; build it first"]
fn the_focus_raw_probe_settles_every_launch_over_a_raw_trip() {
    let (binary, out) = editor_and_out("catalog-probes-focus-raw");
    let trip = raw_trip(&out).expect("CATALOG_PROBES_RAW_TRIP or CATALOG_PROBES_RAW_CORPUS");
    let samples = std::env::var("CATALOG_PROBES_SAMPLES")
        .map_or(30, |samples| samples.parse().expect("a sample count"));
    let context = ProbeContext {
        binary,
        scratch: out.join("probes"),
        samples,
        folder_10k: PathBuf::new(),
        raw_trip: Some(trip.clone()),
    };
    fs::create_dir_all(&context.scratch).unwrap();
    let source = Source {
        label: "the RAW trip".into(),
        folder: trip,
    };
    let mut memory = Memory::default();
    let figures = focus::measure(
        &root().unwrap(),
        &context,
        "focus-raw",
        &source,
        true,
        &mut memory,
    )
    .unwrap();
    let run = read_json(&context.scratch.join("focus-raw/result.json")).unwrap();
    let launches = run["launches"].as_array().unwrap();
    println!(
        "focus-raw: {} launches, exit codes {:?}",
        launches.len(),
        launches
            .iter()
            .map(|launch| launch["exit_code"].clone())
            .collect::<Vec<_>>()
    );
    assert!(launches.iter().all(|launch| launch["exit_code"] == 0));
    for figure in &figures {
        let count = match &figure.outcome {
            Outcome::Measured(samples) => samples.len().to_string(),
            other => format!("{other:?}"),
        };
        println!(
            "{} ({}): {count} samples, detail {}",
            figure.metric, figure.unit, figure.detail
        );
    }
    assert!(!figures.is_empty());
}
