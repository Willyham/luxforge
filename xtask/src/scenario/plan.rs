//! A launch's plan: every frame the launch captures, in capture order, each paired with the script
//! step that produces it and what that frame must show. The script a launch runs and the number of
//! frames it must capture are both derived from the plan, so a step cannot be added without its
//! frame or its expectations, and a check names the step it reads rather than a position that a
//! later insertion would move.
//!
//! [`Plan::check`] is what every launch's evidence goes through before the scenario's own checks:
//! the invariants every frame keeps — the run's lifecycle and identity, each frame's run identity,
//! provenance and state file, the recorded step against the scripted one, `sent` unless the plan
//! expects that step refused, and input errors exactly when the plan expects a refusal — and then
//! each step's [`Expect`]. What `Expect` cannot say, mostly what the pixels show, the scenario checks
//! itself over the [`Checked`] launch, reading each frame by its step's name.
use super::{Frame, preamble};
use crate::*;
use lightwell_evidence as script;

/// The slider draft a frame's client holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Draft {
    /// No draft at all.
    None,
    /// A draft of `action` holding exactly `fields`, conflicted or not.
    Open {
        action: String,
        fields: Value,
        conflicted: bool,
    },
}

/// What one step's frame must show, beyond the invariants every frame keeps. A field left at its
/// default is not checked.
#[derive(Clone, Debug, Default)]
pub struct Expect {
    /// How many revisions the committed stack moved on from the frame before: `0` commits nothing
    /// (the same revision and the same current entry), `n` makes `n` new entries.
    pub commits: Option<u64>,
    /// The current history entry's label.
    pub label: Option<String>,
    /// The stack's first layer of each effect: its stored payload, or `None` for no such layer.
    pub layers: Vec<(String, Option<Value>)>,
    /// What generated fields show: the action, the parameter and the text.
    pub fields: Vec<(String, String, String)>,
    /// Effects whose layer still has the identity it had at a named earlier step.
    pub same_layer: Vec<(String, String)>,
    /// The slider draft.
    pub draft: Option<Draft>,
    /// Sections expanded (`true`) or collapsed.
    pub expanded: Vec<(String, bool)>,
    /// The refusal the step ends in: a scripted step recorded `failed` with a reason that leads with
    /// this, or an open whose outcome is this error code.
    pub refused: Option<String>,
}

impl Expect {
    fn record(&self) -> Value {
        let mut record = serde_json::Map::new();
        if let Some(commits) = self.commits {
            record.insert("commits".into(), json!(commits));
        }
        if let Some(label) = &self.label {
            record.insert("label".into(), json!(label));
        }
        if !self.layers.is_empty() {
            record.insert(
                "layers".into(),
                Value::Object(
                    self.layers
                        .iter()
                        .map(|(effect, payload)| (effect.clone(), json!(payload)))
                        .collect(),
                ),
            );
        }
        if !self.fields.is_empty() {
            record.insert(
                "fields".into(),
                Value::Object(
                    self.fields
                        .iter()
                        .map(|(action, parameter, text)| {
                            (format!("{action}.{parameter}"), json!(text))
                        })
                        .collect(),
                ),
            );
        }
        if !self.same_layer.is_empty() {
            record.insert(
                "same_layer".into(),
                Value::Object(
                    self.same_layer
                        .iter()
                        .map(|(effect, step)| (effect.clone(), json!(step)))
                        .collect(),
                ),
            );
        }
        match &self.draft {
            Some(Draft::None) => {
                record.insert("draft".into(), Value::Null);
            }
            Some(Draft::Open {
                action,
                fields,
                conflicted,
            }) => {
                record.insert(
                    "draft".into(),
                    json!({"action":action,"fields":fields,"conflicted":conflicted}),
                );
            }
            None => {}
        }
        if !self.expanded.is_empty() {
            record.insert(
                "expanded".into(),
                Value::Object(
                    self.expanded
                        .iter()
                        .map(|(module, open)| (module.clone(), json!(open)))
                        .collect(),
                ),
            );
        }
        if let Some(reason) = &self.refused {
            record.insert("refused".into(), json!(reason));
        }
        Value::Object(record)
    }
}

