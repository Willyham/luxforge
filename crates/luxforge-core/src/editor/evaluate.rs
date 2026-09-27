use super::{
    AnalysisPlan, AnalysisSelection, AssetRecord, DraftStamp, EditorService, ExportPlan,
    ExportTarget, PixelSample, SamplePlan,
    source::{Evaluated, RawSettingsMode, raw_settings, validate_source_recipe},
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    AssetId, Cancel, ContentPoint, Draft, EntryId, Error, HistoryEntry, ModuleRegistry, PreviewJob,
    PreviewSource, ProxyBounds, Raster, Recipe, Render, RenderContext, RenderOptions,
    StageTransform,
    analysis::AnalysisIdentity,
    export::CaptureMetadata,
    render::{Compiled, locate, transform_of},
    source::PreparedSource,
};
use std::{borrow::Cow, sync::Arc};

/// One stack bound for evaluation and compiled once: the entry it evaluates, the stack itself —
/// that entry's own or an open draft's effective recipe — bound with the verified bytes of every
/// artifact it references, the providers and the render context it is evaluated with, the draft
/// revision it was planned from, its compilation against the asset's content stage, and, once the
/// original is prepared, the source buffer it reads.
///
/// [`EditorService::evaluation`] builds it, on the catalog owner, and every evaluation the service
/// plans wraps it: a preview job, an analysis plan and job, a sample plan, a point plan, an export
/// plan and target. None of them derives the entry, the stack, the source or the compilation
/// again, so a preview's worker renders the compilation its owner made, and the identity an
/// analysis is filed under reads that compilation's output stage. A stack the host cannot evaluate
/// is kept with the reason it has no compilation, which each wrapper decides how to answer.
///
/// `S` is the source it reads: a [`PreviewSource`], or `()` for a question the catalog alone
/// answers — an export's output stage, a point's content pixel — which prepares nothing.
///
/// Cheap to clone: the bound stack and its compilation are shared, and the source shares its
/// pixels. It is immutable; a caller that wants another stack builds another evaluation.
#[derive(Clone)]
pub struct Evaluation<S = PreviewSource> {
    bound: Arc<Bound>,
    source: S,
}

struct Bound {
    entry: Arc<HistoryEntry>,
    /// The stack evaluated, when it is not the entry's own as stored: an open draft's effective
    /// recipe, or the entry's own with the artifacts it references bound in.
    recipe: Option<Recipe>,
    registry: Arc<ModuleRegistry>,
    context: RenderContext,
    /// The fingerprint of the source the stack is evaluated on: the asset's, which its prepared
    /// source holds.
    fingerprint: String,
    draft: Option<DraftStamp>,
    /// The stack compiled against the content stage at the exact phase, or why it cannot be.
    compiled: Result<Compiled, Error>,
}

impl<S> Evaluation<S> {
    /// The entry this evaluation shows: the one evaluated, or the current one a draft was planned
    /// over. Its identity and snapshot correlate what is evaluated with history; the stack is
    /// [`Self::recipe`], which differs from the entry's own while a draft is open.
    pub fn entry(&self) -> &HistoryEntry {
        &self.bound.entry
    }

    /// The stack evaluated, bound with the verified bytes of every artifact it references, so a
    /// worker evaluates it whatever the owner's artifact cache evicts meanwhile.
    pub fn recipe(&self) -> &Recipe {
        self.bound
            .recipe
            .as_ref()
            .unwrap_or(&self.bound.entry.snapshot.recipe)
    }

    /// The providers the stack is evaluated with; shared, never rebuilt per evaluation.
    pub fn registry(&self) -> &Arc<ModuleRegistry> {
        &self.bound.registry
    }

    /// The budgets and the estimate store every evaluation of the planning service shares.
    pub fn context(&self) -> &RenderContext {
        &self.bound.context
    }

    /// The draft, at the revision it held, whose effective recipe this evaluates; `None` for a
    /// saved entry's own stack.
    pub fn draft(&self) -> Option<&DraftStamp> {
        self.bound.draft.as_ref()
    }

    /// The revision of [`Self::draft`], for correlating a frame with the settings that produced it.
    pub fn draft_revision(&self) -> Option<u64> {
        self.bound.draft.as_ref().map(|draft| draft.draft_revision)
    }

