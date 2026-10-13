//! The consent runtime against isolated preference directories: what an absent, unusable or
//! other-disclosure record collects, the once-only claim across concurrent claimants, the
//! installation ID and image key lifecycle, off on a failed write, retried enables, and another
//! process's revocation caught by the stored-record confirmation.
use super::*;
use crate::{
    ErrorKind,
    preferences::PreferenceChange,
    usage::eligibility::{Eligibility, TestReceiver},
};
use luxforge_testbase::paths::temp_dir;
use std::{
    fs,
    sync::{Arc, Barrier},
};

/// A preference directory of its own, removed at the end.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        Self(temp_dir(&format!("usage-{name}")))
    }

    fn store(&self) -> PreferenceStore {
        PreferenceStore::new(Some(self.0.clone()))
    }

    fn reader(&self) -> ConsentReader {
        ConsentReader::new(Some(self.0.clone()))
    }

    fn file(&self) -> PathBuf {
        self.0.join("preferences.json")
    }

    fn bytes(&self) -> Option<Vec<u8>> {
        fs::read(self.file()).ok()
    }

    fn record(&self) -> Value {
        let file: Value = serde_json::from_slice(&fs::read(self.file()).unwrap()).unwrap();
        file[RECORD_KEY].clone()
    }

    fn write(&self, record: Value) {
        fs::write(
            self.file(),
            serde_json::to_vec(&json!({"format": 1, RECORD_KEY: record})).unwrap(),
        )
        .unwrap();
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An explicitly test-eligible runtime with a loopback receiver, as an isolated test host has.
fn eligible() -> Usage {
    Usage::new(Eligibility::test(
        TestReceiver::loopback("http://127.0.0.1:9/v1/usage/batches").unwrap(),
    ))
}

/// A fresh process's runtime after its launch read of `dir`.
fn launched(usage: Usage, dir: &Dir) -> Usage {
    usage.start(dir.store().read().as_ref());
    usage
}

fn enable(usage: &Usage, dir: &Dir, request_id: &str) -> Result<SetOutcome, Error> {
    usage.set(
        &dir.store(),
        ClientAuthority::Permissions,
        true,
        Some(DISCLOSURE_VERSION),
        request_id,
    )
}

fn disable(usage: &Usage, dir: &Dir, request_id: &str) -> Result<SetOutcome, Error> {
    usage.set(&dir.store(), ClientAuthority::Edit, false, None, request_id)
}

/// A valid enabled record's ID and key.
const ID: &str = "c9a10716-1b78-4cbe-8d1c-b8309e5e7247";
const KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn stored_key(dir: &Dir) -> String {
    dir.record()["image_key"].as_str().unwrap().to_owned()
}

fn stored_id(dir: &Dir) -> String {
    dir.record()["installation_id"].as_str().unwrap().to_owned()
}

#[test]
fn usage_an_absent_record_is_unasked_and_off_and_reading_writes_nothing() {
    let dir = Dir::new("absent");
    let usage = launched(eligible(), &dir);
    assert_eq!(usage.gate().current(), None);
    assert!(usage.active().is_none());
    let state = usage.read(&dir.store());
    assert_eq!(state.prompt_shown, Some(false));
    assert_eq!(state.enabled, Some(false));
    assert!(!state.effective);
    assert_eq!(state.reason, Some(Reason::Choice));
    assert_eq!(state.installation_id, None);
    assert_eq!(state.session_id, None);
    assert_eq!(state.disclosure_version, DISCLOSURE_VERSION);
    assert_eq!(state.destination, "http://127.0.0.1:9/v1/usage/batches");
    assert_eq!(state.limits, Limits::CONTRACT);
    assert_eq!(state.diagnostics, Diagnostics::default());
    assert_eq!(
        dir.reader().stored().unwrap(),
        StoredConsent {
            enabled: false,
            installation_id: None
        }
    );
    assert!(!dir.file().exists(), "reading creates nothing");
}

#[test]
fn usage_an_ineligible_launch_collects_nothing_claims_nothing_and_cannot_enable() {
    let dir = Dir::new("ineligible");
    enable(&eligible(), &dir, "first").unwrap();
    let stored = dir.bytes().unwrap();
    let usage = launched(Usage::headless(), &dir);
    assert_eq!(usage.gate().current(), None, "stored consent is not enough");
    let state = usage.read(&dir.store());
    assert_eq!(state.enabled, Some(true));
    assert!(!state.effective);
    assert_eq!(state.reason, Some(Reason::InactiveDestination));
    assert_eq!(state.destination, super::super::DESTINATION);
    assert!(!usage.claim(&dir.store()).unwrap());
    let refused = enable(&usage, &dir, "again").unwrap_err();
    assert_eq!(refused.kind, ErrorKind::NotReady);
    assert!(
        refused.detail.contains("inactive-destination"),
        "{}",
        refused.detail
    );
    assert_eq!(dir.bytes().unwrap(), stored, "nothing was written");
    // A revocation is always allowed.
    disable(&usage, &dir, "off").unwrap();
    assert_eq!(dir.record()["enabled"], json!(false));

    // An unclaimed configuration stays unclaimed in an ineligible launch.
    let fresh = Dir::new("ineligible-unclaimed");
    assert!(!Usage::headless().claim(&fresh.store()).unwrap());
    assert!(!fresh.file().exists());
}

#[test]
fn usage_an_unusable_record_is_kept_unchanged_and_collects_nothing() {
    let enabled = |id: Value, key: Value| {
        json!({"prompt_shown": true, "enabled": true, "disclosure_version": 1,
               "installation_id": id, "image_key": key})
    };
    for record in [
        enabled(json!("C9A10716-1B78-4CBE-8D1C-B8309E5E7247"), json!(KEY)),
        enabled(json!("c9a107161b784cbe8d1cb8309e5e7247"), json!(KEY)),
        enabled(json!("c9a10716-1b78-1cbe-8d1c-b8309e5e7247"), json!(KEY)),
        enabled(json!(ID), json!(KEY.to_uppercase())),
        enabled(json!(ID), json!(&KEY[..62])),
        enabled(json!(ID), json!(null)),
        json!({"prompt_shown": true, "enabled": true, "disclosure_version": 1}),
        json!({"prompt_shown": true, "enabled": false, "disclosure_version": 1, "installation_id": ID}),
        json!({"prompt_shown": true, "enabled": false, "disclosure_version": 0}),
        json!({"prompt_shown": true, "enabled": false, "disclosure_version": 1, "extra": 1}),
        json!({"prompt_shown": "yes", "enabled": false, "disclosure_version": 1}),
        json!({"enabled": false, "disclosure_version": 1}),
        json!(true),
    ] {
        let dir = Dir::new("unusable");
        dir.write(record.clone());
        let bytes = dir.bytes().unwrap();
        let usage = launched(eligible(), &dir);
        assert_eq!(usage.gate().current(), None, "{record}");
        let state = usage.read(&dir.store());
        assert_eq!(
            (state.prompt_shown, state.enabled),
            (None, None),
            "{record}"
        );
        assert_eq!(state.reason, Some(Reason::UnreadableConfiguration));
        assert_eq!(
            usage.claim(&dir.store()).unwrap_err().kind,
            ErrorKind::Incompatible
        );
        assert_eq!(
            enable(&usage, &dir, "on").unwrap_err().kind,
            ErrorKind::Incompatible
        );
        let off = disable(&usage, &dir, "off").unwrap_err();
        assert_eq!(off.kind, ErrorKind::Incompatible);
        assert_eq!(off.data.unwrap()["state"]["enabled"], json!(null));
        assert!(dir.reader().stored().is_err());
        assert_eq!(usage.gate().current(), None);
        assert_eq!(dir.bytes().unwrap(), bytes, "{record}: kept unchanged");
    }
}

#[test]
fn usage_consent_to_another_disclosure_collects_nothing_and_never_prompts_again() {
    let dir = Dir::new("changed-disclosure");
    dir.write(json!({"prompt_shown": true, "enabled": true,
                     "disclosure_version": DISCLOSURE_VERSION + 1,
                     "installation_id": ID, "image_key": KEY}));
    let usage = launched(eligible(), &dir);
    assert_eq!(usage.gate().current(), None);
    let state = usage.read(&dir.store());
    assert_eq!(state.prompt_shown, Some(true));
    assert_eq!(state.reason, Some(Reason::ChangedDisclosure));
    assert!(!dir.reader().stored().unwrap().enabled);
    assert!(!usage.claim(&dir.store()).unwrap(), "no automatic notice");
    // Agreeing to the current disclosure is new consent, with a new identity.
    assert!(enable(&usage, &dir, "agree").unwrap().changed);
    assert_ne!(stored_id(&dir), ID);
    assert_ne!(stored_key(&dir), KEY);
    assert_eq!(
        dir.record()["disclosure_version"],
        json!(DISCLOSURE_VERSION)
    );
    assert!(usage.read(&dir.store()).effective);
}

/// Each claimant has its own runtime and store over one configuration, as separate processes
/// would; the preferences lock lets exactly one write the claim.
#[test]
fn usage_two_concurrent_claimants_over_one_configuration_make_one_claim() {
    for round in 0..8 {
        let dir = Dir::new(&format!("claim-{round}"));
        let claimants = 2 + round % 3;
        let barrier = Arc::new(Barrier::new(claimants));
        let threads: Vec<_> = (0..claimants)
            .map(|_| {
                let root = dir.0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let usage = eligible();
                    let store = PreferenceStore::new(Some(root));
                    barrier.wait();
                    usage.claim(&store).unwrap()
                })
            })
            .collect();
        let claims = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|claimed| *claimed)
            .count();
        assert_eq!(claims, 1, "round {round}: {claimants} claimants");
        assert_eq!(
            dir.record(),
            json!({"prompt_shown": true, "enabled": false, "disclosure_version": DISCLOSURE_VERSION})
        );
        // Relaunch, dismissal bookkeeping and a later claim never ask again.
        assert!(!launched(eligible(), &dir).claim(&dir.store()).unwrap());
    }
}

