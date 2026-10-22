//! The analysis models `luxforge.ai` offers: each is one installable resource and the
//! `local-runtime` capability that holds it as a tensor session, with the role it plays.
//!
//! **The qualified segmenter** is EfficientViT-SAM L0 from mit-han-lab, the prototype's pinned
//! segmenter (`probes/object-removal`), as two resources because its encoder and decoder are two
//! sessions and a `local-runtime` capability holds one: the image encoder and the prompt decoder,
//! both pinned at the publisher's revision `a2f0c592`, length and SHA-256. Every run offers it.
//! The scene models (the semantic model, the salient-subject model and the people detector) and the
//! depth model are not qualified yet (TASK-004 of `tasks/editing/ai-editing.json`), so an ordinary
//! run offers none and declares no `task.analyse-scene` or `task.analyse-depth`.
//!
//! **The developer models** are the fake runtime's analysis models
//! ([`crate::inference::fake_analysis`]): a small pinned file per role whose first line names the
//! fake it opens as, under the reserved `.invalid` origin that [`super::super::DeveloperModels`]
//! answers. A developer run offers them in place of the qualified segmenter, so every analysis
//! task, the picker and the selection task run end to end without a model.
use crate::{
    capabilities::descriptor::{
        CapabilityDescriptor, CapabilityKind, Introduction, IntroductionPicture, IntroductionStep,
        ModelKind, ResourceDescriptor,
    },
    inference::Precision,
    modules::ai::models::{ModelFile, Published},
};

const MIB: u64 = 1024 * 1024;

/// What a model does in an analysis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    SegmenterEncoder,
    SegmenterDecoder,
    Semantic,
    Subject,
    People,
    Depth,
}

/// One analysis model: its file, the capability that holds it and how.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionModel {
    pub(crate) role: Role,
    pub(crate) file: ModelFile,
    /// The `local-runtime` capability that holds it loaded.
    pub(crate) runtime: &'static str,
    pub(crate) resident_bytes: u64,
    pub(crate) backends: &'static [&'static str],
    pub(crate) precision: Precision,
    /// A semantic model's classes, in its output's order.
    pub(crate) classes: &'static [&'static str],
}

impl SessionModel {
    /// Its download and its runtime.
    pub(crate) fn capabilities(&self) -> [CapabilityDescriptor; 2] {
        [
            self.file.capability(),
            CapabilityDescriptor {
                id: self.runtime.into(),
                kind: CapabilityKind::LocalRuntime {
                    resource: self.file.resource.into(),
                    model: ModelKind::Session,
                    resident_bytes: self.resident_bytes,
                    backends: self.backends.iter().map(|b| (*b).into()).collect(),
                    precision: self.precision,
                    with: Vec::new(),
                },
                purpose: format!("Analyse photos with {}", self.file.title),
            },
        ]
    }

    /// Its resource, offered only while `flag` is on.
    pub(crate) fn descriptor(&self, flag: &str) -> ResourceDescriptor {
        ResourceDescriptor {
            flag: Some(flag.into()),
            ..self.file.descriptor()
        }
    }
}

/// The analysis models of one run, by role.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AnalysisModels {
    pub(crate) models: &'static [SessionModel],
}

impl AnalysisModels {
    /// The model that plays `role`, when the run offers one.
    pub(crate) fn of(&self, role: Role) -> Option<&'static SessionModel> {
        self.models.iter().find(|model| model.role == role)
    }

    /// The models that play every one of `roles`, in that order; `None` unless all are offered.
    pub(crate) fn set(&self, roles: &[Role]) -> Option<Vec<&'static SessionModel>> {
        roles.iter().map(|role| self.of(*role)).collect()
    }
}

/// The segmenter's two roles, which `task.analyse`, the picker and `task.select-object` use.
pub(crate) const SEGMENTER: [Role; 2] = [Role::SegmenterEncoder, Role::SegmenterDecoder];
/// The scene's three roles, which `task.analyse-scene` runs.
pub(crate) const SCENE: [Role; 3] = [Role::Semantic, Role::Subject, Role::People];
/// The depth model, which `task.analyse-depth` runs.
pub(crate) const DEPTH: [Role; 1] = [Role::Depth];

/// A segmenter's install purpose: picking objects.
const SEGMENTER_PURPOSE: &str = "Set up object picking: this downloads the model that finds the \
    object under the pointer. It runs on this computer, so your photos stay on it";
/// A scene model's install purpose: Select's automatic masks.
const SCENE_PURPOSE: &str = "Set up Select: this downloads a model that finds the sky, the subject \
    and people in a photo. It runs on this computer, so your photos stay on it";

