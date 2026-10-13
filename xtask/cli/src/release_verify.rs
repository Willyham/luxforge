//! The release-ready packet verifier and gate recording
//! (`docs/design/release-v0.1.md#readiness-and-gate-results`). A packet is the directory
//! `release-assemble` writes: both targets' downloads and build records, `release.json`, the release
//! notes, the complete source archive once it exists, and `SHA256SUMS` over all of them.
//!
//! `release-gate` records one distribution or qualification result in `release.json`, after checking
//! the packet is consistent, and rewrites that file's `SHA256SUMS` line. `release-verify` reports
//! every reason a packet is not ready and can write a sanitized public summary outside the packet.
//! Neither ever writes readiness into `release.json`, which stays a `build-candidate` record with
//! `release_ready: false`: only the summary states that a packet is release-ready.
use crate::release_artifacts::{
    TARGETS, archive_name, archive_record, asset_names, disk_image_name, hash,
};
use crate::release_candidate::{
    DISTRIBUTION_GATES, EXECUTABLES, GATE_STATUSES, NOT_APPLICABLE, SCHEMA_VERSION, check_record,
    platform_gates, source_archive_name,
};
use crate::release_tag::ReleaseTag;
use crate::{Args, Result, ensure, read_json};
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    sync::LazyLock,
};

pub const RELEASE: &str = "release.json";
pub const SUMS: &str = "SHA256SUMS";
pub const NOTES: &str = "release-notes.md";
/// The shape of the public summary `release-verify --summary` writes.
pub const SUMMARY_SCHEMA_VERSION: u64 = 1;
const MACOS: &str = TARGETS[0].1;
/// macOS gates that test what people download, so only evidence from the distributed, notarized
/// app passes them, never the background test bundle the automated harness copies the editor into.
const DISTRIBUTED_GATES: &[&str] = &["minimum_os", "clean_installation"];
/// The longest evidence reference or scope a gate result records.
const TEXT_LIMIT: usize = 512;

/// One gate in `release.json`: a distribution gate, or a platform's qualification gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateRef {
    pub platform: Option<&'static str>,
    pub name: &'static str,
}

impl GateRef {
    /// `distribution.NAME` or `qualification.PLATFORM.NAME`, exactly as `release.json` lists them.
    pub fn parse(text: &str) -> Result<Self> {
        let parts: Vec<&str> = text.split('.').collect();
        let found = match parts.as_slice() {
            ["distribution", name] => DISTRIBUTION_GATES
                .iter()
                .copied()
                .find(|gate| gate == name)
                .map(|name| Self {
                    platform: None,
                    name,
                }),
            ["qualification", platform, name] => TARGETS
                .iter()
                .map(|(_, p)| *p)
                .find(|p| p == platform)
                .and_then(|platform| {
                    platform_gates(platform)
                        .ok()?
                        .iter()
                        .copied()
                        .find(|gate| gate == name)
                        .map(|name| Self {
                            platform: Some(platform),
                            name,
                        })
                }),
            _ => None,
        };
        found.ok_or_else(|| {
            format!(
                "Unknown gate {text}: gates are distribution.NAME or qualification.PLATFORM.NAME, as release.json lists them"
            )
            .into()
        })
    }

    pub fn label(&self) -> String {
        match self.platform {
            None => format!("distribution.{}", self.name),
            Some(platform) => format!("qualification.{platform}.{}", self.name),
        }
    }

    fn locate<'a>(&self, release: &'a Value) -> &'a Value {
        match self.platform {
            None => &release["distribution"][self.name],
            Some(platform) => &release["qualification"][platform][self.name],
        }
    }

    fn locate_mut<'a>(&self, release: &'a mut Value) -> &'a mut Value {
        match self.platform {
            None => &mut release["distribution"][self.name],
            Some(platform) => &mut release["qualification"][platform][self.name],
        }
    }

    /// Every gate a joint release passes, in `release.json` order.
    pub fn all() -> Vec<Self> {
        let mut gates: Vec<Self> = DISTRIBUTION_GATES
            .iter()
            .copied()
            .map(|name| Self {
                platform: None,
                name,
            })
            .collect();
        for &(_, platform) in TARGETS {
            gates.extend(
                platform_gates(platform)
                    .unwrap_or_default()
                    .iter()
                    .copied()
                    .map(|name| Self {
                        platform: Some(platform),
                        name,
                    }),
            );
        }
        gates
    }
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid privacy pattern")
}

