use std::sync::{Arc, Mutex, OnceLock};

use super::*;
use crate::mask::RectPx;
use crate::runtime::{Backend, Io, Session};
use crate::tensor::Data;

// ---- Geometry against the reference code, without models ----

#[test]
fn evit_frame_follows_get_preprocess_shape() {
    // z6-bridge.jpg after its EXIF rotation.
    let f = Frame::of(Family::EfficientVitL0, (4024, 6048));
    // 4024 · 512 / 6048 = 340.65 → 341; 4024 · 1024 / 6048 = 681.3 → 681.
    assert_eq!(f.content, (341, 512));
    assert_eq!(f.input, 512);
    assert_eq!(f.prompt_scale, (681.0 / 4024.0, 1024.0 / 6048.0));
    let landscape = Frame::of(Family::EfficientVitL0, (5464, 3640));
    // 3640 · 512 / 5464 = 341.08 → 341; 3640 · 1024 / 5464 = 682.2 → 682.
    assert_eq!(landscape.content, (512, 341));
    assert_eq!(landscape.prompt_scale, (1024.0 / 5464.0, 682.0 / 3640.0));
}

#[test]
fn sam21_frame_squashes_to_1024() {
    let f = Frame::of(Family::Sam21Small, (4024, 6048));
    assert_eq!(f.content, (1024, 1024));
    assert_eq!(f.input, 1024);
    assert_eq!(f.prompt_scale, (1024.0 / 4024.0, 1024.0 / 6048.0));
    assert_eq!(f.cells_per_px((256, 256)), (256.0 / 4024.0, 256.0 / 6048.0));
}

#[test]
fn a_source_position_lands_at_the_same_place_in_the_model() {
    // The decoders add half a pixel to the coordinate they receive, so the coordinate plus 0.5
    // must be the position scaled into the prompt frame.
    for family in Family::ALL {
        let f = Frame::of(*family, (4024, 6048));
        for p in [
            Point::new(0.5, 0.5),
            Point::new(1690.0, 3550.0),
            Point::new(4024.0, 6048.0),
        ] {
            let [x, y] = f.prompt_point(p);
            assert!((x + 0.5 - p.x * f.prompt_scale.0).abs() < 1e-3);
            assert!((y + 0.5 - p.y * f.prompt_scale.1).abs() < 1e-3);
        }
        // The far corner of the image is the far edge of the prompt content.
        let [x, _] = f.prompt_point(Point::new(4024.0, 0.0));
        let content = if *family == Family::EfficientVitL0 {
            681.0
        } else {
            1024.0
        };
        assert!((x + 0.5 - content).abs() < 1e-3, "{family:?}: {x}");
    }
}

/// PyTorch's `F.interpolate(..., mode="bilinear", align_corners=False)` along one axis.
fn interpolate(src: &[f32], out: usize) -> Vec<f32> {
    let scale = src.len() as f32 / out as f32;
    (0..out)
        .map(|i| {
            let s = ((i as f32 + 0.5) * scale - 0.5).max(0.0);
            let i0 = (s.floor() as usize).min(src.len() - 1);
            let i1 = (i0 + 1).min(src.len() - 1);
            let f = s - i0 as f32;
            src[i0] * (1.0 - f) + src[i1] * f
        })
        .collect()
}

/// The reference's map from low-resolution logits to source pixels along one axis: EfficientViT
/// upsamples to 1024, crops the prepadded size and resizes to the source; SAM 2 resizes directly.
fn reference_postprocess(
    family: Family,
    cells: &[f32],
    prepadded: usize,
    source: usize,
) -> Vec<f32> {
    match family {
        Family::EfficientVitL0 => {
            let full = interpolate(cells, 1024);
            interpolate(&full[..prepadded], source)
        }
        Family::Sam21Small => interpolate(cells, source),
    }
}

