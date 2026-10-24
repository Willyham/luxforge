//! A release's section of `CHANGELOG.md`, which its published notes include.
use crate::{Result, ensure, release_tag::ReleaseTag};
use std::{fs, path::Path};

pub const FILE: &str = "CHANGELOG.md";

/// The body of the `## MAJOR.MINOR.PATCH` section for `tag`: a pre-release such as `v0.2.0-rc.1`
/// uses the section of `0.2.0`. A missing, repeated or empty section is refused.
pub fn section(text: &str, tag: &ReleaseTag) -> Result<String> {
    let heading = format!("## {}", tag.bundle_version);
    let mut sections = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.starts_with("## ") {
            sections.extend(current.take());
            if line.trim_end() == heading {
                current = Some(Vec::new());
            }
        } else if let Some(body) = &mut current {
            body.push(line);
        }
    }
    sections.extend(current);
    ensure(
        sections.len() < 2,
        format!("{FILE} repeats the section {heading}"),
    )?;
    let body = sections
        .pop()
        .ok_or_else(|| {
            format!("{FILE} has no section {heading}; add the release's changes before releasing")
        })?
        .join("\n")
        .trim()
        .to_owned();
    ensure(
        !body.is_empty(),
        format!("{FILE}'s section {heading} is empty"),
    )?;
    Ok(body)
}

pub fn read(root: &Path, tag: &ReleaseTag) -> Result<String> {
    let text = fs::read_to_string(root.join(FILE)).map_err(|error| format!("{FILE}: {error}"))?;
    section(&text, tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str =
        "# Changelog\n\nIntro.\n\n## 0.2.0\n\n### Agents\n\n- MCP.\n\n## 0.1.0\n\n- First.\n";

    #[test]
    fn a_release_and_its_prereleases_read_their_version_section() {
        for tag in ["v0.2", "v0.2.0", "v0.2.0-rc.1", "v0.2.0-rc.2+build.3"] {
            let tag = ReleaseTag::parse(tag).unwrap();
            assert_eq!(section(TEXT, &tag).unwrap(), "### Agents\n\n- MCP.");
        }
        let tag = ReleaseTag::parse("v0.1.0").unwrap();
        assert_eq!(section(TEXT, &tag).unwrap(), "- First.");
    }

    #[test]
    fn a_missing_repeated_or_empty_section_is_refused() {
        let tag = ReleaseTag::parse("v0.3.0-rc.1").unwrap();
        assert!(section(TEXT, &tag).is_err());
        let tag = ReleaseTag::parse("v0.1.0").unwrap();
        assert!(section(&format!("{TEXT}\n## 0.1.0\n\n- Again.\n"), &tag).is_err());
        assert!(section("## 0.1.0\n## 0.0.9\n\n- Older.\n", &tag).is_err());
        assert!(section("## 0.1.0\n\n", &tag).is_err());
    }
}
