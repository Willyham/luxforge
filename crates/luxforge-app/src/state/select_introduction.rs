//! The Select introduction as plain data: who it is shown to, where its first showing is in its
//! one life, and the sheet it draws
//! ([design](../../../../docs/design/select-introduction.md)).
//!
//! The sheet says once how Select and the catalog work: browse photographs where they are, pick
//! the few worth developing, and only those join the catalog. It is shown the first time Select is
//! shown in a launch a person uses while the `select_introduction_seen` preference is off, and
//! the title bar's ? shows it again at any time. Whether it is on screen is this desktop's own view
//! state, like which workspace is shown.

/// The sheet's capitalised label over its title.
pub(crate) const EYEBROW: &str = "HOW LUXFORGE WORKS";
pub(crate) const TITLE: &str = "Select, then develop";
/// The lead, in runs, each with whether it is one of the two emphasised phrases the view sets
/// apart from the rest.
pub(crate) const LEAD: [(&str, bool); 5] = [
    (
        "Luxforge doesn\u{2019}t import. You browse your photographs ",
        false,
    ),
    ("where they already are", true),
    (", pick the few worth developing, and ", false),
    ("only those join your catalog", true),
    (".", false),
];
/// The comparison's heading.
pub(crate) const COMPARE: &str = "COMING FROM LIGHTROOM?";
/// The footer's promise.
pub(crate) const PROMISE: &str = "Your files are never moved, copied or changed.";
pub(crate) const ADD_FOLDER: &str = "Add a folder\u{2026}";
pub(crate) const START: &str = "Start browsing";
/// The title bar's ? button, which shows the sheet again.
pub(crate) const REOPEN_TOOLTIP: &str = "How Select works";

/// The sheet's width at its size, in the layout's points.
pub(crate) const WIDTH: f32 = 760.0;
/// The least room kept beside the sheet on each side.
pub(crate) const MARGIN: f32 = 32.0;

/// Develop N's chord, as this platform draws it.
pub(crate) const DEVELOP_KEY: &str = if cfg!(target_os = "macos") {
    "\u{2318}\u{21a9}"
} else {
    "Ctrl+Return"
};

/// One of the three steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) name: &'static str,
    pub(crate) body: &'static str,
    /// Where it is done, in words, when the step says so.
    pub(crate) place: Option<&'static str>,
    /// The keys it names, each with what it does, in order.
    pub(crate) keys: &'static [(&'static str, &'static str)],
}

/// The steps, left to right. Their keys are the loupe's and Develop N's, which the keymap's tests
/// hold them to.
pub(crate) const STEPS: [Step; 3] = [
    Step {
        name: "Browse",
        body: "Open a camera card or any folder. Luxforge groups it by day and camera, and keeps \
               each burst, bracket and panorama together as one moment.",
        place: Some("Cards and On disk, on the left"),
        keys: &[],
    },
    Step {
        name: "Pick",
        body: "Step through each moment, check focus at 100%, and pick the frames worth \
               developing. Picks are kept as you go, even across a restart.",
        place: None,
        keys: &[
            ("Space", "look"),
            ("Z", "100%"),
            ("P", "pick"),
            ("\u{2193}", "next moment"),
        ],
    },
    Step {
        name: "Develop",
        body: "Develop brings your picks into the catalog, filed by year, place and day, and opens \
               them for editing.",
        place: Some("Develop N, top right"),
        keys: &[(DEVELOP_KEY, "develop")],
    },
];

/// One side of the comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Approach {
    pub(crate) name: &'static str,
    pub(crate) points: [&'static str; 3],
    /// Luxforge's own, drawn in the accent.
    pub(crate) ours: bool,
}

