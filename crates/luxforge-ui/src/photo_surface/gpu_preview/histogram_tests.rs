//! The histogram reduction's tests: its bounds and its derivation of the report's counters without
//! a device, then the kernel on a headless device the test creates, every count compared with the
//! independent reference's counts of the same codes (`luxforge_reference::tolerance`) at a zero
//! tolerance: the same reduction the release gate holds the GPU's counts to, written from the
//! design, and the one its harness maps the core reducer's report onto counter for counter.
//!
//! A headless test with no adapter prints that it was skipped and asserts nothing. It is not GPU
//! evidence: the skip is the report. Every readback here is completed by the pipeline's own
//! retirement worker, as the desktop's will be: no test polls the device or waits on a submission.
use super::super::tests::{headless, own_pipeline, settle};
use super::*;
use luxforge_reference::tolerance;
use luxforge_testbase::wait_for;

/// A texel with a channel at each end, which no tile holds as its own: counted, it would move every
/// endpoint counter, `both` among them.
const POISON: [u8; 4] = [0, 255, 0, 255];

/// A texture of output codes as the chain's last pass leaves them, `rgba8unorm` with the sRGB-typed
/// view the draw samples it through, holding `rgba`.
fn codes_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    (width, height): (u32, u32),
    rgba: &[u8],
) -> wgpu::Texture {
    texture_of(
        device,
        queue,
        (width, height),
        rgba,
        OUTPUT_FORMAT,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    )
}

fn texture_of(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    (width, height): (u32, u32),
    rgba: &[u8],
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let view_formats: &[wgpu::TextureFormat] = if format == OUTPUT_FORMAT {
        &[wgpu::TextureFormat::Rgba8UnormSrgb]
    } else {
        &[]
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("histogram test codes"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats,
    });
    if usage.contains(wgpu::TextureUsages::COPY_DST) {
        let texel = format
            .block_copy_size(None)
            .expect("an uncompressed format");
        queue.write_texture(
            texture.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * texel),
                rows_per_image: Some(height),
            },
            size,
        );
    }
    texture
}

/// The independent reference's counts of `rect` of a `width`-texel-wide RGBA frame.
fn reference(rgba: &[u8], width: u32, rect: HistogramRect) -> tolerance::Counts {
    let mut pixels = Vec::with_capacity(rect.pixels() as usize * 4);
    for row in rect.y..rect.y + rect.height {
        let start = ((row * width + rect.x) * 4) as usize;
        pixels.extend_from_slice(&rgba[start..start + rect.width as usize * 4]);
    }
    tolerance::Counts::of(&pixels, 4)
}

/// `counts` as the independent comparison holds them, its clipping counters in the order of
/// `tolerance::CLIPPING`, as the release gate's harness maps a report onto them, beside `luma`, a
/// luminance histogram the GPU's counts do not carry.
fn held(counts: &Counts, luma: [u64; 256]) -> tolerance::Counts {
    tolerance::Counts {
        luma,
        bins: [counts.r, counts.g, counts.b],
        clipping: [
            counts.r0,
            counts.g0,
            counts.b0,
            counts.r255,
            counts.g255,
            counts.b255,
            counts.any_shadow,
            counts.any_highlight,
            counts.all_shadow,
            counts.all_highlight,
            counts.both,
        ],
    }
}

/// The GPU's counts against the reference's at a zero tolerance: every bin and every counter equal.
fn assert_exact(gpu: &Counts, expected: &tolerance::Counts, what: &str) {
    // The luminance is the reference's own: what is compared here is the bins and counters.
    let candidate = held(gpu, expected.luma);
    let error = tolerance::histogram(&candidate, expected)
        .unwrap_or_else(|error| panic!("{what}: {error}"));
    assert_eq!(
        (error.bins, error.clipping),
        ([0; 3], [0; tolerance::CLIPPING.len()]),
        "{what}: the GPU's counts differ from the reference's"
    );
    assert_eq!(candidate, *expected, "{what}");
    assert_eq!(gpu.pixels, expected.pixels(), "{what}: the pixels counted");
}

fn command_encoder(device: &wgpu::Device) -> wgpu::CommandEncoder {
    device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("histogram test"),
    })
}

/// The counts once the pipeline's retirement worker has completed the readback.
fn counts_of(readback: &HistogramReadback) -> Counts {
    wait_for("the histogram readback", || readback.poll())
        .unwrap_or_else(|error| panic!("the readback failed: {error}"))
}

