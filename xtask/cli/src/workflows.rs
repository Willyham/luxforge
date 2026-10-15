//! The GitHub workflows' rules, part of `cargo xtask check-repository`: every action is pinned to a
//! full commit, every workflow states its permissions, write permission is granted only where it is
//! needed, no workflow hands out all its secrets, and each scoped secret reaches exactly one step.
//!
//! The workflows are read line by line, as they are written here (two-space indentation, one key
//! per line), rather than through a YAML parser; a workflow written otherwise fails these rules.
use crate::{Result, ensure};
use regex::Regex;
use std::{collections::BTreeMap, fs, path::Path};

/// Where workflows live.
pub const DIR: &str = ".github/workflows";

/// Each secret only one step may receive: the secret, its workflow and the step's name. The step
/// must run a script, not an action.
pub const SCOPED_SECRETS: &[(&str, &str, &str)] = &[
    (
        "LUXFORGE_UPDATE_SIGNING_KEY",
        "release.yml",
        "Sign the update record",
    ),
    (
        "LUXFORGE_UPDATE_PUBLISH_SSH_KEY",
        "update-feeds.yml",
        "Publish the feeds to the update store",
    ),
];

/// The only write permissions granted: the workflow, the job and the permission. Publication
/// writes the release, and attests its downloads' build provenance with an OIDC token.
pub const WRITE_PERMISSIONS: &[(&str, &str, &str)] = &[
    ("release.yml", "publish", "contents"),
    ("release.yml", "publish", "id-token"),
    ("release.yml", "publish", "attestations"),
];

/// Constructs that hand a workflow's secrets or privileges to code it did not choose.
const FORBIDDEN: &[(&str, &str)] = &[
    ("secrets: inherit", "passes every secret on"),
    ("toJSON(secrets)", "exposes every secret"),
    ("pull_request_target", "runs pull requests with secrets"),
    ("permissions: write-all", "grants every permission"),
];

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// A line without its comment, for the workflows' plain values.
fn code(line: &str) -> &str {
    match line.find(" #") {
        Some(at) => &line[..at],
        None if line.trim_start().starts_with('#') => "",
        None => line,
    }
}

/// The list item (a step) that line `at` lies in, as its first line's index, when that item is in
/// a `steps:` list.
fn step_start(lines: &[&str], at: usize) -> Option<usize> {
    let depth = indent(lines[at]);
    let start = (0..=at).rev().find(|&i| {
        let line = code(lines[i]);
        line.trim_start().starts_with("- ") && (i == at || indent(line) < depth)
    })?;
    let item = indent(lines[start]);
    let parent = (0..start)
        .rev()
        .find(|&i| !code(lines[i]).trim().is_empty() && indent(lines[i]) < item)?;
    (code(lines[parent]).trim() == "steps:").then_some(start)
}

/// The keys of the step starting at `start`, with their values, and the index after its last
/// line.
fn step_keys<'a>(lines: &[&'a str], start: usize) -> (BTreeMap<&'a str, &'a str>, usize) {
    let item = indent(lines[start]);
    let mut keys = BTreeMap::new();
    let mut add = |line: &'a str| {
        if let Some((key, value)) = line.split_once(':') {
            keys.entry(key.trim()).or_insert(value.trim());
        }
    };
    add(&code(lines[start]).trim_start()[2..]);
    let mut end = start + 1;
    while end < lines.len() {
        let line = code(lines[end]);
        if !line.trim().is_empty() {
            if indent(line) <= item {
                break;
            }
            if indent(line) == item + 2 {
                add(line.trim_start());
            }
        }
        end += 1;
    }
    (keys, end)
}

/// The job a line lies in: the nearest two-space key above it under `jobs:`.
fn job_of<'a>(lines: &[&'a str], at: usize) -> Option<&'a str> {
    for i in (0..=at).rev() {
        let line = code(lines[i]);
        if indent(line) == 0 && !line.trim().is_empty() {
            return None;
        }
        if indent(line) == 2 && line.trim_end().ends_with(':') {
            return Some(line.trim().trim_end_matches(':'));
        }
    }
    None
}

