//! The identity harness: a module's [`Adjustment`] run natively and its component run on the
//! module engine ([`super::Runner`]) over the same values and the [fixed patches](super::fixtures),
//! held to identical words, labels and buffers.
//!
//! The component is what the host runs; the native build is what a module's own unit tests run.
//! Identity between them is what lets those tests speak for the component. Two values are
//! identical when their bits are, except that any two NaNs are, since the engine canonicalizes a
//! NaN's payload.
use super::fixtures::{SPLIT, patches};
use super::{Error, Runner};
use crate::{Adjustment, Values};

/// What a harness run compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The value sets run.
    pub value_sets: usize,
    /// The channel values held identical.
    pub channels: usize,
}

/// Whether two channel values are identical: equal bits, or both NaN.
fn identical(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

/// The index of the first value at which `native` and `wasm` are not identical, or their shorter
/// length when one is a prefix of the other.
pub fn first_difference(native: &[f32], wasm: &[f32]) -> Option<usize> {
    native
        .iter()
        .zip(wasm)
        .position(|(a, b)| !identical(*a, *b))
        .or_else(|| (native.len() != wasm.len()).then(|| native.len().min(wasm.len())))
}

/// Run `A` natively and `component` on the module engine for every named value set over every
/// fixed patch, each row as two runs split at [`SPLIT`], and answer what was compared, or the first
/// difference: the value set, the patch, the pixel and channel, and both values with their bits.
pub fn compare<A: Adjustment>(
    component: &[u8],
    value_sets: &[(&str, Values)],
) -> Result<Report, Error> {
    let id = std::any::type_name::<A>()
        .rsplit("::")
        .next()
        .unwrap_or("module");
    let runner = Runner::new(id, component)?;
    let mut instance = runner.instance()?;
    let mut report = Report::default();
    for (set, values) in value_sets {
        report.value_sets += 1;
        let native = A::coefficients(values);
        let wasm = instance.coefficients(values.fields())?;
        if native != wasm {
            return Err(format!(
                "value set `{set}`: the coefficients differ: natively {native:?}, in \
                 WebAssembly {wasm:?}"
            ));
        }
        let (native_label, wasm_label) = (A::label(values), instance.label(values.fields())?);
        if native_label != wasm_label {
            return Err(format!(
                "value set `{set}`: the labels differ: natively {native_label:?}, in \
                 WebAssembly {wasm_label:?}"
            ));
        }
        let Ok(words) = native else { continue };
        for patch in patches() {
            for y in 0..patch.height {
                let row = patch.row(y);
                for (x0, run) in [(0, &row[..SPLIT as usize]), (SPLIT, &row[SPLIT as usize..])] {
                    let mut native = run.to_vec();
                    A::apply_row(&words, y, x0, &mut native);
                    let native: Vec<f32> = native.into_iter().flatten().collect();
                    let flat: Vec<f32> = run.iter().flatten().copied().collect();
                    let wasm = instance.apply_row(&words, y, x0, &flat)?;
                    if let Some(at) = first_difference(&native, &wasm) {
                        let shown = |values: &[f32]| {
                            values.get(at).map_or("nothing".to_owned(), |v| {
                                format!("{v:?} ({:#010x})", v.to_bits())
                            })
                        };
                        return Err(format!(
                            "value set `{set}`, patch `{}`, pixel ({}, {y}) channel {}: natively \
                             {}, in WebAssembly {}",
                            patch.name,
                            x0 as usize + at / 3,
                            at % 3,
                            shown(&native),
                            shown(&wasm)
                        ));
                    }
                    report.channels += native.len();
                }
            }
        }
    }
    Ok(report)
}

/// [`compare`], panicking with the first difference: for a module's own tests.
pub fn assert_identical<A: Adjustment>(component: &[u8], value_sets: &[(&str, Values)]) -> Report {
    compare::<A>(component, value_sets).unwrap_or_else(|difference| panic!("{difference}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_bitwise_but_for_nan_payloads() {
        assert_eq!(first_difference(&[1.0, 2.0], &[1.0, 2.0]), None);
        assert_eq!(first_difference(&[0.0, 2.0], &[-0.0, 2.0]), Some(0));
        assert_eq!(
            first_difference(&[1.0, 0.3], &[1.0, f32::from_bits(0.3_f32.to_bits() + 1)]),
            Some(1),
            "one ulp apart"
        );
        assert_eq!(
            first_difference(&[f32::NAN], &[f32::from_bits(0x7fc0_0001)]),
            None
        );
        assert_eq!(first_difference(&[1.0, 2.0], &[1.0]), Some(1));
    }
}
