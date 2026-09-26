//! The DNG opcodes Luxforge implements: the one allowlist that decode, the camera catalog's
//! validation and correction parsing read. It names algorithm capabilities, not camera policy.
//! Dependency-free, because the build script validates the catalog with it too.

/// The TIFF tags of the three DNG opcode lists.
pub(crate) const OPCODE_LIST1: u16 = 51008;
pub(crate) const OPCODE_LIST2: u16 = 51009;
pub(crate) const OPCODE_LIST3: u16 = 51022;

/// The one opcode version the implementations follow (DNG 1.3).
pub(crate) const VERSION: u32 = 0x0103_0000;

/// An implemented operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opcode {
    WarpRectilinear,
    FixVignetteRadial,
    GainMap,
    FixBadPixelsConstant,
    FixBadPixelsList,
}

/// Each implemented operation with the list it runs from and its DNG opcode ID.
const IMPLEMENTED: [(u16, u32, Opcode); 5] = [
    (OPCODE_LIST3, 1, Opcode::WarpRectilinear),
    (OPCODE_LIST3, 3, Opcode::FixVignetteRadial),
    (OPCODE_LIST3, 9, Opcode::GainMap),
    (OPCODE_LIST1, 4, Opcode::FixBadPixelsConstant),
    (OPCODE_LIST1, 5, Opcode::FixBadPixelsList),
];

impl Opcode {
    /// The operation an entry of opcode list `list` with ID `id` is, when Luxforge implements
    /// it there. Anything else is unsupported.
    pub(crate) fn implemented(list: u16, id: u32) -> Option<Self> {
        IMPLEMENTED
            .iter()
            .find(|(implemented_list, implemented_id, _)| {
                (*implemented_list, *implemented_id) == (list, id)
            })
            .map(|(_, _, opcode)| *opcode)
    }

    /// A repair of the retained mosaic, applied during development rather than to its planes.
    pub(crate) fn repairs_sensor(self) -> bool {
        matches!(self, Self::FixBadPixelsConstant | Self::FixBadPixelsList)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_implemented_list_and_id_pairs_are_allowed() {
        let mut allowed = Vec::new();
        for list in [OPCODE_LIST3, OPCODE_LIST2, OPCODE_LIST1] {
            for id in 0..=14 {
                if let Some(opcode) = Opcode::implemented(list, id) {
                    allowed.push((list, id, opcode));
                }
            }
        }
        assert_eq!(allowed, IMPLEMENTED);
        assert_eq!(
            IMPLEMENTED.map(|(_, _, opcode)| opcode.repairs_sensor()),
            [false, false, false, true, true]
        );
    }
}
