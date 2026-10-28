//! Split toning, an example Luxforge adjustment module: the highlights take one hue and the shadows
//! another, each at its own saturation, and the balance moves the point where one gives way to the
//! other. It runs on the colour stage after the colour mixer.
//!
//! The unit works in linear light. A pixel's tonal position is the square root of its Rec. 709
//! luminance, clamped to 0..1; a smooth weight splits it into a highlight share and a shadow share
//! around the balance's pivot. Each share scales the pixel by its tint: the hue's fully saturated
//! colour mixed toward white by the saturation, normalized to unit luminance, so a neutral keeps its
//! luminance and only its colour moves. With both saturations at zero the unit is the identity.
//!
//! `program.wgsl` is the same arithmetic on the GPU. Everything here uses only the basic IEEE
//! operations, so the native build and the component give the same bits.
use luxforge_module_sdk::{Adjustment, Values, Words, export_adjustment, word_f32};

/// Rec. 709 luminance weights of linear sRGB.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// How far a tint at full saturation moves white toward the hue's pure colour.
const STRENGTH: f64 = 0.5;
/// How far the balance moves the pivot from the middle, each way.
const BALANCE_REACH: f64 = 0.45;

/// The words: the highlight and shadow tints less one, the pivot, and the two reciprocals the
/// weight divides by, so neither the reference nor the program divides per pixel.
const WORDS: usize = 9;

pub struct SplitToning;

/// A fully saturated colour of `degrees` on the HSV hexagon.
fn hue(degrees: f64) -> [f64; 3] {
    let mut h = degrees / 60.0;
    if h >= 6.0 {
        h -= 6.0;
    }
    [
        ((h - 3.0).abs() - 1.0).clamp(0.0, 1.0),
        (2.0 - (h - 2.0).abs()).clamp(0.0, 1.0),
        (2.0 - (h - 4.0).abs()).clamp(0.0, 1.0),
    ]
}

/// The factor a tint of `degrees` at `saturation` (0..1) scales a pixel by, less one: white moved
/// toward the hue and normalized to unit luminance.
fn tint(degrees: f64, saturation: f64) -> [f32; 3] {
    let pure = hue(degrees);
    let mixed = pure.map(|channel| 1.0 + saturation * STRENGTH * (channel - 1.0));
    let luminance = f64::from(LUMA[0]) * mixed[0]
        + f64::from(LUMA[1]) * mixed[1]
        + f64::from(LUMA[2]) * mixed[2];
    mixed.map(|channel| (channel / luminance - 1.0) as f32)
}

impl Adjustment for SplitToning {
    fn coefficients(values: &Values) -> Result<Vec<u32>, String> {
        let highlights = tint(
            values.number("highlight-hue")?,
            values.number("highlight-saturation")? / 100.0,
        );
        let shadows = tint(
            values.number("shadow-hue")?,
            values.number("shadow-saturation")? / 100.0,
        );
        let pivot = 0.5 - BALANCE_REACH * values.number("balance")? / 100.0;
        let mut words = Words::new();
        for channel in highlights.into_iter().chain(shadows) {
            words.push_f32(channel);
        }
        words
            .push_f32(pivot as f32)
            .push_f32((1.0 / pivot) as f32)
            .push_f32((1.0 / (1.0 - pivot)) as f32);
        let words = words.into_vec();
        debug_assert_eq!(words.len(), WORDS);
        Ok(words)
    }

    fn apply_row(words: &[u32], _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        let word = |at: usize| word_f32(words, at);
        let highlights = [word(0), word(1), word(2)];
        let shadows = [word(3), word(4), word(5)];
        let (pivot, below, above) = (word(6), word(7), word(8));
        for pixel in rgb {
            let luminance = LUMA[0] * pixel[0] + LUMA[1] * pixel[1] + LUMA[2] * pixel[2];
            let tone = luminance.clamp(0.0, 1.0).sqrt();
            let highlight = if tone <= pivot {
                let t = tone * below;
                0.5 * t * t
            } else {
                let t = (1.0 - tone) * above;
                1.0 - 0.5 * t * t
            };
            let shadow = 1.0 - highlight;
            for channel in 0..3 {
                let factor = 1.0 + highlight * highlights[channel] + shadow * shadows[channel];
                pixel[channel] *= factor;
            }
        }
    }

    fn label(values: &Values) -> Option<String> {
        let number = |name: &str| values.number(name).ok();
        Some(format!(
            "Split toning: highlights {:.0}° {:.0}, shadows {:.0}° {:.0}, balance {:.0}",
            number("highlight-hue")?,
            number("highlight-saturation")?,
            number("shadow-hue")?,
            number("shadow-saturation")?,
            number("balance")?,
        ))
    }
}

export_adjustment!(SplitToning);

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_module_sdk::{Field, Value};

    fn values(hh: f64, hs: f64, balance: f64, sh: f64, ss: f64) -> Values {
        Values::new(vec![
            Field::new("highlight-hue", Value::Number(hh)),
            Field::new("highlight-saturation", Value::Number(hs)),
            Field::new("balance", Value::Number(balance)),
            Field::new("shadow-hue", Value::Number(sh)),
            Field::new("shadow-saturation", Value::Number(ss)),
        ])
    }

    fn apply(values: &Values, rgb: [f32; 3]) -> [f32; 3] {
        let words = SplitToning::coefficients(values).unwrap();
        assert_eq!(words.len(), WORDS);
        let mut row = [rgb];
        SplitToning::apply_row(&words, 0, 0, &mut row);
        row[0]
    }

    fn luminance(rgb: [f32; 3]) -> f32 {
        LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2]
    }

    #[test]
    fn zero_saturation_is_the_identity_whatever_the_hues() {
        let values = values(10.0, 0.0, 60.0, 300.0, 0.0);
        for rgb in [[0.2, 0.5, 0.9], [-0.1, 1.4, 0.0], [0.0; 3], [1.0; 3]] {
            assert_eq!(apply(&values, rgb), rgb);
        }
    }

    #[test]
    fn highlights_take_their_hue_and_shadows_theirs() {
        // Warm highlights, cool shadows.
        let values = values(30.0, 100.0, 0.0, 220.0, 100.0);
        let bright = apply(&values, [0.8; 3]);
        assert!(bright[0] > bright[2], "a bright grey warms: {bright:?}");
        let dark = apply(&values, [0.01; 3]);
        assert!(dark[2] > dark[0], "a dark grey cools: {dark:?}");
    }

    #[test]
    fn a_neutral_keeps_its_luminance() {
        let values = values(30.0, 80.0, 25.0, 220.0, 60.0);
        for grey in [0.02_f32, 0.18, 0.5, 0.9] {
            let toned = apply(&values, [grey; 3]);
            assert!(
                (luminance(toned) - grey).abs() <= 1e-5 * grey.max(1.0),
                "{grey}: {toned:?}"
            );
        }
    }

    #[test]
    fn the_balance_moves_the_split() {
        let toward_highlights = values(30.0, 100.0, 100.0, 220.0, 100.0);
        let toward_shadows = values(30.0, 100.0, -100.0, 220.0, 100.0);
        let grey = [0.18; 3];
        let warm = apply(&toward_highlights, grey);
        let cool = apply(&toward_shadows, grey);
        assert!(warm[0] - warm[2] > cool[0] - cool[2]);
    }

    #[test]
    fn the_label_summarizes_the_values() {
        assert_eq!(
            SplitToning::label(&values(45.0, 30.0, -10.0, 220.0, 20.0)).as_deref(),
            Some("Split toning: highlights 45° 30, shadows 220° 20, balance -10")
        );
    }
}
