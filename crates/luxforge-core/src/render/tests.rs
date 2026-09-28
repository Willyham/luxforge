//! What the render tests share: sources, registries of test modules, stepwise references and
//! the layers and recipes they build.

use super::testing::render;
use super::*;
use crate::{
    EFFECT_FORMAT, Error, Layer, LayerId, ORIENTATION_EFFECT, Orientation, PIXEL_EFFECT,
    PixelReplace, PointwiseColor, Recipe, SnapshotId, SourceImage, Transform,
    modules::{
        ActionInput, ActionPlan, Availability, BoxRect, ColorOperation, CropPayload, CropStage,
        EffectDescriptor, EffectStage, ExactGeometry, ModuleDescriptor, ModuleRegistry, Processing,
        Resample, Stage, StageContext, ToolModule,
    },
};
use luxforge_reference::srgb;
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub(crate) fn registry() -> ModuleRegistry {
    ModuleRegistry::developer()
}

pub(crate) fn source(width: u32, height: u32) -> SourceImage {
    let mut rgba = Vec::new();
    for i in 0..width * height {
        rgba.extend([i as u8, (i + 20) as u8, (i + 40) as u8, 255]);
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:test".into(),
        orientation: 1,
        capture: Default::default(),
    }
}

/// One single-action orientation layer: what a transform commits when the stack does not end
/// in an orientation layer. A sequence of these is the stepwise form every proof below uses.
pub(crate) fn turn(transform: Transform) -> Layer {
    Layer::orientation(Orientation::of(transform))
}

/// The elementary steps one orientation payload means: the mirror, then the quarter turns. The
/// stepwise references apply these one at a time, independently of the composed mapping.
pub(crate) fn steps(payload: &Value) -> Vec<Transform> {
    let orientation: Orientation = serde_json::from_value(payload.clone()).unwrap();
    let mut steps = Vec::new();
    if orientation.mirror {
        steps.push(Transform::MirrorHorizontal);
    }
    for _ in 0..orientation.turns {
        steps.push(Transform::RotateRight);
    }
    steps
}

pub(crate) fn rendered(source: &SourceImage, layers: Vec<Layer>) -> Raster {
    render(
        &registry(),
        source,
        SnapshotId::new(),
        &Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        },
    )
    .unwrap()
}

