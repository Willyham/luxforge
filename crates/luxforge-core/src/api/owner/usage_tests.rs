//! `usage.read`, `usage.prompt.claim` and `usage.set` through the catalog owner: schema discovery
//! and authority, generic preference bypasses, the once-only claim and its retries, and a
//! deduplicated enable answered after a revocation. The runtime's own rules are
//! `usage/consent_tests.rs`'.
use super::*;
use crate::usage::{DISCLOSURE_VERSION, Eligibility, TestReceiver, consent::Usage};
use luxforge_testbase::paths::temp_dir;
use std::fs;

struct Harness {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    /// The host's own client, with permission authority.
    desktop: ClientId,
    /// An ordinary connected client, as the live-session CLI is.
    client: ClientId,
    usage: Arc<Usage>,
    dir: PathBuf,
}

impl Harness {
    /// An explicitly test-eligible host over an isolated configuration.
    fn start(name: &str) -> Self {
        let dir = temp_dir(&format!("usage-owner-{name}"))
            .canonicalize()
            .unwrap();
        Self::over(dir, Some(test_usage()))
    }

    fn over(dir: PathBuf, usage: Option<Usage>) -> Self {
        let usage = Arc::new(usage.unwrap_or_default());
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                preferences_dir: Some(dir.join("config")),
                usage_consent: usage.clone(),
                ..HostConfig::unconfigured()
            },
        )
        .unwrap();
        let desktop = owner.register_with(ClientAuthority::Permissions);
        let client = owner.register();
        Self {
            owner,
            join: Some(join),
            desktop,
            client,
            usage,
            dir,
        }
    }

    /// Stop this owner and start another over the same configuration, as a relaunch does.
    fn relaunch(mut self) -> Self {
        self.stop();
        let dir = std::mem::take(&mut self.dir);
        Self::over(dir, Some(test_usage()))
    }

    fn stop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("config").join("preferences.json")
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
        let response = self.send(client, "call", method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn refused(&self, client: ClientId, method: &str, params: Value) -> String {
        let response = self.send(client, "call", method, params);
        response.error.expect("refused").code
    }

    fn announced(&self, method: &str) -> usize {
        let events = self.ok(self.client, "events.since", json!({"after": 0}));
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["method"] == method)
            .count()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop();
        if !self.dir.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn test_usage() -> Usage {
    Usage::new(Eligibility::test(
        TestReceiver::loopback("http://127.0.0.1:9/v1/usage/batches").unwrap(),
    ))
}

fn mutation(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": "test"})
}

fn enable(request_id: &str) -> Value {
    json!({"enabled": true, "disclosure_version": DISCLOSURE_VERSION, "mutation": mutation(request_id)})
}

fn disable(request_id: &str) -> Value {
    json!({"enabled": false, "mutation": mutation(request_id)})
}

#[test]
fn usage_methods_are_described_with_their_authority_and_no_image_key() {
    let harness = Harness::start("schema");
    let schema = harness.ok(harness.client, "schema.list", json!({}));
    let methods = &schema["methods"];
    for name in ["usage.read", "usage.prompt.claim", "usage.set"] {
        assert!(methods[name].is_object(), "{name} is discoverable");
    }
    assert_eq!(methods["usage.read"]["mutates"], json!(false));
    for name in ["usage.prompt.claim", "usage.set"] {
        assert_eq!(methods[name]["mutates"], json!(true), "{name}");
        let notes = methods[name]["notes"].as_str().unwrap();
        assert!(notes.contains("permission authority"), "{name}: {notes}");
    }
    let parameters: Vec<_> = methods["usage.set"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|parameter| parameter["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(parameters, ["enabled", "disclosure_version"]);
    // No schema names a key field or carries a stored key or ID.
    harness.ok(harness.desktop, "usage.set", enable("schema-on"));
    let record: Value = serde_json::from_slice(&fs::read(harness.file()).unwrap()).unwrap();
    let key = record["usage_statistics"]["image_key"].as_str().unwrap();
    let id = record["usage_statistics"]["installation_id"]
        .as_str()
        .unwrap();
    let text = harness
        .ok(harness.client, "schema.list", json!({}))
        .to_string();
    assert!(!text.contains("image_key") && !text.contains(key) && !text.contains(id));
}

#[test]
fn usage_untrusted_enable_and_generic_preference_bypasses_are_refused() {
    let harness = Harness::start("authority");
    assert_eq!(
        harness.refused(harness.client, "usage.set", enable("cli-on")),
        "forbidden"
    );
    assert_eq!(
        harness.refused(
            harness.client,
            "usage.prompt.claim",
            json!({"mutation": mutation("cli-claim")})
        ),
        "forbidden"
    );
    for params in [
        json!({"enabled": true, "mutation": mutation("no-version")}),
        json!({"enabled": true, "disclosure_version": DISCLOSURE_VERSION + 1, "mutation": mutation("old-version")}),
        json!({"enabled": false, "disclosure_version": DISCLOSURE_VERSION, "mutation": mutation("off-version")}),
        json!({"enabled": true, "disclosure_version": DISCLOSURE_VERSION}),
        json!({"enabled": true, "disclosure_version": DISCLOSURE_VERSION, "mutation": mutation("extra"), "installation_id": "x"}),
    ] {
        assert_eq!(
            harness.refused(harness.desktop, "usage.set", params.clone()),
            "validation",
            "{params}"
        );
    }
    for params in [
        json!({"usage_statistics": {"prompt_shown": true, "enabled": true, "disclosure_version": 1}}),
        json!({"usage_statistics": null}),
    ] {
        assert_eq!(
            harness.refused(harness.desktop, "preferences.set", params.clone()),
            "validation",
            "{params}"
        );
    }
    assert!(!harness.file().exists(), "nothing was written");
    assert_eq!(harness.usage.gate().current(), None);

    // The host's own client may enable; a generic reset keeps the record and the read shows it
    // without the key.
    let on = harness.ok(harness.desktop, "usage.set", enable("desktop-on"));
    assert_eq!(on["outcome"], json!("applied"));
    assert_eq!(on["state"]["effective"], json!(true));
    let id = on["state"]["installation_id"].as_str().unwrap().to_owned();
    let reset: Value = [
        "performance_expanded",
        "auto_collapse_history",
        "auto_lens_profile",
        "raw_look",
        "mask_overlay_colour",
        "canvas_background",
        "interface_size",
        "catalog",
        "workspace",
        "brush",
        "window",
        "export_last",
        "hdr_merge",
        "theme",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), Value::Null))
    .collect::<serde_json::Map<_, _>>()
    .into();
    let preferences = harness.ok(harness.desktop, "preferences.set", reset);
    assert_eq!(
        preferences["usage_statistics"],
        json!({"prompt_shown": true, "enabled": true, "disclosure_version": DISCLOSURE_VERSION, "installation_id": id})
    );
    assert_eq!(
        harness.ok(harness.client, "preferences.read", json!({}))["usage_statistics"],
        preferences["usage_statistics"]
    );
    assert_eq!(
        harness.ok(harness.client, "usage.read", json!({}))["effective"],
        json!(true)
    );
    // An ordinary client may revoke.
    let off = harness.ok(harness.client, "usage.set", disable("cli-off"));
    assert_eq!(off["state"]["effective"], json!(false));
    assert_eq!(off["state"]["reason"], json!("choice"));
    assert_eq!(off["state"]["installation_id"], json!(null));
    assert_eq!(harness.usage.gate().current(), None);
    assert_eq!(harness.announced("usage.set"), 2);
}

#[test]
fn usage_the_startup_notice_is_claimed_once_across_retries_and_relaunches() {
    let harness = Harness::start("claim");
    let claim = |harness: &Harness, request_id: &str| {
        harness.ok(
            harness.desktop,
            "usage.prompt.claim",
            json!({"mutation": mutation(request_id)}),
        )
    };
    let first = claim(&harness, "first");
    assert_eq!(first["show_prompt"], json!(true));
    assert_eq!(first["deduplicated"], json!(false));
    assert_eq!(first["state"]["prompt_shown"], json!(true));
    assert_eq!(first["state"]["enabled"], json!(false));
    let retried = claim(&harness, "first");
    assert_eq!(
        retried["show_prompt"],
        json!(false),
        "no instruction to show it again"
    );
    assert_eq!(retried["deduplicated"], json!(true));
    assert_eq!(claim(&harness, "second")["show_prompt"], json!(false));
    assert_eq!(harness.announced("usage.prompt.claim"), 1);
    let harness = harness.relaunch();
    assert_eq!(
        claim(&harness, "after-relaunch")["show_prompt"],
        json!(false)
    );
    assert_eq!(
        harness.ok(harness.client, "usage.read", json!({}))["prompt_shown"],
        json!(true)
    );
}

#[test]
fn usage_a_deduplicated_enable_answers_the_state_after_a_revocation() {
    let harness = Harness::start("retry");
    let on = harness.ok(harness.desktop, "usage.set", enable("enable"));
    assert_eq!(on["state"]["effective"], json!(true));
    harness.ok(harness.client, "usage.set", disable("disable"));
    let retried = harness.ok(harness.desktop, "usage.set", enable("enable"));
    assert_eq!(retried["deduplicated"], json!(true));
    assert_eq!(retried["state"]["effective"], json!(false));
    assert_eq!(retried["state"]["enabled"], json!(false));
    assert_eq!(harness.usage.gate().current(), None);
    // A relaunch with the opt-out saved stays off; a stored opt-in resumes with its ID. The owner
    // makes its launch read before it answers its first call.
    let harness = harness.relaunch();
    let read = harness.ok(harness.client, "usage.read", json!({}));
    assert_eq!(read["effective"], json!(false));
    assert_eq!(harness.usage.gate().current(), None);
    let on = harness.ok(harness.desktop, "usage.set", enable("enable-again"));
    let id = on["state"]["installation_id"].clone();
    let harness = harness.relaunch();
    let read = harness.ok(harness.client, "usage.read", json!({}));
    assert_eq!(read["effective"], json!(true));
    assert_eq!(read["installation_id"], id);
    assert!(harness.usage.gate().current().is_some());
}

#[test]
fn usage_a_headless_host_reports_the_inactive_destination_and_claims_nothing() {
    let dir = temp_dir("usage-owner-headless").canonicalize().unwrap();
    let harness = Harness::over(dir, None);
    let read = harness.ok(harness.client, "usage.read", json!({}));
    assert_eq!(read["effective"], json!(false));
    assert_eq!(read["reason"], json!("inactive-destination"));
    assert_eq!(read["destination"], json!(crate::usage::DESTINATION));
    assert_eq!(
        read["release"],
        json!(null),
        "a test build carries no revision"
    );
    assert_eq!(read["session_id"], json!(null));
    assert_eq!(read["limits"]["window_events"], json!(512));
    assert_eq!(
        read["diagnostics"],
        json!({
            "recorded": 0,
            "dropped": 0,
            "sent": 0,
            "attempts": 0,
            "accepted": 0,
            "delivery": {"state": "ready", "reason": null},
        })
    );
    let claim = harness.ok(
        harness.desktop,
        "usage.prompt.claim",
        json!({"mutation": mutation("claim")}),
    );
    assert_eq!(claim["show_prompt"], json!(false));
    assert_eq!(
        harness.refused(harness.desktop, "usage.set", enable("on")),
        "not-ready"
    );
    assert!(
        !harness.file().exists(),
        "an ineligible host writes nothing"
    );
}
