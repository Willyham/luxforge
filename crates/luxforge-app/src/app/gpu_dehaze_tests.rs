//! Dehaze behind Detail at 100% (`docs/design/gpu-preview.md`, "At 100% and above"): which
//! atmospheric light a drag's GPU region plan can honestly draw with when Dehaze's global estimate
//! sits behind Detail, measured over the corpus's sources against the exact frame the drag settles
//! to and the CPU's moving frame it stands in for.
//!
//! The exact visible region cannot be windowed there, so the CPU path at 100% draws a whole-frame
//! proxy fitted to the visible region's size while the input moves and the whole exact frame once
//! it settles. A drag of the Presence layer leaves the light's input as it was, so the light the
//! settled frame stored is the drafted stack's own. A drag of Detail, or of a colour layer between
//! Detail and Presence, changes it every tick; the candidates are:
//!
//! - **exact**: the drafted stack's own light, which no tick has: the floor, the GPU's arithmetic
//!   alone;
//! - **held**: the light of the stack the drag started from, which the store holds once that stack
//!   has settled, held for the drag;
//! - **fit**: the light the Fit proxy of the drafted stack prepares, a reduced whole stage;
//! - **moving**: the light the CPU's moving frame at 100% prepares from its own proxy stage.
//!
//! Each candidate's GPU frame is judged first against the exact frame, by the spatial limits. One
//! that misses them is held to the owner's rule instead: its jump to the exact frame no larger than
//! the CPU's moving frame's, in mean and worst block, and the spatial limits against that moving
//! frame at the moving frame's own scale, which leaves its resolution out. The moving frame drawn
//! at 100%, upsampled, is reported beside them as context: against it every frame at full scale
//! differs by the proxy's resolution, whatever its light.
use super::gpu_qualification::{
    CorpusSource, apply_steps, corpus_sources, figures, fit_bounds, headless, largest_view, worst,
};
use luxforge_core::{
    BASIC_EFFECT, Cancel, DETAIL_EFFECT, EstimateSource, GpuAnswer, GpuEstimates, GpuPlanRequest,
    OwnerHandle, PRESENCE_EFFECT, PreviewRequest, ProxyBounds, Raster, Region, RenderContext,
    RenderOptions, gpu_plan_with, qualification, render,
};
use luxforge_reference::{
    preview_error::{self, Class, Rgb8, Statistics},
    srgb,
};
use luxforge_ui::photo_surface::{GpuBoundary, gpu_preview::qualification::Qualifier};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

/// One drag: the stack it starts from, the steps that draft it, and the layer it drafts.
struct Drag {
    id: &'static str,
    committed: Vec<Value>,
    drafted: Value,
    effect: &'static str,
}

fn step(method: &str, params: Value) -> Value {
    json!({"api": {"method": method, "params": params}})
}

fn presence(texture: i32, clarity: i32, dehaze: i32) -> Value {
    step(
        "edit.set-presence",
        json!({"texture": texture, "clarity": clarity, "dehaze": dehaze}),
    )
}

fn detail(params: Value) -> Value {
    step("edit.set-detail", params)
}

fn basic(params: Value) -> Value {
    step("edit.set-basic", params)
}

/// The corpus's three Detail settings: the study's moderate one, noise stress and sharpen stress.
fn detail_moderate() -> Value {
    detail(json!({"luminance": 40, "colour": 40, "sharpening": 50, "radius": 1.0}))
}

fn detail_noise() -> Value {
    detail(json!({"luminance": 100, "colour": 100, "luminance-detail": 0, "colour-detail": 0}))
}

fn detail_sharpen() -> Value {
    detail(json!({"sharpening": 150, "radius": 3, "sharpen-detail": 100, "sharpen-masking": 0}))
}

/// The corpus's full Basic layer, its white balance on a JPEG only, as the corpus sets it.
fn basic_moderate(raw: bool) -> Value {
    let mut params = json!({"exposure": 0.5, "contrast": 25.0, "highlights": -30.0,
        "shadows": 30.0, "whites": -15.0, "blacks": 15.0, "vibrance": 30.0, "saturation": 15.0});
    if !raw {
        params["temperature"] = json!(20.0);
        params["tint"] = json!(-10.0);
    }
    basic(params)
}

/// The drags measured on each source:
///
/// - Presence over Detail, to every field at +100 and at -100;
/// - Detail from none to each of the corpus's settings, from moderate to sharpen stress, and to
///   sharpen stress under every Presence field at +100 and at -100;
/// - a Basic layer under Presence, after Detail where the placement rule puts it: the corpus's
///   settings, 1.5 stops either way with contrast, three stops either way with Whites and Blacks
///   at the same end, and three stops under every Presence field at +100 and at -100.
fn drags(raw: bool) -> Vec<Drag> {
    let moderate = || presence(50, 50, 30);
    let strong = || presence(100, 100, 100);
    let negative = || presence(-100, -100, -100);
    let up = || {
        basic(
            json!({"exposure": 1.5, "contrast": 60.0, "highlights": -60.0, "shadows": 60.0,
            "whites": 40.0, "blacks": -20.0}),
        )
    };
    let down = || basic(json!({"exposure": -1.5, "contrast": 40.0}));
    let plus3 = || basic(json!({"exposure": 3.0, "whites": 100.0, "blacks": 100.0}));
    let minus3 = || basic(json!({"exposure": -3.0, "whites": -100.0, "blacks": -100.0}));
    let drag = |id, committed: Vec<Value>, drafted, effect| Drag {
        id,
        committed,
        drafted,
        effect,
    };
    let base = || vec![detail_moderate(), moderate()];
    vec![
        drag("presence", base(), strong(), PRESENCE_EFFECT),
        drag("presence-negative", base(), negative(), PRESENCE_EFFECT),
        drag(
            "detail-moderate",
            vec![moderate()],
            detail_moderate(),
            DETAIL_EFFECT,
        ),
        drag(
            "detail-noise",
            vec![moderate()],
            detail_noise(),
            DETAIL_EFFECT,
        ),
        drag(
            "detail-sharpen",
            vec![moderate()],
            detail_sharpen(),
            DETAIL_EFFECT,
        ),
        drag(
            "detail-moderate-to-sharpen",
            base(),
            detail_sharpen(),
            DETAIL_EFFECT,
        ),
        drag(
            "detail-sharpen-strong",
            vec![strong()],
            detail_sharpen(),
            DETAIL_EFFECT,
        ),
        drag(
            "detail-sharpen-negative",
            vec![negative()],
            detail_sharpen(),
            DETAIL_EFFECT,
        ),
        drag("basic-moderate", base(), basic_moderate(raw), BASIC_EFFECT),
        drag("basic-up", base(), up(), BASIC_EFFECT),
        drag("basic-down", base(), down(), BASIC_EFFECT),
        drag("basic-plus3", base(), plus3(), BASIC_EFFECT),
        drag("basic-minus3", base(), minus3(), BASIC_EFFECT),
        drag(
            "basic-plus3-strong",
            vec![detail_moderate(), strong()],
            plus3(),
            BASIC_EFFECT,
        ),
        drag(
            "basic-minus3-negative",
            vec![detail_moderate(), negative()],
            minus3(),
            BASIC_EFFECT,
        ),
    ]
}

