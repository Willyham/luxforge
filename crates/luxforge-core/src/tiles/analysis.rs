//! Bounded nearest-point grids over output geometry, served through the tile provider.
use super::{Answered, MaskInputMode, ReadStage, TileReads};
use crate::{
    AssetId, Cancel, Error, Evaluation, PreviewSource, ProxyIdentity, Recipe, RenderContext,
    RendererRecord,
    auto_tone::{ALGORITHM, AnalysisSample, MAX_SIDE},
};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

const RETAINED_BYTES: usize = 32 * 1024 * 1024;

/// An immutable grid and the context that accounts the analysis work performed on it.
pub struct AnalysisRead {
    pub sample: Arc<AnalysisSample>,
    pub answered: Answered,
    pub context: RenderContext,
    pub cancel: Cancel,
}

#[derive(Clone, Debug, PartialEq)]
struct Key {
    asset: AssetId,
    source: ProxyIdentity,
    content: [u8; 32],
    renderer: RendererRecord,
}

#[derive(Default)]
struct Held {
    generation: u64,
    asset: Option<AssetId>,
    selected: Option<Option<AssetId>>,
    samples: Vec<(Key, Arc<AnalysisSample>, Answered)>,
}

/// One shared, bounded cache; no frame or source allocations are retained here.
#[derive(Default)]
pub(crate) struct SampleCache(Mutex<Held>);

impl SampleCache {
    /// Changing the active photo (including leaving Develop) also prevents an older worker from
    /// putting its sample back after the clear.
    pub(crate) fn select(&self, asset: Option<&AssetId>) {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        held.selected = Some(asset.cloned());
        if held.asset.as_ref() != asset {
            held.asset = asset.cloned();
            held.samples.clear();
            held.generation = held.generation.wrapping_add(1);
        }
    }

    fn find(&self, key: &Key) -> (u64, Option<(Arc<AnalysisSample>, Answered)>) {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(selected) = &held.selected {
            if selected.as_ref() != Some(&key.asset) {
                return (held.generation, None);
            }
        } else if held.asset.as_ref() != Some(&key.asset) {
            held.asset = Some(key.asset.clone());
            held.samples.clear();
            held.generation = held.generation.wrapping_add(1);
        }
        (
            held.generation,
            held.samples
                .iter()
                .find(|(k, ..)| k == key)
                .map(|(_, sample, answered)| (Arc::clone(sample), answered.clone())),
        )
    }

    fn keep(&self, generation: u64, key: Key, sample: &Arc<AnalysisSample>, answered: &Answered) {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if held.generation != generation
            || held.asset.as_ref() != Some(&key.asset)
            || sample.bytes() > RETAINED_BYTES
        {
            return;
        }
        held.samples.retain(|(k, ..)| k != &key);
        while held
            .samples
            .iter()
            .map(|(_, sample, _)| sample.bytes())
            .sum::<usize>()
            + sample.bytes()
            > RETAINED_BYTES
        {
            held.samples.remove(0);
        }
        held.samples
            .push((key, Arc::clone(sample), answered.clone()));
    }

    fn make_room(&self, generation: u64, key: &Key, bytes: usize) {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if held.generation != generation || held.asset.as_ref() != Some(&key.asset) {
            return;
        }
        while !held.samples.is_empty()
            && held
                .samples
                .iter()
                .map(|(_, sample, _)| sample.bytes())
                .sum::<usize>()
                + bytes
                > RETAINED_BYTES
        {
            held.samples.remove(0);
        }
    }
}

/// Hash effect content, not transient layer identities allocated by an uncommitted composite.
/// Masks and artifacts are still identified by their recipe data; their contents are immutable.
pub(crate) fn content_hash(recipe: &Recipe) -> Result<[u8; 32], Error> {
    let mut value = serde_json::to_value(recipe).map_err(|e| Error::internal(e.to_string()))?;
    if let Some(layers) = value
        .get_mut("layers")
        .and_then(serde_json::Value::as_array_mut)
    {
        for layer in layers {
            if let Some(fields) = layer.as_object_mut() {
                fields.remove("id");
            }
        }
    }
    Ok(
        Sha256::digest(serde_json::to_vec(&value).map_err(|e| Error::internal(e.to_string()))?)
            .into(),
    )
}

