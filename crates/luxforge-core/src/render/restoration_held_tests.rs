//! A GPU preview boundary read from the held restoration prefix, against the boundary built from
//! the source.
use super::*;
use crate::{
    BASIC_EFFECT, CURVE_EFFECT, DETAIL_EFFECT, Layer, LinearImage, LinearSettings, PreviewSource,
    ProxyBounds, ProxyPlan, RenderOptions, render::boundary::BoundaryFormat,
};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

/// A GPU preview boundary in the segment the held restoration prefix opens reads that prefix, as
/// the render would build it, on both pixel domains: the same texels for a layer that begins the
/// segment and for one after a colour operation in it, and no tile of the prefix evaluated. A
/// boundary in another segment, or a cache holding another key, reads nothing from it.
#[test]
fn a_boundary_after_the_restoration_prefix_reads_the_held_prefix() {
    let registry = ModuleRegistry::builtin();
    let jpeg = super::super::tests::gradient(49, 37);
    let pixels: Vec<_> = (0..49 * 37 * 3)
        .map(|i| ((i * 137) % 997) as f32 / 997.0)
        .collect();
    let sources = [
        (PreviewSource::Jpeg(jpeg), BoundaryFormat::Half),
        (
            PreviewSource::Raw {
                image: LinearImage::new(49, 37, pixels).unwrap(),
                settings: LinearSettings::default(),
            },
            BoundaryFormat::Float,
        ),
    ];
    let recipe = Recipe {
        layers: vec![
            Layer::new(DETAIL_EFFECT, json!({"sharpening":40.0,"luminance":25.0})),
            Layer::new(BASIC_EFFECT, json!({"exposure":0.4})),
            Layer::new(
                CURVE_EFFECT,
                json!({"luminance":[[0.0,0.0],[0.4,0.55],[1.0,1.0]]}),
            ),
        ],
        ..Recipe::default()
    };
    for (source, format) in sources {
        let (width, height) = source.dimensions();
        let whole = Stage { width, height };
        let key = ProxyKey {
            identity: source.identity(),
            plan: ProxyPlan {
                width,
                height,
                bounds: ProxyBounds { width, height },
                window: None,
            },
        };
        let context = crate::RenderContext::new();
        let render = super::super::render(
            &registry,
            source.input(),
            &recipe,
            RenderOptions::default(),
            &context,
        )
        .unwrap();
        let mut cache = RestorationPrefixCache::default();
        assert!(
            render
                .held_prefix(&registry, &recipe, &key, &cache)
                .unwrap()
                .is_none(),
            "nothing held yet"
        );
        render
            .frame_with_restoration_cache(SnapshotId::new(), &registry, &recipe, &key, &mut cache)
            .unwrap();
        let held = render
            .held_prefix(&registry, &recipe, &key, &cache)
            .unwrap()
            .expect("the prefix the frame just held");
        let compiled = render.compiled.as_ref();
        let built = |position| {
            render
                .boundary(compiled, whole, Region::whole(whole), position, format)
                .unwrap()
        };
        let read = |position| {
            render
                .boundary_reading(
                    compiled,
                    whole,
                    Region::whole(whole),
                    position,
                    format,
                    Some(&held),
                )
                .unwrap()
        };
        for layer in [1, 2] {
            let position = compiled.layers[layer];
            assert_eq!(
                position.0, held.segment,
                "layer {layer} is in the prefix's segment"
            );
            let tiles = Arc::new(AtomicU64::new(0));
            super::super::spatial::observe_tiles(tiles.clone());
            let expected = built(position);
            assert!(tiles.load(Ordering::Relaxed) > 0, "built, the prefix runs");
            let tiles = Arc::new(AtomicU64::new(0));
            super::super::spatial::observe_tiles(tiles.clone());
            let boundary = read(position);
            assert_eq!(
                tiles.load(Ordering::Relaxed),
                0,
                "no tile of the prefix ran"
            );
            assert!(
                boundary == expected,
                "layer {layer}: the held prefix's boundary differs"
            );
        }
        // Detail's own input lies in the segment before: the held prefix is not its input.
        let position = compiled.layers[0];
        assert_ne!(position.0, held.segment);
        assert!(read(position) == built(position));
        // Another prefix asks with another key.
        let mut other = recipe.clone();
        other.layers[0].payload = json!({"sharpening":41.0,"luminance":25.0});
        let render = super::super::render(
            &registry,
            source.input(),
            &other,
            RenderOptions::default(),
            &context,
        )
        .unwrap();
        assert!(
            render
                .held_prefix(&registry, &other, &key, &cache)
                .unwrap()
                .is_none()
        );
    }
}
