//! The setup sheet over the workspace, when a download the desktop started was refused for want of
//! consent and its capability introduces the feature it sets up
//! (`docs/design/ai-editing/setup-board.html`). It is modal: the person answers it before the
//! workspace takes another press or key, with the same Allow and Don't allow the consent notice
//! sends ([`crate::state::capabilities::OpenConsent`]).
//!
//! The feature's words and pictures are the module's introduction; the facts are the disclosure's,
//! as the notice states them: the size, where it comes from, its cost and licence, any limit the
//! licence puts on the person's results, and what it needs of this machine against what this
//! machine has. A machine below the model's minimum is told so and offered no Download.
use crate::state::{
    Inputs,
    capabilities::{cost, hosts, human_bytes, introduced, sentence},
    host_commands::glyphs,
    models::memory_text,
};
use luxforge_core::Chord;
// The pictures the module names, which the view draws through this model.
pub(crate) use luxforge_core::capabilities::descriptor::IntroductionPicture;
use serde_json::{Value, json};

/// The sheet as plain data.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetupSheet {
    pub(crate) eyebrow: String,
    pub(crate) title: String,
    pub(crate) lead: String,
    /// One picture instead of the steps.
    pub(crate) hero: Option<IntroductionPicture>,
    pub(crate) steps: Vec<SetupStep>,
    /// The facts' label while steps come first.
    pub(crate) facts_label: Option<String>,
    pub(crate) facts: Vec<SetupFact>,
    /// Lines under the facts, each with whether it warns.
    pub(crate) notes: Vec<(String, bool)>,
    pub(crate) promise: String,
    /// The primary action's label; `None` when this machine cannot run what it installs.
    pub(crate) allow: Option<String>,
    /// The other action's label: Not now, or Close when nothing can be allowed.
    pub(crate) dismiss: String,
}

impl SetupSheet {
    /// What a captured frame records of the sheet: its words, its pictures, its facts and notes,
    /// and its two actions.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "title": self.title,
            "eyebrow": self.eyebrow,
            "hero": self.hero,
            "steps": self.steps.iter().map(|step| json!({
                "title": step.title,
                "picture": step.picture,
                "hints": step.hints,
            })).collect::<Vec<_>>(),
            "facts": self.facts.iter().map(|fact| json!({
                "headline": fact.headline,
                "detail": fact.detail,
                "unmet": fact.kind.unmet(),
            })).collect::<Vec<_>>(),
            "notes": self.notes.iter().map(|(note, _)| note).collect::<Vec<_>>(),
            "promise": self.promise,
            "allow": self.allow,
            "dismiss": self.dismiss,
        })
    }
}

/// One step of how the feature is used, with the keys that do it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetupStep {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) picture: IntroductionPicture,
    /// Each key's label and what it does.
    pub(crate) hints: Vec<(String, String)>,
}

/// One fact of the download.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetupFact {
    pub(crate) kind: FactKind,
    pub(crate) headline: String,
    pub(crate) detail: String,
}

/// What a fact states, which sets its glyph and tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FactKind {
    /// The download's size.
    Size,
    /// Where it comes from, its cost and licence.
    Origin,
    /// The memory it needs: met by this machine, not met, or against a machine whose memory is
    /// not known.
    Memory(Option<bool>),
}

impl FactKind {
    /// Whether the fact states a requirement this machine does not meet.
    pub(crate) fn unmet(self) -> bool {
        self == Self::Memory(Some(false))
    }
}

/// The sheet, while a download that introduces its feature waits for the person's answer.
pub(crate) fn derive(inputs: &Inputs<'_>) -> Option<SetupSheet> {
    let open = inputs.capabilities.consent.as_ref()?;
    let consent = &open.consent;
    if !introduced(consent) {
        return None;
    }
    let disclosure = &consent.disclosure;
    let introduction = disclosure.introduction.as_ref()?;
    let machine = if cfg!(target_os = "macos") {
        "Mac"
    } else {
        "computer"
    };
    let size = disclosure.bytes.map(human_bytes);
    let mut facts = Vec::new();
    if let Some(size) = &size {
        facts.push(SetupFact {
            kind: FactKind::Size,
            headline: format!("{size} download"),
            detail: if disclosure.requirements.is_some() {
                "Once, then it works offline".into()
            } else {
                "One-time download".into()
            },
        });
    }
    let hosts = hosts(disclosure);
    if !hosts.is_empty() {
        facts.push(SetupFact {
            kind: FactKind::Origin,
            headline: format!("From {}", hosts.join(" and ")),
            detail: [Some(cost(disclosure.cost)), disclosure.license.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" \u{b7} "),
        });
    }
    let needed = disclosure
        .requirements
        .as_ref()
        .and_then(|requirements| requirements.min_memory_bytes);
    // Whether this machine fits is the host's answer, carried in the disclosure with the memory it
    // was measured against.
    let met = disclosure.fits;
    let unmet = met == Some(false);
    if let Some(needed) = needed {
        facts.push(SetupFact {
            kind: FactKind::Memory(met),
            headline: format!("Needs {} of memory", memory_text(needed)),
            detail: match disclosure.machine_memory_bytes {
                Some(has) if unmet => format!("This {machine} has {}", memory_text(has)),
                Some(has) => format!("This {machine} has {} \u{2713}", memory_text(has)),
                None => format!("This {machine}'s memory is unknown"),
            },
        });
    }
    let mut notes = Vec::new();
    if let Some(restriction) = &disclosure.use_restriction {
        notes.push((sentence(restriction), true));
    }
    if unmet {
        notes.push((format!("It can't run on this {machine}."), true));
    } else {
        if consent.denied {
            notes.push((
                "You chose Not now before. Nothing has been downloaded.".into(),
                false,
            ));
        }
        notes.push((
            "You can remove it at any time in Settings \u{203a} Models.".into(),
            false,
        ));
    }
    let hint = |key: &str, action: &str| (key.to_owned(), action.to_owned());
    let undo = glyphs(Chord::command('Z'));
    Some(SetupSheet {
        eyebrow: introduction.eyebrow.clone(),
        title: introduction.title.clone(),
        lead: introduction.lead.clone(),
        hero: introduction.hero,
        steps: introduction
            .steps
            .iter()
            .map(|step| SetupStep {
                title: step.title.clone(),
                body: step.body.clone(),
                picture: step.picture,
                hints: match step.picture {
                    IntroductionPicture::PaintOver => vec![hint("[ ]", "brush size")],
                    IntroductionPicture::Candidates => vec![hint("Esc", "cancels")],
                    IntroductionPicture::History => vec![hint(&undo, "undoes it")],
                    IntroductionPicture::Describe => Vec::new(),
                },
            })
            .collect(),
        facts_label: (!introduction.steps.is_empty()).then(|| "Before it can start".into()),
        facts,
        notes,
        promise: introduction.promise.clone(),
        allow: (!unmet).then(|| match &size {
            Some(size) => format!("Download {size}"),
            None => "Download".into(),
        }),
        dismiss: if unmet { "Close" } else { "Not now" }.into(),
    })
}
