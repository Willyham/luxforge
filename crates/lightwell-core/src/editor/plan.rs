use super::{
    AssetRecord, EditorService, EditorState, MutationResult,
    history::{Change, CommittedAction, request_input},
    masks::{recipe_for_target, resolve_mask_target, take_mask_target},
    source::{Evaluated, RawSettingsMode, raw_settings, validate_source_recipe},
};
use crate::{
    AssetId, Draft, EntryId, Error, ErrorKind, Layer, LayerId, LinearImage, LinearSettings, MaskId,
    Mutation, Recipe, ToolModule, Transform,
    mask::commands::MaskOutcome,
    modules::{
        ActionInput, ActionPlan, LayerEdit, MAX_COMPOSE_STEPS, Stage, StageContext, StageQuestions,
        action_label, check_parameters,
    },
    render::Evaluation,
    sample_linear,
    source::PreparedSource,
};
use serde_json::{Value, json};
use std::cell::OnceCell;

impl EditorService {
    /// One action request for every caller: the desktop, the JSON API and headless clients all
    /// arrive here with an action identity and its declared parameters.
    pub fn apply_action(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        action_id: &str,
        parameters: Value,
    ) -> Result<MutationResult, Error> {
        let registry = self.registry.clone();
        let (module, action) = registry.action(action_id).ok_or_else(|| {
            Error::new(ErrorKind::Validation, format!("unknown action {action_id}"))
        })?;
        // An unavailable provider keeps its descriptor so its stored layers stay readable, but it
        // changes nothing; it is refused here, before anything else, and again by the one plan
        // path every commit, draft and query takes.
        available(module)?;
        // The host's one optional target field, taken before the action's own parameters are checked
        // so the module receives exactly its declared fields and never learns a mask was involved.
        let mut parameters = parameters;
        let mask = take_mask_target(&registry, action_id, &mut parameters)?;
        let checked = check_parameters(action, &parameters)?;
        let input = module.parse(action_id, &checked)?;
        // The module labels a request its template cannot describe, such as a field patch; the
        // fallback comes from the action that was requested, which is not always the durable action
        // identity the entry stores: `transform` renders the label, `rotate-left` is stored.
        let label = module
            .label(&input)
            .unwrap_or_else(|| action_label(action, &input.parameters));
        let request = request_input(&input, &mutation, mask.as_ref())?;
        self.mutate(asset_id, &mutation, &request, |service, state| {
            let current = &state.current_entry.snapshot.recipe;
            let planned =
                service.plan_action(&state.asset, current, module, &input, mask.as_ref())?;
            let Some(recipe) = planned else {
                return Ok(Change::NoOp);
            };
            let label = masked_label(current, mask.as_ref(), label);
            Ok(Change::append(recipe, CommittedAction { input, label }))
        })
    }

    /// What one parsed action does to `recipe`, the stack of `asset` it is planned against, or
    /// `None` when it changes nothing: its module plans through [`Self::ask`] and the host
    /// resolves the plan into the stack it produces ([`Self::resolve_plan`]).
    ///
    /// A commit and a draft's effective recipe both come here with the same parsed input and the
    /// same target, so a drafted preview evaluates exactly the stack committing that draft writes,
    /// for every module and source kind alike. Planning costs `O(layers)` plus whatever points the
    /// module samples, and rasterizes nothing.
    pub(super) fn plan_action(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        module: &dyn ToolModule,
        input: &ActionInput,
        mask: Option<&MaskId>,
    ) -> Result<Option<Recipe>, Error> {
        self.ask(asset, recipe, module, mask, |context, bound| {
            let plan = module.plan(input, context)?;
            self.resolve_plan(asset, bound, plan, mask)
        })
    }

    /// Ask `module` one question about `recipe`, the stack of `asset`, for the target `mask`
    /// names: the one path that plans an action for a commit, a draft or a composite's step, and
    /// answers a query.
    ///
    /// The module must be available; the stack must be `asset`'s kind of stack; its artifacts are
    /// bound; a mask target must be one the stack holds; and the module sees the stack of that
    /// target ([`recipe_for_target`]) through a lazy [`StageContext`]
    /// ([`Self::with_stage_context`]). `question` receives the context and the whole bound stack,
    /// which is what a plan is resolved against.
    fn ask<T>(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        module: &dyn ToolModule,
        mask: Option<&MaskId>,
        question: impl FnOnce(&StageContext<'_>, &Recipe) -> Result<T, Error>,
    ) -> Result<T, Error> {
        available(module)?;
        validate_source_recipe(asset, recipe)?;
        // Planning compiles the stack, so its artifacts are bound first.
        let bound = self.bound(recipe)?;
        // A target the stack does not hold is refused here, before a module plans anything.
        resolve_mask_target(&bound, mask)?;
        // The module plans against the stack of one target: the global layer and each mask are
        // distinct targets, so a module that owns one layer still owns one per target and finds it
        // through the context's own-layer lookup for that target.
        let maskable = module
            .descriptor()
            .effects
            .iter()
            .any(|effect| effect.maskable);
        let target = recipe_for_target(&self.registry, &bound, maskable, mask);
        self.with_stage_context(asset, &target, mask, |context| question(context, &bound))
    }

    /// Build the questions a module may ask about one stack of `asset` and hand them to `answer`.
    ///
    /// The output stage is compiled from the asset's dimensions before anything is asked, which is
    /// `O(layers)` and also refuses a stack that cannot compile. Everything else is answered only
    /// when asked ([`HostStage`]): a prefix stage compiles that prefix, and the first question that
    /// reads a pixel or the sensor resolves the verified source, and for a RAW stack its linear
    /// settings, strictly, once for the whole context. A plan that reads no pixel therefore never
    /// needs the original prepared or a RAW developed. Nothing is rasterized either way.
    pub(super) fn with_stage_context<T>(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        target: Option<&MaskId>,
        answer: impl FnOnce(&StageContext<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let stage = self
            .registry
            .compile(asset.width, asset.height, recipe)?
            .stage();
        let questions = HostStage {
            service: self,
            asset,
            recipe,
            source: OnceCell::new(),
            settings: OnceCell::new(),
        };
        answer(&StageContext {
            stage,
            layers: &recipe.layers,
            registry: &self.registry,
            target,
            questions: &questions,
        })
    }

    /// Answer one module query about a saved entry's stack: the read-only counterpart of
    /// [`EditorService::apply_action`].
    ///
    /// The query is resolved from the same registry discovery lists, its parameters go through the
    /// same generic check, and it is asked through the same [`Self::ask`] a commit is planned
    /// through. Nothing is written: no snapshot, no history entry, no request row and no event, so
    /// two clients asking the same question concurrently get the same answer and neither disturbs
    /// the other. Cost is `O(layers)` per point the module samples and no frame is allocated.
    pub fn run_query(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        query_id: &str,
        parameters: Value,
    ) -> Result<Value, Error> {
        let registry = self.registry.clone();
        let (module, query) = registry.query(query_id).ok_or_else(|| {
            Error::new(ErrorKind::Validation, format!("unknown query {query_id}"))
        })?;
        let checked = check_parameters(query, &parameters)?;
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        let recipe = &entry.snapshot.recipe;
        // A query carries no mask target, so it asks about the global layer, and the target view
        // hides the masked layers of the module's own effect. Without it a module that owns one
        // layer would refuse its own query as ambiguous as soon as a mask held a layer of that
        // effect, and the pixels it reads are unchanged: what it samples is the stage *before* its
        // own layer, and a masked layer of the same effect is always after the global one.
        let answer = self.ask(&state.asset, recipe, module, None, |context, _| {
            module.query(query_id, &checked, context)
        });
        self.needing(Evaluated::exactly(&state.asset, &entry.id, recipe), answer)
    }

    /// The recipe an open draft would produce: the current snapshot with the draft's action planned
    /// against it and its plan applied, computed on demand and never persisted. A `NoOp` plan means
    /// the current recipe unchanged, so a gesture that returned to its start previews exactly what
    /// is committed. Nothing is rendered here; the caller decides what to do with the recipe, and
    /// binds it before evaluating it, since it may reference an artifact the current one does not.
    ///
    /// A module action plans through [`Self::plan_action`] and a `mask.*` command through
    /// [`Self::plan_mask_command`], the functions `draft.commit` commits through, with the fields
    /// and the target the commit will carry, so a draft equals its commit by construction.
    pub fn draft_recipe(
        &self,
        asset_id: &AssetId,
        draft: &Draft,
    ) -> Result<(Recipe, EditorState), Error> {
        if &draft.asset_id != asset_id {
            return Err(Error::new(
                ErrorKind::Validation,
                "draft belongs to another asset",
            ));
        }
        let state = self.state(asset_id)?;
        let current = &state.current_entry.snapshot.recipe;
        // Planning evaluates the current stack, so a refusal names what that stack needs.
        let stack = Evaluated::exactly(&state.asset, &state.current_entry.id, current);
        if let Some(command) = crate::mask::commands::find(&draft.action) {
            let checked = check_parameters(&command.action, &Value::Object(draft.fields.clone()))?;
            let target = draft.target.clone().unwrap_or_default();
            let planned = self.plan_mask_command(&state, command, &target, &checked);
            let recipe = match self.needing(stack, planned)? {
                MaskOutcome::NoOp => current.clone(),
                MaskOutcome::Change(change) => change.recipe,
            };
            return Ok((recipe, state));
        }
        let (module, action) = self.registry.action(&draft.action).ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                format!("unknown action {}", draft.action),
            )
        })?;
        let checked = check_parameters(action, &Value::Object(draft.fields.clone()))?;
        let input = module.parse(&draft.action, &checked)?;
        // The draft's target is the one the commit will carry: none for a global gesture, and the
        // mask a masked slider was opened on. The target view hides the layers of the drafted
        // module's effect that belong to another target, which is what lets a global slider drag and
        // a masked one each keep working on a stack that holds both.
        let mask = draft
            .target
            .as_ref()
            .and_then(|target| target.mask.as_ref());
        let planned = self.plan_action(&state.asset, current, module, &input, mask);
        let recipe = self
            .needing(stack, planned)?
            .unwrap_or_else(|| current.clone());
        Ok((recipe, state))
    }

    /// The stack one plan produces from `recipe`, bound, or `None` when it changes nothing.
    ///
    /// `Commit` places the new layer by its effect's declared stage and order: a pixel-stage effect
    /// goes before the geometry tail, so a later crop change carries it instead of moving or
    /// invalidating it, and an effect no provider declares is placed as a geometry one would be and
    /// refused by the whole-stack compile at commit. `Update` keeps the layer's identity and
    /// position, and a missing identity is refused before anything is written.
    ///
    /// `Compose` runs each step exactly as that action would run alone, against the stack the steps
    /// before it produced: the registry finds the action, which must be a field patch; the generic
    /// check and the module's `parse` take its fields; and the module is asked through the same
    /// [`Self::ask`], which refuses an unavailable one. A step carries no mask target, so like an
    /// action sent without one it addresses the global layer: it plans against
    /// [`recipe_for_target`]'s view of the intermediate stack for no mask, and its plan is applied
    /// to the whole intermediate stack, so a masked layer of the step's effect is neither read nor
    /// changed. A composite that was itself given a mask target is refused, since its steps could
    /// not honour it. A step that is itself a composite is refused, and so is any refused step,
    /// before anything is written. The final stack is `None` when it equals the starting one. Each
    /// step plans by comparing payloads, so a composite costs `O(steps × layers)` and rasterizes
    /// nothing.
    fn resolve_plan(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        plan: ActionPlan,
        mask: Option<&MaskId>,
    ) -> Result<Option<Recipe>, Error> {
        let steps = match plan {
            ActionPlan::Compose(steps) => steps,
            plan => return self.apply_plan(recipe, plan, mask),
        };
        if mask.is_some() {
            return Err(Error::new(
                ErrorKind::Validation,
                "a composite action's steps address the global layer, so it takes no mask target",
            ));
        }
        if steps.len() > MAX_COMPOSE_STEPS {
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "a composite action holds {} steps, more than {MAX_COMPOSE_STEPS}",
                    steps.len()
                ),
            ));
        }
        let registry = self.registry.clone();
        let mut resolved = recipe.clone();
        for step in steps {
            let action_id = step.action_id.as_str();
            let (module, action) = registry.action(action_id).ok_or_else(|| {
                Error::new(ErrorKind::Validation, format!("unknown action {action_id}"))
            })?;
            if !action.patch {
                return Err(Error::new(
                    ErrorKind::Validation,
                    format!("{action_id} is not a field-patch action"),
                ));
            }
            let checked = check_parameters(action, &Value::Object(step.parameters))?;
            let input = module.parse(action_id, &checked)?;
            let plan = self.ask(asset, &resolved, module, None, |context, _| {
                module.plan(&input, context)
            })?;
            if let Some(next) = self.apply_plan(&resolved, plan, None)? {
                resolved = next;
            }
        }
        Ok((resolved != *recipe).then_some(resolved))
    }

    /// One step's plan applied to a stack: the placement rules of [`Self::resolve_plan`] for a
    /// single layer, or for each of several in order. A composite here is a step of another
    /// composite, which the host refuses.
    fn apply_plan(
        &self,
        recipe: &Recipe,
        plan: ActionPlan,
        mask: Option<&MaskId>,
    ) -> Result<Option<Recipe>, Error> {
        match plan {
            ActionPlan::NoOp => Ok(None),
            ActionPlan::Commit(layer) => Ok(Some(edited(
                &self.registry,
                recipe,
                LayerEdit::Commit(layer),
                mask,
            )?)),
            ActionPlan::Update(update) => Ok(Some(edited(
                &self.registry,
                recipe,
                LayerEdit::Update(update),
                mask,
            )?)),
            ActionPlan::Edits(edits) => {
                if edits.is_empty() {
                    return Err(Error::new(
                        ErrorKind::Validation,
                        "a plan of layer edits holds none",
                    ));
                }
                let mut stack = recipe.clone();
                for edit in edits {
                    stack = edited(&self.registry, &stack, edit, mask)?;
                }
                Ok(Some(stack))
            }
            ActionPlan::Compose(_) => Err(Error::new(
                ErrorKind::Validation,
                "composite actions do not nest",
            )),
        }
    }

    pub fn apply_pixel(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        x: u32,
        y: u32,
        rgb: [u8; 3],
    ) -> Result<MutationResult, Error> {
        self.apply_action(
            asset_id,
            mutation,
            "set-pixel",
            json!({"x":x,"y":y,"rgb":rgb}),
        )
    }

    pub fn apply_transform(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        transform: Transform,
    ) -> Result<MutationResult, Error> {
        self.apply_action(
            asset_id,
            mutation,
            "transform",
            json!({"transform":transform}),
        )
    }
}

