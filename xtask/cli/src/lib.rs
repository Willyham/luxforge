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
#[path = "../../src/onnxruntime.rs"]
pub mod onnxruntime;
#[path = "../../src/policy.rs"]
pub mod policy;
pub mod release;
pub mod release_artifacts;
pub mod release_candidate;
pub mod release_tag;
pub mod release_verify;
#[path = "../../src/repository.rs"]
pub mod repository;
pub mod update;
pub mod workflows;

pub const HELP: &str = "cargo xtask auto-tone-fit --manifest FILE --output NEW_FILE --consent-owner-photos [--rounds 1..6]|doctor|check [--quick]|check-repository|fmt|lint|test [--quick|--native-watchers]|build [--release]|develop [--debug] [--background] [app args]|fixtures|generate-fixtures [--output NEW]|generate-catalog --output NEW [--files N] [--assets M] [--images N] [--seed N]|gazetteer --source cities15000.txt --output NEW|audit|onnxruntime-attestation --output NEW|raw-camera-metadata --index FILE --ids ID[,ID...] --output NEW [--max-source-mib N]|inspect-dng --source DNG [--json NEW]|raw-authentic --manifest FILE --output NEW|editor-acceptance --output NEW|editor-performance --source JPEG --output NEW [--samples N] [--lens-only (JPEG or RAW)]|detail-performance --source JPEG --output NEW [--samples N] [--case all|render|export|points|sharing]|detail-grid-performance --source JPEG --output NEW [--samples N]|editor-latency --source JPEG_OR_RAW --output NEW [--binary PATH] [--samples N] [--mode drag|commit|burst|paint|hover|crop-start] [--mask-overlay] [--zoom PERCENT] [--moving-pan] [--control slider|curve|wheel] [--action ID --parameter NAME (a field-patch slider, or with --control curve a module curve such as set-curve luminance)] [--crop DEGREES] [--basic] [--presence] [--curve-layer] [--detail] [--lens] [--perspective] [--mask] [--idle] [--warm MS] [--contend N] [--reference-renderer] [--usage off|fast|slow|offline]|lens-qualification --manifest FILE --edges FILE --output NEW|lensfun-import --source DIR --output DIR|icon (macOS: regenerates packaging/icon from its SVG)|inventory --output NEW|package --output NEW|release TAG [--dry-run]|release-tag --tag TAG [--github-output FILE]|release-build --tag TAG --build-number N --output NEW [--allow-dirty]|release-sign --output CANDIDATE --identity ID --keychain-profile PROFILE [--keychain FILE]|release-assemble --tag TAG --input BUILDS --output NEW|release-gate --packet DIR --gate GATE --status not-run|passed|failed|incomplete [--evidence REF --scope TEXT] [--disposition distribution|development-only] [--bundle distributed|background-test] [--executable-identity final|before-signing] [--executable NAME=SHA256]...|release-verify --packet DIR [--summary NEW_FILE]|update-journey --output NEW (macOS)|update-record --tag TAG --input BUILDS --output NEW_FILE --key-file FILE|update-feed --records DIR --published FILE --output NEW_DIR [--withdrawn FILE] [--repository OWNER/NAME] [--deploy NEW_FILE.tar]|smoke --list|smoke --output NEW [--scenario NAME] [--binary PATH] [--source RAW (the scenarios --list shows taking one)] [--manifest FILE (the scenarios --list shows needing one)] [--editor-software-adapter]|smoke --verify-only RUN_DIR --output NEW [--scenario NAME] [--source RAW]|verify --output NEW [--tier quick|rendered|timing|full] [--jobs N] [--binary PATH] [--manifest FILE]|check-capture --image PNG [--orientation N] [--aspect R] [--columns LEFT,RIGHT]|preview-error (--candidate PNG --reference PNG --photo-rect LEFT,TOP,RIGHT,BOTTOM | --evidence DIR --candidate-frame N --reference-frame N) [--class pointwise|spatial] [--output NEW_FILE]|preview-corpus [--manifest FILE] [--output NEW_FILE]|gpu-qualification --output NEW [--manifest FILE] [--fixtures DIR] [--zoom fit|33|50|100|all] [--kind picture-at-rest|picture-in-motion|histogram|sample|export|all] [--families F,...] [--recipes ID,...] [--sources ID,...] [--frames missed|all] [--gate-motion against-rest|against-reference]|grade-performance --source JPEG_OR_RAW --output NEW [--samples N]|grade-align generate --output NEW|grade-align render --round DIR --output NEW|grade-align measure --round DIR --output NEW [--lightroom DIR]|lr-align generate --round 1|2 --output NEW [--corpus CACHE] [--manifest RAW_MANIFEST]|hardening --binary PATH --output NEW|measure --binary PATH --output NEW [--samples N]|catalog-measure --output NEW [--samples N] [--scale tiny|full] [--binary PATH] [--raw-corpus DIR] [--card DIR]|usage-measure --output NEW --source JPEG [--source JPEG]... [--binary PATH] [--samples N] [--repeats N] [--only producer,sampler,owner,drag,presence,quit] (in-process components need --release)|hdr-merge-measure --output NEW [--workloads 24mp-3,60mp-9,24mp-9,60mp-3] [--cold N] [--warm N] [--cancellations N] [--rounds N] [--binary PATH] [--natural DIR]... [--only cold,warm,reopen,editor-drag,natural] [--scale tiny|full] (needs --release but at --scale tiny)";
