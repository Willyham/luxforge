//! A repair step's read set filled before its passes run (`docs/design/corrections.md`,
//! "Rendering"): the step's input over every rectangle its operations read, packed into one plane of
//! its own ([`ReadAtlas`]), so a source across the frame is read whatever window the boundary holds.
//!
//! - **Where from.** A rectangle the boundary holds whole is copied from it, texel for texel. Any
//!   other is derived from the photograph's source the pipeline holds by the boundary's own
//!   derivation ([`Derivation`]) placed at its texels of the plane, so each texel is the one the
//!   boundary would hold at its stage pixel: a cut's, bit for bit, or a reduction's. A rectangle
//!   neither holds is `source-missing`, and the step is not drawn.
//! - **When.** The plane is one of the link's kept textures, which no pass writes: it is filled
//!   when what it would hold changes — the boundary's content, its window, the source, the
//!   derivation or the rectangles ([`key`]) — and the link's passes then all run again over it. A
//!   tick of a gesture over the same boundary fills nothing.
//! - **Bounds.** The plane is charged with the link's kept planes. The derivation's words, a
//!   reduction's coverage tables among them, and each rectangle's origin are held in two buffers of
//!   the link's, at most [`FILL_WORDS`] words (4 MiB), past which the step is `buffer-limit`; a cut
//!   needs eleven words and two a rectangle.
use super::{
    BoundaryTexture, GpuFallback, SourceLayouts, SourceSlot, le_bytes,
    source::{Derivation, PLACED_ORIGIN},
    spatial::ReadAtlas,
};

/// The most words a link's fill buffers hold: 4 MiB, a reduction's coverage tables at any proxy
/// stage and every rectangle's origin.
pub const FILL_WORDS: u64 = 1 << 20;

/// What a link's read sets are filled from: the texture the link reads, when its first step reads
/// the boundary itself, over a window of `size` texels whose texel `(0, 0)` is stage pixel
/// `origin`, with `input` the key of its content; and the source the pipeline holds, with the
/// derivation the boundary is drawn by, when it is derived.
#[derive(Clone, Copy)]
pub struct Fill<'a> {
    pub boundary: Option<&'a wgpu::Texture>,
    pub origin: (u32, u32),
    pub size: (u32, u32),
    pub input: u64,
    pub source: Option<(&'a SourceSlot, &'a SourceLayouts, &'a Derivation)>,
}

/// A link's fill buffers: the derivation's words, whose origin each rectangle's pass overwrites,
/// and every rectangle's origin, copied over them in turn.
pub struct FillBuffers {
    words: wgpu::Buffer,
    origins: wgpu::Buffer,
}

/// The key of what `atlas` holds once filled from `fill`.
pub fn key(fill: &Fill<'_>, atlas: &ReadAtlas) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    (
        fill.boundary.is_some(),
        fill.input,
        fill.origin,
        fill.size,
        atlas,
    )
        .hash(&mut hasher);
    match fill.source {
        Some((source, _, Derivation::Cut { .. })) => (source.version(), 0u8).hash(&mut hasher),
        Some((source, _, Derivation::Reduce(reduction))) => (
            source.version(),
            1u8,
            std::sync::Arc::as_ptr(reduction) as usize,
        )
            .hash(&mut hasher),
        None => 2u8.hash(&mut hasher),
    }
    hasher.finish()
}

/// Encode the fill of `atlas` into `target`, its plane's texture, from `fill`: each rectangle
/// copied from the boundary where it holds it whole, else derived from the source
/// ([module](self) docs). `buffers` are the link's, grown to what the derivation needs.
pub fn encode(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    fill: &Fill<'_>,
    atlas: &ReadAtlas,
    target: &wgpu::Texture,
    buffers: &mut Option<FillBuffers>,
) -> Result<(), GpuFallback> {
    let window = [
        fill.origin.0,
        fill.origin.1,
        fill.origin.0 + fill.size.0,
        fill.origin.1 + fill.size.1,
    ];
    let copied = |[x, y, width, height]: [u32; 4]| {
        fill.boundary
            .filter(|boundary| boundary.format() == target.format())
            .filter(|_| {
                x >= window[0]
                    && y >= window[1]
                    && x + width <= window[2]
                    && y + height <= window[3]
            })
    };
    let derived: Vec<usize> = (0..atlas.rects.len())
        .filter(|&index| copied(atlas.rects[index].0).is_none())
        .collect();
    let mut placed = None;
    if !derived.is_empty() {
        let (source, layouts, derivation) = fill.source.ok_or(GpuFallback::SourceMissing)?;
        if source.kind().boundary().texture() != target.format() {
            return Err(GpuFallback::PipelineFailed);
        }
        let rects: Vec<[u32; 4]> = derived.iter().map(|&index| atlas.rects[index].0).collect();
        let words = source
            .placed_words(derivation, &rects)
            .ok_or(GpuFallback::SourceMissing)?;
        let origins: Vec<u32> = derived
            .iter()
            .flat_map(|&index| {
                let ([x, y, _, _], [ax, ay]) = atlas.rects[index];
                [x.wrapping_sub(ax), y.wrapping_sub(ay)]
            })
            .collect();
        let need = ((words.len() * 4) as u64, (origins.len() * 4) as u64);
        if (need.0 + need.1) / 4 > FILL_WORDS {
            return Err(GpuFallback::BufferLimit {
                bytes: need.0 + need.1,
                limit: FILL_WORDS * 4,
            });
        }
        let fits = buffers
            .as_ref()
            .is_some_and(|held| held.words.size() >= need.0 && held.origins.size() >= need.1);
        if !fits {
            let buffer = |label, size: u64, usage| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: size.max(16).next_multiple_of(16),
                    usage,
                    mapped_at_creation: false,
                })
            };
            let words = buffer(
                "luxforge.gpu_fill.words",
                need.0,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            );
            let origins = buffer(
                "luxforge.gpu_fill.origins",
                need.1,
                wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            );
            *buffers = Some(FillBuffers { words, origins });
        }
        let held = buffers.as_ref().expect("the fill buffers fitted");
        queue.write_buffer(&held.words, 0, &le_bytes(&words));
        queue.write_buffer(&held.origins, 0, &le_bytes(&origins));
        placed = Some((source, layouts, derivation, held));
    }
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut next = 0u64;
    for &(rect, at) in &atlas.rects {
        let [x, y, width, height] = rect;
        if let Some(boundary) = copied(rect) {
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: boundary,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: x - window[0],
                        y: y - window[1],
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: target,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: at[0],
                        y: at[1],
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            continue;
        }
        let (source, layouts, derivation, held) = placed.expect("a rectangle to derive");
        encoder.copy_buffer_to_buffer(
            &held.origins,
            next * 8,
            &held.words,
            (PLACED_ORIGIN * 4) as u64,
            8,
        );
        next += 1;
        source.encode_placed(
            device,
            encoder,
            layouts,
            derivation,
            &held.words,
            &view,
            ((at[0], at[1]), (width, height)),
        );
    }
    Ok(())
}
