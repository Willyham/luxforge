//! The repair layer's payload: ordered Clone/Heal operations with stable numbers
//! (`docs/design/corrections.md`, "Operations").
use super::input::RepairInput;
use crate::{
    ArtifactId, Error,
    artifacts::{
        MAX_LAYER_ARTIFACTS,
        candidates::{MAX_CANDIDATE_LABEL, MAX_PROVENANCE_BYTES},
    },
    path::StrokeId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The most operations one repair layer holds, and so one photograph: the effect's declared
/// `operations.max`.
pub(crate) const MAX_CORRECTIONS: usize = 100;

/// The largest stored offset component, in grid steps: the whole stored coordinate range.
pub(crate) const OFFSET_MAX: i32 = 49152;

/// The largest operation number, the last `id` a lineage can mint.
pub(crate) const MAX_ID: u64 = u32::MAX as u64;

/// The repair layer's payload, format 1: `next`, the reserved `strokes` and the operations in
/// application order (index 0 is applied first). Compact JSON in that field order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepairPayload {
    /// The next operation number to mint: at least 1 and greater than every `id` present. It only
    /// grows, so a number is never reused along a lineage.
    pub next: u64,
    /// Exactly the `stroke` of each Clone/Heal operation, in operation order.
    pub strokes: Vec<StrokeId>,
    pub operations: Vec<Operation>,
}

impl Default for RepairPayload {
    /// The empty layer, before anything was added: what a layer the stack does not hold yet reads
    /// as, and the neutral payload a GPU preview plans in its place.
    fn default() -> Self {
        Self {
            next: 1,
            strokes: Vec::new(),
            operations: Vec::new(),
        }
    }
}

/// One operation of the repair layer, in application order: a Clone or Heal drawn with a stroke,
/// or a frozen patch (TASK-008). Stored as compact JSON in each kind's field order; `kind` says
/// which.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub(crate) enum Operation {
    Brush(Brush),
    Patch(Patch),
}

/// One Clone or Heal operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Brush {
    pub id: u32,
    pub kind: OperationKind,
    pub stroke: StrokeId,
    pub feather: u8,
    pub opacity: u8,
    /// Source anchor minus destination anchor, in grid steps.
    pub offset: [i32; 2],
    pub origin: Origin,
}

/// A frozen patch: immutable pixels a producer generated and a person accepted, composited 1:1 in
/// content pixels over the operations below it (`docs/design/corrections.md`, "Frozen patches").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Patch {
    pub id: u32,
    pub kind: PatchKind,
    /// The display noun, 1 to 32 characters, from the producer: `Remove`, the test producer's
    /// `Patch`.
    pub label: String,
    /// The `repair-patch` artifact: `width × height` RGBA half floats, RGB linear and A the
    /// coverage, not premultiplied.
    pub artifact: ArtifactId,
    /// `[x, y, width, height]` in content pixels; `width × height` is the artifact's.
    pub rect: [u32; 4],
    pub opacity: u8,
    /// What it was generated against.
    pub input: RepairInput,
    /// `{module, task, …}`: at most 2 KiB of string and number fields.
    pub provenance: Map<String, Value>,
}

/// The one kind a [`Patch`] is stored as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PatchKind {
    Patch,
}

impl<'de> Deserialize<'de> for Operation {
    /// By its `kind`: `heal` and `clone` are a [`Brush`], `patch` a [`Patch`], each refusing any
    /// field it does not hold; anything else is named.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let value = Value::deserialize(deserializer)?;
        match value.get("kind").and_then(Value::as_str) {
            Some("heal" | "clone") => serde_json::from_value(value)
                .map(Self::Brush)
                .map_err(D::Error::custom),
            Some("patch") => serde_json::from_value(value)
                .map(Self::Patch)
                .map_err(D::Error::custom),
            Some(other) => Err(D::Error::custom(format!(
                "unknown variant `{other}`, expected one of `heal`, `clone`, `patch`"
            ))),
            None => Err(D::Error::custom("an operation names its kind")),
        }
    }
}

impl Operation {
    pub(crate) fn id(&self) -> u32 {
        match self {
            Self::Brush(brush) => brush.id,
            Self::Patch(patch) => patch.id,
        }
    }

