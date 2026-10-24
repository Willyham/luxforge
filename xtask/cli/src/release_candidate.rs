//! The build-candidate contract (`docs/design/release-v0.1.md#candidate-records`): the source checks
//! a release build makes before it compiles anything, what it records about one target in
//! `target.json` and the bundled `build.json`, and the `release.json` that binds every target to one
//! source identity beside per-platform qualification. Whether a packet is release-ready is the
//! readiness verifier's decision, not this contract's.
use crate::release_artifacts::{TARGETS, hash, platform};
use crate::release_tag::ReleaseTag;
use crate::{Result, ensure, output};
use serde_json::{Map, Value, json};
use std::{fs, path::Path, process::Command};

/// The shape of `target.json`, `build.json` and `release.json`. Only the current shape is read.
pub const SCHEMA_VERSION: u64 = 2;
/// Every release payload's executables: the editor, then the command lines: the headless owner,
/// the live-session client and the MCP server. On macOS the editor is in `Contents/MacOS` and the
/// command lines in `Contents/Helpers`; on Linux they are all at the payload's top.
pub const EXECUTABLES: &[&str] = &["luxforge", "luxforge-json", "luxforge-ctl", "luxforge-mcp"];
/// A field that does not apply to a target, such as Apple signing on Linux. It never stands for a
/// required check that did not run.
pub const NOT_APPLICABLE: &str = "not-applicable";
/// What a qualification or distribution gate can report. `not-applicable` is not among them: every
/// listed gate applies to its platform.
pub const GATE_STATUSES: &[&str] = &["not-run", "passed", "failed", "incomplete"];
/// The gates the source and dependency graph pass for every target.
pub const DISTRIBUTION_GATES: &[&str] = &["source_and_notices_review", "advisory_disposition"];
const MACOS_GATES: &[&str] = &[
    "native_full_verification",
    "gpu_corpus_gate",
    "minimum_os",
    "clean_installation",
];
const LINUX_GATES: &[&str] = &[
    "native_rendered_verification",
    "gpu_corpus_gate",
    "wayland_and_x11",
    "baseline_os",
    "clean_installation",
];

/// The qualification gates a platform's payload passes before a joint release is ready.
pub fn platform_gates(platform: &str) -> Result<&'static [&'static str]> {
    match platform {
        "macos-arm64" => Ok(MACOS_GATES),
        "linux-x86_64" | "linux-arm64" => Ok(LINUX_GATES),
        _ => Err(format!("No qualification gates for {platform}").into()),
    }
}

fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_revision(text: &str) -> bool {
    is_hex(text, 40) || is_hex(text, 64)
}

fn is_macos(target: &str) -> bool {
    target == TARGETS[0].0
}

/// A build number: a positive decimal integer of at most 18 digits, without a leading zero.
pub fn check_build_number(value: &str) -> Result {
    ensure(
        !value.is_empty()
            && value.len() <= 18
            && value.bytes().all(|b| b.is_ascii_digit())
            && !value.starts_with('0'),
        "Build number must be a positive decimal integer, at most 18 digits",
    )
}

/// The source a release build compiles: the checked-out commit, whether the tree differs from it
/// and the lockfile's hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub revision: String,
    pub dirty: bool,
    pub lock_sha256: String,
}

/// Whether the index or working tree differs from HEAD, untracked files included. Ignored files,
/// such as build and evidence output, do not count.
pub fn working_tree_dirty(root: &Path) -> Result<bool> {
    Ok(!output(
        root,
        "git",
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?
    .trim()
    .is_empty())
}

fn tag_commit(root: &Path, tag: &str) -> Result<Option<String>> {
    let result = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/tags/{tag}^{{commit}}"),
        ])
        .current_dir(root)
        .output()?;
    Ok(result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).trim().to_owned()))
}

/// Checks a release build's inputs before anything is built: a shipping target, a new output
/// directory, a full commit and, unless this is a labelled local rehearsal, a clean tree whose
/// commit the tag names. It refuses rather than editing the checkout or guessing the commit.
pub fn check_source(
    root: &Path,
    out: &Path,
    tag: &ReleaseTag,
    target: &str,
    rehearsal: bool,
) -> Result<Source> {
    platform(target)?;
    ensure(!out.exists(), "Release build output must be new")?;
    let revision = output(root, "git", &["rev-parse", "--verify", "HEAD^{commit}"])?
        .trim()
        .to_owned();
    ensure(
        is_revision(&revision),
        "The checked-out source has no full commit ID",
    )?;
    let dirty = working_tree_dirty(root)?;
    ensure(
        rehearsal || !dirty,
        "Release builds require a clean checkout, untracked files included; --allow-dirty is a local rehearsal only",
    )?;
    if !rehearsal {
        let tagged = tag_commit(root, &tag.tag)?
            .ok_or_else(|| format!("Release tag {} does not exist", tag.tag))?;
        ensure(
            tagged == revision,
            format!(
                "Release tag {} does not name the checked-out commit",
                tag.tag
            ),
        )?;
    }
    Ok(Source {
        revision,
        dirty,
        lock_sha256: hash(&root.join("Cargo.lock"))?,
    })
}

