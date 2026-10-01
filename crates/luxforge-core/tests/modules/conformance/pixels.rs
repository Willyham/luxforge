//! In-process evaluation through the public entry points: the byte path over the decoded JPEG and
//! the RAW linear path over the same pixels in linear light. Every stack evaluated here is either a
//! stack the host committed, read back through the API, or a stack the API cannot store (two
//! layers for one target, a refused payload), assembled directly to prove the refusal.
use super::{Checked, ensure, shape::FieldPatch};
use luxforge_core::{
    ErrorKind, ExactGeometry, Layer, LinearImage, LinearSettings, ModuleRegistry, Processing,
    RECIPE_FORMAT, Raster, Recipe, SnapshotId, SourceImage, Stage, open_source,
};
use luxforge_reference::srgb;
use luxforge_testkit::fixtures::{render, render_linear, sample, sample_linear};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

/// The same picture on both evaluation paths.
pub struct Sources {
    pub byte: SourceImage,
    pub linear: LinearImage,
}

impl Sources {
    /// The fixture as the owner decodes it, and the same pixels decoded to linear light as the
    /// planar source a RAW development hands the linear path.
    pub fn open(fixture: &Path) -> Checked<Self> {
        let byte = open_source(fixture)
            .map_err(|error| format!("{} did not decode: {error}", fixture.display()))?;
        let count = byte.width as usize * byte.height as usize;
        let mut planes = vec![0.0f32; count * 3];
        for (index, pixel) in byte.rgba.chunks_exact(4).enumerate() {
            for channel in 0..3 {
                planes[channel * count + index] = decode(pixel[channel]);
            }
        }
        let linear = LinearImage::with_fingerprint(
            byte.width,
            byte.height,
            planes,
            format!("{}-linear", byte.fingerprint),
        )
        .map_err(|error| format!("the linear source was refused: {error}"))?;
        Ok(Self { byte, linear })
    }

    pub fn stage(&self) -> Stage {
        Stage {
            width: self.byte.width,
            height: self.byte.height,
        }
    }

    /// The decoded source's own byte at one pixel.
    pub fn source_pixel(&self, (x, y): (u32, u32)) -> Value {
        let index = (y as usize * self.byte.width as usize + x as usize) * 4;
        json!(&self.byte.rgba[index..index + 4])
    }
}

/// The sRGB transfer function, from the one shared reference.
fn decode(code: u8) -> f32 {
    srgb::decode(code) as f32
}

/// The pixels every sample-against-render comparison reads on an output stage: the four corners,
/// the middle of each edge, the centre and a 4 x 3 stride across the interior. A position-dependent
/// unit is most distinctive at the corners, and a spatial layer evaluates the tile that holds a
/// pixel, so its edges are where a tiling or clamping mistake would show.
pub fn probes(width: u32, height: u32) -> Vec<(u32, u32)> {
    let (right, bottom) = (width.saturating_sub(1), height.saturating_sub(1));
    let mut probes = vec![
        (0, 0),
        (right, 0),
        (0, bottom),
        (right, bottom),
        (width / 2, 0),
        (0, height / 2),
        (right, height / 2),
        (width / 2, bottom),
        (width / 2, height / 2),
    ];
    for row in 0..3 {
        for column in 0..4 {
            probes.push((width * (2 * column + 1) / 8, height * (2 * row + 1) / 6));
        }
    }
    probes.sort_unstable();
    probes.dedup();
    probes
}

pub fn stack(layers: Vec<Layer>) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers,
        masks: Vec::new(),
        ..Recipe::default()
    }
}

pub fn layer(module: &FieldPatch, payload: &Value) -> Layer {
    Layer {
        effect_format: module.effect.format,
        ..Layer::new(module.effect.id.clone(), payload.clone())
    }
}

/// The byte-path render of one stack.
pub fn raster(registry: &ModuleRegistry, sources: &Sources, recipe: &Recipe) -> Checked<Raster> {
    render(registry, &sources.byte, SnapshotId::new(), recipe)
        .map_err(|error| format!("the stack did not render: {error}"))
}

/// Empty compiled work preserves the source allocation and both domains' exact identity pixels,
/// whether the stored configuration is all-default or retains a nondefault inactive setting.
fn identity_pixels(
    registry: &ModuleRegistry,
    sources: &Sources,
    recipe: &Recipe,
    bytes: &Raster,
    linear_identity: &Raster,
    what: &str,
) -> Checked {
    ensure(
        (bytes.width, bytes.height) == (sources.byte.width, sources.byte.height)
            && Arc::ptr_eq(&bytes.rgba, &sources.byte.rgba),
        format!("{what} did not preserve dimensions and share the source allocation"),
    )?;
    let linear = render_linear(
        registry,
        &sources.linear,
        SnapshotId::new(),
        recipe,
        LinearSettings::default(),
    )
    .map_err(|error| format!("{what} did not render on the linear path: {error}"))?;
    ensure(
        (linear.width, linear.height) == (linear_identity.width, linear_identity.height)
            && linear.rgba == linear_identity.rgba,
        format!("{what} changed identity dimensions or a byte on the linear path"),
    )
}

