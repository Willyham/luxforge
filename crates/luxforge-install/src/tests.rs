//! Classification over path and volume fixtures, a real read-only disk image, and the move and
//! relaunch into temporary folders. Nothing here touches the real Applications folders or the
//! Trash: every move names its own roots, and the Trash is a folder the test owns.
use super::*;
use luxforge_test_macros::slow;
use std::{
    cell::RefCell,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A new, empty folder under the worktree's `target/`, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = luxforge_testbase::paths::repository()
            .join("target")
            .join("move-to-applications-tests")
            .join(format!(
                "{}-{}-{name}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        // Resolved, so paths compare with what the system reports.
        Self(fs::canonicalize(&path).unwrap())
    }

    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = Command::new("/bin/chmod")
            .arg("-R")
            .arg("u+w")
            .arg(&self.0)
            .status();
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A small `.app` bundle at `dir/name`, signed ad hoc as a release is signed, with a real Mach-O
/// executable and one resource.
fn fake_app(dir: &Path, name: &str, marker: &str) -> PathBuf {
    let app = dir.join(name);
    let contents = app.join("Contents");
    fs::create_dir_all(contents.join("MacOS")).unwrap();
    fs::create_dir_all(contents.join("Resources")).unwrap();
    fs::copy("/usr/bin/true", contents.join("MacOS/luxforge")).unwrap();
    fs::write(
        contents.join("Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>luxforge</string>
<key>CFBundleIdentifier</key><string>app.luxforge.move-test</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleName</key><string>Luxforge</string>
</dict></plist>
"#,
    )
    .unwrap();
    fs::write(contents.join("Resources/marker.txt"), marker).unwrap();
    let signed = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(&app)
        .output()
        .unwrap();
    assert!(
        signed.status.success(),
        "codesign: {}",
        String::from_utf8_lossy(&signed.stderr)
    );
    app
}

fn marker(app: &Path) -> String {
    fs::read_to_string(app.join("Contents/Resources/marker.txt")).unwrap()
}

fn verifies(app: &Path) -> bool {
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app))
    .is_ok()
}

/// Every path under `root`, `root` included.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![root.to_path_buf()];
    if root.symlink_metadata().unwrap().is_dir() {
        for entry in fs::read_dir(root).unwrap() {
            paths.extend(walk(&entry.unwrap().path()));
        }
    }
    paths
}

/// Probes answering from fixtures.
#[derive(Default)]
struct Stub {
    translocated: Option<Option<PathBuf>>,
    /// Paths under each prefix are on the volume given; any other path is on a writable one.
    volumes: Vec<(PathBuf, Volume)>,
}

impl Probes for Stub {
    fn translocated(&self, _: &Path) -> Option<Option<PathBuf>> {
        self.translocated.clone()
    }

    fn volume(&self, path: &Path) -> io::Result<Volume> {
        Ok(self
            .volumes
            .iter()
            .find(|(prefix, _)| path.starts_with(prefix))
            .map(|(_, volume)| volume.clone())
            .unwrap_or_else(|| writable_volume("/")))
    }
}

fn writable_volume(mount: &str) -> Volume {
    Volume {
        read_only: false,
        device: "/dev/disk3s1".into(),
        mount_point: mount.into(),
        disk_image: false,
    }
}

fn read_only(mount: &str, disk_image: bool) -> Volume {
    Volume {
        read_only: true,
        device: "/dev/disk9s1".into(),
        mount_point: mount.into(),
        disk_image,
    }
}

fn roots() -> Roots {
    Roots {
        system: "/Applications".into(),
        user: "/Users/someone/Applications".into(),
    }
}

#[test]
fn move_to_applications_finds_the_bundle_an_executable_belongs_to() {
    assert_eq!(
        bundle_of(Path::new(
            "/Applications/Luxforge.app/Contents/MacOS/luxforge"
        )),
        Some(PathBuf::from("/Applications/Luxforge.app"))
    );
    for outside in [
        "/Users/someone/luxforge/target/release/luxforge",
        "/Applications/Luxforge.app/Contents/Resources/luxforge",
        "/Applications/Luxforge/Contents/MacOS/luxforge",
        "luxforge",
    ] {
        assert_eq!(bundle_of(Path::new(outside)), None, "{outside}");
    }
}