    /// The output stage of the compiled stack, or `None` when the host cannot evaluate it.
    pub fn stage(&self) -> Option<(u32, u32)> {
        self.bound.compiled.as_ref().ok().map(|compiled| {
            let stage = compiled.stage();
            (stage.width, stage.height)
        })
    }

    /// The identity of the analysis of this evaluation: the asset, the source fingerprint, the
    /// entry and snapshot, the hash of the stack, the draft and the output stage. `O(recipe)`: it
    /// hashes the stack and reads the output stage of its one compilation, so a preview's frame
    /// and an analysis of the same stack are filed under the same identity.
    pub fn identity(&self) -> Result<AnalysisIdentity, Error> {
        let entry = self.entry();
        AnalysisIdentity::of(
            &entry.asset_id,
            &self.bound.fingerprint,
            entry,
            self.recipe(),
            self.bound.draft.clone(),
            self.stage(),
        )
    }

    /// The stack's compilation, or the reason the host cannot evaluate it.
    pub(crate) fn compiled(&self) -> Result<&Compiled, Error> {
        self.bound.compiled.as_ref().map_err(Clone::clone)
    }

    /// This evaluation reading `source`.
    fn reading<T>(self, source: T) -> Evaluation<T> {
        Evaluation {
            bound: self.bound,
            source,
        }
    }
}

impl Evaluation {
    /// Bind `recipe`, shown as `entry`, to `source` and compile it once against the source's
    /// dimensions: the value [`EditorService::evaluation`] builds from a catalog, built from parts
    /// a caller already holds. `recipe` must already carry the bytes of the artifacts it references.
    pub fn new(
        registry: Arc<ModuleRegistry>,
        context: RenderContext,
        source: PreviewSource,
        entry: HistoryEntry,
        recipe: Recipe,
        draft: Option<DraftStamp>,
    ) -> Self {
        let (width, height) = source.dimensions();
        #[cfg(test)]
        context.note_compile();
        let compiled = registry.compile(width, height, &recipe);
        Self {
            bound: Arc::new(Bound {
                entry: Arc::new(entry),
                recipe: Some(recipe),
                registry,
                context,
                fingerprint: source.fingerprint().to_owned(),
                draft,
                compiled,
            }),
            source,
        }
    }

    /// The buffer the stack is evaluated on.
    pub fn source(&self) -> &PreviewSource {
        &self.source
    }

    /// Render the stack at the exact phase under `cancel` from its one compilation, which it
    /// borrows: nothing is compiled again, and a stack the host cannot evaluate answers with the
    /// reason it could not be compiled.
    pub(crate) fn exact(&self, cancel: &Cancel) -> Result<Render<'_>, Error> {
        Render::shared(
            self.source.input(),
            self.compiled()?,
            RenderOptions::exact(cancel),
            self.context(),
        )
    }
}

impl<S: std::fmt::Debug> std::fmt::Debug for Evaluation<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Evaluation")
            .field("entry_id", &self.entry().id)
            .field("draft", &self.bound.draft)
            .field("stage", &self.stage())
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// What an evaluation is for, which decides the one rule a RAW white balance is evaluated under.
#[derive(Clone, Copy, Debug)]
pub(super) enum Purpose {
    /// Every evaluation but a preview: strict ([`RawSettingsMode::Strict`]).
    Exact,
    /// A preview, of the first `layer_count` layers when it names a count. An open draft's preview
    /// is the one evaluation that may approximate ([`RawSettingsMode::DraftPreview`]); a preview
    /// of a saved entry is strict.
    Preview { layer_count: Option<usize> },
}

/// An [`Evaluation`] as its builder holds it on the catalog owner, before it reads a source: with
/// the asset whose original it reads and the rule a RAW white balance is evaluated under, which
/// decide what a `preparation-required` refusal of it names.
pub(super) struct Stack {
    asset: AssetRecord,
    rule: RawSettingsMode,
    evaluation: Evaluation<()>,
}

impl Stack {
    /// The stack a refusal of this evaluation names ([`EditorService::needing`]): the development a
    /// RAW source must hold is the evaluated stack's own, except for a drafted preview, which
    /// approximates on the development its entry holds.
    fn needs(&self) -> Evaluated<'_> {
        let entry = self.evaluation.entry();
        let recipe = self.evaluation.recipe();
        Evaluated {
            asset: &self.asset,
            entry_id: &entry.id,
            recipe,
            developed: match self.rule {
                RawSettingsMode::DraftPreview => &entry.snapshot.recipe,
                RawSettingsMode::Strict => recipe,
            },
        }
    }
}

