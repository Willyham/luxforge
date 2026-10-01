use crate::*;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
fn schema(value: &Value, s: &Value, root: &Value) -> Result {
    if let Some(r) = s["$ref"].as_str() {
        return schema(
            value,
            root.pointer(r.trim_start_matches('#'))
                .ok_or("Unknown schema reference")?,
            root,
        );
    }
    if let Some(k) = s["type"].as_str() {
        ensure(
            match k {
                "object" => value.is_object(),
                "array" => value.is_array(),
                "string" => value.is_string(),
                "integer" => value.is_i64() || value.is_u64(),
                _ => false,
            },
            format!("Schema type {k}"),
        )?;
    }
    if let Some(c) = s.get("const") {
        ensure(value == c, "Schema constant")?;
    }
    if let Some(e) = s["enum"].as_array() {
        ensure(e.contains(value), format!("Schema enum: {value}"))?;
    }
    if let Some(obj) = value.as_object() {
        if let Some(required) = s["required"].as_array() {
            for k in required {
                ensure(
                    obj.contains_key(k.as_str().ok_or("Schema key")?),
                    format!("Missing {k}"),
                )?
            }
        }
        for (k, v) in obj {
            if let Some(child) = s["properties"].get(k) {
                schema(v, child, root)?
            } else {
                ensure(
                    s["additionalProperties"] != false,
                    format!("Unexpected field {k}"),
                )?
            }
        }
    }
    if let Some(a) = value.as_array() {
        ensure(
            a.len() >= s["minItems"].as_u64().unwrap_or(0) as usize,
            "Too few array items",
        )?;
        if s["uniqueItems"] == true {
            let set: BTreeSet<_> = a.iter().map(Value::to_string).collect();
            ensure(set.len() == a.len(), "Duplicate items")?
        }
        for v in a {
            if let Some(items) = s.get("items") {
                schema(v, items, root)?
            }
        }
    }
    if let Some(v) = value.as_str() {
        ensure(
            v.chars().count() >= s["minLength"].as_u64().unwrap_or(0) as usize,
            "Empty string",
        )?;
        if let Some(p) = s["pattern"].as_str() {
            ensure(Regex::new(p)?.is_match(v), "Schema pattern")?;
        }
    }
    if let Some(min) = s["minimum"].as_i64() {
        ensure(value.as_i64().is_some_and(|v| v >= min), "Below minimum")?;
    }
    Ok(())
}
fn plan<'a>(root: &Path, p: &'a Value, s: &Value) -> Result<BTreeMap<&'a str, &'a Value>> {
    schema(p, s, s)?;
    let array = p["tasks"].as_array().ok_or("Tasks array")?;
    let tasks: BTreeMap<_, _> = array
        .iter()
        .map(|t| (t["id"].as_str().unwrap(), t))
        .collect();
    ensure(tasks.len() == array.len(), "Duplicate task IDs")?;
    for (index, task) in array.iter().enumerate() {
        ensure(
            task["id"] == format!("TASK-{:03}", index + 1),
            "Tasks must be ordered with contiguous local IDs starting at TASK-001",
        )?;
    }
    for (id, t) in &tasks {
        for d in t["dependencies"].as_array().unwrap() {
            let dep = d.as_str().unwrap();
            ensure(dep != *id, "Self dependency")?;
            ensure(
                dep.trim_start_matches("TASK-").parse::<usize>()?
                    < id.trim_start_matches("TASK-").parse::<usize>()?,
                format!("{id}: dependencies must precede their consumer"),
            )?;
            let target = tasks.get(dep).ok_or("Unknown dependency")?;
            if matches!(
                t["status"].as_str(),
                Some("ready" | "in_progress" | "completed")
            ) {
                ensure(
                    target["status"] == "completed",
                    format!("{id}: unmet prerequisite {dep}"),
                )?;
            }
        }
        if matches!(t["status"].as_str(), Some("blocked" | "cancelled")) {
            ensure(
                !t["extra_context"].as_array().unwrap().is_empty(),
                "Missing status explanation",
            )?;
        }
        for link in t["context_links"].as_array().unwrap() {
            let target = link["target"].as_str().unwrap();
            match link["kind"].as_str() {
                Some("file") => {
                    let linked = root.join(target.split('#').next().unwrap());
                    ensure(linked.is_file(), format!("{id}: missing {target}"))?;
                    if linked
                        .extension()
                        .is_some_and(|extension| extension == "json")
                    {
                        ensure(
                            !linked
                                .canonicalize()?
                                .starts_with(root.join("tasks").canonicalize()?),
                            format!(
                                "{id}: task files cannot reference other task plans; link a specification"
                            ),
                        )?;
                    }
                }
                Some("task") => ensure(tasks.contains_key(target), "Unknown linked task")?,
                _ => (),
            }
        }
    }
    let mut done = BTreeSet::new();
    let mut waves = Vec::new();
    while done.len() < tasks.len() {
        let ready: Vec<_> = tasks
            .iter()
            .filter(|(id, t)| {
                !done.contains(**id)
                    && t["dependencies"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|d| done.contains(d.as_str().unwrap()))
            })
            .map(|(id, _)| *id)
            .collect();
        ensure(!ready.is_empty(), "Task cycle")?;
        waves.push(json!({"wave":waves.len()+1,"task_ids":ready}));
        done.extend(ready);
    }
    ensure(
        p["execution_waves"] == json!(waves),
        "Stale execution waves",
    )?;
    Ok(tasks)
}
fn active_plans(root: &Path, plans: &[Value], s: &Value) -> Result<Vec<String>> {
    ensure(!plans.is_empty(), "No active task plans")?;
    let mut plan_ids = BTreeSet::new();
    let mut summaries = Vec::new();
    for value in plans {
        let tasks = plan(root, value, s)?;
        let plan_id = value["plan_id"].as_str().ok_or("Missing plan ID")?;
        ensure(
            plan_ids.insert(plan_id),
            format!("Duplicate plan ID {plan_id}"),
        )?;
        summaries.push(format!("{plan_id} {}", tasks.len()));
    }
    Ok(summaries)
}
// The repository rules: what source text may say where, which crate may depend on what, and which
// messages product code must send. Each rule is one row of `SOURCE_RULES`, `DEPENDENCY_RULES` or
// `SENDER_RULES`, served by one token matcher (`holds_token`) and one test-exclusion parser
// (`production_lines`). A task that finishes a concept adds the row that keeps it single; it never
// writes a bespoke check. To add a rule, copy the row nearest in shape, give it a new `name`, and
// add a test with an allowed and a refused path.

/// The file holding the rule tables names every refused token in its rows and tests, so no source
/// rule reads it.
const RULES_FILE: &str = "xtask/src/repository.rs";

/// How a source rule finds a token in a line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Match {
    /// A whole token: where the token starts with an identifier character it may not continue an
    /// identifier before it, and where it ends with one no identifier may continue after it. So
    /// `app::` finds `crate::app::State` but not `snapp::x`, and `RAW_EFFECT` misses `RAW_EFFECTS`.
    Whole,
    /// The start of a token, which may run on into a longer identifier: `mozjpeg` finds
    /// `mozjpeg_sys`, and `app::` still misses `snapp::x`.
    Prefix,
}

/// A rule about which files may hold which tokens.
struct SourceRule {
    /// The rule's name, printed with each refusal and unique across both tables.
    name: &'static str,
    /// What the rule refuses outside its allowed paths.
    tokens: &'static [&'static str],
    /// The directories read, relative to the root and recursively.
    scope: &'static [&'static str],
    /// The file extensions read in them. Rules filter by type because `crates/luxforge-raw/vendor`
    /// holds LibRaw's C++ sources.
    types: &'static [&'static str],
    /// The paths that may hold the tokens, relative to the root: a file, a directory and everything
    /// below it, or a module path without its extension (`modules/raw` for `modules/raw.rs` and
    /// everything under `modules/raw/`).
    allowed: &'static [&'static str],
    /// How the tokens are found.
    mode: Match,
    /// Whether test code is held to the rule too. When it is, every line of every file in scope is
    /// read, comments included. When it is not, a file that is a test by name ([`test_file`]) or
    /// a module declared under `#[cfg(test)]` (with everything below its directory) is skipped,
    /// and each other file is read through [`production_lines`], which also skips comment lines.
    tests: bool,
    /// Whether each allowed path may hold each token on one line only: an expression written
    /// once in its home, so a second copy beside the first is refused as one elsewhere is. An
    /// allowed directory counts each file under it as a home of its own.
    once: bool,
    /// Why the rule holds, printed with each refusal.
    reason: &'static str,
}

/// Every text file type a repository-wide rule reads.
const TEXT: &[&str] = &["rs", "toml", "md", "json", "wgsl", "txt"];

/// The source directories of the crates a shipped binary links.
const SHIPPED_SOURCES: &[&str] = &[
    "crates/luxforge-core/src",
    "crates/luxforge-net/src",
    "crates/luxforge-app/src",
    "crates/luxforge-cli/src",
    "crates/luxforge-ui/src",
    "crates/luxforge-raw/src",
    "crates/luxforge-process/src",
    "crates/luxforge-watch/src",
    "crates/luxforge-evidence/src",
    "crates/luxforge-jpeg/src",
];

/// The crates a shipped binary links, whose normal dependencies the JPEG rules hold.
const SHIPPED_CRATES: &[&str] = &[
    "crates/luxforge-core",
    "crates/luxforge-net",
    "crates/luxforge-app",
    "crates/luxforge-cli",
    "crates/luxforge-ui",
    "crates/luxforge-raw",
    "crates/luxforge-process",
    "crates/luxforge-watch",
    "crates/luxforge-evidence",
    "crates/luxforge-jpeg",
];

