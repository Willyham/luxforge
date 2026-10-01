//! The report's shape and the data set-up, without timing anything: the full command is run by
//! hand (`--scale tiny` proves it end to end).
use super::{
    Scale,
    data::{self, BURST},
    desktop::{DesktopContext, desktop_probes},
    develop_switch::develop_switch,
    measures,
    report::{self, Report, Row},
};
use crate::{stats, *};

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

/// Every row the report holds carries its status, its step and the load at the step's start and
/// end; a failed step is one `failed` row naming it; the markdown lists every row and why each
/// unmeasured one was not measured.
#[test]
fn catalog_measure_report_rows_carry_status_step_and_load() {
    let dir = tempdir();
    let root = dir.path();
    let mut report = Report::new(root, json!({"tool": "catalog-measure", "scale": "tiny"}));
    report.step(root, "measured", || {
        Ok(vec![
            Row::measured("a.figure", "ms", [3.0, 1.0, 2.0])
                .scope("what was timed")
                .target("under 1 ms")
                .cache("warm"),
        ])
    });
    report.step(root, "absent", || {
        Ok(vec![Row::skipped("b.figure", "ms", "corpus absent")])
    });
    report.step(root, "broken", || Err("it broke".into()));
    let value = report.finish().unwrap();
    assert_eq!(value["status"], "failed");
    let rows = stats::rows(&value);
    assert_eq!(rows.len(), 3);
    for row in rows {
        for key in ["metric", "unit", "distribution", "status", "step", "load"] {
            assert!(row.get(key).is_some(), "{key} missing from {row}");
        }
        for end in ["start", "end"] {
            assert_eq!(row["load"][end]["load_threshold"], launch::LOAD_THRESHOLD);
        }
    }
    assert_eq!(rows[0]["status"], report::MEASURED);
    assert_eq!(rows[0]["distribution"]["p50"], 2.0);
    assert_eq!(rows[0]["scope"], "what was timed");
    assert_eq!(rows[1]["status"], report::SKIPPED);
    assert_eq!(rows[1]["reason"], "corpus absent");
    assert!(rows[1]["distribution"].is_null());
    assert_eq!(
        (rows[2]["metric"].clone(), rows[2]["status"].clone()),
        (json!("broken"), json!(report::FAILED))
    );
    assert_eq!(rows[2]["reason"], "it broke");
    // Both files are written, and the summary names every row.
    let written = read_json(&root.join(report::JSON)).unwrap();
    assert_eq!(written, value);
    let markdown = fs::read_to_string(root.join(report::MARKDOWN)).unwrap();
    for metric in ["a.figure", "b.figure", "broken"] {
        assert!(markdown.contains(&format!("`{metric}`")), "{metric}");
    }
    assert!(markdown.contains("corpus absent") && markdown.contains("it broke"));
}

#[test]
fn catalog_measure_status_is_incomplete_until_every_row_is_measured() {
    let measured = Row::measured("a", "ms", [1.0]).value();
    let not = Row::not_measured("b", "ms", "not built").value();
    assert_eq!(report::status(std::slice::from_ref(&measured)), "passed");
    assert_eq!(
        report::status(&[measured.clone(), not.clone()]),
        "incomplete"
    );
    assert_eq!(
        report::status(&[measured, not, Row::failed("c", "x").value()]),
        "failed"
    );
    // A figure the run reached no sample of is not measured, and says so.
    let empty = Row::measured("d", "ms", Vec::new()).value();
    assert_eq!(empty["status"], report::NOT_MEASURED);
    assert!(empty["reason"].is_string());
}

/// Another tool's row, read back, keeps every sample under its new name.
#[test]
fn catalog_measure_rows_read_back_from_another_report_keep_their_samples() {
    let row = stats::row("input_to_presented_frame", "ms", [5.0, 3.0, 4.0]);
    let renamed = Row::from_report("drag.baseline.input_to_presented_frame", &row).value();
    assert_eq!(renamed["metric"], "drag.baseline.input_to_presented_frame");
    assert_eq!(renamed["unit"], "ms");
    assert_eq!(renamed["distribution"], row["distribution"]);
    assert_eq!(renamed["status"], report::MEASURED);
}

