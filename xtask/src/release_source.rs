//! The complete corresponding source archive, `luxforge-VERSION-source.tar.gz`
//! (`docs/design/release-v0.1.md#deliverables`, `docs/engineering/releases.md`). From one exact
//! revision it holds the tracked tree as `git archive` writes it, every locked crate vendored with
//! an offline source configuration added to the repository's own Cargo configuration, the prebuilt
//! ONNX Runtime libraries `ort-sys` would otherwise download, each shipping target's notices and the
//! build, install and relink instructions. A tracked path that looks private, generated or secret
//! refuses the archive, and so does a produced archive that names this machine's home, checkout,
//! output or Cargo/rustup homes. The archive never contains its own hash: `source.json` beside it
//! does, for `release-assemble`.
use crate::*;
use std::io::BufReader;
use std::process::Stdio;
use xtask_cli::onnxruntime::{self, ONNXRUNTIME_VERSION};
use xtask_cli::release_artifacts::{TARGETS, archive_record};
use xtask_cli::release_candidate::{self, SCHEMA_VERSION, source_archive_name};
use xtask_cli::release_source::RECORD;
use xtask_cli::release_tag::ReleaseTag;

/// What the archive adds to the tracked tree, at its top level. A tracked path among them refuses
/// the archive rather than being overwritten.
const ADDED: &[&str] = &[
    "vendor",
    "third_party",
    "notices",
    "BUILD.md",
    "INSTALL.md",
    "RELINK.md",
    "SOURCE.json",
];
/// The instructions, rendered from `release/source/` with the archive's identity.
const INSTRUCTIONS: &[(&str, &str)] = &[
    ("BUILD.md", include_str!("../../release/source/BUILD.md")),
    (
        "INSTALL.md",
        include_str!("../../release/source/INSTALL.md"),
    ),
    ("RELINK.md", include_str!("../../release/source/RELINK.md")),
];
/// Where the extracted ONNX Runtime libraries live, in the layout `ort-sys` reads from
/// `ORT_CACHE_DIR`: `dfbin/<target>/<archive SHA-256>/`.
fn ort_cache() -> String {
    format!("third_party/onnxruntime-{ONNXRUNTIME_VERSION}/ort-cache")
}
/// Tracked test fixtures that look like key material but are throwaway test certificates.
const TEST_KEY_FIXTURES: &[&str] = &["crates/luxforge-net/tests/fixtures/tls/"];

/// Why a tracked path may not enter the source archive, or `None` when it may: build, evidence and
/// private output, owner originals and camera files, generated fixtures, catalogs, environment
/// files, credentials and desktop metadata. The repository ignores all of these; a tracked one
/// means something was committed by mistake, so the archive refuses rather than dropping it.
pub(crate) fn excluded(path: &str) -> Option<&'static str> {
    let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    let (name, directories) = components.split_last()?;
    let lower = name.to_ascii_lowercase();
    let extension = lower.rsplit_once('.').map_or("", |(_, e)| e);
    if matches!(
        components[0],
        "artifacts" | "dist" | "private" | ".tools" | "target"
    ) {
        return Some("local build, evidence or private output");
    }
    if directories
        .iter()
        .any(|c| *c == "target" || *c == "__pycache__")
    {
        return Some("build output or cache");
    }
    if ["fixtures/raw/", "fixtures/jpg/", "fixtures/generated/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return Some("owner originals or locally generated fixtures");
    }
    if path.starts_with("services/usage-statistics/update-store/") {
        return Some("a local update store");
    }
    if matches!(
        extension,
        "nef"
            | "nrw"
            | "raf"
            | "arw"
            | "srf"
            | "sr2"
            | "cr2"
            | "cr3"
            | "crw"
            | "dng"
            | "orf"
            | "rw2"
            | "pef"
            | "srw"
            | "3fr"
            | "iiq"
            | "x3f"
    ) {
        return Some("a camera original");
    }
    if matches!(
        extension,
        "sqlite" | "sqlite3" | "db" | "sqlite-wal" | "sqlite-shm" | "db-wal" | "db-shm"
    ) {
        return Some("a catalog or database");
    }
    if *name == ".env" || (name.starts_with(".env.") && *name != ".env.example") {
        return Some("an environment file, which may hold secrets");
    }
    if matches!(
        lower.as_str(),
        "id_rsa"
            | "id_dsa"
            | "id_ecdsa"
            | "id_ed25519"
            | ".netrc"
            | ".npmrc"
            | ".pypirc"
            | "credentials"
            | "luxforge-update-key"
    ) || matches!(
        extension,
        "p12" | "pfx" | "p8" | "keychain" | "keychain-db" | "mobileprovision" | "provisionprofile"
    ) || (matches!(extension, "pem" | "key")
        && !TEST_KEY_FIXTURES
            .iter()
            .any(|prefix| path.starts_with(prefix)))
    {
        return Some("a credential or key");
    }
    if lower == ".ds_store" || lower == "thumbs.db" {
        return Some("desktop metadata");
    }
    None
}

