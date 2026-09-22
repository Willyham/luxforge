//! The photograph's own surface: a shader primitive that owns one texture and writes a raster into
//! it during the same frame that draws it.
//!
//! The toolkit's image widget reaches the GPU through an allocation round trip: the desktop hands
//! the runtime a handle, the runtime answers with an allocation on a later turn of the event loop,
//! and only then can the view draw it. That answer costs one runtime hop — about one display frame
//! on the owner's Mac — on every preview, which is exactly the hop the instant-preview design
//! removes ([docs/design/instant-preview.md](../../../../docs/design/instant-preview.md),
//! "One frame per hop"). Here the raster is plain data the view borrows: `prepare` writes it into
//! the pipeline's own texture just before `draw` samples that texture, so a frame reaches the
//! screen in the redraw that follows the worker's wake and nothing waits on the runtime.
//!
//! Bytes are reproduced exactly as the toolkit's image widget reproduces them, because every choice
//! that touches a byte is the toolkit's own:
//!
//! - **Format.** The toolkit stores image pixels in an `Rgba8UnormSrgb` atlas when it gamma
//!   corrects, and it gamma corrects exactly when it picked an sRGB surface format. So this texture
//!   is `Rgba8UnormSrgb` when the target format is sRGB and `Rgba8Unorm` when it is not: the
//!   hardware decodes a texel to linear on the sample and encodes it back on the write, which is
//!   the identity on a texel drawn one-to-one.
//! - **Filtering.** A linear sampler clamped to the edge, for minification and magnification alike,
//!   which is the image widget's `FilterMethod::Linear` default.
//! - **Blending.** The image pipeline's own blend state, so an alpha-carrying raster composites
//!   the same way; a photograph's raster is opaque, where that blend is the identity.
//! - **Geometry.** The destination rectangle is computed with [`iced::ContentFit`] and centred
//!   exactly as `iced_widget::image::drawing_bounds` computes it, then snapped to the physical
//!   pixel grid in the vertex shader with WGSL's own `round`, which is what the image shader does
//!   with `snap: true`.
//!
//! Memory: one texture per pipeline — that is, one for the whole application, because one
//! photograph is on screen at a time — recreated only when the raster's dimensions change, plus a
//! 32-byte uniform buffer. The pixels themselves are borrowed from the raster the desktop already
//! retains; this crate copies none of them, and the one copy per changed frame is the write into
//! the texture.

use iced::{
    ContentFit, Element, Length, Point, Rectangle, Size,
    mouse::{self, Cursor},
    widget::shader::{self, Viewport},
};
use std::sync::Arc;

/// How the raster is placed inside the widget's bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Scaled to fit inside the bounds, keeping its aspect ratio, and centred: the Fit view.
    Contain,
    /// Stretched to the whole bounds: a percentage zoom, where the widget is already sized to the
    /// exact box the picture belongs in and the raster may be a smaller display proxy.
    Fill,
}

impl Placement {
    fn content_fit(self) -> ContentFit {
        match self {
            Placement::Contain => ContentFit::Contain,
            Placement::Fill => ContentFit::Fill,
        }
    }
}

/// One photograph's pixels as the surface draws them: an RGBA8 buffer, its size, and a version.
///
/// The version is the only thing that decides whether a frame is written to the texture, so it must
/// change whenever the pixels do and never otherwise. The desktop gives it a counter it increments
/// each time it hands a raster over; any monotone key with that property works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoRaster {
    pixels: Arc<[u8]>,
    width: u32,
    height: u32,
    version: u64,
}

