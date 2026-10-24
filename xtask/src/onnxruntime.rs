//! The prebuilt ONNX Runtime archives a build links, pinned: what `cargo xtask check-repository`
//! holds them to, and `cargo xtask onnxruntime-attestation`, which fetches each one and verifies its
//! hash and its published build attestation.
//!
//! `luxforge-onnx` links ONNX Runtime statically through `ort`. With `download-binaries`, the
//! `ort-sys` build script picks the archive for the target and the feature set from the table in
//! its own source (`build/download/dist.tsv`), downloads it from pyke's CDN, and refuses it unless
//! its SHA-256 is the table's; the extracted library is cached under a directory named by that hash
//! and reused. The lockfile's checksum of `ort-sys` pins the table. This check pins the rest: the
//! exact `ort` and `ort-sys` packages, the features that choose an archive (and none that would let
//! a build take another, link a system library or skip the download), the archive each target the
//! project builds may use with its version, URL, length and SHA-256, and that the resolved `ort-sys`
//! table still names exactly those. pyke publishes a GitHub build attestation (SLSA provenance v1,
//! signed through Sigstore) for each archive since `ort` 2.0.0-rc.12; [`ATTESTATION`] records the
//! one each pinned archive carries, which the attestation command verifies with `gh attestation
//! verify` (network and the GitHub CLI; not part of `check-repository`).
use crate::*;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// The `ort` and `ort-sys` release, and the lockfile checksum of each.
pub const ORT_VERSION: &str = "2.0.0-rc.13";
const ORT_CHECKSUM: &str = "4336a1e2b38848325241c72889086886004e589b7c74f335e60a8e8db5138a0b";
const ORT_SYS_CHECKSUM: &str = "cf211e3776eea6aec988552fa118dd746d70e1b1e5e244058d1c98015f3e5872";
/// The ONNX Runtime release the archives hold.
pub const ONNXRUNTIME_VERSION: &str = "1.28.0";

/// One prebuilt archive a build may link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Archive {
    /// The Rust target it is for.
    pub target: &'static str,
    /// The `ort-sys` feature set that selects it: the platform's execution provider.
    pub features: &'static [&'static str],
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// The archive of every target the project builds (`deny.toml`'s graph targets), as `ort-sys`
/// 2.0.0-rc.13 names them and as fetched and measured on 2026-10-11 (aarch64 Linux on 2026-10-24).
pub const ARCHIVES: &[Archive] = &[
    Archive {
        target: "aarch64-apple-darwin",
        features: &["coreml"],
        url: "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/aarch64-apple-darwin+coreml.tar.lzma2",
        bytes: 9_239_743,
        sha256: "6934874e2e953576d9c1db47ff1af39c62c4f4220dbe6f988e131f72879674c7",
    },
    Archive {
        target: "x86_64-pc-windows-msvc",
        features: &["directml"],
        url: "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/x86_64-pc-windows-msvc+directml.tar.lzma2",
        bytes: 31_076_494,
        sha256: "f7c654b3729cb9e5ad2a36a0c38e5b48e63bf4eed22968931aed33a0ad0b527d",
    },
    Archive {
        target: "x86_64-unknown-linux-gnu",
        features: &[],
        url: "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/x86_64-unknown-linux-gnu.tar.lzma2",
        bytes: 10_060_013,
        sha256: "e454f710f8a49f53aa5b4ff51e3454ae1835777e431c6c35c5255ce6f205fd68",
    },
    Archive {
        target: "aarch64-unknown-linux-gnu",
        features: &[],
        url: "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/aarch64-unknown-linux-gnu.tar.lzma2",
        bytes: 10_548_001,
        sha256: "06a050ab9137ccb32421d0cb49e9ccf72d9e18ab0aeb8f8d038d1b5cc844b35a",
    },
];

