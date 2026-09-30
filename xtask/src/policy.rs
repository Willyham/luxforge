use crate::*;
use regex::Regex;
use time::{Date, OffsetDateTime, format_description::well_known::Iso8601};
fn exceptions() -> Value {
    json!([
 {"id":"RUSTSEC-2024-0436","package":"paste","version":"1.0.15","reviewed":"2026-09-19","expires":"2026-12-18","task":"TASK-002","reason":"Build-time macro in pinned Metal dependency; no supported Iced upgrade removes it. Local S0 development only."},
 {"id":"RUSTSEC-2026-0192","package":"ttf-parser","version":"0.25.1","reviewed":"2026-09-29","expires":"2026-10-29","task":"TASK-001","reason":"Pinned Iced 0.14 system/bundled-font stack (fontdb 0.23; fontdb 0.24 drops ttf-parser but no cosmic-text or Iced release uses it); no application font import. Upstream DoS report still undisclosed and unfixed, so short review window; no distribution approval."}])
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
    /// The first and last days on which every recorded exception is valid:
    /// the latest review date and the day before the earliest expiry.
    fn window(e: &Value) -> (Date, Date) {
        let dates = |k: &str| {
            e.as_array()
                .unwrap()
                .iter()
                .map(|x| day(x[k].as_str().unwrap()))
                .collect::<Vec<_>>()
        };
        let first = dates("reviewed").into_iter().max().unwrap();
        let end = dates("expires").into_iter().min().unwrap();
        (first, end.previous_day().unwrap())
    }
    #[test]
    fn valid_and_exclusive_expiry() {
        let (e, p, t) = inputs();
        let (first, last) = window(&e);
        assert!(first <= last);
        for date in [first, last] {
            let c = validate(&e, &p, &t, "[licenses]\n", date).unwrap();
            assert_eq!(c.matches("{ id =").count(), e.as_array().unwrap().len());
        }
        for date in [first.previous_day().unwrap(), last.next_day().unwrap()] {
            assert!(validate(&e, &p, &t, "", date).is_err())
        }
    }
    #[test]
    fn mutations_fail_closed() {
        let (e, p, t) = inputs();
        let today = window(&e).0;
        assert!(validate(&e, &p, &t, "", today).is_ok());
        for key in ["version", "source"] {
            let mut p = p.clone();
            p[0][key] = json!("changed");
            assert!(validate(&e, &p, &t, "", today).is_err())
        }
        assert!(validate(&e, &json!([]), &t, "", today).is_err());
        for state in ["completed", "cancelled"] {
            let mut t = t.clone();
            t[0]["status"] = json!(state);
            assert!(validate(&e, &p, &t, "", today).is_err())
        }
        assert!(validate(&e, &p, &json!([]), "", today).is_err());
        assert!(validate(&e, &p, &t, "[advisories]\nignore=[]", today).is_err());
        let mut duplicate = e.clone();
        duplicate[1]["id"] = duplicate[0]["id"].clone();
        assert!(validate(&duplicate, &p, &t, "", today).is_err());
        let mut long = e.clone();
        let reviewed = day(long[0]["reviewed"].as_str().unwrap());
        long[0]["expires"] = json!((reviewed + time::Duration::days(91)).to_string());
        assert!(validate(&long, &p, &t, "", today).is_err());
    }
}
