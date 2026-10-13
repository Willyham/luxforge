//! The `model` component kind: a model selection (`docs/design/ai-editing.md#the-model-kind`), a
//! stored 8-bit alpha raster a task computed once and published as an artifact, which this kind
//! samples, thresholds and feathers. It never runs a model: the raster is what renders, so a
//! selection survives the model's removal, a change of runtime and another machine.
//!
//! This file is the production transcription of `model_alpha` and `model_coverage` of the
//! independent `f64` reference at `crates/luxforge-reference/src/mask.rs`, in the reference's
//! expressions and order, so coverage is bit-identical to it: `kx`, `ky`, `t`, `lo`, `span` and
//! `hard` are hoisted to compile time, and the per-pixel path is the bilinear read, one division by
//! 255 and the frozen `smooth`.
//!
//! **Position-based.** The raster is stretched over the whole content stage — texel `(i, j)` has
//! its centre at the stored position `((i + 0.5) / width, (j + 0.5) / height)` — and coverage is a
//! function of the pixel's position alone, so it ignores the pixel it is handed, its bounds are the
//! raster's non-zero rectangle and its smallest feature is the raster's texel scaled by the band
//! the threshold and feather leave. The overlay, the proxy rule and `render.sample` treat it like a
//! gradient.
//!
//! **Bound at compile time.** The payload names its artifact; the recipe's bound artifact table
//! ([`crate::Recipe::artifacts`]) carries the verified bytes, which the kind table's binding hands
//! here. An artifact the recipe was not bound with is refused by name, never skipped, and an
//! artifact that is not an alpha raster of its declared size is refused naming the component.
//!
//! **Kept once per artifact.** The raster's non-zero rectangle and its GPU block are derived from
//! the bytes on first use and kept with the prepared artifact
//! ([`crate::PreparedArtifact::derived`]), so compiling a model component on every preview is
//! `O(1)` after the first, and the GPU block is one shared allocation whose `Arc` every plan holds,
//! which the hosts' block uploads recognise and do not copy again.
use super::{
    Binding, ComponentField, Field,
    parameters::{check_declared, percentage},
    region_from_bounds, smooth,
};
use crate::{
    ArtifactId, Component, Error, ParameterDescriptor, PreparedArtifact,
    modules::{Region, Stage},
    render::gpu::{GpuDescription, GpuProgram, GpuProgramKind},
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, LazyLock, OnceLock};

/// The token a stored component of this kind carries.
pub(super) const KIND: &str = "model";

/// The kind a selection's alpha artifact declares.
pub const ALPHA_KIND: &str = "mask-alpha";

/// The longest side a selection's alpha raster may have: 4096 px, so one raster is at most 16 MiB
/// (`docs/design/masking-workspace.md`, the artifact cap).
pub const ALPHA_SIDE_MAX: u32 = 4096;

/// The legal range of a stored `threshold`, a percentage of full alpha. The floor keeps the hard
/// edge's threshold above zero, so a zero alpha is zero coverage at every setting.
pub const THRESHOLD_MIN: f64 = 1.0;
pub const THRESHOLD_MAX: f64 = 100.0;
pub const THRESHOLD_DEFAULT: f64 = 50.0;

/// The legal range of a stored `feather`, a percentage of the widest band the threshold allows.
pub const FEATHER_MIN: f64 = 0.0;
pub const FEATHER_MAX: f64 = 100.0;
pub const FEATHER_DEFAULT: f64 = 100.0;

/// The longest class, model identity or version a component records.
const LABEL_MAX: usize = 64;
const VERSION_MAX: usize = 128;

/// The model selection's GPU coverage program (`model.wgsl`): the bilinear read of the raster
/// packed four codes to a word in the program's storage block.
pub(crate) static PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_mask_model",
    source: include_str!("model.wgsl"),
    kind: GpuProgramKind::Coverage,
    words: 9,
    enabled: true,
};

/// A model component's stored payload: the artifact it samples, what produced it, and the two
/// numbers that shape it.
///
/// `class`, `model`, `model_version`, `input_fingerprint` and `prefix_hash` are recorded and never
/// read by coverage: they are what `mask.list` reports and what staleness compares
/// (`input_fingerprint` the source identity, `prefix_hash` the repair stage's output identity, both
/// lowercase hex SHA-256).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    pub artifact: ArtifactId,
    pub class: String,
    pub model: String,
    pub model_version: String,
    pub input_fingerprint: String,
    pub prefix_hash: String,
    pub threshold: f64,
    pub feather: f64,
}