const SOURCE_RULES: &[SourceRule] = &[
    // The desktop's layering: the view model reaches no framework, not the widget crate, not the
    // view that draws it and not the update layer above it (`app/`, which depends on it). `app::`
    // and `view::` catch `crate::app::`, `super::view::` and a grouped `use crate::{ view::... }`
    // line alike, and nothing that merely ends in `app` or `view` (`preview::`, `canvas_view::`).
    // The window and panel arithmetic both sides need is the framework-free `layout.rs`.
    SourceRule {
        name: "state-layer",
        tokens: &[
            "use iced",
            "iced::",
            "iced_runtime",
            "app::",
            "luxforge_ui",
            "view::",
        ],
        scope: &["crates/luxforge-app/src/state"],
        types: &["rs", "toml"],
        allowed: &[],
        mode: Match::Prefix,
        tests: true,
        once: false,
        reason: "the view model reaches neither Iced, the widget crate (luxforge_ui), the view \
                 (`view/`) nor the update layer (`app/`) above it",
    },
    // The view, the two canvases and the view transform they draw through included.
    SourceRule {
        name: "view-layer",
        tokens: &["luxforge_core", "OwnerHandle", ".call("],
        scope: &["crates/luxforge-app/src/view"],
        types: &["rs", "toml"],
        allowed: &[],
        mode: Match::Prefix,
        tests: true,
        once: false,
        reason: "the view reads the view model, never authoritative state or the owner",
    },
    SourceRule {
        name: "widget-crate",
        tokens: &["luxforge_core"],
        scope: &["crates/luxforge-ui"],
        types: &["rs", "toml"],
        allowed: &[],
        mode: Match::Prefix,
        tests: true,
        once: false,
        reason: "the widget crate (luxforge-ui) never reaches the core",
    },
    // The desktop keeps no stack between messages. An evaluation holds its source, and a RAW
    // source's developed planes hold the source worker's memory gate, so one kept in the desktop's
    // state keeps the next development — a white-balance change, a history selection, another
    // photograph — from ever starting. A stack reaches the desktop only inside the preview job that
    // carries it to a worker; each coverage worker's job type is the one line that names it.
    SourceRule {
        name: "desktop-keeps-no-stack",
        tokens: &["Evaluation"],
        scope: &["crates/luxforge-app/src"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-app/src/app/thumbnails.rs",
            "crates/luxforge-app/src/app/mask_coverage.rs",
        ],
        mode: Match::Whole,
        tests: false,
        once: true,
        reason: "the desktop keeps no evaluation between messages: it holds its source, and a RAW \
                 development's planes hold the source worker's memory gate; only the thumbnail \
                 and mask coverage workers' job types name one",
    },
    // Nor a planned preview job, which carries its stack. The desktop names `PreviewJob` only in
    // the files that pass one straight through: the message files that carry it (app/message.rs
    // and each seam's app/message/<variant>.rs, such as the crop's `PreviewReady`), the
    // owner tasks that plan and answer with it (app/tasks.rs), the preview request that hands it to
    // the worker (app/preview.rs), and the two answers that read one on its way there, a
    // `draft.set`'s (app/gesture.rs) and the thumbnails' (app/thumbnails.rs). A crop draft keeps
    // frames only; the editor, the view model and the other gestures name no job at all. A token
    // rule reads names, not types: it cannot see a job kept inside one of those files, nor one
    // kept inside a carrier type that holds one (`Refresh`, `PreviewPayload`).
    SourceRule {
        name: "desktop-keeps-no-preview-job",
        tokens: &["PreviewJob"],
        scope: &["crates/luxforge-app/src"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-app/src/app/message",
            "crates/luxforge-app/src/app/tasks.rs",
            "crates/luxforge-app/src/app/preview.rs",
            "crates/luxforge-app/src/app/gesture.rs",
            "crates/luxforge-app/src/app/thumbnails.rs",
            "crates/luxforge-app/src/app/mask_coverage.rs",
        ],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "the desktop keeps no planned preview job between messages: it holds its stack; \
                 only the messages, the owner tasks, the preview request and the answers that \
                 pass one straight to a worker name one",
    },
    // One JPEG codec path: in shipped code only `luxforge-jpeg` names `mozjpeg` (and
    // `mozjpeg_sys`), and nothing decodes JPEG through `image`. Tests may, as an independent
    // decoder.
    SourceRule {
        name: "jpeg-codec-name",
        tokens: &["mozjpeg"],
        scope: SHIPPED_SOURCES,
        types: &["rs"],
        allowed: &["crates/luxforge-jpeg"],
        mode: Match::Prefix,
        tests: false,
        once: false,
        reason: "only the JPEG codec crate (crates/luxforge-jpeg) may name mozjpeg",
    },
    SourceRule {
        name: "jpeg-through-codec",
        tokens: &[
            "codecs::jpeg",
            "ImageFormat::Jpeg",
            "JpegDecoder",
            "image::open",
            "load_from_memory",
            "ImageReader",
        ],
        scope: SHIPPED_SOURCES,
        types: &["rs"],
        allowed: &[],
        mode: Match::Prefix,
        tests: false,
        once: false,
        reason: "shipped code decodes JPEG only through crates/luxforge-jpeg, never through image",
    },
    // The RAW development's identity belongs to the RAW module alone: every other surface decides
    // whether a module applies to a photo from the source kinds its effects declare, and the host's
    // RAW source reads its layer through the module's own helper. The harness under `xtask/`
    // drives the module as an API client does and is not product code.
    SourceRule {
        name: "raw-identity",
        tokens: &["\"luxforge.raw\"", "RAW_EFFECT"],
        scope: &["crates"],
        types: &["rs"],
        allowed: &["crates/luxforge-core/src/modules/raw"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "only the RAW module (crates/luxforge-core/src/modules/raw*) and tests may name \
                 its identity; decide applicability from the declared sources",
    },
    // One answer to "is this a presettable action": `ModuleRegistry::patch_action` words the
    // refusal, and every caller resolves through it, so a second copy of the check fails here.
    SourceRule {
        name: "presettable-action",
        tokens: &["is not a field-patch action"],
        scope: &["crates"],
        types: &["rs"],
        allowed: &["crates/luxforge-core/src/modules/registry/lookups.rs"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "only ModuleRegistry::patch_action decides whether an action is presettable; \
                 resolve it there",
    },
    // The facts a client acts on are the refusal's code and data, never its message: the
    // unavailable-effect refusal is worded once, by `Error::unavailable_effect`, and the full
    // source queues by their two producers through `Error::source_queue_full`. So no client — the
    // desktop above all — matches that text, and no second producer words it without the data.
    SourceRule {
        name: "refusal-text",
        tokens: &["\"unavailable effect", "queue is full"],
        scope: &["crates"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-core/src/error.rs",
            "crates/luxforge-core/src/api/owner.rs",
        ],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "the core words these refusals once, with their facts in data \
                 (Error::unavailable_effect, Error::source_queue_full); read the code and \
                 data (unavailable_effect_id, retries_after_source_job), never the message",
    },
    // A mask component kind is dispatched only through the host's kind table (`mask/mod.rs`) or,
    // on the desktop, the drawn-kind table (`mask_draft/editor.rs`); each kind's own file declares
    // its token, and everything else asks a table (`component_geometry_is_drawn`, `stroke_kind`,
    // `paintable`, `painted_kind`). The names are the constants those files declare a kind's token
    // in, and the two range tokens no other vocabulary spells. The bare words `"linear"`,
    // `"radial"` and `"brush"` are not matched: they also name a colour domain, a fixture's seed
    // and the panel's Brush section in the state capture.
    SourceRule {
        name: "component-kind",
        tokens: &[
            "BRUSH",
            "LINEAR",
            "RADIAL",
            "KIND",
            "LUMINANCE_KIND",
            "COLOUR_KIND",
            "\"luminance-range\"",
            "\"colour-range\"",
        ],
        scope: &["crates"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-core/src/mask/mod.rs",
            "crates/luxforge-core/src/mask/linear.rs",
            "crates/luxforge-core/src/mask/radial.rs",
            "crates/luxforge-core/src/mask/brush.rs",
            "crates/luxforge-core/src/mask/range.rs",
            "crates/luxforge-app/src/mask_draft/editor.rs",
            "crates/luxforge-app/src/mask_draft/linear.rs",
            "crates/luxforge-app/src/mask_draft/radial.rs",
            "crates/luxforge-app/src/mask_draft/brush.rs",
        ],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "only the mask kind table, a kind's own file and the desktop's drawn-kind table and \
                 editors may name a component kind; ask the kind table instead",
    },
    // One pixel-domain pipeline: the tap index of a resample coordinate, `(value - 0.5).floor()`,
    // is what every read rectangle computes, and keying the estimate store by the domain's prefix
    // is what every spatial-entry orchestration does, so each is written once in its home. The
    // trait's `fn estimate_prefix` declaration and the windowed proxy's own halo and tile rule
    // (`WindowPlan::of_rect`) are not copies.
    SourceRule {
        name: "one-read-rectangle",
        tokens: &["- 0.5).floor()"],
        scope: &["crates/luxforge-core/src"],
        types: &["rs"],
        allowed: &["crates/luxforge-core/src/render/geometry.rs"],
        mode: Match::Whole,
        tests: false,
        once: true,
        reason: "a resample's read rectangle is written once, as Resample::reads in \
                 render/geometry.rs; read it through that",
    },
    SourceRule {
        name: "one-spatial-entry",
        tokens: &[".estimate_prefix("],
        scope: &["crates/luxforge-core/src"],
        types: &["rs"],
        allowed: &["crates/luxforge-core/src/render/pipeline.rs"],
        mode: Match::Whole,
        tests: false,
        once: true,
        reason: "a spatial entry's estimates are written once, as SpatialEntry::globals in \
                 render/pipeline.rs; resolve them through that",
    },
    // One field-patch semantics: the field-patch module builds every patch action from its spec,
    // and the RAW module's `set-raw` keeps its own white-balance merge. A module that wants a patch
    // declares a `field_patch::Spec` instead of hand-writing merge and canonical form. The tokens
    // are a patch action's declaration in Rust and in a JSON descriptor.
    SourceRule {
        name: "patch-action",
        tokens: &["patch: true"],
        scope: &["crates"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-core/src/modules/field_patch.rs",
            "crates/luxforge-core/src/modules/raw.rs",
        ],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "only the field-patch module and the RAW module declare a patch action; declare a \
                 field-patch Spec instead of a second patch implementation",
    },
    // One job table: every job kind (source, analysis, capability and export) is one record in
    // one table with one retention, and the catalog owner creates that table once.
    SourceRule {
        name: "job-records",
        tokens: &["VecDeque<JobId>"],
        scope: &["crates/luxforge-core/src"],
        types: &["rs"],
        allowed: &["crates/luxforge-core/src/jobs.rs"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "only the one job table (crates/luxforge-core/src/jobs.rs) keeps finished job \
                 records; register the job kind there",
    },
    SourceRule {
        name: "job-table",
        tokens: &["Jobs::new"],
        scope: &["crates/luxforge-core/src"],
        types: &["rs"],
        allowed: &["crates/luxforge-core/src/api/owner.rs"],
        mode: Match::Whole,
        tests: false,
        once: true,
        reason: "only the catalog owner (crates/luxforge-core/src/api/owner.rs) creates the job \
                 table, once",
    },
    // One envelope check: the dispatcher checks every method's mutation envelope once, before any
    // handler runs, so no handler, service or store checks its own.
    SourceRule {
        name: "one-envelope-check",
        tokens: &["mutation.validate()"],
        scope: &["crates/luxforge-core/src"],
        types: &["rs"],
        allowed: &["crates/luxforge-core/src/api/params.rs"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "the dispatcher checks every mutation envelope once, before any handler runs \
                 (Envelope::check in crates/luxforge-core/src/api/params.rs); a handler never \
                 checks its own",
    },
    // One source preparation path: an original is read only by the source work's file preparation
    // and an artifact only by its verified read, both run by `SourceWork::run` in
    // `editor/source.rs`, which the catalog owner's source worker and the blocking helpers
    // (`EditorService::import`, `EditorService::prepare`) share. No service mode reads inline on a
    // cache miss, test code included: a test prepares through the helpers, never by hand. The
    // bounded read and the verified read are defined in `source.rs` and `artifacts/`, and `lib.rs`
    // re-exports the first. The other readers are the catalog's preview lane, whose design has
    // it read browsed files off the editor's source cache on its own workers: its extraction reads
    // a browsed JPEG for its grid and loupe tiers, and its 100% region a JPEG or a RAW, one RAW
    // development at a time (`docs/design/catalog.md`, "The index and previews cache" and "The
    // 100% region").
    SourceRule {
        name: "one-source-preparation",
        tokens: &[
            "allow_sync_source",
            "prepare_file",
            "read_bounded_file",
            "read_verified",
        ],
        scope: &["crates/luxforge-core/src"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-core/src/editor/source.rs",
            "crates/luxforge-core/src/source.rs",
            "crates/luxforge-core/src/artifacts",
            "crates/luxforge-core/src/lib.rs",
            "crates/luxforge-core/src/previews/extract.rs",
            "crates/luxforge-core/src/previews/region.rs",
        ],
        mode: Match::Whole,
        tests: true,
        once: false,
        reason: "only the source work (SourceWork::run in crates/luxforge-core/src/editor/source.rs) \
                 reads an original or an artifact for the editor, for the source worker and the \
                 blocking helpers alike, and only the catalog's preview lane \
                 (crates/luxforge-core/src/previews/extract.rs and region.rs) reads a browsed \
                 file off the editor's cache; prepare through EditorService::prepare or import, \
                 never inline",
    },
    // The desktop reads a committed crop, the stage it receives and the orientation ahead of it
    // from `recipe.describe` rows, and folds no geometry itself: its product code names neither
    // the crop nor the orientation effect and deserializes neither payload. Tests may, to check
    // what a commit stored.
    SourceRule {
        name: "desktop-crop-rows",
        tokens: &[
            "CROP_EFFECT",
            "ORIENTATION_EFFECT",
            "from_value::<CropPayload>",
            "from_value::<Orientation>",
        ],
        scope: &["crates/luxforge-app/src"],
        types: &["rs"],
        allowed: &[],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "the desktop reads the crop, its stage and the orientation ahead of it from \
                 recipe.describe rows, never from a payload",
    },
    // The crop angle is one declared parameter drawn by the generic stepper, which reads its range,
    // steps and default from the descriptor. Only the frame's own geometry (`crop_draft.rs`)
    // clamps to the core's angle constants; a hand-built angle control that reached for them would
    // be a second path.
    SourceRule {
        name: "declared-crop-angle",
        tokens: &["MIN_ANGLE", "MAX_ANGLE"],
        scope: &["crates/luxforge-app/src"],
        types: &["rs"],
        allowed: &["crates/luxforge-app/src/crop_draft.rs"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "the desktop reads the crop angle's range and steps from its declared parameter \
                 through NumberSpec; only the frame's geometry (crop_draft.rs) clamps to the core's \
                 constants",
    },
    // One start refusal: whether anything may start on the desktop is answered by
    // `Editor::gesture_refusal` in app/gesture.rs, and its editable half is the view model's one
    // editability rule (`state::editable_refusal`, and `state::edit_refusal` for the models). The
    // busy and editable halves are worded once, as constants in state/mod.rs, and a start site or
    // a model writes the reason it is given rather than a sentence of its own. The release
    // refusal's "Return to the current state to apply" is another sentence and not matched.
    SourceRule {
        name: "desktop-start-refusal",
        tokens: &[
            "\"Return to the current state before",
            "\"Waiting for the last request",
        ],
        scope: &["crates/luxforge-app/src"],
        types: &["rs"],
        allowed: &["crates/luxforge-app/src/state/mod.rs"],
        mode: Match::Whole,
        tests: false,
        once: true,
        reason: "a start's refusal is worded once, as the constants beside state::edit_refusal in \
                 crates/luxforge-app/src/state/mod.rs; Editor::gesture_refusal and the models ask \
                 that rule and write the reason it returns",
    },
    // The tools panel model resolves each group's reset for the photo's source kind and the bound
    // target, and the header shows it; `ResetGroup` runs what the section holds
    // (`SectionModel::group_reset`) rather than resolving it a second time.
    SourceRule {
        name: "desktop-group-reset",
        tokens: &["resolve_group_reset"],
        scope: &["crates/luxforge-app/src"],
        types: &["rs"],
        allowed: &["crates/luxforge-app/src/state/tools.rs"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "only the tools panel model (state/tools.rs) resolves a group's reset; read the \
                 one the section resolved through SectionModel::group_reset",
    },
    // Seams report outcomes; only the evidence driver steers a script step. A seam says what
    // happened through `Editor::outcome` (app/outcome.rs), and the evidence driver
    // (app/evidence.rs) matches it to the running step's wait, arms that wait and records a
    // refusal against the step. Tests may arm a wait directly.
    SourceRule {
        name: "evidence-outcomes",
        tokens: &[
            "Settle::",
            "settle_step",
            "await_step",
            "refuse_step",
            "capture_next_frame",
        ],
        scope: &["crates/luxforge-app/src"],
        types: &["rs"],
        // The driver is `evidence.rs` and its own modules (`evidence/`), such as the Select steps.
        allowed: &["crates/luxforge-app/src/app/evidence"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "a desktop seam reports what happened through Editor::outcome \
                 (crates/luxforge-app/src/app/outcome.rs); only the evidence driver \
                 (crates/luxforge-app/src/app/evidence.rs and its modules) names a step's wait and \
                 settles, arms or refuses it",
    },
    // The one-megapixel parallel threshold and the 512 MiB frame limit every per-pixel pass picks
    // its path against are declared once, in luxforge-raw's limits module: luxforge-core depends on
    // luxforge-raw, not the reverse, so that module is the one home both crates can import from.
    // The assignment is matched rather than the bare number, so an unrelated literal (a float
    // tolerance, a loop bound, a sample count) is not mistaken for a duplicate declaration; a
    // coincidental match outside these two crates (luxforge-ui's own texture budget, for one) is a
    // different concept and out of this rule's scope.
    SourceRule {
        name: "render-limits-home",
        tokens: &["= 1_000_000;", "= 512 * 1024 * 1024;"],
        scope: &["crates/luxforge-core/src", "crates/luxforge-raw/src"],
        types: &["rs"],
        allowed: &["crates/luxforge-raw/src/limits.rs"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "the one-megapixel parallel threshold and the 512 MiB frame limit may be declared \
                 only in crates/luxforge-raw/src/limits.rs; import it from there instead",
    },
    // One evaluation rule: the drafted preview's mode, the one evaluation that may approximate a
    // RAW white balance the developed planes do not hold, is decided by the one evaluation builder
    // (`EditorService::evaluation`) and applied by the RAW settings resolver that defines it.
    SourceRule {
        name: "draft-preview-rule",
        tokens: &["RawSettingsMode::DraftPreview"],
        scope: &["crates/luxforge-core/src"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-core/src/editor/evaluate.rs",
            "crates/luxforge-core/src/editor/source.rs",
        ],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "only the evaluation builder (editor/evaluate.rs) decides the drafted preview's \
                 mode, and only the RAW settings resolver (editor/source.rs) applies it; build the \
                 evaluation through EditorService::evaluation",
    },
    // One sRGB reference: `12.92` is the linear-branch slope of both the encode and the decode
    // branch, in every spelling of the transfer function this rule has found (the threshold is
    // written as both `0.04045`/`0.0031308` and `0.040_45`/`0.003_130_8`, but the slope is always
    // `12.92`), so it alone is enough to catch a transcription without also matching every doc
    // comment that merely names the encode or decode threshold.
    SourceRule {
        name: "srgb-transfer-function",
        tokens: &["12.92"],
        scope: &["crates", "xtask"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-core/src/colour.rs",
            "crates/luxforge-reference/src/srgb.rs",
        ],
        mode: Match::Whole,
        tests: true,
        once: false,
        reason: "only the core's production transfer function (crates/luxforge-core/src/colour.rs) \
                 and the one shared test reference (luxforge_reference::srgb) may write the sRGB \
                 transfer function's constants; every other caller, test code included, computes \
                 through one of them",
    },
    // Tests that do not depend on host load: a test orders its steps by a gate or a channel and
    // waits through the one hang-bounded wait, all in `luxforge-testbase`, never by a sleep or a
    // spin of its own. The one production home keeps its one sleep: the widget crate's GPU
    // retirement worker, which may hold it on one line only, so the tests beside it are held to the
    // rule too.
    SourceRule {
        name: "test-waits",
        tokens: &["sleep(", "yield_now"],
        scope: &["crates"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-testbase",
            "crates/luxforge-ui/src/photo_surface.rs",
        ],
        mode: Match::Whole,
        tests: true,
        once: true,
        reason: "a test waits through luxforge_testbase::wait_until (or holds work at a \
                 luxforge_testbase::Gate), never a sleep or spin of its own; extend that crate \
                 instead of writing a second wait",
    },
    SourceRule {
        name: "test-gates",
        tokens: &["Condvar"],
        scope: &["crates"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-testbase",
            // The core's production blocking points: the source worker's plane gate, the
            // latest-job worker, the point-query worker and the 100% region's one RAW
            // development at a time.
            "crates/luxforge-core/src/source.rs",
            "crates/luxforge-core/src/latest.rs",
            "crates/luxforge-core/src/api/owner/point.rs",
            "crates/luxforge-core/src/previews/region.rs",
        ],
        mode: Match::Whole,
        tests: true,
        once: false,
        reason: "a test holds work at the one luxforge_testbase::Gate, never a gate of its own; \
                 extend that crate instead of writing a second gate",
    },
    // One photo locator: a scenario finds the photograph in a capture by the rectangle the editor
    // records per frame, read through `Frame::photo_rect` and the readers over it in
    // `scenario/pixels.rs`, never by indexing the record itself or by searching the capture.
    SourceRule {
        name: "one-photo-locator",
        tokens: &["[\"photo_rect\"]"],
        scope: &["xtask/src"],
        types: &["rs"],
        allowed: &["xtask/src/scenario/pixels.rs"],
        mode: Match::Whole,
        tests: true,
        once: false,
        reason: "a scenario locates the photograph through Frame::photo, Frame::visible_photo, \
                 Frame::photo_edges or Frame::photo_rect in scenario/pixels.rs, never by reading \
                 the frame's photo_rect itself; extend those instead",
    },
    // One percentile definition: every timing figure — xtask's timing tools and the crates' own
    // ignored timing tests alike — is read from `luxforge_testbase::Distribution`'s nearest rank,
    // never from a sort-and-index of its own. The tokens are the shapes each hand-written
    // percentile, median or p50/p95 helper took, and a nearest-rank rank computed again.
    SourceRule {
        name: "one-distribution",
        tokens: &[
            "fn percentile",
            "let percentile",
            "fn median",
            "let median",
            "fn p50",
            "let p50",
            "let p95",
            "div_ceil(100)",
        ],
        scope: &["crates", "xtask"],
        types: &["rs"],
        allowed: &["crates/luxforge-testbase/src/distribution.rs"],
        mode: Match::Prefix,
        tests: true,
        once: false,
        reason: "a percentile, median or p50/p95 is read from luxforge_testbase::Distribution \
                 (xtask writes it through stats::row), never computed by a second definition; \
                 extend that type instead",
    },
    // Production threads start only in the declared worker homes, each a bounded, owned worker.
    SourceRule {
        name: "thread-spawn",
        tokens: &["thread::spawn", "thread::Builder", "thread::scope"],
        scope: &["crates", "xtask"],
        types: &["rs"],
        allowed: &[
            // The core: the source worker and the owner loop, the point-query worker, the API
            // transport's accept and connection threads, the job table's lanes and the
            // latest-job worker.
            "crates/luxforge-core/src/api/owner.rs",
            "crates/luxforge-core/src/api/owner/point.rs",
            "crates/luxforge-core/src/api/transport.rs",
            "crates/luxforge-core/src/jobs.rs",
            "crates/luxforge-core/src/latest.rs",
            // The catalog's bounded workers: the index lane and its watchers, the preview lane,
            // and the develop lane (`docs/design/catalog.md`, "Architecture").
            "crates/luxforge-core/src/index",
            "crates/luxforge-core/src/previews",
            "crates/luxforge-core/src/library",
            // The folder watcher's one thread on Linux (blocking in `poll`) and on Windows
            // (blocking on its completion port); macOS delivers on a dispatch queue instead.
            "crates/luxforge-watch/src/linux.rs",
            "crates/luxforge-watch/src/windows.rs",
            // The desktop's diagnostics log writer.
            "crates/luxforge-app/src/diagnostics.rs",
            // The widget crate's GPU retirement worker.
            "crates/luxforge-ui/src/photo_surface.rs",
            // The test kit's process threads and the test base's server threads.
            "crates/luxforge-testkit/src/process.rs",
            "crates/luxforge-testbase/src/server.rs",
            // `verify`'s component pool, and `check`'s steps and test binaries.
            "xtask/src/verify.rs",
            "xtask/src/check.rs",
        ],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "production threads start only in the declared worker homes",
    },
    // One disk flush: every durable write in the crates reaches the drive through
    // `atomic_file::flush`, and only that file names the test feature that skips it.
    SourceRule {
        name: "one-disk-flush",
        tokens: &["sync_all", "sync_data", "test-skip-disk-flush"],
        scope: &["crates"],
        types: &["rs"],
        allowed: &["crates/luxforge-core/src/atomic_file.rs"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "a durable write flushes only through luxforge-core's atomic_file::flush, the one \
                 place the test-skip-disk-flush feature is read",
    },
    // One way to launch the editor from the harness: the scenario library's `Launch` assembles
    // every argument list and `Run` makes every launch, so each is recorded and watched alike.
    SourceRule {
        name: "editor-launch",
        tokens: &[
            "\"--evidence-dir\"",
            "\"--evidence-script\"",
            "\"--data-root\"",
            "\"--catalog\"",
            "\"--open\"",
            "\"--developer\"",
            "\"--disable-module\"",
            "\"--proof-endpoint\"",
            "\"--window-size\"",
            "spawn_editor",
            "editor_args",
        ],
        scope: &["xtask"],
        types: &["rs"],
        allowed: &["xtask/src/scenario/launch.rs", "xtask/src/launch.rs"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "the harness assembles editor arguments only in the scenario library's Launch and \
                 launches the editor only through its Run (xtask/src/scenario/launch.rs)",
    },
    // One registry assembly: `ModuleRegistry::assemble` decides which linked modules a run serves
    // — the test modules only in developer mode, the capability proof only with a proof endpoint —
    // and registers what `--disable-module` names unavailable, for the desktop, `luxforge-json`
    // and the harness alike. A second assembly would construct a test module or register one
    // unavailable itself.
    SourceRule {
        name: "registry-assembly",
        tokens: &[
            "register_unavailable(",
            "PixelModule::new(",
            "ControlsModule::new(",
            "CapabilitiesProofModule::new(",
        ],
        scope: &["crates", "xtask"],
        types: &["rs"],
        allowed: &["crates/luxforge-core/src/modules/registry/mod.rs"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "a run's registry is assembled only by ModuleRegistry::assemble \
                 (crates/luxforge-core/src/modules/registry/mod.rs), so test modules join only a \
                 developer run and every binary refuses the same flags in the same words",
    },
    // One RAW manifest reader: `raw::manifest` reads and checks every RAW manifest, for `verify`
    // and the `raw-editor` scenario alike. A reader elsewhere names the manifest's list untyped
    // (`["sources"]`) or declares its fields again (`neutral_point:`); code that uses a source
    // `raw::manifest` read names neither.
    SourceRule {
        name: "raw-manifest-reader",
        tokens: &["[\"sources\"]", "neutral_point:"],
        scope: &["xtask"],
        types: &["rs"],
        allowed: &["xtask/src/raw.rs"],
        mode: Match::Whole,
        tests: false,
        once: false,
        reason: "RAW manifests are read only through raw::manifest (xtask/src/raw.rs); read a \
                 manifest's sources through it",
    },
    // One HTTP/1.1 implementation: the module transport runs on `ureq`'s agent, and only it names
    // the protocol crate under that agent or its head parser, so no second hand-written framing
    // appears elsewhere, test code included.
    SourceRule {
        name: "http-framing",
        tokens: &["ureq_proto::", "httparse::"],
        scope: &["crates", "xtask"],
        types: &["rs"],
        allowed: &["crates/luxforge-net/src"],
        mode: Match::Whole,
        tests: true,
        once: false,
        reason: "only the module transport (crates/luxforge-net) speaks HTTP/1.1, through ureq; \
                 no other code frames HTTP",
    },
    SourceRule {
        name: "no-pixel-image-handle",
        tokens: &["Handle::from_rgba"],
        scope: &["crates", "xtask"],
        types: &["rs"],
        allowed: &[
            "crates/luxforge-ui/src/gallery_thumbnails.rs",
            "crates/luxforge-app/src/app/select_previews.rs",
            "crates/luxforge-app/src/app/loupe_frames.rs",
        ],
        mode: Match::Whole,
        tests: true,
        once: false,
        reason: "an image handle made from pixels uploads a new texture each time it is made; the \
                 photo surface owns the photograph's GPU uploads, the components gallery's \
                 stand-in photographs are made once in gallery_thumbnails.rs, the Select \
                 grid's decoded previews once each, when a decode lands, in \
                 app/select_previews.rs, which holds each while its cell may be shown, and the \
                 loupe's decoded frames and 100% regions likewise in app/loupe_frames.rs",
    },
    SourceRule {
        name: "project-name",
        tokens: &["lightwell", "Lightwell", "LIGHTWELL"],
        scope: &["crates", "xtask"],
        types: TEXT,
        allowed: &[],
        mode: Match::Prefix,
        tests: true,
        once: false,
        reason: "the project is Luxforge; its old working name does not come back",
    },
];

/// The manifest tables a dependency rule reads. A target-specific table
/// (`[target.'cfg(unix)'.dependencies]`) counts as the table it names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Table {
    /// `[dependencies]`.
    Normal,
    /// `[dev-dependencies]`.
    Dev,
    /// `[build-dependencies]`.
    Build,
    /// The root manifest's `[workspace.dependencies]`.
    Workspace,
}

/// Every table a manifest can declare a dependency in.
const EVERY_TABLE: &[Table] = &[Table::Normal, Table::Dev, Table::Build, Table::Workspace];

/// Which dependencies a dependency rule refuses. A dependency is known by its key and, when it
/// renames one, the package it names.
#[derive(Clone, Copy)]
enum Depends {
    /// A dependency on exactly this crate.
    On(&'static str),
    /// A dependency on any one of these crates.
    Any(&'static [&'static str]),
    /// A dependency on any crate whose name starts with this.
    Prefixed(&'static str),
    /// `crate` with `feature` anywhere in its declaration.
    Feature {
        dependency: &'static str,
        feature: &'static str,
    },
    /// Any workspace crate: a `luxforge` name, or any path.
    WorkspaceCrate,
}

/// A rule about which crate may depend on what.
struct DependencyRule {
    /// The rule's name, printed with each refusal and unique across both tables.
    name: &'static str,
    /// The dependencies it refuses.
    refuses: Depends,
    /// The crate directories whose `Cargo.toml` is read, relative to the root: `""` is the
    /// workspace root, and `crates/*` every crate under `crates/`.
    manifests: &'static [&'static str],
    /// The tables read in each.
    tables: &'static [Table],
    /// The crate directories that may declare the dependency.
    allowed: &'static [&'static str],
    /// Why the rule holds, printed with each refusal.
    reason: &'static str,
}

const DEPENDENCY_RULES: &[DependencyRule] = &[
    DependencyRule {
        name: "image-jpeg-feature",
        refuses: Depends::Feature {
            dependency: "image",
            feature: "jpeg",
        },
        manifests: SHIPPED_CRATES,
        tables: &[Table::Normal],
        allowed: &[],
        reason: "a shipped crate may not enable image's jpeg feature; JPEG is read only through \
                 crates/luxforge-jpeg",
    },
    DependencyRule {
        name: "workspace-image-jpeg",
        refuses: Depends::Feature {
            dependency: "image",
            feature: "jpeg",
        },
        manifests: &[""],
        tables: &[Table::Workspace],
        allowed: &[],
        reason: "the workspace image dependency may not enable jpeg; a test or tool adds it for \
                 itself",
    },
    DependencyRule {
        name: "mozjpeg-links",
        refuses: Depends::Prefixed("mozjpeg"),
        manifests: SHIPPED_CRATES,
        tables: &[Table::Normal],
        allowed: &["crates/luxforge-jpeg"],
        reason: "only luxforge-jpeg links the JPEG codec",
    },
    // The codec crate is a leaf behind the core.
    DependencyRule {
        name: "jpeg-codec-users",
        refuses: Depends::On("luxforge-jpeg"),
        manifests: &["crates/*", "xtask"],
        tables: EVERY_TABLE,
        allowed: &["crates/luxforge-core"],
        reason: "only luxforge-core may depend on luxforge-jpeg",
    },
    DependencyRule {
        name: "jpeg-codec-leaf",
        refuses: Depends::WorkspaceCrate,
        manifests: &["crates/luxforge-jpeg"],
        tables: EVERY_TABLE,
        allowed: &[],
        reason: "luxforge-jpeg may depend on no workspace crate and no path",
    },
    // The watcher's platform code is a leaf the core's index uses: it builds against no workspace
    // crate. Its tests may use the test base, which reaches no workspace crate either.
    DependencyRule {
        name: "watch-leaf",
        refuses: Depends::WorkspaceCrate,
        manifests: &["crates/luxforge-watch"],
        tables: &[Table::Normal, Table::Build],
        allowed: &[],
        reason: "luxforge-watch may depend on no workspace crate and no path, except in its \
                 [dev-dependencies]",
    },
    // One HTTP client: `ureq` and `ureq-proto` belong to the module transport in `luxforge-net`.
    DependencyRule {
        name: "http-client-crates",
        refuses: Depends::Prefixed("ureq"),
        manifests: &["crates/*", "xtask"],
        tables: EVERY_TABLE,
        allowed: &["crates/luxforge-net"],
        reason: "only luxforge-net, for its module transport, may depend on ureq or ureq-proto",
    },
    // The core links no network stack or secure store: the transport and the Keychain store live
    // in `luxforge-net`, which the desktop and `luxforge-json` give the host through `HostConfig`,
    // so neither the core nor its test binaries compile TLS, HTTP or the Security framework.
    DependencyRule {
        name: "core-links-no-network",
        refuses: Depends::Any(&[
            "rustls",
            "rustls-platform-verifier",
            "ring",
            "ureq",
            "ureq-proto",
            "security-framework",
        ]),
        manifests: &["crates/luxforge-core"],
        tables: &[Table::Normal, Table::Dev, Table::Build],
        allowed: &[],
        reason: "luxforge-core links no TLS, HTTP client or Keychain crate; the transport and the \
                 secret store's Keychain implementation belong to luxforge-net, injected through \
                 HostConfig",
    },
    // The references are independent by construction: nothing they build against can reach the
    // core they check, directly or through a crate that depends on it.
    DependencyRule {
        name: "independent-references",
        refuses: Depends::WorkspaceCrate,
        manifests: &["crates/luxforge-reference"],
        tables: EVERY_TABLE,
        allowed: &[],
        reason: "luxforge-reference may depend on no workspace crate and no path, so it can never \
                 reach luxforge-core",
    },
    // The test base (the one gate, wait and distribution, the loopback test server, the proof
    // endpoint and the fixture paths) serves every crate's tests, the core's own and the widget
    // crate's included, so it can never reach the core: a core that names it builds itself once.
    DependencyRule {
        name: "core-free-test-base",
        refuses: Depends::WorkspaceCrate,
        manifests: &["crates/luxforge-testbase"],
        tables: EVERY_TABLE,
        allowed: &[],
        reason: "luxforge-testbase may depend on no workspace crate and no path, so the core's \
                 and the widget crate's tests can use it without building the core twice",
    },
    // The core names no crate that depends on it, in any table: a dev-dependency on one (the
    // typed test kit above all) builds the core a second time for its own tests. Its integration
    // tests compile the kit's typed helpers in through `#[path]` instead.
    DependencyRule {
        name: "core-builds-once",
        refuses: Depends::Any(&[
            "luxforge-testkit",
            "luxforge-net",
            "luxforge-cli",
            "luxforge-app",
            "xtask",
        ]),
        manifests: &["crates/luxforge-core"],
        tables: EVERY_TABLE,
        allowed: &[],
        reason: "luxforge-core may not name a crate that depends on it (luxforge-testkit, \
                 luxforge-net, luxforge-cli, luxforge-app or xtask), so its tests build it once",
    },
    // The headless binary builds without the GUI stack: no window, renderer or dialog crate, and
    // not the widget crate or the desktop that bring them.
    DependencyRule {
        name: "headless-cli",
        refuses: Depends::Any(&[
            "iced",
            "iced_wgpu",
            "wgpu",
            "rfd",
            "luxforge-ui",
            "luxforge-app",
        ]),
        manifests: &["crates/luxforge-cli"],
        tables: &[Table::Normal],
        allowed: &[],
        reason: "luxforge-cli builds the headless luxforge-json binary and may not depend on the \
                 GUI stack (iced, wgpu, rfd, luxforge-ui or luxforge-app)",
    },
    // Skipping the disk flush is for tests: only a `[dev-dependencies]` table turns the feature on,
    // so no `cargo build` of a binary, whose dependencies are never dev-dependencies, has it.
    DependencyRule {
        name: "disk-flush-only-in-tests",
        refuses: Depends::Feature {
            dependency: "luxforge-core",
            feature: "test-skip-disk-flush",
        },
        manifests: &["", "crates/*", "xtask"],
        tables: &[Table::Normal, Table::Build, Table::Workspace],
        allowed: &[],
        reason: "only a [dev-dependencies] table may turn on luxforge-core's test-skip-disk-flush, \
                 so no build of a binary skips the flush of a durable write",
    },
    // Holding the core's work at a test's gate from outside it is for tests too, the same way.
    DependencyRule {
        name: "test-holds-only-in-tests",
        refuses: Depends::Feature {
            dependency: "luxforge-core",
            feature: "test-holds",
        },
        manifests: &["", "crates/*", "xtask"],
        tables: &[Table::Normal, Table::Build, Table::Workspace],
        allowed: &[],
        reason: "only a [dev-dependencies] table may turn on luxforge-core's test-holds, so no \
                 build of a binary holds its work at a test's gate or links luxforge-testbase",
    },
];

/// A rule that every variant of one message enum has a sender in product code: a production line,
/// outside the scripted drivers, that constructs it. A variant only a driver or a test constructs
/// proves a path no person can take, so evidence and tests drive the messages widgets send.
struct SenderRule {
    /// The rule's name, printed with each refusal and unique across every table.
    name: &'static str,
    /// The enum, as a sender names it: a variant is sent where a line holds `Enum::Variant`.
    message: &'static str,
    /// The file that declares the enum.
    declared: &'static str,
    /// The file whose `match` handles it: a line there that begins with a variant is that
    /// variant's arm, not a sender. Everywhere else, rustfmt may begin a line with one that is.
    handler: &'static str,
    /// The directories whose production lines are read for senders, relative to the root.
    scope: &'static [&'static str],
    /// The scripted drivers, whose constructions are not senders.
    drivers: &'static [&'static str],
    /// Why the rule holds, printed with each refusal.
    reason: &'static str,
}

const SENDER_RULES: &[SenderRule] = &[
    // The widgets' own messages: evidence scripts and tests drive a slider through the `Fraction`
    // and `Released` its widget publishes, never a message of their own that no widget sends.
    SenderRule {
        name: "widget-sent-controls",
        message: "ControlMessage",
        declared: "crates/luxforge-app/src/app/message/control.rs",
        handler: "crates/luxforge-app/src/app/controls.rs",
        scope: &["crates/luxforge-app/src"],
        drivers: &["crates/luxforge-app/src/app/evidence.rs"],
        reason: "every control message has a sender in desktop product code outside the evidence \
                 driver; drive evidence and tests through the message the widget sends",
    },
];

/// Whether `line` holds `token` under `mode`; see [`Match`].
fn holds_token(line: &str, token: &str, mode: Match) -> bool {
    let identifier = |c: char| c.is_alphanumeric() || c == '_';
    let check_start = token.starts_with(identifier);
    let check_end = mode == Match::Whole && token.ends_with(identifier);
    line.match_indices(token).any(|(at, _)| {
        let runs_in = check_start && line[..at].ends_with(identifier);
        let runs_on = check_end && line[at + token.len()..].starts_with(identifier);
        !runs_in && !runs_on
    })
}

/// Whether `path` is test code by its name alone: a `tests` directory, `tests.rs` or `*_tests.rs`.
fn test_file(path: &Path) -> bool {
    path.components().any(|part| part.as_os_str() == "tests")
        || path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
}

/// The lines of `text` outside `#[cfg(test)]` items, numbered from one, and the names of the
/// out-of-line modules it declares under `#[cfg(test)]` (`#[cfg(test)] mod testing;`). An item is
/// skipped from its attribute to the line that closes its braces, or to its `;` when it opens
/// none. Comment lines are skipped too: they name nothing a build can reach.
fn production_lines(text: &str) -> (Vec<(usize, &str)>, Vec<&str>) {
    let mut lines = Vec::new();
    let mut test_modules = Vec::new();
    // `Some((depth, opened))` while inside a test item.
    let mut skipping: Option<(i64, bool)> = None;
    for (index, line) in text.lines().enumerate() {
        let mut trimmed = line.trim();
        if skipping.is_none() {
            let Some(rest) = trimmed.strip_prefix("#[cfg(test)]") else {
                if !trimmed.starts_with("//") {
                    lines.push((index + 1, line));
                }
                continue;
            };
            skipping = Some((0, false));
            trimmed = rest.trim();
            if trimmed.is_empty() {
                continue;
            }
        }
        let Some((depth, opened)) = skipping.as_mut() else {
            continue;
        };
        if trimmed.is_empty() || trimmed.starts_with("#[") || trimmed.starts_with("//") {
            continue;
        }
        if !*opened
            && let Some(name) = trimmed
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ")
                .strip_prefix("mod ")
                .and_then(|rest| rest.strip_suffix(';'))
        {
            test_modules.push(name.trim());
        }
        for c in trimmed.chars() {
            match c {
                '{' => {
                    *depth += 1;
                    *opened = true;
                }
                '}' => *depth -= 1,
                _ => {}
            }
        }
        if (*opened && *depth <= 0) || (!*opened && trimmed.ends_with(';')) {
            skipping = None;
        }
    }
    (lines, test_modules)
}

/// The variants `text` declares for `enum name`, in order, or `None` when it declares no such
/// enum. A variant is a line directly inside the enum's braces that begins with an identifier;
/// attributes, comments and a struct variant's fields are not.
fn enum_variants<'a>(text: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let mut lines = text.lines().skip_while(|line| {
        line.trim()
            .trim_start_matches("pub(crate) ")
            .trim_start_matches("pub ")
            .strip_prefix("enum ")
            .is_none_or(|rest| rest.trim_end_matches('{').trim() != name)
    });
    lines.next()?;
    let mut variants = Vec::new();
    let mut depth = 1;
    for line in lines {
        let trimmed = line.trim();
        if depth == 1 && trimmed.starts_with(|c: char| c.is_ascii_uppercase()) {
            let end = trimmed
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(trimmed.len());
            variants.push(&trimmed[..end]);
        }
        if trimmed.starts_with("//") {
            continue;
        }
        for c in trimmed.chars() {
            match c {
                '{' | '(' => depth += 1,
                '}' | ')' => depth -= 1,
                _ => {}
            }
        }
        if depth <= 0 {
            return Some(variants);
        }
    }
    Some(variants)
}

/// Where an out-of-line module `name` declared in `file` lives: its `name.rs`, and the directory
/// (ending in `/`) that holds its `mod.rs` and its own submodules.
fn module_files(file: &str, name: &str) -> (String, String) {
    let file = Path::new(file);
    let parent = file.parent().unwrap_or(Path::new(""));
    let dir = match file.file_stem().and_then(|stem| stem.to_str()) {
        Some("mod" | "lib" | "main") | None => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
    };
    (
        slashed(&dir.join(format!("{name}.rs"))),
        format!("{}/", slashed(&dir.join(name))),
    )
}

/// `path` with forward slashes, as the rule tables spell paths.
fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Whether `path` is one of `allowed`, or lies under one; see [`SourceRule::allowed`].
fn permitted(path: &str, allowed: &[&str]) -> bool {
    allowed.iter().any(|entry| {
        path == *entry
            || path
                .strip_prefix(entry)
                .is_some_and(|rest| rest.starts_with('/'))
            || Path::new(path).with_extension("") == Path::new(entry)
    })
}

/// One dependency as a manifest declares it.
struct Dependency {
    /// The line that declares it: its key's, or its own table's header.
    line: usize,
    table: Table,
    /// Its key and, when it renames one, the package it names.
    names: Vec<String>,
    /// Whether it names a path.
    path: bool,
    /// Its whole declaration, for its features.
    text: String,
}

/// `text` without a trailing comment, trimmed.
fn uncommented(line: &str) -> &str {
    line.split('#').next().unwrap_or_default().trim()
}

/// A dotted TOML key or table name split into its parts, each unquoted, with dots inside quotes
/// (`target.'cfg(target_os = "macos")'.dependencies`) kept.
fn key_parts(key: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut quote = None;
    for c in key.chars() {
        match quote {
            None if c == '.' => {
                parts.push(String::new());
                continue;
            }
            None if c == '\'' || c == '"' => quote = Some(c),
            Some(open) if c == open => quote = None,
            _ => {}
        }
        parts.last_mut().unwrap().push(c);
    }
    parts
        .iter()
        .map(|part| part.trim().trim_matches(['"', '\'']).to_owned())
        .collect()
}

/// The dependency table a header names, and the dependency when it is one dependency's own table
/// (`[dev-dependencies.name]`); `None` for any other table.
fn dependency_table(header: &str) -> Option<(Table, Option<String>)> {
    let parts = key_parts(header);
    let kind = |part: &str| match part {
        "dependencies" => Some(Table::Normal),
        "dev-dependencies" | "dev_dependencies" => Some(Table::Dev),
        "build-dependencies" | "build_dependencies" => Some(Table::Build),
        _ => None,
    };
    let (at, table) = match parts.first().map(String::as_str) {
        Some("workspace") => (
            1,
            (parts.get(1)? == "dependencies").then_some(Table::Workspace)?,
        ),
        Some("target") => (2, kind(parts.get(2)?)?),
        _ => (0, kind(parts.first()?)?),
    };
    match &parts[at + 1..] {
        [] => Some((table, None)),
        [name] => Some((table, Some(name.clone()))),
        _ => None,
    }
}

/// Record a dependency's `key = value` field: a path, or the package it renames.
fn dependency_field(dependency: &mut Dependency, key: &str, value: &str) {
    match key.trim() {
        "path" => dependency.path = true,
        "package" => dependency
            .names
            .push(value.trim().trim_matches(['"', '\'']).to_owned()),
        _ => {}
    }
}

/// Every dependency a manifest declares, in any table and any spelling: `name = ...`,
/// `name.field = ...`, an inline table, and a dependency's own `[table.name]`. A value that runs
/// over several lines, as a multi-line features array does, is read whole.
fn dependencies(text: &str) -> Vec<Dependency> {
    let mut found: Vec<Dependency> = Vec::new();
    // The dependency table the lines below belong to, and whether it is one dependency's own.
    let mut table: Option<(Table, bool)> = None;
    let mut lines = text.lines().enumerate();
    while let Some((index, line)) = lines.next() {
        let mut line = uncommented(line).to_owned();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            table = dependency_table(line.trim_matches(['[', ']'])).map(|(kind, own)| {
                if let Some(name) = &own {
                    found.push(Dependency {
                        line: index + 1,
                        table: kind,
                        names: vec![name.clone()],
                        path: false,
                        text: String::new(),
                    });
                }
                (kind, own.is_some())
            });
            continue;
        }
        let Some((kind, own)) = table else {
            continue;
        };
        let open = |text: &str| {
            text.chars()
                .map(|c| match c {
                    '[' | '{' => 1,
                    ']' | '}' => -1,
                    _ => 0,
                })
                .sum::<i64>()
        };
        while open(&line) > 0 {
            let Some((_, next)) = lines.next() else {
                break;
            };
            line.push(' ');
            line.push_str(uncommented(next));
        }
        let (key, value) = line.split_once('=').unwrap_or((&line, ""));
        let (key, value) = (key.trim(), value.trim());
        if own {
            let dependency = found.last_mut().expect("a dependency's own table");
            dependency_field(dependency, key, value);
            dependency.text.push_str(&line);
            dependency.text.push('\n');
            continue;
        }
        let parts = key_parts(key);
        let mut dependency = Dependency {
            line: index + 1,
            table: kind,
            names: vec![parts[0].clone()],
            path: false,
            text: line.clone(),
        };
        if let Some(field) = parts.get(1) {
            dependency_field(&mut dependency, field, value);
        } else if value.starts_with('{') {
            for part in value.trim_matches(['{', '}']).split(',') {
                if let Some((key, value)) = part.split_once('=') {
                    dependency_field(&mut dependency, key, value);
                }
            }
        }
        found.push(dependency);
    }
    found
}

impl Depends {
    fn refuses(self, dependency: &Dependency) -> bool {
        let named = |test: &dyn Fn(&str) -> bool| dependency.names.iter().any(|n| test(n));
        match self {
            Depends::On(name) => named(&|n| n == name),
            Depends::Any(names) => named(&|n| names.contains(&n)),
            Depends::Prefixed(prefix) => named(&|n| n.starts_with(prefix)),
            Depends::Feature {
                dependency: name,
                feature,
            } => named(&|n| n == name) && holds_token(&dependency.text, feature, Match::Prefix),
            Depends::WorkspaceCrate => dependency.path || named(&|n| n.starts_with("luxforge")),
        }
    }
}

/// The repository's files, each listed and read once however many rules read it.
struct Tree<'a> {
    root: &'a Path,
    listings: BTreeMap<String, Vec<String>>,
    texts: BTreeMap<String, String>,
}

impl<'a> Tree<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            listings: BTreeMap::new(),
            texts: BTreeMap::new(),
        }
    }

    /// Every file under `dir`, relative to the root; none when `dir` does not exist.
    fn under(&mut self, dir: &str) -> Result<Vec<String>> {
        if let Some(listing) = self.listings.get(dir) {
            return Ok(listing.clone());
        }
        let absolute = self.root.join(dir);
        let listing = if absolute.is_dir() {
            files(&absolute)?
                .iter()
                .map(|path| slashed(path.strip_prefix(self.root).unwrap_or(path)))
                .collect()
        } else {
            Vec::new()
        };
        self.listings.insert(dir.to_owned(), listing.clone());
        Ok(listing)
    }

    fn text(&mut self, path: &str) -> Result<&str> {
        if !self.texts.contains_key(path) {
            let absolute = self.root.join(path);
            let text = fs::read_to_string(&absolute)
                .map_err(|error| format!("{}: {error}", absolute.display()))?;
            self.texts.insert(path.to_owned(), text);
        }
        Ok(&self.texts[path])
    }
}

