//! 3x3 matrix arithmetic shared by the library and the build script's catalog validation.
use std::ops::{Add, Mul, Sub};

/// The determinant of `m` by cofactor expansion along its first row, in `T`'s own arithmetic
/// and in one fixed order, so an `f32` matrix keeps `f32` rounding and every caller gets the
/// same bits for the same matrix.
pub(crate) fn determinant<T>(m: [[T; 3]; 3]) -> T
where
    T: Copy + Add<Output = T> + Sub<Output = T> + Mul<Output = T>,
{
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}