/// `sample` equals the rendered byte at every probe on the byte path, and `sample_linear` equals
/// `render_linear` at every probe on the linear path, for the same stack.
pub fn sample_equals_render(
    registry: &ModuleRegistry,
    sources: &Sources,
    recipe: &Recipe,
) -> Checked<Value> {
    let bytes = raster(registry, sources, recipe)?;
    let probes = probes(bytes.width, bytes.height);
    let mut byte_pixels = Vec::with_capacity(probes.len());
    for &(x, y) in &probes {
        let sampled = sample(registry, &sources.byte, recipe, x, y)
            .map_err(|error| format!("the byte path did not sample ({x}, {y}): {error}"))?
            .rgba;
        let rendered = bytes.pixel(x, y);
        ensure(
            sampled == rendered,
            format!(
                "the byte path sampled {sampled:?} at ({x}, {y}) where it rendered {rendered:?}"
            ),
        )?;
        byte_pixels.push(json!(rendered));
    }
    let settings = LinearSettings::default();
    let linear = render_linear(
        registry,
        &sources.linear,
        SnapshotId::new(),
        recipe,
        settings,
    )
    .map_err(|error| format!("the stack did not render on the linear path: {error}"))?;
    ensure(
        (linear.width, linear.height) == (bytes.width, bytes.height),
        format!(
            "the linear path rendered {}x{} where the byte path rendered {}x{}",
            linear.width, linear.height, bytes.width, bytes.height
        ),
    )?;
    let mut linear_pixels = Vec::with_capacity(probes.len());
    for &(x, y) in &probes {
        let sampled = sample_linear(registry, &sources.linear, recipe, settings, x, y)
            .map_err(|error| format!("the linear path did not sample ({x}, {y}): {error}"))?
            .rgba;
        let rendered = linear.pixel(x, y);
        ensure(
            sampled == rendered,
            format!(
                "the linear path sampled {sampled:?} at ({x}, {y}) where it rendered {rendered:?}"
            ),
        )?;
        linear_pixels.push(json!(rendered));
    }
    Ok(json!({
        "output": [bytes.width, bytes.height],
        "layers": recipe.layers.iter().map(|layer| layer.effect_id.clone()).collect::<Vec<_>>(),
        "probes": probes,
        "byte": byte_pixels,
        "linear": linear_pixels,
    }))
}

/// How many units a payload compiles to at `stage`: colour, spatial or geometry processing.
/// A neutral payload or inactive stored configuration has no units or an identity mapping,
/// which the host drops entirely.
fn compiled_units(
    registry: &ModuleRegistry,
    module: &FieldPatch,
    payload: &Value,
    stage: Stage,
) -> Checked<usize> {
    let provider = registry
        .module(&module.id)
        .ok_or("the module is not registered")?;
    let processing = provider
        .compile(
            &module.effect.id,
            module.effect.format,
            payload,
            luxforge_core::CompileStage::exact(stage),
        )
        .map_err(|error| format!("{payload} did not compile: {error}"))?;
    match processing {
        Processing::Color(operation) => Ok(operation.len()),
        Processing::Spatial(operation) => Ok(operation.len()),
        Processing::ExactGeometry(mapping) => Ok(usize::from(
            mapping
                != ExactGeometry {
                    a: 1,
                    b: 0,
                    c: 0,
                    d: 1,
                    tx: 0,
                    ty: 0,
                    output_width: stage.width,
                    output_height: stage.height,
                },
        )),
        Processing::Warp(step) => Ok(usize::from(!step.is_identity())),
        Processing::Resample(_) => Ok(1),
        other => Err(format!(
            "{payload} compiled to {other:?}, which is not field-patch processing"
        )),
    }
}