/// What applying the rules read: the distinct source files and manifests, and how many files each
/// rule read.
#[derive(Default)]
struct Applied {
    sources: BTreeSet<String>,
    manifests: BTreeSet<String>,
    reads: BTreeMap<&'static str, usize>,
}

/// Every file of one of `types` under the directories of `scope`, except the rule tables' own.
fn scoped(tree: &mut Tree, scope: &[&str], types: &[&str]) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for dir in scope {
        paths.extend(tree.under(dir)?.into_iter().filter(|path| {
            path != RULES_FILE
                && Path::new(path)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| types.contains(&extension))
        }));
    }
    Ok(paths)
}

/// Which of `paths` are test code: a test by name ([`test_file`]), or a module one of them
/// declares under `#[cfg(test)]`, with everything below its directory.
fn test_paths(tree: &mut Tree, paths: &[String]) -> Result<BTreeSet<String>> {
    let mut test_modules = Vec::new();
    for path in paths {
        for name in production_lines(tree.text(path)?).1 {
            test_modules.push(module_files(path, name));
        }
    }
    Ok(paths
        .iter()
        .filter(|path| {
            test_file(Path::new(path))
                || test_modules
                    .iter()
                    .any(|(file, dir)| *path == file || path.starts_with(dir.as_str()))
        })
        .cloned()
        .collect())
}