#[test]
fn logits_sample_where_the_reference_postprocessing_does() {
    // A ramp is reproduced exactly by bilinear interpolation, so away from the clamped edges
    // `Logits::sample` must agree with the reference's chain of resizes.
    let ramp: Vec<f32> = (0..256).map(|i| i as f32).collect();
    for family in Family::ALL {
        for source in [(3000u32, 2000u32), (4024, 6048)] {
            let f = Frame::of(*family, source);
            let cells = f.cells_per_px((256, 256));
            let prepadded = (
                (f.prompt_scale.0 * source.0 as f32).round() as usize,
                (f.prompt_scale.1 * source.1 as f32).round() as usize,
            );
            for axis in 0..2 {
                let n = if axis == 0 { source.0 } else { source.1 } as usize;
                let pre = if axis == 0 { prepadded.0 } else { prepadded.1 };
                let expected = reference_postprocess(*family, &ramp, pre, n);
                let l = Logits {
                    width: 256,
                    height: 256,
                    data: (0..256 * 256)
                        .map(|i| if axis == 0 { i % 256 } else { i / 256 } as f32)
                        .collect(),
                    source,
                    cells_per_px: cells,
                };
                let margin = n / 50;
                for (i, want) in expected.iter().enumerate().take(n - margin).skip(margin) {
                    let c = i as f32 + 0.5;
                    let p = if axis == 0 {
                        Point::new(c, 7.5)
                    } else {
                        Point::new(7.5, c)
                    };
                    let got = l.sample(p);
                    assert!(
                        (got - want).abs() < 2e-3,
                        "{family:?} {source:?} axis {axis} pixel {i}: {got} against {want}"
                    );
                }
            }
        }
    }
}

// ---- Preprocessing ----

fn flat(w: u32, h: u32, c: [u8; 3]) -> Rgb8 {
    Rgb8::new(w, h, c.repeat((w * h) as usize))
}

#[test]
fn evit_input_is_normalized_and_padded_bottom_right() {
    let img = flat(300, 150, [124, 116, 104]);
    let t = encoder_input(Family::EfficientVitL0, &img);
    assert_eq!(t.shape, [1, 3, 512, 512]);
    let v = t.as_f32().unwrap();
    // Content is 512 × 256 (150 · 512 / 300 = 256).
    let at = |c: usize, x: usize, y: usize| v[c * 512 * 512 + y * 512 + x];
    let expect = [
        (124.0 - 123.675) / 58.395,
        (116.0 - 116.28) / 57.12,
        (104.0 - 103.53) / 57.375,
    ];
    for (c, want) in expect.iter().enumerate() {
        for (x, y) in [(0, 0), (511, 0), (200, 255), (0, 255)] {
            assert!((at(c, x, y) - want).abs() < 1e-5, "{c} {x} {y}");
        }
        for (x, y) in [(0, 256), (511, 511), (300, 400)] {
            assert_eq!(at(c, x, y), 0.0, "padding at {c} {x} {y}");
        }
    }
}

#[test]
fn sam21_input_fills_the_square() {
    let img = flat(300, 150, [255, 0, 128]);
    let t = encoder_input(Family::Sam21Small, &img);
    assert_eq!(t.shape, [1, 3, 1024, 1024]);
    let v = t.as_f32().unwrap();
    let at = |c: usize, x: usize, y: usize| v[c * 1024 * 1024 + y * 1024 + x];
    for (x, y) in [(0, 0), (1023, 1023), (512, 900)] {
        assert!((at(0, x, y) - (255.0 - 123.675) / 58.395).abs() < 1e-5);
        assert!((at(1, x, y) - (0.0 - 116.28) / 57.12).abs() < 1e-5);
        assert!((at(2, x, y) - (128.0 - 103.53) / 57.375).abs() < 1e-5);
    }
}

// ---- Prompts ----

fn boy_prompt() -> Prompt {
    Prompt {
        positive: vec![Point::new(100.0, 50.0)],
        negative: vec![Point::new(10.0, 20.0)],
        // Given bottom-right first: the corners are put in order.
        bbox: Some((Point::new(300.0, 200.0), Point::new(40.0, 30.0))),
        previous: None,
    }
}