/// Clear, then reduce each tile in turn, all in one submission, and read the counts back.
fn reduce_together(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    reduction: &mut HistogramReduction,
    tiles: &[(&wgpu::Texture, HistogramRect)],
) -> Counts {
    let mut encoder = command_encoder(device);
    reduction.clear(&mut encoder);
    for (texture, rect) in tiles {
        reduction
            .reduce(&mut encoder, texture, *rect)
            .unwrap_or_else(|error| panic!("{rect:?}: {error}"));
    }
    counts_of(&reduction.read_back(queue, encoder))
}

/// Clear, then reduce each tile in a submission of its own, and read the counts back in one more.
fn reduce_apart(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    reduction: &mut HistogramReduction,
    tiles: &[(&wgpu::Texture, HistogramRect)],
) -> Counts {
    let mut first = command_encoder(device);
    reduction.clear(&mut first);
    queue.submit([first.finish()]);
    for (texture, rect) in tiles {
        let mut encoder = command_encoder(device);
        reduction
            .reduce(&mut encoder, texture, *rect)
            .unwrap_or_else(|error| panic!("{rect:?}: {error}"));
        queue.submit([encoder.finish()]);
    }
    counts_of(&reduction.read_back(queue, command_encoder(device)))
}

/// Deterministic codes, about one channel in eight at each end so that every endpoint counter is
/// busy, with alpha varied too, which is never read: `pixels` texels of RGBA.
fn noise(pixels: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut rgba = Vec::with_capacity(pixels * 4);
    for _ in 0..pixels {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let code = |shift: u32| {
            let byte = (state >> shift) as u8;
            match byte % 16 {
                0 => 0,
                1 => 255,
                _ => byte,
            }
        };
        rgba.extend_from_slice(&[code(0), code(8), code(16), (state >> 24) as u8]);
    }
    rgba
}

/// The tiles of a `size` stage of `rgba`, each at most `side` texels of its own a side and each in a
/// texture of its own with `halo` texels of [`POISON`] on every side, as a tile renderer's halo is
/// in its texture but is not its own; with each tile's interior rectangle.
fn tiles_of(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rgba: &[u8],
    (width, height): (u32, u32),
    side: u32,
    halo: u32,
) -> Vec<(wgpu::Texture, HistogramRect)> {
    let mut tiles = Vec::new();
    for y0 in (0..height).step_by(side as usize) {
        for x0 in (0..width).step_by(side as usize) {
            let (w, h) = (side.min(width - x0), side.min(height - y0));
            let size = (w + 2 * halo, h + 2 * halo);
            let mut texels = POISON.repeat((size.0 * size.1) as usize);
            for row in 0..h {
                let from = (((y0 + row) * width + x0) * 4) as usize;
                let to = (((halo + row) * size.0 + halo) * 4) as usize;
                texels[to..to + (w * 4) as usize]
                    .copy_from_slice(&rgba[from..from + (w * 4) as usize]);
            }
            let rect = HistogramRect {
                x: halo,
                y: halo,
                width: w,
                height: h,
            };
            tiles.push((codes_texture(device, queue, size, &texels), rect));
        }
    }
    tiles
}

// ---- Without a device -------------------------------------------------------------------------

/// The kernel validates as the stage validates its own passes, and its figures are the bounds the
/// module states: the counts buffer and each staging copy 3,096 bytes, the table 32,776 bytes at
/// the toolkit's 8,192 texels.
#[test]
fn the_histogram_kernel_validates_and_holds_its_stated_bounds() {
    let module = validate(&source()).unwrap_or_else(|error| panic!("{error}"));
    let entry = module
        .entry_points
        .iter()
        .find(|entry| entry.name == ENTRY)
        .expect("the kernel's entry point");
    assert_eq!(entry.workgroup_size, [LANES, LANES, 1]);
    assert_eq!((COUNT_WORDS, COUNT_BYTES), (774, 3_096));
    assert_eq!((SPAN, SWEEPS), (64, 4));
    assert_eq!(table_words(8_192) * 4, 32_776);
    assert!(supported(&wgpu::Limits::default()));
    assert!(!supported(&wgpu::Limits::downlevel_webgl2_defaults()));
}

