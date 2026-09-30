//! Rendered previews of developed photographs (`docs/design/catalog.md`, "The index and previews
//! cache"): a photograph's **grid** (512 px) and **large** (2048 px) tiers, rendered from one of its
//! entries — the current one unless another is named — through the Fit preview's proxy path, so each
//! tier is the entry approximated at that size and labelled as the Fit preview labels it.
//!
//! These are the domain functions the preview lane runs `preview.read {item: photo}` and its
//! render worker with (`renders.rs`, `photos.rs`, and the owner's `api/owner/previews/renders.rs`),
//! which write the files and their `photo_previews` rows, collect stale ones and keep the large
//! tier within the byte budget it shares with the loupe tier; this module plans a render, renders
//! it, and names the keys, files and stale rows.
//!
//! # Planned on the owner, rendered on a worker
//!
//! [`plan_render`] runs on the catalog owner in `O(layers)` and reads no pixel and no file of the
//! photograph: [`EditorService::saved_entry`] (the cached asset head and the hydrated, shared entry,
//! its stack admitted as one of the asset's kind), [`EditorService::registry`], the provider of each
//! layer, and [`EditorService::artifact_bindings`] (catalog rows, the artifact root's manifest and
//! one stat per referenced artifact, and the verified bytes the owner already keeps ready). None of
//! them touches the editor's one-slot source cache: nothing is prepared, adopted or evicted. The
//! [`RenderRequest`] it answers owns everything the render needs and is `Send`.
//!
//! [`render`] runs on a preview-lane worker, never on the owner (performance rule 5). It prepares
//! the original **off the editor's source cache and source worker**, on its own thread, through the
//! one sanctioned reader, [`SourceWork::run`]: one bounded read of the file, its fingerprint checked
//! against the asset's, decoded, and a RAW developed at the gains the entry's development layer asks
//! for, exactly as the source worker develops it for `render_current`. The artifacts the owner did
//! not keep ready are read and verified the same way. Nothing prepared is handed back to the
//! service: the render owns its source, and it is gone when the render returns.
//!
//! # The proxy path
//!
//! Each tier is the entry rendered against a proxy source fitted to the tier's square bounds
//! (`PHOTO_GRID_SIDE`, `PHOTO_LARGE_SIDE`) by the steps the preview worker's Fit proxy phase takes:
//! the stack compiled at the exact stage, the proxy plan read from its output stage, the proxy stage
//! compiled once with the window its stack reads, the source area-averaged to that plan, and the
//! proxy stage rendered with the exact stage's spatial estimates. A tier is therefore byte for byte
//! the Fit preview's proxy frame at the same bounds, and it carries the same
//! [`ProxyApproximation`]: a spatial layer's neighbourhoods scale with the stage, and a mask thinner
//! than two proxy pixels is supersampled. A stage that already fits the tier is rendered exactly, as
//! the Fit preview presents its exact phase. A stack that is not proxy-eligible (a pixel-stage
//! layer), or whose proxy fails, takes the exact path and says why ([`TierPath::Exact`]): the entry
//! is rendered exactly once and area-averaged to each tier. Both tiers come from one preparation;
//! the exact frame, when one is needed, is rendered once for both. A tier is labelled
//! `approximate` exactly when its proxy render is ([`RenderedTier::approximate`]); the lane stores
//! the label in its row, so a tier read from the cache says the same.
//!
//! A tier is upright sRGB display bytes, encoded as a baseline JPEG at [`RENDERED_JPEG_QUALITY`]
//! with 4:2:0 chroma and no metadata or profile.
//!
//! # Failing honestly
//!
//! An original that is missing, offline or no longer the file that was imported (its identity,
//! length or fingerprint), or that changes while it is read, is `source-unavailable`: nothing is
//! rendered from other bytes. A layer whose provider is missing or unavailable is refused naming
//! its layers ([`Error::unavailable_effect`]), and an artifact that cannot be bound or read is
//! refused naming the layers that reference it; no tier is ever rendered without an effect. A
//! cancelled render is `cancelled`. Until a photograph's tiers are rendered, the lane shows its
//! camera preview, labelled `embedded` (`camera.rs`); [`is_current`] and [`RenderedKey`] tell it
//! whether a cached tier is the entry's at this [`RENDERER_GENERATION`].
//!
//! # Never delaying Develop
//!
//! Nothing here uses or waits on the editor's source worker, its one-slot source cache, the
//! desktop's preview worker (`preview/queue.rs`), its proxy cache or the point worker: a render
//! reads the file itself on the lane's thread and renders with a [`RenderContext`] of its own
//! ([`rendered_context`]), so a backlog of rendered previews never holds a budget, a queue slot or a
//! lock an open Develop preview needs. It competes with Develop only for CPU on the shared Rayon
//! pool. `a_rendered_backlog_never_delays_an_open_develop_preview` holds a backlog of prepared
//! renders, each holding its decoded original, and shows a Develop preview job completing through
//! the existing preview queue meanwhile.
//!
//! # Memory and cancellation
//!
//! One render holds one preparation. At its peak, by the sizes of its buffers (not measured), a
//! JPEG holds its file's bytes (within `MAX_JPEG_BYTES`, 128 MiB) while they decode into the RGBA8
//! frame (within the 512 MiB evaluated-frame limit, `luxforge_raw::MAX_FRAME_BYTES`): about 230 MiB
//! of frame for a 60 MP JPEG, plus one proxy of at most 16 MiB at 2048 px and its rendered frame. A
//! RAW holds its file's bytes (within `luxforge_raw::MAX_SOURCE_BYTES`) and the mosaic while it
//! decodes, then the mosaic and the float planes (within `luxforge_raw::MAX_RGB_BYTES`) while it
//! develops, then the planes alone — the mosaic is dropped before rendering — about 110 MiB of
//! bytes and mosaic and 460 MiB of planes for a 40 MP RAW, plus a proxy of at most 48 MiB of planes
//! at 2048 px and its frame. The exact path adds one exact frame (within the evaluated-frame limit)
//! while both tiers are made from it. The render is synchronous, so a lane worker holds at most one
//! preparation at a time; the lane keeps the design's one RAW at a time off the editor's cache by
//! running every render on its one render worker (`renders.rs`).
//!
//! Every pass checks the caller's [`Cancel`]: the reads and the RAW decode and development through
//! its flag, the proxy downscale per row, every rendering pass per row or chunk, and the JPEG
//! encode per strip. A cancelled render returns `cancelled` and nothing else.

