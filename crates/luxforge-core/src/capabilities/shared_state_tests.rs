//! Shared-root transfer and visible publication outcomes through independent catalog owners.
use super::*;
use crate::atomic_file::sync_faults;

fn second_owner(fixture: &Fixture) -> Owner {
    Owner::start(
        &fixture.root.join("second.sqlite"),
        fixture.registry(),
        fixture.host(),
    )
}

#[test]
fn shared_root_same_resource_reuses_the_first_install_without_another_download() {
    let (transport, gate) = stalling();
    let fixture = Fixture::new("shared-install", &transport);
    let first = fixture.start();
    let second = second_owner(&fixture);
    first.grant_download(&fixture, "palette", "palette");
    let a = first.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    first.until(&a["job_id"], |job| {
        job["progress"]["fraction"]
            .as_f64()
            .is_some_and(|fraction| fraction > 0.0)
    });
    let b = second.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    second.until(&b["job_id"], |job| job["status"] == "running");
    gate.open();
    assert_eq!(first.finished(&a["job_id"])["status"], "ready");
    assert_eq!(second.finished(&b["job_id"])["status"], "ready");
    assert_eq!(transport.sends(), 1);
    assert_eq!(
        fs::read(fixture.version_dir("palette").join("palette")).unwrap(),
        PALETTE
    );
    assert!(
        fixture
            .version_dir("palette")
            .join(INSTALLED_FILE)
            .is_file()
    );
}

#[test]
fn shared_root_quota_is_rechecked_after_the_first_owner_publishes() {
    let (transport, gate) = stalling();
    let mut fixture = Fixture::new("shared-quota", &transport);
    fixture.quota = PALETTE.len().max(SWATCH.len()) as u64;
    let first = fixture.start();
    let second = second_owner(&fixture);
    first.grant_download(&fixture, "palette", "palette");
    second.grant_download(&fixture, "swatches", "swatch");
    let a = first.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    first.until(&a["job_id"], |job| {
        job["progress"]["fraction"]
            .as_f64()
            .is_some_and(|fraction| fraction > 0.0)
    });
    let b = second.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "swatch"}),
    );
    second.until(&b["job_id"], |job| job["status"] == "running");
    gate.open();
    assert_eq!(first.finished(&a["job_id"])["status"], "ready");
    let rejected = second.finished(&b["job_id"]);
    assert_eq!(rejected["error"]["code"], "resource-limit");
    assert_eq!(transport.sends(), 1);
    assert!(!fixture.version_dir("swatch").exists());
}

#[test]
fn shared_root_lock_waiter_cancels_while_the_first_transfer_remains_held() {
    let (transport, gate) = stalling();
    let fixture = Fixture::new("shared-cancel", &transport);
    let first = fixture.start();
    let second = second_owner(&fixture);
    first.grant_download(&fixture, "palette", "palette");
    let a = first.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    first.until(&a["job_id"], |job| {
        job["progress"]["fraction"]
            .as_f64()
            .is_some_and(|fraction| fraction > 0.0)
    });
    let b = second.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    second.until(&b["job_id"], |job| job["status"] == "running");
    second.ok(JOB_CANCEL, json!({"job_id": b["job_id"]}));
    assert_eq!(second.finished(&b["job_id"])["status"], "cancelled");
    assert_eq!(transport.sends(), 1);
    assert_eq!(first.job(&a["job_id"])["status"], "running");
    gate.open();
    assert_eq!(first.finished(&a["job_id"])["status"], "ready");
}

#[test]
fn installed_marker_sync_failure_keeps_files_and_emits_an_explicit_job_error() {
    let transport = serving();
    let fixture = Fixture::new("marker-visible", &transport);
    let owner = fixture.start();
    owner.grant_download(&fixture, "palette", "palette");
    sync_faults::inject(&fixture.version_dir("palette"));
    let queued = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    let failed = owner.finished(&queued["job_id"]);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error"]["data"]["published"], true);
    assert_eq!(failed["error"]["data"]["durable"], false);
    assert!(
        failed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("visible")
    );
    assert_eq!(owner.status(MODULE)["resources"][0]["state"], "installed");
    assert_eq!(
        fs::read(fixture.version_dir("palette").join("palette")).unwrap(),
        PALETTE
    );
    assert!(owner.methods().contains(&INSTALL.to_owned()));
    let retried = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    assert_eq!(retried["state"], "installed");
    assert_eq!(transport.sends(), 1);
}

#[test]
fn visible_revoke_sync_failure_cancels_the_running_job_and_retry_enforces_revocation() {
    let (transport, gate) = stalling();
    let fixture = Fixture::new("revoke-visible", &transport);
    let owner = fixture.start();
    let grant = owner.grant_download(&fixture, "palette", "palette");
    let queued = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    owner.until(&queued["job_id"], |job| {
        job["progress"]["fraction"]
            .as_f64()
            .is_some_and(|fraction| fraction > 0.0)
    });
    sync_faults::inject(&fixture.config());
    let error = owner.fail(REVOKE, json!({"grant_id": grant["grant"]["grant_id"]}));
    assert_eq!(error.data.as_ref().unwrap()["published"], true);
    assert_eq!(error.data.as_ref().unwrap()["durable"], false);
    let grants = owner.ok(LIST, json!({"module_id": MODULE}));
    assert!(!grants["grants"][0]["revoked"].is_null());
    assert!(owner.methods().contains(&REVOKE.to_owned()));
    gate.open();
    assert_eq!(owner.finished(&queued["job_id"])["status"], "cancelled");
    let retry = owner.ok(REVOKE, json!({"grant_id": grant["grant"]["grant_id"]}));
    assert_eq!(retry["outcome"], "no-op");
}

