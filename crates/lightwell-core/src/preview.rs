use crate::{
    EntryId, Error, HistoryEntry, ModuleRegistry, Raster, Recipe, SourceImage,
    analysis::{AnalysisIdentity, Report},
    render,
};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    mpsc::{Receiver, TryRecvError, sync_channel},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HistorySelection {
    Current,
    Entry(EntryId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum Zoom {
    Fit,
    Percent { value: f32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewState {
    pub zoom: Zoom,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: Zoom::Fit,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

impl ViewState {
    pub fn set_zoom(&mut self, zoom: Zoom) -> Result<(), crate::Error> {
        if let Zoom::Percent { value } = zoom
            && (!value.is_finite() || !(10.0..=1600.0).contains(&value))
        {
            return Err(crate::Error::new(
                crate::ErrorKind::Validation,
                "zoom percent must be finite and within 10..=1600",
            ));
        }
        self.zoom = zoom;
        Ok(())
    }
    pub fn pan_to(&mut self, x: f32, y: f32) -> Result<(), crate::Error> {
        if !x.is_finite() || !y.is_finite() {
            return Err(crate::Error::new(
                crate::ErrorKind::Validation,
                "pan coordinates must be finite",
            ));
        }
        self.pan_x = x;
        self.pan_y = y;
        Ok(())
    }
    pub fn source_detail_required(&self) -> bool {
        matches!(self.zoom, Zoom::Percent { value } if value >= 100.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewSession {
    pub selection: HistorySelection,
    pub view: ViewState,
    pub generation: u64,
}

impl Default for PreviewSession {
    fn default() -> Self {
        Self {
            selection: HistorySelection::Current,
            view: ViewState::default(),
            generation: 0,
        }
    }
}

impl PreviewSession {
    pub fn select(&mut self, selection: HistorySelection) -> u64 {
        self.selection = selection;
        self.generation = self.generation.saturating_add(1);
        self.generation
    }
    pub fn return_current(&mut self) -> u64 {
        self.select(HistorySelection::Current)
    }
    pub fn can_edit(&self) -> bool {
        self.selection == HistorySelection::Current
    }
}

#[derive(Clone, Debug)]
pub struct PreviewJob {
    pub source: SourceImage,
    /// The entry this preview shows. Its identity and snapshot correlate the frame with history;
    /// what is rendered is [`PreviewJob::recipe`], which differs from the entry's own stack while a
    /// draft is open.
    pub entry: HistoryEntry,
    /// The providers the worker evaluates this stack with; shared, never rebuilt per job.
    pub registry: Arc<ModuleRegistry>,
    /// The stack to render: the entry's own recipe, or an open draft's effective recipe.
    pub recipe: Recipe,
    /// `Some(n)` renders only the first `n` layers of that recipe, which is how the desktop shows
    /// the input stage of the layer it is drafting. `None` renders the whole stack.
    pub layer_count: Option<usize>,
    /// The draft revision this recipe was planned from, for correlating a frame with the settings
    /// that produced it. `None` when no draft was involved.
    pub draft_revision: Option<u64>,
    /// Which evaluated image this frame is, computed exactly as an analysis job's identity is. It
    /// describes the whole planned stack, so a truncated job's identity is the stack it was planned
    /// from, not the prefix it renders — which is why a truncated job is never analysed.
    pub identity: AnalysisIdentity,
    /// Reduce the rendered raster into a [`Report`] and return it with the frame, so the displayed
    /// target needs no second render. Refused together with [`PreviewJob::layer_count`].
    pub analyse: bool,
}

#[derive(Debug)]
pub struct PreviewResult {
    pub generation: u64,
    pub entry_id: EntryId,
    /// The identity of the job that produced this frame, so the desktop can submit the report under
    /// the identity a later `analysis.request` will look up.
    pub identity: AnalysisIdentity,
    pub result: Result<Raster, Error>,
    /// The exact reduction of the raster in `result`, when the job asked for it. `None` means the
    /// job did not ask, or the render failed; it never means an empty histogram.
    pub report: Option<Report>,
}

struct Active {
    generation: u64,
    receiver: Receiver<PreviewResult>,
}

#[derive(Default)]
pub struct PreviewQueue {
    generation: u64,
    active: Option<Active>,
    pending: Option<(u64, PreviewJob)>,
}

impl PreviewQueue {
    pub fn request(&mut self, job: PreviewJob) -> u64 {
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        if self.active.is_some() {
            self.pending = Some((generation, job));
        } else {
            self.start(generation, job);
        }
        generation
    }

    pub fn cancel(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.pending = None;
        self.generation
    }

    fn start(&mut self, generation: u64, job: PreviewJob) {
        let (sender, receiver) = sync_channel(1);
        std::thread::spawn(move || {
            let entry_id = job.entry.id.clone();
            // A truncated job copies the layer prefix only; the whole stack is rendered in place.
            let prefix = job.layer_count.map(|count| Recipe {
                format: job.recipe.format,
                layers: job.recipe.layers.iter().take(count).cloned().collect(),
            });
            let result = render(
                &job.registry,
                &job.source,
                job.entry.snapshot.id.clone(),
                prefix.as_ref().unwrap_or(&job.recipe),
            );
            // The histogram is reduced from the frame this worker just produced, in place and
            // without a second render or a copy. A failed reduction leaves no report rather than
            // reporting zeroes.
            let report = match (job.analyse, &result) {
                (true, Ok(raster)) => crate::analysis::reduce_raster(raster).ok(),
                _ => None,
            };
            let _ = sender.send(PreviewResult {
                generation,
                entry_id,
                identity: job.identity,
                report,
                result,
            });
        });
        self.active = Some(Active {
            generation,
            receiver,
        });
    }

    pub fn is_busy(&self) -> bool {
        self.active.is_some() || self.pending.is_some()
    }

    pub fn poll(&mut self) -> Option<PreviewResult> {
        let active = self.active.as_ref()?;
        let result = match active.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.active = None;
                if let Some((generation, job)) = self.pending.take() {
                    self.start(generation, job);
                }
                return None;
            }
        };
        let was_current = active.generation == self.generation;
        self.active = None;
        if let Some((generation, job)) = self.pending.take() {
            self.start(generation, job);
        }
        if was_current { Some(result) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetId, Layer, Snapshot};
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn entry(color: u8) -> PreviewJob {
        job(color, false)
    }

    fn job(color: u8, analyse: bool) -> PreviewJob {
        let asset = AssetId::new();
        let original = Snapshot::original(asset.clone());
        let snapshot = original.append(Layer::pixel(0, 0, [color, 0, 0])).unwrap();
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset,
            sequence: u64::from(color),
            action_id: "set-pixel".into(),
            label: "Pixel 0, 0".into(),
            parameters: json!({}),
            actor: "test".into(),
            timestamp_ms: 0,
            request_id: None,
            base_revision: 0,
            result_revision: 0,
            snapshot,
            undo_parent: None,
            restore_target: None,
        };
        let recipe = entry.snapshot.recipe.clone();
        let identity = AnalysisIdentity::of(
            &entry.asset_id.clone(),
            "test",
            &entry,
            &recipe,
            None,
            Some((1, 1)),
        )
        .unwrap();
        PreviewJob {
            source: SourceImage {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255].into(),
                fingerprint: "test".into(),
                orientation: 1,
            },
            registry: Arc::new(ModuleRegistry::builtin()),
            recipe,
            layer_count: None,
            draft_revision: None,
            identity,
            analyse,
            entry,
        }
    }

    #[test]
    fn newest_preview_wins_with_one_active_and_one_pending() {
        let mut queue = PreviewQueue::default();
        queue.request(entry(1));
        queue.request(entry(2));
        let wanted = queue.request(entry(3));
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = queue.poll() {
                assert_eq!(result.generation, wanted);
                assert_eq!(result.result.unwrap().pixel(0, 0), Some([3, 0, 0, 255]));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    /// The preview worker reduces the frame it just rendered, so a displayed target needs no second
    /// render. The report must equal the reduction of that very raster, byte for byte.
    #[test]
    fn an_analysing_preview_returns_the_exact_reduction_of_the_frame_it_rendered() {
        let mut queue = PreviewQueue::default();
        let wanted = queue.request(job(9, true));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = queue.poll() {
                assert_eq!(result.generation, wanted);
                let raster = result.result.expect("a frame");
                assert_eq!(
                    result.report.expect("the job asked for a report"),
                    crate::analysis::reduce_raster(&raster).unwrap()
                );
                assert!(result.identity.has_output_stage());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the analysing preview never came"
            );
            std::thread::yield_now();
        }
        // A job that does not ask carries no report: `None` is "not asked", never empty counts.
        let wanted = queue.request(job(9, false));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = queue.poll() {
                assert_eq!(result.generation, wanted);
                assert!(result.report.is_none());
                break;
            }
            assert!(Instant::now() < deadline, "the plain preview never came");
            std::thread::yield_now();
        }
    }

    #[test]
    fn history_selection_and_view_are_read_only_validated_session_state() {
        let mut session = PreviewSession::default();
        let entry = EntryId::new();
        session.select(HistorySelection::Entry(entry.clone()));
        assert!(!session.can_edit());
        session
            .view
            .set_zoom(Zoom::Percent { value: 100.0 })
            .unwrap();
        assert!(session.view.source_detail_required());
        assert!(
            session
                .view
                .set_zoom(Zoom::Percent { value: f32::NAN })
                .is_err()
        );
        session.return_current();
        assert!(session.can_edit());
        assert_eq!(session.selection, HistorySelection::Current);
    }
}
