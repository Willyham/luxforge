//! The small dense linear algebra registration needs, in `f64`: 3-vectors and 3 × 3 matrices, the
//! rotation exponential, symmetric eigen-decomposition by cyclic Jacobi sweeps, a Cholesky solve
//! for the bundle adjustment's normal equations, a pivoted Gaussian solve for homographies, and a
//! seeded generator for RANSAC's draws. Every routine runs a fixed or bounded number of steps.

pub(crate) type V3 = [f64; 3];
/// Row-major.
pub(crate) type M3 = [[f64; 3]; 3];

pub(crate) const IDENTITY: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

#[inline]
pub(crate) fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub(crate) fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
pub(crate) fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

#[inline]
pub(crate) fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
pub(crate) fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
pub(crate) fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// `a / |a|`, or `a` itself when it is zero.
#[inline]
pub(crate) fn normalize(a: V3) -> V3 {
    let n = norm(a);
    if n > 0.0 { scale(a, 1.0 / n) } else { a }
}

#[inline]
pub(crate) fn mul(a: &M3, b: &M3) -> M3 {
    std::array::from_fn(|r| {
        std::array::from_fn(|c| a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c])
    })
}

#[inline]
pub(crate) fn transpose(m: &M3) -> M3 {
    std::array::from_fn(|r| std::array::from_fn(|c| m[c][r]))
}

#[inline]
pub(crate) fn apply(m: &M3, v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}

/// `[a]×`, the matrix of `v ↦ a × v`.
#[inline]
pub(crate) fn skew(a: V3) -> M3 {
    [[0.0, -a[2], a[1]], [a[2], 0.0, -a[0]], [-a[1], a[0], 0.0]]
}

/// The angle between two directions, in radians, from the stable `atan2` form.
pub(crate) fn angle_between(a: V3, b: V3) -> f64 {
    norm(cross(a, b)).atan2(dot(a, b))
}

/// `exp([ω]×)`: the rotation by `|ω|` radians about `ω` (Rodrigues).
pub(crate) fn exp_so3(w: V3) -> M3 {
    let theta2 = dot(w, w);
    let theta = theta2.sqrt();
    // Taylor forms below 1e-4 radians keep full precision.
    let (a, b) = if theta < 1e-4 {
        (1.0 - theta2 / 6.0, 0.5 - theta2 / 24.0)
    } else {
        (theta.sin() / theta, (1.0 - theta.cos()) / theta2)
    };
    let k = skew(w);
    let k2 = mul(&k, &k);
    std::array::from_fn(|r| std::array::from_fn(|c| IDENTITY[r][c] + a * k[r][c] + b * k2[r][c]))
}

/// The rotation as a unit quaternion `[x, y, z, w]` with `w ≥ 0` (Shepperd's method).
pub(crate) fn quaternion(m: &M3) -> [f64; 4] {
    let trace = m[0][0] + m[1][1] + m[2][2];
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
            0.25 * s,
        ]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [
            0.25 * s,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[2][1] - m[1][2]) / s,
        ]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [
            (m[0][1] + m[1][0]) / s,
            0.25 * s,
            (m[1][2] + m[2][1]) / s,
            (m[0][2] - m[2][0]) / s,
        ]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            0.25 * s,
            (m[1][0] - m[0][1]) / s,
        ]
    };
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    let sign = if q[3] < 0.0 { -1.0 } else { 1.0 };
    q.map(|v| sign * v / n)
}