/// The repository's Cargo configuration with the archive's offline settings added: crates.io
/// replaced by `vendor/`, and `ORT_CACHE_DIR` naming the bundled ONNX Runtime libraries. Every
/// original line is kept, in order; a configuration that already replaces sources or sets
/// `ORT_CACHE_DIR` is refused rather than merged.
pub(crate) fn offline_config(original: &str) -> Result<String> {
    let header = |line: &str| line.trim_start().starts_with('[');
    ensure(
        !original
            .lines()
            .any(|line| header(line) && line.contains("source")),
        "The repository's Cargo configuration already configures sources",
    )?;
    ensure(
        !original.contains("ORT_CACHE_DIR"),
        "The repository's Cargo configuration already sets ORT_CACHE_DIR",
    )?;
    let ort = [
        "# Added by `cargo xtask release-source`: the prebuilt ONNX Runtime libraries this archive"
            .to_owned(),
        "# carries, so `ort-sys` downloads nothing (`BUILD.md`).".to_owned(),
        format!(
            "ORT_CACHE_DIR = {{ value = \"{}\", relative = true, force = true }}",
            ort_cache()
        ),
    ];
    let mut lines: Vec<String> = original.lines().map(str::to_owned).collect();
    match lines.iter().position(|line| line.trim() == "[env]") {
        Some(at) => {
            lines.splice(at + 1..at + 1, ort);
        }
        None => {
            lines.push(String::new());
            lines.push("[env]".to_owned());
            lines.extend(ort);
        }
    }
    lines.extend(
        [
            "",
            "# Added by `cargo xtask release-source`: build from the vendored crates in `vendor/`,",
            "# with no network access or Cargo source cache.",
            "[source.crates-io]",
            "replace-with = \"vendored-sources\"",
            "",
            "[source.vendored-sources]",
            "directory = \"vendor\"",
        ]
        .map(str::to_owned),
    );
    Ok(format!("{}\n", lines.join("\n")))
}

/// Exports `revision`'s tracked tree into `tree` with `git archive`, keeping file modes and links.
/// Every path must be one the archive may contain ([`excluded`]).
fn export_revision(root: &Path, revision: &str, tree: &Path) -> Result {
    fs::create_dir_all(tree)?;
    let mut child = Command::new("git")
        .args(["archive", "--format=tar", revision])
        .current_dir(root)
        .stdout(Stdio::piped())
        .spawn()?;
    let mut refused = Vec::new();
    let unpacked = (|| -> Result {
        let mut archive = tar::Archive::new(child.stdout.take().ok_or("git archive output")?);
        archive.set_preserve_permissions(true);
        archive.set_overwrite(false);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let kind = entry.header().entry_type();
            // The pax global header holding the commit ID is not a file.
            if !(kind.is_file() || kind.is_dir() || kind.is_symlink()) {
                continue;
            }
            let path = entry
                .path()?
                .to_str()
                .ok_or("Tracked path encoding")?
                .trim_end_matches('/')
                .to_owned();
            if let Some(reason) = excluded(&path) {
                refused.push(format!("{path}: {reason}"));
                continue;
            }
            ensure(
                entry.unpack_in(tree)?,
                format!("Unsafe tracked path: {path}"),
            )?;
        }
        Ok(())
    })();
    let status = child.wait()?;
    unpacked?;
    ensure(status.success(), "git archive failed")?;
    ensure(
        refused.is_empty(),
        format!(
            "Tracked files that may not enter the source archive:\n{}",
            refused.join("\n")
        ),
    )
}

