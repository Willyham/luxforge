#[path = "src/profiles.rs"]
mod profiles;

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
            } else if path.extension().is_some_and(|ext| ext == extension)
                && path
                    .file_name()
                    .is_none_or(|name| name != "postprocessing_ph.cpp")
            {
                // Upstream supplies this alternative stub file for builds without
                // postprocessing. Do not link it alongside the real implementations.
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
    let catalog = profiles::Catalog::parse(include_str!("data/cameras.json"))
        .expect("invalid RAW camera catalog");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("output dir"));
    let mut native = String::from(
        "// Generated from data/cameras.json; do not edit.\nstatic const struct { const char *make, *model; bool calibrated; double xyz_to_camera[9]; } lw_cameras[] = {\n",
    );
    let mut rust = String::from(
        "// Generated from data/cameras.json; do not edit.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]\npub enum RawMode {\n",
    );
    let mut names =
        String::from("impl RawMode { pub(crate) fn id(self) -> &'static str { match self {\n");
    for camera in &catalog.cameras {
        let (calibrated, matrix) = camera
            .calibration
            .as_ref()
            .map(|calibration| {
                (
                    "true",
                    calibration
                        .xyz_to_camera
                        .iter()
                        .flatten()
                        .map(|value| format!("{value:.17e}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            })
            .unwrap_or_else(|| ("false", String::new()));
        let matrix = if matrix.is_empty() {
            "0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0".to_string()
        } else {
            matrix
        };
        native.push_str(&format!(
            "{{\"{}\", \"{}\", {}, {{{}}}}},\n",
            camera.make, camera.model, calibrated, matrix
        ));
        for mode in &camera.modes {
            rust.push_str(&format!("{},\n", mode.id));
            names.push_str(&format!("Self::{} => \"{}\",\n", mode.id, mode.id));
        }
    }
    native.push_str("};\n");
    rust.push_str("}\n");
    names.push_str("} } }\n");
    rust.push_str(&names);
    fs::write(out.join("camera_allowlist.h"), native).expect("write native camera table");
    fs::write(out.join("raw_modes.rs"), rust).expect("write mode identifiers");
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .include(&libraw)
        .include(&out)
        .include(rt.join("src/include"))
        .define("LIBRTPROCESS_STATIC", None)
        .file(manifest.join("native/adapter.cpp"));
    // No USE_ZLIB/JPEG/RAWSPEED/DNGSDK/LCMS or OpenMP features.
    // The qualified NEF/RAF/DNG decoding paths do not require them.
    add_cpp_tree(&mut build, &libraw.join("src"), "cpp");
    for source in ["rcd.cc", "markesteijn.cc", "border.cc"] {
        build.file(rt.join("src/demosaic").join(source));
    }
    build.compile("lightwell_raw_native");
    println!("cargo:rerun-if-changed=data/cameras.json");
    println!("cargo:rerun-if-changed=src/profiles.rs");
    println!("cargo:rerun-if-changed=native/adapter.cpp");
    println!("cargo:rerun-if-changed=vendor/libraw-0.22.2");
    println!("cargo:rerun-if-changed=vendor/librtprocess-9a858270");
}