/// The build attestation every pinned archive carries: who signed it, from which workflow and
/// source revision, and the run that built it.
pub struct Attestation {
    /// The GitHub owner `gh attestation verify --owner` names.
    pub owner: &'static str,
    /// The workflow that signed it, `gh attestation verify --signer-workflow`.
    pub signer_workflow: &'static str,
    /// The source revision it was built from, `--source-digest`.
    pub source_digest: &'static str,
    pub predicate_type: &'static str,
    /// The workflow run that built the archives, as each certificate records it.
    pub run: &'static str,
}

/// The attestation of the pinned archives, verified on 2026-10-11 for the first three and on
/// 2026-10-24 for aarch64 Linux.
pub const ATTESTATION: Attestation = Attestation {
    owner: "pykeio",
    signer_workflow: "pykeio/ort-artifacts/.github/workflows/build-runner.yml",
    source_digest: "25d68de62d14300838a44c104261e0a773d44f8d",
    predicate_type: "https://slsa.dev/provenance/v1",
    run: "https://github.com/pykeio/ort-artifacts/actions/runs/30151376321/attempts/1",
};

/// The `ort` features the workspace may turn on: none of them chooses another archive, links a
/// system or dynamic library, or loosens the archive match (`lax-feature-matching`,
/// `load-dynamic`, `pkg-config`, `alternative-backend`, `copy-dylibs` and every other provider are
/// refused).
const ALLOWED_FEATURES: &[&str] = &[
    "std",
    "download-binaries",
    "tls-native",
    "half",
    "api-17",
    "api-18",
    "api-19",
    "api-20",
    "api-21",
    "api-22",
    "api-23",
    "api-24",
    "api-25",
    "api-26",
    "api-27",
    "api-28",
];

/// The environment variables that point `ort-sys` at another library or skip the pinned download,
/// which no Cargo configuration in the repository may set.
const OVERRIDES: &[&str] = &[
    "ORT_LIB_LOCATION",
    "ORT_LIB_PATH",
    "ORT_PREFER_DYNAMIC_LINK",
    "ORT_IOS_XCFWK_LOCATION",
    "ORT_IOS_XCFWK_PATH",
];

/// Check the pins against the repository: the lockfile, the manifests, the Cargo configuration
/// and the resolved `ort-sys` table. Answers a summary line.
pub fn check(root: &Path) -> Result<String> {
    locked(&fs::read_to_string(root.join("Cargo.lock"))?)?;
    features(
        &fs::read_to_string(root.join("Cargo.toml"))?,
        &fs::read_to_string(root.join("crates/luxforge-onnx/Cargo.toml"))?,
    )?;
    let config = root.join(".cargo/config.toml");
    if config.is_file() {
        no_overrides(&fs::read_to_string(config)?)?;
    }
    targets(&fs::read_to_string(root.join("deny.toml"))?)?;
    let table = dist_table(root)?;
    archives(&fs::read_to_string(&table)?)?;
    Ok(format!(
        "ort {ORT_VERSION} and ONNX Runtime {ONNXRUNTIME_VERSION}: {} pinned archives by SHA-256, \
         matching {}",
        ARCHIVES.len(),
        table.display()
    ))
}

/// One `[[package]]` of the lockfile: name, version, source and checksum.
fn lock_packages(lock: &str) -> Vec<[String; 4]> {
    lock.split("[[package]]")
        .skip(1)
        .map(|block| {
            let field = |key: &str| {
                block
                    .lines()
                    .find_map(|line| line.strip_prefix(&format!("{key} = \"")))
                    .and_then(|rest| rest.strip_suffix('"'))
                    .unwrap_or_default()
                    .to_owned()
            };
            [
                field("name"),
                field("version"),
                field("source"),
                field("checksum"),
            ]
        })
        .collect()
}

