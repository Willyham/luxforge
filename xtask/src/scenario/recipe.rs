//! Recipes a timing tool and a scenario both commit, each written once. The timing tools build
//! their scripts from the shared evidence steps directly rather than through a plan, so a step they
//! share with a scenario lives here rather than in either.
use crate::*;
use luxforge_evidence as script;

/// The Presence layer at full strength: every field at 100, so the stack holds all three of its
/// neighbourhood operations. `performance`'s heavy edit and `editor-latency --presence` both commit
/// it.
pub fn full_presence() -> script::Step {
    script::Step::call(
        "edit.set-presence",
        json!({"texture":100.0,"clarity":100.0,"dehaze":100.0}),
    )
}

/// The Tone curve study's moderate S-curve, a real pointwise colour operation.
/// This is an image-processing precondition, separate from the developer curve control proof.
pub const MODERATE_CURVE: [[f64; 2]; 4] = [[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]];

pub fn moderate_curve() -> script::Step {
    script::Step::call("edit.set-curve", json!({"luminance":MODERATE_CURVE}))
}

/// Moderate Detail baseline shared by latency workloads.
pub fn moderate_detail() -> script::Step {
    script::Step::call(
        "edit.set-detail",
        json!({"sharpening":60.0,"luminance":40.0,"colour":40.0}),
    )
}

/// Select the first eligible profile through the same generic messages the list publishes.
/// The key is returned by the module, so workloads never hard-code a bundled record identity.
pub fn lens_profile() -> [script::Step; 3] {
    [
        script::Step::section("luxforge.lens", true),
        script::ControlsStep::QueryChoiceShared {
            action: "select-lens-profile".into(),
            parameter: "assume-uncorrected".into(),
            text: "true".into(),
        }
        .into(),
        script::ControlsStep::QueryChoiceSelectFirst {
            action: "select-lens-profile".into(),
        }
        .into(),
    ]
}
