//! The `consent-required` failure a gated operation returns before any work starts when no grant
//! covers its exact scope. Its data carries the scope a client grants and the disclosure a person
//! decides on: who asks and why, where data goes or comes from, what and how much, where it is
//! stored, its licence, retention and whether it may cost money. The desktop shows it as a consent
//! notice with Allow and Don't allow; a headless client receives the same failure.
use super::{
    descriptor::{AdapterCost, AdapterDescriptor, CapabilityDescriptor, ResourceDescriptor},
    grants::{GrantKind, GrantScope},
};
use crate::{Error, ErrorKind, ModuleDescriptor};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

/// What a person is told before allowing a capability. Every field is plain data from the
/// descriptor, the settings or a stat; none is ever a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disclosure {
    /// The module's title.
    pub module: String,
    /// The capability's declared purpose.
    pub purpose: String,
    /// Where data goes, or where a download comes from: an origin or a URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// What is read, sent or downloaded, in words.
    pub data: String,
    /// How many bytes are read, sent or downloaded, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Where a download is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// What the receiving service keeps, as its adapter declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
    pub cost: AdapterCost,
}

impl Disclosure {
    fn new(module: &ModuleDescriptor, capability: &CapabilityDescriptor, data: String) -> Self {
        Self {
            module: module.title.clone(),
            purpose: capability.purpose.clone(),
            destination: None,
            data,
            bytes: None,
            storage: None,
            license: None,
            retention: None,
            cost: AdapterCost::Free,
        }
    }
}

/// `download-artifact`: the pinned URL, the resource and its version, its size, where it is
/// installed and its licence. Resources are free to download.
pub fn download_disclosure(
    module: &ModuleDescriptor,
    capability: &CapabilityDescriptor,
    resource: &ResourceDescriptor,
    install_dir: &Path,
) -> Disclosure {
    Disclosure {
        destination: Some(resource.url.clone()),
        bytes: Some(resource.bytes),
        storage: Some(install_dir.display().to_string()),
        license: Some(resource.license.clone()),
        ..Disclosure::new(
            module,
            capability,
            format!("{} {}", resource.title, resource.version),
        )
    }
}

/// `remote-image-request`: the endpoint origin, the data class in words, the exact size of the
/// body the host sends, and the adapter's retention note and cost.
pub fn remote_disclosure(
    module: &ModuleDescriptor,
    capability: &CapabilityDescriptor,
    adapter: &AdapterDescriptor,
    scope: &super::grants::RemoteScope,
) -> Disclosure {
    Disclosure {
        destination: Some(scope.origin.clone()),
        bytes: Some(scope.data.request_bytes()),
        retention: adapter.retention.clone(),
        cost: adapter.cost,
        ..Disclosure::new(module, capability, scope.data.describe().to_owned())
    }
}

/// The failure a gated operation returns when no live grant covers `scope`: `consent-required`
/// with `data.consent: {module_id, capability, kind, scope, disclosure, denied}`. `denied` is true
/// when a denial of exactly this scope is recorded, so a client can tell a first request from one
/// the person already declined.
pub fn consent_required(
    module: &ModuleDescriptor,
    capability: &CapabilityDescriptor,
    scope: &GrantScope,
    disclosure: Disclosure,
    denied: bool,
) -> Error {
    let kind = GrantKind::of(&capability.kind);
    let detail = if denied {
        format!(
            "{} needs permission to use {} ({}); it was not allowed for this scope",
            module.title,
            capability.id,
            kind.name()
        )
    } else {
        format!(
            "{} needs permission to use {} ({})",
            module.title,
            capability.id,
            kind.name()
        )
    };
    Error::new(ErrorKind::ConsentRequired, detail).with_data(json!({
        "consent": {
            "module_id": module.id,
            "capability": capability.id,
            "kind": kind,
            "scope": scope,
            "disclosure": disclosure,
            "denied": denied,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetId,
        capabilities::{
            grants::{DownloadScope, RemoteScope},
            testing::{MODULE, adapter, capability_descriptor, temp},
        },
    };

    #[test]
    fn each_disclosure_names_what_a_person_decides_on() {
        let module = capability_descriptor();
        let root = temp("disclosure");
        std::fs::create_dir_all(&root).unwrap();

        let resource = &module.resources[0];
        let download =
            download_disclosure(&module, &module.capabilities[1], resource, &root.join("v"));
        assert_eq!(download.module, "Capabilities test");
        assert_eq!(download.purpose, "Install the tint palette.");
        assert_eq!(download.destination.as_deref(), Some(resource.url.as_str()));
        assert_eq!(download.data, "Tint palette 1.0.0");
        assert_eq!(download.bytes, Some(12));
        assert_eq!(download.license.as_deref(), Some("CC0-1.0"));
        assert!(download.storage.as_deref().unwrap().ends_with("v"));
        assert_eq!(download.cost, AdapterCost::Free);

        let scope = RemoteScope {
            profile_id: "profile-1".into(),
            adapter: "echo-adapter".into(),
            origin: "https://echo.example.com".into(),
            data: crate::capabilities::descriptor::DataClass::SampleGrid8,
            asset_id: AssetId::new(),
        };
        let remote = remote_disclosure(&module, &module.capabilities[0], &adapter(), &scope);
        assert_eq!(
            remote.destination.as_deref(),
            Some("https://echo.example.com")
        );
        assert_eq!(
            remote.data,
            "an 8 × 8 grid of rendered colour samples of this photo"
        );
        assert_eq!(
            remote.bytes,
            Some(crate::capabilities::data::SAMPLE_GRID_BYTES as u64),
            "the exact JSON body of 64 samples"
        );
        assert_eq!(
            remote.retention.as_deref(),
            Some("The echo service keeps nothing.")
        );

        let download_scope = GrantScope::Download(DownloadScope {
            resource: "palette".into(),
            version: "1.0.0".into(),
            origin: "https://example.com".into(),
        });
        let error = consent_required(
            &module,
            &module.capabilities[1],
            &download_scope,
            download,
            true,
        );
        assert_eq!(error.kind, ErrorKind::ConsentRequired);
        assert!(error.detail.contains("was not allowed"), "{}", error.detail);
        let consent = &error.data.as_ref().unwrap()["consent"];
        assert_eq!(consent["module_id"], MODULE);
        assert_eq!(consent["capability"], "palette");
        assert_eq!(consent["kind"], "download-artifact");
        assert_eq!(consent["scope"]["version"], "1.0.0");
        assert_eq!(consent["denied"], true);
        assert_eq!(consent["disclosure"]["cost"], "free");
        let remote_scope = GrantScope::Remote(scope);
        let error = consent_required(
            &module,
            &module.capabilities[0],
            &remote_scope,
            remote,
            false,
        );
        assert!(!error.detail.contains("not allowed"));
        let _ = std::fs::remove_dir_all(root);
    }
}