/// The lockfile holds exactly one `ort` and one `ort-sys`, each the pinned crates.io release with
/// its pinned checksum.
pub fn locked(lock: &str) -> Result {
    let packages = lock_packages(lock);
    for (name, checksum) in [("ort", ORT_CHECKSUM), ("ort-sys", ORT_SYS_CHECKSUM)] {
        let found: Vec<_> = packages
            .iter()
            .filter(|package| package[0] == name)
            .collect();
        ensure(
            found.len() == 1,
            format!(
                "Cargo.lock holds {} {name} packages; exactly one is pinned",
                found.len()
            ),
        )?;
        let [_, version, source, sum] = found[0];
        ensure(
            version == ORT_VERSION,
            format!("Cargo.lock has {name} {version}; {ORT_VERSION} is pinned"),
        )?;
        ensure(
            source == "registry+https://github.com/rust-lang/crates.io-index",
            format!("Cargo.lock takes {name} from {source}, not crates.io"),
        )?;
        ensure(
            sum == checksum,
            format!("Cargo.lock's {name} checksum {sum} is not the pinned {checksum}"),
        )?;
    }
    Ok(())
}

/// Every `ort` declaration in a manifest: the table it is in and its features.
fn declarations(manifest: &str) -> Result<Vec<(String, BTreeSet<String>, String)>> {
    let features = Regex::new(r#"features\s*=\s*\[([^\]]*)\]"#)?;
    let quoted = Regex::new(r#""([^"]*)""#)?;
    let mut table = String::new();
    let mut found = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            table = line.to_owned();
            continue;
        }
        if !(line.starts_with("ort ") || line.starts_with("ort=") || line.starts_with("ort.")) {
            continue;
        }
        let named = features
            .captures(line)
            .map(|found| {
                quoted
                    .captures_iter(&found[1])
                    .map(|feature| feature[1].to_owned())
                    .collect()
            })
            .unwrap_or_default();
        found.push((table.clone(), named, line.to_owned()));
    }
    Ok(found)
}

/// The workspace declares `ort` once, exactly pinned, without its default features, with the
/// download and only [`ALLOWED_FEATURES`]; `luxforge-onnx` adds Core ML under macOS and DirectML
/// under Windows and nothing else, which are the feature sets the pinned archives were chosen by.
pub fn features(workspace: &str, onnx: &str) -> Result {
    let declared = declarations(workspace)?;
    let [(table, features, line)] = declared.as_slice() else {
        return Err(format!(
            "the workspace manifest declares ort {} times; it is declared once, in \
             [workspace.dependencies]",
            declared.len()
        )
        .into());
    };
    ensure(
        table == "[workspace.dependencies]",
        format!("ort is declared in {table}, not [workspace.dependencies]"),
    )?;
    ensure(
        line.contains(&format!("version = \"={ORT_VERSION}\"")),
        format!("ort is not pinned to ={ORT_VERSION}: {line}"),
    )?;
    ensure(
        line.contains("default-features = false"),
        format!("ort keeps its default features: {line}"),
    )?;
    ensure(
        features.contains("download-binaries"),
        "ort does not have its build fetch the pinned archive (download-binaries)",
    )?;
    for feature in features {
        ensure(
            ALLOWED_FEATURES.contains(&feature.as_str()),
            format!(
                "the workspace's ort feature {feature} could choose another archive or library"
            ),
        )?;
    }
    for (table, features, line) in declarations(onnx)? {
        let expected: &[&str] = match table.as_str() {
            "[dependencies]" => &[],
            "[target.'cfg(target_os = \"macos\")'.dependencies]" => &["coreml"],
            "[target.'cfg(target_os = \"windows\")'.dependencies]" => &["directml"],
            other => {
                return Err(format!("luxforge-onnx declares ort in {other}: {line}").into());
            }
        };
        ensure(
            line.contains("workspace = true"),
            format!("luxforge-onnx's ort is not the workspace's: {line}"),
        )?;
        let expected: BTreeSet<String> = expected.iter().map(|&f| f.to_owned()).collect();
        ensure(
            features == expected,
            format!(
                "luxforge-onnx's ort in {table} asks for {features:?}, not {expected:?}, which no \
                 pinned archive was chosen by"
            ),
        )?;
    }
    Ok(())
}