use crate::{
    AssetId, Cancel, EditorService, EntryId, Error, ErrorKind, HistoryEntry, LinearSettings,
    ModuleRegistry, PreparedArtifact, PreviewSource, ProxyApproximation, ProxyBounds, ProxyPlan,
    Raster, Recipe, Render, RenderContext, RenderOptions, SourceImage,
    artifacts::{ArtifactId, ArtifactRead},
    catalog_types::{
        PHOTO_GRID_SIDE, PHOTO_LARGE_SIDE, PreviewInfo, PreviewItem, PreviewOrigin, PreviewTier,
    },
    editor::{AssetRecord, FilePreparation, Prepared, SourceWork, original_signature},
    source::{PreparedSource, RawPrepared},
};
use luxforge_jpeg::Settings;
use rusqlite::{Connection, params};
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

/// The renderer generation every rendered tier is made and keyed under, recorded in the index's
/// `photo_previews.renderer` column. **Bump it whenever a change makes a rendered tier's bytes
/// differ** — a module's or the renderer's arithmetic, the proxy downscale, the tier sides, the JPEG
/// settings — so every tier of the old generation is discarded and rendered again rather than shown
/// for the new one.
pub(crate) const RENDERER_GENERATION: u32 = 1;

/// The quality a rendered tier's JPEG is written at, with 4:2:0 chroma: an edited Nikon Z 6
/// photograph's grid tier takes 33 KB and its large tier 272 KB.
pub(crate) const RENDERED_JPEG_QUALITY: u8 = 85;

/// A rendered tier's chroma sampling, `(2, 2)` for 4:2:0: at 512 and 2048 px the halved chroma is
/// below what the grid and Develop's first frame show.
const RENDERED_CHROMA: (u8, u8) = (2, 2);