pub(crate) fn reference(source: &SourceImage, layers: &[Layer]) -> (u32, u32, Vec<u8>) {
    let mut width = source.width;
    let mut height = source.height;
    let mut rgba = source.rgba.as_ref().to_vec();
    for layer in layers {
        match layer.effect_id.as_str() {
            PIXEL_EFFECT => {
                let pixel: PixelReplace = serde_json::from_value(layer.payload.clone()).unwrap();
                let offset = ((pixel.y * width + pixel.x) * 4) as usize;
                rgba[offset..offset + 3].copy_from_slice(&pixel.rgb);
            }
            ORIENTATION_EFFECT => {
                for transform in steps(&layer.payload) {
                    let (next_width, next_height) = match transform {
                        Transform::RotateLeft | Transform::RotateRight => (height, width),
                        Transform::MirrorHorizontal | Transform::FlipVertical => (width, height),
                    };
                    let mut next = vec![0; (next_width * next_height * 4) as usize];
                    for y in 0..height {
                        for x in 0..width {
                            let (next_x, next_y) = match transform {
                                Transform::RotateRight => (height - 1 - y, x),
                                Transform::RotateLeft => (y, width - 1 - x),
                                Transform::MirrorHorizontal => (width - 1 - x, y),
                                Transform::FlipVertical => (x, height - 1 - y),
                            };
                            let from = ((y * width + x) * 4) as usize;
                            let to = ((next_y * next_width + next_x) * 4) as usize;
                            next[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
                        }
                    }
                    width = next_width;
                    height = next_height;
                    rgba = next;
                }
            }
            TEST_CROP_EFFECT => {
                let crop: CropPayload = serde_json::from_value(layer.payload.clone()).unwrap();
                assert_eq!(crop.angle, 0.0, "the stepwise reference never straightens");
                let (x, y) = (
                    (crop.x * f64::from(width)).round() as u32,
                    (crop.y * f64::from(height)).round() as u32,
                );
                let next_width = (crop.width * f64::from(width)).round().max(1.0) as u32;
                let next_height = (crop.height * f64::from(height)).round().max(1.0) as u32;
                let mut next = vec![0; (next_width * next_height * 4) as usize];
                for row in 0..next_height {
                    for column in 0..next_width {
                        let from = (((row + y) * width + column + x) * 4) as usize;
                        let to = ((row * next_width + column) * 4) as usize;
                        next[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
                    }
                }
                width = next_width;
                height = next_height;
                rgba = next;
            }
            other => panic!("unexpected test effect {other}"),
        }
    }
    (width, height, rgba)
}

/// A test-only module that compiles crop payloads and plain scaling into the host's primitives.
/// The real crop module is a separate deliverable; this one exists so the host's resample
/// segmentation can be tested without it.
pub(crate) struct GeometryTestModule(ModuleDescriptor);

pub(crate) const TEST_CROP_EFFECT: &str = "test.crop";

pub(crate) const TEST_SCALE_EFFECT: &str = "test.scale";

pub(crate) const TEST_OFFSET_EFFECT: &str = "test.offset";

/// A resample that is finite and has a non-empty output stage, so compilation accepts it, and
/// whose mapping collapses its stage onto a line. No delivered module can declare one; it exists
/// so the host's refusal to invent a mapping for it can be proved.
pub(crate) const TEST_DEGENERATE_EFFECT: &str = "test.degenerate";

impl GeometryTestModule {
    fn shared() -> Arc<dyn ToolModule> {
        Arc::new(Self(ModuleDescriptor {
            id: "test.geometry".into(),
            title: "Test geometry".into(),
            hint: None,
            effects: [
                TEST_CROP_EFFECT,
                TEST_SCALE_EFFECT,
                TEST_OFFSET_EFFECT,
                TEST_DEGENERATE_EFFECT,
            ]
            .into_iter()
            .map(|id| EffectDescriptor {
                id: id.into(),
                format: EFFECT_FORMAT,
                stage: EffectStage::Geometry,
                order: 0,
                maskable: false,
                artifacts: false,
                single: false,
                sources: Vec::new(),
            })
            .collect(),
            actions: Vec::new(),
            queries: Vec::new(),
            controls: Vec::new(),
            reset: None,
            canvas: None,
            developer: false,
            collapsed: false,
            layout: crate::ModuleLayout::Stacked,
            availability: Availability::Available,
            ..ModuleDescriptor::default()
        }))
    }
}

impl ToolModule for GeometryTestModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.0
    }
    fn parse(&self, _: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Err(Error::internal("no actions"))
    }
    fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Ok(ActionPlan::NoOp)
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe(&self, effect_id: &str, _: u32, _: &Value) -> Result<crate::LayerReport, Error> {
        Ok(crate::LayerReport::new(format!(
            "test geometry {effect_id}"
        )))
    }
    fn compile(
        &self,
        effect_id: &str,
        _: u32,
        payload: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        if effect_id == TEST_OFFSET_EFFECT {
            // A raw translation with a smaller output, including mappings the host must reject.
            return Ok(Processing::ExactGeometry(ExactGeometry::crop(
                payload["x"].as_i64().expect("test offset"),
                payload["y"].as_i64().expect("test offset"),
                payload["width"].as_u64().expect("test offset") as u32,
                payload["height"].as_u64().expect("test offset") as u32,
            )));
        }
        if effect_id == TEST_DEGENERATE_EFFECT {
            // Both axes read the same line of the input stage: finite, non-empty, and not a
            // mapping anything can be projected back through.
            return Ok(Processing::Resample(Resample {
                inverse: [1.0, 1.0, 0.0, 1.0, 1.0, 0.0],
                output_width: stage.width,
                output_height: stage.height,
            }));
        }
        if effect_id == TEST_SCALE_EFFECT {
            let scale = payload["scale"].as_f64().expect("test scale");
            return Ok(Processing::Resample(Resample {
                inverse: [1.0 / scale, 0.0, 0.0, 0.0, 1.0 / scale, 0.0],
                output_width: (f64::from(stage.width) * scale) as u32,
                output_height: (f64::from(stage.height) * scale) as u32,
            }));
        }
        let crop: CropPayload = serde_json::from_value(payload.clone())
            .map_err(|error| Error::validation(error.to_string()))?;
        let crop_stage = CropStage {
            width: stage.width,
            height: stage.height,
            angle: crop.angle,
        };
        let rect = crop.output_rect(&crop_stage)?;
        if crop.angle == 0.0 {
            return Ok(Processing::ExactGeometry(ExactGeometry::crop(
                rect.x,
                rect.y,
                rect.width,
                rect.height,
            )));
        }
        Ok(Processing::Resample(Resample {
            inverse: crop_stage.inverse_map((rect.x as f64, rect.y as f64)),
            output_width: rect.width,
            output_height: rect.height,
        }))
    }
}

