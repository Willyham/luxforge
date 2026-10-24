use crate::*;
use std::io::Write;

fn native_raw_notices(root: &Path, out: &Path) -> Result {
    let source = root.join("crates/luxforge-raw");
    let notices = [
        ("THIRD_PARTY.md", "luxforge-raw/THIRD_PARTY.md"),
        (
            "vendor/libraw-0.22.2/LICENSE.LGPL",
            "libraw-0.22.2/LICENSE.LGPL",
        ),
        (
            "vendor/libraw-0.22.2/LICENSE.CDDL",
            "libraw-0.22.2/LICENSE.CDDL",
        ),
        ("vendor/libraw-0.22.2/README.md", "libraw-0.22.2/README.md"),
        (
            "vendor/librtprocess-9a858270/LICENSE.txt",
            "librtprocess-9a858270/LICENSE.txt",
        ),
        (
            "vendor/librtprocess-9a858270/README.md",
            "librtprocess-9a858270/README.md",
        ),
        (
            "vendor/rawspeed-c835b05a/LICENSE",
            "rawspeed-c835b05a/LICENSE",
        ),
        (
            "vendor/rawspeed-c835b05a/credits.txt",
            "rawspeed-c835b05a/credits.txt",
        ),
        (
            "vendor/rawspeed-c835b05a/README.rst",
            "rawspeed-c835b05a/README.rst",
        ),
        ("vendor/pugixml-1.16/LICENSE.md", "pugixml-1.16/LICENSE.md"),
    ];
    for (from, to) in notices {
        let input = source.join(from);
        ensure(
            input.is_file(),
            format!("Missing bundled RAW notice: {}", input.display()),
        )?;
        let destination = out.join("native").join(to);
        fs::create_dir_all(destination.parent().ok_or("Notice parent")?)?;
        fs::copy(input, destination)?;
    }
    Ok(())
}

/// Copies the JPEG codec's native provenance (`luxforge-jpeg`'s, for import and export alike)
/// beside the RAW notices. Its licence file is the `mozjpeg-sys` package's own, which the package
/// loop in [`inventory`] already collects.
fn native_jpeg_notices(root: &Path, out: &Path) -> Result {
    let input = root.join("crates/luxforge-jpeg/THIRD_PARTY.md");
    ensure(
        input.is_file(),
        format!("Missing bundled JPEG notice: {}", input.display()),
    )?;
    let destination = out.join("native/luxforge-jpeg/THIRD_PARTY.md");
    fs::create_dir_all(destination.parent().ok_or("Notice parent")?)?;
    fs::copy(input, destination)?;
    Ok(())
}

/// Copies the WebP codec's native provenance (`luxforge-webp`'s) and libwebp's licence and patent
/// grant beside the other native notices. The `libwebp-sys` package keeps them under `vendor/`,
/// which the package loop in [`inventory`] does not read, so the crate carries copies.
fn native_webp_notices(root: &Path, out: &Path) -> Result {
    let source = root.join("crates/luxforge-webp");
    let notices = [
        ("THIRD_PARTY.md", "luxforge-webp/THIRD_PARTY.md"),
        ("notices/libwebp-1.6.0/COPYING", "libwebp-1.6.0/COPYING"),
        ("notices/libwebp-1.6.0/PATENTS", "libwebp-1.6.0/PATENTS"),
    ];
    for (from, to) in notices {
        let input = source.join(from);
        ensure(
            input.is_file(),
            format!("Missing WebP codec notice: {}", input.display()),
        )?;
        let destination = out.join("native").join(to);
        fs::create_dir_all(destination.parent().ok_or("Notice parent")?)?;
        fs::copy(input, destination)?;
    }
    Ok(())
}

/// Copies the linked ONNX Runtime's provenance (`luxforge-onnx`'s), its MIT licence and the notices
/// of the libraries it builds in beside the other native notices. The prebuilt archive carries no
/// notice of its own; these are the upstream tag's.
fn native_onnxruntime_notices(root: &Path, out: &Path) -> Result {
    let source = root.join("crates/luxforge-onnx");
    let notices = [
        ("THIRD_PARTY.md", "luxforge-onnx/THIRD_PARTY.md"),
        (
            "notices/onnxruntime-1.28.0/LICENSE",
            "onnxruntime-1.28.0/LICENSE",
        ),
        (
            "notices/onnxruntime-1.28.0/ThirdPartyNotices.txt",
            "onnxruntime-1.28.0/ThirdPartyNotices.txt",
        ),
    ];
    for (from, to) in notices {
        let input = source.join(from);
        ensure(
            input.is_file(),
            format!("Missing ONNX Runtime notice: {}", input.display()),
        )?;
        let destination = out.join("native").join(to);
        fs::create_dir_all(destination.parent().ok_or("Notice parent")?)?;
        fs::copy(input, destination)?;
    }
    Ok(())
}

