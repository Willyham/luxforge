//! Marks a tag build, one compiled with `LUXFORGE_RELEASE_VERSION`, with
//! `cfg(luxforge_release_build)`. A tag build leaves out what only a checkout has: it never looks
//! for resources in the checkout it was built from, and embeds no checkout path.
fn main() {
    println!("cargo::rerun-if-env-changed=LUXFORGE_RELEASE_VERSION");
    println!("cargo::rustc-check-cfg=cfg(luxforge_release_build)");
    if std::env::var_os("LUXFORGE_RELEASE_VERSION").is_some() {
        println!("cargo::rustc-cfg=luxforge_release_build");
    }
}
