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
/// framework, the view cannot reach authoritative state or the owner, and the widget crate cannot
/// reach the core at all. Each entry is a directory, the tokens it may not contain and why.
const BOUNDARIES: [(&str, &[&str]); 3] = [
    (
        "crates/lightwell-app/src/state",
        &["use iced", "iced::", "iced_runtime"],
    ),
    (
        "crates/lightwell-app/src/view",
        &["lightwell_core", "OwnerHandle", ".call("],
    ),
    ("crates/lightwell-ui", &["lightwell_core"]),
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
                        !line.contains(token),
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
        let state = tmp.path().join("crates/lightwell-app/src/state");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("clean.rs"), "use crate::app::fields::Fields;\n").unwrap();
        assert_eq!(boundaries(tmp.path()).unwrap(), 1);
        fs::write(state.join("bad.rs"), "use iced::widget::text;\n").unwrap();
        let error = boundaries(tmp.path()).unwrap_err().to_string();
        assert!(
            error.contains("bad.rs:1") && error.contains("use iced"),
            "{error}"
        );
        fs::remove_file(state.join("bad.rs")).unwrap();
        let view = tmp.path().join("crates/lightwell-app/src/view");
        fs::create_dir_all(&view).unwrap();
        fs::write(
            view.join("bad.rs"),
            "\nlet state: lightwell_core::EditorState;\n",
        )
        .unwrap();
        assert!(
            boundaries(tmp.path())
                .unwrap_err()
                .to_string()
                .contains("lightwell_core")
        );
        fs::write(view.join("bad.rs"), "owner.call(client, request)\n").unwrap();
        assert!(
            boundaries(tmp.path())
                .unwrap_err()
                .to_string()
                .contains(".call(")
        );
        fs::remove_file(view.join("bad.rs")).unwrap();
        let ui = tmp.path().join("crates/lightwell-ui");
        fs::create_dir_all(&ui).unwrap();
        fs::write(
            ui.join("Cargo.toml"),
            "[dependencies]\nlightwell_core = { path = \"../lightwell-core\" }\n",
        )
        .unwrap();
        assert!(
            boundaries(tmp.path())
                .unwrap_err()
                .to_string()
                .contains("lightwell-ui")
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
