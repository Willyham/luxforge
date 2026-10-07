//! The light plane's rules on the surface (`docs/design/gpu-preview.md`, "The global estimate"):
//! a light plane is the slot's pool's whatever link declares it, only a light link's step writes
//! one, a step reading one is global, and a light that changes runs again the passes that read it
//! and changes the content key of the link that does. The light link itself, with the Presence
//! program it runs, is held to the CPU by the desktop's tests (`app::gpu_light_tests`).
use super::super::{
    GpuProgram, GpuStep, TexelMap, chain,
    spatial::{
        GpuApply, GpuPass, GpuPlane, GpuSpatial, LIGHT_BYTES, PassShape, PlaneFormat, PlaneSize,
        PlaneTexture, PlanesKey, Pool, PoolKey, Schedule, validate_spatial,
    },
};
use super::{GpuLight, light_charge};
use std::borrow::Cow;

/// A spatial program with a reduction, a selection, and a pass and an apply that read a light.
const PROGRAM: &str = "
fn lf_lighttest_reduce(at: vec2<i32>, words: u32, block: u32) {
    lf_store(at, vec4<f32>(lf_source(at), 1.0));
}

fn lf_lighttest_select(at: vec2<i32>, words: u32, block: u32) {
    lf_store(vec2<i32>(0), lf_plane(0u, vec2<i32>(0)));
}

fn lf_lighttest_scale(at: vec2<i32>, words: u32, block: u32) {
    lf_store(at, vec4<f32>(lf_source(at) * lf_plane(0u, vec2<i32>(0)).xyz, 1.0));
}

fn lf_lighttest_show(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    return lf_plane(planes, at).xyz;
}
";

fn program() -> GpuProgram {
    GpuProgram {
        words: vec![0; 4],
        ..GpuProgram::new("lf_lighttest", PROGRAM)
    }
}

fn plane(format: PlaneFormat, size: PlaneSize) -> GpuPlane {
    GpuPlane { format, size }
}

fn pass(kernel: &'static str, inputs: &[u32], output: u32, shape: PassShape) -> GpuPass {
    GpuPass {
        kernel: Cow::Borrowed(kernel),
        inputs: inputs.to_vec(),
        output,
        words: 0,
        source: 0,
        reads_source: kernel != "lf_lighttest_select",
        shape,
        unit: 0,
    }
}

const EACH: PassShape = PassShape::Texels { span: [1, 1] };

/// A step that draws reading light `k`: one pass scaling its input by the light into the plane its
/// apply reads.
fn reading(k: u32) -> GpuSpatial {
    GpuSpatial {
        program: program(),
        planes: vec![
            plane(PlaneFormat::Quad, PlaneSize::Light(k)),
            plane(PlaneFormat::Quad, PlaneSize::Reduced(1)),
        ],
        passes: vec![pass("lf_lighttest_scale", &[0], 1, EACH)],
        applies: vec![GpuApply {
            function: Cow::Borrowed("lf_lighttest_show"),
            planes: vec![1],
            words: 0,
            identity: false,
        }],
        clamps: false,
        mask: None,
        halos: vec![0],
    }
}

/// A light link's step writing light `k`: its reduction into a block plane, then its selection.
fn writing(k: u32) -> GpuSpatial {
    GpuSpatial {
        program: program(),
        planes: vec![
            plane(PlaneFormat::Quad, PlaneSize::Reduced(16)),
            plane(PlaneFormat::Quad, PlaneSize::Light(k)),
        ],
        passes: vec![
            pass("lf_lighttest_reduce", &[], 0, EACH),
            pass("lf_lighttest_select", &[0], 1, PassShape::Workgroup),
        ],
        applies: Vec::new(),
        clamps: true,
        mask: None,
        halos: Vec::new(),
    }
}

fn steps(spatial: GpuSpatial) -> Vec<GpuStep> {
    vec![GpuStep::Spatial(Box::new(spatial))]
}

/// A light plane is the slot's pool's, whichever link declares it, reader or writer: never a link's
/// kept texture nor a scratch one, and the pool holds one past the largest any link declares,
/// whatever the boundary, one `rgba32float` texel each.
#[test]
fn a_light_plane_is_the_pools_whatever_link_declares_it() {
    let (size, origin) = ((64, 48), (128, 32));
    let read = steps(reading(2));
    let key = PlanesKey::of(&read, size, origin).expect("a spatial step");
    assert_eq!(key.location(0, 0), Some(PlaneTexture::Light(2)));
    assert_eq!(key.location(0, 1), Some(PlaneTexture::Kept(0)));
    assert_eq!(
        key.kept(),
        [plane(PlaneFormat::Quad, PlaneSize::Reduced(1))]
    );
    assert!(key.scratch().is_empty());
    let pool = PoolKey::of([&read[..]], size, origin);
    assert_eq!(
        pool,
        PoolKey::of(std::iter::empty(), size, origin).with_lights(3)
    );
    assert_eq!(pool.bytes(), 3 * LIGHT_BYTES);
    // The writer's block plane is scratch of its own class; its light is the pool's too.
    let write = steps(writing(0));
    let written = PlanesKey::of(&write, size, origin).expect("a spatial step");
    assert_eq!(written.location(0, 1), Some(PlaneTexture::Light(0)));
    assert!(written.kept().is_empty());
    assert_eq!(written.scratch().len(), 1);
    assert_eq!(
        PoolKey::of([&read[..], &write[..]], size, origin),
        PoolKey::of([&write[..]], size, origin).with_lights(3)
    );
    // Light planes take one texel, at any boundary.
    assert_eq!(
        plane(PlaneFormat::Quad, PlaneSize::Light(5)).extent(origin, size),
        (1, 1)
    );
}

