use super::*;
use crate::release_candidate::{fixture_record, release_record};
use std::ffi::OsString;
use std::path::PathBuf;

const LINUX: &str = TARGETS[1].1;

fn write_json(path: &Path, value: &Value) {
    fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(value).unwrap()),
    )
    .unwrap();
}

fn write_sums(dir: &Path) {
    let mut sums = String::new();
    for path in crate::files(dir).unwrap() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name != SUMS {
            sums.push_str(&format!("{}  {name}\n", hash(&path).unwrap()));
        }
    }
    fs::write(dir.join(SUMS), sums).unwrap();
}

struct Packet {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    dir: PathBuf,
}

impl Packet {
    fn verdict(&self) -> Verdict {
        verify(&self.dir)
    }

    fn release(&self) -> Value {
        read_json(&self.dir.join(RELEASE)).unwrap()
    }

    /// Changes `release.json` and recomputes `SHA256SUMS`: a consistent packet that says something
    /// else, as a hand edit would leave it.
    fn rewrite(&self, change: impl FnOnce(&mut Value)) {
        let mut release = self.release();
        change(&mut release);
        write_json(&self.dir.join(RELEASE), &release);
        write_sums(&self.dir);
    }

    /// Changes one platform's build record in `release.json` and its copy beside it.
    fn rewrite_target(&self, platform: &str, change: impl Fn(&mut Value)) {
        let index = TARGETS.iter().position(|(_, p)| *p == platform).unwrap();
        let copy = self.dir.join(format!("{platform}.json"));
        let mut record = read_json(&copy).unwrap();
        change(&mut record);
        write_json(&copy, &record);
        self.rewrite(|release| release["targets"][index] = record);
    }

    fn record(&self, gate: &str, result: Value) -> Result {
        record_gate(&self.dir, &GateRef::parse(gate).unwrap(), result)
    }

    /// Every reason, one per line.
    fn reasons(&self) -> String {
        self.verdict().reasons().join("\n")
    }
}

/// The fixture build's executable hashes: macOS's final signed ones differ from its before-signing
/// ones; Linux's built hashes are final.
fn hashes(platform: &str, before_signing: bool) -> Value {
    let offset = if platform == MACOS && !before_signing {
        16
    } else {
        0
    };
    EXECUTABLES
        .iter()
        .enumerate()
        .map(|(i, name)| ((*name).to_owned(), json!(format!("{:064x}", i + offset))))
        .collect::<Map<_, _>>()
        .into()
}

fn distribution_pass() -> Value {
    json!({"status": "passed", "evidence": "distribution-review/2026-10-20.md", "scope": "Both targets' Rust, native, font, theme and data components", "disposition": "distribution"})
}

fn qualification_pass(platform: &str, name: &str) -> Value {
    let mut result = json!({
        "status": "passed",
        "evidence": format!("{platform}/{name}/summary.json"),
        "scope": "The candidate's three executables on the native host",
        "executable_identity": "final",
        "executables": hashes(platform, false),
    });
    if platform == MACOS {
        result["bundle"] = json!(if DISTRIBUTED_GATES.contains(&name) {
            "distributed"
        } else {
            "background-test"
        });
    }
    result
}

/// A packet as `release-assemble` writes it: both targets, macOS signed as `signing`, every gate
/// not run, and the source archive when `source` is set.
fn candidate(signing: &str, source: bool) -> Packet {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let dir = root.join("packet");
    fs::create_dir_all(&dir).unwrap();
    let tag = ReleaseTag::parse("v0.1").unwrap();
    let records: Vec<Value> = TARGETS
        .iter()
        .map(|&(target, platform)| {
            let stage = root.join("stage").join(platform);
            let signing = if platform == MACOS {
                signing
            } else {
                NOT_APPLICABLE
            };
            let record = fixture_record(&stage, &tag, target, signing);
            for name in asset_names(&tag, target).unwrap() {
                fs::copy(stage.join(&name), dir.join(&name)).unwrap();
            }
            write_json(&dir.join(format!("{platform}.json")), &record);
            record
        })
        .collect();
    let mut release = release_record(&tag, &records);
    if source {
        let archive = dir.join(source_archive_name(&tag));
        fs::write(&archive, b"complete corresponding source").unwrap();
        release["source_archive"] = archive_record(&archive).unwrap();
    }
    write_json(&dir.join(RELEASE), &release);
    fs::write(dir.join(NOTES), "Luxforge 0.1.0\n").unwrap();
    write_sums(&dir);
    Packet {
        _tmp: tmp,
        root,
        dir,
    }
}