impl EditorService {
    /// The one way an evaluation is built: `selection`'s entry and stack, read from the entry cache
    /// without copying the entry, and for a draft planned against the current stack at the revision
    /// it holds now; admitted as a stack of the asset's source kind; its artifacts bound; and
    /// compiled once against the asset's content stage. No pixel is read and no source is prepared:
    /// [`Self::read`] reads the source. A stack the host cannot compile is kept with its reason,
    /// for the caller to answer. `O(layers)` plus the draft's plan; a refusal to bind names what
    /// this evaluation needs prepared, under the rule `purpose` decides.
    pub(super) fn evaluation(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
        purpose: Purpose,
    ) -> Result<Stack, Error> {
        let (asset, entry, drafted) = match selection {
            AnalysisSelection::Current => {
                let (asset, entry) = self.saved_entry(asset_id, None)?;
                (asset, entry, None)
            }
            // A historical entry answers from its own immutable stack, so a later commit by any
            // client never relabels what it evaluates as current.
            AnalysisSelection::Entry(entry_id) => {
                let (asset, entry) = self.saved_entry(asset_id, Some(entry_id))?;
                (asset, entry, None)
            }
            // A draft is evaluated at the revision it holds now: its effective recipe is planned
            // against the current stack and never persisted.
            AnalysisSelection::Draft(draft) => {
                let (recipe, head, entry) = self.planned_draft(asset_id, draft)?;
                validate_source_recipe(&self.registry, &head.asset, &recipe)?;
                let stamp = DraftStamp {
                    draft_id: draft.draft_id.clone(),
                    draft_revision: draft.draft_revision,
                };
                (head.asset, entry, Some((recipe, stamp)))
            }
        };
        if let Purpose::Preview {
            layer_count: Some(count),
        } = purpose
        {
            let layers = drafted
                .as_ref()
                .map_or(&entry.snapshot.recipe, |(recipe, _)| recipe)
                .layers
                .len();
            if count > layers {
                return Err(Error::validation(format!(
                    "preview layer count {count} exceeds the {layers} layers of this entry"
                )));
            }
        }
        // A draft's effective recipe decides the RAW development settings too, so a drafted
        // exposure previews the value the gesture holds rather than the committed one. A drafted
        // temperature or tint the developed planes do not hold is approximated on them, and only
        // in a drafted preview: that is the one evaluation that may, because its frame is a
        // gesture's preview and is labelled so, never analysed and replaced by the exact frame once
        // the release redevelops. Every other evaluation is strict. So a drafted preview that finds
        // no development at all names the one its entry holds, which the gesture's release
        // redevelops from anyway, rather than one per drafted value.
        let rule = match (purpose, &drafted) {
            (Purpose::Preview { .. }, Some(_)) => RawSettingsMode::DraftPreview,
            _ => RawSettingsMode::Strict,
        };
        let needs = |recipe| Evaluated {
            asset: &asset,
            entry_id: &entry.id,
            recipe,
            developed: match rule {
                RawSettingsMode::DraftPreview => &entry.snapshot.recipe,
                RawSettingsMode::Strict => recipe,
            },
        };
        // The evaluated stack carries the verified bytes of every artifact it references, so a
        // cache eviction never breaks it on a worker. An entry whose stack already holds them is
        // evaluated as the cache shares it, uncopied.
        let (recipe, draft) = match drafted {
            Some((mut recipe, stamp)) => {
                let bound = self.bind_artifacts(&mut recipe);
                self.needing(needs(&recipe), bound)?;
                (Some(recipe), Some(stamp))
            }
            None => match self.bound(&entry.snapshot.recipe) {
                Ok(Cow::Borrowed(_)) => (None, None),
                Ok(Cow::Owned(recipe)) => (Some(recipe), None),
                Err(error) => return self.needing(needs(&entry.snapshot.recipe), Err(error)),
            },
        };
        // The stack's one compilation. It reads the asset record's dimensions, so a stack the host
        // cannot evaluate is known without preparing the original; the source a reader prepares
        // has those dimensions ([`Self::read`]), so its renders reuse this compilation.
        let evaluated = recipe.as_ref().unwrap_or(&entry.snapshot.recipe);
        #[cfg(test)]
        self.render.note_compile();
        let compiled = self.registry.compile(asset.width, asset.height, evaluated);
        let fingerprint = asset.fingerprint.clone();
        Ok(Stack {
            asset,
            rule,
            evaluation: Evaluation {
                bound: Arc::new(Bound {
                    entry,
                    recipe,
                    registry: self.registry.clone(),
                    context: self.render.clone(),
                    fingerprint,
                    draft,
                    compiled,
                }),
                source: (),
            },
        })
    }

