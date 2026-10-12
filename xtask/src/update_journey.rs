//! `cargo xtask update-journey --output NEW`: the macOS updater end to end
//! (`docs/design/auto-update.md#macos-sparkle`). A background-only bundle of build 1 updates itself
//! to build 2 through Sparkle and the desktop's user driver, from a loopback feed.
//!
//! - Both bundles hold the same editor, built with a release version into
//!   `target/update-journey/`, and the pinned Sparkle; their `Info.plist`s are the release's
//!   ([`release_build::info_plist`]) with the journey's identifier `org.luxforge.update-journey`, a
//!   loopback `SUFeedURL`, a throwaway public key, `LSBackgroundOnly`, `LuxforgeUpdateJourney`,
//!   which gives these automated launches the Sparkle port and a hidden window, and
//!   `LuxforgeUpdateJourneyRoot`, the data root inside the run that every launch of the bundle
//!   uses, so neither build ever opens the person's catalog. (`LSEnvironment` cannot do this:
//!   LaunchServices does not pass `HOME` from it to the relaunch.)
//! - Both are signed with a temporary self-signed code-signing identity in a temporary keychain
//!   under the output, passed to `codesign --keychain`. The login keychain, the default keychain
//!   and the search list are never used; a search list `security create-keychain` changed is
//!   restored at once, and the keychain is deleted at the end. Without a team identifier library
//!   validation cannot hold, so the journey signs without the hardened runtime. `codesign` refuses
//!   an untrusted self-signed identity ("no identity found"), and trusting it would change the
//!   person's trust settings, so the journey then signs ad hoc ([`Signer`]) and records the
//!   refusal: Sparkle accepts a change of signature while the EdDSA key stays the same.
//! - Build 2 is zipped as the release workflow zips the app, signed with a fresh Ed25519 key
//!   (`luxforge-update`'s `SigningKey`), listed in an appcast as the feed generator writes one, and
//!   served by a loopback HTTP server over `http://127.0.0.1`.
//! - Build 1 runs as an evidence run with the release's own update offer: Check now, the offer,
//!   Install. The port downloads through Sparkle, closes the window the ordinary way and tells
//!   Sparkle to install once the catalog has closed; Sparkle's installer replaces the bundle and
//!   relaunches it, background-only, under the run's data root. The relaunched build is asked
//!   through `luxforge-ctl --catalog` for `update.status`, which must report build 2, then
//!   stopped.
//!
//! Evidence, all under the output: `summary.json` (each stage's result and where the journey
//! stopped, if it did), the HTTP server's request log, build 1's evidence directory, the relaunched
//! build's status, the frontmost application sampled throughout, and Sparkle's unified log. The
//! journey cleans up Sparkle's defaults and caches for the journey's identifier.
use crate::*;
use luxforge_testbase::{TestServer, respond};
use luxforge_update::{Appcast, AppcastItem, Enclosure, SigningKey};
use std::{
    process::{Child, Stdio},
    time::{Duration, Instant},
};

const IDENTIFIER: &str = "org.luxforge.update-journey";
/// The version both builds carry: a stable version, so the default channel follows it.
const VERSION: &str = "0.0.1";
const BUNDLE: &str = "Luxforge Journey.app";
const ARCHIVE: &str = "luxforge-journey.zip";
const IDENTITY: &str = "Luxforge Update Journey";
/// How long build 1 may take from launch to exit, and the relaunch to answer.
const RUN_LIMIT: Duration = Duration::from_secs(5 * 60);
const RELAUNCH_LIMIT: Duration = Duration::from_secs(3 * 60);

pub fn run(root: &Path, args: &mut Args) -> Result {
    let out = absolute(root, &args.path("--output")?);
    args.done()?;
    ensure(
        cfg!(target_os = "macos"),
        "update-journey updates a macOS bundle through Sparkle",
    )?;
    ensure(!out.exists(), "The journey output must be new")?;
    fs::create_dir_all(&out)?;
    let mut journey = Journey {
        root: root.to_owned(),
        out: out.clone(),
        stages: Vec::new(),
        keychain: None,
        started: time_now(),
        frontmost: Vec::new(),
    };
    let outcome = journey.run();
    let cleanup = journey.cleanup();
    let stopped = outcome.as_ref().err().map(ToString::to_string);
    let summary = json!({
        "schema": "luxforge-update-journey-1",
        "identifier": IDENTIFIER,
        "passed": stopped.is_none() && cleanup.is_ok(),
        "stopped": stopped,
        "stages": journey.stages,
        "frontmost": journey.frontmost,
        "cleanup": match &cleanup {
            Ok(done) => json!({"ok": true, "done": done}),
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        },
    });
    write_json(&out.join("summary.json"), &summary)?;
    println!("Update journey: {}", out.join("summary.json").display());
    outcome?;
    cleanup?;
    Ok(())
}