impl SourceRule {
    fn apply(&self, tree: &mut Tree, applied: &mut Applied, refusals: &mut Vec<String>) -> Result {
        let paths = scoped(tree, self.scope, self.types)?;
        let tests = if self.tests {
            BTreeSet::new()
        } else {
            test_paths(tree, &paths)?
        };
        let mut read = 0;
        for path in &paths {
            let test_only = tests.contains(path);
            let home = permitted(path, self.allowed);
            if test_only || (home && !self.once) {
                continue;
            }
            let mut homes: BTreeMap<&str, usize> = BTreeMap::new();
            let text = tree.text(path)?;
            // A token the file does not hold anywhere is on none of its lines.
            let tokens: Vec<&str> = self
                .tokens
                .iter()
                .copied()
                .filter(|token| text.contains(token))
                .collect();
            let lines = if tokens.is_empty() {
                Vec::new()
            } else if self.tests {
                text.lines().enumerate().map(|(i, l)| (i + 1, l)).collect()
            } else {
                production_lines(text).0
            };
            for (number, line) in lines {
                for &token in &tokens {
                    if holds_token(line, token, self.mode) {
                        let seen = homes.entry(token).or_default();
                        if home && *seen == 0 {
                            *seen += 1;
                            continue;
                        }
                        refusals.push(format!(
                            "{path}:{number}: {} (source rule `{}` found `{token}`; a deliberate \
                             new home is a change to that row of SOURCE_RULES in {RULES_FILE}, \
                             not a new check)",
                            self.reason, self.name
                        ));
                    }
                }
            }
            applied.sources.insert(path.clone());
            read += 1;
        }
        *applied.reads.entry(self.name).or_default() += read;
        Ok(())
    }
}

impl SenderRule {
    fn apply(&self, tree: &mut Tree, applied: &mut Applied, refusals: &mut Vec<String>) -> Result {
        let variants: Vec<String> = enum_variants(tree.text(self.declared)?, self.message)
            .ok_or_else(|| format!("{} declares no enum {}", self.declared, self.message))?
            .into_iter()
            .map(|variant| format!("{}::{variant}", self.message))
            .collect();
        let paths = scoped(tree, self.scope, &["rs"])?;
        let tests = test_paths(tree, &paths)?;
        let mut sent = BTreeSet::new();
        let mut read = 0;
        for path in &paths {
            if tests.contains(path) || permitted(path, self.drivers) {
                continue;
            }
            let text = tree.text(path)?;
            let held: Vec<&String> = variants
                .iter()
                .filter(|token| text.contains(token.as_str()))
                .collect();
            let lines = if held.is_empty() {
                Vec::new()
            } else {
                production_lines(text).0
            };
            for (_, line) in lines {
                let arm = path == self.handler;
                for &token in &held {
                    if holds_token(line, token, Match::Whole)
                        && !(arm && line.trim_start().starts_with(token.as_str()))
                    {
                        sent.insert(token.clone());
                    }
                }
            }
            applied.sources.insert(path.clone());
            read += 1;
        }
        for token in variants.iter().filter(|token| !sent.contains(*token)) {
            refusals.push(format!(
                "{}: {} (sender rule `{}` found no sender of `{token}`; a deliberate new \
                 driver is a change to that row of SENDER_RULES in {RULES_FILE}, not a new check)",
                self.declared, self.reason, self.name
            ));
        }
        *applied.reads.entry(self.name).or_default() += read;
        Ok(())
    }
}

impl DependencyRule {
    /// The crate directories whose manifest the rule reads that exist.
    fn crates(&self, root: &Path) -> Result<Vec<String>> {
        let mut crates = Vec::new();
        for entry in self.manifests {
            if let Some(parent) = entry.strip_suffix("/*") {
                let dir = root.join(parent);
                if !dir.is_dir() {
                    continue;
                }
                let mut found = Vec::new();
                for item in fs::read_dir(&dir)? {
                    let name = item?.file_name().to_string_lossy().into_owned();
                    found.push(format!("{parent}/{name}"));
                }
                found.sort();
                crates.extend(found);
            } else {
                crates.push((*entry).to_owned());
            }
        }
        crates.retain(|krate| root.join(krate).join("Cargo.toml").is_file());
        Ok(crates)
    }

    fn apply(&self, tree: &mut Tree, applied: &mut Applied, refusals: &mut Vec<String>) -> Result {
        let mut read = 0;
        for krate in self.crates(tree.root)? {
            let manifest = if krate.is_empty() {
                "Cargo.toml".to_owned()
            } else {
                format!("{krate}/Cargo.toml")
            };
            let allowed = self.allowed.contains(&krate.as_str());
            for dependency in dependencies(tree.text(&manifest)?) {
                if !allowed
                    && self.tables.contains(&dependency.table)
                    && self.refuses.refuses(&dependency)
                {
                    refusals.push(format!(
                        "{manifest}:{}: {} (dependency rule `{}` found `{}`; a deliberate new \
                         dependent is a change to that row of DEPENDENCY_RULES in {RULES_FILE}, \
                         not a new check)",
                        dependency.line,
                        self.reason,
                        self.name,
                        dependency.names.join("` as `"),
                    ));
                }
            }
            applied.manifests.insert(manifest);
            read += 1;
        }
        *applied.reads.entry(self.name).or_default() += read;
        Ok(())
    }
}

/// Apply the named rules of every table (every rule when `only` is empty) to the repository at
/// `root`, failing with every refusal.
fn apply(root: &Path, only: &[&str]) -> Result<Applied> {
    let selected = |name: &str| only.is_empty() || only.contains(&name);
    let mut tree = Tree::new(root);
    let mut applied = Applied::default();
    let mut refusals = Vec::new();
    for rule in SOURCE_RULES.iter().filter(|rule| selected(rule.name)) {
        rule.apply(&mut tree, &mut applied, &mut refusals)?;
    }
    for rule in DEPENDENCY_RULES.iter().filter(|rule| selected(rule.name)) {
        rule.apply(&mut tree, &mut applied, &mut refusals)?;
    }
    for rule in SENDER_RULES.iter().filter(|rule| selected(rule.name)) {
        rule.apply(&mut tree, &mut applied, &mut refusals)?;
    }
    ensure(refusals.is_empty(), refusals.join("\n"))?;
    Ok(applied)
}

/// Apply every rule to the repository, and refuse a stale row: one that reads nothing, whose
/// allowed path no longer exists, or whose name another row shares.
fn rules(root: &Path) -> Result<Applied> {
    let applied = apply(root, &[])?;
    let mut names = BTreeSet::new();
    let senders: Vec<(&str, Vec<&str>)> = SENDER_RULES
        .iter()
        .map(|rule| {
            let mut paths = vec![rule.declared, rule.handler];
            paths.extend(rule.drivers);
            (rule.name, paths)
        })
        .collect();
    let rows = SOURCE_RULES
        .iter()
        .map(|rule| (rule.name, rule.allowed))
        .chain(
            DEPENDENCY_RULES
                .iter()
                .map(|rule| (rule.name, rule.allowed)),
        )
        .chain(
            senders
                .iter()
                .map(|(name, paths)| (*name, paths.as_slice())),
        );
    for (name, allowed) in rows {
        ensure(
            names.insert(name),
            format!("two repository rules are named {name}"),
        )?;
        ensure(
            applied.reads.get(name).is_some_and(|read| *read > 0),
            format!("repository rule {name} read no file; its scope is stale"),
        )?;
        for entry in allowed {
            ensure(
                root.join(entry).exists() || root.join(format!("{entry}.rs")).is_file(),
                format!("repository rule {name} allows {entry}, which does not exist"),
            )?;
        }
    }
    Ok(applied)
}

