use super::{
    AnalysisPlan, AnalysisSelection, AssetRecord, DraftStamp, EditorService, EditorState,
    PixelSample, SamplePlan,
    source::{Evaluated, RawSettingsMode, raw_settings, validate_source_recipe},
};
use crate::{
    AssetId, ContentPoint, Draft, EntryId, Error, ErrorKind, HistoryEntry, PreviewJob,
    PreviewSource, ProxyBounds, Raster, Recipe, RenderOptions, StageTransform,
    analysis::AnalysisIdentity,
    render::{locate_dimensions, stage_transform},
    source::PreparedSource,
};

impl EditorService {
    pub fn render_current(&self, asset_id: &AssetId) -> Result<Raster, Error> {
        let state = self.state(asset_id)?;
        self.render_entry(asset_id, &state.current_entry.id)
    }

    /// A preview job for one entry, or for an open draft's effective recipe. `layer_count`
    /// truncates the rendered stack to its first `n` layers, which the desktop uses to show a
    /// layer's input stage while drafting it; it must not exceed the rendered stack's layer count.
    /// A draft previews the current entry, so naming a historical one beside it is refused.
    /// `proxy` offers the display bounds the frame will be shown in; the queue decides what to do
    /// with them, and this call reads no pixels either way.
    pub fn preview_job(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
        layer_count: Option<usize>,
        draft: Option<&Draft>,
        proxy: Option<ProxyBounds>,
    ) -> Result<PreviewJob, Error> {
        let state = self.state(asset_id)?;
        if draft.is_some() && entry_id.is_some_and(|entry_id| entry_id != &state.current_entry.id) {
            return Err(Error::new(
                ErrorKind::Validation,
                "a draft previews the current entry, not a historical one",
            ));
        }
        let entry = match entry_id {
            Some(entry_id) => self.entry(asset_id, entry_id)?,
            None => state.current_entry.clone(),
        };
        // The draft's effective recipe is planned, not persisted, and costs point queries only.
        let (mut recipe, draft_revision) = match draft {
            Some(draft) => {
                let (recipe, _) = self.draft_recipe(asset_id, draft)?;
                (recipe, Some(draft.draft_revision))
            }
            None => (entry.snapshot.recipe.clone(), None),
        };
        let layers = recipe.layers.len();
        if let Some(count) = layer_count.filter(|count| *count > layers) {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("preview layer count {count} exceeds the {layers} layers of this entry"),
            ));
        }
        // A draft's effective recipe decides the RAW development settings too, so a drafted
        // exposure previews the value the gesture holds rather than the committed one. A drafted
        // temperature or tint the developed planes do not hold is approximated on them, and only
        // here: this is the one evaluation that may, because its frame is a gesture's preview and
        // is labelled so, never analysed and replaced by the exact frame once the release
        // redevelops. A preview without a draft is strict, as every other evaluation is. So a
        // drafted preview that finds no development at all names the one its entry holds, which
        // the gesture's release redevelops from anyway, rather than one per drafted value.
        let mode = if draft.is_some() {
            RawSettingsMode::DraftPreview
        } else {
            RawSettingsMode::Strict
        };
        let result = self.bound_source(&state.asset, &mut recipe, mode);
        let stack = Evaluated {
            asset: &state.asset,
            entry_id: &entry.id,
            recipe: &recipe,
            developed: match draft {
                Some(_) => &entry.snapshot.recipe,
                None => &recipe,
            },
        };
        let source = self.needing(stack, result)?;
        // The identity is computed exactly as an analysis job's is, so a report the preview worker
        // produces from this frame is a cache hit for a later `analysis.request`.
        let draft_stamp = draft.map(|draft| DraftStamp {
            draft_id: draft.draft_id.clone(),
            draft_revision: draft.draft_revision,
        });
        let (identity, _) = self.analysis_identity(
            asset_id,
            source.fingerprint(),
            source.dimensions(),
            &entry,
            &recipe,
            draft_stamp,
        )?;
        Ok(PreviewJob {
            source,
            entry,
            registry: self.registry.clone(),
            context: self.render.clone(),
            recipe,
            layer_count,
            draft_revision,
            identity,
            analyse: false,
            // A truncated job renders a layer prefix, whose output stage a plan computed from the
            // whole stack does not describe, and the desktop shows it only as a drafting aid. It
            // therefore never has a proxy phase, whatever bounds the caller offered.
            proxy: proxy.filter(|_| layer_count.is_none()),
            // A coverage grid is asked for by the client that will draw it, through
            // `PreviewJob::with_mask_overlay`, which validates it against this stack.
            mask_overlay: None,
        })
    }

    /// The identity of the analysis of one evaluated stack, and the reason that stack has no output
    /// stage when the host cannot compile it. `O(layers)`: it compiles the stack to learn its output
    /// dimensions and hashes the recipe, and it reads no pixels and rasterizes nothing, so the
    /// catalog owner may call it while building a job. A stack whose artifacts are missing or not
    /// prepared is an error rather than a stack without an output stage: it is not unevaluable,
    /// only not evaluable yet. A recipe its caller already bound is compiled as it is.
    pub fn analysis_identity(
        &self,
        asset_id: &AssetId,
        source_fingerprint: &str,
        source_dimensions: (u32, u32),
        entry: &HistoryEntry,
        recipe: &Recipe,
        draft: Option<DraftStamp>,
    ) -> Result<(AnalysisIdentity, Option<Error>), Error> {
        let bound = self.bound(recipe)?;
        let stage = self
            .registry
            .compile(source_dimensions.0, source_dimensions.1, &bound)
            .map(|compiled| {
                let stage = compiled.stage();
                (stage.width, stage.height)
            });
        let failure = stage.as_ref().err().cloned();
        let identity = AnalysisIdentity::of(
            asset_id,
            source_fingerprint,
            entry,
            recipe,
            draft,
            stage.ok(),
        )?;
        Ok((identity, failure))
    }

    /// Bind the artifacts `recipe` references into it and resolve the buffer it is evaluated on
    /// ([`Self::preview_source`]). The job's recipe then carries the artifacts' verified bytes to
    /// its worker, so a cache eviction never breaks it there.
    fn bound_source(
        &self,
        asset: &AssetRecord,
        recipe: &mut Recipe,
        mode: RawSettingsMode,
    ) -> Result<PreviewSource, Error> {
        self.bind_artifacts(recipe)?;
        self.preview_source(asset, recipe, mode)
    }

    /// The immutable buffer a preview or an analysis worker renders, chosen by the asset's source
    /// interpretation: the decoded JPEG, or the developed RAW mosaic with the linear settings the
    /// given recipe asks for. A RAW stack whose white balance the prepared image does not hold
    /// reports `preparation-required` rather than rendering a stale development, except under
    /// [`RawSettingsMode::DraftPreview`], where the settings approximate it and the source says so
    /// ([`PreviewSource::approximate_white_balance`]).
    fn preview_source(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        mode: RawSettingsMode,
    ) -> Result<PreviewSource, Error> {
        match self.verified_prepared(asset)? {
            PreparedSource::Jpeg(image) => {
                validate_source_recipe(asset, recipe)?;
                Ok(PreviewSource::Jpeg(image))
            }
            PreparedSource::Raw(raw) => {
                let settings = raw_settings(&raw, recipe, mode)?;
                Ok(PreviewSource::Raw {
                    image: raw.linear.ok_or_else(|| {
                        Error::new(ErrorKind::PreparationRequired, "RAW development required")
                    })?,
                    settings,
                })
            }
        }
    }

    /// Everything one analysis job needs, planned on the catalog owner: the identity that names the
    /// result, the cached verified source, the shared registry and the effective recipe to render.
    /// Costs a state read, a cached source verification and an `O(layers)` plan and compile; no
    /// frame is allocated here and nothing is persisted.
    pub fn analysis_plan(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
    ) -> Result<AnalysisPlan, Error> {
        let state = self.state(asset_id)?;
        let (entry, mut recipe, draft) = match selection {
            AnalysisSelection::Current => {
                let entry = state.current_entry.clone();
                let recipe = entry.snapshot.recipe.clone();
                (entry, recipe, None)
            }
            // A historical entry answers from its own immutable stack, so a later commit by any
            // client never relabels this result as current.
            AnalysisSelection::Entry(entry_id) => {
                let entry = self.entry(asset_id, entry_id)?;
                let recipe = entry.snapshot.recipe.clone();
                (entry, recipe, None)
            }
            // A draft is evaluated at the revision it holds now: its effective recipe is planned
            // against the current stack and never persisted.
            AnalysisSelection::Draft(draft) => {
                let (recipe, drafted) = self.draft_recipe(asset_id, draft)?;
                let stamp = DraftStamp {
                    draft_id: draft.draft_id.clone(),
                    draft_revision: draft.draft_revision,
                };
                (drafted.current_entry, recipe, Some(stamp))
            }
        };
        // The job's recipe carries the verified bytes of every artifact it references, so a cache
        // eviction never breaks it on the worker.
        let bound = self.bind_artifacts(&mut recipe);
        let stack = Evaluated::exactly(&state.asset, &entry.id, &recipe);
        self.needing(stack, bound)?;
        // The identity and the output stage come from the asset record, so a stack the host cannot
        // evaluate at all is reported failed without decoding or developing the original: there is
        // no frame for that job to render. Only an evaluable stack asks for the prepared source.
        let (identity, failure) = self.analysis_identity(
            asset_id,
            &state.asset.fingerprint,
            (state.asset.width, state.asset.height),
            &entry,
            &recipe,
            draft,
        )?;
        // An analysis is a number, so it is never taken from an approximate white balance.
        let source = match failure {
            Some(_) => None,
            None => Some(self.needing(
                stack,
                self.preview_source(&state.asset, &recipe, RawSettingsMode::Strict),
            )?),
        };
        Ok(AnalysisPlan {
            identity,
            source,
            registry: self.registry.clone(),
            context: self.render.clone(),
            recipe,
            failure,
        })
    }

    /// Render one saved entry exactly, as an export does.
    pub fn render_entry(&self, asset_id: &AssetId, entry_id: &EntryId) -> Result<Raster, Error> {
        let state = self.state(asset_id)?;
        let (entry, source) = self.exact_entry(&state, entry_id)?;
        crate::render(
            &self.registry,
            &source,
            &entry.snapshot.recipe,
            RenderOptions::default(),
            &self.render,
        )?
        .frame(entry.snapshot.id.clone())
    }

    /// One saved entry of an asset, bound, with the buffer it is evaluated on exactly: a RAW
    /// development must hold its own white balance. A refusal names everything that entry needs.
    fn exact_entry(
        &self,
        state: &EditorState,
        entry_id: &EntryId,
    ) -> Result<(HistoryEntry, PreviewSource), Error> {
        let mut entry = self.entry(&state.asset.id, entry_id)?;
        let result = self.bound_source(
            &state.asset,
            &mut entry.snapshot.recipe,
            RawSettingsMode::Strict,
        );
        let stack = Evaluated::exactly(&state.asset, &entry.id, &entry.snapshot.recipe);
        let source = self.needing(stack, result)?;
        Ok((entry, source))
    }

    /// Evaluate one output pixel of a saved entry without rasterizing the image.
    pub fn sample_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        x: u32,
        y: u32,
    ) -> Result<PixelSample, Error> {
        let state = self.state(asset_id)?;
        let (entry, source) = self.exact_entry(&state, entry_id)?;
        let sampled = self.sample_point(&source, &entry.snapshot.recipe, x, y)?;
        pixel_sample(entry, &state.asset.fingerprint, sampled, x, y, None)
    }

    /// Bind the asset's current entry for sampling off the catalog owner: its verified source and
    /// its recipe, bound with the verified bytes of every artifact it references and compiled once
    /// here so a stack the host cannot evaluate is refused now. It binds the stack like
    /// [`Self::sample_entry`], so an unprepared source or artifact is `preparation-required`. A
    /// state read, a cached source verification and an `O(layers)` compile; no pixel is read.
    pub(crate) fn sample_plan(&self, asset_id: &AssetId) -> Result<SamplePlan, Error> {
        let state = self.state(asset_id)?;
        // Samples are numbers sent to a provider, so a white balance the planes do not hold is
        // `preparation-required` here, as it is for `render.sample`.
        let (entry, source) = self.exact_entry(&state, &state.current_entry.id)?;
        let recipe = entry.snapshot.recipe;
        let (width, height) = source.dimensions();
        self.registry.compile(width, height, &recipe)?;
        Ok(SamplePlan {
            source,
            registry: self.registry.clone(),
            context: self.render.clone(),
            recipe,
        })
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
        let (mut recipe, state) = self.draft_recipe(asset_id, draft)?;
        // A sampled code is a number, so a drafted white balance the planes do not hold is
        // `preparation-required` here even while the draft's preview approximates it, and the
        // refusal names a development at the drafted white balance.
        let result = self.bound_source(&state.asset, &mut recipe, RawSettingsMode::Strict);
        let stack = Evaluated::exactly(&state.asset, &state.current_entry.id, &recipe);
        let source = self.needing(stack, result)?;
        let sampled = self.sample_point(&source, &recipe, x, y)?;
        let fingerprint = state.asset.fingerprint.clone();
        pixel_sample(
            state.current_entry,
            &fingerprint,
            sampled,
            x,
            y,
            Some(DraftStamp {
                draft_id: draft.draft_id.clone(),
                draft_revision: draft.draft_revision,
            }),
        )
    }

    /// One output pixel of `recipe` over `source`, through the one render entry point: no frame.
    fn sample_point(
        &self,
        source: &PreviewSource,
        recipe: &Recipe,
        x: u32,
        y: u32,
    ) -> Result<crate::Sample, Error> {
        crate::render(
            &self.registry,
            source,
            recipe,
            RenderOptions::default(),
            &self.render,
        )?
        .sample(x, y)
    }

    /// Map one output pixel of a saved entry back to the pixel of the content stage it shows: the
    /// source after EXIF orientation, which is the stage a pixel-stage edit addresses. Like
    /// `sample_entry` it answers from the compiled stack and rasterizes nothing.
    pub fn locate_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        x: u32,
        y: u32,
    ) -> Result<ContentPoint, Error> {
        let state = self.state(asset_id)?;
        let entry = self.bound_entry(&state, entry_id)?;
        locate_dimensions(
            &self.registry,
            state.asset.width,
            state.asset.height,
            &entry.snapshot.recipe,
            x,
            y,
        )
    }

    /// One saved entry of an asset with its artifacts bound, for a question its compiled geometry
    /// answers without the original.
    fn bound_entry(&self, state: &EditorState, entry_id: &EntryId) -> Result<HistoryEntry, Error> {
        let mut entry = self.entry(&state.asset.id, entry_id)?;
        validate_source_recipe(&state.asset, &entry.snapshot.recipe)?;
        let bound = self.bind_artifacts(&mut entry.snapshot.recipe);
        let stack = Evaluated::exactly(&state.asset, &entry.id, &entry.snapshot.recipe);
        self.needing(stack, bound)?;
        Ok(entry)
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
        let state = self.state(asset_id)?;
        let entry = self.bound_entry(&state, entry_id)?;
        stage_transform(
            &self.registry,
            state.asset.width,
            state.asset.height,
            &entry.snapshot.recipe,
        )
    }
}

/// One evaluated point with the identities that produced it. A point outside the rendered image is
/// a validation error naming the stage it missed.
fn pixel_sample(
    entry: HistoryEntry,
    source_fingerprint: &str,
    sampled: crate::Sample,
    x: u32,
    y: u32,
    draft: Option<DraftStamp>,
) -> Result<PixelSample, Error> {
    let rgba = sampled.rgba.ok_or_else(|| {
        Error::new(
            ErrorKind::Validation,
            format!(
                "sample ({x}, {y}) is outside the {}x{} rendered image",
                sampled.width, sampled.height
            ),
        )
    })?;
    Ok(PixelSample {
        entry_id: entry.id,
        snapshot_id: entry.snapshot.id,
        source_fingerprint: source_fingerprint.to_owned(),
        width: sampled.width,
        height: sampled.height,
        x,
        y,
        rgba,
        draft,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewQueue;
    use crate::editor::test_support::{
        SHRINK_ACTION, ShrinkModule, fixture, mutation, shrink, temp,
    };
    use std::time::{Duration, Instant};

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
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(result) = queue.poll() {
                    return result.result.unwrap();
                }
                assert!(Instant::now() < deadline, "the preview worker answered");
                std::thread::yield_now();
            }
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
