//! The stages the `remove` command and the lab share once segmentation has chosen: the removal
//! region for what was chosen, and the fill, which is LaMa through `fill::remove`.
//!
//! Tests replace LaMa with a stand-in that paints the region with the mean colour of the ring
//! around it, so the lab's flows run without the model; it exists only in test builds.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::fill;
use crate::image::Rgb8;
use crate::mask::{Logits, Mask};
use crate::models;
use crate::refine;
use crate::runtime::{self, Backend, Session};
use crate::stroke::Stroke;

/// One thing to remove.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A segmented object, with the stroke that found it (whose footprint joins the region when
    /// `refine::Params::include_stroke` says so).
    Object {
        logits: Logits,
        stroke: Option<Stroke>,
    },
    /// A stroke that found no object: its own footprint.
    Footprint(Stroke),
}

/// The removal region for every target, joined by pointwise maximum.
///
/// An object's region is `refine::removal_region`; a stroke that found nothing leaves
/// `refine::footprint_region`.
pub fn region(targets: &[Target], image: &Rgb8, params: &refine::Params) -> Mask {
    let size = (image.width, image.height);
    let mut out = Mask::empty(size);
    for target in targets {
        let r = match target {
            Target::Object { logits, stroke } => {
                refine::removal_region(logits, stroke.as_ref(), image, params)
            }
            Target::Footprint(stroke) => footprint_region(stroke, size, params),
        };
        out = refine::union(&out, &r);
    }
    out
}

/// A stroke's footprint as a removal region (`refine::footprint_region`).
pub fn footprint_region(stroke: &Stroke, size: (u32, u32), params: &refine::Params) -> Mask {
    refine::footprint_region(stroke, size, params)
}

/// What a fill reports.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FillReport {
    pub fill: &'static str,
    /// The crop sent to the model, `[x, y, width, height]` in source pixels.
    pub crop: Option<[u32; 4]>,
    pub scale: Option<f32>,
    pub grain_sigma: Option<[f32; 3]>,
    pub stages: Vec<Stage>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stage {
    pub stage: String,
    pub ms: f64,
}

/// The fill a command or the lab runs: LaMa, loaded once.
pub enum Filler {
    Lama {
        model: models::FillModel,
        session: Box<dyn Session>,
        params: fill::Params,
    },
    /// The tests' stand-in for LaMa.
    #[cfg(test)]
    StandIn,
}

impl Filler {
    /// A LaMa export on `backend` from the verified models directory, and its load time.
    pub fn open(
        model: models::FillModel,
        backend: Backend,
        models: &Path,
        params: fill::Params,
    ) -> Result<(Filler, Duration), String> {
        let path = model.model().verified(models)?;
        let (session, load) = runtime::open(backend, &path)?;
        Ok((
            Filler::Lama {
                model,
                session,
                params,
            },
            load,
        ))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Filler::Lama { model, .. } => model.name(),
            #[cfg(test)]
            Filler::StandIn => STAND_IN,
        }
    }

    /// The image with `region` filled; every pixel whose coverage is 0 is byte-identical.
    pub fn run(&mut self, image: &Rgb8, region: &Mask) -> Result<(Rgb8, FillReport), String> {
        match self {
            Filler::Lama {
                session, params, ..
            } => {
                let (out, report) = fill::remove(image, region, session.as_mut(), params)?;
                let r = report.crop;
                Ok((
                    out,
                    FillReport {
                        fill: "lama",
                        crop: Some([r.x, r.y, r.width, r.height]),
                        scale: Some(report.scale),
                        grain_sigma: Some(report.grain_sigma),
                        stages: report
                            .timings_ms
                            .into_iter()
                            .map(|(stage, ms)| Stage { stage, ms })
                            .collect(),
                    },
                ))
            }
            #[cfg(test)]
            Filler::StandIn => Ok((
                stand_in_fill(image, region)?,
                FillReport {
                    fill: STAND_IN,
                    ..FillReport::default()
                },
            )),
        }
    }
}

/// The label the tests' stand-in reports.
#[cfg(test)]
pub const STAND_IN: &str = "test stand-in: ring mean colour";

