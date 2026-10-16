//! `cargo xtask lr-align fold --measured DIR [--measured DIR ...] --output NEW`: Basic's slider
//! responses refitted from measured rounds (`docs/design/lightroom-alignment.md`, "Changing a
//! control").
//!
//! Each `DIR` is an `lr-align measure` output. Every family fitted on a Basic field there carries
//! its distance table: each Lightroom value's distance at every Luxforge value of a refined grid.
//! The fold fits each field's families together, the targets and the photographs, each family
//! weighted equally, with knots at the measured values chosen together so every slider step moves
//! the picture and a saturated point continues its neighbour's slope within its ties
//! ([`la::floored_joint_knots`]), and composes the map with the responses the measured build applied,
//! so the new response gives each slider value the strength whose response was closest to
//! Lightroom's at that value. It writes `response.json`, the table Basic's `response.rs` freezes,
//! with `fold.json` and `fold.md`, every figure, into `NEW`, and reads nothing else.
use crate::*;
use luxforge_core::{BasicResponse, basic_response};
use luxforge_reference::lr_align::{self as la, DistanceTable};

/// The Basic fields a response realigns and their ranges, in the payload's order. Exposure,
/// Temperature and Tint act on the scene scale, measured directly (`basic-scene.md`), and have none.
const FIELDS: [(&str, (f64, f64)); 7] = [
    ("contrast", (-100.0, 100.0)),
    ("highlights", (-100.0, 100.0)),
    ("shadows", (-100.0, 100.0)),
    ("whites", (-100.0, 100.0)),
    ("blacks", (-100.0, 100.0)),
    ("vibrance", (-100.0, 100.0)),
    ("saturation", (-100.0, 100.0)),
];

/// The least step of a folded map, as a share of its Lightroom step, in the measured build's slider
/// units: a fifth of the identity's.
const SLOPE_FLOOR: f64 = 0.2;

/// The responses this build applies, as `lr-align measure` records them beside its figures.
pub fn build_responses() -> Value {
    let mut responses = serde_json::Map::new();
    for (field, _) in FIELDS {
        if let Some(response) = basic_response(field) {
            responses.insert(field.into(), json!(response.knots));
        }
    }
    Value::Object(responses)
}

/// One family's distances, and where they came from.
struct Measured {
    family: String,
    round: String,
    table: DistanceTable,
}

