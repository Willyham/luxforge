//! The GPU preview's boundary against the CPU's own input of its layer: the value a colour layer
//! receives inside its run, unclamped, on both pixel domains, through exact steps and after a
//! resample.
use super::boundary::BoundaryFrame;
use super::tests::{
    colour_recipe, colour_registry, exposure_layer, fitted_crop, gradient, positional_layer, turn,
    varied,
};
use super::{Compiled, Render, RenderContext, RenderOptions, RenderSource, layer_input, render};
use crate::{
    Cancel, Layer, LinearSettings, ModuleRegistry, Recipe, SnapshotId, Transform,
    colour::srgb::decode_table,
    modules::{Region, Stage},
};

/// The nearest half float of an `f32`, back as an `f32`: what a texel holds.
fn held(value: f32) -> f32 {
    half::f16::from_f32(value.clamp(-65504.0, 65504.0)).to_f32()
}

/// Where layer `layer` begins in `compiled`, or the end of its last segment one past the stack.
fn position(compiled: &Compiled, layer: usize) -> (usize, usize) {
    match compiled.layers.get(layer) {
        Some(position) => *position,
        None => {
            let last = compiled.segments.len() - 1;
            (last, compiled.segments[last].operations.len())
        }
    }
}

/// The boundary of layer `layer` of `recipe` over `source`, at the exact stage and whole.
fn boundary(
    registry: &ModuleRegistry,
    source: RenderSource<'_>,
    recipe: &Recipe,
    layer: usize,
    context: &RenderContext,
) -> BoundaryFrame {
    let rendered = render(
        registry,
        source,
        recipe,
        RenderOptions::exact(&Cancel::never()),
        context,
    )
    .unwrap();
    let (width, height) = source.dimensions();
    let whole = Stage { width, height };
    rendered
        .boundary(
            &rendered.compiled,
            whole,
            Region::whole(whole),
            position(&rendered.compiled, layer),
        )
        .unwrap()
}

/// Every texel of `frame` against the CPU's own input of layer `layer`, exactly.
fn assert_layer_input(
    registry: &ModuleRegistry,
    source: RenderSource<'_>,
    recipe: &Recipe,
    layer: usize,
    frame: &BoundaryFrame,
    what: &str,
) {
    let context = RenderContext::new();
    let (stage, input) = layer_input(registry, source, recipe, layer, &context).unwrap();
    assert_eq!(frame.stage, stage, "{what}: the stage its layer receives");
    assert_eq!(frame.origin, (0, 0), "{what}");
    assert_eq!((frame.width, frame.height), (stage.width, stage.height));
    for y in 0..stage.height {
        for x in 0..stage.width {
            let expected = input.linear(x, y).unwrap().unwrap().map(|v| held(v as f32));
            assert_eq!(
                frame.texel(x, y).unwrap(),
                expected,
                "{what}: texel ({x}, {y})"
            );
        }
    }
}

