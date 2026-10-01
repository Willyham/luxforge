//! One bounded processed restoration prefix, shared across downstream proxy edits.
use super::{
    Byte, Evaluation, Raster, Render, RenderSource,
    byte::{self, ByteFrame},
    linear::{self, Linear},
    pipeline::input_prefix_key,
    spatial::prefix_hash,
};
use crate::{Error, ModuleRegistry, Recipe, Region, SnapshotId, Stage, proxy::ProxyKey};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::Arc};

pub(crate) const RESTORATION_PREFIX_MAX_BYTES: usize = 128 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrefixUse {
    Built,
    Reused,
    TooLarge,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct RestorationPrefixKey {
    proxy: ProxyKey,
    prefix_hash: String,
    domain: String,
    wide: bool,
    held: Region,
}
#[derive(Clone)]
enum PrefixPixels {
    Byte(ByteFrame),
    Linear(Arc<Vec<f32>>),
}
struct RestorationPrefixFrame {
    key: RestorationPrefixKey,
    held: Region,
    pixels: PrefixPixels,
    bytes: usize,
}
#[derive(Default)]
pub(crate) struct RestorationPrefixCache {
    frame: Option<RestorationPrefixFrame>,
}
impl RestorationPrefixCache {
    pub(crate) fn clear(&mut self) {
        self.frame = None;
    }
    /// A job that needs no proxy still releases the previous source's processed pixels.
    pub(crate) fn clear_unless_source(&mut self, identity: &crate::proxy::ProxyIdentity) {
        if self
            .frame
            .as_ref()
            .is_some_and(|frame| &frame.key.proxy.identity != identity)
        {
            self.clear();
        }
    }
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.frame.as_ref().map_or(0, |frame| frame.bytes)
    }
}
impl Render<'_> {
    /// Render a proxy from the cached leading restoration output; a failure releases the cache.
    pub(crate) fn frame_with_restoration_cache(
        &self,
        snapshot_id: SnapshotId,
        registry: &ModuleRegistry,
        recipe: &Recipe,
        proxy: &ProxyKey,
        cache: &mut RestorationPrefixCache,
    ) -> Result<(Raster, Option<PrefixUse>), Error> {
        let result = self.cached_frame(snapshot_id, registry, recipe, proxy, cache);
        if result.is_err() {
            cache.clear();
        }
        result
    }
    fn cached_frame(
        &self,
        snapshot_id: SnapshotId,
        registry: &ModuleRegistry,
        recipe: &Recipe,
        proxy: &ProxyKey,
        cache: &mut RestorationPrefixCache,
    ) -> Result<(Raster, Option<PrefixUse>), Error> {
        self.options.cancel.check()?;
        let Some(boundary) = self.compiled.restoration_boundary() else {
            cache.clear();
            return self.frame(snapshot_id).map(|r| (r, None));
        };
        let count = registry.restoration_prefix(&recipe.layers);
        if count == 0 {
            cache.clear();
            return self.frame(snapshot_id).map(|r| (r, None));
        }
        let hash = prefix_hash(
            &recipe.layers[..count],
            &recipe.masks,
            self.options.phase.sampling(),
        )?;
        let wide = byte::byte_frame_widths(&self.compiled)[boundary].input;
        let domain = match self.source {
            RenderSource::Byte(source) => input_prefix_key(&Byte(source), &hash).into_owned(),
            RenderSource::Linear { image, settings } => {
                input_prefix_key(&Linear::new(image, settings)?, &hash).into_owned()
            }
        };
        // Multiple restoration boundaries can retain different later windows even when their
        // backwards tile walk reaches the same source window. The source key alone cannot name
        // the pixels at this boundary: include its whole-stage origin and dimensions.
        let previous = &self.compiled.segments[boundary - 1];
        let stage = previous.stage();
        let held = Region {
            x0: previous.output_origin.0,
            y0: previous.output_origin.1,
            width: stage.width,
            height: stage.height,
        };
        let key = RestorationPrefixKey {
            proxy: proxy.clone(),
            prefix_hash: hash,
            domain,
            wide,
            held,
        };
        let per_pixel = match self.source {
            RenderSource::Byte(_) => {
                if wide {
                    6
                } else {
                    4
                }
            }
            RenderSource::Linear { .. } => 12,
        };
        let bytes = usize::try_from(held.pixels())
            .ok()
            .and_then(|n| n.checked_mul(per_pixel))
            .ok_or_else(|| Error::resource_limit("restoration prefix byte length overflow"))?;
        if bytes > RESTORATION_PREFIX_MAX_BYTES {
            cache.clear();
            return self
                .frame(snapshot_id)
                .map(|r| (r, Some(PrefixUse::TooLarge)));
        }
        let reused = cache.frame.as_ref().is_some_and(|frame| frame.key == key);
        if !reused {
            cache.clear();
            let pixels = match self.source {
                RenderSource::Byte(source) => {
                    let (frame, received) = byte::frames(
                        source,
                        &self.compiled,
                        &self.options.cancel,
                        self.options.tiling,
                        self.context,
                        Some(boundary),
                        None,
                    )?;
                    debug_assert_eq!(received, stage);
                    debug_assert_eq!(frame.bytes(), bytes);
                    PrefixPixels::Byte(frame)
                }
                RenderSource::Linear { image, settings } => {
                    let evaluation = Evaluation::frames_prefix(
                        Linear::new(image, settings)?,
                        Cow::Borrowed(&self.compiled),
                        self.options.tiling,
                        &self.options.cancel,
                        self.context,
                        boundary,
                    )?;
                    let frame = evaluation
                        .frame
                        .ok_or_else(|| Error::internal("restoration prefix produced no frame"))?;
                    PrefixPixels::Linear(frame.planes)
                }
            };
            self.options.cancel.check()?;
            cache.frame = Some(RestorationPrefixFrame {
                key,
                held,
                pixels,
                bytes,
            });
        }
        let frame = cache.frame.as_ref().expect("built or reused prefix");
        debug_assert_eq!(frame.bytes, bytes);
        let stage = Stage {
            width: frame.held.width,
            height: frame.held.height,
        };
        let raster = match (self.source, &frame.pixels) {
            (RenderSource::Byte(source), PrefixPixels::Byte(pixels)) => byte::rasterize_suffix(
                source,
                &self.compiled,
                snapshot_id,
                &self.options.cancel,
                self.options.tiling,
                self.context,
                Some((boundary, pixels.clone(), stage)),
            )?,
            (RenderSource::Linear { image, settings }, PrefixPixels::Linear(pixels)) => {
                let evaluation = Evaluation::frames_suffix(
                    Linear::new(image, settings)?,
                    Cow::Borrowed(&self.compiled),
                    self.options.tiling,
                    &self.options.cancel,
                    self.context,
                    boundary,
                    pixels.clone(),
                )?;
                linear::rasterize(&evaluation, snapshot_id, &self.options.cancel, self.context)?
            }
            _ => return Err(Error::internal("restoration prefix domain changed")),
        };
        Ok((
            raster,
            Some(if reused {
                PrefixUse::Reused
            } else {
                PrefixUse::Built
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BASIC_EFFECT, Cancel, DETAIL_EFFECT, Layer, LinearImage, LinearSettings, PreviewSource,
        ProxyBounds, ProxyPlan, RenderOptions,
    };
    use serde_json::json;
    fn key(source: &PreviewSource) -> ProxyKey {
        let (width, height) = source.dimensions();
        ProxyKey {
            identity: source.identity(),
            plan: ProxyPlan {
                width,
                height,
                bounds: ProxyBounds { width, height },
                window: None,
            },
        }
    }
    fn stack(exposure: f64) -> Recipe {
        Recipe {
            layers: vec![
                Layer::new(DETAIL_EFFECT, json!({"sharpening":40.0,"luminance":25.0})),
                Layer::new(BASIC_EFFECT, json!({"exposure":exposure})),
            ],
            ..Recipe::default()
        }
    }
    fn sources() -> [PreviewSource; 2] {
        let jpeg = super::super::tests::gradient(49, 37);
        let plane = 49 * 37;
        let pixels: Vec<_> = (0..plane * 3)
            .map(|i| ((i * 137) % 997) as f32 / 997.0)
            .collect();
        [
            PreviewSource::Jpeg(jpeg),
            PreviewSource::Raw {
                image: LinearImage::new(49, 37, pixels).unwrap(),
                settings: LinearSettings::default(),
            },
        ]
    }
    fn assert_cached(
        registry: &ModuleRegistry,
        source: &PreviewSource,
        recipe: &Recipe,
        key: &ProxyKey,
        cache: &mut RestorationPrefixCache,
        expected: Option<PrefixUse>,
    ) {
        let context = crate::RenderContext::new();
        let render = super::super::render(
            registry,
            source.input(),
            recipe,
            RenderOptions::default(),
            &context,
        )
        .unwrap();
        let (cached, use_) = render
            .frame_with_restoration_cache(SnapshotId::new(), registry, recipe, key, cache)
            .unwrap();
        assert_eq!(use_, expected);
        assert_eq!(cached.rgba, render.frame(SnapshotId::new()).unwrap().rgba);
    }
    #[test]
    fn restoration_prefix_cache_masked_output_and_every_input_identity_match_uncached() {
        let registry = ModuleRegistry::developer();
        for mut source in sources() {
            let mut mask = crate::Mask::new("Restoration cache mask");
            mask.components.push(crate::Component::new(
                "Linear",
                crate::ComponentMode::Add,
                "linear",
                json!({"x0":0.1,"y0":0.1,"x1":0.9,"y1":0.9}),
            ));
            let mut recipe = stack(0.5);
            recipe.layers[0].mask = Some(mask.id.clone());
            recipe.layers.insert(0, Layer::pixel(5, 4, [11, 22, 33]));
            recipe.masks.push(mask);
            let mut key = key(&source);
            let mut cache = RestorationPrefixCache::default();
            assert_cached(
                &registry,
                &source,
                &recipe,
                &key,
                &mut cache,
                Some(PrefixUse::Built),
            );
            assert_cached(
                &registry,
                &source,
                &recipe,
                &key,
                &mut cache,
                Some(PrefixUse::Reused),
            );
            recipe.masks[0].amount = 65.0;
            assert_cached(
                &registry,
                &source,
                &recipe,
                &key,
                &mut cache,
                Some(PrefixUse::Built),
            );
            recipe.layers[0].payload = json!({"x":5,"y":4,"rgb":[44,55,66]});
            assert_cached(
                &registry,
                &source,
                &recipe,
                &key,
                &mut cache,
                Some(PrefixUse::Built),
            );
            key.plan.bounds.width += 1;
            assert_cached(
                &registry,
                &source,
                &recipe,
                &key,
                &mut cache,
                Some(PrefixUse::Built),
            );
            match &mut source {
                PreviewSource::Jpeg(image) => image.fingerprint = "sha256:another-jpeg".into(),
                PreviewSource::Raw { settings, .. } => {
                    settings.white_balance = Some(
                        crate::WhiteBalanceApproximation::from_matrix([
                            [1.1, 0.0, 0.0],
                            [0.0, 1.0, 0.0],
                            [0.0, 0.0, 0.9],
                        ])
                        .unwrap(),
                    )
                }
            }
            key.identity = source.identity();
            assert_cached(
                &registry,
                &source,
                &recipe,
                &key,
                &mut cache,
                Some(PrefixUse::Built),
            );
            // A colour layer before restoration has no active leading restoration prefix.
            recipe.layers.swap(1, 2);
            assert_cached(&registry, &source, &recipe, &key, &mut cache, None);
            assert_eq!(cache.bytes(), 0);
            recipe.layers.swap(1, 2);
            recipe.layers[1].payload = json!({});
            assert_cached(&registry, &source, &recipe, &key, &mut cache, None);
            assert_eq!(cache.bytes(), 0);
        }
    }
    #[test]
    fn restoration_prefix_cache_reuses_downstream_edits_and_matches_uncached_pixels() {
        let registry = ModuleRegistry::builtin();
        let context = crate::RenderContext::new();
        for source in sources() {
            let key = key(&source);
            let mut cache = RestorationPrefixCache::default();
            let mut recipe = stack(0.5);
            for (exposure, expected) in (0..20).map(|i| {
                (
                    0.5 + f64::from(i) * 0.03,
                    if i == 0 {
                        PrefixUse::Built
                    } else {
                        PrefixUse::Reused
                    },
                )
            }) {
                recipe.layers[1].payload = json!({"exposure":exposure});
                let render = super::super::render(
                    &registry,
                    source.input(),
                    &recipe,
                    RenderOptions::default(),
                    &context,
                )
                .unwrap();
                let (cached, use_) = render
                    .frame_with_restoration_cache(
                        SnapshotId::new(),
                        &registry,
                        &recipe,
                        &key,
                        &mut cache,
                    )
                    .unwrap();
                assert_eq!(use_, Some(expected));
                assert_eq!(cached.rgba, render.frame(SnapshotId::new()).unwrap().rgba);
                assert!(cache.bytes() > 0 && cache.bytes() <= RESTORATION_PREFIX_MAX_BYTES);
            }
            recipe.layers[0].payload = json!({"sharpening":45.0,"luminance":25.0});
            let render = super::super::render(
                &registry,
                source.input(),
                &recipe,
                RenderOptions::default(),
                &context,
            )
            .unwrap();
            assert_eq!(
                render
                    .frame_with_restoration_cache(
                        SnapshotId::new(),
                        &registry,
                        &recipe,
                        &key,
                        &mut cache
                    )
                    .unwrap()
                    .1,
                Some(PrefixUse::Built)
            );
        }
    }
    #[test]
    fn restoration_prefix_cache_rebuilds_when_boundary_precision_changes() {
        let registry = ModuleRegistry::builtin();
        let context = crate::RenderContext::new();
        let source = sources().into_iter().next().unwrap();
        let key = key(&source);
        let mut cache = RestorationPrefixCache::default();
        let mut recipe = stack(0.0);
        for (exposure, expected) in [
            (0.0, PrefixUse::Built),
            (0.5, PrefixUse::Built),
            (0.8, PrefixUse::Reused),
            (0.0, PrefixUse::Built),
        ] {
            recipe.layers[1].payload = json!({"exposure":exposure});
            let render = super::super::render(
                &registry,
                source.input(),
                &recipe,
                RenderOptions::default(),
                &context,
            )
            .unwrap();
            let (cached, use_) = render
                .frame_with_restoration_cache(
                    SnapshotId::new(),
                    &registry,
                    &recipe,
                    &key,
                    &mut cache,
                )
                .unwrap();
            assert_eq!(use_, Some(expected));
            assert_eq!(cached.rgba, render.frame(SnapshotId::new()).unwrap().rgba);
        }
    }
    #[test]
    fn restoration_prefix_cache_cancellation_releases_held_pixels() {
        let registry = ModuleRegistry::builtin();
        let context = crate::RenderContext::new();
        let source = sources().into_iter().next().unwrap();
        let key = key(&source);
        let mut cache = RestorationPrefixCache::default();
        let recipe = stack(0.5);
        let cancel = Cancel::new();
        let render = super::super::render(
            &registry,
            source.input(),
            &recipe,
            RenderOptions::exact(&cancel),
            &context,
        )
        .unwrap();
        render
            .frame_with_restoration_cache(SnapshotId::new(), &registry, &recipe, &key, &mut cache)
            .unwrap();
        assert!(cache.bytes() > 0);
        cancel.cancel();
        assert!(
            render
                .frame_with_restoration_cache(
                    SnapshotId::new(),
                    &registry,
                    &recipe,
                    &key,
                    &mut cache
                )
                .is_err()
        );
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn restoration_half_scale_viewport_cache_reuses_tight_window_and_invalidates_on_pan() {
        let registry = ModuleRegistry::builtin();
        let context = crate::RenderContext::new();
        let width = 2049;
        let height = 129;
        let jpeg = super::super::tests::gradient(width, height);
        let plane = (width * height) as usize;
        let raw = LinearImage::new(
            width,
            height,
            (0..plane * 3)
                .map(|i| ((i * 137) % 997) as f32 / 997.0)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for source in [
            PreviewSource::Jpeg(jpeg),
            PreviewSource::Raw {
                image: raw,
                settings: LinearSettings::default(),
            },
        ] {
            let mut recipe = stack(0.4);
            recipe.layers[0].payload = json!({"sharpening":40.0});
            let mut cache = RestorationPrefixCache::default();
            let mut proxies = crate::ProxyCache::default();
            let mut prior = None;
            for (x0, exposure, expected) in [
                (40, 0.4, PrefixUse::Built),
                (40, 0.8, PrefixUse::Reused),
                (1200, 0.8, PrefixUse::Built),
            ] {
                recipe.layers[1].payload = json!({"exposure":exposure});
                let full = super::super::render(
                    &registry,
                    source.input(),
                    &recipe,
                    RenderOptions::default(),
                    &context,
                )
                .unwrap();
                let requested = Region {
                    x0,
                    y0: 20,
                    width: 96,
                    height: 75,
                };
                let plan = full
                    .plan_proxy_region(&registry, &recipe, requested)
                    .unwrap();
                let window = plan.proxy.window.expect("tight viewport has a cut source");
                assert!(window.width < plan.proxy.width);
                if x0 == 1200 {
                    assert_ne!(prior, Some(plan.proxy));
                }
                prior = Some(plan.proxy);
                let key = ProxyKey {
                    identity: source.identity(),
                    plan: plan.proxy,
                };
                let (proxy, built) = proxies
                    .source_for(&key, &source, || source.proxy(plan.proxy))
                    .unwrap();
                if built {
                    cache.clear();
                }
                let (cached, use_) = full
                    .render_proxy_region_cached(
                        &registry,
                        proxy.input(),
                        &recipe,
                        plan,
                        SnapshotId::new(),
                        &context,
                        &key,
                        &mut cache,
                    )
                    .unwrap();
                let super::super::entry::RegionRenderOutcome::Rendered(cached) = cached else {
                    panic!("cached region declined")
                };
                let super::super::entry::RegionRenderOutcome::Rendered(uncached) = full
                    .render_proxy_region(
                        &registry,
                        proxy.input(),
                        &recipe,
                        plan,
                        SnapshotId::new(),
                        &context,
                    )
                    .unwrap()
                else {
                    panic!("uncached region declined")
                };
                assert_eq!(use_, Some(expected));
                assert_eq!(cached.raster.rgba, uncached.raster.rgba);
                assert_eq!(cached.rect, plan.output);
                let cached_frame = cache.frame.as_ref().unwrap();
                assert!(cached_frame.held.width < plan.proxy.width);
                let samples = if matches!(source, PreviewSource::Raw { .. }) {
                    12
                } else {
                    6
                };
                assert_eq!(cache.bytes(), cached_frame.held.pixels() as usize * samples);
            }
        }
    }

    #[test]
    fn restoration_prefix_cache_distinguishes_boundary_windows_inside_the_same_source_window() {
        let registry = ModuleRegistry::builtin();
        let context = crate::RenderContext::new();
        let width = 2049;
        let height = 129;
        let jpeg = super::super::tests::gradient(width, height);
        let raw = LinearImage::new(
            width,
            height,
            (0..width as usize * height as usize * 3)
                .map(|i| ((i * 137) % 997) as f32 / 997.0)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let mut mask = crate::Mask::new("Second restoration window");
        mask.components.push(crate::Component::new(
            "Linear",
            crate::ComponentMode::Add,
            "linear",
            json!({"x0":0.0,"y0":0.0,"x1":1.0,"y1":0.0}),
        ));
        let mut second = Layer::new(DETAIL_EFFECT, json!({"sharpening":45.0}));
        second.mask = Some(mask.id.clone());
        let recipe = Recipe {
            layers: vec![
                Layer::new(DETAIL_EFFECT, json!({"sharpening":40.0})),
                second,
                Layer::new(BASIC_EFFECT, json!({"exposure":0.5})),
            ],
            masks: vec![mask],
            ..Recipe::default()
        };
        for source in [
            PreviewSource::Jpeg(jpeg),
            PreviewSource::Raw {
                image: raw,
                settings: LinearSettings::default(),
            },
        ] {
            let full = super::super::render(
                &registry,
                source.input(),
                &recipe,
                RenderOptions::default(),
                &context,
            )
            .unwrap();
            let mut proxies = crate::ProxyCache::default();
            let mut cache = RestorationPrefixCache::default();
            let mut prior = None;
            let mut prior_held = None;
            for requested in [
                Region {
                    x0: 900,
                    y0: 20,
                    width: 500,
                    height: 75,
                },
                Region {
                    x0: 1040,
                    y0: 20,
                    width: 360,
                    height: 75,
                },
            ] {
                let plan = full
                    .plan_proxy_region(&registry, &recipe, requested)
                    .unwrap();
                let key = ProxyKey {
                    identity: source.identity(),
                    plan: plan.proxy,
                };
                if let Some(prior) = &prior {
                    assert_eq!(prior, &key, "the source window is identical");
                }
                prior = Some(key.clone());
                let (proxy, fresh) = proxies
                    .source_for(&key, &source, || source.proxy(plan.proxy))
                    .unwrap();
                if fresh {
                    cache.clear();
                }
                let (cached, use_) = full
                    .render_proxy_region_cached(
                        &registry,
                        proxy.input(),
                        &recipe,
                        plan,
                        SnapshotId::new(),
                        &context,
                        &key,
                        &mut cache,
                    )
                    .unwrap();
                assert_eq!(
                    use_,
                    Some(PrefixUse::Built),
                    "changed boundary window rebuilds prefix"
                );
                let super::super::entry::RegionRenderOutcome::Rendered(cached) = cached else {
                    panic!("cache declined")
                };
                let super::super::entry::RegionRenderOutcome::Rendered(reference) = full
                    .render_proxy_region(
                        &registry,
                        proxy.input(),
                        &recipe,
                        plan,
                        SnapshotId::new(),
                        &context,
                    )
                    .unwrap()
                else {
                    panic!("reference declined")
                };
                assert_eq!(cached.raster.rgba, reference.raster.rgba);
                let held = cache.frame.as_ref().unwrap().held;
                if let Some(prior) = prior_held {
                    assert_ne!(held, prior, "the later boundary window differs");
                }
                prior_held = Some(held);
            }
        }
    }
}