    /// `stack` reading the buffer it is evaluated on, chosen by the asset's source interpretation —
    /// the decoded JPEG, or the developed RAW planes with the linear settings the stack asks for
    /// under its rule — with the capture metadata read when the original was prepared. A cached
    /// source verification; no pixel is read. A RAW stack whose white balance the prepared planes
    /// do not hold is `preparation-required` unless its rule approximates it, and every refusal
    /// names what `stack` needs.
    fn read(&self, stack: Stack) -> Result<(Evaluation, Arc<CaptureMetadata>), Error> {
        let needs = stack.needs();
        let prepared = self.needing(needs, self.verified_prepared(&stack.asset))?;
        let capture = capture_of(&prepared);
        let source = self.needing(
            needs,
            source_of(prepared, stack.evaluation.recipe(), stack.rule),
        )?;
        // The compilation was made against the asset record's content stage and is rendered
        // against this buffer, so the two must be one stage of one original.
        if source.dimensions() != (stack.asset.width, stack.asset.height)
            || source.fingerprint() != stack.asset.fingerprint
        {
            return Err(Error::internal(
                "the prepared source is not the original the asset records",
            ));
        }
        Ok((stack.evaluation.reading(source), capture))
    }

    /// [`Self::evaluation`] of `selection` under the strict rule, reading its source.
    fn exact_evaluation(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
    ) -> Result<Evaluation, Error> {
        let stack = self.evaluation(asset_id, selection, Purpose::Exact)?;
        Ok(self.read(stack)?.0)
    }

    pub fn render_current(&self, asset_id: &AssetId) -> Result<Raster, Error> {
        self.render_selected(asset_id, AnalysisSelection::Current)
    }

    /// A preview job for one entry, or for an open draft's effective recipe. `layer_count`
    /// truncates the rendered stack to its first `n` layers, which the desktop uses to show a
    /// layer's input stage while drafting it; it must not exceed the rendered stack's layer count.
    /// A draft previews the current entry, so naming a historical one beside it is refused.
    /// `proxy` offers the display bounds the frame will be shown in; the queue decides what to do
    /// with them, and this call reads no pixels either way.
    ///
    /// The job carries the stack's one compilation to its worker, which renders the exact phase
    /// from it: a stack the host cannot evaluate is not refused here but answered by that phase.
    pub fn preview_job(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
        layer_count: Option<usize>,
        draft: Option<&Draft>,
        proxy: Option<ProxyBounds>,
    ) -> Result<PreviewJob, Error> {
        let selection = match (draft, entry_id) {
            (Some(draft), Some(entry_id)) => {
                if entry_id != &self.current_entry_id(asset_id)? {
                    return Err(Error::validation(
                        "a draft previews the current entry, not a historical one",
                    ));
                }
                AnalysisSelection::Draft(draft)
            }
            (Some(draft), None) => AnalysisSelection::Draft(draft),
            (None, Some(entry_id)) => AnalysisSelection::Entry(entry_id),
            (None, None) => AnalysisSelection::Current,
        };
        let stack = self.evaluation(asset_id, selection, Purpose::Preview { layer_count })?;
        // The identity is the evaluation's, exactly as an analysis job's is, so a report the
        // preview worker produces from this frame is a cache hit for a later `analysis.request`.
        let mut job = PreviewJob::new(self.read(stack)?.0)?;
        job.layer_count = layer_count;
        // A truncated job renders a layer prefix, whose output stage a plan computed from the
        // whole stack does not describe, and the desktop shows it only as a drafting aid. It
        // therefore never has a proxy phase, whatever bounds the caller offered.
        job.proxy = proxy.filter(|_| layer_count.is_none());
        Ok(job)
    }

