//! Where a step's program is: the exact integer map from the pixel of the pass a step runs in to
//! the `pos` its program receives, its six header words, and the WGSL that evaluates it.

/// An exact integer map from the pixel `(x, y)` of the pass a step runs in to the `pos` its program
/// receives: `(a·x + b·y + tx, c·x + d·y + ty)`. The linear part is a signed permutation, so every
/// coefficient and every coordinate of an admissible stage is an integer an `f32` holds exactly and
/// `pos` is the integer the CPU unit is handed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionMap {
    pub a: i32,
    pub b: i32,
    pub tx: i32,
    pub c: i32,
    pub d: i32,
    pub ty: i32,
}

impl PositionMap {
    /// `pos` is the pass's own pixel.
    pub const IDENTITY: Self = Self {
        a: 1,
        b: 0,
        tx: 0,
        c: 0,
        d: 1,
        ty: 0,
    };

    /// How many words of a step's header the map takes.
    pub const WORDS: usize = luxforge_gpu_types::POSITION_WORDS;

    /// The six coefficients as the `f32` words a step's header holds: `a, b, tx, c, d, ty`.
    pub fn words(self) -> [u32; Self::WORDS] {
        [self.a, self.b, self.tx, self.c, self.d, self.ty].map(|value| (value as f32).to_bits())
    }

    /// The WGSL `vec2<f32>` the map gives `pixel`, a `vec2<f32>` expression of the pass's pixel,
    /// with its six coefficients read from `lf_words` from index `first`. Every product and sum is
    /// of integers an `f32` holds exactly, so any order of evaluation gives the same `pos`.
    pub fn wgsl(first: usize, pixel: &str) -> String {
        let row = |row: usize| {
            let word = |k: usize| format!("lf_f32({}u)", first + 3 * row + k);
            format!(
                "{} * {pixel}.x + {} * {pixel}.y + {}",
                word(0),
                word(1),
                word(2)
            )
        };
        format!("vec2<f32>({}, {})", row(0), row(1))
    }
}