struct Journey {
    root: PathBuf,
    out: PathBuf,
    stages: Vec<Value>,
    /// The temporary keychain, while it exists.
    keychain: Option<PathBuf>,
    started: String,
    /// The frontmost application's pid, sampled before, during and after.
    frontmost: Vec<Value>,
}

/// The temporary signing identity.
struct Identity {
    keychain: PathBuf,
    sha1: String,
}

/// How the journey's bundles are signed: with the temporary identity, or ad hoc once `codesign`
/// has refused that identity, as it refuses an untrusted self-signed certificate. Trusting the
/// certificate would change the person's trust settings, which the journey never does. Sparkle
/// accepts an ad hoc signature when the old and new builds share their EdDSA key.
enum Signer {
    Identity(Identity),
    AdHoc { refused: String },
}

impl Signer {
    fn record(&self) -> Value {
        match self {
            Self::Identity(identity) => {
                json!({"kind": "identity", "certificate_sha1": identity.sha1, "hardened_runtime": false})
            }
            Self::AdHoc { refused } => {
                json!({"kind": "ad-hoc", "identity_refused": refused, "hardened_runtime": false})
            }
        }
    }

    /// Sign `path`, falling back to ad hoc for this and every later path when `codesign` finds no
    /// usable identity.
    fn sign(&mut self, path: &Path) -> Result {
        if let Self::Identity(identity) = self {
            match codesign(
                &["--sign", &identity.sha1, "--keychain"],
                Some(&identity.keychain),
                path,
            ) {
                Ok(()) => return Ok(()),
                Err(error) if error.to_string().contains("no identity found") => {
                    *self = Self::AdHoc {
                        refused: error.to_string(),
                    };
                }
                Err(error) => return Err(error),
            }
        }
        codesign(&["--sign", "-"], None, path)
    }
}

impl Journey {
    fn stage(&mut self, name: &str, detail: Value) {
        println!("update-journey: {name}");
        self.stages.push(json!({"stage": name, "detail": detail}));
    }

    fn sample_frontmost(&mut self, moment: &str, ours: &[u32]) -> Result {
        let pid = launch::frontmost_pid()?;
        self.frontmost.push(json!({"at": moment, "pid": pid}));
        ensure(
            !ours.contains(&pid),
            format!("The journey's app (pid {pid}) became the frontmost application {moment}"),
        )
    }