/// The directory under `<catalog>.index/previews/` rendered tiers are written into.
const PHOTOS_DIR: &str = "photos";

/// The long edge of a photograph's tier: 512 px for the grid, 2048 px for the large tier. The loupe
/// tier is a file's embedded preview and is never rendered.
pub(crate) fn tier_side(tier: PreviewTier) -> Result<u32, Error> {
    match tier {
        PreviewTier::Grid => Ok(PHOTO_GRID_SIDE),
        PreviewTier::Large => Ok(PHOTO_LARGE_SIDE),
        PreviewTier::Loupe => Err(Error::validation(
            "a developed photograph's rendered tiers are grid and large; loupe is a file's",
        )),
    }
}

/// What one rendered tier is made from: the asset, the entry, the tier and the renderer
/// generation. Every part changes its [`Self::preview_key`] and its [`Self::file_name`], so a commit
/// — a new entry — never overwrites a file a client may be reading, and a generation change never
/// passes an old tier off as a new one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RenderedKey {
    pub asset_id: AssetId,
    pub entry_id: EntryId,
    pub tier: PreviewTier,
    pub generation: u32,
}

impl RenderedKey {
    /// The key of `tier` of `entry_id` at this build's [`RENDERER_GENERATION`]. A loupe tier is
    /// refused.
    pub(crate) fn new(
        asset_id: &AssetId,
        entry_id: &EntryId,
        tier: PreviewTier,
    ) -> Result<Self, Error> {
        tier_side(tier)?;
        Ok(Self {
            asset_id: asset_id.clone(),
            entry_id: entry_id.clone(),
            tier,
            generation: RENDERER_GENERATION,
        })
    }

    /// The key a [`PreviewInfo`] carries: `photo:<asset>:<entry>:<tier>:r<generation>`.
    pub(crate) fn preview_key(&self) -> String {
        format!(
            "photo:{}:{}:{}:r{}",
            self.asset_id,
            self.entry_id,
            self.tier.as_str(),
            self.generation
        )
    }

    /// Where the tier's JPEG goes, relative to `<catalog>.index/previews/`:
    /// `photos/<ab>/<asset>-<entry>-<tier>-r<generation>.jpg`, sharded by the first two characters
    /// of the asset identity's random part into 256 directories, about 800 files each for both
    /// tiers of 100,000 photographs.
    pub(crate) fn file_name(&self) -> PathBuf {
        let random = &self.asset_id.as_str()[AssetId::PREFIX.len()..];
        let shard = random.get(..2).unwrap_or("00");
        PathBuf::from(PHOTOS_DIR).join(shard).join(format!(
            "{}-{}-{}-r{}.jpg",
            self.asset_id,
            self.entry_id,
            self.tier.as_str(),
            self.generation
        ))
    }

    /// The item a preview of this tier is of: the photograph at this entry.
    pub(crate) fn item(&self) -> PreviewItem {
        PreviewItem::Photo {
            asset_id: self.asset_id.clone(),
            entry_id: Some(self.entry_id.clone()),
        }
    }
}

/// Whether a cached preview whose key is `preview_key` is `tier` of `entry_id` rendered at this
/// build's generation: the comparison the lane makes before answering `preview.read` from the cache
/// or queueing a render (and, for a photograph not yet rendered, showing its camera preview
/// meanwhile).
pub(crate) fn is_current(
    preview_key: &str,
    asset_id: &AssetId,
    entry_id: &EntryId,
    tier: PreviewTier,
) -> bool {
    RenderedKey::new(asset_id, entry_id, tier).is_ok_and(|key| key.preview_key() == preview_key)
}

/// How one tier was rendered, which its `approximate` label is read from
/// ([`RenderedTier::approximate`]) and the lane stores with its row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TierPath {
    /// Against a proxy source fitted to the tier: the Fit preview's proxy frame at the tier's
    /// bounds, approximate for the reasons it names when [`ProxyApproximation::is_approximate`].
    Proxy { approximation: ProxyApproximation },
    /// The exact render, area-averaged to the tier when it is larger. `declined` says why there is
    /// no proxy: the stage already fits the tier, the stack is not proxy-eligible, or its proxy
    /// failed.
    Exact { declined: String },
}

