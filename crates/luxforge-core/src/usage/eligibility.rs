//! Which launches may collect: launch mode, build identity (version plus embedded clean source
//! revision) and the production activation switch
//! ([design](../../../../docs/design/usage-statistics.md#build-identity-and-activation)).
//!
//! Visible developer builds send to the fixed test tenant; ordinary revisioned release builds
//! need production activation. Explicit evidence receivers remain opt-in test eligibility.
//! Eligibility never grants consent: every eligible launch still needs the person's choice.
use super::{DESTINATION, TEST_DESTINATION, TEST_RELEASE};
use crate::Error;
use serde::Serialize;
use std::net::IpAddr;

/// Production delivery's checked-in activation switch. Off until the real endpoint has passed the
/// deployment checklist; while it is off every production launch is ineligible and reports
/// [`Reason::InactiveDestination`]. Turning it on is a separate, authorized change.
pub const PRODUCTION_ACTIVE: bool = false;

/// The compile-time variable `cargo xtask package` sets to the clean tree's full source revision.
/// Any other build (plain `cargo`, `cargo xtask develop`, a dirty tree) leaves it unset.
pub const SOURCE_REVISION_ENV: &str = "LUXFORGE_SOURCE_REVISION";

/// How many hexadecimal characters of the revision the reported release carries.
const RELEASE_REVISION_CHARS: usize = 12;

/// Why statistics are not being collected: the fixed reasons `usage.read` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// The person has not opted in, opted out, or this process stopped after a revocation.
    Choice,
    /// A hidden, background, evidence, test or headless launch.
    LaunchMode,
    /// The executable carries no clean source revision.
    Build,
    /// The production activation switch is off.
    InactiveDestination,
    /// The preferences file cannot be read, or holds a record this build cannot use.
    UnreadableConfiguration,
    /// The stored consent was given to another disclosure version.
    ChangedDisclosure,
}

/// What kind of launch this process is, as its host knows it. Only [`Self::Interactive`] can be
/// eligible for production reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchMode {
    /// An ordinary desktop launch a person sees and uses.
    Interactive,
    /// A developer launch: `cargo xtask develop`, a developer flag or proof modules.
    Developer,
    /// A launch whose window is hidden.
    Hidden,
    /// A background-only launch.
    Background,
    /// An evidence run.
    Evidence,
    /// A test host.
    Test,
    /// A host without a desktop: `luxforge-json`, the CLI, a service.
    Headless,
}

/// The build's identity: the application version and the clean source revision `cargo xtask package`
/// or `cargo xtask release-build` embedded, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildIdentity {
    version: String,
    revision: Option<String>,
}

impl BuildIdentity {
    /// This executable's identity: [`crate::APPLICATION_VERSION`], as every executable's
    /// `--version` reports it, and [`SOURCE_REVISION_ENV`] as it was when the core was compiled.
    pub fn current() -> Self {
        Self::new(
            crate::APPLICATION_VERSION,
            option_env!("LUXFORGE_SOURCE_REVISION"),
        )
    }

    /// An identity from a SemVer version and an embedded revision. Only the version's numeric
    /// `MAJOR.MINOR.PATCH` is kept: a pre-release or build suffix is dropped, since the revision
    /// already tells builds apart. A revision that is not a full lowercase hexadecimal Git object
    /// name (40 or 64 characters) counts as none.
    pub fn new(version: &str, revision: Option<&str>) -> Self {
        let version = &version[..version.find(['-', '+']).unwrap_or(version.len())];
        let revision = revision.filter(|revision| {
            matches!(revision.len(), 40 | 64)
                && revision
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        Self {
            version: version.to_owned(),
            revision: revision.map(str::to_owned),
        }
    }

    /// The embedded clean source revision, if any.
    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }

    /// The reported release, `<version>+<first 12 hex characters of the revision>`, such as
    /// `0.0.0+9d4138ca1f2e`; `None` for a build without an embedded revision.
    pub fn release(&self) -> Option<String> {
        self.revision
            .as_deref()
            .map(|revision| format!("{}+{}", self.version, &revision[..RELEASE_REVISION_CHARS]))
    }
}

/// A receiver for tests and evidence: loopback HTTP with an explicit port, or exactly the hosted
/// test destination. It can never be the production destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestReceiver {
    url: String,
}