/// A notarized two-platform packet with its source archive and every gate passed.
fn ready() -> Packet {
    let packet = candidate("notarized", true);
    packet.rewrite(|release| {
        for gate in GateRef::all() {
            *gate.locate_mut(release) = match gate.platform {
                None => distribution_pass(),
                Some(platform) => qualification_pass(platform, gate.name),
            };
        }
    });
    packet
}

fn args(items: &[&str]) -> Args {
    Args(items.iter().map(OsString::from).collect())
}

fn refused(packet: &Packet, gate: &str, result: Value) -> String {
    packet
        .record(gate, result)
        .expect_err("the gate result was recorded")
        .to_string()
}

#[test]
fn a_qualified_two_platform_packet_is_ready_and_summarized_without_private_data() {
    let packet = ready();
    let verdict = packet.verdict();
    assert!(verdict.ready(), "{:?}", verdict.reasons());
    // The summary states readiness; release.json stays a build candidate.
    let public = summary(&verdict).unwrap();
    assert_eq!(public["release_ready"], true);
    assert_eq!(public["status"], "release-ready");
    assert_eq!(packet.release()["release_ready"], false);
    assert_eq!(packet.release()["status"], "build-candidate");
    assert_eq!(
        public["release_json_sha256"],
        hash(&packet.dir.join(RELEASE)).unwrap()
    );
    assert_eq!(public["targets"][0]["assets"].as_array().unwrap().len(), 2);
    assert_eq!(public["targets"][1]["signing"]["status"], NOT_APPLICABLE);
    // Evidence references stay private; an https link is kept.
    let gate = &public["qualification"][MACOS]["clean_installation"];
    assert_eq!(gate["evidence"], "private");
    assert_eq!(gate["bundle"], "distributed");
    let link = "https://github.com/luxforge/luxforge/issues/1";
    packet
        .record(
            "distribution.advisory_disposition",
            json!({"status": "passed", "evidence": link, "scope": "Fresh audit of the locked graph", "disposition": "distribution"}),
        )
        .unwrap();
    let public = summary(&packet.verdict()).unwrap();
    assert_eq!(
        public["distribution"]["advisory_disposition"]["evidence"],
        link
    );
    assert!(!public.to_string().contains("summary.json"));
    // The command writes a new summary outside the packet and succeeds only when ready.
    let out = packet.root.join("summary.json");
    let path = out.to_str().unwrap();
    let packet_dir = packet.dir.to_str().unwrap();
    run_verify(&mut args(&["--packet", packet_dir, "--summary", path])).unwrap();
    assert_eq!(read_json(&out).unwrap()["release_ready"], true);
    assert!(run_verify(&mut args(&["--packet", packet_dir, "--summary", path])).is_err());
    let inside = packet.dir.join("summary.json");
    let error = run_verify(&mut args(&[
        "--packet",
        packet_dir,
        "--summary",
        inside.to_str().unwrap(),
    ]))
    .unwrap_err()
    .to_string();
    assert!(error.contains("outside the packet"), "{error}");
    assert!(!inside.exists());
}

#[test]
fn a_published_build_candidate_stays_not_ready_and_says_why() {
    let packet = candidate("unsigned", false);
    let verdict = packet.verdict();
    assert!(!verdict.ready());
    assert!(verdict.integrity.is_empty(), "{:?}", verdict.integrity);
    let reasons = verdict.readiness.join("\n");
    for gate in GateRef::all() {
        assert!(
            reasons.contains(&format!("{} is not-run", gate.label())),
            "{reasons}"
        );
    }
    for needle in [
        "no complete source archive, luxforge-0.1.0-source.tar.gz",
        "macos-arm64 app signing is unsigned, not notarized",
        "macos-arm64 disk image signing is unsigned",
        "macos-arm64 records no final signed executable hashes",
    ] {
        assert!(reasons.contains(needle), "{needle}: {reasons}");
    }
    // An honest summary of a candidate can be written; it is not ready.
    let public = summary(&verdict).unwrap();
    assert_eq!(public["release_ready"], false);
    assert_eq!(public["status"], "not-ready");
    assert_eq!(
        public["reasons"].as_array().unwrap().len(),
        verdict.readiness.len()
    );
    let out = packet.root.join("candidate-summary.json");
    let error = run_verify(&mut args(&[
        "--packet",
        packet.dir.to_str().unwrap(),
        "--summary",
        out.to_str().unwrap(),
    ]))
    .unwrap_err()
    .to_string();
    assert!(error.contains("is not release-ready"), "{error}");
    assert_eq!(read_json(&out).unwrap()["release_ready"], false);
}

