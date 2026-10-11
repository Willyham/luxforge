//! The lab's flows against a scripted segmenter and the stand-in fill: no models, no window.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::script::{self, Step};
use super::*;
use crate::runtime::{Io, Session};
use crate::segment::{Candidate, Embedding, LoadTimes, Prompt, Segmenter};
use crate::tensor::Tensor;

const W: u32 = 256;
const H: u32 = 192;
/// Objects as `[x0, y0, x1, y1]` in source pixels.
const A: [f32; 4] = [40.0, 40.0, 100.0, 100.0];
const B: [f32; 4] = [150.0, 40.0, 210.0, 100.0];

/// A segmenter over a scene of rectangles: a point inside one gets that rectangle (predicted IoU
/// 0.95); anything else gets an empty mask. Logits are 64 × 48 cells of 4 px.
struct Scene {
    objects: Vec<[f32; 4]>,
    delay: Duration,
    decodes: Arc<AtomicUsize>,
}

fn logits(rect: Option<[f32; 4]>) -> Logits {
    let (w, h) = (64, 48);
    let mut data = vec![-4.0f32; w * h];
    if let Some([x0, y0, x1, y1]) = rect {
        for v in 0..h {
            for u in 0..w {
                let (x, y) = ((u as f32 + 0.5) * 4.0, (v as f32 + 0.5) * 4.0);
                if x >= x0 && x < x1 && y >= y0 && y < y1 {
                    data[v * w + u] = 4.0;
                }
            }
        }
    }
    Logits {
        width: w,
        height: h,
        data,
        source: (W, H),
        cells_per_px: (0.25, 0.25),
    }
}

impl Segmenter for Scene {
    fn family(&self) -> Family {
        Family::EfficientVitL0
    }

    fn encode(&mut self, image: &Rgb8) -> Result<Embedding, String> {
        Ok(Embedding {
            family: Family::EfficientVitL0,
            source: (image.width, image.height),
            tensors: Vec::new(),
        })
    }

    fn decode(&mut self, _: &Embedding, prompt: &Prompt) -> Result<Vec<Candidate>, String> {
        std::thread::sleep(self.delay);
        self.decodes.fetch_add(1, Ordering::Relaxed);
        let hit = prompt.positive.first().and_then(|p| {
            self.objects
                .iter()
                .copied()
                .find(|r| p.x >= r[0] && p.x < r[2] && p.y >= r[1] && p.y < r[3])
        });
        Ok(vec![Candidate {
            logits: logits(hit),
            predicted_iou: 0.95,
        }])
    }
}

/// A LaMa stand-in session that always fails, so the real fill path fails as it would.
struct Broken;

impl Session for Broken {
    fn backend(&self) -> Backend {
        Backend::OrtCpu
    }
    fn inputs(&self) -> &[Io] {
        &[]
    }
    fn outputs(&self) -> &[Io] {
        &[]
    }
    fn run(&mut self, _: &[(&str, &Tensor)]) -> Result<Vec<(String, Tensor)>, String> {
        Err("no model in tests".into())
    }
}

struct Fixture {
    dir: PathBuf,
    input: PathBuf,
    original: Rgb8,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("removal-lab-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = (0..W * H)
            .flat_map(|i| {
                let (x, y) = (i % W, i / W);
                let inside = |r: [f32; 4]| {
                    (x as f32) >= r[0]
                        && (x as f32) < r[2]
                        && (y as f32) >= r[1]
                        && (y as f32) < r[3]
                };
                if inside(A) || inside(B) {
                    [200, 30, 30]
                } else {
                    [(x % 64) as u8 + 60, (y % 48) as u8 + 80, 120]
                }
            })
            .collect();
        let original = Rgb8::new(W, H, data);
        let input = dir.join("scene.png");
        original.save_png(&input).unwrap();
        Fixture {
            dir,
            input,
            original,
        }
    }