/// Copies the embedded Sparkle framework's provenance (`luxforge-sparkle`'s) and its licence, with
/// the external licences it lists, beside the other native notices. Only the macOS bundle holds it.
fn native_sparkle_notices(root: &Path, out: &Path) -> Result {
    let source = root.join("crates/luxforge-sparkle");
    let notices = [
        ("THIRD_PARTY.md", "luxforge-sparkle/THIRD_PARTY.md"),
        ("notices/sparkle-2.10.0/LICENSE", "sparkle-2.10.0/LICENSE"),
    ];
    for (from, to) in notices {
        let input = source.join(from);
        ensure(
            input.is_file(),
            format!("Missing Sparkle notice: {}", input.display()),
        )?;
        let destination = out.join("native").join(to);
        fs::create_dir_all(destination.parent().ok_or("Notice parent")?)?;
        fs::copy(input, destination)?;
    }
    Ok(())
}

/// Copies the generative runtime's provenance (`luxforge-diffusion`'s) and the licences of
/// stable-diffusion.cpp, ggml and the third-party sources stable-diffusion.cpp bundles, from the
/// vendored trees the build compiles. The packaged binary links them only on macOS and Linux.
fn native_diffusion_notices(root: &Path, out: &Path) -> Result {
    let source = root.join("crates/luxforge-diffusion");
    let sd = "vendor/stable-diffusion.cpp-1b0ba108";
    let notices = [
        (
            "THIRD_PARTY.md".to_owned(),
            "luxforge-diffusion/THIRD_PARTY.md",
        ),
        (
            format!("{sd}/LICENSE"),
            "stable-diffusion.cpp-1b0ba108/LICENSE",
        ),
        (
            format!("{sd}/thirdparty/LICENSE.darts_clone.txt"),
            "stable-diffusion.cpp-1b0ba108/thirdparty/LICENSE.darts_clone.txt",
        ),
        (
            format!("{sd}/thirdparty/oniguruma/COPYING"),
            "stable-diffusion.cpp-1b0ba108/thirdparty/oniguruma/COPYING",
        ),
        (
            format!("{sd}/thirdparty/utf8proc/LICENSE.md"),
            "stable-diffusion.cpp-1b0ba108/thirdparty/utf8proc/LICENSE.md",
        ),
        (
            "vendor/ggml-d25b121c/LICENSE".to_owned(),
            "ggml-d25b121c/LICENSE",
        ),
    ];
    for (from, to) in notices {
        let input = source.join(&from);
        ensure(
            input.is_file(),
            format!("Missing generative runtime notice: {}", input.display()),
        )?;
        let destination = out.join("native").join(to);
        fs::create_dir_all(destination.parent().ok_or("Notice parent")?)?;
        fs::copy(input, destination)?;
    }
    Ok(())
}

/// Copies the bundled UI font's provenance and its SIL Open Font License beside the other notices.
fn bundled_font_notices(root: &Path, out: &Path) -> Result {
    let source = root.join("crates/luxforge-ui");
    let notices = [
        ("THIRD_PARTY.md", "luxforge-ui/THIRD_PARTY.md"),
        (
            "assets/fonts/inter-4.1/LICENSE.txt",
            "inter-4.1/LICENSE.txt",
        ),
    ];
    for (from, to) in notices {
        let input = source.join(from);
        ensure(
            input.is_file(),
            format!("Missing bundled font notice: {}", input.display()),
        )?;
        let destination = out.join("fonts").join(to);
        fs::create_dir_all(destination.parent().ok_or("Notice parent")?)?;
        fs::copy(input, destination)?;
    }
    Ok(())
}