/// Lightroom's import, then Luxforge's select.
pub(crate) const APPROACHES: [Approach; 2] = [
    Approach {
        name: "Import, then cull",
        points: [
            "Every frame you shoot goes into the catalog",
            "Flags and stars are how you find the keepers",
            "The catalog, previews and backups grow with every shutter press",
        ],
        ours: false,
    },
    Approach {
        name: "Select, then develop",
        points: [
            "Choosing is its own fast step, one moment at a time",
            "The catalog holds only what you develop: 20 picks from a 1,000-frame trip add 20",
            "The rest stay on disk, organized again whenever you come back",
        ],
        ours: true,
    },
];

/// Whether this launch shows the sheet by itself the first time Select is shown, decided once
/// its arguments are read: an evidence run only when it is given
/// `--evidence-select-introduction`, so it can be captured, and otherwise a launch with its window
/// shown. Hidden, background and test launches never do. The title bar's ? shows it in any launch.
pub(crate) fn introduces(hidden: bool, evidence: bool, flag: bool) -> bool {
    if evidence { flag } else { !hidden }
}

/// Where the first showing is. Once closed or found not to be needed, only the ? shows the sheet
/// again in this launch; a first showing that leaving Select took down waits again, as it was
/// never closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum First {
    /// Waiting for Select to be shown with nothing that delays the sheet open.
    Waiting,
    /// On screen.
    Shown,
    /// Closed, and how.
    Closed(Closing),
    /// Not shown, and why: `launch` for a launch that never shows it, `seen` when the preference
    /// says it was read.
    NotShown(&'static str),
}

impl First {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Shown => "shown",
            Self::Closed(_) => "closed",
            Self::NotShown(_) => "not-shown",
        }
    }
}

/// How the sheet was closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Closing {
    /// Start browsing.
    Start,
    /// Return.
    Return,
    /// Escape.
    Escape,
    /// The ✕.
    Close,
    /// Add a folder…, which opens the folder dialog.
    AddFolder,
}

impl Closing {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Return => "return",
            Self::Escape => "escape",
            Self::Close => "close",
            Self::AddFolder => "add-folder",
        }
    }
}

/// The desktop's Select introduction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Introduction {
    pub(crate) first: First,
    /// The ? showed it again; closing it then stores nothing.
    pub(crate) reopened: bool,
}

impl Introduction {
    /// The introduction of a launch that shows it by itself, or not ([`introduces`]).
    pub(crate) fn new(introduces: bool) -> Self {
        Self {
            first: if introduces {
                First::Waiting
            } else {
                First::NotShown("launch")
            },
            reopened: false,
        }
    }

    /// The sheet is on screen.
    pub(crate) fn open(&self) -> bool {
        self.first == First::Shown || self.reopened
    }

    /// The first showing is still to come while Select is shown: the startup notices wait for it,
    /// so a first launch shows one thing at a time.
    pub(crate) fn holds_notices(&self, select_shown: bool) -> bool {
        self.open() || (select_shown && self.first == First::Waiting)
    }
}

/// The sheet as the Select screen draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IntroductionModel {
    pub(crate) eyebrow: &'static str,
    pub(crate) title: &'static str,
    pub(crate) lead: [(&'static str, bool); 5],
    pub(crate) steps: [Step; 3],
    pub(crate) compare: &'static str,
    pub(crate) approaches: [Approach; 2],
    pub(crate) promise: &'static str,
    /// Add a folder… is offered: nothing is indexed and no card is mounted.
    pub(crate) add_folder: bool,
    /// The window is narrower than the sheet and its margins, so the steps and the comparison's
    /// two sides are drawn one under the other, and the sheet scrolls.
    pub(crate) stacked: bool,
    pub(crate) start: &'static str,
}

impl IntroductionModel {
    /// The sheet's buttons, by label, in order.
    pub(crate) fn buttons(&self) -> Vec<&'static str> {
        self.add_folder
            .then_some(ADD_FOLDER)
            .into_iter()
            .chain([self.start])
            .collect()
    }

    /// Every word the sheet draws, in reading order, for evidence and for the tests that hold the
    /// copy to the catalog's vocabulary.
    pub(crate) fn words(&self) -> Vec<&'static str> {
        let mut words = vec![self.eyebrow, self.title];
        words.extend(self.lead.iter().map(|(run, _)| *run));
        for step in &self.steps {
            words.extend([step.name, step.body]);
            words.extend(step.place);
            words.extend(step.keys.iter().flat_map(|(key, action)| [*key, *action]));
        }
        words.push(self.compare);
        for approach in &self.approaches {
            words.push(approach.name);
            words.extend(approach.points);
        }
        words.push(self.promise);
        words.extend(self.buttons());
        words
    }
}

