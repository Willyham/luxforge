//! Manual keystone on the fixed covered canvas. The host owns the mapping and resample;
//! the module owns integer field patches and exact orientation carry.
use super::{
    EffectStage, ParameterDescriptor, Processing, Stage,
    field_patch::{Field, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::{Error, Orientation};
use serde_json::{Value, json};

pub const PERSPECTIVE_EFFECT: &str = "luxforge.perspective";
#[derive(Debug, Default)]
pub(crate) struct Perspective;
pub(crate) type PerspectiveModule = FieldPatchModule<Perspective>;
impl FieldPatch for Perspective {
    fn spec() -> Spec {
        Spec::new("luxforge.perspective","Perspective","Keystone correction",PERSPECTIVE_EFFECT,EffectStage::Geometry)
            .order(4).presettable(false)
            .fields([("horizontal","Horizontal"),("vertical","Vertical")].map(|(name,label)|
                Field::new(ParameterDescriptor::integer(name,-100,100).default(0).step(1.0).precision(0).notes("manual keystone amount; positive values compress the right or bottom side"),label)))
            .group(Group::new("Perspective",["horizontal","vertical"]))
            .collapsed()
    }
    fn compile(&self, values: &Values<'_>, at: crate::CompileStage) -> Result<Processing, Error> {
        let stage = at.stage;
        Ok(Processing::Warp(super::WarpStep::projective(
            values.number("horizontal") as i64,
            values.number("vertical") as i64,
            stage,
        )?))
    }
    fn carry(
        &self,
        values: &Values<'_>,
        _: Stage,
        orientation: Orientation,
    ) -> Result<Option<Value>, Error> {
        let mut h = values.number("horizontal") as i64;
        let mut v = values.number("vertical") as i64;
        let base = (h, v);
        if orientation.mirror {
            h = -h;
        }
        for _ in 0..orientation.turns {
            (h, v) = (-v, h);
        }
        if (h, v) == base {
            return Ok(None);
        }
        let mut payload = json!({});
        if h != 0 {
            payload["horizontal"] = json!(h);
        }
        if v != 0 {
            payload["vertical"] = json!(v);
        }
        Ok(Some(payload))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EFFECT_FORMAT, modules::ToolModule};
    #[test]
    fn perspective_is_not_presettable_and_has_integer_limits() {
        let m = PerspectiveModule::new();
        let a = m.descriptor().action("set-perspective").unwrap();
        assert!(a.patch);
        assert!(!a.preset);
        for p in &a.parameters {
            assert_eq!(
                p.kind,
                super::super::ParameterKind::Integer {
                    min: -100,
                    max: 100
                }
            );
        }
        assert!(m.descriptor().collapsed);
    }
    #[test]
    fn perspective_carry_conjugates_all_eight_orientations() {
        let m = PerspectiveModule::new();
        let expected = [
            (false, 0, (40, -25)),
            (false, 1, (25, 40)),
            (false, 2, (-40, 25)),
            (false, 3, (-25, -40)),
            (true, 0, (-40, -25)),
            (true, 1, (25, -40)),
            (true, 2, (40, 25)),
            (true, 3, (-25, 40)),
        ];
        for (mirror, turns, (h, v)) in expected {
            let original = json!({"horizontal":40,"vertical":-25});
            let out = m
                .carry(
                    PERSPECTIVE_EFFECT,
                    EFFECT_FORMAT,
                    &original,
                    Stage {
                        width: 6000,
                        height: 4000,
                    },
                    Orientation { mirror, turns },
                )
                .unwrap()
                .unwrap_or(original);
            assert_eq!(out, json!({"horizontal":h,"vertical":v}));
        }
    }
    #[test]
    fn neutral_perspective_is_exact_geometry() {
        let m = PerspectiveModule::new();
        let s = Stage {
            width: 6000,
            height: 4000,
        };
        assert_eq!(
            m.compile(
                PERSPECTIVE_EFFECT,
                EFFECT_FORMAT,
                &json!({}),
                crate::CompileStage::exact(s)
            )
            .unwrap(),
            Processing::ExactGeometry(super::super::ExactGeometry::identity(s.width, s.height))
        );
    }
}