fn pinned_channel(text: &str) -> Result<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("channel"))
        .filter_map(|rest| rest.trim().strip_prefix('='))
        .map(|value| value.trim().trim_matches('"').to_owned())
        .find(|value| !value.is_empty())
        .ok_or_else(|| "rust-toolchain.toml names no channel".into())
}

fn matches_pin(toolchain: &Value) -> bool {
    match (
        toolchain["pinned"].as_str(),
        toolchain["rustc"].as_str(),
        toolchain["cargo"].as_str(),
    ) {
        (Some(pinned), Some(rustc), Some(cargo)) => {
            rustc.starts_with(&format!("rustc {pinned} "))
                && cargo.starts_with(&format!("cargo {pinned} "))
        }
        _ => false,
    }
}

/// The Rust toolchain a build uses beside the repository's pin. A release build must use the pin;
/// a rehearsal records whatever it used.
pub fn toolchain(root: &Path, rehearsal: bool) -> Result<Value> {
    let pinned = pinned_channel(&fs::read_to_string(root.join("rust-toolchain.toml"))?)?;
    let value = json!({
        "pinned": pinned,
        "rustc": output(root, "rustc", &["--version"])?.trim(),
        "cargo": output(root, "cargo", &["--version"])?.trim(),
    });
    ensure(
        rehearsal || matches_pin(&value),
        format!(
            "A release build uses the pinned Rust {pinned}, not {}",
            value["rustc"]
        ),
    )?;
    Ok(value)
}

fn first_line(text: &str) -> Result<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "A build tool reported no version".into())
}

/// The distribution `/etc/os-release` names: its `PRETTY_NAME`, otherwise `NAME VERSION_ID`.
pub fn os_release_name(text: &str) -> Result<String> {
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|value| value.trim().trim_matches('"').trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    field("PRETTY_NAME")
        .or_else(|| Some(format!("{} {}", field("NAME")?, field("VERSION_ID")?)))
        .ok_or_else(|| "os-release names no distribution".into())
}

/// The host a target was built on: its OS and native C compiler, with the macOS SDK or the Linux C
/// library. The field that does not apply to the target is `not-applicable`.
pub fn build_environment(root: &Path, target: &str) -> Result<Value> {
    platform(target)?;
    let compiler = first_line(&output(root, "cc", &["--version"])?)?;
    if is_macos(target) {
        ensure(
            cfg!(target_os = "macos"),
            "A macOS candidate is built on macOS",
        )?;
        let version = output(root, "sw_vers", &["-productVersion"])?;
        let build = output(root, "sw_vers", &["-buildVersion"])?;
        Ok(json!({
            "os": format!("macOS {} ({})", version.trim(), build.trim()),
            "native_compiler": compiler,
            "sdk": first_line(&output(root, "xcrun", &["--show-sdk-version"])?)?,
            "libc": NOT_APPLICABLE,
        }))
    } else {
        ensure(
            cfg!(target_os = "linux"),
            "A Linux candidate is built on Linux",
        )?;
        Ok(json!({
            "os": os_release_name(&fs::read_to_string("/etc/os-release")?)?,
            "native_compiler": compiler,
            "sdk": NOT_APPLICABLE,
            "libc": first_line(&output(root, "getconf", &["GNU_LIBC_VERSION"])?)?,
        }))
    }
}

/// A build's executable hashes, by name. macOS signing rewrites its executables, so a build records
/// them as `before_signing` and signing adds `final`, measured from the signed app. Linux payloads
/// are not signed: what is built is `final`.
pub fn executables(target: &str, hashes: Map<String, Value>) -> Value {
    if is_macos(target) {
        json!({"before_signing": hashes})
    } else {
        json!({"final": hashes})
    }
}

fn executable_hashes(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object().filter(|hashes| {
        hashes.len() == EXECUTABLES.len()
            && EXECUTABLES.iter().all(|name| {
                hashes
                    .get(*name)
                    .and_then(Value::as_str)
                    .is_some_and(|hash| is_hex(hash, 64))
            })
    })
}

