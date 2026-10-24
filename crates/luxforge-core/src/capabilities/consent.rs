//! The `consent-required` failure a gated operation returns before any work starts when no grant
//! covers its exact scope. Its data carries the scope a client grants and the disclosure a person
//! decides on: who asks and why, where data goes or comes from, what and how much, where it is
//! stored, its licence, retention and whether it may cost money. The desktop shows it as a consent
//! notice, or as the setup sheet of a download a setup introduces; a headless client receives the
//! same failure. A setup install asks for every grant its missing downloads need at once
//! ([`setup_consent_required`]).
use super::{
    descriptor::{
        AdapterCost, AdapterDescriptor, CapabilityDescriptor, Introduction, ResourceDescriptor,
    },
    grants::GrantScope,
    resources::Requirements,
    setups::SetupDisclosure,
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{Error, ModuleDescriptor};
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
    /// The other origins a download's further files come from, when any is not the
    /// destination's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub further_origins: Vec<String>,
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
    /// A model's plain trade-off, which the person chose it by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tradeoff: Option<String>,
    /// The one line a download states when the licence restricts how the person may use their own
    /// results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_restriction: Option<String>,
    /// What a model needs of the machine: its backends, precision, resident size and minimum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements: Option<Requirements>,
    /// What the setup a download belongs to says of the tool it sets up: the smallest setup that
    /// holds the resource ([`super::setups::introducing`]), when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introduction: Option<Introduction>,
    /// This machine's memory, for a download that declares a memory minimum, when the host can
    /// tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_memory_bytes: Option<u64>,
    /// Whether this machine meets the download's memory minimum, as `module.resource.list`'s
    /// `fits` says: absent when it declares none or the host cannot tell. A machine that does not
    /// fit is still asked, so a client can say why, and its install is refused `resource-limit`
    /// once it is allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fits: Option<bool>,
}

impl Disclosure {
    fn new(module: &ModuleDescriptor, capability: &CapabilityDescriptor, data: String) -> Self {
        Self {
            module: module.title.clone(),
            purpose: capability.purpose.clone(),
            destination: None,
            further_origins: Vec::new(),
            data,
            bytes: None,
            storage: None,
            license: None,
            retention: None,
            cost: AdapterCost::Free,
            tradeoff: None,
            use_restriction: None,
            requirements: None,
            introduction: None,
            machine_memory_bytes: None,
            fits: None,
        }
    }
}

/// `download-artifact`: the pinned URL and any other origin a further file comes from, the
/// resource and its version, the size of every file together, where it is installed and its
/// licence; for a model, its trade-off, the use
/// restriction when its licence has one, and what it needs of the machine. Resources are free to
/// download. Nothing is said of a model's training data.
pub(crate) fn download_disclosure(
    module: &ModuleDescriptor,
    capability: &CapabilityDescriptor,
    resource: &ResourceDescriptor,
    install_dir: &Path,
) -> Disclosure {
    let data = match resource.files.len() {
        0 => format!("{} {}", resource.title, resource.version),
        further => format!(
            "{} {} ({} files)",
            resource.title,
            resource.version,
            further + 1
        ),
    };
    let origin = |url: &str| {
        super::endpoint::parse_endpoint(
            url,
            &[
                super::endpoint::EndpointClass::Remote,
                super::endpoint::EndpointClass::Loopback,
            ],
        )
        .map(|endpoint| endpoint.origin())
        .ok()
    };
    let own = origin(&resource.url);
    let mut further_origins: Vec<String> = Vec::new();
    for file in &resource.files {
        if let Some(served) = origin(&file.url)
            && Some(&served) != own.as_ref()
            && !further_origins.contains(&served)
        {
            further_origins.push(served);
        }
    }
    Disclosure {
        destination: Some(resource.url.clone()),
        further_origins,
        bytes: Some(resource.total_bytes()),
        storage: Some(install_dir.display().to_string()),
        license: Some(resource.license.clone()),
        tradeoff: resource.tradeoff.clone(),
        use_restriction: resource.use_restriction.clone(),
        requirements: Requirements::of(module, resource),
        introduction: super::setups::introducing(module, &resource.id)
            .map(|setup| setup.introduction.clone()),
        ..Disclosure::new(module, capability, data)
    }
}

