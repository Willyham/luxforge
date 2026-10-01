//! Linear-light mip levels for a full-resolution photograph drawn below its size.
//!
//! A linear sampler reads the four texels around a sample point. Drawn at half its size or more,
//! every source texel still reaches some output pixel; drawn below that the sampler skips texels,
//! and detail beyond the display's Nyquist limit folds into full-contrast replicas of the
//! centre's rings, averaged in encoded codes rather than in light. The cure is a texture that
//! holds the photograph's reductions: each level the mean of the 2 × 2 texels of the one above, the
//! sampler's trilinear filter reading between the two levels that bracket the drawn scale.
//!
//! - **When.** The pipeline gives a photograph's texture a chain when the photograph is drawn with
//!   both axes under half its size ([`minified`]) and a chain is allowed ([`admissible`]): the
//!   photograph is one texture, because a tiled photograph's one-texel aprons would put a seam
//!   into every level, and the whole chain, a third larger than the base, stays within the one
//!   full-allocation cap. Otherwise the texture is the plain one it has always been, and the
//!   desktop draws the exact-derived display reduction it holds instead.
//! - **Where.** The levels are part of the slot's one allocation, counted by the shared photo-slot
//!   budget and retired with it. A texture that has them is never rebuilt without them, so a zoom
//!   back and forth across the threshold allocates once.
//! - **How.** A render pass per level on the GPU, encoded and submitted from `prepare` with no
//!   readback and no wait. The source is read through an sRGB-typed view and the level written
//!   through an sRGB-typed view of the same texture, so the mean is taken in linear light whether
//!   the pipeline's textures are sRGB-typed (the target is) or not. Levels are generated the first
//!   time the photograph is drawn minified after its pixels were written, never for a frame drawn
//!   at its size.

use super::FULL_BUDGET;

/// A photograph is minified, and so worth mip levels, when both axes are drawn at under this
/// fraction of the texture: from half up, a bilinear sample still reads every source texel.
const MINIFIED_BELOW: f32 = 0.5;

/// The format the levels are averaged and encoded in, whatever the texture's own: sRGB, so the
/// hardware decodes each texel to linear on the load and encodes the mean on the write.
pub(super) const MIP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Whether a `size` photograph drawn into `destination` physical pixels is drawn under half its
/// size on both axes.
pub(super) fn minified(
    (width, height): (u32, u32),
    (destination_width, destination_height): (f32, f32),
) -> bool {
    destination_width > 0.0
        && destination_height > 0.0
        && destination_width < width as f32 * MINIFIED_BELOW
        && destination_height < height as f32 * MINIFIED_BELOW
}

/// The mip levels of a `size` texture, the base included: halving, rounding down, to one texel.
pub(super) fn level_count((width, height): (u32, u32)) -> u32 {
    u32::BITS - width.max(height).max(1).leading_zeros()
}

/// The bytes of a `size` RGBA8 texture with `levels` levels, each half the one before and rounded
/// down to a texel.
pub(super) fn chain_bytes((mut width, mut height): (u32, u32), levels: u32) -> u64 {
    let mut bytes = 0;
    for _ in 0..levels {
        bytes += u64::from(width) * u64::from(height) * 4;
        width = (width / 2).max(1);
        height = (height / 2).max(1);
    }
    bytes
}

/// Whether a `size` photograph may have mip levels on a device whose textures are `texture_limit`
/// pixels a side: it is one texture, wider than a texel, and its whole chain is within one full
/// allocation. A photograph that may not is drawn through the plain texture, and the desktop
/// holds a display reduction for it instead.
pub fn admissible(size: (u32, u32), texture_limit: u32) -> bool {
    size.0 > 1
        && size.1 > 1
        && size.0 <= texture_limit
        && size.1 <= texture_limit
        && chain_bytes(size, level_count(size)) <= FULL_BUDGET
}

/// The pass that writes one level from the one above it.
pub(super) struct MipPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

