// Only the RAW-only admission bounds are used here; the cross-crate rendering bounds beside them
// are for luxforge-core's callers.
#[path = "src/limits.rs"]
#[allow(dead_code)]
mod limits;
// The catalog's calibration determinant.
#[path = "src/mat3.rs"]
mod mat3;
// The native adapter's status codes, written into its header.
#[path = "src/native_status.rs"]
mod native_status;
// The catalog is validated against the one opcode allowlist; the list tags it also names are for
// the library's container parser.
#[path = "src/opcodes.rs"]
#[allow(dead_code)]
mod opcodes;
#[path = "src/profiles.rs"]
mod profiles;
// The replaceable-decoder table: the catalog is validated against it and it is written into the
// native adapter's header.
#[path = "src/unpacker.rs"]
mod unpacker;

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Add every `extension` source under `root` except the `excluded` paths, given relative to
/// `root`, each of which must exist.
fn add_cpp_tree(build: &mut cc::Build, root: &Path, extension: &str, excluded: &[&str]) {
    let excluded: Vec<PathBuf> = excluded.iter().map(|path| root.join(path)).collect();
    for path in &excluded {
        assert!(path.is_file(), "excluded native source missing: {path:?}");
    }
    let mut dirs = vec![root.to_path_buf()];
    let mut sources = Vec::<PathBuf>::new();
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).expect("read bundled native sources") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == extension)
                && !excluded.contains(&path)
                && path
                    .file_name()
                    .is_none_or(|name| name != "postprocessing_ph.cpp" && name != "write_ph.cpp")
            {
                // Upstream supplies alternative stubs for builds without postprocessing or
                // output writing. Do not link them alongside the real implementations.
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

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create staged native source directory");
    for entry in fs::read_dir(source).expect("read bundled native source directory") {
        let entry = entry.expect("bundled native source entry");
        let target = destination.join(entry.file_name());
        if entry
            .file_type()
            .expect("bundled native source type")
            .is_dir()
        {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("stage bundled native source");
        }
    }
}

fn hunk_position(token: &str, prefix: char) -> (usize, usize) {
    let coordinates = token.strip_prefix(prefix).expect("patch hunk coordinate");
    let (start, count) = coordinates.split_once(',').expect("patch hunk count");
    (
        start.parse().expect("patch hunk start"),
        count.parse().expect("patch hunk length"),
    )
}

/// Apply the checked-in unified diff without an external `patch` or Git executable. Exact
/// context matching deliberately rejects an upstream update until its patch is reviewed.
fn apply_patch(root: &Path, patch: &str, allowed: &[&str]) {
    let mut lines = patch.lines().peekable();
    while let Some(header) = lines.next() {
        let name = header.strip_prefix("--- a/").expect("patch source header");
        let updated = lines
            .next()
            .and_then(|line| line.strip_prefix("+++ b/"))
            .expect("patch target header");
        assert_eq!(name, updated, "patch cannot rename native source files");
        assert!(
            allowed.contains(&name),
            "patch targets an unexpected native source: {name}"
        );
        let path = root.join(name);
        let original = fs::read_to_string(&path)
            .expect("read staged native source")
            .replace("\r\n", "\n");
        let source_lines: Vec<_> = original.split_inclusive('\n').collect();
        let mut result = String::new();
        let mut cursor = 0;
        while lines.peek().is_some_and(|line| line.starts_with("@@ ")) {
            let hunk = lines.next().expect("patch hunk");
            let mut fields = hunk.split_whitespace();
            assert_eq!(fields.next(), Some("@@"), "patch hunk marker");
            let (old_start, old_count) =
                hunk_position(fields.next().expect("old hunk position"), '-');
            let (new_start, new_count) =
                hunk_position(fields.next().expect("new hunk position"), '+');
            assert!(
                old_start > 0 && new_start > 0,
                "unsupported empty-file hunk"
            );
            assert!(old_start > cursor && old_start - 1 <= source_lines.len());
            for line in &source_lines[cursor..old_start - 1] {
                result.push_str(line);
            }
            assert_eq!(
                result.lines().count(),
                new_start - 1,
                "patch hunk position drift in {name}"
            );
            cursor = old_start - 1;
            let (mut consumed, mut emitted) = (0, 0);
            while let Some(line) = lines.peek() {
                if line.starts_with("@@ ") || line.starts_with("--- a/") {
                    break;
                }
                let line = lines.next().expect("patch content");
                let (kind, content) = line.split_at(1);
                match kind {
                    " " | "-" => {
                        assert_eq!(
                            source_lines.get(cursor).copied(),
                            Some(format!("{content}\n").as_str()),
                            "patch context differs from upstream in {name} at line {}",
                            cursor + 1
                        );
                        cursor += 1;
                        consumed += 1;
                        if kind == " " {
                            result.push_str(content);
                            result.push('\n');
                            emitted += 1;
                        }
                    }
                    "+" => {
                        result.push_str(content);
                        result.push('\n');
                        emitted += 1;
                    }
                    _ => panic!("unsupported patch line in {name}: {line}"),
                }
            }
            assert_eq!(consumed, old_count, "patch removed-line count in {name}");
            assert_eq!(emitted, new_count, "patch added-line count in {name}");
        }
        assert!(cursor > 0, "patch has no hunks for {name}");
        for line in &source_lines[cursor..] {
            result.push_str(line);
        }
        fs::write(path, result).expect("write patched native source");
    }
}