fn check_executables(value: &Value, macos: bool, notarized: bool) -> Result {
    let record = value
        .as_object()
        .ok_or("Build record has no executable hashes")?;
    ensure(
        record
            .keys()
            .all(|key| key == "before_signing" || key == "final"),
        "Executable hashes are before_signing or final",
    )?;
    let before = record.get("before_signing");
    let after = record.get("final");
    if !macos {
        ensure(
            before.is_none(),
            "Linux executables are not signed, so they have no before-signing hashes",
        )?;
        after
            .and_then(executable_hashes)
            .ok_or("A Linux build records its final executable hashes")?;
        return Ok(());
    }
    let before = before
        .and_then(executable_hashes)
        .ok_or("A macOS build records its executable hashes before signing")?;
    match (notarized, after) {
        (false, None) => Ok(()),
        (false, Some(_)) => Err(
            "A macOS candidate that is not notarized has no final signed executable hashes".into(),
        ),
        (true, None) => {
            Err("A notarized candidate records its final signed executable hashes".into())
        }
        (true, Some(after)) => {
            let after =
                executable_hashes(after).ok_or("Final executable hashes name every executable")?;
            ensure(
                EXECUTABLES.iter().all(|name| after[*name] != before[*name]),
                "Final executable hashes are measured after signing, not copied from before it",
            )
        }
    }
}

/// Checks one target's build record against the contract: its tag, version, target and profile,
/// full source identity, toolchain and build environment, Apple signing for macOS only, and
/// executable hashes that keep before-signing and final apart. The archive and disk image are
/// checked against their files by `release_artifacts::verify_record`.
pub fn check_record(record: &Value, tag: &ReleaseTag, target: &str) -> Result {
    let platform = platform(target)?;
    let macos = is_macos(target);
    ensure(
        record["schema_version"] == SCHEMA_VERSION
            && record["tag"] == tag.tag
            && record["version"] == tag.version
            && record["target"] == target
            && record["platform"] == platform
            && record["profile"] == "release",
        "Build artifact identity mismatch",
    )?;
    ensure(
        record["revision"].as_str().is_some_and(is_revision),
        "Build record has no full source revision",
    )?;
    ensure(
        record["lock_sha256"]
            .as_str()
            .is_some_and(|lock| is_hex(lock, 64)),
        "Build record has no lockfile hash",
    )?;
    ensure(
        record["build_number"]
            .as_str()
            .is_some_and(|number| check_build_number(number).is_ok()),
        "Build record has no build number",
    )?;
    let (Some(dirty), Some(rehearsal)) = (
        record["working_tree_dirty"].as_bool(),
        record["rehearsal"].as_bool(),
    ) else {
        return Err("Build record does not say whether it is a clean build".into());
    };
    ensure(
        rehearsal || !dirty,
        "A build from a dirty tree is a labelled rehearsal",
    )?;
    ensure(
        record["release_ready"] == false,
        "Build artifacts must not claim release qualification",
    )?;
    ensure(
        ["pinned", "rustc", "cargo"]
            .iter()
            .all(|key| record["toolchain"][key].is_string()),
        "Build record has no toolchain",
    )?;
    ensure(
        rehearsal || matches_pin(&record["toolchain"]),
        "A release build used another Rust toolchain than the pinned one",
    )?;
    let environment = &record["build_environment"];
    let named = |key: &str| {
        environment[key]
            .as_str()
            .is_some_and(|value| !value.trim().is_empty() && value != NOT_APPLICABLE)
    };
    ensure(
        named("os") && named("native_compiler"),
        "Build record has no build environment",
    )?;
    ensure(
        if macos {
            named("sdk") && environment["libc"] == NOT_APPLICABLE
        } else {
            environment["sdk"] == NOT_APPLICABLE && named("libc")
        },
        "Build environment does not match the target",
    )?;
    ensure(
        record["catalog_format"].as_u64().is_some_and(|f| f > 0),
        "Build record has no catalog format",
    )?;
    ensure(
        !macos || record["minimum_system_version"].is_string(),
        "Build record has no minimum system version",
    )?;
    let signing = record["signing"]["status"].as_str();
    if macos {
        ensure(
            matches!(
                signing,
                Some("unsigned" | "pending" | "failed" | "notarized")
            ),
            "A macOS candidate has no known signing status: Apple signing always applies to it",
        )?;
    } else {
        ensure(
            signing == Some(NOT_APPLICABLE)
                && record["signing"].as_object().is_some_and(|s| s.len() == 1),
            "Apple signing and notarization are not applicable to Linux",
        )?;
        crate::release_elf::check_record(&record["elf_imports"], EXECUTABLES)?;
    }
    check_executables(&record["executables"], macos, signing == Some("notarized"))
}