/// One rendered tier: its key, its size and its JPEG, and how it was rendered.
#[derive(Clone, Debug)]
pub(crate) struct RenderedTier {
    pub key: RenderedKey,
    pub width: u32,
    pub height: u32,
    /// Baseline JPEG, upright sRGB, [`RENDERED_JPEG_QUALITY`], 4:2:0, no metadata.
    pub jpeg: Vec<u8>,
    pub path: TierPath,
}

impl RenderedTier {
    /// Whether the tier approximates its entry: rendered through a proxy whose render is
    /// approximate ([`ProxyApproximation::is_approximate`]: a spatial layer, a thin mask), as the
    /// Fit preview labels the same frame. An exact render, area-averaged to the tier, is not.
    pub(crate) fn approximate(&self) -> bool {
        matches!(self.path, TierPath::Proxy { approximation } if approximation.is_approximate())
    }

    /// The preview this tier is once the lane has written it at `path`, under
    /// `<catalog>.index/previews/`.
    pub(crate) fn info(&self, path: PathBuf) -> PreviewInfo {
        PreviewInfo {
            item: self.key.item(),
            tier: self.key.tier,
            path,
            width: self.width,
            height: self.height,
            origin: PreviewOrigin::Rendered,
            approximate: self.approximate(),
            bytes: self.jpeg.len() as u64,
            key: self.key.preview_key(),
        }
    }
}

/// Everything one render needs, planned on the catalog owner by [`plan_render`]: the asset record
/// (the original's path, identity, length and fingerprint), the entry (shared from the entry
/// cache), how its original is prepared and verified, the artifacts it references — the bytes the
/// owner keeps ready and how the rest are read — the providers, the render context and the tiers.
/// It reads nothing when it is built and owns nothing that scales with the image.
pub(crate) struct RenderRequest {
    asset: AssetRecord,
    entry: Arc<HistoryEntry>,
    target: FilePreparation,
    ready: Vec<Arc<PreparedArtifact>>,
    reads: Vec<ArtifactRead>,
    registry: Arc<ModuleRegistry>,
    context: RenderContext,
    tiers: Vec<PreviewTier>,
}

impl RenderRequest {
    pub(crate) fn asset_id(&self) -> &AssetId {
        &self.asset.id
    }

    /// The entry rendered: the one named, or the current one when the plan was made.
    pub(crate) fn entry_id(&self) -> &EntryId {
        &self.entry.id
    }

    /// The key of every tier this request renders, in the order it renders them.
    pub(crate) fn keys(&self) -> Vec<RenderedKey> {
        self.tiers
            .iter()
            .map(|&tier| RenderedKey {
                asset_id: self.asset.id.clone(),
                entry_id: self.entry.id.clone(),
                tier,
                generation: RENDERER_GENERATION,
            })
            .collect()
    }
}

/// The one render context rendered previews are evaluated in: its scratch and spatial budgets and
/// its estimate store belong to the lane, so a backlog never takes budget from the editor's
/// evaluations or an entry from its store.
pub(crate) fn rendered_context() -> RenderContext {
    static CONTEXT: OnceLock<RenderContext> = OnceLock::new();
    CONTEXT.get_or_init(RenderContext::new).clone()
}

