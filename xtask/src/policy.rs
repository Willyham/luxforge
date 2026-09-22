use crate::*;
use regex::Regex;
use time::{Date, OffsetDateTime, format_description::well_known::Iso8601};
fn exceptions() -> Value {
    json!([
 {"id":"RUSTSEC-2024-0436","package":"paste","version":"1.0.15","reviewed":"2026-09-19","expires":"2026-12-18","task":"TASK-002","reason":"Build-time macro in pinned Metal dependency; no supported Iced upgrade removes it. Local S0 development only."},
 {"id":"RUSTSEC-2026-0192","package":"ttf-parser","version":"0.25.1","reviewed":"2026-09-19","expires":"2026-10-19","task":"TASK-001","reason":"Pinned Iced system/bundled-font stack; no application font import. Undisclosed upstream report requires short review window; no distribution approval."}])
}
fn validate(
    ex: &Value,
    packages: &Value,
    tasks: &Value,
    base: &str,
    today: Date,
) -> Result<String> {
    ensure(
        !Regex::new(r"(?m)^\s*(?:\[advisories\]|ignore\s*=)")?.is_match(base),
        "Static advisory overrides forbidden",
    )?;
    let mut ids = std::collections::BTreeSet::new();
    let mut config = format!("{base}\n[advisories]\nignore = [\n");
    let advisory_id = Regex::new(r"^RUSTSEC-\d{4}-\d{4}$")?;
    for e in ex.as_array().ok_or("Exceptions array")? {
        let get = |k: &str| -> Result<&str> {
            e[k].as_str().ok_or_else(|| format!("Missing {k}").into())
        };
        let id = get("id")?;
        ensure(
            advisory_id.is_match(id) && ids.insert(id),
            "Invalid or duplicate advisory",
        )?;
        let reviewed = Date::parse(get("reviewed")?, &Iso8601::DEFAULT)?;
        let expires = Date::parse(get("expires")?, &Iso8601::DEFAULT)?;
        ensure(
            reviewed <= today
                && today < expires
                && (1..=90).contains(&(expires - reviewed).whole_days()),
            format!(
                "{id}: expired/invalid review window; resolve {}",
                get("task")?
            ),
        )?;
        let matching: Vec<_> = packages
            .as_array()
            .ok_or("Packages array")?
            .iter()
            .filter(|p| p["name"] == e["package"])
            .collect();
        ensure(
            !matching.is_empty()
                && matching.iter().all(|p| {
                    p["version"] == e["version"]
                        && p["source"]
                            .as_str()
                            .is_some_and(|s| s.starts_with("registry+"))
                }),
            format!("{id}: dependency changed or removed"),
        )?;
        let task = tasks
            .as_array()
            .ok_or("Tasks array")?
            .iter()
            .find(|t| t["id"] == e["task"]);
        ensure(
            task.is_some_and(|t| t["status"] != "completed" && t["status"] != "cancelled"),
            "Follow-up task missing or retired",
        )?;
        ensure(!get("reason")?.trim().is_empty(), "Missing rationale")?;
        let reason = format!(
            "{} {}; expires {}; {}. {}",
            get("package")?,
            get("version")?,
            get("expires")?,
            get("task")?,
            get("reason")?
        );
        config.push_str(&format!(
            "  {{ id = {}, reason = {} }},\n",
            json!(id),
            json!(reason)
        ));
    }
    config.push_str("]\n");
    Ok(config)
}
pub fn checked(root: &Path) -> Result<(String, String)> {
    let metadata = output(
        root,
        "cargo",
        &["metadata", "--locked", "--format-version", "1"],
    )?;
    let data: Value = serde_json::from_str(&metadata)?;
    let tasks = read_json(&root.join("tasks/dependency-advisories.json"))?;
    let config = validate(
        &exceptions(),
        &data["packages"],
        &tasks["tasks"],
        &fs::read_to_string(root.join("deny.toml"))?,
        OffsetDateTime::now_utc().date(),
    )?;
    println!("PASS exact advisory versions, follow-up tasks and UTC expiry");
    Ok((metadata, config))
}
pub fn audit(root: &Path) -> Result {
    let checker = root.join(format!(
        ".tools/cargo-deny/bin/cargo-deny{}",
        std::env::consts::EXE_SUFFIX
    ));
    ensure(
        output(root, &checker, &["--version"])?.trim() == "cargo-deny 0.20.2",
        "Install pinned cargo-deny 0.20.2",
    )?;
    let (metadata, config) = checked(root)?;
    let tmp = tempfile::tempdir()?;
    fs::write(tmp.path().join("metadata.json"), metadata)?;
    fs::write(tmp.path().join("deny.toml"), config)?;
    ensure(
        Command::new(checker)
            .current_dir(root)
            .arg("--manifest-path")
            .arg(root.join("Cargo.toml"))
            .arg("--metadata-path")
            .arg(tmp.path().join("metadata.json"))
            .arg("--config")
            .arg(tmp.path().join("deny.toml"))
            .args(["check", "licenses", "sources", "advisories"])
            .status()?
            .success(),
        "Dependency policy failed",
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn inputs() -> (Value, Value, Value) {
        let e = exceptions();
        let p=e.as_array().unwrap().iter().map(|e|json!({"name":e["package"],"version":e["version"],"source":"registry+https://example.invalid"})).collect::<Vec<_>>();
        let t = e
            .as_array()
            .unwrap()
            .iter()
            .map(|e| json!({"id":e["task"],"status":"ready"}))
            .collect::<Vec<_>>();
        (e, json!(p), json!(t))
    }
    fn day(s: &str) -> Date {
        Date::parse(s, &Iso8601::DEFAULT).unwrap()
    }
    #[test]
    fn valid_and_exclusive_expiry() {
        let (e, p, t) = inputs();
        let c = validate(&e, &p, &t, "[licenses]\n", day("2026-09-19")).unwrap();
        assert_eq!(c.matches("{ id =").count(), 2);
        for date in ["2026-09-18", "2026-10-19"] {
            assert!(validate(&e, &p, &t, "", day(date)).is_err())
        }
    }
    #[test]
    fn mutations_fail_closed() {
        let (e, p, t) = inputs();
        for key in ["version", "source"] {
            let mut p = p.clone();
            p[0][key] = json!("changed");
            assert!(validate(&e, &p, &t, "", day("2026-09-19")).is_err())
        }
        assert!(validate(&e, &json!([]), &t, "", day("2026-09-19")).is_err());
        for state in ["completed", "cancelled"] {
            let mut t = t.clone();
            t[0]["status"] = json!(state);
            assert!(validate(&e, &p, &t, "", day("2026-09-19")).is_err())
        }
        assert!(validate(&e, &p, &json!([]), "", day("2026-09-19")).is_err());
        assert!(validate(&e, &p, &t, "[advisories]\nignore=[]", day("2026-09-19")).is_err());
        let mut duplicate = e.clone();
        duplicate[1]["id"] = duplicate[0]["id"].clone();
        assert!(validate(&duplicate, &p, &t, "", day("2026-09-19")).is_err());
        let mut long = e.clone();
        long[0]["expires"] = json!("2027-01-01");
        assert!(validate(&long, &p, &t, "", day("2026-09-19")).is_err());
    }
}