/// The payload a crop draft would commit: the requested fraction of the rotated box, fitted onto
/// the source and normalized, so every case in these tests is a rectangle the contract accepts.
pub(crate) fn fitted_crop(width: u32, height: u32, angle: f64, rect: [f64; 4]) -> CropPayload {
    let stage = CropStage {
        width,
        height,
        angle,
    };
    let (box_width, box_height) = stage.bounding_box();
    let fitted = stage.fit_about_center(BoxRect {
        x: rect[0] * box_width,
        y: rect[1] * box_height,
        width: rect[2] * box_width,
        height: rect[3] * box_height,
    });
    fitted.normalized(&stage)
}

pub(crate) fn crop_layer(crop: CropPayload) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: TEST_CROP_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: serde_json::to_value(crop).unwrap(),
        mask: None,
        artifacts: Vec::new(),
    }
}

pub(crate) fn geometry_registry() -> ModuleRegistry {
    let mut registry = ModuleRegistry::developer();
    registry.register(GeometryTestModule::shared()).unwrap();
    registry
}

/// An asymmetric opaque gradient, so a wrong axis or a wrong weight shows up.
pub(crate) fn gradient(width: u32, height: u32) -> SourceImage {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend([
                (x * 251 / width.max(1)) as u8,
                (y * 241 / height.max(1)) as u8,
                ((x * 7 + y * 3) % 256) as u8,
                255,
            ]);
        }
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:gradient".into(),
        orientation: 1,
        capture: Default::default(),
    }
}

/// An independent f64 evaluation of the crop contract: the rotated box, the rounded output
/// rectangle and one bilinear sample in linear light. It shares no code with the renderer.
pub(crate) struct CropReference {
    box_width: f64,
    box_height: f64,
    cos: f64,
    sin: f64,
    origin: (f64, f64),
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl CropReference {
    pub(crate) fn new(source: &SourceImage, crop: CropPayload) -> Self {
        let rect = [crop.x, crop.y, crop.width, crop.height];
        let radians = crop.angle * std::f64::consts::PI / 180.0;
        let (cos, sin) = (radians.cos(), radians.sin());
        let width = f64::from(source.width);
        let height = f64::from(source.height);
        let box_width = width * cos.abs() + height * sin.abs();
        let box_height = width * sin.abs() + height * cos.abs();
        Self {
            box_width,
            box_height,
            cos,
            sin,
            origin: (
                (rect[0] * box_width).round(),
                (rect[1] * box_height).round(),
            ),
            width: (rect[2] * box_width).round().max(1.0) as u32,
            height: (rect[3] * box_height).round().max(1.0) as u32,
        }
    }

    /// The continuous source position, in index space, that one output pixel center samples.
    pub(crate) fn position(&self, source: &SourceImage, i: u32, j: u32) -> (f64, f64) {
        let x = self.origin.0 + f64::from(i) + 0.5 - self.box_width / 2.0;
        let y = self.origin.1 + f64::from(j) + 0.5 - self.box_height / 2.0;
        (
            self.cos * x + self.sin * y + f64::from(source.width) / 2.0 - 0.5,
            -self.sin * x + self.cos * y + f64::from(source.height) / 2.0 - 0.5,
        )
    }

    pub(crate) fn pixel(&self, source: &SourceImage, i: u32, j: u32) -> [u8; 4] {
        let decode = |value: u8| -> f64 { srgb::decode(value) };
        let encode = |linear: f64| -> u8 { (srgb::encode_clamped(linear) * 255.0).round() as u8 };
        let (u, v) = self.position(source, i, j);
        let (left, top) = (u.floor(), v.floor());
        let (fraction_x, fraction_y) = (u - left, v - top);
        let at = |x: f64, y: f64| -> [u8; 4] {
            let x = (x.max(0.0) as u32).min(source.width - 1);
            let y = (y.max(0.0) as u32).min(source.height - 1);
            let offset = ((y * source.width + x) * 4) as usize;
            [
                source.rgba[offset],
                source.rgba[offset + 1],
                source.rgba[offset + 2],
                source.rgba[offset + 3],
            ]
        };
        let samples = [
            (at(left, top), (1.0 - fraction_x) * (1.0 - fraction_y)),
            (at(left + 1.0, top), fraction_x * (1.0 - fraction_y)),
            (at(left, top + 1.0), (1.0 - fraction_x) * fraction_y),
            (at(left + 1.0, top + 1.0), fraction_x * fraction_y),
        ];
        let mut pixel = [0; 4];
        for (channel, slot) in pixel.iter_mut().enumerate().take(3) {
            *slot = encode(
                samples
                    .iter()
                    .map(|(sample, weight)| weight * decode(sample[channel]))
                    .sum(),
            );
        }
        pixel[3] = samples
            .iter()
            .map(|(sample, weight)| weight * f64::from(sample[3]))
            .sum::<f64>()
            .round() as u8;
        pixel
    }
}

/// One affine map applied to a continuous coordinate, written out here rather than shared with
/// the composition under test.
pub(crate) fn at(matrix: [f64; 6], x: f64, y: f64) -> (f64, f64) {
    (
        matrix[0] * x + matrix[1] * y + matrix[2],
        matrix[3] * x + matrix[4] * y + matrix[5],
    )
}

/// A test-only colour unit: multiply linear light by `2^EV`. The coefficient is computed in f64
/// and applied in f32, which is the working precision the contract declares.
#[derive(Debug)]
pub(crate) struct Exposure {
    ev: f64,
    gain: f32,
}

impl Exposure {
    fn new(ev: f64) -> Self {
        Self {
            ev,
            gain: ev.exp2() as f32,
        }
    }
}

impl PointwiseColor for Exposure {
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            for channel in pixel {
                *channel *= self.gain;
            }
        }
    }
    fn is_finite(&self) -> bool {
        self.ev.is_finite() && self.gain.is_finite()
    }
    fn describe(&self) -> String {
        format!("exposure {:+} EV", self.ev)
    }
}