#[test]
fn visible_settings_sync_failure_reports_the_new_revision_and_event() {
    let fixture = Fixture::new("settings-visible", &serving());
    let owner = fixture.start();
    owner.set(json!({"label": "before"}));
    let mutation = owner.mutation(MODULE);
    sync_faults::inject(&fixture.config());
    let error = owner.fail(
        SET,
        json!({"module_id": MODULE, "values": {"label": "after"}, "mutation": mutation}),
    );
    assert_eq!(error.data.as_ref().unwrap()["published"], true);
    assert_eq!(error.data.as_ref().unwrap()["durable"], false);
    let read = owner.ok(READ, json!({"module_id": MODULE}));
    assert_eq!(read["fields"]["label"]["value"], "after");
    assert_eq!(read["revision"], 2);
    assert_eq!(
        owner
            .methods()
            .iter()
            .filter(|method| method.as_str() == SET)
            .count(),
        2
    );
}

#[test]
fn shared_root_remove_waits_for_install_then_removes_the_whole_resource() {
    let (transport, gate) = stalling();
    let fixture = Fixture::new("shared-remove", &transport);
    let first = fixture.start();
    let second = second_owner(&fixture);
    first.grant_download(&fixture, "palette", "palette");
    let installed = first.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    gate.wait_reached(1, "first resource fetch");
    let removed = second.ok(
        REMOVE,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    second.until(&removed["job_id"], |job| job["status"] == "running");
    assert!(!fixture.version_dir("palette").exists());
    gate.open();
    assert_eq!(first.finished(&installed["job_id"])["status"], "ready");
    assert_eq!(
        second.finished(&removed["job_id"])["result"]["removed"],
        true
    );
    fixture.assert_clean("palette", "serialized removal");
}

#[test]
fn visible_export_preset_sync_errors_reconcile_all_mutations_and_events() {
    let fixture = Fixture::new("export-presets-visible", &serving());
    let preferences = fixture.root.join("preferences");
    let mut config = fixture.host();
    config.preferences_dir = Some(preferences.clone());
    let owner = Owner::start(&fixture.catalog(), fixture.registry(), config);
    let settings = crate::export::presets::builtin()[0].settings.clone();
    sync_faults::inject(&preferences);
    let failure = owner.fail(
        "export.presets.create",
        json!({"name": "Visible", "settings": settings}),
    );
    assert_eq!(failure.data.as_ref().unwrap()["published"], true);
    let listing = owner.ok("export.presets.list", json!({}));
    let preset = listing["presets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|preset| preset["name"] == "Visible")
        .unwrap();
    let id = preset["preset_id"].clone();
    sync_faults::inject(&preferences);
    let failure = owner.fail(
        "export.presets.update",
        json!({"preset_id": id, "name": "Renamed"}),
    );
    assert_eq!(failure.data.as_ref().unwrap()["durable"], false);
    let listing = owner.ok("export.presets.list", json!({}));
    assert!(
        listing["presets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|preset| preset["name"] == "Renamed")
    );
    sync_faults::inject(&preferences);
    let failure = owner.fail("export.presets.delete", json!({"preset_id": id}));
    assert_eq!(failure.data.as_ref().unwrap()["published"], true);
    let listing = owner.ok("export.presets.list", json!({}));
    assert!(
        !listing["presets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|preset| preset["preset_id"] == id)
    );
    let methods = owner.methods();
    assert!(methods.contains(&"export.presets.create".into()));
    assert!(methods.contains(&"export.presets.update".into()));
    assert!(methods.contains(&"export.presets.delete".into()));
}

#[test]
fn removal_failure_after_marker_deletion_reports_visible_uninstall_and_emits_its_event() {
    let fixture = Fixture::new("remove-visible-failure", &serving());
    let owner = fixture.start();
    owner.install_downloaded(&fixture, "palette");
    faults::inject_remove(&fixture.resources(), io::ErrorKind::PermissionDenied);
    let queued = owner.ok(
        REMOVE,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    let failed = owner.finished(&queued["job_id"]);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error"]["data"]["published"], true);
    assert_eq!(failed["error"]["data"]["installed"], false);
    assert!(!fixture.version_dir("palette").join(INSTALLED_FILE).exists());
    assert_eq!(
        fs::read(fixture.version_dir("palette").join("palette")).unwrap(),
        PALETTE
    );
    assert_eq!(
        owner.status(MODULE)["resources"][0]["state"],
        "not-installed"
    );
    assert!(owner.methods().contains(&REMOVE.to_owned()));
    let retry = owner.ok(
        REMOVE,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    assert_eq!(owner.finished(&retry["job_id"])["status"], "ready");
    fixture.assert_clean("palette", "cleanup retry");
}