/// Plan the render of `tiers` (grid, large or both, each once) of `asset_id`'s entry `entry_id`, or
/// of its current entry when none is named, on the catalog owner. `O(layers)`: a cached head and
/// entry read with the stack's admission, one provider lookup per layer, and the artifact
/// bindings (rows and stats); no pixel, no file of the photograph and never the source cache (see
/// the module documentation). A layer without an available provider is refused naming its layers,
/// and an artifact that cannot be bound naming the layers that reference it.
pub(crate) fn plan_render(
    service: &EditorService,
    asset_id: &AssetId,
    entry_id: Option<&EntryId>,
    tiers: &[PreviewTier],
) -> Result<RenderRequest, Error> {
    if tiers.is_empty() {
        return Err(Error::validation("a render makes at least one tier"));
    }
    let mut wanted = Vec::with_capacity(tiers.len());
    for &tier in tiers {
        tier_side(tier)?;
        if wanted.contains(&tier) {
            return Err(Error::validation(format!(
                "the {} tier is named twice",
                tier.as_str()
            )));
        }
        wanted.push(tier);
    }
    let (asset, entry) = service.saved_entry(asset_id, entry_id)?;
    let recipe = &entry.snapshot.recipe;
    let registry = service.registry();
    // A layer no available provider evaluates is refused now, naming every layer of its effect, so
    // nothing is read for a stack no render could evaluate. The worker's compile refuses anything
    // else the stack cannot be evaluated with.
    if let Some(layer) = recipe.layers.iter().find(|layer| {
        registry
            .effect(&layer.effect_id)
            .is_none_or(|(provider, _)| !provider.descriptor().is_available())
    }) {
        let holding: Vec<&str> = recipe
            .layers
            .iter()
            .filter(|other| other.effect_id == layer.effect_id)
            .map(|other| other.id.as_str())
            .collect();
        return Err(Error::unavailable_effect(&layer.effect_id, &holding));
    }
    let (ready, reads) = service
        .artifact_bindings(recipe)
        .map_err(|error| naming_edits(error, recipe, None))?;
    let target = FilePreparation::for_recipe(&asset, recipe)?;
    Ok(RenderRequest {
        asset,
        entry,
        target,
        ready,
        reads,
        registry: Arc::clone(registry),
        context: rendered_context(),
        tiers: wanted,
    })
}

/// Render `request` on the calling thread, a preview-lane worker's: prepare its original off the
/// editor's cache, then each tier through the proxy path and encode it. See the module
/// documentation for what it reads, holds and refuses.
pub(crate) fn render(request: &RenderRequest, cancel: &Cancel) -> Result<Vec<RenderedTier>, Error> {
    render_with(request, cancel, prepare, |key, raster, path| {
        Ok(RenderedTier {
            width: raster.width,
            height: raster.height,
            jpeg: encode(&raster, cancel)?,
            key,
            path,
        })
    })
}

/// Every row of `asset_id` in the index's `photo_previews` that no longer describes a current tier
/// of `current`, its current entry, for the lane to delete with its file: rows of other entries,
/// and rendered rows of another renderer generation. A camera-preview row of the current entry is
/// kept until a render replaces it.
pub(crate) fn stale_rows(
    index: &Connection,
    asset_id: &AssetId,
    current: &EntryId,
) -> Result<Vec<PhotoPreviewRow>, Error> {
    let mut statement = index.prepare(
        "SELECT asset_id,entry_id,tier,renderer,origin,path,bytes FROM photo_previews
         WHERE asset_id=?1 AND (entry_id<>?2 OR (origin='rendered' AND renderer<>?3))
         ORDER BY entry_id,tier",
    )?;
    let rows = statement.query_map(
        params![asset_id.as_str(), current.as_str(), RENDERER_GENERATION],
        PhotoPreviewRow::columns,
    )?;
    rows.map(|row| PhotoPreviewRow::parse(row?)).collect()
}

/// At most `limit` rendered rows of any photograph made by another renderer generation, for the
/// lane to discard, a page at a time, after a build that bumped [`RENDERER_GENERATION`].
pub(crate) fn other_generation_rows(
    index: &Connection,
    limit: usize,
) -> Result<Vec<PhotoPreviewRow>, Error> {
    let mut statement = index.prepare(
        "SELECT asset_id,entry_id,tier,renderer,origin,path,bytes FROM photo_previews
         WHERE origin='rendered' AND renderer<>?1 ORDER BY asset_id,entry_id,tier LIMIT ?2",
    )?;
    let rows = statement.query_map(
        params![
            RENDERER_GENERATION,
            i64::try_from(limit).unwrap_or(i64::MAX)
        ],
        PhotoPreviewRow::columns,
    )?;
    rows.map(|row| PhotoPreviewRow::parse(row?)).collect()
}

