//! Explicit signing of a built macOS candidate. Never reads or exports the operator's secrets.
use crate::*;
use xtask_cli::release_tag::ReleaseTag;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Program {
    Tool(&'static str),
    /// Recreates the update ZIP from the stapled app.
    FinalArchive,
    /// Creates the disk image from the stapled app.
    DiskImage,
}

/// What a notarization submission notarizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subject {
    App,
    DiskImage,
}

#[derive(Clone, Debug)]
struct Step {
    program: Program,
    args: Vec<OsString>,
    submission: Option<Subject>,
}

fn step(program: &'static str, args: impl IntoIterator<Item = OsString>) -> Step {
    Step {
        program: Program::Tool(program),
        args: args.into_iter().collect(),
        submission: None,
    }
}

fn os(values: &[&str]) -> Vec<OsString> {
    values.iter().map(Into::into).collect()
}

/// `codesign` with the Developer ID identity and a secure timestamp; code also takes the hardened
/// runtime.
fn sign(identity: &str, keychain: Option<&Path>, runtime: bool, path: &Path) -> Step {
    let mut args = os(&["--force", "--sign", identity]);
    if runtime {
        args.extend(os(&["--options", "runtime"]));
    }
    args.push("--timestamp".into());
    if let Some(path) = keychain {
        args.extend(["--keychain".into(), path.as_os_str().to_owned()]);
    }
    args.push(path.as_os_str().to_owned());
    step("codesign", args)
}

/// A `notarytool` submission that waits for its result.
fn submit(path: &Path, profile: &str, keychain: Option<&Path>, subject: Subject) -> Step {
    let mut args = os(&["notarytool", "submit"]);
    args.push(path.as_os_str().to_owned());
    args.extend(os(&[
        "--keychain-profile",
        profile,
        "--wait",
        "--timeout",
        "30m",
        "--output-format",
        "json",
    ]));
    if let Some(path) = keychain {
        args.extend(["--keychain".into(), path.as_os_str().to_owned()]);
    }
    Step {
        program: Program::Tool("xcrun"),
        args,
        submission: Some(subject),
    }
}

fn staple(path: &Path) -> [Step; 2] {
    ["staple", "validate"]
        .map(|command| step("xcrun", ["stapler".into(), command.into(), path.into()]))
}