    fn lab(&self, delay: Duration, broken_fill: bool) -> (Lab, Arc<AtomicUsize>) {
        let mut config = Config::new(self.input.clone(), self.dir.join("out"));
        // A small dilation keeps regions local on the small scene.
        config.refine.dilation = Some(2);
        let decodes = Arc::new(AtomicUsize::new(0));
        let scene = Scene {
            objects: vec![A, B],
            delay,
            decodes: decodes.clone(),
        };
        let segmenter: workers::SegmenterOpener = Box::new(move || {
            let s: Box<dyn Segmenter> = Box::new(scene);
            Ok((
                s,
                LoadTimes {
                    encoder: Duration::ZERO,
                    decoder: Duration::ZERO,
                },
            ))
        });
        let filler: workers::FillerOpener = if broken_fill {
            Box::new(|| {
                Ok((
                    Filler::Lama {
                        model: crate::models::FillModel::default(),
                        session: Box::new(Broken),
                        params: fill::Params::default(),
                    },
                    None,
                ))
            })
        } else {
            Box::new(|| Ok((Filler::StandIn, None)))
        };
        let lab = Lab::start_with(
            config,
            Arc::new(Log::memory()),
            Arc::new(|| {}),
            segmenter,
            filler,
        );
        (lab, decodes)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const SETTLE: Duration = Duration::from_secs(20);

fn run(lab: &mut Lab, steps: &str) {
    let steps = script::parse(steps).unwrap();
    script::run_steps(lab, &steps).unwrap();
    script::settle(lab, SETTLE).unwrap();
}

fn changed(a: &Rgb8, b: &Rgb8, x: u32, y: u32) -> bool {
    a.pixel(x, y) != b.pixel(x, y)
}

#[test]
fn hovering_highlights_and_a_click_removes_then_undo_restores() {
    let f = Fixture::new("click");
    let (mut lab, _) = f.lab(Duration::ZERO, false);
    run(&mut lab, "settle\nhover 70,70\nsettle");
    let hover = lab
        .hover()
        .expect("the object under the pointer is highlighted");
    let raster = hover.raster.as_ref().unwrap();
    // The raster lies over object A.
    assert!(raster.rect[0] < 45.0 && raster.rect[0] + raster.rect[2] > 95.0);
    assert!(
        lab.status().contains("Hover to highlight"),
        "{}",
        lab.status()
    );

    run(&mut lab, "click 70,70\nexpect removals 1");
    let after = lab.image().unwrap().clone();
    assert!(changed(&f.original, &after, 70, 70));
    // Object B and the far corner are untouched.
    assert!(!changed(&f.original, &after, 180, 70));
    assert!(!changed(&f.original, &after, 250, 180));
    let click = lab.log().records("click");
    assert_eq!(click[0]["hovered"], true);
    assert_eq!(
        lab.log().records("removed")[0]["fill"]["fill"],
        pipeline::STAND_IN
    );

    run(&mut lab, "undo");
    assert_eq!(**lab.image().unwrap(), f.original);
    assert!(lab.status().contains("Undone"));
}

#[test]
fn a_click_before_any_hover_decodes_the_click_itself() {
    let f = Fixture::new("cold-click");
    let (mut lab, _) = f.lab(Duration::ZERO, false);
    script::settle(&mut lab, SETTLE).unwrap();
    lab.press(Point::new(180.0, 70.0), Instant::now(), 0.0);
    lab.release();
    script::settle(&mut lab, SETTLE).unwrap();
    assert_eq!(lab.removals(), 1);
    assert!(changed(&f.original, lab.image().unwrap(), 180, 70));
    assert!(!changed(&f.original, lab.image().unwrap(), 70, 70));
    assert_eq!(lab.log().records("click")[0]["hovered"], false);
}

#[test]
fn a_stroke_over_one_object_removes_it_with_the_footprint() {
    let f = Fixture::new("one");
    let (mut lab, _) = f.lab(Duration::ZERO, false);
    // The stroke runs past A's right edge onto the background: the footprint joins the region.
    run(
        &mut lab,
        "settle\nbrush 6\nstroke 50,70 90,70 110,70\nexpect decision one\nexpect removals 1",
    );
    let after = lab.image().unwrap();
    assert!(changed(&f.original, after, 60, 50));
    assert!(
        changed(&f.original, after, 108, 70),
        "the footprint is removed too"
    );
    assert!(!changed(&f.original, after, 180, 70));
}

#[test]
fn a_stroke_across_two_objects_offers_both_and_removes_the_chosen_one() {
    let f = Fixture::new("several");
    let (mut lab, _) = f.lab(Duration::ZERO, false);
    run(
        &mut lab,
        "settle\nbrush 6\nstroke 60,70 180,70\nexpect decision several\nexpect choosing 2",
    );
    assert!(
        lab.status().contains("2 objects under the stroke"),
        "{}",
        lab.status()
    );
    let choice = lab.choice().unwrap().clone();
    for item in &choice.items {
        let pin = item.pin.unwrap();
        assert!(item.logits.sample(pin) > 0.0, "each pin sits on its object");
    }
    // Enter with nothing chosen removes nothing.
    lab.confirm();
    assert!(lab.choice().is_some());
    // Choose A by clicking its pin, then remove.
    let a = choice
        .items
        .iter()
        .find(|i| i.logits.sample(Point::new(70.0, 70.0)) > 0.0)
        .unwrap();
    lab.press(a.pin.unwrap(), Instant::now(), 10.0);
    lab.release();
    run(&mut lab, "confirm\nexpect removals 1");
    let after = lab.image().unwrap();
    assert!(changed(&f.original, after, 70, 70));
    // B, and the water between them under the stroke, are left: several removes only the chosen.
    assert!(!changed(&f.original, after, 180, 70));
    assert!(!changed(&f.original, after, 125, 70));
}

#[test]
fn escape_cancels_a_choice_without_removing() {
    let f = Fixture::new("cancel");
    let (mut lab, _) = f.lab(Duration::ZERO, false);
    run(
        &mut lab,
        "settle\nbrush 6\nstroke 60,70 180,70\npick 1,2\ncancel",
    );
    assert!(lab.choice().is_none());
    assert_eq!(lab.removals(), 0);
    assert_eq!(**lab.image().unwrap(), f.original);
}

#[test]
fn a_stroke_over_nothing_removes_the_painted_area() {
    let f = Fixture::new("nothing");
    let (mut lab, _) = f.lab(Duration::ZERO, false);
    run(
        &mut lab,
        "settle\nbrush 5\nstroke 20,150 60,150\nexpect decision nothing\nexpect removals 1",
    );
    let after = lab.image().unwrap();
    assert!(changed(&f.original, after, 40, 150));
    assert!(!changed(&f.original, after, 40, 170));
    assert!(!changed(&f.original, after, 70, 70));
}

#[test]
fn a_failed_fill_leaves_the_image_and_says_why() {
    let f = Fixture::new("failed");
    let (mut lab, _) = f.lab(Duration::ZERO, true);
    run(&mut lab, "settle\nclick 70,70");
    assert_eq!(lab.removals(), 0);
    assert_eq!(**lab.image().unwrap(), f.original);
    assert!(lab.status().contains("Removal failed"), "{}", lab.status());
    assert!(lab.job().is_none());
}

#[test]
fn hover_keeps_only_the_latest_point() {
    let f = Fixture::new("mailbox");
    let (mut lab, decodes) = f.lab(Duration::from_millis(15), false);
    script::settle(&mut lab, SETTLE).unwrap();
    // Twenty moves 1 ms apart against a 15 ms decoder.
    run(&mut lab, "path 45,45 95,95 20 1");
    let n = decodes.load(Ordering::Relaxed);
    assert!(n < 10, "{n} decodes for 20 moves");
    let hovers = lab.log().records("hover");
    let replaced: u64 = hovers.iter().filter_map(|h| h["replaced"].as_u64()).sum();
    assert!(replaced >= 10, "{replaced}");
    // The last point is always answered.
    assert_eq!(lab.hover().unwrap().point, Point::new(95.0, 95.0));
    // Moving within one object keeps one raster: only the first result changed the overlay.
    let applied: Vec<_> = hovers.iter().filter(|h| h["applied"] == true).collect();
    assert_eq!(applied[0]["changed"], true);
    assert!(applied[1..].iter().all(|h| h["changed"] == false));
}

#[test]
fn saving_writes_new_files_and_never_the_input() {
    let f = Fixture::new("save");
    let (mut lab, _) = f.lab(Duration::ZERO, false);
    run(&mut lab, "settle\nclick 70,70\nsave\nsave");
    let saved = lab.log().records("saved");
    assert_eq!(saved.len(), 2);
    let paths: Vec<PathBuf> = saved
        .iter()
        .map(|s| PathBuf::from(s["path"].as_str().unwrap()))
        .collect();
    assert_ne!(paths[0], paths[1]);
    for p in &paths {
        assert_ne!(p, &f.input);
        assert_eq!(Rgb8::load(p).unwrap(), **lab.image().unwrap());
    }
    assert_eq!(Rgb8::load(&f.input).unwrap(), f.original);
}

#[test]
fn the_brush_waits_for_analysis_and_hover_stays_off_while_painting() {
    let f = Fixture::new("brush-early");
    let (mut lab, _) = f.lab(Duration::ZERO, false);
    // Paint as soon as the image is loaded, before analysis is known to be done.
    while lab.image().is_none() {
        lab.wait(Duration::from_millis(50));
    }
    lab.toggle_brush();
    lab.set_brush_radius(6.0);
    lab.press(Point::new(50.0, 70.0), Instant::now(), 0.0);
    lab.pointer_moved(Point::new(90.0, 70.0), Instant::now());
    assert!(lab.hover().is_none());
    assert!(lab.stroke().is_some());
    lab.release();
    script::settle(&mut lab, SETTLE).unwrap();
    assert_eq!(lab.last_decision(), Some("one"));
    assert_eq!(lab.removals(), 1);
}

#[test]
fn brackets_resize_the_brush_within_bounds() {
    let f = Fixture::new("resize");
    let (mut lab, _) = f.lab(Duration::ZERO, false);
    script::settle(&mut lab, SETTLE).unwrap();
    let r = lab.brush_radius();
    lab.grow_brush();
    assert!((lab.brush_radius() - r * 1.25).abs() < 1e-3);
    for _ in 0..100 {
        lab.shrink_brush();
    }
    assert_eq!(lab.brush_radius(), 2.0);
    for _ in 0..100 {
        lab.grow_brush();
    }
    assert_eq!(lab.brush_radius(), W as f32 / 4.0);
}

#[test]
fn the_picture_and_rasters_are_handed_over_once() {
    let f = Fixture::new("rasters");
    let (mut lab, _) = f.lab(Duration::ZERO, false);
    run(&mut lab, "settle\nhover 70,70\nsettle");
    let picture = lab.picture_mut().unwrap();
    assert_eq!(picture.size, (W, H));
    assert!(picture.rgba.take().is_some());
    let ids: Vec<u64> = lab
        .rasters_mut()
        .map(|r| {
            assert!(r.rgba.take().is_some());
            r.id
        })
        .collect();
    assert_eq!(ids.len(), 1);
    // A later unchanged result keeps the raster (and so the window's handle).
    run(&mut lab, "hover 72,72\nsettle");
    let again: Vec<u64> = lab.rasters_mut().map(|r| r.id).collect();
    assert_eq!(again, ids);
    // The script step type is shared with the window's calls.
    assert!(matches!(
        script::parse("hover 1,2").unwrap()[0],
        Step::Hover(_)
    ));
}

/// The whole lab with the pinned models and LaMa: hover the boy on the bridge, click, and the
/// result differs under him and nowhere far from him; undo restores the input exactly.
#[test]
#[ignore = "needs the pinned models and corpus"]
fn the_lab_removes_the_boy_on_the_bridge_with_lama() {
    let bridge = crate::paths::corpus_directory(None).join("z6-bridge.jpg");
    let out = std::env::temp_dir().join(format!("removal-lab-bridge-{}", std::process::id()));
    let mut lab = Lab::start(
        Config::new(bridge.clone(), out.clone()),
        Arc::new(Log::memory()),
        Arc::new(|| {}),
    );
    let settle = Duration::from_secs(120);
    let steps = script::parse("settle\nhover 1690,3550\nsettle\nclick 1690,3550").unwrap();
    script::run_steps(&mut lab, &steps).unwrap();
    script::settle(&mut lab, settle).unwrap();
    assert_eq!(lab.removals(), 1, "{}", lab.status());
    let removed = lab.log().records("removed");
    assert_eq!(removed[0]["fill"]["fill"], "lama");
    let [x, y, w, h] = [0, 1, 2, 3].map(|i| removed[0]["region"][i].as_u64().unwrap() as u32);
    let original = Rgb8::load(&bridge).unwrap();
    let after = lab.image().unwrap().clone();
    assert!(changed(&original, &after, 1690, 3550));
    for (px, py) in [
        (x - 1, y + h / 2),
        (x + w, y + h / 2),
        (100, 100),
        (3900, 5900),
    ] {
        assert!(!changed(&original, &after, px, py), "({px}, {py})");
    }
    lab.undo();
    script::settle(&mut lab, settle).unwrap();
    assert_eq!(**lab.image().unwrap(), original);
    let _ = std::fs::remove_dir_all(out);
}
