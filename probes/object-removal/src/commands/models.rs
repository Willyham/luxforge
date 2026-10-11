//! `removal-probe models`: verify every pinned model file, refusing any that differs by name.

use crate::args::Args;
use crate::models;

pub const USAGE: &str = "  models [--models DIR]                    verify every pinned model file by size and SHA-256;
                                           a file that differs is refused by name and the command fails";

pub fn run(mut a: Args) -> Result<(), String> {
    let dir = models::directory(a.path("--models")?);
    a.done()?;
    println!("models directory {}", dir.display());
    let mut refused = Vec::new();
    for m in models::ALL {
        match m.verified(&dir) {
            Ok(path) => println!("ok       {:<22} {}", m.id, path.display()),
            Err(e) => {
                println!("REFUSED  {:<22} {}", m.id, m.file);
                refused.push(e);
            }
        }
    }
    for m in models::OPTIONAL {
        if !dir.join(m.file).exists() {
            println!("--       {:<22} absent (optional)", m.id);
            continue;
        }
        match m.verified(&dir) {
            Ok(path) => println!("ok       {:<22} {} (optional)", m.id, path.display()),
            Err(e) => {
                println!("REFUSED  {:<22} {} (optional)", m.id, m.file);
                refused.push(e);
            }
        }
    }
    if refused.is_empty() {
        Ok(())
    } else {
        Err(refused.join("\n"))
    }
}
