//! The guest SDK of an external Luxforge adjustment module
//! (`docs/design/module-manager.md#authoring`, `docs/engineering/module-authoring.md`).
//!
//! A module is a `cdylib` crate built for `wasm32-unknown-unknown` that implements [`Adjustment`]
//! and names its type once with [`export_adjustment!`]. That makes the core module export the
//! `luxforge:module/adjust@0.1.0` interface of the repository's `wit/adjustment.wit` through the
//! canonical ABI, which `cargo xtask module-package` turns into the package's component. The
//! component imports nothing: the SDK names no WASI, clock, randomness or host function.
//!
//! The host owns the module's field table, read from its manifest: it checks and canonicalizes
//! every value and hands the module every field in the manifest's order ([`Values`]). The module
//! answers its colour unit's **words** ([`Adjustment::coefficients`]), the unit's identity, and the
//! **CPU reference** of one row ([`Adjustment::apply_row`]), under the core's `PointwiseColor`
//! contract. Its WGSL program, `program.wgsl`, reads the same words on the GPU.
//!
//! The reference must give the same bits natively and in WebAssembly, which the `host` feature's
//! harness checks: use the basic IEEE operations (`+ - * /`, `sqrt`, `abs`, `min`, `max`, `floor`,
//! `clamp`, comparisons) and not the transcendental functions of `std` (`powf`, `exp`, `sin`, …),
//! whose native and WebAssembly implementations differ in the last bit.
//!
//! ```
//! use luxforge_module_sdk::{Adjustment, Values, Words, export_adjustment};
//!
//! struct Gain;
//!
//! impl Adjustment for Gain {
//!     fn coefficients(values: &Values) -> Result<Vec<u32>, String> {
//!         let mut words = Words::new();
//!         words.push_f32(1.0 + values.number("amount")? as f32 / 100.0);
//!         Ok(words.into_vec())
//!     }
//!
//!     fn apply_row(words: &[u32], _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
//!         let gain = luxforge_module_sdk::word_f32(words, 0);
//!         for pixel in rgb {
//!             *pixel = pixel.map(|channel| channel * gain);
//!         }
//!     }
//! }
//!
//! export_adjustment!(Gain);
//! ```
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub mod abi;
#[cfg(all(feature = "host", not(target_arch = "wasm32")))]
pub mod host;

/// The component interface this SDK exports: `luxforge:module/adjust@0.1.0`, which a manifest's
/// `api = "0.1"` names.
pub const INTERFACE: &str = "luxforge:module/adjust@0.1.0";

/// What an adjustment module computes. Every function is pure: the answer depends on nothing but
/// its arguments, so equal values give equal words and equal words and pixels give equal pixels.
pub trait Adjustment {
    /// The colour unit's words for these values: the coefficients `apply_row` and the GPU program
    /// read. The words are the unit's identity. An error refuses the layer, the host naming the
    /// module and showing the text.
    fn coefficients(values: &Values) -> Result<Vec<u32>, String>;

    /// The CPU reference: transform one contiguous run of row `y`, starting at column `x0`, in
    /// place. The pixels are linear-sRGB (D65) and unclamped; the answer may leave 0..1, and later
    /// units see it unclamped. Pixel `i` is at `(x0 + i, y)`, which a unit that does not depend on
    /// position ignores.
    fn apply_row(words: &[u32], y: u32, x0: u32, rgb: &mut [[f32; 3]]);

    /// A summary of these values for a history entry, or `None` to keep the host's own.
    fn label(_values: &Values) -> Option<String> {
        None
    }
}

/// One field's canonical value, of the kind its manifest declares: the WIT `value` variant.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A number field's finite value.
    Number(f64),
    Integer(i64),
    Boolean(bool),
    /// An enum field's option, by name.
    Choice(String),
    /// A colour field's three 8-bit sRGB channels.
    Color([u8; 3]),
}