/// The complete source archive's name for `tag`.
pub fn source_archive_name(tag: &ReleaseTag) -> String {
    format!("luxforge-{}-source.tar.gz", tag.version)
}

fn gates(names: &[&str]) -> Value {
    names
        .iter()
        .map(|name| ((*name).to_owned(), json!({"status": "not-run"})))
        .collect::<Map<_, _>>()
        .into()
}

fn check_gates(value: &Value, names: &[&str], what: &str) -> Result {
    let object = value
        .as_object()
        .ok_or_else(|| format!("release.json has no {what} gates"))?;
    ensure(
        object.len() == names.len()
            && names.iter().all(|name| {
                object
                    .get(*name)
                    .and_then(|gate| gate["status"].as_str())
                    .is_some_and(|status| GATE_STATUSES.contains(&status))
            }),
        format!(
            "release.json's {what} gates are exactly {names:?}, each with a status of {GATE_STATUSES:?}"
        ),
    )
}

/// `release.json` for every target's checked records, in [`TARGETS`] order: their common source
/// identity, no source archive yet, and every distribution and platform gate not run. It never
/// contains its own hash, and lives outside every archive it describes.
pub fn release_record(tag: &ReleaseTag, records: &[Value]) -> Value {
    let qualification: Map<String, Value> = TARGETS
        .iter()
        .map(|(_, platform)| {
            (
                (*platform).to_owned(),
                gates(platform_gates(platform).unwrap_or_default()),
            )
        })
        .collect();
    json!({
        "schema_version": SCHEMA_VERSION,
        "tag": tag.tag,
        "version": tag.version,
        "revision": records[0]["revision"],
        "lock_sha256": records[0]["lock_sha256"],
        "build_number": records[0]["build_number"],
        "status": "build-candidate",
        "release_ready": false,
        "source_archive": null,
        "distribution": gates(DISTRIBUTION_GATES),
        "qualification": qualification,
        "targets": records,
    })
}

/// Checks `release.json`'s shape: every shipping target, each a valid build record sharing the
/// packet's commit, version, lockfile and build number; a source archive named for the version or
/// none yet; and exactly the listed distribution and platform gates. It is not a readiness verdict:
/// a build candidate never claims release readiness.
pub fn check_release(release: &Value, tag: &ReleaseTag) -> Result {
    ensure(
        release["schema_version"] == SCHEMA_VERSION
            && release["tag"] == tag.tag
            && release["version"] == tag.version
            && release["status"] == "build-candidate"
            && release["release_ready"] == false,
        "release.json identity mismatch",
    )?;
    let targets = release["targets"]
        .as_array()
        .filter(|targets| targets.len() == TARGETS.len())
        .ok_or("release.json names exactly the shipping targets")?;
    for ((target, _), record) in TARGETS.iter().zip(targets) {
        check_record(record, tag, target)?;
        for field in ["revision", "lock_sha256", "build_number"] {
            ensure(
                record[field] == release[field],
                format!("{target}'s {field} differs from the release's"),
            )?;
        }
    }
    match &release["source_archive"] {
        Value::Null => {}
        archive => ensure(
            archive["name"] == source_archive_name(tag)
                && archive["size"].is_u64()
                && archive["sha256"].as_str().is_some_and(|h| is_hex(h, 64)),
            "release.json's source archive is named for the version with its size and hash",
        )?,
    }
    check_gates(&release["distribution"], DISTRIBUTION_GATES, "distribution")?;
    let qualification = release["qualification"]
        .as_object()
        .filter(|platforms| platforms.len() == TARGETS.len())
        .ok_or("release.json qualifies exactly the shipping platforms")?;
    for (_, platform) in TARGETS {
        check_gates(
            qualification
                .get(*platform)
                .ok_or_else(|| format!("release.json does not qualify {platform}"))?,
            platform_gates(platform)?,
            platform,
        )?;
    }
    Ok(())
}

