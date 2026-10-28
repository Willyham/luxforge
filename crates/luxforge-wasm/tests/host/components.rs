//! The tests' components: one hand-written core module in WAT, a gain-and-lift adjustment with a
//! position-dependent term, wrapped into a component of `wit/adjustment.wit` by `wit-component`,
//! and variants of it that break the contract one way each. [`native`] is the same arithmetic in
//! Rust.
use luxforge_core::module_package::{FieldEntry, FieldValue, ModuleManifest, ModulePackage};
use sha2::{Digest, Sha256};
use std::path::Path;
use wit_component::{ComponentEncoder, StringEncoding};
use wit_parser::Resolve;

/// The test module's manifest: two numbers, `gain` and `lift`.
pub const MANIFEST: &str = r#"
api = "0.1"
id = "org.example.gainlift"
title = "Gain and lift"
hint = "Scale and lift every channel"
version = "1.0.0"
authors = ["Example"]
license = "MIT"
repository = "https://example.org/gainlift"
toolset = "adjustments"

[effect]
stage = "color"
format = 1

[[fields]]
name = "gain"
kind = "number"
min = -100
max = 100
default = 0
label = "Gain"

[[fields]]
name = "lift"
kind = "number"
min = -100
max = 100
default = 0
label = "Lift"

[[groups]]
label = "Gain and lift"
fields = ["gain", "lift"]
"#;

pub const EFFECT: &str = "org.example.gainlift.adjust";

/// How a variant breaks the contract, in `apply-row` unless it says otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// The contract kept.
    Valid,
    /// Traps (`unreachable`).
    Traps,
    /// Never returns.
    Spins,
    /// Answers one pixel fewer than it was given.
    Short,
    /// Grows its memory by 64 MiB, past every limit the tests set, and traps when it cannot.
    Grows,
    /// Imports a function, `log`, from the host.
    Imports,
}