#[test]
fn evit_puts_points_before_the_box() {
    let f = Frame::of(Family::EfficientVitL0, (1000, 500));
    let (coords, labels) = evit::prompt_tokens(&f, &boy_prompt());
    assert_eq!(labels, [1.0, 0.0, 2.0, 3.0]);
    let s = f.prompt_scale;
    let expect = [
        100.0 * s.0,
        50.0 * s.1,
        10.0 * s.0,
        20.0 * s.1,
        40.0 * s.0,
        30.0 * s.1,
        300.0 * s.0,
        200.0 * s.1,
    ];
    for (c, e) in coords.iter().zip(expect) {
        assert!((c + 0.5 - e).abs() < 1e-4, "{coords:?}");
    }
}

#[test]
fn sam21_puts_the_box_first_and_pads_nothing() {
    let f = Frame::of(Family::Sam21Small, (1000, 500));
    let (coords, labels) = sam21::prompt_tokens(&f, &boy_prompt());
    assert_eq!(labels, [2.0, 3.0, 1.0, 0.0]);
    assert_eq!(coords.len(), 8);
    assert!((coords[0] + 0.5 - 40.0 * 1.024).abs() < 1e-4);
    assert!((coords[3] + 0.5 - 200.0 * 2.048).abs() < 1e-4);
    let points_only = Prompt {
        positive: vec![Point::new(1.0, 1.0)],
        ..Prompt::default()
    };
    assert_eq!(sam21::prompt_tokens(&f, &points_only).1, [1.0]);
}

#[test]
fn empty_and_non_finite_prompts_are_refused() {
    assert!(check_prompt(&Prompt::default()).is_err());
    let nan = Prompt {
        positive: vec![Point::new(f32::NAN, 1.0)],
        ..Prompt::default()
    };
    assert!(check_prompt(&nan).is_err());
    assert!(check_prompt(&boy_prompt()).is_ok());
}

#[test]
fn a_previous_mask_on_the_decoder_grid_passes_through() {
    let f = Frame::of(Family::Sam21Small, (800, 600));
    let l = Logits {
        width: 256,
        height: 256,
        data: (0..256 * 256).map(|i| (i % 97) as f32 - 40.0).collect(),
        source: (800, 600),
        cells_per_px: f.cells_per_px((256, 256)),
    };
    assert_eq!(sam21::mask_input(&l, &f).unwrap(), l.data);
}

#[test]
fn a_previous_mask_on_another_grid_is_resampled() {
    // EfficientViT's logits for the same image cover the padded long-side frame; on SAM 2.1's
    // grid a ramp in source x must come back as the same ramp in source x.
    let source = (800u32, 600u32);
    let evit = Frame::of(Family::EfficientVitL0, source);
    let sam = Frame::of(Family::Sam21Small, source);
    let ce = evit.cells_per_px((256, 256));
    let l = Logits {
        width: 256,
        height: 256,
        // Logit = source x of the cell centre.
        data: (0..256 * 256)
            .map(|i| ((i % 256) as f32 + 0.5) / ce.0)
            .collect(),
        source,
        cells_per_px: ce,
    };
    let m = sam21::mask_input(&l, &sam).unwrap();
    let cs = sam.cells_per_px((256, 256));
    for (u, v) in [(10usize, 3usize), (128, 128), (200, 250)] {
        let x = (u as f32 + 0.5) / cs.0;
        assert!(
            (m[v * 256 + u] - x).abs() < 1e-2,
            "{u},{v}: {}",
            m[v * 256 + u]
        );
    }
    let other = Logits {
        source: (801, 600),
        ..l
    };
    assert!(sam21::mask_input(&other, &sam).is_err());
}

// ---- Decoder plumbing with a stand-in session ----

type Calls = Arc<Mutex<Vec<Vec<(String, Tensor)>>>>;

/// Records every call and answers like a SAM decoder: `masks` of `(B, M, 4, 4)` whose values are
/// each prompt's first x coordinate plus the mask index, and `iou_predictions` rising with the
/// mask index.
struct FakeDecoder {
    masks: usize,
    calls: Calls,
}