impl TestReceiver {
    /// `url` when it is `http://` on `localhost`, `127.0.0.0/8` or `[::1]` with an explicit port,
    /// and carries no credentials, query or fragment, or exactly [`TEST_DESTINATION`]; otherwise
    /// a validation error. No other remote URL or query string is allowed.
    pub fn loopback(url: &str) -> Result<Self, Error> {
        if url == TEST_DESTINATION {
            return Ok(Self {
                url: url.to_owned(),
            });
        }
        let refused = |why: &str| Error::validation(format!("a test receiver {why}"));
        let parsed = url::Url::parse(url).map_err(|_| refused("must be an absolute URL"))?;
        if parsed.scheme() != "http" {
            return Err(refused("must use http on a loopback address"));
        }
        let loopback = match parsed.host() {
            Some(url::Host::Domain(domain)) => domain == "localhost",
            Some(url::Host::Ipv4(address)) => IpAddr::V4(address).is_loopback(),
            Some(url::Host::Ipv6(address)) => IpAddr::V6(address).is_loopback(),
            None => false,
        };
        if !loopback {
            return Err(refused("must be on a loopback address"));
        }
        if parsed.port().is_none() {
            return Err(refused("must name its port"));
        }
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(refused("carries no credentials, query or fragment"));
        }
        Ok(Self {
            url: parsed.to_string(),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

/// Whether this process may collect once a person has opted in, and where it would send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Eligibility {
    /// Production reporting to [`DESTINATION`]. Only [`Eligibility::production`] answers it, and
    /// only with the activation switch on.
    Production { release: String },
    /// A visible debug, unrevisioned or developer launch, always to the hosted test tenant.
    Developer { release: String },
    /// Explicit test eligibility: a test receiver, without the switch or a packaged build.
    Test(TestReceiver),
    /// Not eligible, for this reason.
    Ineligible(Reason),
}

impl Eligibility {
    /// The production or developer-test eligibility of a launch of this executable: [`PRODUCTION_ACTIVE`], the embedded
    /// [`BuildIdentity`], whether this is a debug build and the host's launch mode.
    pub fn production(launch: LaunchMode) -> Self {
        Self::decide(
            PRODUCTION_ACTIVE,
            &BuildIdentity::current(),
            launch,
            cfg!(debug_assertions),
        )
    }

    /// Explicit test eligibility with a validated test receiver. For isolated test and evidence hosts
    /// only; it still collects nothing without stored consent.
    pub fn test(receiver: TestReceiver) -> Self {
        Self::Test(receiver)
    }

    /// Visible developer launches bypass production activation and always use the test tenant.
    /// Other launches keep the production check order: activation, build, launch.
    fn decide(active: bool, build: &BuildIdentity, launch: LaunchMode, debug_build: bool) -> Self {
        if matches!(launch, LaunchMode::Interactive | LaunchMode::Developer)
            && (debug_build || build.release().is_none() || launch == LaunchMode::Developer)
        {
            return Self::Developer {
                release: build.release().unwrap_or_else(|| TEST_RELEASE.to_owned()),
            };
        }
        if !active {
            return Self::Ineligible(Reason::InactiveDestination);
        }
        let Some(release) = build.release() else {
            return Self::Ineligible(Reason::Build);
        };
        if debug_build || launch != LaunchMode::Interactive {
            return Self::Ineligible(Reason::LaunchMode);
        }
        Self::Production { release }
    }

    pub fn is_eligible(&self) -> bool {
        !matches!(self, Self::Ineligible(_))
    }

    /// Why this launch is not eligible, or `None` when it is.
    pub fn reason(&self) -> Option<Reason> {
        match self {
            Self::Ineligible(reason) => Some(*reason),
            _ => None,
        }
    }

    /// Where an eligible launch sends: the developer test tenant, test receiver, or [`DESTINATION`], which is
    /// also what an ineligible launch reports as the disclosed destination.
    pub fn destination(&self) -> &str {
        match self {
            Self::Test(receiver) => receiver.url(),
            Self::Developer { .. } => TEST_DESTINATION,
            _ => DESTINATION,
        }
    }

    /// The release a batch reports: its embedded revision, or the synthetic developer release.
    /// Explicit evidence hosts may override the executable's release in their supervisor.
    pub fn release(&self) -> Option<String> {
        match self {
            Self::Production { release } | Self::Developer { release } => Some(release.clone()),
            _ => BuildIdentity::current().release(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "9d4138ca1f2e5b7a0c3d4e5f60718293a4b5c6d7";

    #[test]
    fn usage_production_activation_stays_off() {
        const { assert!(!PRODUCTION_ACTIVE, "activation is a separate change") };
        let packaged = BuildIdentity::new("0.1.0", Some(REVISION));
        let off = Eligibility::decide(false, &packaged, LaunchMode::Interactive, false);
        assert_eq!(off, Eligibility::Ineligible(Reason::InactiveDestination));
        let on = Eligibility::decide(true, &packaged, LaunchMode::Interactive, false);
        assert_eq!(on.destination(), DESTINATION);
        assert_eq!(on.release().as_deref(), Some("0.1.0+9d4138ca1f2e"));
        assert_eq!(
            on,
            Eligibility::Production {
                release: "0.1.0+9d4138ca1f2e".into()
            }
        );
    }

    #[test]
    fn usage_visible_developer_builds_always_report_to_test_with_a_valid_release() {
        for revision in [
            None,
            Some(""),
            Some("dirty"),
            Some("9d4138ca1f2e"),
            Some("9D4138CA1F2E5B7A0C3D4E5F60718293A4B5C6D7"),
            Some(REVISION),
        ] {
            let build = BuildIdentity::new("0.1.0-rc.1+build.5", revision);
            for active in [false, true] {
                for debug in [false, true] {
                    for launch in [LaunchMode::Interactive, LaunchMode::Developer] {
                        let eligibility = Eligibility::decide(active, &build, launch, debug);
                        if !debug && build.release().is_some() && launch == LaunchMode::Interactive
                        {
                            continue;
                        }
                        assert!(matches!(eligibility, Eligibility::Developer { .. }));
                        assert_eq!(eligibility.destination(), TEST_DESTINATION);
                        let release = eligibility.release().unwrap();
                        assert_eq!(
                            release,
                            build.release().unwrap_or_else(|| TEST_RELEASE.into())
                        );
                        assert!(super::super::Release::parse(&release).is_some());
                    }
                }
            }
        }
    }

    #[test]
    fn usage_suppressed_launches_never_get_developer_eligibility() {
        for active in [false, true] {
            for revision in [None, Some(REVISION)] {
                for debug in [false, true] {
                    let build = BuildIdentity::new("0.0.0", revision);
                    for launch in [
                        LaunchMode::Hidden,
                        LaunchMode::Background,
                        LaunchMode::Evidence,
                        LaunchMode::Test,
                        LaunchMode::Headless,
                    ] {
                        let eligibility = Eligibility::decide(active, &build, launch, debug);
                        assert!(
                            !eligibility.is_eligible(),
                            "{active} {revision:?} {debug} {launch:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn usage_build_identity_retains_only_a_clean_revision_and_numeric_version() {
        let build = BuildIdentity::new("0.1.0-rc.1+build.5", Some(REVISION));
        assert_eq!(build.revision(), Some(REVISION));
        assert_eq!(build.release().as_deref(), Some("0.1.0+9d4138ca1f2e"));
        if option_env!("LUXFORGE_SOURCE_REVISION").is_none() {
            assert_eq!(BuildIdentity::current().release(), None);
        }
    }

    #[test]
    fn usage_test_eligibility_takes_only_loopback_or_exactly_the_hosted_test_url() {
        for url in [
            "http://127.0.0.1:8123/v1/usage/batches",
            "http://localhost:9000/",
            "http://[::1]:4000/batches",
        ] {
            let eligibility = Eligibility::test(TestReceiver::loopback(url).unwrap());
            assert!(eligibility.is_eligible());
            assert_eq!(eligibility.reason(), None);
            assert_ne!(eligibility.destination(), DESTINATION);
            assert!(eligibility.destination().starts_with("http://"));
        }
        let hosted = Eligibility::test(TestReceiver::loopback(TEST_DESTINATION).unwrap());
        assert_eq!(hosted.destination(), TEST_DESTINATION);
        for url in [
            DESTINATION,
            "https://api.luxforge.app/v1/usage/batches?tenant=prod",
            "https://api.luxforge.app/v1/usage/batches?tenant=test&x=1",
            "https://api.luxforge.app/v1/usage/batches?tenant=%74est",
            "https://api.luxforge.app/v1/usage/batches?tenant=test#fragment",
            "https://api.luxforge.app:443/v1/usage/batches?tenant=test",
            "https://user@api.luxforge.app/v1/usage/batches?tenant=test",
            "https://other.example/v1/usage/batches?tenant=test",
            "http://api.luxforge.app/v1/usage/batches?tenant=test",
            "http://127.0.0.1:8123/v1/usage/batches?tenant=test",
            "https://127.0.0.1:8123/",
            "http://api.luxforge.app:80/v1/usage/batches",
            "http://10.0.0.2:8123/",
            "http://127.0.0.1/",
            "http://user:pass@127.0.0.1:8123/",
            "http://127.0.0.1:8123/?q=1",
            "not a url",
        ] {
            let error = TestReceiver::loopback(url).unwrap_err();
            assert_eq!(error.kind, crate::ErrorKind::Validation, "{url}");
        }
    }

    #[test]
    fn usage_reasons_are_spelled_as_usage_read_reports_them() {
        for (reason, spelling) in [
            (Reason::Choice, "choice"),
            (Reason::LaunchMode, "launch-mode"),
            (Reason::Build, "build"),
            (Reason::InactiveDestination, "inactive-destination"),
            (Reason::UnreadableConfiguration, "unreadable-configuration"),
            (Reason::ChangedDisclosure, "changed-disclosure"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), spelling);
        }
    }
}
