//! Prove the current module authoring boundary from outside the core crate.
use luxforge_core::{
    ActionDescriptor, ActionInput, ActionPlan, ColorOperation, CompileStage, EFFECT_FORMAT,
    EffectDescriptor, EffectStage, Error, Layer, LayerReport, LayerUpdate, Mask, ModuleDescriptor,
    ModuleRegistry, NewLayer, Processing, SourceTag, Stage, StageContext, StageQuestions,
    ToolModule,
};
use serde_json::{Map, Value, json};
use std::{cell::RefCell, sync::Arc};

const EFFECT: &str = "luxforge.author.colour";
struct Author(ModuleDescriptor);
impl Author {
    fn new() -> Self {
        Self(ModuleDescriptor {
            id: "luxforge.author".into(),
            title: "Author".into(),
            effects: vec![EffectDescriptor {
                maskable: true,
                single: true,
                ..EffectDescriptor::new(EFFECT, EffectStage::Color)
            }],
            actions: vec![ActionDescriptor::new("author-set", "Set author", "test")],
            ..ModuleDescriptor::default()
        })
    }
}
impl ToolModule for Author {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.0
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: parameters.clone(),
        })
    }
    fn plan(&self, _: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let own = context.own_layer(EFFECT)?;
        let index = own.map_or_else(|| context.insertion_index_for(EFFECT), |(index, _)| index);
        let stage = context.stage_before(index)?;
        let payload = json!({"width": stage.width});
        Ok(match own {
            Some((_, layer)) => ActionPlan::Update(LayerUpdate::new(layer.id.clone(), payload)),
            None => ActionPlan::Commit(NewLayer::new(EFFECT, payload)),
        })
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe(&self, _: &str, _: u32, _: &Value) -> Result<LayerReport, Error> {
        Ok(LayerReport::new("author"))
    }
    fn compile(&self, _: &str, _: u32, _: &Value, _: CompileStage) -> Result<Processing, Error> {
        Ok(Processing::Color(ColorOperation::new(Vec::new())))
    }
}
struct Questions(RefCell<Vec<usize>>);
impl StageQuestions for Questions {
    fn stage_before(&self, index: usize) -> Result<Stage, Error> {
        self.0.borrow_mut().push(index);
        Ok(Stage {
            width: 10 + index as u32,
            height: 20,
        })
    }
    fn sample_before(&self, _: usize, _: u32, _: u32) -> Result<Option<[u8; 4]>, Error> {
        Ok(Some([1, 2, 3, 255]))
    }
    fn sensor_neutral(&self, _: u32, _: u32) -> Result<[f32; 3], Error> {
        Ok([1., 2., 3.])
    }
}

#[test]
fn public_authoring_uses_target_ownership_placement_and_lazy_questions() {
    let author = Arc::new(Author::new());
    let mut registry = ModuleRegistry::default();
    registry.register(author.clone()).unwrap();
    let masks = [Mask::new("first"), Mask::new("second")];
    let mut layers = vec![Layer::new(EFFECT, json!({})), Layer::new(EFFECT, json!({}))];
    layers[1].mask = Some(masks[1].id.clone());
    let questions = Questions(RefCell::new(Vec::new()));
    let context = StageContext {
        layers: &layers,
        registry: &registry,
        target: Some(&masks[0].id),
        kind: SourceTag::Jpeg,
        masks: &masks,
        questions: &questions,
    };
    assert!(context.own_layer(EFFECT).unwrap().is_none());
    assert_eq!(context.insertion_index_for(EFFECT), 1);
    assert!(questions.0.borrow().is_empty());
    let input = author.parse("author-set", &Map::new()).unwrap();
    assert_eq!(
        author.plan(&input, &context).unwrap(),
        ActionPlan::Commit(NewLayer::new(EFFECT, json!({"width":11})))
    );
    assert_eq!(*questions.0.borrow(), [1]);
    let context = StageContext {
        target: Some(&masks[1].id),
        ..context
    };
    assert_eq!(
        author.plan(&input, &context).unwrap(),
        ActionPlan::Update(LayerUpdate::new(layers[1].id.clone(), json!({"width":11})))
    );
    assert_eq!(
        context.stage().unwrap(),
        Stage {
            width: 12,
            height: 20
        }
    );
    assert_eq!(
        context.sample_before(0, 0, 0).unwrap(),
        Some([1, 2, 3, 255])
    );
    assert!(context.input_before(0, 0, 0).unwrap().is_some());
    assert_eq!(context.sensor_neutral(0, 0).unwrap(), [1., 2., 3.]);
    assert_eq!(author.descriptor().effects[0].format, EFFECT_FORMAT);
    let mut ambiguous = layers.clone();
    ambiguous.push(layers[1].clone());
    let context = StageContext {
        layers: &ambiguous,
        ..context
    };
    assert!(context.own_layer(EFFECT).is_err());
}