/// One frame of a launch: the script step that produces it — none for a frame an open captures —
/// and what it must show. The step is the shared evidence script's own type, so the script a
/// launch writes is the one the editor parses.
#[derive(Clone, Debug)]
pub struct Step {
    name: String,
    script: Option<script::Step>,
    expect: Expect,
}

impl Step {
    fn with(name: impl Into<String>, script: Option<script::Step>) -> Self {
        Self {
            name: name.into(),
            script,
            expect: Expect::default(),
        }
    }

    /// A frame captured for an open's outcome, before any script step runs.
    pub fn opened(name: impl Into<String>) -> Self {
        Self::with(name, None)
    }

    /// A frame captured for one script step, built from one of the shared script's steps or a
    /// payload that converts into one.
    pub fn new(name: impl Into<String>, script: impl Into<script::Step>) -> Self {
        Self::with(name, Some(script.into()))
    }

    #[cfg(test)]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The step as the script writes it.
    #[cfg(test)]
    pub fn script(&self) -> Option<Value> {
        self.script.as_ref().map(script::Step::to_value)
    }

    #[cfg(test)]
    pub fn expect(&self) -> &Expect {
        &self.expect
    }

    /// The step as the kept script holds it and the editor records it: a secret redacted.
    fn kept(&self) -> Option<Value> {
        self.script.as_ref().map(script::Step::kept)
    }

    /// `n` revisions committed since the frame before: `0` for none.
    pub fn commits(mut self, n: u64) -> Self {
        self.expect.commits = Some(n);
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.expect.label = Some(label.into());
        self
    }

    /// The stack's first `effect` layer stores exactly `payload`.
    pub fn payload(mut self, effect: &str, payload: Value) -> Self {
        self.expect.layers.push((effect.into(), Some(payload)));
        self
    }

    /// The stack holds no `effect` layer.
    pub fn no_layer(mut self, effect: &str) -> Self {
        self.expect.layers.push((effect.into(), None));
        self
    }

    /// The generated field of `action`'s `parameter` shows `text`.
    pub fn field(mut self, action: &str, parameter: &str, text: impl Into<String>) -> Self {
        self.expect
            .fields
            .push((action.into(), parameter.into(), text.into()));
        self
    }

    /// The `effect` layer is the one the named earlier step's frame shows, updated in place.
    pub fn same_layer(mut self, effect: &str, step: &str) -> Self {
        self.expect.same_layer.push((effect.into(), step.into()));
        self
    }

    pub fn no_draft(mut self) -> Self {
        self.expect.draft = Some(Draft::None);
        self
    }

    /// An open, unconflicted slider draft of `action` holding exactly `fields`.
    pub fn draft(mut self, action: &str, fields: Value) -> Self {
        self.expect.draft = Some(Draft::Open {
            action: action.into(),
            fields,
            conflicted: false,
        });
        self
    }

    /// A slider draft of `action` holding exactly `fields`, kept and marked conflicted by a commit
    /// made under it.
    pub fn conflicted(mut self, action: &str, fields: Value) -> Self {
        self.expect.draft = Some(Draft::Open {
            action: action.into(),
            fields,
            conflicted: true,
        });
        self
    }

    pub fn expanded(mut self, module: &str) -> Self {
        self.expect.expanded.push((module.into(), true));
        self
    }

    pub fn collapsed(mut self, module: &str) -> Self {
        self.expect.expanded.push((module.into(), false));
        self
    }

    /// The step ends refused: a script step is recorded `failed` with a reason that leads with
    /// `reason`, and an open ends in the error code `reason`.
    pub fn refused(mut self, reason: impl Into<String>) -> Self {
        self.expect.refused = Some(reason.into());
        self
    }
}

/// Every frame one launch captures, in capture order: the opens' frames first, then one per script
/// step.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    steps: Vec<Step>,
}

impl Plan {
    pub fn new(steps: Vec<Step>) -> Self {
        let plan = Self { steps };
        #[cfg(test)]
        let plan = mutation::apply(plan);
        plan
    }

