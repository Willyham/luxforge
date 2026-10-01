//! The one parallel gate every rendering pass asks: whether a pass of a given kind over a given
//! number of pixels runs on the shared Rayon pool, from that kind's measured threshold in
//! `luxforge_raw`'s limits module ([`luxforge_raw::parallel_pixels`]).
//!
//! Which way a pass runs changes how its rows or tiles are scheduled, never its arithmetic: every
//! pass writes the same bytes serially and pooled. A test build can force either way on the thread
//! that asks ([`force`]), which is how the exactness tests compare the two at one size and how the
//! break-even measurement times them.

pub(crate) use luxforge_raw::RenderPass;

/// Whether a `pass` over `pixels` runs on the shared Rayon pool: at and past its kind's threshold.
#[cfg(not(test))]
pub(crate) fn pooled(pass: RenderPass, pixels: u64) -> bool {
    pixels >= luxforge_raw::parallel_pixels(pass)
}

/// Whether a `pass` over `pixels` runs on the shared Rayon pool: at and past its kind's threshold,
/// unless this thread forced one way ([`force`]).
#[cfg(test)]
pub(crate) fn pooled(pass: RenderPass, pixels: u64) -> bool {
    FORCED
        .get()
        .unwrap_or_else(|| pixels >= luxforge_raw::parallel_pixels(pass))
}