impl Value {
    fn kind(&self) -> &'static str {
        match self {
            Self::Number(_) => "a number",
            Self::Integer(_) => "an integer",
            Self::Boolean(_) => "a boolean",
            Self::Choice(_) => "a choice",
            Self::Color(_) => "a colour",
        }
    }
}

/// One field of the table and its value: the WIT `field` record.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub value: Value,
}

impl Field {
    pub fn new(name: impl Into<String>, value: Value) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }
}

/// Every field of the table with its value, in the manifest's order, as the host hands them to
/// [`Adjustment::coefficients`] and [`Adjustment::label`]. The typed getters answer an error that
/// names the field when it is missing or of another kind, for `?` in `coefficients`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Values {
    fields: Vec<Field>,
}

impl Values {
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields }
    }

    /// The fields in the manifest's order.
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// The value of the field `name`, if the table has one.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| &field.value)
    }

    fn typed<'a, T>(
        &'a self,
        name: &str,
        expected: &str,
        read: impl FnOnce(&'a Value) -> Option<T>,
    ) -> Result<T, String> {
        let value = self
            .get(name)
            .ok_or_else(|| format!("the values hold no field `{name}`"))?;
        read(value).ok_or_else(|| format!("field `{name}` holds {}, not {expected}", value.kind()))
    }

    pub fn number(&self, name: &str) -> Result<f64, String> {
        self.typed(name, "a number", |value| match value {
            Value::Number(number) => Some(*number),
            _ => None,
        })
    }

    pub fn integer(&self, name: &str) -> Result<i64, String> {
        self.typed(name, "an integer", |value| match value {
            Value::Integer(integer) => Some(*integer),
            _ => None,
        })
    }

    pub fn boolean(&self, name: &str) -> Result<bool, String> {
        self.typed(name, "a boolean", |value| match value {
            Value::Boolean(boolean) => Some(*boolean),
            _ => None,
        })
    }

    /// An enum field's option.
    pub fn choice(&self, name: &str) -> Result<&str, String> {
        self.typed(name, "a choice", |value| match value {
            Value::Choice(choice) => Some(choice.as_str()),
            _ => None,
        })
    }

    /// A colour field's 8-bit sRGB channels.
    pub fn color(&self, name: &str) -> Result<[u8; 3], String> {
        self.typed(name, "a colour", |value| match value {
            Value::Color(color) => Some(*color),
            _ => None,
        })
    }
}

/// A unit's words as `coefficients` builds them: each an `f32`'s bits or an integer, in the order
/// `apply_row` and the program read them (`lf_f32(words + i)` and `lf_word(words + i)` in WGSL).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Words {
    words: Vec<u32>,
}

impl Words {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an `f32`'s bits.
    pub fn push_f32(&mut self, value: f32) -> &mut Self {
        self.words.push(value.to_bits());
        self
    }

    /// Append an integer word.
    pub fn push_u32(&mut self, value: u32) -> &mut Self {
        self.words.push(value);
        self
    }

    pub fn into_vec(self) -> Vec<u32> {
        self.words
    }
}

/// Word `at` read as an `f32`, or `0.0` past the end: `apply_row` is never handed words its own
/// `coefficients` did not make, so a short list is a defect the harness shows rather than a trap.
pub fn word_f32(words: &[u32], at: usize) -> f32 {
    words.get(at).copied().map_or(0.0, f32::from_bits)
}