pub fn check(root: &Path) -> Result {
    let s = read_json(&root.join("tools/task-plan.schema.json"))?;
    let mut plan_paths: Vec<_> = fs::read_dir(root.join("tasks"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "json")
        })
        .collect();
    plan_paths.sort();
    let plans = plan_paths
        .iter()
        .map(|path| read_json(path))
        .collect::<Result<Vec<_>>>()?;
    let summaries = active_plans(root, &plans, &s)?;
    let mut paths = vec![
        root.join("README.md"),
        root.join("AGENTS.md"),
        root.join("CONTRIBUTING.md"),
    ];
    for dir in ["docs", "fixtures", "tasks"] {
        paths.extend(
            files(&root.join(dir))?
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e == "md")),
        );
    }
    let fenced = Regex::new(r"(?s)```.*?```")?;
    let links = Regex::new(r#"\[[^\]\n]*\]\(([^)]+)\)"#)?;
    let scheme = Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:")?;
    let mut count = 0;
    for path in paths {
        let raw = fs::read_to_string(&path)?;
        let text = fenced.replace_all(&raw, "");
        for c in links.captures_iter(&text) {
            let target = c[1].split(" \"").next().unwrap().trim_matches(['<', '>']);
            if scheme.is_match(target) {
                continue;
            }
            let local = target.split(['#', '?']).next().unwrap();
            if local.is_empty() {
                continue;
            }
            let decoded = percent_encoding::percent_decode_str(local).decode_utf8()?;
            ensure(
                path.parent().unwrap().join(decoded.as_ref()).exists(),
                format!("{}: broken link {target}", path.display()),
            )?;
            count += 1;
        }
    }
    println!(
        "PASS local task schemas/DAGs/order ({}), {count} local links",
        summaries.join(", ")
    );
    let applied = rules(root)?;
    println!(
        "PASS {} source rules and {} sender rules ({} files) and {} dependency rules ({} \
         manifests)",
        SOURCE_RULES.len(),
        SENDER_RULES.len(),
        applied.sources.len(),
        DEPENDENCY_RULES.len(),
        applied.manifests.len()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_rejects_wrong_types_and_unknown_fields() {
        let s = json!({"type":"object","required":["n"],"additionalProperties":false,"properties":{"n":{"type":"integer","minimum":1}}});
        for value in [
            json!({}),
            json!({"n":true}),
            json!({"n":0}),
            json!({"n":1,"extra":0}),
        ] {
            assert!(schema(&value, &s, &s).is_err())
        }
        assert!(schema(&json!({"n":2}), &s, &s).is_ok());
    }
    #[test]
    fn slow_active_repository_is_valid() {
        check(&root().unwrap()).unwrap();
    }
    /// The desktop layering rules.
    const LAYERS: &[&str] = &["state-layer", "view-layer", "widget-crate"];
    /// The one-JPEG-codec rules.
    const JPEG: &[&str] = &[
        "jpeg-codec-name",
        "jpeg-through-codec",
        "image-jpeg-feature",
        "workspace-image-jpeg",
        "mozjpeg-links",
        "jpeg-codec-users",
        "jpeg-codec-leaf",
    ];
    /// Apply the named rules, each of which must exist, and answer how many distinct source files
    /// and manifests they read.
    fn read(root: &Path, only: &[&str]) -> Result<(usize, usize)> {
        for name in only {
            assert!(
                SOURCE_RULES.iter().any(|rule| rule.name == *name)
                    || DEPENDENCY_RULES.iter().any(|rule| rule.name == *name)
                    || SENDER_RULES.iter().any(|rule| rule.name == *name),
                "no rule {name}"
            );
        }
        apply(root, only).map(|applied| (applied.sources.len(), applied.manifests.len()))
    }
    /// The refusal the named rules make, which must be one.
    fn refusal(root: &Path, only: &[&str], what: &str) -> String {
        read(root, only)
            .err()
            .unwrap_or_else(|| panic!("{what} was accepted"))
            .to_string()
    }
    #[test]
    fn one_matcher_checks_the_ends_a_token_has() {
        for (line, token, mode, holds) in [
            ("use crate::app::State;", "app::", Match::Whole, true),
            ("let snapp::x = 1;", "app::", Match::Whole, false),
            ("let snapp::x = 1;", "app::", Match::Prefix, false),
            ("RAW_EFFECTS", "RAW_EFFECT", Match::Whole, false),
            ("MY_RAW_EFFECT", "RAW_EFFECT", Match::Whole, false),
            ("crate::RAW_EFFECT)", "RAW_EFFECT", Match::Whole, true),
            ("use mozjpeg_sys::x;", "mozjpeg", Match::Whole, false),
            ("use mozjpeg_sys::x;", "mozjpeg", Match::Prefix, true),
            ("owner.call(c)", ".call(", Match::Whole, true),
        ] {
            assert_eq!(holds_token(line, token, mode), holds, "{token} in {line}");
        }
    }
    #[test]
    fn manifests_are_read_in_every_spelling() {
        let manifest = "[package]\nname = \"x\"\n\n[dependencies]\na = \"1\" # b\n\
                        c.workspace = true\nd = { package = \"e\", path = \"../e\" }\n\
                        f = { version = \"1\", features = [\n    \"g\",\n] }\n\
                        [target.'cfg(target_os = \"macos\")'.dev-dependencies.h]\npath = \"../h\"\n\
                        [workspace.dependencies]\ni = \"1\"\n[lints.rust]\nj = \"deny\"\n";
        let found: Vec<_> = dependencies(manifest)
            .into_iter()
            .map(|d| (d.line, d.names.join("/"), d.path, d.text.contains("\"g\"")))
            .collect();
        assert_eq!(
            found,
            [
                (5, "a".to_owned(), false, false),
                (6, "c".to_owned(), false, false),
                (7, "d/e".to_owned(), true, false),
                (8, "f".to_owned(), false, true),
                (11, "h".to_owned(), true, false),
                (14, "i".to_owned(), false, false),
            ]
        );
        let tables: Vec<_> = dependencies(manifest).iter().map(|d| d.table).collect();
        use Table::*;
        assert!(tables == [Normal, Normal, Normal, Normal, Dev, Workspace]);
    }
    #[test]
    fn layer_boundaries_reject_a_forbidden_import() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("crates/luxforge-app/src/state");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join("clean.rs"),
            "use crate::state::fields::Fields;\nlet snapp::x = wrapp::y;\n",
        )
        .unwrap();
        assert_eq!(read(tmp.path(), LAYERS).unwrap(), (1, 0));
        fs::write(state.join("bad.rs"), "use iced::widget::text;\n").unwrap();
        let error = refusal(tmp.path(), LAYERS, "an Iced import");
        assert!(
            error.contains("state/bad.rs:1:")
                && error.contains("`use iced`")
                && error.contains("source rule `state-layer`")
                && error.contains("SOURCE_RULES"),
            "{error}"
        );
        // The view model never imports the update layer, the view or the widget crate, in any
        // spelling of the path.
        for (import, token) in [
            ("use crate::app::message::Message;\n", "app::"),
            ("\nlet step = super::app::crop::ANGLE_STEP;\n", "app::"),
            ("use crate::{\n    app::fields::Fields,\n};\n", "app::"),
            (
                "/// Seeded like [`crate::app::Editor`] seeds them.\n",
                "app::",
            ),
            ("use crate::view::STATE_PANEL_WIDTH;\n", "view::"),
            ("let inset = crate::view::canvas::FIT_INSET;\n", "view::"),
            (
                "use crate::{\n    layout,\n    view,\n    view::canvas,\n};\n",
                "view::",
            ),
            ("use luxforge_ui::geometry::quantize;\n", "luxforge_ui"),
            (
                "let divider = luxforge_ui::theme::BORDER_WIDTH;\n",
                "luxforge_ui",
            ),
            (
                "pub(crate) icon: Option<luxforge_ui::Icon>,\n",
                "luxforge_ui",
            ),
        ] {
            fs::write(state.join("bad.rs"), import).unwrap();
            let error = refusal(tmp.path(), LAYERS, import);
            assert!(
                error.contains("bad.rs:") && error.contains(&format!("found `{token}`")),
                "{import:?}: {error}"
            );
        }
        // Nor does it refuse what merely ends in `view` or `app`.
        fs::write(
            state.join("bad.rs"),
            "use crate::layout::FIT_INSET;\nlet p = preview::x;\nlet c = canvas_view::y;\n",
        )
        .unwrap();
        assert_eq!(read(tmp.path(), LAYERS).unwrap(), (2, 0));
        fs::remove_file(state.join("bad.rs")).unwrap();
        let view = tmp.path().join("crates/luxforge-app/src/view");
        fs::create_dir_all(&view).unwrap();
        fs::write(
            view.join("bad.rs"),
            "\nlet state: luxforge_core::EditorState;\n",
        )
        .unwrap();
        assert!(refusal(tmp.path(), LAYERS, "the core in the view").contains("`luxforge_core`"));
        fs::write(view.join("bad.rs"), "owner.call(client, request)\n").unwrap();
        assert!(refusal(tmp.path(), LAYERS, "an owner call in the view").contains("`.call(`"));
        fs::remove_file(view.join("bad.rs")).unwrap();
        // The canvases live in the view and are held to it, their tests included.
        fs::write(
            view.join("crop_canvas.rs"),
            "fn f() {}\n#[cfg(test)]\nmod tests {\n    use luxforge_core::CropStage;\n}\n",
        )
        .unwrap();
        let error = refusal(tmp.path(), LAYERS, "the core in a canvas's tests");
        assert!(
            error.contains("view/crop_canvas.rs:4:") && error.contains("`luxforge_core`"),
            "{error}"
        );
        fs::remove_file(view.join("crop_canvas.rs")).unwrap();
        let ui = tmp.path().join("crates/luxforge-ui");
        fs::create_dir_all(&ui).unwrap();
        fs::write(
            ui.join("Cargo.toml"),
            "[dependencies]\nluxforge_core = { path = \"../luxforge-core\" }\n",
        )
        .unwrap();
        assert!(
            refusal(tmp.path(), LAYERS, "the core in the widget crate")
                .contains("luxforge-ui/Cargo.toml:2:")
        );
    }
    #[test]
    fn the_reference_crate_may_depend_on_no_workspace_crate() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("crates/luxforge-reference/Cargo.toml");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let clean = "[package]\nname = \"luxforge-reference\"\n\n[dependencies]\n\n\
                     [dev-dependencies]\nserde.workspace = true # not luxforge\n";
        fs::write(&manifest, clean).unwrap();
        let rule = &["independent-references"];
        assert_eq!(read(tmp.path(), rule).unwrap(), (0, 1));
        for (what, extra) in [
            (
                "the core",
                "[dependencies]\nluxforge-core = { path = \"../luxforge-core\" }\n",
            ),
            (
                "a crate that depends on the core",
                "[dev-dependencies]\nluxforge-testkit.workspace = true\n",
            ),
            (
                "the core under another name",
                "[dependencies]\ncore = { package = \"luxforge-core\", version = \"0\" }\n",
            ),
            (
                "a table naming the core",
                "[target.'cfg(unix)'.dependencies.luxforge-core]\nversion = \"0\"\n",
            ),
            (
                "any path",
                "[build-dependencies]\nhelper = { path = \"../helper\" }\n",
            ),
        ] {
            fs::write(&manifest, format!("{clean}\n{extra}")).unwrap();
            let error = refusal(tmp.path(), rule, what);
            assert!(
                error.contains("luxforge-reference/Cargo.toml:")
                    && error.contains("no workspace crate")
                    && error.contains("DEPENDENCY_RULES"),
                "{what}: {error}"
            );
        }
    }

    #[test]
    fn only_the_codec_crate_names_the_jpeg_codec() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for dir in [
            "crates/luxforge-jpeg/src",
            "crates/luxforge-core/src/export",
            "crates/luxforge-app",
            "crates/luxforge-testkit",
            "xtask",
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let write = |path: &str, text: &str| fs::write(root.join(path), text).unwrap();
        write(
            "Cargo.toml",
            "[workspace.dependencies]\nimage = { version = \"1\", features = [\"png\"] }\n",
        );
        write(
            "crates/luxforge-jpeg/Cargo.toml",
            "[dependencies]\nmozjpeg.workspace = true\n\n[dev-dependencies]\n\
             image = { workspace = true, features = [\"jpeg\"] }\n",
        );
        write(
            "crates/luxforge-jpeg/src/lib.rs",
            "mod decode;\n#[cfg(test)]\nmod tests;\n",
        );
        write(
            "crates/luxforge-jpeg/src/decode.rs",
            "use mozjpeg::Decompress;\n",
        );
        write(
            "crates/luxforge-jpeg/src/tests.rs",
            "use mozjpeg_sys::x;\nlet d = image::codecs::jpeg::JpegDecoder::new(r);\n",
        );
        write(
            "crates/luxforge-core/Cargo.toml",
            "[dependencies]\nluxforge-jpeg = { path = \"../luxforge-jpeg\" }\n\n\
             [dev-dependencies]\nimage = { workspace = true, features = [\"jpeg\"] }\n",
        );
        write(
            "crates/luxforge-core/src/lib.rs",
            "mod export;\n#[cfg(test)]\nmod oracle;\n",
        );
        write(
            "crates/luxforge-core/src/oracle.rs",
            "let d = image::codecs::jpeg::JpegDecoder::new(r);\n",
        );
        write(
            "crates/luxforge-core/src/export.rs",
            "pub fn encode() {}\n\n#[cfg(test)]\nmod tests {\n    use image::load_from_memory;\n    \
             fn f() {\n    }\n}\n",
        );
        // The codec's lib.rs and decode.rs, the core's lib.rs and export.rs; the test modules are
        // not read. And the workspace's, the codec's and the core's manifests.
        assert_eq!(read(root, JPEG).unwrap(), (4, 3));

        for (what, path, text, expected) in [
            (
                "mozjpeg outside the codec crate",
                "crates/luxforge-core/src/export/encode.rs",
                "use mozjpeg::Compress;\n",
                "may name mozjpeg",
            ),
            (
                "the bindings outside the codec crate",
                "crates/luxforge-core/src/export/encode.rs",
                "let e = mozjpeg_sys::jpeg_error_mgr::default();\n",
                "may name mozjpeg",
            ),
            (
                "an image JPEG decode after a test module",
                "crates/luxforge-core/src/export.rs",
                "#[cfg(test)]\nmod tests {\n}\nfn open(p: &Path) { image::open(p); }\n",
                "found `image::open`",
            ),
            (
                "an image JPEG decode in the codec crate",
                "crates/luxforge-jpeg/src/container.rs",
                "let d = image::load_from_memory(b);\n",
                "found `load_from_memory`",
            ),
        ] {
            write(path, text);
            let error = refusal(root, JPEG, what);
            assert!(error.contains(expected), "{what}: {error}");
            fs::remove_file(root.join(path)).unwrap();
        }
        write("crates/luxforge-core/src/export.rs", "pub fn encode() {}\n");

        for (what, path, text, expected) in [
            (
                "a shipped crate with image's jpeg feature",
                "crates/luxforge-app/Cargo.toml",
                "[dependencies]\nimage = { workspace = true, features = [\"jpeg\"] }\n",
                "jpeg",
            ),
            (
                "the jpeg feature in a multi-line array",
                "crates/luxforge-app/Cargo.toml",
                "[dependencies]\nimage = { workspace = true, features = [\n    \"png\",\n    \
                 \"jpeg\",\n] }\n",
                "Cargo.toml:2: a shipped crate may not enable image's jpeg feature",
            ),
            (
                "another crate linking the codec",
                "crates/luxforge-app/Cargo.toml",
                "[target.'cfg(unix)'.dependencies]\nmozjpeg-sys = \"2\"\n",
                "only luxforge-jpeg links",
            ),
            (
                "the core linking the codec itself",
                "crates/luxforge-core/Cargo.toml",
                "[dependencies]\nmozjpeg.workspace = true\n",
                "only luxforge-jpeg links",
            ),
            (
                "the workspace enabling jpeg",
                "Cargo.toml",
                "[workspace.dependencies]\nimage = { version = \"1\", features = [\"jpeg\"] }\n",
                "workspace image",
            ),
            (
                "another crate depending on the codec crate",
                "crates/luxforge-app/Cargo.toml",
                "[dependencies]\nluxforge-jpeg = { path = \"../luxforge-jpeg\" }\n",
                "only luxforge-core may depend on luxforge-jpeg",
            ),
            (
                "a test crate depending on it in a table of its own",
                "crates/luxforge-testkit/Cargo.toml",
                "[dev-dependencies.luxforge-jpeg]\npath = \"../luxforge-jpeg\"\n",
                "only luxforge-core may depend on luxforge-jpeg",
            ),
            (
                "xtask depending on it under another name",
                "xtask/Cargo.toml",
                "[dependencies]\njpeg = { package = \"luxforge-jpeg\", path = \"../x\" }\n",
                "only luxforge-core may depend on luxforge-jpeg",
            ),
            (
                "the codec crate depending on a workspace crate",
                "crates/luxforge-jpeg/Cargo.toml",
                "[dependencies]\nluxforge-process = { path = \"../luxforge-process\" }\n",
                "luxforge-jpeg may depend on no workspace crate",
            ),
            (
                "the codec crate depending on any path",
                "crates/luxforge-jpeg/Cargo.toml",
                "[build-dependencies]\nhelper = { path = \"../helper\" }\n",
                "luxforge-jpeg may depend on no workspace crate",
            ),
        ] {
            let before = fs::read_to_string(root.join(path)).unwrap_or_default();
            write(path, text);
            let error = refusal(root, JPEG, what);
            assert!(error.contains(expected), "{what}: {error}");
            write(path, &before);
        }
        // The app's, the test kit's and xtask's manifests are now read too.
        assert_eq!(read(root, JPEG).unwrap(), (4, 6));
    }

    #[test]
    fn only_the_registry_resolver_words_the_presettable_action_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("crates/luxforge-core/src");
        for dir in [core.join("modules/registry"), core.join("presets")] {
            fs::create_dir_all(dir).unwrap();
        }
        let wording = "format!(\"{id} is not a field-patch action\")\n";
        // The resolver, a test file and a test item may word it; a comment names nothing.
        for (file, text) in [
            (core.join("modules/registry/lookups.rs"), wording.to_owned()),
            (core.join("presets/library_tests.rs"), wording.to_owned()),
            (
                core.join("presets/mod.rs"),
                format!(
                    "#[cfg(test)]\nmod tests {{\n    {wording}}}\n// X is not a field-patch action\n"
                ),
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(read(tmp.path(), &["presettable-action"]).unwrap(), (1, 0));
        // A second copy of the check anywhere else is refused.
        let copy = core.join("presets/library.rs");
        fs::write(&copy, wording).unwrap();
        let error = refusal(tmp.path(), &["presettable-action"], "a second copy");
        assert!(
            error.contains("library.rs:1") && error.contains("ModuleRegistry::patch_action"),
            "{error}"
        );
        fs::remove_file(copy).unwrap();
        assert_eq!(read(tmp.path(), &["presettable-action"]).unwrap(), (1, 0));
    }

    #[test]
    fn only_the_thumbnail_workers_job_names_a_stack_on_the_desktop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let alias = "pub(crate) type ThumbnailJob = luxforge_core::Evaluation;\n";
        // The thumbnail worker's job type, a test file, a test item, a comment and a longer name
        // may.
        write_all(
            root,
            &[
                ("crates/luxforge-app/src/app/thumbnails.rs", alias),
                (
                    "crates/luxforge-app/src/app/preview_tests.rs",
                    "let stack: Evaluation = evaluation();\n",
                ),
                (
                    "crates/luxforge-app/src/app/preview.rs",
                    "#[cfg(test)]\nmod tests {\n    use luxforge_core::Evaluation;\n}\n\
                     // An Evaluation holds its source.\nlet plan = EvaluationPlan::new();\n",
                ),
            ],
        );
        assert_eq!(read(root, &["desktop-keeps-no-stack"]).unwrap(), (2, 0));
        // A second line in the worker's own file, or any other desktop file, is refused.
        refuses_each(
            root,
            "desktop-keeps-no-stack",
            &[(
                "crates/luxforge-app/src/app/sync.rs",
                "latest: Option<(AnalysisIdentity, Evaluation)>,\n",
            )],
            "the desktop keeps no evaluation",
        );
        write_all(
            root,
            &[(
                "crates/luxforge-app/src/app/thumbnails.rs",
                &format!("{alias}latest: Option<(AnalysisIdentity, Evaluation)>,\n"),
            )],
        );
        let error = refusal(root, &["desktop-keeps-no-stack"], "a kept stack");
        assert!(error.contains("thumbnails.rs:2"), "{error}");
    }

    #[test]
    fn only_the_files_that_pass_a_preview_job_through_name_one_on_the_desktop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // The message files, the owner tasks, the preview request and the two answers may, on as
        // many lines as they need; so may a test file, a test item, a comment and a longer name.
        write_all(
            root,
            &[
                (
                    "crates/luxforge-app/src/app/message.rs",
                    "Preview(preview::PreviewMessage),\nfn carried(job: PreviewJob) {}\n",
                ),
                (
                    "crates/luxforge-app/src/app/message/crop.rs",
                    "PreviewReady(StagePlan, Result<Box<PreviewJob>, String>),\n",
                ),
                (
                    "crates/luxforge-app/src/app/message/preview.rs",
                    "ThumbnailSource(Result<Box<PreviewJob>, String>),\n",
                ),
                (
                    "crates/luxforge-app/src/app/tasks.rs",
                    "pub(crate) job: PreviewJob,\n) -> Result<PreviewJob, String> {\n",
                ),
                (
                    "crates/luxforge-app/src/app/preview.rs",
                    "pub(crate) fn request_preview(&mut self, job: PreviewJob) -> u64 {\n",
                ),
                (
                    "crates/luxforge-app/src/app/gesture.rs",
                    "result: Result<(Draft, Option<PreviewJob>, RoundTrip), String>,\n",
                ),
                (
                    "crates/luxforge-app/src/app/thumbnails.rs",
                    "fn note_thumbnail_source(&mut self, job: &PreviewJob) {\n",
                ),
                (
                    "crates/luxforge-app/src/app/preview_tests.rs",
                    "let job: PreviewJob = planned();\n",
                ),
                (
                    "crates/luxforge-app/src/app/crop.rs",
                    "// A PreviewJob holds its stack.\n#[cfg(test)]\nmod tests {\n    \
                     use luxforge_core::PreviewJob;\n}\nlet plans = PreviewJobs::new();\n",
                ),
            ],
        );
        // Only the one file outside those homes is read: an allowed home is not.
        assert_eq!(
            read(root, &["desktop-keeps-no-preview-job"]).unwrap(),
            (1, 0)
        );
        // A job kept in the crop draft, the editor or the view model is refused.
        refuses_each(
            root,
            "desktop-keeps-no-preview-job",
            &[
                (
                    "crates/luxforge-app/src/app/crop_stage.rs",
                    "job: Option<Box<luxforge_core::PreviewJob>>,\n",
                ),
                (
                    "crates/luxforge-app/src/app/mod.rs",
                    "pending: Option<PreviewJob>,\n",
                ),
                (
                    "crates/luxforge-app/src/state/canvas.rs",
                    "stage: Vec<PreviewJob>,\n",
                ),
            ],
            "the desktop keeps no planned preview job",
        );
        write_all(
            root,
            &[(
                "crates/luxforge-app/src/app/crop.rs",
                "job: Option<Box<luxforge_core::PreviewJob>>,\n",
            )],
        );
        // The crop seam's own state file is not its message file.
        let error = refusal(root, &["desktop-keeps-no-preview-job"], "a kept job");
        assert!(error.contains("app/crop.rs:1"), "{error}");
    }

    #[test]
    fn no_client_matches_the_cores_refusal_text() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("crates/luxforge-core/src");
        let app = tmp.path().join("crates/luxforge-app/src");
        for dir in [core.join("api"), core.join("modules"), app.join("app")] {
            fs::create_dir_all(dir).unwrap();
        }
        let wording = "        format!(\"unavailable effect {effect_id}\")\n";
        let full = "        Error::source_queue_full(\"source preparation queue is full\")\n";
        // The constructor's home, the queues' owner, a test file, a test item and a comment.
        for (file, text) in [
            (core.join("error.rs"), wording.to_owned()),
            (core.join("api/owner.rs"), full.to_owned()),
            (core.join("modules/pixel_tests.rs"), wording.to_owned()),
            (
                app.join("app/tasks.rs"),
                format!(
                    "#[cfg(test)]\nmod tests {{\n    {full}}}\n// when its queue is full\n\
                     // it reports the unavailable effect\n"
                ),
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(read(tmp.path(), &["refusal-text"]).unwrap(), (1, 0));
        // The desktop matching either message is refused, and so is a second producer's wording.
        for (file, text, found) in [
            (
                app.join("app/tasks.rs"),
                "    error.detail.starts_with(\"RAW mosaic queue is full\")\n",
                "tasks.rs:1",
            ),
            (
                app.join("app/canvas.rs"),
                "const PREFIX: &str = \"unavailable effect \";\n",
                "canvas.rs:1",
            ),
            (core.join("modules/pixel.rs"), wording, "pixel.rs:1"),
        ] {
            let before = fs::read_to_string(&file).ok();
            fs::write(&file, text).unwrap();
            let error = refusal(tmp.path(), &["refusal-text"], found);
            assert!(
                error.contains(found) && error.contains("Error::unavailable_effect"),
                "{error}"
            );
            match before {
                Some(before) => fs::write(&file, before).unwrap(),
                None => fs::remove_file(&file).unwrap(),
            }
        }
        assert_eq!(read(tmp.path(), &["refusal-text"]).unwrap(), (1, 0));
    }

    #[test]
    fn scenarios_locate_the_photograph_through_one_locator() {
        let tmp = tempfile::tempdir().unwrap();
        let scenario = tmp.path().join("xtask/src/scenario");
        fs::create_dir_all(&scenario).unwrap();
        let read_rect = "        serde_json::from_value(self[\"photo_rect\"].clone())\n";
        fs::write(scenario.join("pixels.rs"), read_rect).unwrap();
        // Writing the key into a record is not reading it.
        fs::write(
            tmp.path().join("xtask/src/zoom_smoke.rs"),
            "        json!({\"photo_rect\": photo})\n",
        )
        .unwrap();
        assert_eq!(read(tmp.path(), &["one-photo-locator"]).unwrap(), (1, 0));
        // A scenario reading the rectangle for itself is a second locator.
        let own = "    let rect = &frame[\"photo_rect\"];\n";
        fs::write(tmp.path().join("xtask/src/vignette_smoke.rs"), own).unwrap();
        let error = refusal(tmp.path(), &["one-photo-locator"], own);
        assert!(
            error.contains("vignette_smoke.rs:") && error.contains("Frame::photo"),
            "{error}"
        );
    }

    #[test]
    fn the_pipeline_keeps_one_read_rectangle_and_one_spatial_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("crates/luxforge-core/src");
        for dir in [core.join("render"), core.join("modules/presence")] {
            fs::create_dir_all(dir).unwrap();
        }
        let floor = "                let index = (value - 0.5).floor();\n";
        let keyed = "            &domain.estimate_prefix(self.prefix_hash),\n";
        // Each home writes its token once; the trait's declarations, the windowed proxy's own halo
        // and tile rule (`WindowPlan::of_rect`), test items and test-only modules are not copies.
        for (file, text) in [
            (core.join("render/geometry.rs"), floor.to_owned()),
            (
                core.join("render/pipeline.rs"),
                format!(
                    "    fn estimate_prefix<'p>(&self, prefix_hash: &'p str) -> Cow<'p, str>;\n{keyed}\
                     #[cfg(test)]\nmod tests {{\n{floor}{keyed}}}\n"
                ),
            ),
            (
                core.join("render/window.rs"),
                "                    let grown = read.grown(operation.summed_halo(input), input);\n\
                 let tile = Tiling::Halo.tile(operation, input);\n\
                 let x0 = grown.x0 / tile * tile;\n\
                 needed = resample.reads((0, 0), read, stage);\n"
                    .to_owned(),
            ),
            (
                core.join("modules/presence/mod.rs"),
                "#[cfg(test)]\nmod oracle;\n".to_owned(),
            ),
            (core.join("modules/presence/oracle.rs"), keyed.to_owned()),
            (core.join("render/linear_tests.rs"), floor.to_owned()),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(
            read(tmp.path(), &["one-read-rectangle", "one-spatial-entry"]).unwrap(),
            (4, 0)
        );
        // The copies this rule replaced, brought back: the proxy window's and the linear tap
        // block's read rectangles, the byte band's second one beside `Resample::reads`, and the
        // byte driver's inline estimate resolution.
        for (file, text) in [
            (core.join("render/window.rs"), floor),
            (core.join("render/linear.rs"), floor),
            (
                core.join("render/byte.rs"),
                "    let start = (top - 0.5).floor() - 2.0;\n",
            ),
            (
                core.join("render/byte.rs"),
                "                                &domain.estimate_prefix(prefix_hash),\n",
            ),
        ] {
            let clean = fs::read_to_string(&file).ok();
            fs::write(&file, format!("{}{text}", clean.as_deref().unwrap_or(""))).unwrap();
            let error = refusal(
                tmp.path(),
                &["one-read-rectangle", "one-spatial-entry"],
                text,
            );
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                error.contains(&format!("{name}:")) && error.contains("written once"),
                "{error}"
            );
            match clean {
                Some(clean) => fs::write(&file, clean).unwrap(),
                None => fs::remove_file(&file).unwrap(),
            }
        }
        assert_eq!(
            read(tmp.path(), &["one-read-rectangle", "one-spatial-entry"]).unwrap(),
            (4, 0)
        );
    }

    #[test]
    fn only_the_raw_module_and_tests_name_its_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("crates/luxforge-core/src");
        let state = tmp.path().join("crates/luxforge-app/src/state");
        for dir in [
            core.join("modules/raw"),
            core.join("editor"),
            tmp.path().join("crates/luxforge-core/tests"),
            state.clone(),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        // The module, its own directory, test files, test items and test-only modules may name it,
        // and a comment or a longer identifier is not a name.
        for (file, text) in [
            (
                core.join("modules/raw.rs"),
                "pub const RAW_EFFECT: &str = \"luxforge.raw\";\n",
            ),
            (
                core.join("modules/raw/white_balance.rs"),
                "use super::RAW_EFFECT;\n",
            ),
            (core.join("modules/lookups_tests.rs"), "RAW_EFFECT\n"),
            (
                tmp.path().join("crates/luxforge-core/tests/raw.rs"),
                "\"luxforge.raw\"\n",
            ),
            (
                state.join("mod.rs"),
                "#[cfg(test)]\nmod testing;\n#[cfg(test)] mod fixtures;\nlet x = 1;\n\
                 #[cfg(test)]\nmod tests {\n    fn raw() {\n        let id = \"luxforge.raw\";\n    }\n}\n\
                 /// Never `RAW_EFFECT` by name.\nlet MY_RAW_EFFECTS = 2;\n",
            ),
            (state.join("testing.rs"), "let id = \"luxforge.raw\";\n"),
            (state.join("fixtures.rs"), "let id = \"luxforge.raw\";\n"),
            (
                core.join("editor/source.rs"),
                "#[cfg(test)]\nuse crate::RAW_EFFECT;\nfn f() {}\n",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(read(tmp.path(), &["raw-identity"]).unwrap(), (2, 0));
        // Anywhere else in product code either spelling is refused, after a test item too.
        for (file, text) in [
            (
                core.join("editor/source.rs"),
                "#[cfg(test)]\nfn t() {\n}\nfn f(l: &Layer) -> bool { l.effect_id == crate::RAW_EFFECT }\n",
            ),
            (
                state.join("canvas.rs"),
                "let raw = mode != \"luxforge.raw\";\n",
            ),
            (
                core.join("modules/mod.rs"),
                "pub use raw::{RAW_EFFECT, RawModule};\n",
            ),
            (core.join("modules/rawish.rs"), "RAW_EFFECT\n"),
        ] {
            let clean = fs::read_to_string(&file).ok();
            fs::write(&file, text).unwrap();
            let error = refusal(tmp.path(), &["raw-identity"], &file.display().to_string());
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                error.contains(&format!("{name}:")) && error.contains("only the RAW module"),
                "{error}"
            );
            match clean {
                Some(clean) => fs::write(&file, clean).unwrap(),
                None => fs::remove_file(&file).unwrap(),
            }
        }
        assert_eq!(read(tmp.path(), &["raw-identity"]).unwrap(), (2, 0));
    }

    /// Every control message needs a sender outside the evidence driver and tests: the view's
    /// widgets, or product code in `app/` such as the handler's own dispatch. The handler's match
    /// arms are not senders, and a sender split across lines by rustfmt still is one.
    #[test]
    fn every_control_message_has_a_sender_outside_evidence_and_tests() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let app = "crates/luxforge-app/src/app";
        write_all(
            root,
            &[
                (
                    "crates/luxforge-app/src/app/message/control.rs",
                    "pub(crate) enum ControlMessage {\n    /// A rail position.\n    Fraction {\n        \
                     fraction: f64,\n    },\n    #[allow(dead_code)]\n    Scripted {\n        \
                     value: f64,\n    },\n    ToggleSection(String),\n    Answered,\n}\n\
                     pub(crate) enum Other {\n    Unsent,\n}\n",
                ),
                (
                    "crates/luxforge-app/src/app/controls.rs",
                    "match message {\n    ControlMessage::Fraction { fraction } => {}\n    \
                     ControlMessage::Scripted { value } => {}\n    \
                     ControlMessage::ToggleSection(id) => {}\n    ControlMessage::Answered => {}\n}\n\
                     let task = Message::Control(ControlMessage::Answered);\n",
                ),
                (
                    "crates/luxforge-app/src/view/tools_panel.rs",
                    "slider(move |fraction| Message::Control(\n    ControlMessage::Fraction {\n        \
                     fraction,\n    }\n));\n\
                     button(Message::Control(ControlMessage::ToggleSection(id)));\n\
                     // ControlMessage::Scripted is only a comment here.\n",
                ),
                (
                    "crates/luxforge-app/src/app/evidence.rs",
                    "self.update(Message::Control(ControlMessage::Scripted { value }));\n",
                ),
                (
                    "crates/luxforge-app/src/app/slider_tests.rs",
                    "editor.update(Message::Control(ControlMessage::Scripted { value: 1.0 }));\n",
                ),
                (
                    "crates/luxforge-app/src/app/mod.rs",
                    "#[cfg(test)]\nmod tests {\n    fn t() { let m = ControlMessage::Scripted { value: 1.0 }; }\n}\n",
                ),
            ],
        );
        // Scripted is constructed only by evidence, a test file and a test module, and named in
        // the handler's arm and a comment: refused, by name, and the only refusal.
        let error = refusal(root, &["widget-sent-controls"], "an evidence-only message");
        assert!(
            error.contains("sender rule `widget-sent-controls`")
                && error.contains("`ControlMessage::Scripted`")
                && error.lines().count() == 1,
            "{error}"
        );
        // A widget sending it is enough.
        write_all(
            root,
            &[(
                "crates/luxforge-app/src/view/tools_panel.rs",
                "slider(move |fraction| Message::Control(\n    ControlMessage::Fraction {\n        \
                 fraction,\n    }\n));\n\
                 button(Message::Control(ControlMessage::ToggleSection(id)));\n\
                 field(Message::Control(ControlMessage::Scripted { value }));\n",
            )],
        );
        // The declaration, the handler, the view and the production lines of `app/mod.rs`.
        assert_eq!(read(root, &["widget-sent-controls"]).unwrap(), (4, 0));
        // So a message only the handler's own arm names has no sender.
        fs::write(
            root.join(app).join("controls.rs"),
            "match message {\n    ControlMessage::Fraction { fraction } => {}\n    \
             ControlMessage::Answered => {}\n}\n",
        )
        .unwrap();
        let error = refusal(root, &["widget-sent-controls"], "an unsent message");
        assert!(error.contains("`ControlMessage::Answered`"), "{error}");
        assert_eq!(
            enum_variants(
                &fs::read_to_string(root.join(app).join("message/control.rs")).unwrap(),
                "ControlMessage"
            ),
            Some(vec!["Fraction", "Scripted", "ToggleSection", "Answered"])
        );
    }

    /// Write each `(path, text)` under `root`, creating its directories.
    fn write_all(root: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    /// Refuse each `(path, text)` in turn, with `expected` in the refusal, removing it afterwards.
    fn refuses_each(root: &Path, rule: &str, files: &[(&str, &str)], expected: &str) {
        for (path, text) in files {
            write_all(root, &[(path, text)]);
            let error = refusal(root, &[rule], path);
            assert!(
                error.contains(&format!("{path}:"))
                    && error.contains(expected)
                    && error.contains(&format!("source rule `{rule}`")),
                "{path}: {error}"
            );
            fs::remove_file(root.join(path)).unwrap();
        }
    }

    #[test]
    fn only_the_launch_envelope_assembles_editor_arguments() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // The envelope, the hidden-window flag's home, test code, xtask's own flags and a longer
        // name may.
        write_all(
            root,
            &[
                (
                    "xtask/src/scenario/launch.rs",
                    "args.extend([\"--evidence-dir\".into(), dir.into()]);\n",
                ),
                (
                    "xtask/src/launch.rs",
                    "pub fn editor_args(args: &[OsString]) {}\n",
                ),
                (
                    "xtask/src/verify.rs",
                    "let a = [\"--output\", \"--binary\"];\nlet b = \"--open-all\";\n\
                     #[cfg(test)]\nmod tests {\n    fn t() { let a = [\"--open\"]; }\n}\n",
                ),
            ],
        );
        assert_eq!(read(root, &["editor-launch"]).unwrap(), (1, 0));
        // Anywhere else in the harness, each spelling is refused: the RAW editor journey's own
        // launch included, now that it is a scenario row.
        refuses_each(
            root,
            "editor-launch",
            &[
                (
                    "xtask/src/raw_editor.rs",
                    "let child = spawn_editor(root, binary, &[\"--catalog\".into()], &log)?;\n",
                ),
                (
                    "xtask/src/editor_latency.rs",
                    "let args = vec![\"--evidence-dir\".into(), evidence.into()];\n",
                ),
                (
                    "xtask/src/diagnostics.rs",
                    "args.extend([\"--data-root\".into(), data.into()]);\n",
                ),
                ("xtask/src/smoke.rs", "args.push(\"--developer\".into());\n"),
                (
                    "xtask/src/measure.rs",
                    "let child = scenario::launch::spawn_editor(root, bin, &args, &log)?;\n",
                ),
                ("xtask/src/tool.rs", "let args = editor_args(&args);\n"),
            ],
            "scenario library's Launch",
        );
    }

    #[test]
    fn only_the_one_assembly_builds_a_runs_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // The assembly, test code and a longer name may.
        write_all(
            root,
            &[
                (
                    "crates/luxforge-core/src/modules/registry/mod.rs",
                    "Arc::new(PixelModule::new()),\nregistry.register_unavailable(module, r)\n",
                ),
                (
                    "crates/luxforge-cli/src/json.rs",
                    "let r = ModuleRegistry::assemble(&options)?;\nlet p = MyPixelModule::new();\n\
                     #[cfg(test)]\nmod tests {\n    fn t() { ControlsModule::new(); }\n}\n",
                ),
            ],
        );
        assert_eq!(read(root, &["registry-assembly"]).unwrap(), (1, 0));
        // Anywhere else, in a binary, the core or the harness, each is refused.
        refuses_each(
            root,
            "registry-assembly",
            &[
                (
                    "crates/luxforge-app/src/app/lifecycle.rs",
                    "registry.register_unavailable(module, DISABLED_REASON)?;\n",
                ),
                (
                    "crates/luxforge-cli/src/json.rs",
                    "registry.register(Arc::new(CapabilitiesProofModule::new(base)))?;\n",
                ),
                (
                    "crates/luxforge-core/src/editor.rs",
                    "let pixel = Arc::new(PixelModule::new());\n",
                ),
                (
                    "xtask/src/controls_smoke.rs",
                    "registry.register(Arc::new(ControlsModule::new()))?;\n",
                ),
            ],
            "ModuleRegistry::assemble",
        );
    }

    #[test]
    fn only_raw_rs_reads_a_raw_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // The one reader, a scenario using a source it read, and test code may.
        write_all(
            root,
            &[
                ("xtask/src/raw.rs", "    pub neutral_point: [u32; 2],\n"),
                (
                    "xtask/src/raw_editor.rs",
                    "let (listed, _) = listed(run)?;\njourney(listed.neutral_point)\n\
                     #[cfg(test)]\nmod tests {\n    fn t() { let s = &m[\"sources\"]; }\n}\n",
                ),
            ],
        );
        assert_eq!(read(root, &["raw-manifest-reader"]).unwrap(), (1, 0));
        // A second reader, typed or untyped, anywhere else in the harness is refused.
        refuses_each(
            root,
            "raw-manifest-reader",
            &[
                (
                    "xtask/src/verify.rs",
                    "let sources = manifest[\"sources\"].as_array();\n",
                ),
                (
                    "xtask/src/raw_editor.rs",
                    "struct Source {\n    neutral_point: [u32; 2],\n}\n",
                ),
            ],
            "raw::manifest",
        );
    }

    #[test]
    fn production_threads_start_only_in_their_homes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A declared home, test code in any spelling, and a longer identifier may.
        write_all(
            root,
            &[
                (
                    "crates/luxforge-core/src/latest.rs",
                    "let worker = thread::Builder::new().spawn(run);\n",
                ),
                ("xtask/src/verify.rs", "std::thread::scope(|scope| {});\n"),
                (
                    "crates/luxforge-core/src/render/linear.rs",
                    "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn t() {\n        \
                     std::thread::spawn(|| {});\n    }\n}\nlet thread::spawner = 1;\n",
                ),
                (
                    "crates/luxforge-core/tests/cancellation.rs",
                    "thread::spawn(move || {});\n",
                ),
                (
                    "crates/luxforge-testkit/src/proof_tests.rs",
                    "thread::spawn(move || {});\n",
                ),
            ],
        );
        assert_eq!(read(root, &["thread-spawn"]).unwrap(), (1, 0));
        // Anywhere else in production code, each spelling is refused.
        refuses_each(
            root,
            "thread-spawn",
            &[
                (
                    "crates/luxforge-core/src/render/spatial.rs",
                    "let worker = std::thread::spawn(move || {});\n",
                ),
                ("xtask/src/main.rs", "let t = thread::Builder::new();\n"),
                (
                    "crates/luxforge-raw/src/lib.rs",
                    "use std::thread;\nfn f() { thread::scope(|s| {}); }\n",
                ),
            ],
            "declared worker homes",
        );
    }

    #[test]
    fn no_image_handle_is_made_from_pixels() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Another constructor may, the gallery's stand-ins made once may, the Select grid's and the
        // loupe's decoded previews made once each may, and the rules file names the token without
        // being read.
        write_all(
            root,
            &[
                (
                    "crates/luxforge-ui/src/photo.rs",
                    "let h = image::Handle::from_path(p);\nlet g = Handle::from_rgba8(p);\n",
                ),
                (
                    "crates/luxforge-ui/src/gallery_thumbnails.rs",
                    "Handle::from_rgba(w, h, render(&scene, ev))\n",
                ),
                (
                    "crates/luxforge-app/src/app/select_previews.rs",
                    "handle: Handle::from_rgba(width, height, rgba),\n",
                ),
                (
                    "crates/luxforge-app/src/app/loupe_frames.rs",
                    "handle: Handle::from_rgba(width, height, rgba),\n",
                ),
                (RULES_FILE, "tokens: &[\"Handle::from_rgba\"],\n"),
            ],
        );
        assert_eq!(read(root, &["no-pixel-image-handle"]).unwrap(), (1, 0));
        // In product and in test code alike, it is refused.
        refuses_each(
            root,
            "no-pixel-image-handle",
            &[
                (
                    "crates/luxforge-app/src/view/canvas.rs",
                    "let h = image::Handle::from_rgba(w, h, pixels);\n",
                ),
                (
                    "crates/luxforge-app/src/view/select.rs",
                    "image: Some(&Handle::from_rgba(w, h, pixels)),\n",
                ),
                (
                    "crates/luxforge-ui/src/photo_tests.rs",
                    "use iced::widget::image::Handle;\nHandle::from_rgba(1, 1, vec![0; 4]);\n",
                ),
            ],
            "uploads a new texture",
        );
    }

    #[test]
    fn only_the_module_transport_frames_http() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let transport = "crates/luxforge-net/src";
        // The transport and its tests may; a longer name and the rules file are not the token.
        write_all(
            root,
            &[
                (
                    &format!("{transport}/agent.rs"),
                    "use ureq_proto::Error;\nlet h = httparse::Status::Partial;\n",
                ),
                (
                    &format!("{transport}/tests.rs"),
                    "let e = ureq_proto::Error::HttpParseTooManyHeaders;\n",
                ),
                (
                    "crates/luxforge-core/src/capabilities/context.rs",
                    "use my_ureq_proto::x;\nlet agent = ureq::Agent::new_with_defaults();\n",
                ),
                (RULES_FILE, "tokens: &[\"ureq_proto::\", \"httparse::\"],\n"),
                (
                    "crates/luxforge-net/Cargo.toml",
                    "[dependencies]\nureq.workspace = true\nureq-proto.workspace = true\n",
                ),
                (
                    "crates/luxforge-testkit/Cargo.toml",
                    "[dependencies]\nurl.workspace = true\n",
                ),
            ],
        );
        let rules = ["http-framing", "http-client-crates"];
        assert!(read(root, &rules).is_ok());
        // Anywhere else, test code included, it is refused.
        refuses_each(
            root,
            "http-framing",
            &[
                (
                    "crates/luxforge-testkit/src/server.rs",
                    "let mut headers = [httparse::EMPTY_HEADER; 64];\n",
                ),
                (
                    "crates/luxforge-core/src/capabilities/transport.rs",
                    "use ureq_proto::client::Call;\n",
                ),
                (
                    "crates/luxforge-core/src/capabilities/proof_tests.rs",
                    "let call = ureq_proto::client::Call::new(request);\n",
                ),
                (
                    "xtask/src/scenario/capabilities.rs",
                    "let parsed = httparse::Response::new(&mut headers);\n",
                ),
            ],
            "speaks HTTP/1.1",
        );
        for (path, text) in [
            (
                "crates/luxforge-testkit/Cargo.toml",
                "[dev-dependencies]\nureq-proto = \"0.6\"\n",
            ),
            (
                "crates/luxforge-core/Cargo.toml",
                "[dependencies]\nureq.workspace = true\n",
            ),
            (
                "xtask/Cargo.toml",
                "[dependencies]\nclient = { package = \"ureq\", version = \"3\" }\n",
            ),
        ] {
            write_all(root, &[(path, text)]);
            let error = refusal(root, &rules, path);
            assert!(
                error.contains(&format!("{path}:"))
                    && error.contains("may depend on ureq")
                    && error.contains("DEPENDENCY_RULES"),
                "{path}: {error}"
            );
            fs::remove_file(root.join(path)).unwrap();
        }
    }

    #[test]
    fn a_durable_write_flushes_through_one_function_and_only_tests_skip_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // The one flush may name the call and the feature; a dev-dependency may turn it on; test
        // code and a longer name may name the call; xtask is out of scope.
        write_all(
            root,
            &[
                (
                    "crates/luxforge-core/src/atomic_file.rs",
                    "const FLUSHES: bool = !cfg!(feature = \"test-skip-disk-flush\");\n\
                     fn flush(file: &File) { file.sync_all() }\n",
                ),
                (
                    "crates/luxforge-core/src/store.rs",
                    "atomic_file::flush(&file)?;\nfn sync_all_entries() {}\n\
                     #[cfg(test)]\nmod tests {\n    fn t() { file.sync_all(); }\n}\n",
                ),
                ("xtask/src/package.rs", "zip.finish()?.sync_all()?;\n"),
                (
                    "crates/luxforge-app/Cargo.toml",
                    "[dependencies]\nluxforge-core = { path = \"../luxforge-core\" }\n\n\
                     [dev-dependencies]\nluxforge-core = { path = \"../luxforge-core\", \
                     features = [\"test-skip-disk-flush\"] }\n",
                ),
            ],
        );
        let rules = ["one-disk-flush", "disk-flush-only-in-tests"];
        assert!(read(root, &rules).is_ok());
        // Anywhere else in the crates' production code, each is refused.
        refuses_each(
            root,
            "one-disk-flush",
            &[
                (
                    "crates/luxforge-core/src/export/publish.rs",
                    "file.sync_all()?;\n",
                ),
                (
                    "crates/luxforge-app/src/diagnostics.rs",
                    "file.sync_data()?;\n",
                ),
                (
                    "crates/luxforge-core/src/editor.rs",
                    "if cfg!(feature = \"test-skip-disk-flush\") {}\n",
                ),
            ],
            "atomic_file::flush",
        );
        // A normal, build or workspace dependency that turns it on is refused.
        for (path, text) in [
            (
                "xtask/Cargo.toml",
                "[dependencies]\nluxforge-core = { path = \"../crates/luxforge-core\", \
                 features = [\"test-skip-disk-flush\"] }\n",
            ),
            (
                "crates/luxforge-cli/Cargo.toml",
                "[build-dependencies.luxforge-core]\npath = \"../luxforge-core\"\n\
                 features = [\"test-skip-disk-flush\"]\n",
            ),
            (
                "Cargo.toml",
                "[workspace.dependencies]\nluxforge-core = { path = \"crates/luxforge-core\", \
                 features = [\"test-skip-disk-flush\"] }\n",
            ),
        ] {
            write_all(root, &[(path, text)]);
            let error = refusal(root, &rules, path);
            assert!(
                error.contains(path) && error.contains("[dev-dependencies] table"),
                "{path}: {error}"
            );
            fs::remove_file(root.join(path)).unwrap();
        }
    }

    #[test]
    fn the_core_links_no_tls_http_or_keychain_crate() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("crates/luxforge-core/Cargo.toml");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let clean = "[package]\nname = \"luxforge-core\"\n\n[dependencies]\n\
                     url.workspace = true\nzeroize.workspace = true\n";
        fs::write(&manifest, clean).unwrap();
        let rule = &["core-links-no-network"];
        assert_eq!(read(tmp.path(), rule).unwrap(), (0, 1));
        // The crate that owns them may; the rule reads only the core's manifest.
        let net = tmp.path().join("crates/luxforge-net/Cargo.toml");
        fs::create_dir_all(net.parent().unwrap()).unwrap();
        fs::write(
            &net,
            "[dependencies]\nrustls.workspace = true\nsecurity-framework.workspace = true\n",
        )
        .unwrap();
        assert_eq!(read(tmp.path(), rule).unwrap(), (0, 1));
        for (what, extra) in [
            ("rustls", "rustls.workspace = true\n"),
            (
                "the platform verifier",
                "rustls-platform-verifier.workspace = true\n",
            ),
            ("ring", "ring = \"0.17\"\n"),
            ("ureq", "ureq.workspace = true\n"),
            ("ureq-proto", "ureq-proto.workspace = true\n"),
            (
                "the Keychain under a macOS target",
                "\n[target.'cfg(target_os = \"macos\")'.dependencies]\n\
                 security-framework.workspace = true\n",
            ),
            (
                "rustls for the tests",
                "\n[dev-dependencies]\nrustls.workspace = true\n",
            ),
            (
                "rustls under another name",
                "tls = { package = \"rustls\", version = \"0.23\" }\n",
            ),
        ] {
            fs::write(&manifest, format!("{clean}{extra}")).unwrap();
            let error = refusal(tmp.path(), rule, what);
            assert!(
                error.contains("luxforge-core/Cargo.toml:")
                    && error.contains("links no TLS")
                    && error.contains("DEPENDENCY_RULES"),
                "{what}: {error}"
            );
        }
    }

    #[test]
    fn the_old_project_name_does_not_return() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_all(
            root,
            &[
                (
                    "crates/luxforge-core/Cargo.toml",
                    "[package]\nname = \"luxforge-core\"\n",
                ),
                ("xtask/src/main.rs", "// Luxforge's own tasks.\n"),
                // Only text files are read.
                ("crates/luxforge-raw/vendor/libraw.cpp", "// lightwell\n"),
            ],
        );
        assert_eq!(read(root, &["project-name"]).unwrap(), (2, 0));
        refuses_each(
            root,
            "project-name",
            &[
                (
                    "crates/luxforge-app/Cargo.toml",
                    "[dependencies]\nlightwell-core = { path = \"../core\" }\n",
                ),
                ("xtask/src/main.rs", "// Lightwell's own tasks.\n"),
                (
                    "crates/luxforge-core/tests/fixtures.rs",
                    "const DIR: &str = \"LIGHTWELL_FIXTURES\";\n",
                ),
            ],
            "old working name",
        );
    }

    #[test]
    fn only_the_kind_tables_and_their_kinds_name_a_component_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let mask = tmp.path().join("crates/luxforge-core/src/mask");
        let draft = tmp.path().join("crates/luxforge-app/src/mask_draft");
        let app = tmp.path().join("crates/luxforge-app/src/app");
        for dir in [&mask.join("commands"), &draft, &app] {
            fs::create_dir_all(dir).unwrap();
        }
        // The kind table, each kind's own file, the desktop's drawn-kind table and its editors,
        // test files and test items may name a kind; a longer identifier, a comment and the bare
        // words a kind shares with other vocabularies are not a kind's name.
        for (file, text) in [
            (
                mask.join("mod.rs"),
                "pub const BRUSH: &str = brush::KIND;\nconst K: [&str; 1] = [range::COLOUR_KIND];\n",
            ),
            (
                mask.join("brush.rs"),
                "pub(super) const KIND: &str = \"brush\";\n",
            ),
            (
                mask.join("range.rs"),
                "pub(super) const LUMINANCE_KIND: &str = \"luminance-range\";\n",
            ),
            (
                draft.join("editor.rs"),
                "const DRAWN_KINDS: [&str; 1] = [super::radial::KIND];\n",
            ),
            (
                draft.join("brush.rs"),
                "pub(crate) const KIND: &str = luxforge_core::mask::BRUSH;\n",
            ),
            (
                app.join("masks_tests.rs"),
                "use crate::mask_draft::BRUSH;\n",
            ),
            (
                mask.join("commands/plan.rs"),
                "fn f(c: &Component) -> bool { component_geometry_is_drawn(&c.kind) }\n\
                 /// Never `BRUSH` by name.\nlet NEUTRAL_BRUSH = 1;\nlet state = json!({\"brush\": 1});\n\
                 #[cfg(test)]\nmod tests {\n    fn t() {\n        let k = super::BRUSH;\n    }\n}\n",
            ),
            (
                tmp.path().join("crates/luxforge-app/src/mask_draft.rs"),
                "#[cfg(test)]\npub(crate) const BRUSH: &str = brush::KIND;\nfn g() {}\n",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(read(tmp.path(), &["component-kind"]).unwrap(), (2, 0));
        // Anywhere else in product code every name is refused, a second kind dispatch included.
        for (file, text) in [
            (
                mask.join("commands/plan.rs"),
                "#[cfg(test)]\nfn t() {\n}\nfn f(c: &Component) -> bool { c.kind != BRUSH }\n",
            ),
            (
                app.join("masks.rs"),
                "self.begin_shape(MaskDraftOp::Create, BRUSH.to_owned(), None)\n",
            ),
            (
                tmp.path().join("crates/luxforge-app/src/mask_draft.rs"),
                "pub(crate) const BRUSH: &str = brush::KIND;\n",
            ),
            (
                app.join("panel.rs"),
                "let value_based = kind == \"colour-range\";\n",
            ),
            (mask.join("rules.rs"), "if kind == super::linear::KIND {}\n"),
            (
                mask.join("parameters.rs"),
                "let k = range::LUMINANCE_KIND;\n",
            ),
        ] {
            let clean = fs::read_to_string(&file).ok();
            fs::write(&file, text).unwrap();
            let error = refusal(tmp.path(), &["component-kind"], &file.display().to_string());
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                error.contains(&format!("{name}:")) && error.contains("ask the kind table"),
                "{error}"
            );
            match clean {
                Some(clean) => fs::write(&file, clean).unwrap(),
                None => fs::remove_file(&file).unwrap(),
            }
        }
        assert_eq!(read(tmp.path(), &["component-kind"]).unwrap(), (2, 0));
    }

    #[test]
    fn only_the_field_patch_and_raw_modules_declare_a_patch_action() {
        let tmp = tempfile::tempdir().unwrap();
        let modules = tmp.path().join("crates/luxforge-core/src/modules");
        fs::create_dir_all(&modules).unwrap();
        fs::create_dir_all(tmp.path().join("crates/luxforge-core/tests")).unwrap();
        // The two owners, test files and test items may declare one, and a comment or a longer
        // identifier is not a declaration.
        for (file, text) in [
            (modules.join("field_patch.rs"), "patch: true,\n"),
            (modules.join("raw.rs"), "patch: true,\n"),
            (
                tmp.path().join("crates/luxforge-core/tests/modules.rs"),
                "ActionDescriptor { patch: true, ..action }\n",
            ),
            (
                modules.join("controls.rs"),
                "#[cfg(test)]\nmod tests {\n    let a = ActionDescriptor { patch: true };\n}\n\
                 /// A `patch: true` action.\nlet dispatch: truest = 1;\n",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(read(tmp.path(), &["patch-action"]).unwrap(), (1, 0));
        // Anywhere else in product code a declaration is refused, as a literal or a struct update.
        for text in [
            "ActionDescriptor { id, patch: true, parameters }\n",
            "ActionDescriptor { patch: true, ..ActionDescriptor::new(id, title, notes) }\n",
        ] {
            fs::write(modules.join("controls.rs"), text).unwrap();
            let error = refusal(tmp.path(), &["patch-action"], text);
            assert!(
                error.contains("controls.rs:1") && error.contains("declare a patch action"),
                "{error}"
            );
        }
    }

    #[test]
    fn only_the_job_table_keeps_job_records_and_only_the_owner_creates_it() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("crates/luxforge-core/src");
        for dir in [core.join("api"), core.join("analysis")] {
            fs::create_dir_all(dir).unwrap();
        }
        // The table keeps its ring, the owner creates it once, and tests may build their own.
        for (file, text) in [
            (
                core.join("jobs.rs"),
                "struct Jobs { finished: VecDeque<JobId> }\n\
                 #[cfg(test)]\nmod tests {\n    fn t() { Jobs::new(d, b); }\n}\n",
            ),
            (
                core.join("api/owner.rs"),
                "let jobs = Jobs::new(deliver, board);\n",
            ),
            (
                core.join("api/owner_tests.rs"),
                "let jobs = Jobs::new(d, b);\n",
            ),
            (
                core.join("analysis/jobs.rs"),
                "struct Queue { generations: VecDeque<(JobId, u64)> }\n\
                 /// Not a `VecDeque<JobId>` ring.\n",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(
            read(tmp.path(), &["job-records", "job-table"]).unwrap(),
            (3, 0)
        );
        // A struct of its own with a finished ring, as each kind's table once had, is refused, and
        // so is a table created anywhere but the owner, or twice there.
        for (file, text, message) in [
            (
                core.join("api/owner.rs"),
                "struct SourceJobs { completed: VecDeque<JobId> }\nlet jobs = Jobs::new(d, b);\n",
                "only the one job table",
            ),
            (
                core.join("capabilities.rs"),
                "jobs: Jobs::new(deliver, board),\n",
                "only the catalog owner",
            ),
            (
                core.join("api/owner.rs"),
                "let jobs = Jobs::new(d, b);\nlet exports = Jobs::new(d, b);\n",
                "creates the job table, once",
            ),
        ] {
            let clean = fs::read_to_string(&file).ok();
            fs::write(&file, text).unwrap();
            let error = refusal(
                tmp.path(),
                &["job-records", "job-table"],
                &file.display().to_string(),
            );
            assert!(error.contains(message), "{error}");
            match clean {
                Some(clean) => fs::write(&file, clean).unwrap(),
                None => fs::remove_file(&file).unwrap(),
            }
        }
        assert_eq!(
            read(tmp.path(), &["job-records", "job-table"]).unwrap(),
            (3, 0)
        );
    }

    #[test]
    fn only_the_dispatcher_checks_a_mutation_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("crates/luxforge-core/src");
        fs::create_dir_all(core.join("api")).unwrap();
        fs::create_dir_all(core.join("capabilities")).unwrap();
        // The dispatcher checks both envelopes; a test may check one itself.
        for (file, text) in [
            (
                core.join("api/params.rs"),
                "Self::Revision => Mutation::deserialize(field).map(|mutation| mutation.validate()),\n\
                 Self::Request => MutationRequest::deserialize(field).map(|mutation| mutation.validate()),\n",
            ),
            (
                core.join("model.rs"),
                "#[cfg(test)]\nmod tests {\n    fn t() { mutation.validate().unwrap(); }\n}\n",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(read(tmp.path(), &["one-envelope-check"]).unwrap(), (1, 0));
        // A handler, a service or a store that checks its own envelope is refused.
        for file in [
            core.join("api/methods.rs"),
            core.join("capabilities/settings.rs"),
        ] {
            fs::write(&file, "    p.mutation.validate()?;\n").unwrap();
            let error = refusal(
                tmp.path(),
                &["one-envelope-check"],
                &file.display().to_string(),
            );
            assert!(error.contains("a handler never checks its own"), "{error}");
            fs::remove_file(&file).unwrap();
        }
        assert_eq!(read(tmp.path(), &["one-envelope-check"]).unwrap(), (1, 0));
    }

    #[test]
    fn the_desktop_words_a_start_refusal_once() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("crates/luxforge-app/src");
        let app = src.join("app");
        let state = src.join("state");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&state).unwrap();
        // The view model's editability rule words it, once; tests may assert the words.
        for (file, text) in [
            (
                state.join("mod.rs"),
                "const NOT_CURRENT: &str = \"Return to the current state before editing\";\n\
                 const IN_FLIGHT: &str = \"Waiting for the last request\";\n\
                 #[cfg(test)]\n\
                 mod tests {\n\
                     fn t() { assert_eq!(r, \"Waiting for the last request\"); }\n\
                 }\n",
            ),
            (
                app.join("slider_tests.rs"),
                "assert_eq!(status, \"Waiting for the last request\");\n",
            ),
            (
                app.join("preset.rs"),
                "let busy = \"Waiting for the last preset request\";\n",
            ),
            (
                app.join("release.rs"),
                "Some(\"Return to the current state to apply\".into())\n",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(
            read(tmp.path(), &["desktop-start-refusal"]).unwrap(),
            (3, 0)
        );
        // A start site, the refusal itself or a model wording its own is a second copy.
        for (file, text) in [
            (
                app.join("pointer.rs"),
                "self.status = \"Return to the current state before picking\".into();\n",
            ),
            (
                app.join("gesture.rs"),
                "const IN_FLIGHT: &str = \"Waiting for the last request\";\n",
            ),
            (
                state.join("masks.rs"),
                "(!enabled).then(|| \"Waiting for the last request\".to_owned())\n",
            ),
        ] {
            fs::write(&file, text).unwrap();
            let error = refusal(tmp.path(), &["desktop-start-refusal"], text);
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                error.contains(&format!("{name}:1")) && error.contains("worded once"),
                "{error}"
            );
            fs::remove_file(&file).unwrap();
        }
        // Its home words each case once.
        let home = state.join("mod.rs");
        let before = fs::read_to_string(&home).unwrap();
        let twice = format!("{before}let again = \"Waiting for the last request\";\n");
        fs::write(&home, &twice).unwrap();
        let error = refusal(tmp.path(), &["desktop-start-refusal"], &twice);
        assert!(error.contains("worded once"), "{error}");
        fs::write(&home, before).unwrap();
        assert_eq!(
            read(tmp.path(), &["desktop-start-refusal"]).unwrap(),
            (3, 0)
        );
    }

    #[test]
    fn only_the_evidence_driver_steers_a_script_step() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("crates/luxforge-app/src/app");
        fs::create_dir_all(&app).unwrap();
        // The driver settles, arms and refuses; a seam reports an outcome; a test arms a wait; a
        // preview's own settle intent and a capability step's `settle` action are other words.
        for (file, text) in [
            (
                app.join("evidence.rs"),
                "fn observe(&mut self) { self.settle_step(Settle::Preview, by); }\n\
                 fn arm(&mut self) { self.await_step(Settle::Draft); self.capture_next_frame(); }\n",
            ),
            (
                app.join("preview.rs"),
                "self.outcome(Outcome::Presented(presented));\n\
                 job.intent = PreviewIntent::Settle;\n",
            ),
            (
                app.join("capabilities.rs"),
                "CapabilityAction::Settle => return Ok(None),\n",
            ),
            (
                app.join("overlay_tests.rs"),
                "editor.await_step(evidence::Settle::Overlay);\n",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(read(tmp.path(), &["evidence-outcomes"]).unwrap(), (2, 0));
        // A seam naming a step's wait, or settling, arming or refusing it, is refused.
        for text in [
            "self.settle_step(Settle::Pick);\n",
            "self.await_step(Settle::Preview);\n",
            "use super::evidence::Settle; let s = Settle::Draft;\n",
            "self.refuse_step(&reason);\n",
            "self.capture_next_frame();\n",
        ] {
            let file = app.join("pointer.rs");
            fs::write(&file, text).unwrap();
            let error = refusal(tmp.path(), &["evidence-outcomes"], text);
            assert!(
                error.contains("pointer.rs:1") && error.contains("Editor::outcome"),
                "{error}"
            );
            fs::remove_file(&file).unwrap();
        }
        assert_eq!(read(tmp.path(), &["evidence-outcomes"]).unwrap(), (2, 0));
    }

    #[test]
    fn the_desktop_reads_the_crop_from_recipe_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("crates/luxforge-app/src/app");
        fs::create_dir_all(&app).unwrap();
        // Test files and test items may parse what a commit stored; product code reads rows.
        for (file, text) in [
            (
                app.join("crop.rs"),
                "fn f(row: &Row) -> f64 { row.values[\"angle\"].as_f64() }\n\
                 #[cfg(test)]\nmod tests {\n    fn t() { serde_json::from_value::<CropPayload>(v); }\n}\n",
            ),
            (
                app.join("crop_tests.rs"),
                "let e = luxforge_core::CROP_EFFECT;\n",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(read(tmp.path(), &["desktop-crop-rows"]).unwrap(), (1, 0));
        for text in [
            "let p = serde_json::from_value::<CropPayload>(layer.payload.clone());\n",
            "let o = serde_json::from_value::<Orientation>(payload);\n",
            "if layer.effect_id == ORIENTATION_EFFECT {}\n",
            "use luxforge_core::{CROP_EFFECT, CropStage};\n",
        ] {
            let file = app.join("gesture.rs");
            fs::write(&file, text).unwrap();
            let error = refusal(tmp.path(), &["desktop-crop-rows"], text);
            assert!(
                error.contains("gesture.rs:1") && error.contains("recipe.describe rows"),
                "{error}"
            );
            fs::remove_file(&file).unwrap();
        }
        assert_eq!(read(tmp.path(), &["desktop-crop-rows"]).unwrap(), (1, 0));
    }

    #[test]
    fn the_desktop_reads_the_crop_angle_from_its_declared_parameter() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("crates/luxforge-app/src");
        let state = src.join("state");
        fs::create_dir_all(&state).unwrap();
        // The frame's geometry clamps to the core's range; a test module and a test item may name
        // it to check that clamp; a comment names nothing.
        for (file, text) in [
            (
                src.join("crop_draft.rs"),
                "self.stage.angle = degrees.clamp(MIN_ANGLE, MAX_ANGLE);
"
                .to_owned(),
            ),
            (
                src.join("app_tests.rs"),
                "assert_eq!(angle, MAX_ANGLE);
"
                .to_owned(),
            ),
            (
                state.join("tools.rs"),
                "// Not MIN_ANGLE: the declared range.
#[cfg(test)]
mod tests {
                     fn t() { assert_eq!(spec.min, MIN_ANGLE); }
}
"
                .to_owned(),
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(read(tmp.path(), &["declared-crop-angle"]).unwrap(), (1, 0));
        // An angle control that takes its rail from the core's constants is a second path.
        for text in [
            "let rail = AngleRailModel { min: MIN_ANGLE, max: MAX_ANGLE };
",
            "use luxforge_core::{CropStage, MAX_ANGLE};
",
        ] {
            let file = state.join("crop_angle.rs");
            fs::write(&file, text).unwrap();
            let error = refusal(tmp.path(), &["declared-crop-angle"], text);
            assert!(
                error.contains("crop_angle.rs:1") && error.contains("declared parameter"),
                "{error}"
            );
            fs::remove_file(&file).unwrap();
        }
        assert_eq!(read(tmp.path(), &["declared-crop-angle"]).unwrap(), (1, 0));
    }

    #[test]
    fn the_desktop_resolves_a_group_reset_once() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("crates/luxforge-app/src");
        let state = src.join("state");
        let app = src.join("app");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&app).unwrap();
        // The tools panel model resolves it; a test file may resolve one to check it.
        for (file, text) in [
            (
                state.join("tools.rs"),
                "let reset = luxforge_core::resolve_group_reset(owner, group, kind, target);\n",
            ),
            (
                app.join("controls_tests.rs"),
                "luxforge_core::resolve_group_reset(&basic.id, control, kind, None)\n",
            ),
            (
                app.join("controls.rs"),
                "let reset = section.group_reset(&path).cloned();\n",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(read(tmp.path(), &["desktop-group-reset"]).unwrap(), (1, 0));
        // A second resolution in the app is refused.
        let file = app.join("controls.rs");
        let before = fs::read_to_string(&file).unwrap();
        let text = "luxforge_core::resolve_group_reset(owner, at_path(controls, path)?, kind, t)\n";
        fs::write(&file, text).unwrap();
        let error = refusal(tmp.path(), &["desktop-group-reset"], text);
        assert!(
            error.contains("controls.rs:1") && error.contains("group's reset"),
            "{error}"
        );
        fs::write(&file, before).unwrap();
        assert_eq!(read(tmp.path(), &["desktop-group-reset"]).unwrap(), (1, 0));
    }

    #[test]
    fn only_the_limits_module_declares_the_shared_thresholds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // The module itself, a pass-through elsewhere in either crate, a test file, and an
        // unrelated crate whose own constant happens to share one of the values may.
        write_all(
            root,
            &[
                (
                    "crates/luxforge-raw/src/limits.rs",
                    "pub const PARALLEL_PIXELS: u64 = 1_000_000;\n\
                     pub const MAX_FRAME_BYTES: u64 = 512 * 1024 * 1024;\n",
                ),
                (
                    "crates/luxforge-core/src/render.rs",
                    "const PARALLEL_RENDER_PIXELS: u64 = luxforge_raw::PARALLEL_PIXELS;\n",
                ),
                (
                    "crates/luxforge-raw/src/dng.rs",
                    "const PARALLEL_CORRECTION_PIXELS: u64 = crate::limits::PARALLEL_PIXELS;\n",
                ),
                (
                    "crates/luxforge-core/tests/parallel.rs",
                    "const PARALLEL_RENDER_PIXELS: u64 = 1_000_000;\n",
                ),
                (
                    "crates/luxforge-ui/src/photo_surface.rs",
                    "const FULL_BUDGET: u64 = 512 * 1024 * 1024;\n",
                ),
            ],
        );
        assert_eq!(read(root, &["render-limits-home"]).unwrap(), (2, 0));
        // A second literal declaration in either covered crate, outside the module, is refused.
        refuses_each(
            root,
            "render-limits-home",
            &[
                (
                    "crates/luxforge-core/src/proxy.rs",
                    "const FRAME_LIMIT_BYTES: u64 = 512 * 1024 * 1024;\n",
                ),
                (
                    "crates/luxforge-raw/src/dng.rs",
                    "const PARALLEL_CORRECTION_PIXELS: u64 = 1_000_000;\n",
                ),
            ],
            "may be declared",
        );
    }

    #[test]
    fn the_drafted_preview_mode_is_named_only_by_the_evaluation_builder() {
        let tmp = tempfile::tempdir().unwrap();
        let editor = tmp.path().join("crates/luxforge-core/src/editor");
        fs::create_dir_all(&editor).unwrap();
        const RULE: &[&str] = &["draft-preview-rule"];
        // The builder, the resolver, test files and test items may name it, and a comment or a
        // longer identifier is not a name.
        for (file, text) in [
            (
                editor.join("evaluate.rs"),
                "let rule = RawSettingsMode::DraftPreview;\n",
            ),
            (
                editor.join("source.rs"),
                "RawSettingsMode::DraftPreview => {}\n",
            ),
            (
                editor.join("plan_tests.rs"),
                "RawSettingsMode::DraftPreview\n",
            ),
            (
                editor.join("plan.rs"),
                "#[cfg(test)]\nfn t() {\n    let m = RawSettingsMode::DraftPreview;\n}\n\
                 /// Never `RawSettingsMode::DraftPreview` here.\n\
                 let m = RawSettingsMode::DraftPreviews;\n",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        let clean = read(tmp.path(), RULE).unwrap();
        // Anywhere else in the core's product code it is refused, after a test item too.
        for (file, text) in [
            (
                editor.join("plan.rs"),
                "#[cfg(test)]\nfn t() {\n}\nlet m = RawSettingsMode::DraftPreview;\n",
            ),
            (
                editor.join("describe.rs"),
                "mode: RawSettingsMode::DraftPreview,\n",
            ),
        ] {
            let before = fs::read_to_string(&file).ok();
            fs::write(&file, text).unwrap();
            let error = refusal(tmp.path(), RULE, &file.display().to_string());
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                error.contains(&format!("{name}:"))
                    && error.contains("source rule `draft-preview-rule`"),
                "{error}"
            );
            match before {
                Some(before) => fs::write(&file, before).unwrap(),
                None => fs::remove_file(&file).unwrap(),
            }
        }
        assert_eq!(read(tmp.path(), RULE).unwrap(), clean);
    }

    #[test]
    fn only_the_shared_srgb_reference_and_production_colour_write_the_transfer_function() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // The one production copy, the one shared test reference, and a threshold literal with no
        // slope beside it (not a transcription) may.
        write_all(
            root,
            &[
                (
                    "crates/luxforge-core/src/colour.rs",
                    "if encoded <= 0.040_45 {\n    encoded / 12.92\n}\n",
                ),
                (
                    "crates/luxforge-reference/src/srgb.rs",
                    "if linear <= 0.003_130_8 {\n    12.92 * linear\n}\n",
                ),
                (
                    "crates/luxforge-core/tests/render/linear.rs",
                    "0.003_130_8_f64.next_up()\n",
                ),
            ],
        );
        // The two allowed files are skipped outright; only the threshold-only file is read.
        assert_eq!(read(root, &["srgb-transfer-function"]).unwrap(), (1, 0));
        // Everywhere else, test code included, and in the underscore-free spelling too.
        refuses_each(
            root,
            "srgb-transfer-function",
            &[
                (
                    "crates/luxforge-core/src/render.rs",
                    "if encoded <= 0.04045 {\n    encoded / 12.92\n}\n",
                ),
                (
                    "crates/luxforge-core/tests/mask/overlay.rs",
                    "fn linear_grey(e: f64) -> f64 {\n    e / 12.92\n}\n",
                ),
                (
                    "crates/luxforge-reference/tests/studies/tone.rs",
                    "12.92 * clamped\n",
                ),
            ],
            "one shared test reference",
        );
    }

    #[test]
    fn tests_wait_and_hold_work_only_through_the_shared_wait_and_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let rules = &["test-waits", "test-gates"];
        let surface = "crates/luxforge-ui/src/photo_surface.rs";
        let worker = "fn worker() {\n    std::thread::sleep(STEP);\n}\n";
        // The shared crate's one wait and its gate, the production home's one sleep, the core's
        // production blocking points, and tests that wait through the shared crate may.
        write_all(
            root,
            &[
                (
                    "crates/luxforge-testbase/src/wait.rs",
                    "        thread::sleep(POLL);\n",
                ),
                (
                    "crates/luxforge-testbase/src/gate.rs",
                    "    changed: Condvar,\n            changed: Condvar::new(),\n",
                ),
                (surface, worker),
                (
                    "crates/luxforge-core/src/latest.rs",
                    "    changed: Condvar,\n",
                ),
                (
                    "crates/luxforge-core/src/preview/tests.rs",
                    "    wait_until(\"the frame\", || queue.poll().is_some());\n",
                ),
            ],
        );
        assert_eq!(read(root, rules).unwrap(), (5, 0));
        // A test's own sleep, spin or gate anywhere else, test code and comments included, and a
        // sleep in the proof module, which has no delay of its own to wait out.
        refuses_each(
            root,
            "test-waits",
            &[
                (
                    "crates/luxforge-core/src/modules/capabilities_proof.rs",
                    "            thread::sleep(rest);\n",
                ),
                (
                    "crates/luxforge-core/src/api/owner/export_tests.rs",
                    "            std::thread::sleep(Duration::from_millis(1));\n",
                ),
                (
                    "crates/luxforge-app/src/app/masks_tests.rs",
                    "        std::thread::yield_now();\n",
                ),
                (
                    "crates/luxforge-process/tests/counters.rs",
                    "    // Spin, then thread::sleep(ms) until the counter moves.\n",
                ),
            ],
            "luxforge_testbase::wait_until",
        );
        refuses_each(
            root,
            "test-gates",
            &[
                (
                    "crates/luxforge-app/src/app/preview_failure_tests.rs",
                    "struct Gate {\n    opened: Condvar,\n}\n",
                ),
                (
                    "crates/luxforge-testkit/src/proof.rs",
                    "    wake: Condvar,\n",
                ),
            ],
            "luxforge_testbase::Gate",
        );
        // The production home holds its one sleep only: a second, in the tests beside it, is a
        // test's own wait.
        write_all(
            root,
            &[(
                surface,
                &format!("{worker}#[cfg(test)]\nmod tests {{\n    std::thread::sleep(STEP);\n}}\n"),
            )],
        );
        let error = refusal(root, &["test-waits"], "a second sleep in a home");
        assert!(
            error.contains(&format!("{surface}:6:")) && error.contains("source rule `test-waits`"),
            "{error}"
        );
        write_all(root, &[(surface, worker)]);
        assert_eq!(read(root, rules).unwrap(), (5, 0));
    }

    #[test]
    fn every_percentile_is_read_from_the_one_distribution() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let rule = &["one-distribution"];
        // The one definition, and a crate's timing test and a timing tool that read through it.
        write_all(
            root,
            &[
                (
                    "crates/luxforge-testbase/src/distribution.rs",
                    "    let rank = (percent * sorted.len()).div_ceil(100).clamp(1, sorted.len());\n    pub fn percentile(&self, percent: usize) -> f64 {\n",
                ),
                (
                    "crates/luxforge-core/src/render/spatial.rs",
                    "            let ms = Distribution::of(samples).expect(\"runs ran\");\n            println!(\"p50 {:.0} ms\", ms.p50);\n",
                ),
                (
                    "xtask/src/editor_latency.rs",
                    "    let input_p95 = stats::Distribution::of(latencies.clone()).map(|d| d.p95);\n",
                ),
            ],
        );
        assert_eq!(read(root, rule).unwrap(), (2, 0));
        // Each shape a second definition took, in test code and tools alike, comments included.
        refuses_each(
            root,
            "one-distribution",
            &[
                (
                    "crates/luxforge-core/src/capabilities/proof_tests.rs",
                    "fn percentiles(samples: &mut [f64]) -> (f64, f64) {\n",
                ),
                (
                    "crates/luxforge-core/src/render/linear.rs",
                    "    fn percentile(values: &[f64], percentile: f64) -> f64 {\n",
                ),
                (
                    "crates/luxforge-core/src/source.rs",
                    "        let median = |mut values: Vec<f64>| {\n",
                ),
                (
                    "crates/luxforge-raw/src/lib.rs",
                    "        fn p50_p95(values: impl Iterator<Item = u128>) -> (u128, u128) {\n",
                ),
                (
                    "crates/luxforge-core/src/modules/presence/oracle.rs",
                    "            let p50 = samples[samples.len() / 2];\n",
                ),
                (
                    "crates/luxforge-core/tests/resources_cost.rs",
                    "    let p95 = samples[(samples.len() as f64 * 0.95).ceil() as usize - 1];\n",
                ),
                (
                    "xtask/src/mask_range_smoke.rs",
                    "        let rank = (latencies.len() * percent).div_ceil(100).max(1) - 1;\n",
                ),
                (
                    "xtask/src/stats.rs",
                    "    let percentile = |percent: usize| -> f64 {\n",
                ),
                (
                    "crates/luxforge-process/tests/cost.rs",
                    "// fn median of the samples, by hand\n",
                ),
            ],
            "luxforge_testbase::Distribution",
        );
    }

    #[test]
    fn the_shared_test_base_may_depend_on_no_workspace_crate() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("crates/luxforge-testbase/Cargo.toml");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let clean = "[package]\nname = \"luxforge-testbase\"\n\n[dependencies]\n";
        fs::write(&manifest, clean).unwrap();
        let rule = &["core-free-test-base"];
        assert_eq!(read(tmp.path(), rule).unwrap(), (0, 1));
        for (what, extra) in [
            (
                "the core",
                "[dependencies]\nluxforge-core = { path = \"../luxforge-core\" }\n",
            ),
            (
                "the test kit, which depends on the core",
                "[dev-dependencies]\nluxforge-testkit = { path = \"../luxforge-testkit\" }\n",
            ),
        ] {
            fs::write(&manifest, format!("{clean}\n{extra}")).unwrap();
            let error = refusal(tmp.path(), rule, what);
            assert!(
                error.contains("luxforge-testbase/Cargo.toml:")
                    && error.contains("no workspace crate")
                    && error.contains("DEPENDENCY_RULES"),
                "{what}: {error}"
            );
        }
    }

    #[test]
    fn the_watcher_builds_against_no_workspace_crate_but_its_tests_may_use_the_test_base() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("crates/luxforge-watch/Cargo.toml");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let clean = "[package]\nname = \"luxforge-watch\"\n\n[dependencies]\n\n\
                     [target.'cfg(target_os = \"linux\")'.dependencies]\n\
                     rustix.workspace = true\n\n[dev-dependencies]\n\
                     luxforge-testbase = { path = \"../luxforge-testbase\" }\n";
        fs::write(&manifest, clean).unwrap();
        let rule = &["watch-leaf"];
        assert_eq!(read(tmp.path(), rule).unwrap(), (0, 1));
        for (what, extra) in [
            (
                "the process counters",
                "[dependencies]\nluxforge-process = { path = \"../luxforge-process\" }\n",
            ),
            (
                "the core, for one platform",
                "[target.'cfg(windows)'.dependencies]\nluxforge-core = { path = \"../luxforge-core\" }\n",
            ),
            (
                "a path in its build",
                "[build-dependencies]\nhelper = { path = \"../helper\" }\n",
            ),
        ] {
            fs::write(&manifest, format!("{clean}\n{extra}")).unwrap();
            let error = refusal(tmp.path(), rule, what);
            assert!(
                error.contains("luxforge-watch/Cargo.toml:")
                    && error.contains("no workspace crate"),
                "{what}: {error}"
            );
        }
    }

    #[test]
    fn the_core_names_no_crate_that_depends_on_it() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("crates/luxforge-core/Cargo.toml");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let clean = "[package]\nname = \"luxforge-core\"\n\n[dependencies]\n\
                     luxforge-raw = { path = \"../luxforge-raw\" }\n\n[dev-dependencies]\n\
                     luxforge-testbase = { path = \"../luxforge-testbase\" }\n";
        fs::write(&manifest, clean).unwrap();
        let rule = &["core-builds-once"];
        assert_eq!(read(tmp.path(), rule).unwrap(), (0, 1));
        for (what, extra) in [
            (
                "the typed test kit",
                "luxforge-testkit = { path = \"../luxforge-testkit\" }\n",
            ),
            (
                "the transport",
                "luxforge-net = { path = \"../luxforge-net\" }\n",
            ),
            (
                "the desktop",
                "luxforge-app = { path = \"../luxforge-app\" }\n",
            ),
        ] {
            fs::write(&manifest, format!("{clean}{extra}")).unwrap();
            let error = refusal(tmp.path(), rule, what);
            assert!(
                error.contains("luxforge-core/Cargo.toml:")
                    && error.contains("build it once")
                    && error.contains("DEPENDENCY_RULES"),
                "{what}: {error}"
            );
        }
    }

    #[test]
    fn the_headless_cli_may_not_depend_on_the_gui_stack() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("crates/luxforge-cli/Cargo.toml");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let clean = "[package]\nname = \"luxforge-cli\"\n\n[dependencies]\n\
                     luxforge-core = { path = \"../luxforge-core\" }\n\
                     serde_json.workspace = true\n";
        fs::write(&manifest, clean).unwrap();
        let rule = &["headless-cli"];
        assert_eq!(read(tmp.path(), rule).unwrap(), (0, 1));
        // A test may drive a GUI crate; the binary never links one.
        fs::write(
            &manifest,
            format!("{clean}\n[dev-dependencies]\niced.workspace = true\n"),
        )
        .unwrap();
        assert_eq!(read(tmp.path(), rule).unwrap(), (0, 1));
        for (what, extra) in [
            ("Iced", "iced.workspace = true\n"),
            ("Iced's renderer", "iced_wgpu.workspace = true\n"),
            ("wgpu", "wgpu = { version = \"27\" }\n"),
            ("the dialog crate", "rfd.workspace = true\n"),
            (
                "the widget crate",
                "luxforge-ui = { path = \"../luxforge-ui\" }\n",
            ),
            (
                "the desktop",
                "luxforge-app = { path = \"../luxforge-app\" }\n",
            ),
            (
                "Iced under another name",
                "gui = { package = \"iced\", version = \"0.14\" }\n",
            ),
        ] {
            fs::write(&manifest, format!("{clean}{extra}")).unwrap();
            let error = refusal(tmp.path(), rule, what);
            assert!(
                error.contains("luxforge-cli/Cargo.toml:")
                    && error.contains("GUI stack")
                    && error.contains("DEPENDENCY_RULES"),
                "{what}: {error}"
            );
        }
    }

    #[test]
    fn only_the_source_work_reads_an_original_or_an_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("crates/luxforge-core/src");
        fs::create_dir_all(core.join("editor")).unwrap();
        fs::create_dir_all(core.join("artifacts")).unwrap();
        fs::create_dir_all(core.join("api")).unwrap();
        // The source work reads both; the reads are defined, re-exported and tested at home, where
        // the rule does not look, and the owner's worker runs the source work.
        for (file, text) in [
            (
                core.join("editor/source.rs"),
                "let bytes = read_bounded_file(&mut file)?;
                 let prepared = EditorService::prepare_file(&path, target, cancel)?;
                 .map(|read| artifacts::read_verified(read, cancel))
",
            ),
            (
                core.join("source.rs"),
                "pub(crate) fn read_bounded_file(file: &mut File) -> Result<Vec<u8>, Error> {
",
            ),
            (
                core.join("artifacts/store.rs"),
                "pub(crate) fn read_verified(read: &ArtifactRead) {}
                 #[cfg(test)]
mod tests {
    fn t() { read_verified(&read, &never).unwrap(); }
}
",
            ),
            (
                core.join("lib.rs"),
                "pub(crate) use source::{open_source_bytes, read_bounded_file};
",
            ),
            (
                core.join("api/owner.rs"),
                "let mut prepared = work.run(reads, cancel)?;
",
            ),
        ] {
            fs::write(file, text).unwrap();
        }
        assert_eq!(
            read(tmp.path(), &["one-source-preparation"]).unwrap(),
            (1, 0)
        );
        // A synchronous service mode, a read on the worker beside the source work, and a test that
        // reads and adopts by hand are all refused.
        for (file, text) in [
            (
                core.join("editor.rs"),
                "    allow_sync_source: bool,
",
            ),
            (
                core.join("api/owner.rs"),
                "EditorService::prepare_file(&key.path, target, cancel)
",
            ),
            (
                core.join("editor/artifact_store.rs"),
                "let verified = artifacts::read_verified(read, &never)?;
",
            ),
            (
                core.join("editor/artifact_tests.rs"),
                "let verified = crate::artifacts::read_verified(&reads[0], &never).unwrap();
",
            ),
        ] {
            let clean = fs::read_to_string(&file).ok();
            fs::write(&file, text).unwrap();
            let error = refusal(
                tmp.path(),
                &["one-source-preparation"],
                &file.display().to_string(),
            );
            assert!(error.contains("only the source work"), "{error}");
            match clean {
                Some(clean) => fs::write(&file, clean).unwrap(),
                None => fs::remove_file(&file).unwrap(),
            }
        }
        assert_eq!(
            read(tmp.path(), &["one-source-preparation"]).unwrap(),
            (1, 0)
        );
    }

    fn minimal_plan(id: &str) -> Value {
        json!({
            "schema_version":"1.0", "plan_id":id, "title":"Local plan",
            "objective":"Test independent task identities", "context_summary":"A standalone plan",
            "tasks":[{
                "id":"TASK-001", "description":"First task", "status":"ready",
                "dependencies":[], "extra_context":[],
                "context_links":[{"kind":"other","label":"Fixture","target":"fixture","relevance":"Test context"}],
                "acceptance_criteria":["One local task"],
                "test_strategy":{"approach":"inspection","steps":["Inspect"],"commands":[],"expected_results":["Valid"]}
            }],
            "execution_waves":[{"wave":1,"task_ids":["TASK-001"]}]
        })
    }
    fn task_schema() -> Value {
        serde_json::from_str(include_str!("../../tools/task-plan.schema.json")).unwrap()
    }
    #[test]
    fn separate_plans_reuse_local_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let first = minimal_plan("one");
        let second = minimal_plan("two");
        assert!(active_plans(tmp.path(), &[first.clone(), second], &task_schema()).is_ok());
        assert!(active_plans(tmp.path(), &[first.clone(), first], &task_schema()).is_err());
    }
    #[test]
    fn numbering_dependency_order_and_plan_references_are_checked() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("tasks")).unwrap();
        fs::write(tmp.path().join("tasks/other.json"), "{}").unwrap();
        let s = task_schema();
        let base = minimal_plan("test");
        let mut skipped = base.clone();
        skipped["tasks"][0]["id"] = json!("TASK-002");
        assert!(plan(tmp.path(), &skipped, &s).is_err());
        let mut external = base.clone();
        external["tasks"][0]["dependencies"] = json!(["TASK-064"]);
        assert!(plan(tmp.path(), &external, &s).is_err());
        let mut linked = base.clone();
        linked["tasks"][0]["context_links"][0] = json!({
            "kind":"file", "label":"Other plan", "target":"tasks/other.json", "relevance":"Invalid cross-file link"
        });
        assert!(plan(tmp.path(), &linked, &s).is_err());
        let mut ordered = base.clone();
        let mut second = ordered["tasks"][0].clone();
        second["id"] = json!("TASK-002");
        second["status"] = json!("pending");
        second["dependencies"] = json!(["TASK-001"]);
        ordered["tasks"].as_array_mut().unwrap().push(second);
        ordered["execution_waves"] = json!([
            {"wave":1,"task_ids":["TASK-001"]},{"wave":2,"task_ids":["TASK-002"]}
        ]);
        assert!(plan(tmp.path(), &ordered, &s).is_ok());
        ordered["tasks"][0]["status"] = json!("pending");
        ordered["tasks"][0]["dependencies"] = json!(["TASK-002"]);
        ordered["tasks"][1]["dependencies"] = json!([]);
        assert!(plan(tmp.path(), &ordered, &s).is_err());
    }
}