    /// Everything one analysis job needs, planned on the catalog owner: the identity that names the
    /// result and the evaluation to render, or why the stack has no output stage. Costs a state
    /// read, a cached source verification and an `O(layers)` plan and compile; no frame is
    /// allocated here and nothing is persisted.
    pub fn analysis_plan(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
    ) -> Result<AnalysisPlan, Error> {
        let stack = self.evaluation(asset_id, selection, Purpose::Exact)?;
        let identity = stack.evaluation.identity()?;
        // A stack the host cannot evaluate at all is reported failed without decoding or
        // developing the original: there is no frame for that job to render. Only an evaluable
        // stack reads the prepared source, and an analysis is a number, so it is never taken from
        // an approximate white balance.
        let evaluation = match stack.evaluation.compiled() {
            Err(failure) => Err(failure),
            Ok(_) => Ok(self.read(stack)?.0),
        };
        Ok(AnalysisPlan {
            identity,
            evaluation,
        })
    }

    /// What `export.plan` answers about one saved entry, the current one unless `entry_id` names
    /// another: its evaluation, which names its identity with the output stage, and where its
    /// original lives. The same `O(layers)` planning an analysis does, without the prepared source:
    /// nothing is read but the catalog, so an unprepared original is no reason to refuse. A stack
    /// the host cannot evaluate is refused with the reason it has no output stage.
    pub(crate) fn export_target(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<ExportTarget, Error> {
        let Stack {
            asset, evaluation, ..
        } = self.evaluation(asset_id, saved(entry_id), Purpose::Exact)?;
        evaluation.compiled()?;
        Ok(ExportTarget {
            evaluation,
            original: asset.locator,
        })
    }

    /// Everything one export job needs, frozen on the catalog owner: the saved entry's identity,
    /// its evaluation — the stack bound with the verified bytes of the artifacts it references, the
    /// verified prepared source evaluated exactly (a RAW development must hold the entry's own
    /// white balance) and its compilation — and the original's capture metadata. Costs what
    /// [`Self::analysis_plan`] costs; no frame is allocated here. A stack the host cannot evaluate
    /// is refused with its reason, a missing or changed original with `source-unavailable`, and an
    /// unprepared one with `preparation-required` naming its needs.
    pub(crate) fn export_plan(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<ExportPlan, Error> {
        let stack = self.evaluation(asset_id, saved(entry_id), Purpose::Exact)?;
        stack.evaluation.compiled()?;
        let identity = stack.evaluation.identity()?;
        let (evaluation, capture) = self.read(stack)?;
        Ok(ExportPlan {
            identity,
            evaluation,
            capture,
        })
    }

    /// Render one saved entry exactly, as an export does.
    pub fn render_entry(&self, asset_id: &AssetId, entry_id: &EntryId) -> Result<Raster, Error> {
        self.render_selected(asset_id, AnalysisSelection::Entry(entry_id))
    }

    /// Render one saved entry's stack exactly from its one compilation.
    fn render_selected(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
    ) -> Result<Raster, Error> {
        let evaluation = self.exact_evaluation(asset_id, selection)?;
        evaluation
            .exact(&Cancel::never())?
            .frame(evaluation.entry().snapshot.id.clone())
    }

    /// Evaluate one output pixel of a saved entry without rasterizing the image.
    pub fn sample_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        x: u32,
        y: u32,
    ) -> Result<PixelSample, Error> {
        self.point_entry(asset_id, entry_id, x, y)?.evaluate()
    }

    /// Plan one output pixel of a saved entry for evaluation here or on another thread: a state
    /// read, a cached source verification, the entry's artifacts bound and one `O(layers)` compile
    /// that refuses a stack the host cannot evaluate now. No pixel is read.
    pub(crate) fn point_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        x: u32,
        y: u32,
    ) -> Result<PointPlan, Error> {
        let evaluation = self.exact_evaluation(asset_id, AnalysisSelection::Entry(entry_id))?;
        PointPlan::new(evaluation, x, y)
    }

    /// Bind the asset's current entry for sampling off the catalog owner: its evaluation, compiled
    /// once here so a stack the host cannot evaluate is refused now. It binds the stack like
    /// [`Self::sample_entry`], so an unprepared source or artifact is `preparation-required`. A
    /// state read, a cached source verification and an `O(layers)` compile; no pixel is read.
    pub(crate) fn sample_plan(&self, asset_id: &AssetId) -> Result<SamplePlan, Error> {
        // Samples are numbers sent to a provider, so a white balance the planes do not hold is
        // `preparation-required` here, as it is for `render.sample`.
        let evaluation = self.exact_evaluation(asset_id, AnalysisSelection::Current)?;
        evaluation.compiled()?;
        Ok(SamplePlan { evaluation })
    }

