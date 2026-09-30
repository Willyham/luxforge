//! The picture a generated file shows: a simple procedural scene per moment — a sky gradient, a
//! horizon, ground and a few coloured shapes placed by the moment's own seed — so the frames of one
//! moment share their framing and moments differ. A burst's first shape moves a few pixels a
//! frame. A bracket's frames are the same scene in linear light scaled by `2^step` before sRGB
//! encoding, so the mean log luminance of their thumbnails steps by the bracket's step: every
//! linear value of a day scene lies in `[0.02, 0.2]`, so two EV either way neither clips nor falls
//! into the transfer function's linear toe.
use super::plan::SceneKey;
use super::random::Random;
use image::{Rgb, RgbImage};

pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 427;
pub const THUMBNAIL_WIDTH: u32 = 160;
pub const THUMBNAIL_HEIGHT: u32 = 107;

/// A night frame's light, relative to a day scene's.
const NIGHT: f32 = 0.08;

/// Linear light to an 8-bit sRGB code, tabulated at 16-bit linear steps from the one reference
/// transfer function, so a frame costs a table lookup a channel.
pub struct Encoder {
    table: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Encoder {
            table: (0..=u16::MAX)
                .map(|step| luxforge_reference::srgb::code(f64::from(step) / 65535.0))
                .collect(),
        }
    }

    fn code(&self, linear: f32) -> u8 {
        self.table[(linear.clamp(0.0, 1.0) * 65535.0 + 0.5) as usize]
    }
}

struct Shape {
    round: bool,
    centre: (f32, f32),
    /// Half its width and height, as fractions of the frame's.
    radius: (f32, f32),
    colour: [f32; 3],
}

struct Scene {
    sky: [[f32; 3]; 2],
    ground: [[f32; 3]; 2],
    horizon: f32,
    shapes: Vec<Shape>,
    /// How far the first shape moves each burst frame, as fractions of the frame.
    drift: (f32, f32),
}

/// The darkest and brightest linear value of a day scene.
const DARKEST: f32 = 0.02;
const BRIGHTEST: f32 = 0.2;

/// Skies, top then horizon, in linear light: clear, overcast and late.
const SKIES: [[[f32; 3]; 2]; 3] = [
    [[0.05, 0.09, 0.2], [0.13, 0.16, 0.2]],
    [[0.1, 0.1, 0.11], [0.16, 0.16, 0.17]],
    [[0.06, 0.05, 0.12], [0.2, 0.1, 0.05]],
];
/// Grounds, horizon then foreground: meadow, water and stone.
const GROUNDS: [[[f32; 3]; 2]; 3] = [
    [[0.04, 0.08, 0.02], [0.03, 0.06, 0.02]],
    [[0.03, 0.06, 0.09], [0.02, 0.04, 0.06]],
    [[0.07, 0.06, 0.05], [0.04, 0.035, 0.03]],
];

fn colour(random: &mut Random) -> [f32; 3] {
    [(); 3].map(|_| random.uniform(f64::from(DARKEST), f64::from(BRIGHTEST)) as f32)
}

/// One of `choices`, each channel varied by up to 15% and kept in the day scene's range.
fn varied(random: &mut Random, choices: &[[[f32; 3]; 2]]) -> [[f32; 3]; 2] {
    let chosen = *random.pick(choices);
    chosen.map(|c| c.map(|v| (v * random.uniform(0.85, 1.15) as f32).clamp(DARKEST, BRIGHTEST)))
}

impl Scene {
    fn new(seed: u64) -> Self {
        let mut random = Random::stream(seed, 0);
        let aspect = WIDTH as f32 / HEIGHT as f32;
        let shapes = (0..random.between(3, 5))
            .map(|_| {
                let round = random.chance(0.5);
                let width = random.uniform(0.04, 0.14) as f32;
                let height = if round {
                    width * aspect
                } else {
                    random.uniform(0.06, 0.25) as f32
                };
                Shape {
                    round,
                    centre: (
                        random.uniform(0.15, 0.85) as f32,
                        random.uniform(0.2, 0.8) as f32,
                    ),
                    radius: (width, height),
                    colour: colour(&mut random),
                }
            })
            .collect();
        let sign = if random.chance(0.5) { 1.0 } else { -1.0 };
        Scene {
            sky: varied(&mut random, &SKIES),
            ground: varied(&mut random, &GROUNDS),
            horizon: random.uniform(0.45, 0.7) as f32,
            shapes,
            drift: (
                sign * random.uniform(3.0, 6.0) as f32 / WIDTH as f32,
                random.uniform(-2.0, 2.0) as f32 / HEIGHT as f32,
            ),
        }
    }

    /// The linear light at `(u, v)`, fractions of the frame from its top left.
    fn light(&self, u: f32, v: f32, shift: f32) -> [f32; 3] {
        for (index, shape) in self.shapes.iter().enumerate() {
            let (mut cx, mut cy) = shape.centre;
            if index == 0 {
                cx += self.drift.0 * shift;
                cy += self.drift.1 * shift;
            }
            let dx = (u - cx) / shape.radius.0;
            let dy = (v - cy) / shape.radius.1;
            let inside = if shape.round {
                dx * dx + dy * dy <= 1.0
            } else {
                dx.abs() <= 1.0 && dy.abs() <= 1.0
            };
            if inside {
                return shape.colour;
            }
        }
        let (from, to, t) = if v < self.horizon {
            (self.sky[0], self.sky[1], v / self.horizon)
        } else {
            (
                self.ground[0],
                self.ground[1],
                (v - self.horizon) / (1.0 - self.horizon),
            )
        };
        [0, 1, 2].map(|c| from[c] + (to[c] - from[c]) * t)
    }
}

/// The full-size image of a frame of the moment `key`, `step` EV from the moment's base exposure.
pub fn render(key: SceneKey, step: i8, encoder: &Encoder) -> RgbImage {
    let scene = Scene::new(key.seed);
    let gain = 2f32.powi(i32::from(step)) * if key.night { NIGHT } else { 1.0 };
    let shift = f32::from(key.shift);
    RgbImage::from_fn(WIDTH, HEIGHT, |x, y| {
        let u = (x as f32 + 0.5) / WIDTH as f32;
        let v = (y as f32 + 0.5) / HEIGHT as f32;
        Rgb(scene
            .light(u, v, shift)
            .map(|linear| encoder.code(linear * gain)))
    })
}

/// The embedded thumbnail of a full-size image: the same picture at 160 × 107.
pub fn thumbnail(image: &RgbImage) -> RgbImage {
    image::imageops::thumbnail(image, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT)
}
