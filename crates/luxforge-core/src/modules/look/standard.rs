//! The Standard look, frozen (`docs/design/raw-looks.md`, "The Standard look").
//!
//! The owner approved these numbers on 2026-10-05 from the corpus study. They are the independent
//! reference's `standard_knots(&STANDARD)` (`crates/luxforge-reference/src/look.rs`), written out as
//! literal `f64` values so the module computes nothing at runtime and a later change to the
//! reference's study code cannot move a photo's look: the tone map `f(Y) = W Y^p / (Y^p + s^p)` at
//! lift +1.15 EV, contrast `p = 1.6` and headroom 1.5 EV, sampled in encoded luminance at whole
//! stops through the shadows and half stops up to a quarter stop below the headroom, then
//! `(encode(2^1.5), 1)`. Every payload stores its own resolved knots, so retuning Standard changes
//! these constants and the photos a later `set-look` writes, never a photo already edited.

/// The Standard look's tone curve: 24 knots in encoded luminance, the last at `x_max =
/// encode(2^1.5)`.
pub(crate) const STANDARD_KNOTS: [[f64; 2]; 24] = [
    [0.0, 0.0],
    [0.00409364858564013, 0.00039631725031666695],
    [0.00818729717128026, 0.0012013355492626597],
    [0.01637459434256052, 0.003641091333019827],
    [0.03274918868512104, 0.0110315077132041],
    [0.06167804340024347, 0.033384274060313426],
    [0.0798042921082514, 0.05592720676783642],
    [0.10074650243722594, 0.08456918951441425],
    [0.12494214161927303, 0.12043127105501406],
    [0.15289663860079958, 0.1651293471953684],
    [0.1851939420781141, 0.22040776792027705],
    [0.2225087187523317, 0.2878759235192819],
    [0.26562044661607553, 0.3684447283398252],
    [0.31542969767027546, 0.46135612950044175],
    [0.3729769502049335, 0.5629666669720048],
    [0.43946432361844046, 0.6661060605739058],
    [0.5162806897996894, 0.7613601614911786],
    [0.6050306856311236, 0.840535293718042],
    [0.7075682326623741, 0.8999532375000836],
    [0.8260352641555969, 0.9408497675926301],
    [0.9629064684817444, 0.9672261577238078],
    [1.1210409835241149, 0.9834961579863238],
    [1.3037421219469065, 0.9932491373491042],
    [1.5720324208053778, 1.0],
];

/// The Standard look's Oklab chroma gain: the corpus median of the cameras' chroma over the
/// Standard-toned development's.
pub(crate) const STANDARD_CHROMA: f64 = 1.2;

/// The Standard look's path-to-white knee: colours whose largest channel stays at or below it are
/// untouched.
pub(crate) const STANDARD_KNEE: f64 = 0.8;

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_reference::look::{STANDARD, standard_knots};
    use serde_json::Value;
    use std::{fs, path::PathBuf};

    /// The frozen constants are the reference's knots, chroma and knee, and the fixture's, bit for
    /// bit.
    #[test]
    fn the_frozen_standard_is_the_references_and_the_fixtures_exactly() {
        let bits = |knots: &[[f64; 2]]| -> Vec<[u64; 2]> {
            knots
                .iter()
                .map(|[x, y]| [x.to_bits(), y.to_bits()])
                .collect()
        };
        assert_eq!(bits(&STANDARD_KNOTS), bits(&standard_knots(&STANDARD)));
        assert_eq!(STANDARD_CHROMA.to_bits(), STANDARD.chroma.to_bits());
        assert_eq!(STANDARD_KNEE.to_bits(), STANDARD.knee.to_bits());

        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/look/look-cases.json");
        let fixture: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("the look fixture"))
                .expect("valid fixture JSON");
        let knots: Vec<[f64; 2]> = fixture["standard_knots"]
            .as_array()
            .expect("the fixture's Standard knots")
            .iter()
            .map(|knot| [knot[0].as_f64().unwrap(), knot[1].as_f64().unwrap()])
            .collect();
        assert_eq!(bits(&STANDARD_KNOTS), bits(&knots));
        assert_eq!(
            fixture["standard"]["chroma"].as_f64(),
            Some(STANDARD_CHROMA)
        );
        assert_eq!(fixture["standard"]["knee"].as_f64(), Some(STANDARD_KNEE));
    }
}