/// One row of the index's `photo_previews`, as the stale-row listings return it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PhotoPreviewRow {
    pub asset_id: AssetId,
    pub entry_id: EntryId,
    pub tier: PreviewTier,
    pub renderer: i64,
    pub origin: PreviewOrigin,
    /// As the lane stored it.
    pub path: PathBuf,
    pub bytes: u64,
}

/// One `photo_previews` row's columns as SQLite holds them, in the listings' order.
type PhotoPreviewColumns = (String, String, String, i64, String, String, i64);

impl PhotoPreviewRow {
    fn columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<PhotoPreviewColumns> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ))
    }

    fn parse(columns: PhotoPreviewColumns) -> Result<Self, Error> {
        let (asset_id, entry_id, tier, renderer, origin, path, bytes) = columns;
        let unreadable = || Error::catalog("the index holds an unreadable photo preview row");
        Ok(Self {
            asset_id: AssetId::parse(asset_id).map_err(|_| unreadable())?,
            entry_id: EntryId::parse(entry_id).map_err(|_| unreadable())?,
            tier: PreviewTier::ALL
                .into_iter()
                .find(|known| known.as_str() == tier)
                .ok_or_else(unreadable)?,
            renderer,
            origin: PreviewOrigin::parse(&origin).ok_or_else(unreadable)?,
            path: PathBuf::from(path),
            bytes: u64::try_from(bytes).map_err(|_| unreadable())?,
        })
    }
}

/// What one render's preparation made: the source its tiers are rendered against, and the bytes
/// of the artifacts the owner did not keep ready, read and verified here.
struct PreparedRender {
    source: PreviewSource,
    verified: Vec<Arc<PreparedArtifact>>,
}

/// [`render`] with its preparation and what each tier becomes as parameters, so a test can hold a
/// render inside its preparation and read its rasters. `prepare` runs once for every tier.
fn render_with<T>(
    request: &RenderRequest,
    cancel: &Cancel,
    prepare: impl FnOnce(&RenderRequest, &Cancel) -> Result<PreparedRender, Error>,
    mut finish: impl FnMut(RenderedKey, Raster, TierPath) -> Result<T, Error>,
) -> Result<Vec<T>, Error> {
    cancel.check()?;
    let PreparedRender { source, verified } = prepare(request, cancel)?;
    cancel.check()?;
    let recipe = bound(&request.entry.snapshot.recipe, &request.ready, verified);
    let snapshot = &request.entry.snapshot.id;
    // The stack's one compilation at the exact stage, as the preview worker makes it: every tier's
    // proxy plan reads its output stage, and the exact frame, when one is needed, renders it. A
    // layer this build cannot evaluate is refused here, naming its layers.
    let exact = crate::render(
        &request.registry,
        source.input(),
        &recipe,
        RenderOptions::exact(cancel),
        &request.context,
    )?;
    let eligible = request.registry.proxy_eligible(&recipe);
    let mut exact_frame: Option<Raster> = None;
    let mut rendered = Vec::with_capacity(request.tiers.len());
    for key in request.keys() {
        cancel.check()?;
        let side = tier_side(key.tier)?;
        let bounds = ProxyBounds {
            width: side,
            height: side,
        };
        // In the Fit preview's order: eligibility, then whether the stage already fits.
        let declined = match (&eligible, exact.proxy_plan(bounds)) {
            (Err(ineligible), _) => ineligible.detail.clone(),
            (Ok(()), None) => "the stage already fits the tier".to_owned(),
            (Ok(()), Some(plan)) => {
                match proxy_tier(&exact, &recipe, &source, plan, request, cancel) {
                    Ok((raster, approximation)) => {
                        rendered.push(finish(key, raster, TierPath::Proxy { approximation })?);
                        continue;
                    }
                    Err(error) if error.kind == ErrorKind::Cancelled => return Err(error),
                    Err(error) => error.detail,
                }
            }
        };
        let frame = match &exact_frame {
            Some(frame) => frame,
            None => exact_frame.insert(exact.frame(snapshot.clone())?),
        };
        let raster = fitted(frame, bounds, cancel)?;
        rendered.push(finish(key, raster, TierPath::Exact { declined })?);
    }
    Ok(rendered)
}

