//! The `update.*` methods through the catalog owner, against the fake port
//! ([`crate::update::testing::FakeUpdatePort`]): discovery and authority, a host with no port and a
//! development build's port, a check answered with its outcome and announced, a held check and one
//! that joins it, a catalog format change, skip, the preferences and the flag reaching the port,
//! install refused without an update or while an export runs, and the move into Applications.
use super::*;
use crate::update::{
    CATALOG_FORMAT, CheckOutcome, UnavailablePort, UnavailableReason, UpdatePort, Updater,
    testing::FakeUpdatePort,
};
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};
use std::fs;

struct Harness {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    /// The host's own client, with permission authority.
    desktop: ClientId,
    /// An ordinary connected client, as the live-session CLI is.
    client: ClientId,
    dir: PathBuf,
}

impl Harness {
    fn start(name: &str, updates: Option<Arc<dyn UpdatePort>>) -> Self {
        let dir = temp_dir(&format!("update-{name}")).canonicalize().unwrap();
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                preferences_dir: Some(dir.join("config")),
                updates,
                ..HostConfig::unconfigured()
            },
        )
        .unwrap();
        let desktop = owner.register_with(ClientAuthority::Permissions);
        let client = owner.register();
        let harness = Self {
            owner,
            join: Some(join),
            desktop,
            client,
            dir,
        };
        // Answered once the owner has started, so its port has been attached and its policy
        // applied.
        harness.status();
        harness
    }

    fn with(name: &str, port: &Arc<FakeUpdatePort>) -> Self {
        Self::start(name, Some(port.clone()))
    }

    fn send(&self, client: ClientId, id: &str, method: &str, params: Value) -> ApiResponse {
        self.owner
            .call(
                client,
                ApiRequest {
                    id: id.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(&self, client: ClientId, method: &str, params: Value) -> Value {
        let response = self.send(client, method, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    /// The refusal's code and `data.reason`, or `data` itself when it names no reason.
    fn refused(&self, client: ClientId, method: &str, params: Value) -> (String, Value) {
        let error = self
            .send(client, method, method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to be refused"));
        let data = error.data.unwrap_or(Value::Null);
        let reason = data.get("reason").cloned().unwrap_or(data);
        (error.code, reason)
    }

    fn status(&self) -> Value {
        self.ok(self.client, "update.status", json!({}))
    }

    /// The request ids of the events recorded under `method`.
    fn announced(&self, method: &str) -> Vec<String> {
        self.ok(self.client, "events.since", json!({"after": 0}))["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["method"] == method)
            .map(|event| event["request_id"].as_str().unwrap().to_owned())
            .collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn mutation(request_id: &str) -> Value {
    json!({"mutation": {"request_id": request_id, "actor": "test"}})
}

#[test]
fn update_methods_are_described_and_a_host_without_a_port_checks_nothing() {
    let harness = Harness::start("headless", None);
    let schema = harness.ok(harness.client, "schema.list", json!({}));
    let methods = &schema["methods"];
    for name in [
        "update.status",
        "update.check",
        "update.install",
        "update.skip",
        "update.move-to-applications",
    ] {
        assert!(methods[name].is_object(), "{name} is discoverable");
    }
    for name in ["update.install", "update.move-to-applications"] {
        assert_eq!(methods[name]["mutates"], json!(true), "{name}");
        let notes = methods[name]["notes"].as_str().unwrap();
        assert!(notes.contains("permission authority"), "{name}: {notes}");
    }
    let skip: Vec<_> = methods["update.skip"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|parameter| parameter["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(skip, ["version"]);

    let status = harness.status();
    assert_eq!(
        (&status["mechanism"], &status["reason"]),
        (&json!("unavailable"), &json!("headless"))
    );
    assert_eq!(status["current"]["version"], crate::update::CURRENT_VERSION);
    assert_eq!(status["current"]["build"], Value::Null);
    assert_eq!(
        (&status["checks"], &status["install_automatically"]),
        (&json!(true), &json!(false))
    );
    for (client, method, params) in [
        (harness.client, "update.check", json!({})),
        (harness.desktop, "update.install", mutation("install")),
    ] {
        assert_eq!(
            harness.refused(client, method, params),
            ("not-ready".into(), json!("headless")),
            "{method}"
        );
    }
    assert_eq!(
        harness.refused(
            harness.desktop,
            "update.move-to-applications",
            mutation("move")
        ),
        ("not-ready".into(), json!("not-supported"))
    );
}

#[test]
fn update_a_development_build_names_its_reason_and_never_reaches_the_feed() {
    let harness = Harness::start(
        "development",
        Some(Arc::new(UnavailablePort::new(
            UnavailableReason::DevelopmentBuild,
        ))),
    );
    let status = harness.status();
    assert_eq!(
        (&status["mechanism"], &status["reason"]),
        (&json!("unavailable"), &json!("development-build"))
    );
    assert_eq!(
        harness.refused(harness.client, "update.check", json!({})),
        ("not-ready".into(), json!("development-build"))
    );
    // A fake standing in for a port that reports the same is never asked to check.
    let port = FakeUpdatePort::new(Updater::Unavailable(UnavailableReason::NoUpdateKey));
    let harness = Harness::with("no-key", &port);
    assert_eq!(
        harness.refused(harness.client, "update.check", json!({})),
        ("not-ready".into(), json!("no-update-key"))
    );
    assert_eq!(port.checks(), 0, "no check, so no network request");
    assert_eq!(port.attached(), 1);
}

#[test]
fn update_a_check_is_answered_with_its_outcome_and_announced() {
    let port = FakeUpdatePort::new(Updater::Sparkle);
    let harness = Harness::with("check", &port);
    let policy = port.policy().expect("the policy is applied at launch");
    assert!(policy.checks && !policy.install_automatically && !policy.prerelease_channel);
    assert_eq!(policy.catalog_format(), CATALOG_FORMAT);

    let up_to_date = harness.ok(harness.client, "update.check", json!({}));
    assert_eq!(up_to_date["last_check"]["outcome"], "up-to-date");
    assert_eq!(up_to_date["available"], Value::Null);

    port.finds(Some(FakeUpdatePort::release("0.2.0", 2)));
    let found = harness.ok(harness.client, "update.check", json!({}));
    assert_eq!(found["last_check"]["outcome"], "found");
    assert_eq!(found["checking"], false);
    assert_eq!(
        found["available"],
        json!({
            "version": "0.2.0",
            "build": 2,
            "notes_url": "https://github.com/luxforge/luxforge/releases/tag/v0.2.0",
            "catalog_format": CATALOG_FORMAT,
            "catalog_format_change": false,
            "prerelease": false,
            "skipped": false,
            "installs_automatically": false,
        })
    );
    assert_eq!(port.checks(), 2);
    assert_eq!(
        harness.announced("update.status"),
        ["update:1", "update:2"],
        "each outcome is announced once"
    );

    // Download progress alone is not announced; readiness is.
    port.update(|status| {
        status.progress = Some(crate::update::Progress {
            received: 10,
            total: Some(100),
        })
    });
    assert_eq!(
        harness.status()["progress"],
        json!({"received": 10, "total": 100})
    );
    assert_eq!(harness.announced("update.status").len(), 2);
    port.update(|status| {
        status.progress = None;
        status.ready = true;
    });
    assert_eq!(harness.status()["ready"], true);
    assert_eq!(harness.announced("update.status").len(), 3);

    // A failure is reported in words.
    port.fails("the feed answered 503");
    let failed = harness.ok(harness.client, "update.check", json!({}));
    assert_eq!(failed["last_check"]["outcome"], "failed");
    assert_eq!(failed["error"], "the feed answered 503");
}

#[test]
fn update_a_held_check_answers_when_it_ends_and_a_second_joins_it() {
    let port = FakeUpdatePort::new(Updater::LinuxPayload);
    let harness = Harness::with("held", &port);
    port.hold_checks(true);
    port.finds(Some(FakeUpdatePort::release("0.2.0", 2)));
    let owner = harness.owner.clone();
    let client = harness.client;
    let waiting = std::thread::spawn(move || {
        owner
            .call(
                client,
                ApiRequest {
                    id: "held".into(),
                    method: "update.check".into(),
                    params: json!({}),
                    token: None,
                },
            )
            .unwrap()
    });
    luxforge_testbase::wait_for("the check to start", || {
        (port.held_checks() == 1).then_some(())
    });
    // A caller that may not wait is answered at once, joining the running check.
    let unparked = harness
        .owner
        .call_unparked(
            harness.desktop,
            ApiRequest {
                id: "unparked".into(),
                method: "update.check".into(),
                params: json!({}),
                token: None,
            },
        )
        .unwrap()
        .result
        .unwrap();
    assert_eq!(unparked["checking"], true);
    assert_eq!(unparked["available"], Value::Null);
    assert_eq!(port.checks(), 1, "a second call joins the check that runs");
    port.release_checks();
    let answered = waiting.join().unwrap();
    assert_eq!(answered.id, "held");
    let answered = answered.result.unwrap();
    assert_eq!(answered["last_check"]["outcome"], "found");
    assert_eq!(answered["available"]["version"], "0.2.0");
    assert_eq!(answered["checking"], false);
    // The next check is a new one.
    port.hold_checks(false);
    harness.ok(harness.client, "update.check", json!({}));
    assert_eq!(port.checks(), 2);
}

#[test]
fn update_a_catalog_format_change_is_offered_and_never_installed_automatically() {
    let port = FakeUpdatePort::new(Updater::Sparkle);
    let harness = Harness::with("format", &port);
    harness.ok(
        harness.client,
        "preferences.set",
        json!({"update_install_automatically": true}),
    );
    let policy = port.policy().unwrap();
    assert!(
        policy.install_automatically,
        "the preference reached the port"
    );
    port.finds(Some(FakeUpdatePort::release("0.2.0", 2)));
    let same = harness.ok(harness.client, "update.check", json!({}));
    assert_eq!(same["available"]["installs_automatically"], true);
    port.finds(Some(crate::update::FeedUpdate {
        catalog_format: CATALOG_FORMAT + 1,
        ..FakeUpdatePort::release("0.3.0", 3)
    }));
    let changed = harness.ok(harness.client, "update.check", json!({}));
    assert_eq!(changed["available"]["catalog_format_change"], true);
    assert_eq!(changed["available"]["installs_automatically"], false);
    assert!(
        !port
            .policy()
            .unwrap()
            .installs_automatically(&port.status().available.unwrap())
    );
    // Asked for, it installs.
    let installed = harness.ok(harness.desktop, "update.install", mutation("format"));
    assert_eq!(installed["outcome"], "requested");
    assert_eq!(port.installs(), 1);
}

#[test]
fn update_install_needs_authority_an_update_and_no_running_export() {
    let port = FakeUpdatePort::new(Updater::Sparkle);
    let harness = Harness::with("install", &port);
    assert_eq!(
        harness.refused(harness.client, "update.install", mutation("cli")),
        ("forbidden".into(), Value::Null)
    );
    assert_eq!(
        harness.refused(harness.desktop, "update.install", mutation("none")),
        ("not-ready".into(), json!("no-update"))
    );
    port.finds(Some(FakeUpdatePort::release("0.2.0", 2)));
    harness.ok(harness.client, "update.check", json!({}));

    // An export that runs holds the update back, named.
    let path = harness.dir.join("photo.jpg");
    fs::copy(fixture(), &path).unwrap();
    let asset = super::library::opening::import(&harness.owner, harness.client, &path);
    let (reached, reaches) = std::sync::mpsc::channel();
    let (release, released) = sync_channel::<()>(4);
    let (reached, released) = (
        std::sync::Mutex::new(reached),
        std::sync::Mutex::new(released),
    );
    harness.owner.hold_exports(Some(Arc::new(move |phase| {
        if phase == "rendering" {
            let _ = reached.lock().unwrap().send(());
            let _ = released.lock().unwrap().recv();
        }
    })));
    let destination = harness.dir.join("exported.jpg");
    let export = harness.ok(
        harness.client,
        "export.jpeg",
        json!({
            "asset_id": asset,
            "destination": destination,
            "mutation": {"request_id": "export", "actor": "test"},
        }),
    );
    reaches.recv_timeout(luxforge_testbase::HANG).unwrap();
    let error = harness
        .send(harness.desktop, "busy", "update.install", mutation("busy"))
        .error
        .expect("refused while an export runs");
    assert_eq!(error.code, "conflict");
    assert!(error.message.contains("export job-"), "{}", error.message);
    assert_eq!(
        error.data.unwrap()["jobs"],
        json!([{"job_id": export["job_id"], "kind": "export"}])
    );
    assert_eq!(port.installs(), 0);
    harness.owner.hold_exports(None);
    let _ = release.send(());
    luxforge_testbase::wait_for("the export to finish", || {
        let read = harness.ok(
            harness.client,
            "job.read",
            json!({"job_id": export["job_id"]}),
        );
        (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(())
    });

    // Then it is requested once, and a retry answers the first answer.
    let requested = harness.ok(harness.desktop, "update.install", mutation("go"));
    assert_eq!(requested["outcome"], "requested");
    assert_eq!(requested["status"]["install_requested"], true);
    harness.ok(harness.desktop, "update.install", mutation("go"));
    assert_eq!(port.installs(), 1);
    assert_eq!(harness.announced("update.install"), ["update.install"]);

    // The port's own refusal is answered as it is.
    port.refuse_next(Error::not_ready("the download is not verified yet"));
    let (code, _) = harness.refused(harness.desktop, "update.install", mutation("again"));
    assert_eq!(code, "not-ready");
}

#[test]
fn update_a_notify_only_installation_offers_and_never_installs() {
    let port = FakeUpdatePort::new(Updater::LinuxNotify);
    let harness = Harness::with("notify", &port);
    port.finds(Some(FakeUpdatePort::release("0.2.0", 2)));
    let found = harness.ok(harness.client, "update.check", json!({}));
    assert_eq!(found["mechanism"], "linux-notify");
    assert_eq!(found["available"]["version"], "0.2.0");
    assert_eq!(
        harness.refused(harness.desktop, "update.install", mutation("notify")),
        ("not-ready".into(), json!("notify-only"))
    );
    assert_eq!(port.installs(), 0);
}

#[test]
fn update_skip_is_stored_kept_by_preferences_and_handed_to_the_port() {
    let port = FakeUpdatePort::new(Updater::Sparkle);
    let harness = Harness::with("skip", &port);
    port.finds(Some(FakeUpdatePort::release("0.2.0", 2)));
    harness.ok(harness.client, "update.check", json!({}));
    let skipped = harness.ok(harness.client, "update.skip", json!({"version": "0.2.0"}));
    assert_eq!(skipped["outcome"], "applied");
    assert_eq!(skipped["status"]["available"]["skipped"], true);
    assert_eq!(skipped["status"]["skipped_version"], "0.2.0");
    assert_eq!(port.skips(), ["0.2.0"]);
    assert_eq!(
        port.policy().unwrap().skipped_version.as_deref(),
        Some("0.2.0")
    );
    let again = harness.ok(harness.client, "update.skip", json!({"version": "0.2.0"}));
    assert_eq!(again["outcome"], "no-op");
    assert_eq!(harness.announced("update.skip"), ["update.skip"]);
    assert_eq!(
        harness.refused(harness.client, "update.skip", json!({"version": "../0.2"})),
        ("validation".into(), Value::Null)
    );

    // preferences.read shows it, and preferences.set cannot change or reset it.
    let read = harness.ok(harness.client, "preferences.read", json!({}));
    assert_eq!(read["update_skipped_version"], "0.2.0");
    let refused = harness
        .send(
            harness.client,
            "bypass",
            "preferences.set",
            json!({"update_skipped_version": null}),
        )
        .error
        .unwrap();
    assert!(
        refused.message.contains("unknown field"),
        "{}",
        refused.message
    );
    let reset = harness.ok(
        harness.client,
        "preferences.set",
        json!({"update_checks": null, "update_install_automatically": null}),
    );
    assert_eq!(reset["update_skipped_version"], "0.2.0");

    // A later version is offered as usual.
    port.finds(Some(FakeUpdatePort::release("0.2.1", 3)));
    let later = harness.ok(harness.client, "update.check", json!({}));
    assert_eq!(later["available"]["skipped"], false);
}

#[test]
fn update_preferences_and_the_prerelease_flag_reach_the_port_and_are_announced() {
    let port = FakeUpdatePort::new(Updater::Sparkle);
    let harness = Harness::with("preferences", &port);
    let applied = port.applied();
    let answer = harness.ok(
        harness.client,
        "preferences.set",
        json!({"update_checks": false, "update_install_automatically": true}),
    );
    assert_eq!(
        (
            &answer["update_checks"],
            &answer["update_install_automatically"]
        ),
        (&json!(false), &json!(true))
    );
    let policy = port.policy().unwrap();
    assert!(!policy.checks && policy.install_automatically);
    assert_eq!(port.applied(), applied + 1);
    // The same values again change nothing and reach nothing.
    harness.ok(
        harness.client,
        "preferences.set",
        json!({"update_checks": false}),
    );
    assert_eq!(port.applied(), applied + 1);
    assert_eq!(harness.announced("preferences.set"), ["preferences.set"]);
    let status = harness.status();
    assert_eq!(
        (&status["checks"], &status["install_automatically"]),
        (&json!(false), &json!(true))
    );

    // A pre-release is offered only once the flag is on.
    port.finds(Some(crate::update::FeedUpdate {
        prerelease: true,
        ..FakeUpdatePort::release("0.3.0-rc.1", 4)
    }));
    let hidden = harness.ok(harness.client, "update.check", json!({}));
    assert_eq!(hidden["available"], Value::Null);
    assert_eq!(hidden["last_check"]["outcome"], "found");
    harness.ok(
        harness.client,
        "flags.set",
        json!({"flag": crate::flags::PRERELEASE_CHANNEL, "value": true}),
    );
    assert!(port.policy().unwrap().prerelease_channel);
    let shown = harness.status();
    assert_eq!(shown["prerelease_channel"], true);
    assert_eq!(shown["available"]["prerelease"], true);
    assert_eq!(
        port.status().last_check.map(|check| check.outcome),
        Some(CheckOutcome::Found)
    );
}

#[test]
fn update_move_to_applications_needs_authority_and_a_port_that_can() {
    let port = FakeUpdatePort::new(Updater::Unavailable(UnavailableReason::OutsideApplications));
    let harness = Harness::with("move", &port);
    assert_eq!(
        harness.refused(
            harness.desktop,
            "update.move-to-applications",
            mutation("cannot")
        ),
        ("not-ready".into(), json!("not-supported"))
    );
    port.update(|status| status.can_move_to_applications = true);
    let status = harness.status();
    assert_eq!(
        (&status["reason"], &status["can_move_to_applications"]),
        (&json!("outside-applications"), &json!(true))
    );
    assert_eq!(
        harness.refused(
            harness.client,
            "update.move-to-applications",
            mutation("cli")
        ),
        ("forbidden".into(), Value::Null)
    );
    let moved = harness.ok(
        harness.desktop,
        "update.move-to-applications",
        mutation("move"),
    );
    assert_eq!(moved["outcome"], "requested");
    assert_eq!(port.moves(), 1);
    // The port refuses a move that would replace an existing app without the confirmation, and
    // its refusal is answered as it is; the confirmed move carries replace.
    port.refuse_next(
        Error::conflict("Luxforge is already in /Applications")
            .with_data(json!({"reason": "existing-app", "path": "/Applications/Luxforge.app"})),
    );
    assert_eq!(
        harness.refused(
            harness.desktop,
            "update.move-to-applications",
            mutation("in-the-way")
        ),
        ("conflict".into(), json!("existing-app"))
    );
    let mut confirmed = mutation("replace");
    confirmed["replace"] = json!(true);
    harness.ok(harness.desktop, "update.move-to-applications", confirmed);
    assert_eq!(port.move_requests(), [false, true]);
}