pub fn read(
    evaluation: &Evaluation,
    index: usize,
    reads: &dyn TileReads,
    cancel: &Cancel,
) -> Result<AnalysisRead, Error> {
    cancel.check()?;
    let recipe = evaluation.recipe();
    let layers = recipe
        .layers
        .get(..index)
        .ok_or_else(|| Error::validation("analysis prefix exceeds stack"))?;
    let prefix = Recipe {
        layers: layers.to_vec(),
        masks: recipe
            .masks
            .iter()
            .filter(|mask| {
                layers
                    .iter()
                    .any(|layer| layer.mask.as_ref() == Some(&mask.id))
            })
            .cloned()
            .collect(),
        ..recipe.clone()
    };
    let (width, height) = evaluation.source().dimensions();
    let output = crate::stage_transform(evaluation.registry(), width, height, recipe)?;
    let before = crate::stage_transform(evaluation.registry(), width, height, &prefix)?;
    let longest = output.output.width.max(output.output.height);
    let grid = [output.output.width, output.output.height].map(|n| {
        if longest <= MAX_SIDE {
            n
        } else {
            ((u64::from(n) * u64::from(MAX_SIDE)) / u64::from(longest)).max(1) as u32
        }
    });
    let mode = crate::render::MaskInputMode::for_layer(evaluation.registry(), recipe, index);
    let mut digest = Sha256::new();
    digest.update(content_hash(&prefix)?);
    digest.update(output.sha256().as_bytes());
    digest.update(ALGORITHM.as_bytes());
    for value in grid {
        digest.update(value.to_le_bytes());
    }
    digest.update([u8::from(matches!(
        mode,
        crate::render::MaskInputMode::ColourRun
    ))]);
    let key = reads.analysis_renderer().map(|renderer| Key {
        asset: evaluation.entry().asset_id.clone(),
        source: evaluation.source().identity(),
        content: digest.finalize().into(),
        renderer,
    });
    let cache = evaluation.context().analysis_samples();
    let (generation, kept) = key.as_ref().map_or((0, None), |key| cache.find(key));
    if let Some((sample, answered)) = kept {
        return Ok(AnalysisRead {
            sample,
            answered,
            context: evaluation.context().clone(),
            cancel: cancel.clone(),
        });
    }
    let count = grid[0] as usize * grid[1] as usize;
    if let Some(key) = &key {
        cache.make_room(generation, key, count * 13);
    }
    // Before allocation: positions, source flags, result RGB, gather sort order and one tile.
    // These are sample-sized; the provider's existing stage/tile storage has its own accounting.
    let _scratch = evaluation
        .context()
        .scratch()
        .reserve(count * (8 + 1 + 12 + size_of::<usize>()) + 256 * 256 * 12);
    let mut points = Vec::with_capacity(count);
    let mut source_white = Vec::with_capacity(count);
    for row in 0..grid[1] {
        cancel.check()?;
        for column in 0..grid[0] {
            let x = (f64::from(column) + 0.5) * f64::from(output.output.width) / f64::from(grid[0]);
            let y = (f64::from(row) + 0.5) * f64::from(output.output.height) / f64::from(grid[1]);
            let Ok((sx, sy)) = output.to_content(x, y) else {
                continue;
            };
            let Ok((px, py)) = before.to_output(sx, sy) else {
                continue;
            };
            if !px.is_finite()
                || !py.is_finite()
                || px < 0.
                || py < 0.
                || px >= f64::from(before.output.width)
                || py >= f64::from(before.output.height)
            {
                continue;
            }
            points.push([px.floor() as u32, py.floor() as u32]);
            let white = match evaluation.source() {
                PreviewSource::Jpeg(image)
                    if sx >= 0. && sy >= 0. && sx < f64::from(width) && sy < f64::from(height) =>
                {
                    let offset = (sy.floor() as usize * width as usize + sx.floor() as usize) * 4;
                    image.rgba[offset..offset + 3].contains(&255)
                }
                // The prepared RAW planes currently expose no per-pixel sensor saturation mask.
                // Scene-linear values >= 1 are not evidence of sensor saturation.
                _ => false,
            };
            source_white.push(white);
        }
    }
    let answer = reads.session(evaluation, cancel).gather(
        ReadStage::Before {
            layer: index,
            mode: MaskInputMode::from(mode),
        },
        &points,
        cancel,
    )?;
    let answered = answer.answered.unwrap_or_else(|| Answered::reference(None));
    let sample = Arc::new(AnalysisSample {
        grid,
        rgb: answer.linear,
        source_white,
    });
    cancel.check()?;
    if let Some(key) = key
        && key.renderer == answered.record
    {
        cache.keep(generation, key, &sample, &answered);
    }
    Ok(AnalysisRead {
        sample,
        answered,
        context: evaluation.context().clone(),
        cancel: cancel.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_tone_cache_is_bounded_and_late_reads_cannot_repopulate_after_leaving_develop() {
        let cache = SampleCache::default();
        let asset = AssetId::new();
        cache.select(Some(&asset));
        let source = crate::PreviewSource::Jpeg(crate::SourceImage {
            width: 1,
            height: 1,
            rgba: Arc::new(vec![0, 0, 0, 255]),
            fingerprint: "cache-test".into(),
            orientation: 1,
            capture: Arc::default(),
        });
        let key = |n| Key {
            asset: asset.clone(),
            source: source.identity(),
            content: [n; 32],
            renderer: RendererRecord::Reference,
        };
        let sample = Arc::new(AnalysisSample {
            grid: [1024, 1024],
            rgb: vec![[0.; 3]; 1024 * 1024],
            source_white: vec![false; 1024 * 1024],
        });
        let generation = cache.find(&key(0)).0;
        for n in 0..4 {
            cache.keep(generation, key(n), &sample, &Answered::reference(None));
        }
        assert!(
            cache
                .0
                .lock()
                .unwrap()
                .samples
                .iter()
                .map(|(_, s, _)| s.bytes())
                .sum::<usize>()
                <= RETAINED_BYTES
        );
        assert!(cache.find(&key(0)).1.is_none());
        assert!(cache.find(&key(3)).1.is_some());
        cache.select(None);
        cache.keep(generation, key(0), &sample, &Answered::reference(None));
        let newer = cache.find(&key(0)).0;
        cache.keep(newer, key(0), &sample, &Answered::reference(None));
        assert!(cache.0.lock().unwrap().samples.is_empty());
        cache.select(Some(&asset));
        let fresh = cache.find(&key(0)).0;
        cache.keep(fresh, key(0), &sample, &Answered::reference(None));
        assert!(cache.find(&key(0)).1.is_some());
    }
}