    /// What a person calls it: the kind's title, or a patch's stored noun, and its number,
    /// `Heal 5`, `Remove 7`.
    pub(crate) fn name(&self) -> String {
        match self {
            Self::Brush(brush) => brush.name(),
            Self::Patch(patch) => format!("{} {}", patch.label, patch.id),
        }
    }

    /// The identity the API spells it by, `correction-5`.
    pub(crate) fn identity(&self) -> String {
        identity(self.id())
    }

    pub(crate) fn brush(&self) -> Option<&Brush> {
        match self {
            Self::Brush(brush) => Some(brush),
            Self::Patch(_) => None,
        }
    }

    pub(crate) fn patch(&self) -> Option<&Patch> {
        match self {
            Self::Patch(patch) => Some(patch),
            Self::Brush(_) => None,
        }
    }
}

impl Brush {
    /// What a person calls it: the kind's title and its number, `Heal 5`.
    pub(crate) fn name(&self) -> String {
        format!("{} {}", self.kind.title(), self.id)
    }

    /// The identity the API spells it by, `correction-5`.
    pub(crate) fn identity(&self) -> String {
        identity(self.id)
    }
}

/// The API spelling of operation number `id`.
pub(crate) fn identity(id: u32) -> String {
    format!("correction-{id}")
}

/// The operation number a `correction-<n>` identity names: `n` decimal in `1..=4294967295` with
/// no leading zero, as the identity check admits it.
pub(crate) fn number(identity: &str) -> Result<u32, Error> {
    crate::CorrectionId::parse(identity)
        .map(crate::CorrectionId::number)
        .map_err(|_| Error::validation(format!("invalid correction identity {identity}")))
}

/// What a Clone/Heal operation does with its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum OperationKind {
    /// Borrows the source's detail and adapts it to the destination's tone.
    Heal,
    /// Copies the source's pixels.
    Clone,
}

impl OperationKind {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Heal => "Heal",
            Self::Clone => "Clone",
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Heal => "heal",
            Self::Clone => "clone",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "heal" => Some(Self::Heal),
            "clone" => Some(Self::Clone),
            _ => None,
        }
    }
}

/// Where an operation's source came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Origin {
    /// The automatic search chose the offset.
    Auto,
    /// A person or client did.
    Set,
}

impl Origin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Set => "set",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "set" => Some(Self::Set),
            _ => None,
        }
    }
}

impl RepairPayload {
    /// A stored payload, structurally checked ([`Self::check`]). A payload this build cannot read
    /// — an unknown field or kind, such as a later build's frozen patch — is refused by name and
    /// never rewritten.
    pub(crate) fn parse(payload: &Value) -> Result<Self, Error> {
        let parsed: Self = serde_json::from_value(payload.clone())
            .map_err(|error| Error::validation(format!("invalid Corrections payload: {error}")))?;
        parsed.check()?;
        Ok(parsed)
    }