/// Each unit's estimate, as the qualification module answers it.
type Estimates = Vec<Option<Vec<f64>>>;

/// The atmospheric light among a Presence layer's estimates.
fn light(estimates: &Estimates) -> Option<[f64; 3]> {
    estimates
        .iter()
        .flatten()
        .find(|values| values.len() == 3)
        .map(|values| [values[0], values[1], values[2]])
}

/// `estimates` with the atmospheric light among them replaced by `light`.
fn with_light(estimates: &Estimates, light: Option<Vec<f64>>) -> Estimates {
    estimates
        .iter()
        .map(|values| match values {
            Some(values) if values.len() == 3 => light.clone(),
            other => other.clone(),
        })
        .collect()
}

/// The light a colour drag from layer `boundary` of `recipe` would take from a held reduced stage
/// (`qualification::light_after_reduction`), and how long the part a tick repeats took.
fn reduced_light(
    registry: &luxforge_core::ModuleRegistry,
    evaluation: &luxforge_core::Evaluation,
    recipe: &luxforge_core::Recipe,
    boundary: usize,
    presence: usize,
) -> Result<(Option<Vec<f64>>, std::time::Duration), String> {
    let prefix = luxforge_core::Recipe {
        layers: recipe.layers[..boundary]
            .iter()
            .chain([&recipe.layers[presence]])
            .cloned()
            .collect(),
        ..recipe.clone()
    };
    let colour = luxforge_core::Recipe {
        layers: recipe.layers[boundary..presence].to_vec(),
        ..recipe.clone()
    };
    qualification::light_after_reduction(registry, evaluation.source().into(), &prefix, &colour)
        .map_err(|error| error.to_string())
}