/// The counters are derived from the endpoint classes as the reducer derives them: a twin of the
/// kernel's counting over every combination of endpoint and inner codes, read as the readback
/// reads the buffer, equals the reference's counts of the same pixels; and counts that do not add
/// up to the pixels reduced, or a buffer of the wrong length, are refused rather than read.
#[test]
fn the_counters_are_derived_from_the_classes_as_the_reducer_derives_them() {
    let codes = [0u8, 1, 128, 254, 255];
    let mut rgba = Vec::new();
    let mut index = 0u8;
    for r in codes {
        for g in codes {
            for b in codes {
                // Each combination a different number of times, so no two counters agree by
                // accident.
                index += 1;
                for _ in 0..index {
                    rgba.extend_from_slice(&[r, g, b, index]);
                }
            }
        }
    }
    let ends = |code: u8| usize::from(code == 0) | (usize::from(code == 255) << 1);
    let mut words = [0u32; COUNT_WORDS];
    for pixel in rgba.chunks_exact(4) {
        for (channel, code) in pixel[..3].iter().enumerate() {
            words[channel * BINS + usize::from(*code)] += 1;
        }
        let (r, g, b) = (ends(pixel[0]), ends(pixel[1]), ends(pixel[2]));
        if r | g | b != 0 {
            words[ANY_WORD + (r | g | b) - 1] += 1;
        }
        if r & g & b != 0 {
            words[ALL_WORD + (r & g & b) - 1] += 1;
        }
    }
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let pixels = (rgba.len() / 4) as u64;
    let counts = Counts::read(&bytes, pixels).expect("the twin's counts add up");
    assert_exact(
        &counts,
        &tolerance::Counts::of(&rgba, 4),
        "every endpoint combination",
    );
    assert_eq!(
        Counts::read(&bytes, pixels + 1),
        Err(HistogramError::Inconsistent {
            channel: 0,
            counted: pixels,
            pixels: pixels + 1,
        })
    );
    assert_eq!(
        Counts::read(&bytes[..COUNT_WORDS * 4 - 4], pixels),
        Err(HistogramError::ReadbackFailed)
    );
}

// ---- On a headless device ---------------------------------------------------------------------

/// Every RGB triple once, 4,096 × 4,096 texels: each code of each channel 65,536 times and every
/// combination of endpoints, over 64 × 64 workgroups. The GPU counts each bin and counter as the
/// reference does, and a second reduction of the same codes counts the same again.
#[test]
fn the_gpu_counts_every_rgb_triple_as_the_reference_does_and_again() {
    let test = "the_gpu_counts_every_rgb_triple_as_the_reference_does_and_again";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let pipeline = own_pipeline(&device, &queue);
    let mut reduction = HistogramReduction::new(&pipeline, &device).expect("the reduction");
    let side = 4_096;
    let rgba: Vec<u8> = (0u32..1 << 24)
        .flat_map(|index| {
            [
                index as u8,
                (index >> 8) as u8,
                (index >> 16) as u8,
                (index >> 5) as u8,
            ]
        })
        .collect();
    let texture = codes_texture(&device, &queue, (side, side), &rgba);
    let whole = HistogramRect::whole(&texture);
    let expected = reference(&rgba, side, whole);
    assert!(expected.bins.iter().flatten().all(|count| *count == 65_536));
    let first = reduce_together(&device, &queue, &mut reduction, &[(&texture, whole)]);
    assert_exact(&first, &expected, "every triple");
    let second = reduce_together(&device, &queue, &mut reduction, &[(&texture, whole)]);
    assert_eq!(first, second, "a second reduction of the same codes");
}

/// Every combination of channel endpoints and inner codes, each a different number of times, in a
/// frame 97 texels wide, wider than a workgroup's 64: each counter as the reference counts it.
#[test]
fn clipped_pixels_of_every_channel_combination_are_counted_exactly() {
    let test = "clipped_pixels_of_every_channel_combination_are_counted_exactly";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let pipeline = own_pipeline(&device, &queue);
    let mut reduction = HistogramReduction::new(&pipeline, &device).expect("the reduction");
    let codes = [0u8, 1, 128, 254, 255];
    let mut rgba = Vec::new();
    let mut index = 0;
    for r in codes {
        for g in codes {
            for b in codes {
                index += 1;
                for _ in 0..index {
                    rgba.extend_from_slice(&[r, g, b, (index * 37) as u8]);
                }
            }
        }
    }
    let width = 97;
    let height = (rgba.len() / 4).div_ceil(width) as u32;
    // The last row's tail: an inner grey, counted like any other texel.
    rgba.resize(width * height as usize * 4, 100);
    let width = width as u32;
    let texture = codes_texture(&device, &queue, (width, height), &rgba);
    let whole = HistogramRect::whole(&texture);
    let counts = reduce_together(&device, &queue, &mut reduction, &[(&texture, whole)]);
    assert_exact(
        &counts,
        &reference(&rgba, width, whole),
        "every combination",
    );
}