/// Copies the bundled Omarchy themes' notice and Omarchy's MIT licence beside the other notices;
/// the palettes themselves are compiled into the core.
fn bundled_theme_notices(root: &Path, out: &Path) -> Result {
    let source = root.join("crates/luxforge-core/src/theme/omarchy/bundled");
    for name in ["NOTICE.md", "LICENSE"] {
        let input = source.join(name);
        ensure(
            input.is_file(),
            format!("Missing bundled theme notice: {}", input.display()),
        )?;
        let destination = out.join("themes/omarchy").join(name);
        fs::create_dir_all(destination.parent().ok_or("Notice parent")?)?;
        fs::copy(input, destination)?;
    }
    Ok(())
}

/// Copies the bundled place names' provenance, attribution and indication of changes (CC BY 4.0
/// asks for all three) beside the other notices. The licence is named by its address, so there is no
/// licence file to copy.
fn bundled_data_notices(root: &Path, out: &Path) -> Result {
    let input = root.join("crates/luxforge-core/THIRD_PARTY.md");
    ensure(
        input.is_file(),
        format!("Missing bundled data notice: {}", input.display()),
    )?;
    let destination = out.join("data/luxforge-core/THIRD_PARTY.md");
    fs::create_dir_all(destination.parent().ok_or("Notice parent")?)?;
    fs::copy(input, destination)?;
    Ok(())
}

/// The bundled Lensfun resource's directory, relative to the repository root.
pub(crate) const LENS_DIRECTORY: &str = "crates/luxforge-core/data/lensfun";
pub(crate) const LENS_FILES: &[&str] = &[
    "index.json",
    "provenance.json",
    "LICENSE-CC-BY-SA-3.0.txt",
    "ATTRIBUTION.md",
];

pub(crate) fn lens_resources(root: &Path, destination: &Path) -> Result {
    fs::create_dir_all(destination)?;
    for name in LENS_FILES {
        let source = root.join(LENS_DIRECTORY).join(name);
        ensure(
            source.is_file(),
            format!("Missing bundled Lensfun resource: {}", source.display()),
        )?;
        fs::copy(source, destination.join(name))?;
    }
    Ok(())
}

pub fn inventory(root: &Path, out: &Path) -> Result {
    inventory_for(root, out, &host(root)?)
}