fn rgb_of(raster: &Raster) -> Vec<u8> {
    raster
        .rgba
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// `rect` of a whole `width`-wide RGB frame.
fn crop(rgb: &[u8], width: u32, rect: Region) -> Vec<u8> {
    let mut out = Vec::with_capacity(rect.pixels() as usize * 3);
    for y in rect.y0..rect.y1() {
        let start = (y as usize * width as usize + rect.x0 as usize) * 3;
        out.extend_from_slice(&rgb[start..start + rect.width as usize * 3]);
    }
    out
}

/// `rect` of a `stage` output drawn from `proxy`, a whole frame of that output at a smaller size,
/// as the photo surface draws it at 100%: each pixel's centre mapped onto the proxy and its four
/// nearest texels blended in linear light, as an sRGB texture's linear filter blends them.
fn upsampled(proxy: &Raster, stage: (u32, u32), rect: Region) -> Vec<u8> {
    let (pw, ph) = (proxy.width as usize, proxy.height as usize);
    let linear: Vec<f64> = proxy
        .rgba
        .chunks_exact(4)
        .flat_map(|pixel| [0, 1, 2].map(|c| srgb::decode(pixel[c])))
        .collect();
    let (sx, sy) = (
        proxy.width as f64 / f64::from(stage.0),
        proxy.height as f64 / f64::from(stage.1),
    );
    let mut out = Vec::with_capacity(rect.pixels() as usize * 3);
    for y in rect.y0..rect.y1() {
        let v = ((f64::from(y) + 0.5) * sy - 0.5).clamp(0.0, (ph - 1) as f64);
        let (y0, fy) = (v.floor() as usize, v - v.floor());
        let y1 = (y0 + 1).min(ph - 1);
        for x in rect.x0..rect.x1() {
            let u = ((f64::from(x) + 0.5) * sx - 0.5).clamp(0.0, (pw - 1) as f64);
            let (x0, fx) = (u.floor() as usize, u - u.floor());
            let x1 = (x0 + 1).min(pw - 1);
            for c in 0..3 {
                let at = |x: usize, y: usize| linear[(y * pw + x) * 3 + c];
                let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
                let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
                out.push(srgb::code(top * (1.0 - fy) + bottom * fy));
            }
        }
    }
    out
}

/// The pixels of a `proxy`-sized frame of a `stage` output whose span lies wholly inside `rect`,
/// and `rgb`, a frame of `rect`, reduced onto them: each the linear-light mean of the region's
/// pixels whose centres fall in it, as a box downscale gathers them.
fn reduced(rgb: &[u8], rect: Region, stage: (u32, u32), proxy: (u32, u32)) -> (Region, Vec<u8>) {
    let scale = |of: u32, to: u32| f64::from(to) / f64::from(of);
    let (sx, sy) = (scale(stage.0, proxy.0), scale(stage.1, proxy.1));
    let first = |at: u32, s: f64| (f64::from(at) * s).ceil() as u32;
    let last = |at: u32, s: f64| (f64::from(at) * s).floor() as u32;
    let (x0, x1) = (first(rect.x0, sx), last(rect.x1(), sx));
    let (y0, y1) = (first(rect.y0, sy), last(rect.y1(), sy));
    let (width, height) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    let mut sums = vec![[0.0f64; 3]; (width * height) as usize];
    let mut counts = vec![0u32; (width * height) as usize];
    for y in rect.y0..rect.y1() {
        let py = ((f64::from(y) + 0.5) * sy).floor() as u32;
        if py < y0 || py >= y1 {
            continue;
        }
        for x in rect.x0..rect.x1() {
            let px = ((f64::from(x) + 0.5) * sx).floor() as u32;
            if px < x0 || px >= x1 {
                continue;
            }
            let at = ((py - y0) * width + (px - x0)) as usize;
            let from = ((y - rect.y0) as usize * rect.width as usize + (x - rect.x0) as usize) * 3;
            for c in 0..3 {
                sums[at][c] += srgb::decode(rgb[from + c]);
            }
            counts[at] += 1;
        }
    }
    let codes = sums
        .iter()
        .zip(&counts)
        .flat_map(|(sum, &count)| sum.map(|value| srgb::code(value / f64::from(count.max(1)))))
        .collect();
    (
        Region {
            x0,
            y0,
            width,
            height,
        },
        codes,
    )
}

fn stats(s: &Statistics) -> Value {
    json!({"mean": s.mean, "worst_block": s.worst_block, "p99": s.p99,
        "mean_delta_l": s.mean_delta_l, "max": s.max})
}

fn compare(width: u32, height: u32, a: &[u8], b: &[u8]) -> Result<Statistics, String> {
    preview_error::compare(
        Rgb8::new(width, height, a)?,
        Rgb8::new(width, height, b)?,
        [0, 0, width, height],
    )
}

fn passes(statistics: &Statistics) -> bool {
    preview_error::verdict(statistics, Class::Spatial).passed()
}

/// What one candidate light drew.
struct Candidate {
    name: &'static str,
    light: Option<[f64; 3]>,
    /// Against the exact frame the drag settles to.
    exact: Statistics,
    /// Against the CPU's moving frame at its own scale, when it is a proxy.
    at_scale: Option<Statistics>,
    /// Against the CPU's moving frame as the surface draws it at 100%.
    upsampled: Statistics,
}

/// One drag on one source: every candidate's GPU frame over the visible region, against the exact
/// region of the drafted stack and the CPU's moving frame. `held` keeps the light each starting
/// stack's settled frame stored, by source and stack, so a stack several drags start from is
/// rendered once.
fn cell(
    qualifier: &Qualifier,
    source: &CorpusSource,
    drag: &Drag,
    output: &std::path::Path,
    name: &str,
    held: &mut HashMap<String, Estimates>,
) -> Result<Value, String> {
    let catalog = output.join(format!("{name}.sqlite"));
    let (owner, join) = OwnerHandle::start(&catalog).map_err(|error| error.to_string())?;
    let client = owner.register();
    let asset = crate::app::testing::import_and_adopt(&owner, client, &source.path);
    let result = (|| -> Result<Value, String> {
        let job = |owner: &OwnerHandle| {
            crate::app::tasks::ready_preview_job(owner, PreviewRequest::new(client, asset.clone()))
        };
        let layer_of = |recipe: &luxforge_core::Recipe, effect: &str| {
            recipe
                .layers
                .iter()
                .position(|layer| layer.effect_id == effect)
                .ok_or_else(|| format!("no {effect} layer"))
        };
        // The stack the drag starts from, and the light its settled frame stores.
        apply_steps(&owner, client, &asset, &drag.committed)?;
        let key = format!("{}|{}", source.id, Value::Array(drag.committed.clone()));
        let held = match held.get(&key) {
            Some(estimates) => estimates.clone(),
            None => {
                let committed = job(&owner)?.evaluation;
                let context = RenderContext::new();
                let start = render(
                    committed.registry(),
                    committed.source(),
                    committed.recipe(),
                    RenderOptions::exact(&Cancel::never()),
                    &context,
                )
                .map_err(|error| error.to_string())?;
                start
                    .frame(committed.entry().snapshot.id.clone())
                    .map_err(|error| error.to_string())?;
                let estimates = qualification::held_estimates(
                    &start,
                    layer_of(committed.recipe(), PRESENCE_EFFECT)?,
                )
                .map_err(|error| error.to_string())?
                .ok_or("the settled frame stored no estimate")?;
                held.insert(key, estimates.clone());
                estimates
            }
        };

        // The drafted stack: its exact frame, which the drag settles to, and its own light.
        apply_steps(&owner, client, &asset, std::slice::from_ref(&drag.drafted))?;
        let drafted = job(&owner)?.evaluation;
        let registry = drafted.registry().clone();
        let recipe = drafted.recipe().clone();
        let presence = layer_of(&recipe, PRESENCE_EFFECT)?;
        let boundary_layer = layer_of(&recipe, drag.effect)?;
        let context = RenderContext::new();
        let exact = render(
            &registry,
            drafted.source(),
            &recipe,
            RenderOptions::exact(&Cancel::never()),
            &context,
        )
        .map_err(|error| error.to_string())?;
        let snapshot = drafted.entry().snapshot.id.clone();
        let whole = exact.frame(snapshot.clone()).map_err(|e| e.to_string())?;
        let own = qualification::held_estimates(&exact, presence)
            .map_err(|error| error.to_string())?
            .ok_or("the exact frame stored no estimate")?;
        let stage = (whole.width, whole.height);
        let rect = largest_view(stage, 100.0).ok_or("no visible region")?;
        let settled = crop(&rgb_of(&whole), whole.width, rect);
        drop(whole);

        // A proxy of the drafted stack at `bounds`, its frame and the light it prepares; none
        // where no proxy smaller than the source fits, and the worker renders the exact frame.
        let proxied = |bounds: ProxyBounds| -> Result<Option<(Raster, Estimates)>, String> {
            let Some((plan, _)) = qualification::fit_proxy(&exact, &registry, &recipe, bounds)
            else {
                return Ok(None);
            };
            let source = drafted.source().proxy(plan).map_err(|e| e.to_string())?;
            let context = RenderContext::new();
            let proxy = qualification::proxy_render(
                &exact,
                &registry,
                &recipe,
                bounds,
                (&source).into(),
                &context,
            )
            .map_err(|error| error.to_string())?;
            let raster = proxy.frame(snapshot.clone()).map_err(|e| e.to_string())?;
            let estimates = qualification::held_estimates(&proxy, presence)
                .map_err(|error| error.to_string())?
                .ok_or("the proxy stored no estimate")?;
            Ok(Some((raster, estimates)))
        };
        // The CPU's moving frame at 100%: the worker declines the region and renders the whole
        // frame's proxy within the region's size.
        let moving_proxy = proxied(ProxyBounds {
            width: rect.width,
            height: rect.height,
        })?;
        let (width, height) = (rect.width, rect.height);
        // The moving frame drawn at 100%, and at its own scale the pixels it holds of the region.
        let (upsampled_moving, moving, at_scale) = match &moving_proxy {
            Some((frame, estimates)) => {
                let size = (frame.width, frame.height);
                let (on, _) = reduced(&settled, rect, stage, size);
                (
                    upsampled(frame, stage, rect),
                    estimates.clone(),
                    Some((on, crop(&rgb_of(frame), frame.width, on), size)),
                )
            }
            None => (settled.clone(), own.clone(), None),
        };
        let moving_size = moving_proxy
            .as_ref()
            .map(|(frame, _)| (frame.width, frame.height));
        drop(moving_proxy);
        let fit = proxied(fit_bounds())?.map_or_else(|| own.clone(), |(_, estimates)| estimates);
        let at_scale_of = |rgb: &[u8]| -> Result<Option<Statistics>, String> {
            match &at_scale {
                Some((on, proxy, size)) => {
                    let (_, codes) = reduced(rgb, rect, stage, *size);
                    compare(on.width, on.height, &codes, proxy).map(Some)
                }
                None => Ok(None),
            }
        };

        // The region's boundary, as the worker renders it for the drag's first job.
        let format = if source.raw {
            luxforge_core::BoundaryFormat::Float
        } else {
            luxforge_core::BoundaryFormat::Half
        };
        let frame = qualification::region_boundary(
            &exact,
            boundary_layer,
            [rect.x0, rect.y0, rect.width, rect.height],
            format,
        )
        .map_err(|error| error.to_string())?;
        let full = drafted.source().dimensions();
        // For a colour drag, a light from a held reduced stage: Dehaze's own reduction of the
        // dragged layer's input over the exact stage, the drafted colour run over it.
        let mut lights = vec![
            ("exact", own.clone()),
            ("held", held),
            ("fit", fit),
            ("moving", moving),
        ];
        let mut tick = None;
        if drag.effect == BASIC_EFFECT {
            let (light, took) =
                reduced_light(&registry, &drafted, &recipe, boundary_layer, presence)?;
            lights.push(("reduced", with_light(&own, light)));
            tick = Some(took.as_secs_f64() * 1000.0);
        }
        let mut candidates = Vec::new();
        let mut charged = 0;
        for (candidate, values) in &lights {
            let (candidate, values) = (*candidate, values);
            // A store holding this candidate's light under the drafted stack's key, as a drag's
            // plan reads it.
            let context = RenderContext::new();
            let keyed = render(
                &registry,
                drafted.source(),
                &recipe,
                RenderOptions::exact(&Cancel::never()),
                &context,
            )
            .map_err(|error| error.to_string())?;
            qualification::hold_estimates(&keyed, presence, values)
                .map_err(|error| error.to_string())?;
            let request = GpuPlanRequest::exact(
                boundary_layer,
                luxforge_core::Stage {
                    width: full.0,
                    height: full.1,
                },
            )
            .drafted(boundary_layer);
            let request = if source.raw {
                request.linear()
            } else {
                request
            };
            let estimates = GpuEstimates {
                context: &context,
                source: EstimateSource::Render(drafted.source().into()),
            };
            let plan = match gpu_plan_with(&registry, &recipe, request, Some(estimates))
                .map_err(|error| error.to_string())?
            {
                GpuAnswer::Plan(plan) => *plan,
                GpuAnswer::Fallback(reason) => return Err(format!("{}: {reason}", reason.code())),
            };
            if plan.approximate() {
                return Err(format!(
                    "{candidate}: the plan did not read the light it was given"
                ));
            }
            let boundary = GpuBoundary::new(
                frame.texels.clone(),
                frame.width,
                frame.height,
                1,
                super::gpu_plan::boundary_format(frame.format),
            )
            .ok_or("a boundary")?;
            let grid = plan
                .geometry
                .grid(rect, 1.0)
                .map_err(|error| error.to_string())?
                .map(|grid| super::gpu_plan::WarpGrid::new(&grid));
            let converted = super::gpu_plan::surface_plan_over(
                &plan,
                boundary,
                frame.origin,
                grid.as_ref(),
                Some(rect),
            )
            .map_err(|reason| format!("{reason:?}"))?;
            charged = qualifier
                .charged_bytes(&converted)
                .map_err(|reason| format!("{reason:?}"))?;
            let gpu: Vec<u8> = qualifier
                .evaluate_codes(&converted)?
                .iter()
                .flat_map(|code| [code[0], code[1], code[2]])
                .collect();
            candidates.push(Candidate {
                name: candidate,
                light: light(values),
                exact: compare(width, height, &gpu, &settled)?,
                at_scale: at_scale_of(&gpu)?,
                upsampled: compare(width, height, &gpu, &upsampled_moving)?,
            });
        }
        // The CPU path's own jump at settle, and its moving frame at its scale against the exact
        // frame reduced onto it.
        let cpu_jump = compare(width, height, &upsampled_moving, &settled)?;
        let cpu_at_scale = at_scale_of(&settled)?;
        let verdict = |candidate: &Candidate| {
            let primary = passes(&candidate.exact);
            let secondary = candidate.exact.mean <= cpu_jump.mean
                && candidate.exact.worst_block <= cpu_jump.worst_block
                && candidate.at_scale.as_ref().is_some_and(passes);
            (primary, secondary)
        };
        for candidate in &candidates {
            let (primary, secondary) = verdict(candidate);
            eprintln!(
                "{name} {}: light {:?}: exact {} | at scale {} | upsampled {}{}",
                candidate.name,
                candidate.light,
                figures(&candidate.exact),
                candidate.at_scale.as_ref().map_or("-".to_owned(), figures),
                figures(&candidate.upsampled),
                match (primary, secondary) {
                    (true, _) => "",
                    (false, true) => " SECONDARY",
                    (false, false) => " MISS",
                }
            );
        }
        eprintln!(
            "{name}: CPU moving frame against exact {} | at scale {} | boundary {}x{} | charged \
             {charged} B",
            figures(&cpu_jump),
            cpu_at_scale.as_ref().map_or("-".to_owned(), figures),
            frame.width,
            frame.height
        );
        Ok(json!({
            "cell": name,
            "region": [rect.x0, rect.y0, rect.width, rect.height],
            "boundary": [frame.width, frame.height],
            "charged_bytes": charged,
            "moving_proxy": moving_size.map(|(width, height)| [width, height]),
            "reduced_tick_ms": tick,
            "cpu_moving_against_exact": stats(&cpu_jump),
            "cpu_moving_at_scale": cpu_at_scale.as_ref().map(stats),
            "candidates": candidates.iter().map(|candidate| {
                let (primary, secondary) = verdict(candidate);
                json!({
                    "candidate": candidate.name,
                    "light": candidate.light,
                    "against_exact": stats(&candidate.exact),
                    "against_moving_at_scale": candidate.at_scale.as_ref().map(stats),
                    "against_moving_upsampled": stats(&candidate.upsampled),
                    "primary": primary,
                    "secondary": secondary,
                })
            }).collect::<Vec<_>>(),
        }))
    })();
    owner.stop();
    let _ = join.join();
    let _ = std::fs::remove_file(&catalog);
    result
}

/// Every candidate light for a drag of Dehaze behind Detail at 100%, over every source this host
/// has, in the largest window the owner's display holds: each drag's GPU frame over the visible
/// region against the exact frame it settles to and the CPU's moving frame, with the budget out of
/// the way (the slot's charge is recorded, not enforced). Writes `cells.json` to
/// `LUXFORGE_GPU_CORPUS_OUTPUT`; `LUXFORGE_GPU_CORPUS_SOURCES` and `LUXFORGE_DEHAZE_DRAGS`
/// (comma-separated ids) run only those.
#[test]
#[ignore = "a measurement: set LUXFORGE_GPU_CORPUS_OUTPUT to a new directory, \
            LUXFORGE_GENERATED_FIXTURES to the generated JPEGs and, for the RAWs, \
            LUXFORGE_RAW_MANIFEST"]
fn gpu_dehaze_after_detail_candidates_at_100_percent() {
    let test = "gpu_dehaze_after_detail_candidates_at_100_percent";
    let output = std::path::PathBuf::from(
        std::env::var("LUXFORGE_GPU_CORPUS_OUTPUT").expect("LUXFORGE_GPU_CORPUS_OUTPUT"),
    );
    assert!(
        !output.exists(),
        "{} exists: use a new directory",
        output.display()
    );
    let Some(qualifier) = headless(test) else {
        return;
    };
    std::fs::create_dir_all(&output).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &std::path::Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let corpus = read(&root.join("fixtures/preview/corpus.json"));
    let generated = std::env::var("LUXFORGE_GENERATED_FIXTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("fixtures/generated"));
    let manifest = std::env::var("LUXFORGE_RAW_MANIFEST")
        .ok()
        .map(|path| read(std::path::Path::new(&path)));
    let only = |name: &str| -> Option<Vec<String>> {
        std::env::var(name)
            .ok()
            .map(|ids| ids.split(',').map(str::to_owned).collect())
    };
    let sources_only = only("LUXFORGE_GPU_CORPUS_SOURCES");
    let drags_only = only("LUXFORGE_DEHAZE_DRAGS");
    // The sources the corpus measures Detail beside Presence on.
    let chained = corpus["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|recipe| recipe["id"] == "detail-presence")
        .expect("the corpus's detail-presence recipe")["sources"]
        .clone();
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let mut cells = Vec::new();
    let mut held = HashMap::new();
    // Per drag and candidate: the figures against the exact frame, and how many cells pass the
    // primary rule, the secondary one, or neither.
    let mut summary: BTreeMap<(String, String), (Vec<Statistics>, [usize; 3])> = BTreeMap::new();
    for source in corpus_sources(&corpus, &generated, manifest.as_ref()) {
        if !chained
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == source.id.as_str())
            || sources_only
                .as_ref()
                .is_some_and(|ids| !ids.contains(&source.id))
        {
            continue;
        }
        for drag in drags(source.raw) {
            if drags_only
                .as_ref()
                .is_some_and(|ids| !ids.iter().any(|id| id == drag.id))
            {
                continue;
            }
            let name = format!("{}--{}", drag.id, source.id);
            match cell(&qualifier, &source, &drag, &output, &name, &mut held) {
                Ok(cell) => {
                    for candidate in cell["candidates"].as_array().unwrap() {
                        let s = &candidate["against_exact"];
                        let get = |key: &str| s[key].as_f64().unwrap();
                        let entry = summary
                            .entry((
                                drag.id.to_owned(),
                                candidate["candidate"].as_str().unwrap().to_owned(),
                            ))
                            .or_default();
                        entry.0.push(Statistics {
                            pixels: 0,
                            mean: get("mean"),
                            worst_block: get("worst_block"),
                            worst_block_origin: [0, 0],
                            p99: get("p99"),
                            mean_delta_l: get("mean_delta_l"),
                            max: get("max"),
                        });
                        let slot =
                            match (candidate["primary"] == true, candidate["secondary"] == true) {
                                (true, _) => 0,
                                (false, true) => 1,
                                (false, false) => 2,
                            };
                        entry.1[slot] += 1;
                    }
                    cells.push(cell);
                }
                Err(error) => {
                    eprintln!("{name}: gap: {error}");
                    cells.push(json!({"cell": name, "gap": error}));
                }
            }
        }
    }
    for ((drag, candidate), (statistics, [primary, secondary, neither])) in &summary {
        eprintln!(
            "{drag} {candidate}: worst against exact over {} sources: {} | primary {primary}, \
             secondary {secondary}, neither {neither}",
            statistics.len(),
            figures(&worst(statistics)),
        );
    }
    std::fs::write(
        output.join("cells.json"),
        serde_json::to_string_pretty(&json!({
            "adapter": qualifier.adapter(),
            "zoom": 100.0,
            "cells": cells,
        }))
        .unwrap(),
    )
    .unwrap();
}

/// One drag at Fit behind the corpus's straightened crop: the stack it starts from, the step that
/// drafts it, and the layer it drafts.
struct CroppedDrag {
    id: &'static str,
    committed: Vec<Value>,
    drafted: Value,
}

/// The corpus's straightened crop.
fn cropped() -> Value {
    step("edit.crop-fit", json!({"aspect": "16:9", "angle": 7.0}))
}

/// The drags measured at Fit behind the crop: Dehaze to ±100 and every field to +100, a Basic
/// layer under Dehaze at the corpus's settings and at three stops either way, and Detail under it.
fn cropped_drags() -> Vec<CroppedDrag> {
    let drag = |id, committed: Vec<Value>, drafted| CroppedDrag {
        id,
        committed,
        drafted,
    };
    vec![
        drag(
            "crop-dehaze",
            vec![cropped(), presence(0, 0, 50)],
            presence(0, 0, 100),
        ),
        drag(
            "crop-dehaze-negative",
            vec![cropped(), presence(0, 0, -50)],
            presence(0, 0, -100),
        ),
        drag(
            "crop-presence-all",
            vec![cropped(), presence(50, 50, 50)],
            presence(100, 100, 100),
        ),
        drag(
            "crop-basic-under-dehaze",
            vec![cropped(), presence(0, 0, 100)],
            basic(json!({"exposure": 0.5, "contrast": 25.0})),
        ),
        drag(
            "crop-basic-plus3-under-dehaze",
            vec![cropped(), presence(0, 0, 100)],
            basic(json!({"exposure": 3.0, "whites": 100.0, "blacks": 100.0})),
        ),
        drag(
            "crop-basic-minus3-under-negative",
            vec![cropped(), presence(0, 0, -100)],
            basic(json!({"exposure": -3.0, "whites": -100.0, "blacks": -100.0})),
        ),
        drag(
            "crop-detail-under-dehaze",
            vec![cropped(), presence(0, 0, 100)],
            detail_sharpen(),
        ),
    ]
}

/// The linear texels of a preview source, row by row, as a boundary over it holds them; `None` for
/// a RAW whose white balance is approximated.
fn texels_of(source: &luxforge_core::PreviewSource) -> Option<Vec<[f32; 3]>> {
    match source {
        luxforge_core::PreviewSource::Jpeg(image) => {
            let table = luxforge_core::colour::srgb::decode_table();
            Some(
                image
                    .rgba
                    .chunks_exact(4)
                    .map(|pixel| [0, 1, 2].map(|c| table[usize::from(pixel[c])]))
                    .collect(),
            )
        }
        luxforge_core::PreviewSource::Raw { image, settings } => {
            if settings.white_balance.is_some() {
                return None;
            }
            let (width, height) = (image.width(), image.height());
            Some(
                (0..height)
                    .flat_map(|y| (0..width).map(move |x| (x, y)))
                    .map(|(x, y)| image.pixel(x, y).expect("a viewed pixel"))
                    .collect(),
            )
        }
    }
}

/// One drag at Fit behind the crop on one source: the CPU's moving Fit frame of the drafted stack,
/// a windowed proxy handed the exact stage's light, against the GPU frame of the plan from the
/// stack's first pixel layer with each candidate light:
///
/// - **window**: taken on the GPU over the window the proxy holds, as a drag's plan took it;
/// - **whole**: taken on the GPU over the whole proxy stage at the same scale, the boundary holding
///   all of it;
/// - **exact**: the drafted stack's own exact-stage light, which a Presence drag reads from the
///   store;
/// - **held**: the exact-stage light of the stack the drag started from.
fn cropped_cell(
    qualifier: &Qualifier,
    source: &CorpusSource,
    drag: &CroppedDrag,
    output: &std::path::Path,
    name: &str,
) -> Result<Value, String> {
    use luxforge_core::{PhaseOutcome, PreviewIntent, PreviewQueue};
    let catalog = output.join(format!("{name}.sqlite"));
    let (owner, join) = OwnerHandle::start(&catalog).map_err(|error| error.to_string())?;
    let client = owner.register();
    let asset = crate::app::testing::import_and_adopt(&owner, client, &source.path);
    let result = (|| -> Result<Value, String> {
        let bounds = fit_bounds();
        let job = |owner: &OwnerHandle| {
            crate::app::tasks::ready_preview_job(
                owner,
                PreviewRequest::new(client, asset.clone()).proxy(bounds),
            )
        };
        let layer_of = |recipe: &luxforge_core::Recipe| {
            recipe
                .layers
                .iter()
                .position(|layer| layer.effect_id == PRESENCE_EFFECT)
                .ok_or("no Presence layer")
        };
        // The starting stack's exact-stage light.
        apply_steps(&owner, client, &asset, &drag.committed)?;
        let committed = job(&owner)?.evaluation;
        let context = RenderContext::new();
        let start = render(
            committed.registry(),
            committed.source(),
            committed.recipe(),
            RenderOptions::exact(&Cancel::never()),
            &context,
        )
        .map_err(|error| error.to_string())?;
        start
            .frame(committed.entry().snapshot.id.clone())
            .map_err(|error| error.to_string())?;
        let held = qualification::held_estimates(&start, layer_of(committed.recipe())?)
            .map_err(|error| error.to_string())?
            .ok_or("the settled frame stored no estimate")?;
        drop(start);

        // The drafted stack's moving Fit frame, through the preview worker.
        apply_steps(&owner, client, &asset, std::slice::from_ref(&drag.drafted))?;
        let mut moving = job(&owner)?;
        moving.intent = PreviewIntent::Interactive;
        let evaluation = moving.evaluation.clone();
        let mut queue = PreviewQueue::default();
        let generation = queue.request(moving);
        let cpu = luxforge_testbase::wait_for("the moving Fit frame", || {
            let result = queue.poll()?;
            match result.outcome {
                PhaseOutcome::Proxy(proxy) if result.generation == generation => {
                    Some(Ok(proxy.raster))
                }
                PhaseOutcome::Exact(exact) if result.generation == generation => {
                    Some(exact.result.map_err(|error| error.to_string()))
                }
                _ => None,
            }
        })?;
        let registry = evaluation.registry().clone();
        let recipe = evaluation.recipe().clone();
        let presence = layer_of(&recipe)?;
        let context = RenderContext::new();
        let exact = render(
            &registry,
            evaluation.source(),
            &recipe,
            RenderOptions::exact(&Cancel::never()),
            &context,
        )
        .map_err(|error| error.to_string())?;
        exact
            .frame(evaluation.entry().snapshot.id.clone())
            .map_err(|error| error.to_string())?;
        let own = qualification::held_estimates(&exact, presence)
            .map_err(|error| error.to_string())?
            .ok_or("the exact frame stored no estimate")?;
        let Some((plan, window)) = qualification::fit_proxy(&exact, &registry, &recipe, bounds)
        else {
            return Err("no proxy at these bounds".into());
        };
        let Some([x, y, _, _]) = window else {
            return Err("the proxy holds the whole stage".into());
        };
        let full = evaluation.source().dimensions();
        let (width, height) = (cpu.width, cpu.height);
        let reference = rgb_of(&cpu);
        // A gap is this cell's error, as any other reason it has no figures.
        let boundary_layer = super::gpu_qualification::boundary_layer(
            &registry,
            &recipe,
            evaluation.source().dimensions(),
        )??;
        let format = if source.raw {
            luxforge_ui::photo_surface::BoundaryFormat::Float
        } else {
            luxforge_ui::photo_surface::BoundaryFormat::Half
        };
        // The GPU frame over a boundary of `proxied`, a proxy source whose first texel is `origin`
        // of the proxy stage, with the light `light` gives, or taken on the GPU over that boundary.
        let draw = |proxied: &luxforge_core::PreviewSource,
                    origin: (u32, u32),
                    light: Option<&Estimates>|
         -> Result<(Statistics, u64), String> {
            let texels = texels_of(proxied).ok_or("an approximated white balance")?;
            let (w, h) = proxied.dimensions();
            let boundary = luxforge_ui::photo_surface::gpu_preview::qualification::boundary_as(
                format, w, h, 1, &texels,
            )
            .ok_or("a boundary")?;
            let request = GpuPlanRequest::fit(
                boundary_layer,
                luxforge_core::Stage {
                    width: plan.width,
                    height: plan.height,
                },
                luxforge_core::Stage {
                    width: full.0,
                    height: full.1,
                },
            );
            let request = if source.raw {
                request.linear()
            } else {
                request
            };
            let context = RenderContext::new();
            let keyed = render(
                &registry,
                evaluation.source(),
                &recipe,
                RenderOptions::exact(&Cancel::never()),
                &context,
            )
            .map_err(|error| error.to_string())?;
            if let Some(light) = light {
                qualification::hold_estimates(&keyed, presence, light)
                    .map_err(|error| error.to_string())?;
            }
            let estimates = GpuEstimates {
                context: &context,
                source: EstimateSource::Whole {
                    source: evaluation.source().into(),
                    stage: luxforge_core::Stage {
                        width: full.0,
                        height: full.1,
                    },
                },
            };
            let planned = match gpu_plan_with(&registry, &recipe, request, Some(estimates))
                .map_err(|error| error.to_string())?
            {
                GpuAnswer::Plan(planned) => *planned,
                GpuAnswer::Fallback(reason) => return Err(format!("{}: {reason}", reason.code())),
            };
            if light.is_some() == planned.approximate() {
                return Err("the plan did not take the light it was given".into());
            }
            let output_stage = planned.geometry.output();
            let grid = planned
                .geometry
                .grid(
                    Region {
                        x0: 0,
                        y0: 0,
                        width: output_stage.width,
                        height: output_stage.height,
                    },
                    1.0,
                )
                .map_err(|error| error.to_string())?
                .map(|grid| super::gpu_plan::WarpGrid::new(&grid));
            let converted =
                super::gpu_plan::surface_plan_at(&planned, boundary, origin, grid.as_ref())
                    .map_err(|reason| format!("{reason:?}"))?;
            let charged = qualifier
                .charged_bytes(&converted)
                .map_err(|reason| format!("{reason:?}"))?;
            let gpu: Vec<u8> = qualifier
                .evaluate_codes(&converted)?
                .iter()
                .flat_map(|code| [code[0], code[1], code[2]])
                .collect();
            Ok((compare(width, height, &gpu, &reference)?, charged))
        };
        let windowed = evaluation.source().proxy(plan).map_err(|e| e.to_string())?;
        let whole = evaluation
            .source()
            .proxy(qualification::whole_proxy(plan))
            .map_err(|e| e.to_string())?;
        // For a colour drag, a light from a held reduced stage, as at 100%.
        let mut tick = None;
        let reduced = if recipe.layers[boundary_layer].effect_id == BASIC_EFFECT {
            let (light, took) =
                reduced_light(&registry, &evaluation, &recipe, boundary_layer, presence)?;
            tick = Some(took.as_secs_f64() * 1000.0);
            Some(with_light(&own, light))
        } else {
            None
        };
        let mut lights = vec![
            ("window", &windowed, (x, y), None),
            ("whole", &whole, (0, 0), None),
            ("exact", &windowed, (x, y), Some(&own)),
            ("held", &windowed, (x, y), Some(&held)),
        ];
        if let Some(reduced) = &reduced {
            lights.push(("reduced", &windowed, (x, y), Some(reduced)));
        }
        let mut candidates = Vec::new();
        for (candidate, proxied, origin, light) in lights {
            let (statistics, charged) = draw(proxied, origin, light)?;
            eprintln!(
                "{name} {candidate}: {} | charged {charged} B{}",
                figures(&statistics),
                if passes(&statistics) { "" } else { " MISS" }
            );
            candidates.push(
                json!({"candidate": candidate, "against_cpu": stats(&statistics),
                "passed": passes(&statistics), "charged_bytes": charged,
                "light": light.and_then(light_of)}),
            );
        }
        Ok(
            json!({"cell": name, "frame": [width, height], "proxy": [plan.width, plan.height],
            "window": window, "reduced_tick_ms": tick, "candidates": candidates}),
        )
    })();
    owner.stop();
    let _ = join.join();
    let _ = std::fs::remove_file(&catalog);
    result
}

fn light_of(estimates: &Estimates) -> Option<[f64; 3]> {
    light(estimates)
}

/// Dehaze at Fit behind a straightened crop, whose proxy holds only the window of its stage the
/// crop reads, so the CPU's windowed proxy is handed the exact stage's light ([`cropped_cell`]),
/// over every source this host has. Writes `cells.json` to `LUXFORGE_GPU_CORPUS_OUTPUT`;
/// `LUXFORGE_GPU_CORPUS_SOURCES` and `LUXFORGE_DEHAZE_DRAGS` run only those.
#[test]
#[ignore = "a measurement: set LUXFORGE_GPU_CORPUS_OUTPUT to a new directory, \
            LUXFORGE_GENERATED_FIXTURES to the generated JPEGs and, for the RAWs, \
            LUXFORGE_RAW_MANIFEST"]
fn gpu_dehaze_under_a_cropped_fit_window() {
    let test = "gpu_dehaze_under_a_cropped_fit_window";
    let output = std::path::PathBuf::from(
        std::env::var("LUXFORGE_GPU_CORPUS_OUTPUT").expect("LUXFORGE_GPU_CORPUS_OUTPUT"),
    );
    assert!(
        !output.exists(),
        "{} exists: use a new directory",
        output.display()
    );
    let Some(qualifier) = headless(test) else {
        return;
    };
    std::fs::create_dir_all(&output).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &std::path::Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let corpus = read(&root.join("fixtures/preview/corpus.json"));
    let generated = std::env::var("LUXFORGE_GENERATED_FIXTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("fixtures/generated"));
    let manifest = std::env::var("LUXFORGE_RAW_MANIFEST")
        .ok()
        .map(|path| read(std::path::Path::new(&path)));
    let only = |name: &str| -> Option<Vec<String>> {
        std::env::var(name)
            .ok()
            .map(|ids| ids.split(',').map(str::to_owned).collect())
    };
    let sources_only = only("LUXFORGE_GPU_CORPUS_SOURCES");
    let drags_only = only("LUXFORGE_DEHAZE_DRAGS");
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let mut cells = Vec::new();
    let mut summary: BTreeMap<(String, String), Vec<Statistics>> = BTreeMap::new();
    for source in corpus_sources(&corpus, &generated, manifest.as_ref()) {
        if sources_only
            .as_ref()
            .is_some_and(|ids| !ids.contains(&source.id))
        {
            continue;
        }
        for drag in cropped_drags() {
            if drags_only
                .as_ref()
                .is_some_and(|ids| !ids.iter().any(|id| id == drag.id))
            {
                continue;
            }
            let name = format!("{}--{}", drag.id, source.id);
            match cropped_cell(&qualifier, &source, &drag, &output, &name) {
                Ok(cell) => {
                    for candidate in cell["candidates"].as_array().unwrap() {
                        let s = &candidate["against_cpu"];
                        let get = |key: &str| s[key].as_f64().unwrap();
                        summary
                            .entry((
                                drag.id.to_owned(),
                                candidate["candidate"].as_str().unwrap().to_owned(),
                            ))
                            .or_default()
                            .push(Statistics {
                                pixels: 0,
                                mean: get("mean"),
                                worst_block: get("worst_block"),
                                worst_block_origin: [0, 0],
                                p99: get("p99"),
                                mean_delta_l: get("mean_delta_l"),
                                max: get("max"),
                            });
                    }
                    cells.push(cell);
                }
                Err(error) => {
                    eprintln!("{name}: gap: {error}");
                    cells.push(json!({"cell": name, "gap": error}));
                }
            }
        }
    }
    for ((drag, candidate), statistics) in &summary {
        let worst = worst(statistics);
        eprintln!(
            "{drag} {candidate}: worst over {} sources: {}{}",
            statistics.len(),
            figures(&worst),
            if passes(&worst) { "" } else { " MISS" }
        );
    }
    std::fs::write(
        output.join("cells.json"),
        serde_json::to_string_pretty(&json!({"adapter": qualifier.adapter(), "cells": cells}))
            .unwrap(),
    )
    .unwrap();
}
