//! Standalone photographic evidence utility, linked to the built core (no GUI or timing).
//! Arguments: private RAW manifest, authentic JPEG, fresh evidence directory.
//! All images come from EditorService::render_current; final views use the shared reduce-only
//! preview path over those exact pixels. Run instructions live in fixtures/detail/README.md.

use image::{RgbaImage, imageops};
use luxforge_core::{EditorService, Mutation, PreviewIntent, PreviewQueue, ProxyBounds, Raster};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{error::Error, fs, path::Path, sync::Arc, time::Duration};

fn parameters(id: &str) -> Value {
    let mut p = json!({"sharpening":0,"radius":1.0,"sharpen-detail":25,
        "sharpen-masking":0,"luminance":0,"luminance-detail":50,"colour":0,"colour-detail":50});
    match id {
        "moderate" => {
            p["luminance"] = json!(40);
            p["colour"] = json!(40);
            p["sharpening"] = json!(50);
        }
        "noise-stress" => {
            p["luminance"] = json!(100);
            p["colour"] = json!(100);
            p["luminance-detail"] = json!(0);
            p["colour-detail"] = json!(0);
        }
        "sharpen-stress" => {
            p["sharpening"] = json!(150);
            p["radius"] = json!(3.0);
            p["sharpen-detail"] = json!(100);
        }
        "baseline" => {}
        _ => panic!("unknown configuration"),
    }
    p
}

fn image(raster: &Raster) -> RgbaImage {
    RgbaImage::from_raw(raster.width, raster.height, (*raster.rgba).clone()).unwrap()
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(3..=4).contains(&args.len()) {
        return Err(
            "expected private RAW manifest, JPEG, fresh output directory, optional source key"
                .into(),
        );
    }
    let private: Value = serde_json::from_slice(&fs::read(&args[0])?)?;
    let corpus: Value = serde_json::from_slice(&fs::read("fixtures/detail/corpus.json")?)?;
    let output = Path::new(&args[2]);
    fs::create_dir(output)?;
    let mut records = vec![];
    for source in corpus["sources"].as_array().unwrap() {
        let key = source["manifest_key"].as_str().unwrap();
        if args.get(3).is_some_and(|selected| selected != key) {
            continue;
        }
        let path = if key == "will-sapa-drone-jpeg" {
            args[1].clone()
        } else {
            private["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["id"] == key)
                .ok_or("missing private source")?["path"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let expected_hash = source["sha256"].as_str().unwrap();
        let hash = format!("{:x}", Sha256::digest(fs::read(&path)?));
        if hash != expected_hash {
            return Err(format!("{key}: original hash mismatch").into());
        }
        let directory = output.join(key);
        fs::create_dir(&directory)?;
        let mut service = EditorService::open(&directory.join("catalog.sqlite"))?;
        let asset = service.import(Path::new(&path))?.asset.id;
        let mut queue = PreviewQueue::default();
        let (wake, receive) = std::sync::mpsc::channel();
        queue.set_waker(Arc::new(move || {
            let _ = wake.send(());
        }));
        let mut crops = vec![];
        for config in ["baseline", "moderate", "noise-stress", "sharpen-stress"] {
            if config != "baseline" {
                let revision = service.state(&asset)?.revision;
                service.apply_action(
                    &asset,
                    Mutation {
                        expected_revision: revision,
                        request_id: format!("detail-{config}"),
                        actor: "detail-study".into(),
                    },
                    "set-detail",
                    parameters(config),
                )?;
            }
            println!("{key}: rendering {config}");
            let raster = service.render_current(&asset)?;
            if raster.source_fingerprint != expected_hash {
                return Err("render fingerprint mismatch".into());
            }
            let orientation = service
                .preview_job(&asset, None, None, None, None)?
                .evaluation
                .source()
                .orientation();
            let full = image(&raster);
            for (index, crop) in source["inspection_crops"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
            {
                let c: Vec<_> = crop
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect();
                let content_width = source["dimensions"][0].as_u64().unwrap() as u32;
                let content_height = source["dimensions"][1].as_u64().unwrap() as u32;
                let rectangle = if (raster.width, raster.height) == (content_width, content_height)
                {
                    [c[0], c[1], c[2], c[3]]
                } else if orientation == 8
                    && (raster.width, raster.height) == (content_height, content_width)
                {
                    [c[1], content_width - c[0] - c[2], c[3], c[2]]
                } else {
                    return Err(format!(
                        "{key}: unsupported orientation/dimension mapping {orientation}"
                    )
                    .into());
                };
                let filename = format!("crop{}-{config}.png", index + 1);
                imageops::crop_imm(
                    &full,
                    rectangle[0],
                    rectangle[1],
                    rectangle[2],
                    rectangle[3],
                )
                .to_image()
                .save(directory.join(&filename))?;
                crops.push(
                    json!({"crop":index+1,"configuration":config,"content_rectangle":crop,
                    "render_rectangle":rectangle,"file":format!("{key}/{filename}")}),
                );
            }
            // The final image is the shared, linear-light reduction of the final exact Raster.
            // No moving proxy approximation or extra recipe render enters this inspection view.
            let mut job = service.preview_job(
                &asset,
                None,
                None,
                None,
                Some(ProxyBounds {
                    width: 1024,
                    height: 1024,
                }),
            )?;
            job.reduce = Some(Arc::new(raster.clone()));
            job.intent = PreviewIntent::Reduce;
            queue.request(job);
            loop {
                if let Some(result) = queue.poll() {
                    if let Some(exact) = result.exact() {
                        exact
                            .result
                            .as_ref()
                            .map_err(|e| format!("reduce: {e:?}"))?;
                        image(exact.display.as_ref().ok_or("no final-size reduction")?)
                            .save(directory.join(format!("final-{config}.png")))?;
                        break;
                    }
                } else {
                    receive.recv_timeout(Duration::from_secs(60))?;
                }
            }
            println!(
                "{key}: saved {config} at {}x{} and final size",
                raster.width, raster.height
            );
        }
        drop((queue, service));
        fs::remove_file(directory.join("catalog.sqlite"))?;
        let after_hash = format!("{:x}", Sha256::digest(fs::read(&path)?));
        if after_hash != expected_hash {
            return Err(format!("{key}: original changed").into());
        }
        records.push(json!({"source":key,"sha256_before":hash,"sha256_after":after_hash,
            "parameter_sets":["baseline","moderate","noise-stress","sharpen-stress"],"crops":crops}));
    }
    fs::write(
        output.join("renders.json"),
        serde_json::to_vec_pretty(&json!({
        "format":1,"pipeline":"EditorService::render_current then PreviewIntent::Reduce",
        "final_bounds":[1024,1024],
        "configuration_requests":{"baseline":null,"moderate":parameters("moderate"),
            "noise-stress":parameters("noise-stress"),"sharpen-stress":parameters("sharpen-stress")},
        "sources":records }))?,
    )?;
    Ok(())
}
