//! `removal-probe`: object removal experiments outside the editor.

use std::path::PathBuf;
use std::process::ExitCode;

use luxforge_removal_probe::args::Args;
use luxforge_removal_probe::commands;
use luxforge_removal_probe::models;
use luxforge_removal_probe::runtime::{self, Backend};

const USAGE: &str = "usage: removal-probe <command> [options]
  models [--models DIR]                    verify every pinned model file
  inspect MODEL.onnx [--backend B]         list a model's inputs and outputs and its load time";

fn usage() -> String {
    [
        USAGE,
        commands::segment::USAGE,
        commands::fill::USAGE,
        commands::remove::USAGE,
        commands::bench::USAGE,
        commands::parity::USAGE,
    ]
    .join("\n")
}

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        eprintln!("{}", usage());
        return ExitCode::FAILURE;
    }
    let command = argv.remove(0);
    let result = match command.as_str() {
        "models" => verify_models(Args::new(argv)),
        "inspect" => inspect(Args::new(argv)),
        "segment" => commands::segment::run(Args::new(argv)),
        "fill" => commands::fill::run(Args::new(argv)),
        "remove" => commands::remove::run(Args::new(argv)),
        "bench" => commands::bench::run(Args::new(argv)),
        "parity" => commands::parity::run(Args::new(argv)),
        _ => Err(format!("unknown command {command:?}\n{}", usage())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn verify_models(mut a: Args) -> Result<(), String> {
    let dir = models::directory(a.path("--models")?);
    a.done()?;
    for m in models::ALL {
        let path = m.verified(&dir)?;
        println!("ok  {:<22} {}", m.id, path.display());
    }
    for m in models::OPTIONAL {
        if dir.join(m.file).exists() {
            let path = m.verified(&dir)?;
            println!("ok  {:<22} {} (optional)", m.id, path.display());
        } else {
            println!("--  {:<22} absent (optional)", m.id);
        }
    }
    Ok(())
}

fn inspect(mut a: Args) -> Result<(), String> {
    let backend: Backend = a.parsed("--backend")?.unwrap_or(Backend::OrtCpu);
    let model = PathBuf::from(a.positional().ok_or("inspect needs a model path")?);
    a.done()?;
    let (session, load) = runtime::open(backend, &model)?;
    println!(
        "{} on {} loaded in {:.1} ms",
        model.display(),
        backend.name(),
        load.as_secs_f64() * 1e3
    );
    for io in session.inputs() {
        println!("  in   {:<28} {}", io.name, io.description);
    }
    for io in session.outputs() {
        println!("  out  {:<28} {}", io.name, io.description);
    }
    Ok(())
}
