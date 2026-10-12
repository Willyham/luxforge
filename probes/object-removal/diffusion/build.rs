//! The harness builds the editor's generative runtime from its one copy of the pinned sources:
//! `crates/luxforge-diffusion`'s vendored stable-diffusion.cpp and ggml, compiled by that crate's
//! own build module.
#[path = "../../../crates/luxforge-diffusion/native/build.rs"]
mod native_build;

fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../../../crates/luxforge-diffusion");
    println!("cargo:rerun-if-changed=build.rs");
    native_build::build(&root.canonicalize().expect("crates/luxforge-diffusion"));
}