    #[cfg(test)]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// How many frames the launch must capture.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Whether the launch runs a script at all.
    pub fn scripted(&self) -> bool {
        self.steps.iter().any(|step| step.script.is_some())
    }

    /// The evidence script: every scripted step, in order, as the editor reads it.
    pub fn script(&self) -> Value {
        let steps: Vec<script::Step> = self
            .steps
            .iter()
            .filter_map(|step| step.script.clone())
            .collect();
        script::write(&steps)
    }

    /// The script as it is kept beside the evidence: [`Plan::script`] with each secret redacted.
    pub fn kept(&self) -> Value {
        Value::Array(self.steps.iter().filter_map(Step::kept).collect())
    }

    /// Where the named step's frame is in capture order.
    pub fn index(&self, name: &str) -> Option<usize> {
        self.steps.iter().position(|step| step.name == name)
    }

    /// Whether the plan expects a refusal anywhere, which is exactly when the run may record an
    /// input error.
    pub fn refuses(&self) -> bool {
        self.steps.iter().any(|step| step.expect.refused.is_some())
    }

    /// Every name is unique, and every open's frame comes before the first script step's, which is
    /// the order the editor captures them in.
    pub fn validate(&self) -> Result {
        ensure(!self.is_empty(), "A launch captures at least one frame")?;
        let mut names = std::collections::BTreeSet::new();
        for step in &self.steps {
            ensure(
                !step.name.is_empty() && names.insert(step.name.as_str()),
                format!("The plan names step {:?} twice, or not at all", step.name),
            )?;
            for (_, earlier) in &step.expect.same_layer {
                let at = self.index(earlier);
                ensure(
                    at.is_some_and(|at| Some(at) < self.index(&step.name)),
                    format!(
                        "Step {:?} compares a layer with {earlier:?}, which is not an earlier step",
                        step.name
                    ),
                )?;
            }
        }
        for step in &self.steps {
            if let Some(script) = &step.script {
                script.validate().map_err(|error| {
                    format!("Step {:?} is not a valid script step: {error}", step.name)
                })?;
            }
        }
        let first_scripted = self.steps.iter().position(|step| step.script.is_some());
        ensure(
            first_scripted
                .is_none_or(|first| self.steps[first..].iter().all(|step| step.script.is_some())),
            "An open's frame is planned after a script step's",
        )?;
        ensure(
            self.steps
                .first()
                .is_none_or(|step| step.expect.commits.is_none()),
            "The first frame has no frame before it to commit against",
        )?;
        Ok(())
    }

