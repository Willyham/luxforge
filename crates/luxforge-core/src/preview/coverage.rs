//! One mask's coverage over a whole evaluated stage on a small grid, and the identity of what that
//! grid depends on: the Masks panel's per-mask thumbnail.
//!
//! The grid is the one [`mask_overlay_for`] fills for the preview's overlay — the same compiled
//! mask, the same geometry tail and, for a mask that reads pixels, the same input of its first
//! bound layer — asked for over the whole output stage at the caller's cell count. It reads no pixel
//! of any rendered frame and allocates only the cells, so a thumbnail costs no render.
//!
//! **The key is what makes it cheap to keep.** A thumbnail is asked for every mask after every
//! settled frame, and almost every such frame leaves most masks exactly as they were. The key hashes
//! everything the grid is a function of — the source's identity, the geometry tail, the mask itself
//! and, only when the mask reads pixels, the stack before its first bound layer with the masks that
//! stack applies through — so a caller holding the key of the grid it already has learns in
//! `O(recipe)` and without filling a cell that nothing changed.
use super::{MaskOverlayOutcome, MaskOverlayRequest, PreviewSource, worker::mask_overlay_for};
use crate::{Cancel, Error, Evaluation, MaskId, mask::CompiledMask, modules::Stage};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// One mask's coverage grid, or the key alone when the caller already holds the grid it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskCoverage {
    /// The identity of everything the grid is a function of. Equal keys are equal grids within
    /// one process; it is never persisted.
    pub key: u64,
    /// The grid, or the host's reason there is none, exactly as the overlay would carry it. `None`
    /// when the caller's key matched and nothing was filled.
    pub outcome: Option<MaskOverlayOutcome>,
}

/// Serialized and formatted values written straight into a hasher, so a key allocates nothing.
struct Sink<'a>(&'a mut DefaultHasher);

impl std::io::Write for Sink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::fmt::Write for Sink<'_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0.write(text.as_bytes());
        Ok(())
    }
}

/// Hash one serializable value by its canonical JSON, the form a recipe is hashed in for its
/// analysis identity.
fn hash_json(hasher: &mut DefaultHasher, value: &impl serde::Serialize) -> Result<(), Error> {
    serde_json::to_writer(Sink(hasher), value).map_err(|error| {
        Error::internal(format!(
            "a mask's coverage key could not be serialized: {error}"
        ))
    })?;
    // A separator, so two adjacent values cannot hash as one.
    0xffu8.hash(hasher);
    Ok(())
}

impl Evaluation {
    /// `mask`'s composed coverage over this evaluation's whole output stage, on a `cells_w ×
    /// cells_h` grid, unless `cached` is already the key of that grid.
    ///
    /// The grid is the overlay's own ([`mask_overlay_for`]), so a thumbnail and the overlay cannot
    /// disagree about a cell. A mask that reads pixels is answered on the input of its first bound
    /// layer, and has no grid — with the host's reason in [`MaskOverlayOutcome::absent`] — when no
    /// layer is bound to it or the stack before that layer holds a spatial one.
    ///
    /// Cost: `O(recipe)` to key it, which composes the geometry tail and compiles the one mask but
    /// reads no pixel; on a miss, `O(cells × components)` plus one point query per cell through the
    /// prefix for a mask that reads pixels. A cancel ends it with [`crate::ErrorKind::Cancelled`],
    /// never with an absent grid a caller could mistake for an answer.
    pub fn mask_coverage(
        &self,
        mask: &MaskId,
        (cells_w, cells_h): (u32, u32),
        cached: Option<u64>,
        cancel: &Cancel,
    ) -> Result<MaskCoverage, Error> {
        let recipe = self.recipe();
        let held = recipe
            .masks
            .iter()
            .find(|held| &held.id == mask)
            .ok_or_else(|| {
                Error::validation(format!(
                    "mask {mask} is not in the stack this evaluation holds"
                ))
            })?;
        // Lazy: it borrows the one compilation and reads no pixel until asked for one.
        let frame = self.exact(cancel)?;
        let transform = frame.transform()?;
        let stage = Stage {
            width: transform.content.width,
            height: transform.content.height,
        };
        let compiled = CompiledMask::new(held, stage, &recipe.strokes)?;
        let mut hasher = DefaultHasher::new();
        std::fmt::write(
            &mut Sink(&mut hasher),
            format_args!("{:?}", self.source().identity()),
        )
        .map_err(|_| Error::internal("a source identity could not be formatted"))?;
        (
            transform.content.width,
            transform.content.height,
            transform.output.width,
            transform.output.height,
        )
            .hash(&mut hasher);
        transform.forward.map(f64::to_bits).hash(&mut hasher);
        (cells_w, cells_h).hash(&mut hasher);
        hash_json(&mut hasher, held)?;
        // A position-only mask is a function of its own geometry and the stage alone. A mask that
        // reads pixels is also a function of the pixel its first bound layer receives: the stack
        // before that layer, the masks that stack applies through and the settings a RAW source is
        // evaluated under.
        compiled.reads_pixels().hash(&mut hasher);
        if compiled.reads_pixels() {
            match crate::mask::commands::input_layer_index(recipe, mask) {
                Ok(index) => {
                    index.hash(&mut hasher);
                    for layer in &recipe.layers[..index] {
                        hash_json(&mut hasher, layer)?;
                        if let Some(bound) = layer.mask.as_ref().and_then(|bound| {
                            recipe.masks.iter().find(|candidate| &candidate.id == bound)
                        }) {
                            hash_json(&mut hasher, bound)?;
                        }
                    }
                    if let PreviewSource::Raw { settings, .. } = self.source() {
                        std::fmt::write(&mut Sink(&mut hasher), format_args!("{settings:?}"))
                            .map_err(|_| Error::internal("RAW settings could not be formatted"))?;
                    }
                }
                Err(error) => error.detail.hash(&mut hasher),
            }
        }
        let key = hasher.finish();
        if cached == Some(key) {
            return Ok(MaskCoverage { key, outcome: None });
        }
        let request = MaskOverlayRequest {
            mask: mask.clone(),
            component: None,
            cells_w,
            cells_h,
            whole_cells_w: cells_w,
            whole_cells_h: cells_h,
        };
        let outcome = mask_overlay_for(
            self.registry(),
            &frame,
            recipe,
            &request,
            None,
            cancel,
            self.context(),
        );
        // The overlay reports a cancel as an absence with no reason, which is right for a frame a
        // newer one replaces and wrong for a grid a caller would keep: say it was cancelled.
        cancel.check()?;
        Ok(MaskCoverage {
            key,
            outcome: Some(outcome),
        })
    }
}
