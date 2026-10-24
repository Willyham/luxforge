//! The `luxforge-mcp` command line: read the flags, then serve standard input and output until the
//! input ends.
use super::{
    adapter::Adapter,
    config,
    owner::{LiveTarget, Owner},
    serve::{self, Output, Subscriptions},
    tools::{Profile, Tools},
};
use crate::{
    Paths,
    live::{EXIT_USAGE, USAGE},
};
use serde_json::json;
use std::{
    ffi::OsString,
    io::{BufRead, Write},
    path::PathBuf,
};

const HELP: &str = "luxforge-mcp [--catalog PATH] [--tools compact] [--no-images]
luxforge-mcp config CLIENT [--headless] [--catalog PATH]

A Model Context Protocol server for the Luxforge desktop's live session, on standard input and
output. An MCP client (Claude Code, Claude Desktop, Codex, Cursor and others) starts it and owns its
lifetime. It speaks MCP 2026-07-28, and 2025-11-25 or 2025-06-18 after initialize. It answers
discovery and lists its tools without Luxforge open, and attaches to the running Luxforge at the
first tool call that needs it, as one more live client with edit authority. It never starts
Luxforge or another catalog owner.

--catalog PATH    the catalog whose live session to attach to; without it, the one running
                  Luxforge's, from the registry of running live sessions.
--tools compact   the tools offered. compact, the default, is three tools over the whole registry:
                  luxforge_methods, luxforge_describe and luxforge_call.
--no-images       never attach pictures to results; their paths are still returned as data.
--help, --version print this help or the version, without serving.

config CLIENT     print CLIENT's configuration for starting this luxforge-mcp, without serving:
                  the text on standard output, and where it goes on standard error. CLIENT is
                  claude-code, claude-desktop, codex, cursor or json (the mcpServers block other
                  clients read). --catalog PATH and --headless are added to the arguments the
                  client starts it with. It never writes another application's configuration.

Diagnostics go to standard error. The server ends when standard input ends, with exit status 0;
a command line that cannot be read exits with status 2.";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Parsed {
    Help,
    Version,
    Serve(Options),
}

#[derive(Debug, PartialEq)]
struct Options {
    catalog: Option<PathBuf>,
    profile: Profile,
    images: bool,
}

/// Read the arguments, after the program's name.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Parsed, String> {
    let mut args = args.into_iter();
    let mut options = Options {
        catalog: None,
        profile: Profile::Compact,
        images: true,
    };
    let mut tools = false;
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--help" | "-h") => return Ok(Parsed::Help),
            Some("--version") => return Ok(Parsed::Version),
            Some("--catalog") => {
                if options.catalog.is_some() {
                    return Err("--catalog is given twice".into());
                }
                options.catalog = Some(args.next().ok_or("--catalog requires a path")?.into());
            }
            Some("--tools") => {
                if tools {
                    return Err("--tools is given twice".into());
                }
                tools = true;
                let value = args.next().ok_or("--tools requires a profile")?;
                match value.to_str() {
                    Some("compact") => options.profile = Profile::Compact,
                    _ => {
                        return Err(format!(
                            "--tools {} is not available: typed tool profiles (all, or a list of \
                             namespaces) are not built yet; use --tools compact",
                            value.to_string_lossy()
                        ));
                    }
                }
            }
            Some("--no-images") => {
                if !options.images {
                    return Err("--no-images is given twice".into());
                }
                options.images = false;
            }
            _ => {
                return Err(format!(
                    "unknown argument {}; use --help",
                    argument.to_string_lossy()
                ));
            }
        }
    }
    Ok(Parsed::Serve(options))
}

/// Run `luxforge-mcp` with `args`, after the program's name, serving `stdin` and `stdout` until
/// the input ends, and answer its exit status. `paths` is where the desktop's configuration is,
/// for the registry of running live sessions.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    stdin: &mut dyn BufRead,
    mut stdout: Box<dyn Write + Send>,
    stderr: &mut dyn Write,
    paths: Option<Paths>,
) -> i32 {
    let mut args = args.into_iter().peekable();
    if args.peek().is_some_and(|first| first == "config") {
        args.next();
        let binary = std::env::current_exe().and_then(|path| path.canonicalize());
        return config::run(
            args,
            binary,
            luxforge_core::agents::Platform::current(),
            &mut stdout,
            stderr,
        );
    }
    let options = match parse(args) {
        Ok(Parsed::Help) => {
            let _ = writeln!(stdout, "{HELP}");
            return 0;
        }
        Ok(Parsed::Version) => {
            let _ = writeln!(stdout, "luxforge-mcp {}", crate::VERSION);
            return 0;
        }
        Ok(Parsed::Serve(options)) => options,
        Err(message) => {
            let _ = writeln!(
                stderr,
                "{}",
                json!({"error": {"code": USAGE, "message": message}})
            );
            return EXIT_USAGE;
        }
    };
    let subscriptions = Subscriptions::default();
    let owner = Owner::new(Box::new(LiveTarget {
        catalog: options.catalog,
        paths,
    }));
    let adapter = Adapter::new(
        Tools {
            profile: options.profile,
            images: options.images,
        },
        owner,
        subscriptions.clone(),
    );
    serve::serve(stdin, Output::new(stdout), subscriptions, adapter);
    0
}

#[cfg(test)]
mod tests;
