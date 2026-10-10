//! A repair step's read set filled before its passes run (`docs/design/corrections.md`,
//! "Rendering"): the step's input over every rectangle its operations read, packed into one plane of
//! its own ([`ReadAtlas`]), so a source across the frame is read whatever window the boundary holds.
//!
//! - **Where from.** A rectangle the boundary holds whole is copied from it, texel for texel. Any
//!   other is derived by the boundary's own derivation ([`Derivation`]) placed at its texels of the
//!   plane, so each texel is the one the boundary would hold at its stage pixel: a cut's, bit for
//!   bit, or a reduction's. It is derived from the source the pipeline holds when that holds it —
//!   the photo surface holds the whole source — or else from a window of the source holding that
//!   rectangle alone, which the tile runner holds beside the window its tiles are cut from
//!   ([`Fill::reads`]). A rectangle none holds is `source-missing`, and the step is not drawn.
//! - **When.** The plane is one of the link's kept textures, which no pass writes: it is filled
//!   when what it would hold changes — the boundary's content, its window, the source, the
//!   derivation or the rectangles ([`key`]) — and the link's passes then all run again over it. A
//!   tick of a gesture over the same boundary fills nothing.
//! - **Bounds.** The plane is charged with the link's kept planes. The derivation's words, a
//!   reduction's coverage tables among them, and each rectangle's header — the held size and map
//!   of the slot it is derived from, and its origin — are held in two buffers of the link's, at
//!   most [`FILL_WORDS`] words (4 MiB), past which the step is `buffer-limit`; a cut needs eleven
//!   words and eleven a rectangle.
use super::{
    BoundaryTexture, GpuFallback, SourceLayouts, SourceSlot, le_bytes,
    source::{Derivation, PLACED_HEADER},
    spatial::ReadAtlas,
};

/// The most words a link's fill buffers hold: 4 MiB, a reduction's coverage tables at any proxy
/// stage and every rectangle's origin.
pub const FILL_WORDS: u64 = 1 << 20;

/// What a link's read sets are filled from: the texture the link reads, when its first step reads
/// the boundary itself, over a window of `size` texels whose texel `(0, 0)` is stage pixel
/// `origin`, with `input` the key of its content; and the source the pipeline holds, with the
/// derivation the boundary is drawn by, when it is derived; and windows of the source each holding
/// rectangles of the read set the source does not, of the same source.
#[derive(Clone, Copy)]
pub struct Fill<'a> {
    pub boundary: Option<&'a wgpu::Texture>,
    pub origin: (u32, u32),
    pub size: (u32, u32),
    pub input: u64,
    pub source: Option<(&'a SourceSlot, &'a SourceLayouts, &'a Derivation)>,
    pub reads: &'a [SourceSlot],
}

/// A link's fill buffers: the derivation's words, whose header each rectangle's pass overwrites,
/// and every rectangle's header, copied over them in turn.
pub struct FillBuffers {
    words: wgpu::Buffer,
    headers: wgpu::Buffer,
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
    for read in fill.reads {
        (read.version(), read.window()).hash(&mut hasher);
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
        // Each rectangle from the first slot holding it: the source, else a window of its own.
        let mut chosen: Vec<&SourceSlot> = Vec::with_capacity(derived.len());
        let mut headers: Vec<u32> = Vec::with_capacity(derived.len() * PLACED_HEADER);
        for &index in &derived {
            let (rect, at) = atlas.rects[index];
            let slot = std::iter::once(source)
                .chain(fill.reads)
                .find(|slot| slot.places(derivation, rect))
                .ok_or(GpuFallback::SourceMissing)?;
            if slot.kind() != source.kind() {
                return Err(GpuFallback::PipelineFailed);
            }
            headers.extend(
                slot.placed_header((rect[0].wrapping_sub(at[0]), rect[1].wrapping_sub(at[1]))),
            );
            chosen.push(slot);
        }
        // The first slot's words, whose header each pass overwrites: a reduction's tables, which
        // only the whole source holds, are every rectangle's.
        let words = chosen[0]
            .placed_words(derivation, &[atlas.rects[derived[0]].0])
            .ok_or(GpuFallback::SourceMissing)?;
        let need = ((words.len() * 4) as u64, (headers.len() * 4) as u64);
        if (need.0 + need.1) / 4 > FILL_WORDS {
            return Err(GpuFallback::BufferLimit {
                bytes: need.0 + need.1,
                limit: FILL_WORDS * 4,
            });
        }
        let fits = buffers
            .as_ref()
            .is_some_and(|held| held.words.size() >= need.0 && held.headers.size() >= need.1);
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
            let headers = buffer(
                "luxforge.gpu_fill.headers",
                need.1,
                wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            );
            *buffers = Some(FillBuffers { words, headers });
        }
        let held = buffers.as_ref().expect("the fill buffers fitted");
        queue.write_buffer(&held.words, 0, &le_bytes(&words));
        queue.write_buffer(&held.headers, 0, &le_bytes(&headers));
        placed = Some((chosen, layouts, derivation, held));
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
        let (chosen, layouts, derivation, held) = placed.as_ref().expect("a rectangle to derive");
        let source = chosen[next as usize];
        let header = (PLACED_HEADER * 4) as u64;
        encoder.copy_buffer_to_buffer(&held.headers, next * header, &held.words, 0, header);
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