/// The Fit preview's proxy phase at `plan`, as the preview worker renders it: the proxy stage
/// compiled with the window its stack reads, the source downscaled to it and that compilation
/// rendered, with the frame's approximation read from it.
fn proxy_tier(
    exact: &Render<'_>,
    recipe: &Recipe,
    source: &PreviewSource,
    plan: ProxyPlan,
    request: &RenderRequest,
    cancel: &Cancel,
) -> Result<(Raster, ProxyApproximation), Error> {
    let stage = exact.proxy_window(&request.registry, recipe, plan);
    let proxy = source.proxy_cancellable(stage.plan(), cancel)?;
    let render = exact.render_proxy(proxy.input(), stage, cancel, &request.context)?;
    Ok((
        render.frame(request.entry.snapshot.id.clone())?,
        render.approximation(),
    ))
}

/// An exact frame fitted to `bounds` by the proxy's area-average downscale of display bytes, or
/// the frame itself, shared, when it already fits.
fn fitted(frame: &Raster, bounds: ProxyBounds, cancel: &Cancel) -> Result<Raster, Error> {
    let size = (frame.width, frame.height);
    let Some(plan) = ProxyPlan::fit(size, size, bounds) else {
        return Ok(frame.clone());
    };
    let upright = PreviewSource::Jpeg(SourceImage {
        width: frame.width,
        height: frame.height,
        rgba: Arc::clone(&frame.rgba),
        fingerprint: frame.source_fingerprint.clone(),
        orientation: 1,
        capture: Arc::default(),
    });
    let PreviewSource::Jpeg(scaled) = upright.proxy_cancellable(plan, cancel)? else {
        return Err(Error::internal("a byte proxy came back as planes"));
    };
    Ok(Raster {
        width: scaled.width,
        height: scaled.height,
        rgba: scaled.rgba,
        source_fingerprint: frame.source_fingerprint.clone(),
        snapshot_id: frame.snapshot_id.clone(),
    })
}

/// `recipe` bound with every artifact it references: as it is when its own table already holds
/// them all, else a copy whose table is the owner's ready bytes and the ones this render verified.
fn bound<'r>(
    recipe: &'r Recipe,
    ready: &[Arc<PreparedArtifact>],
    verified: Vec<Arc<PreparedArtifact>>,
) -> Cow<'r, Recipe> {
    let held = recipe
        .layers
        .iter()
        .flat_map(|layer| &layer.artifacts)
        .all(|id| recipe.artifacts.get(id).is_some());
    if held {
        return Cow::Borrowed(recipe);
    }
    let mut bound = recipe.clone();
    bound.artifacts = ready.iter().cloned().chain(verified).collect();
    Cow::Owned(bound)
}