/// No Cargo configuration points `ort-sys` at another library.
pub fn no_overrides(config: &str) -> Result {
    for name in OVERRIDES {
        ensure(
            !config.contains(name),
            format!(
                ".cargo/config.toml sets {name}, which links a library other than the pinned \
                 archive"
            ),
        )?;
    }
    Ok(())
}

/// Every target the project builds has a pinned archive, and every pinned archive is one of them.
pub fn targets(deny: &str) -> Result {
    let section = deny
        .split("targets = [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .ok_or("deny.toml lists no graph targets")?;
    let quoted = Regex::new(r#""([^"]+)""#)?;
    let built: BTreeSet<_> = quoted
        .captures_iter(section)
        .map(|target| target[1].to_owned())
        .collect();
    let pinned: BTreeSet<_> = ARCHIVES
        .iter()
        .map(|archive| archive.target.to_owned())
        .collect();
    ensure(
        built == pinned,
        format!(
            "the targets built ({built:?}) and the targets with a pinned ONNX Runtime archive \
             ({pinned:?}) differ"
        ),
    )
}

/// The resolved `ort-sys` package's archive table.
fn dist_table(root: &Path) -> Result<PathBuf> {
    let metadata: Value = serde_json::from_str(&output(
        root,
        "cargo",
        &["metadata", "--locked", "--format-version", "1"],
    )?)?;
    let manifest = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata lists no packages")?
        .iter()
        .find(|package| package["name"] == "ort-sys" && package["version"] == ORT_VERSION)
        .and_then(|package| package["manifest_path"].as_str())
        .ok_or_else(|| format!("cargo metadata resolves no ort-sys {ORT_VERSION}"))?;
    Ok(Path::new(manifest)
        .parent()
        .ok_or("ort-sys manifest has no directory")?
        .join("build/download/dist.tsv"))
}

/// The `ort-sys` table names every pinned archive by its target and feature set, at its pinned URL
/// and SHA-256, for the pinned ONNX Runtime release.
pub fn archives(table: &str) -> Result {
    let rows: Vec<Vec<&str>> = table
        .lines()
        .skip(1)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split('\t').collect())
        .collect();
    for archive in ARCHIVES {
        ensure(
            archive.url.contains(&format!("/ms@{ONNXRUNTIME_VERSION}/"))
                && archive.url.starts_with("https://"),
            format!(
                "{}'s pinned URL is not ONNX Runtime {ONNXRUNTIME_VERSION}",
                archive.target
            ),
        )?;
        let features = if archive.features.is_empty() {
            "none".to_owned()
        } else {
            archive.features.join(",")
        };
        let row = rows
            .iter()
            .find(|row| row.len() == 4 && row[0] == archive.target && row[1] == features)
            .ok_or_else(|| {
                format!(
                    "ort-sys names no {} archive with features {features}",
                    archive.target
                )
            })?;
        ensure(
            row[2] == archive.url,
            format!(
                "ort-sys fetches {} from {}, not the pinned {}",
                archive.target, row[2], archive.url
            ),
        )?;
        ensure(
            row[3] == archive.sha256,
            format!(
                "ort-sys accepts the {} archive by SHA-256 {}, not the pinned {}",
                archive.target, row[3], archive.sha256
            ),
        )?;
    }
    Ok(())
}

/// `cargo xtask onnxruntime-attestation --output NEW`: fetch every pinned archive into `out`, check
/// its length and SHA-256 against the pin, and verify its build attestation with the GitHub CLI
/// against [`ATTESTATION`]: the owner, the signing workflow, the source revision and the predicate
/// type. Writes `attestation.json` with each archive's result. Needs the network, `curl` and `gh`.
pub fn attest(out: &Path) -> Result {
    ensure(!out.exists(), format!("{} already exists", out.display()))?;
    fs::create_dir_all(out)?;
    let mut results = Vec::new();
    for archive in ARCHIVES {
        let name = archive
            .url
            .rsplit('/')
            .next()
            .ok_or("archive URL has no name")?;
        let path = out.join(name);
        let fetched = Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
            ])
            .arg("--output")
            .arg(&path)
            .arg(archive.url)
            .status()?;
        ensure(
            fetched.success(),
            format!("could not fetch {}", archive.url),
        )?;
        verify_file(archive, &fs::read(&path)?)?;
        let verified = Command::new("gh")
            .args(["attestation", "verify"])
            .arg(&path)
            .args([
                "--owner",
                ATTESTATION.owner,
                "--signer-workflow",
                ATTESTATION.signer_workflow,
                "--source-digest",
                ATTESTATION.source_digest,
                "--predicate-type",
                ATTESTATION.predicate_type,
                "--format",
                "json",
            ])
            .output()?;
        ensure(
            verified.status.success(),
            format!(
                "the attestation of {name} does not verify: {}",
                String::from_utf8_lossy(&verified.stderr)
            ),
        )?;
        let answer: Value = serde_json::from_slice(&verified.stdout)?;
        attested(archive, &answer)?;
        println!(
            "PASS {name}: {} bytes, SHA-256 and attestation",
            archive.bytes
        );
        results.push(json!({
            "target": archive.target,
            "url": archive.url,
            "bytes": archive.bytes,
            "sha256": archive.sha256,
            "attestation": answer,
        }));
    }
    fs::write(
        out.join("attestation.json"),
        serde_json::to_vec_pretty(&json!({
            "ort": ORT_VERSION,
            "onnxruntime": ONNXRUNTIME_VERSION,
            "archives": results,
        }))?,
    )?;
    Ok(())
}