/// A skipped step lists the same metrics a measured one would, so a report without the corpus
/// has every row the design asks for.
#[test]
fn catalog_measure_skipped_figures_keep_their_metric_names() {
    let rows: Vec<Value> = measures::skipped(
        "first_browse",
        &measures::FIRST_BROWSE,
        "corpus absent: no --raw-corpus",
    )
    .into_iter()
    .map(Row::value)
    .collect();
    let metrics: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["metric"].as_str())
        .collect();
    assert_eq!(
        metrics,
        [
            "first_browse.listed",
            "first_browse.first_screen",
            "first_browse.known",
            "first_browse.grid_previews"
        ]
    );
    assert!(rows.iter().all(|row| row["status"] == report::SKIPPED
        && row["reason"] == "corpus absent: no --raw-corpus"
        && row["target"].is_string()
        && row["scope"].is_string()));
}

#[test]
fn catalog_measure_desktop_probes_and_develop_switch_name_their_lane() {
    let dir = tempdir();
    let context = DesktopContext {
        binary: dir.path().join("luxforge"),
        scratch: dir.path().join("desktop"),
        samples: 3,
        folder_10k: dir.path().join("folder"),
        raw_trip: None,
    };
    let desktop: Vec<Value> = desktop_probes(&context)
        .unwrap()
        .into_iter()
        .map(Row::value)
        .collect();
    assert_eq!(desktop.len(), super::desktop::PROBES.len());
    assert!(
        desktop
            .iter()
            .all(|row| row["status"] == report::NOT_MEASURED
                && row["reason"].as_str().unwrap().contains("lane B")
                && row["target"].is_string())
    );
    let switch: Vec<Value> = develop_switch(&context)
        .unwrap()
        .into_iter()
        .map(Row::value)
        .collect();
    assert_eq!(switch.len(), 1);
    assert_eq!(switch[0]["status"], report::NOT_MEASURED);
    assert!(switch[0]["reason"].as_str().unwrap().contains("TASK-021"));
}

#[test]
fn catalog_measure_scales_parse() {
    assert_eq!(Scale::parse("tiny").unwrap(), Scale::Tiny);
    assert_eq!(Scale::parse("full").unwrap(), Scale::Full);
    assert!(Scale::parse("huge").is_err());
    assert_eq!(Scale::Full.sizes().trip, 1_000);
    assert_eq!(Scale::Full.sizes().tree, 200_000);
    assert!(Scale::Tiny.sizes().tree < Scale::Full.sizes().tree);
    assert_eq!(
        (Scale::Full.sizes().links, Scale::Tiny.sizes().links),
        (200_000, 2_000)
    );
}

/// A tree of hard links holds exactly its count of links, in folders of its size, each a link to
/// its source in turn: every link of one source shares its file identity, and each source counts
/// its links.
#[test]
fn catalog_measure_hard_link_tree_shares_each_sources_identity() {
    let dir = tempdir();
    let sources: Vec<PathBuf> = (0..3)
        .map(|index| {
            let path = dir.path().join(format!("source-{index}.jpg"));
            fs::write(&path, format!("image {index}")).unwrap();
            path
        })
        .collect();
    let tree = data::write_links(&sources, &dir.path().join("links"), 7, 3)
        .unwrap()
        .unwrap();
    assert_eq!((tree.files, tree.folders, tree.sources), (7, 3, 3));
    let written = files(&tree.dir).unwrap();
    assert_eq!(written.len(), 7);
    assert_eq!(
        written[0].strip_prefix(&tree.dir).unwrap(),
        Path::new("0000/IMG_000001.jpg")
    );
    assert_eq!(
        written[6].strip_prefix(&tree.dir).unwrap(),
        Path::new("0002/IMG_000007.jpg")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let identity = |path: &Path| {
            let metadata = fs::metadata(path).unwrap();
            (metadata.dev(), metadata.ino())
        };
        for (index, path) in written.iter().enumerate() {
            assert_eq!(identity(path), identity(&sources[index % 3]), "{index}");
        }
        let identities: std::collections::HashSet<(u64, u64)> =
            written.iter().map(|path| identity(path)).collect();
        assert_eq!(identities.len(), 3, "three identities among seven links");
        // Each source counts itself and its links: 0, 3 and 6 link the first.
        let links: Vec<u64> = sources
            .iter()
            .map(|path| fs::metadata(path).unwrap().nlink())
            .collect();
        assert_eq!(links, [4, 3, 3]);
    }
}