/// A unit whose coefficients are finite but whose result is not: two of them in one operation
/// overflow f32 for any non-zero channel, which is the render-time failure the contract names.
#[derive(Debug)]
pub(crate) struct Overflow;

impl PointwiseColor for Overflow {
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            for channel in pixel {
                *channel *= f32::MAX;
            }
        }
    }
    fn is_finite(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "overflow".into()
    }
}

/// A test-only colour unit whose result depends on where its pixel is: it adds `x / width` to
/// red and `y / height` to green of the stage its layer receives. A unit like this agrees
/// between a rendered raster and a point sample only when both paths hand it the same
/// coordinates, which is exactly what the positional contract promises a finish-stage effect.
#[derive(Debug)]
pub(crate) struct Positional {
    width: f32,
    height: f32,
}

impl PointwiseColor for Positional {
    fn apply_row(&self, y: u32, x0: u32, rgb: &mut [[f32; 3]]) {
        for (offset, pixel) in rgb.iter_mut().enumerate() {
            pixel[0] += (x0 + offset as u32) as f32 / self.width;
            pixel[1] += y as f32 / self.height;
        }
    }
    fn is_finite(&self) -> bool {
        self.width.is_finite() && self.height.is_finite()
    }
    fn describe(&self) -> String {
        format!("positional {}x{}", self.width, self.height)
    }
}

pub(crate) const TEST_COLOR_EFFECT: &str = "test.colour";

/// A test-only module with one colour effect. The Basic module is a separate deliverable; this
/// one exists so the host's colour stage can be tested without it. Its effect declares itself
/// maskable, so a masked layer of it compiles exactly as a delivered maskable effect's does.
pub(crate) struct ColorTestModule {
    descriptor: ModuleDescriptor,
    /// Handed to the `counting` unit when a payload asks for one, so a test can count the pixels
    /// a masked operation actually evaluated.
    counter: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

impl ColorTestModule {
    pub(crate) fn with_counter(
        counter: Option<Arc<std::sync::atomic::AtomicUsize>>,
    ) -> Arc<dyn ToolModule> {
        Arc::new(Self {
            descriptor: ModuleDescriptor {
                id: "test.colour".into(),
                title: "Test colour".into(),
                hint: None,
                effects: vec![EffectDescriptor {
                    id: TEST_COLOR_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Color,
                    order: 0,
                    maskable: true,
                    artifacts: false,
                    single: false,
                    sources: Vec::new(),
                }],
                actions: Vec::new(),
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            },
            counter,
        })
    }

