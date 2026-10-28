//! The fixed pixel patches the harness and `cargo xtask module-check` run a module's reference
//! over: linear-sRGB, 64 × 64 each (four 16 × 16 blocks a side), deterministic, and covering
//! neutrals, every hue, saturation, light outside 0..1 and noise.

/// A patch's side, in pixels.
pub const SIDE: u32 = 64;

/// The column each row is split at: a row is run as two contiguous runs, `0..SPLIT` and
/// `SPLIT..SIDE`, so a module is held to `x0` as well as to its pixels.
pub const SPLIT: u32 = 23;

/// One fixed patch of linear-sRGB pixels, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Patch {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<[f32; 3]>,
}

impl Patch {
    fn of(name: &'static str, pixel: impl Fn(u32, u32) -> [f32; 3]) -> Self {
        let rgb = (0..SIDE)
            .flat_map(|y| (0..SIDE).map(move |x| (x, y)))
            .map(|(x, y)| pixel(x, y))
            .collect();
        Self {
            name,
            width: SIDE,
            height: SIDE,
            rgb,
        }
    }

    /// Row `y`'s pixels.
    pub fn row(&self, y: u32) -> &[[f32; 3]] {
        let start = (y * self.width) as usize;
        &self.rgb[start..start + self.width as usize]
    }
}

/// A fully saturated colour of hue `h` in sixths of the circle, `[0, 6)`: the hexagon of HSV.
fn hue(h: f32) -> [f32; 3] {
    [
        ((h - 3.0).abs() - 1.0).clamp(0.0, 1.0),
        (2.0 - (h - 2.0).abs()).clamp(0.0, 1.0),
        (2.0 - (h - 4.0).abs()).clamp(0.0, 1.0),
    ]
}

/// A xorshift sequence of values in `[0, 1)`, from a fixed seed.
struct Sequence(u32);

impl Sequence {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / 16_777_216.0
    }
}

/// Every fixed patch, in a fixed order.
pub fn patches() -> Vec<Patch> {
    let last = (SIDE * SIDE - 1) as f32;
    let side = SIDE as f32;
    let mut noise = Sequence(0x9e37_79b9);
    let noise: Vec<[f32; 3]> = (0..SIDE * SIDE)
        .map(|_| {
            let [r, g, b] = [noise.next(), noise.next(), noise.next()];
            [r * r, g * g, b * b]
        })
        .collect();
    let mut wide = Sequence(0x85eb_ca6b);
    let extended: Vec<[f32; 3]> = (0..SIDE * SIDE)
        .map(|_| [wide.next(), wide.next(), wide.next()].map(|v| v * 2.0 - 0.25))
        .collect();
    vec![
        Patch::of("grey-ramp", |x, y| {
            let v = (y * SIDE + x) as f32 / last;
            [v * v; 3]
        }),
        Patch::of("hue-sweep", |x, y| {
            let value = (y + 1) as f32 / side;
            hue(x as f32 * 6.0 / side).map(|c| c * value * value)
        }),
        Patch::of("saturation-sweep", |x, y| {
            let saturation = x as f32 / (side - 1.0);
            hue(y as f32 * 6.0 / side).map(|c| 0.25 * (1.0 - saturation + saturation * c))
        }),
        Patch::of("extended", |x, y| extended[(y * SIDE + x) as usize]),
        Patch::of("noise", |x, y| noise[(y * SIDE + x) as usize]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_are_fixed_sized_and_named_once() {
        let patches = patches();
        let mut names: Vec<_> = patches.iter().map(|patch| patch.name).collect();
        names.dedup();
        assert_eq!(names.len(), patches.len());
        for patch in &patches {
            assert_eq!(patch.rgb.len(), (patch.width * patch.height) as usize);
            assert!(patch.rgb.iter().flatten().all(|v| v.is_finite()));
        }
        assert_eq!(patches, super::patches(), "the patches are deterministic");
    }

    #[test]
    fn the_extended_patch_leaves_zero_to_one_both_ways() {
        let patches = patches();
        let extended = patches.iter().find(|p| p.name == "extended").unwrap();
        let values: Vec<f32> = extended.rgb.iter().flatten().copied().collect();
        assert!(values.iter().any(|v| *v < 0.0));
        assert!(values.iter().any(|v| *v > 1.0));
    }
}
