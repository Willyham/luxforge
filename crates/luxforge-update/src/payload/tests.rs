//! The payload updater in temporary directories, with test keys and an in-memory fetch: choosing an
//! update, refusing what a download or an archive must not be, the swap and its cleanup, the
//! notify fallback, cancellation and the desktop entry.
use super::{
    testing::{self, EXECUTABLE, Entry, FEED_BASE, MemoryFetch},
    *,
};
use crate::{SigningKey, Target};
use std::sync::Arc;

const TARGET: Target = Target::LinuxX86_64;
const ARCHIVE_URL: &str = "https://github.com/Willyham/luxforge/releases/download/v0.2.0/a.tar.gz";

fn feed_url() -> String {
    format!("{FEED_BASE}{}", TARGET.feed_path())
}

/// A payload `<tmp>/apps/Luxforge` at `version` and `build`.
struct Installation {
    _tmp: tempfile::TempDir,
    parent: PathBuf,
    payload: Payload,
}

fn installed(version: &str, build: u64) -> Installation {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().canonicalize().unwrap().join("apps");
    let executable = testing::install_payload(&parent.join("Luxforge"), version, build, TARGET);
    Installation {
        payload: Payload::locate(&executable).unwrap(),
        parent,
        _tmp: tmp,
    }
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// A fetch serving `archive` at [`ARCHIVE_URL`], and its item at `build` signed by `key`.
fn served(archive: &[u8], build: u64, key: &SigningKey) -> (Arc<MemoryFetch>, AppcastItem) {
    let fetch = MemoryFetch::new();
    fetch.serve(ARCHIVE_URL, archive.to_vec());
    let item = testing::item("0.2.0", build, ARCHIVE_URL, archive, key, None, Some(1));
    (fetch, item)
}

fn prepare(installation: &Installation, fetch: &MemoryFetch, item: &AppcastItem) -> Result<Staged> {
    let key = testing::test_key().public_key();
    installation
        .payload
        .prepare(fetch, item, &key, &Cancel::new(), &mut |_, _| {})
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    path.symlink_metadata().unwrap().permissions().mode() & 0o7777
}

#[test]
fn a_payload_is_located_by_its_build_json() {
    let installation = installed("0.1.0", 7);
    let payload = &installation.payload;
    assert_eq!(
        payload.build(),
        &BuildInfo {
            version: "0.1.0".into(),
            build: 7,
            target: TARGET
        }
    );
    assert_eq!(payload.dir(), installation.parent.join("Luxforge"));
    assert_eq!(payload.executable(), payload.dir().join(EXECUTABLE));
    assert_eq!(
        payload.previous(),
        installation.parent.join("Luxforge.previous")
    );
    assert_eq!(
        payload.staging(9),
        installation.parent.join(".luxforge-update-9")
    );
    // A development binary has no build.json beside it.
    let tmp = tempfile::tempdir().unwrap();
    let loose = tmp.path().join("luxforge");
    fs::write(&loose, b"").unwrap();
    assert_eq!(Payload::locate(&loose).unwrap_err().code(), "not-a-payload");
}

#[test]
fn build_json_is_read_as_the_release_build_writes_it() {
    let written = BuildInfo::parse(&testing::build_json("0.2.0-rc.1", 12, TARGET)).unwrap();
    assert_eq!(
        (written.version.as_str(), written.build),
        ("0.2.0-rc.1", 12)
    );
    assert_eq!(written.target, TARGET);
    let numeric =
        BuildInfo::parse(r#"{"version":"1.0.0","build_number":3,"target":"linux-x86_64"}"#)
            .unwrap();
    assert_eq!((numeric.build, numeric.target), (3, TARGET));
    for bad in [
        "not json",
        r#"{"build_number":"3","target":"linux-x86_64"}"#,
        r#"{"version":"1.0.0","build_number":"3a","target":"linux-x86_64"}"#,
        r#"{"version":"1.0.0","build_number":-3,"target":"linux-x86_64"}"#,
        r#"{"version":"1.0.0","build_number":"3","target":"riscv64-linux"}"#,
    ] {
        assert_eq!(
            BuildInfo::parse(bad).unwrap_err().code(),
            "not-a-payload",
            "{bad}"
        );
    }
}

#[test]
fn check_diagnostics_keep_original_rss_positions_after_parser_skips() {
    let key = testing::test_key();
    let item = |build, format| testing::item("0.2.0", build, ARCHIVE_URL, b"x", &key, None, format);
    let xml = testing::feed(vec![
        item(5, Some(1)),
        item(6, None),
        item(7, Some(1)),
        item(8, Some(1)),
    ])
    .replacen(
        "<sparkle:version>5</sparkle:version>",
        "<sparkle:version>bad</sparkle:version>",
        1,
    )
    .replace(
        "<sparkle:version>8</sparkle:version>",
        "<sparkle:version>bad</sparkle:version>",
    );
    let fetch = MemoryFetch::new();
    fetch.serve(&feed_url(), xml.into_bytes());
    let installation = installed("0.1.0", 4);
    let checked = installation
        .payload
        .check(&*fetch, FEED_BASE, &[Channel::Default], &Cancel::new())
        .unwrap();
    assert_eq!(checked.item.unwrap().build, 7);
    assert_eq!(
        checked
            .skipped
            .iter()
            .map(|item| item.index)
            .collect::<Vec<_>>(),
        vec![0, 1, 3]
    );
    assert_eq!(checked.skipped[1].reason, "no luxforge:catalogFormat");
}

#[test]
fn a_check_offers_the_newest_followed_build_with_a_catalog_format() {
    let key = testing::test_key();
    let item = |version: &str, build, channel, format| {
        testing::item(version, build, ARCHIVE_URL, b"x", &key, channel, format)
    };
    let fetch = MemoryFetch::new();
    fetch.serve(
        &feed_url(),
        testing::feed(vec![
            item("0.3.0", 9, None, None),
            item("0.2.1-rc.1", 7, Some("prerelease"), Some(1)),
            item("0.2.0", 5, None, Some(1)),
            item("0.1.1", 3, None, Some(1)),
            item("9.9.9", 99, Some("nightly"), Some(1)),
        ]),
    );
    let installation = installed("0.1.0", 4);
    let check = |followed: &[Channel]| {
        installation
            .payload
            .check(&*fetch, FEED_BASE, followed, &Cancel::new())
            .unwrap()
    };
    let default = check(&[Channel::Default]);
    assert_eq!(default.item.unwrap().build, 5);
    // The item without a catalog format is never offered, and says why.
    assert_eq!(
        default.skipped,
        vec![SkippedItem {
            index: 0,
            reason: "no luxforge:catalogFormat".into()
        }]
    );
    let both = check(&[Channel::Default, Channel::Prerelease]);
    assert_eq!(both.item.unwrap().version, "0.2.1-rc.1");
    assert_eq!(fetch.requests(), vec![feed_url(), feed_url()]);
    // Nothing above this build.
    let newest = installed("0.3.0", 9);
    let checked = newest
        .payload
        .check(
            &*fetch,
            FEED_BASE,
            &[Channel::Default, Channel::Prerelease],
            &Cancel::new(),
        )
        .unwrap();
    assert_eq!(checked.item, None);
}

#[test]
fn a_check_refuses_a_failed_oversize_or_malformed_feed() {
    let installation = installed("0.1.0", 1);
    let check = |fetch: &MemoryFetch| {
        installation
            .payload
            .check(fetch, FEED_BASE, &[Channel::Default], &Cancel::new())
            .unwrap_err()
            .code()
    };
    let fetch = MemoryFetch::new();
    fetch.fail(&feed_url(), "the server answered HTTP 503");
    assert_eq!(check(&fetch), "fetch-failed");
    fetch.serve(&feed_url(), vec![b' '; crate::MAX_APPCAST_BYTES + 1]);
    assert_eq!(check(&fetch), "oversize");
    fetch.serve(&feed_url(), b"<html/>".to_vec());
    assert_eq!(check(&fetch), "malformed-appcast");
    fetch.serve(&feed_url(), vec![0xff, 0xfe]);
    assert_eq!(check(&fetch), "malformed-appcast");
    let cancel = Cancel::new();
    cancel.cancel();
    fetch.serve(&feed_url(), testing::feed(Vec::new()));
    assert_eq!(
        installation
            .payload
            .check(&*fetch, FEED_BASE, &[Channel::Default], &cancel)
            .unwrap_err()
            .code(),
        "cancelled"
    );
}

#[test]
fn an_update_is_staged_installed_and_its_previous_payload_removed_after_launch() {
    for method in [SwapMethod::Renames, SwapMethod::Auto] {
        let installation = installed("0.1.0", 1);
        let archive = testing::release_archive("0.2.0", 2, TARGET);
        let (fetch, item) = served(&archive, 2, &testing::test_key());
        let mut reported = Vec::new();
        let staged = installation
            .payload
            .prepare(
                &*fetch,
                &item,
                &testing::test_key().public_key(),
                &Cancel::new(),
                &mut |received, total| reported.push((received, total)),
            )
            .unwrap();
        let length = archive.len() as u64;
        assert_eq!(reported.last(), Some(&(length, length)));
        assert!(reported.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!((staged.version(), staged.build()), ("0.2.0", 2));
        assert_eq!(
            staged.staging(),
            installation.parent.join(".luxforge-update-2")
        );
        // The archive is gone once extracted; only the new payload is staged.
        assert_eq!(names(staged.staging()), vec![NEW_PAYLOAD]);
        let new = staged.new_payload();
        assert_eq!(names(&new), vec!["build.json", "lensfun", EXECUTABLE]);
        #[cfg(unix)]
        {
            assert_eq!(mode(&new.join(EXECUTABLE)), 0o755);
            assert_eq!(mode(&new.join("build.json")), 0o644);
            assert_eq!(mode(&new.join("lensfun")), 0o755);
        }
        // Nothing changed in the installed payload yet.
        assert_eq!(
            BuildInfo::read(installation.payload.dir()).unwrap().build,
            1
        );

        let installed = installation
            .payload
            .install_with(&staged, None, method)
            .unwrap();
        assert_eq!(installed.executable, installation.payload.executable());
        assert_eq!(
            installed.exchanged,
            cfg!(target_os = "linux") && method == SwapMethod::Auto,
            "{method:?}"
        );
        assert_eq!(installed.previous, Some(installation.payload.previous()));
        assert_eq!(installed.desktop_entry, Ok(false));
        assert_eq!(
            names(&installation.parent),
            vec!["Luxforge", "Luxforge.previous"]
        );
        assert_eq!(
            BuildInfo::read(installation.payload.dir()).unwrap().build,
            2
        );
        assert_eq!(
            fs::read_to_string(installation.payload.executable()).unwrap(),
            "luxforge 0.2.0\n"
        );
        assert_eq!(
            BuildInfo::read(&installation.payload.previous())
                .unwrap()
                .build,
            1
        );

        // The new version's first successful launch removes the way back and any stray staging.
        fs::create_dir(installation.parent.join(".luxforge-update-7")).unwrap();
        let launched = Payload::locate(&installation.payload.executable()).unwrap();
        assert_eq!(launched.build().build, 2);
        let cleaned = launched.clean_after_launch().unwrap();
        assert!(cleaned.previous);
        assert_eq!(
            cleaned.staging,
            vec![installation.parent.join(".luxforge-update-7")]
        );
        assert_eq!(names(&installation.parent), vec!["Luxforge"]);
        assert_eq!(launched.clean_after_launch().unwrap(), Cleaned::default());
    }
}

#[test]
#[cfg(target_os = "linux")]
fn a_failed_filesystem_sync_refuses_the_update_and_preserves_the_installed_payload() {
    let installation = installed("0.1.0", 1);
    let archive = testing::release_archive("0.2.0", 2, TARGET);
    let (fetch, item) = served(&archive, 2, &testing::test_key());
    let before = fs::read(installation.payload.executable()).unwrap();
    SYNC_FAILURE.with(|failure| failure.set(Some(std::io::ErrorKind::Other)));

    let error = prepare(&installation, &fetch, &item).unwrap_err();

    assert!(matches!(error, Error::Io(error) if error.kind() == std::io::ErrorKind::Other));
    assert_eq!(names(&installation.parent), vec!["Luxforge"]);
    assert_eq!(fs::read(installation.payload.executable()).unwrap(), before);
    assert_eq!(
        BuildInfo::read(installation.payload.dir()).unwrap().build,
        1
    );
}

#[test]
fn an_install_replaces_an_earlier_previous_payload_and_refuses_a_changed_stage() {
    let installation = installed("0.1.0", 1);
    testing::install_payload(&installation.payload.previous(), "0.0.9", 0, TARGET);
    let archive = testing::release_archive("0.2.0", 2, TARGET);
    let (fetch, item) = served(&archive, 2, &testing::test_key());
    let staged = prepare(&installation, &fetch, &item).unwrap();
    // A staged payload changed after verification is refused before anything moves.
    let build = staged.new_payload().join(BUILD_FILE);
    let verified = fs::read(&build).unwrap();
    fs::write(&build, testing::build_json("0.2.0", 3, TARGET)).unwrap();
    let refused = installation.payload.install(&staged, None).unwrap_err();
    assert_eq!(refused.code(), "install-failed");
    assert_eq!(
        BuildInfo::read(installation.payload.dir()).unwrap().build,
        1
    );
    assert_eq!(
        BuildInfo::read(&installation.payload.previous())
            .unwrap()
            .build,
        0
    );
    fs::write(&build, verified).unwrap();
    installation.payload.install(&staged, None).unwrap();
    assert_eq!(
        BuildInfo::read(&installation.payload.previous())
            .unwrap()
            .build,
        1
    );
    assert_eq!(
        BuildInfo::read(installation.payload.dir()).unwrap().build,
        2
    );
}

#[test]
fn a_bad_signature_is_refused_and_leaves_nothing_behind() {
    let installation = installed("0.1.0", 1);
    let archive = testing::release_archive("0.2.0", 2, TARGET);
    let other = SigningKey::from_seed(&[7; 32]).unwrap();
    let (fetch, item) = served(&archive, 2, &other);
    assert_eq!(
        prepare(&installation, &fetch, &item).unwrap_err().code(),
        "bad-signature"
    );
    assert_eq!(names(&installation.parent), vec!["Luxforge"]);
    // A signature over other bytes of the same length.
    let mut tampered = archive.clone();
    *tampered.last_mut().unwrap() ^= 1;
    let (fetch, item) = served(&archive, 2, &testing::test_key());
    fetch.serve(ARCHIVE_URL, tampered);
    assert_eq!(
        prepare(&installation, &fetch, &item).unwrap_err().code(),
        "bad-signature"
    );
    assert_eq!(names(&installation.parent), vec!["Luxforge"]);
}

#[test]
fn a_truncated_or_oversize_download_is_refused() {
    let installation = installed("0.1.0", 1);
    let archive = testing::release_archive("0.2.0", 2, TARGET);
    let (fetch, item) = served(&archive, 2, &testing::test_key());
    fetch.serve(ARCHIVE_URL, archive[..archive.len() - 10].to_vec());
    assert!(matches!(
        prepare(&installation, &fetch, &item),
        Err(Error::LengthMismatch { expected, actual })
            if expected == archive.len() as u64 && actual == expected - 10
    ));
    assert_eq!(names(&installation.parent), vec!["Luxforge"]);
    let mut longer = archive.clone();
    longer.extend_from_slice(&[0; 40_000]);
    fetch.serve(ARCHIVE_URL, longer);
    assert!(matches!(
        prepare(&installation, &fetch, &item),
        Err(Error::Oversize { limit }) if limit == archive.len() as u64
    ));
    assert_eq!(names(&installation.parent), vec!["Luxforge"]);
    // A feed item stating more than the absolute cap is refused before anything is fetched.
    let mut huge = item.clone();
    huge.enclosure.length = MAX_ARCHIVE_BYTES + 1;
    let requests = fetch.requests().len();
    assert_eq!(
        prepare(&installation, &fetch, &huge).unwrap_err().code(),
        "oversize"
    );
    assert_eq!(fetch.requests().len(), requests);
    // A failed request names it.
    fetch.fail(ARCHIVE_URL, "the server answered HTTP 404");
    let failed = prepare(&installation, &fetch, &item).unwrap_err();
    assert_eq!(
        failed.to_string(),
        "fetch-failed: the server answered HTTP 404"
    );
    assert_eq!(names(&installation.parent), vec!["Luxforge"]);
}

#[test]
fn traversal_link_escapes_and_special_entries_are_refused() {
    let base = || testing::release_entries("0.2.0", 2, TARGET);
    let file = |path: &str| Entry::File(path.into(), b"x".to_vec(), 0o644);
    let cases: Vec<(&str, Entry)> = vec![
        ("parent", file("Luxforge/../escaped")),
        ("nested parent", file("Luxforge/lensfun/../../escaped")),
        ("absolute", file("/tmp/escaped")),
        ("outside the root", file("Other/escaped")),
        ("bare file", file("escaped")),
        (
            "absolute link",
            Entry::Symlink("Luxforge/passwd".into(), "/etc/passwd".into()),
        ),
        (
            "climbing link",
            Entry::Symlink("Luxforge/up".into(), "../..".into()),
        ),
        (
            "climbing link in a directory",
            Entry::Symlink("Luxforge/lensfun/up".into(), "../build.json".into()),
        ),
        (
            "hard link outside",
            Entry::HardLink("Luxforge/passwd".into(), "/etc/passwd".into()),
        ),
        (
            "hard link climbing",
            Entry::HardLink("Luxforge/passwd".into(), "Luxforge/../../passwd".into()),
        ),
        (
            "hard link to nothing",
            Entry::HardLink("Luxforge/copy".into(), "Luxforge/missing".into()),
        ),
        ("fifo", Entry::Fifo("Luxforge/fifo".into())),
        ("device", Entry::CharDevice("Luxforge/null".into())),
        ("duplicate", file("Luxforge/build.json")),
    ];
    let key = testing::test_key();
    for (name, entry) in cases {
        let installation = installed("0.1.0", 1);
        let mut entries = base();
        entries.push(entry);
        let archive = testing::archive(&entries);
        let (fetch, item) = served(&archive, 2, &key);
        let refused = prepare(&installation, &fetch, &item).unwrap_err();
        assert_eq!(refused.code(), "unsafe-archive", "{name}: {refused}");
        assert_eq!(names(&installation.parent), vec!["Luxforge"], "{name}");
        assert!(
            !installation
                .parent
                .parent()
                .unwrap()
                .join("escaped")
                .exists(),
            "{name}"
        );
    }
    // Links that stay inside are kept.
    let installation = installed("0.1.0", 1);
    let mut entries = base();
    entries.push(Entry::Symlink(
        "Luxforge/lenses.xml".into(),
        "lensfun/lenses.xml".into(),
    ));
    entries.push(Entry::HardLink(
        "Luxforge/lensfun/copy.xml".into(),
        "Luxforge/lensfun/lenses.xml".into(),
    ));
    let archive = testing::archive(&entries);
    let (fetch, item) = served(&archive, 2, &key);
    let staged = prepare(&installation, &fetch, &item).unwrap();
    let new = staged.new_payload();
    assert_eq!(
        fs::read_link(new.join("lenses.xml")).unwrap(),
        Path::new("lensfun/lenses.xml")
    );
    assert_eq!(
        fs::read(new.join("lensfun/copy.xml")).unwrap(),
        b"<lensdatabase/>\n"
    );
}

#[test]
fn extraction_stays_under_its_size_and_entry_caps() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("a.tar.gz");
    fs::write(&archive, testing::release_archive("0.2.0", 2, TARGET)).unwrap();
    let extract = |name: &str, limits| {
        extract::extract(&archive, &tmp.path().join(name), &Cancel::new(), limits)
    };
    let refused = extract(
        "bytes",
        extract::Limits {
            bytes: 64,
            entries: 100,
        },
    )
    .unwrap_err();
    assert_eq!(refused.code(), "unsafe-archive", "{refused}");
    let refused = extract(
        "entries",
        extract::Limits {
            bytes: 1 << 20,
            entries: 3,
        },
    )
    .unwrap_err();
    assert_eq!(refused.code(), "unsafe-archive", "{refused}");
    extract("whole", extract::Limits::DEFAULT).unwrap();
}

#[test]
fn a_build_json_that_is_not_the_feed_items_is_refused() {
    let key = testing::test_key();
    for (name, archive) in [
        ("build", testing::release_archive("0.2.0", 3, TARGET)),
        ("version", testing::release_archive("0.2.1", 2, TARGET)),
        (
            "target",
            testing::release_archive("0.2.0", 2, Target::MacosArm64),
        ),
        ("missing", {
            let mut entries = testing::release_entries("0.2.0", 2, TARGET);
            entries.retain(
                |entry| !matches!(entry, Entry::File(path, ..) if path.ends_with(BUILD_FILE)),
            );
            testing::archive(&entries)
        }),
        ("no executable", {
            let mut entries = testing::release_entries("0.2.0", 2, TARGET);
            entries.retain(
                |entry| !matches!(entry, Entry::File(path, ..) if path.ends_with(EXECUTABLE)),
            );
            testing::archive(&entries)
        }),
    ] {
        let installation = installed("0.1.0", 1);
        let (fetch, item) = served(&archive, 2, &key);
        let refused = prepare(&installation, &fetch, &item).unwrap_err();
        assert_eq!(refused.code(), "build-mismatch", "{name}: {refused}");
        assert_eq!(names(&installation.parent), vec!["Luxforge"], "{name}");
    }
}

#[test]
fn a_package_marker_or_an_unwritable_place_gives_notify_mode() {
    let installation = installed("0.1.0", 1);
    assert_eq!(installation.payload.mode(), Mode::Payload);
    // The probe leaves nothing behind.
    assert_eq!(names(&installation.parent), vec!["Luxforge"]);
    let marker = installation.payload.dir().join(PACKAGE_MARKER);
    fs::write(&marker, b"").unwrap();
    assert_eq!(
        installation.payload.mode(),
        Mode::Notify(NotifyReason::PackageManaged)
    );
    fs::remove_file(&marker).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if running_as_root(&installation.parent) {
            eprintln!("skipped: permission checks do not apply to root");
            return;
        }
        let set = |path: &Path, mode| {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        };
        set(installation.payload.dir(), 0o555);
        assert_eq!(
            installation.payload.mode(),
            Mode::Notify(NotifyReason::PayloadNotWritable)
        );
        set(installation.payload.dir(), 0o755);
        set(&installation.parent, 0o555);
        assert_eq!(
            installation.payload.mode(),
            Mode::Notify(NotifyReason::ParentNotWritable)
        );
        // An install into an unwritable parent fails before anything moves.
        set(&installation.parent, 0o755);
        let archive = testing::release_archive("0.2.0", 2, TARGET);
        let (fetch, item) = served(&archive, 2, &testing::test_key());
        let staged = prepare(&installation, &fetch, &item).unwrap();
        set(&installation.parent, 0o555);
        let refused = installation
            .payload
            .install_with(&staged, None, SwapMethod::Renames)
            .unwrap_err();
        set(&installation.parent, 0o755);
        assert_eq!(refused.code(), "install-failed", "{refused}");
        assert_eq!(
            BuildInfo::read(installation.payload.dir()).unwrap().build,
            1
        );
    }
}

#[cfg(unix)]
fn running_as_root(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    path.metadata().unwrap().uid() == 0
}

#[test]
fn a_cancelled_download_stops_and_removes_its_staging() {
    let installation = Arc::new(installed("0.1.0", 1));
    let archive = testing::release_archive("0.2.0", 2, TARGET);
    // Large enough to stall part way through.
    let mut entries = testing::release_entries("0.2.0", 2, TARGET);
    entries.push(Entry::File(
        "Luxforge/lensfun/large.bin".into(),
        (0..200_000u32).flat_map(u32::to_le_bytes).collect(),
        0o644,
    ));
    let large = testing::archive(&entries);
    assert!(large.len() > 64 * 1024, "{} bytes", large.len());
    let (fetch, item) = served(&large, 2, &testing::test_key());
    let stalled = fetch.stall(ARCHIVE_URL, large.clone(), 32 * 1024);
    let cancel = Cancel::new();
    let worker = {
        let (installation, fetch, cancel) = (installation.clone(), fetch.clone(), cancel.clone());
        std::thread::spawn(move || {
            let key = testing::test_key().public_key();
            installation
                .payload
                .prepare(&*fetch, &item, &key, &cancel, &mut |_, _| {})
        })
    };
    stalled
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the download stalls");
    assert_eq!(
        names(&installation.parent),
        vec![".luxforge-update-2", "Luxforge"]
    );
    cancel.cancel();
    assert_eq!(worker.join().unwrap().unwrap_err().code(), "cancelled");
    assert_eq!(names(&installation.parent), vec!["Luxforge"]);
    // A request already cancelled writes nothing.
    let (fetch, item) = served(&archive, 2, &testing::test_key());
    let cancelled = Cancel::new();
    cancelled.cancel();
    let refused = installation
        .payload
        .prepare(
            &*fetch,
            &item,
            &testing::test_key().public_key(),
            &cancelled,
            &mut |_, _| {},
        )
        .unwrap_err();
    assert_eq!(refused.code(), "cancelled");
    assert_eq!(names(&installation.parent), vec!["Luxforge"]);
}

#[test]
fn a_cancel_runs_each_hook_once_whenever_it_was_registered() {
    let cancel = Cancel::new();
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = runs.clone();
    cancel.on_cancel(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    cancel.cancel();
    cancel.cancel();
    let counted = runs.clone();
    cancel.on_cancel(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(cancel.check().is_err());
}

#[test]
fn a_desktop_entry_pointing_at_the_payload_is_refreshed_and_others_are_left() {
    let installation = installed("0.1.0", 1);
    let payload = &installation.payload;
    let applications = installation.parent.join("applications");
    fs::create_dir(&applications).unwrap();
    let entry = applications.join(DESKTOP_ENTRY_FILE);
    let build = BuildInfo {
        version: "0.2.0".into(),
        build: 2,
        target: TARGET,
    };
    let executable = payload.executable();
    let ours = format!(
        "# Written by the install helper\n[Desktop Entry]\nType=Application\nName=Luxforge\n\
         Exec={} %F\nTryExec={}\nIcon={}/icon.png\nX-Luxforge-Version=0.1.0\n\n\
         [Desktop Action New]\nExec=other\n",
        executable.display(),
        executable.display(),
        payload.dir().display()
    );
    fs::write(&entry, &ours).unwrap();
    assert!(refresh_desktop_entry(&entry, payload.dir(), &executable, &build).unwrap());
    assert_eq!(
        fs::read_to_string(&entry).unwrap(),
        format!(
            "# Written by the install helper\n[Desktop Entry]\nType=Application\nName=Luxforge\n\
             Exec={} %F\nTryExec={}\nIcon={}/icon.png\nX-Luxforge-Version=0.2.0\n\
             X-Luxforge-Build=2\n\n[Desktop Action New]\nExec=other\n",
            executable.display(),
            executable.display(),
            payload.dir().display()
        )
    );
    // A quoted program with arguments.
    let spaced = installation.parent.join("my apps");
    let quoted_exe = testing::install_payload(&spaced.join("Luxforge"), "0.1.0", 1, TARGET);
    let quoted = Payload::locate(&quoted_exe).unwrap();
    fs::write(
        &entry,
        format!(
            "[Desktop Entry]\nExec=\"{}\" --open %F\n",
            quoted.executable().display()
        ),
    )
    .unwrap();
    assert!(refresh_desktop_entry(&entry, quoted.dir(), &quoted.executable(), &build).unwrap());
    assert_eq!(
        fs::read_to_string(&entry).unwrap(),
        format!(
            "[Desktop Entry]\nExec=\"{}\" --open %F\nX-Luxforge-Version=0.2.0\nX-Luxforge-Build=2\n",
            quoted.executable().display()
        )
    );
    // An entry for another installation, or none at all, is left alone.
    let elsewhere = "[Desktop Entry]\nExec=/opt/luxforge/luxforge %F\n";
    fs::write(&entry, elsewhere).unwrap();
    assert!(!refresh_desktop_entry(&entry, payload.dir(), &executable, &build).unwrap());
    assert_eq!(fs::read_to_string(&entry).unwrap(), elsewhere);
    fs::remove_file(&entry).unwrap();
    assert!(!refresh_desktop_entry(&entry, payload.dir(), &executable, &build).unwrap());
    assert!(!entry.exists());
    // An install refreshes the entry after the swap.
    fs::write(&entry, &ours).unwrap();
    let archive = testing::release_archive("0.2.0", 2, TARGET);
    let (fetch, item) = served(&archive, 2, &testing::test_key());
    let staged = prepare(&installation, &fetch, &item).unwrap();
    let installed = payload.install(&staged, Some(&entry)).unwrap();
    assert_eq!(installed.desktop_entry, Ok(true));
    assert!(
        fs::read_to_string(&entry)
            .unwrap()
            .contains("X-Luxforge-Build=2\n")
    );
}

#[test]
fn the_user_desktop_entry_follows_xdg_data_home() {
    let entry = |xdg: Option<&str>, home: Option<&str>| {
        user_desktop_entry(xdg.map(OsStr::new), home.map(OsStr::new))
    };
    use std::ffi::OsStr;
    assert_eq!(
        entry(Some("/data"), Some("/home/a")),
        Some(PathBuf::from("/data/applications/luxforge.desktop"))
    );
    assert_eq!(
        entry(Some("relative"), Some("/home/a")),
        Some(PathBuf::from(
            "/home/a/.local/share/applications/luxforge.desktop"
        ))
    );
    assert_eq!(entry(None, Some("")), None);
    assert_eq!(entry(None, None), None);
}
