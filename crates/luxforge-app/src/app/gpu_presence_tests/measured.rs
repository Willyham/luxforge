//! What a masked Presence layer costs the GPU-preview budget with every link's scratch planes in
//! the slot's one pool and Texture's band in one channel (`docs/design/gpu-preview.md`, "Bounds";
//! recorded in `docs/specs/performance.md`, "The scratch pool's memory"): the slot's charge, which
//! the live slot's `gpu_preview_in_use_bytes` equals, its breakdown, how many layers the budget
//! holds, and what the same plans charge with each link holding its own scratch planes, the layout
//! without the pool.
use super::stage;
use crate::app::compare_after::DEVICE_TEXTURE_LIMIT;
use crate::app::gpu_plan::surface_plan_over;
use crate::app::gpu_qualification::{headless, largest_view};
use luxforge_core::{
    Component, ComponentMode, GpuAnswer, GpuPlanRequest, GpuSpatial, Layer, Mask, ModuleRegistry,
    PRESENCE_EFFECT, Recipe, Region, gpu_plan,
};
use luxforge_gpu::{
    BoundaryFormat, ChainCharge, GPU_PREVIEW_BUDGET, GpuBoundary, GpuPlan, GpuStep, chain_charge,
    qualification::Qualifier, texture_charge,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// The layer counts measured, the last the cap on masked spatial layers.
const COUNTS: [usize; 5] = [1, 2, 4, 8, 16];
/// The largest layer count whose charge is taken directly to check the count the budget holds.
const DIRECT: usize = 64;
/// The exact stage of a 24 MP photograph.
const PHOTOGRAPH: (u32, u32) = (6000, 4000);
/// The 100% window: the largest window the owner's display holds read through Detail, Texture
/// and Clarity, held fixed at every layer count. A real chain's window at 100% grows by every
/// chained spatial layer's halo, 207 px on every side for a masked Presence layer of Texture and
/// Clarity, so past one layer these figures understate what the desktop charges at 100%.
const WINDOW: (u32, u32) = (3778, 2578);

/// The fields every masked layer holds.
#[derive(Clone, Copy)]
enum Fields {
    TextureClarity,
    All,
}

impl Fields {
    fn name(self) -> &'static str {
        match self {
            Self::TextureClarity => "Texture and Clarity",
            Self::All => "Texture, Clarity and Dehaze",
        }
    }

    fn payload(self) -> Value {
        match self {
            Self::TextureClarity => json!({"texture": 40, "clarity": 30}),
            Self::All => json!({"texture": 40, "clarity": 30, "dehaze": 25}),
        }
    }
}

/// Where the plans are drawn: the boundary's size and stage origin, the region a percentage zoom
/// draws, and the request at JPEG's byte path.
#[derive(Clone, Copy)]
struct Place {
    name: &'static str,
    size: (u32, u32),
    origin: (u32, u32),
    region: Option<Region>,
    request: GpuPlanRequest,
}

/// `layers` masked Presence layers of `fields`, each through a feathered radial of its own: the
/// `index`-th centred on a 4 × 4 grid over the middle of the frame, inside the 100% window.
fn stack(fields: Fields, layers: usize) -> Recipe {
    let mut recipe = Recipe::default();
    for index in 0..layers {
        let mut mask = Mask::new(format!("Mask {}", index + 1));
        let (column, row) = ((index % 4) as f64, ((index / 4) % 4) as f64);
        mask.components.push(Component::new(
            "Radial 1",
            ComponentMode::Add,
            "radial",
            json!({"x": 0.3 + 0.4 * column / 3.0, "y": 0.3 + 0.4 * row / 3.0, "radius_x": 0.06,
                   "radius_y": 0.05, "angle": 10.0, "feather": 40.0}),
        ));
        recipe.layers.push(Layer {
            mask: Some(mask.id.clone()),
            ..Layer::new(PRESENCE_EFFECT, fields.payload())
        });
        recipe.masks.push(mask);
    }
    recipe
}

/// The bytes a link holding `spatial` takes for scratch planes of its own: every plane the core
/// declares less those an apply reads, which the link keeps.
fn own_scratch(spatial: &GpuSpatial, origin: (u32, u32), size: (u32, u32)) -> u64 {
    let kept: u64 = spatial
        .planes
        .iter()
        .enumerate()
        .filter(|(number, _)| {
            spatial
                .applies
                .iter()
                .any(|apply| apply.planes.contains(number))
        })
        .map(|(_, plane)| {
            let (width, height) = plane.extent(origin, size);
            u64::from(width) * u64::from(height) * plane.format.texel_bytes()
        })
        .sum();
    spatial.plane_bytes(origin, size) - kept
}

