//! The Sparkle port driven through the catalog owner's `update.*` commands against a scripted
//! bridge, which records every command and answers as Sparkle does, on a thread of its own: the
//! preferences applied to Sparkle, channel filtering, the catalog format guard, a requested install
//! through the quit path, an automatic one, skips, a check while an offer is held, a postponed
//! relaunch and a cancelled installation at quit.
use super::*;
use crate::app::update_port::{JOURNEY_KEY, feed_update_from};
use luxforge_core::{
    ApiRequest, ClientAuthority, ClientId, HostConfig, ModuleRegistry, OwnerHandle,
    update::CATALOG_FORMAT,
};
use luxforge_sparkle::Item;
use luxforge_testbase::wait_until;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

/// What the scripted bridge answers a command with.
type Script = Box<dyn FnMut(&Command) -> Vec<Event> + Send>;

/// The bridge, scripted: it records each command and delivers the events its script answers in
/// order on one thread, as Sparkle delivers them on the main thread after the call returned.
struct FakeSparkle {
    commands: Mutex<Vec<Command>>,
    script: Mutex<Script>,
    events: Mutex<mpsc::Sender<Event>>,
}

impl FakeSparkle {
    fn start(listener: Arc<PortListener>, script: Script) -> Arc<Self> {
        let (events, receive) = mpsc::channel::<Event>();
        std::thread::spawn(move || {
            for event in receive {
                listener.event(event);
            }
        });
        Arc::new(Self {
            commands: Mutex::new(Vec::new()),
            script: Mutex::new(script),
            events: Mutex::new(events),
        })
    }

    fn commands(&self) -> Vec<Command> {
        self.commands.lock().unwrap().clone()
    }

    fn answers(&self) -> Vec<(Reply, Choice)> {
        self.commands()
            .into_iter()
            .filter_map(|command| match command {
                Command::Answer(reply, choice) => Some((reply, choice)),
                _ => None,
            })
            .collect()
    }

    /// Deliver `event` as Sparkle would, unasked.
    fn raise(&self, event: Event) {
        self.events.lock().unwrap().send(event).unwrap();
    }
}

impl Driver for FakeSparkle {
    fn send(&self, command: Command) {
        self.commands.lock().unwrap().push(command.clone());
        let events = (self.script.lock().unwrap())(&command);
        for event in events {
            self.raise(event);
        }
    }
}

fn item(version: &str, build: u64, catalog_format: Option<i64>) -> Item {
    Item {
        version: version.into(),
        build: build.to_string(),
        link: Some(format!(
            "https://github.com/Willyham/luxforge/releases/tag/v{version}"
        )),
        catalog_format: catalog_format.map(|format| format.to_string()),
        channel: None,
        length: 4_000_000,
    }
}

/// Sparkle as it answers a feed offering `found`: a check finds it, an install downloads,
/// extracts and readies it, and the installer takes it when told to install.
fn sparkle(found: Option<Item>) -> Script {
    Box::new(move |command| match command {
        Command::Check | Command::CheckInBackground => {
            let mut events = vec![Event::CheckStarted(CheckKind::Asked)];
            match &found {
                Some(item) => events.push(Event::Found {
                    item: item.clone(),
                    stage: Stage::NotDownloaded,
                }),
                None => events.extend([
                    Event::NotFound,
                    Event::CycleFinished { error: None },
                    Event::Dismissed,
                ]),
            }
            events
        }
        Command::Answer(Reply::Found, Choice::Install) => vec![
            Event::DownloadStarted,
            Event::DownloadLength(4_000_000),
            Event::DownloadReceived(1_000_000),
            Event::DownloadReceived(3_000_000),
            Event::Extracting,
            Event::Ready,
        ],
        Command::Answer(Reply::Found, Choice::Dismiss) => vec![
            Event::Dismissed,
            Event::CycleFinished { error: None },
        ],
        Command::Answer(Reply::Ready, Choice::Install) => {
            vec![Event::Installing { terminated: false }]
        }
        Command::Answer(_, Choice::Skip) => vec![Event::Dismissed],
        _ => Vec::new(),
    })
}