    /// Check one launch's evidence against the plan: the invariants every frame keeps, then each
    /// step's expectations. Writes `plan-checks.json` beside the evidence, and returns the launch
    /// with its frames for the scenario's own checks.
    pub fn check(&self, evidence: &Path) -> Result<Checked> {
        let (app, events) = preamble(evidence, self.len()).map_err(|error| {
            let captured = read_json(&evidence.join("result.json"))
                .ok()
                .and_then(|app| app["frames"].as_array().map(Vec::len));
            match captured {
                Some(captured) if captured != self.len() => format!(
                    "{}: the plan has {} steps, and so {} frames, but the launch captured {captured}",
                    evidence.display(),
                    self.len(),
                    self.len()
                ),
                _ => format!("{}: {error}", evidence.display()),
            }
        })?;
        self.validate()?;
        let records = app["frames"].as_array().ok_or("Missing frames")?;
        let script: &[Value] = app["script"].as_array().map_or(&[], Vec::as_slice);
        let scripted = self
            .steps
            .iter()
            .filter(|step| step.script.is_some())
            .count();
        ensure(
            script.len() == scripted,
            format!(
                "The plan scripts {scripted} steps, but the run records {}",
                script.len()
            ),
        )?;
        let logged: Vec<&Value> = events
            .iter()
            .filter(|event| event["event"] == "script_step")
            .collect();
        ensure(
            logged.len() == scripted,
            format!(
                "The plan scripts {scripted} steps, but the log has {} script_step events",
                logged.len()
            ),
        )?;
        ensure(
            app["had_input_errors"] == json!(self.refuses()),
            if self.refuses() {
                "The plan expects a refusal, but the run recorded no input error".to_owned()
            } else {
                format!(
                    "The run recorded an input error the plan does not expect: {}",
                    json!(
                        script
                            .iter()
                            .filter(|step| step["status"] != "sent")
                            .collect::<Vec<_>>()
                    )
                )
            },
        )?;
        let mut frames = Vec::with_capacity(records.len());
        let mut number = 0usize;
        for (index, (step, record)) in self.steps.iter().zip(records).enumerate() {
            let at = |error: &dyn std::fmt::Display| {
                format!(
                    "Step {:?} (frame {index}, {}): {error}",
                    step.name,
                    record["file"].as_str().unwrap_or("no file")
                )
            };
            let frame = Frame::identified(evidence, &app, record).map_err(|error| at(&error))?;
            let recorded = &frame["step"];
            match step.kept() {
                None => {
                    ensure(
                        recorded.is_null(),
                        at(&format!(
                            "an open's frame records a script step: {recorded}"
                        )),
                    )?;
                    if let Some(code) = &step.expect.refused {
                        let state = frame.state();
                        ensure(
                            state["phase"] == "error" && state["error_code"] == json!(code),
                            at(&format!(
                                "the open was to fail with {code:?}, but ended in phase {} with {}",
                                state["phase"], state["error_code"]
                            )),
                        )?;
                    }
                }
                Some(scripted) => {
                    let scripted = &scripted;
                    number += 1;
                    ensure(
                        recorded["step"] == json!(number),
                        at(&format!(
                            "the frame records step {}, not {number}",
                            recorded["step"]
                        )),
                    )?;
                    ensure(
                        request_matches(&recorded["request"], scripted),
                        at(&format!(
                            "the editor recorded {} for the scripted {scripted}",
                            recorded["request"]
                        )),
                    )?;
                    let mut listed = script[number - 1].clone();
                    let file = listed
                        .as_object_mut()
                        .and_then(|object| object.remove("frame"));
                    ensure(
                        &listed == recorded && file.as_ref() == Some(&frame["file"]),
                        at(&format!(
                            "the run's script lists {} for this frame's {recorded}",
                            script[number - 1]
                        )),
                    )?;
                    let detail = &logged[number - 1]["detail"];
                    ensure(
                        detail["step"] == recorded["step"]
                            && detail["request"] == recorded["request"],
                        at(&format!("the log's script_step is {detail}")),
                    )?;
                    match &step.expect.refused {
                        None => ensure(
                            recorded["status"] == "sent",
                            at(&format!("the step was not sent: {recorded}")),
                        )?,
                        Some(reason) => ensure(
                            recorded["status"] == "failed"
                                && recorded["reason"]
                                    .as_str()
                                    .is_some_and(|recorded| recorded.starts_with(reason.as_str())),
                            at(&format!(
                                "the step was to be refused with {reason:?}: {recorded}"
                            )),
                        )?,
                    }
                }
            }
            frames.push(frame);
        }
        let mut checks = Vec::with_capacity(frames.len());
        for (index, step) in self.steps.iter().enumerate() {
            let frame = &frames[index];
            self.expect(index, &frames).map_err(|error| {
                format!(
                    "Step {:?} (frame {index}, {}): {error}",
                    step.name,
                    frame["file"].as_str().unwrap_or("no file")
                )
            })?;
            checks.push(json!({
                "step": step.name,
                "frame": frame["file"],
                "request": frame["step"]["request"],
                "expected": step.expect.record(),
            }));
        }
        write_json(
            &evidence.join("plan-checks.json"),
            &json!({
                "frames": frames.len(),
                "steps": checks,
                "invariants": "Every frame: run identity, backend, renderer-readback provenance, its state file equal to the run's record; a scripted frame's recorded step equal to the scripted one as the editor reads it back, listed in the run's script and the log, sent unless refused; an input error exactly when a refusal is planned.",
            }),
        )?;
        Ok(Checked {
            evidence: evidence.into(),
            app,
            events,
            frames,
            names: self.steps.iter().map(|step| step.name.clone()).collect(),
        })
    }