static PRIVATE: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        // Home, temporary, volume and system directories; any absolute path; Windows drives and
        // shares; `~`, `$HOME` and file URLs.
        (
            "a private file path",
            r"(?i)(?:^|[^A-Za-z0-9])(?:/(?:Users|home|root|private|var|tmp|Volumes|mnt|media|run|opt|srv|etc)(?:/|\b)|~[/\\]|\$HOME\b|%USERPROFILE%|file:)",
        ),
        ("a private file path", r#"(?:^|[\s=:(\['"])/[^\s/]+/"#),
        ("a private file path", r"(?i)(?:^|[^A-Za-z0-9])[A-Z]:[\\/]"),
        ("a private file path", r"\\\\[^\s\\]+\\"),
        (
            "coordinates",
            r"-?\b\d{1,3}\.\d{3,}\s*[,;]\s*-?\d{1,3}\.\d{3,}",
        ),
        ("coordinates", r"(?i)\b(?:latitude|longitude|gps)\b|\d\s*°"),
        (
            "a photo file name",
            r"(?i)[A-Za-z0-9_-]\.(?:nef|nrw|raf|dng|cr2|cr3|crw|arw|srf|sr2|orf|rw2|pef|srw|3fr|iiq|x3f|erf|mef|mos|kdc|dcr|rwl|raw|jpe?g|heic|heif|hif|avif|tiff?|png|webp|gif|bmp|psd|xmp)\b",
        ),
        (
            "a photo file name",
            r"(?i)(?:^|[^A-Za-z0-9])_?(?:DSC|DSCF|DSCN|DSF|IMG|DJI|PXL|GOPR|MVIMG)_?\d{3,}",
        ),
        ("a secret", r"-----BEGIN"),
        (
            "a secret",
            r"(?i)\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_\w{20,}|AKIA[0-9A-Z]{16}|xox[abprs]-[A-Za-z0-9-]{10,}|sk-[A-Za-z0-9_-]{20,})",
        ),
        (
            "a secret",
            r"(?i)(?:password|passwd|secret|token|api[_-]?key|private[_ -]?key|credential)s?\s*[:=]",
        ),
        // An Apple app-specific password.
        ("a secret", r"\b[a-z]{4}-[a-z]{4}-[a-z]{4}-[a-z]{4}\b"),
        (
            "an email address",
            r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+",
        ),
    ]
    .into_iter()
    .map(|(what, pattern)| (what, regex(pattern)))
    .collect()
});

static TOKEN: LazyLock<Regex> = LazyLock::new(|| regex(r"[A-Za-z0-9+]{32,}={0,2}"));

/// A long key-like token: mixed-case letters and digits, not a hexadecimal hash or commit.
fn is_key_like(token: &str) -> bool {
    let token = token.trim_end_matches('=');
    !token.bytes().all(|b| b.is_ascii_hexdigit())
        && token.bytes().any(|b| b.is_ascii_uppercase())
        && token.bytes().any(|b| b.is_ascii_lowercase())
        && token.bytes().any(|b| b.is_ascii_digit())
}

/// What private data `text` appears to contain: file paths, coordinates, photo file names, secrets
/// or email addresses. Detection is deliberately broad; public text is reworded rather than
/// published with something that looks private.
pub fn privacy_problems(text: &str) -> Vec<&'static str> {
    let mut found = BTreeSet::new();
    for (what, pattern) in PRIVATE.iter() {
        if pattern.is_match(text) {
            found.insert(*what);
        }
    }
    if TOKEN
        .find_iter(text)
        .any(|token| is_key_like(token.as_str()))
    {
        found.insert("a secret");
    }
    found.into_iter().collect()
}

fn is_public_url(text: &str) -> bool {
    text.strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains(char::is_whitespace))
}

/// A gate's evidence reference or scope: one line, short, with nothing private in it. Evidence is a
/// reference inside the private evidence store, such as `m4-full-2026-10-20/summary.json`, or an
/// `https` URL, never a file path.
fn text_problems(label: &str, field: &str, text: Option<&Value>, reference: bool) -> Vec<String> {
    let Some(text) = text
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    else {
        return vec![format!("{label} records no {field}")];
    };
    let mut problems = Vec::new();
    if text.len() > TEXT_LIMIT || text.chars().any(char::is_control) {
        problems.push(format!(
            "{label}'s {field} is not one line of at most {TEXT_LIMIT} bytes"
        ));
    }
    if reference
        && !is_public_url(text)
        && (text.starts_with(['/', '\\', '~'])
            || text.contains(':')
            || text.split(['/', '\\']).any(|part| part == ".."))
    {
        problems.push(format!(
            "{label}'s evidence is neither a reference inside the private evidence store nor an https URL"
        ));
    }
    for what in privacy_problems(text) {
        problems.push(format!("{label}'s {field} names {what}"));
    }
    problems
}

