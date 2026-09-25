//! The mask family's structural rules, each written once as a function of the facts `mask.list`
//! reports, and each returning the host's own refusal.
//!
//! The `mask.*` commands refuse through these functions, and a client that wants to say *before* a
//! click why a control is not offered calls the same functions with the same facts from its listing —
//! a mask's name and component count, a component's mode and position, a stroke or sample count,
//! whether a layer is bound to the mask. So what a panel states and what the command answers are one
//! rule in one spelling, by construction rather than by a second copy kept in step: a client that
//! sends the command anyway gets exactly the sentence the panel showed. None of them reads a pixel, a
//! recipe or the catalog; each is `O(1)` or `O(components)`.
use crate::{
    ComponentMode, Error, ErrorKind,
    model::{COMPONENTS_PER_MASK, MASKS_PER_RECIPE},
};

/// The three modes a component can take, in the order the host declares them: the `mode` parameter
/// every mode-taking command declares is generated from this list.
pub const MODES: [ComponentMode; 3] = [
    ComponentMode::Add,
    ComponentMode::Subtract,
    ComponentMode::Intersect,
];

/// The composition's one structural rule, as the host words it wherever it states it.
pub const FIRST_COMPONENT_IS_ADD: &str = "the first component of a mask is always add";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

/// A declared mode token as the mode it names.
pub fn mode(token: &str) -> Result<ComponentMode, Error> {
    MODES
        .into_iter()
        .find(|mode| mode.as_str() == token)
        .ok_or_else(|| validation(format!("unknown component mode {token}")))
}

/// A component kind this build cannot evaluate. `Incompatible` and not `Validation`: the stored
/// stack is well formed and this build simply cannot draw part of it, so the component is kept byte
/// for byte and every edit to it is refused in these words.
pub fn unknown_kind(kind: &str) -> Error {
    Error::new(
        ErrorKind::Incompatible,
        format!("unknown mask component {kind}"),
    )
}

/// Room for one more mask in a recipe that holds `masks` of them: what `mask.create-<kind>`,
/// `mask.duplicate` and a stroke that draws a new mask refuse with at the limit.
pub fn room_for_mask(masks: usize) -> Result<(), Error> {
    if masks >= MASKS_PER_RECIPE {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "recipe already has {MASKS_PER_RECIPE} masks; the limit is {MASKS_PER_RECIPE} masks per recipe"
            ),
        ));
    }
    Ok(())
}

/// Room for one more component in mask `mask`, which holds `components` of them: what
/// `mask.add-<kind>` and a stroke that puts a new brush on a mask refuse with at the limit.
pub fn room_for_component(mask: &str, components: usize) -> Result<(), Error> {
    if components >= COMPONENTS_PER_MASK {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "mask {mask} has {COMPONENTS_PER_MASK} components; the limit is {COMPONENTS_PER_MASK} components per mask"
            ),
        ));
    }
    Ok(())
}

/// Whether a component of `mode` may be a mask's first: only an add, because nothing precedes the
/// first component to subtract from or intersect with. A new mask's first component is therefore
/// always an add, and a client whose next component is set to another mode says so rather than
/// creating an add in its place.
pub fn may_lead(mode: ComponentMode) -> bool {
    mode == ComponentMode::Add
}

/// Whether a mask may lead with a component of `mode`. Nothing precedes the first component, so it
/// can only add to an empty coverage: a mask that would begin by subtracting or intersecting is
/// refused as it stands, with the mode named. Every command that can change which component leads —
/// a mode change, a reorder, a delete, an add to an empty mask — reaches this through the mask's own
/// structural check.
pub fn leading(mask: &str, mode: ComponentMode) -> Result<(), Error> {
    if !may_lead(mode) {
        return Err(validation(format!(
            "mask {mask} begins with a {} component; {FIRST_COMPONENT_IS_ADD}",
            mode.as_str()
        )));
    }
    Ok(())
}

/// A destination index inside a list that currently holds `len` items, naming what it counted.
pub fn position(index: u64, len: usize, what: &str) -> Result<usize, Error> {
    let index = usize::try_from(index).unwrap_or(usize::MAX);
    if index >= len {
        return Err(validation(format!(
            "index {index} is outside the {len} {what} of this stack"
        )));
    }
    Ok(index)
}