/// The pinned ONNX Runtime archive for `target`, downloaded into `target/onnxruntime/` when it is
/// missing and refused unless its length and SHA-256 are the pinned ones.
fn onnxruntime_archive(root: &Path, archive: &onnxruntime::Archive) -> Result<PathBuf> {
    let verify = |path: &Path| -> Result {
        ensure(
            fs::metadata(path)?.len() == archive.bytes && hash(path)? == archive.sha256,
            format!(
                "{} does not match its pinned length and SHA-256",
                archive.url
            ),
        )
    };
    let cache = root.join("target/onnxruntime").join(ONNXRUNTIME_VERSION);
    let name = archive.url.rsplit('/').next().ok_or("Archive URL")?;
    let path = cache.join(name);
    if verify(&path).is_err() {
        fs::create_dir_all(&cache)?;
        let partial = cache.join(format!("{name}.partial"));
        let _ = fs::remove_file(&partial);
        let status = Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--max-filesize",
                &(archive.bytes + 1).to_string(),
                "--output",
            ])
            .arg(&partial)
            .arg(archive.url)
            .status()?;
        ensure(
            status.success(),
            format!("Downloading {} failed", archive.url),
        )?;
        if let Err(error) = verify(&partial) {
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
        fs::rename(&partial, &path)?;
    }
    verify(&path)?;
    Ok(path)
}

/// Extracts a pinned `.tar.lzma2` ONNX Runtime archive, as `ort-sys` does, into `destination`,
/// answering each file's path, size and SHA-256. Only plain relative files are accepted.
fn extract_onnxruntime(archive: &Path, destination: &Path) -> Result<Vec<Value>> {
    fs::create_dir_all(destination)?;
    let reader =
        lzma_rust2::Lzma2Reader::new(BufReader::new(fs::File::open(archive)?), 1 << 26, None);
    let mut tar = tar::Archive::new(reader);
    let mut extracted = Vec::new();
    for entry in tar.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        ensure(
            path.components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            format!("Unsafe ONNX Runtime archive path: {}", path.display()),
        )?;
        let output = destination.join(&path);
        fs::create_dir_all(output.parent().ok_or("Library parent")?)?;
        std::io::copy(&mut entry, &mut fs::File::create(&output)?)?;
        extracted.push(json!({
            "path": path.to_str().ok_or("Library path encoding")?.replace('\\', "/"),
            "size": fs::metadata(&output)?.len(),
            "sha256": hash(&output)?,
        }));
    }
    ensure(
        !extracted.is_empty(),
        format!("{} holds no library", archive.display()),
    )?;
    Ok(extracted)
}

/// Every path under `directory`, sorted, each directory before its contents. Links are listed, not
/// followed.
fn walk(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<_> = fs::read_dir(directory)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    entries.sort();
    let mut paths = Vec::new();
    for path in entries {
        let directory = fs::symlink_metadata(&path)?.is_dir();
        paths.push(path.clone());
        if directory {
            paths.extend(walk(&path)?);
        }
    }
    Ok(paths)
}

/// Writes `tree` as a gzip-compressed tar whose entries sit under `prefix/`, in sorted order, with
/// no owner, group or time of this machine: only the path, the content, the link target and
/// whether the file is executable.
pub(crate) fn pack(tree: &Path, prefix: &str, destination: &Path) -> Result {
    let gz = flate2::write::GzEncoder::new(
        fs::File::create_new(destination)?,
        flate2::Compression::default(),
    );
    let mut builder = tar::Builder::new(gz);
    builder.mode(tar::HeaderMode::Deterministic);
    builder.follow_symlinks(false);
    builder.append_path_with_name(tree, prefix)?;
    for path in walk(tree)? {
        builder.append_path_with_name(&path, Path::new(prefix).join(path.strip_prefix(tree)?))?;
    }
    builder.into_inner()?.finish()?.sync_all()?;
    Ok(())
}

/// The absolute paths of this machine that must not appear in the archive: the home directory,
/// the checkout, the output, and the Cargo and rustup homes. Short or root paths are left out so
/// that they cannot match by accident.
fn local_paths(root: &Path, out: &Path) -> Vec<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut paths: Vec<PathBuf> = vec![root.to_path_buf(), out.to_path_buf()];
    for (variable, default) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
        paths.extend(
            std::env::var_os(variable)
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|h| h.join(default))),
        );
    }
    paths.extend(home);
    let mut needles = std::collections::BTreeSet::new();
    for path in paths {
        for form in [Some(path.clone()), fs::canonicalize(&path).ok()]
            .into_iter()
            .flatten()
        {
            let text = form.to_string_lossy().trim_end_matches('/').to_owned();
            if text.len() >= 6 {
                needles.insert(text);
            }
        }
    }
    needles.into_iter().collect()
}