/// The sheet, while it is on screen, in a window `width` points wide. `nothing_indexed` is that no
/// folder is indexed and no card is mounted, when Add a folder… is the obvious next step.
pub(crate) fn model(
    introduction: &Introduction,
    nothing_indexed: bool,
    width: f32,
) -> Option<IntroductionModel> {
    introduction.open().then_some(IntroductionModel {
        eyebrow: EYEBROW,
        title: TITLE,
        lead: LEAD,
        steps: STEPS,
        compare: COMPARE,
        approaches: APPROACHES,
        promise: PROMISE,
        add_folder: nothing_indexed,
        stacked: width < WIDTH + 2.0 * MARGIN,
        start: START,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_shown_window_or_an_evidence_run_that_asks_introduces_select() {
        assert!(introduces(false, false, false));
        assert!(!introduces(true, false, false));
        assert!(!introduces(true, true, false));
        assert!(!introduces(false, true, false));
        assert!(introduces(true, true, true));
        assert_eq!(Introduction::new(false).first, First::NotShown("launch"));
        assert_eq!(Introduction::new(true).first, First::Waiting);
    }

    #[test]
    fn the_notices_wait_while_the_sheet_is_up_or_due_in_select() {
        let waiting = Introduction::new(true);
        assert!(waiting.holds_notices(true));
        assert!(
            !waiting.holds_notices(false),
            "Develop's notices do not wait for a sheet Select has not shown"
        );
        let closed = Introduction {
            first: First::Closed(Closing::Start),
            ..waiting
        };
        assert!(!closed.holds_notices(true));
        let reopened = Introduction {
            reopened: true,
            ..closed
        };
        assert!(reopened.open() && reopened.holds_notices(true));
    }

    #[test]
    fn add_a_folder_is_offered_only_while_nothing_is_indexed() {
        let shown = Introduction {
            first: First::Shown,
            ..Introduction::new(true)
        };
        assert_eq!(
            model(&shown, true, 1440.0).unwrap().buttons(),
            ["Add a folder\u{2026}", "Start browsing"]
        );
        assert_eq!(
            model(&shown, false, 1440.0).unwrap().buttons(),
            ["Start browsing"]
        );
        assert!(model(&Introduction::new(true), true, 1440.0).is_none());
        assert!(!model(&shown, true, 824.0).unwrap().stacked);
        assert!(
            model(&shown, true, 823.0).unwrap().stacked,
            "a narrow window stacks the steps"
        );
    }

    /// The sheet speaks the catalog's vocabulary: Luxforge has no import, no library and no
    /// shoot, so the words never offer them, apart from naming Lightroom's import to contrast it.
    #[test]
    fn the_words_follow_the_catalogs_vocabulary() {
        let shown = Introduction {
            first: First::Shown,
            ..Introduction::new(true)
        };
        let model = model(&shown, true, 1440.0).unwrap();
        let ours: String = model
            .words()
            .into_iter()
            .filter(|word| !APPROACHES[0].points.contains(word) && *word != APPROACHES[0].name)
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        assert!(ours.contains("doesn\u{2019}t import"));
        for word in ["imported", "importing", "library", "libraries", "a shoot"] {
            assert!(!ours.contains(word), "{word}");
        }
        assert_eq!(
            ours.matches("import").count(),
            1,
            "only to say there is none"
        );
    }
}