/// `plan` with `links` links, each a copy of one of its spatial steps' links in turn: a chain
/// longer than a recipe may hold, for charging the layer count the budget holds directly.
fn repeated(plan: &GpuPlan, links: usize) -> GpuPlan {
    let spatial: Vec<usize> = plan
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| matches!(step, GpuStep::Spatial(_)))
        .map(|(index, _)| index)
        .collect();
    let last = *spatial.last().expect("a spatial step");
    let bodies: Vec<&[GpuStep]> = spatial
        .iter()
        .enumerate()
        .map(|(number, &start)| {
            let end = spatial.get(number + 1).copied().unwrap_or(last + 1);
            &plan.steps[start..end]
        })
        .collect();
    let mut steps = plan.steps[..spatial[0]].to_vec();
    for link in 0..links {
        steps.extend_from_slice(bodies[link % bodies.len()]);
    }
    steps.extend_from_slice(&plan.steps[last + 1..]);
    GpuPlan {
        steps,
        ..plan.clone()
    }
}

/// The largest layer count whose charge, `first` for one layer and `increment` for each after it,
/// is within the budget.
fn within(first: u64, increment: u64) -> usize {
    if first > GPU_PREVIEW_BUDGET {
        0
    } else {
        1 + ((GPU_PREVIEW_BUDGET - first) / increment) as usize
    }
}

/// Decimal megabytes to one place, thousands separated, as the design's tables give them.
fn mb(bytes: u64) -> String {
    let tenths = (bytes + 50_000) / 100_000;
    let whole = (tenths / 10).to_string();
    let mut grouped = String::new();
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{grouped}.{}", tenths % 10)
}