fn table(entry: &Value) -> Option<DistanceTable> {
    let distances = &entry["distances"];
    let values: Vec<f64> = distances["values"]
        .as_array()?
        .iter()
        .map(Value::as_f64)
        .collect::<Option<_>>()?;
    let rows = distances["rows"]
        .as_array()?
        .iter()
        .map(|row| {
            let distances: Vec<f64> = row["distances"]
                .as_array()?
                .iter()
                .map(|d| d.as_f64().or(Some(f64::INFINITY)))
                .collect::<Option<_>>()?;
            (distances.len() == values.len()).then_some((row["lightroom"].as_f64()?, distances))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(DistanceTable { values, rows })
}

fn knots_of(value: &Value) -> Option<Vec<[f64; 2]>> {
    value
        .as_array()?
        .iter()
        .map(|knot| Some([knot[0].as_f64()?, knot[1].as_f64()?]))
        .collect()
}

pub fn run(root: &Path, mut a: Args) -> Result {
    let mut measured = Vec::new();
    while let Some(dir) = a.value("--measured")? {
        measured.push(absolute(root, Path::new(&dir)));
    }
    let out = absolute(root, &a.path("--output")?);
    a.done()?;
    ensure(!measured.is_empty(), super::USAGE)?;
    fold(&measured, &out)
}

fn fold(measured: &[PathBuf], out: &Path) -> Result {
    ensure(!out.exists(), format!("{} already exists", out.display()))?;
    for dir in measured {
        ensure(
            !out.starts_with(dir),
            format!("the output may not be inside {}", dir.display()),
        )?;
    }
    let reports: Vec<Value> = measured
        .iter()
        .map(|dir| read_json(&dir.join("lr-align.json")))
        .collect::<Result<_>>()?;
    let responses = reports[0]["basic_responses"].clone();
    ensure(
        responses.is_object(),
        "the measured run records no Basic responses: measure it with this tool",
    )?;
    ensure(
        reports.iter().all(|r| r["basic_responses"] == responses),
        "the measured runs applied different Basic responses",
    )?;
    let mut folded = serde_json::Map::new();
    let mut figures = Vec::new();
    let mut page = String::from(
        "# Basic responses folded from measured rounds\n\nThe joint fit of every measured family of each Basic field, each family weighted equally, through Luxforge's whole-frame CPU reference renderer; no GPU output was measured. `Luxforge` is the measured build's slider value, `strength` what the folded response applies; distances are median CIEDE2000 against Lightroom's carried response, now and at the former strength (the Lightroom value applied as the strength).\n\nMeasured runs:\n\n",
    );
    for (dir, report) in measured.iter().zip(&reports) {
        page += &format!(
            "- `{}`: round {}, `{}`\n",
            dir.display(),
            report["round"],
            report["round_folder"].as_str().unwrap_or("")
        );
    }
    for (field, range) in FIELDS {
        let applied = BasicResponse {
            knots: knots_of(&responses[field])
                .unwrap_or_else(|| vec![[range.0, range.0], [0.0, 0.0], [range.1, range.1]]),
        };
        let mut families: Vec<Measured> = Vec::new();
        for report in &reports {
            for entry in report["families"].as_array().into_iter().flatten() {
                if entry["luxforge"] != format!("set-basic {field}") {
                    continue;
                }
                if let Some(table) = table(entry) {
                    families.push(Measured {
                        family: entry["family"].as_str().unwrap_or("").into(),
                        round: report["round"].to_string(),
                        table,
                    });
                }
            }
        }
        if families.is_empty() {
            folded.insert(field.into(), json!({"knots": applied.knots}));
            page +=
                &format!("\n## {field}\n\nNot measured: the measured build's response is kept.\n");
            figures.push(json!({"field": field, "measured": false}));
            continue;
        }
        let tables: Vec<DistanceTable> = families.iter().map(|m| m.table.clone()).collect();
        let fit = la::joint_fit(&tables, Some((0.0, 0.0)), range);
        // Knots at the measured values themselves, so the response passes through each fitted
        // point rather than a resampling of them.
        let floored = la::floored_joint_knots(&tables, (0.0, 0.0), SLOPE_FLOOR, range).ok_or(
            format!("{field}: no knots on the measured grid meet the slope floor"),
        )?;
        ensure(
            floored.first().map(|k| k[0]) == Some(range.0)
                && floored.last().map(|k| k[0]) == Some(range.1),
            format!("{field}: the measured values do not reach both ends of its range"),
        )?;
        let strengths: Vec<[f64; 2]> = floored
            .iter()
            .map(|[lr, u]| {
                let strength = applied.strength(*u);
                [*lr, (strength * 1e4).round() / 1e4]
            })
            .collect();
        ensure(
            strengths
                .windows(2)
                .all(|p| p[0][0] < p[1][0] && p[0][1] < p[1][1])
                && strengths.contains(&[0.0, 0.0]),
            format!("{field}: the folded response is not strictly increasing through neutral"),
        )?;
        let on_floored = |lr: f64| -> f64 {
            BasicResponse {
                knots: floored.clone(),
            }
            .strength(lr)
        };
        let names: Vec<String> = families
            .iter()
            .map(|m| format!("{} (round {})", m.family, m.round))
            .collect();
        page += &format!(
            "\n## {field}\n\nFamilies: {}.\n\n| Lightroom | Luxforge | strength | joint residual | {} | shortfall |\n| ---: | ---: | ---: | ---: | {} | --- |\n",
            names.join(", "),
            names
                .iter()
                .map(|n| format!("{n} now (former)"))
                .collect::<Vec<_>>()
                .join(" | "),
            names.iter().map(|_| "---:").collect::<Vec<_>>().join(" | "),
        );
        let mut points = Vec::new();
        for point in &fit.points {
            let u = on_floored(point.lightroom);
            let former = applied.value(point.lightroom);
            let per: Vec<Value> = families
                .iter()
                .map(|m| {
                    json!({
                        "family": m.family,
                        "now": m.table.at(point.lightroom, u),
                        "former": former.and_then(|f| m.table.at(point.lightroom, f)),
                    })
                })
                .collect();
            let held: Vec<f64> = per.iter().filter_map(|p| p["now"].as_f64()).collect();
            let joint = held.iter().sum::<f64>() / held.len().max(1) as f64;
            page += &format!(
                "| {} | {:.3} | {:.3} | {:.2} | {} | {} |\n",
                point.lightroom,
                u,
                applied.strength(u),
                joint,
                per.iter()
                    .map(|p| format!(
                        "{} ({})",
                        p["now"].as_f64().map_or("–".into(), |d| format!("{d:.2}")),
                        p["former"]
                            .as_f64()
                            .map_or("–".into(), |d| format!("{d:.2}"))
                    ))
                    .collect::<Vec<_>>()
                    .join(" | "),
                if point.shortfall { "yes" } else { "" },
            );
            points.push(json!({
                "lightroom": point.lightroom, "luxforge": u, "strength": applied.strength(u),
                "joint_residual": joint, "fitted": point.luxforge, "fitted_residual": point.residual,
                "shortfall": point.shortfall, "span": point.span, "families": per,
            }));
        }
        page += &format!(
            "\nResponse knots `[value, strength]`: {}\n",
            serde_json::to_string(&strengths)?
        );
        figures.push(json!({
            "field": field, "measured": true, "families": names, "direction": format!("{:?}", fit.direction),
            "map": floored, "response": strengths, "points": points,
        }));
        folded.insert(field.into(), json!({"knots": strengths}));
    }
    let rounds: Vec<String> = reports
        .iter()
        .map(|r| {
            let folder = r["round_folder"].as_str().unwrap_or("");
            let name = Path::new(folder)
                .file_name()
                .map_or(folder.into(), |n| n.to_string_lossy().into_owned());
            format!("round {} ({name})", r["round"])
        })
        .collect();
    let response = json!({
        "format": "luxforge.basic-response",
        "version": 1,
        "basis": format!(
            "lr-align fold of {}: each field's measured families fitted jointly, each weighted equally, every step at least {SLOPE_FLOOR} of its Lightroom step, composed with the measured build's responses",
            rounds.join(", ")
        ),
        "responses": folded,
    });
    fs::create_dir(out)?;
    write_json(&out.join("response.json"), &response)?;
    write_json(
        &out.join("fold.json"),
        &json!({"tool": "cargo xtask lr-align fold", "measured": measured, "applied": responses, "fields": figures}),
    )?;
    fs::write(out.join("fold.md"), page)?;
    println!("wrote {}", out.display());
    Ok(())
}
