//! The per-release update record, `luxforge-<version>-updates.json`, a release asset.
//!
//! The release workflow signs the final archives once and writes one record; the feeds are
//! regenerated from the records of every published release, so the release history is the only
//! source of truth.
use crate::{
    Channel, Target,
    error::{Error, Result},
    key::{PublicKey, Signature},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The record shape this crate reads and writes. Any other is refused.
pub const RECORD_SCHEMA: u32 = 1;
/// The largest record read.
pub const MAX_RECORD_BYTES: usize = 64 * 1024;

/// A macOS archive's signing state, as the release build's `target.json` records it. Only a
/// notarized macOS archive is ever listed in a feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SigningStatus {
    Notarized,
    Unsigned,
    Pending,
    Failed,
    /// A target without platform signing, Linux.
    NotApplicable,
}

impl SigningStatus {
    pub fn parse(text: &str) -> Option<SigningStatus> {
        serde_json::from_value(serde_json::Value::String(text.to_owned())).ok()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SigningStatus::Notarized => "notarized",
            SigningStatus::Unsigned => "unsigned",
            SigningStatus::Pending => "pending",
            SigningStatus::Failed => "failed",
            SigningStatus::NotApplicable => "not-applicable",
        }
    }
}

/// One target's archive in a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveRecord {
    /// The release asset's file name, `luxforge-<version>-<target>.<zip|tar.gz>`.
    pub file: String,
    /// Its byte length.
    pub length: u64,
    /// The build number, which orders releases: `CFBundleVersion` on macOS and `build.json`'s
    /// `build_number` on Linux.
    pub build: u64,
    /// `sparkle:edSignature` over the whole archive.
    pub ed_signature: Signature,
    /// `LSMinimumSystemVersion`, macOS only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_system_version: Option<String>,
    /// The catalog format the build opens.
    pub catalog_format: u32,
    pub signing: SigningStatus,
}

/// A release's update record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRecord {
    pub schema_version: u32,
    /// The release tag, such as `v0.2.0-rc.1`.
    pub tag: String,
    /// The tag's normalized SemVer version, such as `0.2.0-rc.1`.
    pub version: String,
    pub channel: Channel,
    /// The public key of the key that signed every archive.
    pub public_key: PublicKey,
    pub archives: BTreeMap<Target, ArchiveRecord>,
}

impl UpdateRecord {
    /// The record's release asset name: `luxforge-<version>-updates.json`.
    pub fn file_name(version: &str) -> String {
        format!("luxforge-{version}-updates.json")
    }

    /// Reads and validates a record.
    pub fn parse(bytes: &[u8]) -> Result<UpdateRecord> {
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(Error::MalformedRecord(format!(
                "a record is at most {MAX_RECORD_BYTES} bytes"
            )));
        }
        let record: UpdateRecord = serde_json::from_slice(bytes)
            .map_err(|error| Error::MalformedRecord(error.to_string()))?;
        record.validate()?;
        Ok(record)
    }

    /// The record as written: pretty JSON with a final newline. Field and target order are fixed,
    /// so one record always writes the same bytes.
    pub fn to_json(&self) -> String {
        format!(
            "{}\n",
            serde_json::to_string_pretty(self).expect("a record serializes")
        )
    }

    /// The record's SemVer version.
    pub fn semver(&self) -> Result<semver::Version> {
        semver::Version::parse(&self.version)
            .map_err(|_| self.refuse(format!("version {:?} is not SemVer", self.version)))
    }

    /// Checks the record against itself: its shape, its tag, version and channel, and each
    /// archive's name and fields.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != RECORD_SCHEMA {
            return Err(self.refuse(format!(
                "schema_version {} is not the current {RECORD_SCHEMA}",
                self.schema_version
            )));
        }
        let version = self.semver()?;
        if version.to_string() != self.version {
            return Err(self.refuse("the version is not in normalized form".into()));
        }
        if tag_version(&self.tag).as_deref() != Some(self.version.as_str()) {
            return Err(self.refuse(format!("the tag does not name version {}", self.version)));
        }
        if self.channel != Channel::of(&version) {
            return Err(self.refuse("the channel does not match the version".into()));
        }
        if self.archives.is_empty() {
            return Err(self.refuse("it lists no archive".into()));
        }
        for (target, archive) in &self.archives {
            let prefix = format!("luxforge-{}-{target}.", self.version);
            let safe = archive
                .file
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b));
            if !archive.file.starts_with(&prefix) || !safe {
                return Err(self.refuse(format!(
                    "{target}'s file {:?} is not {prefix}<extension>",
                    archive.file
                )));
            }
            if archive.length == 0 || archive.build == 0 || archive.catalog_format == 0 {
                return Err(self.refuse(format!(
                    "{target}'s length, build and catalog format must be positive"
                )));
            }
            match (&archive.minimum_system_version, target.is_macos()) {
                (Some(minimum), true) if system_version(minimum) => {}
                (None, false) => {}
                _ => {
                    return Err(self.refuse(format!(
                        "{target} needs a minimum system version exactly when it is macOS"
                    )));
                }
            }
        }
        Ok(())
    }

    fn refuse(&self, why: String) -> Error {
        Error::MalformedRecord(format!("{}: {why}", self.tag))
    }
}

/// The normalized version a release tag names: `v0.1` and `v0.1.0` name `0.1.0`, and
/// `v0.2-rc.1` names `0.2.0-rc.1`, as `cargo xtask release-tag` normalizes them.
pub fn tag_version(tag: &str) -> Option<String> {
    let text = tag.strip_prefix('v')?;
    let split = text.find(['-', '+']).unwrap_or(text.len());
    let (numbers, suffix) = text.split_at(split);
    let normalized = match numbers.split('.').count() {
        2 => format!("{numbers}.0{suffix}"),
        3 => text.to_owned(),
        _ => return None,
    };
    semver::Version::parse(&normalized)
        .ok()
        .map(|v| v.to_string())
}

/// Whether `text` is a dotted numeric system version such as `14.0`.
fn system_version(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 16
        && text
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}
