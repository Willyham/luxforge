//! The stage holder on a headless device whose largest texture is 64 pixels, so a stage of
//! 100 × 90 is held in 2 × 2 textures with seams at 64: tiles copied in across the seams, from
//! textures holding them past a halo, and windows copied out across them, are the stage's texels
//! bit for bit; a stage past two textures either way is refused before anything is charged or
//! created; and its charge is its texels.
use super::*;

const LIMIT: u32 = 64;
const STAGE: (u32, u32) = (100, 90);
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

/// A device whose largest texture is [`LIMIT`]; `None`, having printed that `test` was skipped,
/// without an adapter.
fn device(test: &str) -> Option<(wgpu::Device, wgpu::Queue)> {
    let limits = wgpu::Limits {
        max_texture_dimension_2d: LIMIT,
        ..wgpu::Limits::default()
    };
    let (device, queue, _) = super::super::headless::device(test, limits)?;
    Some((device, queue))
}

/// The stage's texel `(x, y)`: four values every one of which tells it apart.
fn texel(x: u32, y: u32) -> [f32; 4] {
    [
        x as f32,
        y as f32,
        (x * 1000 + y) as f32 + 0.25,
        -((y * 1000 + x) as f32) - 0.5,
    ]
}

fn texture(device: &wgpu::Device, (width, height): (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("luxforge.gpu_preview.stage_test"),
        size: extent(width, height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// A texture of `size` holding the stage's texels from `(x0, y0)` on: a tile drawn past its halo.
fn drawn(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: (u32, u32),
    at: (i64, i64),
) -> wgpu::Texture {
    let texture = texture(device, size);
    let mut bytes = Vec::with_capacity((size.0 * size.1 * 16) as usize);
    for y in 0..size.1 {
        for x in 0..size.0 {
            // Past the stage the halo holds what a stage texel never does.
            let (sx, sy) = (at.0 + i64::from(x), at.1 + i64::from(y));
            let value = if sx < 0 || sy < 0 {
                [f32::NAN; 4]
            } else {
                texel(sx as u32, sy as u32)
            };
            bytes.extend(value.iter().flat_map(|value| value.to_le_bytes()));
        }
    }
    queue.write_texture(
        texture.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.0 * 16),
            rows_per_image: Some(size.1),
        },
        extent(size.0, size.1),
    );
    texture
}

/// `texture`'s texels, `size` of them, read back row by row.
fn read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: (u32, u32),
) -> Vec<[f32; 4]> {
    let row = size.0 * 16;
    let padded =
        row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("luxforge.gpu_preview.stage_test.read"),
        size: u64::from(padded) * u64::from(size.1),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(size.1),
            },
        },
        extent(size.0, size.1),
    );
    queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("the device finishes");
    let mapped = buffer.slice(..).get_mapped_range();
    let texels = mapped
        .chunks_exact(padded as usize)
        .flat_map(|line| {
            line[..row as usize].chunks_exact(16).map(|texel| {
                std::array::from_fn(|channel| {
                    f32::from_le_bytes(
                        texel[channel * 4..channel * 4 + 4]
                            .try_into()
                            .expect("four bytes"),
                    )
                })
            })
        })
        .collect();
    drop(mapped);
    buffer.unmap();
    texels
}

/// Tiles of 40 × 30 cover the stage, each copied in from a texture holding it past a halo of 6
/// (NaN past the stage's first edges); windows across one seam, both and none, and one at the
/// stage's far corner, copy out the stage's texels bit for bit.
#[test]
fn tiles_copied_in_and_windows_copied_out_across_the_seams_are_the_stages_texels() {
    let test = "tiles_copied_in_and_windows_copied_out_across_the_seams_are_the_stages_texels";
    let Some((device, queue)) = device(test) else {
        return;
    };
    let mut charged = 0;
    let holder = StageHolder::create(&device, STAGE, FORMAT, |bytes| {
        charged = bytes;
        Ok::<(), GpuFallback>(())
    })
    .expect("a holder");
    assert_eq!(charged, u64::from(STAGE.0 * STAGE.1) * 16, "its texels");
    assert_eq!((holder.bytes(), holder.stage()), (charged, STAGE));
    assert_eq!(holder.grid, (2, 2), "four textures, seams at 64");
    let halo = 6u32;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let mut drawn_tiles = Vec::new();
    for y0 in (0..STAGE.1).step_by(30) {
        for x0 in (0..STAGE.0).step_by(40) {
            let rect = [x0, y0, (x0 + 40).min(STAGE.0), (y0 + 30).min(STAGE.1)];
            let at = (
                i64::from(x0) - i64::from(halo),
                i64::from(y0) - i64::from(halo),
            );
            let size = (rect[2] - rect[0] + 2 * halo, rect[3] - rect[1] + 2 * halo);
            let tile = drawn(&device, &queue, size, at);
            holder.copy_in(&mut encoder, &tile, (halo, halo), rect);
            drawn_tiles.push(tile);
        }
    }
    queue.submit([encoder.finish()]);
    for window in [
        [50, 10, 30, 20],
        [10, 50, 20, 30],
        [40, 35, 50, 45],
        [5, 5, 50, 50],
        [70, 70, 30, 20],
        [36, 26, 64, 64],
    ] {
        let size = (window[2], window[3]);
        let out = texture(&device, size);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        holder.copy_out(&mut encoder, &out, window);
        queue.submit([encoder.finish()]);
        let texels = read(&device, &queue, &out, size);
        for (at, value) in texels.iter().enumerate() {
            let (x, y) = (
                window[0] + at as u32 % size.0,
                window[1] + at as u32 / size.0,
            );
            assert_eq!(
                value.map(f32::to_bits),
                texel(x, y).map(f32::to_bits),
                "{window:?}: ({x}, {y})"
            );
        }
    }
    assert_eq!(holder.into_textures().len(), 4);
}

/// A stage past two textures either way is refused as `texture-limit` before its charge is asked
/// for; a refused charge creates nothing; one texture holds a stage within the limit.
#[test]
fn a_stage_past_two_textures_either_way_is_refused_before_anything_is_charged() {
    let test = "a_stage_past_two_textures_either_way_is_refused_before_anything_is_charged";
    let Some((device, _)) = device(test) else {
        return;
    };
    for stage in [(129, 10), (10, 129), (0, 10)] {
        let mut asked = false;
        let refused = StageHolder::create(&device, stage, FORMAT, |_| {
            asked = true;
            Ok::<(), GpuFallback>(())
        });
        assert!(
            matches!(refused, Err(GpuFallback::TextureLimit { limit: 128, .. })),
            "{stage:?}"
        );
        assert!(!asked, "{stage:?}: nothing charged");
        assert!(StageHolder::charge(stage, FORMAT, LIMIT).is_err());
    }
    let over = GpuFallback::BudgetExceeded {
        requested: 1,
        in_use: 0,
        budget: 0,
    };
    assert_eq!(
        StageHolder::create(&device, STAGE, FORMAT, |_| Err(over)).err(),
        Some(over),
        "a refused charge"
    );
    let one = StageHolder::create(&device, (64, 64), wgpu::TextureFormat::Rgba16Float, |_| {
        Ok::<(), GpuFallback>(())
    })
    .expect("a holder");
    assert_eq!((one.grid, one.bytes()), ((1, 1), 64 * 64 * 8));
}