#[test]
fn move_to_applications_classifies_paths_and_volumes() {
    let stub = Stub {
        volumes: vec![
            (
                "/Volumes/Luxforge".into(),
                read_only("/Volumes/Luxforge", true),
            ),
            ("/Volumes/Disc".into(), read_only("/Volumes/Disc", false)),
        ],
        ..Stub::default()
    };
    let at = |path: &str| classify_with(Path::new(path), &roots(), &stub);
    for (path, location, cleanup) in [
        (
            "/Applications/Luxforge.app",
            Location::Applications,
            Cleanup::Leave,
        ),
        (
            "/Applications/Photo/Luxforge.app",
            Location::Applications,
            Cleanup::Leave,
        ),
        (
            "/Users/someone/Applications/Luxforge.app",
            Location::UserApplications,
            Cleanup::Leave,
        ),
        (
            "/Users/someone/Downloads/Luxforge.app",
            Location::Elsewhere,
            Cleanup::Trash("/Users/someone/Downloads/Luxforge.app".into()),
        ),
        // A folder merely named like Applications is elsewhere.
        (
            "/Applications Old/Luxforge.app",
            Location::Elsewhere,
            Cleanup::Trash("/Applications Old/Luxforge.app".into()),
        ),
        (
            "/Volumes/Luxforge/Luxforge.app",
            Location::DiskImage,
            Cleanup::Eject("/dev/disk9s1".into()),
        ),
        (
            "/Volumes/Disc/Luxforge.app",
            Location::ReadOnlyVolume,
            Cleanup::Leave,
        ),
    ] {
        let placement = at(path);
        assert_eq!(
            (
                placement.location,
                &placement.cleanup,
                placement.original.as_path()
            ),
            (location, &cleanup, Path::new(path)),
            "{path}"
        );
        assert_eq!(
            location.in_applications(),
            matches!(
                location,
                Location::Applications | Location::UserApplications
            )
        );
    }
}

#[test]
fn move_to_applications_classifies_a_translocated_app_by_its_original() {
    let mirror = "/private/var/folders/xy/T/AppTranslocation/1234/d/Luxforge.app";
    let cases = [
        // Opened from Downloads: the original goes to the Trash.
        (
            Some(PathBuf::from("/Users/someone/Downloads/Luxforge.app")),
            "/Users/someone/Downloads/Luxforge.app",
            Cleanup::Trash("/Users/someone/Downloads/Luxforge.app".into()),
        ),
        // Opened from the quarantined disk image: the image is ejected.
        (
            Some(PathBuf::from("/Volumes/Luxforge/Luxforge.app")),
            "/Volumes/Luxforge/Luxforge.app",
            Cleanup::Eject("/dev/disk9s1".into()),
        ),
        // The system did not say where it was put: it is left alone.
        (None, mirror, Cleanup::Leave),
    ];
    for (original, expected, cleanup) in cases {
        let stub = Stub {
            translocated: Some(original),
            volumes: vec![
                (
                    "/private/var/folders".into(),
                    read_only("/private/var/folders/xy/T/AppTranslocation/1234", false),
                ),
                (
                    "/Volumes/Luxforge".into(),
                    read_only("/Volumes/Luxforge", true),
                ),
            ],
        };
        let placement = classify_with(Path::new(mirror), &roots(), &stub);
        assert_eq!(placement.location, Location::Translocated);
        assert_eq!(placement.bundle, Path::new(mirror));
        assert_eq!(placement.original, Path::new(expected));
        assert_eq!(placement.cleanup, cleanup);
    }
}

#[test]
fn move_to_applications_reads_the_real_system_for_an_ordinary_folder() {
    let scratch = Scratch::new("ordinary");
    let app = fake_app(&scratch.0, "Luxforge.app", "a");
    // Security's translocation functions answer for a path that is not translocated, and the
    // folder's volume is the writable one the worktree is on.
    assert_eq!(System.translocated(&app), None);
    let volume = System.volume(&app).unwrap();
    assert!(!volume.read_only && !volume.disk_image, "{volume:?}");
    let placement = classify(&app);
    assert_eq!(placement.location, Location::Elsewhere);
    assert_eq!(placement.cleanup, Cleanup::Trash(app.clone()));
}

/// An attached read-only disk image, detached when dropped.
struct Attached {
    mount: PathBuf,
}

impl Drop for Attached {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/hdiutil")
            .args(["detach", "-force", "-quiet"])
            .arg(&self.mount)
            .status();
    }
}

