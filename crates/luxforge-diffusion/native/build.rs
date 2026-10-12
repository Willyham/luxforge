//! Builds stable-diffusion.cpp and ggml from the pinned trees in `vendor/` with the `cc` crate:
//! no CMake, no Python, no download. THIRD_PARTY.md records the pins, the selection and the one
//! local change. On macOS the ggml Metal backend is compiled in and its kernels are embedded as
//! source, exactly as upstream's `GGML_METAL_EMBED_LIBRARY` does; on Linux the CPU backend alone.
//! Windows builds nothing: the crate's runtime then offers no backend (THIRD_PARTY.md says why).
//!
//! This file is a module of two build scripts, `luxforge-diffusion`'s own `build.rs` and the object
//! removal probe's (`probes/object-removal/diffusion/build.rs`), each calling [`build`] with this
//! crate's directory, so the probe builds these pinned sources rather than a copy of them.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

const SD: &str = "vendor/stable-diffusion.cpp-1b0ba108";
const GGML: &str = "vendor/ggml-d25b121c";
/// The pinned upstream commits, compiled into the library's version strings.
const SD_COMMIT: &str = "1b0ba10";
const SD_VERSION: &str = "master-956-1b0ba10";
const GGML_VERSION: &str = "0.25.3";
const GGML_COMMIT: &str = "d25b121c";

/// The Metal kernel sources, in upstream's order (`ggml-metal/CMakeLists.txt`); each becomes one
/// embedded source library with `ggml_metallib_<kind>_{start,end}` symbols.
const METAL_KERNELS: &[&str] = &[
    "fa",
    "mul_mv",
    "mul_mm",
    "quantize",
    "softmax",
    "norm",
    "unary",
    "binbcast",
    "reduce",
    "tri",
    "ssm",
    "wkv",
    "gated_delta_net",
    "solve_tri",
    "rope",
    "conv",
    "upscale",
    "argsort",
    "pool",
    "misc",
];

/// The one local change, in three places: a hook around `get_learned_condition`, so the harness
/// can record a text conditioning and later supply it without a text encoder, and (for a context
/// opened without a text encoder) the encoder's absent weights are not required. Each anchor must
/// match exactly once.
const HOOK_INCLUDE_ANCHOR: &str = "#include \"diffusion_engine.h\"\n";
const HOOK_INCLUDE: &str =
    "#include \"diffusion_engine.h\"\n#include \"luxforge_condition_hook.h\"\n";
const HOOK_ANCHOR: &str =
    "SDCondition StableDiffusionGGML::get_learned_condition(const ConditionerParams& params) {
    return conditioning_cache_->get(*cond_stage_model, n_threads, params);
}
";
const HOOK_REPLACEMENT: &str =
    "SDCondition StableDiffusionGGML::get_learned_condition(const ConditionerParams& params) {
    SDCondition luxforge_supplied;
    if (luxforge_condition_supplied(params.text, luxforge_supplied)) {
        return luxforge_supplied;
    }
    SDCondition condition = conditioning_cache_->get(*cond_stage_model, n_threads, params);
    luxforge_condition_observed(params.text, condition);
    return condition;
}
";

const IGNORE_ANCHOR: &str = "    model_manager->set_common_ignore_tensors(ignore_tensors);\n";
const IGNORE_REPLACEMENT: &str = "    if (luxforge_text_encoder_absent()) {
        ignore_tensors.insert(\"text_encoders.\");
    }
    model_manager->set_common_ignore_tensors(ignore_tensors);\n";

/// Whether a target compiles the runtime: macOS (Metal) and Linux (CPU). The crate's
/// `cfg(luxforge_sdcpp)` is set when it does.
pub fn builds_runtime(target_os: &str) -> bool {
    matches!(target_os, "macos" | "linux")
}