#[test]
fn rehearsals_and_dirty_trees_are_never_ready() {
    let packet = ready();
    packet.rewrite_target(LINUX, |record| {
        record["rehearsal"] = json!(true);
        record["working_tree_dirty"] = json!(true);
    });
    let verdict = packet.verdict();
    assert!(verdict.integrity.is_empty(), "{:?}", verdict.integrity);
    let reasons = verdict.readiness.join("\n");
    assert!(reasons.contains("linux-x86_64 is a labelled local rehearsal"));
    assert!(reasons.contains("linux-x86_64 was built from a dirty tree"));
}

#[test]
fn pending_failed_and_incomplete_gates_are_reported_by_name() {
    let packet = ready();
    let mut failed = qualification_pass(LINUX, "wayland_and_x11");
    failed["status"] = json!("failed");
    packet
        .record("qualification.linux-x86_64.wayland_and_x11", failed)
        .unwrap();
    let mut incomplete = qualification_pass(MACOS, "gpu_corpus_gate");
    incomplete["status"] = json!("incomplete");
    packet
        .record("qualification.macos-arm64.gpu_corpus_gate", incomplete)
        .unwrap();
    packet
        .record(
            "distribution.source_and_notices_review",
            json!({"status": "not-run"}),
        )
        .unwrap();
    let reasons = packet.reasons();
    for needle in [
        "qualification.linux-x86_64.wayland_and_x11 is failed",
        "qualification.macos-arm64.gpu_corpus_gate is incomplete",
        "distribution.source_and_notices_review is not-run",
    ] {
        assert!(reasons.contains(needle), "{needle}: {reasons}");
    }
    assert_eq!(packet.verdict().readiness.len(), 3, "{reasons}");
    // A result without evidence or scope is refused; a gate that has not run records nothing.
    let gate = "qualification.linux-x86_64.baseline_os";
    for (field, needle) in [
        ("evidence", "records no evidence"),
        ("scope", "records no scope"),
    ] {
        let mut bare = qualification_pass(LINUX, "baseline_os");
        bare.as_object_mut().unwrap().remove(field);
        let error = refused(&packet, gate, bare);
        assert!(error.contains(needle), "{error}");
    }
    let error = refused(
        &packet,
        gate,
        json!({"status": "not-run", "scope": "skipped"}),
    );
    assert!(error.contains("records no result"), "{error}");
    for status in ["skipped", "not-applicable", "passed-with-warnings"] {
        let mut bad = qualification_pass(LINUX, "baseline_os");
        bad["status"] = json!(status);
        refused(&packet, gate, bad);
    }
    assert!(GateRef::parse("qualification.linux-x86_64.minimum_os").is_err());
    assert!(GateRef::parse("qualification.windows-x86_64.baseline_os").is_err());
    assert!(GateRef::parse("distribution.dependency_audit").is_err());
}

