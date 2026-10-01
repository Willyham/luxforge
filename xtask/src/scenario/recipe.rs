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

/// The Lens precondition through the same generic messages the panel publishes. A JPEG's detected
/// profile is offered with the acknowledgement its Apply sends, so the script expands the section
/// and presses Apply. A supported RAW is imported with its detected profile already applied as its
/// first-open entry, so it only expands the section. The key comes from the module, so workloads
/// never hard-code a bundled record identity.
pub fn lens_profile(raw: bool) -> Vec<script::Step> {
    let mut steps = vec![script::Step::section("luxforge.lens", true)];
    if !raw {
        steps.push(
            script::ControlsStep::QueryChoiceApply {
                action: "select-lens-profile".into(),
            }
            .into(),
        );
    }
    steps
}

/// A RAW baseline without Lens: the profile its import applied is turned off, keeping the layer.
pub fn lens_off() -> script::Step {
    script::Step::call("edit.reset-lens-profile", json!({}))
}
