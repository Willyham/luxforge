//! Start the hosted release workflow only from a verified, published main commit.
use crate::{Args, Result, check, ensure, output, release_tag::ReleaseTag};
use std::{path::Path, process::Command};

#[derive(Debug, PartialEq, Eq)]
struct State {
    revision: String,
    destination: String,
}

fn unused_version(tag: &ReleaseTag, names: impl Iterator<Item = impl AsRef<str>>) -> Result {
    for name in names {
        let name = name.as_ref();
        if ReleaseTag::parse(name).is_ok_and(|existing| existing.version == tag.version) {
            return Err(format!(
                "Version {} already has tag {name}; choose a new version",
                tag.version
            )
            .into());
        }
    }
    Ok(())
}

fn preflight(root: &Path, tag: &ReleaseTag) -> Result<State> {
    let branch = output(root, "git", &["symbolic-ref", "--quiet", "HEAD"])
        .map_err(|_| "Release requires an attached main branch (detached HEAD is refused)")?;
    ensure(
        branch.trim() == "refs/heads/main",
        "Release is allowed only on main",
    )?;
    ensure(
        output(
            root,
            "git",
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?
        .trim()
        .is_empty(),
        "Release requires a clean index and working tree, including untracked files",
    )?;
    ensure(
        Command::new("git")
            .args(["cat-file", "-e", "HEAD:.github/workflows/release.yml"])
            .current_dir(root)
            .output()?
            .status
            .success(),
        "Commit .github/workflows/release.yml before releasing",
    )?;
    unused_version(
        tag,
        output(
            root,
            "git",
            &["for-each-ref", "--format=%(refname:strip=2)", "refs/tags"],
        )?
        .lines(),
    )?;
    let destinations = output(
        root,
        "git",
        &["remote", "get-url", "--push", "--all", "origin"],
    )?;
    let destinations: Vec<_> = destinations.lines().collect();
    ensure(
        destinations.len() == 1 && !destinations[0].is_empty(),
        "origin must have exactly one push destination",
    )?;
    let destination = destinations[0].to_owned();
    // Query the actual push destination, rather than a stale tracking ref or a separate fetch URL.
    let refs = output(
        root,
        "git",
        &[
            "ls-remote",
            "--refs",
            "--",
            &destination,
            "refs/heads/main",
            "refs/tags/v*",
        ],
    )?;
    let refs: Vec<_> = refs
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    let main = refs
        .iter()
        .find_map(|(commit, name)| (*name == "refs/heads/main").then_some(*commit))
        .ok_or("origin's push destination has no main branch")?;
    let revision = output(root, "git", &["rev-parse", "HEAD"])?
        .trim()
        .to_owned();
    ensure(
        revision == main,
        "HEAD must match the live origin/main; push or pull main before releasing",
    )?;
    unused_version(
        tag,
        refs.iter()
            .filter_map(|(_, name)| name.strip_prefix("refs/tags/")),
    )?;
    Ok(State {
        revision,
        destination,
    })
}

fn release(
    root: &Path,
    tag: &ReleaseTag,
    dry_run: bool,
    checks: &mut impl FnMut() -> Result,
) -> Result {
    let state = preflight(root, tag)?;
    println!(
        "Release {} (version {}) from {}",
        tag.tag, tag.version, state.revision
    );
    if dry_run {
        println!(
            "Dry run: Git/remote checks passed. Would run quick and native watcher checks, create an annotated tag and push only {} to origin. No tag created or pushed; quick and native watcher checks did not run.",
            tag.tag
        );
        return Ok(());
    }
    println!("Running quick and native watcher checks before creating the release tag...");
    checks()?;
    ensure(
        preflight(root, tag)? == state,
        "Release commit or push destination changed during checks; no tag created",
    )?;
    output(
        root,
        "git",
        &[
            "tag",
            "--annotate",
            "--message",
            &format!("Luxforge {}", tag.version),
            &tag.tag,
            &state.revision,
        ],
    )?;
    let result = Command::new("git")
        .args([
            "push",
            "--porcelain",
            "--no-follow-tags",
            "--no-mirror",
            "--",
        ])
        .arg(&state.destination)
        .arg(format!("refs/tags/{0}:refs/tags/{0}", tag.tag))
        .current_dir(root)
        .status();
    let pushed = result.as_ref().is_ok_and(|status| status.success());
    ensure(
        pushed,
        format!(
            "Tag push failed; local tag {} was kept. Check the remote before retrying; existing tags are never overwritten.{}",
            tag.tag,
            result
                .err()
                .map(|error| format!(" {error}"))
                .unwrap_or_default()
        ),
    )?;
    println!(
        "Pushed {}. GitHub Actions → Release builds will build both platforms and automatically publish the pre-release after successful checks/uploads. This command does not wait for those jobs.",
        tag.tag
    );
    Ok(())
}

pub fn run(root: &Path, args: &mut Args) -> Result {
    let dry_run = args.flag("--dry-run");
    ensure(
        args.0.len() == 1,
        "Usage: cargo xtask release TAG [--dry-run]",
    )?;
    let tag = ReleaseTag::parse(args.0.remove(0).to_str().ok_or("Invalid tag encoding")?)?;
    args.done()?;
    release(root, &tag, dry_run, &mut || {
        check::check(root, true)?;
        check::native_watchers(root)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_test_macros::slow;
    use std::{fs, path::PathBuf};

    struct Repository {
        _temp: tempfile::TempDir,
        root: PathBuf,
        remote: PathBuf,
    }

    impl Repository {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("checkout with spaces");
            let remote = temp.path().join("remote.git");
            fs::create_dir(&root).unwrap();
            let repo = Self {
                _temp: temp,
                root,
                remote,
            };
            repo.git(&["init", "--initial-branch=main"]);
            for (key, value) in [
                ("user.name", "Release Test"),
                ("user.email", "release@example.invalid"),
                ("commit.gpgSign", "false"),
                ("tag.gpgSign", "false"),
                ("core.hooksPath", ".git/test-hooks"),
            ] {
                repo.git(&["config", key, value]);
            }
            fs::create_dir_all(repo.root.join(".github/workflows")).unwrap();
            fs::write(
                repo.root.join(".github/workflows/release.yml"),
                "name: Release builds\n",
            )
            .unwrap();
            repo.git(&["add", "."]);
            repo.git(&["commit", "-m", "release tooling"]);
            repo.git(&["init", "--bare", repo.remote.to_str().unwrap()]);
            repo.remote_git(&["config", "core.hooksPath", "hooks"]);
            repo.git(&["remote", "add", "origin", repo.remote.to_str().unwrap()]);
            repo.git(&["push", "origin", "main"]);
            repo
        }

        fn git(&self, args: &[&str]) -> String {
            output(&self.root, "git", args).unwrap()
        }

        fn remote_git(&self, args: &[&str]) -> String {
            output(&self.remote, "git", args).unwrap()
        }

        fn start(&self, dry_run: bool, checks: &mut impl FnMut() -> Result) -> Result {
            release(
                &self.root,
                &ReleaseTag::parse("v0.1").unwrap(),
                dry_run,
                checks,
            )
        }

        fn no_tags(&self) {
            assert!(self.git(&["tag", "--list"]).trim().is_empty());
            assert!(self.remote_git(&["tag", "--list"]).trim().is_empty());
        }
    }

    #[test]
    #[slow]
    fn release_pushes_one_annotated_tag_after_checks_and_honors_push_destination() {
        let repo = Repository::new();
        let fetch_remote = repo._temp.path().join("unrelated-fetch.git");
        repo.git(&["init", "--bare", fetch_remote.to_str().unwrap()]);
        repo.git(&[
            "remote",
            "set-url",
            "--push",
            "origin",
            repo.remote.to_str().unwrap(),
        ]);
        repo.git(&[
            "remote",
            "set-url",
            "origin",
            fetch_remote.to_str().unwrap(),
        ]);
        // This annotated non-release tag must not get pushed through push.followTags.
        repo.git(&["tag", "-a", "internal-check", "-m", "internal"]);
        repo.git(&["config", "push.followTags", "true"]);
        let before = repo.remote_git(&["show-ref", "--heads"]);
        let mut checked = false;
        repo.start(false, &mut || {
            checked = true;
            Ok(())
        })
        .unwrap();
        assert!(checked);
        assert_eq!(repo.remote_git(&["tag", "--list"]).trim(), "v0.1");
        assert_eq!(
            repo.remote_git(&["cat-file", "-t", "refs/tags/v0.1"])
                .trim(),
            "tag"
        );
        assert_eq!(
            repo.remote_git(&["rev-parse", "v0.1^{commit}"]),
            repo.git(&["rev-parse", "HEAD"])
        );
        assert_eq!(repo.remote_git(&["show-ref", "--heads"]), before);
        assert!(
            output(&fetch_remote, "git", &["tag", "--list"])
                .unwrap()
                .trim()
                .is_empty()
        );
    }

    #[test]
    #[slow]
    fn release_dry_run_and_check_failure_leave_refs_unchanged() {
        let repo = Repository::new();
        repo.start(true, &mut || panic!("dry-run must not run quick checks"))
            .unwrap();
        repo.no_tags();
        assert!(
            repo.start(false, &mut || Err("check failed".into()))
                .is_err()
        );
        repo.no_tags();
    }

    #[test]
    #[slow]
    fn release_refuses_dirty_staged_untracked_branch_and_detached_states() {
        let repo = Repository::new();
        fs::write(repo.root.join("untracked"), "untracked").unwrap();
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.git(&["add", "untracked"]);
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.git(&["reset", "--", "untracked"]);
        fs::remove_file(repo.root.join("untracked")).unwrap();
        fs::write(repo.root.join(".github/workflows/release.yml"), "changed").unwrap();
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.git(&["restore", ".github/workflows/release.yml"]);
        repo.git(&["switch", "-c", "feature"]);
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.git(&["switch", "--detach", "main"]);
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.no_tags();
    }

    #[test]
    #[slow]
    fn release_refuses_local_and_remote_version_aliases() {
        let repo = Repository::new();
        repo.git(&["tag", "v0.1.0"]);
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.git(&["push", "origin", "refs/tags/v0.1.0"]);
        repo.git(&["tag", "-d", "v0.1.0"]);
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        assert_eq!(repo.remote_git(&["tag", "--list"]).trim(), "v0.1.0");
        assert!(repo.git(&["tag", "--list"]).trim().is_empty());
    }

    #[test]
    #[slow]
    fn release_requires_committed_workflow_single_origin_and_matching_live_main() {
        let repo = Repository::new();
        repo.git(&["remote", "remove", "origin"]);
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.git(&["remote", "add", "origin", repo.remote.to_str().unwrap()]);
        repo.git(&[
            "config",
            "--add",
            "remote.origin.pushurl",
            repo.remote.to_str().unwrap(),
        ]);
        repo.git(&[
            "config",
            "--add",
            "remote.origin.pushurl",
            repo.remote.to_str().unwrap(),
        ]);
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.git(&["config", "--unset-all", "remote.origin.pushurl"]);
        repo.git(&["commit", "--allow-empty", "-m", "ahead"]);
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.git(&["push", "origin", "main"]);
        repo.git(&["reset", "--hard", "HEAD~1"]);
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.git(&["reset", "--hard", "origin/main"]);
        repo.git(&["rm", ".github/workflows/release.yml"]);
        repo.git(&["commit", "-m", "remove workflow"]);
        repo.git(&["push", "origin", "main"]);
        assert!(
            repo.start(false, &mut || panic!("checks must not run"))
                .is_err()
        );
        repo.no_tags();
    }

    #[test]
    #[slow]
    fn release_rechecks_state_after_checks() {
        for mutation in ["dirty", "branch", "commit", "remote", "destination"] {
            let repo = Repository::new();
            let mut checks = || {
                match mutation {
                    "dirty" => fs::write(repo.root.join("late change"), "changed")?,
                    "branch" => {
                        repo.git(&["switch", "-c", "late-branch"]);
                    }
                    "commit" => {
                        repo.git(&["commit", "--allow-empty", "-m", "late commit"]);
                        repo.git(&["push", "origin", "main"]);
                    }
                    "remote" => {
                        repo.remote_git(&["update-ref", "-d", "refs/heads/main"]);
                    }
                    "destination" => {
                        let other = repo._temp.path().join("other.git");
                        repo.git(&[
                            "clone",
                            "--bare",
                            repo.remote.to_str().unwrap(),
                            other.to_str().unwrap(),
                        ]);
                        repo.git(&[
                            "remote",
                            "set-url",
                            "--push",
                            "origin",
                            other.to_str().unwrap(),
                        ]);
                    }
                    _ => unreachable!(),
                }
                Ok(())
            };
            assert!(
                repo.start(false, &mut checks).is_err(),
                "accepted {mutation}"
            );
            repo.no_tags();
        }
    }

    #[cfg(unix)]
    #[test]
    #[slow]
    fn release_rejected_push_retains_local_tag_without_remote_changes() {
        use std::os::unix::fs::PermissionsExt;
        let repo = Repository::new();
        let hook = repo.remote.join("hooks/pre-receive");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let error = repo.start(false, &mut || Ok(())).unwrap_err().to_string();
        assert!(error.contains("local tag v0.1 was kept"));
        assert_eq!(repo.git(&["tag", "--list"]).trim(), "v0.1");
        assert!(repo.remote_git(&["tag", "--list"]).trim().is_empty());
    }

    #[test]
    fn release_invalid_arguments_fail_before_git_queries() {
        let tmp = tempfile::tempdir().unwrap();
        for arguments in [
            vec![],
            vec!["v0.1", "--force"],
            vec!["v0.1", "v0.2"],
            vec!["--dry-run"],
            vec!["v0.1", "--dry-run", "--dry-run"],
            vec!["v1.2$(id)"],
        ] {
            let error = run(
                tmp.path(),
                &mut Args(arguments.into_iter().map(Into::into).collect()),
            )
            .unwrap_err()
            .to_string();
            assert!(
                !error.contains("main"),
                "invalid arguments reached Git: {error}"
            );
        }
    }
}
