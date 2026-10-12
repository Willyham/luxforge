//! Feed generation: one appcast per target from the update records of every published release.
use crate::{
    Appcast, AppcastItem, Enclosure, SigningStatus, Target, UpdateRecord,
    appcast::ENCLOSURE_TYPE,
    date::pub_date,
    error::{Error, Result},
    key::PublicKey,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// The GitHub repository whose releases hold the archives.
pub const DEFAULT_REPOSITORY: &str = "Willyham/luxforge";

/// A published GitHub release: its tag, publication time (RFC 3339, UTC) and the byte length of
/// each published asset by name. A draft is not a published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedRelease {
    pub tag: String,
    pub published_at: String,
    pub assets: BTreeMap<String, u64>,
}

/// A withdrawn release, from `release/withdrawn-updates.json`: no feed lists it. Its assets are
/// never altered.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Withdrawal {
    pub tag: String,
    pub reason: String,
}

/// Reads a withdrawal list: a JSON array of `{"tag": "v0.2.0", "reason": "…"}`, each tag once and
/// each reason stated.
pub fn parse_withdrawn(json: &str) -> Result<Vec<Withdrawal>> {
    let list: Vec<Withdrawal> = serde_json::from_str(json)
        .map_err(|error| Error::MalformedInput(format!("withdrawal list: {error}")))?;
    let mut tags = BTreeSet::new();
    for withdrawal in &list {
        if withdrawal.reason.trim().is_empty() || !tags.insert(withdrawal.tag.as_str()) {
            return Err(Error::MalformedInput(format!(
                "withdrawal list: {} needs one entry with a reason",
                withdrawal.tag
            )));
        }
    }
    Ok(list)
}

/// What a feed generation reads.
pub struct FeedInputs<'a> {
    pub records: &'a [UpdateRecord],
    pub published: &'a [PublishedRelease],
    pub withdrawn: &'a [Withdrawal],
    /// `owner/name` on GitHub, [`DEFAULT_REPOSITORY`] for Luxforge.
    pub repository: &'a str,
    /// The committed public key every record must be signed with; `None` while none is committed.
    pub public_key: Option<&'a PublicKey>,
}

/// The generated feeds by target, and a note for each release or archive left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feeds {
    pub appcasts: BTreeMap<Target, Appcast>,
    pub notes: Vec<String>,
}

