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
/// The desktop's layering rule, enforced here rather than by review: the view model cannot reach a
/// framework or the update layer above it (`app/`, which depends on it), the view cannot reach
/// authoritative state or the owner, and the widget crate cannot reach the core at all. Each entry
/// is a directory and the tokens it may not contain. A token that starts with an identifier
/// character matches only at the start of a path segment, so `app::` catches `crate::app::`,
/// `super::app::` and a grouped `use crate::{ app::... }` line alike, and nothing that merely ends
/// in `app`.
const BOUNDARIES: [(&str, &[&str]); 3] = [
    (
        "crates/luxforge-app/src/state",
        &["use iced", "iced::", "iced_runtime", "app::"],
    ),
    (
        "crates/luxforge-app/src/view",
        &["luxforge_core", "OwnerHandle", ".call("],
    ),
    ("crates/luxforge-ui", &["luxforge_core"]),
];

/// Fail on the first forbidden token, naming the file, the line and the token.
fn boundaries(root: &Path) -> Result<usize> {
    let mut checked = 0;
    for (directory, forbidden) in BOUNDARIES {
        let dir = root.join(directory);
        if !dir.is_dir() {
            continue;
        }
        for path in files(&dir)? {
            let extension = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            if !matches!(extension, "rs" | "toml") {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            for (number, line) in text.lines().enumerate() {
                for token in forbidden {
                    ensure(
                        !contains_token(line, token),
                        format!(
                            "{}:{}: {directory} may not contain {token}",
                            path.display(),
                            number + 1
                        ),
                    )?;
                }
            }
            checked += 1;
        }
    }
    Ok(checked)
}

/// Whether `line` holds `token`. A token that starts with an identifier character must also start
/// one, so `app::` is found in `crate::app::x` but not in `snapp::x`.
fn contains_token(line: &str, token: &str) -> bool {
    let identifier = |c: char| c.is_alphanumeric() || c == '_';
    if !token.starts_with(identifier) {
        return line.contains(token);
    }
    line.match_indices(token)
        .any(|(at, _)| !line[..at].ends_with(identifier))
}

/// The references are independent by construction: `luxforge-reference` depends on no workspace
/// crate, so nothing it builds against can reach the core it checks, directly or through a crate
/// that depends on it. Every dependency table (normal, dev, build or target-specific) is read.
const REFERENCE_MANIFEST: &str = "crates/luxforge-reference/Cargo.toml";

/// Fail on the first dependency of the reference crate that names a `luxforge` crate or a path,
/// naming the line; answer how many dependency lines were read.
fn independent_references(root: &Path) -> Result<usize> {
    let path = root.join(REFERENCE_MANIFEST);
    let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut table = String::new();
    let mut checked = 0;
    for (number, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            table = line.trim_matches(['[', ']']).to_owned();
        }
        if !table.contains("dependencies") {
            continue;
        }
        let names_a_crate = line.contains("luxforge");
        let is_a_path = line.split(['{', ',', '}']).any(|part| {
            part.split('=')
                .next()
                .is_some_and(|key| key.trim() == "path")
        });
        ensure(
            !names_a_crate && !is_a_path,
            format!(
                "{}:{}: luxforge-reference may depend on no workspace crate, so it can never \
                 reach luxforge-core: {line}",
                path.display(),
                number + 1
            ),
        )?;
        checked += 1;
    }
    Ok(checked)
}

/// The crates a shipped binary links: their non-test code and normal dependencies are held to the
/// one JPEG codec path below.
const SHIPPED_CRATES: [&str; 7] = [
    "crates/luxforge-core",
    "crates/luxforge-app",
    "crates/luxforge-ui",
    "crates/luxforge-raw",
    "crates/luxforge-process",
    "crates/luxforge-evidence",
    "crates/luxforge-jpeg",
];

/// The one crate that may name `mozjpeg` (and `mozjpeg_sys`), in its sources and its manifest.
const JPEG_CODEC: &str = "crates/luxforge-jpeg";

