//! The generative models `luxforge.ai` offers and how a run installs one.
//!
//! **The qualified model** ([`KLEIN`]) is FLUX.2 klein 4B, Q4_0, with fal's object removal LoRA
//! and the prompt-free removal conditioning, run by `luxforge-diffusion` (stable-diffusion.cpp)
//! under the backend name `metal` on macOS and `diffusion-cpu` on Linux. Its weights come from
//! their publishers on Hugging Face, pinned by revision, length and SHA-256; the conditioning is a
//! file the project computes and publishes on its GitHub releases. Its text encoder, which only
//! Replace needs, is a separate, optional resource, held loaded with the model by a second
//! `local-runtime` capability. Every run offers it.
//!
//! **The developer model** is the deterministic fake generator
//! ([`crate::inference::fake::FakeInference`]), which a developer host serves under the backend
//! name [`crate::inference::fake::FAKE_HOST_BACKEND`]; its resources are two small pinned files
//! whose URLs lie under the reserved `.invalid` domain, which never resolves. A developer run offers
//! it before the qualified model and installs it through the ordinary download grant and install
//! job, its transport wrapped in [`DeveloperModels`], which answers exactly those URLs from memory
//! and passes every other request to the transport it wraps. A host without the wrapper cannot
//! install them: the download fails, and nothing reaches the network.
//!
//! The file names inside a model's directory are the contract with `luxforge-diffusion`, whose
//! tests hold its layout to these descriptors.
use crate::{
    Error,
    capabilities::{
        descriptor::{
            CapabilityDescriptor, CapabilityKind, ModelKind, ResourceDescriptor, ResourceFile,
        },
        transport::{Method, SendOptions, Transport, TransportRequest, TransportResponse},
    },
    inference::Precision,
};
use std::{io::Write, sync::Arc};

/// The origin every developer model's URL is under.
pub(crate) const DEVELOPER_ORIGIN: &str = "https://developer.luxforge.invalid";

const GIB: u64 = 1024 * 1024 * 1024;

/// A published file's download: its pinned URL and length.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Published {
    pub(crate) url: &'static str,
    pub(crate) bytes: u64,
}

/// A further file of a model's resource, installed beside its own file under `name`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FurtherFile {
    pub(crate) name: &'static str,
    pub(crate) url: &'static str,
    pub(crate) bytes: u64,
    pub(crate) sha256: &'static str,
    pub(crate) format: &'static str,
}

/// One installable resource of a model: its own file, any further files, and the capability that
/// installs it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ModelFile {
    pub(crate) resource: &'static str,
    pub(crate) title: &'static str,
    pub(crate) version: &'static str,
    /// A developer fixture's whole content, which [`DeveloperModels`] serves; empty for a
    /// published file.
    pub(crate) bytes: &'static [u8],
    /// Where a published file comes from; `None` for a developer fixture.
    pub(crate) published: Option<Published>,
    pub(crate) sha256: &'static str,
    pub(crate) format: &'static str,
    pub(crate) license: &'static str,
    pub(crate) provenance: &'static str,
    /// The `download-artifact` capability that installs it.
    pub(crate) download: &'static str,
    pub(crate) files: &'static [FurtherFile],
    pub(crate) tradeoff: Option<&'static str>,
    pub(crate) min_memory_bytes: Option<u64>,
    pub(crate) redirect_origins: &'static [&'static str],
}

impl ModelFile {
    pub(crate) fn url(&self) -> String {
        match self.published {
            Some(published) => published.url.into(),
            None => format!(
                "{DEVELOPER_ORIGIN}/models/{}/{}/{}.bin",
                self.resource, self.version, self.resource
            ),
        }
    }

    /// The own file's length.
    pub(crate) fn length(&self) -> u64 {
        self.published
            .map_or(self.bytes.len() as u64, |published| published.bytes)
    }

