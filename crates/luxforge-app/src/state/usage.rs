//! Usage statistics as this desktop shows them: the one startup notice and the Settings sheet's
//! Privacy tab ([design](../../../../docs/design/usage-statistics.md#consent-and-first-launch)).
//!
//! Consent is the host's, not this desktop's. Whether the notice may be shown is decided by the
//! core's `usage.prompt.claim`, the switch reports the effective state `usage.read`, `usage.prompt.claim`
//! and `usage.set` answer, and every change goes through `usage.set`. What is held here is plain
//! data: where the startup notice is in its one life, the consent as last read (never its
//! identifiers, which this desktop does not keep) and the switch's write in flight. Both surfaces
//! exist only in an eligible launch, which the launch decides once.
use serde::Deserialize;

/// The startup notice's title.
pub(crate) const NOTICE_TITLE: &str = "Help improve Luxforge?";
/// The short disclosure the notice and the Privacy tab share. It says what the events are for and
/// what they never include; it never calls them anonymous or free of personal data, since they
/// carry persistent random identifiers.
pub(crate) const DISCLOSURE: &str = "Luxforge can send usage events to api.luxforge.app for \
     debugging and analytics. They never include your name, contact details, IP address, photos, \
     filenames or photo metadata.";
/// The same privacy disclosure, naming this launch's test destination when applicable.
fn disclosure(usage: &Usage) -> String {
    match usage
        .read
        .as_ref()
        .and_then(|read| read.destination.as_deref())
    {
        Some(destination) if destination != luxforge_core::usage::DESTINATION => {
            format!(
                "{} These reports are test data.",
                DISCLOSURE.replace("api.luxforge.app", destination)
            )
        }
        _ => DISCLOSURE.to_owned(),
    }
}

/// Where the notice says the choice can be changed.
pub(crate) const NOTICE_SETTINGS: &str = "You can change this in Settings \u{203a} Privacy.";
/// The notice's two neutral actions, in order. Neither is preselected.
pub(crate) const SHARE: &str = "Share statistics";
pub(crate) const DECLINE: &str = "No thanks";
/// The Privacy tab's switch.
pub(crate) const SWITCH: &str = "Share usage and performance statistics";
/// The Privacy tab's collapsed section.
pub(crate) const WHAT_IS_SENT: &str = "What is sent";

/// The identifiers a report carries, as the collapsed list names them: the only three uploaded.
pub(crate) const IDENTIFIERS: [(&str, &str); 3] = [
    (
        "Installation ID",
        "A random ID created when you turn sharing on and kept while it stays on, so this \
         installation's sessions can be compared. Turning sharing off deletes it; turning it on \
         again creates a new one.",
    ),
    (
        "Session ID",
        "A random ID for one launch, never stored, so one session's events can be followed in \
         order.",
    ),
    (
        "Image ID",
        "A code for a photo open in Develop, made with a key that never leaves this computer, so \
         one photo's events can be linked without identifying the file.",
    ),
];

/// The device fields a report carries ([design](../../../../docs/design/usage-statistics.md#privacy-device-context-and-the-event-registry)).
pub(crate) const DEVICE: [(&str, &str); 7] = [
    (
        "Release",
        "Luxforge's version and source revision, or a synthetic release for an unrevisioned developer build.",
    ),
    (
        "Operating system",
        "Its family and numeric version; on Linux, the distribution family and kernel version.",
    ),
    ("Architecture", "arm64, x86_64 or other."),
    (
        "Processor",
        "Its vendor, model family and physical and logical core counts.",
    ),
    ("Memory", "Installed memory, rounded up to a band."),
    (
        "Graphics",
        "Up to two adapters in use: vendor, model, memory band and numeric driver version.",
    ),
    (
        "Renderer",
        "Whether the GPU or the reference renderer draws, and why it fell back.",
    ),
];