/// The one crate that may depend on the codec crate.
const JPEG_CODEC_USER: &str = "crates/luxforge-core";

/// Ways to decode JPEG through `image`, which shipped code never uses: JPEG is read only through
/// the codec crate. Tests may, as an independent decoder.
const IMAGE_JPEG: [&str; 6] = [
    "codecs::jpeg",
    "ImageFormat::Jpeg",
    "JpegDecoder",
    "image::open",
    "load_from_memory",
    "ImageReader",
];

/// The files under `src` that only a test build compiles: each module declared as
/// `#[cfg(test)] mod name;`, and everything below its directory.
fn test_only_files(src: &Path, sources: &[PathBuf]) -> Result<BTreeSet<PathBuf>> {
    let mut test_only = BTreeSet::new();
    let mut test_dirs = Vec::new();
    for path in sources {
        let text = fs::read_to_string(path)?;
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let parent = path.parent().unwrap_or(src);
        let dir = if ["mod", "lib", "main"].contains(&stem) {
            parent.to_path_buf()
        } else {
            parent.join(stem)
        };
        for pair in lines.windows(2) {
            if pair[0] != "#[cfg(test)]" {
                continue;
            }
            let declaration = pair[1]
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ");
            if let Some(name) = declaration
                .strip_prefix("mod ")
                .and_then(|rest| rest.strip_suffix(';'))
            {
                test_only.insert(dir.join(format!("{name}.rs")));
                test_dirs.push(dir.join(name));
            }
        }
    }
    for path in sources {
        if test_dirs.iter().any(|dir| path.starts_with(dir)) {
            test_only.insert(path.clone());
        }
    }
    Ok(test_only)
}

/// `text` without its inline `#[cfg(test)] mod name { ... }` blocks, each ending at the closing
/// brace indented as its `mod` line, as rustfmt writes it; each kept line with its number.
fn non_test_lines(text: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let opens_test_module = line.trim() == "#[cfg(test)]"
            && lines.get(index + 1).is_some_and(|next| {
                let next = next.trim();
                next.contains("mod ") && next.ends_with('{')
            });
        if opens_test_module {
            let module = lines[index + 1];
            let close = format!("{}}}", &module[..module.len() - module.trim_start().len()]);
            index += 2;
            while index < lines.len() && lines[index] != close {
                index += 1;
            }
            index += 1;
            continue;
        }
        kept.push((index + 1, line));
        index += 1;
    }
    kept
}