/// Checks every workflow and answers how many it read.
pub fn check(root: &Path) -> Result<usize> {
    let dir = root.join(DIR);
    if !dir.is_dir() {
        return Ok(0);
    }
    let pinned = Regex::new(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$")?;
    let secret = Regex::new(r#"secrets(?:\.([A-Za-z0-9_]+)|\[\s*['"]([A-Za-z0-9_]+)['"]\s*\])"#)?;
    let write = Regex::new(r"^\s+([a-z-]+):\s*write\s*$")?;
    let mut names = fs::read_dir(&dir)?
        .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.retain(|name| name.ends_with(".yml") || name.ends_with(".yaml"));
    names.sort();
    let mut refusals = Vec::new();
    let mut scoped: BTreeMap<&str, usize> = BTreeMap::new();
    for name in &names {
        let text = fs::read_to_string(dir.join(name))?;
        let lines: Vec<&str> = text.lines().collect();
        let mut refuse = |line: usize, why: String| {
            refusals.push(format!("{DIR}/{name}:{line}: {why}"));
        };
        if !lines
            .iter()
            .any(|line| code(line).trim_end() == "permissions:")
        {
            refuse(1, "the workflow states no top-level permissions".into());
        }
        for (i, line) in lines.iter().enumerate() {
            let code = code(line);
            let number = i + 1;
            if let Some(value) = code
                .trim_start()
                .trim_start_matches("- ")
                .strip_prefix("uses:")
            {
                let value = value.trim().trim_matches(['"', '\'']);
                if !value.starts_with("./") && !pinned.is_match(value) {
                    refuse(
                        number,
                        format!("`{value}` is not pinned to a full commit SHA"),
                    );
                }
            }
            for (construct, why) in FORBIDDEN {
                if code.contains(construct) {
                    refuse(number, format!("`{construct}` {why}"));
                }
            }
            if let Some(permission) = write.captures(code) {
                let job = job_of(&lines, i).unwrap_or("(top level)");
                if !WRITE_PERMISSIONS
                    .iter()
                    .any(|w| (w.0, w.1, w.2) == (name.as_str(), job, &permission[1]))
                {
                    refuse(
                        number,
                        format!(
                            "{} write permission in {job} is not one the workflows need",
                            &permission[1]
                        ),
                    );
                }
            }
            for capture in secret.captures_iter(code) {
                let named = capture.get(1).or(capture.get(2)).map_or("", |m| m.as_str());
                let Some(&(secret, file, step)) = SCOPED_SECRETS.iter().find(|s| s.0 == named)
                else {
                    continue;
                };
                *scoped.entry(secret).or_default() += 1;
                let place = step_start(&lines, i).map(|start| step_keys(&lines, start).0);
                let in_step = place
                    .as_ref()
                    .and_then(|keys| keys.get("name").copied())
                    .is_some_and(|n| n == step);
                if name != file || !in_step {
                    refuse(
                        number,
                        format!("{secret} may reach only the step `{step}` of {DIR}/{file}"),
                    );
                } else if place.is_some_and(|keys| keys.contains_key("uses")) {
                    refuse(
                        number,
                        format!("`{step}` runs an action; {secret} may reach only a script"),
                    );
                }
            }
        }
    }
    for (secret, file, step) in SCOPED_SECRETS {
        let count = scoped.get(secret).copied().unwrap_or(0);
        if count != 1 {
            refusals.push(format!(
                "{secret} is referenced {count} times; it must be named once, in the step \
                 `{step}` of {DIR}/{file}"
            ));
        }
    }
    ensure(refusals.is_empty(), refusals.join("\n"))?;
    Ok(names.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = "name: Release builds
on:
  push:
    tags: ['v*']
permissions:
  contents: read
jobs:
  build:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2
      - name: Build
        run: cargo build
  publish:
    runs-on: ubuntu-24.04
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2
      # Only this step receives the key.
      - name: Sign the update record
        if: vars.LUXFORGE_PUBLISH_UPDATES == 'true'
        env:
          LUXFORGE_UPDATE_SIGNING_KEY: ${{ secrets.LUXFORGE_UPDATE_SIGNING_KEY }}
        run: |
          sign
      - name: Publish
        env:
          GH_TOKEN: ${{ github.token }}
        run: gh release create
";

    const FEEDS: &str = "name: Update feeds
on:
  workflow_dispatch:
permissions:
  contents: read
jobs:
  feeds:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683
      - name: Publish the feeds to the update store
        env:
          LUXFORGE_UPDATE_PUBLISH_SSH_KEY: ${{ secrets.LUXFORGE_UPDATE_PUBLISH_SSH_KEY }}
        run: ssh
";

    fn tree(release: &str, feeds: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("release.yml"), release).unwrap();
        fs::write(dir.join("update-feeds.yml"), feeds).unwrap();
        tmp
    }

    fn refused(release: &str, feeds: &str, expected: &str) {
        let tmp = tree(release, feeds);
        let error = check(tmp.path())
            .err()
            .unwrap_or_else(|| panic!("accepted; expected {expected}"))
            .to_string();
        assert!(error.contains(expected), "expected {expected}: {error}");
    }

    #[test]
    fn the_repositorys_workflows_keep_their_rules() {
        assert!(check(&crate::root().unwrap()).unwrap() >= 3);
    }

    #[test]
    fn scoped_secrets_reach_one_script_step_only() {
        assert_eq!(check(tree(RELEASE, FEEDS).path()).unwrap(), 2);
        let key = "${{ secrets.LUXFORGE_UPDATE_SIGNING_KEY }}";
        // In a second step as well.
        refused(
            RELEASE,
            &FEEDS.replace(
                "        run: ssh\n",
                &format!("        run: ssh\n      - name: Other\n        env:\n          K: {key}\n        run: x\n"),
            ),
            "LUXFORGE_UPDATE_SIGNING_KEY may reach only the step `Sign the update record` of .github/workflows/release.yml",
        );
        // In the job's environment, which every step receives.
        refused(
            &RELEASE.replace(
                "    permissions:\n      contents: write\n",
                &format!("    permissions:\n      contents: write\n    env:\n      K: {key}\n"),
            ),
            FEEDS,
            "may reach only the step",
        );
        // In another step of the right workflow.
        refused(
            &RELEASE.replace(
                "          GH_TOKEN: ${{ github.token }}\n",
                &format!("          GH_TOKEN: ${{{{ github.token }}}}\n          K: {key}\n"),
            ),
            FEEDS,
            "referenced 2 times",
        );
        // Through an action rather than a script.
        refused(
            &RELEASE.replace(
                "        run: |\n          sign\n",
                "        uses: someone/sign@0123456789012345678901234567890123456789\n",
            ),
            FEEDS,
            "runs an action",
        );
        // Indexed rather than dotted, outside its step.
        refused(
            &RELEASE.replace(
                "        run: gh release create\n",
                "        run: echo ${{ secrets['LUXFORGE_UPDATE_SIGNING_KEY'] }}\n",
            ),
            FEEDS,
            "may reach only the step",
        );
        // Not at all.
        refused(
            RELEASE,
            &FEEDS.replace("${{ secrets.LUXFORGE_UPDATE_PUBLISH_SSH_KEY }}", "x"),
            "LUXFORGE_UPDATE_PUBLISH_SSH_KEY is referenced 0 times",
        );
        // A step outside a steps list (a matrix entry) is not a step.
        refused(
            &RELEASE.replace(
                "    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2\n      - name: Build\n",
                &format!("    runs-on: ubuntu-24.04\n    strategy:\n      matrix:\n        include:\n          - name: Sign the update record\n            k: {key}\n    steps:\n      - name: Build\n"),
            ),
            FEEDS,
            "referenced 2 times",
        );
    }

    #[test]
    fn actions_are_pinned_and_privileges_are_least() {
        refused(
            &RELEASE.replace(
                "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2",
                "actions/checkout@v4",
            ),
            FEEDS,
            "`actions/checkout@v4` is not pinned",
        );
        refused(
            RELEASE,
            &FEEDS.replace(
                "      - name: Publish",
                "      - uses: someone/action@main\n      - name: Publish",
            ),
            "`someone/action@main` is not pinned",
        );
        refused(
            &RELEASE.replace("permissions:\n  contents: read\n", ""),
            FEEDS,
            "release.yml:1: the workflow states no top-level permissions",
        );
        refused(
            RELEASE,
            &FEEDS.replace("  contents: read\n", "  contents: write\n"),
            "contents write permission in (top level)",
        );
        refused(
            &RELEASE.replace(
                "      contents: write\n",
                "      contents: write\n      packages: write\n",
            ),
            FEEDS,
            "packages write permission in publish",
        );
        refused(
            &RELEASE.replace(
                "  build:\n    runs-on: ubuntu-24.04\n",
                "  build:\n    runs-on: ubuntu-24.04\n    permissions:\n      id-token: write\n",
            ),
            FEEDS,
            "id-token write permission in build",
        );
        for construct in ["secrets: inherit", "toJSON(secrets)", "pull_request_target"] {
            refused(
                RELEASE,
                &FEEDS.replace(
                    "  workflow_dispatch:\n",
                    &format!(
                        "  workflow_dispatch:\n# not a comment {construct}\n  x: {construct}\n"
                    ),
                ),
                construct,
            );
        }
        // A local reusable workflow needs no pin, and a commented line is not read.
        let local = RELEASE.replace(
            "      - name: Build\n",
            "      # - uses: actions/checkout@v4\n      - uses: ./.github/actions/local\n      - name: Build\n",
        );
        check(tree(&local, FEEDS).path()).unwrap();
    }
}
