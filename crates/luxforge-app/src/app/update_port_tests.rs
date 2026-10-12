//! The update port: which launch gets which mechanism, and the Linux payload port driven through the
//! catalog owner's `update.*` commands against a payload in a temporary directory, an in-memory
//! feed and a test key: check, find, install requested, the quit request and the quit hook's
//! install and relaunch; an automatic install at quit; notify mode; the schedule; a failed
//! download; the cleanup after launch; and a quit that cancels a download.
use super::*;
use luxforge_core::{
    ApiRequest, ClientAuthority, ClientId, HostConfig, ModuleRegistry, OwnerHandle,
    update::{CATALOG_FORMAT, Mechanism},
};
use luxforge_testbase::wait_until;
use luxforge_update::{
    SigningKey, Target,
    payload::{
        BuildInfo, PACKAGE_MARKER,
        testing::{self, FEED_BASE, MemoryFetch},
    },
};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};

const TARGET: Target = Target::LinuxX86_64;
const ARCHIVE: &str = "https://github.com/Willyham/luxforge/releases/download/v0.2.0/luxforge-0.2.0-linux-x86_64.tar.gz";
/// Long enough that no automatic check runs during a test that does not ask for one.
const NEVER: Duration = Duration::from_secs(3600);

fn feed_url() -> String {
    format!("{FEED_BASE}{}", TARGET.feed_path())
}