fn is_hash(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// Why a gate result is malformed or does not belong to this candidate. `target` is the build
/// record of a qualification gate's platform, which its executable hashes must match. A gate that
/// has not run records only its status; any other result records its evidence and scope, a
/// distribution gate its disposition, and a qualification gate the executables its evidence ran.
pub fn gate_problems(gate: &GateRef, value: &Value, target: Option<&Value>) -> Vec<String> {
    let label = gate.label();
    let Some(object) = value.as_object() else {
        return vec![format!("release.json has no {label} result")];
    };
    let Some(status) = object
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| GATE_STATUSES.contains(status))
    else {
        return vec![format!("{label} has no status of {GATE_STATUSES:?}")];
    };
    if status == "not-run" {
        return if object.len() == 1 {
            Vec::new()
        } else {
            vec![format!("{label} has not run, so it records no result")]
        };
    }
    let macos = gate.platform == Some(MACOS);
    let mut allowed = vec!["status", "evidence", "scope"];
    match gate.platform {
        None => allowed.push("disposition"),
        Some(_) => {
            allowed.extend(["executables", "executable_identity"]);
            if macos {
                allowed.push("bundle");
            }
        }
    }
    let mut problems: Vec<String> = object
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .map(|key| match gate.platform {
            Some(platform) if platform != MACOS && key == "bundle" => format!(
                "{label} records an Apple bundle context, which does not apply to {platform}"
            ),
            _ => format!("{label} records a field that does not apply to it: {key}"),
        })
        .collect();
    problems.extend(text_problems(
        &label,
        "evidence",
        object.get("evidence"),
        true,
    ));
    problems.extend(text_problems(&label, "scope", object.get("scope"), false));
    let Some(platform) = gate.platform else {
        match object.get("disposition").and_then(Value::as_str) {
            Some("distribution") => {}
            Some("development-only") if status == "passed" => problems.push(format!(
                "{label} is a development-only disposition, which never passes a distribution gate"
            )),
            Some("development-only") => {}
            _ => problems.push(format!(
                "{label} records no disposition of distribution or development-only"
            )),
        }
        return problems;
    };
    let identity = object.get("executable_identity").and_then(Value::as_str);
    let key = match identity {
        Some("final") => "final",
        Some("before-signing") if macos => "before_signing",
        Some("before-signing") => {
            problems.push(format!(
                "{label} binds before-signing hashes, but {platform} executables are not signed: evidence binds their final hashes"
            ));
            return problems;
        }
        _ => {
            problems.push(format!(
                "{label} records no executable identity of final or before-signing"
            ));
            return problems;
        }
    };
    if macos {
        let bundle = object.get("bundle").and_then(Value::as_str);
        match bundle {
            Some("distributed") if key != "final" => problems.push(format!(
                "{label} tested the distributed app, which carries final signed executables, not before-signing ones"
            )),
            Some("distributed") => {}
            Some("background-test") if DISTRIBUTED_GATES.contains(&gate.name) => problems.push(
                format!(
                    "{label} needs evidence from the distributed app, not the background test bundle"
                ),
            ),
            Some("background-test") => {}
            _ => problems.push(format!(
                "{label} records no bundle of distributed or background-test"
            )),
        }
    }
    let Some(executables) = object
        .get("executables")
        .and_then(Value::as_object)
        .filter(|hashes| !hashes.is_empty())
    else {
        problems.push(format!(
            "{label} binds no executable hashes to the evidence"
        ));
        return problems;
    };
    if !executables.contains_key(EXECUTABLES[0]) {
        problems.push(format!("{label} binds no {} hash", EXECUTABLES[0]));
    }
    let Some(target) = target else {
        problems.push(format!(
            "{label} has no {platform} build record to bind its evidence to"
        ));
        return problems;
    };
    for (name, value) in executables {
        if !EXECUTABLES.contains(&name.as_str()) || !is_hash(value) {
            problems.push(format!(
                "{label} binds something other than an executable's SHA-256: {name}"
            ));
        } else if target["executables"][key][name] != *value {
            problems.push(format!(
                "{label}'s {name} hash is not this candidate's {} hash: its evidence ran another build",
                identity.unwrap_or_default()
            ));
        }
    }
    problems
}

