//! `task.analyse-scene` and `task.analyse-depth`: the scene models (semantic classes, the salient
//! subject and people) and the depth model over the analysis input, and the result each lists.
//!
//! The tensors are the contract the fake runtime's scene and depth models answer
//! ([`crate::inference::fake_analysis`]): `pixel_values` `[1, 3, h, w]`, the analysis input reduced
//! to at most [`SCENE_SIDE`] on its long side as sRGB values in `[0, 1]`, answered by
//! `class_logits` `[1, K, gh, gw]` over the model's declared classes, `saliency` `[1, 1, gh, gw]` in
//! `[0, 1]`, `boxes` `[1, P, 4]` (normalized corners) with `scores` `[1, P]`, and `depth`
//! `[1, 1, gh, gw]` (relative, nearer is larger). The qualified models (TASK-004) each bring their
//! own pre- and post-processing behind the same results.
use super::{Raster, key, models::SessionModel, models_value, prepared};
use crate::{
    Error,
    capabilities::context::ModuleContext,
    inference::{Tensor, TensorData},
    modules::ai::segment::resample,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// The scene input's long side at most.
pub(crate) const SCENE_SIDE: u32 = 512;
/// The scene's cache kind.
const SCENE_KIND: &str = "scene";
/// A class covering less of the frame than this is not listed.
const MIN_CLASS_AREA: f64 = 0.005;

/// What the scene models found, as the cache holds it.
#[derive(Debug)]
pub(crate) struct SceneAnalysis {
    /// Each class's probability per cell of `class_grid`, in the model's order.
    pub(crate) classes: Vec<(String, Vec<f32>)>,
    pub(crate) class_grid: (u32, u32),
    /// The subject's probability per cell of `subject_grid`.
    pub(crate) subject: Vec<f32>,
    pub(crate) subject_grid: (u32, u32),
    /// Each person's normalized bounds `[x0, y0, x1, y1]` and confidence.
    pub(crate) people: Vec<([f32; 4], f32)>,
}

impl SceneAnalysis {
    fn bytes(&self) -> u64 {
        let planes: usize = self.classes.iter().map(|(_, plane)| plane.len()).sum();
        ((planes + self.subject.len()) * 4 + self.people.len() * 20) as u64
    }
}

/// What the depth model found, as the cache holds it.
#[derive(Debug)]
pub(crate) struct DepthAnalysis {
    pub(crate) grid: (u32, u32),
    pub(crate) values: Vec<f32>,
}

/// The scene input of an analysis input.
fn scene_input(raster: &Raster) -> Result<Tensor, Error> {
    let size = super::reduced_size((raster.width, raster.height), SCENE_SIDE);
    let codes = raster.codes();
    let resampled = if size == (raster.width, raster.height) {
        codes
    } else {
        resample(&codes, (raster.width, raster.height), size)
    };
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut planes = vec![0.0_f32; 3 * w * h];
    for (i, pixel) in resampled.chunks_exact(3).enumerate() {
        for (c, value) in pixel.iter().enumerate() {
            planes[c * w * h + i] = value / 255.0;
        }
    }
    Tensor::new(
        "pixel_values",
        vec![1, 3, h as u64, w as u64],
        TensorData::F32(planes),
    )
}

/// The named output, `f32`, finite and of rank `rank`, or `incompatible` naming it.
fn output<'a>(outputs: &'a [Tensor], name: &str, rank: usize) -> Result<&'a Tensor, Error> {
    let tensor = outputs
        .iter()
        .find(|tensor| tensor.name() == name)
        .ok_or_else(|| Error::incompatible(format!("the model answered no {name}")))?;
    let finite = tensor
        .f32()
        .is_some_and(|values| values.iter().all(|value| value.is_finite()));
    if tensor.shape().len() != rank || !finite {
        return Err(Error::incompatible(format!(
            "the model answered {name} of shape {:?}, not finite f32 of rank {rank}",
            tensor.shape()
        )));
    }
    Ok(tensor)
}

/// A `[1, planes, gh, gw]` output's grid, checked to hold `planes`.
fn grid(tensor: &Tensor, planes: u64) -> Result<(u32, u32), Error> {
    match tensor.shape() {
        [1, p, h, w] if *p == planes && *h > 0 && *w > 0 => Ok((*w as u32, *h as u32)),
        shape => Err(Error::incompatible(format!(
            "the model answered {} of shape {shape:?}, not [1, {planes}, h, w]",
            tensor.name()
        ))),
    }
}

/// The cache version of a set of models: each one's identity and version.
fn version(models: &[&SessionModel]) -> String {
    models
        .iter()
        .map(|model| format!("{}@{}", model.file.resource, model.file.version))
        .collect::<Vec<_>>()
        .join("+")
}