/// A payload at build 1 under a scratch directory, and a feed offering build 2.
struct Fixture {
    dir: PathBuf,
    payload: Payload,
    fetch: Arc<MemoryFetch>,
    archive: Vec<u8>,
    quits: Arc<AtomicUsize>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = luxforge_testbase::paths::temp_dir(&format!("update-port-{name}"))
            .canonicalize()
            .unwrap();
        let executable = testing::install_payload(&dir.join("apps/Luxforge"), "0.1.0", 1, TARGET);
        let fixture = Self {
            dir,
            payload: Payload::locate(&executable).unwrap(),
            fetch: MemoryFetch::new(),
            archive: testing::release_archive("0.2.0", 2, TARGET),
            quits: Arc::new(AtomicUsize::new(0)),
        };
        fixture.offer(&testing::test_key());
        fixture
    }

    /// Serve the archive and a feed offering it, signed by `key`.
    fn offer(&self, key: &SigningKey) {
        let item = testing::item(
            "0.2.0",
            2,
            ARCHIVE,
            &self.archive,
            key,
            None,
            Some(CATALOG_FORMAT as u32),
        );
        self.fetch.serve(&feed_url(), testing::feed(vec![item]));
        self.fetch.serve(ARCHIVE, self.archive.clone());
    }

    fn port(&self, first_check: Duration) -> Arc<LinuxPayloadPort> {
        let quits = self.quits.clone();
        LinuxPayloadPort::start(
            self.payload.clone(),
            PortSettings {
                feed_base_url: FEED_BASE.into(),
                key: testing::test_key().public_key(),
                fetch: self.fetch.clone(),
                quit: Arc::new(move || {
                    quits.fetch_add(1, Ordering::SeqCst);
                }),
                desktop_entry: None,
                arguments: vec!["--open".into(), "a.jpg".into()],
                first_check,
                interval: CHECK_INTERVAL,
            },
        )
    }

    fn apps(&self) -> PathBuf {
        self.dir.join("apps")
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.apps())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn installed_build(&self) -> u64 {
        BuildInfo::read(self.payload.dir()).unwrap().build
    }

    fn feed_requests(&self) -> usize {
        self.fetch
            .requests()
            .iter()
            .filter(|url| **url == feed_url())
            .count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A catalog owner whose host has the port, with the desktop's client.
struct Owner {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    desktop: ClientId,
}

impl Owner {
    fn start(fixture: &Fixture, port: Arc<LinuxPayloadPort>, preferences: Option<Value>) -> Self {
        let config = fixture.dir.join("config");
        if let Some(preferences) = preferences {
            std::fs::create_dir_all(&config).unwrap();
            std::fs::write(config.join("preferences.json"), preferences.to_string()).unwrap();
        }
        let (owner, join) = OwnerHandle::start_with_host(
            &fixture.dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                preferences_dir: Some(config),
                updates: Some(port),
                ..HostConfig::unconfigured()
            },
        )
        .unwrap();
        let desktop = owner.register_with(ClientAuthority::Permissions);
        let owner = Self {
            owner,
            join: Some(join),
            desktop,
        };
        // Answered once the owner has started, attached the port and applied the policy.
        owner.status();
        owner
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, (String, Value)> {
        let response = self
            .owner
            .call(
                self.desktop,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered");
        match response.error {
            None => Ok(response.result.expect("a result")),
            Some(error) => Err((error.code, error.data.unwrap_or(Value::Null))),
        }
    }

    fn status(&self) -> Value {
        self.call("update.status", json!({})).unwrap()
    }

    fn install(&self) -> Result<Value, (String, Value)> {
        self.call(
            "update.install",
            json!({"mutation": {"request_id": "install", "actor": "test"}}),
        )
    }

    /// Stop the owner and wait for its thread, as the quit path does before installing.
    fn stop(mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// `release/update.json` before its public key is committed.
const UNKEYED: &str =
    r#"{"feed_base_url": "https://api.luxforge.app/v1/updates/", "public_key": null}"#;

/// A macOS bundle as `release-build` makes it, with the update key.
const RELEASED: Bundle = Bundle {
    framework: true,
    key: true,
    journey: false,
    outside: false,
};

#[test]
fn only_a_release_launched_by_a_person_on_linux_with_a_key_gets_the_payload_port() {
    let key = testing::test_key().public_key();
    let keyed = format!(
        r#"{{"feed_base_url": "https://api.luxforge.app/v1/updates/", "public_key": "{}"}}"#,
        key.to_base64()
    );
    let payload = Choice::Payload {
        feed_base_url: "https://api.luxforge.app/v1/updates/".into(),
        key,
        classes: &[EndpointClass::Remote],
    };
    assert_eq!(choose(true, false, Platform::Linux, &keyed, None), payload);
    let development = UnavailableReason::DevelopmentBuild;
    for (release, automated, platform) in [
        (true, true, Platform::Linux),
        (false, false, Platform::Linux),
        (false, true, Platform::Linux),
        (false, false, Platform::Other),
        (true, false, Platform::Other),
        (false, false, Platform::Macos(RELEASED)),
        (true, true, Platform::Macos(RELEASED)),
    ] {
        assert_eq!(
            choose(release, automated, platform, &keyed, None),
            Choice::Unavailable(development),
            "release {release}, automated {automated}, {platform:?}"
        );
    }
    // A configuration that commits no key reports it on every platform.
    for platform in [Platform::Linux, Platform::Macos(RELEASED), Platform::Other] {
        assert_eq!(
            choose(true, false, platform, UNKEYED, None),
            Choice::Unavailable(UnavailableReason::NoUpdateKey),
            "{platform:?}"
        );
    }
    // The checked-in configuration commits the update key, so a Linux release gets the payload
    // port on the production feed.
    let embedded = UpdateConfig::parse(RELEASE_UPDATE_CONFIG).unwrap();
    assert_eq!(
        embedded.feed_base_url,
        "https://api.luxforge.app/v1/updates/"
    );
    assert!(matches!(
        choose(true, false, Platform::Linux, RELEASE_UPDATE_CONFIG, None),
        Choice::Payload {
            classes: &[EndpointClass::Remote],
            ..
        }
    ));
}

#[test]
fn only_a_release_bundle_with_sparkle_and_a_key_in_place_gets_the_sparkle_port() {
    let key = testing::test_key().public_key();
    let keyed = format!(
        r#"{{"feed_base_url": "https://api.luxforge.app/v1/updates/", "public_key": "{}"}}"#,
        key.to_base64()
    );
    let macos = |bundle| choose(true, false, Platform::Macos(bundle), &keyed, None);
    assert_eq!(macos(RELEASED), Choice::Sparkle);
    // A release binary outside `release-build`'s bundle, such as `cargo run` with the version.
    assert_eq!(
        macos(Bundle {
            framework: false,
            ..RELEASED
        }),
        Choice::Unavailable(UnavailableReason::DevelopmentBuild)
    );
    assert_eq!(
        macos(Bundle {
            key: false,
            ..RELEASED
        }),
        Choice::Unavailable(UnavailableReason::NoUpdateKey)
    );
    let outside = Bundle {
        outside: true,
        ..RELEASED
    };
    assert_eq!(
        macos(outside),
        Choice::Unavailable(UnavailableReason::OutsideApplications)
    );
    // The update journey's bundles carry a test key and run hidden, as automated launches do;
    // they alone take Sparkle while automated, and only from a release bundle with both.
    let journey = Bundle {
        journey: true,
        ..RELEASED
    };
    assert_eq!(
        choose(
            true,
            true,
            Platform::Macos(journey),
            RELEASE_UPDATE_CONFIG,
            None
        ),
        Choice::Sparkle
    );
    for bundle in [
        Bundle {
            framework: false,
            ..journey
        },
        Bundle {
            key: false,
            ..journey
        },
    ] {
        assert_ne!(
            choose(true, true, Platform::Macos(bundle), &keyed, None),
            Choice::Sparkle
        );
    }
    assert_eq!(
        choose(false, true, Platform::Macos(journey), &keyed, None),
        Choice::Unavailable(UnavailableReason::DevelopmentBuild)
    );
}

#[test]
fn a_development_feed_is_honoured_only_in_a_development_build_on_linux() {
    let key = testing::test_key().public_key();
    let dev = |feed: &str, key: &str| dev_feed(Some(feed.into()), Some(key.into()));
    let local = dev("http://127.0.0.1:8123/updates/", &key.to_base64()).unwrap();
    assert_eq!(
        choose(
            false,
            false,
            Platform::Linux,
            RELEASE_UPDATE_CONFIG,
            Some(local.clone())
        ),
        Choice::Payload {
            feed_base_url: "http://127.0.0.1:8123/updates/".into(),
            key,
            classes: &[EndpointClass::Remote, EndpointClass::Loopback],
        }
    );
    // Never in a release build, an automated launch or off Linux.
    for (release, automated, platform) in [
        (true, false, Platform::Linux),
        (false, true, Platform::Linux),
        (false, false, Platform::Other),
        (false, false, Platform::Macos(RELEASED)),
    ] {
        assert!(
            !matches!(
                choose(release, automated, platform, UNKEYED, Some(local.clone())),
                Choice::Payload { .. }
            ),
            "release {release}, automated {automated}, {platform:?}"
        );
    }
    for (feed, key) in [
        ("http://127.0.0.1:8123/updates", key.to_base64()),
        ("file:///tmp/updates/", key.to_base64()),
        ("http://127.0.0.1:8123/updates/?a", key.to_base64()),
        ("http://127.0.0.1:8123/updates/", "not a key".into()),
    ] {
        assert_eq!(dev(feed, &key), None, "{feed}");
    }
    assert_eq!(dev_feed(None, Some(key.to_base64().into())), None);
}

#[test]
fn development_and_automated_launches_never_check() {
    let transport: Arc<dyn Transport> = Arc::new(luxforge_net::HttpTransport::system());
    for config in [
        Config::default(),
        Config {
            hidden: true,
            ..Config::default()
        },
        Config {
            evidence: Some(PathBuf::from("/nonexistent/evidence")),
            ..Config::default()
        },
    ] {
        let port = select(&config, transport.clone());
        assert_eq!(
            port.status().updater,
            Updater::Unavailable(UnavailableReason::DevelopmentBuild)
        );
    }
    assert!(INSTALLED.get().is_none());
}

#[test]
fn a_requested_install_downloads_then_quits_and_the_quit_hook_swaps_and_relaunches() {
    let fixture = Fixture::new("requested");
    let port = fixture.port(NEVER);
    let owner = Owner::start(&fixture, port.clone(), None);
    let status = owner.status();
    assert_eq!(status["mechanism"], "linux-payload");
    assert_eq!(status["current"]["build"], 1);
    // Nothing is checked or installed without being asked.
    assert_eq!(fixture.fetch.requests(), Vec::<String>::new());
    assert_eq!(owner.install().unwrap_err().1["reason"], "no-update");

    let checked = owner.call("update.check", json!({})).unwrap();
    assert_eq!(checked["last_check"]["outcome"], "found", "{checked}");
    let available = &checked["available"];
    assert_eq!(
        (&available["version"], &available["build"]),
        (&json!("0.2.0"), &json!(2))
    );
    assert_eq!(
        available["notes_url"],
        "https://github.com/Willyham/luxforge/releases/tag/v0.2.0"
    );
    assert_eq!(available["catalog_format_change"], false);
    assert_eq!(checked["ready"], false);
    assert_eq!(fixture.feed_requests(), 1);

    let requested = owner.install().unwrap();
    assert_eq!(requested["outcome"], "requested");
    wait_until("the quit request", || {
        fixture.quits.load(Ordering::SeqCst) == 1
    });
    let ready = owner.status();
    assert_eq!(
        (&ready["ready"], &ready["progress"]),
        (&json!(true), &Value::Null)
    );
    assert_eq!(fixture.names(), vec![".luxforge-update-2", "Luxforge"]);
    // Nothing is replaced until the catalog has closed.
    assert_eq!(fixture.installed_build(), 1);
    owner.stop();
    assert_eq!(
        port.at_quit(),
        AtQuit::Relaunch {
            executable: fixture.payload.executable(),
            arguments: vec!["--open".into(), "a.jpg".into()],
        }
    );
    assert_eq!(fixture.installed_build(), 2);
    assert_eq!(fixture.names(), vec!["Luxforge", "Luxforge.previous"]);
    assert_eq!(fixture.quits.load(Ordering::SeqCst), 1);
}

#[test]
fn an_automatic_update_downloads_in_the_background_and_installs_at_quit_without_relaunching() {
    let fixture = Fixture::new("automatic");
    let port = fixture.port(NEVER);
    let owner = Owner::start(
        &fixture,
        port.clone(),
        Some(json!({"format": 1, "update_install_automatically": true})),
    );
    let checked = owner.call("update.check", json!({})).unwrap();
    assert_eq!(
        checked["available"]["installs_automatically"], true,
        "{checked}"
    );
    wait_until("the background download", || {
        owner.status()["ready"] == true
    });
    // Downloaded, but nobody asked to quit.
    assert_eq!(fixture.quits.load(Ordering::SeqCst), 0);
    owner.stop();
    assert_eq!(port.at_quit(), AtQuit::Installed);
    assert_eq!(fixture.installed_build(), 2);
}

#[test]
fn a_package_managed_payload_offers_its_release_page_and_refuses_to_install() {
    let fixture = Fixture::new("notify");
    std::fs::write(fixture.payload.dir().join(PACKAGE_MARKER), b"").unwrap();
    assert_eq!(
        fixture.payload.mode(),
        Mode::Notify(luxforge_update::payload::NotifyReason::PackageManaged)
    );
    let port = fixture.port(NEVER);
    let owner = Owner::start(&fixture, port.clone(), None);
    assert_eq!(owner.status()["mechanism"], "linux-notify");
    let checked = owner.call("update.check", json!({})).unwrap();
    assert_eq!(
        checked["available"]["notes_url"],
        "https://github.com/Willyham/luxforge/releases/tag/v0.2.0"
    );
    let (code, data) = owner.install().unwrap_err();
    assert_eq!(
        (code.as_str(), &data["reason"]),
        ("not-ready", &json!("notify-only"))
    );
    // The port refuses on its own too, and never downloads.
    assert_eq!(
        port.request_install().unwrap_err().data.unwrap()["reason"],
        "notify-only"
    );
    owner.stop();
    assert_eq!(port.at_quit(), AtQuit::Nothing);
    assert_eq!(fixture.fetch.requests(), vec![feed_url()]);
    assert_eq!(fixture.installed_build(), 1);
}

#[test]
fn checks_are_due_shortly_after_launch_then_daily_and_only_while_they_are_on() {
    let fixture = Fixture::new("schedule");
    let port = fixture.port(Duration::from_secs(10));
    let shared = &port.shared;
    let launched = Instant::now();
    let mut state = State {
        policy: Some(UpdatePolicy::new(true, false, false, None)),
        ..State::default()
    };
    // Nothing is due before an owner attaches.
    assert_eq!(shared.due(&state), None);
    state.started = Some(launched);
    assert_eq!(shared.due(&state), Some(launched + Duration::from_secs(10)));
    let checked = launched + Duration::from_secs(30);
    state.last_finished = Some(checked);
    assert_eq!(shared.due(&state), Some(checked + CHECK_INTERVAL));
    state.policy = Some(UpdatePolicy::new(false, true, false, None));
    assert_eq!(shared.due(&state), None);
}

#[test]
fn a_launch_check_runs_without_being_asked_while_checks_are_on() {
    let fixture = Fixture::new("schedule-on");
    let owner = Owner::start(&fixture, fixture.port(Duration::from_millis(20)), None);
    wait_until("the launch check", || {
        owner.status()["last_check"]["outcome"] == "found"
    });
    assert_eq!(fixture.feed_requests(), 1);
}

#[test]
fn a_failed_download_is_reported_and_does_not_quit() {
    let fixture = Fixture::new("bad-signature");
    fixture.offer(&SigningKey::from_seed(&[9; 32]).unwrap());
    let port = fixture.port(NEVER);
    let owner = Owner::start(&fixture, port.clone(), None);
    owner.call("update.check", json!({})).unwrap();
    owner.install().unwrap();
    wait_until("the refusal", || owner.status()["error"].is_string());
    let status = owner.status();
    let error = status["error"].as_str().unwrap();
    assert!(error.contains("bad-signature"), "{error}");
    assert_eq!(status["ready"], false);
    assert_eq!(fixture.quits.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.names(), vec!["Luxforge"]);
    owner.stop();
    assert_eq!(port.at_quit(), AtQuit::Nothing);
    assert_eq!(fixture.installed_build(), 1);
}

#[test]
fn a_failed_check_names_its_reason() {
    let fixture = Fixture::new("failed-check");
    fixture
        .fetch
        .fail(&feed_url(), "the server answered HTTP 503");
    let owner = Owner::start(&fixture, fixture.port(NEVER), None);
    let checked = owner.call("update.check", json!({})).unwrap();
    assert_eq!(checked["last_check"]["outcome"], "failed");
    assert_eq!(
        checked["error"],
        "The update check failed: fetch-failed: the server answered HTTP 503"
    );
}

#[test]
fn a_successful_launch_removes_the_previous_payload_and_stray_staging() {
    let fixture = Fixture::new("cleanup");
    testing::install_payload(
        &fixture.apps().join("Luxforge.previous"),
        "0.0.9",
        0,
        TARGET,
    );
    std::fs::create_dir(fixture.apps().join(".luxforge-update-5")).unwrap();
    let port = fixture.port(NEVER);
    // Nothing is removed before the owner has opened the catalog.
    assert_eq!(fixture.names().len(), 3);
    let _owner = Owner::start(&fixture, port, None);
    wait_until("the cleanup", || fixture.names() == vec!["Luxforge"]);
}

#[test]
fn quitting_cancels_a_background_download_and_installs_nothing() {
    let fixture = Fixture::new("quit-cancels");
    let stalled = fixture
        .fetch
        .stall(ARCHIVE, fixture.archive.clone(), fixture.archive.len() / 2);
    let port = fixture.port(NEVER);
    let owner = Owner::start(
        &fixture,
        port.clone(),
        Some(json!({"format": 1, "update_install_automatically": true})),
    );
    owner.call("update.check", json!({})).unwrap();
    stalled
        .recv_timeout(Duration::from_secs(60))
        .expect("the download stalls");
    assert!(owner.status()["progress"].is_object());
    owner.stop();
    assert_eq!(port.at_quit(), AtQuit::Nothing);
    assert_eq!(fixture.names(), vec!["Luxforge"]);
    assert_eq!(fixture.installed_build(), 1);
}

#[test]
fn the_port_reports_its_mechanism_and_build_before_any_owner() {
    let fixture = Fixture::new("idle");
    let status = fixture.port(NEVER).status();
    assert_eq!(status.updater.mechanism(), Mechanism::LinuxPayload);
    assert_eq!(status.build, Some(1));
    assert!(!status.checking && status.available.is_none());
}

/// A bundle the location check put outside Applications.
fn placed(location: luxforge_install::Location) -> Placement {
    Placement {
        bundle: "/Users/someone/Downloads/Luxforge.app".into(),
        original: "/Users/someone/Downloads/Luxforge.app".into(),
        location,
        cleanup: luxforge_install::Cleanup::Leave,
    }
}

#[test]
fn move_to_applications_a_release_outside_applications_can_move_and_nothing_else() {
    let port = MovePort::new(Some(7), placed(luxforge_install::Location::Elsewhere));
    let status = port.status();
    assert_eq!(
        (
            status.updater,
            status.build,
            status.can_move_to_applications
        ),
        (
            Updater::Unavailable(UnavailableReason::OutsideApplications),
            Some(7),
            true
        )
    );
    let refused = port.request_install().unwrap_err();
    assert_eq!(
        refused.data.unwrap()["reason"],
        json!("outside-applications")
    );
    // A Luxforge in the way is a conflict naming it; any other failure says why.
    let existing = move_refusal(MoveError::Existing("/Applications/Luxforge.app".into()));
    assert_eq!(existing.kind, ErrorKind::Conflict);
    assert_eq!(
        *existing.data.unwrap(),
        json!({"reason": "existing-app", "path": "/Applications/Luxforge.app"})
    );
    let copy = move_refusal(MoveError::Copy("disk full".into()));
    assert_eq!(copy.kind, ErrorKind::NotReady);
    assert_eq!(*copy.data.unwrap(), json!({"reason": "copy-failed"}));
    // A bundle already in Applications is never moved, and nothing is relaunched.
    let refused = move_into_applications(
        &placed(luxforge_install::Location::Applications),
        false,
        &luxforge_install::System,
    )
    .unwrap_err();
    assert_eq!(refused.data.unwrap()["reason"], json!("in-applications"));
    assert!(crate::app::relaunch::take().is_none());
}

#[test]
fn move_to_applications_answers_at_once_and_reports_a_failed_move() {
    // A bundle no Applications folder holds, which the move refuses on its own thread: it is
    // already in Applications, so nothing is read beyond the destination or written anywhere.
    let mut placement = placed(luxforge_install::Location::Applications);
    placement.original = "/Users/someone/Downloads/Luxforge Move Test 7f3a.app".into();
    placement.bundle = placement.original.clone();
    let port = MovePort::new(Some(7), placement);
    port.move_to_applications(false).unwrap();
    wait_until("the failed move", || port.status().error.is_some());
    let status = port.status();
    assert_eq!(
        status.error.as_deref(),
        Some("Luxforge is already in an Applications folder")
    );
    assert!(status.can_move_to_applications, "it may be tried again");
    assert!(crate::app::relaunch::take().is_none());
}
