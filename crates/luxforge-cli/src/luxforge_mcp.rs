//! `luxforge-mcp`: a Model Context Protocol server for the Luxforge desktop's live session, on
//! standard input and output ([`luxforge_cli::mcp::command`]).
use luxforge_cli::{Paths, mcp::command};

fn main() {
    let status = command::run(
        std::env::args_os().skip(1),
        &mut std::io::stdin().lock(),
        Box::new(std::io::stdout()),
        // Unlocked: the server's threads write diagnostics to it while it serves.
        &mut std::io::stderr(),
        Paths::resolve(None),
    );
    std::process::exit(status);
}