/// What `verify` found. `integrity` lists why the packet's files and records disagree or are
/// malformed, `readiness` why a consistent packet is not ready. A packet is ready only when both
/// are empty.
#[derive(Debug, Default)]
pub struct Verdict {
    pub integrity: Vec<String>,
    pub readiness: Vec<String>,
    pub release: Option<Value>,
    pub release_sha256: Option<String>,
}

impl Verdict {
    pub fn ready(&self) -> bool {
        self.release.is_some() && self.integrity.is_empty() && self.readiness.is_empty()
    }

    pub fn reasons(&self) -> Vec<String> {
        self.integrity
            .iter()
            .chain(&self.readiness)
            .cloned()
            .collect()
    }
}

fn parse_sums(text: &str, verdict: &mut Verdict) -> BTreeMap<String, String> {
    let mut sums = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let parsed = line.split_once("  ").filter(|(digest, name)| {
            is_hash(&json!(digest))
                && !name.is_empty()
                && !name.contains(['/', '\\'])
                && !matches!(*name, "." | ".." | SUMS)
        });
        match parsed {
            Some((digest, name)) => {
                if sums.insert(name.to_owned(), digest.to_owned()).is_some() {
                    verdict
                        .integrity
                        .push(format!("SHA256SUMS lists {name} more than once"));
                }
            }
            None => verdict.integrity.push(format!(
                "SHA256SUMS line {} is not `SHA256  NAME` for a file in the packet",
                index + 1
            )),
        }
    }
    sums
}

fn target_record<'a>(release: &'a Value, target: &str) -> Option<&'a Value> {
    release["targets"]
        .as_array()?
        .iter()
        .find(|record| record["target"] == target)
}

/// Checks a packet and reports every reason it is not release-ready. It reads only the packet and
/// changes nothing.
pub fn verify(packet: &Path) -> Verdict {
    let mut verdict = Verdict::default();
    if !packet.is_dir() {
        verdict
            .integrity
            .push(format!("{} is not a packet directory", packet.display()));
        return verdict;
    }
    // The packet's own files, then SHA256SUMS against them.
    let mut present = BTreeSet::new();
    match fs::read_dir(packet) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.path().is_dir() {
                    verdict
                        .integrity
                        .push(format!("The packet holds a directory, {name}"));
                } else {
                    present.insert(name);
                }
            }
        }
        Err(error) => {
            verdict
                .integrity
                .push(format!("The packet cannot be read: {error}"));
            return verdict;
        }
    }
    let sums = match fs::read_to_string(packet.join(SUMS)) {
        Ok(text) => parse_sums(&text, &mut verdict),
        Err(_) => {
            verdict.integrity.push("Missing SHA256SUMS".into());
            BTreeMap::new()
        }
    };
    for (name, digest) in &sums {
        if !present.contains(name) {
            verdict
                .integrity
                .push(format!("Missing {name}, which SHA256SUMS lists"));
        } else if hash(&packet.join(name)).ok().as_ref() != Some(digest) {
            verdict
                .integrity
                .push(format!("{name} does not match its SHA256SUMS hash"));
        }
    }
    let release = match read_json(&packet.join(RELEASE)) {
        Ok(release) => release,
        Err(_) => {
            verdict
                .integrity
                .push("Missing or unreadable release.json".into());
            return verdict;
        }
    };
    verdict.release_sha256 = hash(&packet.join(RELEASE)).ok();
    let tag = match release["tag"].as_str().map(ReleaseTag::parse) {
        Some(Ok(tag)) => tag,
        _ => {
            verdict
                .integrity
                .push("release.json names no valid release tag".into());
            verdict.release = Some(release);
            return verdict;
        }
    };
    check_release_identity(&release, &tag, &mut verdict);
    // Every file the packet must hold is covered by SHA256SUMS; nothing else is in the packet but
    // SHA256SUMS itself and the update record, which is published beside it.
    let mut expected = vec![RELEASE.to_owned(), NOTES.to_owned()];
    for &(target, platform) in TARGETS {
        if target_record(&release, target).is_some() {
            expected.push(format!("{platform}.json"));
            expected.extend(asset_names(&tag, target).unwrap_or_default());
        }
    }
    for name in &expected {
        if !sums.contains_key(name) {
            verdict.integrity.push(if present.contains(name) {
                format!("SHA256SUMS does not cover {name}")
            } else {
                format!("Missing {name}")
            });
        }
    }
    let update_record = format!("luxforge-{}-updates.json", tag.version);
    for name in &present {
        if name != SUMS && *name != update_record && !sums.contains_key(name) {
            verdict
                .integrity
                .push(format!("{name} is in the packet but not in SHA256SUMS"));
        }
    }
    check_targets(packet, &release, &tag, &mut verdict);
    check_source_archive(packet, &release, &tag, &sums, &present, &mut verdict);
    check_gates(&release, &mut verdict);
    verdict.release = Some(release);
    verdict
}