/// Refuse a provider registered unavailable: it keeps its descriptor so its stored layers stay
/// readable, but the host never plans, queries or commits through it.
fn available(module: &dyn ToolModule) -> Result<(), Error> {
    let descriptor = module.descriptor();
    if descriptor.is_available() {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::Incompatible,
        format!("unavailable module {}", descriptor.id),
    ))
}

/// The history label of a module edit: a masked edit always names its mask, where a `mask.*`
/// command names one only once the stack holds more than one. The difference is not an
/// inconsistency but the ambiguity each one actually has: `Update Linear 1` is unmistakable while a
/// recipe holds one mask, but `Exposure +2.00 EV` is exactly what this module's *global* edit
/// writes, so a single mask is already enough for a history row to show two entries nothing
/// distinguishes.
fn masked_label(recipe: &Recipe, mask: Option<&MaskId>, label: String) -> String {
    match mask.and_then(|id| recipe.masks.iter().find(|mask| &mask.id == id)) {
        Some(mask) => format!("{} · {label}", mask.name),
        None => label,
    }
}

/// The host's answers to one stack's stage questions, each resolved the first time a module asks
/// it: the verified source is looked up by the first question that reads a pixel or the sensor, and
/// a RAW stack's linear settings by the first that reads a pixel, and both are kept for the rest of
/// the context. So a plan that asks only for stages — a transform, a crop, a RAW white balance —
/// needs neither the original prepared nor the development to hold its white balance, and one that
/// samples finds out exactly when it does.
struct HostStage<'s> {
    service: &'s EditorService,
    asset: &'s AssetRecord,
    /// The stack the module sees, bound.
    recipe: &'s Recipe,
    source: OnceCell<PreparedSource>,
    settings: OnceCell<LinearSettings>,
}

impl HostStage<'_> {
    /// The verified source, from the prepared-source cache: `preparation-required` when it is not
    /// there, since the catalog owner never decodes.
    fn source(&self) -> Result<&PreparedSource, Error> {
        if let Some(source) = self.source.get() {
            return Ok(source);
        }
        let source = self.service.verified_prepared(self.asset)?;
        Ok(self.source.get_or_init(|| source))
    }

    /// A RAW stack's developed planes and the linear settings this recipe asks of them. Strict:
    /// planes that do not hold the recipe's white balance, or no planes at all, are
    /// `preparation-required`, because a sample is a number and is never approximated.
    fn linear(&self) -> Result<(&LinearImage, LinearSettings), Error> {
        let PreparedSource::Raw(raw) = self.source()? else {
            return Err(Error::new(
                ErrorKind::Incompatible,
                "JPEG source has no linear image",
            ));
        };
        let settings = match self.settings.get() {
            Some(settings) => *settings,
            None => {
                let settings = raw_settings(raw, self.recipe, RawSettingsMode::Strict)?;
                *self.settings.get_or_init(|| settings)
            }
        };
        let linear = raw.linear.as_ref().ok_or_else(|| {
            Error::new(ErrorKind::PreparationRequired, "RAW development required")
        })?;
        Ok((linear, settings))
    }
}

impl StageQuestions for HostStage<'_> {
    /// Compile the prefix before `index`. Compiling folds declared output stages and allocates only
    /// the operation lists, so this copies no part of the stack and rasterizes nothing.
    fn stage_before(&self, index: usize) -> Result<Stage, Error> {
        let recipe = self.recipe;
        Ok(self
            .service
            .registry
            .compile_layers(
                self.asset.width,
                self.asset.height,
                prefix(&recipe.layers, index)?,
                &recipe.masks,
                &recipe.strokes,
                &recipe.artifacts,
            )?
            .stage())
    }

    /// One pixel of the stage a prefix produces. Compiling the prefix costs `O(layers)` and the
    /// evaluation answers the point per segment, so nothing is rasterized.
    fn sample_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        let recipe = self.recipe;
        let registry = &self.service.registry;
        let layers = prefix(&recipe.layers, index)?;
        match self.source()? {
            PreparedSource::Jpeg(image) => Evaluation::over_layers(
                registry,
                image,
                layers,
                &recipe.masks,
                &recipe.strokes,
                &recipe.artifacts,
            )?
            .pixel(x, y),
            PreparedSource::Raw(_) => {
                let (linear, settings) = self.linear()?;
                let prefix_recipe = Recipe {
                    format: recipe.format,
                    layers: layers.to_vec(),
                    // A prefix keeps the whole mask table: the masks a prefix layer references are
                    // the recipe's, not the prefix's, and dropping them would make a valid stack
                    // look as if it named a mask that does not exist.
                    masks: recipe.masks.clone(),
                    // And the strokes those masks resolved to, and the artifacts the recipe was
                    // bound with, for the same reason.
                    strokes: recipe.strokes.clone(),
                    artifacts: recipe.artifacts.clone(),
                };
                Ok(sample_linear(registry, linear, &prefix_recipe, settings, x, y)?.rgba)
            }
        }
    }

    /// The RAW mosaic's own patch, which needs the decoded sensor and no development.
    fn sensor_neutral(&self, x: u32, y: u32) -> Result<[f32; 3], Error> {
        match self.source()? {
            PreparedSource::Raw(raw) => crate::source::neutral_at(raw, x, y),
            PreparedSource::Jpeg(_) => Err(Error::new(
                ErrorKind::Validation,
                "RAW neutral picker requires a RAW original",
            )),
        }
    }
}