/// Generates every target's appcast: [`Target::RELEASED`] and any target a record names.
///
/// Refused with a named error: a malformed record; two records for one tag or one version; a
/// record signed by another key than the committed one; build numbers that do not strictly rise
/// with SemVer order within a target (every record counts, withdrawn or not listed, since a person
/// may have installed it); and a listed archive whose release or asset is not published, or is
/// published with another length. Left out with a note: a withdrawn release and a macOS archive
/// that is not notarized. Items are newest first; the same inputs always give the same feeds.
pub fn generate(inputs: &FeedInputs) -> Result<Feeds> {
    let repository = valid_repository(inputs.repository)?;
    let mut records = Vec::with_capacity(inputs.records.len());
    for record in inputs.records {
        record.validate()?;
        if inputs
            .public_key
            .is_some_and(|key| *key != record.public_key)
        {
            return Err(Error::RecordKeyMismatch {
                tag: record.tag.clone(),
            });
        }
        records.push((record.semver()?, record));
    }
    records.sort_by(|(a, _), (b, _)| a.cmp_precedence(b));
    for pair in records.windows(2) {
        let ((a, earlier), (b, later)) = (&pair[0], &pair[1]);
        if a.cmp_precedence(b).is_eq() || earlier.tag == later.tag {
            return Err(Error::DuplicateRelease(format!(
                "{} and {} are the same release",
                earlier.tag, later.tag
            )));
        }
    }
    let mut targets: BTreeSet<Target> = Target::RELEASED.into_iter().collect();
    for (_, record) in &records {
        targets.extend(record.archives.keys().copied());
    }
    for &target in &targets {
        let mut previous: Option<&UpdateRecord> = None;
        for (_, record) in &records {
            let Some(archive) = record.archives.get(&target) else {
                continue;
            };
            if let Some(earlier) = previous {
                let earlier_build = earlier.archives[&target].build;
                if archive.build <= earlier_build {
                    return Err(Error::BuildNotRising {
                        target,
                        earlier: earlier.tag.clone(),
                        earlier_build,
                        later: record.tag.clone(),
                        later_build: archive.build,
                    });
                }
            }
            previous = Some(record);
        }
    }

    let mut published = BTreeMap::new();
    for release in inputs.published {
        if published.insert(release.tag.as_str(), release).is_some() {
            return Err(Error::MalformedInput(format!(
                "published releases: {} is listed twice",
                release.tag
            )));
        }
    }
    let withdrawn: BTreeMap<&str, &Withdrawal> = inputs
        .withdrawn
        .iter()
        .map(|w| (w.tag.as_str(), w))
        .collect();
    let mut notes = Vec::new();
    for withdrawal in inputs.withdrawn {
        if !records.iter().any(|(_, r)| r.tag == withdrawal.tag) {
            notes.push(format!(
                "{}: withdrawn, and no record names it",
                withdrawal.tag
            ));
        }
    }
    let mut items: BTreeMap<Target, Vec<AppcastItem>> =
        targets.iter().map(|&t| (t, Vec::new())).collect();
    // Newest first.
    for (_, record) in records.iter().rev() {
        if let Some(withdrawal) = withdrawn.get(record.tag.as_str()) {
            notes.push(format!(
                "{}: withdrawn ({}), not listed",
                record.tag, withdrawal.reason
            ));
            continue;
        }
        for (&target, archive) in &record.archives {
            if target.is_macos() && archive.signing != SigningStatus::Notarized {
                notes.push(format!(
                    "{}: {target} archive is {}, not notarized; not listed",
                    record.tag,
                    archive.signing.as_str()
                ));
                continue;
            }
            let unpublished = || Error::UnpublishedAsset {
                tag: record.tag.clone(),
                file: archive.file.clone(),
            };
            let release = published.get(record.tag.as_str()).ok_or_else(unpublished)?;
            let &length = release.assets.get(&archive.file).ok_or_else(unpublished)?;
            if length != archive.length {
                return Err(Error::AssetLengthMismatch {
                    tag: record.tag.clone(),
                    file: archive.file.clone(),
                    record: archive.length,
                    published: length,
                });
            }
            let date = pub_date(&release.published_at).ok_or_else(|| {
                Error::MalformedInput(format!(
                    "{}: published_at {:?} is not an RFC 3339 UTC time",
                    record.tag, release.published_at
                ))
            })?;
            items
                .get_mut(&target)
                .expect("every target")
                .push(AppcastItem {
                    title: Some(format!("Luxforge {}", record.version)),
                    link: Some(format!(
                        "https://github.com/{repository}/releases/tag/{}",
                        path_segment(&record.tag)
                    )),
                    pub_date: Some(date),
                    build: archive.build,
                    version: record.version.clone(),
                    minimum_system_version: archive.minimum_system_version.clone(),
                    channel: record.channel.sparkle_name().map(str::to_owned),
                    catalog_format: Some(archive.catalog_format),
                    enclosure: Enclosure {
                        url: format!(
                            "https://github.com/{repository}/releases/download/{}/{}",
                            path_segment(&record.tag),
                            path_segment(&archive.file)
                        ),
                        length: archive.length,
                        mime_type: Some(ENCLOSURE_TYPE.to_owned()),
                        ed_signature: archive.ed_signature,
                    },
                });
        }
    }
    let appcasts = items
        .into_iter()
        .map(|(target, items)| {
            let appcast = Appcast {
                title: "Luxforge".to_owned(),
                link: Some(format!("https://github.com/{repository}/releases")),
                description: Some(format!("Luxforge updates for {}", target.label())),
                items,
            };
            (target, appcast)
        })
        .collect();
    Ok(Feeds { appcasts, notes })
}

fn valid_repository(repository: &str) -> Result<&str> {
    let part = |p: &str| {
        !p.is_empty()
            && !p.starts_with('.')
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    match repository.split_once('/') {
        Some((owner, name)) if part(owner) && part(name) => Ok(repository),
        _ => Err(Error::MalformedInput(format!(
            "repository {repository:?} is not owner/name"
        ))),
    }
}

/// `text` percent-encoded for one URL path segment: every byte outside RFC 3986's unreserved
/// characters, such as a tag's `+`, is encoded.
fn path_segment(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}
