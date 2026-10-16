//! `cargo xtask lr-align fold --measured DIR [--measured DIR ...] --output NEW`: slider responses
//! refitted from measured rounds (`docs/design/lightroom-alignment.md`, "Changing a control").
//!
//! Each `DIR` is an `lr-align measure` output. Every family fitted on a realignable slider there
//! carries its distance table: each Lightroom value's distance at every Luxforge value of a refined
//! grid. The fold fits each slider's families together, the targets and the photographs, each
//! family weighted equally ([`la::joint_fit`]), with knots at the measured values, a saturated
//! point continuing its neighbour's slope within its ties ([`la::continued_within_ties`]) and
//! every slider step moving the picture ([`la::with_slope_floor`]), and composes the map with the
//! responses the measured build applied, so the new response gives each slider value the strength
//! whose response was closest to Lightroom's at that value. It writes `response-<action>.json` per
//! module, the tables each module's `response.json` freezes, with `fold.json` and `fold.md`, every
//! figure, into `NEW`, and reads nothing else.
use crate::*;
use luxforge_core::{BasicResponse, slider_response};
use luxforge_reference::lr_align::{self as la, DistanceTable};
use std::collections::BTreeMap;

/// One slider a response can realign: the action that patches it, its field, its range and its
/// neutral value, which Lightroom's default shares.
#[derive(Clone, Copy)]
struct Slider {
    action: &'static str,
    field: &'static str,
    range: (f64, f64),
    neutral: f64,
}

const fn slider(action: &'static str, field: &'static str) -> Slider {
    Slider {
        action,
        field,
        range: (-100.0, 100.0),
        neutral: 0.0,
    }
}

/// The colour mixer's eight ranges, whose hue, saturation and luminance are each a slider.
const RANGES: [&str; 8] = [
    "red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta",
];

/// Every slider a response realigns, in each module's payload order. Basic's Exposure, Temperature
/// and Tint act on the scene scale, measured directly (`basic-scene.md`); Texture's map is
/// undetermined and the vignette's Roundness and Feather differ in kind, so they have none.
fn sliders() -> Vec<Slider> {
    let mut all: Vec<Slider> = [
        "contrast",
        "highlights",
        "shadows",
        "whites",
        "blacks",
        "vibrance",
        "saturation",
    ]
    .map(|field| slider("set-basic", field))
    .to_vec();
    all.extend(["clarity", "dehaze"].map(|field| slider("set-presence", field)));
    for property in ["hue", "saturation", "luminance"] {
        for range in RANGES {
            let field: &'static str = Box::leak(format!("{range}-{property}").into_boxed_str());
            all.push(slider("set-mixer", field));
        }
    }
    all.push(slider("set-vignette", "amount"));
    all.push(Slider {
        action: "set-vignette",
        field: "midpoint",
        range: (0.0, 100.0),
        neutral: 50.0,
    });
    all
}

/// The least step of a folded map, as a share of its Lightroom step, in the measured build's slider
/// units: a fifth of the identity's.
const SLOPE_FLOOR: f64 = 0.2;

/// The responses this build applies, `"<action> <field>"` to knots, as `lr-align measure` records
/// them beside its figures.
pub fn build_responses() -> Value {
    let mut responses = serde_json::Map::new();
    for slider in sliders() {
        if let Some(response) = slider_response(slider.action, slider.field) {
            responses.insert(
                format!("{} {}", slider.action, slider.field),
                json!(response.knots),
            );
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
    let responses = reports[0]["slider_responses"].clone();
    ensure(
        responses.is_object(),
        "the measured run records no slider responses: measure it with this tool",
    )?;
    ensure(
        reports.iter().all(|r| r["slider_responses"] == responses),
        "the measured runs applied different slider responses",
    )?;
    let mut folded: BTreeMap<&str, serde_json::Map<String, Value>> = BTreeMap::new();
    let mut figures = Vec::new();
    let mut page = String::from(
        "# Slider responses folded from measured rounds\n\nThe joint fit of every measured family of each slider, each family weighted equally, through Luxforge's whole-frame CPU reference renderer; no GPU output was measured. `Luxforge` is the measured build's slider value, `strength` what the folded response applies; distances are median CIEDE2000 against Lightroom's carried response, now and at the former strength (the Lightroom value applied as the strength).\n\nMeasured runs:\n\n",
    );
    for (dir, report) in measured.iter().zip(&reports) {
        page += &format!(
            "- `{}`: round {}, `{}`\n",
            dir.display(),
            report["round"],
            report["round_folder"].as_str().unwrap_or("")
        );
    }
    for slider in sliders() {
        let Slider {
            action,
            field,
            range,
            neutral,
        } = slider;
        let key = format!("{action} {field}");
        let applied = BasicResponse {
            knots: knots_of(&responses[&key]).unwrap_or_else(|| {
                vec![[range.0, range.0], [neutral, neutral], [range.1, range.1]]
            }),
        };
        let mut families: Vec<Measured> = Vec::new();
        for report in &reports {
            for entry in report["families"].as_array().into_iter().flatten() {
                if entry["luxforge"] != key {
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
            // A slider no measured run fitted keeps the response the measured build applied, if
            // it has one; otherwise it stays unlisted.
            if slider_response(action, field).is_some() {
                folded
                    .entry(action)
                    .or_default()
                    .insert(field.into(), json!({"knots": applied.knots}));
            }
            page +=
                &format!("\n## {key}\n\nNot measured: the measured build's response is kept.\n");
            figures.push(json!({"action": action, "field": field, "measured": false}));
            continue;
        }
        let tables: Vec<DistanceTable> = families.iter().map(|m| m.table.clone()).collect();
        let fit = la::joint_fit(&tables, Some((neutral, neutral)), range);
        // Knots at the measured values themselves, so the response passes through each fitted
        // point rather than a resampling of them.
        let knots = la::continued_within_ties(&fit.points, (neutral, neutral));
        ensure(
            knots.first().map(|k| k[0]) == Some(range.0)
                && knots.last().map(|k| k[0]) == Some(range.1),
            format!("{key}: the measured values do not reach both ends of its range"),
        )?;
        let floored = la::with_slope_floor(&knots, neutral, SLOPE_FLOOR, range);
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
                && strengths.contains(&[neutral, neutral]),
            format!("{key}: the folded response is not strictly increasing through neutral"),
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
            "\n## {key}\n\nFamilies: {}.\n\n| Lightroom | Luxforge | strength | joint residual | {} | shortfall |\n| ---: | ---: | ---: | ---: | {} | --- |\n",
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
            "action": action, "field": field, "measured": true, "families": names, "direction": format!("{:?}", fit.direction),
            "map": floored, "response": strengths, "points": points,
        }));
        folded
            .entry(action)
            .or_default()
            .insert(field.into(), json!({"knots": strengths}));
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
    fs::create_dir(out)?;
    let basis = format!(
        "lr-align fold of {}: each field's measured families fitted jointly, each weighted equally, every step at least {SLOPE_FLOOR} of its Lightroom step, composed with the measured build's responses",
        rounds.join(", ")
    );
    for (action, responses) in &folded {
        let response = json!({
            "format": "luxforge.slider-response",
            "version": 1,
            "basis": basis,
            "responses": responses,
        });
        write_json(&out.join(format!("response-{action}.json")), &response)?;
    }
    write_json(
        &out.join("fold.json"),
        &json!({"tool": "cargo xtask lr-align fold", "measured": measured, "applied": responses, "fields": figures}),
    )?;
    fs::write(out.join("fold.md"), page)?;
    println!("wrote {}", out.display());
    Ok(())
}
