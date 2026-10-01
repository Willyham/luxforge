//! Capture sharpening and manual noise reduction, before the pointwise colour run.
//! The independent reference and measured limits are frozen in `docs/design/detail-study.md`.

mod denoise;
mod filters;
mod sharpen;

#[cfg(test)]
mod oracle;

use super::{
    CompileStage, EffectStage, FitSettle, Processing, SpatialOperation, SpatialUnit,
    field_patch::{Field, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::Error;
use std::sync::Arc;

pub const DETAIL_EFFECT: &str = "luxforge.detail.adjust";
const SHARPENING: &str = "sharpening";
const RADIUS: &str = "radius";
const SHARPEN_DETAIL: &str = "sharpen-detail";
const SHARPEN_MASKING: &str = "sharpen-masking";
const LUMINANCE: &str = "luminance";
const LUMINANCE_DETAIL: &str = "luminance-detail";
const COLOUR: &str = "colour";
const COLOUR_DETAIL: &str = "colour-detail";

#[derive(Debug, Default)]
pub(crate) struct Detail;
pub(crate) type DetailModule = FieldPatchModule<Detail>;

fn field(
    name: &'static str,
    label: &str,
    history: &str,
    default: f64,
    max: f64,
    notes: &str,
) -> Field {
    Field::slider(name, label, notes)
        .range(0.0, max)
        .default(default)
        .zero(0.0)
        .history(history)
}

impl FieldPatch for Detail {
    fn spec() -> Spec {
        Spec::new("luxforge.detail", "Detail", "Judge fine detail at 100%", DETAIL_EFFECT, EffectStage::Restoration)
            .maskable().fit_settle(FitSettle::Exact)
            .set_notes("merges the named Detail fields into one layer per target before the colour run; omitted fields keep their stored values; noise reduction precedes capture sharpening; a patch that changes nothing is a reported no-op")
            .fields([
                field(SHARPENING,"Amount","Sharpening",0.0,150.0,"capture-sharpening gain; zero is off"),
                Field::slider(RADIUS,"Radius","Takes effect when Amount is above 0")
                    .range(0.5,3.0).default(1.0).step(0.1).precision(1).unit("px").history("Sharpen radius"),
                field(SHARPEN_DETAIL,"Detail","Sharpen detail",25.0,100.0,"Takes effect when Amount is above 0"),
                field(SHARPEN_MASKING,"Masking","Sharpen masking",0.0,100.0,"Takes effect when Amount is above 0"),
                field(LUMINANCE,"Luminance","Luminance noise",0.0,100.0,"lightness-noise suppression; zero is off"),
                field(LUMINANCE_DETAIL,"Luminance detail","Luminance noise detail",50.0,100.0,"Takes effect when Luminance is above 0"),
                field(COLOUR,"Colour","Colour noise",0.0,100.0,"chroma-noise suppression; zero is off"),
                field(COLOUR_DETAIL,"Colour detail","Colour noise detail",50.0,100.0,"Takes effect when Colour is above 0"),
            ])
            .group(Group::new("Sharpening",[SHARPENING,RADIUS,SHARPEN_DETAIL,SHARPEN_MASKING]))
            .group(Group::new("Noise reduction",[LUMINANCE,LUMINANCE_DETAIL,COLOUR,COLOUR_DETAIL]))
            .collapsed()
    }

    fn compile(&self, values: &Values<'_>, at: CompileStage) -> Result<Processing, Error> {
        if ![at.scale.x, at.scale.y]
            .iter()
            .all(|s| s.is_finite() && *s > 0.0 && *s <= 1.0)
        {
            return Err(Error::resource_limit(
                "Detail sampling scales must be finite and in (0, 1]",
            ));
        }
        let mut units: Vec<Arc<dyn SpatialUnit>> = Vec::with_capacity(2);
        let (luminance, colour) = (values.number(LUMINANCE), values.number(COLOUR));
        if luminance != 0.0 || colour != 0.0 {
            units.push(Arc::new(denoise::Denoise::new(
                luminance,
                values.number(LUMINANCE_DETAIL),
                colour,
                values.number(COLOUR_DETAIL),
                at.scale,
            )));
        }
        let sharpening = values.number(SHARPENING);
        if sharpening != 0.0 {
            units.push(Arc::new(sharpen::Sharpen::new(
                sharpening,
                values.number(RADIUS),
                values.number(SHARPEN_DETAIL),
                values.number(SHARPEN_MASKING),
                at.scale,
            )));
        }
        Ok(Processing::Spatial(SpatialOperation::new(units)?))
    }
}