/// A file system that refuses a hard link skips the tree with its reason, and the hard-link rows
/// keep their metric names and say so, as every row the design asks for is listed.
#[test]
fn catalog_measure_refused_hard_links_skip_their_rows_with_the_reason() {
    let dir = tempdir();
    let missing = [dir.path().join("missing.jpg")];
    let reason = data::write_links(&missing, &dir.path().join("links"), 2, 1)
        .unwrap()
        .unwrap_err();
    assert!(reason.contains("refused a hard link"), "{reason}");
    let rows: Vec<Value> = measures::first_index_hard_links_skipped(&reason)
        .into_iter()
        .map(Row::value)
        .collect();
    let metrics: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["metric"].as_str())
        .collect();
    assert_eq!(
        metrics,
        [
            "first_index_hard_links.duration",
            "first_index_hard_links.owner_round_trip"
        ]
    );
    assert!(rows.iter().all(|row| row["status"] == report::SKIPPED
        && row["reason"] == reason.as_str()
        && row["target"].is_string()
        && row["scope"].as_str().unwrap().contains("hard links")));
}

/// A tree holds exactly its count of copies, in folders of its size, each source in turn and each
/// copy a file of its own (never a link to its source).
#[test]
fn catalog_measure_tree_holds_its_count_in_folders_each_its_own_file() {
    let dir = tempdir();
    let sources: Vec<PathBuf> = (0..3)
        .map(|index| {
            let path = dir.path().join(format!("source-{index}.jpg"));
            fs::write(&path, format!("image {index}")).unwrap();
            path
        })
        .collect();
    let tree = data::write_tree(&sources, &dir.path().join("tree"), 7, Some(3)).unwrap();
    assert_eq!((tree.files, tree.folders), (7, 3));
    let written = files(&tree.dir).unwrap();
    assert_eq!(written.len(), 7);
    assert_eq!(
        written[0].strip_prefix(&tree.dir).unwrap(),
        Path::new("0000/IMG_000001.jpg")
    );
    assert_eq!(
        written[6].strip_prefix(&tree.dir).unwrap(),
        Path::new("0002/IMG_000007.jpg")
    );
    for (index, path) in written.iter().enumerate() {
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            format!("image {}", index % 3)
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let identities: std::collections::HashSet<u64> = written
            .iter()
            .chain(&sources)
            .map(|path| fs::metadata(path).unwrap().ino())
            .collect();
        assert_eq!(identities.len(), 10, "every copy has its own file identity");
    }
    let flat = data::write_tree(&sources, &dir.path().join("flat"), 4, None).unwrap();
    assert_eq!((flat.files, flat.folders), (4, 1));
    assert_eq!(files(&flat.dir).unwrap().len(), 4);
    assert!(flat.dir.join("IMG_000004.jpg").is_file());
}

/// A header with two EXIF dates, a date that is not one (month 13) and a NUL between them.
fn raw_bytes(tag: &str) -> Vec<u8> {
    format!("II*\0{tag}\02019:05:04 12:00:01\0junk2019:13:04 12:00:01\0DateTime2020:01:02 03:04:05\0tail")
        .into_bytes()
}