    fn shared() -> Arc<dyn ToolModule> {
        Self::with_counter(None)
    }
}

impl ToolModule for ColorTestModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(&self, _: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Err(Error::internal("no actions"))
    }
    fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Ok(ActionPlan::NoOp)
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe(&self, _: &str, _: u32, payload: &Value) -> Result<crate::LayerReport, Error> {
        Ok(crate::LayerReport::new(format!("test colour {payload}")))
    }
    fn compile(&self, _: &str, _: u32, payload: &Value, stage: Stage) -> Result<Processing, Error> {
        let mut units: Vec<Arc<dyn PointwiseColor>> = Vec::new();
        for ev in payload["exposure"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
        {
            units.push(Arc::new(Exposure::new(ev.as_f64().expect("an EV number"))));
        }
        if payload["infinite"] == json!(true) {
            units.push(Arc::new(Exposure::new(f64::INFINITY)));
        }
        for _ in 0..payload["overflow"].as_u64().unwrap_or(0) {
            units.push(Arc::new(Overflow));
        }
        if payload["positional"] == json!(true) {
            units.push(Arc::new(Positional {
                width: stage.width as f32,
                height: stage.height as f32,
            }));
        }
        if payload["counting"] == json!(true)
            && let Some(counter) = &self.counter
        {
            units.push(Arc::new(Counting(counter.clone())));
        }
        Ok(Processing::Color(ColorOperation::new(units)))
    }
}

pub(crate) fn colour_layer(payload: Value) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: TEST_COLOR_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload,
        mask: None,
        artifacts: Vec::new(),
    }
}

/// One colour layer of exposure units in order.
pub(crate) fn exposure_layer(evs: &[f64]) -> Layer {
    colour_layer(json!({ "exposure": evs }))
}

/// One colour layer whose unit depends on its pixel position.
pub(crate) fn positional_layer() -> Layer {
    colour_layer(json!({ "positional": true }))
}

pub(crate) fn colour_registry() -> ModuleRegistry {
    let mut registry = geometry_registry();
    registry.register(ColorTestModule::shared()).unwrap();
    registry
}

pub(crate) fn colour_recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        format: crate::RECIPE_FORMAT,
        layers,
        masks: Vec::new(),
        ..Recipe::default()
    }
}

/// The sRGB transfer function forwards in f64, from the one shared reference, unclamped; the
/// renderer's own encoder rounds to a byte and is not consulted here.
pub(crate) fn encode_reference(linear: f64) -> f64 {
    srgb::encode_extended(linear)
}

pub(crate) fn decode_reference(encoded: f64) -> f64 {
    srgb::decode_encoded(encoded)
}

/// The tolerance the design freezes for float colour: an exact code everywhere except within
/// `1e-6 + 1e-6·|value|` of the threshold between two codes, where one code of difference is
/// permitted because the production path decodes and multiplies in f32.
pub(crate) fn assert_code_within_tolerance(actual: u8, expected: u8, linear: f64, case: &str) {
    if actual == expected {
        return;
    }
    let difference = i32::from(actual) - i32::from(expected);
    assert!(difference.abs() <= 1, "{case}: {actual} against {expected}");
    let crossed = u32::from(actual.max(expected));
    let threshold = decode_reference((f64::from(crossed) - 0.5) / 255.0);
    let tolerance = 1e-6 + 1e-6 * threshold.abs();
    assert!(
        (linear - threshold).abs() <= tolerance,
        "{case}: {actual} against {expected} is not within {tolerance} of the threshold {threshold} ({linear})"
    );
}

/// Every grey, once per byte, so a wrong table entry cannot hide behind a neighbour.
pub(crate) fn greys() -> SourceImage {
    let mut rgba = Vec::with_capacity(256 * 4);
    for value in 0..=255_u8 {
        rgba.extend([value, value, value, 255]);
    }
    SourceImage {
        width: 256,
        height: 1,
        rgba: rgba.into(),
        fingerprint: "sha256:greys".into(),
        orientation: 1,
        capture: Default::default(),
    }
}

/// A test-only colour unit that counts the pixels it was handed, so "outside the bounds nothing
/// is evaluated" is a counted fact and not an argument. It leaves red alone and marks green, so a
/// frame also shows where it ran.
#[derive(Debug)]
pub(crate) struct Counting(Arc<std::sync::atomic::AtomicUsize>);

impl PointwiseColor for Counting {
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        self.0
            .fetch_add(rgb.len(), std::sync::atomic::Ordering::Relaxed);
        for pixel in rgb {
            pixel[1] = 1.0;
        }
    }
    fn is_finite(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "counting".into()
    }
}