/// Run a disk-image command, retrying within the shared hang bound: Disk Arbitration can refuse
/// one while it is busy, as a hosted runner's can. A persistent failure names its last answer.
fn hdiutil(command: &mut Command) {
    let mut last = None;
    let result = luxforge_testbase::try_wait_for("the disk-image command to succeed", || {
        let answer = run(command);
        let succeeded = answer.is_ok();
        last = Some(answer);
        succeeded.then_some(())
    });
    if let Err(hung) = result {
        panic!(
            "{hung}\n{command:?}: {:?}",
            last.expect("the command was attempted")
        );
    }
}

#[test]
#[slow]
fn move_to_applications_classifies_a_real_read_only_disk_image() {
    let scratch = Scratch::new("disk-image");
    let source = scratch.join("source");
    fs::create_dir_all(&source).unwrap();
    fake_app(&source, "Luxforge.app", "image");
    let image = scratch.join("Luxforge.dmg");
    hdiutil(
        Command::new("/usr/bin/hdiutil")
            .args([
                "create",
                "-quiet",
                "-ov",
                "-fs",
                "HFS+",
                "-format",
                "UDZO",
                "-volname",
                "LuxforgeTest",
                "-srcfolder",
            ])
            .arg(&source)
            .arg(&image),
    );
    let mount = scratch.join("mounted");
    fs::create_dir_all(&mount).unwrap();
    hdiutil(
        Command::new("/usr/bin/hdiutil")
            .args([
                "attach",
                "-quiet",
                "-readonly",
                "-nobrowse",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(&mount)
            .arg(&image),
    );
    let attached = Attached {
        mount: mount.clone(),
    };
    let app = mount.join("Luxforge.app");
    let volume = System.volume(&app).unwrap();
    assert!(volume.read_only, "{volume:?}");
    assert!(volume.disk_image, "{volume:?}");
    assert!(volume.device.starts_with("/dev/disk"), "{volume:?}");
    let placement = classify(&app);
    assert_eq!(placement.location, Location::DiskImage);
    assert_eq!(placement.cleanup, Cleanup::Eject(volume.device.clone()));
    // A move from it copies into the destination and leaves the image as it was.
    let applications = scratch.join("Applications");
    fs::create_dir_all(&applications).unwrap();
    let env = Env::new(&scratch, &applications, true);
    let roots = Roots {
        system: applications.clone(),
        user: scratch.join("home/Applications"),
    };
    let moved = move_to_applications(&placement, &roots, false, &env).unwrap();
    assert_eq!(marker(&moved.app), "image");
    assert!(verifies(&moved.app));
    assert_eq!(moved.cleanup, Cleanup::Eject(volume.device));
    assert!(app.exists());
    drop(attached);
}

/// A move's effects outside its destination: `/Applications`' writability as a test says, and a
/// Trash that is a folder the test owns.
struct Env {
    system: PathBuf,
    system_writable: bool,
    trash: PathBuf,
    trashed: RefCell<Vec<PathBuf>>,
}

impl Env {
    fn new(scratch: &Scratch, system: &Path, system_writable: bool) -> Self {
        let trash = scratch.join("Trash");
        fs::create_dir_all(&trash).unwrap();
        Self {
            system: system.to_path_buf(),
            system_writable,
            trash,
            trashed: RefCell::new(Vec::new()),
        }
    }
}

impl MoveEnv for Env {
    fn writable(&self, folder: &Path) -> bool {
        if folder == self.system {
            self.system_writable
        } else {
            System.writable(folder)
        }
    }

    fn trash(&self, path: &Path) -> io::Result<()> {
        let count = self.trashed.borrow().len();
        fs::rename(
            path,
            self.trash.join(format!(
                "{count}-{}",
                path.file_name().unwrap().to_string_lossy()
            )),
        )?;
        self.trashed.borrow_mut().push(path.to_path_buf());
        Ok(())
    }
}

/// A downloaded app in `Downloads`, quarantined throughout, with roots under the scratch folder.
struct Download {
    scratch: Scratch,
    app: PathBuf,
    roots: Roots,
}

impl Download {
    fn new(name: &str) -> Self {
        let scratch = Scratch::new(name);
        let downloads = scratch.join("Downloads");
        fs::create_dir_all(&downloads).unwrap();
        let app = fake_app(&downloads, "Luxforge.app", "new");
        for path in walk(&app) {
            native::quarantine(&path).unwrap();
        }
        let roots = Roots {
            system: scratch.join("Applications"),
            user: scratch.join("home/Applications"),
        };
        fs::create_dir_all(&roots.system).unwrap();
        Self {
            scratch,
            app,
            roots,
        }
    }

    fn placement(&self) -> Placement {
        classify_with(&self.app, &self.roots, &System)
    }

    /// Nothing but the destination folder's own entries, by name.
    fn entries(&self, folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

#[test]
fn move_to_applications_copies_verifies_and_clears_quarantine() {
    let download = Download::new("move");
    let placement = download.placement();
    assert_eq!(placement.location, Location::Elsewhere);
    let env = Env::new(&download.scratch, &download.roots.system, true);
    let moved = move_to_applications(&placement, &download.roots, false, &env).unwrap();
    assert_eq!(moved.app, download.roots.system.join("Luxforge.app"));
    assert_eq!((moved.replaced.as_ref(), moved.user), (None, false));
    assert_eq!(moved.cleanup, Cleanup::Trash(download.app.clone()));
    assert!(verifies(&moved.app), "the copy keeps its signature");
    assert_eq!(marker(&moved.app), "new");
    // The copy's quarantine is cleared everywhere; the original keeps its own until it goes.
    assert_eq!(moved.quarantine_cleared, walk(&download.app).len());
    assert!(walk(&moved.app).iter().all(|path| !quarantined(path)));
    assert!(walk(&download.app).iter().all(|path| quarantined(path)));
    // Nothing was trashed, and no hidden copy is left beside the app.
    assert!(env.trashed.borrow().is_empty());
    assert_eq!(download.entries(&download.roots.system), ["Luxforge.app"]);
    // Already in Applications, it does not move again.
    let there = classify_with(&moved.app, &download.roots, &System);
    assert_eq!(there.location, Location::Applications);
    assert_eq!(
        move_to_applications(&there, &download.roots, true, &env),
        Err(MoveError::InApplications)
    );
}

#[test]
fn move_to_applications_replaces_an_existing_app_only_when_confirmed() {
    let download = Download::new("existing");
    let existing = fake_app(&download.roots.system, "Luxforge.app", "old");
    let placement = download.placement();
    let env = Env::new(&download.scratch, &download.roots.system, true);
    let refused = move_to_applications(&placement, &download.roots, false, &env).unwrap_err();
    assert_eq!(refused, MoveError::Existing(existing.clone()));
    assert_eq!(refused.reason(), "existing-app");
    // Unconfirmed, nothing changed: the old app, no copy, nothing in the Trash.
    assert_eq!(marker(&existing), "old");
    assert_eq!(download.entries(&download.roots.system), ["Luxforge.app"]);
    assert!(env.trashed.borrow().is_empty());
    let moved = move_to_applications(&placement, &download.roots, true, &env).unwrap();
    assert_eq!(moved.replaced.as_deref(), Some(existing.as_path()));
    assert_eq!(*env.trashed.borrow(), std::slice::from_ref(&existing));
    assert_eq!(marker(&moved.app), "new");
    assert_eq!(marker(&env.trash.join("0-Luxforge.app")), "old");
    assert_eq!(download.entries(&download.roots.system), ["Luxforge.app"]);
}

#[test]
fn move_to_applications_falls_back_to_the_home_applications_folder() {
    let download = Download::new("fallback");
    // A real unwritable folder reads as unwritable.
    let locked = download.scratch.join("locked");
    fs::create_dir_all(&locked).unwrap();
    run(Command::new("/bin/chmod").arg("0555").arg(&locked)).unwrap();
    assert!(!System.writable(&locked));
    assert!(System.writable(&download.roots.system));
    let roots = Roots {
        system: locked.clone(),
        user: download.roots.user.clone(),
    };
    assert!(!roots.user.exists());
    let placement = classify_with(&download.app, &roots, &System);
    let moved = move_to_applications(&placement, &roots, false, &System).unwrap();
    assert!(moved.user);
    assert_eq!(moved.app, roots.user.join("Luxforge.app"));
    assert!(verifies(&moved.app));
    assert!(fs::read_dir(&locked).unwrap().next().is_none());
}

#[test]
fn move_to_applications_leaves_everything_when_the_copy_does_not_verify() {
    let download = Download::new("unverified");
    let existing = fake_app(&download.roots.system, "Luxforge.app", "old");
    // A resource changed after signing: the copy carries the broken seal.
    fs::write(
        download.app.join("Contents/Resources/marker.txt"),
        "tampered",
    )
    .unwrap();
    let before: Vec<_> = walk(&download.app)
        .into_iter()
        .map(|path| (fs::read(&path).ok(), quarantined(&path), path))
        .collect();
    let placement = download.placement();
    let env = Env::new(&download.scratch, &download.roots.system, true);
    let refused = move_to_applications(&placement, &download.roots, true, &env).unwrap_err();
    assert!(matches!(refused, MoveError::Verify(_)), "{refused:?}");
    assert_eq!(refused.reason(), "verify-failed");
    // The original is untouched, the app in the way stays, nothing went to the Trash, and the
    // hidden copy is gone.
    let after: Vec<_> = walk(&download.app)
        .into_iter()
        .map(|path| (fs::read(&path).ok(), quarantined(&path), path))
        .collect();
    assert_eq!(before, after);
    assert_eq!(marker(&existing), "old");
    assert!(env.trashed.borrow().is_empty());
    assert_eq!(download.entries(&download.roots.system), ["Luxforge.app"]);
}

/// A stand-in tool that appends one line a run to `record`: whether process `pid` still existed as
/// it ran, then its arguments.
fn recorder(dir: &Path, name: &str, record: &Path, pid: u32) -> PathBuf {
    let tool = dir.join(name);
    let script = format!(
        "#!/bin/sh\n\
         if /bin/kill -0 {pid} 2>/dev/null; then state=running; else state=exited; fi\n\
         echo \"$state $*\" >> '{}'\n",
        record.display()
    );
    fs::write(&tool, script).unwrap();
    run(Command::new("/bin/chmod").arg("0755").arg(&tool)).unwrap();
    tool
}

/// The lines `path` holds, once it holds `lines` of them.
fn recorded(path: &Path, lines: usize) -> Vec<String> {
    luxforge_testbase::wait_for(&format!("{lines} lines in {}", path.display()), || {
        let text = fs::read_to_string(path).unwrap_or_default();
        let found: Vec<String> = text.lines().map(str::to_owned).collect();
        (found.len() >= lines).then_some(found)
    })
}

#[test]
#[slow]
fn move_to_applications_relaunches_after_the_process_exits_then_cleans_up() {
    let scratch = Scratch::new("relaunch");
    let env = Env::new(&scratch, &scratch.join("Applications"), true);
    let original = fake_app(&scratch.0, "Original.app", "original");
    let app = scratch.join("Applications/Luxforge.app");
    let opened = scratch.join("opened.txt");
    let detached = scratch.join("detached.txt");

    // The waiter opens the app only once the process it waits for has exited: the stand-in opener
    // records whether that process still existed when it ran. The killed child is waited for
    // here, so it is gone rather than a zombie by then.
    let mut running = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let tools = Tools {
        open: recorder(&scratch.0, "open", &opened, running.id()),
        hdiutil: recorder(&scratch.0, "hdiutil", &detached, running.id()),
    };
    let started = Relaunch {
        app: app.clone(),
        cleanup: Cleanup::Trash(original.clone()),
    }
    .start_with(&env, &tools, running.id(), 1200)
    .unwrap();
    assert_eq!(started.trashed.as_deref(), Some(original.as_path()));
    assert_eq!(*env.trashed.borrow(), std::slice::from_ref(&original));
    running.kill().unwrap();
    running.wait().unwrap();
    assert_eq!(
        recorded(&opened, 1),
        [format!("exited -n -a {}", app.display())]
    );
    assert!(!detached.exists(), "a trashed original ejects nothing");

    // A disk image's device is detached after the app opens, and its app is not trashed.
    let mut running = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let tools = Tools {
        open: recorder(&scratch.0, "open-2", &opened, running.id()),
        hdiutil: recorder(&scratch.0, "hdiutil-2", &detached, running.id()),
    };
    let started = Relaunch {
        app: app.clone(),
        cleanup: Cleanup::Eject("/dev/disk9s1".into()),
    }
    .start_with(&env, &tools, running.id(), 1200)
    .unwrap();
    assert_eq!(started.eject.as_deref(), Some("/dev/disk9s1"));
    assert_eq!(env.trashed.borrow().len(), 1);
    running.kill().unwrap();
    running.wait().unwrap();
    assert_eq!(
        recorded(&opened, 2)[1],
        format!("exited -n -a {}", app.display())
    );
    assert_eq!(
        recorded(&detached, 1),
        ["exited detach /dev/disk9s1 -quiet"]
    );
}