/// The rotation of a unit quaternion `[x, y, z, w]`.
pub(crate) fn quaternion_matrix([x, y, z, w]: [f64; 4]) -> M3 {
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

/// The nearest rotation to a nearly orthonormal matrix, through its normalized quaternion: what
/// keeps repeated small updates from drifting off the rotations.
pub(crate) fn orthonormalize(m: &M3) -> M3 {
    quaternion_matrix(quaternion(m))
}

/// The eigenvalues of a symmetric `N × N` matrix, ascending, with their unit eigenvectors as the
/// columns of the second matrix, by cyclic Jacobi sweeps (at most 64, stopping once the
/// off-diagonal is negligible).
// Row and column updates of a matrix read clearest by index.
#[allow(clippy::needless_range_loop)]
pub(crate) fn symmetric_eigen<const N: usize>(a: [[f64; N]; N]) -> ([f64; N], [[f64; N]; N]) {
    let mut a = a;
    let mut v = [[0.0; N]; N];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    let scale: f64 = a.iter().flatten().map(|x| x * x).sum::<f64>().sqrt();
    for _ in 0..64 {
        let off: f64 = (0..N)
            .flat_map(|p| (0..N).filter(move |&q| q != p).map(move |q| (p, q)))
            .map(|(p, q)| a[p][q] * a[p][q])
            .sum::<f64>()
            .sqrt();
        if off <= 1e-15 * scale.max(f64::MIN_POSITIVE) {
            break;
        }
        for p in 0..N {
            for q in p + 1..N {
                if a[p][q] == 0.0 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..N {
                    let (akp, akq) = (a[k][p], a[k][q]);
                    a[k][p] = c * akp - s * akq;
                    a[k][q] = s * akp + c * akq;
                }
                for k in 0..N {
                    let (apk, aqk) = (a[p][k], a[q][k]);
                    a[p][k] = c * apk - s * aqk;
                    a[q][k] = s * apk + c * aqk;
                }
                for row in v.iter_mut() {
                    let (vkp, vkq) = (row[p], row[q]);
                    row[p] = c * vkp - s * vkq;
                    row[q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut order: [usize; N] = std::array::from_fn(|i| i);
    order.sort_by(|&i, &j| a[i][i].total_cmp(&a[j][j]).then(i.cmp(&j)));
    let values = order.map(|i| a[i][i]);
    let vectors = std::array::from_fn(|r| std::array::from_fn(|c| v[r][order[c]]));
    (values, vectors)
}

/// The rotation `R` best taking each `a` to its `b` (maximizing `Σ wᵢ bᵢ·R aᵢ`), by Horn's
/// quaternion method; `None` when the pairs do not fix one (fewer than two independent
/// directions).
pub(crate) fn fit_rotation(pairs: impl Iterator<Item = (V3, V3, f64)>) -> Option<M3> {
    let mut s = [[0.0f64; 3]; 3];
    let mut count = 0usize;
    for (a, b, w) in pairs {
        for r in 0..3 {
            for c in 0..3 {
                s[r][c] += w * a[r] * b[c];
            }
        }
        count += 1;
    }
    if count < 2 {
        return None;
    }
    let n = [
        [
            s[0][0] + s[1][1] + s[2][2],
            s[1][2] - s[2][1],
            s[2][0] - s[0][2],
            s[0][1] - s[1][0],
        ],
        [
            s[1][2] - s[2][1],
            s[0][0] - s[1][1] - s[2][2],
            s[0][1] + s[1][0],
            s[2][0] + s[0][2],
        ],
        [
            s[2][0] - s[0][2],
            s[0][1] + s[1][0],
            -s[0][0] + s[1][1] - s[2][2],
            s[1][2] + s[2][1],
        ],
        [
            s[0][1] - s[1][0],
            s[2][0] + s[0][2],
            s[1][2] + s[2][1],
            -s[0][0] - s[1][1] + s[2][2],
        ],
    ];
    let (values, vectors) = symmetric_eigen(n);
    // The two largest eigenvalues tie when the directions are all parallel.
    let spread = values[3] - values[2];
    if spread.is_nan() || spread <= 1e-12 * values[3].abs().max(1e-300) {
        return None;
    }
    let (w, x, y, z) = (vectors[0][3], vectors[1][3], vectors[2][3], vectors[3][3]);
    let rotation = quaternion_matrix([x, y, z, w]);
    rotation
        .iter()
        .flatten()
        .all(|v| v.is_finite())
        .then_some(rotation)
}

/// Solve the symmetric positive definite `n × n` system `a·x = b` in place by Cholesky, with `a`
/// row-major; `None` when `a` is not positive definite.
pub(crate) fn cholesky_solve(a: &mut [f64], b: &[f64], n: usize) -> Option<Vec<f64>> {
    for j in 0..n {
        let mut d = a[j * n + j];
        for k in 0..j {
            d -= a[j * n + k] * a[j * n + k];
        }
        if d.is_nan() || d <= 0.0 {
            return None;
        }
        let d = d.sqrt();
        a[j * n + j] = d;
        for i in j + 1..n {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= a[i * n + k] * a[j * n + k];
            }
            a[i * n + j] = s / d;
        }
    }
    let mut y = b.to_vec();
    for i in 0..n {
        for k in 0..i {
            y[i] -= a[i * n + k] * y[k];
        }
        y[i] /= a[i * n + i];
    }
    for i in (0..n).rev() {
        for k in i + 1..n {
            y[i] -= a[k * n + i] * y[k];
        }
        y[i] /= a[i * n + i];
    }
    y.iter().all(|v| v.is_finite()).then_some(y)
}

/// Solve the `N × N` system `a·x = b` by Gaussian elimination with partial pivoting; `None` when a
/// pivot is negligible.
// Row and column updates of a matrix read clearest by index.
#[allow(clippy::needless_range_loop)]
pub(crate) fn solve<const N: usize>(mut a: [[f64; N]; N], mut b: [f64; N]) -> Option<[f64; N]> {
    let scale = a.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
    for col in 0..N {
        let pivot = (col..N)
            .max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()).then(j.cmp(&i)))?;
        if a[pivot][col].abs() <= 1e-12 * scale {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..N {
            let f = a[row][col] / a[col][col];
            for k in col..N {
                a[row][k] -= f * a[col][k];
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = [0.0; N];
    for row in (0..N).rev() {
        let mut s = b[row];
        for k in row + 1..N {
            s -= a[row][k] * x[k];
        }
        x[row] = s / a[row][row];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// SplitMix64, seeded per use, for RANSAC's reproducible draws.
#[derive(Clone, Debug)]
pub(crate) struct Draws(pub(crate) u64);

impl Draws {
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`, by remainder.
    pub(crate) fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panorama_registration_linalg_rotations_and_solves() {
        let w = [0.3, -0.2, 0.5];
        let r = exp_so3(w);
        let rt = mul(&r, &transpose(&r));
        for (i, row) in rt.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                assert!((v - IDENTITY[i][j]).abs() < 1e-14);
            }
        }
        let back = quaternion_matrix(quaternion(&r));
        for i in 0..3 {
            for j in 0..3 {
                assert!((back[i][j] - r[i][j]).abs() < 1e-14);
            }
        }
        // Horn recovers the rotation from noise-free direction pairs.
        let dirs = [[1.0, 0.2, 0.9], [-0.3, 0.5, 1.0], [0.1, -0.7, 0.8]];
        let fitted = fit_rotation(
            dirs.iter()
                .map(|&a| (normalize(a), apply(&r, normalize(a)), 1.0)),
        )
        .unwrap();
        for i in 0..3 {
            for j in 0..3 {
                assert!((fitted[i][j] - r[i][j]).abs() < 1e-12);
            }
        }
        assert!(fit_rotation([([0.0, 0.0, 1.0], [0.0, 0.0, 1.0], 1.0)].into_iter()).is_none());
        let (values, vectors) =
            symmetric_eigen([[2.0, 1.0, 0.0], [1.0, 2.0, 0.0], [0.0, 0.0, 5.0]]);
        assert!(
            (values[0] - 1.0).abs() < 1e-14
                && (values[1] - 3.0).abs() < 1e-14
                && (values[2] - 5.0).abs() < 1e-14
        );
        assert!((vectors[0][0].abs() - 0.5f64.sqrt()).abs() < 1e-14);
        let mut a = vec![4.0, 2.0, 2.0, 3.0];
        let x = cholesky_solve(&mut a, &[2.0, 1.0], 2).unwrap();
        assert!(
            (4.0 * x[0] + 2.0 * x[1] - 2.0).abs() < 1e-14
                && (2.0 * x[0] + 3.0 * x[1] - 1.0).abs() < 1e-14
        );
        let x = solve([[0.0, 2.0], [3.0, 1.0]], [4.0, 5.0]).unwrap();
        assert!((x[0] - 1.0).abs() < 1e-14 && (x[1] - 2.0).abs() < 1e-14);
    }
}