#[test]
fn usage_a_saved_choice_answers_the_notice() {
    for enabled in [true, false] {
        let dir = Dir::new("answered");
        let usage = eligible();
        if enabled {
            enable(&usage, &dir, "choice").unwrap();
        } else {
            disable(&usage, &dir, "choice").unwrap();
        }
        assert_eq!(dir.record()["prompt_shown"], json!(true));
        assert!(!usage.claim(&dir.store()).unwrap());
    }
}

#[test]
fn usage_the_id_and_image_key_exist_only_while_a_saved_opt_in_does() {
    let dir = Dir::new("lifecycle");
    let usage = launched(eligible(), &dir);
    // An untrusted or incomplete enable is refused and writes nothing.
    let untrusted = usage
        .set(
            &dir.store(),
            ClientAuthority::Edit,
            true,
            Some(DISCLOSURE_VERSION),
            "untrusted",
        )
        .unwrap_err();
    assert_eq!(untrusted.kind, ErrorKind::Forbidden);
    for version in [None, Some(DISCLOSURE_VERSION + 1)] {
        let refused = usage
            .set(
                &dir.store(),
                ClientAuthority::Permissions,
                true,
                version,
                "incomplete",
            )
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Validation, "{version:?}");
    }
    let refused = usage
        .set(
            &dir.store(),
            ClientAuthority::Edit,
            false,
            Some(DISCLOSURE_VERSION),
            "off-with-version",
        )
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Validation);
    assert!(!dir.file().exists());
    assert_eq!(usage.gate().current(), None);

    // The saved opt-in creates both, and only then does collection start.
    let outcome = enable(&usage, &dir, "opt-in").unwrap();
    assert!(outcome.changed && outcome.state.effective);
    assert_eq!(outcome.state.reason, None);
    let (id, key) = (stored_id(&dir), stored_key(&dir));
    assert!(InstallationId::parse(&id).is_some());
    assert_eq!(key.len(), 64);
    assert!(
        key.bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    );
    let active = usage.active().unwrap();
    assert_eq!(active.installation_id.to_string(), id);
    assert_eq!(
        active.installation_id.report_id(),
        crate::usage::ReportId::parse(&id).unwrap()
    );
    assert_eq!(usage.release(), None, "a test build carries no revision");
    assert_eq!(Some(active.generation), usage.gate().current());
    assert_eq!(outcome.state.installation_id.unwrap().to_string(), id);
    assert_eq!(
        dir.reader().stored().unwrap(),
        StoredConsent {
            enabled: true,
            installation_id: Some(active.installation_id)
        }
    );
    // Enabling again is a no-op that keeps both.
    let again = enable(&usage, &dir, "opt-in-again").unwrap();
    assert!(!again.changed && again.state.effective);
    assert_eq!(
        (stored_id(&dir), stored_key(&dir)),
        (id.clone(), key.clone())
    );
    assert_eq!(usage.active().unwrap().generation, active.generation);

    // A relaunch, or another catalog's owner, keeps the same identity.
    let relaunched = launched(eligible(), &dir);
    let resumed = relaunched.active().unwrap();
    assert_eq!(resumed.installation_id.to_string(), id);
    assert_eq!(resumed.image_key.as_bytes(), active.image_key.as_bytes());
    usage.start(dir.store().read().as_ref());
    assert_eq!(usage.active().unwrap().generation, active.generation);

    // A saved opt-out removes both and keeps the notice answered.
    let off = disable(&usage, &dir, "opt-out").unwrap();
    assert!(off.changed && !off.state.effective);
    assert_eq!(off.state.reason, Some(Reason::Choice));
    assert_eq!(off.state.installation_id, None);
    assert_eq!(
        dir.record(),
        json!({"prompt_shown": true, "enabled": false, "disclosure_version": DISCLOSURE_VERSION})
    );
    assert_eq!(usage.gate().current(), None);
    assert!(!usage.gate().is_current(active.generation));
    assert!(!disable(&usage, &dir, "opt-out-again").unwrap().changed);

    // A later opt-in is a fresh identity in a new generation.
    enable(&usage, &dir, "opt-in-later").unwrap();
    assert_ne!(stored_id(&dir), id);
    assert_ne!(stored_key(&dir), key);
    assert_ne!(usage.active().unwrap().generation, active.generation);
}