#[test]
fn missing_tampered_or_unlisted_files_are_refused() {
    let tag = ReleaseTag::parse("v0.1").unwrap();
    let tarball = archive_name(&tag, TARGETS[1].0).unwrap();
    let image = disk_image_name(&tag, TARGETS[0].0).unwrap().unwrap();
    let zip = archive_name(&tag, TARGETS[0].0).unwrap();
    let source = source_archive_name(&tag);
    type Tamper<'a> = Box<dyn Fn(&Path) + 'a>;
    let cases: Vec<(&str, Tamper)> = vec![
        (
            "luxforge-0.1.0-linux-x86_64.tar.gz does not match its SHA256SUMS hash",
            Box::new(|dir| fs::write(dir.join(&tarball), b"tampered").unwrap()),
        ),
        (
            "Missing luxforge-0.1.0-macos-arm64.dmg, which SHA256SUMS lists",
            Box::new(|dir| fs::remove_file(dir.join(&image)).unwrap()),
        ),
        (
            "luxforge-0.1.0-macos-arm64.zip does not match macos-arm64's build record",
            Box::new(|dir| {
                // Tampered and relisted: the build record still catches it.
                fs::write(dir.join(&zip), b"another app").unwrap();
                write_sums(dir);
            }),
        ),
        (
            "release.json does not match its SHA256SUMS hash",
            Box::new(|dir| {
                let text = fs::read_to_string(dir.join(RELEASE)).unwrap();
                fs::write(dir.join(RELEASE), text.replacen("passed", "failed", 1)).unwrap();
            }),
        ),
        (
            "SHA256SUMS does not cover release.json",
            Box::new(|dir| {
                let sums = fs::read_to_string(dir.join(SUMS)).unwrap();
                let kept: String = sums
                    .lines()
                    .filter(|line| !line.ends_with("  release.json"))
                    .map(|line| format!("{line}\n"))
                    .collect();
                fs::write(dir.join(SUMS), kept).unwrap();
            }),
        ),
        (
            "Missing SHA256SUMS",
            Box::new(|dir| fs::remove_file(dir.join(SUMS)).unwrap()),
        ),
        (
            "notes.txt is in the packet but not in SHA256SUMS",
            Box::new(|dir| fs::write(dir.join("notes.txt"), b"extra").unwrap()),
        ),
        (
            "The packet holds a directory, evidence",
            Box::new(|dir| fs::create_dir(dir.join("evidence")).unwrap()),
        ),
        (
            "linux-x86_64.json differs from release.json's linux-x86_64 build record",
            Box::new(|dir| {
                let path = dir.join("linux-x86_64.json");
                let mut record = read_json(&path).unwrap();
                record["build_number"] = json!("2");
                write_json(&path, &record);
                write_sums(dir);
            }),
        ),
        (
            "luxforge-0.1.0-source.tar.gz does not match release.json's source archive",
            Box::new(|dir| {
                fs::write(dir.join(&source), b"partial source").unwrap();
                write_sums(dir);
            }),
        ),
        (
            "Missing luxforge-0.1.0-source.tar.gz",
            Box::new(|dir| {
                fs::remove_file(dir.join(&source)).unwrap();
                write_sums(dir);
            }),
        ),
        (
            "SHA256SUMS line 1 is not",
            Box::new(|dir| {
                let sums = fs::read_to_string(dir.join(SUMS)).unwrap();
                fs::write(
                    dir.join(SUMS),
                    format!("{}  ../escape\n{sums}", "0".repeat(64)),
                )
                .unwrap();
            }),
        ),
    ];
    for (needle, tamper) in cases {
        let packet = ready();
        tamper(&packet.dir);
        let verdict = packet.verdict();
        assert!(!verdict.ready(), "{needle}");
        let reasons = verdict.integrity.join("\n");
        assert!(reasons.contains(needle), "{needle}: {reasons}");
        // Nothing is recorded over an inconsistent packet, and the packet is left as it was.
        let before = fs::read(packet.dir.join(RELEASE)).ok();
        let error = refused(
            &packet,
            "distribution.advisory_disposition",
            distribution_pass(),
        );
        assert!(error.contains("inconsistent"), "{error}");
        assert_eq!(fs::read(packet.dir.join(RELEASE)).ok(), before);
    }
    // The update record is published beside the packet without being listed.
    let packet = ready();
    fs::write(packet.dir.join("luxforge-0.1.0-updates.json"), b"{}").unwrap();
    assert!(packet.verdict().ready(), "{}", packet.reasons());
}

#[test]
fn targets_share_one_tag_version_commit_lockfile_and_build_number() {
    for (field, value) in [
        ("revision", json!("c".repeat(40))),
        ("lock_sha256", json!("c".repeat(64))),
        ("build_number", json!("2")),
    ] {
        let packet = ready();
        packet.rewrite_target(LINUX, |record| record[field] = value.clone());
        let reasons = packet.verdict().integrity.join("\n");
        assert!(
            reasons.contains(&format!(
                "linux-x86_64's {field} differs from release.json's"
            )),
            "{reasons}"
        );
    }
    let packet = ready();
    packet.rewrite(|release| release["version"] = json!("0.1.1"));
    assert!(
        packet
            .reasons()
            .contains("release.json's version is not 0.1.0")
    );
    let packet = ready();
    packet.rewrite(|release| release["tag"] = json!("v0.2"));
    let reasons = packet.reasons();
    assert!(
        reasons.contains("release.json's version is not 0.2.0"),
        "{reasons}"
    );
    assert!(reasons.contains("macos-arm64's tag differs"), "{reasons}");
    // release.json never states readiness itself.
    let packet = ready();
    packet.rewrite(|release| {
        release["release_ready"] = json!(true);
        release["status"] = json!("release-ready");
    });
    assert!(
        packet
            .reasons()
            .contains("claims more than a build candidate")
    );
    let packet = ready();
    packet.rewrite(|release| release["targets"].as_array_mut().unwrap().reverse());
    assert!(packet.reasons().contains("not in shipping order"));
}