/// `task.analyse-scene`: the semantic classes, the subject and people of the context's stage.
pub(crate) fn analyse_scene(
    context: &ModuleContext,
    models: &[&'static SessionModel],
) -> Result<Value, Error> {
    let [semantic, subject, people] = models else {
        return Err(Error::internal("the scene is three models"));
    };
    let identity = context.identity()?;
    let cache = context.analysis()?;
    let scene_key = key(identity, SCENE_KIND, &version(models));
    let (found, cached) = match cache.get::<SceneAnalysis>(&scene_key) {
        Some(found) => (found, true),
        None => {
            context.progress(Some(0.0), "reading the photograph");
            let (raster, _, _) = prepared(context)?;
            let input = scene_input(&raster)?;
            context.progress(Some(0.4), "finding what is in the photograph");
            let run = |model: &SessionModel| -> Result<Vec<Tensor>, Error> {
                context
                    .session(model.runtime)?
                    .run(std::slice::from_ref(&input), context.cancel())
            };
            let outputs = run(semantic)?;
            let logits = output(&outputs, "class_logits", 4)?;
            let class_grid = grid(logits, semantic.classes.len() as u64)?;
            let classes = softmax(logits.f32().unwrap_or_default(), semantic.classes);
            let outputs = run(subject)?;
            let saliency = output(&outputs, "saliency", 4)?;
            let subject_grid = grid(saliency, 1)?;
            let subject_values: Vec<f32> = saliency
                .f32()
                .unwrap_or_default()
                .iter()
                .map(|value| value.clamp(0.0, 1.0))
                .collect();
            let outputs = run(people)?;
            let found = Arc::new(SceneAnalysis {
                classes,
                class_grid,
                subject: subject_values,
                subject_grid,
                people: detections(&outputs)?,
            });
            cache.insert(scene_key, found.clone(), found.bytes())?;
            (found, false)
        }
    };
    Ok(scene_value(&found, cached, models))
}

/// Each class's probabilities, a softmax over the class planes of each cell.
fn softmax(logits: &[f32], classes: &[&str]) -> Vec<(String, Vec<f32>)> {
    let k = classes.len();
    let cells = logits.len() / k.max(1);
    let mut planes = vec![vec![0.0_f32; cells]; k];
    for cell in 0..cells {
        let max = (0..k)
            .map(|c| logits[c * cells + cell])
            .fold(f32::NEG_INFINITY, f32::max);
        let total: f32 = (0..k).map(|c| (logits[c * cells + cell] - max).exp()).sum();
        for (c, plane) in planes.iter_mut().enumerate() {
            plane[cell] = (logits[c * cells + cell] - max).exp() / total;
        }
    }
    classes
        .iter()
        .map(|class| (*class).to_owned())
        .zip(planes)
        .collect()
}

/// The people the detector found: each box clamped to the frame, kept at a score of at least 0.5.
fn detections(outputs: &[Tensor]) -> Result<Vec<([f32; 4], f32)>, Error> {
    let boxes = output(outputs, "boxes", 3)?;
    let scores = output(outputs, "scores", 2)?;
    let people = match (boxes.shape(), scores.shape()) {
        ([1, p, 4], [1, q]) if p == q => *p as usize,
        (b, s) => {
            return Err(Error::incompatible(format!(
                "the people detector answered boxes {b:?} and scores {s:?}"
            )));
        }
    };
    let (boxes, scores) = (
        boxes.f32().unwrap_or_default(),
        scores.f32().unwrap_or_default(),
    );
    Ok((0..people)
        .filter(|&i| scores[i] >= 0.5)
        .map(|i| {
            let b: [f32; 4] = std::array::from_fn(|k| boxes[i * 4 + k].clamp(0.0, 1.0));
            (
                [
                    b[0].min(b[2]),
                    b[1].min(b[3]),
                    b[0].max(b[2]),
                    b[1].max(b[3]),
                ],
                scores[i],
            )
        })
        .collect())
}

/// Normalized bounds of the cells of a `grid` where `inside` holds.
fn cell_bounds(grid: (u32, u32), inside: impl Fn(usize) -> bool) -> Option<[f64; 4]> {
    let (w, h) = (grid.0 as usize, grid.1 as usize);
    let mut bounds: Option<[usize; 4]> = None;
    for i in (0..w * h).filter(|&i| inside(i)) {
        let (u, v) = (i % w, i / w);
        bounds = Some(match bounds {
            None => [u, v, u + 1, v + 1],
            Some([x0, y0, x1, y1]) => [x0.min(u), y0.min(v), x1.max(u + 1), y1.max(v + 1)],
        });
    }
    bounds.map(|[x0, y0, x1, y1]| {
        [
            x0 as f64 / w as f64,
            y0 as f64 / h as f64,
            x1 as f64 / w as f64,
            y1 as f64 / h as f64,
        ]
    })
}

/// Three decimals: what a result reports of a share or a confidence.
fn rounded(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// The scene result: each class covering at least half a percent of the frame with its area share
/// and its mean confidence where it wins, largest first; the subject's area, bounds and confidence
/// (`null` when nothing stands out); each person's bounds and confidence; the models.
fn scene_value(found: &SceneAnalysis, cached: bool, models: &[&SessionModel]) -> Value {
    let cells = found.classes.first().map_or(0, |(_, plane)| plane.len());
    let winner = |cell: usize| {
        (0..found.classes.len())
            .max_by(|&a, &b| {
                found.classes[a].1[cell]
                    .total_cmp(&found.classes[b].1[cell])
                    .then(b.cmp(&a))
            })
            .unwrap_or(0)
    };
    let mut areas = vec![(0_usize, 0.0_f64); found.classes.len()];
    for cell in 0..cells {
        let class = winner(cell);
        areas[class].0 += 1;
        areas[class].1 += f64::from(found.classes[class].1[cell]);
    }
    let mut classes: Vec<(usize, f64, f64)> = areas
        .iter()
        .enumerate()
        .filter(|(_, (count, _))| *count > 0)
        .map(|(class, (count, sum))| {
            (
                class,
                *count as f64 / cells.max(1) as f64,
                sum / *count as f64,
            )
        })
        .filter(|(_, area, _)| *area >= MIN_CLASS_AREA)
        .collect();
    classes.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let subject_cells: Vec<usize> = (0..found.subject.len())
        .filter(|&i| found.subject[i] >= 0.5)
        .collect();
    let subject = (!subject_cells.is_empty()).then(|| {
        let confidence = subject_cells
            .iter()
            .map(|&i| f64::from(found.subject[i]))
            .sum::<f64>()
            / subject_cells.len() as f64;
        json!({
            "area": rounded(subject_cells.len() as f64 / found.subject.len() as f64),
            "bounds": cell_bounds(found.subject_grid, |i| found.subject[i] >= 0.5)
                .map(|b| b.map(rounded)),
            "confidence": rounded(confidence),
        })
    });
    json!({
        "cached": cached,
        "classes": classes
            .iter()
            .map(|(class, area, confidence)| json!({
                "class": found.classes[*class].0,
                "area": rounded(*area),
                "bounds": cell_bounds(found.class_grid, |i| winner(i) == *class)
                    .map(|b| b.map(rounded)),
                "confidence": rounded(*confidence),
            }))
            .collect::<Vec<_>>(),
        "subject": subject,
        "people": found
            .people
            .iter()
            .enumerate()
            .map(|(person, (bounds, confidence))| json!({
                "person": person,
                "bounds": bounds.map(|b| rounded(f64::from(b))),
                "confidence": rounded(f64::from(*confidence)),
            }))
            .collect::<Vec<_>>(),
        "models": models_value(models),
    })
}

/// `task.analyse-depth`: the depth model over the context's stage.
pub(crate) fn analyse_depth(
    context: &ModuleContext,
    models: &[&'static SessionModel],
) -> Result<Value, Error> {
    let [depth] = models else {
        return Err(Error::internal("depth is one model"));
    };
    let identity = context.identity()?;
    let cache = context.analysis()?;
    let depth_key = key(identity, depth.file.resource, depth.file.version);
    let (found, cached) = match cache.get::<DepthAnalysis>(&depth_key) {
        Some(found) => (found, true),
        None => {
            context.progress(Some(0.0), "reading the photograph");
            let (raster, _, _) = prepared(context)?;
            let input = scene_input(&raster)?;
            context.progress(Some(0.4), "estimating depth");
            let outputs = context
                .session(depth.runtime)?
                .run(std::slice::from_ref(&input), context.cancel())?;
            let tensor = output(&outputs, "depth", 4)?;
            let found = Arc::new(DepthAnalysis {
                grid: grid(tensor, 1)?,
                values: tensor.f32().unwrap_or_default().to_vec(),
            });
            cache.insert(depth_key, found.clone(), found.values.len() as u64 * 4)?;
            (found, false)
        }
    };
    let (near, far) = found
        .values
        .iter()
        .fold((f32::NEG_INFINITY, f32::INFINITY), |(near, far), &v| {
            (near.max(v), far.min(v))
        });
    Ok(json!({
        "cached": cached,
        "grid": [found.grid.0, found.grid.1],
        "range": {"near": rounded(f64::from(near)), "far": rounded(f64::from(far))},
        "models": models_value(models),
    }))
}