/// The events a report can hold, by their registry name, as the collapsed list describes them.
/// Every event also carries its sequence number and time.
pub(crate) const EVENTS: [(&str, &str); 11] = [
    (
        "session.start",
        "A session started, at launch or when you turn sharing on.",
    ),
    (
        "image.open",
        "A photo became the open photo in Develop, with its image ID.",
    ),
    ("app.presented", "The window was shown or hidden."),
    (
        "app.idle",
        "No interaction for a minute, or interaction resumed.",
    ),
    ("workspace.enter", "You moved between Select and Develop."),
    (
        "interaction",
        "A completed operation, by feature: navigation, zoom, compare, history, versions, \
         presets, copy and paste, masks, catalog, Locate, preferences or export.",
    ),
    (
        "tool.commit",
        "An adjustment group an edit changed, with no setting name or value.",
    ),
    (
        "images.added",
        "How many photos an import added, and nothing about them.",
    ),
    (
        "operation.end",
        "How preparing a photo, a catalog scan, an export or Auto tone ended, and how long it took.",
    ),
    (
        "preview.settled",
        "How long a preview took and which renderer drew it.",
    ),
    (
        "process.sample",
        "Luxforge's processor, memory and graphics use, sampled periodically.",
    ),
];

/// The consent as `usage.read` reports it, and as the `state` in a claim's or a write's answer.
/// Only what this desktop shows is read: the installation and session IDs the answer may carry are
/// ignored, so they are never held, drawn or recorded here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct UsageRead {
    /// Whether the startup notice was claimed, or `None` when the record cannot be read.
    #[serde(default)]
    pub(crate) prompt_shown: Option<bool>,
    /// The stored choice, or `None` when the record cannot be read.
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
    /// Whether this process collects now.
    pub(crate) effective: bool,
    /// The disclosure version the core asks agreement to.
    pub(crate) disclosure_version: u32,
    /// Where an eligible launch sends: the production URL, hosted test tenant or loopback
    /// receiver. Recorded so evidence shows which.
    #[serde(default)]
    pub(crate) destination: Option<String>,
    /// Why this process does not collect, as one fixed reason.
    #[serde(default)]
    pub(crate) reason: Option<String>,
    /// The statistics worker's bounded counts and delivery state, as `usage.read` reports them;
    /// absent from a claim's or a write's state.
    #[serde(default)]
    pub(crate) diagnostics: Option<UsageDiagnostics>,
}

/// `usage.read`'s `diagnostics`: counts and a fixed delivery state, never an identifier or error
/// text.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct UsageDiagnostics {
    #[serde(default)]
    pub(crate) recorded: u64,
    #[serde(default)]
    pub(crate) dropped: u64,
    #[serde(default)]
    pub(crate) sent: u64,
    #[serde(default)]
    pub(crate) attempts: u64,
    #[serde(default)]
    pub(crate) accepted: u64,
    #[serde(default)]
    pub(crate) delivery: Option<serde_json::Value>,
}

/// An owner answer as the consent it reports: `usage.read`'s whole answer, or the `state` field of
/// a `usage.prompt.claim` or `usage.set` answer.
pub(crate) fn parse(answer: serde_json::Value) -> Result<UsageRead, String> {
    serde_json::from_value(answer).map_err(|error| format!("unreadable usage statistics: {error}"))
}

/// Where the one startup notice is. It moves forward only: one claim per launch, and once it has
/// been answered, refused or found not to be needed, nothing brings it back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Prompt {
    /// Not claimed yet: waiting for an eligible launch's workspace to be usable.
    #[default]
    Waiting,
    /// `usage.prompt.claim` is in flight.
    Claiming,
    /// The claim said to show it, and it waits for no native dialog, module consent notice or
    /// Settings sheet to be open.
    Pending,
    /// On screen.
    Shown,
    /// The person answered Share statistics.
    Shared,
    /// The person answered No thanks or Escape.
    Declined,
    /// The person chose in Settings › Privacy before the notice appeared, which answers it.
    Superseded,
    /// The claim said not to show it: already answered, ineligible, or another instance won.
    NotShown,
    /// The claim failed, so the notice is not shown; the core's record decides any later launch.
    Failed(String),
}

impl Prompt {
    /// The name a frame's state records.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Claiming => "claiming",
            Self::Pending => "pending",
            Self::Shown => "shown",
            Self::Shared => "shared",
            Self::Declined => "declined",
            Self::Superseded => "superseded",
            Self::NotShown => "not-shown",
            Self::Failed(_) => "failed",
        }
    }
}

