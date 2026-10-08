//! Bounded nearest-point sample grids over output geometry, served through the tile provider.
//!
//! A grid is the linear values of the stage one layer receives, at uniform points over the output
//! frame, with a per-point flag for points whose source was already clipped. It knows nothing of
//! the feature that reads it: a module's analysis asks for one through
//! [`crate::StageQuestions::grid_before`] and keeps its own algorithm out of the grid's identity.
use super::{Answered, MaskInputMode, ReadStage, TileReads};
use crate::{
    AssetId, Cancel, Error, Evaluation, PreviewSource, ProxyIdentity, Recipe, RenderContext,
    RendererRecord,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// The most points a grid has on its long side.
pub const MAX_SIDE: u32 = 1024;
/// The most points one grid holds.
pub const MAX_POINTS: usize = MAX_SIDE as usize * MAX_SIDE as usize;
/// Retained grids, across every photo, in one render context.
pub(crate) const RETAINED_BYTES: usize = 32 * 1024 * 1024;
/// One point's retained bytes: linear RGB and its source-clipped flag.
const POINT_BYTES: usize = size_of::<[f32; 3]>() + size_of::<bool>();

/// A bounded uniform grid of linear input values and, independently, whether each point's source
/// was clipped: a JPEG code 255 or a RAW sensor site at its white level in any channel. Points
/// outside the input stage are absent. Non-finite values are kept as read.
#[derive(Clone, Debug)]
pub struct SampleGrid {
    pub grid: [u32; 2],
    pub rgb: Vec<[f32; 3]>,
    pub source_clipped: Vec<bool>,
}

impl SampleGrid {
    pub fn validate(&self) -> Result<(), Error> {
        let count = self.rgb.len();
        if self.grid.contains(&0)
            || self.grid.iter().any(|&n| n > MAX_SIDE)
            || count > (self.grid[0] as usize * self.grid[1] as usize)
            || count > MAX_POINTS
            || count != self.source_clipped.len()
        {
            return Err(Error::validation("invalid sample grid"));
        }
        Ok(())
    }

    pub fn bytes(&self) -> usize {
        self.rgb.capacity() * size_of::<[f32; 3]>() + self.source_clipped.capacity()
    }
}

/// Whether the sensor was saturated under an upright content pixel of a RAW development, which a
/// RAW grid's clipped flags read: the retained sensor
/// ([`luxforge_raw::RawSource::sensor_clipped_at`]), or a test's stand-in.
pub trait SensorClip: Send + Sync {
    fn clipped_at(&self, x: u32, y: u32) -> bool;
}

impl SensorClip for luxforge_raw::RawSource {
    /// A point the sensor cannot answer for is not flagged.
    fn clipped_at(&self, x: u32, y: u32) -> bool {
        self.sensor_clipped_at(x, y).unwrap_or(false)
    }
}

/// How a grid's source-clipped flags were found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipDetection {
    /// A JPEG pixel with any channel at code 255.
    JpegCode255,
    /// A RAW pixel developed from a sensor site at its white level
    /// ([`SensorClip`]).
    RawSensorWhite,
    /// A RAW read without its sensor, which flags nothing.
    Unavailable,
}

impl ClipDetection {
    fn of(evaluation: &Evaluation) -> Self {
        match (evaluation.source(), evaluation.sensor()) {
            (PreviewSource::Jpeg(_), _) => Self::JpegCode255,
            (PreviewSource::Raw { .. }, Some(_)) => Self::RawSensorWhite,
            (PreviewSource::Raw { .. }, None) => Self::Unavailable,
        }
    }
}

/// Whether the source was already clipped under content point `(x, y)`.
fn clipped_at(evaluation: &Evaluation, x: f64, y: f64) -> bool {
    let (width, height) = evaluation.source().dimensions();
    if !(x >= 0. && y >= 0. && x < f64::from(width) && y < f64::from(height)) {
        return false;
    }
    let (x, y) = (x.floor() as u32, y.floor() as u32);
    match evaluation.source() {
        PreviewSource::Jpeg(image) => {
            let offset = (y as usize * width as usize + x as usize) * 4;
            image.rgba[offset..offset + 3].contains(&255)
        }
        PreviewSource::Raw { .. } => evaluation
            .sensor()
            .is_some_and(|sensor| sensor.clipped_at(x, y)),
    }
}