/// `UnsupportedCfa` as `UNSUPPORTED_CFA`: a status variant's name as a C enumerator's suffix.
fn screaming_snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// Borrowed static text: a `Debug`-escaped string is a valid Rust string literal.
fn text(value: &str) -> String {
    format!("Cow::Borrowed({value:?})")
}

fn dng_value(dng: Option<&profiles::Dng>) -> String {
    match dng {
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
            let role = |value: Option<profiles::DngOpticalRole>| {
                value.map_or_else(
                    || "None".to_string(),
                    |role| format!("Some(DngOpticalRole::{role:?})"),
                )
            };
            let optics = dng.optics.map_or_else(|| "None".to_string(), |optics| {
                    format!("Some(DngOptics {{ gain_map: {}, warp_rectilinear: {}, fix_vignette_radial: {} }})",
                        role(optics.gain_map), role(optics.warp_rectilinear), role(optics.fix_vignette_radial))
                });
            format!(
                "Some(Dng {{ container: DngContainer::{:?}, calibration: DngCalibration::{:?}, illuminants: {:?}, selected_matrix: {}, calibration_identity: {}, corrections: DngCorrections::{:?}, interpretation: {}, optics: {optics}, required_opcodes: Cow::Borrowed(&[{}]), decoder_active_bottom_trim: {} }})",
                dng.container,
                dng.calibration,
                dng.illuminants,
                dng.selected_matrix,
                text(&dng.calibration_identity),
                dng.corrections,
                text(&dng.interpretation),
                opcodes.join(", "),
                dng.decoder_active_bottom_trim,
            )
        }
    }
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
    let processing = mode.processing.as_ref().map_or_else(
        || "None".to_string(),
        |processing| {
            format!(
                "Some(Processing {{ crop: Crop::{:?}, dng: {} }})",
                processing.crop,
                dng_value(processing.dng.as_ref())
            )
        },
    );
    format!(
        "Mode {{ id: {}, bits: {}, raw_count: {}, decoder: {}, dng_version: {:?}, validation: ModeValidation::{:?}, compression: {compression}, frame_size: {:?}, unpacker: Unpacker::{:?}, processing: {processing} }}",
        text(&mode.id),
        mode.bits,
        mode.raw_count,
        text(&mode.decoder),
        mode.dng_version,
        mode.validation,
        mode.frame_size,
        mode.unpacker,
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
        let dng = dng_value(camera.dng.as_ref());
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
            "Camera {{ make: {}, model: {}, sensor_size: {:?}, cfa_size: {:?}, channels: {}, crop: Crop::{:?}, dng: {dng}, calibration: {calibration}, modes: Cow::Borrowed(&MODES_{index}) }}",
            text(&camera.make),
            text(&camera.model),
            camera.sensor_size,
            camera.cfa_size,
            camera.channels,
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
    let libraw_upstream = manifest.join("vendor/libraw-0.22.2");
    let rt_upstream = manifest.join("vendor/librtprocess-9a858270");
    let catalog = profiles::Catalog::parse(include_str!("data/cameras.json"))
        .expect("invalid RAW camera catalog");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("output dir"));
    let libraw = out.join("libraw-0.22.2-arw6");
    if libraw.exists() {
        fs::remove_dir_all(&libraw).expect("remove staged LibRaw");
    }
    copy_tree(&libraw_upstream, &libraw);
    apply_patch(
        &libraw,
        include_str!("patches/libraw-arw6.patch"),
        &[
            "internal/libraw_cameraids.h",
            "internal/libraw_internal_funcs.h",
            "src/metadata/identify.cpp",
            "src/metadata/normalize_model.cpp",
            "src/metadata/sony.cpp",
            "src/metadata/tiff.cpp",
            "src/tables/cameralist.cpp",
            "src/tables/colordata.cpp",
            "src/utils/decoder_info.cpp",
            "src/utils/open.cpp",
        ],
    );
    fs::write(
        libraw.join("src/decoders/sony_arw6.cpp"),
        include_bytes!("patches/sony_arw6.cpp"),
    )
    .expect("stage upstream Sony ARW6 decoder");
    let mut native = String::from(
        "// Generated from data/cameras.json; do not edit.\nstatic const struct { const char *make, *model; bool calibrated; double xyz_to_camera[9]; } lf_cameras[] = {\n",
    );
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
    }
    native.push_str("};\n\n#include \"native_limits.h\"\n");
    // The limits and status codes both native adapters share; the camera table above is the
    // LibRaw adapter's alone.
    let mut shared = format!(
        "// Generated from src/limits.rs and src/native_status.rs; do not edit.\n#pragma once\n\n#define LF_MAX_SOURCE_BYTES {}ull\n#define LF_MAX_PIXELS {}ull\n#define LF_MAX_SIDE {}u\n#define LF_MAX_RGB_BYTES {}ull\n",
        limits::MAX_SOURCE_BYTES,
        limits::MAX_PIXELS,
        limits::MAX_SIDE,
        limits::MAX_RGB_BYTES
    );
    shared.push_str("\nenum LfStatus {\n");
    for status in native_status::NativeStatus::ALL {
        shared.push_str(&format!(
            "  LF_STATUS_{} = {},\n",
            screaming_snake(&format!("{status:?}")),
            status as i32
        ));
    }
    shared.push_str("};\n");
    fs::write(out.join("native_limits.h"), shared).expect("write native limits");
    fs::write(out.join("camera_allowlist.h"), native).expect("write native camera table");
    fs::write(out.join("rawspeed_decoders.h"), unpacker::native_header())
        .expect("write replaceable decoder table");
    fs::write(out.join("camera_catalog.rs"), static_catalog(&catalog))
        .expect("write static camera catalog");
    let rt = out.join("librtprocess-9a858270");
    if rt.exists() {
        fs::remove_dir_all(&rt).expect("remove previous staged native sources");
    }
    copy_tree(&rt_upstream, &rt);
    apply_patch(
        &rt,
        include_str!("patches/librtprocess-local.patch"),
        &[
            "src/demosaic/markesteijn.cc",
            "src/demosaic/rcd.cc",
            "src/include/librtprocess.h",
            "src/include/mytime.h",
        ],
    );
    // The sensor-site input stacks on the local patch: its context is the locally patched source.
    apply_patch(
        &rt,
        include_str!("patches/librtprocess-mosaic.patch"),
        &[
            "src/demosaic/border.cc",
            "src/demosaic/markesteijn.cc",
            "src/demosaic/rcd.cc",
            "src/include/librtprocess.h",
        ],
    );
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
    add_cpp_tree(&mut build, &libraw.join("src"), "cpp", &[]);
    for source in ["rcd.cc", "markesteijn.cc", "border.cc"] {
        build.file(rt.join("src/demosaic").join(source));
    }
    build.compile("luxforge_raw_native");
    rawspeed(&manifest, &out);
    println!("cargo:rerun-if-changed=data/cameras.json");
    println!("cargo:rerun-if-changed=src/profiles.rs");
    println!("cargo:rerun-if-changed=src/mat3.rs");
    println!("cargo:rerun-if-changed=src/opcodes.rs");
    println!("cargo:rerun-if-changed=src/limits.rs");
    println!("cargo:rerun-if-changed=src/native_status.rs");
    println!("cargo:rerun-if-changed=src/unpacker.rs");
    println!("cargo:rerun-if-changed=native/adapter.cpp");
    println!("cargo:rerun-if-changed=vendor/libraw-0.22.2");
    println!("cargo:rerun-if-changed=vendor/librtprocess-9a858270");
    println!("cargo:rerun-if-changed=patches/librtprocess-local.patch");
    println!("cargo:rerun-if-changed=patches/librtprocess-mosaic.patch");
    println!("cargo:rerun-if-changed=patches/libraw-arw6.patch");
    println!("cargo:rerun-if-changed=patches/sony_arw6.cpp");
    println!("cargo:rerun-if-changed=native/rawspeed_adapter.cpp");
    println!("cargo:rerun-if-changed=native/rawspeed");
    println!("cargo:rerun-if-changed={RAWSPEED}");
    println!("cargo:rerun-if-changed={PUGIXML}");
}

