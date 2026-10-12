//! The question a release build launched by a person asks when it runs outside Applications on
//! macOS, as plain data: who may be asked, where the question is in its one life, and the notice
//! it draws ([design](../../../../docs/design/auto-update.md#moving-into-applications)).
//!
//! Where the app runs and how it moves are `luxforge-install`'s; the update loop only holds the
//! answer it classified and the phase. A move runs off the loop and answers once, so the notice
//! shows Moving until it does.
use luxforge_install::{Cleanup, Placement, Roots};
use std::path::{Path, PathBuf};

pub(crate) const TITLE: &str = "Move Luxforge to Applications?";
pub(crate) const BODY: &str = "Luxforge can only update itself from the Applications folder.";
pub(crate) const MOVE: &str = "Move and relaunch";
pub(crate) const NOT_NOW: &str = "Not now";
pub(crate) const DONT_ASK: &str = "Don't ask again";
pub(crate) const REPLACE_TITLE: &str = "Replace the Luxforge in Applications?";
pub(crate) const REPLACE: &str = "Replace and relaunch";
pub(crate) const CANCEL: &str = "Cancel";
pub(crate) const MOVING_TITLE: &str = "Moving Luxforge to Applications\u{2026}";
pub(crate) const MOVING_BODY: &str = "Luxforge copies itself, checks the copy's signature, then \
     closes the catalog and relaunches from Applications.";
pub(crate) const FAILED_TITLE: &str = "Luxforge could not move to Applications";
pub(crate) const TRY_AGAIN: &str = "Try again";

/// Whether this launch may ask, decided once at launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Asker {
    /// A development build, or a hidden, background, evidence, smoke or verify launch, or any
    /// launch off macOS: never asks.
    #[default]
    Never,
    /// A release build a person launched, with its window shown.
    Person,
    /// An evidence run given `--evidence-move-to-applications`: it shows the question wherever it
    /// runs, so it can be captured, and never moves.
    Evidence,
}

impl Asker {
    /// Who this launch asks: a person-launched macOS release build, or an evidence run that asks
    /// for the question.
    pub(crate) fn of(release: bool, macos: bool, hidden: bool, evidence: bool, flag: bool) -> Self {
        if evidence && flag {
            Self::Evidence
        } else if release && macos && !hidden && !evidence {
            Self::Person
        } else {
            Self::Never
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Person => "person",
            Self::Evidence => "evidence",
        }
    }
}

/// Where the question is. It moves forward only, but for Cancel on the replace confirmation and Try
/// again after a failure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Waiting for the workspace to be usable.
    #[default]
    Waiting,
    /// The running bundle is being classified, off the update loop.
    Classifying,
    /// It will ask, once no sheet, dialog or other startup notice is open.
    Pending,
    /// On screen.
    Shown,
    /// Move found a Luxforge already at this path, and asks before it goes to the Trash.
    Confirming(PathBuf),
    /// A move runs, off the update loop.
    Moving { replace: bool },
    /// The move failed, and why; nothing was changed.
    Failed(String),
    /// Moved: the ordinary quit path runs, and the app relaunches from Applications after it.
    Relaunching,
    /// Not now.
    Declined,
    /// It does not ask, and why: `in-applications`, `not-a-bundle` or `dont-ask`.
    NotAsked(&'static str),
}

impl Phase {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Classifying => "classifying",
            Self::Pending => "pending",
            Self::Shown => "shown",
            Self::Confirming(_) => "confirming",
            Self::Moving { .. } => "moving",
            Self::Failed(_) => "failed",
            Self::Relaunching => "relaunching",
            Self::Declined => "declined",
            Self::NotAsked(_) => "not-asked",
        }
    }
}

/// The desktop's move question.
#[derive(Clone, Debug)]
pub(crate) struct MoveQuestion {
    pub(crate) asker: Asker,
    pub(crate) phase: Phase,
    /// The notice's Don't ask again switch, stored as `update_ask_to_move: false` with Not now.
    pub(crate) dont_ask: bool,
    /// Where the running bundle is, once classified; `None` outside a bundle.
    pub(crate) placement: Option<Placement>,
    /// The Applications folders a move chooses between: the real ones, or a test's.
    pub(crate) roots: Roots,
}

impl Default for MoveQuestion {
    fn default() -> Self {
        Self {
            asker: Asker::Never,
            phase: Phase::Waiting,
            dont_ask: false,
            placement: None,
            roots: Roots::standard(),
        }
    }
}

impl MoveQuestion {
    pub(crate) fn new(asker: Asker) -> Self {
        Self {
            asker,
            ..Self::default()
        }
    }

    /// Where the classified bundle is, by name, or `None` before or outside one.
    pub(crate) fn location(&self) -> Option<&'static str> {
        self.placement
            .as_ref()
            .map(|placement| placement.location.as_str())
    }
}

/// One of the notice's buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MoveAction {
    Move,
    NotNow,
    Replace,
    Cancel,
}

/// The notice as the workspace draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MoveNotice {
    pub(crate) title: &'static str,
    pub(crate) body: String,
    /// The buttons, in order, by their labels.
    pub(crate) actions: Vec<(&'static str, MoveAction)>,
    /// The Don't ask again switch, when it is drawn, and whether it is on.
    pub(crate) dont_ask: Option<bool>,
}