#[test]
fn a_one_platform_packet_is_never_ready() {
    for (index, platform) in [(1, LINUX), (0, MACOS)] {
        let packet = ready();
        let tag = ReleaseTag::parse("v0.1").unwrap();
        for name in asset_names(&tag, TARGETS[index].0).unwrap() {
            fs::remove_file(packet.dir.join(name)).unwrap();
        }
        fs::remove_file(packet.dir.join(format!("{platform}.json"))).unwrap();
        packet.rewrite(|release| {
            release["targets"].as_array_mut().unwrap().remove(index);
        });
        let verdict = packet.verdict();
        assert!(!verdict.ready());
        let reasons = verdict.reasons().join("\n");
        assert!(
            reasons.contains(&format!(
                "No {platform} build: a one-platform packet is never ready"
            )),
            "{reasons}"
        );
        // Its platform's evidence has nothing to bind to.
        assert!(
            reasons.contains(&format!("has no {platform} build record")),
            "{reasons}"
        );
    }
}

#[test]
fn linux_records_and_results_carry_no_apple_signing() {
    for signing in [
        json!({"status": "notarized", "submission_id": "app-submission"}),
        json!({"status": NOT_APPLICABLE, "disk_image": {"status": "notarized"}}),
        json!({"status": "unsigned"}),
    ] {
        let packet = ready();
        packet.rewrite_target(LINUX, |record| record["signing"] = signing.clone());
        let reasons = packet.reasons();
        assert!(
            reasons.contains("linux-x86_64 signing must be exactly not-applicable"),
            "{reasons}"
        );
    }
    let packet = ready();
    let gate = "qualification.linux-x86_64.clean_installation";
    let mut result = qualification_pass(LINUX, "clean_installation");
    result["bundle"] = json!("distributed");
    let error = refused(&packet, gate, result);
    assert!(
        error.contains("Apple bundle context, which does not apply"),
        "{error}"
    );
    let mut result = qualification_pass(LINUX, "clean_installation");
    result["executable_identity"] = json!("before-signing");
    let error = refused(&packet, gate, result);
    assert!(error.contains("are not signed"), "{error}");
    let mut result = qualification_pass(LINUX, "clean_installation");
    result["submission_id"] = json!("app-submission");
    let error = refused(&packet, gate, result);
    assert!(error.contains("does not apply to it"), "{error}");
}

#[test]
fn macos_must_be_notarized_with_final_signed_hashes() {
    type Change = fn(&mut Value);
    let cases: &[(&str, Change)] = &[
        ("app signing is pending", |r| {
            r["signing"]["status"] = json!("pending")
        }),
        ("disk image signing is failed", |r| {
            r["signing"]["disk_image"]["status"] = json!("failed")
        }),
        ("records no notary submission", |r| {
            r["signing"]
                .as_object_mut()
                .unwrap()
                .remove("submission_id");
        }),
        ("final executable hashes repeat", |r| {
            r["executables"]["final"]["luxforge-ctl"] =
                r["executables"]["before_signing"]["luxforge-ctl"].clone()
        }),
        ("records no final signed executable hashes", |r| {
            r["executables"].as_object_mut().unwrap().remove("final");
        }),
    ];
    for &(needle, change) in cases {
        let packet = ready();
        packet.rewrite_target(MACOS, change);
        let verdict = packet.verdict();
        assert!(!verdict.ready());
        let reasons = verdict.readiness.join("\n");
        assert!(reasons.contains(needle), "{needle}: {reasons}");
    }
}