/// The boundary is the value its layer receives: the decoded source before any colour, and the
/// run's unclamped value inside a colour run, through an exact turn, on a JPEG and on RAW planes.
#[test]
fn a_boundary_is_the_cpus_input_of_its_layer_on_both_domains() {
    let registry = colour_registry();
    let jpeg = gradient(41, 29);
    let planes = varied(41, 29);
    let stacks = [
        (
            "a bright exposure before a positional layer",
            vec![exposure_layer(&[2.5]), positional_layer()],
        ),
        (
            "colour on both sides of an exact turn",
            vec![
                exposure_layer(&[1.5]),
                turn(Transform::RotateRight),
                positional_layer(),
            ],
        ),
    ];
    for (what, layers) in stacks {
        let recipe = colour_recipe(layers);
        // A plan never starts at a geometry layer, whose draft changes the stage itself.
        for layer in (0..recipe.layers.len())
            .filter(|layer| recipe.layers[*layer].effect_id != crate::ORIENTATION_EFFECT)
        {
            for (domain, source) in [
                ("JPEG", RenderSource::Byte(&jpeg)),
                (
                    "RAW",
                    RenderSource::Linear {
                        image: &planes,
                        settings: LinearSettings::default(),
                    },
                ),
            ] {
                let what = format!("{what}, layer {layer}, {domain}");
                let frame = boundary(&registry, source, &recipe, layer, &RenderContext::new());
                assert_layer_input(&registry, source, &recipe, layer, &frame, &what);
            }
        }
    }
    // Inside the run, a value past white survives: the CPU would quantize it only at the run's end.
    let recipe = colour_recipe(vec![exposure_layer(&[2.5]), positional_layer()]);
    let frame = boundary(
        &registry,
        RenderSource::Byte(&jpeg),
        &recipe,
        1,
        &RenderContext::new(),
    );
    let brightest = (0..frame.height)
        .flat_map(|y| (0..frame.width).map(move |x| (x, y)))
        .map(|(x, y)| frame.texel(x, y).unwrap()[0])
        .fold(0.0_f32, f32::max);
    assert!(
        brightest > 1.0,
        "the boundary holds the run's unclamped value"
    );
}

/// A boundary behind a resample is the frame the resample writes, which the CPU quantizes there.
#[test]
fn a_boundary_behind_a_resample_is_the_frame_it_writes() {
    let registry = colour_registry();
    let source = gradient(41, 29);
    let crop = Layer::crop(fitted_crop(41, 29, 4.0, [0.1, 0.12, 0.8, 0.75]));
    let recipe = colour_recipe(vec![exposure_layer(&[0.4]), crop, positional_layer()]);
    let frame = boundary(
        &registry,
        RenderSource::Byte(&source),
        &recipe,
        2,
        &RenderContext::new(),
    );
    let prefix = Recipe {
        layers: recipe.layers[..2].to_vec(),
        ..recipe.clone()
    };
    let written = super::testing::render(&registry, &source, SnapshotId::new(), &prefix).unwrap();
    assert_eq!((frame.width, frame.height), (written.width, written.height));
    let table = decode_table();
    for (index, pixel) in written.rgba.chunks_exact(4).enumerate() {
        let (x, y) = (index as u32 % written.width, index as u32 / written.width);
        assert_eq!(
            frame.texel(x, y).unwrap(),
            [0, 1, 2].map(|channel| held(table[usize::from(pixel[channel])])),
            "texel ({x}, {y})"
        );
    }
}

/// A value past the half range is held at the largest finite half of its sign, never infinite.
#[test]
fn a_value_past_the_half_range_is_held_finite() {
    let mut texel = [0u8; 8];
    super::boundary::write_texel(&mut texel, [1.0e9, -1.0e9, 0.5]);
    assert_eq!(
        super::boundary::read_texel(&texel),
        [65504.0, -65504.0, 0.5]
    );
    assert_eq!(&texel[6..8], &half::f16::ONE.to_bits().to_le_bytes());
}

/// The render a boundary is asked of must render the compilation it names.
#[test]
fn a_boundary_of_another_compilation_is_refused() {
    let registry = colour_registry();
    let source = gradient(9, 7);
    let one = colour_recipe(vec![exposure_layer(&[0.1])]);
    let two = colour_recipe(vec![
        exposure_layer(&[0.1]),
        Layer::crop(fitted_crop(9, 7, 4.0, [0.1, 0.1, 0.8, 0.8])),
    ]);
    let context = RenderContext::new();
    let rendered: Render<'_> = render(
        &registry,
        RenderSource::Byte(&source),
        &one,
        RenderOptions::exact(&Cancel::never()),
        &context,
    )
    .unwrap();
    let other = registry.compile(9, 7, &two).unwrap();
    let whole = Stage {
        width: 9,
        height: 7,
    };
    assert!(
        rendered
            .boundary(&other, whole, Region::whole(whole), (0, 0))
            .is_err()
    );
}
