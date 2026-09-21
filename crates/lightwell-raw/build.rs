use std::{
    fs,
    path::{Path, PathBuf},
};

fn add_cpp_tree(build: &mut cc::Build, root: &Path, extension: &str) {
    let mut dirs = vec![root.to_path_buf()];
    let mut sources = Vec::<PathBuf>::new();
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).expect("read bundled native sources") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == extension) {
                sources.push(path);
            }
        }
    }
    sources.sort();
    assert!(!sources.is_empty(), "bundled native sources missing");
    for source in sources {
        build.file(source);
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let libraw = manifest.join("vendor/libraw-0.22.2");
    let rt = manifest.join("vendor/librtprocess-9a858270");
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .include(&libraw)
        .include(rt.join("src/include"))
        .define("LIBRTPROCESS_STATIC", None)
        .file(manifest.join("native/adapter.cpp"));
    // No USE_ZLIB/JPEG/RAWSPEED/DNGSDK/LCMS or OpenMP features. The qualified
    // lossless NEF and RAF paths do not require them. DNG remains unqualified
    // until its mandatory opcodes are supported.
    add_cpp_tree(&mut build, &libraw.join("src"), "cpp");
    for source in ["rcd.cc", "markesteijn.cc", "border.cc"] {
        build.file(rt.join("src/demosaic").join(source));
    }
    build.compile("lightwell_raw_native");
    println!("cargo:rerun-if-changed=native/adapter.cpp");
    println!("cargo:rerun-if-changed=vendor/libraw-0.22.2");
    println!("cargo:rerun-if-changed=vendor/librtprocess-9a858270");
}
