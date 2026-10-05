//! Source-preserving API qualification. Unmarked photographs remain unqualified.
use crate::*;
use luxforge_core::{ApiRequest, ClientId, MappingDescriptor, OwnerHandle};
use serde::Deserialize;
use std::time::{Duration, Instant};
const MAX_EDGES_BYTES: u64 = 1024 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edges {
    format: u32,
    sources: Vec<Marked>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Marked {
    id: String,
    edges: Vec<Vec<[f64; 2]>>,
}
fn edges(path: &Path) -> Result<Edges> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_EDGES_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure(
        bytes.len() as u64 <= MAX_EDGES_BYTES,
        "Edge annotations exceed 1 MiB",
    )?;
    let e: Edges = serde_json::from_slice(&bytes)?;
    ensure(e.format == 1, "Unsupported edge annotation format")?;
    let mut ids = std::collections::HashSet::new();
    for source in &e.sources {
        ensure(ids.insert(&source.id), "Duplicate annotated source")?;
        ensure(
            (3..=64).contains(&source.edges.len()),
            "Each annotated photo needs 3..64 edges",
        )?;
        for edge in &source.edges {
            ensure(
                (5..=256).contains(&edge.len())
                    && edge.iter().flatten().all(|v| v.is_finite() && *v >= 0.0),
                "Each edge needs 5..256 finite content pixel coordinates",
            )?;
            ensure(
                edge.iter().any(|p| *p != edge[0]),
                "An edge must contain distinct points",
            )?;
        }
    }
    Ok(e)
}
struct Api {
    owner: OwnerHandle,
    client: ClientId,
    serial: usize,
}
impl Drop for Api {
    fn drop(&mut self) {
        self.owner.stop();
    }
}
impl Api {
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.serial += 1;
        let response = self.owner.call(
            self.client,
            ApiRequest {
                id: format!("qualification-{}", self.serial),
                method: method.into(),
                params,
                token: None,
            },
        )?;
        if let Some(e) = response.error {
            return Err(format!("{method}: {}: {}", e.code, e.message).into());
        }
        response
            .result
            .ok_or_else(|| format!("{method} answered without a result").into())
    }
    fn envelope(&mut self, revision: Option<u64>) -> Value {
        self.serial += 1;
        let mut m = json!({"request_id":format!("qualification-edit-{}",self.serial),"actor":"lens-qualification"});
        if let Some(r) = revision {
            m["expected_revision"] = json!(r);
        }
        m
    }
    fn ready(&mut self, job: &Value) -> Result<Value> {
        let start = Instant::now();
        loop {
            let result = self.call("job.read", json!({"job_id":job}))?;
            match result["status"].as_str() {
                Some("ready") => return Ok(result),
                Some("failed" | "cancelled") => {
                    return Err(format!("Qualification job failed: {result}").into());
                }
                _ => {
                    ensure(
                        start.elapsed() < Duration::from_secs(180),
                        "Qualification job timed out",
                    )?;
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}
/// Total-least-squares line fit: maximum orthogonal residual, independent of axis or slope.
fn deviation(points: &[[f64; 2]]) -> f64 {
    let n = points.len() as f64;
    let centre = [
        points.iter().map(|p| p[0]).sum::<f64>() / n,
        points.iter().map(|p| p[1]).sum::<f64>() / n,
    ];
    let (mut xx, mut xy, mut yy) = (0.0, 0.0, 0.0);
    for p in points {
        let (x, y) = (p[0] - centre[0], p[1] - centre[1]);
        xx += x * x;
        xy += x * y;
        yy += y * y;
    }
    let angle = 0.5 * (2.0 * xy).atan2(xx - yy);
    let normal = [-angle.sin(), angle.cos()];
    points
        .iter()
        .map(|p| ((p[0] - centre[0]) * normal[0] + (p[1] - centre[1]) * normal[1]).abs())
        .fold(0.0, f64::max)
}
fn measure(
    marked: &Marked,
    before: &MappingDescriptor,
    after: &MappingDescriptor,
) -> Result<Value> {
    let mut rows = Vec::new();
    let norm = 6048.0 / before.output.width.max(before.output.height) as f64;
    for points in &marked.edges {
        ensure(
            points.iter().all(|p| {
                p[0] <= before.content.width as f64 && p[1] <= before.content.height as f64
            }),
            "Annotated point is outside the content stage",
        )?;
        let map = |m: &MappingDescriptor| -> Result<Vec<[f64; 2]>> {
            points
                .iter()
                .map(|p| {
                    m.to_output(p[0], p[1]).map(|(x, y)| [x, y]).map_err(|e| {
                        format!("Annotated edge outside the visible mapped domain: {e:?}").into()
                    })
                })
                .collect()
        };
        let a = deviation(&map(before)?) * norm;
        let b = deviation(&map(after)?) * norm;
        rows.push(json!({"before_px_at_6048":a,"after_px_at_6048":b,"qualified":b<=a*0.25&&b<=3.0,"points":points.len()}));
    }
    let passed = rows.iter().all(|r| r["qualified"] == true);
    Ok(json!({"status":if passed{"qualified"}else{"failed"},"edges":rows}))
}
fn source(
    source: &raw::EditorSource,
    path: &Path,
    marked: Option<&Marked>,
    out: &Path,
) -> Result<Value> {
    ensure(
        hash(path)? == source.sha256,
        format!("Source hash differs for {}", source.id),
    )?;
    let (owner, _join) = OwnerHandle::start(&out.join(format!("{}.sqlite", source.id)))?;
    let client = owner.register();
    let mut api = Api {
        owner,
        client,
        serial: 0,
    };
    // Opened as a client opens a file: developed at once (`pick.develop`), its photograph prepared
    // (`source.prepare`) and adopted (`job.adopt`).
    let mutation = api.envelope(None);
    let developed = api.call(
        "pick.develop",
        json!({"targets":{"kind":"paths","paths":[path]},"into":[],"confirm_removable":true,"mutation":mutation}),
    )?;
    let developed = api.ready(&developed["job_id"])?;
    ensure(
        developed["result"]["failed"]
            .as_array()
            .is_none_or(Vec::is_empty),
        format!(
            "{} was not developed: {}",
            source.id, developed["result"]["failed"]
        ),
    )?;
    let photograph = developed["result"]["developed"][0]["asset_id"].clone();
    let queued = api.call("source.prepare", json!({"asset_id":photograph}))?;
    let job: luxforge_core::JobId = serde_json::from_value(queued["job_id"].clone())?;
    api.owner.wait_source(client, Some(&job))?;
    api.ready(&queued["job_id"])?;
    let adopted = api.call("job.adopt", json!({"job_id":job}))?;
    let state = &adopted["asset"];
    let asset = state["asset"]["id"].clone();
    ensure(!asset.is_null(), "Open answer has no asset")?;
    // A supported RAW is opened with its detected profile applied as its first-open entry, so
    // the uncorrected geometry is its Original's, the entry that one undoes to.
    let entry = &state["current_entry"];
    let uncorrected = if entry["action_id"] == "select-lens-profile" && entry["actor"] == "system" {
        entry["undo_parent"].clone()
    } else {
        entry["id"].clone()
    };
    let before: MappingDescriptor = serde_json::from_value(api.call(
        "render.transform",
        json!({"asset_id":asset,"entry_id":uncorrected}),
    )?)?;
    let query = api.call("query.lens-profiles", json!({"asset_id":asset}))?;
    let candidate = Some(&query["status"]["detected"]).filter(|r| r["eligible"] == true);
    let Some(candidate) = candidate else {
        ensure(hash(path)? == source.sha256, "Source changed")?;
        return Ok(
            json!({"id":source.id,"source_sha256":source.sha256,"source_preserved":true,"status":"unsupported","quality":{"status":"untested","reason":"no eligible offline camera/lens profile"},"query":query}),
        );
    };
    let mutation = api.envelope(state["revision"].as_u64());
    let selected = api.call(
        "edit.select-lens-profile",
        json!({"asset_id":asset,"profile":candidate["key"],"mutation":mutation}),
    )?;
    let after: MappingDescriptor =
        serde_json::from_value(api.call("render.transform", json!({"asset_id":asset}))?)?;
    let mutation = api.envelope(None);
    let export=api.call("export.jpeg",json!({"asset_id":asset,"destination":out.join(format!("{}.jpg",source.id)),"mutation":mutation}))?;
    let exported = api.ready(&export["job_id"])?;
    ensure(
        hash(path)? == source.sha256,
        "Qualification modified original",
    )?;
    let quality = match marked {
        Some(marked) => measure(marked, &before, &after)?,
        None => {
            json!({"status":"untested","reason":"no marked straight edges; selection and export only"})
        }
    };
    Ok(
        json!({"id":source.id,"source_sha256":source.sha256,"status":quality["status"],"quality":quality,"query":query,"selection":selected,"mapping":after,"export":exported,"source_preserved":true}),
    )
}
pub fn run(manifest: &Path, annotations: &Path, out: &Path) -> Result {
    let manifest = raw::manifest::<raw::EditorSource>(manifest)?;
    let annotations = edges(annotations)?;
    ensure(
        !out.exists(),
        "Qualification output must be a new directory",
    )?;
    ensure(
        annotations
            .sources
            .iter()
            .all(|s| manifest.sources.iter().any(|m| m.id == s.id)),
        "Edge annotations name an unknown manifest source",
    )?;
    fs::create_dir_all(out)?;
    let mut rows = Vec::new();
    for (entry, path) in manifest.located() {
        println!(
            "Qualifying {} (selection/export; edges only when supplied)",
            entry.id
        );
        let marked = annotations.sources.iter().find(|e| e.id == entry.id);
        let result = source(entry, &path, marked, out);
        match result {
            Ok(row) => rows.push(row),
            Err(e) => rows.push(json!({"id":entry.id,"status":"failed","error":e.to_string()})),
        };
        write_json(
            &out.join("result.json"),
            &json!({"format":1,"status":if rows.iter().any(|r|r["status"]=="failed"){"failed"}else if rows.iter().any(|r|r["status"]=="untested"||r["status"]=="unsupported"){"incomplete"}else{"qualified"},"criterion":{"relative":0.25,"maximum_px_at_6048":3.0,"minimum_edges":3,"minimum_points":5},"sources":rows}),
        )?;
    }
    ensure(
        !rows.iter().any(|r| r["status"] == "failed"),
        "One or more qualification sources failed; see result.json",
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_fit_is_rotation_invariant_and_measures_orthogonal_residual() {
        let p = [[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0], [4.0, 0.0]];
        assert_eq!(deviation(&p), 0.0);
        let q = p.map(|[x, y]| [x * 0.6 - y * 0.8, x * 0.8 + y * 0.6]);
        assert!(deviation(&q) < 1e-15);
        let curved = [[0.0, 1.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0], [4.0, 1.0]];
        assert!((deviation(&curved) - 0.6).abs() < 1e-12);
    }
}
