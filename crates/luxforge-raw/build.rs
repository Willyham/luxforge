#[path = "src/limits.rs"]
mod limits;
// The catalog is validated against the one opcode allowlist; the list tags it also names are for
// the library's container parser.
#[path = "src/opcodes.rs"]
#[allow(dead_code)]
mod opcodes;
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

/// Borrowed static text: a `Debug`-escaped string is a valid Rust string literal.
fn text(value: &str) -> String {
    format!("Cow::Borrowed({value:?})")
}

fn mode(mode: &profiles::Mode) -> String {
    let compression = mode.compression.map_or_else(
        || "None".to_string(),
        |compression| {
            format!(
                "Some(Compression {{ probe: CompressionProbe::{:?}, value: {} }})",
                compression.probe, compression.value
            )
        },
    );
    format!(
        "Mode {{ id: {}, bits: {}, raw_count: {}, decoder: {}, dng_version: {:?}, validation: ModeValidation::{:?}, compression: {compression} }}",
        text(&mode.id),
        mode.bits,
        mode.raw_count,
        text(&mode.decoder),
        mode.dng_version,
        mode.validation,
    )
}

/// The validated catalog as static Rust data, so the library never parses JSON. Each camera's
/// modes and opcodes are named statics its entry borrows. `Debug` prints every `f64` as its
/// shortest round-tripping literal, so the coefficients are exact.
fn static_catalog(catalog: &profiles::Catalog) -> String {
    let mut out = String::from(
        "// Generated from data/cameras.json; do not edit.\nuse crate::profiles::*;\nuse std::borrow::Cow;\n",
    );
    let mut cameras = Vec::new();
    for (index, camera) in catalog.cameras.iter().enumerate() {
        let modes: Vec<String> = camera.modes.iter().map(mode).collect();
        out.push_str(&format!(
            "static MODES_{index}: [Mode; {}] = [{}];\n",
            modes.len(),
            modes.join(", ")
        ));
        let dng = match &camera.dng {
            None => "None".to_string(),
            Some(dng) => {
                let opcodes: Vec<String> = dng
                    .required_opcodes
                    .iter()
                    .map(|op| {
                        format!(
                            "Opcode {{ id: {}, list: {}, version: {}, flags: {} }}",
                            op.id, op.list, op.version, op.flags
                        )
                    })
                    .collect();
                out.push_str(&format!(
                    "static OPCODES_{index}: [Opcode; {}] = [{}];\n",
                    opcodes.len(),
                    opcodes.join(", ")
                ));
                format!(
                    "Some(Dng {{ container: DngContainer::{:?}, calibration: DngCalibration::{:?}, illuminants: {:?}, selected_matrix: {}, calibration_identity: {}, corrections: DngCorrections::{:?}, interpretation: {}, required_opcodes: Cow::Borrowed(&OPCODES_{index}), decoder_active_bottom_trim: {} }})",
                    dng.container,
                    dng.calibration,
                    dng.illuminants,
                    dng.selected_matrix,
                    text(&dng.calibration_identity),
                    dng.corrections,
                    text(&dng.interpretation),
                    dng.decoder_active_bottom_trim,
                )
            }
        };
        let calibration = camera.calibration.as_ref().map_or_else(
            || "None".to_string(),
            |calibration| {
                format!(
                    "Some(Calibration {{ xyz_to_camera: {:?}, source: {}, license: {} }})",
                    calibration.xyz_to_camera,
                    text(&calibration.source),
                    text(&calibration.license),
                )
            },
        );
        cameras.push(format!(
            "Camera {{ make: {}, model: {}, sensor_size: {:?}, cfa_size: {:?}, crop: Crop::{:?}, dng: {dng}, calibration: {calibration}, modes: Cow::Borrowed(&MODES_{index}) }}",
            text(&camera.make),
            text(&camera.model),
            camera.sensor_size,
            camera.cfa_size,
            camera.crop,
        ));
    }
    out.push_str(&format!(
        "static CAMERAS: [Camera; {}] = [\n{}\n];\npub(crate) static CATALOG: Catalog = Catalog {{ version: {}, cameras: Cow::Borrowed(&CAMERAS) }};\n",
        cameras.len(),
        cameras.join(",\n"),
        catalog.version
    ));
    out
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let libraw = manifest.join("vendor/libraw-0.22.2");
    let rt = manifest.join("vendor/librtprocess-9a858270");
    let catalog = profiles::Catalog::parse(include_str!("data/cameras.json"))
        .expect("invalid RAW camera catalog");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("output dir"));
    let mut native = String::from(
        "// Generated from data/cameras.json; do not edit.\nstatic const struct { const char *make, *model; bool calibrated; double xyz_to_camera[9]; } lf_cameras[] = {\n",
    );
    let mut rust = String::from(
        "// Generated from data/cameras.json; do not edit.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]\npub enum RawMode {\n",
    );
    let mut names =
        String::from("impl RawMode { pub(crate) fn id(self) -> &'static str { match self {\n");
    for camera in catalog.cameras.iter() {
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
        for mode in camera.modes.iter() {
            rust.push_str(&format!("{},\n", mode.id));
            names.push_str(&format!("Self::{} => \"{}\",\n", mode.id, mode.id));
        }
    }
    native.push_str("};\n");
    rust.push_str("}\n");
    names.push_str("} } }\n");
    rust.push_str(&names);
    native.push_str(&format!(
        "\n#define LF_MAX_SOURCE_BYTES {}ull\n#define LF_MAX_PIXELS {}ull\n#define LF_MAX_SIDE {}u\n#define LF_MAX_RGB_BYTES {}ull\n",
        limits::MAX_SOURCE_BYTES, limits::MAX_PIXELS, limits::MAX_SIDE, limits::MAX_RGB_BYTES
    ));
    fs::write(out.join("camera_allowlist.h"), native).expect("write native camera table");
    fs::write(out.join("raw_modes.rs"), rust).expect("write mode identifiers");
    fs::write(out.join("camera_catalog.rs"), static_catalog(&catalog))
        .expect("write static camera catalog");
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .include(&libraw)
        .include(&out)
        .include(rt.join("src/include"))
        .define("LIBRTPROCESS_STATIC", None)
        // libraw is compiled directly into this static library, never as a
        // DLL and never consumed as one. On MSVC, libraw.h auto-enables
        // LIBRAW_WIN32_DLLDEFS, which makes exported symbols dllimport
        // unless told otherwise, and MSVC rejects defining a dllimport
        // function. LIBRAW_NODLL disables that path on all platforms.
        .define("LIBRAW_NODLL", None)
        .file(manifest.join("native/adapter.cpp"));
    // No USE_ZLIB/JPEG/RAWSPEED/DNGSDK/LCMS or OpenMP features.
    // The qualified NEF/RAF/DNG decoding paths do not require them.
    add_cpp_tree(&mut build, &libraw.join("src"), "cpp");
    for source in ["rcd.cc", "markesteijn.cc", "border.cc"] {
        build.file(rt.join("src/demosaic").join(source));
    }
    build.compile("luxforge_raw_native");
    println!("cargo:rerun-if-changed=data/cameras.json");
    println!("cargo:rerun-if-changed=src/profiles.rs");
    println!("cargo:rerun-if-changed=src/opcodes.rs");
    println!("cargo:rerun-if-changed=src/limits.rs");
    println!("cargo:rerun-if-changed=native/adapter.cpp");
    println!("cargo:rerun-if-changed=vendor/libraw-0.22.2");
    println!("cargo:rerun-if-changed=vendor/librtprocess-9a858270");
}