#[test]
fn usage_off_is_immediate_even_when_the_write_fails() {
    let dir = Dir::new("off-fails");
    let usage = eligible();
    enable(&usage, &dir, "on").unwrap();
    let generation = usage.gate().current().unwrap();
    let stored = dir.bytes().unwrap();
    // The staged copy's path is taken by a directory, so the write cannot happen.
    fs::create_dir(dir.0.join("preferences.json.tmp")).unwrap();
    let failed = disable(&usage, &dir, "off").unwrap_err();
    assert_eq!(usage.gate().current(), None, "off before the write");
    assert!(!usage.gate().is_current(generation));
    assert!(usage.active().is_none());
    assert!(
        failed
            .detail
            .contains("the stored choice and installation ID were not changed"),
        "{}",
        failed.detail
    );
    let state = &failed.data.as_ref().unwrap()["state"];
    assert_eq!(state["enabled"], json!(true), "the stored choice as it is");
    assert_eq!(state["effective"], json!(false));
    assert_eq!(state["reason"], json!("choice"));
    assert_eq!(dir.bytes().unwrap(), stored);
    // No readback turns it on again: not a read, not another owner's launch read.
    usage.start(dir.store().read().as_ref());
    assert_eq!(usage.gate().current(), None);
    assert!(!usage.read(&dir.store()).effective);
    // Once the write can happen, the opt-out is saved.
    fs::remove_dir(dir.0.join("preferences.json.tmp")).unwrap();
    disable(&usage, &dir, "off-retry").unwrap();
    assert_eq!(dir.record()["enabled"], json!(false));
}