impl Session for FakeDecoder {
    fn backend(&self) -> Backend {
        Backend::OrtCpu
    }
    fn inputs(&self) -> &[Io] {
        &[]
    }
    fn outputs(&self) -> &[Io] {
        &[]
    }
    fn run(&mut self, inputs: &[(&str, &Tensor)]) -> Result<Vec<(String, Tensor)>, String> {
        self.calls.lock().unwrap().push(
            inputs
                .iter()
                .map(|(n, t)| (n.to_string(), (*t).clone()))
                .collect(),
        );
        let coords = inputs.iter().find(|(n, _)| *n == "point_coords").unwrap().1;
        let (b, n) = (coords.shape[0], coords.shape[1]);
        let c = coords.as_f32().unwrap();
        let m = self.masks;
        let mut masks = Vec::new();
        let mut iou = Vec::new();
        for i in 0..b {
            for j in 0..m {
                masks.extend(std::iter::repeat_n(c[i * n * 2] + j as f32, 16));
                iou.push(j as f32 / 10.0);
            }
        }
        Ok(vec![
            ("masks".into(), Tensor::f32([b, m, 4, 4], masks)),
            ("iou_predictions".into(), Tensor::f32([b, m], iou)),
        ])
    }
}

struct NoEncoder;

impl Session for NoEncoder {
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
        Err("not used".into())
    }
}

fn fake(family: Family) -> (Box<dyn Segmenter>, Calls) {
    let calls = Calls::default();
    let decoder = |masks| {
        Box::new(FakeDecoder {
            masks,
            calls: calls.clone(),
        })
    };
    let s: Box<dyn Segmenter> = match family {
        Family::EfficientVitL0 => {
            Box::new(evit::EfficientVit::new(Box::new(NoEncoder), decoder(1)))
        }
        Family::Sam21Small => Box::new(sam21::Sam21::new(Box::new(NoEncoder), decoder(3))),
    };
    (s, calls)
}

fn embedding(family: Family, source: (u32, u32)) -> Embedding {
    let names: &[&str] = match family {
        Family::EfficientVitL0 => &["image_embeddings"],
        Family::Sam21Small => &["image_embed", "high_res_feats_0", "high_res_feats_1"],
    };
    Embedding {
        family,
        source,
        tensors: names
            .iter()
            .map(|n| (n.to_string(), Tensor::f32([1], vec![0.0])))
            .collect(),
    }
}

fn point_prompt(x: f32, extra: usize) -> Prompt {
    Prompt {
        positive: (0..=extra).map(|_| Point::new(x, 10.0)).collect(),
        ..Prompt::default()
    }
}

#[test]
fn evit_batches_prompts_of_one_length_and_keeps_their_order() {
    let (mut s, calls) = fake(Family::EfficientVitL0);
    let e = embedding(Family::EfficientVitL0, (1024, 512));
    // Lengths 1, 2, 1, 2, 1: two calls, results back in prompt order.
    let prompts: Vec<Prompt> = (0..5)
        .map(|i| point_prompt(100.0 * (i + 1) as f32, i % 2))
        .collect();
    let out = s.decode_many(&e, &prompts).unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    let shapes: Vec<_> = calls
        .iter()
        .map(|c| {
            c.iter()
                .find(|(n, _)| n == "point_coords")
                .unwrap()
                .1
                .shape
                .clone()
        })
        .collect();
    assert_eq!(shapes, [vec![3, 1, 2], vec![2, 2, 2]]);
    for (i, c) in out.iter().enumerate() {
        assert_eq!(c.len(), 1);
        // 1024 × 512 has prompt scale 1, so the first coordinate is x − 0.5.
        assert_eq!(
            c[0].logits.data[0],
            100.0 * (i + 1) as f32 - 0.5,
            "prompt {i}"
        );
        assert_eq!(c[0].logits.source, (1024, 512));
        assert_eq!(c[0].logits.cells_per_px, (4.0 / 1024.0, 4.0 / 1024.0));
    }
    // Labels travel as f32, one per point.
    let labels = &calls[1]
        .iter()
        .find(|(n, _)| n == "point_labels")
        .unwrap()
        .1;
    assert_eq!(labels.data, Data::F32(vec![1.0; 4]));
}

