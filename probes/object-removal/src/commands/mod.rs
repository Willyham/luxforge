//! One file per `removal-probe` command, so each can change independently.
//!
//! To add a command, write `commands/NAME.rs` with a `USAGE` text and `run(Args)`, declare the
//! module here and add one row to [`ALL`]. `removal-probe` dispatches and prints its usage from
//! the table, so nothing else changes.

use crate::args::Args;

pub mod bench;
pub mod corpus;
pub mod fill;
pub mod inspect;
pub mod models;
pub mod parity;
pub mod paste;
pub mod remove;
pub mod score;
pub mod segment;

/// A `removal-probe` command: its name, its usage text (each line indented two spaces) and entry.
pub struct Command {
    pub name: &'static str,
    pub usage: &'static str,
    pub run: fn(Args) -> Result<(), String>,
}

/// Every command, in the order the usage lists them.
pub const ALL: &[Command] = &[
    Command {
        name: "models",
        usage: models::USAGE,
        run: models::run,
    },
    Command {
        name: "inspect",
        usage: inspect::USAGE,
        run: inspect::run,
    },
    Command {
        name: "segment",
        usage: segment::USAGE,
        run: segment::run,
    },
    Command {
        name: "fill",
        usage: fill::USAGE,
        run: fill::run,
    },
    Command {
        name: "remove",
        usage: remove::USAGE,
        run: remove::run,
    },
    Command {
        name: "bench",
        usage: bench::USAGE,
        run: bench::run,
    },
    Command {
        name: "parity",
        usage: parity::USAGE,
        run: parity::run,
    },
    Command {
        name: "corpus",
        usage: corpus::USAGE,
        run: corpus::run,
    },
    Command {
        name: "paste",
        usage: paste::USAGE,
        run: paste::run,
    },
    Command {
        name: "score",
        usage: score::USAGE,
        run: score::run,
    },
];

/// The usage text: one block per command.
pub fn usage() -> String {
    let mut lines = vec!["usage: removal-probe <command> [options]".to_string()];
    lines.extend(ALL.iter().map(|c| c.usage.to_string()));
    lines.join("\n")
}

/// Runs the command `name` with its arguments.
pub fn dispatch(name: &str, args: Vec<String>) -> Result<(), String> {
    match ALL.iter().find(|c| c.name == name) {
        Some(c) => (c.run)(Args::new(args)),
        None => Err(format!("unknown command {name:?}\n{}", usage())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_named_once_and_has_usage_that_starts_with_its_name() {
        let mut names: Vec<_> = ALL.iter().map(|c| c.name).collect();
        for c in ALL {
            assert!(
                c.usage.trim_start().starts_with(c.name),
                "{} usage: {}",
                c.name,
                c.usage
            );
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL.len());
    }

    #[test]
    fn an_unknown_command_lists_the_usage() {
        let e = dispatch("nonsense", Vec::new()).unwrap_err();
        assert!(e.contains("unknown command") && e.contains("score"), "{e}");
    }
}
