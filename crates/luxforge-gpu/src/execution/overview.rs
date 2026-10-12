//! The high-zoom minimap's overview on the GPU (`docs/design/minimap.md`, "Overview rendering and
//! lifetime"): a navigation thumbnail of the whole stage, at most 256 × 256, which a surface holds
//! beside its picture at rest rather than inside it.
//!
//! - **Made by the picture at rest.** A picture at rest handed with an overview ([`RestOverview`])
//!   reduces each of its final tiles a second time, into an accumulator of the overview's size, as
//!   it reduces them to the view ([`super::rest`]); after the last tile the accumulator is quantized
//!   into the overview's own texture, which then leaves the picture at rest for the surface's
//!   [`OverviewStore`]. The view's reduction, the counts and the tiles are as they were: no tile is
//!   drawn for the overview alone, and nothing is uploaded for it.
//! - **Or uploaded once.** Where the reference renderer drew the stage, the desktop reduces the
//!   frame it already retains on a worker and hands the codes here ([`OverviewUpload`]); the
//!   surface uploads them into a texture of their size once, keyed by version.
//! - **One current, one replacement.** The desktop names the versions it may still show
//!   ([`OverviewWant`]): the settled one it keeps and the one it awaits. A surface holds a finished
//!   overview of those versions alone, at most two, and retires any other as its frame is prepared;
//!   a picture at rest's overview in progress is the replacement until it finishes. Handed no want,
//!   the surface lets every overview go: another photograph, a view below 200%, Develop left.
//! - **Bounded.** Every overview allocation — accumulator, coverage tables and parameters, finished
//!   textures and their placement uniforms, current, replacement and retiring alike — is charged to
//!   the pipeline's overview ledger within [`OVERVIEW_GPU_BUDGET`] and to the GPU-preview budget
//!   before it is created, and discharged when the retirement worker drops it. An overview that
//!   cannot be reserved is refused by version ([`OverviewFigures::refused`]); the picture at rest,
//!   its counts and every overview already held are untouched.
use super::{GpuFallback, Held, OUTPUT_FORMAT, RestReduction, SAMPLED_FORMAT};
use crate::{Executor, Output, OutputTile, State, UNIFORM_SIZE};
use std::sync::Arc;

/// The most the minimap overview's GPU storage takes together on one pipeline, resident, in
/// progress and retiring: 4 MiB inside the GPU-preview budget (the core's `OVERVIEW_GPU_BYTES`,
/// which the picture at rest's share leaves free at 100% and above).
pub const OVERVIEW_GPU_BUDGET: u64 = 4 << 20;

/// The overview a picture at rest reduces its final tiles to beside the view: plain data, as the
/// tiles are. `version` is the desktop's, the one its [`OverviewWant`] names.
#[derive(Clone, Debug, PartialEq)]
pub struct RestOverview {
    pub version: u64,
    pub reduction: RestReduction,
}

/// What a surface may keep of the overview, handed with every frame that draws the photograph at
/// a minimap's zoom: the settled version the desktop shows, and the one it waits for. `upload`
/// carries a reference overview's codes, uploaded once for its version.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OverviewWant {
    pub keep: Option<u64>,
    pub awaited: Option<u64>,
    pub upload: Option<OverviewUpload>,
}

impl OverviewWant {
    fn wants(&self, version: u64) -> bool {
        self.keep == Some(version) || self.awaited == Some(version)
    }
}

/// A reference overview's codes, opaque RGBA row by row, shared with the desktop that reduced them.
#[derive(Clone)]
pub struct OverviewUpload {
    pub version: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

impl std::fmt::Debug for OverviewUpload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OverviewUpload")
            .field("version", &self.version)
            .field("size", &(self.width, self.height))
            .finish_non_exhaustive()
    }
}

impl PartialEq for OverviewUpload {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && (self.width, self.height) == (other.width, other.height)
            && Arc::ptr_eq(&self.rgba, &other.rgba)
    }
}

/// A finished overview a surface holds: its version and size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverviewShown {
    pub version: u64,
    pub width: u32,
    pub height: u32,
}

