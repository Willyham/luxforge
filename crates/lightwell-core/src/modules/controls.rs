//! Developer proof for the complete first-slice control vocabulary. Its stored layer describes
//! control values but compiles to an identity colour operation, so it cannot change photo pixels.
use super::{
    ActionInput, ActionPlan, ColorOperation, LayerUpdate, ModuleDescriptor, NewLayer, Processing,
    Stage, StageContext, ToolModule, check_parameters,
};
use crate::{EFFECT_FORMAT, Error};
use serde_json::{Map, Value, json};

pub const CONTROLS_EFFECT: &str = "lightwell.controls.identity";
pub const SET_CONTROLS: &str = "set-controls";
pub const RESET_CONTROLS: &str = "reset-controls";
pub const SAMPLE_CONTROLS_CURVE: &str = "sample-controls-curve";
const SAMPLE_SEGMENTS: usize = 256;

#[derive(Debug)]
pub struct ControlsModule {
    descriptor: ModuleDescriptor,
}

impl Default for ControlsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlsModule {
    pub fn new() -> Self {
        let mut parameters = vec![
            json!({"name":"amount","kind":"number","min":-10.0,"max":10.0,"soft_min":-5.0,"soft_max":5.0,"step":0.1,"fine_step":0.01,"zero":0.0,"precision":2,"default":0.0}),
            json!({"name":"coordinate","kind":"number","min":0.0,"max":100.0,"step":1.0,"precision":0,"default":50.0}),
            json!({"name":"count","kind":"integer","min":0,"max":20,"step":1.0,"default":0}),
            json!({"name":"enabled","kind":"boolean","default":false}),
            json!({"name":"mode","kind":"enum","options":["one","two","three"],"default":"one"}),
            json!({"name":"mode-chips","kind":"enum","options":["one","two","three","four","five"],"default":"one"}),
            json!({"name":"mode-menu","kind":"enum","options":["one","two","three","four","five"],"default":"one"}),
            json!({"name":"rgb","kind":"color","default":[64,128,192]}),
            json!({"name":"rgb-fields","kind":"color","default":[0,0,0]}),
            json!({"name":"master","kind":"curve","points_min":2,"points_max":8,"monotone":true,"step":0.01,"default":[[0.0,0.0],[0.5,0.5],[1.0,1.0]]}),
            json!({"name":"red","kind":"curve","points_min":2,"points_max":8,"monotone":false,"step":0.01,"default":[[0.0,0.0],[0.5,0.5],[1.0,1.0]]}),
        ];
        for parameter in &mut parameters {
            parameter["required"] = json!(false);
            parameter["notes"] =
                json!("Developer control parity fixture; values never alter pixels");
        }
        let query_parameters: Vec<Value> = parameters[9..]
            .iter()
            .cloned()
            .map(|mut parameter| {
                parameter
                    .as_object_mut()
                    .expect("parameter object")
                    .remove("default");
                parameter
            })
            .collect();
        let descriptor = ModuleDescriptor::parse(&json!({
            "id":"lightwell.controls", "title":"Controls", "hint":"Developer control vocabulary",
            "effects":[{"id":CONTROLS_EFFECT,"format":EFFECT_FORMAT,"stage":"color","single":true}],
            "actions":[
                {"id":SET_CONTROLS,"title":"Set controls","notes":"One field patch for every generated control","patch":true,"parameters":parameters},
                {"id":RESET_CONTROLS,"title":"Reset controls","notes":"Clear all proof control values","parameters":[]}
            ],
            "queries":[{"id":SAMPLE_CONTROLS_CURVE,"title":"Sample controls curve","notes":"257 linearly interpolated fractions from the one submitted channel","parameters":query_parameters}],
            "controls":[{"kind":"group","label":"Control vocabulary","reset":{"action":RESET_CONTROLS},"controls":[
                {"kind":"number","action":SET_CONTROLS,"parameter":"amount","label":"Amount","rail":"temperature"},
                {"kind":"number","action":SET_CONTROLS,"parameter":"coordinate","label":"Coordinate","style":"field"},
                {"kind":"number","action":SET_CONTROLS,"parameter":"count","label":"Count","style":"stepper"},
                {"kind":"toggle","action":SET_CONTROLS,"parameter":"enabled","label":"Enabled"},
                {"kind":"choice","action":SET_CONTROLS,"parameter":"mode","label":"Mode","style":"segmented"},
                {"kind":"choice","action":SET_CONTROLS,"parameter":"mode-chips","label":"Mode chips","style":"chips"},
                {"kind":"choice","action":SET_CONTROLS,"parameter":"mode-menu","label":"Mode menu","style":"menu"},
                {"kind":"color","action":SET_CONTROLS,"parameter":"rgb","label":"Colour","style":"picker"},
                {"kind":"color","action":SET_CONTROLS,"parameter":"rgb-fields","label":"RGB fields","style":"fields"},
                {"kind":"curve","action":SET_CONTROLS,"channels":[{"parameter":"master","label":"Master"},{"parameter":"red","label":"Red"}],"label":"Curve","sample_query":SAMPLE_CONTROLS_CURVE,"background":"histogram"},
                {"kind":"action","action":SET_CONTROLS,"label":"Enable","style":"default","preset":{"enabled":true}},
                {"kind":"action","action":SET_CONTROLS,"label":"Mode two","style":"primary","preset":{"mode":"two"}},
                {"kind":"action","action":SET_CONTROLS,"label":"Reset amount","style":"icon","icon":"reset","preset":{"amount":0.0}}
            ]}],
            "reset":{"action":RESET_CONTROLS},
            "developer":true,"availability":{"kind":"available"}
        })).expect("static controls descriptor is valid");
        Self { descriptor }
    }

