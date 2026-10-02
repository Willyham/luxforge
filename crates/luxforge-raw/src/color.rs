//! Source matrix conversion for linear/camera DNG interpretations.
use crate::mat3::determinant;
/// The three-colour version of LibRaw's row-normalized camera response conversion.
pub(crate) fn camera_to_rgb(response: [[f64; 3]; 3]) -> Result<[[f32; 4]; 3], &'static str> {
    let xyz_rgb = [
        [0.412453, 0.357580, 0.180423],
        [0.212671, 0.715160, 0.072169],
        [0.019334, 0.119193, 0.950227],
    ];
    let mut a: [[f64; 3]; 3] = std::array::from_fn(|r| {
        std::array::from_fn(|c| (0..3).map(|k| response[r][k] * xyz_rgb[k][c]).sum())
    });
    for row in &mut a {
        let sum = row.iter().sum::<f64>();
        if !sum.is_finite() || sum <= 1e-5 {
            return Err("DNG display response");
        }
        for value in row {
            *value /= sum;
        }
    }
    let det = determinant(a);
    if !det.is_finite() || det.abs() < 1e-8 {
        return Err("DNG display inverse");
    }
    let mut inverse = [[0.0; 4]; 3];
    for (r, row) in inverse.iter_mut().enumerate() {
        for (c, value) in row.iter_mut().enumerate().take(3) {
            let rr = [(c + 1) % 3, (c + 2) % 3];
            let cc = [(r + 1) % 3, (r + 2) % 3];
            *value = ((a[rr[0]][cc[0]] * a[rr[1]][cc[1]] - a[rr[0]][cc[1]] * a[rr[1]][cc[0]]) / det)
                as f32;
        }
    }
    Ok(inverse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_response_inverse_preserves_neutral_and_rejects_invalid_matrices() {
        let matrix = camera_to_rgb([[0.8, -0.1, 0.2], [0.1, 0.9, 0.1], [0.05, 0.1, 0.7]]).unwrap();
        for row in matrix {
            assert!((row[..3].iter().sum::<f32>() - 1.0).abs() < 1e-6);
            assert_eq!(row[3], 0.0);
        }
        assert!(camera_to_rgb([[1.0; 3]; 3]).is_err());
        assert!(camera_to_rgb([[0.0; 3]; 3]).is_err());
        assert!(camera_to_rgb([[f64::NAN; 3]; 3]).is_err());
    }
}