/// One JPEG codec path, enforced rather than reviewed: in the shipped crates' non-test code only
/// `luxforge-jpeg` names `mozjpeg`, nothing decodes JPEG through `image`, and no manifest gives a
/// shipped crate `image`'s `jpeg` feature or names `mozjpeg` outside `luxforge-jpeg`; and
/// [`jpeg_codec_dependencies`] holds. Answers how many source files were read.
fn one_jpeg_codec(root: &Path) -> Result<usize> {
    let mut checked = 0;
    for krate in SHIPPED_CRATES {
        let src = root.join(krate).join("src");
        if src.is_dir() {
            let sources: Vec<_> = files(&src)?
                .into_iter()
                .filter(|path| path.extension().is_some_and(|e| e == "rs"))
                .collect();
            let test_only = test_only_files(&src, &sources)?;
            for path in sources.iter().filter(|path| !test_only.contains(*path)) {
                let relative = path.strip_prefix(root).unwrap_or(path);
                let relative = relative.to_string_lossy().replace('\\', "/");
                let text = fs::read_to_string(path)?;
                for (number, line) in non_test_lines(&text) {
                    ensure(
                        krate == JPEG_CODEC || !contains_token(line, "mozjpeg"),
                        format!(
                            "{relative}:{number}: only the JPEG codec crate ({JPEG_CODEC}) may \
                             name mozjpeg"
                        ),
                    )?;
                    for token in IMAGE_JPEG {
                        ensure(
                            !contains_token(line, token),
                            format!(
                                "{relative}:{number}: shipped code decodes JPEG only through \
                                 {JPEG_CODEC}, not {token}"
                            ),
                        )?;
                    }
                }
                checked += 1;
            }
        }
        let manifest = root.join(krate).join("Cargo.toml");
        if manifest.is_file() {
            let text = fs::read_to_string(&manifest)?;
            let mut table = String::new();
            for (number, line) in text.lines().enumerate() {
                let line = line.split('#').next().unwrap_or_default().trim();
                if line.starts_with('[') {
                    table = line.trim_matches(['[', ']']).to_owned();
                    continue;
                }
                let normal = table == "dependencies" || table.ends_with(".dependencies");
                if !normal {
                    continue;
                }
                let name = line.split(['=', '.', ' ']).next().unwrap_or_default();
                ensure(
                    krate == JPEG_CODEC || !name.starts_with("mozjpeg"),
                    format!(
                        "{krate}/Cargo.toml:{}: only luxforge-jpeg links the JPEG codec",
                        number + 1
                    ),
                )?;
                ensure(
                    name != "image" || !line.contains("jpeg"),
                    format!(
                        "{krate}/Cargo.toml:{}: a shipped crate may not enable image's jpeg \
                         feature",
                        number + 1
                    ),
                )?;
            }
        }
    }
    let workspace = fs::read_to_string(root.join("Cargo.toml"))?;
    let mut table = String::new();
    for (number, line) in workspace.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') {
            table = line.trim_matches(['[', ']']).to_owned();
            continue;
        }
        ensure(
            table != "workspace.dependencies"
                || !(line.starts_with("image ") || line.starts_with("image="))
                || !line.contains("jpeg"),
            format!(
                "Cargo.toml:{}: the workspace image dependency may not enable jpeg; a test or tool \
                 adds it for itself",
                number + 1
            ),
        )?;
    }
    jpeg_codec_dependencies(root)?;
    Ok(checked)
}