#[test]
fn usage_a_retried_enable_never_reapplies_after_an_opt_out() {
    let dir = Dir::new("retry");
    let usage = eligible();
    enable(&usage, &dir, "enable").unwrap();
    disable(&usage, &dir, "disable").unwrap();
    let retried = enable(&usage, &dir, "enable").unwrap();
    assert!(!retried.changed);
    assert!(!retried.state.effective);
    assert_eq!(retried.state.enabled, Some(false));
    assert_eq!(usage.gate().current(), None);
    assert_eq!(dir.record()["enabled"], json!(false));
    assert_eq!(
        disable(&usage, &dir, "enable").unwrap_err().kind,
        ErrorKind::Conflict,
        "the same request with other input"
    );
}

#[test]
fn usage_another_process_revocation_ends_the_generation_at_the_next_confirmation() {
    let dir = Dir::new("cross-process");
    let this = eligible();
    enable(&this, &dir, "on").unwrap();
    let generation = this.gate().current().unwrap();
    let reader = dir.reader();
    assert!(this.confirm_stored(&reader, generation));
    assert!(
        this.gate().is_current(generation),
        "confirming changes nothing"
    );

    // Another process opts out: the next confirmation ends this generation.
    let other = launched(eligible(), &dir);
    disable(&other, &dir, "other-off").unwrap();
    assert!(!this.confirm_stored(&reader, generation));
    assert_eq!(this.gate().current(), None);
    assert!(this.active().is_none());
    assert!(!this.confirm_stored(&reader, generation));

    // Another process opts out and back in: a different stored ID ends it too.
    enable(&this, &dir, "on-again").unwrap();
    let second = this.gate().current().unwrap();
    disable(&other, &dir, "other-off-again").unwrap();
    enable(&other, &dir, "other-on").unwrap();
    assert!(dir.reader().stored().unwrap().enabled);
    assert!(!this.confirm_stored(&reader, second));
    assert_eq!(this.gate().current(), None);
    assert_eq!(this.read(&dir.store()).reason, Some(Reason::Choice));

    // A stale generation is never confirmed, and an unreadable file ends the current one.
    enable(&this, &dir, "on-third").unwrap();
    let third = this.gate().current().unwrap();
    assert!(!this.confirm_stored(&reader, second));
    assert!(this.gate().is_current(third));
    fs::write(dir.file(), b"not json").unwrap();
    assert!(!this.confirm_stored(&reader, third));
    assert_eq!(this.gate().current(), None);
}

