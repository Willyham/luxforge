//! `removal-probe inspect`: a model's inputs, outputs and load time on one backend.

use std::path::PathBuf;

use crate::args::Args;
use crate::runtime::{self, Backend};

pub const USAGE: &str = "  inspect MODEL.onnx [--backend B]         list a model's inputs and outputs and its load time";

pub fn run(mut a: Args) -> Result<(), String> {
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
