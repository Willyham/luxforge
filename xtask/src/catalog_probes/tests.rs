//! The probes end to end at a tiny scale: they run and answer well-formed rows. This proves the
//! probes, not the editor's speed: its figures are not a measurement.
use super::*;

/// Every desktop probe run once with four samples over the 120 generated JPEGs, a folder of 500
/// generated JPEGs standing in for the 10,000-file folder, and the RAW trip in
/// `CATALOG_PROBES_RAW_TRIP` when it names one. It launches the release editor in the background
/// (`CATALOG_PROBES_BINARY`, by default the workspace's `target/release/luxforge`, which it does
/// not build: run `cargo xtask build --release` first) and writes into `CATALOG_PROBES_OUT`, a new
/// directory (by default one under the system's temporary directory). Each row must hold a
/// distribution of at least one sample with its scope, or say why it was not measured.
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
    let context = ProbeContext {
        binary,
        scratch: out.join("probes"),
        samples: 4,
        folder_10k: grid.join("images"),
        raw_trip: std::env::var_os("CATALOG_PROBES_RAW_TRIP").map(PathBuf::from),
    };
    let rows = desktop_probes(&context).unwrap();
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
    assert!(!rows.is_empty());
    for row in &rows {
        assert!(row["metric"].as_str().is_some(), "{row}");
        if row["status"] == "not_measured" {
            assert!(
                row["reason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty()),
                "{row}"
            );
        } else {
            assert_eq!(row["unit"], "ms", "{row}");
            assert!(row["distribution"]["count"].as_u64() > Some(0), "{row}");
            assert!(row["scope"].is_object(), "{row}");
        }
    }
}