/// The core module: a bump allocator reset by every post-return function, and the three exports.
/// The return areas sit at fixed addresses below the heap: `coefficients` at 32, `apply-row` at 48
/// and `label` at 56; the label's text at 128 and the refusal's at 160.
fn core_module(variant: Variant) -> String {
    let apply_prefix = match variant {
        Variant::Traps => "unreachable",
        Variant::Spins => "(loop $spin (br $spin))",
        Variant::Grows => {
            "(if (i32.eq (memory.grow (i32.const 1024)) (i32.const -1)) (then unreachable))"
        }
        _ => "",
    };
    let answered = match variant {
        Variant::Short => "(i32.sub (local.get $n) (i32.const 3))",
        _ => "(local.get $n)",
    };
    let import = match variant {
        Variant::Imports => r#"(import "$root" "log" (func $log (param i32 i32)))"#,
        _ => "",
    };
    let log = match variant {
        Variant::Imports => "(call $log (i32.const 128) (i32.const 4))",
        _ => "",
    };
    format!(
        r#"
(module
  {import}
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))
  (data (i32.const 128) "Gain")
  (data (i32.const 160) "expected two fields")

  (func $realloc (export "cabi_realloc")
    (param $old i32) (param $old_size i32) (param $align i32) (param $size i32) (result i32)
    (local $ptr i32) (local $end i32) (local $have i32)
    (local.set $ptr
      (i32.and
        (i32.add (global.get $heap) (i32.sub (local.get $align) (i32.const 1)))
        (i32.sub (i32.const 0) (local.get $align))))
    (local.set $end (i32.add (local.get $ptr) (local.get $size)))
    (local.set $have (i32.mul (memory.size) (i32.const 65536)))
    (if (i32.gt_u (local.get $end) (local.get $have))
      (then
        (if (i32.eq
              (memory.grow
                (i32.div_u
                  (i32.add (i32.sub (local.get $end) (local.get $have)) (i32.const 65535))
                  (i32.const 65536)))
              (i32.const -1))
          (then unreachable))))
    (global.set $heap (local.get $end))
    (local.get $ptr))

  (func $reset (param i32) (global.set $heap (i32.const 1024)))
  (export "cabi_post_luxforge:module/adjust@0.1.0#coefficients" (func $reset))
  (export "cabi_post_luxforge:module/adjust@0.1.0#apply-row" (func $reset))
  (export "cabi_post_luxforge:module/adjust@0.1.0#label" (func $reset))

  ;; words = [f32(1 + gain/100), f32(lift/100)], from the two fields' numbers (a field is 24
  ;; bytes: its name, then its value's tag at 8 and its number at 16).
  (func (export "luxforge:module/adjust@0.1.0#coefficients")
    (param $ptr i32) (param $len i32) (result i32)
    (local $words i32)
    {log}
    (if (i32.ne (local.get $len) (i32.const 2))
      (then
        (i32.store8 (i32.const 32) (i32.const 1))
        (i32.store (i32.const 36) (i32.const 160))
        (i32.store (i32.const 40) (i32.const 19))
        (return (i32.const 32))))
    (local.set $words (call $realloc (i32.const 0) (i32.const 0) (i32.const 4) (i32.const 8)))
    (f32.store (local.get $words)
      (f32.demote_f64
        (f64.add (f64.const 1) (f64.mul (f64.load offset=16 (local.get $ptr)) (f64.const 0.01)))))
    (f32.store offset=4 (local.get $words)
      (f32.demote_f64 (f64.mul (f64.load offset=40 (local.get $ptr)) (f64.const 0.01))))
    (i32.store8 (i32.const 32) (i32.const 0))
    (i32.store (i32.const 36) (local.get $words))
    (i32.store (i32.const 40) (i32.const 2))
    (i32.const 32))

  ;; r = r * gain + lift + (x0 + k) * 0.0001; g = g * gain + lift; b = b * gain + lift + y * 0.0005
  (func (export "luxforge:module/adjust@0.1.0#apply-row")
    (param $w i32) (param $wl i32) (param $y i32) (param $x0 i32) (param $p i32) (param $n i32)
    (result i32)
    (local $gain f32) (local $lift f32) (local $yf f32) (local $k i32) (local $at i32)
    {apply_prefix}
    (local.set $gain (f32.load (local.get $w)))
    (local.set $lift (f32.load offset=4 (local.get $w)))
    (local.set $yf (f32.mul (f32.convert_i32_u (local.get $y)) (f32.const 0.0005)))
    (block $done
      (loop $next
        (br_if $done (i32.ge_u (i32.mul (local.get $k) (i32.const 3)) (local.get $n)))
        (local.set $at (i32.add (local.get $p) (i32.mul (local.get $k) (i32.const 12))))
        (f32.store (local.get $at)
          (f32.add
            (f32.add
              (f32.mul (f32.load (local.get $at)) (local.get $gain))
              (local.get $lift))
            (f32.mul
              (f32.convert_i32_u (i32.add (local.get $x0) (local.get $k)))
              (f32.const 0.0001))))
        (f32.store offset=4 (local.get $at)
          (f32.add
            (f32.mul (f32.load offset=4 (local.get $at)) (local.get $gain))
            (local.get $lift)))
        (f32.store offset=8 (local.get $at)
          (f32.add
            (f32.add
              (f32.mul (f32.load offset=8 (local.get $at)) (local.get $gain))
              (local.get $lift))
            (local.get $yf)))
        (local.set $k (i32.add (local.get $k) (i32.const 1)))
        (br $next)))
    (i32.store (i32.const 48) (local.get $p))
    (i32.store (i32.const 52) {answered})
    (i32.const 48))

  (func (export "luxforge:module/adjust@0.1.0#label")
    (param $ptr i32) (param $len i32) (result i32)
    (i32.store8 (i32.const 56) (i32.const 1))
    (i32.store (i32.const 60) (i32.const 128))
    (i32.store (i32.const 64) (i32.const 4))
    (i32.const 56))
)
"#
    )
}

