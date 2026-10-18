//! Update records and feeds: `update-record` signs a release's final archives into its
//! `luxforge-<version>-updates.json`, and `update-feed` regenerates every target's appcast from the
//! records of every published release, and with `--deploy` the archive the release workflow
//! publishes to the API's update store. Neither prints, logs or writes the private key.
use crate::release_artifacts::{TARGETS, archive_name, verify_record};
use crate::release_candidate;
use crate::release_tag::ReleaseTag;
use crate::{Args, Result, ensure};
use luxforge_update::{
    ArchiveRecord, Channel, MAX_PRIVATE_KEY_TEXT, MAX_RECORD_BYTES, RECORD_SCHEMA, SigningKey,
    SigningStatus, Target, UpdateConfig, UpdateRecord, WITHDRAWN_PATH,
    feed::{self, FeedInputs, Feeds, PublishedRelease},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;

/// The largest published-release list read.
const MAX_PUBLISHED_BYTES: u64 = 64 * 1024 * 1024;

/// `cargo xtask update-record --tag TAG --input BUILDS --output NEW_FILE --key-file FILE`.
pub fn run_record(root: &Path, args: &mut Args) -> Result {
    let tag = args.value("--tag")?.ok_or("Required: --tag")?;
    let input = args.path("--input")?;
    let output = args.path("--output")?;
    let key_file = args.path("--key-file")?;
    args.done()?;
    let config = UpdateConfig::load_from_repository(root)?;
    let key = read_key(&key_file)?;
    let Recorded { record, notes } = record(
        &config,
        &input,
        &output,
        tag.to_str().ok_or("Tag encoding")?,
        &key,
    )?;
    for note in &notes {
        println!("NOTE {note}");
    }
    for (target, archive) in &record.archives {
        println!(
            "{target}: {} ({} bytes, build {}, {})",
            archive.file,
            archive.length,
            archive.build,
            archive.signing.as_str()
        );
    }
    if config.public_key.is_none() {
        println!(
            "NOTE release/update.json commits no public key: no release build can verify this \
             record's signatures (signing key's public key: {})",
            record.public_key.to_base64()
        );
    }
    println!("Update record: {}", output.display());
    Ok(())
}

/// Reads a private key file as Sparkle's `generate_keys -x` exports it. The text is zeroed when
/// dropped and appears in no error.
pub fn read_key(path: &Path) -> Result<SigningKey> {
    let file = fs::File::open(path).map_err(|e| format!("Update key file: {e}"))?;
    ensure(
        file.metadata()?.len() <= MAX_PRIVATE_KEY_TEXT as u64,
        "Update key file is larger than any exported key",
    )?;
    let mut text = Zeroizing::new(String::with_capacity(MAX_PRIVATE_KEY_TEXT + 1));
    file.take(MAX_PRIVATE_KEY_TEXT as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Update key file is not UTF-8 text")?;
    Ok(SigningKey::from_sparkle_export(&text)?)
}

/// A written update record, and a note for each archive it leaves out.
pub struct Recorded {
    pub record: UpdateRecord,
    pub notes: Vec<String>,
}

/// Signs the final archives `release-assemble` reads from `input` (`<platform>/target.json` and
/// its archive for each target) and writes the record to `output`, a new file named
/// `luxforge-<version>-updates.json`. The key's public key must be `config`'s when it commits one.
/// A macOS archive that is not notarized is checked like the others but neither signed nor
/// recorded, so the update key signs nothing a feed may not list.
pub fn record(
    config: &UpdateConfig,
    input: &Path,
    output: &Path,
    tag: &str,
    key: &SigningKey,
) -> Result<Recorded> {
    let tag = ReleaseTag::parse(tag)?;
    let name = UpdateRecord::file_name(&tag.version);
    ensure(
        output.file_name().is_some_and(|n| *n == *name),
        format!("The update record must be named {name}"),
    )?;
    ensure(!output.exists(), "The update record output must be new")?;
    let public_key = key.public_key();
    if config
        .public_key
        .is_some_and(|committed| committed != public_key)
    {
        return Err(luxforge_update::Error::KeyMismatch.into());
    }
    let mut archives = BTreeMap::new();
    let mut notes = Vec::new();
    let builds = TARGETS
        .iter()
        .map(|&(target, platform)| verify_record(&input.join(platform), &tag, target))
        .collect::<Result<Vec<_>>>()?;
    // The standalone signing command enforces the same common candidate identity as assembly,
    // before signing either archive, including a macOS build that will be left out of the feed.
    release_candidate::check_release(&release_candidate::release_record(&tag, &builds), &tag)?;
    for (&(rust_target, platform), build) in TARGETS.iter().zip(&builds) {
        let target = Target::from_rust_target(rust_target).ok_or("Unknown release target")?;
        let directory = input.join(platform);
        ensure(
            build["working_tree_dirty"] == false && build["rehearsal"] == false,
            "A dirty/local rehearsal cannot be signed for updates",
        )?;
        let number = build["build_number"]
            .as_str()
            .filter(|n| !n.starts_with('0'))
            .and_then(|n| n.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("{platform}: target.json has no build number"))?;
        let catalog_format = build["catalog_format"]
            .as_u64()
            .and_then(|f| u32::try_from(f).ok())
            .filter(|f| *f > 0)
            .ok_or_else(|| format!("{platform}: target.json has no catalog format"))?;
        let signing = build["signing"]["status"]
            .as_str()
            .and_then(SigningStatus::parse)
            .ok_or_else(|| format!("{platform}: target.json has no known signing status"))?;
        let minimum_system_version = if target.is_macos() {
            Some(
                build["minimum_system_version"]
                    .as_str()
                    .ok_or_else(|| {
                        format!("{platform}: target.json has no minimum system version")
                    })?
                    .to_owned(),
            )
        } else {
            None
        };
        let file = archive_name(&tag, rust_target)?;
        if target.is_macos() && signing != SigningStatus::Notarized {
            notes.push(format!(
                "{target}: the build is {}, not notarized; {file} is not signed or recorded",
                signing.as_str()
            ));
            continue;
        }
        let path = directory.join(&file);
        let (ed_signature, length) = key.sign_file(&path)?;
        ensure(
            build["archive"]["size"].as_u64() == Some(length),
            format!("{platform}: the archive changed while it was signed"),
        )?;
        public_key.verify_file(&path, length, &ed_signature)?;
        archives.insert(
            target,
            ArchiveRecord {
                file,
                length,
                build: number,
                ed_signature,
                minimum_system_version,
                catalog_format,
                signing,
            },
        );
    }
    let builds: Vec<u64> = archives.values().map(|a| a.build).collect();
    ensure(
        builds.windows(2).all(|pair| pair[0] == pair[1]),
        "Platform build_number mismatch",
    )?;
    let version = semver::Version::parse(&tag.version)?;
    let record = UpdateRecord {
        schema_version: RECORD_SCHEMA,
        tag: tag.tag.clone(),
        version: tag.version.clone(),
        channel: Channel::of(&version),
        public_key,
        archives,
    };
    record.validate()?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?
        .write_all(record.to_json().as_bytes())?;
    Ok(Recorded { record, notes })
}

/// `cargo xtask update-feed --records DIR --published FILE --output NEW_DIR [--withdrawn FILE]
/// [--repository OWNER/NAME] [--deploy NEW_FILE]`.
pub fn run_feed(root: &Path, args: &mut Args) -> Result {
    let records = args.path("--records")?;
    let published = args.path("--published")?;
    let output = args.path("--output")?;
    let deploy_path = args.value("--deploy")?.map(std::path::PathBuf::from);
    let withdrawn = args
        .value("--withdrawn")?
        .map(Into::into)
        .unwrap_or_else(|| root.join(WITHDRAWN_PATH));
    let repository = args.value("--repository")?;
    let repository = match &repository {
        Some(value) => value.to_str().ok_or("Repository encoding")?,
        None => feed::DEFAULT_REPOSITORY,
    };
    args.done()?;
    if let Some(path) = &deploy_path {
        ensure_new_deploy(path)?;
    }
    let config = UpdateConfig::load_from_repository(root)?;
    let feeds = feeds(
        &config, &records, &published, &withdrawn, repository, &output,
    )?;
    for note in &feeds.notes {
        println!("NOTE {note}");
    }
    if config.public_key.is_none() {
        println!("NOTE release/update.json commits no public key; records were not held to one");
    }
    for (target, appcast) in &feeds.appcasts {
        println!(
            "{}: {} item(s)",
            output.join(target.feed_path()).display(),
            appcast.items.len()
        );
    }
    if let Some(path) = &deploy_path {
        deploy(&feeds, path)?;
        println!("Deploy archive: {}", path.display());
    }
    Ok(())
}

/// The largest feed the API serves, and the clients read.
const MAX_DEPLOY_FEED: usize = luxforge_update::MAX_APPCAST_BYTES;

fn ensure_new_deploy(path: &Path) -> Result {
    ensure(
        path.extension().is_some_and(|e| e == "tar"),
        "The deploy archive must be named *.tar",
    )?;
    ensure(!path.exists(), "The deploy archive output must be new")
}

/// Writes the deploy archive the API's update store installs (the backend's `luxforge-updates`
/// Worker, or `install-feeds.py` in the reference stack): an uncompressed ustar archive holding
/// exactly `<target>/appcast.xml` for each feed, as regular files in target order with fixed
/// metadata, so the same feeds write the same bytes.
pub fn deploy(feeds: &Feeds, path: &Path) -> Result {
    ensure_new_deploy(path)?;
    let mut entries = Vec::new();
    for (target, appcast) in &feeds.appcasts {
        let xml = appcast.to_xml();
        ensure(
            xml.len() <= MAX_DEPLOY_FEED,
            format!("The {target} feed is larger than the API serves"),
        )?;
        entries.push((target.feed_path(), xml));
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut archive = tar::Builder::new(std::io::BufWriter::new(file));
    for (name, xml) in &entries {
        let mut header = tar::Header::new_ustar();
        header.set_path(name)?;
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(xml.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        archive.append(&header, xml.as_bytes())?;
    }
    archive.into_inner()?.flush()?;
    Ok(())
}

/// Reads the records in `records`, the published releases and the withdrawal list, and writes
/// `<target>/appcast.xml` for each target into `output`, a new directory.
pub fn feeds(
    config: &UpdateConfig,
    records: &Path,
    published: &Path,
    withdrawn: &Path,
    repository: &str,
    output: &Path,
) -> Result<Feeds> {
    ensure(!output.exists(), "The feed output must be new")?;
    let records = read_records(records)?;
    let published = parse_published(&read_bounded(published, MAX_PUBLISHED_BYTES)?)?;
    let withdrawn = feed::parse_withdrawn(&fs::read_to_string(withdrawn)?)?;
    let feeds = feed::generate(&FeedInputs {
        records: &records,
        published: &published,
        withdrawn: &withdrawn,
        repository,
        public_key: config.public_key.as_ref(),
    })?;
    for (target, appcast) in &feeds.appcasts {
        let path = output.join(target.feed_path());
        fs::create_dir_all(path.parent().ok_or("Feed path")?)?;
        fs::write(path, appcast.to_xml())?;
    }
    Ok(feeds)
}

/// Every file in `directory` must be a record named `luxforge-<version>-updates.json` for its own
/// version.
fn read_records(directory: &Path) -> Result<Vec<UpdateRecord>> {
    let mut records = Vec::new();
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        ensure(
            entry.file_type()?.is_file()
                && name.starts_with("luxforge-")
                && name.ends_with("-updates.json"),
            format!("{name} in the records directory is not an update record"),
        )?;
        let bytes = read_bounded(&entry.path(), MAX_RECORD_BYTES as u64)?;
        let record = UpdateRecord::parse(&bytes).map_err(|error| format!("{name}: {error}"))?;
        ensure(
            *name == UpdateRecord::file_name(&record.version),
            format!("{name} holds the record of {}", record.tag),
        )?;
        records.push(record);
    }
    Ok(records)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    ensure(
        file.metadata()?.len() <= limit,
        format!("{} is larger than {limit} bytes", path.display()),
    )?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The published releases from GitHub's release list, as `gh api --paginate --slurp
/// repos/OWNER/NAME/releases` writes it (an array of pages) or a single page: each release's
/// `tag_name`, `draft`, `published_at` and `assets` with their `name`, `size` and `state`. A draft
/// or a release with no publication time is not published, and neither is an asset whose state is
/// not `uploaded`.
pub fn parse_published(bytes: &[u8]) -> Result<Vec<PublishedRelease>> {
    let malformed = |why: &str| -> Box<dyn std::error::Error> {
        luxforge_update::Error::MalformedInput(format!("published releases: {why}")).into()
    };
    let value: Value = serde_json::from_slice(bytes).map_err(|e| malformed(&e.to_string()))?;
    let top = value.as_array().ok_or_else(|| malformed("not an array"))?;
    let mut releases = Vec::new();
    for entry in top {
        match entry {
            Value::Array(page) => releases.extend(page),
            other => releases.push(other),
        }
    }
    let mut published = Vec::new();
    for release in releases {
        let tag = release["tag_name"]
            .as_str()
            .ok_or_else(|| malformed("a release has no tag_name"))?;
        let draft = release["draft"]
            .as_bool()
            .ok_or_else(|| malformed(&format!("{tag} has no draft flag")))?;
        let published_at = match &release["published_at"] {
            Value::String(time) => time.clone(),
            Value::Null => continue,
            _ => return Err(malformed(&format!("{tag} has a malformed published_at"))),
        };
        if draft {
            continue;
        }
        let mut assets = BTreeMap::new();
        for asset in release["assets"]
            .as_array()
            .ok_or_else(|| malformed(&format!("{tag} has no assets list")))?
        {
            let (Some(name), Some(size)) = (asset["name"].as_str(), asset["size"].as_u64()) else {
                return Err(malformed(&format!(
                    "{tag} has an asset without name or size"
                )));
            };
            if !matches!(asset.get("state"), None | Some(Value::String(_))) {
                return Err(malformed(&format!("{tag}: {name} has a malformed state")));
            }
            if asset.get("state").is_some_and(|state| state != "uploaded") {
                continue;
            }
            ensure(
                assets.insert(name.to_owned(), size).is_none(),
                format!("published releases: {tag} lists {name} twice"),
            )?;
        }
        published.push(PublishedRelease {
            tag: tag.to_owned(),
            published_at,
            assets,
        });
    }
    Ok(published)
}

#[cfg(test)]
mod tests;
