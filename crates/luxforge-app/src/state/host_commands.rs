//! The host's own commands that a chord, the command palette and an evidence script reach alike,
//! declared once: what each is called, what it calls, its chord and where the chord acts. The
//! keyboard table answers these chords from this table, the palette lists these entries from it
//! with their chords beside them, and an evidence step presses the chord the table declares, so no
//! second list of them exists. A module's own chords are its actions' declared `shortcut`s, which
//! [`action_shortcuts`] collects; the chords the host keeps are `luxforge_core::HOST_CHORDS`, which
//! registration holds every module to.
use crate::state::copy_settings::CopyModel;
use luxforge_core::{Chord, ModuleDescriptor};

/// One host command a chord and the palette both reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostCommand {
    /// Copy the displayed photograph's settings with the remembered groups.
    CopySettings,
    /// Copy them through the chooser, which picks the groups first.
    CopySettingsChoosing,
    /// Paste the copied settings onto the displayed photograph, or onto the selected ones.
    PasteSettings,
    /// Paste the previous photograph's settings, copied on the way.
    PastePrevious,
}

/// A host command's declaration.
pub(crate) struct HostCommandSpec {
    pub(crate) command: HostCommand,
    /// The palette entry's label.
    pub(crate) label: &'static str,
    /// The methods the command calls, which the palette shows beside the label.
    pub(crate) detail: &'static str,
    pub(crate) chord: Chord,
    /// Whether the chord acts in the Select workspace as well as in Develop.
    pub(crate) in_select: bool,
}

/// Every host command, in the order the palette lists them.
pub(crate) static HOST_COMMANDS: [HostCommandSpec; 4] = [
    HostCommandSpec {
        command: HostCommand::CopySettings,
        label: "Copy settings",
        detail: "preset.capture",
        chord: Chord::command('C'),
        in_select: true,
    },
    HostCommandSpec {
        command: HostCommand::CopySettingsChoosing,
        label: "Copy settings\u{2026}",
        detail: "preset.capture",
        chord: Chord::command('C').shift(),
        in_select: true,
    },
    HostCommandSpec {
        command: HostCommand::PasteSettings,
        label: "Paste settings",
        detail: "edit.apply-settings / batch.apply-settings",
        chord: Chord::command('V'),
        in_select: true,
    },
    HostCommandSpec {
        command: HostCommand::PastePrevious,
        label: "Paste settings from previous photograph",
        detail: "preset.capture \u{2192} edit.apply-settings",
        chord: Chord::command('V').option(),
        in_select: false,
    },
];

impl HostCommand {
    pub(crate) fn spec(self) -> &'static HostCommandSpec {
        HOST_COMMANDS
            .iter()
            .find(|spec| spec.command == self)
            .expect("every host command is declared")
    }

    pub(crate) fn chord(self) -> Chord {
        self.spec().chord
    }

    /// The chord as a hint beside the command names it ([`glyphs`]).
    pub(crate) fn glyphs(self) -> String {
        glyphs(self.chord())
    }

    /// The command `chord` runs in the workspace shown, if any.
    pub(crate) fn bound(chord: Chord, select: bool) -> Option<Self> {
        HOST_COMMANDS
            .iter()
            .find(|spec| spec.chord == chord && (spec.in_select || !select))
            .map(|spec| spec.command)
    }

    /// Why the command cannot run now, as its palette entry shows it.
    pub(crate) fn refusal(self, model: &CopyModel) -> Option<String> {
        match self {
            Self::CopySettings | Self::CopySettingsChoosing => model.copy_refusal.clone(),
            Self::PasteSettings => model.paste_refusal.clone(),
            Self::PastePrevious => model.previous_refusal.clone(),
        }
    }

    /// The palette entry's label now: Paste names how many photographs it pastes onto when it is
    /// more than one.
    pub(crate) fn label(self, model: &CopyModel) -> String {
        match self {
            Self::PasteSettings if model.targets > 1 => {
                format!("Paste settings to {} photographs", model.targets)
            }
            _ => self.spec().label.to_owned(),
        }
    }
}

/// A chord as the desktop writes it beside a command: the modifier glyphs in the platform order,
/// Option, Shift, Command, then the key, `⌥⌘V`.
pub(crate) fn glyphs(chord: Chord) -> String {
    let mut written = String::new();
    for (held, glyph) in [
        (chord.option, '\u{2325}'),
        (chord.shift, '\u{21e7}'),
        (chord.command, '\u{2318}'),
    ] {
        if held {
            written.push(glyph);
        }
    }
    written.push(chord.key);
    written
}

/// Every chord a registered module declares for one of its actions, with the action it runs. The
/// action answers for itself when it cannot run, as its control does, so a module that does not
/// apply or is unavailable keeps its chord and the chord shows that action's refusal.
pub(crate) fn action_shortcuts(modules: &[ModuleDescriptor]) -> Vec<(Chord, String)> {
    modules
        .iter()
        .flat_map(|module| &module.actions)
        .filter_map(|action| Some((action.shortcut?, action.id.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is the one list: every command declared once, in its own place, and none of its
    /// chords one a module could declare.
    #[test]
    fn every_host_command_is_declared_once_with_a_chord_the_host_keeps() {
        for spec in &HOST_COMMANDS {
            assert!(std::ptr::eq(spec.command.spec(), spec));
            assert!(
                luxforge_core::host_chord(spec.chord).is_some(),
                "{} is not kept from modules",
                spec.chord
            );
            assert_eq!(
                HostCommand::bound(spec.chord, false),
                Some(spec.command),
                "{} runs {}",
                spec.chord,
                spec.label
            );
        }
        assert_eq!(
            HostCommand::bound(Chord::command('V').option(), true),
            None,
            "Paste previous is Develop's"
        );
        assert_eq!(
            glyphs(HostCommand::PastePrevious.chord()),
            "\u{2325}\u{2318}V"
        );
        assert_eq!(
            glyphs(HostCommand::CopySettingsChoosing.chord()),
            "\u{21e7}\u{2318}C"
        );
    }

    #[test]
    fn module_actions_declare_their_chords() {
        assert_eq!(
            action_shortcuts(&crate::state::testing::descriptors()),
            [(Chord::command('U'), "auto-tone".to_owned())]
        );
    }
}