/// Prefixes of archive paths that are not scanned: prebuilt upstream libraries, identified by their
/// pinned hashes, whose content was built elsewhere and may name the paths of that build.
fn unscanned(prefix: &str) -> Vec<String> {
    vec![format!("{prefix}/third_party/")]
}

/// Reads the gzip-compressed tar at `archive` and answers every entry whose path, link target or
/// content contains one of `needles`, as `path: needle`, except entries under `skip`. Content is
/// read in bounded chunks that overlap by the longest needle.
pub(crate) fn scan(archive: &Path, needles: &[String], skip: &[String]) -> Result<Vec<String>> {
    if needles.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = regex::bytes::Regex::new(
        &needles
            .iter()
            .map(|n| regex::escape(n))
            .collect::<Vec<_>>()
            .join("|"),
    )?;
    let overlap = needles.iter().map(String::len).max().unwrap_or(0);
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(archive)?));
    let mut hits = Vec::new();
    let mut buffer = vec![0; 1 << 20];
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        if skip.iter().any(|prefix| path.starts_with(prefix)) {
            continue;
        }
        let mut text = path.clone().into_bytes();
        if let Some(link) = entry.link_name()? {
            text.extend_from_slice(b"\n");
            text.extend_from_slice(link.to_string_lossy().as_bytes());
        }
        let mut found = pattern.find(&text).map(|m| m.as_bytes().to_vec());
        let mut carried = 0;
        while found.is_none() {
            let read = std::io::Read::read(&mut entry, &mut buffer[carried..])?;
            if read == 0 {
                break;
            }
            let filled = carried + read;
            found = pattern
                .find(&buffer[..filled])
                .map(|m| m.as_bytes().to_vec());
            carried = overlap.saturating_sub(1).min(filled);
            buffer.copy_within(filled - carried..filled, 0);
        }
        if let Some(needle) = found {
            hits.push(format!("{path}: {}", String::from_utf8_lossy(&needle)));
        }
    }
    Ok(hits)
}

/// Fills `{{name}}` placeholders in an instruction template; a placeholder left unfilled is an
/// error.
fn render(template: &str, values: &[(&str, String)]) -> Result<String> {
    let mut text = template.to_owned();
    for (name, value) in values {
        text = text.replace(&format!("{{{{{name}}}}}"), value);
    }
    ensure(
        !text.contains("{{"),
        "An instruction template has an unfilled placeholder",
    )?;
    Ok(text)
}

pub fn run(root: &Path, a: &mut Args) -> Result {
    let tag = a.value("--tag")?.ok_or("Required: --tag")?;
    let out = absolute(root, &a.path("--output")?);
    let allow_dirty = a.flag("--allow-dirty");
    a.done()?;
    build(root, &out, tag.to_str().ok_or("Tag encoding")?, allow_dirty)
}

