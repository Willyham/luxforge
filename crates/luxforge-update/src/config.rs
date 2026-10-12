//! `release/update.json`: the feed base URL and the update public key, the single source for the
//! macOS `SUPublicEDKey` and the Linux updater's compiled constant.
use crate::{
    Target,
    error::{Error, Result},
    key::PublicKey,
};
use serde::Deserialize;
use std::path::Path;

/// The configuration's path from the repository root.
pub const CONFIG_PATH: &str = "release/update.json";
/// The checked-in withdrawal list's path from the repository root.
pub const WITHDRAWN_PATH: &str = "release/withdrawn-updates.json";
/// The largest configuration read.
const MAX_CONFIG_BYTES: u64 = 16 * 1024;

/// The update configuration.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateConfig {
    /// The feeds' common base, an `https` URL ending in `/`.
    pub feed_base_url: String,
    /// The update public key; `None` until the operator commits it, and a build without it has no
    /// working updater.
    pub public_key: Option<PublicKey>,
}

impl UpdateConfig {
    pub fn parse(json: &str) -> Result<UpdateConfig> {
        let config: UpdateConfig = serde_json::from_str(json)
            .map_err(|error| Error::MalformedConfig(error.to_string()))?;
        let url = &config.feed_base_url;
        let valid = url.starts_with("https://")
            && url.len() > "https://".len() + 1
            && url.ends_with('/')
            && url
                .bytes()
                .all(|b| b.is_ascii_graphic() && !b"?#\"<>\\".contains(&b));
        if !valid {
            return Err(Error::MalformedConfig(format!(
                "feed_base_url {url:?} must be an https URL ending in /, with no query or fragment"
            )));
        }
        Ok(config)
    }

    /// Reads the configuration from `path`, for xtask and build scripts.
    pub fn load(path: &Path) -> Result<UpdateConfig> {
        let file = std::fs::File::open(path)?;
        if file.metadata()?.len() > MAX_CONFIG_BYTES {
            return Err(Error::MalformedConfig(format!(
                "{} is larger than {MAX_CONFIG_BYTES} bytes",
                path.display()
            )));
        }
        UpdateConfig::parse(&std::fs::read_to_string(path)?)
    }

    /// The configuration in a repository checkout.
    pub fn load_from_repository(root: &Path) -> Result<UpdateConfig> {
        UpdateConfig::load(&root.join(CONFIG_PATH))
    }

    /// A target's feed: `<base><target>/appcast.xml`.
    pub fn feed_url(&self, target: Target) -> String {
        format!("{}{}", self.feed_base_url, target.feed_path())
    }
}