    fn run(&mut self) -> Result {
        self.sample_frontmost("before the journey", &[])?;
        let (editor, ctl) = self.build()?;
        let sparkle = sparkle::fetch(&self.root)?;
        self.stage(
            "sparkle",
            json!({"version": sparkle::VERSION, "sha256": sparkle::SHA256}),
        );
        let mut signer = Signer::Identity(self.identity()?);
        let feed = self.out.join("feed");
        fs::create_dir(&feed)?;
        let server = serve(&feed)?;
        let port = server.address().port();
        let feed_url = format!("http://127.0.0.1:{port}/appcast.xml");
        let key = SigningKey::from_seed(&random_seed()?)?;
        let public_key = key.public_key().to_base64();
        let data = self.out.join("data");
        let config = data.join("config");
        let mut bundle = |build: &str, at: &Path| -> Result<PathBuf> {
            let app = at.join(BUNDLE);
            let contents = app.join("Contents");
            fs::create_dir_all(contents.join("MacOS"))?;
            fs::copy(&editor, contents.join("MacOS/luxforge"))?;
            let extra = format!(
                "<key>LSBackgroundOnly</key><true/><key>LuxforgeUpdateJourney</key><true/><key>LuxforgeUpdateJourneyRoot</key><string>{}</string>",
                xml_text(data.to_str().ok_or("Journey path encoding")?)
            );
            fs::write(
                contents.join("Info.plist"),
                release_build::info_plist(&release_build::BundleInfo {
                    identifier: IDENTIFIER,
                    version: VERSION,
                    build,
                    feed_url: &feed_url,
                    public_key: Some(&public_key),
                    extra: &extra,
                }),
            )?;
            sparkle::embed(&sparkle, &app)?;
            for path in sparkle::signing_order(&app)
                .iter()
                .chain(std::iter::once(&app))
            {
                signer.sign(path)?;
            }
            let verified = Command::new("codesign")
                .args(["--verify", "--deep", "--strict"])
                .arg(&app)
                .output()?;
            ensure(
                verified.status.success(),
                format!(
                    "The journey bundle does not verify: {}",
                    String::from_utf8_lossy(&verified.stderr)
                ),
            )?;
            Ok(app)
        };
        let installed = bundle("1", &self.out.join("installed"))?;
        let update = bundle("2", &self.out.join("update"))?;
        let archive = feed.join(ARCHIVE);
        let status = Command::new("ditto")
            .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
            .arg(&update)
            .arg(&archive)
            .status()?;
        ensure(status.success(), "Zipping build 2 failed")?;
        let (signature, length) = key.sign_file(&archive)?;
        let appcast = Appcast {
            title: "Luxforge update journey".into(),
            link: None,
            description: None,
            items: vec![AppcastItem {
                title: Some(format!("Luxforge {VERSION} (2)")),
                link: Some(format!("http://127.0.0.1:{port}/notes")),
                pub_date: luxforge_update::pub_date("2026-10-12T09:00:00Z"),
                build: 2,
                version: VERSION.into(),
                minimum_system_version: Some(release_build::MACOS_MINIMUM_SYSTEM_VERSION.into()),
                channel: None,
                catalog_format: Some(u32::try_from(luxforge_core::CATALOG_FORMAT)?),
                enclosure: Enclosure {
                    url: format!("http://127.0.0.1:{port}/{ARCHIVE}"),
                    length,
                    mime_type: Some(luxforge_update::ENCLOSURE_TYPE.into()),
                    ed_signature: signature,
                },
            }],
        };
        fs::write(feed.join("appcast.xml"), appcast.to_xml())?;
        self.stage(
            "bundles",
            json!({"installed": installed, "update": update, "feed": feed_url, "archive_bytes": length, "signing": signer.record()}),
        );

        // Build 1: Check now, the offer, Install, then idle while the port quits the app.
        let script = self.out.join("script.json");
        let mut steps = vec![
            json!({"update": "check"}),
            json!({"update": "offer"}),
            json!({"update": {"notice": "install"}}),
        ];
        steps.extend((0..12).map(|_| json!({"wait": {"ms": 10_000}})));
        write_json(&script, &json!(steps))?;
        let evidence = self.out.join("evidence-1");
        let launched = Instant::now();
        let mut child = Command::new(installed.join("Contents/MacOS/luxforge"))
            .args(launch::editor_args(&[
                "--evidence-dir".into(),
                evidence.clone().into(),
                "--evidence-script".into(),
                script.into(),
            ]))
            .current_dir(&self.out)
            .stdin(Stdio::null())
            .stdout(fs::File::create(self.out.join("build-1.stdout.log"))?)
            .stderr(fs::File::create(self.out.join("build-1.stderr.log"))?)
            .spawn()?;
        let first = child.id();
        let exit = self.wait_for_exit(&mut child, first)?;
        self.stage(
            "build-1",
            json!({"pid": first, "exit": exit, "seconds": launched.elapsed().as_secs_f64(), "evidence": evidence}),
        );
        fs::write(self.out.join("requests.log"), request_log(&server))?;

        // The installer replaces the bundle once build 1 has exited, then relaunches it.
        let deadline = Instant::now() + RELAUNCH_LIMIT;
        let relaunched = loop {
            if let Some(pid) = live_pid(&config) {
                break pid;
            }
            self.sample_frontmost("while relaunching", &[first])?;
            ensure(
                Instant::now() < deadline,
                format!(
                    "No relaunched build answered within {} s; the installed bundle states build {}",
                    RELAUNCH_LIMIT.as_secs(),
                    bundle_build(&installed).unwrap_or_else(|| "?".into())
                ),
            )?;
            std::thread::sleep(Duration::from_millis(500));
        };
        let status = loop {
            let answer = Command::new(&ctl)
                .arg("--catalog")
                .arg(config.join("catalog.sqlite"))
                .args(["call", "update.status"])
                .stdin(Stdio::null())
                .output()?;
            if answer.status.success() {
                break serde_json::from_slice::<Value>(&answer.stdout)?;
            }
            ensure(
                Instant::now() < deadline,
                format!(
                    "The relaunched build's live session never answered: {}",
                    String::from_utf8_lossy(&answer.stderr)
                ),
            )?;
            std::thread::sleep(Duration::from_millis(500));
        };
        write_json(&self.out.join("relaunched-status.json"), &status)?;
        self.sample_frontmost("after the relaunch", &[first, relaunched])?;
        let _ = Command::new("kill")
            .args(["-TERM", &relaunched.to_string()])
            .status();
        let reported = status["current"]["build"].as_u64();
        self.stage(
            "relaunched",
            json!({"pid": relaunched, "current": status["current"], "mechanism": status["mechanism"], "bundle_build": bundle_build(&installed)}),
        );
        fs::write(self.out.join("requests.log"), request_log(&server))?;
        ensure(
            reported == Some(2),
            format!("The relaunched build reports build {reported:?}, not 2"),
        )?;
        ensure(
            relaunched != first,
            "The relaunched build is build 1's process",
        )?;
        Ok(())
    }