/// The notice, while the question is on screen.
pub(crate) fn notice(question: &MoveQuestion) -> Option<MoveNotice> {
    let ask = |title, body: String, actions: Vec<(&'static str, MoveAction)>, toggle: bool| {
        Some(MoveNotice {
            title,
            body,
            actions,
            dont_ask: toggle.then_some(question.dont_ask),
        })
    };
    match &question.phase {
        Phase::Shown => ask(
            TITLE,
            BODY.into(),
            vec![(MOVE, MoveAction::Move), (NOT_NOW, MoveAction::NotNow)],
            true,
        ),
        Phase::Confirming(path) => ask(
            REPLACE_TITLE,
            replace_body(path),
            vec![(REPLACE, MoveAction::Replace), (CANCEL, MoveAction::Cancel)],
            false,
        ),
        Phase::Moving { .. } | Phase::Relaunching => {
            ask(MOVING_TITLE, MOVING_BODY.into(), Vec::new(), false)
        }
        Phase::Failed(reason) => ask(
            FAILED_TITLE,
            format!("{reason}. Nothing was changed."),
            vec![(TRY_AGAIN, MoveAction::Move), (NOT_NOW, MoveAction::NotNow)],
            true,
        ),
        _ => None,
    }
}

/// What the replace confirmation says of the app in the way.
pub(crate) fn replace_body(path: &Path) -> String {
    format!(
        "Another Luxforge is already at {}. Replacing it moves that one to the Trash.",
        path.display()
    )
}

/// What a move leaves of the original, by name, as the log and captured state record it.
pub(crate) fn cleanup_name(cleanup: &Cleanup) -> &'static str {
    match cleanup {
        Cleanup::Trash(_) => "trash",
        Cleanup::Eject(_) => "eject",
        Cleanup::Leave => "leave",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_install::Location;

    fn question(phase: Phase) -> MoveQuestion {
        MoveQuestion {
            asker: Asker::Person,
            phase,
            ..MoveQuestion::default()
        }
    }

    #[test]
    fn move_to_applications_only_a_shown_release_build_launch_asks() {
        // (release, macOS, hidden, evidence, flag)
        assert_eq!(Asker::of(true, true, false, false, false), Asker::Person);
        for never in [
            (false, true, false, false, false),
            (true, false, false, false, false),
            (true, true, true, false, false),
            (true, true, false, true, false),
            // The evidence flag means nothing without an evidence run.
            (false, true, false, false, true),
        ] {
            let (release, macos, hidden, evidence, flag) = never;
            assert_eq!(
                Asker::of(release, macos, hidden, evidence, flag),
                Asker::Never,
                "{never:?}"
            );
        }
        assert_eq!(Asker::of(false, true, true, true, true), Asker::Evidence);
    }

    #[test]
    fn move_to_applications_the_question_says_the_designs_words() {
        let mut shown = question(Phase::Shown);
        let notice = notice(&shown).unwrap();
        assert_eq!(notice.title, "Move Luxforge to Applications?");
        assert_eq!(
            notice.body,
            "Luxforge can only update itself from the Applications folder."
        );
        assert_eq!(
            notice.actions,
            [
                ("Move and relaunch", MoveAction::Move),
                ("Not now", MoveAction::NotNow)
            ]
        );
        assert_eq!(notice.dont_ask, Some(false));
        shown.dont_ask = true;
        assert_eq!(super::notice(&shown).unwrap().dont_ask, Some(true));
    }

    #[test]
    fn move_to_applications_a_replacement_is_confirmed_and_a_failure_offers_again() {
        let path = PathBuf::from("/Applications/Luxforge.app");
        let confirming = notice(&question(Phase::Confirming(path))).unwrap();
        assert_eq!(confirming.title, REPLACE_TITLE);
        assert_eq!(
            confirming.body,
            "Another Luxforge is already at /Applications/Luxforge.app. Replacing it moves that \
             one to the Trash."
        );
        assert_eq!(
            confirming.actions,
            [(REPLACE, MoveAction::Replace), (CANCEL, MoveAction::Cancel)]
        );
        assert_eq!(confirming.dont_ask, None);
        let moving = notice(&question(Phase::Moving { replace: false })).unwrap();
        assert!(moving.actions.is_empty() && moving.title == MOVING_TITLE);
        let failed = notice(&question(Phase::Failed(
            "the copy's signature did not verify".into(),
        )))
        .unwrap();
        assert_eq!(
            failed.body,
            "the copy's signature did not verify. Nothing was changed."
        );
        assert_eq!(failed.actions[0], (TRY_AGAIN, MoveAction::Move));
        for quiet in [
            Phase::Waiting,
            Phase::Classifying,
            Phase::Pending,
            Phase::Declined,
            Phase::NotAsked("in-applications"),
        ] {
            assert_eq!(notice(&question(quiet.clone())), None, "{quiet:?}");
        }
    }

    #[test]
    fn move_to_applications_names_where_it_runs() {
        let mut asked = question(Phase::Pending);
        assert_eq!(asked.location(), None);
        asked.placement = Some(Placement {
            bundle: "/Volumes/Luxforge/Luxforge.app".into(),
            original: "/Volumes/Luxforge/Luxforge.app".into(),
            location: Location::DiskImage,
            cleanup: Cleanup::Eject("/dev/disk9s1".into()),
        });
        assert_eq!(asked.location(), Some("disk-image"));
        assert_eq!(cleanup_name(&asked.placement.unwrap().cleanup), "eject");
    }
}