#[test]
fn distribution_dispositions_are_for_distribution() {
    let packet = ready();
    let gate = "distribution.advisory_disposition";
    let mut development = distribution_pass();
    development["disposition"] = json!("development-only");
    let error = refused(&packet, gate, development.clone());
    assert!(error.contains("development-only disposition"), "{error}");
    // Recorded honestly, a development-only disposition fails the gate.
    development["status"] = json!("failed");
    packet.record(gate, development).unwrap();
    assert!(packet.reasons().contains("advisory_disposition is failed"));
    // A hand edit cannot pass it either, nor can a result without a disposition.
    let packet = ready();
    packet.rewrite(|release| {
        release["distribution"]["advisory_disposition"]["disposition"] = json!("development-only");
        release["distribution"]["source_and_notices_review"]
            .as_object_mut()
            .unwrap()
            .remove("disposition");
    });
    let reasons = packet.verdict().integrity.join("\n");
    assert!(
        reasons.contains("development-only disposition"),
        "{reasons}"
    );
    assert!(
        reasons.contains("source_and_notices_review records no disposition"),
        "{reasons}"
    );
}

#[test]
fn gate_results_bind_this_candidates_executables_and_bundle() {
    let packet = ready();
    let gate = "qualification.macos-arm64.native_full_verification";
    // Pre-signing evidence from the background test bundle binds the before-signing hashes.
    let mut before = qualification_pass(MACOS, "native_full_verification");
    before["executable_identity"] = json!("before-signing");
    before["executables"] = hashes(MACOS, true);
    packet.record(gate, before.clone()).unwrap();
    assert!(packet.verdict().ready(), "{}", packet.reasons());
    // SHA256SUMS was rewritten for the new release.json.
    let sums = fs::read_to_string(packet.dir.join(SUMS)).unwrap();
    assert!(sums.contains(&format!(
        "{}  release.json",
        hash(&packet.dir.join(RELEASE)).unwrap()
    )));
    let recorded = || packet.release()["qualification"][MACOS]["native_full_verification"].clone();
    assert_eq!(recorded(), before);
    let change = |edit: fn(&mut Value)| {
        let mut other = before.clone();
        edit(&mut other);
        other
    };
    let refusals: Vec<(&str, &str, Value)> = vec![
        (
            "ran another build",
            gate,
            change(|r| r["executables"]["luxforge"] = json!("d".repeat(64))),
        ),
        (
            // Before-signing hashes labelled final.
            "ran another build",
            gate,
            change(|r| r["executable_identity"] = json!("final")),
        ),
        (
            "binds no luxforge hash",
            gate,
            change(|r| {
                r["executables"].as_object_mut().unwrap().remove("luxforge");
            }),
        ),
        (
            "binds no executable hashes",
            gate,
            change(|r| {
                r.as_object_mut().unwrap().remove("executables");
            }),
        ),
        (
            "records no bundle",
            gate,
            change(|r| {
                r.as_object_mut().unwrap().remove("bundle");
            }),
        ),
        (
            "not before-signing ones",
            "qualification.macos-arm64.minimum_os",
            change(|r| r["bundle"] = json!("distributed")),
        ),
        (
            "not the background test bundle",
            "qualification.macos-arm64.clean_installation",
            {
                let mut other = qualification_pass(MACOS, "clean_installation");
                other["bundle"] = json!("background-test");
                other
            },
        ),
    ];
    for (needle, gate, result) in refusals {
        let error = refused(&packet, gate, result);
        assert!(error.contains(needle), "{needle}: {error}");
    }
    assert_eq!(recorded(), before);
    // The command line records the same shape.
    let packet = candidate("notarized", true);
    let linux = hashes(LINUX, false);
    let binding: Vec<String> = EXECUTABLES
        .iter()
        .map(|name| format!("{name}={}", linux[*name].as_str().unwrap()))
        .collect();
    let mut items = vec![
        "--packet",
        packet.dir.to_str().unwrap(),
        "--gate",
        "qualification.linux-x86_64.baseline_os",
        "--status",
        "passed",
        "--evidence",
        "ubuntu-24.04/baseline/summary.json",
        "--scope",
        "Extracted tarball on Ubuntu 24.04 with glibc 2.39",
        "--executable-identity",
        "final",
    ];
    for item in &binding {
        items.extend(["--executable", item.as_str()]);
    }
    run_gate(&mut args(&items)).unwrap();
    assert_eq!(
        packet.release()["qualification"][LINUX]["baseline_os"],
        json!({"status": "passed", "evidence": "ubuntu-24.04/baseline/summary.json", "scope": "Extracted tarball on Ubuntu 24.04 with glibc 2.39", "executable_identity": "final", "executables": linux})
    );
    assert!(packet.verdict().integrity.is_empty());
    let mut repeated = items.clone();
    repeated.extend(["--executable", binding[0].as_str()]);
    assert!(run_gate(&mut args(&repeated)).is_err());
}