/// What this kind declares to the API, and the whole of what makes `mask.create-model`,
/// `mask.add-model` and `mask.set-model`: the artifact by identity, the provenance as text and the
/// two shaping numbers with their defaults. The artifact has no default, so no button offers the
/// kind; a task and `mask.set-model` set it.
pub(super) fn parameters(required: bool) -> Vec<ParameterDescriptor> {
    let digest = |name: &str, notes: &str| {
        ParameterDescriptor::string(name, 64)
            .required(required)
            .notes(notes)
    };
    vec![
        ParameterDescriptor::artifact("artifact")
            .required(required)
            .notes(
                "the selection's alpha raster: a published mask-alpha artifact of at most 4096 px \
                 a side, stretched over the content stage",
            ),
        ParameterDescriptor::string("class", LABEL_MAX)
            .required(required)
            .notes("what the model selected, such as sky or subject"),
        ParameterDescriptor::string("model", LABEL_MAX)
            .required(required)
            .notes("the model resource that computed the selection"),
        ParameterDescriptor::string("model_version", VERSION_MAX)
            .required(required)
            .notes("the model resource's version when the selection was computed"),
        digest(
            "input_fingerprint",
            "the source identity the selection was computed from, as lowercase hex SHA-256",
        ),
        digest(
            "prefix_hash",
            "the repair stage's output identity the selection was computed from, as lowercase hex \
             SHA-256",
        ),
        percentage(
            "threshold",
            required,
            THRESHOLD_MIN,
            THRESHOLD_MAX,
            THRESHOLD_DEFAULT,
            "the alpha at the middle of the edge, as a percentage of full alpha",
        )
        .default(THRESHOLD_DEFAULT),
        percentage(
            "feather",
            required,
            FEATHER_MIN,
            FEATHER_MAX,
            FEATHER_DEFAULT,
            "how soft the edge is, as a percentage of the widest band around the threshold; 0 is a \
             hard edge, and 100 at a threshold of 50 keeps the model's own soft edge",
        )
        .default(FEATHER_DEFAULT),
    ]
}

/// [`parameters`], declared once for the parser to check a stored payload against.
static DECLARED: LazyLock<Vec<ParameterDescriptor>> = LazyLock::new(|| parameters(true));

fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Parse and range-check one stored `model` payload, without a stage or the artifact's bytes.
pub(super) fn parse(component: &Component) -> Result<ModelSelection, Error> {
    let selection: ModelSelection =
        serde_json::from_value(component.payload.clone()).map_err(|error| {
            Error::validation(format!(
                "component {} has an invalid {KIND} payload: {error}",
                component.name
            ))
        })?;
    check_declared(component, KIND, &DECLARED, &component.payload)?;
    for (field, value) in [
        ("input_fingerprint", &selection.input_fingerprint),
        ("prefix_hash", &selection.prefix_hash),
    ] {
        if !is_digest(value) {
            return Err(Error::validation(format!(
                "component {} {KIND} parameter {field} must be a lowercase hex SHA-256",
                component.name
            )));
        }
    }
    Ok(selection)
}

/// The artifact one stored `model` component references, when its payload names one.
pub(super) fn artifact_of(component: &Component) -> Option<ArtifactId> {
    component
        .payload
        .get("artifact")
        .and_then(|value| value.as_str())
        .and_then(|text| ArtifactId::parse(text).ok())
}

/// Whether artifact metadata describes a selection's alpha raster: kind [`ALPHA_KIND`], both
/// dimensions present and within [`ALPHA_SIDE_MAX`], and `width · height` bytes. The refusal names
/// what is wrong; the caller names the component.
pub fn check_alpha(
    kind: &str,
    width: Option<u32>,
    height: Option<u32>,
    bytes: u64,
) -> Result<(u32, u32), String> {
    if kind != ALPHA_KIND {
        return Err(format!("is a {kind} artifact, not a {ALPHA_KIND} raster"));
    }
    let (Some(width), Some(height)) = (width, height) else {
        return Err("declares no dimensions".to_owned());
    };
    if width == 0 || height == 0 || width > ALPHA_SIDE_MAX || height > ALPHA_SIDE_MAX {
        return Err(format!(
            "is {width}x{height}, outside 1..={ALPHA_SIDE_MAX} px a side"
        ));
    }
    if bytes != u64::from(width) * u64::from(height) {
        return Err(format!(
            "holds {bytes} bytes for a {width}x{height} raster"
        ));
    }
    Ok((width, height))
}