    /// One output pixel of an open draft's effective recipe, evaluated the same way: the draft's
    /// action is planned against the current stack and the resulting recipe answers the point. No
    /// frame is rasterized and nothing is persisted.
    pub fn sample_draft(
        &self,
        asset_id: &AssetId,
        draft: &Draft,
        x: u32,
        y: u32,
    ) -> Result<PixelSample, Error> {
        self.point_draft(asset_id, draft, x, y)?.evaluate()
    }

    /// Plan one output pixel of an open draft's effective recipe, as [`Self::point_entry`] plans a
    /// saved entry's: the draft's action is planned against the current stack at the revision the
    /// draft holds now, and the plan names the current entry and that revision.
    pub(crate) fn point_draft(
        &self,
        asset_id: &AssetId,
        draft: &Draft,
        x: u32,
        y: u32,
    ) -> Result<PointPlan, Error> {
        // A sampled code is a number, so a drafted white balance the planes do not hold is
        // `preparation-required` here even while the draft's preview approximates it, and the
        // refusal names a development at the drafted white balance.
        let evaluation = self.exact_evaluation(asset_id, AnalysisSelection::Draft(draft))?;
        PointPlan::new(evaluation, x, y)
    }

    /// Map one output pixel of a saved entry back to the pixel of the content stage it shows: the
    /// source after EXIF orientation, which is the stage a pixel-stage edit addresses. Like
    /// `sample_entry` it answers from the compiled stack and rasterizes nothing, and it reads no
    /// source.
    pub fn locate_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        x: u32,
        y: u32,
    ) -> Result<ContentPoint, Error> {
        let stack =
            self.evaluation(asset_id, AnalysisSelection::Entry(entry_id), Purpose::Exact)?;
        locate(
            stack.evaluation.compiled()?,
            stack.asset.width,
            stack.asset.height,
            x,
            y,
        )
    }

    /// The content-to-output affine of a saved entry's geometry tail, both ways. `locate_entry`
    /// answers one point; this answers all of them at once, so a gesture over the photograph maps
    /// pointer positions itself instead of asking per move. Like `locate_entry` it reads the compiled
    /// stack only and rasterizes nothing.
    pub fn transform_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
    ) -> Result<StageTransform, Error> {
        let stack =
            self.evaluation(asset_id, AnalysisSelection::Entry(entry_id), Purpose::Exact)?;
        transform_of(
            stack.evaluation.compiled()?,
            stack.asset.width,
            stack.asset.height,
        )
    }
}

/// The buffer a prepared source is evaluated on under `mode`: the decoded JPEG, or the developed
/// RAW planes with the linear settings `recipe` asks for ([`raw_settings`]). `O(1)`; it reads no
/// pixel.
fn source_of(
    prepared: PreparedSource,
    recipe: &Recipe,
    mode: RawSettingsMode,
) -> Result<PreviewSource, Error> {
    match prepared {
        PreparedSource::Jpeg(image) => Ok(PreviewSource::Jpeg(image)),
        PreparedSource::Raw(raw) => {
            let settings = raw_settings(&raw, recipe, mode)?;
            Ok(PreviewSource::Raw {
                image: raw
                    .linear
                    .ok_or_else(|| Error::preparation_required("RAW development required"))?,
                settings,
            })
        }
    }
}

/// One output pixel planned on the catalog owner and evaluated wherever its caller chooses: the
/// evaluation it reads, whose entry, snapshot and draft revision the answer names whatever is
/// committed before it is evaluated. It shares the evaluation and owns nothing that scales with the
/// image.
pub(crate) struct PointPlan {
    evaluation: Evaluation,
    /// Whether the point evaluates a spatial tile: the only point that costs more than
    /// `O(layers)`, and the one the catalog owner hands to its point worker.
    spatial: bool,
    x: u32,
    y: u32,
}

impl PointPlan {
    /// The point `(x, y)` of `evaluation`, refused now when no evaluation of it could answer: a
    /// stack the host cannot compile, or a source the stack cannot be rendered against.
    fn new(evaluation: Evaluation, x: u32, y: u32) -> Result<Self, Error> {
        let spatial = evaluation.exact(&Cancel::never())?.evaluates_spatial();
        Ok(Self {
            evaluation,
            spatial,
            x,
            y,
        })
    }