impl MipPipeline {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("luxforge.photo_surface.mips.layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                // Read with `textureLoad`, which needs no filtering.
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("luxforge.photo_surface.mips.shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("mips.wgsl"))),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("luxforge.photo_surface.mips.pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("luxforge.photo_surface.mips.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: MIP_FORMAT,
                    // A replacement: every texel of the level is written.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        Self { pipeline, layout }
    }

    /// Write levels `1..levels` of `texture` from its level 0, in linear light, and submit the
    /// passes. The queue runs them after every `write_texture` already made to the texture and
    /// before any draw submitted later, so nothing here reads back or waits. `texture` must have
    /// been created with `RENDER_ATTACHMENT` and [`MIP_FORMAT`] among its view formats.
    pub(super) fn generate(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        levels: u32,
    ) {
        let view = |level: u32, usage: wgpu::TextureUsages| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("luxforge.photo_surface.mips.view"),
                format: Some(MIP_FORMAT),
                dimension: Some(wgpu::TextureViewDimension::D2),
                usage: Some(usage),
                aspect: wgpu::TextureAspect::All,
                base_mip_level: level,
                mip_level_count: Some(1),
                base_array_layer: 0,
                array_layer_count: Some(1),
            })
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.photo_surface.mips.encoder"),
        });
        for level in 1..levels {
            let source = view(level - 1, wgpu::TextureUsages::TEXTURE_BINDING);
            let target = view(level, wgpu::TextureUsages::RENDER_ATTACHMENT);
            let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("luxforge.photo_surface.mips.bindings"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source),
                }],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("luxforge.photo_surface.mips.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_halves_to_one_texel_and_a_24_mp_photograph_gains_a_third() {
        assert_eq!(level_count((1, 1)), 1);
        assert_eq!(level_count((2, 1)), 2);
        assert_eq!(level_count((6000, 4000)), 13);
        assert_eq!(level_count((8192, 8192)), 14);
        // 5 × 3, then 2 × 1, then 1 × 1.
        assert_eq!(chain_bytes((5, 3), level_count((5, 3))), (15 + 2 + 1) * 4);
        let base = 6000u64 * 4000 * 4;
        let chain = chain_bytes((6000, 4000), 13);
        assert!(chain > base && chain < base * 4 / 3, "{chain}");
        assert_eq!(chain, base + 31_999_028);
    }

    #[test]
    fn a_photograph_is_minified_when_both_axes_are_drawn_under_half() {
        assert!(minified((6000, 4000), (1716.0, 1144.0)));
        assert!(minified((6000, 4000), (2999.0, 1999.0)));
        assert!(!minified((6000, 4000), (3000.0, 2000.0)));
        assert!(!minified((6000, 4000), (1716.0, 2000.0)));
        // A proxy drawn at about its own size, magnified, or with nothing visible.
        assert!(!minified((1716, 1144), (1715.0, 1144.0)));
        assert!(!minified((1716, 1144), (3432.0, 2288.0)));
        assert!(!minified((6000, 4000), (0.0, 0.0)));
    }

    #[test]
    fn a_chain_is_admitted_for_one_texture_within_the_allocation_cap() {
        assert!(admissible((6000, 4000), 8192));
        assert!(admissible((8192, 8192), 8192));
        // Wider than the device's texture: held in tiles, whose aprons would break every level.
        assert!(!admissible((10_000, 6000), 8192));
        assert!(!admissible((8193, 100), 8192));
        // One texture within the limit but a chain over the 512 MiB cap: 16384² is 1 GiB alone,
        // and 11000² is 484 MB, which with its third more passes the cap where 10000² (400 MB,
        // 533 MB with its levels) does not.
        assert!(!admissible((16_384, 16_384), 16_384));
        assert!(!admissible((11_000, 11_000), 16_384));
        assert!(admissible((10_000, 10_000), 16_384));
        assert!(!admissible((1, 100), 8192));
    }
}
