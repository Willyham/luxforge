//! Download targets and update channels.
use serde::{Deserialize, Serialize};
use std::fmt;

/// A download target, with one feed each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Target {
    #[serde(rename = "macos-arm64")]
    MacosArm64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
    /// Expressible for when Windows is distributed; no release has it yet.
    #[serde(rename = "windows-x86_64")]
    WindowsX86_64,
}

impl Target {
    pub const ALL: [Target; 3] = [
        Target::MacosArm64,
        Target::LinuxX86_64,
        Target::WindowsX86_64,
    ];
    /// The targets every feed generation writes, whether or not a record names them.
    pub const RELEASED: [Target; 2] = [Target::MacosArm64, Target::LinuxX86_64];

    /// The name in file names, records and feed paths: `macos-arm64`.
    pub fn as_str(self) -> &'static str {
        match self {
            Target::MacosArm64 => "macos-arm64",
            Target::LinuxX86_64 => "linux-x86_64",
            Target::WindowsX86_64 => "windows-x86_64",
        }
    }

    pub fn parse(name: &str) -> Option<Target> {
        Target::ALL.into_iter().find(|t| t.as_str() == name)
    }

    /// The Rust target triple a release build of it records in `target.json`.
    pub fn rust_target(self) -> &'static str {
        match self {
            Target::MacosArm64 => "aarch64-apple-darwin",
            Target::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Target::WindowsX86_64 => "x86_64-pc-windows-msvc",
        }
    }

    pub fn from_rust_target(triple: &str) -> Option<Target> {
        Target::ALL.into_iter().find(|t| t.rust_target() == triple)
    }

    /// The feed's path below the feed base URL: `macos-arm64/appcast.xml`.
    pub fn feed_path(self) -> String {
        format!("{}/appcast.xml", self.as_str())
    }

    /// A person-facing name, for a feed's description.
    pub fn label(self) -> &'static str {
        match self {
            Target::MacosArm64 => "Apple silicon macOS",
            Target::LinuxX86_64 => "x86-64 Linux",
            Target::WindowsX86_64 => "x86-64 Windows",
        }
    }

    /// Whether Sparkle installs it, so its items carry `sparkle:minimumSystemVersion` and only a
    /// notarized archive is listed.
    pub fn is_macos(self) -> bool {
        self == Target::MacosArm64
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An update channel. Clients always follow the default channel; the pre-release channel only
/// when the person turns it on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    Default,
    Prerelease,
}

impl Channel {
    /// The channel of a release version: `prerelease` for a SemVer pre-release such as
    /// `0.2.0-rc.1`, otherwise the default.
    pub fn of(version: &semver::Version) -> Channel {
        if version.pre.is_empty() {
            Channel::Default
        } else {
            Channel::Prerelease
        }
    }

    /// The `sparkle:channel` text, which the default channel has none of.
    pub fn sparkle_name(self) -> Option<&'static str> {
        match self {
            Channel::Default => None,
            Channel::Prerelease => Some("prerelease"),
        }
    }
}
