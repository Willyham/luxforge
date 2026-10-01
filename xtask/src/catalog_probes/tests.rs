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

/// Every desktop probe run once through the measuring command's desktop step, with four samples,
/// over the 120 JPEGs the probes generate, a folder of 500 generated JPEGs standing in for the
/// 10,000-file folder, and the RAW trip in `CATALOG_PROBES_RAW_TRIP` when it names one. It launches
/// the release editor in the background (`CATALOG_PROBES_BINARY`, by default the workspace's
/// `target/release/luxforge`, which it does not build: run `cargo xtask build --release` first) and
/// writes into `CATALOG_PROBES_OUT`, a new directory (by default one under the system's temporary
/// directory). Each row must hold a distribution of at least one sample, or say why it has none.
#[test]
#[ignore = "launches the release editor in the background; build it first"]
fn the_desktop_probes_run_at_a_tiny_scale() {
    let root = root().unwrap();
    let binary = std::env::var_os("CATALOG_PROBES_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| binary(&root).unwrap());
    assert!(binary.is_file(), "no editor at {}", binary.display());
    let out = std::env::var_os("CATALOG_PROBES_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("luxforge-catalog-probes-{}", std::process::id()))
        });
    assert!(!out.exists(), "{} must be new", out.display());
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
        raw_trip: std::env::var_os("CATALOG_PROBES_RAW_TRIP").map(PathBuf::from),
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