#[test]
fn private_paths_coordinates_photos_and_secrets_stay_out_of_public_text() {
    for text in [
        "/Users/will/evidence/summary.json",
        "ran from /home/owner/luxforge",
        "~/Library/Logs/luxforge.log",
        "C:\\Users\\owner\\evidence",
        "file:///private/var/folders/x",
        "$HOME/evidence",
        "at 51.50735, -0.12776 in London",
        "GPS position recorded",
        "51° 30' N",
        "exported DSC_1234.NEF",
        "_DSF0042 and IMG_2001",
        "capture of holiday.jpg",
        "-----BEGIN PRIVATE KEY-----",
        "token=abc",
        "ghp_0123456789abcdefghijABCDEFGHIJ",
        "app password abcd-efgh-ijkl-mnop",
        "aGVsbG8gd29ybGQgdGhpcyBpcyBhIGtleQ1234567",
        "owner@example.com",
    ] {
        assert!(!privacy_problems(text).is_empty(), "{text}");
    }
    let hex = "ab".repeat(32);
    for text in [
        "Background test bundle org.luxforge.background-test; not Finder launch or Gatekeeper",
        "m4-full-2026-10-20/summary.json",
        "https://github.com/luxforge/luxforge/actions/runs/123456789",
        "macOS 14.7.6 (23H626) on Apple M4, full corpus of 24 RAW and JPEG sources",
        "rustc 1.96.0 (ac68faa20 2026-05-25)",
        "Apple clang version 15.0.0 (clang-1500.3.9.4)",
        "luxforge-0.1.0-linux-x86_64.tar.gz",
        hex.as_str(),
        "qualification.linux-x86_64.native_rendered_verification",
    ] {
        assert!(
            privacy_problems(text).is_empty(),
            "{text}: {:?}",
            privacy_problems(text)
        );
    }
    // Recording refuses private evidence references and scopes, without repeating them.
    let packet = ready();
    let gate = "qualification.linux-x86_64.baseline_os";
    for (field, text, needle) in [
        (
            "evidence",
            "/Users/will/evidence/summary.json",
            "evidence names a private file path",
        ),
        ("evidence", "../outside/summary.json", "neither a reference"),
        (
            "evidence",
            "http://example.com/report",
            "neither a reference",
        ),
        (
            "scope",
            "Opened DSC_1234.NEF from the corpus",
            "scope names a photo file name",
        ),
        (
            "scope",
            "Shot at 51.50735, -0.12776",
            "scope names coordinates",
        ),
        ("scope", "Signed with token: abc", "scope names a secret"),
        ("scope", "two\nlines", "not one line"),
    ] {
        let mut result = qualification_pass(LINUX, "baseline_os");
        result[field] = json!(text);
        let error = refused(&packet, gate, result);
        assert!(error.contains(needle), "{text}: {error}");
        assert!(
            !error.contains(text),
            "the refusal repeats the private text: {error}"
        );
    }
    // A hand-edited leak, in a gate or a build record, is never published.
    let packet = ready();
    packet.rewrite(|release| {
        release["qualification"][LINUX]["baseline_os"]["scope"] = json!("Ran /home/owner/luxforge");
    });
    assert!(!packet.verdict().ready());
    let error = summary(&packet.verdict()).unwrap_err().to_string();
    assert!(
        error.contains("/qualification/linux-x86_64/baseline_os/scope names a private file path"),
        "{error}"
    );
    let packet = ready();
    packet.rewrite_target(MACOS, |record| {
        record["build_environment"]["os"] = json!("macOS 14.7.6 on owner@example.com's Mac");
    });
    let error = summary(&packet.verdict()).unwrap_err().to_string();
    assert!(
        error.contains("build_environment/os names an email address"),
        "{error}"
    );
    let out = packet.root.join("leaky-summary.json");
    let error = run_verify(&mut args(&[
        "--packet",
        packet.dir.to_str().unwrap(),
        "--summary",
        out.to_str().unwrap(),
    ]))
    .unwrap_err()
    .to_string();
    assert!(error.contains("so it is not written"), "{error}");
    assert!(!out.exists());
}
