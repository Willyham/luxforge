//! The keyboard chord an action may declare ([`super::ActionDescriptor::shortcut`]), and the
//! chords the host keeps for its own commands, which no module may declare.
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// One key pressed with modifiers, written `Command+U`, `Command+Shift+C` or `Command+Option+V`:
/// the modifiers that are held, in the order `Command`, `Option`, `Shift`, then one key.
///
/// `Command` is the platform's command modifier: ⌘ on macOS, Control on Windows and Linux.
/// `Option` is ⌥, Alt elsewhere. The key is an uppercase ASCII letter, a digit or one of the
/// punctuation keys [`Chord::PUNCTUATION`]; a letter matches either case, so `Command+Shift+C` is the C
/// key with Shift held, never a different key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Chord {
    pub command: bool,
    pub option: bool,
    pub shift: bool,
    pub key: char,
}

impl Chord {
    /// The punctuation keys a chord may name besides letters and digits.
    pub const PUNCTUATION: &str = ",.-=+[];'/\\`";

    /// `Command` and `key`, with no other modifier.
    pub const fn command(key: char) -> Self {
        Self {
            command: true,
            option: false,
            shift: false,
            key,
        }
    }

    /// This chord with Shift held as well.
    pub const fn shift(self) -> Self {
        Self {
            shift: true,
            ..self
        }
    }

    /// This chord with Option held as well.
    pub const fn option(self) -> Self {
        Self {
            option: true,
            ..self
        }
    }

    fn valid_key(key: char) -> bool {
        key.is_ascii_uppercase() || key.is_ascii_digit() || Self::PUNCTUATION.contains(key)
    }
}

impl FromStr for Chord {
    type Err = String;

    fn from_str(written: &str) -> Result<Self, Self::Err> {
        let mut rest = written;
        let mut take = |modifier: &str| match rest.strip_prefix(modifier) {
            Some(after) if !after.is_empty() => {
                rest = after;
                true
            }
            _ => false,
        };
        let command = take("Command+");
        let option = take("Option+");
        let shift = take("Shift+");
        let mut keys = rest.chars();
        match (keys.next(), keys.next()) {
            (Some(key), None) if Self::valid_key(key) => Ok(Self {
                command,
                option,
                shift,
                key,
            }),
            _ => Err(format!(
                "chord {written:?} is not the held modifiers in the order Command, Option, Shift, \
                 each followed by +, then one uppercase letter, digit or one of {}",
                Self::PUNCTUATION
            )),
        }
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (held, name) in [
            (self.command, "Command+"),
            (self.option, "Option+"),
            (self.shift, "Shift+"),
        ] {
            if held {
                out.write_str(name)?;
            }
        }
        write!(out, "{}", self.key)
    }
}

impl TryFrom<String> for Chord {
    type Error = String;

    fn try_from(written: String) -> Result<Self, Self::Error> {
        written.parse()
    }
}

impl From<Chord> for String {
    fn from(chord: Chord) -> Self {
        chord.to_string()
    }
}

/// The chords the desktop host answers with its own commands, whatever modules are registered, and
/// what each does. Registration refuses an action shortcut that is one of them, so a module's chord
/// never shadows a host command or is shadowed by it. The desktop's keyboard table is held to this
/// list by a test that presses every chord it could answer.
pub const HOST_CHORDS: &[(Chord, &str)] = &[
    (Chord::command('C'), "Copy settings"),
    (
        Chord::command('C').shift(),
        "Copy settings with the chooser",
    ),
    (Chord::command('V'), "Paste settings"),
    (
        Chord::command('V').option(),
        "Paste settings from the previous photograph",
    ),
    (Chord::command('A'), "Select all"),
    (Chord::command('D'), "Select none"),
    (Chord::command('F'), "Find"),
    (Chord::command('F').option(), "Collapse the filmstrip"),
    (
        Chord::command('F').option().shift(),
        "Collapse the filmstrip",
    ),
    (Chord::command('Z'), "Undo"),
    (Chord::command('Z').shift(), "Redo"),
    (Chord::command('Z').option(), "Undo"),
    (Chord::command('Z').option().shift(), "Redo"),
    (Chord::command('O'), "Open"),
    (Chord::command('O').shift(), "Open"),
    (Chord::command('O').option(), "Open"),
    (Chord::command('O').option().shift(), "Open"),
    (Chord::command('K'), "Command palette"),
    (Chord::command('K').shift(), "Command palette"),
    (Chord::command('K').option(), "Command palette"),
    (Chord::command('K').option().shift(), "Command palette"),
    (Chord::command('E'), "Export"),
    (Chord::command('E').shift(), "Export keeping metadata"),
    (Chord::command('E').option(), "Export"),
    (
        Chord::command('E').option().shift(),
        "Export keeping metadata",
    ),
    (Chord::command(','), "Settings"),
    (Chord::command(',').shift(), "Settings"),
    (Chord::command(',').option(), "Settings"),
    (Chord::command(',').option().shift(), "Settings"),
    (Chord::command('='), "Zoom in"),
    (Chord::command('=').shift(), "Zoom in"),
    (Chord::command('=').option(), "Zoom in"),
    (Chord::command('=').option().shift(), "Zoom in"),
    (Chord::command('+'), "Zoom in"),
    (Chord::command('+').shift(), "Zoom in"),
    (Chord::command('+').option(), "Zoom in"),
    (Chord::command('+').option().shift(), "Zoom in"),
    (Chord::command('-'), "Zoom out"),
    (Chord::command('-').shift(), "Zoom out"),
    (Chord::command('-').option(), "Zoom out"),
    (Chord::command('-').option().shift(), "Zoom out"),
    (Chord::command('[').option(), "Show or hide the left panel"),
    (
        Chord::command('[').option().shift(),
        "Show or hide the left panel",
    ),
    (Chord::command(']').option(), "Show or hide the right panel"),
    (
        Chord::command(']').option().shift(),
        "Show or hide the right panel",
    ),
];

/// What the host does with `chord`, when it keeps it.
pub fn host_chord(chord: Chord) -> Option<&'static str> {
    HOST_CHORDS
        .iter()
        .find(|(kept, _)| *kept == chord)
        .map(|(_, command)| *command)
}