    /// Every rule a stored payload keeps: at most [`MAX_CORRECTIONS`] operations with unique
    /// numbers in `1..=4294967295`, `next` greater than each and at most one past the last number,
    /// feather and opacity within `0..=100`, each offset within the stored range and not zero, each
    /// patch's label, rectangle, input and provenance within their bounds and at most
    /// [`MAX_LAYER_ARTIFACTS`] distinct patch artifacts, and `strokes` exactly the Clone/Heal
    /// operations' strokes in order, so the host's carrier and the operations cannot disagree.
    pub(crate) fn check(&self) -> Result<(), Error> {
        let refuse = |detail: String| {
            Err(Error::validation(format!(
                "invalid Corrections payload: {detail}"
            )))
        };
        if self.operations.len() > MAX_CORRECTIONS {
            return refuse(format!(
                "{} operations, more than the {MAX_CORRECTIONS} a photograph holds",
                self.operations.len()
            ));
        }
        if self.next < 1 || self.next > MAX_ID + 1 {
            return refuse(format!("next {} is outside 1..={}", self.next, MAX_ID + 1));
        }
        let mut seen = std::collections::BTreeSet::new();
        for operation in &self.operations {
            let name = operation.identity();
            let id = operation.id();
            if id == 0 {
                return refuse("an operation number is 0".into());
            }
            if !seen.insert(id) {
                return refuse(format!("{name} appears twice"));
            }
            if u64::from(id) >= self.next {
                return refuse(format!("{name} is not below next {}", self.next));
            }
            match operation {
                Operation::Brush(brush) => {
                    if brush.feather > 100 || brush.opacity > 100 {
                        return refuse(format!("{name} has a feather or opacity outside 0..=100"));
                    }
                    if brush.offset.iter().any(|value| value.abs() > OFFSET_MAX) {
                        return refuse(format!("{name} has an offset outside ±{OFFSET_MAX}"));
                    }
                    if brush.offset == [0, 0] {
                        return refuse(format!("{name} has a zero offset"));
                    }
                }
                Operation::Patch(patch) => {
                    if let Err(error) = patch.check() {
                        return refuse(format!("{name}: {}", error.detail));
                    }
                }
            }
        }
        if self.artifacts().len() > MAX_LAYER_ARTIFACTS {
            return refuse(format!(
                "more than {MAX_LAYER_ARTIFACTS} distinct patches"
            ));
        }
        let strokes: Vec<&StrokeId> = self.brushes().map(|brush| &brush.stroke).collect();
        if self.strokes.len() != strokes.len()
            || self
                .strokes
                .iter()
                .zip(strokes)
                .any(|(stroke, held)| stroke != held)
        {
            return refuse("strokes must list each operation's stroke in operation order".into());
        }
        Ok(())
    }

    /// The Clone and Heal operations, in order.
    pub(crate) fn brushes(&self) -> impl Iterator<Item = &Brush> {
        self.operations.iter().filter_map(Operation::brush)
    }

    /// The artifacts the layer must list: each patch's, in operation order, each once. The host
    /// admits a repair layer only when its `artifacts` are exactly these.
    pub(crate) fn artifacts(&self) -> Vec<ArtifactId> {
        let mut artifacts: Vec<ArtifactId> = Vec::new();
        for patch in self.operations.iter().filter_map(Operation::patch) {
            if !artifacts.contains(&patch.artifact) {
                artifacts.push(patch.artifact.clone());
            }
        }
        artifacts
    }

    /// This payload as stored: `strokes` rebuilt from the operations first.
    pub(crate) fn stored(mut self) -> Value {
        self.strokes = self.brushes().map(|brush| brush.stroke.clone()).collect();
        serde_json::to_value(self).expect("a repair payload is serializable")
    }

    /// The operation numbered `id` and its index, or the refusal of one the layer does not hold.
    pub(crate) fn find(&self, id: u32) -> Result<(usize, &Operation), Error> {
        self.operations
            .iter()
            .enumerate()
            .find(|(_, operation)| operation.id() == id)
            .ok_or_else(|| {
                Error::validation(format!("no correction {} in this photograph", identity(id)))
            })
    }
}