#[test]
fn sam21_sorts_its_three_masks_and_feeds_the_previous_one() {
    let (mut s, calls) = fake(Family::Sam21Small);
    let source = (2048, 1024);
    let e = embedding(Family::Sam21Small, source);
    let first = s.decode(&e, &point_prompt(1000.0, 0)).unwrap();
    // Highest predicted IoU first: the fake's mask 2.
    let ious: Vec<f32> = first.iter().map(|c| c.predicted_iou).collect();
    assert_eq!(ious, [0.2, 0.1, 0.0]);
    assert_eq!(first[0].logits.data[0], 1000.0 * 0.5 - 0.5 + 2.0);
    {
        let calls = calls.lock().unwrap();
        let get = |n: &str| calls[0].iter().find(|(k, _)| k == n).unwrap().1.clone();
        assert_eq!(get("has_mask_input").data, Data::F32(vec![0.0]));
        assert_eq!(get("mask_input").shape, [1, 1, 256, 256]);
        assert_eq!(get("point_coords").shape, [1, 1, 2]);
        for name in ["image_embed", "high_res_feats_0", "high_res_feats_1"] {
            assert_eq!(get(name).shape, [1]);
        }
    }
    let previous = Logits {
        width: 256,
        height: 256,
        data: vec![3.0; 256 * 256],
        source,
        cells_per_px: Frame::of(Family::Sam21Small, source).cells_per_px((256, 256)),
    };
    let again = Prompt {
        previous: Some(previous),
        ..point_prompt(1000.0, 0)
    };
    s.decode(&e, &again).unwrap();
    let calls = calls.lock().unwrap();
    let get = |n: &str| calls[1].iter().find(|(k, _)| k == n).unwrap().1.clone();
    assert_eq!(get("has_mask_input").data, Data::F32(vec![1.0]));
    assert_eq!(get("mask_input").data, Data::F32(vec![3.0; 256 * 256]));
}

#[test]
fn an_embedding_from_the_other_family_is_refused() {
    let (mut s, _) = fake(Family::Sam21Small);
    let e = embedding(Family::EfficientVitL0, (100, 100));
    assert!(s.decode(&e, &point_prompt(5.0, 0)).is_err());
}

// ---- With the pinned models and the corpus ----

const BRIDGE: &str = "/Users/will/projects/lightwell/private/object-removal/corpus/z6-bridge.jpg";
/// On the boy's shorts, walking toward the camera (the image is portrait after EXIF rotation).
const BOY_POINT: Point = Point {
    x: 1690.0,
    y: 3550.0,
};
/// The boy from hair to sandals, including the bag in his right hand.
const BOY_BOX: RectPx = RectPx {
    x: 1400,
    y: 2820,
    width: 610,
    height: 1410,
};

struct Fixture {
    image: Rgb8,
    embedding: Embedding,
    segmenter: Mutex<Box<dyn Segmenter>>,
}

fn fixture(family: Family) -> &'static Fixture {
    static EVIT: OnceLock<Fixture> = OnceLock::new();
    static SAM: OnceLock<Fixture> = OnceLock::new();
    let cell = match family {
        Family::EfficientVitL0 => &EVIT,
        Family::Sam21Small => &SAM,
    };
    cell.get_or_init(|| {
        let image = Rgb8::load(std::path::Path::new(BRIDGE)).unwrap();
        assert_eq!(
            (image.width, image.height),
            (4024, 6048),
            "EXIF rotation applied"
        );
        let dir = crate::models::directory(None);
        let mut segmenter = open(family, Backend::OrtCpu, Backend::OrtCpu, &dir).unwrap();
        let embedding = segmenter.encode(&image).unwrap();
        Fixture {
            image,
            embedding,
            segmenter: Mutex::new(segmenter),
        }
    })
}

fn decode(family: Family, prompt: &Prompt) -> Vec<Candidate> {
    let f = fixture(family);
    f.segmenter
        .lock()
        .unwrap()
        .decode(&f.embedding, prompt)
        .unwrap()
}

/// A candidate's footprint in source pixels.
#[derive(Debug)]
struct Footprint {
    area: u64,
    inside_boy: u64,
    bounds: RectPx,
}