/// The pinned RawSpeed and pugixml trees, as `vendor/` directory names.
const RAWSPEED: &str = "vendor/rawspeed-c835b05a";
const PUGIXML: &str = "vendor/pugixml-1.16";

/// The preprocessor settings every translation unit that includes a RawSpeed or pugixml header
/// shares, so the vendored library and the adapter agree on every class layout and inline body.
/// RawSpeed's own configuration is the hand-written `native/rawspeed/rawspeedconfig.h`.
fn rawspeed_settings(build: &mut cc::Build, manifest: &Path, vendored_include: bool) {
    let rawspeed = manifest.join(RAWSPEED);
    let includes = [
        manifest.join("native/rawspeed"),
        rawspeed.join("src/librawspeed"),
        rawspeed.join("src/external"),
        manifest.join(PUGIXML).join("src"),
    ];
    build
        .cpp(true)
        .std("c++20")
        // Upstream's Release build type: no assertions.
        .define("NDEBUG", None)
        // RawSpeed reads cameras.xml with pugixml's DOM only.
        .define("PUGIXML_NO_XPATH", None);
    let msvc = build.get_compiler().is_like_msvc();
    if !msvc {
        // Upstream's CMake visibility presets.
        build
            .flag("-fvisibility=hidden")
            .flag("-fvisibility-inlines-hidden");
    }
    for include in includes {
        if vendored_include && !msvc {
            // The adapter keeps its own warnings; the vendored headers it includes do not add any.
            build
                .flag("-isystem")
                .flag(include.to_str().expect("UTF-8 include path"));
        } else {
            build.include(include);
        }
    }
}