/// The trip copies the corpus's RAW files in name order, a burst from each in turn, under a
/// camera's names with each source's extension, rewrites each copy's dates to its frame's time,
/// and leaves every original as it was, a read-only one included.
#[test]
fn catalog_measure_trip_names_frames_and_rewrites_only_the_copies() {
    let dir = tempdir();
    let corpus = dir.path().join("corpus");
    fs::create_dir_all(corpus.join("nested")).unwrap();
    fs::write(corpus.join("b.NEF"), raw_bytes("b")).unwrap();
    fs::write(corpus.join("nested/a.cr2"), raw_bytes("a")).unwrap();
    fs::write(corpus.join(".hidden.NEF"), raw_bytes("hidden")).unwrap();
    fs::write(corpus.join("notes.txt"), "not a RAW file").unwrap();
    let mut read_only = fs::metadata(corpus.join("b.NEF")).unwrap().permissions();
    read_only.set_readonly(true);
    fs::set_permissions(corpus.join("b.NEF"), read_only).unwrap();
    let sources = data::corpus(Some(&corpus)).unwrap();
    assert_eq!(
        sources
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect::<Vec<_>>(),
        ["a.cr2", "b.NEF"]
    );
    let trip = data::write_trip(&sources, &dir.path().join("trip"), 10).unwrap();
    let names: Vec<String> = trip
        .frames
        .iter()
        .map(|frame| {
            frame
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names[0], "DSC_0001.cr2");
    assert_eq!(names[BURST], "DSC_0005.NEF");
    assert_eq!(names[9], "DSC_0010.cr2");
    for (index, frame) in trip.frames.iter().enumerate() {
        assert_eq!(frame.source, sources[(index / BURST) % 2]);
        assert_eq!(frame.capture, data::capture_time(index));
        assert_eq!(frame.rewritten, 2, "both dates of {}", names[index]);
        let bytes = fs::read(&frame.path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(text.matches(frame.capture.as_str()).count(), 2);
        assert!(text.contains("2019:13:04 12:00:01"), "not a date, so kept");
        assert_eq!(bytes.len(), raw_bytes("a").len());
    }
    // The originals are exactly as they were.
    assert_eq!(fs::read(corpus.join("b.NEF")).unwrap(), raw_bytes("b"));
    assert_eq!(
        fs::read(corpus.join("nested/a.cr2")).unwrap(),
        raw_bytes("a")
    );
    assert!(
        fs::metadata(corpus.join("b.NEF"))
            .unwrap()
            .permissions()
            .readonly()
    );
}

#[test]
fn catalog_measure_absent_corpus_is_skipped_with_its_reason() {
    let dir = tempdir();
    assert!(data::corpus(None).unwrap_err().contains("--raw-corpus"));
    assert!(
        data::corpus(Some(&dir.path().join("missing")))
            .unwrap_err()
            .contains("is not a directory")
    );
    fs::write(dir.path().join("notes.txt"), "text").unwrap();
    assert!(
        data::corpus(Some(dir.path()))
            .unwrap_err()
            .contains("holds no RAW files")
    );
}

#[test]
fn catalog_measure_capture_times_spread_like_a_trip() {
    assert_eq!(data::capture_time(0), "2026:06:01 08:30:00");
    assert_eq!(data::capture_time(1), "2026:06:01 08:30:01");
    assert_eq!(data::capture_time(BURST), "2026:06:01 08:37:00");
    // Fifty bursts a day: the 201st frame starts the second day.
    assert_eq!(data::capture_time(200), "2026:06:02 08:30:00");
    assert_eq!(data::capture_time(999), "2026:06:05 14:13:03");
    assert_eq!(data::capture_time(0).len(), 19);
}

#[test]
fn catalog_measure_finds_exif_dates_and_nothing_else() {
    let bytes = raw_bytes("x");
    let found = data::datetimes(&bytes);
    assert_eq!(found.len(), 2);
    assert_eq!(&bytes[found[0]..found[0] + 19], b"2019:05:04 12:00:01");
    assert_eq!(&bytes[found[1]..found[1] + 19], b"2020:01:02 03:04:05");
    assert!(data::datetimes(b"2019:05:04 12:00").is_empty());
    assert!(data::datetimes(b"2019-05-04 12:00:01").is_empty());
}

/// `--card` names any path on a mounted card; the card is the one whose mount point holds it,
/// the longest when several do.
#[test]
fn catalog_measure_card_is_found_by_the_mount_point_holding_the_path() {
    let cards = json!({"cards": [
        {"volume": {"id": "volume-root", "mount_point": "/"}, "dcim": "/DCIM"},
        {"volume": {"id": "volume-z8", "mount_point": "/Volumes/NIKON Z 8"}, "dcim": "/Volumes/NIKON Z 8/DCIM"},
    ]});
    assert_eq!(
        measures::card_volume(&cards, Path::new("/Volumes/NIKON Z 8/DCIM/100NZ8_1")).unwrap(),
        "volume-z8"
    );
    assert_eq!(
        measures::card_volume(&cards, Path::new("/Volumes/NIKON Z 8")).unwrap(),
        "volume-z8"
    );
    assert_eq!(
        measures::card_volume(&cards, Path::new("/Users/someone")).unwrap(),
        "volume-root"
    );
    assert!(
        measures::card_volume(&json!({"cards": []}), Path::new("/Volumes/X"))
            .unwrap_err()
            .to_string()
            .contains("no mounted camera card")
    );
}