impl Footprint {
    fn of(l: &Logits) -> Footprint {
        let (w, h) = l.source;
        let (mut area, mut inside) = (0, 0);
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if l.sample(Point::new(x as f32 + 0.5, y as f32 + 0.5)) > 0.0 {
                    area += 1;
                    inside += BOY_BOX.contains(x, y) as u64;
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
                }
            }
        }
        Footprint {
            area,
            inside_boy: inside,
            bounds: RectPx {
                x: x0.min(x1),
                y: y0.min(y1),
                width: x1.saturating_sub(x0),
                height: y1.saturating_sub(y0),
            },
        }
    }

    /// Share of the boy's box this candidate covers.
    fn covers(&self) -> f64 {
        self.inside_boy as f64 / (BOY_BOX.width as f64 * BOY_BOX.height as f64)
    }

    /// Share of this candidate inside the boy's box.
    fn within(&self) -> f64 {
        self.inside_boy as f64 / self.area.max(1) as f64
    }

    /// IoU of the candidate's bounds with the boy's box.
    fn box_iou(&self) -> f64 {
        let (a, b) = (self.bounds, BOY_BOX);
        let ix = (a.x + a.width)
            .min(b.x + b.width)
            .saturating_sub(a.x.max(b.x)) as f64;
        let iy = (a.y + a.height)
            .min(b.y + b.height)
            .saturating_sub(a.y.max(b.y)) as f64;
        let inter = ix * iy;
        let union = a.width as f64 * a.height as f64 + b.width as f64 * b.height as f64 - inter;
        inter / union
    }
}

fn point_on_the_boy_selects_him(family: Family) {
    let prompt = Prompt {
        positive: vec![BOY_POINT],
        ..Prompt::default()
    };
    let candidates = decode(family, &prompt);
    let prints: Vec<Footprint> = candidates
        .iter()
        .map(|c| Footprint::of(&c.logits))
        .collect();
    for (c, p) in candidates.iter().zip(&prints) {
        eprintln!(
            "{}: iou {:.3} area {} covers {:.3} within {:.3} bounds {:?}",
            family.name(),
            c.predicted_iou,
            p.area,
            p.covers(),
            p.within(),
            p.bounds
        );
    }
    // The best candidate is under the point and stays on the boy, off the bridge.
    assert!(candidates[0].logits.sample(BOY_POINT) > 0.0);
    assert!(prints[0].within() > 0.97, "{:?}", prints[0]);
    assert!(prints[0].covers() > 0.2, "{:?}", prints[0]);
    // One candidate is the whole boy: over a third of his box, all of it on him.
    assert!(
        prints
            .iter()
            .any(|p| p.covers() > 0.38 && p.within() > 0.97),
        "{prints:?}"
    );
}

#[test]
#[ignore = "needs the pinned models and corpus"]
fn evit_point_on_the_boy_selects_him() {
    point_on_the_boy_selects_him(Family::EfficientVitL0);
}

#[test]
#[ignore = "needs the pinned models and corpus"]
fn sam21_point_on_the_boy_selects_him() {
    point_on_the_boy_selects_him(Family::Sam21Small);
}

fn same_prompt_same_logits(family: Family) {
    let f = fixture(family);
    let prompt = Prompt {
        positive: vec![BOY_POINT],
        negative: vec![Point::new(2600.0, 4800.0)],
        ..Prompt::default()
    };
    let a = decode(family, &prompt);
    let b = decode(family, &prompt);
    assert_eq!(a, b);
    // And a second encoding of the same image decodes to the same logits.
    let mut s = f.segmenter.lock().unwrap();
    let again = s.encode(&f.image).unwrap();
    assert_eq!(again.tensors, f.embedding.tensors);
    assert_eq!(s.decode(&again, &prompt).unwrap(), a);
}

#[test]
#[ignore = "needs the pinned models and corpus"]
fn evit_same_prompt_same_logits() {
    same_prompt_same_logits(Family::EfficientVitL0);
}

#[test]
#[ignore = "needs the pinned models and corpus"]
fn sam21_same_prompt_same_logits() {
    same_prompt_same_logits(Family::Sam21Small);
}