/// A complete, valid build record for `target` in `directory`, as `release-build` (and, when
/// `signing` is `notarized`, `release-sign`) writes it, with its archive and any disk image beside
/// it.
#[cfg(test)]
pub(crate) fn fixture_record(
    directory: &Path,
    tag: &ReleaseTag,
    target: &str,
    signing: &str,
) -> Value {
    use crate::release_artifacts::{archive_name, archive_record, disk_image_name};
    fs::create_dir_all(directory).unwrap();
    let archive = directory.join(archive_name(tag, target).unwrap());
    fs::write(&archive, format!("final {target} archive")).unwrap();
    let macos = is_macos(target);
    let built: Map<String, Value> = EXECUTABLES
        .iter()
        .enumerate()
        .map(|(i, name)| ((*name).to_owned(), json!(format!("{i:064x}"))))
        .collect();
    let mut record = json!({
        "schema_version": SCHEMA_VERSION,
        "tag": tag.tag,
        "version": tag.version,
        "build_number": "1",
        "catalog_format": 1,
        "revision": "a".repeat(40),
        "target": target,
        "platform": platform(target).unwrap(),
        "profile": "release",
        "working_tree_dirty": false,
        "rehearsal": false,
        "lock_sha256": "b".repeat(64),
        "toolchain": {"pinned": "1.94.0", "rustc": "rustc 1.94.0 (4a4ef493e 2026-03-02)", "cargo": "cargo 1.94.0 (85eff7c80 2026-01-15)"},
        "build_environment": if macos {
            json!({"os": "macOS 14.7.6 (23H626)", "native_compiler": "Apple clang version 15.0.0 (clang-1500.3.9.4)", "sdk": "14.5", "libc": NOT_APPLICABLE})
        } else {
            json!({"os": "Ubuntu 24.04.3 LTS", "native_compiler": "cc (Ubuntu 13.3.0-6ubuntu2~24.04) 13.3.0", "sdk": NOT_APPLICABLE, "libc": "glibc 2.39"})
        },
        "executables": executables(target, built.clone()),
        "release_ready": false,
        "signing": {"status": if macos { signing } else { NOT_APPLICABLE }},
        "archive": archive_record(&archive).unwrap(),
    });
    if !macos {
        let imports = || json!({"needed": ["libgcc_s.so.1", "libm.so.6", "libc.so.6", "ld-linux-x86-64.so.2"], "versions": {"libc.so.6": ["GLIBC_2.2.5", "GLIBC_2.39"]}, "max_glibc": "2.39"});
        record["elf_imports"] = json!({
            "inspected_with": "GNU readelf (GNU Binutils for Ubuntu) 2.42 (-W --dynamic --version-info)",
            "glibc_baseline": crate::release_elf::GLIBC_BASELINE,
            "max_glibc": "2.39",
            "executables": EXECUTABLES.iter().map(|name| ((*name).to_owned(), imports())).collect::<Map<_, _>>(),
        });
    }
    if let Some(name) = disk_image_name(tag, target).unwrap() {
        let image = directory.join(name);
        fs::write(&image, b"final disk image").unwrap();
        record["minimum_system_version"] = json!("14.0");
        record["disk_image"] = archive_record(&image).unwrap();
        record["signing"]["disk_image"] = json!({"status": signing});
        if signing == "notarized" {
            record["signing"]["submission_id"] = json!("app-submission");
            record["signing"]["disk_image"]["submission_id"] = json!("image-submission");
            let signed: Map<String, Value> = EXECUTABLES
                .iter()
                .enumerate()
                .map(|(i, name)| ((*name).to_owned(), json!(format!("{:064x}", i + 16))))
                .collect();
            record["executables"]["final"] = signed.into();
        }
    }
    fs::write(
        directory.join("target.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
    record
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_test_macros::slow;
    use std::path::PathBuf;

    fn v01() -> ReleaseTag {
        ReleaseTag::parse("v0.1").unwrap()
    }

    fn refused(record: &Value, tag: &ReleaseTag, target: &str) -> String {
        check_record(record, tag, target)
            .expect_err("the record was accepted")
            .to_string()
    }

    /// An isolated checkout, in a path with spaces and non-ASCII, with an ignored build directory
    /// and a commit tagged `v0.1`.
    struct Checkout {
        _temp: tempfile::TempDir,
        root: PathBuf,
    }

    impl Checkout {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("checkout wïth spaces");
            fs::create_dir(&root).unwrap();
            let checkout = Self { _temp: temp, root };
            checkout.git(&["init", "--initial-branch=main"]);
            for (key, value) in [
                ("user.name", "Release Test"),
                ("user.email", "release@example.invalid"),
                ("commit.gpgSign", "false"),
                ("tag.gpgSign", "false"),
                ("core.hooksPath", ".git/test-hooks"),
                ("status.showUntrackedFiles", "no"),
            ] {
                checkout.git(&["config", key, value]);
            }
            fs::write(checkout.root.join("Cargo.lock"), "# lock\n").unwrap();
            fs::write(checkout.root.join(".gitignore"), "/target/\n/artifacts/\n").unwrap();
            checkout.git(&["add", "."]);
            checkout.git(&["commit", "-m", "candidate"]);
            checkout.git(&["tag", "-a", "v0.1", "-m", "v0.1"]);
            checkout
        }

        fn git(&self, args: &[&str]) -> String {
            output(&self.root, "git", args).unwrap()
        }

        fn check(&self, tag: &str, rehearsal: bool) -> Result<Source> {
            check_source(
                &self.root,
                &self.root.join("artifacts/new"),
                &ReleaseTag::parse(tag).unwrap(),
                TARGETS[1].0,
                rehearsal,
            )
        }
    }

    #[test]
    #[slow]
    fn source_checks_refuse_dirty_input_another_commit_and_reused_output() {
        let checkout = Checkout::new();
        let head = checkout.git(&["rev-parse", "HEAD"]).trim().to_owned();
        // Ignored build and evidence output leaves the tree clean.
        fs::create_dir_all(checkout.root.join("target/release")).unwrap();
        fs::write(checkout.root.join("target/release/luxforge"), b"built").unwrap();
        let source = checkout.check("v0.1", false).unwrap();
        assert_eq!(
            source,
            Source {
                revision: head.clone(),
                dirty: false,
                lock_sha256: hash(&checkout.root.join("Cargo.lock")).unwrap(),
            }
        );
        // The short tag's alias names no tag; nor does an unknown version.
        for tag in ["v0.1.0", "v0.2"] {
            let error = checkout.check(tag, false).unwrap_err().to_string();
            assert!(error.contains("does not exist"), "{error}");
        }
        // An untracked file, even with untracked files hidden from status, and a tracked change.
        fs::write(checkout.root.join("notes.txt"), b"untracked").unwrap();
        let error = checkout.check("v0.1", false).unwrap_err().to_string();
        assert!(error.contains("clean checkout"), "{error}");
        // A labelled rehearsal records the dirty tree rather than refusing it.
        assert!(checkout.check("v0.1", true).unwrap().dirty);
        fs::remove_file(checkout.root.join("notes.txt")).unwrap();
        fs::write(checkout.root.join("Cargo.lock"), "# changed\n").unwrap();
        assert!(checkout.check("v0.1", false).is_err());
        checkout.git(&["checkout", "--", "Cargo.lock"]);
        // A commit after the tag: the tag no longer names HEAD; a rehearsal needs no tag.
        checkout.git(&["commit", "--allow-empty", "-m", "after the tag"]);
        let error = checkout.check("v0.1", false).unwrap_err().to_string();
        assert!(error.contains("does not name"), "{error}");
        assert!(!checkout.check("v0.2", true).unwrap().dirty);
        // Reused output and an unsupported target are refused before anything else.
        let tag = v01();
        fs::create_dir_all(checkout.root.join("artifacts/used")).unwrap();
        let reused = check_source(
            &checkout.root,
            &checkout.root.join("artifacts/used"),
            &tag,
            TARGETS[1].0,
            true,
        );
        assert!(reused.unwrap_err().to_string().contains("must be new"));
        for target in ["x86_64-apple-darwin", "riscv64gc-unknown-linux-gnu"] {
            let error = check_source(&checkout.root, &checkout.root.join("n"), &tag, target, true)
                .unwrap_err()
                .to_string();
            assert!(error.contains("only"), "{error}");
        }
    }

    #[test]
    fn records_bind_target_version_and_tag() {
        let tmp = tempfile::tempdir().unwrap();
        let tag = v01();
        for &(target, platform) in TARGETS {
            let record = fixture_record(&tmp.path().join(platform), &tag, target, "unsigned");
            check_record(&record, &tag, target).unwrap();
            // The same record under another tag, version or target.
            assert!(check_record(&record, &ReleaseTag::parse("v0.2").unwrap(), target).is_err());
            let other = TARGETS.iter().find(|(t, _)| *t != target).unwrap().0;
            assert!(check_record(&record, &tag, other).is_err());
            for (field, value) in [
                ("version", json!("0.1.1")),
                ("tag", json!("v0.1.0")),
                ("platform", json!("windows-x86_64")),
                ("schema_version", json!(1)),
                ("profile", json!("dev")),
                ("revision", json!("a1b2c3")),
                ("lock_sha256", json!(null)),
                ("build_number", json!("007")),
                ("release_ready", json!(true)),
            ] {
                let mut bad = record.clone();
                bad[field] = value;
                assert!(check_record(&bad, &tag, target).is_err(), "{field}");
            }
            // A Linux record names what its executables import, within the glibc baseline.
            if !is_macos(target) {
                let mut uninspected = record.clone();
                uninspected.as_object_mut().unwrap().remove("elf_imports");
                assert!(refused(&uninspected, &tag, target).contains("ELF"));
            }
            // A dirty tree is only a labelled rehearsal.
            let mut dirty = record.clone();
            dirty["working_tree_dirty"] = json!(true);
            assert!(refused(&dirty, &tag, target).contains("rehearsal"));
            dirty["rehearsal"] = json!(true);
            check_record(&dirty, &tag, target).unwrap();
        }
    }

    #[test]
    fn records_name_the_pinned_toolchain_and_their_build_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let tag = v01();
        for &(target, platform) in TARGETS {
            let record = fixture_record(&tmp.path().join(platform), &tag, target, "unsigned");
            let mut other = record.clone();
            other["toolchain"]["rustc"] = json!("rustc 1.99.0 (b940084d7 2026-09-28)");
            assert!(refused(&other, &tag, target).contains("pinned"));
            // A rehearsal records whatever toolchain it used.
            other["rehearsal"] = json!(true);
            check_record(&other, &tag, target).unwrap();
            for key in ["os", "native_compiler", "sdk", "libc"] {
                let mut bad = record.clone();
                bad["build_environment"]
                    .as_object_mut()
                    .unwrap()
                    .remove(key);
                assert!(check_record(&bad, &tag, target).is_err(), "{key}");
            }
            // The SDK is macOS's, the C library Linux's; neither stands in for the other.
            let mut swapped = record.clone();
            let environment = &mut swapped["build_environment"];
            let (sdk, libc) = (environment["sdk"].clone(), environment["libc"].clone());
            environment["sdk"] = libc;
            environment["libc"] = sdk;
            assert!(refused(&swapped, &tag, target).contains("does not match"));
        }
        assert_eq!(
            pinned_channel("[toolchain]\nchannel = \"1.94.0\"\nprofile = \"minimal\"\n").unwrap(),
            "1.94.0"
        );
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let pinned =
            pinned_channel(&fs::read_to_string(repository.join("rust-toolchain.toml")).unwrap())
                .unwrap();
        assert!(pinned.split('.').all(|part| part.parse::<u32>().is_ok()));
        assert_eq!(
            os_release_name(
                "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"Ubuntu 24.04.3 LTS\"\n"
            )
            .unwrap(),
            "Ubuntu 24.04.3 LTS"
        );
        assert_eq!(
            os_release_name("NAME=Ubuntu\nVERSION_ID=24.04\n").unwrap(),
            "Ubuntu 24.04"
        );
        assert!(os_release_name("ID=ubuntu\n").is_err());
    }

    #[test]
    fn before_signing_and_final_executable_hashes_stay_distinct() {
        let tmp = tempfile::tempdir().unwrap();
        let tag = v01();
        let (mac, linux) = (TARGETS[0].0, TARGETS[1].0);
        let unsigned = fixture_record(&tmp.path().join("unsigned"), &tag, mac, "unsigned");
        let notarized = fixture_record(&tmp.path().join("notarized"), &tag, mac, "notarized");
        let built = fixture_record(&tmp.path().join("linux"), &tag, linux, NOT_APPLICABLE);
        check_record(&unsigned, &tag, mac).unwrap();
        check_record(&notarized, &tag, mac).unwrap();
        check_record(&built, &tag, linux).unwrap();
        assert!(unsigned["executables"].get("final").is_none());
        assert!(built["executables"].get("before_signing").is_none());
        // An unsigned app has no final signed hashes, a notarized one must have them, and they
        // must have been measured after signing rather than copied.
        let mut bad = unsigned.clone();
        bad["executables"]["final"] = bad["executables"]["before_signing"].clone();
        assert!(refused(&bad, &tag, mac).contains("not notarized"));
        let mut bad = notarized.clone();
        bad["executables"].as_object_mut().unwrap().remove("final");
        assert!(refused(&bad, &tag, mac).contains("final signed"));
        let mut bad = notarized.clone();
        bad["executables"]["final"]["luxforge-ctl"] =
            bad["executables"]["before_signing"]["luxforge-ctl"].clone();
        assert!(refused(&bad, &tag, mac).contains("after signing"));
        let mut bad = notarized.clone();
        bad["executables"]["final"]
            .as_object_mut()
            .unwrap()
            .remove("luxforge-json");
        assert!(check_record(&bad, &tag, mac).is_err());
        // Every command line is bound, the MCP server included, on both platforms.
        let mut bad = unsigned.clone();
        bad["executables"]["before_signing"]
            .as_object_mut()
            .unwrap()
            .remove("luxforge-mcp");
        assert!(check_record(&bad, &tag, mac).is_err());
        let mut bad = built.clone();
        bad["executables"]["final"]
            .as_object_mut()
            .unwrap()
            .remove("luxforge-mcp");
        assert!(check_record(&bad, &tag, linux).is_err());
        // Linux executables are not signed: no before-signing hashes, and its final ones required.
        let mut bad = built.clone();
        bad["executables"]["before_signing"] = bad["executables"]["final"].clone();
        assert!(refused(&bad, &tag, linux).contains("not signed"));
        let mut bad = built.clone();
        bad["executables"] = executables(
            mac,
            built["executables"]["final"].as_object().unwrap().clone(),
        );
        assert!(check_record(&bad, &tag, linux).is_err());
    }

    #[test]
    fn apple_signing_applies_to_macos_only_and_cannot_be_waived() {
        let tmp = tempfile::tempdir().unwrap();
        let tag = v01();
        let (mac, linux) = (TARGETS[0].0, TARGETS[1].0);
        let unsigned = fixture_record(&tmp.path().join("mac"), &tag, mac, "unsigned");
        for status in [NOT_APPLICABLE, "skipped", "passed"] {
            let mut bad = unsigned.clone();
            bad["signing"]["status"] = json!(status);
            assert!(
                refused(&bad, &tag, mac).contains("always applies"),
                "{status}"
            );
        }
        let mut bad = unsigned.clone();
        bad.as_object_mut()
            .unwrap()
            .remove("minimum_system_version");
        assert!(refused(&bad, &tag, mac).contains("minimum system version"));
        let built = fixture_record(&tmp.path().join("linux"), &tag, linux, NOT_APPLICABLE);
        for signing in [
            json!({"status": "notarized"}),
            json!({"status": "unsigned"}),
            json!({"status": NOT_APPLICABLE, "submission_id": "app-submission"}),
        ] {
            let mut bad = built.clone();
            bad["signing"] = signing;
            assert!(refused(&bad, &tag, linux).contains("not applicable"));
        }
    }

    #[test]
    fn release_json_binds_both_targets_and_lists_every_gate_unrun() {
        let tmp = tempfile::tempdir().unwrap();
        let tag = v01();
        let records: Vec<Value> = TARGETS
            .iter()
            .map(|(target, platform)| {
                fixture_record(&tmp.path().join(platform), &tag, target, "unsigned")
            })
            .collect();
        let release = release_record(&tag, &records);
        check_release(&release, &tag).unwrap();
        assert_eq!(release["source_archive"], Value::Null);
        for (name, gate) in release["distribution"].as_object().unwrap() {
            assert!(DISTRIBUTION_GATES.contains(&name.as_str()));
            assert_eq!(gate["status"], "not-run");
        }
        assert_eq!(
            release["qualification"]["linux-x86_64"]
                .as_object()
                .unwrap()
                .len(),
            LINUX_GATES.len()
        );
        // A source archive is named for the version, with its size and hash.
        let mut sourced = release.clone();
        sourced["source_archive"] =
            json!({"name": "luxforge-0.1.0-source.tar.gz", "size": 1, "sha256": "c".repeat(64)});
        check_release(&sourced, &tag).unwrap();
        sourced["source_archive"]["name"] = json!("source.tar.gz");
        assert!(check_release(&sourced, &tag).is_err());
        let changes: &[fn(&mut Value)] = &[
            |r| r["targets"].as_array_mut().unwrap().truncate(1),
            |r| r["targets"].as_array_mut().unwrap().reverse(),
            |r| r["targets"][1]["revision"] = json!("c".repeat(40)),
            |r| r["targets"][1]["lock_sha256"] = json!("c".repeat(64)),
            |r| r["targets"][0]["build_number"] = json!("2"),
            |r| r["release_ready"] = json!(true),
            |r| {
                r["qualification"]["linux-x86_64"]["gpu_corpus_gate"]["status"] =
                    json!(NOT_APPLICABLE)
            },
            |r| r["qualification"]["macos-arm64"]["minimum_os"]["status"] = json!("skipped"),
            |r| {
                r["qualification"]["macos-arm64"]
                    .as_object_mut()
                    .unwrap()
                    .remove("clean_installation");
            },
            |r| {
                r["qualification"]
                    .as_object_mut()
                    .unwrap()
                    .remove("linux-x86_64");
            },
            |r| r["distribution"]["advisory_disposition"] = json!("passed"),
        ];
        for (index, change) in changes.iter().enumerate() {
            let mut bad = release.clone();
            change(&mut bad);
            assert!(check_release(&bad, &tag).is_err(), "change {index}");
        }
    }
}
