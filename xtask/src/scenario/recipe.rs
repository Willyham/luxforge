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
