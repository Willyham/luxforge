//! Any other platform: only the build's architecture is known.
use super::{OsFamily, Raw};

pub(super) const FAMILY: OsFamily = OsFamily::Other;

pub(super) fn read() -> Raw {
    Raw::default()
}