#[cfg(test)]
thread_local! {
    static FORCED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Force every gate this thread asks to pool (`Some(true)`) or to run serially (`Some(false)`), or
/// return it to the thresholds (`None`), for a test; returns the previous setting. The setting
/// belongs to the thread, so one test's never reaches a render another test runs beside it; every
/// gate is asked on the thread that asked for the frame or the proxy, and a spatial tile's own
/// passes follow the gate its batch was given.
#[cfg(test)]
pub(crate) fn force(pooled: Option<bool>) -> Option<bool> {
    FORCED.replace(pooled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BASIC_EFFECT, CROP_EFFECT, CropStage, EFFECT_FORMAT, Layer, LayerId, LinearImage,
        LinearSettings, ModuleRegistry, PRESENCE_EFFECT, PreviewSource, ProxyBounds, ProxyPlan,
        RECIPE_FORMAT, Recipe, RenderContext, RenderOptions, SnapshotId, Transform,
        render::testing::{frame_in, linear},
        render::tests::{fitted_crop, gradient, turn},
    };
    use serde_json::{Value, json};

    fn layer(effect_id: &str, payload: Value) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: effect_id.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn recipe(layers: Vec<Layer>) -> Recipe {
        Recipe {
            format: RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        }
    }

    fn exposure() -> Layer {
        layer(BASIC_EFFECT, json!({"exposure": 1.0}))
    }

    /// Basic with every field non-neutral: four colour units in one pass, a heavy colour pass.
    fn full_basic() -> Layer {
        layer(
            BASIC_EFFECT,
            json!({
                "exposure": 0.5, "contrast": 25.0, "highlights": -30.0, "shadows": 30.0,
                "whites": -15.0, "blacks": 15.0, "temperature": 20.0, "tint": -10.0,
                "vibrance": 30.0, "saturation": 15.0,
            }),
        )
    }

    fn presence(payload: Value) -> Layer {
        layer(PRESENCE_EFFECT, payload)
    }

    /// A straightened crop of a `width` × `height` stage through the real crop module: one
    /// interpolating resample, and the pixels of the output it writes.
    fn straightened_crop(width: u32, height: u32) -> (Layer, u64) {
        let crop = fitted_crop(width, height, 10.0, [0.1, 0.1, 0.8, 0.8]);
        let rect = crop
            .output_rect(&CropStage {
                width,
                height,
                angle: 10.0,
            })
            .unwrap();
        (
            layer(CROP_EFFECT, serde_json::to_value(crop).unwrap()),
            u64::from(rect.width) * u64::from(rect.height),
        )
    }

    fn linear_gradient(width: u32, height: u32) -> LinearImage {
        let mut planes = Vec::with_capacity((width * height * 3) as usize);
        for channel in 0..3 {
            for y in 0..height {
                for x in 0..width {
                    planes
                        .push(((x * 7 + y * 13 + channel * 29) % 251) as f32 / 251.0 * 0.9 + 0.02);
                }
            }
        }
        LinearImage::with_fingerprint(width, height, planes, "sha256:parallel-linear").unwrap()
    }

    /// A 3:2 stage of about `megapixels`.
    fn stage_of(megapixels: f64) -> (u32, u32) {
        let height = (megapixels * 1e6 / 1.5).sqrt().round() as u32;
        (height * 3 / 2, height)
    }

    fn pixels((width, height): (u32, u32)) -> u64 {
        u64::from(width) * u64::from(height)
    }

    /// The proxy plan a box downscale of a `width` × `height` source to a third of each side
    /// reads: every source pixel once.
    fn third(width: u32, height: u32) -> ProxyPlan {
        ProxyPlan::fit(
            (width, height),
            (width, height),
            ProxyBounds {
                width: width / 3,
                height: height / 3,
            },
        )
        .unwrap()
    }

    /// Where a case's pixels come from.
    #[derive(Clone, Copy)]
    enum Domain {
        Byte,
        Linear,
    }

    /// What one case runs: a render of the recipe, or a proxy build.
    enum Work {
        Render(Vec<Layer>),
        /// Synthetic frozen terms isolate the nonlinear kernel from profile lookup.
        Warp(Vec<Layer>),
        Proxy,
    }

    /// What one run of a case wrote.
    enum Output {
        Frame(crate::Raster),
        Proxy(PreviewSource),
    }

    impl Output {
        /// Every byte the run wrote: the frame's, or the proxy source's codes or float bits.
        fn bytes(&self) -> Vec<u8> {
            match self {
                Self::Frame(raster) => raster.rgba.to_vec(),
                Self::Proxy(PreviewSource::Jpeg(image)) => image.rgba.to_vec(),
                Self::Proxy(PreviewSource::Raw { image, .. }) => image
                    .planes()
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect(),
            }
        }
    }

    /// One case at a stage: the pixels its gate counts, and something that runs it once.
    struct Run {
        counted: u64,
        once: Box<dyn Fn() -> Output>,
    }

    fn run(domain: Domain, (width, height): (u32, u32), work: Work, counted: u64) -> Run {
        let registry = match &work {
            Work::Warp(_) => crate::render::warp_tests::registry(),
            _ => ModuleRegistry::builtin(),
        };
        let context = RenderContext::new();
        let once: Box<dyn Fn() -> Output> = match (work, domain) {
            (Work::Render(layers) | Work::Warp(layers), Domain::Byte) => {
                let source = gradient(width, height);
                let stack = recipe(layers);
                Box::new(move || {
                    let options = RenderOptions::default();
                    let frame = frame_in(
                        &context,
                        &registry,
                        &source,
                        SnapshotId::new(),
                        &stack,
                        options,
                    );
                    Output::Frame(frame.unwrap())
                })
            }
            (Work::Render(layers) | Work::Warp(layers), Domain::Linear) => {
                let source = linear_gradient(width, height);
                let stack = recipe(layers);
                Box::new(move || {
                    let source = linear(&source, LinearSettings::default());
                    let options = RenderOptions::default();
                    let frame = frame_in(
                        &context,
                        &registry,
                        source,
                        SnapshotId::new(),
                        &stack,
                        options,
                    );
                    Output::Frame(frame.unwrap())
                })
            }
            (Work::Proxy, domain) => {
                let source = match domain {
                    Domain::Byte => PreviewSource::Jpeg(gradient(width, height)),
                    Domain::Linear => PreviewSource::Raw {
                        image: linear_gradient(width, height),
                        settings: LinearSettings::default(),
                    },
                };
                let plan = third(width, height);
                Box::new(move || Output::Proxy(source.proxy(plan).unwrap()))
            }
        };
        Run { counted, once }
    }

    /// One pass kind's single-unit case at a stage of about `megapixels`, as the gate counts it.
    fn case(label: &str, megapixels: f64) -> Run {
        let stage = stage_of(megapixels);
        let whole = pixels(stage);
        let render = |domain, layers| run(domain, stage, Work::Render(layers), whole);
        let lens = || {
            layer(
                "luxforge.lens.distortion",
                json!({"model":"ptlens","terms":[0.019,-0.056,0.063],"unit":1.0}),
            )
        };
        let perspective = || {
            layer(
                "luxforge.perspective",
                json!({"horizontal":35,"vertical":-25}),
            )
        };
        match label {
            "segment: quarter turn" => render(Domain::Byte, vec![turn(Transform::RotateRight)]),
            "segment: exposure +1 EV" => render(Domain::Byte, vec![exposure()]),
            "segment: exposure +1 EV, linear" => render(Domain::Linear, vec![exposure()]),
            "heavy colour: full Basic" => render(Domain::Byte, vec![full_basic()]),
            "heavy colour: full Basic, linear" => render(Domain::Linear, vec![full_basic()]),
            "resample: 10 degree crop" => {
                // The crop writes about 0.43 of its input's pixels; the input is sized so that the
                // output, which the gate counts, is about the target.
                let input = stage_of(megapixels / 0.43);
                let (crop, output) = straightened_crop(input.0, input.1);
                run(Domain::Byte, input, Work::Render(vec![crop]), output)
            }
            "warp: lens, byte" => run(Domain::Byte, stage, Work::Warp(vec![lens()]), whole),
            "warp: lens, linear" => run(Domain::Linear, stage, Work::Warp(vec![lens()]), whole),
            "warp: perspective, byte" => render(Domain::Byte, vec![perspective()]),
            "warp: perspective, linear" => render(Domain::Linear, vec![perspective()]),
            "warp: lens + perspective + crop, byte" | "warp: lens + perspective + crop, linear" => {
                let input = stage_of(megapixels / 0.43);
                let (crop, output) = straightened_crop(input.0, input.1);
                let domain = if label.ends_with("linear") {
                    Domain::Linear
                } else {
                    Domain::Byte
                };
                run(
                    domain,
                    input,
                    Work::Warp(vec![lens(), perspective(), crop]),
                    output,
                )
            }
            "spatial: texture +100" => {
                render(Domain::Byte, vec![presence(json!({"texture": 100.0}))])
            }
            "spatial: clarity +100" => {
                render(Domain::Byte, vec![presence(json!({"clarity": 100.0}))])
            }
            "spatial: dehaze +100" => {
                render(Domain::Byte, vec![presence(json!({"dehaze": 100.0}))])
            }
            "spatial: all three +100" => render(
                Domain::Byte,
                vec![presence(
                    json!({"texture": 100.0, "clarity": 100.0, "dehaze": 100.0}),
                )],
            ),
            "spatial: clarity +100, linear" => {
                render(Domain::Linear, vec![presence(json!({"clarity": 100.0}))])
            }
            "proxy: JPEG to a third" => run(Domain::Byte, stage, Work::Proxy, whole),
            "proxy: RAW to a third" => run(Domain::Linear, stage, Work::Proxy, whole),
            _ => unreachable!("{label}"),
        }
    }

    /// Every pass kind at a size its own threshold pools and the one-megapixel threshold every pass
    /// shared before did not (256 Ki pixels for a heavy colour pass), on both pixel domains where
    /// the pass has one: the pooled run and a serial run write the same bytes.
    #[test]
    fn slow_a_pass_between_the_shared_and_its_own_threshold_pools_to_the_serial_bytes() {
        let (crop, crop_output) = straightened_crop(720, 480);
        let presence_all = presence(json!({"clarity": 60.0, "texture": 40.0, "dehaze": 30.0}));
        let cases = [
            (
                RenderPass::Transform,
                (900, 600),
                Work::Render(vec![turn(Transform::RotateRight)]),
            ),
            (
                RenderPass::Colour,
                (450, 300),
                Work::Render(vec![exposure()]),
            ),
            (
                RenderPass::HeavyColour,
                (240, 160),
                Work::Render(vec![full_basic()]),
            ),
            (RenderPass::Resample, (720, 480), Work::Render(vec![crop])),
            (
                RenderPass::Spatial,
                (660, 440),
                Work::Render(vec![presence_all]),
            ),
            (RenderPass::Proxy, (900, 600), Work::Proxy),
        ];
        for (pass, stage, work) in cases {
            let counted = match pass {
                RenderPass::Resample => crop_output,
                _ => pixels(stage),
            };
            let shared = match pass {
                RenderPass::HeavyColour => 256 * 1024,
                _ => luxforge_raw::PARALLEL_PIXELS,
            };
            assert!(
                pooled(pass, counted) && counted < shared,
                "{pass:?}: {counted} pixels lie between the thresholds"
            );
            let layers = match &work {
                Work::Render(layers) | Work::Warp(layers) => Some(layers.clone()),
                Work::Proxy => None,
            };
            for domain in [Domain::Byte, Domain::Linear] {
                let work = match &layers {
                    Some(layers) => Work::Render(layers.clone()),
                    None => Work::Proxy,
                };
                let Run { once, .. } = run(domain, stage, work, counted);
                let pooled = once().bytes();
                force(Some(false));
                let serial = once().bytes();
                force(None);
                assert!(
                    pooled == serial,
                    "{pass:?} over {counted} pixels: pooled and serial differ"
                );
            }
        }
    }

    const CASES: [&str; 19] = [
        "segment: quarter turn",
        "segment: exposure +1 EV",
        "segment: exposure +1 EV, linear",
        "heavy colour: full Basic",
        "heavy colour: full Basic, linear",
        "resample: 10 degree crop",
        "warp: lens, byte",
        "warp: lens, linear",
        "warp: perspective, byte",
        "warp: perspective, linear",
        "warp: lens + perspective + crop, byte",
        "warp: lens + perspective + crop, linear",
        "spatial: texture +100",
        "spatial: clarity +100",
        "spatial: dehaze +100",
        "spatial: all three +100",
        "spatial: clarity +100, linear",
        "proxy: JPEG to a third",
        "proxy: RAW to a third",
    ];

    /// Each rendering pass kind's single-unit case, serial against pooled at 0.025 to 2 megapixels:
    /// the measurement each threshold in the limits module cites. The two ways alternate sample by
    /// sample so host load moves both alike; each figure is the p50 over the samples, with the
    /// process's CPU time over each run as a percentage of one core.
    /// `LUXFORGE_BREAK_EVEN_CASE` narrows it to the cases whose label contains it, and
    /// `LUXFORGE_BREAK_EVEN_SAMPLES` sets the samples per way (21).
    #[test]
    #[ignore = "measurement, run explicitly in release"]
    fn parallel_break_even_per_pass() {
        let samples: usize = std::env::var("LUXFORGE_BREAK_EVEN_SAMPLES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(21);
        let filter = std::env::var("LUXFORGE_BREAK_EVEN_CASE").unwrap_or_default();
        let mut sampler = luxforge_process::Sampler::new();
        for label in CASES.into_iter().filter(|label| label.contains(&filter)) {
            for megapixels in [0.025, 0.05, 0.1, 0.25, 0.5, 0.75, 1.0, 2.0] {
                let Run { counted, once } = case(label, megapixels);
                // Warm both ways: the allocator, the estimate store and the tables.
                for way in [false, true] {
                    force(Some(way));
                    once();
                }
                let mut timings: [Vec<f64>; 4] = Default::default();
                for _ in 0..samples {
                    for (index, way) in [false, true].into_iter().enumerate() {
                        force(Some(way));
                        let before = sampler.read().cpu_time_ns.expect("this process's CPU time");
                        let started = std::time::Instant::now();
                        drop(once());
                        let elapsed = started.elapsed();
                        let after = sampler.read().cpu_time_ns.expect("this process's CPU time");
                        timings[index].push(elapsed.as_secs_f64() * 1000.0);
                        timings[index + 2]
                            .push((after - before) as f64 / elapsed.as_nanos() as f64 * 100.0);
                    }
                }
                force(None);
                let [serial, pooled, serial_cpu, pooled_cpu] =
                    timings.map(|values| luxforge_testbase::Distribution::of(values).unwrap());
                println!(
                    "{label} | {:.3} MP | serial {:.2} ms (min {:.2}, {:.0}%) | pooled {:.2} ms \
                     (min {:.2}, {:.0}%) | pooled/serial {:.2} (min {:.2})",
                    counted as f64 / 1e6,
                    serial.p50,
                    serial.min,
                    serial_cpu.p50,
                    pooled.p50,
                    pooled.min,
                    pooled_cpu.p50,
                    pooled.p50 / serial.p50,
                    pooled.min / serial.min,
                );
            }
        }
    }
}