/// What a surface's diagnostics say of its overview: the finished ones it holds, an overview it
/// could not reserve, and its counters; with the pipeline's overview bytes, every surface's
/// resident, in progress and retiring overview storage together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverviewFigures {
    pub held: [Option<OverviewShown>; 2],
    /// The overview version whose storage could not be reserved, and why.
    pub refused: Option<(u64, GpuFallback)>,
    /// The version a picture at rest is reducing now, if any.
    pub reducing: Option<u64>,
    /// Overviews a picture at rest finished, and reference overviews uploaded with their bytes:
    /// what made each texture, so an unchanged overview's reuse shows as no change.
    pub builds: u64,
    pub uploads: u64,
    pub upload_bytes: u64,
    pub bytes: u64,
}

impl OverviewFigures {
    /// The finished overview of `version`, if the surface holds it.
    pub fn holds(&self, version: u64) -> Option<OverviewShown> {
        self.held
            .iter()
            .flatten()
            .find(|held| held.version == version)
            .copied()
    }
}

/// What retires of an overview, never read, held until the GPU is done with it: a reduction's
/// parts in progress, a finished reduction's buffers, or a finished texture.
#[allow(dead_code)]
pub(super) enum OverviewHeld {
    Parts(Box<super::rest::RestParts>),
    Buffers(Vec<super::Charged>),
    Texture(Box<Output>),
}

/// A finished overview: its texture with its placement uniform, and what both are charged.
pub(super) struct Finished {
    pub(super) version: u64,
    pub(super) output: Output,
    pub(super) bytes: u64,
}

/// One surface's finished overviews, at most the two its desktop wants, and its counters.
#[derive(Default)]
pub struct OverviewStore {
    held: Vec<Finished>,
    refused: Option<(u64, GpuFallback)>,
    builds: u64,
    uploads: u64,
    upload_bytes: u64,
}

impl OverviewStore {
    /// The finished overview of `version`'s texture, drawn as a photograph tile is.
    pub fn output(&self, version: u64) -> Option<&Output> {
        self.held
            .iter()
            .find(|held| held.version == version)
            .map(|held| &held.output)
    }

    /// Every finished overview's texture, for a surface to bind once each.
    pub fn outputs(&self) -> impl Iterator<Item = &Output> {
        self.held.iter().map(|held| &held.output)
    }
}

impl State {
    /// The overview of `version` could not be reserved: refused, with why, until a want no longer
    /// names it.
    pub(super) fn overview_refused(&mut self, version: u64, fallback: GpuFallback) {
        self.overview.refused = Some((version, fallback));
    }

    /// What this surface holds of the overview, with `bytes`, the pipeline's overview ledger.
    pub fn overview_figures(&self, bytes: u64) -> OverviewFigures {
        let store = &self.overview;
        let mut held = [None, None];
        for (slot, finished) in held.iter_mut().zip(&store.held) {
            *slot = Some(OverviewShown {
                version: finished.version,
                width: finished.output.width,
                height: finished.output.height,
            });
        }
        OverviewFigures {
            held,
            refused: store.refused,
            reducing: self.rest.as_ref().and_then(|rest| rest.overview_version()),
            builds: store.builds,
            uploads: store.uploads,
            upload_bytes: store.upload_bytes,
            bytes,
        }
    }
}

/// An overview texture of `size`, written through its storage view by a reduction's quantization
/// or by an upload, sampled through an sRGB view as the photograph's textures are, and its
/// placement uniform. Charged by the caller first.
pub(super) fn overview_texture(
    device: &wgpu::Device,
    size: (u32, u32),
) -> (Output, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("luxforge.gpu_overview.output"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: OUTPUT_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[SAMPLED_FORMAT],
    });
    let storage = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("luxforge.gpu_overview.uniform"),
        size: UNIFORM_SIZE as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    (
        Output {
            tiles: vec![OutputTile { texture, uniform }],
            width: size.0,
            height: size.1,
            capacity: size,
            version: 0,
            region_key: None,
        },
        storage,
    )
}

/// What an overview texture of `size` and its placement uniform are charged.
pub(super) fn texture_bytes(size: (u32, u32)) -> u64 {
    u64::from(size.0) * u64::from(size.1) * 4 + UNIFORM_SIZE as u64
}