    pub(crate) fn descriptor(&self) -> ResourceDescriptor {
        ResourceDescriptor {
            id: self.resource.into(),
            title: self.title.into(),
            version: self.version.into(),
            url: self.url(),
            bytes: self.length(),
            sha256: self.sha256.into(),
            format: self.format.into(),
            license: self.license.into(),
            provenance: self.provenance.into(),
            redirect_origins: self
                .redirect_origins
                .iter()
                .map(|origin| (*origin).into())
                .collect(),
            files: self
                .files
                .iter()
                .map(|file| ResourceFile {
                    name: file.name.into(),
                    url: file.url.into(),
                    bytes: file.bytes,
                    sha256: file.sha256.into(),
                })
                .collect(),
            tradeoff: self.tradeoff.map(Into::into),
            use_restriction: None,
            min_memory_bytes: self.min_memory_bytes,
            flag: None,
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

    /// The format of the installed file named `name`: the own file's, or a further file's.
    pub(crate) fn format_of(&self, name: &str) -> Option<&'static str> {
        if name == self.resource {
            return Some(self.format);
        }
        self.files
            .iter()
            .find(|file| file.name == name)
            .map(|file| file.format)
    }
}

/// The `local-runtime` capability that holds a model's text encoder loaded with its generator, for
/// prompts: its resource is the text encoder and it opens with the generator's.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromptRuntime {
    pub(crate) id: &'static str,
    pub(crate) resident_bytes: u64,
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
    /// The capability a prompt runs on, when the generator's own runtime does not load the text
    /// encoder: it holds the encoder with the generator, at its own resident size.
    pub(crate) prompt_runtime: Option<PromptRuntime>,
}

impl GenerativeModel {
    /// The capabilities the model declares: its files' downloads, its generator's runtime and, with
    /// a prompt runtime, the runtime that holds the text encoder with it.
    pub(crate) fn capabilities(&self) -> Vec<CapabilityDescriptor> {
        let runtime = |id: &str, resource: &str, resident_bytes: u64, with: Vec<String>| {
            CapabilityDescriptor {
                id: id.into(),
                kind: CapabilityKind::LocalRuntime {
                    resource: resource.into(),
                    model: ModelKind::Generator,
                    resident_bytes,
                    backends: self.backends.iter().map(|b| (*b).into()).collect(),
                    precision: self.precision,
                    with,
                },
                purpose: format!("Generate fills with {}", self.generator.title),
            }
        };
        let mut capabilities = vec![
            self.generator.capability(),
            runtime(
                self.runtime,
                self.generator.resource,
                self.resident_bytes,
                Vec::new(),
            ),
        ];
        if let Some(encoder) = self.text_encoder {
            capabilities.push(encoder.capability());
            if let Some(prompt) = self.prompt_runtime {
                let mut held = runtime(
                    prompt.id,
                    encoder.resource,
                    prompt.resident_bytes,
                    vec![self.generator.resource.into()],
                );
                held.purpose = format!("Replace with {}", self.generator.title);
                capabilities.push(held);
            }
        }
        capabilities
    }

    /// Every resource of the model.
    pub(crate) fn files(&self) -> impl Iterator<Item = ModelFile> {
        std::iter::once(self.generator).chain(self.text_encoder)
    }

    /// The capabilities a request uses: the generator's runtime for a removal; for a prompt the
    /// prompt runtime, or the generator's with the encoder's install when the generator's runtime
    /// encodes the prompt itself. `None` when the model takes no prompt.
    pub(crate) fn uses(&self, prompt: bool) -> Option<Vec<&'static str>> {
        if !prompt {
            return Some(vec![self.runtime]);
        }
        let encoder = self.text_encoder?;
        Some(match self.prompt_runtime {
            Some(runtime) => vec![runtime.id],
            None => vec![self.runtime, encoder.download],
        })
    }

    /// The `local-runtime` capability a request generates with.
    pub(crate) fn runtime_for(&self, prompt: bool) -> &'static str {
        match self.prompt_runtime {
            Some(runtime) if prompt => runtime.id,
            _ => self.runtime,
        }
    }
}

/// The developer generator's file.
pub(crate) const FAKE_GENERATOR: ModelFile = ModelFile {
    resource: "fake-generator",
    title: "Developer fake generator",
    version: "1",
    bytes: b"Luxforge developer fake generator, version 1.\n",
    published: None,
    sha256: "ed04660c7aef25e09c822083df086a020160d9d8a80a082b37557e5070b65206",
    format: "luxforge-fake-generator",
    license: "GPL-3.0-or-later",
    provenance: "A Luxforge developer fixture: a deterministic stand-in for a generative model, run by the fake inference runtime",
    download: "download-fake-generator",
    files: &[],
    tradeoff: None,
    min_memory_bytes: None,
    redirect_origins: &[],
};