/// Build RawSpeed with its pugixml, and the crate's RawSpeed adapter, as two static libraries.
/// RawSpeed is C++20, generic-CPU (no `-march`), serial (no OpenMP) and has no zlib or libjpeg;
/// no source file is patched. The adapter links first so its references resolve into RawSpeed.
fn rawspeed(manifest: &Path, out: &Path) {
    let xml = fs::read(manifest.join(RAWSPEED).join("data/cameras.xml")).expect("read cameras.xml");
    assert!(!xml.is_empty(), "bundled cameras.xml is empty");
    let mut embedded = String::with_capacity(xml.len() * 4);
    for line in xml.chunks(32) {
        for byte in line {
            embedded.push_str(&byte.to_string());
            embedded.push(',');
        }
        embedded.push('\n');
    }
    fs::write(out.join("rawspeed_cameras_xml.inc"), embedded).expect("write embedded cameras.xml");

    let mut adapter = cc::Build::new();
    rawspeed_settings(&mut adapter, manifest, true);
    adapter
        .warnings(true)
        .include(out)
        .file(manifest.join("native/rawspeed_adapter.cpp"));
    adapter.compile("luxforge_rawspeed_adapter");

    let mut library = cc::Build::new();
    rawspeed_settings(&mut library, manifest, false);
    // Upstream builds with its own warning set; its warnings are not Luxforge's to fix here.
    library.warnings(false);
    // `common/Common.cpp` defines only `rawspeed::writeLog`, which prints to standard output; the
    // adapter defines its own, which prints to standard error.
    add_cpp_tree(
        &mut library,
        &manifest.join(RAWSPEED).join("src/librawspeed"),
        "cpp",
        &["common/Common.cpp"],
    );
    library.file(manifest.join(PUGIXML).join("src/pugixml.cpp"));
    library.compile("luxforge_rawspeed");
}
