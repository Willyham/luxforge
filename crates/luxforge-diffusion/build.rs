//! The vendored stable-diffusion.cpp and ggml, compiled by [`native_build::build`] (shared with the
//! object removal probe's build script).
#[path = "native/build.rs"]
mod native_build;

fn main() {
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=build.rs");
    native_build::build(&root);
}
