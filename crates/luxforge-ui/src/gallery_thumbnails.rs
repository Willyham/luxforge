//! Stand-in photographs for the gallery's Select states: small generated scenes (a sky, a
//! horizon, water or a field, a sun and a dark shore) as image handles made once and held for the
//! life of the process.
//!
//! The gallery rebuilds its states in every `view()`, and an image handle made there would carry a
//! new id each time, so Iced would upload it again on every frame. Every handle here is made once,
//! in a `LazyLock`, and cloned, which keeps its id — exactly the rule the app's own grid follows
//! with the previews it holds.

// Removed once the Select gallery states draw these (lane D phase 0).
#![allow(dead_code)]

use iced::widget::image::Handle;
use std::sync::LazyLock;

/// How many distinct scenes [`thumbnail`] cycles through.
pub(crate) const SCENES: usize = 12;

/// One generated scene: its size and palette.
struct Scene {
    width: u32,
    height: u32,
    /// Sky at the top and at the horizon, and the ground or water below it.
    sky: [f32; 3],
    haze: [f32; 3],
    ground: [f32; 3],
    /// Where the horizon sits, as a fraction of the height.
    horizon: f32,
    /// The sun's centre as fractions of the width and height, and its radius as a fraction of
    /// the height.
    sun: (f32, f32, f32),
}

const fn rgb(r: u8, g: u8, b: u8) -> [f32; 3] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

/// The scenes: mostly 3:2 landscapes, one 4:3 and one portrait, so a grid shows fitted previews of
/// more than one shape.
#[rustfmt::skip]
const PALETTES: [(u32, u32, [f32; 3], [f32; 3], [f32; 3], f32, (f32, f32, f32)); SCENES] = [
    (240, 160, rgb(58, 104, 168), rgb(170, 196, 222), rgb(38, 64, 104), 0.55, (0.7, 0.25, 0.07)),
    (240, 160, rgb(92, 140, 196), rgb(214, 222, 226), rgb(78, 128, 52), 0.5, (0.25, 0.2, 0.06)),
    (240, 160, rgb(34, 44, 88), rgb(236, 150, 92), rgb(40, 34, 44), 0.62, (0.5, 0.58, 0.09)),
    (240, 180, rgb(120, 150, 180), rgb(200, 206, 208), rgb(96, 104, 86), 0.45, (0.8, 0.18, 0.05)),
    (160, 240, rgb(70, 118, 170), rgb(190, 210, 224), rgb(62, 92, 48), 0.6, (0.3, 0.2, 0.06)),
    (240, 160, rgb(150, 170, 188), rgb(228, 230, 228), rgb(180, 172, 150), 0.52, (0.6, 0.3, 0.04)),
    (240, 160, rgb(40, 76, 130), rgb(140, 186, 214), rgb(26, 74, 96), 0.48, (0.15, 0.3, 0.05)),
    (240, 160, rgb(96, 120, 150), rgb(168, 176, 170), rgb(110, 124, 74), 0.4, (0.85, 0.15, 0.05)),
    (240, 160, rgb(210, 120, 80), rgb(246, 200, 140), rgb(64, 50, 58), 0.66, (0.4, 0.62, 0.1)),
    (240, 160, rgb(64, 110, 162), rgb(196, 214, 230), rgb(92, 150, 64), 0.58, (0.55, 0.22, 0.06)),
    (240, 160, rgb(24, 30, 52), rgb(64, 78, 120), rgb(20, 22, 30), 0.6, (0.75, 0.3, 0.03)),
    (240, 160, rgb(110, 150, 190), rgb(220, 226, 232), rgb(120, 98, 70), 0.5, (0.35, 0.28, 0.05)),
];

