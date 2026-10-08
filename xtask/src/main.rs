mod auto_tone_fit;
mod auto_tone_smoke;
mod basic_acceptance;
mod basic_smoke;
mod capabilities_smoke;
mod catalog_measure;
mod catalog_probes;
/// The field-patch conformance suite the core's own integration test runs, compiled in rather than
/// copied, so `editor-acceptance` records the evidence of exactly the checks `cargo test` makes.
#[path = "../../crates/luxforge-core/tests/modules/conformance/mod.rs"]
mod conformance;
mod controls_smoke;
mod copy_settings_smoke;
mod crop_smoke;
mod curve_acceptance;
mod curve_smoke;
mod detail_grid_performance;
mod detail_performance;
mod detail_smoke;
mod develop_picks_smoke;
mod diagnostics;
mod editor_acceptance;
mod editor_latency;
mod editor_performance;
mod export_smoke;
mod filmstrip_smoke;
mod fixtures;
mod gallery_smoke;
mod gazetteer;
mod generate_catalog;
mod gpu_preview_smoke;
mod gpu_preview_zoom_smoke;
mod gpu_qualification;
mod grade_align;
mod grade_performance;
mod grading_smoke;
mod histogram_smoke;
mod information_smoke;
mod inspect_dng;
mod launch;
mod lens_performance;
mod lens_qualification;
mod lens_smoke;
mod lensfun_import;
mod look_smoke;
mod loupe_smoke;
mod mask_acceptance;
mod mask_brush_smoke;
mod mask_combine_smoke;
mod mask_interactions_smoke;
mod mask_panel_smoke;
mod mask_range_smoke;
mod mask_smoke;
mod minify_smoke;
mod mixer_smoke;
mod no_gpu_render_smoke;
mod package;
mod performance_smoke;
mod presence_mixer_vignette_acceptance;
mod presence_smoke;
mod presets_smoke;
mod preview_error;
mod raw;
mod raw_camera;
mod raw_editor;
mod raw_panel_smoke;
mod resolve_missing_smoke;
mod scenario;
mod select_catalog_smoke;
mod select_smoke;
mod settings_smoke;
mod smoke;
mod stats;
mod theme_smoke;
mod verify;
mod viewport_smoke;
mod vignette_smoke;
mod visibility_smoke;
mod workspace_smoke;
mod zone_plate;
mod zoom_smoke;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
use xtask_cli::{
    Args, Result, cargo, cargo_command, check, ensure, files, output, policy, read_json,
    repository, root,
};
fn write_json(path: &Path, value: &Value) -> Result {
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}
fn hash(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut b = [0; 65536];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
fn run(root: &Path, program: impl AsRef<OsStr>, args: &[&str]) -> Result {
    let status = Command::new(program)
        .args(args)
        .current_dir(root)
        .status()?;
    ensure(
        status.success(),
        format!("Command {args:?} failed: {status}"),
    )
}
fn absolute(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        root.join(path)
    }
}
fn binary(root: &Path) -> Result<PathBuf> {
    let data: Value = serde_json::from_str(&output(
        root,
        "cargo",
        &["metadata", "--locked", "--no-deps", "--format-version", "1"],
    )?)?;
    Ok(Path::new(
        data["target_directory"]
            .as_str()
            .ok_or("Missing target directory")?,
    )
    .join("release")
    .join(format!("luxforge{}", std::env::consts::EXE_SUFFIX)))
}
fn host(root: &Path) -> Result<String> {
    output(root, "rustc", &["-vV"])?
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_owned))
        .ok_or_else(|| "Missing rustc host".into())
}

