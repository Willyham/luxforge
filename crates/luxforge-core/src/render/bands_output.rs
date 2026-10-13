//! An export's output from the reference renderer's banded mode (`docs/design/export-settings.md`,
//! "Paged sources"): what [`super::bands::Banded`] renders of a paged source's output stage, band by
//! band, as the bands of the export's [`OutputSpec`].
use super::bands::Banded;
use crate::{
    Cancel, Error,
    tiles::{OutputSpec, Pixels},
};

/// The bands of an export's output, from the top, each its row count and its pixels at the
/// output's depth.
pub(crate) type OutputBands<'a> = Box<dyn Iterator<Item = Result<(u32, Pixels), Error>> + 'a>;

/// `output` of `banded`'s output stage, in bands from the top. The full-size 8-bit output is the
/// banded mode's own bands, each the whole render's rows byte for byte.
pub(crate) fn output_bands<'b>(
    banded: &'b Banded<'_>,
    output: OutputSpec,
    _cancel: &'b Cancel,
) -> Result<OutputBands<'b>, Error> {
    let stage = banded.stage();
    output.check(stage.width, stage.height)?;
    if output.is_exact_frame(stage.width, stage.height) {
        return Ok(Box::new(banded.bands().map(|band| {
            band.map(|(rows, rgba)| (rows, Pixels::Eight(rgba)))
        })));
    }
    Err(Error::resource_limit(
        "a paged source is exported in bands only at its full size, at 8 bits and unsharpened",
    ))
}
