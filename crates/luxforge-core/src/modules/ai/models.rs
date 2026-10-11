//! The generative models `luxforge.ai` offers and how a developer run installs one.
//!
//! A run offers only a model it can install: this build links no qualified generative model, so
//! the module is registered in developer runs only, with the developer model table below, and a
//! production registry has no `luxforge.ai` at all until the real-models work adds its qualified
//! models. The developer model is the deterministic fake generator
//! ([`crate::inference::fake::FakeInference`]), which a developer host serves under the backend
//! name [`crate::inference::fake::FAKE_HOST_BACKEND`]; its resources are two small pinned files whose URLs lie under the reserved
//! `.invalid` domain, which never resolves. A developer host installs them through the ordinary
//! download grant and install job, its transport wrapped in [`DeveloperModels`], which answers
//! exactly those URLs from memory and passes every other request to the transport it wraps. A host
//! without the wrapper cannot install them: the download fails, and nothing reaches the network.
use crate::{
    Error,
    capabilities::{
        descriptor::{CapabilityDescriptor, CapabilityKind, ModelKind, ResourceDescriptor},
        transport::{Method, SendOptions, Transport, TransportRequest, TransportResponse},
    },
    inference::Precision,
};
use std::{io::Write, sync::Arc};

/// The origin every developer model's URL is under.
pub(crate) const DEVELOPER_ORIGIN: &str = "https://developer.luxforge.invalid";

/// One installable file of a model: a managed resource and the capability that installs it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ModelFile {
    pub(crate) resource: &'static str,
    pub(crate) title: &'static str,
    pub(crate) version: &'static str,
    pub(crate) bytes: &'static [u8],
    pub(crate) sha256: &'static str,
    pub(crate) format: &'static str,
    pub(crate) provenance: &'static str,
    /// The `download-artifact` capability that installs it.
    pub(crate) download: &'static str,
}

impl ModelFile {
    pub(crate) fn url(&self) -> String {
        format!(
            "{DEVELOPER_ORIGIN}/models/{}/{}/{}.bin",
            self.resource, self.version, self.resource
        )
    }

    pub(crate) fn descriptor(&self) -> ResourceDescriptor {
        ResourceDescriptor {
            id: self.resource.into(),
            title: self.title.into(),
            version: self.version.into(),
            url: self.url(),
            bytes: self.bytes.len() as u64,
            sha256: self.sha256.into(),
            format: self.format.into(),
            license: "GPL-3.0-or-later".into(),
            provenance: self.provenance.into(),
            redirect_origins: Vec::new(),
            files: Vec::new(),
            tradeoff: None,
            use_restriction: None,
            min_memory_bytes: None,
        }
    }

    fn capability(&self) -> CapabilityDescriptor {
        CapabilityDescriptor {
            id: self.download.into(),
            kind: CapabilityKind::DownloadArtifact {
                resource: self.resource.into(),
            },
            purpose: format!("Install {}", self.title),
        }
    }
}

/// One generative model the Generative tier offers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GenerativeModel {
    /// The model's identity in a request's `model`: its generator resource's.
    pub(crate) id: &'static str,
    pub(crate) generator: ModelFile,
    /// The `local-runtime` capability that holds the generator loaded.
    pub(crate) runtime: &'static str,
    pub(crate) resident_bytes: u64,
    pub(crate) backends: &'static [&'static str],
    pub(crate) precision: Precision,
    /// The square side the model generates at.
    pub(crate) side: u32,
    /// Denoising steps per candidate.
    pub(crate) steps: u32,
    /// The text encoder a prompt needs, a separate optional resource.
    pub(crate) text_encoder: Option<ModelFile>,
}

impl GenerativeModel {
    /// The capabilities the model declares: its files' downloads and its generator's runtime.
    pub(crate) fn capabilities(&self) -> Vec<CapabilityDescriptor> {
        let mut capabilities = vec![
            self.generator.capability(),
            CapabilityDescriptor {
                id: self.runtime.into(),
                kind: CapabilityKind::LocalRuntime {
                    resource: self.generator.resource.into(),
                    model: ModelKind::Generator,
                    resident_bytes: self.resident_bytes,
                    backends: self.backends.iter().map(|b| (*b).into()).collect(),
                    precision: self.precision,
                },
                purpose: format!("Generate fills with {}", self.generator.title),
            },
        ];
        capabilities.extend(self.text_encoder.map(|encoder| encoder.capability()));
        capabilities
    }