/// `values` with every run of one value written once, as `count × value B`.
fn runs(values: &[u64]) -> String {
    let mut parts: Vec<(usize, u64)> = Vec::new();
    for &value in values {
        match parts.last_mut() {
            Some((count, held)) if *held == value => *count += 1,
            _ => parts.push((1, value)),
        }
    }
    if parts.is_empty() {
        return "none".to_owned();
    }
    parts
        .iter()
        .map(|(count, value)| format!("{count} × {value} B"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One count's measurement of a cell.
struct Charge {
    layers: usize,
    /// `Qualifier::charged_bytes`.
    slot: u64,
    /// The chain's charge.
    chain: ChainCharge,
    /// The boundary, the output in its size bucket and its uniform.
    textures: u64,
    /// Each link's words and blocks buffers, in chain order.
    buffers: Vec<u64>,
    /// Each link's own scratch planes, in chain order.
    scratch: Vec<u64>,
    /// The slot with each link holding its own scratch planes.
    unpooled: u64,
    line: String,
}

/// The slot's charge for `layers` masked layers of `fields` at `place` over `held`, its breakdown,
/// and the layout without the pool; and the surface plan, for [`repeated`].
#[allow(clippy::too_many_arguments)]
fn measure(
    qualifier: &Qualifier,
    registry: &ModuleRegistry,
    fields: Fields,
    place: Place,
    request: GpuPlanRequest,
    held: &GpuBoundary,
    label: &str,
    layers: usize,
) -> (Charge, GpuPlan) {
    let plan = match gpu_plan(registry, &stack(fields, layers), request).expect("it compiles") {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("{label}, {layers} layers: {reason}"),
    };
    let surface = surface_plan_over(&plan, held.clone(), place.origin, None, place.region)
        .expect("a runnable plan");
    let spatial: Vec<&luxforge_gpu::GpuSpatial> = surface
        .steps
        .iter()
        .filter_map(|step| match step {
            GpuStep::Spatial(spatial) => Some(&**spatial),
            _ => None,
        })
        .collect();
    assert_eq!(spatial.len(), layers, "{label}: a link a layer");
    assert!(
        !surface
            .steps
            .iter()
            .any(|step| matches!(step, GpuStep::Geometry(_))),
        "{label}: no tail"
    );
    let origin = (
        surface.texels.origin[0].max(0.0) as u32,
        surface.texels.origin[1].max(0.0) as u32,
    );
    assert_eq!(origin, place.origin, "{label}: the boundary's stage origin");
    let (size, format) = (place.size, held.format());
    let slot = qualifier.charged_bytes(&surface).expect("a charge");
    // The slot's figure as `slot_charge` composes it: the chain's charge, the boundary, the output
    // in its size bucket and its uniform, and every link's words and blocks buffers.
    let chain = chain_charge(&surface.steps, size, origin, format);
    let output = place
        .region
        .map_or(size, |region| (region.width, region.height));
    let textures = texture_charge(
        size,
        format,
        output,
        None,
        place.region.is_some(),
        DEVICE_TEXTURE_LIMIT,
    );
    let buffers = qualifier.buffer_bytes(&surface).expect("the buffers");
    assert_eq!(
        slot,
        chain.total() + textures + buffers.iter().sum::<u64>(),
        "{label}, {layers} layers: the slot is its chain, textures and buffers"
    );
    // Each link's own scratch: the core's planes less the ones its applies read, which are the
    // surface step's planes.
    let scratch: Vec<u64> = plan
        .spatial
        .iter()
        .map(|spatial| own_scratch(spatial, origin, size))
        .collect();
    for (core, surface) in plan.spatial.iter().zip(&spatial) {
        assert_eq!(
            core.plane_bytes(origin, size),
            surface.plane_bytes(origin, size),
            "{label}: the surface holds the core's planes"
        );
    }
    let unpooled = slot - chain.pool + scratch.iter().sum::<u64>();
    // Links of one shape: the pool is one link's scratch, so the layout before it charged the
    // slot each later link's scratch again.
    assert_eq!(
        unpooled,
        slot + scratch[1..].iter().sum::<u64>(),
        "{label}, {layers} layers: the pool is one link's scratch"
    );
    if layers >= 2 {
        assert!(slot < unpooled, "{label}, {layers} layers: the pool saves");
    }
    let boundary = u64::from(size.0) * u64::from(size.1) * format.texel_bytes() as u64;
    let line = format!(
        "{label}, {layers} layers: slot {slot} B = chain {} B (intermediates {}; kept per link, \
         planes and parameter slices, {}; pool {} B) + boundary {boundary} B + output in its size \
         bucket and uniform {} B + buffers per link {}; own scratch per link {}; each link holding \
         its own scratch {unpooled} B",
        chain.total(),
        runs(&chain.intermediates),
        runs(&chain.kept),
        chain.pool,
        textures - boundary,
        runs(&buffers),
        runs(&scratch),
    );
    (
        Charge {
            layers,
            slot,
            chain,
            textures,
            buffers,
            scratch,
            unpooled,
            line,
        },
        surface,
    )
}

/// The slot's charge (`Qualifier::charged_bytes`, which the live slot's `gpu_preview_in_use_bytes`
/// equals) for 1, 2, 4, 8 and 16 masked Presence layers, each through a radial of its own: layers
/// of Texture and Clarity, and of all three fields; at a 24 MP photograph's full-screen Fit stage,
/// a 2292 × 1528 boundary against a 6000 × 4000 stage, and over the 100% window of 3778 × 2578 of
/// that exact stage, centred on the region the desktop's view asks for at 100% in the largest
/// window, far from the stage's edges, and drawing the window itself; on a JPEG's half-float
/// boundary and a RAW's `f32` one on the linear path, whose intermediates are `rgba32float`. For
/// every cell: the charge, the increment of each layer after the first, the chain's breakdown
/// (`chain_charge`), how many layers the 2 GiB budget holds — from the first charge and the
/// increment, checked by charging that count and the next directly — and the charge with each link
/// holding its own scratch planes, the layout before the pool, with today's one-channel band.
///
/// It asserts only what must hold: each charge is the first plus the increment for every layer
/// after it; the pool charges less than the links' own scratch from two layers; and every figure
/// is its chain's charge, the boundary, the output in its size bucket, its uniform and the
/// buffers, as `slot_charge` composes it. The figures themselves are the measurement's.
///
/// ```sh
/// cargo test --release -p luxforge-app --bin luxforge gpu_shared_scratch_measured -- \
///     --ignored --nocapture
/// ```
#[test]
#[ignore = "the scratch pool's memory measurement (docs/specs/performance.md, The scratch \
            pool's memory): run it in release with --ignored --nocapture and record its table"]
fn gpu_shared_scratch_measured() {
    let test = "gpu_shared_scratch_measured";
    let Some(qualifier) = headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let photograph = stage(PHOTOGRAPH.0, PHOTOGRAPH.1);
    // The region the desktop's view asks for at 100% in the largest window, scrolled to the
    // photograph's centre, and the window centred on it.
    let view = largest_view(PHOTOGRAPH, 100.0).expect("a visible region");
    let origin = (
        view.x0 - (WINDOW.0 - view.width) / 2,
        view.y0 - (WINDOW.1 - view.height) / 2,
    );
    assert!(
        origin.0 > 0
            && origin.1 > 0
            && origin.0 + WINDOW.0 < PHOTOGRAPH.0
            && origin.1 + WINDOW.1 < PHOTOGRAPH.1,
        "the window lies away from the stage's edges"
    );
    let window = Region {
        x0: origin.0,
        y0: origin.1,
        width: WINDOW.0,
        height: WINDOW.1,
    };
    let places = [
        Place {
            name: "Fit",
            size: (2292, 1528),
            origin: (0, 0),
            region: None,
            request: GpuPlanRequest::fit(0, stage(2292, 1528), photograph),
        },
        Place {
            name: "100%",
            size: WINDOW,
            origin,
            region: Some(window),
            request: GpuPlanRequest::exact(0, photograph),
        },
    ];
    eprintln!(
        "{test}: adapter {}; budget {GPU_PREVIEW_BUDGET} B; Fit: a 2292x1528 boundary at (0, 0) \
         of a 2292x1528 proxy stage of a {}x{} photograph, the whole stage drawn; 100%: a {}x{} \
         window at ({}, {}) of the {}x{} exact stage, centred on the desktop's 100% view region \
         in the largest window ({}x{} at ({}, {})), drawing the window itself as its region",
        qualifier.adapter(),
        PHOTOGRAPH.0,
        PHOTOGRAPH.1,
        WINDOW.0,
        WINDOW.1,
        origin.0,
        origin.1,
        PHOTOGRAPH.0,
        PHOTOGRAPH.1,
        view.width,
        view.height,
        view.x0,
        view.y0,
    );
    let mut charges = Vec::new();
    let mut breakdowns = Vec::new();
    let mut unpooled = Vec::new();
    let mut details = Vec::new();
    for fields in [Fields::TextureClarity, Fields::All] {
        for place in places {
            for (format, source) in [
                (BoundaryFormat::Half, "JPEG"),
                (BoundaryFormat::Float, "RAW"),
            ] {
                let request = match format {
                    BoundaryFormat::Half => place.request,
                    BoundaryFormat::Float => place.request.linear(),
                };
                let (width, height) = place.size;
                let held = GpuBoundary::new(
                    Arc::new(vec![0u8; (width * height) as usize * format.texel_bytes()]),
                    width,
                    height,
                    1,
                    format,
                )
                .expect("a boundary");
                let label = format!("{}, {}, {source}", fields.name(), place.name);
                let row = format!("| {} | {} | {source} |", fields.name(), place.name);
                let mut measured = Vec::new();
                let mut largest = None;
                for layers in COUNTS {
                    let (charge, surface) = measure(
                        &qualifier, &registry, fields, place, request, &held, &label, layers,
                    );
                    details.push(charge.line.clone());
                    measured.push(charge);
                    largest = Some(surface);
                }
                let largest = largest.expect("the largest plan");
                let first = &measured[0];
                let increment = measured[1].slot - first.slot;
                for charge in &measured {
                    assert_eq!(
                        charge.slot,
                        first.slot + (charge.layers as u64 - 1) * increment,
                        "{label}: {} layers charge the first and an increment for each after it",
                        charge.layers
                    );
                }
                // A chain longer than a recipe holds, each link a copy of one of the largest
                // plan's in turn, charges what the recipe's own plan does where both exist.
                for charge in &measured {
                    let copied = qualifier
                        .charged_bytes(&repeated(&largest, charge.layers))
                        .expect("a charge");
                    assert_eq!(
                        copied, charge.slot,
                        "{label}: {} copied links",
                        charge.layers
                    );
                }
                let fits = within(first.slot, increment);
                let direct = if (1..=DIRECT).contains(&fits) {
                    let at = |links| {
                        qualifier
                            .charged_bytes(&repeated(&largest, links))
                            .expect("a charge")
                    };
                    let (held, past) = (at(fits), at(fits + 1));
                    assert!(
                        held <= GPU_PREVIEW_BUDGET && past > GPU_PREVIEW_BUDGET,
                        "{label}: {fits} layers charge {held} B and {} charge {past} B",
                        fits + 1
                    );
                    format!("{fits} layers {held} B, {} layers {past} B", fits + 1)
                } else {
                    "not charged directly".to_owned()
                };
                let steps: Vec<String> = measured
                    .windows(2)
                    .map(|pair| {
                        let more = (pair[1].layers - pair[0].layers) as u64;
                        ((pair[1].slot - pair[0].slot) / more).to_string()
                    })
                    .collect();
                details.push(format!(
                    "{label}: each layer after the first {increment} B (1 to 2, 2 to 4, 4 to 8, 8 \
                     to 16: {} B a layer); {fits} layers within {GPU_PREVIEW_BUDGET} B, charged \
                     directly: {direct}",
                    steps.join(", ")
                ));
                let cells: Vec<String> = measured.iter().map(|charge| mb(charge.slot)).collect();
                charges.push(format!(
                    "{row} {} | {} | {fits} |",
                    cells.join(" | "),
                    mb(increment)
                ));
                // The slot's parts, and those of one layer after the first, from the two-layer
                // chain.
                let two = &measured[1];
                let boundary = u64::from(width) * u64::from(height) * format.texel_bytes() as u64;
                breakdowns.push(format!(
                    "{row} {} | {} | {} | {} | {} | {} | {} | {} |",
                    mb(boundary),
                    mb(two.textures - boundary),
                    mb(two.chain.intermediates[0]),
                    mb(two.chain.kept[1]),
                    mb(two.chain.pool),
                    mb(two.scratch[1]),
                    two.buffers[1],
                    mb(two.chain.intermediates[0] + two.chain.kept[1] + two.buffers[1]),
                ));
                let unpooled_increment = measured[1].unpooled - first.unpooled;
                let cells: Vec<String> =
                    measured.iter().map(|charge| mb(charge.unpooled)).collect();
                let sixteen = measured.last().expect("16 layers");
                unpooled.push(format!(
                    "{row} {} | {} | {} | {} |",
                    cells.join(" | "),
                    mb(unpooled_increment),
                    within(first.unpooled, unpooled_increment),
                    mb(sixteen.unpooled - sixteen.slot),
                ));
            }
        }
    }
    // The region the view asks for inside the window, drawn instead of the window: only the
    // output's size bucket changes, the same in every 100% cell.
    let view_textures = |format| {
        texture_charge(
            WINDOW,
            format,
            (view.width, view.height),
            None,
            true,
            DEVICE_TEXTURE_LIMIT,
        )
    };
    let window_textures =
        |format| texture_charge(WINDOW, format, WINDOW, None, true, DEVICE_TEXTURE_LIMIT);
    let square = |format| texture_charge(WINDOW, format, WINDOW, None, false, DEVICE_TEXTURE_LIMIT);
    let header = "| Layers | Stage | Boundary |";
    eprintln!("\n{test}: the slot's charge, MB (10^6 B), and the layers within the 2 GiB budget\n");
    eprintln!(
        "{header} 1 layer | 2 | 4 | 8 | 16 | Each layer after the first | Layers within 2 GiB |"
    );
    eprintln!("| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for row in &charges {
        eprintln!("{row}");
    }
    eprintln!(
        "\n{test}: the breakdown, MB: the slot's boundary, its output in its size bucket with its \
         uniform, and of each layer after the first its intermediate, kept planes with their \
         parameter slices and buffers; the pool once; and the scratch planes a link held as its \
         own before the pool\n"
    );
    eprintln!(
        "{header} Boundary texture | Output and uniform | Intermediate | Kept planes and \
         parameters | Pool | A link's own scratch | Words and blocks buffers, B | Each layer after \
         the first |"
    );
    eprintln!("| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for row in &breakdowns {
        eprintln!("{row}");
    }
    eprintln!(
        "\n{test}: each link holding its own scratch planes, as before the pool, with today's \
         one-channel band, MB\n"
    );
    eprintln!(
        "{header} 1 layer | 2 | 4 | 8 | 16 | Each layer after the first | Layers within 2 GiB | \
         The pool saves at 16 |"
    );
    eprintln!("| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for row in &unpooled {
        eprintln!("{row}");
    }
    eprintln!("\n{test}: every charge, in bytes\n");
    for line in &details {
        eprintln!("{test}: {line}");
    }
    for (format, source) in [
        (BoundaryFormat::Half, "JPEG"),
        (BoundaryFormat::Float, "RAW"),
    ] {
        eprintln!(
            "{test}: 100%, {source}: drawing the view's {}x{} region inside the window instead of \
             the window charges {} B less in every cell; the window's output in the photograph's \
             square size bucket, as when the design measured it, would charge {} B more",
            view.width,
            view.height,
            window_textures(format) - view_textures(format),
            square(format) - window_textures(format),
        );
    }
}
