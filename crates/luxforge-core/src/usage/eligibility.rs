//! Which launches may collect: launch mode, build identity (version plus embedded clean source
//! revision) and the production activation switch
//! ([design](../../../../docs/design/usage-statistics.md#build-identity-and-activation)).
//!
//! A launch is eligible in exactly two ways: production, which needs the activation switch on, a
//! build `cargo xtask package` revisioned from a clean tree, a release (not debug) build and an
//! ordinary visible desktop launch; or explicit test eligibility, which names a loopback receiver
//! and can never reach [`super::DESTINATION`]. Every other launch is ineligible with one fixed
//! [`Reason`], whatever consent is stored. Eligibility never grants consent: an eligible launch
//! still collects nothing until a person opts in.
use super::DESTINATION;
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
    /// A debug/developer, hidden, background, evidence, test or headless launch.
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

/// A local receiver for tests and evidence runs: a plain-HTTP URL on a loopback address with an
/// explicit port. It can never be the production destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestReceiver {
    url: String,
}

impl TestReceiver {
    /// `url` when it is `http://` on `localhost`, `127.0.0.0/8` or `[::1]` with an explicit port,
    /// and carries no credentials, query or fragment; otherwise a validation error.
    pub fn loopback(url: &str) -> Result<Self, Error> {
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
    /// Explicit test eligibility: a loopback receiver, without the switch or a packaged build.
    Test(TestReceiver),
    /// Not eligible, for this reason.
    Ineligible(Reason),
}

impl Eligibility {
    /// The eligibility of a launch of this executable: [`PRODUCTION_ACTIVE`], the embedded
    /// [`BuildIdentity`], whether this is a debug build and the host's launch mode.
    pub fn production(launch: LaunchMode) -> Self {
        Self::decide(
            PRODUCTION_ACTIVE,
            &BuildIdentity::current(),
            launch,
            cfg!(debug_assertions),
        )
    }

    /// Explicit test eligibility with a loopback receiver. For isolated test and evidence hosts
    /// only; it still collects nothing without stored consent.
    pub fn test(receiver: TestReceiver) -> Self {
        Self::Test(receiver)
    }

    /// The fixed order of the production checks: the activation switch, then the build, then the
    /// launch. Private, so no caller can choose an active switch.
    fn decide(active: bool, build: &BuildIdentity, launch: LaunchMode, debug_build: bool) -> Self {
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

    /// Where an eligible launch sends: the test receiver's URL, otherwise [`DESTINATION`], which is
    /// also what an ineligible launch reports as the disclosed destination.
    pub fn destination(&self) -> &str {
        match self {
            Self::Test(receiver) => receiver.url(),
            _ => DESTINATION,
        }
    }

    /// The release a batch reports: the packaged build's, or this executable's for any other
    /// launch (`None` when it carries no revision).
    pub fn release(&self) -> Option<String> {
        match self {
            Self::Production { release } => Some(release.clone()),
            _ => BuildIdentity::current().release(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "9d4138ca1f2e5b7a0c3d4e5f60718293a4b5c6d7";

    #[test]
    fn usage_the_activation_switch_is_off_and_every_production_launch_reports_it() {
        const {
            assert!(
                !PRODUCTION_ACTIVE,
                "turning production on is a separate change"
            )
        };
        for launch in [
            LaunchMode::Interactive,
            LaunchMode::Developer,
            LaunchMode::Headless,
        ] {
            assert_eq!(
                Eligibility::production(launch),
                Eligibility::Ineligible(Reason::InactiveDestination)
            );
        }
        let packaged = BuildIdentity::new("0.0.0", Some(REVISION));
        assert_eq!(
            Eligibility::decide(false, &packaged, LaunchMode::Interactive, false),
            Eligibility::Ineligible(Reason::InactiveDestination)
        );
    }

    #[test]
    fn usage_an_unrevisioned_build_is_ineligible_and_a_packaged_one_reports_its_release() {
        for revision in [
            None,
            Some(""),
            Some("9d4138ca1f2e"),
            Some("9D4138CA1F2E5B7A0C3D4E5F60718293A4B5C6D7"),
            Some("dirty"),
        ] {
            let build = BuildIdentity::new("0.0.0", revision);
            assert_eq!(build.release(), None, "{revision:?}");
            assert_eq!(
                Eligibility::decide(true, &build, LaunchMode::Interactive, false),
                Eligibility::Ineligible(Reason::Build),
                "{revision:?}"
            );
        }
        let packaged = BuildIdentity::new("0.0.0", Some(REVISION));
        assert_eq!(packaged.revision(), Some(REVISION));
        assert_eq!(packaged.release().as_deref(), Some("0.0.0+9d4138ca1f2e"));
        assert!(super::super::Release::parse(&packaged.release().unwrap()).is_some());
        assert_eq!(
            Eligibility::decide(true, &packaged, LaunchMode::Interactive, false),
            Eligibility::Production {
                release: "0.0.0+9d4138ca1f2e".into()
            }
        );
        let tagged = BuildIdentity::new("0.1.0-rc.1+build.5", Some(REVISION));
        assert_eq!(tagged.release().as_deref(), Some("0.1.0+9d4138ca1f2e"));
        assert!(super::super::Release::parse(&tagged.release().unwrap()).is_some());
        // An ordinary test build of the core has no embedded revision.
        if option_env!("LUXFORGE_SOURCE_REVISION").is_none() {
            assert_eq!(BuildIdentity::current().release(), None);
        }
    }

    #[test]
    fn usage_every_launch_but_an_ordinary_release_desktop_is_ineligible() {
        let packaged = BuildIdentity::new("0.0.0", Some(REVISION));
        for launch in [
            LaunchMode::Developer,
            LaunchMode::Hidden,
            LaunchMode::Background,
            LaunchMode::Evidence,
            LaunchMode::Test,
            LaunchMode::Headless,
        ] {
            assert_eq!(
                Eligibility::decide(true, &packaged, launch, false),
                Eligibility::Ineligible(Reason::LaunchMode),
                "{launch:?}"
            );
        }
        assert_eq!(
            Eligibility::decide(true, &packaged, LaunchMode::Interactive, true),
            Eligibility::Ineligible(Reason::LaunchMode),
            "a debug build is a developer launch"
        );
        let ineligible = Eligibility::Ineligible(Reason::LaunchMode);
        assert!(!ineligible.is_eligible());
        assert_eq!(ineligible.destination(), DESTINATION);
    }

    #[test]
    fn usage_test_eligibility_takes_only_a_loopback_receiver() {
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
        for url in [
            DESTINATION,
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
