fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Profile identity is independent of a caller's debug-assertions override.
    println!(
        "cargo:rustc-env=LUXFORGE_XTASK_PROFILE={}",
        std::env::var("PROFILE").expect("Cargo supplies the build profile")
    );
}