/// Make `$module`, a type implementing [`Adjustment`], the component's `adjust` interface: the
/// interface's three functions, their post-return functions and `cabi_realloc`, exported under the
/// names the canonical ABI gives them. Name exactly one type, once, in the module crate's root.
/// It exports nothing outside a WebAssembly build, so the same crate compiles natively for its
/// tests.
#[macro_export]
macro_rules! export_adjustment {
    ($module:ty) => {
        #[cfg(target_arch = "wasm32")]
        #[allow(unsafe_code)]
        const _: () = {
            #[unsafe(export_name = "cabi_realloc")]
            unsafe extern "C" fn __luxforge_realloc(
                old: *mut u8,
                old_len: usize,
                align: usize,
                new_len: usize,
            ) -> *mut u8 {
                unsafe { $crate::abi::realloc(old, old_len, align, new_len) }
            }

            #[unsafe(export_name = "luxforge:module/adjust@0.1.0#coefficients")]
            unsafe extern "C" fn __luxforge_coefficients(values: *mut u8, len: usize) -> *mut u8 {
                unsafe { $crate::abi::coefficients::<$module>(values, len) }
            }

            #[unsafe(export_name = "cabi_post_luxforge:module/adjust@0.1.0#coefficients")]
            unsafe extern "C" fn __luxforge_post_coefficients(answer: *mut u8) {
                unsafe { $crate::abi::post_coefficients(answer) }
            }

            #[unsafe(export_name = "luxforge:module/adjust@0.1.0#apply-row")]
            unsafe extern "C" fn __luxforge_apply_row(
                words: *mut u8,
                words_len: usize,
                y: u32,
                x0: u32,
                rgb: *mut u8,
                rgb_len: usize,
            ) -> *mut u8 {
                unsafe { $crate::abi::apply_row::<$module>(words, words_len, y, x0, rgb, rgb_len) }
            }

            #[unsafe(export_name = "cabi_post_luxforge:module/adjust@0.1.0#apply-row")]
            unsafe extern "C" fn __luxforge_post_apply_row(answer: *mut u8) {
                unsafe { $crate::abi::post_apply_row(answer) }
            }

            #[unsafe(export_name = "luxforge:module/adjust@0.1.0#label")]
            unsafe extern "C" fn __luxforge_label(values: *mut u8, len: usize) -> *mut u8 {
                unsafe { $crate::abi::label::<$module>(values, len) }
            }

            #[unsafe(export_name = "cabi_post_luxforge:module/adjust@0.1.0#label")]
            unsafe extern "C" fn __luxforge_post_label(answer: *mut u8) {
                unsafe { $crate::abi::post_label(answer) }
            }
        };
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values::new(vec![
            Field::new("amount", Value::Number(12.5)),
            Field::new("steps", Value::Integer(-3)),
            Field::new("preserve", Value::Boolean(true)),
            Field::new("mode", Value::Choice("soft".into())),
            Field::new("paper", Value::Color([255, 250, 240])),
        ])
    }

    #[test]
    fn typed_getters_read_each_kind_by_name() {
        let values = values();
        assert_eq!(values.number("amount"), Ok(12.5));
        assert_eq!(values.integer("steps"), Ok(-3));
        assert_eq!(values.boolean("preserve"), Ok(true));
        assert_eq!(values.choice("mode"), Ok("soft"));
        assert_eq!(values.color("paper"), Ok([255, 250, 240]));
        assert_eq!(values.fields().len(), 5);
    }

    #[test]
    fn typed_getters_name_a_missing_field_and_a_mismatched_kind() {
        let values = values();
        assert_eq!(
            values.number("missing"),
            Err("the values hold no field `missing`".into())
        );
        assert_eq!(
            values.number("mode"),
            Err("field `mode` holds a choice, not a number".into())
        );
        assert_eq!(
            values.color("steps"),
            Err("field `steps` holds an integer, not a colour".into())
        );
    }

    #[test]
    fn words_hold_f32_bits_and_integers_in_order() {
        let mut words = Words::new();
        words.push_f32(0.5).push_u32(7).push_f32(-2.0);
        let words = words.into_vec();
        assert_eq!(words, vec![0.5_f32.to_bits(), 7, (-2.0_f32).to_bits()]);
        assert_eq!(word_f32(&words, 0), 0.5);
        assert_eq!(word_f32(&words, 2), -2.0);
        assert_eq!(word_f32(&words, 3), 0.0);
    }
}
