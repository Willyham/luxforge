//! The repair primitive a repair-stage module compiles its layer into ([`crate::Processing::Repair`],
//! `docs/design/corrections.md`, "Rendering (TASK-003)").
//!
//! A repair layer holds an ordered list of operations, each reading a region of its own input that
//! may lie anywhere in the stage: Clone copies a source region over a destination, Heal adapts the
//! source to the destination's surroundings. A module compiles its payload into one
//! [`RepairTable`] of at most [`MAX_REPAIR_OPERATIONS`] rows in **application order** (row 0 is
//! applied first); operation `i` reads only its input `P_i`, the layer's input with rows `0..i`
//! applied, for both its source and its destination's surroundings. The host owns everything
//! else: binding the table to the stage it renders at, the regions each row reads, the bounded
//! working set, the CPU reference evaluation (`crate::repair`) and the GPU program.
//!
//! The table is plain data in normalized content coordinates, exactly as the payload stores them,
//! so one compiled table binds to the full-resolution content stage and to any reduced stage a GPU
//! frame is drawn at. Nothing here reads a pixel or allocates anything that scales with the image.
use crate::{Error, path};
use std::sync::Arc;

pub(crate) use crate::repair::RepairKind;

/// The most rows one repair table holds: the owner's 100 corrections per photograph (2026-10-09).
/// A repair effect is single and admission keeps its layer in the leading run, so this is also the
/// limit per photograph. A table past it is refused with `resource-limit`, never truncated.
pub const MAX_REPAIR_OPERATIONS: usize = 100;

/// The largest offset component, in grid steps: the full coordinate range (`-1..2` content
/// units).
pub(crate) const REPAIR_OFFSET_MAX: i32 = 49_152;

/// The largest radius a row may hold, in content heights: a quarter of the frame's height.
pub(crate) const REPAIR_RADIUS_MAX: f64 = 0.25;

/// One row of a repair table: one stored Clone or Heal operation, in the stored-path grid
/// ([`path::COORDINATE_STEPS_PER_UNIT`] steps per unit; `x` a fraction of the content width and `y`
/// of its height).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepairOperation {
    /// The operation's number (`correction-<id>`), which refusals name it by.
    pub(crate) id: u32,
    pub(crate) kind: RepairKind,
    /// The stroke's stored positions, `1..=`[`path::POINTS_PER_STROKE`] of them; a spot is one.
    pub(crate) points: Arc<[[i32; 2]]>,
    /// The stroke's radius in grid steps of the content height.
    pub(crate) radius: i32,
    /// `0..=100`: the share of the radius over which coverage falls to zero.
    pub(crate) feather: u8,
    /// `0..=100`: the blend's scale.
    pub(crate) opacity: u8,
    /// Source anchor minus destination anchor, in grid steps; not both zero.
    pub(crate) offset: [i32; 2],
    /// A frozen patch's pixels, placement and name, for [`RepairKind::Patch`] only: a patch row
    /// holds no points, radius, feather or offset.
    pub(crate) patch: Option<RepairPatch>,
}

/// What a frozen patch row carries: its verified bytes and rectangle in the content stage, and the
/// noun its name is spelled with (`Remove 7`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepairPatch {
    pub(crate) source: crate::repair::patch::PatchSource,
    pub(crate) label: Arc<str>,
}

impl RepairOperation {
    /// A frozen patch row: `source` composited at `opacity`, named `label` and `id`.
    pub(crate) fn patch(
        id: u32,
        label: &str,
        opacity: u8,
        source: crate::repair::patch::PatchSource,
    ) -> Self {
        Self {
            id,
            kind: RepairKind::Patch,
            points: Arc::from([]),
            radius: 0,
            feather: 0,
            opacity,
            offset: [0, 0],
            patch: Some(RepairPatch {
                source,
                label: label.into(),
            }),
        }
    }
}

impl RepairOperation {
    /// The display name refusals use: `Heal 5`, `Clone 5`.
    pub(crate) fn name(&self) -> String {
        let title = match (self.kind, &self.patch) {
            (RepairKind::Clone, _) => "Clone",
            (RepairKind::Heal, _) => "Heal",
            (RepairKind::Patch, Some(patch)) => &patch.label,
            (RepairKind::Patch, None) => "Patch",
        };
        format!("{title} {}", self.id)
    }

    /// The stored positions in normalized coordinates.
    pub(crate) fn normalized_points(&self) -> Vec<[f64; 2]> {
        path::from_grid(&self.points).collect()
    }

    /// The radius in content heights.
    pub(crate) fn radius_units(&self) -> f64 {
        path::steps_to_units(self.radius)
    }