    /// Step `index`'s expectations against its frame, the frame before it and any earlier step it
    /// names.
    fn expect(&self, index: usize, frames: &[Frame]) -> Result {
        let expect = &self.steps[index].expect;
        let frame = &frames[index];
        if let Some(n) = expect.commits {
            let before = &frames[index.checked_sub(1).ok_or("No frame before the first")?];
            let (from, to) = (before.revision()?, frame.revision()?);
            ensure(
                to == from + n,
                format!("the revision moved from {from} to {to}, expected {n} commit(s)"),
            )?;
            let (was, is) = (before.entry()?, frame.entry()?);
            if n == 0 {
                ensure(
                    is == was,
                    format!(
                        "nothing was to be committed, but the current entry moved from {was} to {is}"
                    ),
                )?;
            } else {
                ensure(
                    is != was,
                    format!("the commit left the current entry at {is}"),
                )?;
            }
        }
        if let Some(label) = &expect.label {
            let shown = frame.label()?;
            ensure(
                shown == label,
                format!("the entry is labelled {shown:?}, expected {label:?}"),
            )?;
        }
        for (effect, payload) in &expect.layers {
            let stored = frame.payload(effect);
            ensure(
                stored == payload.as_ref(),
                match payload {
                    Some(payload) => {
                        format!("the {effect} layer holds {stored:?}, expected {payload}")
                    }
                    None => format!("the stack holds an {effect} layer: {stored:?}"),
                },
            )?;
        }
        for (action, parameter, text) in &expect.fields {
            let shown = frame.field(action, parameter)?;
            ensure(
                shown == text,
                format!("{action}.{parameter} shows {shown:?}, expected {text:?}"),
            )?;
        }
        for (effect, step) in &expect.same_layer {
            let earlier = self
                .index(step)
                .ok_or_else(|| format!("no step is named {step:?}"))?;
            let identity = frames[earlier]
                .layer_id(effect)
                .ok_or_else(|| format!("step {step:?} shows no {effect} layer"))?;
            ensure(
                frame.layer_id(effect) == Some(identity),
                format!(
                    "the {effect} layer is {:?}, not step {step:?}'s {identity} updated in place",
                    frame.layer_id(effect)
                ),
            )?;
        }
        match &expect.draft {
            Some(Draft::None) => frame.expect_no_draft("the frame")?,
            Some(Draft::Open {
                action,
                fields,
                conflicted,
            }) => {
                let draft = frame.draft();
                ensure(
                    draft["action"] == json!(action)
                        && &draft["fields"] == fields
                        && draft["conflicted"] == json!(conflicted),
                    format!(
                        "the draft is {draft}, expected {action} holding {fields}{}",
                        if *conflicted { ", conflicted" } else { "" }
                    ),
                )?;
            }
            None => {}
        }
        for (module, open) in &expect.expanded {
            // The section's own recorded state, not merely "not expanded": a section the frame
            // does not list at all is neither.
            let recorded = &frame.state()["expanded"][module];
            ensure(
                recorded == &json!(open),
                format!(
                    "the {module} section records {recorded}, expected {}",
                    if *open { "expanded" } else { "collapsed" }
                ),
            )?;
        }
        Ok(())
    }
}

