//! Switching photographs in Develop with a cached large preview: **lane D's probe**. It takes the
//! desktop probes' context ([`DesktopContext`]) and returns its rows as they do
//! ([`super::desktop`]).
//!
//! It generates real JPEGs (`generate-catalog --images 120`), develops `samples + 1` of them into a
//! catalog of its own with `pick.develop`, and launches the editor in the background over it through
//! the scenario library ([`Run::tool`], [`Launch`]): `G`, All photographs, the first photograph
//! clicked and `D`, which opens Develop on it with the view's photographs as the development set;
//! then, `samples` times, the neighbours' large previews decoded ahead and `→`. Each `→` frame is
//! captured in the frame after the key and records how many frames the photo surface drew from the
//! key until the photograph's cached preview was drawn (`develop.timing.presented_after`): one
//! sample each. A `→` frame that drew no preview of the photograph moved to fails the probe rather
//! than counting.
use super::{desktop::DesktopContext, report::Row};
use crate::{
    generate_catalog,
    scenario::{Launch, Plan, Run, Step},
    smoke::PANELLED,
    *,
};
use luxforge_core::{ApiRequest, EditorService, OwnerHandle};
use luxforge_evidence::{self as script, DevelopStep, SelectStep, SetStep};
use std::time::Duration;

pub const METRIC: &str = "desktop.develop_switch.key_to_presented";
pub const TARGET: &str = "Switching photographs in Develop with a cached large preview: presented in the frame after the key";

/// The most samples one launch takes: four scripted steps reach Develop, and each sample is two
/// more, within the evidence script's 64 — the 30 a p95 needs.
pub const MAX_SAMPLES: usize = (script::MAX_SCRIPT_STEPS - 4) / 2;
const SEED: u64 = 1;
const IMAGES: u32 = 120;
/// The probe's own deadline for its one launch.
const DEADLINE: Duration = Duration::from_secs(600);

/// The script: Select, All photographs, the first photograph, `D`; then `samples` times the
/// look-ahead decoded and `→`.
pub fn plan(samples: usize) -> Plan {
    let develop = |name: String, step: DevelopStep| Step::new(name, script::Step::Develop(step));
    let mut steps = vec![
        Step::opened("opened"),
        Step::new("select", script::Step::key("g")),
        Step::new(
            "all",
            script::Step::Select(SelectStep::Source("All photographs".into())),
        ),
        Step::new(
            "first",
            script::Step::Select(SelectStep::Click {
                position: 0,
                shift: false,
                command: false,
            }),
        ),
        develop("open".into(), DevelopStep::Active),
    ];
    for at in 1..=samples {
        steps.push(develop(format!("ready-{at}"), DevelopStep::Ready));
        steps.push(develop(
            format!("next-{at}"),
            DevelopStep::Step(SetStep::Next),
        ));
    }
    Plan::new(steps)
}

/// The frames from each `→` until its photograph's cached preview was drawn, from the `next-*`
/// frames' states, in order: the surface's own record of the frame it first drew the preview in.
/// Each must have handed over a preview of the photograph it moved to; the capture may show that
/// preview or, once a small original opened before it, the photograph's render.
pub fn key_to_presented<'a>(
    frames: impl IntoIterator<Item = (&'a str, &'a Value)>,
) -> Result<Vec<f64>> {
    let mut samples = Vec::new();
    for (name, state) in frames {
        if !name.starts_with("next-") {
            continue;
        }
        let develop = &state["develop"];
        let preview = &develop["preview"];
        let timing = &develop["timing"];
        let active = develop["set"]["active"].as_u64().unwrap_or(u64::MAX) as usize;
        let moved_to = develop["set"]["assets"].get(active);
        ensure(
            !timing["preview_version"].is_null()
                && moved_to == Some(&timing["asset_id"])
                && (preview.is_null()
                    || (preview["drawn"] == true && preview["asset_id"] == timing["asset_id"])),
            format!("{name}: no cached preview of the photograph moved to was drawn: {develop}"),
        )?;
        let frames = timing["presented_after"]
            .as_u64()
            .ok_or_else(|| format!("{name}: the preview's frames were not counted: {timing}"))?;
        samples.push(frames as f64);
    }
    Ok(samples)
}

/// One request through the probe's own client.
fn ask(
    owner: &OwnerHandle,
    client: luxforge_core::ClientId,
    method: &str,
    params: Value,
) -> Result<Value> {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("develop-switch-{method}"),
                method: method.into(),
                params,
                token: None,
            },
        )
        .map_err(|error| format!("{method}: {error}"))?;
    match response.error {
        Some(error) => Err(format!("{method}: {}: {}", error.code, error.message).into()),
        None => Ok(response.result.unwrap_or(Value::Null)),
    }
}