/// The developer text encoder's file.
pub(crate) const FAKE_TEXT_ENCODER: ModelFile = ModelFile {
    resource: "fake-text-encoder",
    title: "Developer fake text encoder",
    version: "1",
    bytes: b"Luxforge developer fake text encoder, version 1.\n",
    published: None,
    sha256: "76f363a0a32ab7362bad24a0f2e2c4f329dd1fa57be4f9c84395542527933fdb",
    format: "luxforge-fake-text-encoder",
    license: "GPL-3.0-or-later",
    provenance: "A Luxforge developer fixture: stands in for a generative model's text encoder, which a prompt needs",
    download: "download-fake-text-encoder",
    files: &[],
    tradeoff: None,
    min_memory_bytes: None,
    redirect_origins: &[],
};

/// The developer model: the fake generator, with its fake text encoder.
pub(crate) const FAKE_MODEL: GenerativeModel = GenerativeModel {
    id: "fake-generator",
    generator: FAKE_GENERATOR,
    runtime: "run-fake-generator",
    resident_bytes: 64 * 1024 * 1024,
    backends: &[crate::inference::fake::FAKE_HOST_BACKEND],
    precision: Precision::Fp32,
    side: 512,
    steps: 8,
    text_encoder: Some(FAKE_TEXT_ENCODER),
    prompt_runtime: Some(PromptRuntime {
        id: "run-fake-generator-replace",
        resident_bytes: 128 * 1024 * 1024,
    }),
};

/// Where Hugging Face redirects a pinned download (its CDN and Xet bridges), and where GitHub
/// redirects a release asset.
const HUGGING_FACE_REDIRECTS: &[&str] = &[
    "https://us.aws.cdn.hf.co",
    "https://eu.aws.cdn.hf.co",
    "https://cas-bridge.xethub.hf.co",
    "https://cdn-lfs.hf.co",
];
const KLEIN_REDIRECTS: &[&str] = &[
    "https://us.aws.cdn.hf.co",
    "https://eu.aws.cdn.hf.co",
    "https://cas-bridge.xethub.hf.co",
    "https://cdn-lfs.hf.co",
    "https://release-assets.githubusercontent.com",
    "https://objects.githubusercontent.com",
];

/// The removal conditioning's release asset: the project publishes it on its GitHub releases under
/// this tag and name (`docs/design/ai-editing.md`, "Prompt-free fill, prompted Replace").
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const CONDITIONING_RELEASE_TAG: &str = "models-generative-1";
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const CONDITIONING_ASSET: &str = "klein-4b-removal-conditioning-ba26d15a.lfsdcond";
pub(crate) const CONDITIONING_URL: &str = "https://github.com/Willyham/luxforge/releases/download/models-generative-1/klein-4b-removal-conditioning-ba26d15a.lfsdcond";

/// FLUX.2 klein 4B's resource: the Q4_0 transformer as its own file, with the VAE, fal's removal
/// LoRA and the removal conditioning.
pub(crate) const KLEIN_GENERATOR: ModelFile = ModelFile {
    resource: "generative-klein-4b",
    title: "FLUX.2 klein 4B with the object removal LoRA",
    version: "klein-4b-q4_0-3b1f5a9d-lora-0e3f5879-cond-ba26d15a",
    bytes: &[],
    published: Some(Published {
        url: "https://huggingface.co/leejet/FLUX.2-klein-4B-GGUF/resolve/3b1f5a9dc3abb32238b053aeb3d823c30afdacbd/flux-2-klein-4b-Q4_0.gguf",
        bytes: 2_460_378_560,
    }),
    sha256: "d1023499ef3f2f82ff7c50e6778495195c1b6cc34835741778868428111f9ff4",
    format: "gguf",
    license: "Apache-2.0",
    provenance: "Black Forest Labs FLUX.2 klein 4B (black-forest-labs/FLUX.2-klein-4B@e7b7dc27), quantised to GGUF Q4_0 by leejet (leejet/FLUX.2-klein-4B-GGUF@3b1f5a9d); fal's object removal LoRA (fal/flux-2-klein-4B-object-remove-lora@0e3f5879, ComfyUI-converted); the removal conditioning computed by Luxforge with Qwen3-4B Q8_0 through the pinned stable-diffusion.cpp",
    download: "download-klein",
    files: &[
        FurtherFile {
            name: "vae.safetensors",
            url: "https://huggingface.co/black-forest-labs/FLUX.2-klein-4B/resolve/e7b7dc27f91deacad38e78976d1f2b499d76a294/vae/diffusion_pytorch_model.safetensors",
            bytes: 168_120_878,
            sha256: "ca70d2202afe6415bdbcb8793ba8cd99fd159cfe6192381504d6c4d3036e0f04",
            format: "safetensors",
        },
        FurtherFile {
            name: "removal-lora.safetensors",
            url: "https://huggingface.co/fal/flux-2-klein-4B-object-remove-lora/resolve/0e3f58790356bf1319b263fc56b333c294b42ff7/kDEkt5q7tDLKOpQJIVMPx_pytorch_lora_weights_comfy_converted.safetensors",
            bytes: 76_038_936,
            sha256: "dc197de62e174863f83fc4052465603f4389de7c3a78e17f3d41a1f6f11488ec",
            format: "safetensors",
        },
        FurtherFile {
            name: "removal-conditioning.lfsdcond",
            url: CONDITIONING_URL,
            bytes: 15_728_691,
            sha256: "ba26d15a8b280abf7f436cbd817d2caaba412b9e186aa6a2163cf6a73c199141",
            format: "lfsdcond-1",
        },
    ],
    tradeoff: Some("Rebuilds hidden detail; about 2 minutes a fill on an M-series Mac"),
    min_memory_bytes: Some(16 * GIB),
    redirect_origins: KLEIN_REDIRECTS,
};

