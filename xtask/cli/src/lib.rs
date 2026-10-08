//! Repository, dependency-policy and Cargo tooling, independent of the photo core.
mod support;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
pub use support::*;
#[path = "../../src/check.rs"]
pub mod check;
#[path = "../../src/policy.rs"]
pub mod policy;
#[path = "../../src/repository.rs"]
pub mod repository;

pub const HELP: &str = "cargo xtask auto-tone-fit --manifest FILE --output NEW_FILE --consent-owner-photos [--rounds 1..6]|doctor|check [--quick]|check-repository|fmt|lint|test [--quick]|build [--release]|develop [--debug] [--background] [app args]|fixtures|generate-fixtures [--output NEW]|generate-catalog --output NEW [--files N] [--assets M] [--images N] [--seed N]|gazetteer --source cities15000.txt --output NEW|audit|raw-camera-metadata --index FILE --ids ID[,ID...] --output NEW [--max-source-mib N]|inspect-dng --source DNG [--json NEW]|raw-authentic --manifest FILE --output NEW|editor-acceptance --output NEW|editor-performance --source JPEG --output NEW [--samples N] [--lens-only (JPEG or RAW)]|detail-performance --source JPEG --output NEW [--samples N] [--case all|render|export|points|sharing]|detail-grid-performance --source JPEG --output NEW [--samples N]|editor-latency --source JPEG_OR_RAW --output NEW [--binary PATH] [--samples N] [--mode drag|commit|burst|paint|hover|crop-start] [--mask-overlay] [--zoom PERCENT] [--moving-pan] [--control slider|curve] [--action ID --parameter NAME (a field-patch slider, or with --control curve a module curve such as set-curve luminance)] [--crop DEGREES] [--basic] [--presence] [--curve-layer] [--detail] [--lens] [--perspective] [--mask] [--idle] [--warm MS] [--contend N] [--reference-renderer]|lens-qualification --manifest FILE --edges FILE --output NEW|lensfun-import --source DIR --output DIR|inventory --output NEW|package --output NEW|smoke --list|smoke --output NEW [--scenario NAME] [--binary PATH] [--source RAW (the scenarios --list shows taking one)] [--manifest FILE (the scenarios --list shows needing one)] [--editor-software-adapter]|smoke --verify-only RUN_DIR --output NEW [--scenario NAME] [--source RAW]|verify --output NEW [--tier quick|rendered|timing|full] [--jobs N] [--binary PATH] [--manifest FILE]|check-capture --image PNG [--orientation N] [--aspect R] [--columns LEFT,RIGHT]|preview-error (--candidate PNG --reference PNG --photo-rect LEFT,TOP,RIGHT,BOTTOM | --evidence DIR --candidate-frame N --reference-frame N) [--class pointwise|spatial] [--output NEW_FILE]|preview-corpus [--manifest FILE] [--output NEW_FILE]|gpu-qualification --output NEW [--manifest FILE] [--fixtures DIR] [--zoom fit|33|50|100|all] [--kind picture-at-rest|picture-in-motion|histogram|sample|export|all] [--families F,...] [--recipes ID,...] [--sources ID,...] [--frames missed|all] [--gate-motion against-rest|against-reference]|hardening --binary PATH --output NEW|measure --binary PATH --output NEW [--samples N]|catalog-measure --output NEW [--samples N] [--scale tiny|full] [--binary PATH] [--raw-corpus DIR] [--card DIR]";