impl PhotoRaster {
    /// A raster of `width` × `height` RGBA8 pixels, or `None` when the buffer does not hold exactly
    /// that many bytes. A surface never draws a buffer it cannot account for.
    pub fn new(pixels: Arc<[u8]>, width: u32, height: u32, version: u64) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))?;
        (width > 0 && height > 0 && pixels.len() == expected).then_some(Self {
            pixels,
            width,
            height,
            version,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

/// The photograph, drawn by a primitive that owns its texture.
///
/// `width`/`height` are the widget's own sizing rule: `Fill`/`Fill` at Fit, where [`Placement`] is
/// `Contain` and the primitive centres the picture inside whatever it is given, and the exact
/// displayed box at a percentage, where the placement is `Fill`.
pub fn photo_surface<'a, Message: 'a>(
    raster: &PhotoRaster,
    placement: Placement,
    width: Length,
    height: Length,
) -> Element<'a, Message> {
    shader::Shader::new(PhotoSurface {
        raster: raster.clone(),
        placement,
    })
    .width(width)
    .height(height)
    .into()
}

/// Where the toolkit draws a fitted raster inside `bounds`, in the bounds' own coordinate frame:
/// [`ContentFit`] sized and centred, which is `iced_widget::image::drawing_bounds` for an image
/// with no crop, no rotation and unit scale.
fn placement_rect(
    placement: Placement,
    (width, height): (u32, u32),
    bounds: Rectangle,
) -> Option<Rectangle> {
    let content = Size::new(width as f32, height as f32);
    if !(content.width > 0.0
        && content.height > 0.0
        && bounds.width > 0.0
        && bounds.height > 0.0
        && bounds.x.is_finite()
        && bounds.y.is_finite())
    {
        return None;
    }
    let size = placement.content_fit().fit(content, bounds.size());
    (size.width > 0.0 && size.height > 0.0).then(|| {
        Rectangle::new(
            Point::new(
                bounds.center_x() - size.width / 2.0,
                bounds.center_y() - size.height / 2.0,
            ),
            size,
        )
    })
}

#[derive(Debug)]
struct PhotoSurface {
    raster: PhotoRaster,
    placement: Placement,
}

impl<Message> shader::Program<Message> for PhotoSurface {
    type State = ();
    type Primitive = PhotoPrimitive;

    fn draw(&self, _state: &(), _cursor: Cursor, _bounds: Rectangle) -> PhotoPrimitive {
        PhotoPrimitive {
            raster: self.raster.clone(),
            placement: self.placement,
        }
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        _bounds: Rectangle,
        _cursor: Cursor,
    ) -> mouse::Interaction {
        // The photograph is not a control: every pointer event belongs to the mouse area around it,
        // which is what the image widget it replaces reports too.
        mouse::Interaction::None
    }
}

/// The raster and its placement, as the renderer receives them for one frame.
#[derive(Debug)]
pub struct PhotoPrimitive {
    raster: PhotoRaster,
    placement: Placement,
}

/// The little uniform block the vertex shader reads: the render pass's viewport and the destination
/// rectangle, both in physical pixels of the whole frame.
fn uniform_bytes(viewport: [f32; 4], destination: [f32; 4]) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    for (index, value) in viewport.iter().chain(destination.iter()).enumerate() {
        // GPU buffers are little-endian on every platform wgpu targets.
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

impl shader::Primitive for PhotoPrimitive {
    type Pipeline = PhotoPipeline;

    fn prepare(
        &self,
        pipeline: &mut PhotoPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        // One write per changed frame, never per redraw: the texture is recreated only when the
        // raster's dimensions change, and written only when its version does.
        pipeline.write(device, queue, &self.raster);
        // The uniform is refreshed every prepare instead, because the bounds and the viewport can
        // change with no new frame at all — a window resize, a pan, a panel opening.
        let scale = viewport.scale_factor();
        let Some(destination) = placement_rect(self.placement, self.raster.size(), *bounds) else {
            return;
        };
        queue.write_buffer(
            &pipeline.uniform,
            0,
            &uniform_bytes(
                [
                    bounds.x * scale,
                    bounds.y * scale,
                    bounds.width * scale,
                    bounds.height * scale,
                ],
                [
                    destination.x * scale,
                    destination.y * scale,
                    destination.width * scale,
                    destination.height * scale,
                ],
            ),
        );
    }

    fn draw(&self, pipeline: &PhotoPipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        // The render pass's viewport is already this widget's bounds and its scissor the visible
        // part of them, so the quad is positioned inside that frame by the uniform alone.
        if let Some(photo) = &pipeline.photo {
            render_pass.set_pipeline(&pipeline.pipeline);
            render_pass.set_bind_group(0, &photo.bindings, &[]);
            render_pass.draw(0..6, 0..1);
        }
        // Drawn either way: with no texture there is nothing to show, and the encoder fallback
        // would only begin a render pass to draw the same nothing.
        true
    }
}

/// The texture on the GPU, with what it holds.
struct Photo {
    texture: wgpu::Texture,
    bindings: wgpu::BindGroup,
    width: u32,
    height: u32,
    /// The version of the raster last written into it.
    version: u64,
}

/// The render pipeline, the sampler, the uniform buffer and the one texture, shared by every
/// instance of [`PhotoPrimitive`]. One photograph is on screen at a time, so this is one texture
/// for the application.
pub struct PhotoPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    texture_format: wgpu::TextureFormat,
    photo: Option<Photo>,
}

impl PhotoPipeline {
    /// Make the texture hold `raster`, creating it when the dimensions changed and writing the
    /// pixels when the version did.
    fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, raster: &PhotoRaster) {
        let (width, height) = raster.size();
        let fresh = !self
            .photo
            .as_ref()
            .is_some_and(|photo| photo.width == width && photo.height == height);
        if fresh {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("lightwell.photo_surface.texture"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.texture_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("lightwell.photo_surface.bindings"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.photo = Some(Photo {
                texture,
                bindings,
                width,
                height,
                // No version can match until the pixels are written below.
                version: raster.version.wrapping_sub(1),
            });
        }
        let Some(photo) = &mut self.photo else {
            return;
        };
        if photo.version == raster.version {
            return;
        }
        queue.write_texture(
            photo.texture.as_image_copy(),
            &raster.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        photo.version = raster.version;
    }
}

impl shader::Pipeline for PhotoPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        // The toolkit gamma corrects exactly when it chose an sRGB target, and stores image pixels
        // in an sRGB-typed texture when it does. Matching that is what makes a raster byte land on
        // the surface as the image widget lands it.
        let texture_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lightwell.photo_surface.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            min_filter: wgpu::FilterMode::Linear,
            mag_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lightwell.photo_surface.layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lightwell.photo_surface.shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "photo_surface.wgsl"
            ))),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lightwell.photo_surface.pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lightwell.photo_surface.pipeline"),
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
                    format,
                    // The image pipeline's own blend state, so an alpha-carrying raster composites
                    // the same way. A photograph's raster is opaque, where this is a replacement.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            // The pass a custom primitive draws into is the frame itself, which is not
            // multisampled; the toolkit resolves its own antialiased meshes separately.
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lightwell.photo_surface.uniform"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            sampler,
            uniform,
            texture_format,
            photo: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(width: u32, height: u32, version: u64) -> PhotoRaster {
        let pixels: Arc<[u8]> = vec![0u8; (width * height * 4) as usize].into();
        PhotoRaster::new(pixels, width, height, version).expect("a whole raster")
    }

    /// A raster is exactly its declared size, or it is not a raster at all.
    #[test]
    fn a_raster_is_refused_unless_the_buffer_matches_its_dimensions() {
        let pixels: Arc<[u8]> = vec![0u8; 16].into();
        assert_eq!(
            raster(2, 2, 7).size(),
            (2, 2),
            "four RGBA pixels are a 2x2 raster"
        );
        assert_eq!(raster(2, 2, 7).version(), 7);
        for (width, height) in [(2, 3), (3, 2), (0, 2), (2, 0)] {
            assert!(
                PhotoRaster::new(pixels.clone(), width, height, 1).is_none(),
                "{width}x{height}"
            );
        }
    }

    /// Contain is the toolkit's own rule: the largest rectangle of the raster's ratio that fits,
    /// centred in the bounds. This is the Fit view, and it is where the thirds overlay and the
    /// pointer mapping in `view::canvas` expect the photograph to be.
    #[test]
    fn contain_centres_the_largest_fitting_rectangle() {
        let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 400.0));
        let rect = placement_rect(Placement::Contain, (200, 100), bounds).expect("a rectangle");
        assert_eq!((rect.width, rect.height), (400.0, 200.0));
        assert_eq!((rect.x, rect.y), (10.0, 120.0));
        // A taller-than-wide raster is bounded by the width instead, and still centred.
        let rect = placement_rect(Placement::Contain, (100, 400), bounds).expect("a rectangle");
        assert_eq!((rect.width, rect.height), (100.0, 400.0));
        assert_eq!((rect.x, rect.y), (160.0, 20.0));
        // The raster's own ratio decides, never the widget's: a proxy of the same picture lands in
        // the same rectangle as the exact render, to within its own rounding.
        let exact = placement_rect(Placement::Contain, (6000, 4000), bounds).expect("a rectangle");
        let proxy = placement_rect(Placement::Contain, (1200, 800), bounds).expect("a rectangle");
        assert_eq!((exact.x, exact.y), (proxy.x, proxy.y));
        assert_eq!((exact.width, exact.height), (proxy.width, proxy.height));
    }

    /// Fill takes the whole widget, whatever the raster's size: at a percentage the widget is
    /// already the exact stage's displayed box, and the texture in it may be a smaller proxy.
    #[test]
    fn fill_takes_the_whole_widget_whatever_the_raster_measures() {
        let bounds = Rectangle::new(Point::new(5.0, 7.0), Size::new(300.0, 200.0));
        for size in [(6000, 4000), (1200, 800), (10, 10000)] {
            let rect = placement_rect(Placement::Fill, size, bounds).expect("a rectangle");
            assert_eq!((rect.x, rect.y), (bounds.x, bounds.y), "{size:?}");
            assert_eq!(
                (rect.width, rect.height),
                (bounds.width, bounds.height),
                "{size:?}"
            );
        }
    }

    /// Nothing is placed where there is no room, and nothing is placed for a raster with no pixels.
    #[test]
    fn a_surface_with_no_room_places_nothing() {
        let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(400.0, 400.0));
        for empty in [
            Rectangle::new(Point::new(0.0, 0.0), Size::new(0.0, 400.0)),
            Rectangle::new(Point::new(0.0, 0.0), Size::new(400.0, 0.0)),
            Rectangle::new(Point::new(f32::NAN, 0.0), Size::new(400.0, 400.0)),
        ] {
            assert!(placement_rect(Placement::Contain, (200, 100), empty).is_none());
        }
        assert!(placement_rect(Placement::Contain, (0, 100), bounds).is_none());
    }

    /// The uniform block is the two rectangles the vertex shader reads, in order, as little-endian
    /// floats: eight of them in thirty-two bytes, which is what the buffer is sized for.
    #[test]
    fn the_uniform_block_is_the_viewport_then_the_destination() {
        let bytes = uniform_bytes([1.0, 2.0, 3.0, 4.0], [5.0, 6.0, 7.0, 8.0]);
        let read = |index: usize| {
            f32::from_le_bytes(
                bytes[index * 4..index * 4 + 4]
                    .try_into()
                    .expect("four bytes"),
            )
        };
        for index in 0..8 {
            assert_eq!(read(index), index as f32 + 1.0);
        }
    }
}