/// Moving the component at `from` of mask `mask` to `to`, given every component's mode in order:
/// the destination has to be inside the list, and whichever component ends up first has to be an
/// add.
pub fn reorder_component(
    mask: &str,
    modes: &[ComponentMode],
    from: usize,
    to: u64,
) -> Result<(), Error> {
    let to = position(to, modes.len(), "components")?;
    let leading = if to == 0 {
        modes.get(from)
    } else if from == 0 {
        modes.get(1)
    } else {
        modes.first()
    };
    match leading {
        Some(mode) => self::leading(mask, *mode),
        None => Ok(()),
    }
}

/// Deleting one component of mask `mask`, which holds `components`. A mask never exists empty from a
/// command, so its last component is removed by deleting the mask, which says what it removed.
pub fn delete_component(mask: &str, components: usize) -> Result<(), Error> {
    if components == 1 {
        return Err(validation(format!(
            "mask {mask} has one component; delete the mask rather than its last component"
        )));
    }
    Ok(())
}

/// Deleting stroke `stroke` from component `component`, which holds `strokes`. A component with no
/// stroke covers nothing and is not a thing a person drew, so its last stroke goes by removing the
/// component — the same rule, and the same wording, that keeps a mask from existing empty.
pub fn delete_stroke(stroke: &str, component: &str, strokes: usize) -> Result<(), Error> {
    if strokes == 1 {
        return Err(validation(format!(
            "stroke {stroke} is the only stroke of {component}; delete the component instead"
        )));
    }
    Ok(())
}

/// Room for one more sampled colour in component `component` of `kind`, which holds `held` of the
/// `limit` its kind allows.
pub fn room_for_sample(
    component: &str,
    kind: &str,
    held: usize,
    limit: usize,
) -> Result<(), Error> {
    if held >= limit {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "component {component} already holds {limit} sampled colours; the limit is \
                 {limit} per {kind} component"
            ),
        ));
    }
    Ok(())
}

/// Reading the pixel the operation mask `mask` modulates receives — a pick into one of its
/// components, a stroke held to a colour — needs such an operation, so a mask no layer is bound to
/// is refused by name rather than answered from the source or the finished frame.
pub fn bound_layer(mask: &str, bound: bool) -> Result<(), Error> {
    if !bound {
        return Err(validation(format!(
            "no layer is bound to mask {mask}, and reading the pixel an operation receives \
             needs an operation; apply an adjustment through {mask} first"
        )));
    }
    Ok(())
}

/// A stroke that draws a new mask cannot be held to a colour: a new mask is bound to no layer yet,
/// so there is no operation whose input the limit could read.
pub fn limit_on_new_mask() -> Error {
    validation(
        "a stroke that draws a new mask cannot be limited to a colour: the limit reads the pixel \
         the operation the mask modulates receives, and a new mask is bound to no layer yet. \
         Paint the mask, apply an adjustment through it, then limit the strokes after that",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reorder is refused exactly when the component it leaves first is not an add, and the
    /// refusal is the leading rule's own; a destination outside the list is refused with the count.
    #[test]
    fn a_reorder_is_refused_by_the_component_it_would_leave_leading() {
        use ComponentMode::{Add, Subtract};
        let modes = [Add, Subtract, Add];
        assert!(reorder_component("Mask 1", &modes, 2, 0).is_ok());
        let refused = reorder_component("Mask 1", &modes, 1, 0).unwrap_err();
        assert_eq!(
            refused.detail,
            "mask Mask 1 begins with a subtract component; the first component of a mask is always add"
        );
        // Moving the leading add away leaves the subtract behind it first.
        assert!(reorder_component("Mask 1", &modes, 0, 2).is_err());
        assert!(reorder_component("Mask 1", &[Add, Add, Subtract], 0, 1).is_ok());
        assert_eq!(
            reorder_component("Mask 1", &modes, 1, 3)
                .unwrap_err()
                .detail,
            "index 3 is outside the 3 components of this stack"
        );
    }

    #[test]
    fn every_declared_mode_token_names_its_mode() {
        for known in MODES {
            assert_eq!(mode(known.as_str()).unwrap(), known);
        }
        assert_eq!(
            mode("exclude").unwrap_err().detail,
            "unknown component mode exclude"
        );
    }
}