/// What setting up the segmenter says of the feature.
fn segmenter_introduction() -> Introduction {
    let step = |title: &str, body: &str, picture| IntroductionStep {
        title: title.into(),
        body: body.into(),
        picture,
    };
    Introduction {
        eyebrow: "AI editing".into(),
        title: "Pick an object by pointing at it".into(),
        lead: "Hold the pointer over something in the photo and Luxforge outlines the whole \
            object, ready to remove or to mask. It runs entirely on this computer, so your photos \
            never leave it."
            .into(),
        hero: None,
        steps: vec![
            step(
                "Point at it",
                "Move over the photo: the object under the pointer is outlined as you go.",
                IntroductionPicture::PaintOver,
            ),
            step(
                "Click or paint",
                "Click to choose it, or paint across several and choose among them.",
                IntroductionPicture::Candidates,
            ),
        ],
        promise: "Your photos stay on this computer.".into(),
    }
}

/// What setting up a scene or depth model says of the feature.
fn scene_introduction() -> Introduction {
    Introduction {
        eyebrow: "AI editing".into(),
        title: "Masks that find what is in the photo".into(),
        lead: "Select finds the sky, the subject and people in a photo so a mask can follow them. \
            It runs entirely on this computer, so your photos never leave it."
            .into(),
        hero: Some(IntroductionPicture::PaintOver),
        steps: Vec::new(),
        promise: "Your photos stay on this computer.".into(),
    }
}

/// Where Hugging Face redirects a pinned download.
const HUGGING_FACE_REDIRECTS: &[&str] = &[
    "https://us.aws.cdn.hf.co",
    "https://eu.aws.cdn.hf.co",
    "https://cas-bridge.xethub.hf.co",
    "https://cdn-lfs.hf.co",
];

/// The backends the ONNX runtime serves a session under: Core ML on macOS, DirectML on Windows,
/// the CPU everywhere.
const ONNX_BACKENDS: &[&str] = &["coreml", "directml", "cpu"];

/// EfficientViT-SAM L0's version: the publisher's revision of both files.
const L0_VERSION: &str = "efficientvit-sam-l0-a2f0c592";
const L0_PROVENANCE: &str = "MIT Han Lab EfficientViT-SAM L0, ONNX export as published in \
    mit-han-lab/efficientvit-sam@a2f0c592 (onnx/l0_encoder.onnx and onnx/l0_decoder.onnx)";

/// EfficientViT-SAM L0's image encoder.
pub(crate) const L0_ENCODER: SessionModel = SessionModel {
    role: Role::SegmenterEncoder,
    file: ModelFile {
        resource: "segmenter-l0-encoder",
        title: "Object picking: image encoder (EfficientViT-SAM L0)",
        purpose: SEGMENTER_PURPOSE,
        introduction: segmenter_introduction,
        version: L0_VERSION,
        bytes: &[],
        published: Some(Published {
            url: "https://huggingface.co/mit-han-lab/efficientvit-sam/resolve/a2f0c5929196a13ef1ffc9338d3f9e482e1e0e68/onnx/l0_encoder.onnx",
            bytes: 123_112_917,
        }),
        sha256: "7a37ebd16e65c31253416e33897e7e6b07699127765d538e99ecbfaa3ad1e727",
        format: "onnx",
        license: "Apache-2.0",
        provenance: L0_PROVENANCE,
        download: "download-segmenter-l0-encoder",
        files: &[],
        tradeoff: Some("Finds the object under the pointer; about 120 MB"),
        min_memory_bytes: None,
        redirect_origins: HUGGING_FACE_REDIRECTS,
    },
    runtime: "run-segmenter-l0-encoder",
    resident_bytes: 384 * MIB,
    backends: ONNX_BACKENDS,
    precision: Precision::Fp32,
    classes: &[],
};

/// EfficientViT-SAM L0's prompt decoder.
pub(crate) const L0_DECODER: SessionModel = SessionModel {
    role: Role::SegmenterDecoder,
    file: ModelFile {
        resource: "segmenter-l0-decoder",
        title: "Object picking: prompt decoder (EfficientViT-SAM L0)",
        purpose: SEGMENTER_PURPOSE,
        introduction: segmenter_introduction,
        version: L0_VERSION,
        bytes: &[],
        published: Some(Published {
            url: "https://huggingface.co/mit-han-lab/efficientvit-sam/resolve/a2f0c5929196a13ef1ffc9338d3f9e482e1e0e68/onnx/l0_decoder.onnx",
            bytes: 16_470_362,
        }),
        sha256: "053e72c5703736a8a8a92482c9990c4a26dde9dcb7ff26b05843cf0bc433f42f",
        format: "onnx",
        license: "Apache-2.0",
        provenance: L0_PROVENANCE,
        download: "download-segmenter-l0-decoder",
        files: &[],
        tradeoff: Some("Outlines the object the encoder found; about 16 MB"),
        min_memory_bytes: None,
        redirect_origins: HUGGING_FACE_REDIRECTS,
    },
    runtime: "run-segmenter-l0-decoder",
    resident_bytes: 64 * MIB,
    backends: ONNX_BACKENDS,
    precision: Precision::Fp32,
    classes: &[],
};