/// One layer change of a plan, applied by the host: the half of a layer's identity a module never
/// chooses.
///
/// A commit gets a new identity, the format its effect declares and the request's mask target, and
/// is placed by its effect's declared stage and order: within that region, a masked layer follows
/// the global layer of its effect and the masked layers of earlier masks, so overlapping masks apply
/// in the order the mask list shows. An update replaces the payload and artifacts of the layer with
/// that identity, where it is, and keeps its effect and its mask: the mask is the host's, so an
/// update can neither drop nor move it. An effect no provider declares, and an identity that is not
/// in the stack, are refused before anything is written. `O(layers)`; reads no pixels.
pub(super) fn edited(
    registry: &crate::ModuleRegistry,
    recipe: &Recipe,
    edit: LayerEdit,
    mask: Option<&MaskId>,
) -> Result<Recipe, Error> {
    let format = |effect_id: &str| {
        registry
            .effect(effect_id)
            .map(|(_, effect)| effect.format)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Incompatible,
                    format!("unavailable effect {effect_id}"),
                )
            })
    };
    match edit {
        LayerEdit::Commit(new) => {
            let index = registry.insertion_index_for_target(
                &recipe.layers,
                &new.effect_id,
                mask,
                &recipe.masks,
            );
            let layer = Layer {
                id: LayerId::new(),
                effect_format: format(&new.effect_id)?,
                effect_id: new.effect_id,
                payload: new.payload,
                mask: mask.cloned(),
                artifacts: new.artifacts,
            };
            recipe.with_layer_inserted(index, layer)
        }
        LayerEdit::Update(update) => {
            let existing = recipe
                .layers
                .iter()
                .find(|layer| layer.id == update.id)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Validation,
                        "plan updates a layer that is not in the stack",
                    )
                })?;
            let layer = Layer {
                effect_format: format(&existing.effect_id)?,
                payload: update.payload,
                artifacts: update.artifacts,
                ..existing.clone()
            };
            recipe.with_layer_replaced(layer)
        }
    }
}