/// The production preparation, on the calling thread: the artifacts the owner did not keep ready,
/// each read and verified through the one source work, then the original, checked against the
/// asset it was imported as and prepared through the one source work at the entry's development.
/// Nothing is handed to the service.
fn prepare(request: &RenderRequest, cancel: &Cancel) -> Result<PreparedRender, Error> {
    let asset = &request.asset;
    let recipe = &request.entry.snapshot.recipe;
    // The artifacts first: they are small, and one that is gone refuses the render before the
    // original is decoded.
    let mut verified = Vec::with_capacity(request.reads.len());
    for read in &request.reads {
        cancel.check()?;
        let work = SourceWork::Artifacts(asset.id.clone());
        match work.run(std::slice::from_ref(read), cancel.flag()) {
            Ok(Prepared::Artifacts(_, artifacts)) => {
                verified.extend(artifacts.into_iter().map(|artifact| artifact.artifact));
            }
            Ok(_) => return Err(Error::internal("artifact work prepared an original")),
            Err(error) => {
                return Err(cancelled_or(
                    cancel,
                    naming_edits(error, recipe, Some(&read.id)),
                ));
            }
        }
    }
    cancel.check()?;
    // Missing, offline, or no longer the imported file by identity or length.
    original_signature(asset)?;
    let unavailable = |error: Error| {
        if let Err(cancelled) = cancel.check() {
            return cancelled;
        }
        if !asset.locator.exists() {
            return Error::source_unavailable("the original is missing");
        }
        match error.kind {
            // The file changed between its checks, or while it was read.
            ErrorKind::Conflict => Error::source_unavailable(format!(
                "the original changed while it was read: {}",
                error.detail
            )),
            _ => error,
        }
    };
    let work =
        SourceWork::file(&asset.locator, Some(request.target.clone())).map_err(unavailable)?;
    // The fingerprint is checked against the asset's inside the work, so another file's bytes are
    // `source-unavailable` before anything is rendered from them.
    let Prepared::File(file, _) = work.run(&[], cancel.flag()).map_err(unavailable)? else {
        return Err(Error::internal("a file's work prepared something else"));
    };
    let source = match file.source {
        PreparedSource::Jpeg(image) => PreviewSource::Jpeg(image),
        PreparedSource::Raw(RawPrepared {
            sensor,
            linear,
            gains,
            capture,
        }) => {
            // The mosaic and the capture fields go before any frame is rendered.
            drop((sensor, capture));
            if request.target.raw.as_ref().map(|raw| raw.gains) != Some(gains) {
                return Err(Error::internal(
                    "the RAW was not developed at its entry's gains",
                ));
            }
            PreviewSource::Raw {
                image: linear.ok_or_else(|| Error::internal("a RAW development without planes"))?,
                // The planes hold the entry's own white balance, so nothing is approximated: the
                // settings every committed render uses.
                settings: LinearSettings::default(),
            }
        }
    };
    if source.dimensions() != (asset.width, asset.height) || file.fingerprint != asset.fingerprint {
        return Err(Error::internal(
            "the prepared original is not the one the asset records",
        ));
    }
    Ok(PreparedRender { source, verified })
}

/// `raster` as a rendered tier's JPEG: [`RENDERED_JPEG_QUALITY`], 4:2:0, no metadata, checking
/// `cancel` per strip.
fn encode(raster: &Raster, cancel: &Cancel) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let settings = Settings {
        quality: RENDERED_JPEG_QUALITY,
        chroma: RENDERED_CHROMA,
        segments: &[],
        icc: None,
    };
    luxforge_jpeg::encode(
        &mut out,
        raster.width,
        raster.height,
        &raster.rgba,
        &settings,
        &mut |_| cancel.check(),
    )?;
    Ok(out)
}

/// `error`, binding or reading an artifact of `recipe`, naming the edits it leaves unrenderable:
/// the layers that reference `artifact` — or the one the error names — or, when it names none (the
/// artifact directory is missing), every layer that references an artifact. The kind is kept;
/// `data.layers` lists the layers.
fn naming_edits(error: Error, recipe: &Recipe, artifact: Option<&ArtifactId>) -> Error {
    if error.kind == ErrorKind::Cancelled {
        return error;
    }
    let named = artifact.cloned().or_else(|| {
        recipe
            .layers
            .iter()
            .flat_map(|layer| &layer.artifacts)
            .find(|id| error.detail.contains(id.as_str()))
            .cloned()
    });
    let affected: Vec<_> = recipe
        .layers
        .iter()
        .filter(|layer| match &named {
            Some(id) => layer.artifacts.contains(id),
            None => !layer.artifacts.is_empty(),
        })
        .collect();
    if affected.is_empty() {
        return error;
    }
    let edits: Vec<String> = affected
        .iter()
        .map(|layer| format!("layer {} of {}", layer.id, layer.effect_id))
        .collect();
    let layers: Vec<&str> = affected.iter().map(|layer| layer.id.as_str()).collect();
    Error::new(
        error.kind,
        format!("{} cannot be rendered: {}", edits.join(", "), error.detail),
    )
    .with_data(serde_json::json!({
        "layers": layers,
        "artifact_id": named,
    }))
}

/// `error`, or `cancelled` when `cancel` is set: the RAW crate and the artifact reads report a
/// cancellation of their own kind.
fn cancelled_or(cancel: &Cancel, error: Error) -> Error {
    match cancel.check() {
        Err(cancelled) => cancelled,
        Ok(()) => error,
    }
}

#[cfg(test)]
mod preview_rendered;