/// Compile the vendored runtime under `root`, `luxforge-diffusion`'s directory, into static
/// libraries in `OUT_DIR` and link them; set `cfg(luxforge_sdcpp)` when this target builds it.
pub fn build(root: &Path) {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    println!("cargo:rustc-check-cfg=cfg(luxforge_sdcpp)");
    for watched in ["native", "vendor"] {
        println!("cargo:rerun-if-changed={}", root.join(watched).display());
    }
    if !builds_runtime(&target_os) {
        return;
    }
    println!("cargo:rustc-cfg=luxforge_sdcpp");
    let metal = target_os == "macos";
    let sd = root.join(SD);
    let ggml = root.join(GGML);

    let generated = out.join("generated");
    fs::create_dir_all(generated.join("oniguruma")).unwrap();
    fs::write(
        generated.join("ggml-version.h"),
        format!(
            "#pragma once\n\n#define GGML_VERSION \"{GGML_VERSION}\"\n#define GGML_COMMIT  \"{GGML_COMMIT}\"\n"
        ),
    )
    .unwrap();
    fs::copy(
        root.join("native/oniguruma-config.h"),
        generated.join("oniguruma/config.h"),
    )
    .unwrap();

    // The staged copy of the one patched file.
    let engine = sd.join("src/pipeline/diffusion_engine.cpp");
    let staged = generated.join("diffusion_engine.cpp");
    let text = fs::read_to_string(&engine).unwrap();
    let text = replace_once(&text, HOOK_INCLUDE_ANCHOR, HOOK_INCLUDE);
    let text = replace_once(&text, HOOK_ANCHOR, HOOK_REPLACEMENT);
    let text = replace_once(&text, IGNORE_ANCHOR, IGNORE_REPLACEMENT);
    fs::write(&staged, text).unwrap();

    let common_defines: &[(&str, Option<&str>)] = &[
        ("NDEBUG", None),
        ("GGML_MAX_NAME", Some("160")),
        ("GGML_SCHED_MAX_COPIES", Some("4")),
        ("GGML_USE_CPU", None),
        ("_XOPEN_SOURCE", Some("600")),
    ];
    let mut defines: Vec<(&str, Option<&str>)> = common_defines.to_vec();
    if metal {
        defines.push(("GGML_USE_METAL", None));
        defines.push(("GGML_METAL_EMBED_LIBRARY", None));
        defines.push(("_DARWIN_C_SOURCE", None));
        defines.push(("GGML_USE_ACCELERATE", None));
        defines.push(("ACCELERATE_NEW_LAPACK", None));
        defines.push(("ACCELERATE_LAPACK_ILP64", None));
    }
    if target_os == "linux" {
        defines.push(("_GNU_SOURCE", None));
    }
    // Apple silicon's baseline (dotprod and fp16 arithmetic, no i8mm), so the library runs on
    // every M-series Mac rather than only the one that built it.
    let arch_flags: &[&str] = match (target_os.as_str(), target_arch.as_str()) {
        ("macos", "aarch64") => &["-mcpu=apple-m1"],
        _ => &[],
    };
    let ggml_includes = [
        ggml.join("include"),
        ggml.join("src"),
        ggml.join("src/ggml-cpu"),
        generated.clone(),
    ];

    let configure = |build: &mut cc::Build, cpp: bool| {
        build
            .opt_level(3)
            .debug(false)
            .warnings(false)
            .extra_warnings(false);
        for (name, value) in &defines {
            build.define(name, *value);
        }
        for flag in arch_flags {
            build.flag(flag);
        }
        if cpp {
            build.cpp(true).std("c++17");
        } else {
            build.std("c11");
        }
    };

    // Every object, archived once at the end.
    let mut objects: Vec<PathBuf> = Vec::new();

    // ggml: C sources.
    let mut c = cc::Build::new();
    configure(&mut c, false);
    c.includes(&ggml_includes);
    for file in [
        "src/ggml.c",
        "src/ggml-alloc.c",
        "src/ggml-quants.c",
        "src/ggml-cpu/ggml-cpu.c",
        "src/ggml-cpu/quants.c",
    ] {
        c.file(ggml.join(file));
    }
    // ggml's per-architecture CPU kernels; THIRD_PARTY.md lists the two vendored.
    let arch = match target_arch.as_str() {
        "aarch64" => "arm",
        "x86_64" => "x86",
        other => panic!(
            "luxforge-diffusion vendors ggml's CPU kernels for aarch64 and x86_64, not {other}"
        ),
    };
    c.file(ggml.join(format!("src/ggml-cpu/arch/{arch}/quants.c")));
    objects.extend(c.compile_intermediates());

    // ggml: C++ sources.
    let mut cpp = cc::Build::new();
    configure(&mut cpp, true);
    cpp.includes(&ggml_includes);
    for file in [
        "src/ggml.cpp",
        "src/ggml-backend.cpp",
        "src/ggml-backend-meta.cpp",
        "src/ggml-backend-reg.cpp",
        "src/ggml-backend-dl.cpp",
        "src/ggml-opt.cpp",
        "src/ggml-threading.cpp",
        "src/gguf.cpp",
        "src/ggml-cpu/ggml-cpu.cpp",
        "src/ggml-cpu/repack.cpp",
        "src/ggml-cpu/iqp.cpp",
        "src/ggml-cpu/hbm.cpp",
        "src/ggml-cpu/traits.cpp",
        "src/ggml-cpu/amx/amx.cpp",
        "src/ggml-cpu/amx/mmq.cpp",
        "src/ggml-cpu/binary-ops.cpp",
        "src/ggml-cpu/unary-ops.cpp",
        "src/ggml-cpu/vec.cpp",
        "src/ggml-cpu/ops.cpp",
    ] {
        cpp.file(ggml.join(file));
    }
    cpp.file(ggml.join(format!("src/ggml-cpu/arch/{arch}/repack.cpp")));
    if metal {
        for file in [
            "src/ggml-metal/ggml-metal.cpp",
            "src/ggml-metal/ggml-metal-device.cpp",
            "src/ggml-metal/ggml-metal-common.cpp",
            "src/ggml-metal/ggml-metal-fusion.cpp",
            "src/ggml-metal/ggml-metal-ops.cpp",
            "src/ggml-metal/ggml-metal-tuning.cpp",
        ] {
            cpp.file(ggml.join(file));
        }
    }
    objects.extend(cpp.compile_intermediates());

    if metal {
        // Objective-C, without ARC as upstream builds it.
        let mut objc = cc::Build::new();
        configure(&mut objc, false);
        objc.includes(&ggml_includes);
        objc.file(ggml.join("src/ggml-metal/ggml-metal-device.m"));
        objc.file(ggml.join("src/ggml-metal/ggml-metal-context.m"));
        objects.extend(objc.compile_intermediates());

        let mut asm = cc::Build::new();
        asm.flag("-Wno-unused-command-line-argument");
        for kind in METAL_KERNELS {
            asm.file(embed_metal_kernel(&ggml, &generated, kind));
        }
        objects.extend(asm.compile_intermediates());
    }

    // Third-party C under stable-diffusion.cpp/thirdparty.
    let third = sd.join("thirdparty");
    let mut tc = cc::Build::new();
    configure(&mut tc, false);
    tc.define("ONIG_STATIC", None)
        .define("UTF8PROC_STATIC", None)
        .include(&third)
        .include(third.join("oniguruma"))
        .include(generated.join("oniguruma"))
        .include(third.join("utf8proc"));
    tc.file(third.join("zip.c"));
    for file in [
        "ascii.c",
        "regcomp.c",
        "regenc.c",
        "regerror.c",
        "regexec.c",
        "regparse.c",
        "st.c",
        "unicode.c",
        "unicode_fold1_key.c",
        "unicode_fold2_key.c",
        "unicode_fold3_key.c",
        "unicode_unfold_key.c",
        "utf8.c",
    ] {
        tc.file(third.join("oniguruma").join(file));
    }
    tc.file(third.join("utf8proc/utf8proc.c"));
    objects.extend(tc.compile_intermediates());

    // stable-diffusion.cpp itself, upstream's source glob, with the staged engine and the local
    // vocabulary and shim sources in place of the generated vocabulary tables.
    let src = sd.join("src");
    let mut sources = Vec::new();
    for dir in [
        "",
        "conditioning",
        "core",
        "extensions",
        "pipeline",
        "runtime",
        "model_io",
        "tokenizers",
    ] {
        sources.extend(cpp_files(&src.join(dir)));
    }
    for entry in fs::read_dir(src.join("model")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources.extend(cpp_files(&path));
        }
    }
    let excluded = [engine.clone(), src.join("tokenizers/vocab/vocab.cpp")];
    for path in &excluded {
        assert!(path.is_file(), "excluded source missing: {path:?}");
    }
    sources.retain(|path| !excluded.contains(path));
    sources.sort();
    let mut sdb = cc::Build::new();
    configure(&mut sdb, true);
    sdb.define("ONIG_STATIC", None)
        .define("UTF8PROC_STATIC", None)
        .include(&sd)
        .include(&src)
        .include(sd.join("include"))
        .include(src.join("core"))
        .include(src.join("pipeline"))
        .include(&third)
        .include(third.join("oniguruma"))
        .include(third.join("utf8proc"))
        .include(root.join("native"))
        .includes(&ggml_includes);
    for source in &sources {
        sdb.file(source);
    }
    sdb.file(&staged);
    sdb.file(root.join("native/vocab.cpp"));
    sdb.file(root.join("native/shim.cpp"));
    // version.cpp reads these, as CMake sets them from git.
    sdb.define("SDCPP_BUILD_COMMIT", Some(SD_COMMIT))
        .define("SDCPP_BUILD_VERSION", Some(SD_VERSION));
    objects.extend(sdb.compile_intermediates());

    // One archive of every object: GNU ld resolves an archive's members against each other
    // whatever their order, but not one archive against an earlier one, and stable-diffusion.cpp,
    // its third-party code and ggml's C, C++ and Metal parts call each other in both directions.
    cc::Build::new().objects(&objects).compile("luxforge_sdcpp");

    if metal {
        for framework in ["Foundation", "Metal", "MetalKit", "Accelerate"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
    // The C++ runtime the archive needs.
    if target_os == "linux" {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    } else {
        println!("cargo:rustc-link-lib=dylib=c++");
    }
}

fn replace_once(text: &str, anchor: &str, replacement: &str) -> String {
    let count = text.matches(anchor).count();
    assert_eq!(
        count, 1,
        "the local patch's anchor matched {count} times in the vendored source; an upstream update \
         must be reviewed: {anchor:?}"
    );
    text.replacen(anchor, replacement, 1)
}

fn cpp_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {dir:?}: {error}"))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "cpp"))
        .collect();
    files.sort();
    files
}