    /// The editor and `luxforge-ctl`, built with the journey's release version into their own
    /// target directory so no other build's binaries change.
    fn build(&mut self) -> Result<(PathBuf, PathBuf)> {
        let target = self.root.join("target/update-journey");
        let status = cargo_command()
            .current_dir(&self.root)
            .args([
                "build",
                "--locked",
                "--package",
                "luxforge-app",
                "--package",
                "luxforge-cli",
            ])
            .env("CARGO_TARGET_DIR", &target)
            .env("LUXFORGE_RELEASE_VERSION", VERSION)
            .status()?;
        ensure(status.success(), "Building the journey's editor failed")?;
        let (editor, ctl) = (
            target.join("debug/luxforge"),
            target.join("debug/luxforge-ctl"),
        );
        let version = output(&self.root, &editor, &["--version"])?;
        ensure(
            version.trim() == format!("luxforge {VERSION}"),
            format!("The journey's editor reports {version:?}"),
        )?;
        self.stage("build", json!({"editor": editor, "version": VERSION}));
        Ok((editor, ctl))
    }

    /// A self-signed code-signing identity in a new keychain under the output. The search list is
    /// read before and restored if creating the keychain changed it.
    fn identity(&mut self) -> Result<Identity> {
        let dir = self.out.join("identity");
        fs::create_dir(&dir)?;
        let password = hex(&random_seed()?);
        let pass = format!("pass:{password}");
        let (key, cert, p12) = (
            dir.join("key.pem"),
            dir.join("cert.pem"),
            dir.join("id.p12"),
        );
        run_tool(
            Command::new("/usr/bin/openssl")
                .args([
                    "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                ])
                .args(["-subj", &format!("/CN={IDENTITY}")])
                .args(["-addext", "basicConstraints=critical,CA:false"])
                .args(["-addext", "keyUsage=critical,digitalSignature"])
                .args(["-addext", "extendedKeyUsage=critical,codeSigning"])
                .arg("-keyout")
                .arg(&key)
                .arg("-out")
                .arg(&cert),
            "Creating the journey's certificate",
        )?;
        run_tool(
            Command::new("/usr/bin/openssl")
                .args(["pkcs12", "-export", "-passout", &pass])
                .arg("-inkey")
                .arg(&key)
                .arg("-in")
                .arg(&cert)
                .arg("-out")
                .arg(&p12),
            "Packing the journey's identity",
        )?;
        let fingerprint = run_tool(
            Command::new("/usr/bin/openssl")
                .args(["x509", "-noout", "-fingerprint", "-sha1", "-in"])
                .arg(&cert),
            "Reading the certificate's fingerprint",
        )?;
        let sha1: String = fingerprint
            .split_once('=')
            .ok_or("No fingerprint")?
            .1
            .chars()
            .filter(char::is_ascii_hexdigit)
            .collect();
        let keychain = dir.join("journey.keychain-db");
        let search = search_list()?;
        let created = Command::new("/usr/bin/security")
            .args(["create-keychain", "-p", &password])
            .arg(&keychain)
            .output()?;
        self.keychain = Some(keychain.clone());
        ensure(
            created.status.success(),
            format!(
                "Creating the journey's keychain failed: {}",
                String::from_utf8_lossy(&created.stderr)
            ),
        )?;
        let restored = search_list()? != search;
        if restored {
            set_search_list(&search)?;
        }
        ensure(
            search_list()? == search,
            "The keychain search list could not be restored",
        )?;
        for (args, what) in [
            (vec!["unlock-keychain", "-p", &password], "Unlocking"),
            (vec!["set-keychain-settings"], "Configuring"),
        ] {
            run_tool(
                Command::new("/usr/bin/security").args(args).arg(&keychain),
                &format!("{what} the journey's keychain"),
            )?;
        }
        run_tool(
            Command::new("/usr/bin/security")
                .arg("import")
                .arg(&p12)
                .arg("-k")
                .arg(&keychain)
                .args(["-P", &password, "-T", "/usr/bin/codesign"]),
            "Importing the journey's identity",
        )?;
        run_tool(
            Command::new("/usr/bin/security")
                .args([
                    "set-key-partition-list",
                    "-S",
                    "apple-tool:,apple:,codesign:",
                ])
                .args(["-s", "-k", &password])
                .arg(&keychain),
            "Allowing codesign the journey's key",
        )?;
        for file in [&key, &p12] {
            fs::remove_file(file)?;
        }
        // What the keychain offers for code signing, valid or not, and why not.
        let offered = run_tool(
            Command::new("/usr/bin/security")
                .args(["find-identity", "-p", "codesigning"])
                .arg(&keychain),
            "Listing the journey's identities",
        )?;
        self.stage(
            "identity",
            json!({"keychain": keychain, "certificate_sha1": sha1, "search_list_restored": restored, "find_identity": offered}),
        );
        Ok(Identity { keychain, sha1 })
    }

    fn wait_for_exit(&mut self, child: &mut Child, pid: u32) -> Result<Option<i32>> {
        let deadline = Instant::now() + RUN_LIMIT;
        let mut sampled = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status.code());
            }
            if sampled.elapsed() >= Duration::from_secs(2) {
                sampled = Instant::now();
                self.sample_frontmost("while build 1 runs", &[pid])?;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "Build 1 did not quit within {} s: the update was not installed",
                    RUN_LIMIT.as_secs()
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Delete the keychain, stop any journey process still running, save Sparkle's log, and
    /// remove Sparkle's defaults and caches for the journey's identifier.
    fn cleanup(&mut self) -> Result<Vec<String>> {
        let mut done = Vec::new();
        for pid in journey_pids(&self.out) {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
            done.push(format!("stopped pid {pid}"));
        }
        let log = Command::new("/usr/bin/log")
            .args(["show", "--style", "json", "--start", &self.started])
            .args([
                "--predicate",
                "subsystem == \"org.sparkle-project.Sparkle\" OR process == \"Autoupdate\" OR process == \"Updater\"",
            ])
            .output();
        if let Ok(log) = log {
            fs::write(self.out.join("sparkle-log.json"), &log.stdout)?;
            done.push("saved Sparkle's unified log".into());
        }
        if let Some(keychain) = self.keychain.take() {
            let search = search_list()?;
            run_tool(
                Command::new("/usr/bin/security")
                    .arg("delete-keychain")
                    .arg(&keychain),
                "Deleting the journey's keychain",
            )?;
            if search_list()? != search {
                set_search_list(&search)?;
            }
            done.push("deleted the temporary keychain".into());
        }
        let _ = Command::new("/usr/bin/defaults")
            .args(["delete", IDENTIFIER])
            .output();
        done.push(format!("deleted the {IDENTIFIER} defaults"));
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            for place in ["Library/Caches", "Library/HTTPStorages"] {
                let path = home.join(place).join(IDENTIFIER);
                if path.exists() {
                    fs::remove_dir_all(&path)?;
                    done.push(format!("removed {}", path.display()));
                }
            }
        }
        Ok(done)
    }
}

/// A loopback HTTP server ([`TestServer`]) for `dir`'s files, on a port the system picks.
/// Anything but `GET /<file>` of an existing file is answered 404. It stops when dropped.
fn serve(dir: &Path) -> Result<TestServer> {
    let dir = dir.to_owned();
    Ok(TestServer::http(move |request, out| {
        let name = request.path.trim_start_matches('/');
        let file = (request.method == "GET" && !name.contains('/') && !name.starts_with('.'))
            .then(|| dir.join(name))
            .filter(|path| path.is_file());
        match file.map(fs::read) {
            Some(Ok(body)) => {
                let kind = if name.ends_with(".xml") {
                    "application/rss+xml"
                } else {
                    "application/octet-stream"
                };
                respond(out, "200 OK", &format!("Content-Type: {kind}\r\n"), &body);
            }
            _ => respond(out, "404 Not Found", "", b""),
        }
    })?)
}

/// The requests `server` read, one line each: its connection's index, the method and the path.
fn request_log(server: &TestServer) -> String {
    server
        .requests()
        .iter()
        .map(|request| format!("{} {} {}\n", request.index, request.method, request.path))
        .collect()
}

/// The pid of the editor serving a live session from the configuration directory `config`, from
/// the registry entry `<pid>-<id>.json` it writes there while it runs.
fn live_pid(config: &Path) -> Option<u32> {
    let registry = config.join("live-sessions");
    fs::read_dir(registry).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().into_string().ok()?;
        let pid: u32 = name
            .strip_suffix(".json")?
            .split('-')
            .next()?
            .parse()
            .ok()?;
        (Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .ok()?
            .success())
        .then_some(pid)
    })
}