/// What a raster's bytes give once: its non-zero rectangle in texels `[x0, x1] × [y0, y1]`
/// (inclusive), and its GPU block, packed on first need.
pub(crate) struct AlphaRaster {
    nonzero: Option<[u32; 4]>,
    block: OnceLock<Arc<[u32]>>,
}

impl AlphaRaster {
    fn of(artifact: &PreparedArtifact, width: u32) -> Self {
        let mut nonzero: Option<[u32; 4]> = None;
        for (j, row) in artifact.bytes.chunks_exact(width as usize).enumerate() {
            let Some(first) = row.iter().position(|&code| code != 0) else {
                continue;
            };
            let last = row.iter().rposition(|&code| code != 0).unwrap_or(first);
            let (first, last, j) = (first as u32, last as u32, j as u32);
            nonzero = Some(match nonzero {
                None => [first, j, last, j],
                Some([x0, y0, x1, _]) => [x0.min(first), y0, x1.max(last), j],
            });
        }
        Self {
            nonzero,
            block: OnceLock::new(),
        }
    }

    /// Four codes a word, little end first: code `k` is bits `8·(k mod 4)` of word `k / 4`.
    fn block(&self, bytes: &[u8]) -> Arc<[u32]> {
        self.block
            .get_or_init(|| {
                bytes
                    .chunks(4)
                    .map(|chunk| {
                        let mut word = [0u8; 4];
                        word[..chunk.len()].copy_from_slice(chunk);
                        u32::from_le_bytes(word)
                    })
                    .collect()
            })
            .clone()
    }
}

/// One model selection bound to a stage and its raster, with every term that does not depend on
/// the pixel already computed.
#[derive(Debug)]
pub(super) struct Compiled {
    artifact: Arc<PreparedArtifact>,
    raster: Arc<AlphaRaster>,
    width: u32,
    height: u32,
    /// `width · H / W` and `height`: mask space to texel coordinates.
    kx: f64,
    ky: f64,
    /// `threshold / 100`, and the band's start and width.
    t: f64,
    lo: f64,
    span: f64,
    /// The band has no width: a hard edge at `t`, so `span` is never divided by.
    hard: bool,
}

impl std::fmt::Debug for AlphaRaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlphaRaster")
            .field("nonzero", &self.nonzero)
            .finish()
    }
}

impl Compiled {
    fn code(&self, i: usize, j: usize) -> f64 {
        f64::from(self.artifact.bytes[j * self.width as usize + i])
    }

    /// The raster at mask-space `(u, v)`, bilinear with the edges clamped, as a fraction.
    fn alpha(&self, u: f64, v: f64) -> f64 {
        let ax = u * self.kx - 0.5;
        let ay = v * self.ky - 0.5;
        let x0 = ax.floor();
        let y0 = ay.floor();
        let fx = ax - x0;
        let fy = ay - y0;
        let last_x = i64::from(self.width) - 1;
        let last_y = i64::from(self.height) - 1;
        let i0 = (x0 as i64).clamp(0, last_x) as usize;
        let i1 = (x0 as i64 + 1).clamp(0, last_x) as usize;
        let j0 = (y0 as i64).clamp(0, last_y) as usize;
        let j1 = (y0 as i64 + 1).clamp(0, last_y) as usize;
        let top = self.code(i0, j0) + fx * (self.code(i1, j0) - self.code(i0, j0));
        let bottom = self.code(i0, j1) + fx * (self.code(i1, j1) - self.code(i0, j1));
        (top + fy * (bottom - top)) / 255.0
    }
}

impl ComponentField for Compiled {
    /// ```text
    /// a = alpha(u, v)
    /// c = if hard { if a >= t { 1 } else { 0 } }
    ///     else    { smooth(clamp((a - lo) / span, 0, 1)) }
    /// ```
    fn coverage(&self, u: f64, v: f64, _rgb: [f64; 3]) -> f64 {
        let a = self.alpha(u, v);
        if self.hard {
            if a >= self.t { 1.0 } else { 0.0 }
        } else {
            smooth(((a - self.lo) / self.span).clamp(0.0, 1.0))
        }
    }

    fn reads_pixels(&self) -> bool {
        false
    }