/// An immutable grid and the context that accounts the work performed on it.
pub struct GridRead {
    pub sample: Arc<SampleGrid>,
    pub answered: Answered,
    /// How the grid's source-clipped flags were found.
    pub clipping: ClipDetection,
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
    /// Advanced by every release, so a read that began before one cannot put a grid back after it.
    generation: u64,
    /// Least recently used first.
    grids: VecDeque<(Key, Arc<SampleGrid>, Answered)>,
}

impl Held {
    fn bytes(&self) -> usize {
        self.grids.iter().map(|(_, grid, _)| grid.bytes()).sum()
    }

    /// Evict the least recently used grids until `bytes` more fit.
    fn make_room(&mut self, bytes: usize) {
        while !self.grids.is_empty() && self.bytes() + bytes > RETAINED_BYTES {
            self.grids.pop_front();
        }
    }
}

/// One shared least-recently-used cache, bounded by [`RETAINED_BYTES`]. It serves every client of
/// its render context alike, holds no frame, source or evaluation, and loses only time when it
/// evicts. A photograph's grids go when the catalog forgets the photograph.
#[derive(Default)]
pub(crate) struct GridCache(Mutex<Held>);

impl GridCache {
    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drop every grid of `asset`, and refuse the grids of reads already under way.
    pub(crate) fn release(&self, asset: &AssetId) {
        let mut held = self.held();
        held.grids.retain(|(key, ..)| key.asset != *asset);
        held.generation = held.generation.wrapping_add(1);
    }

    /// The current generation and the grid for `key`, which becomes the most recently used.
    fn find(&self, key: &Key) -> (u64, Option<(Arc<SampleGrid>, Answered)>) {
        let mut held = self.held();
        let hit = held
            .grids
            .iter()
            .position(|(k, ..)| k == key)
            .and_then(|index| held.grids.remove(index));
        let found = hit
            .as_ref()
            .map(|(_, grid, answered)| (Arc::clone(grid), answered.clone()));
        if let Some(entry) = hit {
            held.grids.push_back(entry);
        }
        (held.generation, found)
    }

    fn make_room(&self, bytes: usize) {
        self.held().make_room(bytes);
    }