fn scene(index: usize) -> Scene {
    let (width, height, sky, haze, ground, horizon, sun) = PALETTES[index % SCENES];
    Scene {
        width,
        height,
        sky,
        haze,
        ground,
        horizon,
        sun,
    }
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Renders `scene` into RGBA bytes, `ev` stops brighter or darker, as a camera bracket would.
fn render(scene: &Scene, ev: f32) -> Vec<u8> {
    let (w, h) = (scene.width as usize, scene.height as usize);
    let gain = 2f32.powf(ev);
    let mut pixels = vec![0u8; w * h * 4];
    let horizon = scene.horizon * h as f32;
    let (sx, sy, sr) = (
        scene.sun.0 * w as f32,
        scene.sun.1 * h as f32,
        scene.sun.2 * h as f32,
    );
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // A shoreline of low hills along the horizon, the same shape in every scene.
            let shore =
                horizon - 6.0 - 5.0 * ((fx / w as f32) * 9.0).sin().abs() * (h as f32 / 160.0);
            let mut colour = if fy < shore {
                let t = (fy / shore).clamp(0.0, 1.0);
                mix(scene.sky, scene.haze, t * t)
            } else if fy < horizon {
                [0.08, 0.09, 0.08]
            } else {
                let t = ((fy - horizon) / (h as f32 - horizon)).clamp(0.0, 1.0);
                // Ripples on water or furrows in a field: faint bands that close up toward the
                // horizon.
                let band = 0.06 * ((fy - horizon).sqrt() * 5.0).sin();
                let lit = mix(scene.haze, scene.ground, 0.6 + 0.4 * t);
                [lit[0] + band, lit[1] + band, lit[2] + band]
            };
            let d = ((fx - sx).powi(2) + (fy - sy).powi(2)).sqrt();
            if d < sr {
                colour = [1.0, 0.96, 0.86];
            } else if d < sr * 3.0 && fy < shore {
                colour = mix([1.0, 0.94, 0.8], colour, (d - sr) / (sr * 2.0));
            }
            let at = (y * w + x) * 4;
            for channel in 0..3 {
                pixels[at + channel] = ((colour[channel] * gain).clamp(0.0, 1.0) * 255.0) as u8;
            }
            pixels[at + 3] = 255;
        }
    }
    pixels
}

fn handle(index: usize, ev: f32) -> Handle {
    let scene = scene(index);
    Handle::from_rgba(scene.width, scene.height, render(&scene, ev))
}

static THUMBNAILS: LazyLock<Vec<Handle>> =
    LazyLock::new(|| (0..SCENES).map(|index| handle(index, 0.0)).collect());

/// A bracket's three exposures of scene 9: −2, 0 and +2 EV.
static BRACKET: LazyLock<[Handle; 3]> =
    LazyLock::new(|| [handle(9, -2.0), handle(9, 0.0), handle(9, 2.0)]);

/// Scene `index` (cycling through [`SCENES`]), the same handle — the same id — every call.
pub(crate) fn thumbnail(index: usize) -> Handle {
    THUMBNAILS[index % SCENES].clone()
}

/// Exposure `step` (0, 1 or 2: −2, 0 and +2 EV) of the bracket scene, the same handle every call.
pub(crate) fn bracket(step: usize) -> Handle {
    BRACKET[step.min(2)].clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thumbnail_keeps_its_id_across_calls_so_nothing_uploads_twice() {
        assert_eq!(thumbnail(3).id(), thumbnail(3).id());
        assert_eq!(thumbnail(3).id(), thumbnail(3 + SCENES).id());
        assert_ne!(thumbnail(3).id(), thumbnail(4).id());
        assert_eq!(bracket(0).id(), bracket(0).id());
    }

    #[test]
    fn a_brighter_exposure_is_brighter() {
        let scene = scene(9);
        let mean = |ev: f32| {
            let pixels = render(&scene, ev);
            pixels.iter().map(|&b| b as u64).sum::<u64>() / pixels.len() as u64
        };
        assert!(mean(-2.0) < mean(0.0) && mean(0.0) < mean(2.0));
    }
}