/// Upstream's embed step for one kernel source: the shared kernel headers it includes, then the
/// source, without the internal includes and `#pragma once`, with `ggml-common.h` and
/// `ggml-metal-impl.h` inlined; then an assembly file that places the text between two symbols.
fn embed_metal_kernel(ggml: &Path, generated: &Path, kind: &str) -> PathBuf {
    let metal = ggml.join("src/ggml-metal");
    let source = fs::read_to_string(metal.join(format!("kernels/{kind}.metal"))).unwrap();
    let mut parts = vec![fs::read_to_string(metal.join("kernels/common.h")).unwrap()];
    if source.contains("#include \"dequantize.h\"") {
        parts.push(fs::read_to_string(metal.join("kernels/dequantize.h")).unwrap());
    }
    if source.contains("#include \"quantize.h\"") {
        parts.push(fs::read_to_string(metal.join("kernels/quantize.h")).unwrap());
    }
    parts.push(source);
    let common = fs::read_to_string(ggml.join("src/ggml-common.h")).unwrap();
    let impl_header = fs::read_to_string(metal.join("ggml-metal-impl.h")).unwrap();
    let mut text = String::new();
    for line in parts.concat().lines() {
        if line.contains("#include \"common.h\"")
            || line.contains("#include \"dequantize.h\"")
            || line.contains("#include \"quantize.h\"")
            || line.contains("#pragma once")
        {
            continue;
        }
        if line.contains("__embed_ggml-common.h__") {
            text.push_str(&common);
            if !common.ends_with('\n') {
                text.push('\n');
            }
            continue;
        }
        if line.contains("#include \"ggml-metal-impl.h\"") {
            text.push_str(&impl_header);
            if !impl_header.ends_with('\n') {
                text.push('\n');
            }
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    let embedded = generated.join(format!("ggml-metal-embed-{kind}.metal"));
    fs::write(&embedded, text).unwrap();
    let asm = generated.join(format!("ggml-metal-embed-{kind}.s"));
    fs::write(
        &asm,
        format!(
            ".section __DATA,__ggml_metallib\n.globl _ggml_metallib_{kind}_start\n_ggml_metallib_{kind}_start:\n.incbin \"{}\"\n.globl _ggml_metallib_{kind}_end\n_ggml_metallib_{kind}_end:\n",
            embedded.display()
        ),
    )
    .unwrap();
    asm
}