/// The fetched bytes are the pinned archive: its length and SHA-256.
pub fn verify_file(archive: &Archive, bytes: &[u8]) -> Result {
    ensure(
        bytes.len() as u64 == archive.bytes,
        format!(
            "the {} archive is {} bytes, not the pinned {}",
            archive.target,
            bytes.len(),
            archive.bytes
        ),
    )?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    ensure(
        digest == archive.sha256,
        format!(
            "the {} archive's SHA-256 is {digest}, not the pinned {}",
            archive.target, archive.sha256
        ),
    )
}

/// `gh attestation verify`'s answer names the archive's own digest as its subject, under the
/// recorded certificate: the signing workflow, the source revision and the run.
pub fn attested(archive: &Archive, answer: &Value) -> Result {
    let results = answer
        .as_array()
        .ok_or("the attestation answer is not a list")?;
    ensure(!results.is_empty(), "no attestation verified")?;
    for result in results {
        let verified = &result["verificationResult"];
        let subjects = verified["statement"]["subject"]
            .as_array()
            .ok_or("the attestation names no subject")?;
        ensure(
            subjects
                .iter()
                .any(|subject| subject["digest"]["sha256"] == archive.sha256),
            format!(
                "the attestation's subject is not the {} archive",
                archive.target
            ),
        )?;
        ensure(
            verified["statement"]["predicateType"] == ATTESTATION.predicate_type,
            "the attestation is not SLSA provenance v1",
        )?;
        let certificate = &verified["signature"]["certificate"];
        ensure(
            certificate["buildSignerURI"]
                .as_str()
                .is_some_and(|uri| uri.contains(ATTESTATION.signer_workflow)),
            format!(
                "the {} archive was signed by another workflow",
                archive.target
            ),
        )?;
        ensure(
            certificate["sourceRepositoryDigest"] == ATTESTATION.source_digest,
            format!(
                "the {} archive was built from another revision",
                archive.target
            ),
        )?;
        ensure(
            certificate["runInvocationURI"] == ATTESTATION.run,
            format!("the {} archive was built by another run", archive.target),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_test_macros::slow;

    fn root_file(name: &str) -> String {
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(name),
        )
        .unwrap()
    }

    fn refused(result: Result, says: &str) {
        let error = result.expect_err(says).to_string();
        assert!(error.contains(says), "{error}");
    }

    const LOCK: &str = "version = 4\n\n[[package]]\nname = \"ort\"\nversion = \"2.0.0-rc.13\"\n\
        source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
        checksum = \"4336a1e2b38848325241c72889086886004e589b7c74f335e60a8e8db5138a0b\"\n\n\
        [[package]]\nname = \"ort-sys\"\nversion = \"2.0.0-rc.13\"\n\
        source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
        checksum = \"cf211e3776eea6aec988552fa118dd746d70e1b1e5e244058d1c98015f3e5872\"\n";

    #[test]
    #[slow]
    fn the_repositorys_own_pins_hold() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        check(&root).unwrap();
    }

    #[test]
    fn the_lockfile_must_hold_the_pinned_ort_packages() {
        locked(LOCK).unwrap();
        refused(
            locked(&LOCK.replace("4336a1e2", "0000a1e2")),
            "Cargo.lock's ort checksum 0000a1e2",
        );
        refused(
            locked(&LOCK.replacen("2.0.0-rc.13", "2.0.0-rc.12", 1)),
            "Cargo.lock has ort 2.0.0-rc.12; 2.0.0-rc.13 is pinned",
        );
        refused(
            locked(&LOCK.replace(
                "registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"cf21",
                "git+https://example.invalid/ort\"\nchecksum = \"cf21",
            )),
            "Cargo.lock takes ort-sys from git+https://example.invalid/ort",
        );
        refused(
            locked(&LOCK.replace("name = \"ort-sys\"", "name = \"other\"")),
            "Cargo.lock holds 0 ort-sys packages",
        );
        refused(
            locked(&format!(
                "{LOCK}\n[[package]]\nname = \"ort\"\nversion = \"1.16.3\"\n"
            )),
            "Cargo.lock holds 2 ort packages",
        );
    }

    const WORKSPACE: &str = "[workspace.dependencies]\nserde = \"1\"\n\
        ort = { version = \"=2.0.0-rc.13\", default-features = false, features = [\"std\", \
        \"download-binaries\", \"tls-native\", \"api-24\", \"half\"] }\n";
    const ONNX: &str = "[dependencies]\nort.workspace = true\n\n\
        [target.'cfg(target_os = \"macos\")'.dependencies]\n\
        ort = { workspace = true, features = [\"coreml\"] }\n\n\
        [target.'cfg(target_os = \"windows\")'.dependencies]\n\
        ort = { workspace = true, features = [\"directml\"] }\n";

    #[test]
    fn the_features_that_choose_an_archive_are_pinned() {
        features(WORKSPACE, ONNX).unwrap();
        features(
            &root_file("Cargo.toml"),
            &root_file("crates/luxforge-onnx/Cargo.toml"),
        )
        .unwrap();
        for (feature, says) in [
            (
                "lax-feature-matching",
                "feature lax-feature-matching could choose another",
            ),
            ("load-dynamic", "feature load-dynamic"),
            ("pkg-config", "feature pkg-config"),
            ("cuda", "feature cuda"),
            ("copy-dylibs", "feature copy-dylibs"),
        ] {
            refused(
                features(
                    &WORKSPACE.replace("\"half\"", &format!("\"half\", \"{feature}\"")),
                    ONNX,
                ),
                says,
            );
        }
        refused(
            features(&WORKSPACE.replace("\"download-binaries\", ", ""), ONNX),
            "download-binaries",
        );
        refused(
            features(&WORKSPACE.replace("=2.0.0-rc.13", "2"), ONNX),
            "ort is not pinned to =2.0.0-rc.13",
        );
        refused(
            features(&WORKSPACE.replace("default-features = false, ", ""), ONNX),
            "ort keeps its default features",
        );
        refused(
            features(&format!("{WORKSPACE}\n[dependencies]\nort = \"2\"\n"), ONNX),
            "declares ort 2 times",
        );
        refused(
            features(
                WORKSPACE,
                &ONNX.replace("[\"coreml\"]", "[\"coreml\", \"webgpu\"]"),
            ),
            "asks for {\"coreml\", \"webgpu\"}",
        );
        refused(
            features(
                WORKSPACE,
                &ONNX.replace("[\"directml\"]", "[\"directml\", \"cuda\"]"),
            ),
            "which no pinned archive was chosen by",
        );
        refused(
            features(
                WORKSPACE,
                &format!(
                    "{ONNX}\n[target.'cfg(target_os = \"linux\")'.dependencies]\nort = {{ workspace = true, features = [\"cuda\"] }}\n"
                ),
            ),
            "luxforge-onnx declares ort in [target.'cfg(target_os = \"linux\")'.dependencies]",
        );
        refused(
            features(
                WORKSPACE,
                &ONNX.replace("ort.workspace = true", "ort = \"=2.0.0-rc.13\""),
            ),
            "is not the workspace's",
        );
    }

    #[test]
    fn no_configuration_points_the_build_at_another_library() {
        no_overrides(&root_file(".cargo/config.toml")).unwrap();
        refused(
            no_overrides("[env]\nORT_LIB_LOCATION = \"/opt/onnxruntime\"\n"),
            "sets ORT_LIB_LOCATION",
        );
        refused(
            no_overrides("[env]\nORT_PREFER_DYNAMIC_LINK = \"1\"\n"),
            "sets ORT_PREFER_DYNAMIC_LINK",
        );
    }

    #[test]
    fn every_built_target_has_one_pinned_archive() {
        targets(&root_file("deny.toml")).unwrap();
        refused(
            targets(
                "[graph]\ntargets = [\n\"aarch64-apple-darwin\",\n\"x86_64-pc-windows-msvc\",\n\"x86_64-unknown-linux-gnu\",\n\"aarch64-unknown-linux-gnu\",\n\"x86_64-apple-darwin\",\n]\n",
            ),
            "differ",
        );
        refused(targets("[graph]\n"), "deny.toml lists no graph targets");
    }

    /// The `ort-sys` 2.0.0-rc.13 rows of the pinned archives, beside rows for other feature sets.
    const TABLE: &str = "target\tfeature_set\turl\tsha256_hash\n\
        aarch64-apple-darwin\tcoreml\thttps://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/aarch64-apple-darwin+coreml.tar.lzma2\t6934874e2e953576d9c1db47ff1af39c62c4f4220dbe6f988e131f72879674c7\n\
        aarch64-apple-darwin\tcoreml,webgpu\thttps://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/aarch64-apple-darwin+coreml,webgpu.tar.lzma2\tc887d7ec10b06832d95e0fa035a28425794b26e1a5c34804bda7c6b50f7197a3\n\
        x86_64-pc-windows-msvc\tdirectml\thttps://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/x86_64-pc-windows-msvc+directml.tar.lzma2\tf7c654b3729cb9e5ad2a36a0c38e5b48e63bf4eed22968931aed33a0ad0b527d\n\
        x86_64-unknown-linux-gnu\tnone\thttps://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/x86_64-unknown-linux-gnu.tar.lzma2\te454f710f8a49f53aa5b4ff51e3454ae1835777e431c6c35c5255ce6f205fd68\n\
        aarch64-unknown-linux-gnu\tnone\thttps://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/aarch64-unknown-linux-gnu.tar.lzma2\t06a050ab9137ccb32421d0cb49e9ccf72d9e18ab0aeb8f8d038d1b5cc844b35a\n";

    #[test]
    fn the_resolved_ort_sys_table_names_exactly_the_pinned_archives() {
        archives(TABLE).unwrap();
        refused(
            archives(&TABLE.replace(
                "\t6934874e2e953576d9c1db47ff1af39c62c4f4220dbe6f988e131f72879674c7",
                "\t0000000000000000000000000000000000000000000000000000000000000000",
            )),
            "ort-sys accepts the aarch64-apple-darwin archive by SHA-256 0000",
        );
        refused(
            archives(&TABLE.replace(
                "ms@1.28.0/x86_64-pc-windows-msvc+directml",
                "ms@1.29.0/x86_64-pc-windows-msvc+directml",
            )),
            "ort-sys fetches x86_64-pc-windows-msvc from https://cdn.pyke.io/0/pyke:ort-rs/ms@1.29.0",
        );
        refused(
            archives(&TABLE.replace(
                "x86_64-unknown-linux-gnu\tnone",
                "x86_64-unknown-linux-gnu\twebgpu",
            )),
            "ort-sys names no x86_64-unknown-linux-gnu archive with features none",
        );
        refused(
            archives("target\tfeature_set\turl\tsha256_hash\n"),
            "ort-sys names no",
        );
    }

    #[test]
    fn a_fetched_archive_must_be_the_pinned_bytes() {
        let archive = Archive {
            target: "test-target",
            features: &[],
            url: "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/test.tar.lzma2",
            bytes: 3,
            // SHA-256 of "abc".
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        verify_file(&archive, b"abc").unwrap();
        refused(
            verify_file(&archive, b"abcd"),
            "is 4 bytes, not the pinned 3",
        );
        refused(
            verify_file(&archive, b"abd"),
            "archive's SHA-256 is a52d159f262b2c6ddb724a61840befc36eb30c88877a4030b65cbe86298449c9, \
             not the pinned ba7816bf",
        );
    }

    #[test]
    fn an_attestation_must_name_the_archive_and_the_recorded_build() {
        let archive = &ARCHIVES[0];
        let answer = |subject: &str, digest: &str, run: &str, signer: &str| {
            json!([{"verificationResult": {
                "statement": {
                    "predicateType": "https://slsa.dev/provenance/v1",
                    "subject": [{"name": "aarch64-apple-darwin+coreml.tar.lzma2",
                                 "digest": {"sha256": subject}}],
                },
                "signature": {"certificate": {
                    "buildSignerURI": signer,
                    "sourceRepositoryDigest": digest,
                    "runInvocationURI": run,
                }},
            }}])
        };
        let signer = "https://github.com/pykeio/ort-artifacts/.github/workflows/build-runner.yml@refs/heads/main";
        attested(
            archive,
            &answer(
                archive.sha256,
                ATTESTATION.source_digest,
                ATTESTATION.run,
                signer,
            ),
        )
        .unwrap();
        refused(
            attested(
                archive,
                &answer(
                    &"0".repeat(64),
                    ATTESTATION.source_digest,
                    ATTESTATION.run,
                    signer,
                ),
            ),
            "the attestation's subject is not the aarch64-apple-darwin archive",
        );
        refused(
            attested(
                archive,
                &answer(archive.sha256, &"1".repeat(40), ATTESTATION.run, signer),
            ),
            "built from another revision",
        );
        refused(
            attested(
                archive,
                &answer(
                    archive.sha256,
                    ATTESTATION.source_digest,
                    "https://example.invalid/run",
                    signer,
                ),
            ),
            "built by another run",
        );
        refused(
            attested(
                archive,
                &answer(
                    archive.sha256,
                    ATTESTATION.source_digest,
                    ATTESTATION.run,
                    "https://github.com/someone/fork/.github/workflows/build.yml@refs/heads/main",
                ),
            ),
            "signed by another workflow",
        );
        refused(attested(archive, &json!([])), "no attestation verified");
    }
}