/// The whole sequence: sign Sparkle's nested code innermost first (`Autoupdate`, `Updater.app`,
/// then the framework), the helpers and the app, each on its own and never with `--deep`;
/// notarize and staple the app, recreate its ZIP, then create, sign, notarize and staple the disk
/// image `image` from the stapled app.
fn plan(
    out: &Path,
    image: &Path,
    identity: &str,
    profile: &str,
    keychain: Option<&Path>,
) -> Vec<Step> {
    let app = out.join("Luxforge.app");
    let helpers = release_build::executable_paths(out, release_build::TARGETS[0].0);
    let mut result = Vec::new();
    for path in sparkle::signing_order(&app)
        .iter()
        .chain(helpers.iter().skip(1))
        .chain(std::iter::once(&app))
    {
        result.push(sign(identity, keychain, true, path));
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
    let verify_app = step(
        "codesign",
        [
            "--verify".into(),
            "--deep".into(),
            "--strict".into(),
            app.as_os_str().to_owned(),
        ],
    );
    result.push(verify_app.clone());
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
    result.push(submit(&upload, profile, keychain, Subject::App));
    result.extend(staple(&app));
    result.push(verify_app);
    result.push(step(
        "spctl",
        [
            "--assess".into(),
            "--type".into(),
            "execute".into(),
            app.as_os_str().to_owned(),
        ],
    ));
    for program in [Program::FinalArchive, Program::DiskImage] {
        result.push(Step {
            program,
            args: Vec::new(),
            submission: None,
        });
    }
    result.push(sign(identity, keychain, false, image));
    result.push(step(
        "codesign",
        [
            "--verify".into(),
            "--strict".into(),
            image.as_os_str().to_owned(),
        ],
    ));
    result.push(submit(image, profile, keychain, Subject::DiskImage));
    result.extend(staple(image));
    result.push(step(
        "spctl",
        [
            "--assess".into(),
            "--type".into(),
            "open".into(),
            "--context".into(),
            "context:primary-signature".into(),
            image.as_os_str().to_owned(),
        ],
    ));
    result
}

/// Runs `steps` in order, stopping at the first failure or a submission that is not Accepted, and
/// returns the signing record with both submissions.
fn execute(steps: &[Step], runner: &mut impl FnMut(&Step) -> Result<Vec<u8>>) -> Result<Value> {
    let mut app = None;
    let mut image = None;
    for step in steps {
        let bytes = runner(step)?;
        if let Some(subject) = step.submission {
            let result: Value = serde_json::from_slice(&bytes)?;
            let what = match subject {
                Subject::App => "app",
                Subject::DiskImage => "disk image",
            };
            ensure(
                result["status"] == "Accepted",
                format!(
                    "Notarization of the {what} was not Accepted: {}",
                    result["status"]
                ),
            )?;
            ensure(
                result["id"].is_string(),
                format!("The {what}'s notarization response has no submission ID"),
            )?;
            let record = json!({"status":"notarized","submission_id":result["id"]});
            match subject {
                Subject::App => app = Some(record),
                Subject::DiskImage => image = Some(record),
            }
        }
    }
    let mut signing = app.ok_or("App notarization submission did not run")?;
    signing["disk_image"] = image.ok_or("Disk image notarization submission did not run")?;
    Ok(signing)
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
    let archive = out.join(release_build::archive_name(
        &tag,
        release_build::TARGETS[0].0,
    )?);
    let image = out.join(
        release_build::disk_image_name(&tag, release_build::TARGETS[0].0)?
            .ok_or("macOS has a disk image")?,
    );
    record["signing"] = json!({"status":"pending"});
    write_json(&out.join("target.json"), &record)?;
    let outcome = (|| -> Result {
        let signing = execute(
            &plan(&out, &image, identity, profile, keychain.as_deref()),
            &mut |step| match step.program {
                Program::Tool(program) => {
                    let result = Command::new(program)
                        .args(&step.args)
                        .current_dir(root)
                        .output()?;
                    ensure(
                        result.status.success(),
                        format!(
                            "{program} failed: {}",
                            String::from_utf8_lossy(&result.stderr)
                        ),
                    )?;
                    Ok(result.stdout)
                }
                Program::FinalArchive => {
                    fs::remove_file(&archive)?;
                    release_build::archive_payload(&out, &tag, release_build::TARGETS[0].0)?;
                    Ok(Vec::new())
                }
                Program::DiskImage => {
                    release_build::create_disk_image(&out, &tag)?;
                    Ok(Vec::new())
                }
            },
        )?;
        record["archive"] = release_build::archive_record(&archive)?;
        record["disk_image"] = release_build::archive_record(&image)?;
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
        // The final hashes, measured from the signed and stapled app; signing rewrote every
        // executable, so they differ from the hashes recorded before it.
        record["executables"]["final"] = json!(hashes);
        xtask_cli::release_candidate::check_record(&record, &tag, release_build::TARGETS[0].0)?;
        write_json(&out.join("target.json"), &record)?;
        Ok(())
    })();
    let _ = fs::remove_file(out.join("notarization-upload.zip"));
    if let Err(error) = outcome {
        record["signing"] = json!({"status":"failed"});
        if let Some(executables) = record["executables"].as_object_mut() {
            executables.remove("final");
        }
        write_json(&out.join("target.json"), &record)?;
        return Err(error);
    }
    println!(
        "Developer ID signatures, notarization and stapled tickets verified for the app and its disk image; native release qualification remains open."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_plan(keychain: Option<&Path>) -> Vec<Step> {
        plan(
            Path::new("candidate with spaces"),
            Path::new("candidate with spaces/luxforge-0.1.0-macos-arm64.dmg"),
            "Developer ID Application: Test",
            "luxforge-notarization",
            keychain,
        )
    }

    fn names(step: &Step) -> bool {
        step.args
            .last()
            .is_some_and(|a| a.to_string_lossy().ends_with(".dmg"))
    }

    fn accepting(status: &str, image_status: &str) -> impl Fn(&Step) -> Vec<u8> {
        let (status, image_status) = (status.to_owned(), image_status.to_owned());
        move |step| match step.submission {
            Some(Subject::App) => {
                serde_json::to_vec(&json!({"id":"app-submission","status":status})).unwrap()
            }
            Some(Subject::DiskImage) => {
                serde_json::to_vec(&json!({"id":"image-submission","status":image_status})).unwrap()
            }
            None => Vec::new(),
        }
    }

    #[test]
    fn helpers_precede_app_and_only_accepted_submission_reaches_stapling() {
        let steps = test_plan(Some(Path::new("temporary keychain")));
        let signed: Vec<String> = steps
            .iter()
            .take_while(|s| s.args.iter().any(|a| a == "--sign"))
            .map(|s| s.args.last().unwrap().to_string_lossy().into_owned())
            .collect();
        let framework = "Luxforge.app/Contents/Frameworks/Sparkle.framework";
        let expected = [
            format!("{framework}/Versions/B/Autoupdate"),
            format!("{framework}/Versions/B/Updater.app"),
            framework.to_owned(),
            "Contents/Helpers/luxforge-json".to_owned(),
            "Contents/Helpers/luxforge-ctl".to_owned(),
            "Luxforge.app".to_owned(),
        ];
        assert_eq!(signed.len(), expected.len(), "{signed:?}");
        for (path, suffix) in signed.iter().zip(&expected) {
            assert!(path.ends_with(suffix.as_str()), "{path} is not {suffix}");
        }
        // Sparkle's code is signed as the app's: hardened runtime, secure timestamp, the keychain.
        for step in &steps[..3] {
            assert_eq!(
                step.args[..8],
                os(&[
                    "--force",
                    "--sign",
                    "Developer ID Application: Test",
                    "--options",
                    "runtime",
                    "--timestamp",
                    "--keychain",
                    "temporary keychain",
                ])
            );
        }
        assert!(
            steps
                .iter()
                .filter(|s| s.args.iter().any(|a| a == "--sign"))
                .all(|s| !s.args.iter().any(|a| a == "--deep"))
        );
        for (status, image_status) in [
            ("Accepted", "Accepted"),
            ("Invalid", "Accepted"),
            ("In Progress", "Accepted"),
            ("Accepted", "Invalid"),
            ("Accepted", "In Progress"),
        ] {
            let respond = accepting(status, image_status);
            let (mut stapled_app, mut imaged, mut stapled_image) = (false, false, false);
            let result = execute(&steps, &mut |step| {
                if step.args.iter().any(|a| a == "staple") {
                    if names(step) {
                        stapled_image = true;
                    } else {
                        stapled_app = true;
                    }
                }
                imaged |= step.program == Program::DiskImage;
                Ok(respond(step))
            });
            assert_eq!(
                result.is_ok(),
                status == "Accepted" && image_status == "Accepted"
            );
            assert_eq!(stapled_app, status == "Accepted");
            assert_eq!(imaged, status == "Accepted");
            assert_eq!(
                stapled_image,
                status == "Accepted" && image_status == "Accepted"
            );
        }
    }

    #[test]
    fn the_disk_image_is_made_from_the_stapled_app_then_signed_notarized_and_stapled() {
        let steps = test_plan(Some(Path::new("temporary keychain")));
        let position = |what: &str, found: &dyn Fn(&Step) -> bool| {
            let at: Vec<usize> = (0..steps.len()).filter(|&i| found(&steps[i])).collect();
            assert_eq!(at.len(), 1, "{what}: {at:?}");
            at[0]
        };
        let has = |step: &Step, word: &str| step.args.iter().any(|a| a == word);
        let app_validated = position("app validate", &|s| has(s, "validate") && !names(s));
        let app_assessed = position("app assessment", &|s| has(s, "execute"));
        let archive = position("final ZIP", &|s| s.program == Program::FinalArchive);
        let created = position("disk image", &|s| s.program == Program::DiskImage);
        let signed = position("image signature", &|s| has(s, "--sign") && names(s));
        let verified = position("image verify", &|s| has(s, "--verify") && names(s));
        let submitted = position("image submission", &|s| {
            s.submission == Some(Subject::DiskImage)
        });
        let stapled = position("image staple", &|s| has(s, "staple") && names(s));
        let validated = position("image validate", &|s| has(s, "validate") && names(s));
        let assessed = position("image assessment", &|s| has(s, "open") && names(s));
        let order = [
            app_validated,
            app_assessed,
            archive,
            created,
            signed,
            verified,
            submitted,
            stapled,
            validated,
            assessed,
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
        assert_eq!(assessed, steps.len() - 1);
        assert_eq!(
            steps[signed].args,
            os(&[
                "--force",
                "--sign",
                "Developer ID Application: Test",
                "--timestamp",
                "--keychain",
                "temporary keychain",
                "candidate with spaces/luxforge-0.1.0-macos-arm64.dmg",
            ])
        );
        assert!(steps[submitted].args.windows(2).any(|pair| pair
            == os(&["--keychain-profile", "luxforge-notarization"]).as_slice()));
        assert_eq!(
            steps[assessed].args,
            os(&[
                "--assess",
                "--type",
                "open",
                "--context",
                "context:primary-signature",
                "candidate with spaces/luxforge-0.1.0-macos-arm64.dmg",
            ])
        );
        let respond = accepting("Accepted", "Accepted");
        let signing = execute(&steps, &mut |step| Ok(respond(step))).unwrap();
        assert_eq!(
            signing,
            json!({"status":"notarized","submission_id":"app-submission","disk_image":{"status":"notarized","submission_id":"image-submission"}})
        );
    }

    #[test]
    fn a_failed_signing_step_never_submits_or_staples() {
        let steps = test_plan(None);
        let respond = accepting("Accepted", "Accepted");
        for failure in 0..steps.len() {
            let mut calls = 0;
            let result = execute(&steps, &mut |step| {
                let current = calls;
                calls += 1;
                if current == failure {
                    return Err("simulated tool failure".into());
                }
                Ok(respond(step))
            });
            assert!(result.is_err());
            assert_eq!(calls, failure + 1);
        }
    }
}
