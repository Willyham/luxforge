//! Independent f64 reference for the removal region of an object artifact
//! (`docs/design/ai-editing.md`, "The removal region"; `docs/design/ai-tools-workspace.md`, "The
//! removal region of an object artifact").
//!
//! A picked object is a selection's 8-bit alpha raster, stretched over the content stage exactly as
//! the model kind stretches it ([`crate::mask::model_alpha`]). Its removal region is the
//! prototype's, with the raster in place of the decoder's prior:
//!
//! ```text
//! texel    = max(W / width, H / height)                    -- one raster texel, in stage pixels
//! cap      = max(round(15 · sqrt(W · H / 1.5e6)), 1)       -- dilation and feather together
//! peak     = max over stage pixels p of model_alpha(p's centre)
//! core     = { stage pixel p : model_alpha(p's centre) >= peak / 2 }
//! feather  = min(max(round(texel), 2), cap)                -- the soft outer edge, one texel
//! r        = clamp(round(texel / 2 + 0.04 · min(core width, core height)), 2, max(cap − feather, 2))
//! d(p)     = the Euclidean distance from p to the nearest core pixel, centre to centre
//! c(p)     = 1                                    where d <= r
//!          = smooth(clamp((r + feather − d) / feather, 0, 1))   elsewhere
//! ```
//!
//! `round` is half away from zero. The core's width and height are those of its bounding box. An
//! opaque object peaks at 1, so its core is where it reaches one half; a soft one keeps the pixels
//! where it reaches half its own peak. A raster that is zero at every stage pixel has no core and
//! no region.
//!
//! This file evaluates the core over the whole stage, and each distance by a search that shares
//! nothing with production's: the least, over the core's rows, of the row distance squared plus the
//! squared horizontal distance to that row's nearest core pixel. A pixel more than `r + feather`
//! from the core's bounding box on either axis is at least that far from every core pixel, so its
//! coverage is zero and its search is skipped. Production computes only inside the raster's
//! non-zero window and must equal this everywhere, zero outside that window included.
use crate::mask::{Model, Stage, model_alpha, smooth};

/// The removal region of `model`'s raster over `stage`: one coverage in `[0, 1]` a stage pixel,
/// row-major over the whole stage. Every value is zero when the raster has no core.
pub fn object_region(model: &Model, stage: &Stage) -> Vec<f64> {
    let (w, h) = (stage.width as usize, stage.height as usize);
    let height = f64::from(stage.height);
    let alpha: Vec<f64> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let u = (x as f64 + 0.5) / height;
            let v = (y as f64 + 0.5) / height;
            model_alpha(model, stage, u, v)
        })
        .collect();
    let peak = alpha.iter().copied().fold(0.0, f64::max);
    let mut out = vec![0.0; w * h];
    if peak <= 0.0 {
        return out;
    }
    let mut core = vec![false; w * h];
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if alpha[y * w + x] >= peak / 2.0 {
                core[y * w + x] = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 == usize::MAX {
        return out;
    }
    let texel = (f64::from(stage.width) / f64::from(model.width))
        .max(f64::from(stage.height) / f64::from(model.height));
    let cap = (15.0 * (f64::from(stage.width) * f64::from(stage.height) / 1.5e6).sqrt())
        .round()
        .max(1.0);
    let feather = texel.round().max(2.0).min(cap);
    let shorter = ((x1 - x0 + 1).min(y1 - y0 + 1)) as f64;
    let r = (0.5 * texel + 0.04 * shorter)
        .round()
        .clamp(2.0, (cap - feather).max(2.0));
    let reach = r + feather;

    // For each core row, every column's horizontal distance to that row's nearest core pixel, by
    // a scan from each side.
    let rows: Vec<(usize, Vec<f64>)> = (y0..=y1)
        .filter_map(|j| {
            let line = &core[j * w..(j + 1) * w];
            if !line.contains(&true) {
                return None;
            }
            let mut nearest = vec![f64::INFINITY; w];
            let mut last = None;
            for x in 0..w {
                if line[x] {
                    last = Some(x);
                }
                if let Some(c) = last {
                    nearest[x] = (x - c) as f64;
                }
            }
            last = None;
            for x in (0..w).rev() {
                if line[x] {
                    last = Some(x);
                }
                if let Some(c) = last {
                    nearest[x] = nearest[x].min((c - x) as f64);
                }
            }
            Some((j, nearest))
        })
        .collect();
    let beyond = reach.ceil() as usize;
    let (bx0, by0) = (x0.saturating_sub(beyond), y0.saturating_sub(beyond));
    let (bx1, by1) = ((x1 + beyond).min(w - 1), (y1 + beyond).min(h - 1));
    for y in by0..=by1 {
        for x in bx0..=bx1 {
            let d2 = rows
                .iter()
                .map(|(j, nearest)| {
                    let dy = y.abs_diff(*j) as f64;
                    dy * dy + nearest[x] * nearest[x]
                })
                .fold(f64::INFINITY, f64::min);
            let d = d2.sqrt();
            out[y * w + x] = if d <= r {
                1.0
            } else {
                smooth(((reach - d) / feather).clamp(0.0, 1.0))
            };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn model(width: u32, height: u32, codes: Vec<u8>) -> Model {
        Model {
            width,
            height,
            alpha: Arc::from(codes),
            threshold: 50.0,
            feather: 100.0,
        }
    }

    #[test]
    fn a_soft_raster_keeps_its_core_at_half_its_peak_and_an_empty_one_has_no_region() {
        // A faint centre of 100 over a ring of 40: the core is where it reaches 50, not one half.
        let mut codes = vec![40; 8 * 8];
        for j in 3..5 {
            for i in 3..5 {
                codes[j * 8 + i] = 100;
            }
        }
        let region = object_region(&model(8, 8, codes), &Stage::new(8, 8));
        assert_eq!(region[3 * 8 + 3], 1.0);
        assert!(region.iter().any(|&c| c < 1.0));
        let empty = model(4, 4, vec![0; 16]);
        assert!(
            object_region(&empty, &Stage::new(8, 8))
                .iter()
                .all(|&c| c == 0.0)
        );
    }

    #[test]
    fn the_region_is_full_out_to_the_dilation_and_falls_to_zero_over_the_feather() {
        // One texel a pixel: the core is the single opaque pixel at (10, 10); the cap at this size
        // is 1, so the feather is 1 px and the dilation its floor of 2 px.
        let mut codes = vec![0; 21 * 21];
        codes[10 * 21 + 10] = 255;
        let region = object_region(&model(21, 21, codes), &Stage::new(21, 21));
        let at = |x: usize, y: usize| region[y * 21 + x];
        assert_eq!(at(10, 10), 1.0);
        assert_eq!(at(12, 10), 1.0);
        assert_eq!(at(11, 11), 1.0);
        // sqrt(5) is past r = 2 and within the 1 px feather; 3 px is past both.
        assert!(at(12, 11) > 0.0 && at(12, 11) < 1.0);
        assert_eq!(at(13, 10), 0.0);
    }
}
