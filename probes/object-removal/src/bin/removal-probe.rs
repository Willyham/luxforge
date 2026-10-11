//! `removal-probe`: object removal experiments outside the editor. The commands are in
//! `luxforge_removal_probe::commands`, one file each.

use std::process::ExitCode;

use luxforge_removal_probe::commands;

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        eprintln!("{}", commands::usage());
        return ExitCode::FAILURE;
    }
    let command = argv.remove(0);
    match commands::dispatch(&command, argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
