//! What the family's tests share: the built-in registry, planning without a colour read, the
//! two gradients' parameters and a stack holding one created mask.
use super::plan::MaskChange;
use super::{MaskCommand, MaskOutcome, MaskTarget, find};
use crate::{Error, ModuleRegistry, Recipe};
use serde_json::{Map, Value, json};

pub(super) fn registry() -> ModuleRegistry {
    ModuleRegistry::builtin()
}

/// Plan a command that needs no colour read, which is every command here: only a stroke asking to
/// be limited to a colour takes a seed, and the host reads that one — so the tests that cover it
/// call [`super::plan`] with the seed themselves.
pub(super) fn plan(
    command: &MaskCommand,
    recipe: &Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
    registry: &ModuleRegistry,
) -> Result<MaskOutcome, Error> {
    super::plan(command, recipe, target, parameters, registry, None)
}

pub(super) fn linear(x0: f64, y0: f64, x1: f64, y1: f64) -> Map<String, Value> {
    let mut parameters = Map::new();
    for (name, value) in [("x0", x0), ("y0", y0), ("x1", x1), ("y1", y1)] {
        parameters.insert(name.into(), json!(value));
    }
    parameters
}

pub(super) fn radial(x: f64, y: f64, radius: f64, feather: f64) -> Map<String, Value> {
    let mut parameters = Map::new();
    for (name, value) in [
        ("x", x),
        ("y", y),
        ("radius_x", radius),
        ("radius_y", radius),
        ("angle", 0.0),
        ("feather", feather),
    ] {
        parameters.insert(name.into(), json!(value));
    }
    parameters
}

pub(super) fn apply(
    recipe: &Recipe,
    method: &str,
    target: MaskTarget,
    parameters: Map<String, Value>,
) -> Result<MaskChange, Error> {
    let command = find(method).expect("a declared command");
    match plan(command, recipe, &target, &parameters, &registry())? {
        MaskOutcome::NoOp => panic!("{method} changed nothing"),
        MaskOutcome::Change(change) => Ok(change),
    }
}

pub(super) fn created() -> (Recipe, MaskChange) {
    let recipe = Recipe::default();
    let change = apply(
        &recipe,
        "mask.create-linear",
        MaskTarget::default(),
        linear(0.0, 0.0, 0.0, 1.0),
    )
    .unwrap();
    (change.recipe.clone(), change)
}