    /// The raster's non-zero rectangle widened by the bilinear read's reach: coverage is exactly
    /// zero wherever all four texels a position reads are zero, because a zero alpha is zero
    /// coverage at every threshold and feather (`lo >= 0`, and the hard edge's `t >= 0.01`). A
    /// position reads texels `floor(ax)` and `floor(ax) + 1`, so it can reach the rectangle
    /// `[x0, x1]` only while `x0 - 1 < ax < x1 + 1`; in stage pixels that is
    /// `px = (ax + 0.5) · H / kx - 0.5`. Inverted, coverage is non-zero outside it, so the whole
    /// stage; an all-zero raster selects nothing.
    fn support(&self, stage: Stage, inverted: bool) -> Region {
        if inverted {
            return Region::whole(stage);
        }
        let Some([x0, y0, x1, y1]) = self.raster.nonzero else {
            return Region::EMPTY;
        };
        let height = f64::from(stage.height);
        let to_x = |ax: f64| (ax + 0.5) * height / self.kx - 0.5;
        let to_y = |ay: f64| (ay + 0.5) * height / self.ky - 0.5;
        let slack = |value: f64| 16.0 * f64::EPSILON * (value.abs() + 1.0);
        let min_x = to_x(f64::from(x0) - 1.0);
        let max_x = to_x(f64::from(x1) + 1.0);
        let min_y = to_y(f64::from(y0) - 1.0);
        let max_y = to_y(f64::from(y1) + 1.0);
        region_from_bounds(
            stage,
            min_x - slack(min_x),
            max_x + slack(max_x),
            min_y - slack(min_y),
            max_y + slack(max_y),
        )
    }

    /// The narrowest transition the raster can draw: one texel, in `stage`'s pixels on the finer
    /// axis, crossed by the band `[lo, lo + span]` of a full step of alpha. A hard edge has no ramp
    /// and answers `0`.
    fn feature_px(&self, stage: Stage) -> f64 {
        let texel = (f64::from(stage.width) / f64::from(self.width))
            .min(f64::from(stage.height) / f64::from(self.height));
        if self.hard { 0.0 } else { self.span * texel }
    }

    /// The stage's height and the compiled terms narrowed to `f32`, the hard edge and the
    /// raster's dimensions, over the packed raster as the program's block.
    fn gpu(&self, stage: Stage) -> Option<GpuDescription> {
        let mut words: Vec<u32> = [
            f64::from(stage.height),
            self.kx,
            self.ky,
            self.t,
            self.lo,
            self.span,
        ]
        .iter()
        .map(|word| (*word as f32).to_bits())
        .collect();
        words.push(u32::from(self.hard));
        words.push(self.width);
        words.push(self.height);
        let block = self.raster.block(&self.artifact.bytes);
        Some(GpuDescription::new(&PROGRAM, words).with_block(block))
    }
}

/// This kind's row of the kind table: a stored payload checked without a stage or its bytes.
pub(super) fn validate(component: &Component) -> Result<(), Error> {
    parse(component).map(|_| ())
}

/// This kind's row of the kind table: the payload parsed, its artifact found in the recipe's bound
/// table and checked as an alpha raster, and both bound to the stage.
pub(super) fn compile(component: &Component, binding: &Binding<'_>) -> Result<Field, Error> {
    let stored = parse(component)?;
    let artifact = binding.artifacts.get(&stored.artifact).ok_or_else(|| {
        Error::source_unavailable(format!(
            "artifact {} of component {} of mask {} is not bound to this recipe",
            stored.artifact, component.name, binding.mask
        ))
    })?;
    let (width, height) = check_alpha(
        &artifact.kind,
        artifact.width,
        artifact.height,
        artifact.bytes.len() as u64,
    )
    .map_err(|reason| {
        Error::validation(format!(
            "artifact {} of component {} {reason}",
            stored.artifact, component.name
        ))
    })?;
    let raster = artifact.derived(|artifact| AlphaRaster::of(artifact, width));
    let stage = binding.stage;
    let kx = f64::from(width) * f64::from(stage.height) / f64::from(stage.width);
    let ky = f64::from(height);
    let t = stored.threshold / 100.0;
    let w = (stored.feather / 100.0) * t.min(1.0 - t);
    let lo = t - w;
    let span = w + w;
    Ok(Arc::new(Compiled {
        artifact: artifact.clone(),
        raster,
        width,
        height,
        kx,
        ky,
        t,
        lo,
        span,
        hard: span == 0.0,
    }))
}