/// A rectangle counts its own texels and none outside it, wherever it lies: inside, at the far
/// corner and one texel, each counted as the reference counts that rectangle, while every texel
/// around them is [`POISON`].
#[test]
fn a_rectangle_counts_its_own_texels_and_never_one_outside() {
    let test = "a_rectangle_counts_its_own_texels_and_never_one_outside";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let pipeline = own_pipeline(&device, &queue);
    let mut reduction = HistogramReduction::new(&pipeline, &device).expect("the reduction");
    let (width, height) = (300, 200);
    let inner = noise((width * height) as usize, 7);
    for rect in [
        HistogramRect {
            x: 37,
            y: 23,
            width: 211,
            height: 150,
        },
        HistogramRect {
            x: 250,
            y: 170,
            width: 50,
            height: 30,
        },
        HistogramRect {
            x: 64,
            y: 64,
            width: 1,
            height: 1,
        },
    ] {
        let mut rgba = POISON.repeat((width * height) as usize);
        for row in rect.y..rect.y + rect.height {
            let start = ((row * width + rect.x) * 4) as usize;
            let end = start + rect.width as usize * 4;
            rgba[start..end].copy_from_slice(&inner[start..end]);
        }
        let texture = codes_texture(&device, &queue, (width, height), &rgba);
        let counts = reduce_together(&device, &queue, &mut reduction, &[(&texture, rect)]);
        assert_exact(
            &counts,
            &reference(&rgba, width, rect),
            &format!("{rect:?}"),
        );
    }
}

/// A 517 × 389 stage in tiles of at most 160 × 160 texels, each in its own texture with a halo of
/// [`POISON`], reduced into one cleared reduction: in one submission in order, and in reverse order
/// a submission each, the sum of the tiles is the whole stage's counts, as the reference counts
/// them and as the GPU counts the stage in one texture, whatever the order.
#[test]
fn a_stage_in_tiles_counts_what_the_whole_stage_does_in_any_order() {
    let test = "a_stage_in_tiles_counts_what_the_whole_stage_does_in_any_order";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let pipeline = own_pipeline(&device, &queue);
    let mut reduction = HistogramReduction::new(&pipeline, &device).expect("the reduction");
    let (width, height) = (517, 389);
    let rgba = noise((width * height) as usize, 11);
    let expected = reference(
        &rgba,
        width,
        HistogramRect {
            x: 0,
            y: 0,
            width,
            height,
        },
    );
    let tiles = tiles_of(&device, &queue, &rgba, (width, height), 160, 7);
    assert_eq!(tiles.len(), 12);
    let forward: Vec<_> = tiles
        .iter()
        .map(|(texture, rect)| (texture, *rect))
        .collect();
    let backward: Vec<_> = forward.iter().rev().copied().collect();
    let together = reduce_together(&device, &queue, &mut reduction, &forward);
    assert_exact(&together, &expected, "the tiles in one submission");
    let apart = reduce_apart(&device, &queue, &mut reduction, &backward);
    assert_exact(&apart, &expected, "the tiles in reverse, a submission each");
    let stage = codes_texture(&device, &queue, (width, height), &rgba);
    let whole = reduce_together(
        &device,
        &queue,
        &mut reduction,
        &[(&stage, HistogramRect::whole(&stage))],
    );
    assert_eq!(together, whole);
    assert_eq!(apart, whole);
}

/// A texture as wide, then as tall, as the device allows — 8,192 texels under the default limits,
/// the toolkit's — counts every texel: the table's last value names its width or height.
#[test]
fn a_texture_at_the_device_limit_counts_every_texel() {
    let test = "a_texture_at_the_device_limit_counts_every_texel";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let pipeline = own_pipeline(&device, &queue);
    let mut reduction = HistogramReduction::new(&pipeline, &device).expect("the reduction");
    let limit = device.limits().max_texture_dimension_2d;
    for size in [(limit, 3), (3, limit)] {
        let rgba = noise((size.0 * size.1) as usize, u64::from(size.0));
        let texture = codes_texture(&device, &queue, size, &rgba);
        let whole = HistogramRect::whole(&texture);
        let counts = reduce_together(&device, &queue, &mut reduction, &[(&texture, whole)]);
        assert_exact(
            &counts,
            &reference(&rgba, size.0, whole),
            &format!("{size:?}"),
        );
    }
}