/// [`inventory`] for `target`'s dependency graph rather than the host's, as the source archive
/// collects the notices of each shipping target.
pub(crate) fn inventory_for(root: &Path, out: &Path, target: &str) -> Result {
    let target = target.to_owned();
    let data: Value = serde_json::from_str(&output(
        root,
        "cargo",
        &[
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            &target,
        ],
    )?)?;
    let active: std::collections::BTreeSet<_> = data["resolve"]["nodes"]
        .as_array()
        .ok_or("Missing resolve nodes")?
        .iter()
        .map(|n| n["id"].clone().to_string())
        .collect();
    let mut packages = Vec::new();
    fs::create_dir_all(out.join("licenses"))?;
    for p in data["packages"].as_array().ok_or("Missing packages")? {
        if !active.contains(&p["id"].to_string()) {
            continue;
        }
        packages.push(json!({"name":p["name"],"version":p["version"],"license":p["license"]}));
        let dir = Path::new(p["manifest_path"].as_str().ok_or("Missing manifest")?)
            .parent()
            .ok_or("Manifest parent")?;
        let dest = out.join("licenses").join(format!(
            "{}-{}",
            p["name"].as_str().unwrap(),
            p["version"].as_str().unwrap()
        ));
        for f in fs::read_dir(dir)? {
            let f = f?.path();
            let name = f.file_name().unwrap().to_string_lossy().to_lowercase();
            if f.is_file()
                && ["license", "copying", "notice"]
                    .iter()
                    .any(|p| name.starts_with(p))
            {
                fs::create_dir_all(&dest)?;
                fs::copy(&f, dest.join(f.file_name().unwrap()))?;
            }
        }
    }
    native_raw_notices(root, out)?;
    native_jpeg_notices(root, out)?;
    native_webp_notices(root, out)?;
    native_onnxruntime_notices(root, out)?;
    native_diffusion_notices(root, out)?;
    let macos = target == crate::release_build::TARGETS[0].0;
    if macos {
        native_sparkle_notices(root, out)?;
    }
    bundled_font_notices(root, out)?;
    bundled_theme_notices(root, out)?;
    bundled_data_notices(root, out)?;
    lens_resources(root, &out.join("data/lensfun"))?;
    let mut dependencies = json!({
        "target":target,
        "packages":packages,
        "native_sources":[
            {
                "name":"LibRaw",
                "version":"0.22.2",
                "revision":"b93f6e45c194f5df9b02a43b1af9a54b4f41f33f",
                "selected_license":"LGPL-2.1",
                "notices":"native/libraw-0.22.2",
                "build":"Bundled source; no USE_ZLIB, USE_JPEG, USE_RAWSPEED, USE_DNGSDK, USE_LCMS"
            },
            {
                "name":"librtprocess",
                "version":"0.11.0",
                "revision":"9a858270acb2096e2e403d932760ee688fcac425",
                "selected_license":"GPL-3.0-or-later",
                "notices":"native/librtprocess-9a858270",
                "build":"Bundled RCD, Markesteijn and border source; no OpenMP"
            },
            {
                "name":"RawSpeed",
                "version":"develop (no release)",
                "revision":"c835b05aecfacb7343f7c424abd620aa12116c3f",
                "selected_license":"LGPL-2.0-or-later (code; per-file exceptions in THIRD_PARTY.md) AND CC-BY-SA-3.0 (data/cameras.xml)",
                "notices":"native/rawspeed-c835b05a",
                "build":"Bundled library source and embedded cameras.xml; C++20, generic CPU; no OpenMP, zlib or libjpeg; not yet routed"
            },
            {
                "name":"pugixml",
                "version":"1.16",
                "revision":"c8033ce9d039e7f9d134877c363397b3cfe20816",
                "selected_license":"MIT",
                "notices":"native/pugixml-1.16",
                "build":"Bundled source built with RawSpeed; PUGIXML_NO_XPATH"
            },
            {
                "name":"libjpeg-turbo (mozjpeg-sys)",
                "version":"mozjpeg-sys 2.2.3",
                "revision":"93e9c78d0e9afb018a224e1105f06e48aec77766",
                "selected_license":"IJG AND BSD-3-Clause AND Zlib",
                "notices":"licenses/mozjpeg-sys-2.2.3",
                "provenance":"native/luxforge-jpeg/THIRD_PARTY.md",
                "build":"Crate-bundled source built with cc; with_simd (NEON on aarch64, portable C on x86_64 without NASM) and unwinding only"
            },
            {
                "name":"libwebp (libwebp-sys)",
                "version":"libwebp 1.6.0, libwebp-sys 0.14.4",
                "revision":"3a40281fd97537b693174acb7375d64d3b471a56",
                "selected_license":"BSD-3-Clause (with Google's patent grant); bindings MIT",
                "notices":"native/libwebp-1.6.0",
                "provenance":"native/luxforge-webp/THIRD_PARTY.md",
                "build":"Crate-vendored source built with cc, no default features (no threads); NEON on aarch64, SSE2 on x86_64; no bindgen, no system library"
            },
            {
                "name":"ONNX Runtime",
                "version":"1.28.0",
                "revision":"da9b5e364c465de65c49d91e696cd6485270757f",
                "selected_license":"MIT (bundled libraries listed in ThirdPartyNotices.txt)",
                "notices":"native/onnxruntime-1.28.0",
                "provenance":"native/luxforge-onnx/THIRD_PARTY.md",
                "build":"pyke's attested prebuilt static archive (pykeio/ort-artifacts 25d68de6) pinned by SHA-256; Core ML provider on macOS, DirectML on Windows, CPU on Linux"
            },
            {
                "name":"stable-diffusion.cpp",
                "version":"master-956-1b0ba10",
                "revision":"1b0ba10893f4e2e0656103011fc1c4645a02a734",
                "selected_license":"MIT (bundled third-party sources: nlohmann/json MIT, zip Unlicense, miniz MIT, darts-clone BSD-2-Clause, Oniguruma BSD-2-Clause, utf8proc MIT)",
                "notices":"native/stable-diffusion.cpp-1b0ba108",
                "provenance":"native/luxforge-diffusion/THIRD_PARTY.md",
                "build":"Bundled source subset built with cc and one staged patch; macOS and Linux only, not Windows"
            },
            {
                "name":"ggml",
                "version":"0.25.3",
                "revision":"d25b121ce1d6ae0c7e62ad080460d9fc1e46dd93",
                "selected_license":"MIT",
                "notices":"native/ggml-d25b121c",
                "provenance":"native/luxforge-diffusion/THIRD_PARTY.md",
                "build":"Bundled source built with cc; Metal and CPU backends on macOS, CPU on Linux"
            }
        ],
        "native_provenance":"native/luxforge-raw/THIRD_PARTY.md",
        "bundled_fonts":[
            {
                "name":"Inter",
                "version":"4.1",
                "revision":"e3a3d4c57d5ecc01453a575621882a384c1995a3",
                "files":["Inter-Regular.ttf","Inter-SemiBold.ttf"],
                "license":"OFL-1.1",
                "notices":"fonts/inter-4.1",
                "provenance":"fonts/luxforge-ui/THIRD_PARTY.md"
            }
        ],
        "bundled_data":[
            {
                "name":"GeoNames cities15000 (modified extract)",
                "downloaded":"2026-09-30",
                "source":"https://download.geonames.org/export/dump/cities15000.zip",
                "source_sha256":"44347468d5656101a999ba0cbb64b0cbdc7c2b48de83c8f6e129b50e697dd05d",
                "files":["places.tsv"],
                "license":"CC-BY-4.0",
                "license_url":"https://creativecommons.org/licenses/by/4.0/",
                "notices":"data/luxforge-core",
                "provenance":"data/luxforge-core/THIRD_PARTY.md"
            },{
            "name":"Lensfun distortion profile index",
            "version":"0.3.4",
            "revision":"101c745e847a5de4a1e569a94368ce2027198598",
            "license":"CC-BY-SA-3.0",
            "files":LENS_FILES,
            "notices":"data/lensfun",
            "provenance":"data/lensfun/provenance.json",
            "modifications":"Filtered version-1 XML converted to an offline rectilinear distortion index"
        },{
            "name":"Omarchy themes (Tokyo Night, Catppuccin, Catppuccin Latte, Gruvbox, Nord, Everforest)",
            "version":"quattro",
            "revision":"035ce29f03bdd97a09af80ef5f2d22d7a98930d6",
            "license":"MIT",
            "files":["colors.toml of each theme, compiled into the binary"],
            "notices":"themes/omarchy",
            "provenance":"themes/omarchy/NOTICE.md",
            "modifications":"None"
        }],
        "review_status":"Inventory only; manual license, native and asset reviews deferred"
    });
    if macos {
        dependencies["native_sources"]
            .as_array_mut()
            .ok_or("Native sources")?
            .push(json!({
                "name":"Sparkle",
                "version":crate::sparkle::VERSION,
                "revision":crate::sparkle::REVISION,
                "selected_license":"MIT (bundled bsdiff BSD-2-Clause, sais-lite MIT, ed25519 Zlib and others listed in LICENSE)",
                "notices":"native/sparkle-2.10.0",
                "provenance":"native/luxforge-sparkle/THIRD_PARTY.md",
                "build":"The official release archive's prebuilt Sparkle.framework, pinned by SHA-256, without XPC services; embedded in the macOS bundle only and loaded at run time"
            }));
    }
    write_json(&out.join("dependencies.json"), &dependencies)?;
    println!("Inventory: {} configured packages", packages.len());
    Ok(())
}
pub(crate) fn archive(directory: &Path, destination: &Path) -> Result {
    if destination.extension().is_some_and(|e| e == "zip") {
        let mut zip = zip::ZipWriter::new(fs::File::create(destination)?);
        for path in files(directory)? {
            let relative = path
                .strip_prefix(directory.parent().unwrap())?
                .to_str()
                .ok_or("Archive path encoding")?
                .replace('\\', "/");
            let mode = if path.file_name().is_some_and(|s| {
                s.to_str()
                    .is_some_and(|name| crate::release_build::EXECUTABLES.contains(&name))
            }) {
                0o755
            } else {
                0o644
            };
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(mode);
            zip.start_file(relative, options)?;
            std::io::copy(&mut fs::File::open(path)?, &mut zip)?;
        }
        zip.finish()?.sync_all()?;
    } else {
        let gz = flate2::write::GzEncoder::new(
            fs::File::create(destination)?,
            flate2::Compression::default(),
        );
        let mut tar = tar::Builder::new(gz);
        tar.append_dir_all("Luxforge", directory)?;
        tar.into_inner()?.finish()?.sync_all()?;
    }
    Ok(())
}
/// The source revision a packaged or tag build embeds for usage statistics' build identity
/// (`docs/design/usage-statistics.md#build-identity-and-activation`): the full revision of a clean
/// tree, and none for a dirty one, whose build is then ineligible for production reporting.
pub(crate) fn embedded_revision(revision: &str, dirty: bool) -> Option<&str> {
    (!dirty).then_some(revision)
}