    fn check(&self) -> Result<(), Error> {
        let name = self.name();
        if self.kind == RepairKind::Patch || self.patch.is_some() {
            let Some(patch) = self.patch.as_ref().filter(|_| self.kind == RepairKind::Patch) else {
                return Err(Error::internal(format!(
                    "{name} mixes a patch with a stroke operation"
                )));
            };
            if self.opacity > 100 {
                return Err(Error::validation(format!(
                    "{name}'s opacity must lie within 0..=100"
                )));
            }
            return patch.source.check(&name);
        }
        if self.points.is_empty() || self.points.len() > path::POINTS_PER_STROKE {
            return Err(Error::resource_limit(format!(
                "{name} holds {} stroke positions; a repair operation holds 1..={}",
                self.points.len(),
                path::POINTS_PER_STROKE
            )));
        }
        if !self
            .points
            .iter()
            .flatten()
            .all(|&value| path::in_grid(value))
        {
            return Err(Error::validation(format!(
                "{name} holds a stroke position outside the coordinate grid"
            )));
        }
        let radius = self.radius_units();
        if self.radius <= 0 || radius > REPAIR_RADIUS_MAX {
            return Err(Error::validation(format!(
                "{name}'s radius {radius} is outside 0..={REPAIR_RADIUS_MAX} content heights"
            )));
        }
        if self.feather > 100 || self.opacity > 100 {
            return Err(Error::validation(format!(
                "{name}'s feather and opacity must lie within 0..=100"
            )));
        }
        if self.offset == [0, 0]
            || self
                .offset
                .iter()
                .any(|component| component.unsigned_abs() > REPAIR_OFFSET_MAX as u32)
        {
            return Err(Error::validation(format!(
                "{name}'s source offset must be non-zero and within ±{REPAIR_OFFSET_MAX} grid steps"
            )));
        }
        Ok(())
    }
}

/// What a repair layer compiles into: its operations in application order, at most
/// [`MAX_REPAIR_OPERATIONS`]. An empty table is the neutral layer, which the host drops, so an
/// emptied layer keeps the identity path and the shared source buffer.
///
/// Cloning shares the rows. Equality compares every row, which is what a GPU plan's tick compares
/// to find the operations a gesture changed.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RepairTable {
    operations: Arc<[RepairOperation]>,
}

impl RepairTable {
    /// A table over these rows in application order, or the refusal of the first row the host
    /// cannot evaluate: `resource-limit` past [`MAX_REPAIR_OPERATIONS`] rows or
    /// [`path::POINTS_PER_STROKE`] positions, `validation` for a row out of its declared ranges.
    /// The module validated its payload already; the host checks again here because it evaluates
    /// what it is handed, not what a module promised.
    pub(crate) fn new(operations: Vec<RepairOperation>) -> Result<Self, Error> {
        let table = Self {
            operations: operations.into(),
        };
        table.validate()?;
        Ok(table)
    }

    /// The neutral table: no rows, which the host drops, for the module's emptied layer.
    pub(crate) fn neutral() -> Self {
        Self::default()
    }

    /// The checks [`Self::new`] makes, for the host to repeat on a table it was handed.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.operations.len() > MAX_REPAIR_OPERATIONS {
            return Err(Error::resource_limit(format!(
                "a repair layer holds {} operations; {MAX_REPAIR_OPERATIONS} is a photograph's limit",
                self.operations.len()
            )));
        }
        let mut ids = std::collections::HashSet::with_capacity(self.operations.len());
        for operation in self.operations.iter() {
            operation.check()?;
            if !ids.insert(operation.id) {
                return Err(Error::validation(format!(
                    "a repair layer holds two operations numbered {}",
                    operation.id
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn operations(&self) -> &[RepairOperation] {
        &self.operations
    }

    pub fn len(&self) -> usize {
        self.operations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    fn spot(id: u32) -> RepairOperation {
        RepairOperation {
            id,
            kind: RepairKind::Heal,
            points: Arc::from([[8192, 8192]]),
            radius: 160,
            feather: 50,
            opacity: 100,
            offset: [1024, 0],
            patch: None,
        }
    }

    #[test]
    fn a_table_holds_at_most_the_declared_operations() {
        let rows: Vec<_> = (1..=MAX_REPAIR_OPERATIONS as u32).map(spot).collect();
        assert_eq!(RepairTable::new(rows.clone()).unwrap().len(), 100);
        let mut over = rows;
        over.push(spot(101));
        let error = RepairTable::new(over).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert!(error.detail.contains("100"), "{}", error.detail);
    }

    #[test]
    fn out_of_range_rows_are_refused_by_name() {
        let refuse = |row: RepairOperation, kind: ErrorKind| {
            let error = RepairTable::new(vec![row]).unwrap_err();
            assert_eq!(error.kind, kind, "{}", error.detail);
            assert!(error.detail.contains("Heal 1"), "{}", error.detail);
        };
        refuse(
            RepairOperation {
                offset: [0, 0],
                ..spot(1)
            },
            ErrorKind::Validation,
        );
        refuse(
            RepairOperation {
                radius: 0,
                ..spot(1)
            },
            ErrorKind::Validation,
        );
        refuse(
            RepairOperation {
                opacity: 101,
                ..spot(1)
            },
            ErrorKind::Validation,
        );
        refuse(
            RepairOperation {
                points: vec![[0, 0]; path::POINTS_PER_STROKE + 1].into(),
                ..spot(1)
            },
            ErrorKind::ResourceLimit,
        );
        let error = RepairTable::new(vec![spot(3), spot(3)]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
    }
}