/// Whether the editor's record of a request is the scripted request as its parser reads it back.
/// The record is the parsed step written out again, so it spells out the defaults a script may
/// leave out (`release` and `cancel` false, `finish` open, empty `params`), holds a number the
/// script wrote as text (`"100"`) as a number, and holds a value the editor keeps in single
/// precision to that precision. Nothing else may differ.
pub fn request_matches(recorded: &Value, scripted: &Value) -> bool {
    match (recorded, scripted) {
        (Value::Object(recorded), Value::Object(scripted)) => {
            scripted.iter().all(|(key, value)| {
                recorded
                    .get(key)
                    .is_some_and(|recorded| request_matches(recorded, value))
            }) && recorded.iter().all(|(key, value)| {
                scripted.contains_key(key)
                    || *value == json!(false)
                    || *value == json!({})
                    || (key == "finish" && *value == json!("open"))
            })
        }
        (Value::Array(recorded), Value::Array(scripted)) => {
            recorded.len() == scripted.len()
                && recorded
                    .iter()
                    .zip(scripted)
                    .all(|(recorded, scripted)| request_matches(recorded, scripted))
        }
        (Value::Number(recorded), Value::Number(scripted)) => recorded
            .as_f64()
            .zip(scripted.as_f64())
            .is_some_and(same_number),
        (Value::Number(recorded), Value::String(scripted)) => recorded
            .as_f64()
            .zip(scripted.parse::<f64>().ok())
            .is_some_and(same_number),
        _ => recorded == scripted,
    }
}

fn same_number((recorded, scripted): (f64, f64)) -> bool {
    recorded == scripted || recorded as f32 == scripted as f32
}

/// One launch's evidence once its plan has held: the run's own result and log, and every frame,
/// each read by the name of the step that produced it.
pub struct Checked {
    pub evidence: PathBuf,
    pub app: Value,
    pub events: Vec<Value>,
    pub frames: Vec<Frame>,
    names: Vec<String>,
}

/// A one-launch scenario's launch.
pub fn only(launches: &[Checked]) -> Result<&Checked> {
    match launches {
        [launch] => Ok(launch),
        _ => Err(format!("Expected one launch, found {}", launches.len()).into()),
    }
}

impl std::fmt::Debug for Checked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Checked")
            .field("evidence", &self.evidence)
            .field("steps", &self.names)
            .finish_non_exhaustive()
    }
}

impl Checked {
    /// Where the named step's frame is in capture order.
    pub fn index(&self, name: &str) -> Result<usize> {
        self.names
            .iter()
            .position(|step| step == name)
            .ok_or_else(|| format!("The plan has no step named {name:?}").into())
    }

    /// The named step's frame.
    pub fn at(&self, name: &str) -> Result<&Frame> {
        Ok(&self.frames[self.index(name)?])
    }

    /// The step names, in capture order.
    pub fn names(&self) -> &[String] {
        &self.names
    }
}

/// Plan mutations for the proof that a plan's own count and labels are what a scenario is checked
/// against: a test sets one, and every plan made on that thread afterwards is mutated.
#[cfg(test)]
pub mod mutation {
    use super::*;
    use std::cell::RefCell;

    #[derive(Clone, Debug)]
    pub enum Mutation {
        /// Remove the last step.
        DropLast,
        /// Change the first expected label, naming the step it changed.
        Relabel,
    }