/// The payload checks that need no catalog: every neutral spelling compiles to nothing, keeps the
/// identity byte path and shares the source allocation. Configuration neutrality and compiled
/// pixel work are checked separately: inactive nondefault settings retain their reported values
/// while proving the same pixel identity, and active payloads compile and render processing.
pub fn payloads(
    registry: &ModuleRegistry,
    module: &FieldPatch,
    sources: &Sources,
) -> Checked<Value> {
    let stage = sources.stage();
    let identity = render_linear(
        registry,
        &sources.linear,
        SnapshotId::new(),
        &stack(Vec::new()),
        LinearSettings::default(),
    )
    .map_err(|error| format!("the empty stack did not render on the linear path: {error}"))?;

    let mut neutral = Vec::new();
    for (what, payload) in module.neutral_payloads() {
        let layer = layer(module, &payload);
        registry
            .validate_layer(&layer)
            .map_err(|error| format!("{what} {payload} was refused: {error}"))?;
        let report = registry
            .layer_report(&layer)
            .map_err(|reason| format!("{what} was not described: {reason}"))?;
        ensure(
            report.neutral,
            format!("{what} {payload} is not reported neutral"),
        )?;
        let described = report.summary;
        ensure(
            described == "Neutral",
            format!("{what} {payload} is described as {described:?}, not Neutral"),
        )?;
        let values = report.values;
        for field in &module.fields {
            ensure(
                field.same(values.get(&field.name), &field.default),
                format!(
                    "{what} reports {} as {:?}",
                    field.name,
                    values.get(&field.name)
                ),
            )?;
        }
        let units = compiled_units(registry, module, &payload, stage)?;
        ensure(
            units == 0,
            format!("{what} {payload} compiled to {units} unit(s) instead of nothing"),
        )?;
        let recipe = stack(vec![layer]);
        let rendered = raster(registry, sources, &recipe)?;
        identity_pixels(
            registry,
            sources,
            &recipe,
            &rendered,
            &identity,
            &format!("{what} {payload}"),
        )?;
        neutral.push(json!({"payload": payload, "shows": what}));
    }

    let mut single = Vec::new();
    for (field, payload) in module.single_field_payloads() {
        let layer = layer(module, &payload);
        registry
            .validate_layer(&layer)
            .map_err(|error| format!("{payload} was refused: {error}"))?;
        let report = registry
            .layer_report(&layer)
            .map_err(|reason| format!("{payload} was not described: {reason}"))?;
        let is_neutral = report.neutral;
        let described = report.summary;
        ensure(
            described != "Neutral",
            format!("{payload} moves a field but is described as Neutral"),
        )?;
        let values = report.values;
        for other in &module.fields {
            let expected = if other.name == field.name {
                field.high()
            } else {
                other.default.clone()
            };
            ensure(
                other.same(values.get(&other.name), &expected),
                format!(
                    "{payload} reports {} as {:?}",
                    other.name,
                    values.get(&other.name)
                ),
            )?;
        }
        let units = compiled_units(registry, module, &payload, stage)?;
        let recipe = stack(vec![layer]);
        let rendered = raster(registry, sources, &recipe)?;
        let shared = Arc::ptr_eq(&rendered.rgba, &sources.byte.rgba);
        if !module.renders {
            renders_nothing(&payload, units, shared)?;
        }
        if is_neutral {
            ensure(
                units == 0,
                format!(
                    "{payload} is neutral by the module's rule but compiled to {units} unit(s)"
                ),
            )?;
        }
        if units == 0 {
            identity_pixels(
                registry,
                sources,
                &recipe,
                &rendered,
                &identity,
                &payload.to_string(),
            )?;
        } else {
            ensure(
                !shared,
                format!("{payload} compiled to {units} unit(s) but shared the source allocation"),
            )?;
        }
        single.push(
            json!({"payload": payload, "neutral": is_neutral, "units": units,
            "pixels_identity": units == 0, "shares_source": shared,
            "changes_byte_pixels": rendered.rgba != sources.byte.rgba}),
        );
    }
    ensure(
        !module.renders
            || single.iter().any(|case| {
                case["units"].as_u64().is_some_and(|n| n > 0)
                    && case["changes_byte_pixels"] == json!(true)
            }),
        "no single field compiles pixel work that changes the image",
    )?;

    let mut whole = Vec::new();
    let mut rasters = Vec::new();
    for payload in [module.full_high(), module.full_low()] {
        let layer = layer(module, &payload);
        ensure(
            !registry
                .layer_report(&layer)
                .is_ok_and(|report| report.neutral),
            format!("{payload} moves every field but is reported neutral"),
        )?;
        let units = compiled_units(registry, module, &payload, stage)?;
        let recipe = stack(vec![layer]);
        let rendered = raster(registry, sources, &recipe)?;
        let shared = Arc::ptr_eq(&rendered.rgba, &sources.byte.rgba);
        if module.renders {
            ensure(units > 0, format!("{payload} compiled to nothing"))?;
            ensure(!shared, format!("{payload} shared the source allocation"))?;
        } else {
            renders_nothing(&payload, units, shared)?;
            identity_pixels(
                registry,
                sources,
                &recipe,
                &rendered,
                &identity,
                &payload.to_string(),
            )?;
        }
        whole.push(json!({"payload": payload, "units": units}));
        rasters.push(rendered);
    }
    ensure(
        !module.renders || rasters[0].rgba != rasters[1].rgba,
        "the two whole payloads render the same bytes, so history could not tell them apart",
    )?;
    Ok(json!({"neutral": neutral, "single_field": single, "whole": whole}))
}