/// One developer fake's model: a pinned file whose first line names the fake, installed by
/// `download-<resource>` and held by `run-<resource>`.
#[allow(clippy::too_many_arguments)]
const fn fake(
    role: Role,
    [resource, download, runtime]: [&'static str; 3],
    title: &'static str,
    purpose: &'static str,
    introduction: fn() -> Introduction,
    bytes: &'static [u8],
    sha256: &'static str,
    classes: &'static [&'static str],
) -> SessionModel {
    SessionModel {
        role,
        file: ModelFile {
            resource,
            title,
            purpose,
            introduction,
            version: "1",
            bytes,
            published: None,
            sha256,
            format: "luxforge-fake-analysis",
            license: "GPL-3.0-or-later",
            provenance: "A Luxforge developer fixture: a deterministic stand-in for an analysis model, run by the fake inference runtime",
            download,
            files: &[],
            tradeoff: None,
            min_memory_bytes: None,
            redirect_origins: &[],
        },
        runtime,
        resident_bytes: 16 * MIB,
        backends: &[crate::inference::fake::FAKE_HOST_BACKEND],
        precision: Precision::Fp32,
        classes,
    }
}

/// The fake semantic model's classes, in its output's order.
pub(crate) const FAKE_CLASSES: &[&str] = &["sky", "vegetation", "person", "other"];

pub(crate) const FAKE_SEGMENTER_ENCODER: SessionModel = fake(
    Role::SegmenterEncoder,
    [
        "fake-segmenter-encoder",
        "download-fake-segmenter-encoder",
        "run-fake-segmenter-encoder",
    ],
    "Developer fake segmenter encoder",
    SEGMENTER_PURPOSE,
    segmenter_introduction,
    b"LFFAKE1 segmenter-encoder\nLuxforge developer fake segmenter encoder, version 1.\n",
    "f0e5823d04812120d2ef9796eb3c2d4502c882542c7f8e306eacfd2bf65fe6fa",
    &[],
);
pub(crate) const FAKE_SEGMENTER_DECODER: SessionModel = fake(
    Role::SegmenterDecoder,
    [
        "fake-segmenter-decoder",
        "download-fake-segmenter-decoder",
        "run-fake-segmenter-decoder",
    ],
    "Developer fake segmenter decoder",
    SEGMENTER_PURPOSE,
    segmenter_introduction,
    b"LFFAKE1 segmenter-decoder\nLuxforge developer fake segmenter decoder, version 1.\n",
    "b9c2e8b6a9ed7520cc72b3074f5082544811b08d05fd5b4402ee5b9168b3058a",
    &[],
);
pub(crate) const FAKE_SEMANTIC: SessionModel = fake(
    Role::Semantic,
    [
        "fake-semantic",
        "download-fake-semantic",
        "run-fake-semantic",
    ],
    "Developer fake semantic model",
    SCENE_PURPOSE,
    scene_introduction,
    b"LFFAKE1 semantic\nLuxforge developer fake semantic model, version 1.\n",
    "96e7da45c5a49c27cae1b3c1dfbcdf7adb0db92d71f9fe6d72184aa7ea74a51a",
    FAKE_CLASSES,
);
pub(crate) const FAKE_SUBJECT: SessionModel = fake(
    Role::Subject,
    ["fake-subject", "download-fake-subject", "run-fake-subject"],
    "Developer fake salient-subject model",
    SCENE_PURPOSE,
    scene_introduction,
    b"LFFAKE1 subject\nLuxforge developer fake salient-subject model, version 1.\n",
    "1d8f08df28c0ed4cb9df647f68a89d8d21d5b82c48b5f4f6bcd5c2796dba28cb",
    &[],
);
pub(crate) const FAKE_PEOPLE: SessionModel = fake(
    Role::People,
    ["fake-people", "download-fake-people", "run-fake-people"],
    "Developer fake people detector",
    SCENE_PURPOSE,
    scene_introduction,
    b"LFFAKE1 people\nLuxforge developer fake people detector, version 1.\n",
    "e026a362af7d5031c647f77d898caf3c62cc5f9ccb2c6ca8e3e85413665e093f",
    &[],
);
pub(crate) const FAKE_DEPTH: SessionModel = fake(
    Role::Depth,
    ["fake-depth", "download-fake-depth", "run-fake-depth"],
    "Developer fake depth model",
    SCENE_PURPOSE,
    scene_introduction,
    b"LFFAKE1 depth\nLuxforge developer fake depth model, version 1.\n",
    "79c59458a4c2f7b90f987c8fb2a6054e8ebfa27bcd38fc5f511ac80c3ab41cc4",
    &[],
);

/// The analysis models an ordinary run offers: the qualified segmenter.
pub(crate) const PRODUCT_ANALYSIS: AnalysisModels = AnalysisModels {
    models: &[L0_ENCODER, L0_DECODER],
};

/// The analysis models a developer run offers: every fake.
pub(crate) const DEVELOPER_ANALYSIS: AnalysisModels = AnalysisModels {
    models: &[
        FAKE_SEGMENTER_ENCODER,
        FAKE_SEGMENTER_DECODER,
        FAKE_SEMANTIC,
        FAKE_SUBJECT,
        FAKE_PEOPLE,
        FAKE_DEPTH,
    ],
};