    thread_local! {
        static MUTATION: RefCell<Option<Mutation>> = const { RefCell::new(None) };
        static CHANGED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// Mutate every plan made on this thread until the next call; `None` stops.
    pub fn set(mutation: Option<Mutation>) {
        MUTATION.with(|cell| *cell.borrow_mut() = mutation);
        CHANGED.with(|cell| cell.borrow_mut().clear());
    }

    /// The steps whose label a mutation changed, in the order the plans were made.
    pub fn changed() -> Vec<String> {
        CHANGED.with(|cell| cell.borrow().clone())
    }

    pub(super) fn apply(mut plan: Plan) -> Plan {
        match MUTATION.with(|cell| cell.borrow().clone()) {
            None => {}
            Some(Mutation::DropLast) => {
                plan.steps.pop();
            }
            Some(Mutation::Relabel) => {
                if let Some(step) = plan
                    .steps
                    .iter_mut()
                    .find(|step| step.expect.label.is_some())
                {
                    step.expect.label = Some("A label no step commits".into());
                    CHANGED.with(|cell| cell.borrow_mut().push(step.name.clone()));
                }
            }
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_request_matches_the_scripted_one_as_the_parser_reads_it() {
        let slider = json!({"slider":{"action":"set-basic","parameter":"exposure","values":[1.0]}});
        let recorded = json!({"slider":{"action":"set-basic","parameter":"exposure","values":[1.0],"release":false,"cancel":false}});
        assert!(request_matches(&recorded, &slider));
        // A default the script left out is only a default when it is the default.
        let released = json!({"slider":{"action":"set-basic","parameter":"exposure","values":[1.0],"release":true,"cancel":false}});
        assert!(!request_matches(&released, &slider));
        assert!(request_matches(
            &json!({"view":{"zoom":100.0}}),
            &json!({"view":{"zoom":"100"}})
        ));
        assert!(!request_matches(
            &json!({"view":{"zoom":50.0}}),
            &json!({"view":{"zoom":"100"}})
        ));
        assert!(request_matches(
            &json!({"api":{"method":"history.undo","params":{}}}),
            &json!({"api":{"method":"history.undo"}})
        ));
        // Single precision: the editor keeps a normalized position as `f32`.
        assert!(request_matches(
            &json!({"mask":{"sweep":{"to":[0.92,0.5]}}}),
            &json!({"mask":{"sweep":{"to":[0.919_999_999_999_999_9,0.5]}}})
        ));
        assert!(!request_matches(
            &json!({"mask":{"sweep":{"to":[0.93,0.5]}}}),
            &json!({"mask":{"sweep":{"to":[0.92,0.5]}}})
        ));
        assert!(request_matches(
            &json!({"picker":{"action":"a","parameter":"p","finish":"open"}}),
            &json!({"picker":{"action":"a","parameter":"p"}})
        ));
        assert!(!request_matches(
            &json!({"picker":{"action":"a","parameter":"p","finish":"release"}}),
            &json!({"picker":{"action":"a","parameter":"p"}})
        ));
        // A value the script set is never excused, and nothing the script wrote may be missing.
        assert!(!request_matches(
            &json!({"section":{"module":"m"}}),
            &json!({"section":{"module":"m","expanded":false}})
        ));
        assert!(!request_matches(
            &json!({"wait":{"ms":500,"extra":1}}),
            &json!({"wait":{"ms":500}})
        ));
    }

    #[test]
    fn a_plan_derives_its_script_and_refuses_a_malformed_order() {
        let secret = |value: &str| {
            script::CapabilityStep::new(
                "m",
                script::CapabilityAction::Secret {
                    field: "key".into(),
                    value: script::Secret::new(value.into()),
                    profile: None,
                },
            )
        };
        let plan = Plan::new(vec![
            Step::opened("opened"),
            Step::new("expand", script::Step::section("m", true)).commits(0),
            Step::new("key", secret("k")),
        ]);
        assert!(plan.validate().is_ok());
        assert_eq!(plan.len(), 3);
        let secret = |value: &str| json!({"capability":{"module":"m","secret":{"field":"key","value":value}}});
        assert_eq!(
            plan.script(),
            json!([{"section":{"module":"m","expanded":true}},secret("k")])
        );
        // What is kept, and recorded, holds the secret redacted.
        assert_eq!(
            plan.kept(),
            json!([{"section":{"module":"m","expanded":true}},secret("<redacted>")])
        );
        assert_eq!(plan.index("key"), Some(2));
        let late = Plan::new(vec![
            Step::new("a", script::Step::wait(1)),
            Step::opened("b"),
        ]);
        assert!(late.validate().is_err());
        let twice = Plan::new(vec![Step::opened("a"), Step::opened("a")]);
        assert!(twice.validate().is_err());
        let forward = Plan::new(vec![
            Step::opened("a").same_layer("e", "b"),
            Step::new("b", script::Step::wait(1)),
        ]);
        assert!(forward.validate().is_err());
        // A step the editor would refuse to parse is refused by the plan first.
        let invalid = Plan::new(vec![
            Step::opened("a"),
            Step::new("b", script::Step::wait(0)),
        ]);
        let error = invalid.validate().unwrap_err().to_string();
        assert!(
            error.contains("Step \"b\" is not a valid script step"),
            "{error}"
        );
    }

    /// A three-frame launch written the way the editor writes one, for the checks below.
    fn launch(dir: &Path, labels: [&str; 3], errors: bool) {
        let requests = [
            Value::Null,
            json!({"slider":{"action":"set-x","parameter":"p","values":[1.0],"release":true,"cancel":false}}),
            json!({"wait":{"ms":10}}),
        ];
        let mut frames = Vec::new();
        let mut script = Vec::new();
        let mut events = vec![json!({"event":"startup","run_id":"r"})];
        for (index, (label, request)) in labels.iter().zip(&requests).enumerate() {
            let file = format!("frame-{}.png", index + 1);
            image::RgbImage::new(4, 4).save(dir.join(&file)).unwrap();
            let step = if request.is_null() {
                Value::Null
            } else {
                json!({"step":index,"status":"sent","request":request})
            };
            if !step.is_null() {
                let mut listed = step.clone();
                listed["frame"] = json!(file);
                script.push(listed);
                events.push(json!({"event":"script_step","run_id":"r","detail":step}));
            }
            let frame = json!({
                "file": file,
                "capture_provenance": "window-renderer-readback",
                "step": step,
                "state": {
                    "run_id": "r",
                    "backend": {"backend":"metal","adapter":"test"},
                    "stack": {"revision": index.min(1), "entry": format!("e{}", index.min(1)), "label": label, "layers": []},
                    "draft": null,
                },
            });
            write_json(&dir.join(format!("state-{}.json", index + 1)), &frame).unwrap();
            frames.push(frame);
        }
        events.push(json!({"event":"shutdown","run_id":"r"}));
        fs::write(
            dir.join("events.jsonl"),
            events
                .iter()
                .map(|event| format!("{event}\n"))
                .collect::<String>(),
        )
        .unwrap();
        write_json(
            &dir.join("result.json"),
            &json!({"status":"captured","run_id":"r","had_input_errors":errors,"frames":frames,"script":script}),
        )
        .unwrap();
    }

    fn plan(label: &str) -> Plan {
        Plan::new(vec![
            Step::opened("opened").no_draft(),
            Step::new(
                "release",
                script::SliderStep::new("set-x", "p", [1.0]).release(),
            )
            .commits(1)
            .label(label),
            Step::new("wait", script::Step::wait(10)).commits(0),
        ])
    }

    #[test]
    fn a_launch_is_checked_step_by_step_against_its_plan() {
        let tmp = tempfile::tempdir().unwrap();
        launch(tmp.path(), ["Original", "X 1", "X 1"], false);
        let checked = plan("X 1").check(tmp.path()).unwrap();
        assert_eq!(checked.index("wait").unwrap(), 2);
        assert_eq!(checked.at("release").unwrap().label().unwrap(), "X 1");
        assert!(checked.at("absent").is_err());
        assert!(tmp.path().join("plan-checks.json").is_file());

        // A changed label fails on its own step.
        let error = plan("X 2").check(tmp.path()).unwrap_err().to_string();
        assert!(error.starts_with("Step \"release\" (frame 1"), "{error}");
        assert!(error.contains("expected \"X 2\""), "{error}");

        // A step removed from the plan fails on the frame count.
        let mut short = plan("X 1");
        short.steps.pop();
        let error = short.check(tmp.path()).unwrap_err().to_string();
        assert!(error.contains("the plan has 2 steps"), "{error}");

        // A scripted step the editor recorded differently fails on that step.
        let mut other = plan("X 1");
        other.steps[2].script = Some(script::Step::wait(20));
        let error = other.check(tmp.path()).unwrap_err().to_string();
        assert!(error.starts_with("Step \"wait\" (frame 2"), "{error}");

        // An input error nothing in the plan refuses fails the run, and a planned refusal the run
        // did not record fails too.
        let errors = tempfile::tempdir().unwrap();
        launch(errors.path(), ["Original", "X 1", "X 1"], true);
        assert!(plan("X 1").check(errors.path()).is_err());
        let mut refusing = plan("X 1");
        refusing.steps[2] = refusing.steps[2].clone().refused("no");
        assert!(refusing.check(tmp.path()).is_err());
    }
}