/// Builds the source archive and `source.json` in the new directory `out`.
pub fn build(root: &Path, out: &Path, tag: &str, allow_dirty: bool) -> Result {
    let tag = ReleaseTag::parse(tag)?;
    // The source checks of a release build: a new output, a full commit and, unless this is a
    // labelled rehearsal, a clean tree whose commit the tag names.
    let release_candidate::Source {
        revision, dirty, ..
    } = release_candidate::check_source(root, out, &tag, TARGETS[1].0, allow_dirty)?;
    let toolchain = release_candidate::toolchain(root, allow_dirty)?;
    fs::create_dir_all(out)?;
    let staging = out.join("staging");
    let prefix = format!("luxforge-{}-source", tag.version);
    let tree = staging.join(&prefix);
    let archive = out.join(source_archive_name(&tag));
    let outcome = (|| -> Result<Value> {
        export_revision(root, &revision, &tree)?;
        for name in ADDED {
            ensure(
                fs::symlink_metadata(tree.join(name)).is_err(),
                format!("The tracked tree already has {name}, which the source archive adds"),
            )?;
        }
        let lock = hash(&tree.join("Cargo.lock"))?;
        // Vendor every locked crate, from the exported tree and its own lockfile.
        println!("Vendoring the locked crates");
        let status = cargo_command()
            .current_dir(&tree)
            .args(["vendor", "--locked", "--versioned-dirs", "vendor"])
            .stdout(Stdio::null())
            .status()?;
        ensure(status.success(), "cargo vendor failed")?;
        ensure(
            hash(&tree.join("Cargo.lock"))? == lock,
            "Vendoring changed the lockfile",
        )?;
        let packages = fs::read_dir(tree.join("vendor"))?
            .filter(|entry| entry.as_ref().is_ok_and(|e| e.path().is_dir()))
            .count();
        let config = tree.join(".cargo/config.toml");
        let original = fs::read_to_string(&config).unwrap_or_default();
        fs::create_dir_all(config.parent().ok_or("Config parent")?)?;
        fs::write(&config, offline_config(&original)?)?;
        // The prebuilt ONNX Runtime of each shipping target.
        let mut runtimes = Vec::new();
        for &(target, _) in TARGETS {
            let pinned = onnxruntime::ARCHIVES
                .iter()
                .find(|a| a.target == target)
                .ok_or_else(|| format!("No pinned ONNX Runtime archive for {target}"))?;
            let downloaded = onnxruntime_archive(root, pinned)?;
            let files = extract_onnxruntime(
                &downloaded,
                &tree
                    .join(ort_cache())
                    .join("dfbin")
                    .join(target)
                    .join(pinned.sha256),
            )?;
            runtimes.push(json!({
                "target": target,
                "url": pinned.url,
                "archive_bytes": pinned.bytes,
                "archive_sha256": pinned.sha256,
                "files": files,
            }));
        }
        // Each shipping target's notices, from the exported tree's vendored graph.
        let mut notices = serde_json::Map::new();
        for &(target, platform) in TARGETS {
            let directory = format!("notices/{platform}");
            package::inventory_for(&tree, &tree.join(&directory), target)?;
            notices.insert(platform.to_owned(), json!(directory));
        }
        let identity = json!({
            "schema_version": SCHEMA_VERSION,
            "tag": tag.tag,
            "version": tag.version,
            "revision": revision,
            "lock_sha256": lock,
            "toolchain": toolchain,
            "working_tree_dirty": dirty,
            "rehearsal": allow_dirty,
        });
        let rows: Vec<String> = runtimes
            .iter()
            .flat_map(|r| {
                r["files"].as_array().into_iter().flatten().map(move |f| {
                    format!(
                        "| `{}` | `{}` ({} bytes, SHA-256 `{}`) | `{}/dfbin/{}/{}/{}` ({} bytes, SHA-256 `{}`) |",
                        r["target"].as_str().unwrap_or_default(),
                        r["url"].as_str().unwrap_or_default(),
                        r["archive_bytes"],
                        r["archive_sha256"].as_str().unwrap_or_default(),
                        ort_cache(),
                        r["target"].as_str().unwrap_or_default(),
                        r["archive_sha256"].as_str().unwrap_or_default(),
                        f["path"].as_str().unwrap_or_default(),
                        f["size"],
                        f["sha256"].as_str().unwrap_or_default(),
                    )
                })
            })
            .collect();
        let values = [
            ("version", tag.version.clone()),
            ("tag", tag.tag.clone()),
            ("revision", revision.clone()),
            ("lock_sha256", lock.clone()),
            (
                "rust",
                toolchain["pinned"].as_str().unwrap_or_default().to_owned(),
            ),
            ("packages", packages.to_string()),
            (
                "onnxruntime",
                format!(
                    "| Target | Pinned archive | Library in this archive |\n| --- | --- | --- |\n{}",
                    rows.join("\n")
                ),
            ),
            (
                "rehearsal",
                if allow_dirty {
                    "\n\n**This is a local rehearsal archive, not a release deliverable.** It holds the committed tree of the commit above; changes that were not committed are not in it.".to_owned()
                } else {
                    String::new()
                },
            ),
        ];
        for (name, template) in INSTRUCTIONS {
            fs::write(tree.join(name), render(template, &values)?)?;
        }
        let mut manifest = identity.clone();
        manifest["contents"] = json!({
            "project": format!("git archive of {revision}, with .cargo/config.toml extended for offline builds"),
            "vendored_packages": packages,
            "vendor": "vendor",
            "onnxruntime": runtimes,
            "notices": notices,
            "instructions": INSTRUCTIONS.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        });
        manifest["excluded"] = json!([
            "Untracked and ignored files: build output (target/), evidence and verification output (artifacts/, dist/), private inputs (private/), owner RAW originals and generated fixtures (fixtures/raw/, fixtures/jpg/, fixtures/generated/), local tools and caches, environment files and the local update store",
            "The licensed sample corpus and the manifests that name private originals",
            "Catalogs, credentials and keys (a tracked one refuses the archive; the throwaway TLS test certificates under crates/luxforge-net/tests/fixtures/tls/ are kept)",
        ]);
        manifest["test_only_omissions"] = json!([
            "Owner RAW originals, the licensed sample corpus and their manifests, read by the RAW, GPU qualification and corpus checks",
            "Locally generated JPEG fixtures, which cargo xtask generate-fixtures recreates",
            "The dependencies of the separate services/ and probes/ packages, which have their own lockfiles and are not part of the shipped executables",
        ]);
        manifest["review"] = json!({
            "source_and_notices_review": "not-run",
            "note": "The notices are an inventory; the distribution licence and notice review has not been completed",
        });
        write_json(&tree.join("SOURCE.json"), &manifest)?;
        println!("Packing {}", archive.display());
        pack(&tree, &prefix, &archive)?;
        let hits = scan(&archive, &local_paths(root, out), &unscanned(&prefix))?;
        ensure(
            hits.is_empty(),
            format!(
                "The source archive names local paths of this machine:\n{}",
                hits.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
            ),
        )?;
        ensure(
            output(root, "git", &["rev-parse", "HEAD"])?.trim() == revision,
            "Source revision changed while the archive was made",
        )?;
        let dirty_after = release_candidate::working_tree_dirty(root)?;
        ensure(
            allow_dirty || !dirty_after,
            "Source files changed while the archive was made",
        )?;
        let mut record = identity;
        record["working_tree_dirty"] = json!(dirty || dirty_after);
        record["archive"] = archive_record(&archive)?;
        Ok(record)
    })();
    let cleanup = if staging.exists() {
        fs::remove_dir_all(&staging)
    } else {
        Ok(())
    };
    let record = outcome?;
    cleanup?;
    write_json(&out.join(RECORD), &record)?;
    xtask_cli::release_source::verify(out, &tag)?;
    println!(
        "Source archive{}: {} ({} bytes, SHA-256 {})",
        if allow_dirty { " (rehearsal)" } else { "" },
        archive.display(),
        record["archive"]["size"],
        record["archive"]["sha256"].as_str().unwrap_or_default()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_test_macros::slow;

    #[test]
    fn private_generated_and_secret_paths_never_enter_the_archive() {
        for path in [
            "artifacts/run/state.json",
            "target/release/luxforge",
            "crates/luxforge-core/target/debug/build.rs",
            "dist/release.json",
            "private/object-removal/corpus/a.jpg",
            ".tools/cargo-deny/bin/cargo-deny",
            "fixtures/raw/DSC_0001.NEF",
            "fixtures/generated/basic.jpg",
            "fixtures/jpg/owner.jpg",
            "fixtures/s0/sample.dng",
            "docs/DSCF0001.RAF",
            "tools/__pycache__/x.pyc",
            "catalog.sqlite",
            "fixtures/catalog/library.db",
            ".env",
            "services/usage-statistics/.env.production",
            "release/luxforge-update-key",
            "certificate.p12",
            "signing/developer-id.pem",
            "home/.ssh/id_ed25519",
            "AuthKey_ABC.p8",
            "docs/.DS_Store",
            "services/usage-statistics/update-store/feeds.json",
        ] {
            assert!(excluded(path).is_some(), "{path}");
        }
        for path in [
            "Cargo.toml",
            ".env.example",
            "services/usage-statistics/.env.example",
            "crates/luxforge-core/src/artifacts/store.rs",
            "crates/luxforge-net/tests/fixtures/tls/leaf.key",
            "crates/luxforge-net/tests/fixtures/tls/ca.pem",
            "fixtures/s0/srgb.jpg",
            "fixtures/sample-corpus.json",
            "docs/design/release-v0.1.md",
            "crates/luxforge-raw/vendor/libraw-0.22.2/src/decoders/dng.cpp",
        ] {
            assert_eq!(excluded(path), None, "{path}");
        }
    }

    #[test]
    fn the_offline_configuration_keeps_every_repository_setting() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let original = fs::read_to_string(root.join(".cargo/config.toml")).unwrap();
        let config = offline_config(&original).unwrap();
        // Every original line survives, in order.
        let mut lines = config.lines();
        for line in original.lines() {
            assert!(lines.any(|l| l == line), "lost: {line}");
        }
        assert!(config.contains("xtask = \"run --package xtask-cli --\""));
        assert!(config.contains("MACOSX_DEPLOYMENT_TARGET"));
        assert!(config.contains("[source.crates-io]\nreplace-with = \"vendored-sources\""));
        assert!(config.contains("[source.vendored-sources]\ndirectory = \"vendor\""));
        // ORT_CACHE_DIR sits in the existing [env] table, before the next table.
        let env = config.find("[env]").unwrap();
        let ort = config.find("ORT_CACHE_DIR = ").unwrap();
        let next = config[env + 1..].find("\n[").map(|i| i + env + 1).unwrap();
        assert!(env < ort && ort < next, "{config}");
        assert_eq!(config.matches("[env]").count(), 1);
        assert!(config.contains(
            "ORT_CACHE_DIR = { value = \"third_party/onnxruntime-1.28.0/ort-cache\", relative = true, force = true }"
        ));
        // A configuration without [env] gains one; one that already configures sources or the
        // ONNX Runtime cache is refused.
        let fresh = offline_config("[alias]\nb = \"build\"\n").unwrap();
        assert!(fresh.starts_with("[alias]\nb = \"build\"\n\n[env]\n"));
        assert!(offline_config("").unwrap().contains("[env]\n"));
        for refused in [
            "[source.crates-io]\nreplace-with = \"mirror\"\n",
            "[env]\nORT_CACHE_DIR = \"/tmp\"\n",
        ] {
            assert!(offline_config(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn instruction_templates_fill_every_placeholder() {
        let values: Vec<(&str, String)> = [
            "version",
            "tag",
            "revision",
            "lock_sha256",
            "rust",
            "packages",
            "onnxruntime",
            "rehearsal",
        ]
        .into_iter()
        .map(|name| (name, format!("<{name}>")))
        .collect();
        for (name, template) in INSTRUCTIONS {
            let text = render(template, &values).unwrap();
            assert!(text.contains("<version>"), "{name}");
        }
        assert!(render("{{unknown}}", &values).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn packed_archives_are_sorted_ownerless_and_scanned_for_local_paths() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let tree = tmp.path().join("tree");
        fs::create_dir_all(tree.join("b/inner")).unwrap();
        fs::write(tree.join("a.txt"), b"plain").unwrap();
        fs::write(tree.join("b/inner/tool"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(tree.join("b/inner/tool"), fs::Permissions::from_mode(0o750)).unwrap();
        std::os::unix::fs::symlink("a.txt", tree.join("link.md")).unwrap();
        // A needle that straddles the scanner's 1 MiB chunks is still found.
        let mut large = vec![b'x'; (1 << 20) - 3];
        large.extend_from_slice(b"/Users/someone-private/projects");
        fs::write(tree.join("b/large.bin"), &large).unwrap();
        fs::create_dir_all(tree.join("third_party")).unwrap();
        fs::write(tree.join("third_party/lib.a"), b"/Users/someone-private").unwrap();
        let archive = tmp.path().join("source.tar.gz");
        pack(&tree, "luxforge-0.1.0-source", &archive).unwrap();
        // The output is never overwritten.
        assert!(pack(&tree, "luxforge-0.1.0-source", &archive).is_err());
        let mut entries = Vec::new();
        let mut reader = tar::Archive::new(flate2::read::GzDecoder::new(
            fs::File::open(&archive).unwrap(),
        ));
        for entry in reader.entries().unwrap() {
            let entry = entry.unwrap();
            let header = entry.header();
            assert_eq!(header.uid().unwrap(), 0);
            assert_eq!(header.gid().unwrap(), 0);
            assert_eq!(header.mtime().unwrap(), 1153704088);
            assert!(header.username().unwrap().unwrap_or_default().is_empty());
            entries.push((
                entry.path().unwrap().to_string_lossy().into_owned(),
                header.mode().unwrap(),
                header.entry_type(),
            ));
        }
        let names: Vec<_> = entries.iter().map(|(n, ..)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "luxforge-0.1.0-source",
                "luxforge-0.1.0-source/a.txt",
                "luxforge-0.1.0-source/b",
                "luxforge-0.1.0-source/b/inner",
                "luxforge-0.1.0-source/b/inner/tool",
                "luxforge-0.1.0-source/b/large.bin",
                "luxforge-0.1.0-source/link.md",
                "luxforge-0.1.0-source/third_party",
                "luxforge-0.1.0-source/third_party/lib.a",
            ]
        );
        assert_eq!(entries[1].1, 0o644);
        assert_eq!(entries[4].1, 0o755);
        assert!(entries[6].2.is_symlink());
        let needles = ["/Users/someone-private".to_owned()];
        let skip = unscanned("luxforge-0.1.0-source");
        assert_eq!(
            scan(&archive, &needles, &skip).unwrap(),
            ["luxforge-0.1.0-source/b/large.bin: /Users/someone-private"]
        );
        assert_eq!(scan(&archive, &needles, &[]).unwrap().len(), 2);
        assert!(
            scan(&archive, &["/home/nobody-here".to_owned()], &[])
                .unwrap()
                .is_empty()
        );
        // A link target is scanned too.
        assert_eq!(
            scan(&archive, &["a.txt".to_owned()], &[]).unwrap(),
            [
                "luxforge-0.1.0-source/a.txt: a.txt",
                "luxforge-0.1.0-source/link.md: a.txt"
            ]
        );
    }

    #[test]
    fn local_paths_name_the_checkout_output_and_homes() {
        let root = Path::new("/var/empty/checkout");
        let out = Path::new("/var/empty/checkout/artifacts/source");
        let paths = local_paths(root, out);
        assert!(paths.iter().any(|p| p == "/var/empty/checkout"));
        assert!(
            paths
                .iter()
                .any(|p| p == "/var/empty/checkout/artifacts/source")
        );
        if let Some(home) = std::env::var_os("HOME").filter(|h| h.len() >= 6) {
            assert!(
                paths
                    .iter()
                    .any(|p| *p == home.to_string_lossy().trim_end_matches('/'))
            );
        }
        assert!(paths.iter().all(|p| p.len() >= 6));
    }

    /// An isolated checkout with one commit tagged `v0.1` and an ignored output directory.
    #[cfg(unix)]
    fn checkout() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("checkout");
        fs::create_dir(&root).unwrap();
        let git = |args: &[&str]| output(&root, "git", args).unwrap();
        git(&["init", "--initial-branch=main"]);
        for (key, value) in [
            ("user.name", "Source Test"),
            ("user.email", "source@example.invalid"),
            ("commit.gpgSign", "false"),
            ("tag.gpgSign", "false"),
            ("core.hooksPath", ".git/test-hooks"),
        ] {
            git(&["config", key, value]);
        }
        fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
        fs::write(root.join("committed.txt"), "committed\n").unwrap();
        fs::write(root.join(".gitignore"), "/artifacts/\n").unwrap();
        fs::create_dir(root.join("tools")).unwrap();
        fs::write(root.join("tools/run"), "#!/bin/sh\n").unwrap();
        let mut permissions = fs::metadata(root.join("tools/run")).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        fs::set_permissions(root.join("tools/run"), permissions).unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "candidate"]);
        git(&["tag", "-a", "v0.1", "-m", "v0.1"]);
        (temp, root)
    }

    #[test]
    #[slow]
    #[cfg(unix)]
    fn a_dirty_tree_a_used_output_and_tracked_private_files_are_refused() {
        let (_temp, root) = checkout();
        // A dirty tree is refused before anything is written; a used output is never reused.
        fs::write(root.join("untracked.txt"), b"untracked").unwrap();
        let out = root.join("artifacts/source");
        let error = build(&root, &out, "v0.1", false).unwrap_err().to_string();
        assert!(error.contains("clean checkout"), "{error}");
        assert!(!out.exists());
        fs::remove_file(root.join("untracked.txt")).unwrap();
        fs::create_dir_all(&out).unwrap();
        let error = build(&root, &out, "v0.1", true).unwrap_err().to_string();
        assert!(error.contains("must be new"), "{error}");
        // The exported tree is the commit's, not the working tree's, with its file modes.
        fs::write(root.join("committed.txt"), "changed, not committed\n").unwrap();
        let revision = output(&root, "git", &["rev-parse", "HEAD"]).unwrap();
        let tree = root.join("artifacts/tree");
        export_revision(&root, revision.trim(), &tree).unwrap();
        assert_eq!(
            fs::read_to_string(tree.join("committed.txt")).unwrap(),
            "committed\n"
        );
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &fs::metadata(tree.join("tools/run")).unwrap().permissions(),
        );
        assert_eq!(mode & 0o111, 0o111);
        // A tracked catalog or key refuses the export.
        fs::write(root.join("library.sqlite"), b"catalog").unwrap();
        fs::write(root.join(".env"), b"SECRET=1").unwrap();
        let git = |args: &[&str]| output(&root, "git", args).unwrap();
        git(&["add", "-f", "library.sqlite", ".env"]);
        git(&["commit", "-m", "mistake"]);
        let revision = git(&["rev-parse", "HEAD"]);
        let error = export_revision(&root, revision.trim(), &root.join("artifacts/refused"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("library.sqlite: a catalog"), "{error}");
        assert!(error.contains(".env: an environment file"), "{error}");
    }
}