fn box_beats_point(family: Family) {
    let point = decode(
        family,
        &Prompt {
            positive: vec![BOY_POINT],
            ..Prompt::default()
        },
    );
    let corners = (
        Point::new(BOY_BOX.x as f32, BOY_BOX.y as f32),
        Point::new(
            (BOY_BOX.x + BOY_BOX.width) as f32,
            (BOY_BOX.y + BOY_BOX.height) as f32,
        ),
    );
    let boxed = decode(
        family,
        &Prompt {
            bbox: Some(corners),
            ..Prompt::default()
        },
    );
    let (p, b) = (
        Footprint::of(&point[0].logits),
        Footprint::of(&boxed[0].logits),
    );
    eprintln!(
        "{}: point iou {:.3} box-iou {:.3} covers {:.3}; box iou {:.3} box-iou {:.3} covers {:.3}",
        family.name(),
        point[0].predicted_iou,
        p.box_iou(),
        p.covers(),
        boxed[0].predicted_iou,
        b.box_iou(),
        b.covers()
    );
    // The box's best mask is the whole boy: it spans his box, stays inside it, covers more of
    // him than the point's best mask, and the decoder is more confident in it.
    assert!(b.box_iou() > 0.85, "{b:?}");
    assert!(b.within() > 0.97, "{b:?}");
    assert!(b.box_iou() >= p.box_iou());
    assert!(b.covers() >= p.covers());
    assert!(boxed[0].predicted_iou > point[0].predicted_iou);
}

#[test]
#[ignore = "needs the pinned models and corpus"]
fn evit_box_beats_point() {
    box_beats_point(Family::EfficientVitL0);
}

#[test]
#[ignore = "needs the pinned models and corpus"]
fn sam21_box_beats_point() {
    box_beats_point(Family::Sam21Small);
}

#[test]
#[ignore = "needs the pinned models and corpus"]
fn evit_batched_prompts_decode_as_they_do_alone() {
    let f = fixture(Family::EfficientVitL0);
    let prompts: Vec<Prompt> = [(1690.0, 3550.0), (1700.0, 3100.0), (600.0, 1500.0)]
        .iter()
        .map(|&(x, y)| Prompt {
            positive: vec![Point::new(x, y)],
            ..Prompt::default()
        })
        .chain([Prompt {
            positive: vec![BOY_POINT],
            negative: vec![Point::new(1500.0, 5000.0)],
            ..Prompt::default()
        }])
        .collect();
    let mut s = f.segmenter.lock().unwrap();
    let batched = s.decode_many(&f.embedding, &prompts).unwrap();
    for (p, b) in prompts.iter().zip(&batched) {
        let alone = s.decode(&f.embedding, p).unwrap();
        let worst = alone[0]
            .logits
            .data
            .iter()
            .zip(&b[0].logits.data)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "{p:?}: {worst}");
        assert!((alone[0].predicted_iou - b[0].predicted_iou).abs() < 1e-4);
    }
}

#[test]
#[ignore = "needs the pinned models and corpus"]
fn sam21_previous_mask_reaches_the_decoder() {
    // Feeding the whole-boy mask back with the same point changes the decoder's answer and keeps
    // it on the boy. In multimask mode it does not change the ranking: SAM 2.1 still puts his
    // clothes first (about 0.93) and the whole boy among the three (about 0.64).
    let point = Prompt {
        positive: vec![BOY_POINT],
        ..Prompt::default()
    };
    let first = decode(Family::Sam21Small, &point);
    let whole = first
        .iter()
        .max_by_key(|c| Footprint::of(&c.logits).inside_boy)
        .unwrap();
    let refined = decode(
        Family::Sam21Small,
        &Prompt {
            previous: Some(whole.logits.clone()),
            ..point
        },
    );
    for c in first.iter().chain(&refined) {
        eprintln!("iou {:.3} {:?}", c.predicted_iou, Footprint::of(&c.logits));
    }
    assert_ne!(first, refined);
    let prints: Vec<Footprint> = refined.iter().map(|c| Footprint::of(&c.logits)).collect();
    assert!(prints.iter().all(|p| p.within() > 0.97), "{prints:?}");
    assert!(prints.iter().any(|p| p.covers() > 0.38), "{prints:?}");
}