impl Patch {
    /// A patch's own bounds: a label of 1 to 32 printable characters, a non-empty rectangle, an
    /// opacity within `0..=100`, an input in its stored form and a provenance of string and number
    /// fields of at most 2 KiB.
    pub(crate) fn check(&self) -> Result<(), Error> {
        let characters = self.label.chars().count();
        if characters == 0
            || characters > MAX_CANDIDATE_LABEL
            || self.label.chars().any(char::is_control)
            || self.label.trim() != self.label
        {
            return Err(Error::validation(format!(
                "a patch label is 1 to {MAX_CANDIDATE_LABEL} printable characters"
            )));
        }
        let [x, y, width, height] = self.rect;
        if width == 0
            || height == 0
            || u64::from(x) + u64::from(width) > u64::from(u32::MAX)
            || u64::from(y) + u64::from(height) > u64::from(u32::MAX)
        {
            return Err(Error::validation("a patch rectangle must be non-empty"));
        }
        if self.opacity > 100 {
            return Err(Error::validation("a patch's opacity is within 0..=100"));
        }
        self.input.check()?;
        if !self
            .provenance
            .values()
            .all(|value| value.is_string() || value.is_number())
        {
            return Err(Error::validation(
                "a patch's provenance holds string and number fields",
            ));
        }
        let length = serde_json::to_vec(&self.provenance)
            .map_err(|error| Error::internal(error.to_string()))?
            .len();
        if length > MAX_PROVENANCE_BYTES {
            return Err(Error::validation(format!(
                "a patch's provenance is at most {MAX_PROVENANCE_BYTES} bytes"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stroke(n: u8) -> String {
        format!("{n:032x}")
    }

    fn payload() -> Value {
        json!({
            "next": 8,
            "strokes": [stroke(1), stroke(2)],
            "operations": [
                {"id": 2, "kind": "heal", "stroke": stroke(1), "feather": 50, "opacity": 100,
                 "offset": [-1573, 0], "origin": "auto"},
                {"id": 5, "kind": "clone", "stroke": stroke(2), "feather": 20, "opacity": 80,
                 "offset": [0, -2458], "origin": "set"},
            ],
        })
    }

    #[test]
    fn the_frozen_payload_round_trips_in_its_field_order() {
        let parsed = RepairPayload::parse(&payload()).unwrap();
        assert_eq!(parsed.operations[1].name(), "Clone 5");
        assert_eq!(parsed.operations[0].identity(), "correction-2");
        assert_eq!(parsed.clone().stored(), payload());
        assert_eq!(
            serde_json::to_string(&parsed.operations[0]).unwrap(),
            format!(
                "{{\"id\":2,\"kind\":\"heal\",\"stroke\":\"{}\",\"feather\":50,\"opacity\":100,\
                 \"offset\":[-1573,0],\"origin\":\"auto\"}}",
                stroke(1)
            )
        );
        assert_eq!(
            RepairPayload::default().stored(),
            json!({"next": 1, "strokes": [], "operations": []})
        );
    }

    #[test]
    fn a_payload_breaking_a_rule_is_refused_by_name() {
        type Change = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Change)> = vec![
            ("is not below next 2", Box::new(|p| p["next"] = json!(2))),
            ("next 0", Box::new(|p| p["next"] = json!(0))),
            (
                "appears twice",
                Box::new(|p| p["operations"][1]["id"] = json!(2)),
            ),
            (
                "zero offset",
                Box::new(|p| p["operations"][0]["offset"] = json!([0, 0])),
            ),
            (
                "offset outside",
                Box::new(|p| p["operations"][0]["offset"] = json!([49153, 0])),
            ),
            (
                "feather or opacity",
                Box::new(|p| p["operations"][0]["opacity"] = json!(101)),
            ),
            (
                "operation order",
                Box::new(|p| p["strokes"] = json!([stroke(2), stroke(1)])),
            ),
            (
                "operation order",
                Box::new(|p| p["strokes"] = json!([stroke(1)])),
            ),
            (
                "unknown variant `remove`",
                Box::new(|p| p["operations"][0]["kind"] = json!("remove")),
            ),
            ("unknown field", Box::new(|p| p["extra"] = json!(1))),
            (
                "an operation number is 0",
                Box::new(|p| p["operations"][0]["id"] = json!(0)),
            ),
        ];
        for (expected, change) in cases {
            let mut value = payload();
            change(&mut value);
            let error = RepairPayload::parse(&value).unwrap_err();
            assert_eq!(error.kind, crate::ErrorKind::Validation);
            assert!(
                error.detail.contains(expected),
                "{expected}: {}",
                error.detail
            );
        }
    }

    #[test]
    fn a_correction_identity_is_its_number_without_leading_zeros() {
        assert_eq!(number("correction-5").unwrap(), 5);
        assert_eq!(number("correction-4294967295").unwrap(), u32::MAX);
        for bad in [
            "correction-0",
            "correction-05",
            "correction-",
            "correction-4294967296",
            "mask-1",
            "correction--1",
        ] {
            assert!(number(bad).is_err(), "{bad}");
        }
        assert_eq!(identity(9), "correction-9");
        let error = RepairPayload::parse(&payload())
            .unwrap()
            .find(9)
            .unwrap_err();
        assert_eq!(
            error.detail,
            "no correction correction-9 in this photograph"
        );
    }
}