/// The reduction's buffers are charged to the GPU-preview budget before they are created and a
/// readback's staging copy while the GPU holds it; both return to zero through the retirement
/// worker; and a budget that cannot hold the reduction, or its staging copy, refuses it by name.
#[test]
fn the_reduction_is_charged_to_the_budget_and_released() {
    let test = "the_reduction_is_charged_to_the_budget_and_released";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let pipeline = own_pipeline(&device, &queue);
    let preview = &pipeline.figures.preview;
    let limit = device.limits().max_texture_dimension_2d;
    let charged = table_words(limit) * 4 + RECT_BYTES + COUNT_BYTES;
    assert_eq!(preview.in_use(), 0);
    let mut reduction = HistogramReduction::new(&pipeline, &device).expect("the reduction");
    assert_eq!(reduction.charged_bytes(), charged);
    assert_eq!(preview.in_use(), charged);
    let rgba = noise(64 * 64, 3);
    let texture = codes_texture(&device, &queue, (64, 64), &rgba);
    let whole = HistogramRect::whole(&texture);
    reduce_together(&device, &queue, &mut reduction, &[(&texture, whole)]);
    // The staging copy was charged beside the buffers, and leaves once the GPU is done with it.
    settle(&pipeline);
    assert_eq!(preview.peak(), charged + COUNT_BYTES);
    assert_eq!(preview.in_use(), charged);
    drop(reduction);
    settle(&pipeline);
    assert_eq!(preview.in_use(), 0);
    // A budget a byte short of the buffers: refused before anything is created.
    pipeline.set_gpu_budget(charged - 1);
    assert_eq!(
        HistogramReduction::new(&pipeline, &device).err(),
        Some(HistogramError::BudgetExceeded {
            requested: charged,
            in_use: 0,
            budget: charged - 1,
        })
    );
    assert_eq!(preview.in_use(), 0);
    // A budget that holds the buffers but not a staging copy: the readback answers why, at once.
    pipeline.set_gpu_budget(charged + COUNT_BYTES - 1);
    let mut reduction = HistogramReduction::new(&pipeline, &device).expect("the reduction");
    let mut encoder = command_encoder(&device);
    reduction.clear(&mut encoder);
    reduction
        .reduce(&mut encoder, &texture, whole)
        .expect("the tile");
    let readback = reduction.read_back(&queue, encoder);
    assert_eq!(
        readback.poll(),
        Some(Err(HistogramError::BudgetExceeded {
            requested: COUNT_BYTES,
            in_use: charged,
            budget: charged + COUNT_BYTES - 1,
        }))
    );
    drop(reduction);
    settle(&pipeline);
    assert_eq!(preview.in_use(), 0);
}