/// Why the last `usage.set` failed, as the Privacy tab says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SaveFailure {
    /// The value that was asked for.
    pub(crate) enabled: bool,
    pub(crate) reason: String,
}

impl SaveFailure {
    /// The sentence the Privacy tab shows. A failed opt-out leaves this process off, but the
    /// stored choice, and the installation ID with it, as they were.
    pub(crate) fn text(&self) -> String {
        if self.enabled {
            format!("Could not turn on usage statistics: {}", self.reason)
        } else {
            format!(
                "Usage statistics are off for this session, but the stored choice did not change: {}",
                self.reason
            )
        }
    }
}

/// This desktop's usage statistics state.
#[derive(Clone, Debug, Default)]
pub(crate) struct Usage {
    /// This launch may show the notice and the Privacy tab: decided once, at launch, from the
    /// eligibility its host was started with.
    pub(crate) eligible: bool,
    pub(crate) prompt: Prompt,
    /// The consent as last read or answered, or `None` before the first answer.
    pub(crate) read: Option<UsageRead>,
    /// A `usage.read` is in flight.
    pub(crate) reading: bool,
    /// Why the last read failed, until one succeeds.
    pub(crate) read_error: Option<String>,
    /// The `usage.set` in flight: the value asked for. One write is out at a time.
    pub(crate) writing: Option<bool>,
    /// A newer change made while one was in flight; it is sent when that one lands.
    pub(crate) waiting: Option<bool>,
    /// Why the last write failed, until the next write succeeds.
    pub(crate) failure: Option<SaveFailure>,
    /// The Privacy tab's What is sent section is open.
    pub(crate) details_open: bool,
}

impl Usage {
    /// The notice may appear now: claimed, and nothing that delays it open.
    pub(crate) fn may_present(&self, delayed: bool) -> bool {
        self.prompt == Prompt::Pending && !delayed
    }

    /// The newest change asked for and not answered yet.
    fn outstanding(&self) -> Option<bool> {
        self.waiting.or(self.writing)
    }

    /// How many writes are in flight or waiting.
    pub(crate) fn writes_outstanding(&self) -> usize {
        usize::from(self.writing.is_some()) + usize::from(self.waiting.is_some())
    }
}

/// The startup notice as the workspace draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageNotice {
    pub(crate) title: &'static str,
    pub(crate) body: String,
    /// The two actions, in order: Share statistics, then No thanks.
    pub(crate) actions: [&'static str; 2],
}

/// The notice, while it is on screen.
pub(crate) fn notice(usage: &Usage) -> Option<UsageNotice> {
    (usage.eligible && usage.prompt == Prompt::Shown).then(|| UsageNotice {
        title: NOTICE_TITLE,
        body: format!("{} {NOTICE_SETTINGS}", disclosure(usage)),
        actions: [SHARE, DECLINE],
    })
}

/// The Privacy tab as the sheet draws it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PrivacyModel {
    /// The disclosure names this launch's destination.
    pub(crate) disclosure: String,
    /// The switch: the effective state, turned off at once by an opt-out and on only once one is
    /// saved.
    pub(crate) on: bool,
    /// A write is in flight, so the switch is held until it lands.
    pub(crate) saving: bool,
    /// The consent has not been read yet.
    pub(crate) loading: bool,
    /// What the tab says under the switch about why statistics are off, when that is not simply
    /// the person's choice.
    pub(crate) note: Option<String>,
    /// The failed read or write, as the tab says it.
    pub(crate) error: Option<String>,
    pub(crate) details_open: bool,
}

