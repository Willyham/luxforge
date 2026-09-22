//! Immutable derived artifacts: content-addressed bytes that committed recipes reference by an
//! opaque identity, stored beside the catalog. See `docs/design/module-capabilities.md`.
use crate::{Error, ErrorKind};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

const PREFIX: &str = "artifact-";

/// One artifact's identity, derived by the host from the SHA-256 of its bytes. Clients treat it as
/// opaque; the host relies on it naming exactly one content, so publishing the same bytes twice
/// yields the same artifact.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactId(String);

impl ArtifactId {
    /// The identity of the bytes whose lowercase hexadecimal SHA-256 is `sha256`.
    pub fn for_hash(sha256: &str) -> Result<Self, Error> {
        Self::parse(format!("{PREFIX}{sha256}"))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        let valid = value.strip_prefix(PREFIX).is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if valid {
            Ok(Self(value))
        } else {
            Err(Error::new(ErrorKind::Validation, "invalid ArtifactId"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The lowercase hexadecimal SHA-256 of the artifact's bytes.
    pub fn sha256(&self) -> &str {
        &self.0[PREFIX.len()..]
    }
}

impl std::fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for ArtifactId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ArtifactId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_artifact_identity_is_the_prefix_and_a_lowercase_sha256() {
        let hash = "a".repeat(64);
        let id = ArtifactId::for_hash(&hash).unwrap();
        assert_eq!(id.as_str(), format!("artifact-{hash}"));
        assert_eq!(id.sha256(), hash);
        for bad in [
            "artifact-".to_owned(),
            format!("artifact-{}", "A".repeat(64)),
            format!("artifact-{}", "a".repeat(63)),
            format!("layer-{}", "a".repeat(64)),
            format!("artifact-{}g", "a".repeat(63)),
        ] {
            assert!(ArtifactId::parse(bad.clone()).is_err(), "{bad}");
        }
        let json = serde_json::to_value(&id).unwrap();
        assert_eq!(serde_json::from_value::<ArtifactId>(json).unwrap(), id);
    }
}