struct Fixture {
    dir: PathBuf,
    port: Arc<SparklePort>,
    sparkle: Arc<FakeSparkle>,
    quits: Arc<AtomicUsize>,
}

impl Fixture {
    fn new(name: &str, script: Script) -> Self {
        let dir = luxforge_testbase::paths::temp_dir(&format!("sparkle-port-{name}"));
        let quits = Arc::new(AtomicUsize::new(0));
        let counted = quits.clone();
        let (port, listener) = SparklePort::new(
            Some(41),
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let sparkle = FakeSparkle::start(listener, script);
        port.drive(sparkle.clone());
        Self {
            dir,
            port,
            sparkle,
            quits,
        }
    }

    fn quits(&self) -> usize {
        self.quits.load(Ordering::SeqCst)
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
    fn start(fixture: &Fixture, preferences: Option<Value>) -> Self {
        let config = fixture.dir.join("config");
        std::fs::create_dir_all(&config).unwrap();
        if let Some(preferences) = preferences {
            std::fs::write(config.join("preferences.json"), preferences.to_string()).unwrap();
        }
        let (owner, join) = OwnerHandle::start_with_host(
            &fixture.dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                preferences_dir: Some(config),
                updates: Some(fixture.port.clone()),
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

    fn skip(&self, version: &str) -> Value {
        self.call(
            "update.skip",
            json!({"version": version}),
        )
        .unwrap()
    }

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

const QUICK: Duration = Duration::from_secs(5);

#[test]
fn sparkle_takes_the_persons_preferences_and_never_downloads_on_its_own() {
    let fixture = Fixture::new("preferences", sparkle(None));
    let owner = Owner::start(&fixture, None);
    let status = owner.status();
    assert_eq!(
        (&status["mechanism"], &status["current"]["build"]),
        (&json!("sparkle"), &json!(41))
    );
    // The launch configures Sparkle from the stored preferences and asks for one check shortly
    // after; Sparkle's own automatic downloads stay off (the bridge sets them off every time).
    assert_eq!(
        fixture.sparkle.commands(),
        vec![
            Command::Configure {
                automatic_checks: true
            },
            Command::CheckInBackgroundAfter(LAUNCH_CHECK_SECONDS),
        ]
    );
    owner
        .call(
            "preferences.set",
            json!({"update_checks": false}),
        )
        .unwrap();
    wait_until("the policy applied", || {
        fixture.sparkle.commands().last()
            == Some(&Command::Configure {
                automatic_checks: false,
            })
    });
    // The launch check is asked for once only.
    assert_eq!(
        fixture
            .sparkle
            .commands()
            .iter()
            .filter(|command| matches!(command, Command::CheckInBackgroundAfter(_)))
            .count(),
        1
    );
}

#[test]
fn sparkle_follows_the_prerelease_channel_only_with_its_flag() {
    let fixture = Fixture::new("channels", sparkle(None));
    let listener = PortListener(fixture.port.shared.clone());
    assert!(listener.allowed_channels().is_empty());
    fixture
        .port
        .apply(&UpdatePolicy::new(true, false, true, None));
    assert_eq!(listener.allowed_channels(), vec!["prerelease".to_owned()]);
    fixture
        .port
        .apply(&UpdatePolicy::new(true, false, false, None));
    assert!(listener.allowed_channels().is_empty());
    // An item outside the followed channels is dismissed, never offered.
    let mut prerelease = item("0.3.0-rc.1", 50, Some(CATALOG_FORMAT));
    prerelease.channel = Some("prerelease".into());
    fixture.sparkle.raise(Event::Found {
        item: prerelease,
        stage: Stage::NotDownloaded,
    });
    wait_until("the dismissal", || {
        fixture.sparkle.answers() == vec![(Reply::Found, Choice::Dismiss)]
    });
    assert_eq!(fixture.port.status().available, None);
}

#[test]
fn sparkle_items_read_as_the_core_compares_them() {
    let update = feed_update_from(&item("0.2.0", 42, Some(CATALOG_FORMAT)));
    assert_eq!(
        (update.version.as_str(), update.build, update.catalog_format),
        ("0.2.0", 42, CATALOG_FORMAT)
    );
    assert!(!update.prerelease);
    // An item that does not state its catalog format counts as a change.
    let unknown = feed_update_from(&item("0.2.0", 42, None));
    assert!(UpdatePolicy::new(true, true, false, None).changes_catalog_format(&unknown));
    assert_eq!(JOURNEY_KEY, "LuxforgeUpdateJourney");
}

#[test]
fn sparkle_installs_automatically_only_an_update_with_this_catalog_format() {
    for (format, automatic) in [
        (Some(CATALOG_FORMAT), true),
        (Some(CATALOG_FORMAT + 1), false),
        (None, false),
    ] {
        let fixture = Fixture::new(
            "automatic",
            sparkle(Some(item("0.2.0", 42, format))),
        );
        let owner = Owner::start(
            &fixture,
            Some(json!({"format": 1, "update_install_automatically": true})),
        );
        let checked = owner.call("update.check", json!({})).unwrap();
        assert_eq!(checked["last_check"]["outcome"], "found", "{checked}");
        assert_eq!(
            checked["available"]["catalog_format_change"], !automatic,
            "{checked}"
        );
        if automatic {
            wait_until("the background download", || {
                owner.status()["ready"] == true
            });
            assert_eq!(
                fixture.sparkle.answers(),
                vec![(Reply::Found, Choice::Install)]
            );
            // Ready, and nobody asked to quit: it installs when the app next quits, without
            // relaunching, by Sparkle's installer.
            assert_eq!(fixture.quits(), 0);
            owner.stop();
            assert_eq!(fixture.port.at_quit(QUICK, QUICK), AtQuit::Installed);
            assert_eq!(
                fixture.sparkle.answers(),
                vec![(Reply::Found, Choice::Install)]
            );
        } else {
            // Offered, never prepared for the installer without being asked.
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(owner.status()["ready"], false);
            assert!(fixture.sparkle.answers().is_empty());
            owner.stop();
            assert_eq!(fixture.port.at_quit(QUICK, QUICK), AtQuit::Nothing);
            assert!(fixture.sparkle.answers().is_empty());
        }
    }
}

#[test]
fn sparkle_a_requested_install_downloads_quits_and_installs_once_the_catalog_closed() {
    let fixture = Fixture::new(
        "requested",
        sparkle(Some(item("0.2.0", 42, Some(CATALOG_FORMAT + 1)))),
    );
    let owner = Owner::start(&fixture, None);
    assert_eq!(owner.install().unwrap_err().1["reason"], "no-update");
    let checked = owner.call("update.check", json!({})).unwrap();
    let available = &checked["available"];
    assert_eq!(
        (&available["version"], &available["build"]),
        (&json!("0.2.0"), &json!(42))
    );
    assert_eq!(
        available["notes_url"],
        "https://github.com/Willyham/luxforge/releases/tag/v0.2.0"
    );
    // A catalog format change installs when the person asks.
    assert_eq!(available["catalog_format_change"], true);
    let requested = owner.install().unwrap();
    assert_eq!(requested["outcome"], "requested");
    wait_until("the quit request", || fixture.quits() == 1);
    let ready = owner.status();
    assert_eq!(
        (&ready["ready"], &ready["progress"]),
        (&json!(true), &Value::Null)
    );
    // Sparkle is not told to install until the catalog has closed, and asking the delegate in
    // the meantime postpones the relaunch.
    assert_eq!(
        fixture.sparkle.answers(),
        vec![(Reply::Found, Choice::Install)]
    );
    assert!(PortListener(fixture.port.shared.clone()).postpone_relaunch());
    owner.stop();
    assert_eq!(fixture.port.at_quit(QUICK, QUICK), AtQuit::Installed);
    assert_eq!(
        fixture.sparkle.answers(),
        vec![
            (Reply::Found, Choice::Install),
            (Reply::Ready, Choice::Install)
        ]
    );
    assert!(!PortListener(fixture.port.shared.clone()).postpone_relaunch());
    assert_eq!(fixture.quits(), 1);
}

#[test]
fn sparkle_a_postponed_relaunch_quits_first_and_resumes_at_quit() {
    let fixture = Fixture::new("postponed", sparkle(Some(item("0.2.0", 42, Some(CATALOG_FORMAT)))));
    let owner = Owner::start(&fixture, None);
    owner.call("update.check", json!({})).unwrap();
    owner.install().unwrap();
    wait_until("the quit request", || fixture.quits() == 1);
    // Sparkle starting a relaunch on its own before the catalog closed is postponed, and asks
    // for the quit path.
    fixture.sparkle.raise(Event::RelaunchPostponed);
    wait_until("another quit request", || fixture.quits() == 2);
    owner.stop();
    fixture.sparkle.raise(Event::Installing { terminated: false });
    assert_eq!(fixture.port.at_quit(QUICK, QUICK), AtQuit::Installed);
    assert!(fixture.sparkle.commands().contains(&Command::Resume));
    assert!(
        !fixture
            .sparkle
            .answers()
            .contains(&(Reply::Ready, Choice::Install))
    );
}

#[test]
fn sparkle_an_installer_that_does_not_take_the_update_fails_the_quit_hook() {
    let mut script = sparkle(Some(item("0.2.0", 42, Some(CATALOG_FORMAT))));
    let fixture = Fixture::new(
        "stalled",
        Box::new(move |command| match command {
            Command::Answer(Reply::Ready, Choice::Install) => Vec::new(),
            other => script(other),
        }),
    );
    let owner = Owner::start(&fixture, None);
    owner.call("update.check", json!({})).unwrap();
    owner.install().unwrap();
    wait_until("the quit request", || fixture.quits() == 1);
    owner.stop();
    assert!(matches!(
        fixture
            .port
            .at_quit(Duration::from_millis(50), QUICK),
        AtQuit::Failed(_)
    ));
}

#[test]
fn sparkle_a_skip_dismisses_the_offer_and_cancels_a_prepared_install() {
    let fixture = Fixture::new("skip", sparkle(Some(item("0.2.0", 42, Some(CATALOG_FORMAT)))));
    let owner = Owner::start(&fixture, None);
    owner.call("update.check", json!({})).unwrap();
    let skipped = owner.skip("0.2.0");
    assert_eq!(skipped["status"]["available"]["skipped"], true, "{skipped}");
    // Luxforge's skip is its own: the offer is dismissed, never skipped in Sparkle.
    wait_until("the dismissal", || {
        fixture.sparkle.answers() == vec![(Reply::Found, Choice::Dismiss)]
    });

    // A prepared installation skipped is cancelled, so it never installs at termination.
    let fixture = Fixture::new(
        "skip-ready",
        sparkle(Some(item("0.3.0", 43, Some(CATALOG_FORMAT)))),
    );
    let owner = Owner::start(
        &fixture,
        Some(json!({"format": 1, "update_install_automatically": true})),
    );
    owner.call("update.check", json!({})).unwrap();
    wait_until("the background download", || {
        owner.status()["ready"] == true
    });
    owner.skip("0.3.0");
    wait_until("the cancellation", || owner.status()["ready"] == false);
    assert_eq!(
        fixture.sparkle.answers(),
        vec![(Reply::Found, Choice::Install), (Reply::Ready, Choice::Skip)]
    );
    owner.stop();
    assert_eq!(fixture.port.at_quit(QUICK, QUICK), AtQuit::Nothing);
}

#[test]
fn sparkle_an_installation_no_longer_wanted_is_cancelled_at_quit() {
    let fixture = Fixture::new(
        "unwanted",
        sparkle(Some(item("0.2.0", 42, Some(CATALOG_FORMAT)))),
    );
    let owner = Owner::start(
        &fixture,
        Some(json!({"format": 1, "update_install_automatically": true})),
    );
    owner.call("update.check", json!({})).unwrap();
    wait_until("the background download", || {
        owner.status()["ready"] == true
    });
    owner
        .call(
            "preferences.set",
            json!({"update_install_automatically": false}),
        )
        .unwrap();
    owner.stop();
    assert_eq!(fixture.port.at_quit(QUICK, QUICK), AtQuit::Nothing);
    assert_eq!(
        fixture.sparkle.answers(),
        vec![(Reply::Found, Choice::Install), (Reply::Ready, Choice::Skip)]
    );
    assert_eq!(fixture.port.status().ready, false);
}

#[test]
fn sparkle_a_check_while_an_offer_is_held_reads_the_feed_again() {
    let fixture = Fixture::new("recheck", sparkle(Some(item("0.2.0", 42, Some(CATALOG_FORMAT)))));
    let owner = Owner::start(&fixture, None);
    owner.call("update.check", json!({})).unwrap();
    let again = owner.call("update.check", json!({})).unwrap();
    assert_eq!(again["last_check"]["outcome"], "found", "{again}");
    let commands = fixture.sparkle.commands();
    let dismissed = commands
        .iter()
        .position(|command| *command == Command::Answer(Reply::Found, Choice::Dismiss))
        .expect("the held offer is dismissed");
    assert_eq!(
        commands[dismissed..]
            .iter()
            .filter(|command| **command == Command::Check)
            .count(),
        1,
        "{commands:?}"
    );
}

#[test]
fn sparkle_nothing_newer_and_failures_are_reported() {
    let fixture = Fixture::new("none", sparkle(None));
    let owner = Owner::start(&fixture, None);
    let checked = owner.call("update.check", json!({})).unwrap();
    assert_eq!(checked["last_check"]["outcome"], "up-to-date", "{checked}");
    assert_eq!(checked["available"], Value::Null);

    let fixture = Fixture::new(
        "failed",
        Box::new(|command| match command {
            Command::Check => vec![
                Event::CheckStarted(CheckKind::Asked),
                Event::Error("The update feed could not be read.".into()),
                Event::CycleFinished {
                    error: Some("The update feed could not be read.".into()),
                },
            ],
            _ => Vec::new(),
        }),
    );
    let owner = Owner::start(&fixture, None);
    let checked = owner.call("update.check", json!({})).unwrap();
    assert_eq!(checked["last_check"]["outcome"], "failed", "{checked}");
    assert_eq!(checked["error"], "The update feed could not be read.");
    // An updater that could not start says why.
    fixture
        .sparkle
        .raise(Event::Failed("Sparkle could not start: no feed".into()));
    wait_until("the failure", || {
        owner.status()["error"] == "Sparkle could not start: no feed"
    });
}

#[test]
fn sparkle_download_progress_is_reported_in_steps() {
    let fixture = Fixture::new("progress", Box::new(|_| Vec::new()));
    fixture
        .port
        .apply(&UpdatePolicy::new(true, false, false, None));
    let raise = |event| fixture.sparkle.raise(event);
    raise(Event::Found {
        item: item("0.2.0", 42, Some(CATALOG_FORMAT)),
        stage: Stage::NotDownloaded,
    });
    wait_until("the offer", || fixture.port.status().available.is_some());
    fixture.port.request_install().unwrap();
    raise(Event::DownloadStarted);
    raise(Event::DownloadLength(100 * 1024 * 1024));
    raise(Event::DownloadReceived(100 * 1024));
    raise(Event::DownloadReceived(2 * 1024 * 1024));
    wait_until("a progress step", || {
        fixture.port.status().progress
            == Some(Progress {
                received: 2 * 1024 * 1024 + 100 * 1024,
                total: Some(100 * 1024 * 1024),
            })
    });
}