    /// Every file of the model.
    pub(crate) fn files(&self) -> impl Iterator<Item = ModelFile> {
        std::iter::once(self.generator).chain(self.text_encoder)
    }
}

/// The developer generator's file.
pub(crate) const FAKE_GENERATOR: ModelFile = ModelFile {
    resource: "fake-generator",
    title: "Developer fake generator",
    version: "1",
    bytes: b"Luxforge developer fake generator, version 1.\n",
    sha256: "ed04660c7aef25e09c822083df086a020160d9d8a80a082b37557e5070b65206",
    format: "luxforge-fake-generator",
    provenance: "A Luxforge developer fixture: a deterministic stand-in for a generative model, run by the fake inference runtime",
    download: "download-fake-generator",
};

/// The developer text encoder's file.
pub(crate) const FAKE_TEXT_ENCODER: ModelFile = ModelFile {
    resource: "fake-text-encoder",
    title: "Developer fake text encoder",
    version: "1",
    bytes: b"Luxforge developer fake text encoder, version 1.\n",
    sha256: "76f363a0a32ab7362bad24a0f2e2c4f329dd1fa57be4f9c84395542527933fdb",
    format: "luxforge-fake-text-encoder",
    provenance: "A Luxforge developer fixture: stands in for a generative model's text encoder, which a prompt needs",
    download: "download-fake-text-encoder",
};

/// The models a developer run offers: the fake generator, with its fake text encoder.
pub(crate) const DEVELOPER_MODELS: &[GenerativeModel] = &[GenerativeModel {
    id: "fake-generator",
    generator: FAKE_GENERATOR,
    runtime: "run-fake-generator",
    resident_bytes: 64 * 1024 * 1024,
    backends: &[crate::inference::fake::FAKE_HOST_BACKEND],
    precision: Precision::Fp32,
    side: 512,
    steps: 8,
    text_encoder: Some(FAKE_TEXT_ENCODER),
}];

/// A transport that answers the developer models' URLs from memory and passes every other request
/// to the transport it wraps: what a developer run installs its fake models through. Only a
/// developer host wraps its transport in it.
#[derive(Debug)]
pub struct DeveloperModels {
    inner: Arc<dyn Transport>,
}

impl DeveloperModels {
    /// `inner`, answering the developer models' URLs itself.
    pub fn wrap(inner: Arc<dyn Transport>) -> Arc<dyn Transport> {
        Arc::new(Self { inner })
    }

    /// A run's transport: `inner` wrapped in a developer run, `inner` itself otherwise.
    pub fn for_run(developer: bool, inner: Arc<dyn Transport>) -> Arc<dyn Transport> {
        if developer { Self::wrap(inner) } else { inner }
    }

    fn file(request: &TransportRequest) -> Option<ModelFile> {
        if request.method != Method::Get {
            return None;
        }
        let url = request.endpoint.url.as_str();
        DEVELOPER_MODELS
            .iter()
            .flat_map(GenerativeModel::files)
            .find(|file| file.url() == url)
    }
}

impl Transport for DeveloperModels {
    fn send(
        &self,
        request: &TransportRequest,
        options: SendOptions<'_>,
        sink: &mut dyn Write,
    ) -> Result<TransportResponse, Error> {
        let Some(file) = Self::file(request) else {
            return self.inner.send(request, options, sink);
        };
        options.control.checkpoint()?;
        let length = file.bytes.len() as u64;
        if length > options.max_response_bytes {
            return Err(Error::resource_limit(format!(
                "{} is {length} bytes, past the {} the request accepts",
                file.resource, options.max_response_bytes
            )));
        }
        sink.write_all(file.bytes)
            .map_err(|error| Error::file_access(format!("writing {}: {error}", file.resource)))?;
        (options.progress)(length, Some(length));
        Ok(TransportResponse {
            status: 200,
            headers: vec![("content-length".into(), length.to_string())],
            final_url: request.endpoint.url.clone(),
            received: length,
        })
    }
}