    fn keep(&self, generation: u64, key: Key, grid: &Arc<SampleGrid>, answered: &Answered) {
        let mut held = self.held();
        if held.generation != generation || grid.bytes() > RETAINED_BYTES {
            return;
        }
        held.grids.retain(|(k, ..)| k != &key);
        held.make_room(grid.bytes());
        held.grids
            .push_back((key, Arc::clone(grid), answered.clone()));
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

/// The grid of the stage the layer at `index` receives, at most [`MAX_SIDE`] points on the output
/// frame's long side, each the input pixel nearest the point mapped back through the stack's
/// geometry. Its identity is the source, the content of the layers before `index`, the output
/// geometry, the grid's dimensions, the input mode, how clipping is detected and the renderer:
/// the layers from `index` on change nothing but the geometry. A point's clipped flag reads the
/// source under its content position: a JPEG's code, or a RAW's sensor when the evaluation holds
/// it ([`ClipDetection`]).
pub fn read(
    evaluation: &Evaluation,
    index: usize,
    reads: &dyn TileReads,
    cancel: &Cancel,
) -> Result<GridRead, Error> {
    cancel.check()?;
    let recipe = evaluation.recipe();
    let layers = recipe
        .layers
        .get(..index)
        .ok_or_else(|| Error::validation("sample grid prefix exceeds stack"))?;
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
    let clipping = ClipDetection::of(evaluation);
    let mut digest = Sha256::new();
    digest.update(content_hash(&prefix)?);
    digest.update(output.sha256().as_bytes());
    for value in grid {
        digest.update(value.to_le_bytes());
    }
    digest.update([
        u8::from(matches!(mode, crate::render::MaskInputMode::ColourRun)),
        clipping as u8,
    ]);
    let key = reads.grid_renderer().map(|renderer| Key {
        asset: evaluation.entry().asset_id.clone(),
        source: evaluation.source().identity(),
        content: digest.finalize().into(),
        renderer,
    });
    let cache = evaluation.context().sample_grids();
    let (generation, kept) = key.as_ref().map_or((0, None), |key| cache.find(key));
    if let Some((sample, answered)) = kept {
        return Ok(GridRead {
            sample,
            answered,
            clipping,
            context: evaluation.context().clone(),
            cancel: cancel.clone(),
        });
    }
    let count = grid[0] as usize * grid[1] as usize;
    if key.is_some() {
        cache.make_room(count * POINT_BYTES);
    }
    // Before allocation: positions, source flags, result RGB, gather sort order and one tile.
    // These are sample-sized; the provider's existing stage/tile storage has its own accounting.
    let _scratch = evaluation
        .context()
        .scratch()
        .reserve(count * (8 + 1 + 12 + size_of::<usize>()) + 256 * 256 * 12);
    let mut points = Vec::with_capacity(count);
    let mut source_clipped = Vec::with_capacity(count);
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
            source_clipped.push(clipped_at(evaluation, sx, sy));
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
    let sample = Arc::new(SampleGrid {
        grid,
        rgb: answer.linear,
        source_clipped,
    });
    cancel.check()?;
    if let Some(key) = key
        && key.renderer == answered.record
    {
        cache.keep(generation, key, &sample, &answered);
    }
    Ok(GridRead {
        sample,
        answered,
        clipping,
        context: evaluation.context().clone(),
        cancel: cancel.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(asset: &AssetId, n: u8) -> Key {
        let source = crate::PreviewSource::Jpeg(crate::SourceImage {
            width: 1,
            height: 1,
            rgba: Arc::new(vec![0, 0, 0, 255]),
            fingerprint: "cache-test".into(),
            orientation: 1,
            capture: Arc::default(),
        });
        Key {
            asset: asset.clone(),
            source: source.identity(),
            content: [n; 32],
            renderer: RendererRecord::Reference,
        }
    }

    fn grid() -> Arc<SampleGrid> {
        Arc::new(SampleGrid {
            grid: [1024, 1024],
            rgb: vec![[0.; 3]; 1024 * 1024],
            source_clipped: vec![false; 1024 * 1024],
        })
    }

    fn retained(cache: &GridCache) -> usize {
        cache.held().bytes()
    }

    /// Two of these 13 MiB grids fit the 32 MiB bound; a third evicts the least recently used,
    /// whichever photo or client it served.
    #[test]
    fn the_grid_cache_is_a_byte_bounded_lru_across_photos() {
        let cache = GridCache::default();
        let (first, second) = (AssetId::new(), AssetId::new());
        let generation = cache.find(&key(&first, 0)).0;
        cache.keep(
            generation,
            key(&first, 0),
            &grid(),
            &Answered::reference(None),
        );
        cache.keep(
            generation,
            key(&second, 1),
            &grid(),
            &Answered::reference(None),
        );
        assert!(retained(&cache) <= RETAINED_BYTES);
        // A hit makes the first photo's grid the most recently used, so the third evicts the second.
        assert!(cache.find(&key(&first, 0)).1.is_some());
        cache.keep(
            generation,
            key(&first, 2),
            &grid(),
            &Answered::reference(None),
        );
        assert!(retained(&cache) <= RETAINED_BYTES);
        assert!(cache.find(&key(&first, 0)).1.is_some());
        assert!(cache.find(&key(&second, 1)).1.is_none());
        assert!(cache.find(&key(&first, 2)).1.is_some());
        // Room is made before a read builds its grid, oldest first.
        cache.make_room(grid().bytes());
        assert_eq!(cache.held().grids.len(), 1);
        assert!(cache.find(&key(&first, 2)).1.is_some());
    }

    #[test]
    fn releasing_a_photo_drops_its_grids_and_refuses_reads_already_under_way() {
        let cache = GridCache::default();
        let (released, other) = (AssetId::new(), AssetId::new());
        let before = cache.find(&key(&released, 0)).0;
        cache.keep(before, key(&other, 1), &grid(), &Answered::reference(None));
        cache.keep(
            before,
            key(&released, 0),
            &grid(),
            &Answered::reference(None),
        );
        cache.release(&released);
        assert!(cache.find(&key(&released, 0)).1.is_none());
        assert!(cache.find(&key(&other, 1)).1.is_some());
        // A read that began before the release cannot put its grid back.
        cache.keep(
            before,
            key(&released, 0),
            &grid(),
            &Answered::reference(None),
        );
        assert!(cache.find(&key(&released, 0)).1.is_none());
        let after = cache.find(&key(&released, 0)).0;
        cache.keep(
            after,
            key(&released, 0),
            &grid(),
            &Answered::reference(None),
        );
        assert!(cache.find(&key(&released, 0)).1.is_some());
    }
}
