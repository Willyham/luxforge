//! `removal-probe corpus`: list the manifest and what is present. It never downloads.

use crate::args::Args;
use crate::corpus::{Dirs, Manifest, Presence, check_public};

pub const USAGE: &str = "  corpus [--verify | --curl] [--corpus DIR] [--public DIR]
                                           list corpus.json: the private images by name and use, the public
                                           CC0 subset with its source URL and licence, and the ground-truth
                                           cases. --verify checks each present public file's SHA-256 and
                                           fails on a mismatch; a missing file is reported with its URL
                                           (fetch it by hand: the probe never downloads).
                                           --curl prints, and does not run, the curl commands that fetch the
                                           absent public files into DIR";

pub fn run(mut a: Args) -> Result<(), String> {
    let verify = a.flag("--verify");
    let curl = a.flag("--curl");
    let dirs = Dirs::new(a.path("--corpus")?, a.path("--public")?);
    a.done()?;
    let m = Manifest::embedded();
    if curl {
        if verify {
            return Err("--verify and --curl exclude each other".into());
        }
        println!("mkdir -p '{}'", dirs.public.display());
        for i in &m.public.images {
            if check_public(i, &dirs.public, false) == Presence::Absent {
                println!(
                    "curl -fL --output '{}' '{}'",
                    dirs.public.join(&i.file).display(),
                    i.url
                );
            }
        }
        return Ok(());
    }
    let mut refused = Vec::new();

    println!(
        "private corpus {} (never committed)",
        dirs.private.display()
    );
    for i in &m.private.images {
        let state = if dirs.private.join(&i.file).is_file() {
            "present"
        } else {
            "absent"
        };
        println!("  {state:<8} {:<24} {}", i.file, i.usage);
    }

    println!(
        "public CC0 subset {} ({} images)",
        dirs.public.display(),
        m.public.images.len()
    );
    for i in &m.public.images {
        let state = match check_public(i, &dirs.public, verify) {
            Presence::Absent => "absent",
            Presence::Present => "present",
            Presence::Verified => "verified",
            Presence::Refused(why) => {
                refused.push(why);
                "REFUSED"
            }
        };
        println!(
            "  {state:<8} {:<30} {:<13} {} x {}  {}  by {}",
            i.file, i.category, i.width, i.height, i.licence, i.author
        );
        if state == "absent" {
            println!("           fetch {}", i.url);
        }
    }

    println!(
        "ground truth: {} pasted-object cases, edge ring {} px",
        m.ground_truth.cases.len(),
        m.ground_truth.ring_width
    );
    for c in &m.ground_truth.cases {
        let present = dirs.resolve(&c.image).map(|p| p.is_file()).unwrap_or(false);
        println!(
            "  {:<9} {:<26} {}",
            if present { "ready" } else { "no image" },
            c.id,
            c.image
        );
    }
    if refused.is_empty() {
        Ok(())
    } else {
        Err(refused.join("\n"))
    }
}