/// Set the core's compile-time source revision for `revision`, or remove any inherited value so a
/// dirty tree's build embeds none.
pub(crate) fn embed_revision(command: &mut std::process::Command, revision: Option<&str>) {
    let variable = luxforge_core::usage::eligibility::SOURCE_REVISION_ENV;
    match revision {
        Some(revision) => command.env(variable, revision),
        None => command.env_remove(variable),
    };
}

pub fn package(root: &Path, out: &Path) -> Result {
    ensure(!out.exists(), "Package output must be new")?;
    let revision = output(root, "git", &["rev-parse", "HEAD"])?
        .trim()
        .to_owned();
    let dirty = !output(root, "git", &["status", "--porcelain"])?
        .trim()
        .is_empty();
    let embedded = embedded_revision(&revision, dirty);
    xtask_cli::cargo_with(root, "build", true, |command| {
        embed_revision(command, embedded)
    })?;
    fs::create_dir_all(out)?;
    let target = out.join("Luxforge");
    fs::create_dir(&target)?;
    let binary = binary(root)?;
    if cfg!(target_os = "macos") {
        let app = target.join("Luxforge.app/Contents");
        fs::create_dir_all(app.join("MacOS"))?;
        fs::copy(&binary, app.join("MacOS/luxforge"))?;
        lens_resources(root, &app.join("Resources/lensfun"))?;
        crate::icon::copy_icns(root, &app.join("Resources").join(crate::icon::ICNS))?;
        fs::write(
            app.join("Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>org.luxforge.app</string><key>CFBundleName</key><string>Luxforge</string><key>CFBundleExecutable</key><string>luxforge</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleIconFile</key><string>luxforge</string><key>CFBundleShortVersionString</key><string>{}</string><key>LSMinimumSystemVersion</key><string>14.0</string><key>NSHighResolutionCapable</key><true/></dict></plist>"#,
                env!("CARGO_PKG_VERSION")
            ),
        )?;
    } else {
        fs::copy(&binary, target.join(binary.file_name().unwrap()))?;
        lens_resources(root, &target.join("lensfun"))?;
    }
    write_json(
        &target.join("build.json"),
        &json!({"revision":revision,"working_tree_dirty":dirty,"release":luxforge_core::usage::BuildIdentity::new(env!("CARGO_PKG_VERSION"),embedded).release(),"target":host(root)?,"profile":"release","binary_sha256":hash(&binary)?,"lock_sha256":hash(&root.join("Cargo.lock"))?}),
    )?;
    fs::copy(root.join("LICENSE"), target.join("LICENSE"))?;
    inventory(root, &target.join("notices"))?;
    fs::write(
        target.join("README.txt"),
        "Unsigned development artifact. Manual license/native Windows/Linux reviews are deferred. See docs/engineering/platforms.md for runtime prerequisites.\n",
    )?;
    let archive_path = out.join(if cfg!(target_os = "linux") {
        "luxforge-development.tar.gz"
    } else {
        "luxforge-development.zip"
    });
    archive(&target, &archive_path)?;
    writeln!(
        fs::File::create(out.join("checksums.txt"))?,
        "{}  {}",
        hash(&archive_path)?,
        archive_path.file_name().unwrap().to_string_lossy()
    )?;
    println!("{}", archive_path.display());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_embeds_only_a_clean_revision() {
        let revision = "9d4138ca1f2e5b7a0c3d4e5f60718293a4b5c6d7";
        assert_eq!(embedded_revision(revision, false), Some(revision));
        assert_eq!(embedded_revision(revision, true), None);
        let variable = std::ffi::OsStr::new(luxforge_core::usage::eligibility::SOURCE_REVISION_ENV);
        let set = |revision| {
            let mut command = std::process::Command::new("cargo");
            embed_revision(&mut command, revision);
            command
                .get_envs()
                .find(|(key, _)| *key == variable)
                .map(|(_, value)| value.map(|value| value.to_owned()))
        };
        assert_eq!(set(Some(revision)), Some(Some(revision.into())));
        // A dirty tree's build removes any inherited value rather than leaving it.
        assert_eq!(set(None), Some(None));
        assert_eq!(
            luxforge_core::usage::BuildIdentity::new("0.0.0", embedded_revision(revision, false))
                .release()
                .as_deref(),
            Some("0.0.0+9d4138ca1f2e")
        );
    }
    #[test]
    fn copies_bundled_raw_native_notices() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        native_raw_notices(root, tmp.path()).unwrap();
        let libraw_license =
            fs::read_to_string(tmp.path().join("native/libraw-0.22.2/LICENSE.LGPL")).unwrap();
        assert!(libraw_license.contains("GNU LESSER GENERAL PUBLIC LICENSE"));
        let rtprocess_license =
            fs::read_to_string(tmp.path().join("native/librtprocess-9a858270/LICENSE.txt"))
                .unwrap();
        assert!(rtprocess_license.contains("GNU GENERAL PUBLIC LICENSE"));
        assert!(
            tmp.path()
                .join("native/luxforge-raw/THIRD_PARTY.md")
                .is_file()
        );
    }
    #[test]
    fn copies_the_bundled_jpeg_provenance() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        native_jpeg_notices(root, tmp.path()).unwrap();
        let provenance =
            fs::read_to_string(tmp.path().join("native/luxforge-jpeg/THIRD_PARTY.md")).unwrap();
        assert!(provenance.contains("based in part on the work of the Independent JPEG Group"));
    }
    #[test]
    fn copies_the_webp_codec_licence_and_provenance() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        native_webp_notices(root, tmp.path()).unwrap();
        let native = tmp.path().join("native");
        let licence = fs::read_to_string(native.join("libwebp-1.6.0/COPYING")).unwrap();
        assert!(licence.contains("Copyright (c) 2010, Google Inc."));
        let patents = fs::read_to_string(native.join("libwebp-1.6.0/PATENTS")).unwrap();
        assert!(patents.contains("Additional IP Rights Grant (Patents)"));
        let provenance = fs::read_to_string(native.join("luxforge-webp/THIRD_PARTY.md")).unwrap();
        assert!(provenance.contains("not a completed audit"));
    }
    #[test]
    fn copies_the_onnx_runtime_licence_and_notices() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        native_onnxruntime_notices(root, tmp.path()).unwrap();
        let native = tmp.path().join("native");
        let licence = fs::read_to_string(native.join("onnxruntime-1.28.0/LICENSE")).unwrap();
        assert!(licence.contains("MIT License"));
        assert!(licence.contains("Microsoft Corporation"));
        assert!(
            native
                .join("onnxruntime-1.28.0/ThirdPartyNotices.txt")
                .is_file()
        );
        let provenance = fs::read_to_string(native.join("luxforge-onnx/THIRD_PARTY.md")).unwrap();
        assert!(provenance.contains("not a completed audit"));
    }
    #[test]
    fn copies_the_sparkle_licence() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        native_sparkle_notices(root, tmp.path()).unwrap();
        let native = tmp.path().join("native");
        let licence = fs::read_to_string(native.join("sparkle-2.10.0/LICENSE")).unwrap();
        assert!(licence.contains("Andy Matuschak"));
        assert!(licence.contains("EXTERNAL LICENSES"));
        let provenance =
            fs::read_to_string(native.join("luxforge-sparkle/THIRD_PARTY.md")).unwrap();
        assert!(provenance.contains(crate::sparkle::SHA256));
        assert!(provenance.contains(crate::sparkle::REVISION));
    }
    #[test]
    fn copies_the_generative_runtime_licences() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        native_diffusion_notices(root, tmp.path()).unwrap();
        let native = tmp.path().join("native");
        for licence in [
            "stable-diffusion.cpp-1b0ba108/LICENSE",
            "ggml-d25b121c/LICENSE",
        ] {
            let text = fs::read_to_string(native.join(licence)).unwrap();
            assert!(text.contains("MIT License"), "{licence}");
        }
        assert!(
            native
                .join("stable-diffusion.cpp-1b0ba108/thirdparty/oniguruma/COPYING")
                .is_file()
        );
        let provenance =
            fs::read_to_string(native.join("luxforge-diffusion/THIRD_PARTY.md")).unwrap();
        assert!(provenance.contains("1b0ba10893f4e2e0656103011fc1c4645a02a734"));
        assert!(provenance.contains("d25b121ce1d6ae0c7e62ad080460d9fc1e46dd93"));
    }
    #[test]
    fn copies_the_bundled_font_licence() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        bundled_font_notices(root, tmp.path()).unwrap();
        let licence = fs::read_to_string(tmp.path().join("fonts/inter-4.1/LICENSE.txt")).unwrap();
        assert!(licence.contains("SIL OPEN FONT LICENSE Version 1.1"));
        assert!(
            tmp.path()
                .join("fonts/luxforge-ui/THIRD_PARTY.md")
                .is_file()
        );
    }
    #[test]
    fn copies_the_bundled_theme_licence() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        bundled_theme_notices(root, tmp.path()).unwrap();
        let licence = fs::read_to_string(tmp.path().join("themes/omarchy/LICENSE")).unwrap();
        assert!(licence.contains("Copyright (c) David Heinemeier Hansson"));
        let notice = fs::read_to_string(tmp.path().join("themes/omarchy/NOTICE.md")).unwrap();
        assert!(notice.contains("035ce29f03bdd97a09af80ef5f2d22d7a98930d6"));
    }
    #[test]
    fn copies_the_bundled_place_names_notice() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        bundled_data_notices(root, tmp.path()).unwrap();
        let notice =
            fs::read_to_string(tmp.path().join("data/luxforge-core/THIRD_PARTY.md")).unwrap();
        assert!(notice.contains("Creative Commons Attribution 4.0 License"));
        assert!(notice.contains("https://www.geonames.org"));
        assert!(notice.contains("modified extract"));
    }
    /// The notice records the asset the binary compiles in (rows, bytes and hash), so a regenerated
    /// asset cannot ship under the previous one's provenance.
    #[test]
    fn the_place_names_notice_records_the_asset_it_ships() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let asset = root.join("crates/luxforge-core/assets/gazetteer/places.tsv");
        let notice = fs::read_to_string(root.join("crates/luxforge-core/THIRD_PARTY.md")).unwrap();
        let record = format!(
            "| `assets/gazetteer/places.tsv` | {} | {} | `{}` |",
            fs::read_to_string(&asset).unwrap().lines().count() - 1,
            fs::metadata(&asset).unwrap().len(),
            hash(&asset).unwrap()
        );
        assert!(notice.contains(&record), "THIRD_PARTY.md lacks {record}");
    }
    #[test]
    fn package_copies_lens_index_and_its_notices() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let temp = tempfile::tempdir().unwrap();
        lens_resources(
            root,
            &temp.path().join("Luxforge.app/Contents/Resources/lensfun"),
        )
        .unwrap();
        lens_resources(root, &temp.path().join("notices/data/lensfun")).unwrap();
        for name in LENS_FILES {
            assert_eq!(
                fs::read(root.join("crates/luxforge-core/data/lensfun").join(name)).unwrap(),
                fs::read(
                    temp.path()
                        .join("Luxforge.app/Contents/Resources/lensfun")
                        .join(name)
                )
                .unwrap()
            );
            assert!(
                temp.path()
                    .join("notices/data/lensfun")
                    .join(name)
                    .is_file()
            );
        }
        let attribution =
            fs::read_to_string(temp.path().join("notices/data/lensfun/ATTRIBUTION.md")).unwrap();
        assert!(attribution.contains("CC BY-SA 3.0"));
        assert!(attribution.contains("modified, filtered conversion"));
    }
    #[test]
    fn archives_preserve_payload_and_zip_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Luxforge");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("luxforge"), b"test executable").unwrap();
        let dest = tmp.path().join("test.zip");
        archive(&dir, &dest).unwrap();
        let mut zip = zip::ZipArchive::new(fs::File::open(dest).unwrap()).unwrap();
        let mut file = zip.by_name("Luxforge/luxforge").unwrap();
        assert_eq!(file.unix_mode().unwrap() & 0o777, 0o755);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"test executable");
        let dest = tmp.path().join("test.tar.gz");
        archive(&dir, &dest).unwrap();
        let mut tar =
            tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(dest).unwrap()));
        let mut found = false;
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap() == Path::new("Luxforge/luxforge") {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, b"test executable");
                found = true;
            }
        }
        assert!(found);
    }
}