/// Only a light link's step, one that draws nothing, writes a light plane, which is one
/// `rgba32float` texel; a step that draws reads it. A step reading a light is global: a change to
/// the light changes its output everywhere.
#[test]
fn only_a_light_link_writes_a_light_plane_and_its_readers_are_global() {
    validate_spatial(&reading(0)).expect("a reader");
    validate_spatial(&writing(0)).expect("a light link's step");
    let mut drawn = reading(0);
    drawn.passes[0].output = 0;
    drawn.passes[0].inputs = vec![1];
    assert!(
        validate_spatial(&drawn).is_err(),
        "a drawing step writes no light"
    );
    let mut half = reading(0);
    half.planes[0].format = PlaneFormat::Colour;
    assert!(validate_spatial(&half).is_err(), "a light is rgba32float");
    assert!(reading(1).global());
    let mut local = reading(0);
    local.planes[0] = plane(PlaneFormat::Quad, PlaneSize::Reduced(1));
    assert!(!local.global());
    assert_eq!(reading(4).lights().collect::<Vec<_>>(), [4]);
}

/// A light link's steps name the light they write and the block their reduction writes; any other
/// steps are not a light link's, and have no charge.
#[test]
fn a_light_links_steps_name_its_light() {
    let light = GpuLight {
        stage: (300, 200),
        steps: steps(writing(3)),
        input: super::LightInput::Source,
    };
    assert_eq!(light.index(), Some(3));
    assert_eq!(light.block(), Some(16));
    let reader = GpuLight {
        stage: (300, 200),
        steps: steps(reading(3)),
        input: super::LightInput::Source,
    };
    assert_eq!(reader.index(), None);
    assert_eq!(
        light_charge(
            &reader,
            crate::photo_surface::BoundaryFormat::Half,
            8192,
            1 << 27
        ),
        None
    );
    // One tile of a 300 × 200 stage at 8192: its half floats, the 19 × 13 block plane, and the
    // buffers at their capacities: the words, the blocks, the parameters and one cut's words.
    let charge = light_charge(
        &light,
        crate::photo_surface::BoundaryFormat::Half,
        8192,
        1 << 27,
    )
    .expect("a light link");
    assert_eq!(charge, 300 * 200 * 8 + 19 * 13 * 16 + 4 * 1024);
    // At a texture side of 160 the stage is cut in four tiles of 160 at most, each with its cut's
    // words, and the tile texture is 160 square: the five parameter slices take 2 KiB.
    let tiled = light_charge(
        &light,
        crate::photo_surface::BoundaryFormat::Float,
        160,
        1 << 27,
    )
    .expect("a light link");
    assert_eq!(
        tiled,
        160 * 160 * 16 + 19 * 13 * 16 + (1024 + 1024 + 2048) + 4 * 1024
    );
    // A light whose blocks grow — a stroke on a mask before it — keeps its link, whose blocks
    // buffer grows in place; another stage does not.
    let shape = |light: &GpuLight| {
        super::Shape::of(light, crate::photo_surface::BoundaryFormat::Half, 8192).expect("a shape")
    };
    let mut grown = light.clone();
    if let GpuStep::Spatial(spatial) = &mut grown.steps[0] {
        spatial.program.block = std::sync::Arc::from(vec![7u32; 4096]);
    }
    assert_ne!(shape(&grown), shape(&light));
    assert!(shape(&light).holds(&shape(&grown)));
    let wider = GpuLight {
        stage: (316, 200),
        ..light.clone()
    };
    assert!(!shape(&light).holds(&shape(&wider)));
}

/// A light that changes runs again the passes that read it, and nothing else.
#[test]
fn a_light_that_changes_runs_again_what_reads_it() {
    let test = "a_light_that_changes_runs_again_what_reads_it";
    let Some((device, _queue)) = super::super::tests::headless(test) else {
        return;
    };
    let read = steps(reading(0));
    let (size, origin) = ((64, 48), (0, 0));
    let mut pool = Pool::default();
    pool.fit(
        &device,
        &PoolKey::of([&read[..]], size, origin),
        &mut |_| Ok::<(), ()>(()),
        &mut |_, _| {},
    )
    .expect("the pool");
    assert!(pool.light_view(0).is_some() && pool.light_key(0).is_none());
    let key = PlanesKey::of(&read, size, origin).expect("a spatial step");
    let (mut words, mut blocks) = (Vec::new(), Vec::new());
    chain::pack_steps(TexelMap::IDENTITY, (0, 0), &read, &mut words, &mut blocks);
    let mut schedule = Schedule::new(&mut pool);
    let mut run = |pool: &mut Pool| schedule.run(&read, &words, &blocks, 1, &key, pool);
    assert_eq!(run(&mut pool), [true], "the first tick");
    assert_eq!(run(&mut pool), [false], "nothing changed");
    pool.set_light_key(0, 7);
    assert_eq!(run(&mut pool), [true], "the light was written");
    assert_eq!(run(&mut pool), [false], "the same light");
    pool.set_light_key(0, 7);
    assert_eq!(run(&mut pool), [false], "the same light written again");
    pool.set_light_key(0, 8);
    assert_eq!(run(&mut pool), [true], "another light");
}