/// A world that exports `adjust` and also imports `log`.
const IMPORTING: &str = r#"
package luxforge:module-test;

world importing {
    import log: func(message: string);
    export luxforge:module/adjust@0.1.0;
}
"#;

/// The component of `variant`.
pub fn component(variant: Variant) -> Vec<u8> {
    let mut resolve = Resolve::default();
    let wit = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wit/adjustment.wit");
    let package = resolve.push_file(&wit).expect("the interface reads");
    let world = if variant == Variant::Imports {
        let test = resolve
            .push_str("importing.wit", IMPORTING)
            .expect("the importing world reads");
        resolve.select_world(&[test], Some("importing"))
    } else {
        resolve.select_world(&[package], Some("adjustment"))
    }
    .expect("the world");
    let mut module = wat::parse_str(core_module(variant)).expect("the core module assembles");
    wit_component::embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8)
        .expect("the metadata embeds");
    ComponentEncoder::default()
        .module(&module)
        .expect("the module is a core module of the world")
        .validate(true)
        .encode()
        .expect("the component encodes")
}

/// The package of `variant`'s component, with the hash of its component's bytes.
pub fn package(variant: Variant) -> ModulePackage {
    let component = component(variant);
    ModulePackage {
        manifest: ModuleManifest::parse(MANIFEST).expect("the manifest reads"),
        hash: format!("sha256:{:x}", Sha256::digest(&component)),
        component,
        program: None,
    }
}

/// Every field at these values, in the manifest's order.
pub fn values(gain: f64, lift: f64) -> Vec<FieldEntry> {
    vec![
        FieldEntry {
            name: "gain".into(),
            value: FieldValue::Number(gain),
        },
        FieldEntry {
            name: "lift".into(),
            value: FieldValue::Number(lift),
        },
    ]
}

/// The same arithmetic in Rust: the component's words, and its row.
pub mod native {
    use luxforge_core::module_package::{AdjustmentUnits, FieldEntry, FieldValue};
    use luxforge_core::{ColorOperation, CompileStage, Error, OperationIdentity, PointwiseColor};
    use std::sync::Arc;

    pub fn words(values: &[FieldEntry]) -> [u32; 2] {
        let number = |at: usize| match values[at].value {
            FieldValue::Number(value) => value,
            _ => panic!("a number"),
        };
        [
            ((1.0 + number(0) * 0.01) as f32).to_bits(),
            ((number(1) * 0.01) as f32).to_bits(),
        ]
    }

    pub fn apply_row(words: [u32; 2], y: u32, x0: u32, rgb: &mut [[f32; 3]]) {
        let [gain, lift] = words.map(f32::from_bits);
        let yf = y as f32 * 0.0005;
        for (k, [r, g, b]) in rgb.iter_mut().enumerate() {
            *r = *r * gain + lift + (x0 + k as u32) as f32 * 0.0001;
            *g = *g * gain + lift;
            *b = *b * gain + lift + yf;
        }
    }

    /// The native units, for the same manifest's module in a registry of its own.
    pub struct Units;

    struct Unit([u32; 2]);

    impl AdjustmentUnits for Units {
        fn compile(&self, values: &[FieldEntry], _: CompileStage) -> Result<ColorOperation, Error> {
            Ok(ColorOperation::new(vec![Arc::new(Unit(words(values)))]))
        }
    }

    impl PointwiseColor for Unit {
        fn apply_row(&self, y: u32, x0: u32, rgb: &mut [[f32; 3]]) {
            apply_row(self.0, y, x0, rgb);
        }
        fn is_finite(&self) -> bool {
            true
        }
        fn identity(&self) -> OperationIdentity {
            OperationIdentity::new("test.native", self.0.map(u64::from))
        }
        fn describe(&self) -> String {
            "native gain and lift".into()
        }
    }
}