/// A developer proof's layer describes values and changes no pixel: whatever it holds, it compiles
/// to nothing and its render shares the source allocation.
fn renders_nothing(payload: &Value, units: usize, shared: bool) -> Checked {
    ensure(
        units == 0 && shared,
        format!(
            "{payload} of a developer proof compiled to {units} unit(s) and {} the source \
             allocation",
            if shared { "shared" } else { "did not share" }
        ),
    )
}

/// A stored payload the provider cannot read is refused by name and never rewritten: a format the
/// module does not declare is `incompatible`, and an unknown field, a value outside its range and
/// a payload that is not an object are `validation`.
pub fn stored_refusals(
    registry: &ModuleRegistry,
    module: &FieldPatch,
    sources: &Sources,
) -> Checked<Value> {
    let effect = &module.effect;
    let first = &module.fields[0];
    let future = Layer {
        effect_format: effect.format + 1,
        ..layer(module, &module.full_high())
    };
    let kept = future.clone();
    let error = registry
        .validate_layer(&future)
        .err()
        .ok_or("a layer of an undeclared format was accepted")?;
    ensure(
        error.kind == ErrorKind::Incompatible && error.detail.contains("unsupported effect format"),
        format!("an undeclared format was refused with {error}"),
    )?;
    let render_error = raster(registry, sources, &stack(vec![future.clone()]))
        .err()
        .ok_or("a layer of an undeclared format rendered")?;
    ensure(
        render_error.contains("unsupported effect format"),
        format!("rendering an undeclared format failed with {render_error}"),
    )?;
    ensure(future == kept, "the refused layer was rewritten")?;

    let mut unknown = module.full_high();
    unknown["conformance-unknown"] = json!(1.0);
    let mut out_of_range = serde_json::Map::new();
    out_of_range.insert(first.name.clone(), first.outside());
    let mut refused = Vec::new();
    for (what, payload, names) in [
        ("an unknown field", unknown, Some("conformance-unknown")),
        (
            "a value its declaration refuses",
            Value::Object(out_of_range),
            Some(first.name.as_str()),
        ),
        ("a payload that is not an object", json!([]), None),
    ] {
        let error = registry
            .validate_layer(&layer(module, &payload))
            .err()
            .ok_or_else(|| format!("{what} {payload} was accepted"))?;
        ensure(
            error.kind == ErrorKind::Validation
                && names.is_none_or(|name| error.detail.contains(name)),
            format!("{what} {payload} was refused with {error}"),
        )?;
        refused.push(json!({"payload": payload, "error": error.to_string()}));
    }
    Ok(json!({"format": error.to_string(), "fields": refused}))
}

/// A stack holding two layers of the effect for one target is refused by name, by rendering and by
/// sampling alike, and nothing is rewritten: every layer still describes itself.
pub fn refuses_as_ambiguous(
    registry: &ModuleRegistry,
    module: &FieldPatch,
    sources: &Sources,
    recipe: &Recipe,
) -> Checked<Value> {
    let expected = format!("ambiguous {} layers", module.title);
    let kept = recipe.clone();
    let rendered = render(registry, &sources.byte, SnapshotId::new(), recipe)
        .err()
        .ok_or("a stack with two layers for one target rendered")?;
    let sampled = sample(registry, &sources.byte, recipe, 0, 0)
        .err()
        .ok_or("a stack with two layers for one target sampled")?;
    for error in [&rendered, &sampled] {
        ensure(
            error.kind == ErrorKind::Validation && error.detail == expected,
            format!("two layers for one target were refused with {error}, not {expected}"),
        )?;
    }
    ensure(recipe == &kept, "the refused stack was rewritten")?;
    let provider = registry
        .module(&module.id)
        .ok_or("the module is not registered")?;
    for layer in recipe
        .layers
        .iter()
        .filter(|layer| layer.effect_id == module.effect.id)
    {
        provider
            .describe(&layer.effect_id, layer.effect_format, &layer.payload)
            .map_err(|error| format!("a refused stack's layer is unreadable: {error}"))?;
    }
    Ok(json!({"error": rendered.to_string()}))
}