impl Executor {
    /// Charge `bytes` of overview storage ([`OVERVIEW_GPU_BUDGET`]), or why it cannot be held.
    pub(super) fn charge_overview(&self, bytes: u64) -> Result<(), GpuFallback> {
        self.figures.preview.charge_overview(bytes)
    }

    /// Hand `held`, charged `bytes` of overview storage, to the retirement worker.
    pub(super) fn retire_overview(&self, held: OverviewHeld, bytes: u64) {
        self.retire_preview(Held::Overview(Box::new(held)), bytes);
    }

    /// A picture at rest's overview just finished: held as `version`'s, beside at most one other,
    /// until the next [`Self::prepare_overview`] keeps or retires it by the desktop's want.
    pub(super) fn adopt_overview(&self, surface: &mut State, finished: Finished) {
        let store = &mut surface.overview;
        if let Some(at) = store
            .held
            .iter()
            .position(|held| held.version == finished.version)
        {
            let old = store.held.remove(at);
            self.retire_overview(OverviewHeld::Texture(Box::new(old.output)), old.bytes);
        }
        store.builds += 1;
        if store
            .refused
            .is_some_and(|(version, _)| version == finished.version)
        {
            store.refused = None;
        }
        store.held.push(finished);
        // Never more than two: the oldest goes first, whatever the next want says of it.
        while store.held.len() > 2 {
            let old = store.held.remove(0);
            self.retire_overview(OverviewHeld::Texture(Box::new(old.output)), old.bytes);
        }
    }

    /// Keep what `want` names of `surface`'s overviews and retire the rest; then upload a reference
    /// overview it hands for a version the surface does not hold, once, charged first. No want lets
    /// every overview go. Called once a frame, after the picture at rest's tiles, so an overview
    /// just finished is kept when it is the one awaited. No pixel is read back and nothing waits.
    pub fn prepare_overview(
        &self,
        surface: &mut State,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        want: Option<&OverviewWant>,
    ) {
        let Some(want) = want else {
            self.release_overview(surface);
            return;
        };
        let store = &mut surface.overview;
        let mut at = 0;
        while at < store.held.len() {
            if want.wants(store.held[at].version) {
                at += 1;
            } else {
                let old = store.held.remove(at);
                self.retire_overview(OverviewHeld::Texture(Box::new(old.output)), old.bytes);
            }
        }
        if store
            .refused
            .is_some_and(|(version, _)| !want.wants(version))
        {
            store.refused = None;
        }
        let Some(upload) = want.upload.as_ref().filter(|upload| {
            want.wants(upload.version)
                && !store.held.iter().any(|held| held.version == upload.version)
                && store
                    .refused
                    .is_none_or(|(version, _)| version != upload.version)
        }) else {
            return;
        };
        let size = (upload.width, upload.height);
        let limit = device.limits().max_texture_dimension_2d;
        if size.0 == 0
            || size.1 == 0
            || size.0 > limit
            || size.1 > limit
            || upload.rgba.len() as u64 != u64::from(size.0) * u64::from(size.1) * 4
        {
            store.refused = Some((upload.version, GpuFallback::PipelineFailed));
            return;
        }
        let bytes = texture_bytes(size);
        if let Err(fallback) = self.charge_overview(bytes) {
            store.refused = Some((upload.version, fallback));
            return;
        }
        let (mut output, _) = overview_texture(device, size);
        queue.write_texture(
            output.tiles[0].texture.as_image_copy(),
            &upload.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size.0 * 4),
                rows_per_image: Some(size.1),
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        output.version = upload.version;
        store.uploads += 1;
        store.upload_bytes += u64::from(size.0) * u64::from(size.1) * 4;
        store.held.push(Finished {
            version: upload.version,
            output,
            bytes,
        });
    }

    /// Retire every finished overview `surface` holds; a picture at rest's overview in progress
    /// goes with the picture at rest.
    pub fn release_overview(&self, surface: &mut State) {
        let store = &mut surface.overview;
        store.refused = None;
        for old in std::mem::take(&mut store.held) {
            self.retire_overview(OverviewHeld::Texture(Box::new(old.output)), old.bytes);
        }
    }
}

#[cfg(test)]
#[path = "overview_tests.rs"]
mod tests;
