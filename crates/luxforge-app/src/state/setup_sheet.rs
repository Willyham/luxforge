//! The setup sheet over the workspace, when a setup install the desktop asked for was refused for
//! want of consent ([board](../../../../docs/design/ai-tools-workspace/board.html), board 4). It is
//! modal: the person answers it before the workspace takes another press or key. Download grants
//! every scope the refusal named and asks for the setup again; Not now denies each
//! ([`crate::state::capabilities::SetupConsent::requests`]).
//!
//! The tool's words and pictures are the setup's introduction; the facts are summed over the parts
//! still missing: the size, every origin and the licences, any limit a licence puts on the
//! person's results, and the setup's memory minimum against what this machine has. The includes
//! list names every part with its purpose and size, an installed one marked and not counted. A
//! machine below the minimum is told so and offered no Download.
use crate::state::{
    Inputs,
    capabilities::{cost, human_bytes, sentence, unique_hosts},
    fill_setup::this_machine,
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
    /// Every part the setup holds: its name, purpose and size, or that it is installed.
    pub(crate) includes: Vec<SetupInclude>,
    /// Lines under the facts, each with whether it warns.
    pub(crate) notes: Vec<(String, bool)>,
    pub(crate) promise: String,
    /// The primary action's label; `None` when this machine cannot run what it installs.
    pub(crate) allow: Option<String>,
    /// The other action's label: Not now, or Close when nothing can be allowed.
    pub(crate) dismiss: String,
    /// The setup it installs.
    pub(crate) setup: String,
    /// The requests Download sends, as Copy as JSON request copies them.
    pub(crate) allow_requests: Vec<Value>,
    /// The requests Not now (or Close) sends.
    pub(crate) dismiss_requests: Vec<Value>,
}

impl SetupSheet {
    /// What a captured frame records of the sheet: its words, its pictures, its facts, includes
    /// and notes, and its two actions with the requests each sends.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "setup": self.setup,
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
            "includes": self.includes.iter().map(|part| json!({
                "name": part.name,
                "purpose": part.purpose,
                "size": part.size,
                "installed": part.installed,
            })).collect::<Vec<_>>(),
            "notes": self.notes.iter().map(|(note, _)| note).collect::<Vec<_>>(),
            "promise": self.promise,
            "allow": self.allow,
            "dismiss": self.dismiss,
            "requests": {
                "allow": self.allow.as_ref().map(|_| &self.allow_requests),
                "dismiss": self.dismiss_requests,
            },
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

/// One part the setup holds, as the includes list names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetupInclude {
    pub(crate) name: String,
    pub(crate) purpose: String,
    /// `2.5 GB`, or `installed ✓` for a part already installed, which is not counted.
    pub(crate) size: String,
    pub(crate) installed: bool,
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

/// The sheet, while a setup install waits for the person's answer.
pub(crate) fn derive(inputs: &Inputs<'_>) -> Option<SetupSheet> {
    let consent = inputs.capabilities.setup_consent.as_ref()?;
    let disclosure = &consent.disclosure;
    let introduction = &disclosure.introduction;
    let machine = this_machine();
    let size = human_bytes(disclosure.bytes);
    let missing_parts = disclosure
        .parts
        .iter()
        .filter(|part| !part.installed)
        .count();
    let mut facts = vec![SetupFact {
        kind: FactKind::Size,
        headline: format!("{size} download"),
        detail: match missing_parts {
            0 | 1 => "Once, then it works offline".into(),
            2 => "Two parts, once, then offline".into(),
            count => format!("{count} parts, once, then offline"),
        },
    }];
    let hosts = unique_hosts(disclosure.origins.iter());
    if !hosts.is_empty() {
        // Each download's cost, each once, before the licences.
        let costs = disclosure
            .downloads
            .iter()
            .map(|download| cost(download.cost))
            .fold(Vec::new(), |mut costs, cost| {
                if !costs.contains(&cost) {
                    costs.push(cost);
                }
                costs
            });
        facts.push(SetupFact {
            kind: FactKind::Origin,
            headline: format!("From {}", hosts.join(" and ")),
            detail: costs
                .into_iter()
                .map(str::to_owned)
                .chain(disclosure.licenses.iter().cloned())
                .collect::<Vec<_>>()
                .join(" \u{b7} "),
        });
    }
    // Whether this machine fits is the host's answer, carried in the disclosure with the memory it
    // was measured against.
    let met = disclosure.fits;
    let unmet = met == Some(false);
    if let Some(needed) = disclosure.minimum_memory {
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
    let includes = disclosure
        .parts
        .iter()
        .map(|part| SetupInclude {
            name: part.name.clone(),
            purpose: part.purpose.clone(),
            size: if part.installed {
                "installed \u{2713}".into()
            } else {
                human_bytes(part.bytes)
            },
            installed: part.installed,
        })
        .collect();
    let mut notes: Vec<(String, bool)> = disclosure
        .use_restrictions
        .iter()
        .map(|restriction| (sentence(restriction), true))
        .collect();
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
    let placeholder = || json!({"request_id": "<per press>", "actor": crate::state::ACTOR});
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
                    IntroductionPicture::Describe | IntroductionPicture::Hover => Vec::new(),
                    IntroductionPicture::Selected => {
                        vec![hint("\u{21e7}", "adds"), hint("\u{2325}", "takes away")]
                    }
                },
            })
            .collect(),
        facts_label: (!introduction.steps.is_empty()).then(|| "Before it can start".into()),
        facts,
        includes,
        notes,
        promise: introduction.promise.clone(),
        allow: (!unmet).then(|| format!("Download {size}")),
        dismiss: if unmet { "Close" } else { "Not now" }.into(),
        setup: consent.setup.clone(),
        allow_requests: consent.requests(true, placeholder),
        dismiss_requests: consent.requests(false, placeholder),
    })
}