/// Not a fill: the region blended by its coverage toward the mean colour of the uncovered pixels
/// in a ring around its bounds (a twentieth of their long side, at least 8 px). Pixels with no
/// coverage are untouched.
#[cfg(test)]
pub fn stand_in_fill(image: &Rgb8, region: &Mask) -> Result<Rgb8, String> {
    assert_eq!(
        region.image,
        (image.width, image.height),
        "region of another image"
    );
    let bounds = region.bounds().ok_or("the removal region is empty")?;
    let reach = (bounds.width.max(bounds.height) / 20).max(8);
    let ring = bounds.grown(reach, (image.width, image.height));
    let (mut sum, mut n) = ([0u64; 3], 0u64);
    for y in ring.y..ring.y + ring.height {
        for x in ring.x..ring.x + ring.width {
            if region.get(x, y) == 0 {
                for (s, c) in sum.iter_mut().zip(image.pixel(x, y)) {
                    *s += c as u64;
                }
                n += 1;
            }
        }
    }
    if n == 0 {
        return Err("the region covers the whole image; no colour around it".into());
    }
    let mean = sum.map(|s| (s as f64 / n as f64) as f32);
    let mut out = image.clone();
    for y in bounds.y..bounds.y + bounds.height {
        for x in bounds.x..bounds.x + bounds.width {
            let a = region.get(x, y) as f32 / 255.0;
            if a == 0.0 {
                continue;
            }
            let i = (y as usize * image.width as usize + x as usize) * 3;
            for (o, m) in out.data[i..i + 3].iter_mut().zip(mean) {
                *o = (*o as f32 * (1.0 - a) + m * a).round() as u8;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask::{Point, RectPx};

    fn image() -> Rgb8 {
        let (w, h) = (64u32, 48u32);
        let data = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                [(x * 4) as u8, (y * 5) as u8, ((x + y) * 2) as u8]
            })
            .collect();
        Rgb8::new(w, h, data)
    }

    #[test]
    fn the_stand_in_leaves_uncovered_pixels_byte_identical() {
        let img = image();
        let window = RectPx {
            x: 20,
            y: 10,
            width: 12,
            height: 8,
        };
        let mut data = vec![255u8; 96];
        data[0] = 0;
        data[1] = 100;
        let region = Mask::new((64, 48), window, data);
        let out = stand_in_fill(&img, &region).unwrap();
        for y in 0..48 {
            for x in 0..64 {
                if region.get(x, y) == 0 {
                    assert_eq!(out.pixel(x, y), img.pixel(x, y), "({x}, {y})");
                }
            }
        }
        // Fully covered pixels all take the one ring colour.
        assert_eq!(out.pixel(25, 12), out.pixel(30, 16));
        assert_ne!(out.pixel(25, 12), img.pixel(25, 12));
        assert!(stand_in_fill(&img, &Mask::empty((64, 48))).is_err());
    }

    #[test]
    fn a_footprint_region_is_the_stroke_dilated_as_asked() {
        let stroke = Stroke {
            points: vec![Point::new(20.0, 20.0), Point::new(40.0, 20.0)],
            radius: 3.0,
        };
        let params = refine::Params {
            dilation: Some(2),
            feather: Some(0),
            ..refine::Params::default()
        };
        let r = footprint_region(&stroke, (64, 48), &params);
        assert_eq!(r.get(30, 20), 255);
        // Pixel centres within the radius are rows 17 to 22; the dilation adds two rows.
        assert_eq!(r.get(30, 24), 255);
        assert_eq!(r.get(30, 25), 0);
        let joined = region(&[Target::Footprint(stroke.clone())], &image(), &params);
        assert_eq!(joined, r);
        // By default the footprint is not dilated, only feathered past its edge.
        let plain = footprint_region(&stroke, (64, 48), &refine::Params::default());
        assert_eq!(plain.get(30, 22), 255);
        assert!(plain.get(30, 23) > 0 && plain.get(30, 23) < 255);
        assert_eq!(plain.get(30, 27), 0);
    }
}