    fn action(&self) -> &super::ActionDescriptor {
        self.descriptor.action(SET_CONTROLS).expect("static action")
    }

    /// The stored payload contains only fields away from their defaults.
    fn payload(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
    ) -> Result<Map<String, Value>, Error> {
        if effect_id != CONTROLS_EFFECT {
            return Err(Error::incompatible(format!(
                "unavailable effect {effect_id}"
            )));
        }
        if format != EFFECT_FORMAT {
            return Err(Error::incompatible(format!(
                "unsupported effect format {format}"
            )));
        }
        let fields = check_parameters(self.action(), value)?;
        if let Some((name, _)) = fields.iter().find(|(name, value)| {
            self.action()
                .parameter(name)
                .and_then(|parameter| parameter.default.as_ref())
                == Some(*value)
        }) {
            return Err(Error::validation(format!(
                "controls payload stores default field {name}"
            )));
        }
        Ok(fields)
    }
}

impl ToolModule for ControlsModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        if action_id != SET_CONTROLS && action_id != RESET_CONTROLS {
            return Err(Error::validation(format!("unknown action {action_id}")));
        }
        let declared = self.descriptor.action(action_id).expect("static action");
        let checked = check_parameters(declared, &Value::Object(parameters.clone()))?;
        if action_id == SET_CONTROLS && checked.len() != 1 {
            return Err(Error::validation("set-controls requires exactly one field"));
        }
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: checked,
        })
    }

    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        if input.action_id != SET_CONTROLS && input.action_id != RESET_CONTROLS {
            return Err(Error::validation(format!(
                "unknown action {}",
                input.action_id
            )));
        }
        let declared = self
            .descriptor
            .action(&input.action_id)
            .expect("static action");
        let patch = check_parameters(declared, &Value::Object(input.parameters.clone()))?;
        if input.action_id == SET_CONTROLS && patch.len() != 1 {
            return Err(Error::validation("set-controls requires exactly one field"));
        }
        let old = context.own_layer(CONTROLS_EFFECT)?.map(|(_, layer)| layer);
        let mut merged = match old {
            Some(layer) => self.payload(&layer.effect_id, layer.effect_format, &layer.payload)?,
            None => Map::new(),
        };
        if input.action_id == RESET_CONTROLS {
            merged.clear();
        } else {
            for (name, value) in patch {
                let default = self
                    .action()
                    .parameter(&name)
                    .and_then(|parameter| parameter.default.as_ref());
                if default == Some(&value) {
                    merged.remove(&name);
                } else {
                    merged.insert(name, value);
                }
            }
        }
        if old.is_some_and(|layer| layer.payload.as_object() == Some(&merged))
            || (old.is_none() && merged.is_empty())
        {
            return Ok(ActionPlan::NoOp);
        }
        let payload = Value::Object(merged);
        Ok(match old {
            Some(layer) => ActionPlan::Update(LayerUpdate::new(layer.id.clone(), payload)),
            None => ActionPlan::Commit(NewLayer::new(CONTROLS_EFFECT, payload)),
        })
    }

    fn validate_payload(&self, effect_id: &str, format: u32, value: &Value) -> Result<(), Error> {
        self.payload(effect_id, format, value).map(|_| ())
    }

    fn describe_layer(&self, effect_id: &str, format: u32, value: &Value) -> Result<String, Error> {
        let fields = self.payload(effect_id, format, value)?;
        Ok(format!("Controls ({} changed fields)", fields.len()))
    }

    fn values(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
    ) -> Result<Map<String, Value>, Error> {
        let fields = self.payload(effect_id, format, value)?;
        Ok(self
            .action()
            .parameters
            .iter()
            .filter_map(|parameter| {
                fields
                    .get(&parameter.name)
                    .or(parameter.default.as_ref())
                    .map(|value| (parameter.name.clone(), value.clone()))
            })
            .collect())
    }

    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        _: &StageContext<'_>,
    ) -> Result<Value, Error> {
        if query_id != SAMPLE_CONTROLS_CURVE {
            return Err(Error::validation(format!("unknown query {query_id}")));
        }
        let declared = self.descriptor.query(query_id).expect("static query");
        let checked = check_parameters(declared, &Value::Object(parameters.clone()))?;
        let Some((_, value)) = checked.iter().next().filter(|_| checked.len() == 1) else {
            return Err(Error::validation(
                "sample-controls-curve requires exactly one channel parameter",
            ));
        };
        let points = value.as_array().expect("generic curve validation");
        let vertices: Vec<[f64; 2]> = points
            .iter()
            .map(|point| {
                let pair = point.as_array().expect("generic curve validation");
                [
                    pair[0].as_f64().expect("fraction"),
                    pair[1].as_f64().expect("fraction"),
                ]
            })
            .collect();
        let sampled: Vec<Value> = (0..=SAMPLE_SEGMENTS)
            .map(|index| {
                let x = index as f64 / SAMPLE_SEGMENTS as f64;
                let y = if x <= vertices[0][0] {
                    vertices[0][1]
                } else if x >= vertices[vertices.len() - 1][0] {
                    vertices[vertices.len() - 1][1]
                } else {
                    let pair = vertices
                        .windows(2)
                        .find(|pair| x >= pair[0][0] && x <= pair[1][0])
                        .expect("ordered curve spans x");
                    let t = (x - pair[0][0]) / (pair[1][0] - pair[0][0]);
                    pair[0][1] + t * (pair[1][1] - pair[0][1])
                };
                json!([x, y])
            })
            .collect();
        Ok(json!({"points": sampled}))
    }

    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
        _: Stage,
    ) -> Result<Processing, Error> {
        self.payload(effect_id, format, value)?;
        Ok(Processing::Color(ColorOperation::neutral()))
    }
}