/// The codec crate is a leaf behind the core: in every workspace manifest (`crates/*` and `xtask`)
/// and every dependency table, normal, dev, build or target-specific, only `luxforge-core` names
/// `luxforge-jpeg`, and `luxforge-jpeg` itself names no `luxforge` crate and no path. Answers how
/// many manifests were read.
fn jpeg_codec_dependencies(root: &Path) -> Result<usize> {
    let mut manifests = Vec::new();
    let crates = root.join("crates");
    if crates.is_dir() {
        for entry in fs::read_dir(&crates)? {
            let manifest = entry?.path().join("Cargo.toml");
            if manifest.is_file() {
                manifests.push(manifest);
            }
        }
    }
    let xtask = root.join("xtask/Cargo.toml");
    if xtask.is_file() {
        manifests.push(xtask);
    }
    manifests.sort();
    for manifest in &manifests {
        let krate = manifest
            .parent()
            .and_then(|dir| dir.strip_prefix(root).ok())
            .map(|dir| dir.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let text = fs::read_to_string(manifest)?;
        let mut dependencies = false;
        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.starts_with('[') {
                dependencies = line.contains("dependencies");
            }
            if !dependencies {
                continue;
            }
            let at = format!("{krate}/Cargo.toml:{}", number + 1);
            ensure(
                krate == JPEG_CODEC_USER || !line.contains("luxforge-jpeg"),
                format!("{at}: only luxforge-core may depend on luxforge-jpeg: {line}"),
            )?;
            ensure(
                krate != JPEG_CODEC || !(line.contains("luxforge") || line.contains("path")),
                format!("{at}: luxforge-jpeg may depend on no workspace crate: {line}"),
            )?;
        }
    }
    Ok(manifests.len())
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
    println!(
        "PASS desktop layer boundaries ({} files)",
        boundaries(root)?
    );
    println!(
        "PASS independent references ({} dependency lines, no workspace crate)",
        independent_references(root)?
    );
    println!(
        "PASS one JPEG codec ({} shipped source files, mozjpeg only in luxforge-jpeg, which only \
         luxforge-core depends on)",
        one_jpeg_codec(root)?
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
    fn active_repository_is_valid() {
        check(&root().unwrap()).unwrap();
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
        assert_eq!(boundaries(tmp.path()).unwrap(), 1);
        fs::write(state.join("bad.rs"), "use iced::widget::text;\n").unwrap();
        let error = boundaries(tmp.path()).unwrap_err().to_string();
        assert!(
            error.contains("bad.rs:1") && error.contains("use iced"),
            "{error}"
        );
        // The view model never imports the update layer, in any spelling of the path.
        for import in [
            "use crate::app::message::Message;\n",
            "\nlet step = super::app::crop::ANGLE_STEP;\n",
            "use crate::{\n    app::fields::Fields,\n};\n",
            "/// Seeded like [`crate::app::Editor`] seeds them.\n",
        ] {
            fs::write(state.join("bad.rs"), import).unwrap();
            let error = boundaries(tmp.path()).unwrap_err().to_string();
            assert!(
                error.contains("bad.rs:") && error.contains("may not contain app::"),
                "{import:?}: {error}"
            );
        }
        fs::remove_file(state.join("bad.rs")).unwrap();
        let view = tmp.path().join("crates/luxforge-app/src/view");
        fs::create_dir_all(&view).unwrap();
        fs::write(
            view.join("bad.rs"),
            "\nlet state: luxforge_core::EditorState;\n",
        )
        .unwrap();
        assert!(
            boundaries(tmp.path())
                .unwrap_err()
                .to_string()
                .contains("luxforge_core")
        );
        fs::write(view.join("bad.rs"), "owner.call(client, request)\n").unwrap();
        assert!(
            boundaries(tmp.path())
                .unwrap_err()
                .to_string()
                .contains(".call(")
        );
        fs::remove_file(view.join("bad.rs")).unwrap();
        let ui = tmp.path().join("crates/luxforge-ui");
        fs::create_dir_all(&ui).unwrap();
        fs::write(
            ui.join("Cargo.toml"),
            "[dependencies]\nluxforge_core = { path = \"../luxforge-core\" }\n",
        )
        .unwrap();
        assert!(
            boundaries(tmp.path())
                .unwrap_err()
                .to_string()
                .contains("luxforge-ui")
        );
    }
    #[test]
    fn the_reference_crate_may_depend_on_no_workspace_crate() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join(REFERENCE_MANIFEST);
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let clean = "[package]\nname = \"luxforge-reference\"\n\n[dependencies]\n\n\
                     [dev-dependencies]\nserde.workspace = true # not luxforge\n";
        fs::write(&manifest, clean).unwrap();
        assert_eq!(independent_references(tmp.path()).unwrap(), 3);
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
            let error = independent_references(tmp.path())
                .err()
                .unwrap_or_else(|| panic!("{what} was accepted"))
                .to_string();
            assert!(
                error.contains("Cargo.toml:") && error.contains("no workspace crate"),
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
        // not read.
        assert_eq!(one_jpeg_codec(root).unwrap(), 4);

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
                "not image::open",
            ),
            (
                "an image JPEG decode in the codec crate",
                "crates/luxforge-jpeg/src/container.rs",
                "let d = image::load_from_memory(b);\n",
                "not load_from_memory",
            ),
        ] {
            write(path, text);
            let error = one_jpeg_codec(root)
                .err()
                .unwrap_or_else(|| panic!("{what} was accepted"))
                .to_string();
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
            let error = one_jpeg_codec(root)
                .err()
                .unwrap_or_else(|| panic!("{what} was accepted"))
                .to_string();
            assert!(error.contains(expected), "{what}: {error}");
            write(path, &before);
        }
        assert_eq!(one_jpeg_codec(root).unwrap(), 4);
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