#[test]
fn usage_generic_preference_writes_and_resets_keep_the_record() {
    let dir = Dir::new("generic");
    enable(&eligible(), &dir, "on").unwrap();
    let record = dir.record();
    let store = dir.store();
    store
        .set(PreferenceChange {
            performance_expanded: Some(Some(false)),
            interface_size: Some(Some(125)),
            ..PreferenceChange::default()
        })
        .unwrap();
    store
        .set(PreferenceChange {
            performance_expanded: Some(None),
            auto_collapse_history: Some(None),
            auto_lens_profile: Some(None),
            raw_look: Some(None),
            mask_overlay_colour: Some(None),
            canvas_background: Some(None),
            interface_size: Some(None),
            catalog: Some(None),
            workspace: Some(None),
            brush: Some(None),
            window: Some(None),
            export_last: Some(None),
            hdr_merge: Some(None),
            panorama_merge: Some(None),
            theme: Some(None),
            update_checks: Some(None),
            update_install_automatically: Some(None),
            update_ask_to_move: Some(None),
        })
        .unwrap();
    store.set_flag("developer", Some(json!(true))).unwrap();
    store.set_flag("developer", None).unwrap();
    assert_eq!(dir.record(), record);
}

#[test]
fn usage_the_image_key_never_appears_in_a_read_or_a_debug_rendering() {
    let dir = Dir::new("key-private");
    let usage = eligible();
    enable(&usage, &dir, "on").unwrap();
    let (id, key) = (stored_id(&dir), stored_key(&dir));
    let preferences = dir.store().read().unwrap();
    let state = serde_json::to_string(&usage.read(&dir.store())).unwrap();
    let public = public_record(preferences.usage_statistics.as_ref()).to_string();
    assert!(state.contains(&id) && public.contains(&id));
    for read in [&state, &public] {
        assert!(
            !read.contains(&key) && !read.contains("image_key"),
            "{read}"
        );
    }
    for debug in [
        format!("{:?}", usage.active().unwrap()),
        format!("{usage:?}"),
        format!("{preferences:?}"),
        format!("{:?}", dir.reader().stored().unwrap()),
    ] {
        assert!(!debug.contains(&key) && !debug.contains(&id), "{debug}");
    }
}

#[test]
fn usage_a_record_round_trips_in_its_one_shape() {
    let dir = Dir::new("shape");
    let record = json!({"prompt_shown": true, "enabled": true, "disclosure_version": 1,
                        "installation_id": ID, "image_key": KEY});
    dir.write(record.clone());
    let preferences = dir.store().read().unwrap();
    let stored = preferences.usage_statistics.as_ref().unwrap();
    assert_eq!(
        stored.image_key.as_ref().unwrap().as_bytes()[..2],
        [0x00, 0x11]
    );
    assert_eq!(serde_json::to_value(stored).unwrap(), record);
}

#[test]
fn usage_hosted_developer_eligibility_still_needs_explicit_consent() {
    let dir = Dir::new("developer-consent");
    let usage = launched(
        Usage::new(Eligibility::Developer {
            release: super::super::TEST_RELEASE.into(),
        }),
        &dir,
    );
    let state = usage.read(&dir.store());
    assert_eq!(state.destination, super::super::TEST_DESTINATION);
    assert!(!state.effective);
    assert!(usage.active().is_none());
    assert!(usage.claim(&dir.store()).unwrap());
    assert!(
        usage.active().is_none(),
        "claiming never enables collection"
    );
    enable(&usage, &dir, "developer-on").unwrap();
    assert!(usage.read(&dir.store()).effective);
    disable(&usage, &dir, "developer-off").unwrap();
    assert!(usage.active().is_none());
}