/// Every running process whose executable lies under the journey's output.
fn journey_pids(out: &Path) -> Vec<u32> {
    let Ok(list) = Command::new("ps").args(["-axo", "pid=,comm="]).output() else {
        return Vec::new();
    };
    let prefix = out.to_string_lossy().into_owned();
    String::from_utf8_lossy(&list.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(' ')?;
            command
                .trim()
                .starts_with(&prefix)
                .then(|| pid.parse().ok())?
        })
        .collect()
}

/// `CFBundleVersion` of `app` as it stands.
fn bundle_build(app: &Path) -> Option<String> {
    let out = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleVersion", "raw", "-o", "-"])
        .arg(app.join("Contents/Info.plist"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// `codesign --force --timestamp=none` with `args`, then `keychain` when given, then `path`.
fn codesign(args: &[&str], keychain: Option<&Path>, path: &Path) -> Result {
    run_tool(
        Command::new("/usr/bin/codesign")
            .args(["--force", "--timestamp=none"])
            .args(args)
            .args(keychain)
            .arg(path),
        &format!("Signing {}", path.display()),
    )
    .map(drop)
}

/// The user's keychain search list, as `security list-keychains` prints it.
fn search_list() -> Result<Vec<String>> {
    let out = run_tool(
        Command::new("/usr/bin/security").args(["list-keychains", "-d", "user"]),
        "Reading the keychain search list",
    )?;
    Ok(out
        .lines()
        .map(|line| line.trim().trim_matches('"').to_owned())
        .filter(|line| !line.is_empty())
        .collect())
}

fn set_search_list(list: &[String]) -> Result {
    run_tool(
        Command::new("/usr/bin/security")
            .args(["list-keychains", "-d", "user", "-s"])
            .args(list),
        "Restoring the keychain search list",
    )
    .map(drop)
}

/// Runs `command`, answering its standard output, or fails naming `what` with its standard error.
fn run_tool(command: &mut Command, what: &str) -> Result<String> {
    let out = command.stdin(Stdio::null()).output()?;
    ensure(
        out.status.success(),
        format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    )?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn random_seed() -> Result<[u8; 32]> {
    let mut seed = [0; 32];
    std::io::Read::read_exact(&mut fs::File::open("/dev/urandom")?, &mut seed)?;
    Ok(seed)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn xml_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Now, as `log show --start` takes it.
fn time_now() -> String {
    String::from_utf8_lossy(
        &Command::new("date")
            .arg("+%Y-%m-%d %H:%M:%S")
            .output()
            .map(|out| out.stdout)
            .unwrap_or_default(),
    )
    .trim()
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_journey_server_serves_its_files_only() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("appcast.xml"), b"<rss/>").unwrap();
        let server = serve(dir.path()).unwrap();
        let get = |path: &str| {
            let mut stream = std::net::TcpStream::connect(server.address()).unwrap();
            std::io::Write::write_all(
                &mut stream,
                format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes(),
            )
            .unwrap();
            let mut text = String::new();
            std::io::Read::read_to_string(&mut stream, &mut text).unwrap();
            text
        };
        let found = get("/appcast.xml");
        assert!(found.starts_with("HTTP/1.1 200 OK"), "{found}");
        assert!(found.ends_with("<rss/>"));
        for missing in ["/other.xml", "/../Cargo.toml", "/sub/appcast.xml"] {
            assert!(get(missing).starts_with("HTTP/1.1 404"), "{missing}");
        }
        assert_eq!(server.requests().len(), 4);
        assert!(request_log(&server).starts_with("0 GET /appcast.xml\n"));
    }
}
