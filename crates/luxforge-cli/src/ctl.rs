//! `luxforge-ctl`: operate the catalog the Luxforge desktop has open through its live session
//! ([`luxforge_cli::live::command`]).
use luxforge_cli::{Paths, live::command};

fn main() {
    let status = command::run(
        std::env::args_os().skip(1),
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
        Paths::resolve(None).as_ref(),
    );
    std::process::exit(status);
}