fn check_release_identity(release: &Value, tag: &ReleaseTag, verdict: &mut Verdict) {
    if release["schema_version"] != SCHEMA_VERSION {
        verdict.integrity.push(format!(
            "release.json is not schema version {SCHEMA_VERSION}"
        ));
    }
    if release["version"] != tag.version {
        verdict.integrity.push(format!(
            "release.json's version is not {}, the version {} names",
            tag.version, tag.tag
        ));
    }
    if release["status"] != "build-candidate" || release["release_ready"] != false {
        verdict.integrity.push(
            "release.json claims more than a build candidate: readiness is stated only by the verifier's summary".into(),
        );
    }
    for field in ["revision", "lock_sha256", "build_number"] {
        if !release[field].is_string() {
            verdict
                .integrity
                .push(format!("release.json records no common {field}"));
        }
    }
}

fn check_targets(packet: &Path, release: &Value, tag: &ReleaseTag, verdict: &mut Verdict) {
    let records = release["targets"].as_array().cloned().unwrap_or_default();
    let shipping: Vec<&str> = TARGETS.iter().map(|(target, _)| *target).collect();
    let bound: BTreeSet<&str> = records
        .iter()
        .filter_map(|record| record["target"].as_str())
        .filter(|target| shipping.contains(target))
        .collect();
    if bound.len() != records.len() {
        verdict
            .integrity
            .push("release.json binds a build that is not one shipping target".into());
    }
    let mut order = Vec::new();
    for &(target, platform) in TARGETS {
        let Some(index) = records.iter().position(|record| record["target"] == target) else {
            verdict.readiness.push(format!(
                "No {platform} build: a one-platform packet is never ready for the joint release"
            ));
            continue;
        };
        order.push(index);
        let record = &records[index];
        if let Err(error) = check_record(record, tag, target) {
            verdict
                .integrity
                .push(format!("{platform}'s build record: {error}"));
        }
        for field in ["tag", "version", "revision", "lock_sha256", "build_number"] {
            if record[field] != release[field] {
                verdict
                    .integrity
                    .push(format!("{platform}'s {field} differs from release.json's"));
            }
        }
        match read_json(&packet.join(format!("{platform}.json"))) {
            Ok(copy) if copy == *record => {}
            Ok(_) => verdict.integrity.push(format!(
                "{platform}.json differs from release.json's {platform} build record"
            )),
            Err(_) => {} // Reported as a missing file above.
        }
        check_assets(packet, record, tag, target, platform, verdict);
        if record["rehearsal"] != false {
            verdict
                .readiness
                .push(format!("{platform} is a labelled local rehearsal"));
        }
        if record["working_tree_dirty"] != false {
            verdict
                .readiness
                .push(format!("{platform} was built from a dirty tree"));
        }
        check_signing(record, platform, verdict);
    }
    if !order.is_sorted() {
        verdict
            .integrity
            .push("release.json's targets are not in shipping order".into());
    }
}

fn check_assets(
    packet: &Path,
    record: &Value,
    tag: &ReleaseTag,
    target: &str,
    platform: &str,
    verdict: &mut Verdict,
) {
    let mut assets = vec![("archive", archive_name(tag, target).ok())];
    let image = disk_image_name(tag, target).ok().flatten();
    if image.is_none() && record.get("disk_image").is_some() {
        verdict.integrity.push(format!(
            "{platform} records a disk image, which only macOS has"
        ));
    }
    if image.is_some() {
        assets.push(("disk_image", image));
    }
    for (field, name) in assets {
        let Some(name) = name else { continue };
        let path = packet.join(&name);
        if path.is_file() && archive_record(&path).ok().as_ref() != Some(&record[field]) {
            verdict.integrity.push(format!(
                "{name} does not match {platform}'s build record (name, size or SHA-256)"
            ));
        }
    }
}

