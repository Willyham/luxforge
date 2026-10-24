//! `luxforge-mcp config CLIENT [--headless] [--catalog PATH]`: print `CLIENT`'s configuration for
//! this executable, the text the desktop's Settings › Agents tab shows
//! ([`luxforge_core::agents`]). It writes nothing: the person pastes the text where it goes.
use crate::live::{EXIT_FAILURE, EXIT_USAGE, INTERNAL, USAGE};
use luxforge_core::agents::{self, Client, Launch, Platform, SnippetError};
use serde_json::json;
use std::{ffi::OsString, io::Write, path::PathBuf};

/// What `config` was asked for.
#[derive(Debug, PartialEq)]
struct Request {
    client: Client,
    catalog: Option<PathBuf>,
    headless: bool,
}

fn client_ids() -> String {
    Client::ALL
        .iter()
        .map(|client| client.id())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Read the arguments after `config`.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Request, String> {
    let mut args = args.into_iter();
    let first = args
        .next()
        .ok_or_else(|| format!("config requires a client: {}", client_ids()))?;
    let client = first.to_str().and_then(Client::from_id).ok_or_else(|| {
        format!(
            "config has no client {}; use one of {}",
            first.to_string_lossy(),
            client_ids()
        )
    })?;
    let mut request = Request {
        client,
        catalog: None,
        headless: false,
    };
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--headless") => {
                if request.headless {
                    return Err("--headless is given twice".into());
                }
                request.headless = true;
            }
            Some("--catalog") => {
                if request.catalog.is_some() {
                    return Err("--catalog is given twice".into());
                }
                request.catalog = Some(args.next().ok_or("--catalog requires a path")?.into());
            }
            _ => {
                return Err(format!(
                    "unknown config argument {}; use --help",
                    argument.to_string_lossy()
                ));
            }
        }
    }
    if request.headless && request.catalog.is_none() {
        return Err("--headless requires --catalog".into());
    }
    Ok(request)
}

/// Print the configuration `args` (after `config`) asks for, for `binary`, this executable's
/// canonical path, on `platform`: the snippet's text on `stdout`, where it goes and what to do with
/// it on `stderr`. Answers the exit status.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    binary: std::io::Result<PathBuf>,
    platform: Platform,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let fail = |stderr: &mut dyn Write, code: &str, message: String| {
        let _ = writeln!(
            stderr,
            "{}",
            json!({"error": {"code": code, "message": message}})
        );
        if code == USAGE {
            EXIT_USAGE
        } else {
            EXIT_FAILURE
        }
    };
    let request = match parse(args) {
        Ok(request) => request,
        Err(message) => return fail(stderr, USAGE, message),
    };
    // A client starts the server from a folder of its own, so a catalog is named absolutely.
    let catalog = match request.catalog.as_deref().map(std::path::absolute) {
        Some(Err(error)) => return fail(stderr, USAGE, format!("--catalog: {error}")),
        Some(Ok(catalog)) => Some(catalog),
        None => None,
    };
    let binary = match binary {
        Ok(binary) => binary,
        Err(error) => {
            let message = format!("this executable's path cannot be read: {error}");
            return fail(stderr, INTERNAL, message);
        }
    };
    let launch = Launch {
        binary: &binary,
        catalog: catalog.as_deref(),
        headless: request.headless,
    };
    match agents::snippet(request.client, launch, platform) {
        Ok(snippet) => {
            let _ = writeln!(stdout, "{}", snippet.text);
            if let Some(location) = &snippet.location {
                let _ = writeln!(stderr, "In {location}:");
            }
            let _ = writeln!(stderr, "{}", snippet.instruction);
            0
        }
        // The executable's own path failing is not the command line's fault.
        Err(error @ SnippetError::NotUnicode(_)) if binary.to_str().is_none() => {
            fail(stderr, INTERNAL, error.to_string())
        }
        Err(error) => fail(stderr, USAGE, error.to_string()),
    }
}

#[cfg(test)]
mod tests;