/// A catalog of `count` developed generated JPEGs under `generated`.
fn make(generated: &Path, count: usize) -> Result<PathBuf> {
    generate_catalog::run(
        &generated.join("source"),
        &generate_catalog::Options {
            seed: SEED,
            files: None,
            assets: None,
            images: Some(IMAGES),
        },
    )?;
    let images = generated.join("source").join("images");
    let manifest = read_json(&images.join("manifest.json"))?;
    let paths: Vec<PathBuf> = manifest["files"]
        .as_array()
        .ok_or("The image manifest lists no files")?
        .iter()
        .filter_map(|file| file["path"].as_str())
        .take(count)
        .map(|path| path.split('/').fold(images.clone(), |p, c| p.join(c)))
        .collect();
    ensure(paths.len() == count, format!("Only {} images", paths.len()))?;
    let catalog = generated.join(generate_catalog::CATALOG);
    drop(EditorService::open(&catalog).map_err(|error| format!("a new catalog: {error}"))?);
    let (owner, join) = OwnerHandle::start(&catalog)
        .map_err(|error| format!("the core cannot open the catalog: {error}"))?;
    let client = owner.register();
    let developed = (|| -> Result {
        let started = ask(
            &owner,
            client,
            "pick.develop",
            json!({"targets": {"kind": "paths", "paths": paths}, "into": [],
                "confirm_removable": true,
                "mutation": {"request_id": "develop-switch-setup", "actor": "setup"}}),
        )?;
        let begun = std::time::Instant::now();
        loop {
            let read = ask(
                &owner,
                client,
                "job.read",
                json!({"job_id": started["job_id"]}),
            )?;
            match read["status"].as_str() {
                Some("queued" | "running") => {
                    ensure(
                        begun.elapsed() < DEADLINE,
                        "The setup's Develop did not end",
                    )?;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Some("ready") if read["result"]["failed"] == json!([]) => return Ok(()),
                _ => return Err(format!("The setup could not develop: {read}").into()),
            }
        }
    })();
    owner.stop();
    let _ = join.join();
    developed?;
    Ok(catalog)
}

/// The Develop switch's rows.
pub fn develop_switch(context: &DesktopContext) -> Result<Vec<Row>> {
    ensure(
        context.binary.is_file(),
        format!(
            "the editor binary {} does not exist",
            context.binary.display()
        ),
    )?;
    let samples = context.samples.clamp(1, MAX_SAMPLES);
    let dir = context.scratch.join("develop-switch");
    let catalog = make(&dir.join("generated"), samples + 1)?;
    let plan = plan(samples);
    let launch = Launch::app()
        .catalog(&catalog)
        .script("script.json", plan.script())
        .window(PANELLED);
    let mut figures = Vec::new();
    let run = Run::tool(
        &crate::root()?,
        &dir.join("run"),
        "develop-switch",
        &context.binary,
        DEADLINE,
    )?;
    run.check(|run| {
        let evidence = run.launch(launch)?.dir;
        let checked = plan.check(&evidence)?;
        let frames: Vec<(&str, &Value)> = checked
            .names()
            .iter()
            .map(String::as_str)
            .zip(checked.frames.iter().map(|frame| frame.state()))
            .collect();
        figures = key_to_presented(frames)?;
        run.record("key_to_presented", json!(figures));
        Ok(())
    })?;
    Ok(vec![
        Row::measured(METRIC, "frames", figures)
            .target(TARGET)
            .scope(format!(
                "the release editor in the background at 1440 × 900 over {} developed 640 × 427 \
                 JPEGs, Develop opened on All photographs' first with the view as the set; each \
                 sample one → after the look-ahead's large previews were decoded",
                samples + 1
            ))
            .cache("warm: the neighbours' large previews rendered and decoded before each key")
            .detail(
                json!({"context": context.record(), "samples": samples, "run": dir.join("run")}),
            ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_develop_switch_plan_is_well_formed() {
        for samples in [1, 10, MAX_SAMPLES] {
            let plan = plan(samples);
            plan.validate().unwrap();
            assert_eq!(plan.len(), 5 + 2 * samples);
            assert!(plan.scripted());
        }
        assert_eq!(MAX_SAMPLES, 30);
        assert!(
            plan(MAX_SAMPLES)
                .steps()
                .iter()
                .filter(|step| step.script().is_some())
                .count()
                <= script::MAX_SCRIPT_STEPS
        );
    }

    #[test]
    fn develop_switch_reads_the_frames_from_each_key_to_its_preview() {
        let moved = |at: u64, frames: u64, shown: bool| {
            let asset = ["asset-a", "asset-b", "asset-c"][at as usize];
            json!({"develop": {
                "set": {"assets": ["asset-a", "asset-b", "asset-c"], "active": at},
                "preview": shown.then(|| json!({"asset_id": asset, "drawn": true})),
                "timing": {"asset_id": asset, "preview_version": 7, "presented_after": frames},
            }})
        };
        let opened = json!({"develop": {"preview": null}});
        // The second's render had already replaced its preview when its frame was captured.
        let first = moved(1, 1, true);
        let second = moved(2, 2, false);
        let frames = [
            ("open", &opened),
            ("ready-1", &opened),
            ("next-1", &first),
            ("ready-2", &opened),
            ("next-2", &second),
        ];
        assert_eq!(key_to_presented(frames).unwrap(), vec![1.0, 2.0]);
        // A move that handed over no preview, one of another photograph, or one not drawn, is no
        // sample.
        let none = json!({"develop": {
            "set": {"assets": ["asset-a", "asset-b"], "active": 1},
            "preview": null,
            "timing": {"asset_id": "asset-b"},
        }});
        assert!(key_to_presented([("next-1", &none)]).is_err());
        let other = json!({"develop": {
            "set": {"assets": ["asset-a", "asset-b"], "active": 1},
            "preview": {"asset_id": "asset-a", "drawn": true},
            "timing": {"asset_id": "asset-b", "preview_version": 3, "presented_after": 1},
        }});
        assert!(key_to_presented([("next-1", &other)]).is_err());
        let elsewhere = json!({"develop": {
            "set": {"assets": ["asset-a", "asset-b"], "active": 1},
            "preview": null,
            "timing": {"asset_id": "asset-a", "preview_version": 3, "presented_after": 1},
        }});
        assert!(key_to_presented([("next-1", &elsewhere)]).is_err());
        let undrawn = json!({"develop": {
            "set": {"assets": ["asset-a", "asset-b"], "active": 1},
            "preview": {"asset_id": "asset-b", "drawn": false},
            "timing": {"asset_id": "asset-b", "preview_version": 3, "presented_after": 1},
        }});
        assert!(key_to_presented([("next-1", &undrawn)]).is_err());
    }
}