fn samples(a: &mut Args, default: usize) -> Result<usize> {
    Ok(a.value("--samples")?
        .map(|s| s.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(default))
}
fn main() -> ExitCode {
    match main_result() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("FAIL: {e}");
            // `verify` reporting a tier `incomplete` (a skip or a component that never ran, with
            // nothing failed outright) gets its own exit code, distinct from a pass and from an
            // ordinary failure.
            if e.downcast_ref::<verify::Incomplete>().is_some() {
                ExitCode::from(verify::INCOMPLETE_EXIT_CODE)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}
fn main_result() -> Result {
    let root = root()?;
    let mut args = std::env::args_os().skip(1);
    let op = args.next().unwrap_or_else(|| "help".into());
    let mut a = Args(args.collect());
    match op.to_str().ok_or("Invalid command")? {
        "auto-tone-fit" => auto_tone_fit::run(&mut a)?,
        "develop" => {
            let debug = a.flag("--debug");
            let background = a.flag("--background");
            if background {
                ensure(
                    cfg!(target_os = "macos"),
                    "develop --background is currently supported only on macOS",
                )?;
                let supplied = a.value("--binary")?.map(PathBuf::from);
                let bin = if let Some(bin) = supplied {
                    absolute(&root, &bin)
                } else {
                    cargo(&root, "build", !debug)?;
                    let mut bin = binary(&root)?;
                    if debug {
                        bin = bin
                            .parent()
                            .unwrap()
                            .parent()
                            .unwrap()
                            .join("debug")
                            .join(bin.file_name().unwrap());
                    }
                    bin
                };
                let launch = launch::Background::new(&bin)?;
                ensure(
                    Command::new(&launch.executable)
                        .current_dir(&root)
                        .args(a.0)
                        .status()?
                        .success(),
                    "Application failed",
                )?;
                return Ok(());
            }
            let mut cmd = cargo_command();
            cmd.current_dir(&root).args(["run", "--locked"]);
            if !debug {
                cmd.arg("--release");
            }
            ensure(
                cmd.args(["--package", "luxforge-app", "--bin", "luxforge", "--"])
                    .args(a.0)
                    .status()?
                    .success(),
                "Application failed",
            )?;
        }
        "check" => {
            let quick = a.flag("--quick");
            a.done()?;
            check::check(&root, quick)?;
            println!("Headless checks passed. GUI and platform acceptance remain separate.");
        }
        "check-repository" => {
            a.done()?;
            repository::check(&root)?;
        }
        "test" => {
            let quick = a.flag("--quick");
            a.done()?;
            check::tests(&root, quick)?;
        }
        "build" | "fmt" | "lint" => {
            let release = a.flag("--release");
            a.done()?;
            cargo(&root, op.to_str().unwrap(), release)?;
        }
        "doctor" => {
            a.done()?;
            println!("Host: {}", host(&root)?);
            for (p, as_) in [
                ("rustc", vec!["--version"]),
                ("cargo", vec!["fmt", "--version"]),
                ("cargo", vec!["clippy", "--version"]),
            ] {
                run(&root, p, &as_)?
            }
            if cfg!(target_os = "macos") {
                run(&root, "clang", &["--version"])?;
            }
            if cfg!(target_os = "linux") {
                run(&root, "cc", &["--version"])?;
                run(&root, "pkg-config", &["--version"])?;
                ensure(
                    std::env::var_os("DISPLAY").is_some()
                        || std::env::var_os("WAYLAND_DISPLAY").is_some(),
                    "No graphical session for smoke (headless check/build still available)",
                )?;
            }
            // The adapters the built editor sees, as an evidence run identifies the one that drew
            // it: a GPU, a software rasterizer (device type Cpu, lavapipe on Linux CI) or none.
            let backends = std::env::var("WGPU_BACKEND")
                .map(|value| format!("WGPU_BACKEND={value}"))
                .unwrap_or_else(|_| "every backend".into());
            match launch::gpu_adapters(&root)? {
                None => println!(
                    "Graphics adapters: not listed until the release editor is built (cargo xtask build --release)"
                ),
                Some(adapters) if adapters.is_empty() => println!(
                    "Graphics adapters ({backends}): none; the editor cannot open its window with them"
                ),
                Some(adapters) => {
                    for adapter in adapters {
                        println!(
                            "Graphics adapter ({backends}): {} on {}, device type {}: {adapter}",
                            adapter["adapter"].as_str().unwrap_or_default(),
                            adapter["backend"].as_str().unwrap_or_default(),
                            adapter["device_type"].as_str().unwrap_or_default(),
                        );
                    }
                }
            }
            println!(
                "Native graphics driver, SDK/runtime libraries and an unlocked graphical session are required for UI checks; not proven by Doctor. A Cpu adapter is a software rasterizer: functional evidence, never GPU evidence."
            );
        }
        "fixtures" => {
            a.done()?;
            fixtures::check(&root)?;
        }
        "generate-fixtures" => {
            let out = a
                .value("--output")?
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("fixtures/generated"));
            a.done()?;
            fixtures::generate(&absolute(&root, &out))?;
        }
        "gazetteer" => {
            let source = absolute(&root, &a.path("--source")?);
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            gazetteer::run(&source, &out)?;
        }
        "generate-catalog" => {
            let out = absolute(&root, &a.path("--output")?);
            let mut number = |key: &str| -> Result<Option<u64>> {
                Ok(a.value(key)?
                    .map(|v| v.to_string_lossy().parse::<u64>())
                    .transpose()?)
            };
            let seed = number("--seed")?.unwrap_or(1);
            let mut count = |key: &str| -> Result<Option<u32>> {
                Ok(number(key)?.map(u32::try_from).transpose()?)
            };
            let options = generate_catalog::Options {
                seed,
                files: count("--files")?,
                assets: count("--assets")?,
                images: count("--images")?,
            };
            a.done()?;
            generate_catalog::run(&out, &options)?;
        }
        "raw-camera-metadata" => {
            let index = absolute(&root, &a.path("--index")?);
            let ids: Vec<String> = a
                .value("--ids")?
                .ok_or("Required: --ids")?
                .to_string_lossy()
                .split(',')
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect();
            let out = absolute(&root, &a.path("--output")?);
            let mib = a
                .value("--max-source-mib")?
                .map(|s| s.to_string_lossy().parse::<u64>())
                .transpose()?
                .unwrap_or(raw_camera::DEFAULT_SOURCE_MIB);
            a.done()?;
            raw_camera::run(&index, &ids, &out, mib)?;
        }
        "inspect-dng" => {
            let source = absolute(&root, &a.path("--source")?);
            let out = a.value("--json")?.map(|p| absolute(&root, Path::new(&p)));
            a.done()?;
            inspect_dng::run(&source, out.as_deref())?;
        }
        "raw-authentic" => {
            let manifest = absolute(&root, &a.path("--manifest")?);
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            verify::authentic(&root, &manifest, &out)?;
        }
        "lens-qualification" => {
            let manifest = absolute(&root, &a.path("--manifest")?);
            let edges = absolute(&root, &a.path("--edges")?);
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            lens_qualification::run(&manifest, &edges, &out)?;
        }
        "lensfun-import" => {
            let source = absolute(&root, &a.path("--source")?);
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            lensfun_import::run(&root, &source, &out)?;
        }
        "inventory" | "package" => {
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            if op == "package" {
                package::package(&root, &out)?
            } else {
                package::inventory(&root, &out)?;
            }
        }
        "audit" => {
            a.done()?;
            policy::audit(&root)?;
        }
        "editor-acceptance" => {
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            editor_acceptance::run(&root, &out)?;
        }
        "grade-performance" => {
            let source = absolute(&root, &a.path("--source")?);
            let out = absolute(&root, &a.path("--output")?);
            let samples = samples(&mut a, 30)?;
            a.done()?;
            let _gate = launch::TimingGate::acquire()?;
            grade_performance::run(&root, &source, &out, samples)?;
        }
        "editor-performance" => {
            let source = absolute(&root, &a.path("--source")?);
            let out = absolute(&root, &a.path("--output")?);
            let samples = samples(&mut a, 10)?;
            let lens_only = a.flag("--lens-only");
            a.done()?;
            let _gate = launch::TimingGate::acquire()?;
            if lens_only {
                lens_performance::run(&root, &source, &out, samples)?;
            } else {
                editor_performance::run(&root, &source, &out, samples)?;
            }
        }
        "detail-performance" => {
            let source = absolute(&root, &a.path("--source")?);
            let out = absolute(&root, &a.path("--output")?);
            let samples = samples(&mut a, 30)?;
            let case = a.value("--case")?.unwrap_or_else(|| "all".into());
            a.done()?;
            let _gate = launch::TimingGate::acquire()?;
            detail_performance::run(&root, &source, &out, samples, &case.to_string_lossy())?;
        }
        "detail-grid-performance" => {
            let source = absolute(&root, &a.path("--source")?);
            let out = absolute(&root, &a.path("--output")?);
            let samples = samples(&mut a, 30)?;
            a.done()?;
            let _gate = launch::TimingGate::acquire()?;
            detail_grid_performance::run(&root, &source, &out, samples)?;
        }
        "editor-latency" => {
            let source = absolute(&root, &a.path("--source")?);
            let out = absolute(&root, &a.path("--output")?);
            let bin = a
                .value("--binary")?
                .map(|path| absolute(&root, Path::new(&path)))
                .map_or_else(|| binary(&root), Ok)?;
            let crop = a
                .value("--crop")?
                .map(|s| s.to_string_lossy().parse::<f64>())
                .transpose()?;
            let idle = a.flag("--idle");
            let basic = a.flag("--basic");
            let presence = a.flag("--presence");
            let curve_layer = a.flag("--curve-layer");
            let detail = a.flag("--detail");
            let lens = a.flag("--lens");
            let perspective = a.flag("--perspective");
            let mask = a.flag("--mask");
            let zoom = a
                .value("--zoom")?
                .map(|value| value.to_string_lossy().parse::<f32>())
                .transpose()?;
            let moving_pan = a.flag("--moving-pan");
            let mask_overlay = a.flag("--mask-overlay");
            let contend = a
                .value("--contend")?
                .map(|value| value.to_string_lossy().parse::<usize>())
                .transpose()?;
            let warm_ms = a
                .value("--warm")?
                .map(|value| value.to_string_lossy().parse::<u64>())
                .transpose()?;
            let no_gpu_render = a.flag("--reference-renderer");
            let masks = a
                .value("--masks")?
                .map(|value| value.to_string_lossy().parse::<usize>())
                .transpose()?
                .unwrap_or(1);
            let mask_presence = a.flag("--mask-presence");
            let window = a
                .value("--window")?
                .map(|value| -> Result<[u32; 2]> {
                    let value = value.to_string_lossy().into_owned();
                    let (width, height) = value
                        .split_once('x')
                        .ok_or("--window is WIDTHxHEIGHT logical points")?;
                    Ok([width.parse()?, height.parse()?])
                })
                .transpose()?;
            let control = match a.value("--control")?.as_deref().and_then(OsStr::to_str) {
                None | Some("slider") => editor_latency::Control::Slider,
                Some("curve") => editor_latency::Control::Curve,
                Some("wheel") => editor_latency::Control::Wheel,
                Some(other) => {
                    return Err(format!("--control is slider, curve or wheel, not {other}").into());
                }
            };
            let mode = match a.value("--mode")?.as_deref().and_then(OsStr::to_str) {
                None | Some("drag") => editor_latency::Mode::Drag,
                Some("commit") => editor_latency::Mode::Commit,
                Some("burst") => editor_latency::Mode::Burst,
                Some("paint") => editor_latency::Mode::Paint,
                Some("hover") => editor_latency::Mode::Hover,
                Some("crop-start") => editor_latency::Mode::CropStart,
                Some(other) => {
                    return Err(format!(
                        "--mode is drag, commit, burst, paint, hover or crop-start, not {other}"
                    )
                    .into());
                }
            };
            // A paint run's samples are the stroke's positions, and it paints hundreds by default.
            let samples = samples(
                &mut a,
                if mode == editor_latency::Mode::Paint {
                    editor_latency::PAINT_POSITIONS
                } else {
                    30
                },
            )?;
            if mask_overlay
                && !matches!(
                    mode,
                    editor_latency::Mode::Paint | editor_latency::Mode::Hover
                )
            {
                return Err("--mask-overlay requires --mode paint or hover".into());
            }
            let action = a
                .value("--action")?
                .map(|v| v.to_string_lossy().into_owned());
            let parameter = a
                .value("--parameter")?
                .map(|v| v.to_string_lossy().into_owned());
            a.done()?;
            let _gate = launch::TimingGate::acquire()?;
            editor_latency::run(
                &root,
                &out,
                &bin,
                editor_latency::Options {
                    source: &source,
                    samples,
                    mode,
                    control,
                    action: action.as_deref(),
                    parameter: parameter.as_deref(),
                    crop,
                    idle,
                    basic,
                    presence,
                    curve_layer,
                    detail,
                    lens,
                    perspective,
                    mask,
                    zoom,
                    moving_pan,
                    mask_overlay,
                    contend,
                    warm_ms,
                    no_gpu_render,
                    masks,
                    mask_presence,
                    window,
                },
            )?;
        }
        "smoke" => {
            if a.flag("--list") {
                a.done()?;
                print!("{}", smoke::list(&root));
                return Ok(());
            }
            let out = absolute(&root, &a.path("--output")?);
            let scenario = a
                .value("--scenario")?
                .map(|s| s.into_string().map_err(|_| "Invalid scenario"))
                .transpose()?;
            let bin = a
                .value("--binary")?
                .map(PathBuf::from)
                .map(|p| absolute(&root, &p));
            let source = a
                .value("--source")?
                .map(PathBuf::from)
                .map(|p| absolute(&root, &p));
            if let Some(recorded) = a.value("--verify-only")? {
                let recorded = absolute(&root, Path::new(&recorded));
                let scenario = match scenario {
                    Some(scenario) => scenario,
                    None => read_json(&recorded.join("result.json"))?["scenario"]
                        .as_str()
                        .ok_or("The recorded run names no scenario; pass --scenario")?
                        .to_owned(),
                };
                a.done()?;
                smoke::verify_only(&root, &recorded, &out, &scenario, source.map(|s| vec![s]))?;
                return Ok(());
            }
            let scenario = scenario.unwrap_or_else(|| "load".into());
            // A replay reads the recorded run's own copy of the manifest, so only a new run takes
            // one.
            let manifest = a
                .value("--manifest")?
                .map(|path| absolute(&root, Path::new(&path)));
            if a.flag("--editor-software-adapter") {
                launch::draw_on_a_software_adapter();
            }
            a.done()?;
            let timeout = std::time::Duration::from_secs(35);
            // The row is found before anything is built, so an unknown name or a `--source` the
            // scenario does not take fails at once.
            smoke::find(&scenario)?;
            let bin = bin.map_or_else(|| binary(&root), Ok)?;
            smoke::dispatch(
                &root,
                &out,
                &scenario,
                &bin,
                timeout,
                source.map(|source| vec![source]),
                manifest.as_deref(),
            )?;
        }
        "verify" => {
            let out = absolute(&root, &a.path("--output")?);
            let tier = a
                .value("--tier")?
                .map(|t| {
                    t.into_string()
                        .map_err(|_| "Invalid tier".into())
                        .and_then(|t| verify::Tier::parse(&t))
                })
                .transpose()?
                .unwrap_or(verify::Tier::Quick);
            let bin = a.value("--binary")?.map(PathBuf::from);
            let manifest = a.value("--manifest")?.map(PathBuf::from);
            let jobs = a
                .value("--jobs")?
                .map(|s| s.to_string_lossy().parse::<usize>())
                .transpose()?
                .unwrap_or(verify::JOBS);
            a.done()?;
            verify::run(&root, &out, tier, bin, manifest, jobs)?;
        }
        "check-capture" => {
            let path = absolute(&root, &a.path("--image")?);
            let orientation = a
                .value("--orientation")?
                .map(|s| s.to_string_lossy().parse::<u8>())
                .transpose()?
                .unwrap_or(6);
            let aspect = a
                .value("--aspect")?
                .map(|s| s.to_string_lossy().parse::<f64>())
                .transpose()?;
            let columns = a
                .value("--columns")?
                .map(|s| -> Result<[u32; 2]> {
                    let text = s.to_string_lossy();
                    let (left, right) = text
                        .split_once(',')
                        .ok_or("--columns expects LEFT,RIGHT physical pixels")?;
                    Ok([left.trim().parse()?, right.trim().parse()?])
                })
                .transpose()?;
            a.done()?;
            println!(
                "{}",
                scenario::pixels::fixture(
                    &image::open(&path)?.to_rgb8(),
                    &scenario::Fixture {
                        aspect,
                        columns,
                        ..scenario::Fixture::fit(orientation)
                    }
                )?
            );
        }
        "catalog-measure" => {
            let out = absolute(&root, &a.path("--output")?);
            let samples = samples(&mut a, catalog_measure::DEFAULT_SAMPLES)?;
            let scale = a
                .value("--scale")?
                .map(|scale| catalog_measure::Scale::parse(&scale.to_string_lossy()))
                .transpose()?
                .unwrap_or(catalog_measure::Scale::Full);
            let binary = a
                .value("--binary")?
                .map(|path| absolute(&root, Path::new(&path)))
                .map_or_else(|| binary(&root), Ok)?;
            let corpus = a
                .value("--raw-corpus")?
                .or_else(|| std::env::var_os(catalog_measure::CORPUS_ENV))
                .map(|path| absolute(&root, Path::new(&path)));
            let card = a
                .value("--card")?
                .map(|path| absolute(&root, Path::new(&path)));
            let only = a
                .value("--only")?
                .map(|step| step.to_string_lossy().into_owned());
            a.done()?;
            // Timing runs never overlap, whether they were started by `verify` or by hand.
            let _gate = launch::TimingGate::acquire()?;
            catalog_measure::run(
                &root,
                &out,
                &catalog_measure::Options {
                    samples,
                    scale,
                    binary,
                    corpus,
                    card,
                    only,
                },
            )?;
        }
        "hardening" | "measure" => {
            let out = absolute(&root, &a.path("--output")?);
            let bin = absolute(&root, &a.path("--binary")?);
            let samples = samples(&mut a, 5)?;
            a.done()?;
            if op == "hardening" {
                diagnostics::hardening(&root, &out, &bin)?
            } else {
                // Timing runs never overlap, whether they were started by `verify` or by hand.
                let _gate = launch::TimingGate::acquire()?;
                diagnostics::measure(&root, &out, &bin, samples)?;
            }
        }
        "preview-error" => preview_error::run(a)?,
        "preview-corpus" => preview_error::corpus::run(&root, a)?,
        "grade-align" => grade_align::run(&root, a)?,
        "gpu-qualification" => {
            let options = gpu_qualification::Options::parse(&root, &mut a)?;
            a.done()?;
            gpu_qualification::run(&root, &options)?;
        }
        "__hang" => std::thread::sleep(std::time::Duration::from_secs(60)),
        "help" => println!("{}", xtask_cli::HELP),
        _ => return Err("Unknown command; use cargo xtask help".into()),
    }
    Ok(())
}