/// The ordered layers before a position in the stack, which is what a module asks about when it
/// plans against the stage that position receives. A position past the end is a validation error.
pub(crate) fn prefix(layers: &[Layer], index: usize) -> Result<&[Layer], Error> {
    layers.get(..index).ok_or_else(|| {
        Error::new(
            ErrorKind::Validation,
            format!(
                "layer index {index} is outside the {} layers of the stack",
                layers.len()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{
        MutationOutcome,
        test_support::{
            MISSING_ACTION, SHRINK_ACTION, ShrinkModule, TAIL_ACTION, fixture, mutation, shrink,
            temp,
        },
    };
    use crate::{
        BoxRect, CROP_EFFECT, CropPayload, CropStage, EffectStage, ModuleRegistry,
        ORIENTATION_EFFECT, Orientation, PIXEL_EFFECT, Raster, SnapshotId, open_source, render,
    };
    use serde_json::Map;
    use std::sync::Arc;

    #[test]
    fn transform_composition_is_persistent_and_exact() {
        let catalog = temp("transform.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 0, 0, [1, 2, 3])
            .unwrap();
        service
            .apply_transform(&asset, mutation(1, "right"), Transform::RotateRight)
            .unwrap();
        let state = service.state(&asset).unwrap();
        assert_eq!(
            (
                service.render_current(&asset).unwrap().width,
                service.render_current(&asset).unwrap().height
            ),
            (320, 480)
        );
        assert_eq!(state.current_entry.snapshot.recipe.layers.len(), 2);
        service.undo(&asset, mutation(2, "undo-transform")).unwrap();
        assert_eq!(
            (
                service.render_current(&asset).unwrap().width,
                service.render_current(&asset).unwrap().height
            ),
            (480, 320)
        );
        drop(service);
        let service = EditorService::open(&catalog).unwrap();
        assert_eq!(service.state(&asset).unwrap().revision, 3);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Four Rotate right actions are four history entries and one neutral orientation layer whose
    /// render is the source itself. Every entry keeps its own stack, so undo walks back through
    /// three, two and one quarter turn with the dimensions and pixels each of them produced.
    #[test]
    fn four_quarter_turns_leave_one_neutral_orientation_layer_and_four_entries() {
        let catalog = temp("orientation-collapse.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.render_current(&asset).unwrap();
        assert_eq!((original.width, original.height), (480, 320));

        let mut turned = Vec::new();
        let mut layer_id = None;
        for turn in 0..4u64 {
            service
                .apply_transform(
                    &asset,
                    mutation(turn, &format!("turn-{turn}")),
                    Transform::RotateRight,
                )
                .unwrap();
            let stack = service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers;
            assert_eq!(stack.len(), 1, "turn {turn} keeps one orientation layer");
            assert_eq!(stack[0].effect_id, ORIENTATION_EFFECT);
            assert_eq!(
                stack[0].payload,
                json!({"mirror": false, "turns": (turn + 1) % 4}),
                "turn {turn}"
            );
            match &layer_id {
                None => layer_id = Some(stack[0].id.clone()),
                Some(id) => assert_eq!(&stack[0].id, id, "turn {turn} updates the same layer"),
            }
            turned.push(service.render_current(&asset).unwrap());
        }
        let entries = service.history(&asset, None, 50).unwrap().entries;
        assert_eq!(entries.len(), 5, "Original and one entry per action");
        for entry in entries.iter().take(4) {
            assert_eq!(entry.action_id, "rotate-right");
        }
        let neutral = &turned[3];
        assert_eq!((neutral.width, neutral.height), (480, 320));
        assert_eq!(neutral.rgba, original.rgba, "four turns render the source");

        // Undo walks back through the three, two and one turn states.
        for (step, back) in [(0usize, 2usize), (1, 1), (2, 0)] {
            let at = service.state(&asset).unwrap().revision;
            service
                .undo(&asset, mutation(at, &format!("undo-{step}")))
                .unwrap();
            let state = service.state(&asset).unwrap();
            assert_eq!(state.current_entry.snapshot.recipe.layers.len(), 1);
            assert_eq!(
                state.current_entry.snapshot.recipe.layers[0].payload,
                json!({"mirror": false, "turns": back + 1})
            );
            let raster = service.render_current(&asset).unwrap();
            assert_eq!(
                (raster.width, raster.height),
                (turned[back].width, turned[back].height),
                "undo {step}"
            );
            assert_eq!(raster.rgba, turned[back].rgba, "undo {step}");
        }
        let at = service.state(&asset).unwrap().revision;
        service.undo(&asset, mutation(at, "undo-3")).unwrap();
        assert!(
            service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers
                .is_empty(),
            "undoing the first turn returns to the original empty stack"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// The planning rule at the service: a transform composes into the orientation layer ahead of
    /// the crop, and a crop never ends the tail: the next transform composes into the same layer and
    /// carries the crop through it, so the turned photograph is exactly the cropped one turned. A
    /// pixel layer never ends it either, because the host puts it before the tail.
    #[test]
    fn a_transform_composes_ahead_of_the_crop_and_carries_it() {
        let catalog = temp("orientation-placement.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let revision = |service: &EditorService| service.state(&asset).unwrap().revision;
        let layers = |service: &EditorService| -> Vec<Layer> {
            service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers
        };

        // A pixel layer is not a geometry layer, so the first transform appends the tail.
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 0, 0, [1, 2, 3])
            .unwrap();
        let at = revision(&service);
        service
            .apply_transform(&asset, mutation(at, "first"), Transform::RotateRight)
            .unwrap();
        let stack = layers(&service);
        assert_eq!(
            stack
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [PIXEL_EFFECT, ORIENTATION_EFFECT]
        );
        let first_orientation = stack[1].id.clone();

        // A further pixel edit joins the stack before the tail, so the tail is still last and the
        // next transform composes into it.
        let at = revision(&service);
        service
            .apply_pixel(&asset, mutation(at, "pixel-2"), 1, 0, [4, 5, 6])
            .unwrap();
        let at = revision(&service);
        service
            .apply_transform(&asset, mutation(at, "second"), Transform::MirrorHorizontal)
            .unwrap();
        let stack = layers(&service);
        assert_eq!(
            stack
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [PIXEL_EFFECT, PIXEL_EFFECT, ORIENTATION_EFFECT]
        );
        assert_eq!(stack[2].id, first_orientation, "the same layer, in place");
        assert_eq!(stack[2].payload, json!({"mirror": true, "turns": 3}));

        // A crop joins the tail after the orientation layer; the next transform still composes into
        // that layer, ahead of the crop, and re-expresses the crop so it frames the same content.
        // The crop is off-center, so a turn or a reflection has to move it.
        let at = revision(&service);
        service
            .apply_action(
                &asset,
                mutation(at, "crop"),
                "crop",
                json!({"x":0.1,"y":0.2,"width":0.5,"height":0.5}),
            )
            .unwrap();
        let crop = layers(&service)[3].clone();
        let cropped = service.render_current(&asset).unwrap();
        let at = revision(&service);
        service
            .apply_transform(&asset, mutation(at, "third"), Transform::RotateRight)
            .unwrap();
        let stack = layers(&service);
        assert_eq!(
            stack
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [PIXEL_EFFECT, PIXEL_EFFECT, ORIENTATION_EFFECT, CROP_EFFECT],
            "one orientation layer, ahead of the crop"
        );
        assert_eq!(stack[2].id, first_orientation, "the same layer, in place");
        assert_eq!(stack[2].payload, json!({"mirror": true, "turns": 0}));
        assert_eq!(stack[3].id, crop.id, "the crop keeps its identity");
        assert_ne!(
            stack[3].payload, crop.payload,
            "and is re-expressed in the turned stage"
        );
        // The quarter turn carries the visible crop and swaps its ratio: every pixel of the turned
        // photograph is the cropped one's, turned clockwise.
        let turned = service.render_current(&asset).unwrap();
        assert_eq!(
            (turned.width, turned.height),
            (cropped.height, cropped.width)
        );
        for y in 0..turned.height {
            for x in 0..turned.width {
                assert_eq!(
                    turned.pixel(x, y),
                    cropped.pixel(y, cropped.height - 1 - x),
                    "turned pixel {x}, {y}"
                );
            }
        }

        // A reflection composes into the same layer and carries the off-center composition with
        // the image instead of re-cutting it: every row of the turned crop, bottom to top.
        let at = revision(&service);
        service
            .apply_transform(&asset, mutation(at, "fourth"), Transform::FlipVertical)
            .unwrap();
        let stack = layers(&service);
        assert_eq!(stack.len(), 4);
        assert_eq!(stack[2].id, first_orientation, "the same layer, in place");
        assert_eq!(stack[2].payload, json!({"mirror": false, "turns": 2}));
        assert_eq!(stack[3].id, crop.id);
        let flipped = service.render_current(&asset).unwrap();
        assert_eq!(
            (flipped.width, flipped.height),
            (turned.width, turned.height)
        );
        let row = turned.width as usize * 4;
        for y in 0..turned.height as usize {
            let from = (turned.height as usize - 1 - y) * row;
            assert_eq!(
                &flipped.rgba[y * row..y * row + row],
                &turned.rgba[from..from + row],
                "row {y}"
            );
        }
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Each pixel of `before` against the pixel of `turned` that `orientation` puts it at: the mean
    /// absolute difference of their colour channels, in codes, and the largest one.
    fn difference_under(before: &Raster, turned: &Raster, orientation: Orientation) -> (f64, u8) {
        let mapping = orientation.geometry(before.width, before.height);
        assert_eq!(
            (turned.width, turned.height),
            (mapping.output_width, mapping.output_height)
        );
        let mut total = 0u64;
        let mut largest = 0;
        for y in 0..before.height {
            for x in 0..before.width {
                let (ix, iy) = (i64::from(x), i64::from(y));
                let tx = mapping.a * ix + mapping.b * iy + mapping.tx;
                let ty = mapping.c * ix + mapping.d * iy + mapping.ty;
                let old = before.pixel(x, y).unwrap();
                let new = turned.pixel(tx as u32, ty as u32).unwrap();
                for channel in 0..3 {
                    let difference = old[channel].abs_diff(new[channel]);
                    total += u64::from(difference);
                    largest = largest.max(difference);
                }
            }
        }
        let channels = 3 * u64::from(before.width) * u64::from(before.height);
        (total as f64 / channels as f64, largest)
    }

    /// Through a straightened crop a turn or a reflection still frames the same content: the crop
    /// keeps its extents and moves by at most half a box pixel, onto the nearest whole-pixel origin
    /// in the turned stage, so the output is the old one turned up to that sub-pixel resample. The
    /// geometry tests bound the move; this proves the stack, the payload and the render agree.
    #[test]
    fn a_transform_over_a_straightened_crop_frames_the_same_content() {
        let catalog = temp("straightened-carry.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let revision = |service: &EditorService| service.state(&asset).unwrap().revision;
        let stage = CropStage {
            width: 480,
            height: 320,
            angle: 7.0,
        };
        let (box_width, box_height) = stage.bounding_box();
        let payload = stage
            .fit_about_center(BoxRect::from_center(
                (box_width * 0.42, box_height * 0.56),
                box_width * 0.5,
                box_height * 0.45,
            ))
            .normalized(&stage);
        service
            .apply_action(
                &asset,
                mutation(0, "crop"),
                "crop",
                serde_json::to_value(payload).unwrap(),
            )
            .unwrap();
        let mut before = service.render_current(&asset).unwrap();
        let mut composed = Orientation::NEUTRAL;
        let mut input = (480, 320);
        for (request, transform) in [
            ("right", Transform::RotateRight),
            ("mirror", Transform::MirrorHorizontal),
            ("flip", Transform::FlipVertical),
            ("left", Transform::RotateLeft),
        ] {
            let stored: CropPayload = serde_json::from_value(
                service
                    .state(&asset)
                    .unwrap()
                    .current_entry
                    .snapshot
                    .recipe
                    .layers
                    .last()
                    .unwrap()
                    .payload
                    .clone(),
            )
            .unwrap();
            let at = revision(&service);
            service
                .apply_transform(&asset, mutation(at, request), transform)
                .unwrap();
            composed = composed.then(transform);
            let stack = service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers;
            assert_eq!(
                stack
                    .iter()
                    .map(|layer| layer.effect_id.as_str())
                    .collect::<Vec<_>>(),
                [ORIENTATION_EFFECT, CROP_EFFECT],
                "{request}"
            );
            assert_eq!(
                serde_json::from_value::<Orientation>(stack[0].payload.clone()).unwrap(),
                composed,
                "{request}"
            );
            // The catalog stores the payload as JSON, so compare the whole-pixel rectangle it
            // renders rather than the last bit of each fraction.
            let carried: CropPayload = serde_json::from_value(stack[1].payload.clone()).unwrap();
            let expected = stored.carried(input, Orientation::of(transform)).unwrap();
            let turned_input = if Orientation::of(transform).turns % 2 == 1 {
                (input.1, input.0)
            } else {
                input
            };
            let turned_stage = CropStage {
                width: turned_input.0,
                height: turned_input.1,
                angle: expected.angle,
            };
            assert_eq!(
                carried.output_rect(&turned_stage).unwrap(),
                expected.output_rect(&turned_stage).unwrap(),
                "{request}"
            );
            let old = stored
                .output_rect(&CropStage {
                    width: input.0,
                    height: input.1,
                    angle: stored.angle,
                })
                .unwrap();
            let new = carried.output_rect(&turned_stage).unwrap();
            assert_eq!(
                if turned_input == input {
                    (new.width, new.height)
                } else {
                    (new.height, new.width)
                },
                (old.width, old.height),
                "{request}: a crop clear of the source edges keeps its extents"
            );
            // A reflection reverses the straightening angle; a quarter turn keeps it.
            assert_eq!(
                carried.angle,
                if composed.mirror { -7.0 } else { 7.0 },
                "{request}"
            );
            // The resample reads the same content from under a box pixel away, so every output
            // value lies within what the old output holds a pixel around it.
            // The resample reads the same content from under a box pixel away, so the output is
            // the old one turned up to that move: it differs only where detail is finer than the
            // move, such as the fixture's one-pixel stripes, and a framing of any other content
            // would differ by tens of codes throughout.
            let turned = service.render_current(&asset).unwrap();
            let (mean, _) = difference_under(&before, &turned, Orientation::of(transform));
            assert!(mean < 4.0, "{request}: {mean} codes apart on average");
            input = turned_input;
            before = turned;
        }
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A stored stack may hold an orientation layer after the crop, where the host never places
    /// one. The next transform folds that layer ahead of the crop with itself and leaves it
    /// neutral, so the output is exactly the transform applied to what the stack showed, and the
    /// crop's input stage holds every turn from then on.
    #[test]
    fn a_transform_folds_an_orientation_stored_after_the_crop() {
        let catalog = temp("trailing-orientation.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = {
            let asset = service.import(&fixture()).unwrap().asset.id;
            service.state(&asset).unwrap()
        };
        let asset = state.asset.id.clone();
        let crop = Layer::crop(CropPayload {
            angle: 0.0,
            x: 0.1,
            y: 0.2,
            width: 0.5,
            height: 0.5,
        });
        let trailing = Layer::orientation(Orientation::of(Transform::RotateRight));
        let snapshot = state
            .current_entry
            .snapshot
            .with_recipe(Recipe {
                layers: vec![crop.clone(), trailing.clone()],
                ..state.current_entry.snapshot.recipe.clone()
            })
            .unwrap();
        service
            .commit_snapshot(
                &asset,
                mutation(0, "stored"),
                json!({}),
                snapshot,
                &state.asset,
                CommittedAction {
                    input: ActionInput {
                        action_id: "test-stored-stack".into(),
                        parameters: Map::new(),
                    },
                    label: "Stored stack".into(),
                },
            )
            .unwrap();
        let before = service.render_current(&asset).unwrap();
        assert_eq!((before.width, before.height), (160, 240));
        service
            .apply_transform(&asset, mutation(1, "mirror"), Transform::MirrorHorizontal)
            .unwrap();
        let stack = service
            .state(&asset)
            .unwrap()
            .current_entry
            .snapshot
            .recipe
            .layers;
        assert_eq!(
            stack
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [ORIENTATION_EFFECT, CROP_EFFECT, ORIENTATION_EFFECT]
        );
        assert_eq!(
            serde_json::from_value::<Orientation>(stack[0].payload.clone()).unwrap(),
            Orientation::of(Transform::RotateRight).then(Transform::MirrorHorizontal),
            "the stored turn, then the mirror, ahead of the crop"
        );
        assert_eq!(stack[1].id, crop.id);
        assert_eq!(
            stack[2].id, trailing.id,
            "the stored layer keeps its identity"
        );
        assert_eq!(stack[2].payload, json!({"mirror": false, "turns": 0}));
        let mirrored = service.render_current(&asset).unwrap();
        assert_eq!(
            difference_under(
                &before,
                &mirrored,
                Orientation::of(Transform::MirrorHorizontal)
            ),
            (0.0, 0),
            "every pixel is the stored stack's, mirrored"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn rejected_actions_leave_state_history_and_the_request_table_untouched() {
        let catalog = temp("actions.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let before = service.state(&asset).unwrap();
        for (case, action, parameters, fragment) in [
            ("unknown action", "paint", json!({}), "unknown action paint"),
            (
                "missing required parameter",
                "set-pixel",
                json!({"x":0,"y":0}),
                "missing required parameter rgb",
            ),
            (
                "unknown parameter",
                "set-pixel",
                json!({"x":0,"y":0,"rgb":[1,2,3],"z":1}),
                "unknown parameter z",
            ),
            (
                "integer out of range",
                "set-pixel",
                json!({"x":-1,"y":0,"rgb":[1,2,3]}),
                "parameter x must be an integer within 0..=16383",
            ),
            (
                "malformed color",
                "set-pixel",
                json!({"x":0,"y":0,"rgb":[1,2]}),
                "parameter rgb must be three sRGB channels 0..=255",
            ),
            (
                "unknown enum option",
                "transform",
                json!({"transform":"rotate-sideways"}),
                "parameter transform must be one of",
            ),
            (
                "outside the content stage",
                "set-pixel",
                json!({"x":9000,"y":0,"rgb":[1,2,3]}),
                "pixel (9000, 0) is outside the 480x320 content stage",
            ),
        ] {
            let error = service
                .apply_action(&asset, mutation(0, case), action, parameters)
                .expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
        assert_eq!(service.state(&asset).unwrap(), before);
        let count = |table: &str| -> i64 {
            service
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        assert_eq!(count("entries"), 1, "no history row was written");
        assert_eq!(count("requests"), 0, "no request result was recorded");
        assert_eq!(before.revision, 0);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn actions_wrappers_and_no_op_detection_agree() {
        let catalog = temp("action-equivalence.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.render_current(&asset).unwrap().pixel(0, 0).unwrap();
        let repeated = service
            .apply_action(
                &asset,
                mutation(0, "same-value"),
                "set-pixel",
                json!({"x":0,"y":0,"rgb":[original[0],original[1],original[2]]}),
            )
            .unwrap();
        assert_eq!(repeated.outcome, MutationOutcome::NoOp);
        let through_action = service
            .apply_action(
                &asset,
                mutation(0, "action"),
                "set-pixel",
                json!({"x":0,"y":0,"rgb":[1,2,3]}),
            )
            .unwrap();
        let entry = service
            .entry(&asset, &through_action.current_entry_id)
            .unwrap();
        assert_eq!(entry.action_id, "set-pixel");
        assert_eq!(entry.parameters, json!({"x":0,"y":0,"rgb":[1,2,3]}));
        let rotated = service
            .apply_action(
                &asset,
                mutation(1, "rotate"),
                "transform",
                json!({"transform":"rotate-right"}),
            )
            .unwrap();
        let entry = service.entry(&asset, &rotated.current_entry_id).unwrap();
        assert_eq!(entry.action_id, "rotate-right");
        assert_eq!(entry.parameters, json!({"transform":"rotate-right"}));
        // The action keeps its durable identity; the stack records the orientation it reached.
        assert_eq!(
            entry.snapshot.recipe.layers[1].effect_id,
            ORIENTATION_EFFECT
        );
        assert_eq!(
            entry.snapshot.recipe.layers[1].payload,
            json!({"mirror":false,"turns":1})
        );
        // The wrapper retries the same request and deduplicates through the same hash.
        let retry = service
            .apply_transform(&asset, mutation(1, "rotate"), Transform::RotateRight)
            .unwrap();
        assert!(retry.deduplicated);
        assert_eq!(retry.current_entry_id, rotated.current_entry_id);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    fn rows(service: &EditorService, table: &str) -> i64 {
        service
            .connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn an_update_replaces_its_layer_in_place_and_leaves_earlier_snapshots_alone() {
        let catalog = temp("update.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let appended = service
            .apply_action(
                &asset,
                mutation(0, "append"),
                SHRINK_ACTION,
                shrink(100, 100),
            )
            .unwrap();
        let first = service.entry(&asset, &appended.current_entry_id).unwrap();
        let layer_id = first.snapshot.recipe.layers[0].id.clone();
        // A pixel layer addresses the content stage, so the host puts it before the geometry tail
        // and the shrink layer keeps its own identity and position after it.
        service
            .apply_pixel(&asset, mutation(1, "pixel"), 10, 10, [1, 2, 3])
            .unwrap();
        let updated = service
            .apply_action(&asset, mutation(2, "update"), SHRINK_ACTION, shrink(50, 50))
            .unwrap();
        assert_eq!(updated.outcome, MutationOutcome::Applied);
        let entry = service.entry(&asset, &updated.current_entry_id).unwrap();
        assert_eq!(
            entry.snapshot.recipe.layers.len(),
            2,
            "an update adds no layer"
        );
        assert_eq!(
            entry.snapshot.recipe.layers[0].effect_id, PIXEL_EFFECT,
            "the pixel layer stays before the geometry tail"
        );
        assert_eq!(
            entry.snapshot.recipe.layers[1].id, layer_id,
            "the updated layer keeps its identity and position"
        );
        assert_eq!(entry.snapshot.recipe.layers[1].payload, shrink(50, 50));
        assert_ne!(
            entry.snapshot.id, first.snapshot.id,
            "an update produces a new snapshot identity"
        );
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 4);
        assert_eq!(
            service
                .entry(&asset, &first.id)
                .unwrap()
                .snapshot
                .recipe
                .layers[0]
                .payload,
            shrink(100, 100),
            "the earlier entry keeps its own stack"
        );
        let raster = service.render_current(&asset).unwrap();
        assert_eq!((raster.width, raster.height), (50, 50));
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn an_update_that_invalidates_a_later_layer_is_rejected_atomically() {
        let catalog = temp("update-invalid.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_action(
                &asset,
                mutation(0, "append"),
                SHRINK_ACTION,
                shrink(100, 100),
            )
            .unwrap();
        // A second geometry layer follows the first and addresses the stage it produces.
        service
            .apply_action(&asset, mutation(1, "tail"), TAIL_ACTION, shrink(90, 90))
            .unwrap();
        let before = service.state(&asset).unwrap();
        let (entries, requests) = (rows(&service, "entries"), rows(&service, "requests"));
        let error = service
            .apply_action(
                &asset,
                mutation(2, "shrink-too-far"),
                SHRINK_ACTION,
                shrink(50, 50),
            )
            .expect_err("the tail layer would fall outside the new stage");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .contains("shrink 90x90 is larger than the 50x50 input stage"),
            "{error}"
        );
        assert_eq!(service.state(&asset).unwrap(), before);
        assert_eq!(
            rows(&service, "entries"),
            entries,
            "no history row was written"
        );
        assert_eq!(
            rows(&service, "requests"),
            requests,
            "no request result was recorded"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A stored stack whose finish layer precedes a geometry layer is refused by planning exactly
    /// as it is by compilation: the request fails with the named validation error, no history row
    /// and no request result are written, and the stack stays readable through `state`.
    ///
    /// The host never builds that order, so the stack is planted the only way it can arise: a
    /// registry in which the effect is a colour effect writes it, and a registry in which the same
    /// effect is a finish effect reads it back.
    #[test]
    fn a_stored_finish_layer_before_geometry_is_refused_without_being_rewritten() {
        use crate::modules::{STAGE_ACTION, STAGE_EFFECT, StageModule};
        let registry = |stage: EffectStage| {
            let mut registry = ModuleRegistry::builtin();
            registry
                .register(StageModule::shared(
                    "test.stage",
                    STAGE_EFFECT,
                    STAGE_ACTION,
                    stage,
                    0,
                ))
                .unwrap();
            Arc::new(registry)
        };
        let catalog = temp("finish-before-geometry.sqlite");
        let mut service = EditorService::open_with(&catalog, registry(EffectStage::Color)).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_action(&asset, mutation(0, "stage"), STAGE_ACTION, json!({}))
            .unwrap();
        service
            .apply_action(
                &asset,
                mutation(1, "turn"),
                "transform",
                json!({"transform":"rotate-left"}),
            )
            .unwrap();
        let written = service.state(&asset).unwrap();
        assert_eq!(
            written
                .current_entry
                .snapshot
                .recipe
                .layers
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            vec![STAGE_EFFECT, ORIENTATION_EFFECT],
        );
        drop(service);

        let mut service =
            EditorService::open_with(&catalog, registry(EffectStage::Finish)).unwrap();
        let before = service.state(&asset).unwrap();
        let (entries, requests) = (rows(&service, "entries"), rows(&service, "requests"));
        let error = service
            .apply_pixel(&asset, mutation(2, "pixel"), 0, 0, [1, 2, 3])
            .expect_err("the stored order has no stage a new layer could address");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error.detail.starts_with("finish layer precedes geometry"),
            "{}",
            error.detail
        );
        assert_eq!(
            service.state(&asset).unwrap(),
            before,
            "the refused stack is left exactly as it stands"
        );
        assert_eq!(rows(&service, "entries"), entries, "no history row");
        assert_eq!(rows(&service, "requests"), requests, "no request result");
        assert_eq!(
            before.current_entry.snapshot.recipe.layers.len(),
            2,
            "both layers stay readable"
        );
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 3);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn an_update_naming_a_layer_that_is_not_in_the_stack_is_rejected() {
        let catalog = temp("update-missing.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let before = service.state(&asset).unwrap();
        let error = service
            .apply_action(
                &asset,
                mutation(0, "missing"),
                MISSING_ACTION,
                shrink(10, 10),
            )
            .expect_err("the planned identity is not in the stack");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            "plan updates a layer that is not in the stack"
        );
        assert_eq!(service.state(&asset).unwrap(), before);
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 1);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A plan names an effect and a payload, or an identity and a payload; the host owns the rest.
    /// A commit gets a new identity, its effect's declared format and the request's mask target; an
    /// update keeps the layer's identity, effect, position and mask whatever the module sends, so no
    /// module can drop or move a mask. An effect nobody provides is refused before anything is
    /// written.
    #[test]
    fn the_host_owns_a_planned_layers_identity_format_and_mask() {
        let registry = ModuleRegistry::builtin();
        let mask = crate::Mask::new("Mask 1");
        let recipe = Recipe {
            layers: vec![Layer::pixel(0, 0, [1, 2, 3])],
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        let committed = edited(
            &registry,
            &recipe,
            LayerEdit::Commit(crate::NewLayer::new(
                crate::BASIC_EFFECT,
                json!({"exposure": 1.0}),
            )),
            Some(&mask.id),
        )
        .unwrap();
        let layer = &committed.layers[1];
        assert_eq!(layer.effect_id, crate::BASIC_EFFECT);
        assert_eq!(layer.effect_format, crate::EFFECT_FORMAT);
        assert_eq!(
            layer.mask.as_ref(),
            Some(&mask.id),
            "the target is the host's"
        );
        assert!(layer.artifacts.is_empty());
        assert_ne!(layer.id, recipe.layers[0].id);

        let updated = edited(
            &registry,
            &committed,
            LayerEdit::Update(crate::LayerUpdate::new(
                layer.id.clone(),
                json!({"exposure": -1.0}),
            )),
            None,
        )
        .unwrap();
        assert_eq!(
            updated.layers[1],
            Layer {
                payload: json!({"exposure": -1.0}),
                ..layer.clone()
            },
            "an update keeps the identity, effect, position and mask"
        );
        assert_eq!(updated.layers[0], recipe.layers[0]);

        let unknown = edited(
            &registry,
            &recipe,
            LayerEdit::Commit(crate::NewLayer::new("test.nobody", json!({}))),
            None,
        )
        .expect_err("no provider declares the effect");
        assert_eq!(unknown.kind, ErrorKind::Incompatible);
        assert_eq!(unknown.detail, "unavailable effect test.nobody");
    }

    /// The 480x320 fixture's crop journey: an exact copy at angle zero, in-place updates that keep
    /// one layer and one entry per commit, a resampled angled crop, composition with a later
    /// quarter turn and a content-stage pixel before the tail, reset, navigation and reopen.
    #[test]
    fn the_crop_journey_keeps_exact_pixels_one_layer_and_every_snapshot() {
        let catalog = temp("crop-journey.sqlite");
        let source_path = fixture();
        let source_bytes = std::fs::read(&source_path).unwrap();
        let (asset, layer_id, first_entry, angled_entry, before_reopen);
        {
            let mut service = EditorService::open(&catalog).unwrap();
            let state = service.import(&source_path).unwrap();
            asset = state.asset.id.clone();
            let original = service.render_current(&asset).unwrap();
            assert_eq!((original.width, original.height), (480, 320));
            let revision = |service: &EditorService| service.state(&asset).unwrap().revision;
            let layers = |service: &EditorService| -> Vec<Layer> {
                service
                    .state(&asset)
                    .unwrap()
                    .current_entry
                    .snapshot
                    .recipe
                    .layers
            };

            // An angle-zero crop copies the source rectangle byte for byte.
            let at = revision(&service);
            let first = service
                .apply_action(
                    &asset,
                    mutation(at, "crop-a"),
                    "crop",
                    json!({"x":0.25,"y":0.25,"width":0.5,"height":0.5}),
                )
                .unwrap();
            assert_eq!(first.outcome, MutationOutcome::Applied);
            first_entry = first.current_entry_id.clone();
            let entry = service.entry(&asset, &first_entry).unwrap();
            assert_eq!(entry.action_id, "crop");
            assert_eq!(
                entry.parameters,
                json!({"angle":0.0,"x":0.25,"y":0.25,"width":0.5,"height":0.5})
            );
            assert_eq!(entry.snapshot.recipe.layers.len(), 1);
            layer_id = entry.snapshot.recipe.layers[0].id.clone();
            assert_eq!(entry.snapshot.recipe.layers[0].effect_id, CROP_EFFECT);
            let cropped = service.render_current(&asset).unwrap();
            assert_eq!((cropped.width, cropped.height), (240, 160));
            let row_bytes = 240 * 4;
            for y in 0..160usize {
                let start = (80 + y) * 480 * 4 + 120 * 4;
                assert_eq!(
                    &cropped.rgba[y * row_bytes..(y + 1) * row_bytes],
                    &original.rgba[start..start + row_bytes],
                    "crop row {y} is not an exact copy of the source"
                );
            }

            // A second rectangle updates the same layer; the earlier snapshot keeps its own stack.
            let at = revision(&service);
            let second = service
                .apply_action(
                    &asset,
                    mutation(at, "crop-b"),
                    "crop",
                    json!({"x":0.5,"y":0.0,"width":0.5,"height":0.5}),
                )
                .unwrap();
            let updated = service.entry(&asset, &second.current_entry_id).unwrap();
            assert_eq!(
                updated.snapshot.recipe.layers.len(),
                1,
                "an update adds no layer"
            );
            assert_eq!(updated.snapshot.recipe.layers[0].id, layer_id);
            assert_ne!(updated.snapshot.id, entry.snapshot.id, "a new snapshot");
            assert_eq!(
                service.entry(&asset, &first_entry).unwrap(),
                entry,
                "the earlier entry and snapshot are untouched"
            );
            assert_eq!(
                service.history(&asset, None, 50).unwrap().entries.len(),
                3,
                "one entry per commit, plus the import's original"
            );
            let moved = service.render_current(&asset).unwrap();
            assert_eq!((moved.width, moved.height), (240, 160));
            for y in 0..160usize {
                let start = y * 480 * 4 + 240 * 4;
                assert_eq!(
                    &moved.rgba[y * row_bytes..(y + 1) * row_bytes],
                    &original.rgba[start..start + row_bytes],
                    "moved crop row {y}"
                );
            }
            // The same rectangle again is a reported no-op with no history row.
            let at = revision(&service);
            let repeated = service
                .apply_action(
                    &asset,
                    mutation(at, "crop-b-again"),
                    "crop",
                    json!({"x":0.5,"y":0.0,"width":0.5,"height":0.5}),
                )
                .unwrap();
            assert_eq!(repeated.outcome, MutationOutcome::NoOp);
            assert_eq!(service.history(&asset, None, 50).unwrap().entries.len(), 3);

            // An angled crop resamples; the rendered stage is exactly the payload's output rect.
            let angled_payload = CropPayload {
                angle: 5.0,
                x: 0.2,
                y: 0.2,
                width: 0.6,
                height: 0.6,
            };
            let expected = angled_payload
                .output_rect(&CropStage {
                    width: 480,
                    height: 320,
                    angle: 5.0,
                })
                .expect("the angled rectangle is covered");
            let at = revision(&service);
            angled_entry = service
                .apply_action(
                    &asset,
                    mutation(at, "crop-angled"),
                    "crop",
                    json!({"angle":5.0,"x":0.2,"y":0.2,"width":0.6,"height":0.6}),
                )
                .unwrap()
                .current_entry_id;
            let raster = service.render_current(&asset).unwrap();
            assert_eq!(
                (raster.width, raster.height),
                (expected.width, expected.height)
            );
            assert_eq!(layers(&service)[0].id, layer_id);

            // A quarter turn after the crop swaps the dimensions and goes ahead of the crop layer,
            // which it carries: the crop frames the same content in the turned stage.
            let at = revision(&service);
            service
                .apply_transform(&asset, mutation(at, "rotate"), Transform::RotateRight)
                .unwrap();
            let rotated = service.render_current(&asset).unwrap();
            assert_eq!(
                (rotated.width, rotated.height),
                (expected.height, expected.width)
            );
            let stack = layers(&service);
            assert_eq!(stack.len(), 2);
            assert_eq!(stack[0].effect_id, ORIENTATION_EFFECT);
            assert_eq!(stack[1].id, layer_id, "the crop layer keeps its identity");

            // Re-cropping after the quarter turn updates in place, in the turned stage.
            let at = revision(&service);
            service
                .apply_action(
                    &asset,
                    mutation(at, "crop-c"),
                    "crop",
                    json!({"x":0.1,"y":0.1,"width":0.5,"height":0.5}),
                )
                .unwrap();
            let stack = layers(&service);
            assert_eq!(stack.len(), 2);
            assert_eq!(stack[1].id, layer_id, "the crop layer keeps its identity");
            assert_eq!(
                stack[0].effect_id, ORIENTATION_EFFECT,
                "the transform stays ahead of the crop"
            );
            let recropped = service.render_current(&asset).unwrap();
            assert_eq!((recropped.width, recropped.height), (160, 240));

            // A pixel edit addresses the content stage, so the host puts it before the crop and a
            // rectangle that no longer covers it is accepted instead of rejected.
            let at = revision(&service);
            service
                .apply_pixel(&asset, mutation(at, "pixel"), 150, 230, [1, 2, 3])
                .unwrap();
            let stack = layers(&service);
            assert_eq!(stack.len(), 3);
            assert_eq!(
                stack[0].effect_id, PIXEL_EFFECT,
                "the pixel layer joins the stack before the geometry tail"
            );
            assert_eq!(stack[1].effect_id, ORIENTATION_EFFECT);
            assert_eq!(stack[2].id, layer_id, "the crop layer stays last");
            let at = revision(&service);
            assert_eq!(
                service
                    .apply_action(
                        &asset,
                        mutation(at, "crop-smaller"),
                        "crop",
                        json!({"x":0.1,"y":0.1,"width":0.25,"height":0.25}),
                    )
                    .unwrap()
                    .outcome,
                MutationOutcome::Applied,
                "a smaller rectangle is never rejected because of a content-stage pixel"
            );

            // Reset returns the crop layer's output to its own input stage.
            let at = revision(&service);
            let reset = service
                .apply_action(&asset, mutation(at, "crop-reset"), "crop-reset", json!({}))
                .unwrap();
            assert_eq!(reset.outcome, MutationOutcome::Applied);
            let stack = layers(&service);
            assert_eq!(stack[2].id, layer_id);
            assert_eq!(
                stack[2].payload,
                json!({"angle":0.0,"x":0.0,"y":0.0,"width":1.0,"height":1.0})
            );
            let full = service.render_current(&asset).unwrap();
            assert_eq!(
                (full.width, full.height),
                (320, 480),
                "the whole input stage, turned by the quarter turn ahead of the crop"
            );
            let at = revision(&service);
            assert_eq!(
                service
                    .apply_action(
                        &asset,
                        mutation(at, "crop-reset-again"),
                        "crop-reset",
                        json!({})
                    )
                    .unwrap()
                    .outcome,
                MutationOutcome::NoOp,
                "an already neutral crop layer"
            );

            // Undo, redo and restore keep every entry and every snapshot.
            let recorded: Vec<_> = service
                .history(&asset, None, 50)
                .unwrap()
                .entries
                .iter()
                .map(|row| service.entry(&asset, &row.id).unwrap())
                .collect();
            let reset_entry = reset.current_entry_id.clone();
            let at = revision(&service);
            service.undo(&asset, mutation(at, "undo")).unwrap();
            assert_ne!(service.state(&asset).unwrap().current_entry.id, reset_entry);
            let at = revision(&service);
            service.redo(&asset, mutation(at, "redo")).unwrap();
            assert_eq!(service.state(&asset).unwrap().current_entry.id, reset_entry);
            let at = revision(&service);
            let restored = service
                .restore(&asset, mutation(at, "restore-first"), &first_entry)
                .unwrap();
            assert_eq!(
                service
                    .entry(&asset, &restored.current_entry_id)
                    .unwrap()
                    .snapshot
                    .recipe,
                service.entry(&asset, &first_entry).unwrap().snapshot.recipe,
                "restore copies the first crop's stack"
            );
            for entry in &recorded {
                assert_eq!(
                    &service.entry(&asset, &entry.id).unwrap(),
                    entry,
                    "entry {} and its snapshot are unchanged",
                    entry.id
                );
            }
            before_reopen = service.render_current(&asset).unwrap();
            assert_eq!(
                (before_reopen.width, before_reopen.height),
                (240, 160),
                "the restored first crop"
            );
        }

        let service = EditorService::open(&catalog).unwrap();
        let stack = service
            .state(&asset)
            .unwrap()
            .current_entry
            .snapshot
            .recipe
            .layers;
        assert_eq!(stack.len(), 1);
        assert_eq!(
            stack[0].id, layer_id,
            "the crop layer's identity survives reopen"
        );
        let reopened = service.render_current(&asset).unwrap();
        assert_eq!(reopened.rgba, before_reopen.rgba, "identical pixels");
        let angled = service.render_entry(&asset, &angled_entry).unwrap();
        assert!(
            angled.width < 480 && angled.height < 320,
            "the angled entry still renders its trimmed stage"
        );
        assert_eq!(
            std::fs::read(&source_path).unwrap(),
            source_bytes,
            "source unchanged"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A pixel edit addresses the content stage: the source after EXIF orientation. The host puts
    /// it before the geometry tail, so moving, growing and shrinking the crop never moves the
    /// edit, is never rejected because of it, and a rectangle that hides it keeps it.
    #[test]
    fn a_pixel_edit_holds_its_content_pixel_through_every_crop_change() {
        let catalog = temp("content-stage.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let revision = |service: &EditorService| service.state(&asset).unwrap().revision;
        let layers = |service: &EditorService| -> Vec<Layer> {
            service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers
        };

        // The tail: an angle-zero rectangle at (48, 32) of the 480x320 source, then a quarter turn,
        // which goes ahead of the crop and carries it: the crop frames the same content at (128, 48)
        // of the turned 320x480 stage.
        service
            .apply_action(
                &asset,
                mutation(0, "crop"),
                "crop",
                json!({"x":0.1,"y":0.1,"width":0.5,"height":0.5}),
            )
            .unwrap();
        let at = revision(&service);
        service
            .apply_transform(&asset, mutation(at, "rotate"), Transform::RotateRight)
            .unwrap();
        let before = service.render_current(&asset).unwrap();
        assert_eq!((before.width, before.height), (160, 240));

        // The edit joins the stack before the tail and shows where its content pixel is drawn.
        let at = revision(&service);
        service
            .apply_pixel(&asset, mutation(at, "visible"), 150, 100, [1, 2, 3])
            .unwrap();
        let stack = layers(&service);
        assert_eq!(
            stack
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [PIXEL_EFFECT, ORIENTATION_EFFECT, CROP_EFFECT]
        );
        let edited = service.render_current(&asset).unwrap();
        // Content (150, 100) is drawn at (219, 150) of the turned stage, less the crop origin.
        let shown = (219 - 128, 150 - 48);
        assert_eq!(edited.pixel(shown.0, shown.1), Some([1, 2, 3, 255]));
        for y in 0..edited.height {
            for x in 0..edited.width {
                if (x, y) != shown {
                    assert_eq!(edited.pixel(x, y), before.pixel(x, y), "({x}, {y})");
                }
            }
        }

        // A content pixel the crop does not cover is accepted and simply not drawn.
        let at = revision(&service);
        let hidden = service
            .apply_pixel(&asset, mutation(at, "hidden"), 150, 230, [4, 5, 6])
            .unwrap();
        assert_eq!(hidden.outcome, MutationOutcome::Applied);
        assert_eq!(layers(&service).len(), 4);
        assert_eq!(
            service.render_current(&asset).unwrap().rgba,
            edited.rgba,
            "an edit outside the cropped output changes no rendered pixel"
        );

        // A coordinate outside the content stage is a validation error naming that stage.
        let at = revision(&service);
        let error = service
            .apply_pixel(&asset, mutation(at, "outside"), 500, 10, [7, 8, 9])
            .expect_err("500 is outside the 480 pixel wide content stage");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            "pixel (500, 10) is outside the 480x320 content stage"
        );

        // Replacing a content pixel with the value the content already holds is a no-op.
        let at = revision(&service);
        assert_eq!(
            service
                .apply_pixel(&asset, mutation(at, "same"), 150, 100, [1, 2, 3])
                .unwrap()
                .outcome,
            MutationOutcome::NoOp
        );

        // Moving and growing the crop keep the same content pixel edited. The crop addresses the
        // turned stage, where that pixel is drawn at (219, 150).
        for (request, rectangle, origin, stage) in [
            (
                "crop-moved",
                json!({"x":0.3,"y":0.2,"width":0.5,"height":0.5}),
                (96u32, 96u32),
                (160u32, 240u32),
            ),
            (
                "crop-grown",
                json!({"x":0.0,"y":0.0,"width":1.0,"height":1.0}),
                (0, 0),
                (320, 480),
            ),
        ] {
            let at = revision(&service);
            let result = service
                .apply_action(&asset, mutation(at, request), "crop", rectangle)
                .unwrap();
            assert_eq!(result.outcome, MutationOutcome::Applied, "{request}");
            let raster = service.render_current(&asset).unwrap();
            assert_eq!((raster.width, raster.height), stage, "{request}");
            assert_eq!(
                raster.pixel(219 - origin.0, 150 - origin.1),
                Some([1, 2, 3, 255]),
                "{request}"
            );
        }

        // A rectangle that hides the edit is accepted, and growing it back shows it again.
        let at = revision(&service);
        let shrunk = service
            .apply_action(
                &asset,
                mutation(at, "crop-shrunk"),
                "crop",
                json!({"x":0.0,"y":0.0,"width":0.1,"height":0.1}),
            )
            .unwrap();
        assert_eq!(
            shrunk.outcome,
            MutationOutcome::Applied,
            "a shrink that hides the pixel is accepted"
        );
        let hidden = service.render_current(&asset).unwrap();
        assert_eq!((hidden.width, hidden.height), (32, 48));
        assert!(
            hidden
                .rgba
                .chunks_exact(4)
                .all(|pixel| pixel[..3] != [1, 2, 3]),
            "the hidden edit draws nothing"
        );
        let at = revision(&service);
        service
            .apply_action(
                &asset,
                mutation(at, "crop-regrown"),
                "crop",
                json!({"x":0.0,"y":0.0,"width":1.0,"height":1.0}),
            )
            .unwrap();
        assert_eq!(
            service.render_current(&asset).unwrap().pixel(219, 150),
            Some([1, 2, 3, 255]),
            "the edit was hidden, not lost"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Through a straightened crop the resample carries the content edit: the rendered frame
    /// matches the point sampler everywhere, the edit shows as one small cluster of blended output
    /// pixels, and replacing the content pixel with its content value is still a reported no-op
    /// although the output shows a different value there.
    #[test]
    fn a_content_edit_under_a_straightened_crop_matches_the_reference_sampler() {
        let catalog = temp("content-angled.sqlite");
        let source_path = fixture();
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&source_path).unwrap().asset.id;
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 150, 100, [1, 2, 3])
            .unwrap();
        let at = service.state(&asset).unwrap().revision;
        service
            .apply_action(
                &asset,
                mutation(at, "crop-angled"),
                "crop",
                json!({"angle":5.0,"x":0.2,"y":0.2,"width":0.6,"height":0.6}),
            )
            .unwrap();
        let recipe = service
            .state(&asset)
            .unwrap()
            .current_entry
            .snapshot
            .recipe
            .clone();
        assert_eq!(
            recipe
                .layers
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [PIXEL_EFFECT, CROP_EFFECT],
            "the edit stays before the crop that resamples it"
        );
        let raster = service.render_current(&asset).unwrap();
        let registry = ModuleRegistry::builtin();
        let source = open_source(&source_path).unwrap();

        // The rendered frame and the point sampler evaluate the same stack by different paths.
        for y in 0..raster.height {
            for x in 0..raster.width {
                assert_eq!(
                    crate::sample(&registry, &source, &recipe, x, y)
                        .unwrap()
                        .rgba,
                    raster.pixel(x, y),
                    "({x}, {y})"
                );
            }
        }

        // Against the same crop without the edit, the difference is one small cluster.
        let mut plain = recipe.clone();
        plain.layers.retain(|layer| layer.effect_id != PIXEL_EFFECT);
        let unedited = render(&registry, &source, SnapshotId::new(), &plain).unwrap();
        assert_eq!(
            (unedited.width, unedited.height),
            (raster.width, raster.height)
        );
        let differing: Vec<(u32, u32)> = (0..raster.height)
            .flat_map(|y| (0..raster.width).map(move |x| (x, y)))
            .filter(|(x, y)| raster.pixel(*x, *y) != unedited.pixel(*x, *y))
            .collect();
        assert!(
            !differing.is_empty(),
            "the resample carries the content edit into the output"
        );
        let span = |axis: fn(&(u32, u32)) -> u32| {
            differing.iter().map(axis).max().unwrap() - differing.iter().map(axis).min().unwrap()
        };
        assert!(
            span(|point| point.0) <= 1 && span(|point| point.1) <= 1,
            "one content pixel blends into its own neighbourhood: {differing:?}"
        );
        assert!(
            differing
                .iter()
                .all(|(x, y)| raster.pixel(*x, *y) != Some([1, 2, 3, 255])),
            "the output shows the resampled blend, not the stored value"
        );

        // The no-op is decided in the content stage, not against what the output shows.
        let at = service.state(&asset).unwrap().revision;
        assert_eq!(
            service
                .apply_pixel(&asset, mutation(at, "same"), 150, 100, [1, 2, 3])
                .unwrap()
                .outcome,
            MutationOutcome::NoOp
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A draft's effective recipe holds the layers its commit stored, apart from the identity the
    /// host gives a newly committed layer. The committed stack is read back from the catalog, whose
    /// JSON parse of a 17-digit double can differ from the value written in its last bit, so the
    /// drafted payloads are compared as they would be stored: through the same JSON text.
    fn same_stack(drafted: &Recipe, committed: &Recipe) {
        let shape = |recipe: &Recipe,
                     payload: &dyn Fn(&Value) -> Value|
         -> Vec<(String, u32, Value, Option<MaskId>)> {
            recipe
                .layers
                .iter()
                .map(|layer| {
                    (
                        layer.effect_id.clone(),
                        layer.effect_format,
                        payload(&layer.payload),
                        layer.mask.clone(),
                    )
                })
                .collect()
        };
        let stored = |payload: &Value| serde_json::from_str(&payload.to_string()).unwrap();
        assert_eq!(shape(drafted, &stored), shape(committed, &Value::clone));
        assert_eq!(drafted.masks, committed.masks);
    }

    /// TASK-014: the workspace `serde_json` (root `Cargo.toml`) enables `float_roundtrip`, so a
    /// stored `f64` reads back bit-exact through a recipe's JSON form — the same `encode`/`decode`
    /// pair every catalog write and read uses (`crate::editor::catalog`). Each of these values'
    /// shortest decimal needs 17 significant digits and, without the feature, parses one ULP off;
    /// the second is a RAW tint-like magnitude (a tint control spans roughly -150..150), matching
    /// the observed bug where a stored tint read back different from the value committed.
    #[test]
    fn recipe_json_round_trips_seventeen_digit_doubles_bit_exact() {
        let values = [
            -3.6837011971450995_f64,
            -124.74988053139269,
            -211.98290340231378,
        ];
        let mut recipe = Recipe::default();
        for (index, value) in values.iter().enumerate() {
            recipe.layers.push(Layer::new(
                "test.float-roundtrip",
                json!({"tint": value, "index": index}),
            ));
        }
        let encoded = crate::editor::catalog::encode(&recipe).unwrap();
        let decoded: Recipe = crate::editor::catalog::decode("recipe", encoded).unwrap();
        assert_eq!(decoded.layers.len(), values.len());
        for (layer, value) in decoded.layers.iter().zip(values.iter()) {
            let read = layer.payload["tint"].as_f64().unwrap();
            assert_eq!(
                read.to_bits(),
                value.to_bits(),
                "tint {value} must read back bit exact, read {read}"
            );
        }
    }

    /// The frame a preview job renders, on this thread.
    fn preview_frame(service: &EditorService, job: &crate::PreviewJob) -> Raster {
        job.source
            .render(
                &service.registry,
                job.entry.snapshot.id.clone(),
                &job.recipe,
            )
            .unwrap()
    }

    /// A drafted Basic edit previews exactly the bytes its commit renders: the draft is planned
    /// through the one function the commit is planned through, with the same fields and target.
    #[test]
    fn a_drafted_basic_preview_equals_the_committed_render_byte_for_byte() {
        let catalog = temp("draft-equals-commit.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let asset = state.asset.id.clone();
        let original = service.render_current(&asset).unwrap();
        // A crop first, so the Basic layer is placed ahead of the geometry tail in both stacks.
        service
            .apply_action(
                &asset,
                mutation(0, "crop"),
                "crop",
                json!({"angle": 3.0, "x": 0.1, "y": 0.1, "width": 0.7, "height": 0.8}),
            )
            .unwrap();
        let revision = service.state(&asset).unwrap().revision;
        let fields = Map::from_iter([
            ("exposure".to_owned(), json!(0.35)),
            ("contrast".to_owned(), json!(12)),
            ("vibrance".to_owned(), json!(-20)),
        ]);
        let mut draft = Draft::new("set-basic", asset.clone(), revision);
        draft.merge(fields.clone());
        let drafted = service
            .preview_job(&asset, None, None, Some(&draft), None)
            .unwrap();
        let drafted_frame = preview_frame(&service, &drafted);

        service
            .apply_action(
                &asset,
                mutation(revision, "commit"),
                "set-basic",
                Value::Object(fields),
            )
            .unwrap();
        let committed = service.state(&asset).unwrap().current_entry.snapshot.recipe;
        same_stack(&drafted.recipe, &committed);
        let committed_frame = service.render_current(&asset).unwrap();
        assert_eq!(
            (drafted_frame.width, drafted_frame.height),
            (committed_frame.width, committed_frame.height)
        );
        assert!(drafted_frame.rgba == committed_frame.rgba, "the same bytes");
        assert!(
            original.rgba != committed_frame.rgba,
            "the edit changed them"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// On a real RAW file: a drafted exposure previews exactly the bytes its commit renders, and a
    /// drafted temperature's effective stack is exactly the one its commit writes, although its
    /// preview approximates that white balance until the release redevelops the mosaic. Run with
    /// LIGHTWELL_RAW_FIXTURE pointing to a private qualified NEF, RAF or DNG.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_drafted_raw_exposure_preview_equals_the_committed_render_byte_for_byte() {
        let path = std::path::PathBuf::from(
            std::env::var("LIGHTWELL_RAW_FIXTURE").expect("LIGHTWELL_RAW_FIXTURE"),
        );
        let catalog = temp("raw-draft-equals-commit.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&path).unwrap();
        let asset = state.asset.id.clone();

        let mut temperature = Draft::new("set-raw-temperature", asset.clone(), state.revision);
        temperature.merge(Map::from_iter([("kelvin".to_owned(), json!(4200.0))]));
        let (drafted_temperature, _) = service.draft_recipe(&asset, &temperature).unwrap();

        let exposure = Map::from_iter([("ev".to_owned(), json!(0.75))]);
        let mut draft = Draft::new("set-raw-exposure", asset.clone(), state.revision);
        draft.merge(exposure.clone());
        let drafted = service
            .preview_job(&asset, None, None, Some(&draft), None)
            .unwrap();
        assert!(
            !drafted.source.approximate_white_balance(),
            "the planes hold the white balance"
        );
        let drafted_frame = preview_frame(&service, &drafted);
        let result = service
            .apply_action(
                &asset,
                mutation(state.revision, "exposure"),
                "set-raw-exposure",
                Value::Object(exposure),
            )
            .unwrap();
        let committed = service.state(&asset).unwrap().current_entry.snapshot.recipe;
        same_stack(&drafted.recipe, &committed);
        let committed_frame = service.render_current(&asset).unwrap();
        assert!(drafted_frame.rgba == committed_frame.rgba, "the same bytes");

        // Undo the exposure, then commit the drafted temperature over the stack it was drafted on.
        service
            .undo(&asset, mutation(result.revision, "undo"))
            .unwrap();
        let at = service.state(&asset).unwrap().revision;
        service
            .apply_action(
                &asset,
                mutation(at, "temperature"),
                "set-raw-temperature",
                json!({"kelvin": 4200.0}),
            )
            .unwrap();
        let committed = service.state(&asset).unwrap().current_entry.snapshot.recipe;
        same_stack(&drafted_temperature, &committed);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