    /// Whether evaluating this point evaluates a spatial tile, the declared exception to point
    /// queries costing `O(layers)`.
    pub(crate) fn evaluates_spatial(&self) -> bool {
        self.spatial
    }

    /// Evaluate the point from the plan's own compilation: `O(layers)`, and through a spatial
    /// layer the one tile that contains it, whose byte is the byte a render writes there. A point
    /// outside the rendered image is a validation error naming the stage it missed.
    pub(crate) fn evaluate(self) -> Result<PixelSample, Error> {
        let Self {
            evaluation, x, y, ..
        } = self;
        let sampled = evaluation.exact(&Cancel::never())?.sample(x, y)?;
        let rgba = sampled.rgba.ok_or_else(|| {
            Error::validation(format!(
                "sample ({x}, {y}) is outside the {}x{} rendered image",
                sampled.width, sampled.height
            ))
        })?;
        let entry = evaluation.entry();
        Ok(PixelSample {
            entry_id: entry.id.clone(),
            snapshot_id: entry.snapshot.id.clone(),
            source_fingerprint: evaluation.source().fingerprint().to_owned(),
            width: sampled.width,
            height: sampled.height,
            x,
            y,
            rgba,
            draft: evaluation.draft().cloned(),
        })
    }
}

/// A saved entry of an asset: the named one, or its current entry.
fn saved(entry_id: Option<&EntryId>) -> AnalysisSelection<'_> {
    match entry_id {
        Some(entry_id) => AnalysisSelection::Entry(entry_id),
        None => AnalysisSelection::Current,
    }
}