/// One grant a setup install still needs: the capability and its exact scope, as
/// `module.permission.grant` takes them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupGrant {
    pub capability: String,
    pub kind: super::grants::GrantKind,
    pub scope: GrantScope,
    /// A denial of exactly this scope is recorded.
    pub denied: bool,
}

/// The failure `module.setup.install` returns when any download it would start has no live
/// grant: `consent-required` with `data.consent: {module_id, setup, grants: [{capability, kind,
/// scope, denied}], disclosure, denied}`, every missing grant listed in declared order, so one
/// answer grants them all. `denied` is true when a denial of any of them is recorded.
pub(crate) fn setup_consent_required(
    module: &ModuleDescriptor,
    setup: &str,
    grants: Vec<SetupGrant>,
    disclosure: SetupDisclosure,
) -> Error {
    let denied = grants.iter().any(|grant| grant.denied);
    let count = grants.len();
    let what = if count == 1 {
        "one download".to_owned()
    } else {
        format!("{count} downloads")
    };
    let mut detail = format!(
        "{} needs permission for {what} to set up {}",
        module.title, disclosure.title
    );
    if denied {
        detail.push_str("; one was not allowed before");
    }
    Error::consent_required(detail).with_data(json!({
        "consent": {
            "module_id": module.id,
            "setup": setup,
            "grants": grants,
            "disclosure": disclosure,
            "denied": denied,
        }
    }))
}

/// `remote-image-request`: the endpoint origin, the data class in words, the exact size of the
/// body the host sends, and the adapter's retention note and cost.
pub(crate) fn remote_disclosure(
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
pub(crate) fn consent_required(
    module: &ModuleDescriptor,
    capability: &CapabilityDescriptor,
    scope: &GrantScope,
    disclosure: Disclosure,
    denied: bool,
) -> Error {
    let kind = scope.kind();
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
    Error::consent_required(detail).with_data(json!({
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
        assert_eq!(
            (
                &download.tradeoff,
                &download.use_restriction,
                &download.requirements
            ),
            (&None, &None, &None),
            "a resource that is no model says nothing of one"
        );
        assert_eq!(download.introduction, None, "nor introduces a feature");

        // A model of two files: their size together, its trade-off, its use restriction and what it
        // needs of the machine; never a word of training data.
        let mut model = module.clone();
        let resource = &mut model.resources[0];
        resource.files = vec![crate::capabilities::descriptor::ResourceFile {
            name: "decoder".into(),
            url: "https://example.com/decoder".into(),
            bytes: 30,
            sha256: "c".repeat(64),
        }];
        resource.tradeoff = Some("Finds smaller objects; slower to start".into());
        resource.use_restriction = Some("Non-commercial use only".into());
        resource.min_memory_bytes = Some(8 << 30);
        // The introduction of the setup that holds it travels with it, for a client's setup sheet.
        let introduction = Introduction {
            eyebrow: "Palette".into(),
            title: "Tint anything".into(),
            lead: "Download the palette to tint a photo.".into(),
            hero: Some(crate::capabilities::descriptor::IntroductionPicture::Describe),
            steps: Vec::new(),
            promise: "Your photos stay on this computer.".into(),
        };
        model.setups = vec![crate::capabilities::descriptor::SetupDescriptor {
            id: "tint".into(),
            title: "Tint".into(),
            introduction: introduction.clone(),
            parts: vec![crate::capabilities::descriptor::SetupPart {
                name: "Palette".into(),
                purpose: "The gains a tint is drawn from".into(),
                resources: vec!["palette".into()],
            }],
        }];
        let disclosed = download_disclosure(
            &model,
            &model.capabilities[1],
            &model.resources[0],
            &root.join("v"),
        );
        assert_eq!(disclosed.data, "Tint palette 1.0.0 (2 files)");
        assert_eq!(disclosed.bytes, Some(42));
        assert_eq!(
            disclosed.tradeoff.as_deref(),
            Some("Finds smaller objects; slower to start")
        );
        assert_eq!(
            disclosed.use_restriction.as_deref(),
            Some("Non-commercial use only")
        );
        assert_eq!(
            disclosed.requirements,
            Some(Requirements {
                min_memory_bytes: Some(8 << 30),
                ..Requirements::default()
            })
        );
        assert_eq!(disclosed.introduction, Some(introduction));
        let text = serde_json::to_string(&disclosed).unwrap().to_lowercase();
        assert!(!text.contains("training"), "{text}");

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
