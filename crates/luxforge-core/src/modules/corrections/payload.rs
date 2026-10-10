//! The repair layer's payload: ordered Clone/Heal operations with stable numbers
//! (`docs/design/corrections.md`, "Operations").
use crate::{Error, path::StrokeId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// One Clone or Heal operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
    pub id: u32,
    pub kind: OperationKind,
    pub stroke: StrokeId,
    pub feather: u8,
    pub opacity: u8,
    /// Source anchor minus destination anchor, in grid steps.
    pub offset: [i32; 2],
    pub origin: Origin,
}

impl Operation {
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
    /// feather and opacity within `0..=100`, each offset within the stored range and not zero, and
    /// `strokes` exactly the operations' strokes in order, so the host's carrier and the
    /// operations cannot disagree.
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
            if operation.id == 0 {
                return refuse("an operation number is 0".into());
            }
            if !seen.insert(operation.id) {
                return refuse(format!("{name} appears twice"));
            }
            if u64::from(operation.id) >= self.next {
                return refuse(format!("{name} is not below next {}", self.next));
            }
            if operation.feather > 100 || operation.opacity > 100 {
                return refuse(format!("{name} has a feather or opacity outside 0..=100"));
            }
            if operation
                .offset
                .iter()
                .any(|value| value.abs() > OFFSET_MAX)
            {
                return refuse(format!("{name} has an offset outside ±{OFFSET_MAX}"));
            }
            if operation.offset == [0, 0] {
                return refuse(format!("{name} has a zero offset"));
            }
        }
        if self.strokes.len() != self.operations.len()
            || self
                .strokes
                .iter()
                .zip(&self.operations)
                .any(|(stroke, operation)| *stroke != operation.stroke)
        {
            return refuse("strokes must list each operation's stroke in operation order".into());
        }
        Ok(())
    }

    /// This payload as stored: `strokes` rebuilt from the operations first.
    pub(crate) fn stored(mut self) -> Value {
        self.strokes = self
            .operations
            .iter()
            .map(|operation| operation.stroke.clone())
            .collect();
        serde_json::to_value(self).expect("a repair payload is serializable")
    }

    /// The operation numbered `id` and its index, or the refusal of one the layer does not hold.
    pub(crate) fn find(&self, id: u32) -> Result<(usize, &Operation), Error> {
        self.operations
            .iter()
            .enumerate()
            .find(|(_, operation)| operation.id == id)
            .ok_or_else(|| {
                Error::validation(format!("no correction {} in this photograph", identity(id)))
            })
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
                "unknown variant",
                Box::new(|p| p["operations"][0]["kind"] = json!("patch")),
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