/// The capture metadata the source worker read from the original when it prepared it.
fn capture_of(prepared: &PreparedSource) -> Arc<CaptureMetadata> {
    match prepared {
        PreparedSource::Jpeg(image) => image.capture.clone(),
        PreparedSource::Raw(raw) => raw.capture.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewQueue;
    use crate::editor::test_support::{
        SHRINK_ACTION, ShrinkModule, fixture, mutation, shrink, temp,
    };

    #[test]
    fn a_truncated_preview_job_renders_the_layer_prefix_and_rejects_an_out_of_range_count() {
        let catalog = temp("truncated-preview.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.render_current(&asset).unwrap().pixel(0, 0).unwrap();
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 0, 0, [1, 2, 3])
            .unwrap();
        service
            .apply_action(
                &asset,
                mutation(1, "shrink"),
                SHRINK_ACTION,
                shrink(100, 100),
            )
            .unwrap();
        let rendered = |service: &EditorService, layer_count: Option<usize>| -> Raster {
            let job = service
                .preview_job(&asset, None, layer_count, None, None)
                .unwrap();
            let mut queue = PreviewQueue::default();
            queue.request(job);
            luxforge_testbase::wait_for("the preview worker's answer", || queue.poll())
                .into_raster()
                .unwrap()
        };
        let full = rendered(&service, None);
        assert_eq!((full.width, full.height), (100, 100));
        let prefix = rendered(&service, Some(1));
        assert_eq!(
            (prefix.width, prefix.height),
            (480, 320),
            "one layer renders the crop's input stage"
        );
        assert_eq!(prefix.pixel(0, 0), Some([1, 2, 3, 255]));
        let none = rendered(&service, Some(0));
        assert_eq!((none.width, none.height), (480, 320));
        assert_eq!(none.pixel(0, 0), Some(original), "no layer, no edit");
        assert_ne!(none.pixel(0, 0), prefix.pixel(0, 0));
        let error = service
            .preview_job(&asset, None, Some(3), None, None)
            .expect_err("an out-of-range layer count");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .contains("preview layer count 3 exceeds the 2 layers"),
            "{error}"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A drafted preview job compiles its stack once, on the catalog owner, and its worker renders
    /// that compilation: the whole frame, and a viewport region that declines to the whole-frame
    /// path, compile nothing more. Planning the draft's action asks its lazy stage context for no
    /// stage, so the evaluation's compile of the drafted stack is the only one the owner makes. The
    /// frame is byte for byte the one a fresh compilation of the same stack renders.
    #[test]
    fn a_drafted_preview_job_compiles_its_stack_once_and_its_worker_compiles_nothing() {
        let catalog = temp("drafted-preview-compiles.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let mut draft = Draft::new(
            "set-basic",
            asset.clone(),
            service.revision(&asset).unwrap(),
        );
        draft.merge(serde_json::Map::from_iter([(
            "exposure".to_owned(),
            serde_json::json!(0.5),
        )]));
        let context = service.render_context().clone();
        let before = context.compiles();
        crate::modules::stack_compiles::take();
        let job = service
            .preview_job(&asset, None, None, Some(&draft), None)
            .unwrap();
        assert_eq!(
            crate::modules::stack_compiles::take(),
            1,
            "the owner compiled the drafted stack once, for its evaluation, and nothing to plan it"
        );
        assert_eq!(
            context.compiles() - before,
            1,
            "the evaluated stack was compiled once"
        );
        let evaluation = &job.evaluation;
        let reference = crate::render(
            evaluation.registry(),
            evaluation.source(),
            evaluation.recipe(),
            RenderOptions::default(),
            &RenderContext::new(),
        )
        .unwrap()
        .frame(evaluation.entry().snapshot.id.clone())
        .unwrap();
        // A region wholly outside the stage clips to nothing, which declines to the whole frame.
        let outside = crate::Region {
            x0: 10_000,
            y0: 10_000,
            width: 4,
            height: 4,
        };
        let mut queue = PreviewQueue::default();
        for viewport in [None, Some(outside)] {
            let mut job = job.clone();
            job.viewport = viewport;
            job.intent = crate::PreviewIntent::Settle;
            queue.request(job);
            let result = luxforge_testbase::wait_for("the preview worker's exact answer", || {
                queue.poll().filter(|result| result.exact().is_some())
            });
            assert_eq!(result.viewport_declined.is_some(), viewport.is_some());
            let frame = result.into_raster().unwrap();
            assert_eq!(frame.rgba.as_ref(), reference.rgba.as_ref(), "{viewport:?}");
        }
        assert_eq!(
            context.compiles() - before,
            1,
            "the worker rendered the owner's compilation, the region's fallback included"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// With the original not prepared, as on the catalog owner after a reopen: a refusal names the
    /// stack that was evaluated — a historical entry's sample names that entry, a draft's planning
    /// and a sampling commit name the current one — and a plan that samples nothing needs nothing.
    #[test]
    fn a_refusal_names_the_entry_it_evaluated_and_a_plan_that_samples_nothing_needs_nothing() {
        let catalog = temp("preparation-needs.sqlite");
        let asset = {
            let mut service = EditorService::open(&catalog).unwrap();
            let asset = service.import(&fixture()).unwrap().asset.id;
            service
                .apply_action(
                    &asset,
                    mutation(0, "exposure"),
                    "set-basic",
                    serde_json::json!({"exposure": 0.5}),
                )
                .unwrap();
            asset
        };
        let mut service = EditorService::open(&catalog).unwrap();
        service.disable_sync_source();
        let state = service.state(&asset).unwrap();
        let original = service.history(&asset, None, 10).unwrap().entries[1].clone();
        assert_eq!(original.action_id, "original");
        let needs = |entry_id: &EntryId| crate::PreparationNeeds {
            asset_id: asset.clone(),
            entry_id: entry_id.clone(),
            gains: None,
            artifacts: Vec::new(),
        };

        let refused = service
            .sample_entry(&asset, &original.id, 0, 0)
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::PreparationRequired);
        assert_eq!(refused.needs(), Some(&needs(&original.id)));

        // A pixel replacement compares the pixel it would replace, so its plan samples.
        let refused = service
            .apply_pixel(&asset, mutation(1, "pixel"), 0, 0, [1, 2, 3])
            .unwrap_err();
        assert_eq!(refused.needs(), Some(&needs(&state.current_entry.id)));
        let mut draft = Draft::new("set-pixel", asset.clone(), 1);
        draft.merge(serde_json::Map::from_iter([
            ("x".to_owned(), serde_json::json!(0)),
            ("y".to_owned(), serde_json::json!(0)),
            ("rgb".to_owned(), serde_json::json!([1, 2, 3])),
        ]));
        let refused = service.draft_recipe(&asset, &draft).unwrap_err();
        assert_eq!(refused.needs(), Some(&needs(&state.current_entry.id)));

        // A Basic patch and a transform read no pixel, so they commit without the original.
        service
            .apply_action(
                &asset,
                mutation(1, "contrast"),
                "set-basic",
                serde_json::json!({"contrast": 10}),
            )
            .expect("a field patch plans without the original");
        service
            .apply_transform(&asset, mutation(2, "turn"), crate::Transform::RotateRight)
            .expect("a transform plans without the original");
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