/// Nothing is counted from a texture whose texels are not exactly output codes, or from a
/// rectangle outside its texture or past the pixel limit: each is refused by name with nothing
/// encoded, so the reduction's next readback counts only the tile it was given.
#[test]
fn what_cannot_be_counted_exactly_is_refused_and_encodes_nothing() {
    let test = "what_cannot_be_counted_exactly_is_refused_and_encodes_nothing";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let pipeline = own_pipeline(&device, &queue);
    let mut reduction = HistogramReduction::new(&pipeline, &device).expect("the reduction");
    let size = (40, 30);
    let rgba = noise(40 * 30, 5);
    let codes = codes_texture(&device, &queue, size, &rgba);
    let whole = HistogramRect::whole(&codes);
    let srgb = texture_of(
        &device,
        &queue,
        size,
        &rgba,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let half = texture_of(
        &device,
        &queue,
        size,
        &vec![0; 40 * 30 * 8],
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let unbound = texture_of(
        &device,
        &queue,
        size,
        &rgba,
        OUTPUT_FORMAT,
        wgpu::TextureUsages::COPY_DST,
    );
    let mut encoder = command_encoder(&device);
    reduction.clear(&mut encoder);
    for (what, texture) in [
        ("an sRGB-typed texture", &srgb),
        ("a float texture", &half),
        ("a texture that cannot be bound", &unbound),
    ] {
        assert!(
            matches!(
                reduction.reduce(&mut encoder, texture, whole),
                Err(HistogramError::Texture(_))
            ),
            "{what}"
        );
    }
    for rect in [
        HistogramRect { width: 41, ..whole },
        HistogramRect { y: 1, ..whole },
        HistogramRect {
            x: u32::MAX,
            y: 0,
            width: 2,
            height: 1,
        },
    ] {
        assert_eq!(
            reduction.reduce(&mut encoder, &codes, rect),
            Err(HistogramError::Rectangle {
                rect,
                width: 40,
                height: 30,
            })
        );
    }
    let empty = HistogramRect { width: 0, ..whole };
    assert_eq!(reduction.reduce(&mut encoder, &codes, empty), Ok(()));
    assert_eq!(reduction.pixels(), 0);
    reduction.pixels = MAX_PIXELS - 10;
    assert_eq!(
        reduction.reduce(&mut encoder, &codes, whole),
        Err(HistogramError::PixelLimit {
            pixels: MAX_PIXELS - 10 + 1_200,
        })
    );
    reduction.pixels = 0;
    reduction
        .reduce(&mut encoder, &codes, whole)
        .expect("the tile");
    let counts = counts_of(&reduction.read_back(&queue, encoder));
    assert_exact(&counts, &reference(&rgba, 40, whole), "the one tile given");
}

/// A clear that never reached the GPU leaves the last reduction's counts in the buffer: the next
/// readback's bins then do not add up to the pixels reduced since the clear, and it answers that,
/// never the counts.
#[test]
fn counts_that_do_not_add_up_are_refused_never_read() {
    let test = "counts_that_do_not_add_up_are_refused_never_read";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let pipeline = own_pipeline(&device, &queue);
    let mut reduction = HistogramReduction::new(&pipeline, &device).expect("the reduction");
    let rgba = noise(32 * 32, 9);
    let texture = codes_texture(&device, &queue, (32, 32), &rgba);
    let whole = HistogramRect::whole(&texture);
    let first = reduce_together(&device, &queue, &mut reduction, &[(&texture, whole)]);
    assert_exact(&first, &reference(&rgba, 32, whole), "the first reduction");
    let mut abandoned = command_encoder(&device);
    reduction.clear(&mut abandoned);
    drop(abandoned);
    let mut encoder = command_encoder(&device);
    reduction
        .reduce(&mut encoder, &texture, whole)
        .expect("the tile");
    let readback = reduction.read_back(&queue, encoder);
    assert_eq!(
        wait_for("the histogram readback", || readback.poll()),
        Err(HistogramError::Inconsistent {
            channel: 0,
            counted: 2 * 1_024,
            pixels: 1_024,
        })
    );
}

/// Once the device is lost nothing is counted on it: a tile and a readback are refused, the
/// readback's handle answering the loss at once, and no reduction is created. A readback finished
/// before the loss still answers its own counts.
#[test]
fn a_lost_device_answers_an_error_never_counts() {
    let test = "a_lost_device_answers_an_error_never_counts";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let pipeline = own_pipeline(&device, &queue);
    let mut reduction = HistogramReduction::new(&pipeline, &device).expect("the reduction");
    let rgba = noise(16 * 16, 13);
    let texture = codes_texture(&device, &queue, (16, 16), &rgba);
    let whole = HistogramRect::whole(&texture);
    let mut encoder = command_encoder(&device);
    reduction.clear(&mut encoder);
    reduction
        .reduce(&mut encoder, &texture, whole)
        .expect("the tile");
    let finished = reduction.read_back(&queue, encoder);
    let counts = counts_of(&finished);
    pipeline.simulate_device_loss();
    assert_eq!(finished.poll(), Some(Ok(counts)));
    let mut encoder = command_encoder(&device);
    reduction.clear(&mut encoder);
    assert_eq!(
        reduction.reduce(&mut encoder, &texture, whole),
        Err(HistogramError::DeviceLost)
    );
    let readback = reduction.read_back(&queue, encoder);
    assert_eq!(readback.poll(), Some(Err(HistogramError::DeviceLost)));
    assert_eq!(
        HistogramReduction::new(&pipeline, &device).err(),
        Some(HistogramError::DeviceLost)
    );
}

/// A launch that refused the GPU stage has no reduction either: the counts stay the reference's.
#[test]
fn a_refused_stage_has_no_reduction() {
    let test = "a_refused_stage_has_no_reduction";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let refused = PhotoPipeline::with_stage(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        Arc::default(),
        true,
    );
    assert_eq!(
        HistogramReduction::new(&refused, &device).err(),
        Some(HistogramError::Unavailable(GpuStageState::NoAdapter {
            refused: true
        }))
    );
    assert_eq!(refused.figures.preview.in_use(), 0);
}