fn check_signing(record: &Value, platform: &str, verdict: &mut Verdict) {
    let signing = &record["signing"];
    if platform != MACOS {
        if *signing != json!({"status": NOT_APPLICABLE}) {
            verdict.readiness.push(format!(
                "{platform} signing must be exactly not-applicable: Apple signing and notarization do not apply to it"
            ));
        }
        return;
    }
    let status = signing["status"].as_str().unwrap_or("missing");
    if status != "notarized" {
        verdict
            .readiness
            .push(format!("{platform} app signing is {status}, not notarized"));
    }
    let image = signing["disk_image"]["status"]
        .as_str()
        .unwrap_or("missing");
    if image != "notarized" {
        verdict.readiness.push(format!(
            "{platform} disk image signing is {image}, not notarized"
        ));
    }
    if status == "notarized"
        && !(signing["submission_id"].is_string()
            && signing["disk_image"]["submission_id"].is_string())
    {
        verdict.readiness.push(format!(
            "{platform} records no notary submission for its app and disk image"
        ));
    }
    let before = &record["executables"]["before_signing"];
    let after = &record["executables"]["final"];
    if !EXECUTABLES.iter().all(|name| is_hash(&after[*name])) {
        verdict.readiness.push(format!(
            "{platform} records no final signed executable hashes"
        ));
    } else if EXECUTABLES.iter().any(|name| after[*name] == before[*name]) {
        verdict.readiness.push(format!(
            "{platform}'s final executable hashes repeat its before-signing ones"
        ));
    }
}

fn check_source_archive(
    packet: &Path,
    release: &Value,
    tag: &ReleaseTag,
    sums: &BTreeMap<String, String>,
    present: &BTreeSet<String>,
    verdict: &mut Verdict,
) {
    let name = source_archive_name(tag);
    let archive = &release["source_archive"];
    if archive.is_null() {
        verdict.readiness.push(format!(
            "release.json records no complete source archive, {name}"
        ));
        return;
    }
    if archive["name"] != name {
        verdict
            .integrity
            .push(format!("release.json's source archive is not named {name}"));
    }
    if !present.contains(&name) {
        verdict.integrity.push(format!("Missing {name}"));
        return;
    }
    if !sums.contains_key(&name) {
        verdict
            .integrity
            .push(format!("SHA256SUMS does not cover {name}"));
    }
    if archive_record(&packet.join(&name)).ok().as_ref() != Some(archive) {
        verdict.integrity.push(format!(
            "{name} does not match release.json's source archive (name, size or SHA-256)"
        ));
    }
}

fn check_gates(release: &Value, verdict: &mut Verdict) {
    let expected = GateRef::all();
    let listed: usize = release["distribution"].as_object().map_or(0, Map::len)
        + release["qualification"].as_object().map_or(0, |platforms| {
            platforms
                .values()
                .map(|gates| gates.as_object().map_or(1, Map::len))
                .sum()
        });
    if listed != expected.len()
        || release["qualification"]
            .as_object()
            .is_none_or(|platforms| platforms.len() != TARGETS.len())
    {
        verdict.integrity.push(
            "release.json lists other gates than the distribution and both platforms' qualification gates".into(),
        );
    }
    for gate in expected {
        let target = gate.platform.and_then(|platform| {
            let target = TARGETS.iter().find(|(_, p)| *p == platform)?.0;
            target_record(release, target)
        });
        let value = gate.locate(release);
        let status = value["status"].as_str();
        if status != Some("passed") {
            verdict.readiness.push(format!(
                "{} is {}",
                gate.label(),
                status.unwrap_or("missing")
            ));
        }
        verdict
            .integrity
            .extend(gate_problems(&gate, value, target));
    }
}

