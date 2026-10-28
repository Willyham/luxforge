# Authoring an adjustment module

How to write, package and check an external adjustment module: a sandboxed WebAssembly component with its own WGSL program, installed from a `.lfmodule` package. The contract is the [module manager design](../design/module-manager.md#phase-2-external-adjustment-modules); this guide is the workflow. The example, [`examples/modules/split-toning`](../../examples/modules/split-toning), is a complete module to copy.

Installing a package and the panel it draws arrive with the rest of phase 2. Today a module can be written, built, packaged, checked and held to its component, and a package reads through the core's package reader exactly as installing it will.

## What a module is

A module is a directory holding three things:

- **`Cargo.toml` and `src/lib.rs`**: a `cdylib` crate that depends on [`luxforge-module-sdk`](../../crates/luxforge-module-sdk) alone. It implements the SDK's `Adjustment` trait and names its type once with `export_adjustment!`.
- **`luxforge-module.toml`**: the manifest, with the module's identity, its one effect and its field table, in the schema of the design's [package and manifest](../design/module-manager.md#package-and-manifest). The host owns the table: it builds the module's `edit.set-<name>` and `edit.reset-<name>` actions, controls, history, presets and copy settings from it, so the module's code never sees an invalid value.
- **`program.wgsl`** (optional): the same arithmetic for the GPU. Without one, the module renders through its reference, which is correct but slower.

A crate outside the Luxforge workspace needs its own empty `[workspace]` table, as the example's has. Inside this repository the example is also listed in the root manifest's `exclude`.

## The Rust side

`Adjustment` has three functions, each pure:

- `coefficients(values) -> Result<Vec<u32>, String>`: the unit's **words** from every field's canonical value, in the manifest's order. `Values` reads a field by name and kind (`number`, `integer`, `boolean`, `choice`, `color`) and answers an error naming the field otherwise, so `?` refuses a mismatched table. `Words` builds the list: `push_f32` stores an `f32`'s bits and `push_u32` an integer. The words are the unit's identity: equal values must give equal words. Do the expensive, once-per-layer work here (angles, normalizations, reciprocals), so the per-pixel code has none.
- `apply_row(words, y, x0, rgb)`: the **CPU reference**, transforming one contiguous run of row `y` from column `x0` in place, as linear-sRGB (D65) triplets that may leave 0..1, under the core's `PointwiseColor` contract. Read words with `word_f32`.
- `label(values) -> Option<String>`: an optional history summary; the default keeps the host's own.

The component imports nothing: no WASI, clock, randomness, files or network. The SDK writes the interface's exports against the canonical ABI itself and depends on no crate, so it adds none of those. Build for `wasm32-unknown-unknown`, never a WASI target.

**Bit-identical arithmetic.** The reference must give the same bits natively and in WebAssembly, so a module's native unit tests speak for its component. Use the basic IEEE operations: `+ - * /`, `sqrt`, `abs`, `min`, `max`, `floor`, `clamp` and comparisons. Avoid `powf`, `exp`, `ln`, `sin`, `cos` and the other transcendental functions of `std`: the native ones come from the platform's maths library and the WebAssembly ones from Rust's, and they differ in the last bit. Approximate a curve with a polynomial or a table in the words instead. The harness below finds any difference.

## The GPU program

`program.wgsl` declares one entry, `fn lf_module(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32>`, and only functions and constants whose names start with `lf_module`: no bindings, globals, types, overrides or entry points. It is assembled after the surface's prelude, which declares `lf_word(i)` and `lf_f32(i)`: word `k` of the unit is `lf_f32(words + k)`. `rgb` is the unclamped linear pixel and `pos` its integer position as `f32`, the `(x0 + i, y)` that `apply_row` addresses.

Keep the program the reference's arithmetic, step for step, so the GPU stays within the colour class's tolerance ([GPU-first](../design/gpu-first.md)): pointwise, mean ΔE00 0.5, worst 16 × 16 block 1.0, p99 2.0 and |signed mean ΔL\*| 0.25 against the reference, as 8-bit sRGB. The host validates the program when it loads it and enables it only after a probe at first use; a program outside the limits leaves the module on its reference.

## Testing

Write ordinary unit tests in the module crate against `Adjustment` natively. The SDK's `host` feature adds the tooling a native test can use:

- `host::build_component(dir, target_dir)` builds the crate at `dir` for `wasm32-unknown-unknown` with the repository's pinned toolchain and makes it a component.
- `host::harness::assert_identical::<Module>(&component, &value_sets)` runs `coefficients`, `label` and `apply_row` natively and on the module engine itself, `luxforge-wasm`, with the host's configuration, memory limit and deadline, over the SDK's fixed patches for each named value set, each row as two runs, and panics with the first difference: the value set, the patch, the pixel and channel, and both values with their bits. Two NaNs count as identical, since the engine canonicalizes NaN payloads.

`tools/cargo-cached test -p luxforge-module-sdk` holds the example this way: its source is compiled into the SDK's test natively and its crate built and run as a component.

The pinned toolchain needs the WebAssembly target: `rustup target add wasm32-unknown-unknown --toolchain 1.96.0`.

## Packaging and checking

```sh
tools/cargo-cached xtask module-package examples/modules/split-toning --output target/evidence/split-toning.lfmodule
tools/cargo-cached xtask module-check target/evidence/split-toning.lfmodule [--json NEW_FILE]
```

`module-package DIR --output FILE` reads the manifest first, so a table the host would refuse fails before anything is built. It then builds the crate in release for `wasm32-unknown-unknown` with the pinned toolchain into `target/modules`, keeping the crate's `Cargo.lock` (`--locked`) when it has one, makes the core module a component, and writes the three files at the package's root, Deflate, at a fixed time, so the same inputs make the same package. The package is read back through the core's reader before it replaces `FILE`, and the command prints its hash.

`module-check FILE` reports what installing and first use will find, each step naming what it refuses:

1. **Package**: the core's reader, with the refusal's code (`content`, `manifest`, `reserved-identity` and the rest).
2. **Component**: loaded by the module engine as the host loads it, refused if it imports anything or does not export the `adjust` interface. The engine instantiates it on the reference's first call.
3. **Reference**: `coefficients` for value sets the table gives (the defaults; every field at its least, middle and greatest; each field at each end with the rest at their defaults; three mixed sets), and `apply_row` over the fixed patches for each: every answer accepted, the right length and finite, and the same from a second instance. A failure names the value set and the patch, such as ``value set `defaults`: coefficients refused: the values hold no field `highlight-hue` ``.
4. **Program**: validated by `naga` under the prelude with a harness that calls `lf_module`, and refused for any name or declaration the contract does not allow.
5. **GPU**: the program alone, in a compute harness on the local adapter (the first hardware adapter of Vulkan, Metal or DirectX 12), over the same patches and value sets, against the reference by the pointwise class's limits. It is reported, not gated: outside the limits, the command names the cells and says that the host would render through the reference. Without an adapter it says it did not measure.

`module-check` reports; it does not qualify a module. The host checks a package again when it is installed and probes its program at first use, on the person's adapter, inside the real renderer.

## Outstanding

`module-check`'s GPU step runs the program alone; the host's probe runs it in the renderer once runtime programs are loaded.
