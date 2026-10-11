//! Release-tag identity, parsed before CI installs native dependencies or builds the editor.
use crate::{Args, Result, ensure};
use semver::Version;
use serde_json::json;
use std::{fs::OpenOptions, io::Write};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseTag {
    pub tag: String,
    pub version: String,
    pub bundle_version: String,
}

impl ReleaseTag {
    pub fn parse(tag: &str) -> Result<Self> {
        ensure(tag.len() <= 128, "Release tag is too long")?;
        let text = tag
            .strip_prefix('v')
            .ok_or("Release tags must start with v")?;
        let split = text.find(['-', '+']).unwrap_or(text.len());
        let (numbers, suffix) = text.split_at(split);
        let normalized = match numbers.split('.').count() {
            2 => format!("{numbers}.0{suffix}"),
            3 => text.to_owned(),
            _ => {
                return Err(
                    "Use vMAJOR.MINOR or vMAJOR.MINOR.PATCH, optionally with a SemVer suffix"
                        .into(),
                );
            }
        };
        let version = Version::parse(&normalized)?;
        ensure(
            [version.major, version.minor, version.patch]
                .into_iter()
                .all(|v| v <= u32::MAX as u64),
            "Release version components must fit in 32 bits",
        )?;
        Ok(Self {
            tag: tag.to_owned(),
            version: version.to_string(),
            bundle_version: format!("{}.{}.{}", version.major, version.minor, version.patch),
        })
    }
}

pub fn run(args: &mut Args) -> Result {
    let tag = args.value("--tag")?.ok_or("Required: --tag")?;
    let tag = ReleaseTag::parse(tag.to_str().ok_or("Invalid tag encoding")?)?;
    let github_output = args.value("--github-output")?;
    args.done()?;
    if let Some(path) = github_output {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        // SemVer validation excludes newlines and workflow-output injection.
        writeln!(file, "tag={}\nversion={}", tag.tag, tag.version)?;
    }
    println!(
        "{}",
        json!({"tag":tag.tag,"version":tag.version,"bundle_version":tag.bundle_version})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_full_and_semver_suffixes_share_normalized_identity() {
        for (input, expected) in [
            ("v0.1", "0.1.0"),
            ("v0.1.0", "0.1.0"),
            ("v12.34.56", "12.34.56"),
            ("v0.1-rc.1", "0.1.0-rc.1"),
            ("v0.1.2-rc.1+build.4", "0.1.2-rc.1+build.4"),
        ] {
            let tag = ReleaseTag::parse(input).unwrap();
            assert_eq!(tag.tag, input);
            assert_eq!(tag.version, expected);
            assert!(!tag.bundle_version.contains(['-', '+']));
        }
    }

    #[test]
    fn malformed_versions_and_workflow_or_shell_injection_are_refused() {
        for input in [
            "0.1",
            "v1",
            "v01.2",
            "v1.02.3",
            "v1.2.3.4",
            "v1.2-01",
            "v1.2;echo",
            "v1.2\nversion=9.0.0",
            "v1.2$(id)",
            "v1.2/other",
            "v4294967296.1",
        ] {
            assert!(ReleaseTag::parse(input).is_err(), "accepted {input:?}");
        }
    }
}