/// The public summary of a verified packet: its identity, final assets and per-gate status and
/// scope, with every evidence reference but an `https` URL replaced by `private`. It refuses, rather
/// than writes, a summary that still appears to name a private path, coordinates, a photo, a secret
/// or an email address.
pub fn summary(verdict: &Verdict) -> Result<Value> {
    let release = verdict
        .release
        .as_ref()
        .ok_or("No readable release.json to summarize")?;
    let ready = verdict.ready();
    let pick = |value: &Value, keys: &[&str]| -> Value {
        keys.iter()
            .filter_map(|key| Some(((*key).to_owned(), value.get(*key)?.clone())))
            .collect::<Map<_, _>>()
            .into()
    };
    let gate = |value: &Value| -> Value {
        let mut public = pick(
            value,
            &[
                "status",
                "scope",
                "disposition",
                "bundle",
                "executable_identity",
                "executables",
            ],
        );
        if let Some(evidence) = value["evidence"].as_str() {
            public["evidence"] = json!(if is_public_url(evidence) {
                evidence
            } else {
                "private"
            });
        }
        public
    };
    let targets: Vec<Value> = release["targets"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|record| {
            let mut target = pick(
                record,
                &[
                    "target",
                    "platform",
                    "catalog_format",
                    "minimum_system_version",
                    "rehearsal",
                    "working_tree_dirty",
                    "toolchain",
                    "build_environment",
                    "executables",
                ],
            );
            target["signing"] = json!({
                "status": record["signing"]["status"],
            });
            if let Some(image) = record["signing"].get("disk_image") {
                target["signing"]["disk_image"] = image["status"].clone();
            }
            target["assets"] = ["archive", "disk_image"]
                .iter()
                .filter_map(|field| record.get(*field))
                .map(|asset| pick(asset, &["name", "size", "sha256"]))
                .collect();
            target
        })
        .collect();
    let gates = |value: &Value| -> Value {
        value
            .as_object()
            .map(|gates| {
                gates
                    .iter()
                    .map(|(name, value)| (name.clone(), gate(value)))
                    .collect::<Map<_, _>>()
            })
            .unwrap_or_default()
            .into()
    };
    let qualification: Map<String, Value> = release["qualification"]
        .as_object()
        .map(|platforms| {
            platforms
                .iter()
                .map(|(platform, value)| (platform.clone(), gates(value)))
                .collect()
        })
        .unwrap_or_default();
    let value = json!({
        "schema_version": SUMMARY_SCHEMA_VERSION,
        "kind": "luxforge-release-summary",
        "tag": release["tag"],
        "version": release["version"],
        "revision": release["revision"],
        "lock_sha256": release["lock_sha256"],
        "build_number": release["build_number"],
        "release_json_sha256": verdict.release_sha256,
        "release_ready": ready,
        "status": if ready { "release-ready" } else { "not-ready" },
        "reasons": verdict.reasons(),
        "source_archive": if release["source_archive"].is_object() {
            pick(&release["source_archive"], &["name", "size", "sha256"])
        } else {
            Value::Null
        },
        "targets": targets,
        "distribution": gates(&release["distribution"]),
        "qualification": qualification,
    });
    let mut leaks = Vec::new();
    find_leaks(&value, "", &mut leaks);
    ensure(
        leaks.is_empty(),
        format!(
            "The public summary would publish private data, so it is not written:\n{}",
            bullets(&leaks)
        ),
    )?;
    Ok(value)
}

fn find_leaks(value: &Value, pointer: &str, leaks: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            for what in privacy_problems(text) {
                leaks.push(format!("{pointer} names {what}"));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                find_leaks(item, &format!("{pointer}/{index}"), leaks);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                for what in privacy_problems(key) {
                    leaks.push(format!("{pointer}: a key names {what}"));
                }
                find_leaks(item, &format!("{pointer}/{key}"), leaks);
            }
        }
        _ => {}
    }
}