/// klein's text encoder for Replace: Qwen3-4B, Q4_K_M, with its tokenizer.
pub(crate) const KLEIN_TEXT_ENCODER: ModelFile = ModelFile {
    resource: "generative-klein-text-encoder",
    title: "Text encoder for Replace (Qwen3-4B)",
    version: "qwen3-4b-q4_k_m-22c9fc8a-tokenizer-e7b7dc27",
    bytes: &[],
    published: Some(Published {
        url: "https://huggingface.co/unsloth/Qwen3-4B-GGUF/resolve/22c9fc8a8c7700b76a1789366280a6a5a1ad1120/Qwen3-4B-Q4_K_M.gguf",
        bytes: 2_497_281_312,
    }),
    sha256: "f6f851777709861056efcdad3af01da38b31223a3ba26e61a4f8bf3a2195813a",
    format: "gguf",
    license: "Apache-2.0",
    provenance: "Alibaba Qwen3-4B, quantised to GGUF Q4_K_M by Unsloth (unsloth/Qwen3-4B-GGUF@22c9fc8a); tokenizer from black-forest-labs/FLUX.2-klein-4B@e7b7dc27",
    download: "download-klein-text-encoder",
    files: &[FurtherFile {
        name: "tokenizer.json",
        url: "https://huggingface.co/black-forest-labs/FLUX.2-klein-4B/resolve/e7b7dc27f91deacad38e78976d1f2b499d76a294/tokenizer/tokenizer.json",
        bytes: 11_422_654,
        sha256: "aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4",
        format: "json",
    }],
    tradeoff: Some(
        "Needed only to describe what Replace puts in the region; needs 24 GB of memory",
    ),
    min_memory_bytes: Some(24 * GIB),
    redirect_origins: HUGGING_FACE_REDIRECTS,
};

/// The backends `luxforge-diffusion` serves klein under: Metal on macOS, the CPU elsewhere.
pub(crate) const DIFFUSION_BACKENDS: &[&str] = &["metal", "diffusion-cpu"];

/// The qualified model (owner, 2026-10-11): 1024 px, four flow steps, one candidate at a time;
/// about 5 GiB resident for a removal and 9 GiB with the text encoder for Replace, as measured.
pub(crate) const KLEIN: GenerativeModel = GenerativeModel {
    id: "generative-klein-4b",
    generator: KLEIN_GENERATOR,
    runtime: "run-klein",
    resident_bytes: 5 * GIB,
    backends: DIFFUSION_BACKENDS,
    precision: Precision::Mixed,
    side: 1024,
    steps: 4,
    text_encoder: Some(KLEIN_TEXT_ENCODER),
    prompt_runtime: Some(PromptRuntime {
        id: "run-klein-replace",
        resident_bytes: 9 * GIB,
    }),
};

/// The models an ordinary run offers.
pub(crate) const PRODUCT_MODELS: &[GenerativeModel] = &[KLEIN];

/// The models a developer run offers: the fake first, then the qualified model.
pub(crate) const DEVELOPER_MODELS: &[GenerativeModel] = &[FAKE_MODEL, KLEIN];

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
        [FAKE_GENERATOR, FAKE_TEXT_ENCODER]
            .into_iter()
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
