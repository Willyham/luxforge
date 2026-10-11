//! Explicit signing of a built macOS candidate. Never reads or exports the operator's secrets.
use crate::*;
use xtask_cli::release_tag::ReleaseTag;

#[derive(Clone, Debug)]
struct Step {
    program: &'static str,
    args: Vec<OsString>,
    submission: bool,
}

fn step(program: &'static str, args: impl IntoIterator<Item = OsString>) -> Step {
    Step {
        program,
        args: args.into_iter().collect(),
        submission: false,
    }
}

fn plan(out: &Path, identity: &str, profile: &str, keychain: Option<&Path>) -> Vec<Step> {
    let app = out.join("Luxforge.app");
    let helpers = release_build::executable_paths(out, release_build::TARGETS[0].0);
    let mut result = Vec::new();
    for path in helpers.iter().skip(1).chain(std::iter::once(&app)) {
        let mut args: Vec<OsString> = [
            "--force",
            "--sign",
            identity,
            "--options",
            "runtime",
            "--timestamp",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        if let Some(path) = keychain {
            args.extend(["--keychain".into(), path.as_os_str().to_owned()]);
        }
        args.push(path.as_os_str().to_owned());
        result.push(step("codesign", args));
    }
    for path in helpers.iter().skip(1) {
        result.push(step(
            "codesign",
            [
                "--verify".into(),
                "--strict".into(),
                path.as_os_str().to_owned(),
            ],
        ));
    }
    result.push(step(
        "codesign",
        [
            "--verify".into(),
            "--deep".into(),
            "--strict".into(),
            app.as_os_str().to_owned(),
        ],
    ));
    let upload = out.join("notarization-upload.zip");
    result.push(step(
        "ditto",
        [
            "-c".into(),
            "-k".into(),
            "--sequesterRsrc".into(),
            "--keepParent".into(),
            app.as_os_str().to_owned(),
            upload.as_os_str().to_owned(),
        ],
    ));
    let mut args: Vec<OsString> = ["notarytool", "submit"]
        .into_iter()
        .map(Into::into)
        .collect();
    args.push(upload.as_os_str().to_owned());
    args.extend(
        [
            "--keychain-profile",
            profile,
            "--wait",
            "--timeout",
            "30m",
            "--output-format",
            "json",
        ]
        .into_iter()
        .map(Into::into),
    );
    if let Some(path) = keychain {
        args.extend(["--keychain".into(), path.as_os_str().to_owned()]);
    }
    result.push(Step {
        program: "xcrun",
        args,
        submission: true,
    });
    for command in ["staple", "validate"] {
        result.push(step(
            "xcrun",
            ["stapler".into(), command.into(), app.as_os_str().to_owned()],
        ));
    }
    result.push(step(
        "codesign",
        [
            "--verify".into(),
            "--deep".into(),
            "--strict".into(),
            app.as_os_str().to_owned(),
        ],
    ));
    result.push(step(
        "spctl",
        [
            "--assess".into(),
            "--type".into(),
            "execute".into(),
            app.as_os_str().to_owned(),
        ],
    ));
    result
}

fn execute(steps: &[Step], runner: &mut impl FnMut(&Step) -> Result<Vec<u8>>) -> Result<Value> {
    let mut submission = None;
    for step in steps {
        let bytes = runner(step)?;
        if step.submission {
            let result: Value = serde_json::from_slice(&bytes)?;
            ensure(
                result["status"] == "Accepted",
                format!("Notarization was not Accepted: {}", result["status"]),
            )?;
            ensure(
                result["id"].is_string(),
                "Notarization response has no submission ID",
            )?;
            submission = Some(json!({"status":"notarized","submission_id":result["id"]}));
        }
    }
    submission.ok_or_else(|| "Notarization submission did not run".into())
}

pub fn run(root: &Path, args: &mut Args) -> Result {
    let out = absolute(root, &args.path("--output")?);
    let identity = args.value("--identity")?.ok_or("Required: --identity")?;
    let profile = args
        .value("--keychain-profile")?
        .ok_or("Required: --keychain-profile")?;
    let keychain = args.value("--keychain")?.map(PathBuf::from);
    args.done()?;
    ensure(cfg!(target_os = "macos"), "release-sign requires macOS")?;
    let identity = identity.to_str().ok_or("Identity encoding")?;
    let profile = profile.to_str().ok_or("Profile encoding")?;
    ensure(
        identity.starts_with("Developer ID Application:")
            && !identity.chars().any(char::is_control),
        "Select an explicit Developer ID Application identity",
    )?;
    ensure(
        !profile.is_empty() && !profile.chars().any(char::is_control),
        "Invalid Keychain profile",
    )?;
    let original = read_json(&out.join("target.json"))?;
    let tag = ReleaseTag::parse(original["tag"].as_str().ok_or("Missing candidate tag")?)?;
    let mut record = release_build::verify_record(&out, &tag, release_build::TARGETS[0].0)?;
    ensure(
        record["signing"]["status"] == "unsigned",
        "Signing requires a fresh unsigned build candidate",
    )?;
    record["signing"] = json!({"status":"pending"});
    write_json(&out.join("target.json"), &record)?;
    let outcome = (|| -> Result {
        let signing = execute(
            &plan(&out, identity, profile, keychain.as_deref()),
            &mut |step| {
                let result = Command::new(step.program)
                    .args(&step.args)
                    .current_dir(root)
                    .output()?;
                ensure(
                    result.status.success(),
                    format!(
                        "{} failed: {}",
                        step.program,
                        String::from_utf8_lossy(&result.stderr)
                    ),
                )?;
                Ok(result.stdout)
            },
        )?;
        let archive = out.join(release_build::archive_name(
            &tag,
            release_build::TARGETS[0].0,
        )?);
        fs::remove_file(&archive)?;
        release_build::archive_payload(&out, &tag, release_build::TARGETS[0].0)?;
        record["archive"] = release_build::archive_record(&archive)?;
        record["signing"] = signing;
        let mut hashes = serde_json::Map::new();
        for (name, path) in release_build::EXECUTABLES
            .iter()
            .zip(release_build::executable_paths(
                &out,
                release_build::TARGETS[0].0,
            ))
        {
            hashes.insert((*name).to_owned(), json!(hash(&path)?));
        }
        record["signed_executables"] = json!(hashes);
        write_json(&out.join("target.json"), &record)?;
        Ok(())
    })();
    let _ = fs::remove_file(out.join("notarization-upload.zip"));
    if let Err(error) = outcome {
        record["signing"] = json!({"status":"failed"});
        write_json(&out.join("target.json"), &record)?;
        return Err(error);
    }
    println!(
        "Developer ID signature, notarization and stapled ticket verified; native release qualification remains open."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_precede_app_and_only_accepted_submission_reaches_stapling() {
        let steps = plan(
            Path::new("candidate with spaces"),
            "Developer ID Application: Test",
            "luxforge-notarization",
            Some(Path::new("temporary keychain")),
        );
        assert!(
            steps[0]
                .args
                .last()
                .unwrap()
                .to_string_lossy()
                .ends_with("Helpers/luxforge-json")
        );
        assert!(
            steps[1]
                .args
                .last()
                .unwrap()
                .to_string_lossy()
                .ends_with("Helpers/luxforge-ctl")
        );
        assert!(
            steps[2]
                .args
                .last()
                .unwrap()
                .to_string_lossy()
                .ends_with("Luxforge.app")
        );
        assert!(
            steps
                .iter()
                .filter(|s| s.args.iter().any(|a| a == "--sign"))
                .all(|s| !s.args.iter().any(|a| a == "--deep"))
        );
        for status in ["Accepted", "Invalid", "In Progress"] {
            let mut stapled = false;
            let result = execute(&steps, &mut |step| {
                if step.args.iter().any(|a| a == "staple") {
                    stapled = true;
                }
                Ok(if step.submission {
                    serde_json::to_vec(&json!({"id":"submission","status":status})).unwrap()
                } else {
                    Vec::new()
                })
            });
            assert_eq!(result.is_ok(), status == "Accepted");
            assert_eq!(stapled, status == "Accepted");
        }
    }

    #[test]
    fn a_failed_signing_step_never_submits_or_staples() {
        let steps = plan(
            Path::new("candidate"),
            "Developer ID Application: Test",
            "luxforge-notarization",
            None,
        );
        for failure in 0..steps.len() {
            let mut calls = 0;
            let result = execute(&steps, &mut |step| {
                let current = calls;
                calls += 1;
                if current == failure {
                    return Err("simulated tool failure".into());
                }
                Ok(if step.submission {
                    br#"{"id":"submission","status":"Accepted"}"#.to_vec()
                } else {
                    Vec::new()
                })
            });
            assert!(result.is_err());
            assert_eq!(calls, failure + 1);
        }
    }
}