/// The Privacy tab for this state.
pub(crate) fn privacy(usage: &Usage) -> PrivacyModel {
    let read = usage.read.as_ref();
    let effective = read.is_some_and(|read| read.effective);
    // An opt-out shows at once, since this process stops collecting before it saves; an opt-in
    // shows only once `usage.set` has saved it.
    let on = match usage.outstanding() {
        Some(false) => false,
        _ => effective,
    };
    let note = read
        .filter(|read| !read.effective && usage.outstanding().is_none())
        .and_then(|read| read.reason.as_deref())
        .and_then(reason_note);
    let error = usage.failure.as_ref().map(SaveFailure::text).or_else(|| {
        usage
            .read_error
            .as_ref()
            .map(|reason| format!("Could not read the usage statistics setting: {reason}"))
    });
    PrivacyModel {
        disclosure: disclosure(usage),
        on,
        saving: usage.writing.is_some(),
        loading: read.is_none() && usage.read_error.is_none(),
        note,
        error,
        details_open: usage.details_open,
    }
}

/// What the tab says for a reason other than the person's own choice. An eligible launch never
/// reports the launch-mode, build or inactive-destination reasons, since without eligibility the
/// tab is not shown.
fn reason_note(reason: &str) -> Option<String> {
    match reason {
        "unreadable-configuration" => Some(
            "Statistics are off because the preferences file cannot be read or holds a record \
             this version cannot use. It is kept as it is."
                .into(),
        ),
        "changed-disclosure" => Some(
            "Statistics are off: what is sent has changed since you agreed. Turn this on to agree \
             to the description above."
                .into(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(enabled: bool, effective: bool, reason: Option<&str>) -> UsageRead {
        UsageRead {
            prompt_shown: Some(true),
            enabled: Some(enabled),
            effective,
            disclosure_version: 1,
            destination: None,
            reason: reason.map(str::to_owned),
            diagnostics: None,
        }
    }

    #[test]
    fn usage_both_disclosures_name_the_test_destination() {
        let destination = luxforge_core::usage::TEST_DESTINATION;
        let mut read = read(false, false, Some("choice"));
        read.destination = Some(destination.into());
        let usage = Usage {
            eligible: true,
            prompt: Prompt::Shown,
            read: Some(read),
            ..Usage::default()
        };
        let notice = notice(&usage).unwrap();
        let privacy = privacy(&usage);
        assert!(notice.body.contains(destination));
        assert!(notice.body.contains("These reports are test data."));
        assert!(notice.body.starts_with(&privacy.disclosure));
        assert!(privacy.disclosure.contains("They never include your name"));
    }

    #[test]
    fn usage_the_notice_says_exactly_the_designs_words_and_never_claims_anonymity() {
        let usage = Usage {
            eligible: true,
            prompt: Prompt::Shown,
            ..Usage::default()
        };
        let notice = notice(&usage).unwrap();
        assert_eq!(notice.title, "Help improve Luxforge?");
        assert_eq!(
            notice.body,
            "Luxforge can send usage events to api.luxforge.app for debugging and analytics. They \
             never include your name, contact details, IP address, photos, filenames or photo \
             metadata. You can change this in Settings \u{203a} Privacy."
        );
        assert_eq!(notice.actions, ["Share statistics", "No thanks"]);
        let every_word = [DISCLOSURE, NOTICE_SETTINGS, SWITCH]
            .into_iter()
            .chain(
                IDENTIFIERS
                    .iter()
                    .chain(&DEVICE)
                    .chain(&EVENTS)
                    .flat_map(|(a, b)| [*a, *b]),
            )
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        for claim in ["anonymous", "anonymised", "anonymized", "no personal data"] {
            assert!(!every_word.contains(claim), "{claim}");
        }
    }

    #[test]
    fn usage_the_notice_shows_only_while_shown_in_an_eligible_launch() {
        for prompt in [
            Prompt::Waiting,
            Prompt::Claiming,
            Prompt::Pending,
            Prompt::Shared,
            Prompt::Declined,
            Prompt::Superseded,
            Prompt::NotShown,
            Prompt::Failed("x".into()),
        ] {
            let usage = Usage {
                eligible: true,
                prompt,
                ..Usage::default()
            };
            assert!(notice(&usage).is_none(), "{:?}", usage.prompt);
        }
        let ineligible = Usage {
            prompt: Prompt::Shown,
            ..Usage::default()
        };
        assert!(notice(&ineligible).is_none());
    }

    #[test]
    fn usage_a_pending_notice_waits_while_something_delays_it() {
        let usage = Usage {
            eligible: true,
            prompt: Prompt::Pending,
            ..Usage::default()
        };
        assert!(!usage.may_present(true));
        assert!(usage.may_present(false));
        let claiming = Usage {
            prompt: Prompt::Claiming,
            ..usage
        };
        assert!(!claiming.may_present(false));
    }

    #[test]
    fn usage_the_switch_turns_off_at_once_and_on_only_once_saved() {
        let mut usage = Usage {
            eligible: true,
            read: Some(read(false, false, Some("choice"))),
            ..Usage::default()
        };
        assert_eq!(
            privacy(&usage).note,
            None,
            "the person's own choice needs no note"
        );
        usage.writing = Some(true);
        let model = privacy(&usage);
        assert!(!model.on && model.saving, "an opt-in waits for its save");
        usage.writing = None;
        usage.read = Some(read(true, true, None));
        assert!(privacy(&usage).on);
        usage.writing = Some(false);
        let model = privacy(&usage);
        assert!(!model.on && model.saving, "an opt-out shows at once");
    }

    #[test]
    fn usage_a_failed_opt_out_says_the_stored_choice_did_not_change() {
        let usage = Usage {
            eligible: true,
            // The core keeps this process off and reports the stored choice it could not change.
            read: Some(read(true, false, Some("choice"))),
            failure: Some(SaveFailure {
                enabled: false,
                reason: "the preferences file is read-only".into(),
            }),
            ..Usage::default()
        };
        let model = privacy(&usage);
        assert!(!model.on);
        assert_eq!(
            model.error.as_deref(),
            Some(
                "Usage statistics are off for this session, but the stored choice did not \
                 change: the preferences file is read-only"
            )
        );
        let failed_on = SaveFailure {
            enabled: true,
            reason: "denied".into(),
        };
        assert_eq!(
            failed_on.text(),
            "Could not turn on usage statistics: denied"
        );
    }

    #[test]
    fn usage_an_unreadable_record_or_a_changed_disclosure_is_said_under_the_switch() {
        for (reason, starts) in [
            ("unreadable-configuration", "Statistics are off because"),
            ("changed-disclosure", "Statistics are off: what is sent"),
        ] {
            let usage = Usage {
                eligible: true,
                read: Some(read(true, false, Some(reason))),
                ..Usage::default()
            };
            assert!(
                privacy(&usage).note.unwrap().starts_with(starts),
                "{reason}"
            );
        }
        let unread = privacy(&Usage::default());
        assert!(unread.loading && !unread.on);
    }

    #[test]
    fn usage_a_read_keeps_no_identifier() {
        let parsed = parse(json!({
            "prompt_shown": true, "enabled": true, "effective": true, "disclosure_version": 1,
            "destination": "http://127.0.0.1:9/", "installation_id": "5f0c1e9e-0000-4000-8000-000000000000",
            "session_id": "0b7d2a10-0000-4000-8000-000000000000", "limits": {}, "diagnostics": {}
        }))
        .unwrap();
        assert_eq!(
            parsed,
            UsageRead {
                destination: Some("http://127.0.0.1:9/".into()),
                diagnostics: Some(UsageDiagnostics::default()),
                ..read(true, true, None)
            }
        );
        let debug = format!("{parsed:?}");
        assert!(!debug.contains("5f0c1e9e") && !debug.contains("0b7d2a10"));
        let unreadable = parse(json!({
            "prompt_shown": null, "enabled": null, "effective": false, "disclosure_version": 1,
            "reason": "unreadable-configuration"
        }))
        .unwrap();
        assert_eq!(unreadable.enabled, None);
    }

    #[test]
    fn usage_what_is_sent_lists_exactly_the_registrys_events() {
        let listed: Vec<&str> = EVENTS.iter().map(|(name, _)| *name).collect();
        let mut registry: Vec<&str> = luxforge_core::usage::EventName::ALL
            .iter()
            .map(|name| name.as_str())
            .collect();
        let mut sorted = listed.clone();
        sorted.sort_unstable();
        registry.sort_unstable();
        assert_eq!(sorted, registry);
    }
}