fn bullets(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn replace_file(path: &Path, bytes: &[u8]) -> Result {
    let name = path
        .file_name()
        .ok_or("A packet file has no name")?
        .to_string_lossy();
    let partial = path.with_file_name(format!(".{name}.partial"));
    fs::write(&partial, bytes)?;
    fs::rename(&partial, path)?;
    Ok(())
}

/// Records `result` as `gate`'s result in the packet's `release.json` and rewrites its
/// `SHA256SUMS` line. It refuses an inconsistent packet and a malformed result, or one whose
/// executable hashes are not this candidate's, and leaves the packet as it was.
pub fn record_gate(packet: &Path, gate: &GateRef, result: Value) -> Result {
    let before = verify(packet);
    ensure(
        before.integrity.is_empty(),
        format!(
            "The packet is inconsistent, so no gate result is recorded:\n{}",
            bullets(&before.integrity)
        ),
    )?;
    let mut release = before.release.ok_or("No release.json")?;
    let target = gate.platform.and_then(|platform| {
        let target = TARGETS.iter().find(|(_, p)| *p == platform)?.0;
        target_record(&release, target).cloned()
    });
    let problems = gate_problems(gate, &result, target.as_ref());
    ensure(
        problems.is_empty(),
        format!("The gate result is refused:\n{}", bullets(&problems)),
    )?;
    *gate.locate_mut(&mut release) = result;
    let text = format!("{}\n", serde_json::to_string_pretty(&release)?);
    let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
    let sums: String = fs::read_to_string(packet.join(SUMS))?
        .lines()
        .map(|line| match line.split_once("  ") {
            Some((_, RELEASE)) => format!("{digest}  {RELEASE}\n"),
            _ => format!("{line}\n"),
        })
        .collect();
    replace_file(&packet.join(RELEASE), text.as_bytes())?;
    replace_file(&packet.join(SUMS), sums.as_bytes())?;
    let after = verify(packet);
    ensure(
        after.integrity.is_empty(),
        format!(
            "The recorded packet is inconsistent:\n{}",
            bullets(&after.integrity)
        ),
    )
}

fn text(value: std::ffi::OsString, what: &str) -> Result<String> {
    value
        .into_string()
        .map_err(|_| format!("{what} is not valid UTF-8").into())
}

/// `release-gate --packet DIR --gate GATE --status STATUS [--evidence REF --scope TEXT]
/// [--disposition distribution|development-only] [--bundle distributed|background-test]
/// [--executable-identity final|before-signing] [--executable NAME=SHA256]...`
pub fn run_gate(args: &mut Args) -> Result {
    let packet = args.path("--packet")?;
    let gate = GateRef::parse(&text(
        args.value("--gate")?.ok_or("Required: --gate")?,
        "--gate",
    )?)?;
    let status = text(
        args.value("--status")?.ok_or("Required: --status")?,
        "--status",
    )?;
    let mut result = json!({"status": status});
    for (flag, key) in [
        ("--evidence", "evidence"),
        ("--scope", "scope"),
        ("--disposition", "disposition"),
        ("--bundle", "bundle"),
        ("--executable-identity", "executable_identity"),
    ] {
        if let Some(value) = args.value(flag)? {
            result[key] = json!(text(value, flag)?);
        }
    }
    let mut executables = Map::new();
    while let Some(value) = args.value("--executable")? {
        let value = text(value, "--executable")?;
        let (name, digest) = value.split_once('=').ok_or("--executable is NAME=SHA256")?;
        ensure(
            executables
                .insert(name.to_owned(), json!(digest.to_ascii_lowercase()))
                .is_none(),
            format!("{name} is bound more than once"),
        )?;
    }
    if !executables.is_empty() {
        result["executables"] = executables.into();
    }
    args.done()?;
    record_gate(&packet, &gate, result)?;
    println!(
        "Recorded {} as {status} in {}; SHA256SUMS updated.",
        gate.label(),
        packet.join(RELEASE).display()
    );
    Ok(())
}

/// `release-verify --packet DIR [--summary NEW_FILE]`. Exits successfully only for a ready packet;
/// otherwise it fails with every reason. `--summary` writes the public summary, ready or not,
/// outside the packet.
pub fn run_verify(args: &mut Args) -> Result {
    let packet = args.path("--packet")?;
    let summary_path = args.value("--summary")?.map(std::path::PathBuf::from);
    args.done()?;
    if let Some(path) = &summary_path {
        ensure(!path.exists(), "The summary file must be new")?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let inside = match (parent.canonicalize(), packet.canonicalize()) {
            (Ok(parent), Ok(packet)) => parent.starts_with(packet),
            (Err(error), _) => return Err(format!("The summary's directory: {error}").into()),
            (_, Err(_)) => false,
        };
        ensure(
            !inside,
            "The summary is written outside the packet, whose files SHA256SUMS covers exactly",
        )?;
    }
    let verdict = verify(&packet);
    let mut refusal = None;
    if let Some(path) = &summary_path {
        match summary(&verdict) {
            Ok(value) => {
                fs::write(path, format!("{}\n", serde_json::to_string_pretty(&value)?))?;
                println!("Wrote the public summary to {}", path.display());
            }
            Err(error) => refusal = Some(error.to_string()),
        }
    }
    let reasons = verdict.reasons();
    ensure(
        verdict.ready() && refusal.is_none(),
        format!(
            "{}The packet is {}release-ready ({} reasons):\n{}",
            refusal.map(|r| format!("{r}\n")).unwrap_or_default(),
            if verdict.ready() { "" } else { "not " },
            reasons.len(),
            bullets(&reasons)
        ),
    )?;
    println!(
        "Release-ready: {} at {}, every distribution and qualification gate passed",
        verdict
            .release
            .as_ref()
            .map_or("", |r| r["tag"].as_str().unwrap_or("")),
        verdict
            .release
            .as_ref()
            .map_or("", |r| r["revision"].as_str().unwrap_or(""))
    );
    Ok(())
}

#[cfg(test)]
mod tests;
