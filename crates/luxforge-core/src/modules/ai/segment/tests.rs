use super::session::{EMBEDDING_SHAPE, prompt_tokens};
use super::*;
use crate::{
    Cancel,
    inference::{
        Device, ModelIdentity, Placement, Precision, Session, Tensor, TensorData, TensorSpec,
    },
};
use std::sync::Mutex;

#[test]
fn frames_follow_the_long_side_rules() {
    // A 1024 × 683 analysis input: the encoder sees 512 × 342, prompts live in 1024 × 683.
    let frame = Frame::of((1024, 683));
    assert_eq!(frame.content, (512, 342));
    assert_eq!(frame.prompt_scale, (1.0, 1.0));
    // A position lands at itself once the decoder adds its half pixel.
    assert_eq!(frame.prompt_point(Point::new(10.0, 20.0)), [9.5, 19.5]);
    // 256 logit cells over the 1024 prompt frame: a quarter cell per pixel.
    assert_eq!(frame.cells_per_px((256, 256)), (0.25, 0.25));
    // A portrait input scales both ways by the long side; rounding is half up.
    let tall = Frame::of((3, 512));
    assert_eq!(tall.content, (3, 512));
    assert_eq!(Frame::of((5, 1024)).content, (3, 512));
}

#[test]
fn normalized_planes_pad_after_normalizing() {
    let rgb = [123.675, 116.28, 103.53, 182.07, 173.4, 160.905];
    let planes = frame::normalized_nchw(&rgb, (2, 1), 3);
    assert_eq!(planes.len(), 27);
    // The mean normalizes to 0, a mean plus one deviation to 1, and the padding stays 0.
    for plane in planes.chunks_exact(9) {
        assert!(
            plane[0].abs() < 1e-6 && (plane[1] - 1.0).abs() < 1e-5,
            "{plane:?}"
        );
        assert!(plane[2..].iter().all(|v| *v == 0.0), "{plane:?}");
    }
}

#[test]
fn resampling_halves_by_averaging_pairs() {
    // A 4 × 1 ramp to 2 × 1: PIL's triangle filter widened by 2 weights 1, 3, 3, 1 over the
    // reachable taps, clipped at the edges.
    let rgb: Vec<f32> = [0.0, 10.0, 20.0, 30.0]
        .iter()
        .flat_map(|&v| [v, v, v])
        .collect();
    let out = resample(&rgb, (4, 1), (2, 1));
    let expected = [
        (0.0 * 3.0 + 10.0 * 3.0 + 20.0) / 7.0,
        (10.0 + 20.0 * 3.0 + 30.0 * 3.0) / 7.0,
    ];
    for (i, e) in expected.iter().enumerate() {
        assert!((out[i * 3] - e).abs() < 1e-4, "{} vs {e}", out[i * 3]);
    }
    // The identity size reproduces the input.
    assert_eq!(resample(&rgb, (4, 1), (4, 1)), rgb);
}

#[test]
fn prompt_tokens_put_points_before_box_corners() {
    let frame = Frame::of((1024, 512));
    let prompt = Prompt {
        positive: vec![Point::new(1.0, 2.0)],
        negative: vec![Point::new(3.0, 4.0)],
        bbox: Some((Point::new(50.0, 60.0), Point::new(10.0, 20.0))),
    };
    let (coords, labels) = prompt_tokens(&frame, &prompt);
    assert_eq!(labels, vec![1.0, 0.0, 2.0, 3.0]);
    assert_eq!(
        coords,
        vec![0.5, 1.5, 2.5, 3.5, 9.5, 19.5, 49.5, 59.5],
        "the box's corners are ordered top-left, bottom-right"
    );
    assert_eq!(prompt.tokens(), 4);
}

/// A decoder that records each call's batch and answers one mask per prompt whose logit is the
/// prompt's first x coordinate, with two candidates scored low then high.
struct Recording {
    calls: Mutex<Vec<Vec<u64>>>,
    model: ModelIdentity,
    placement: Placement,
}

impl Recording {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            model: ModelIdentity {
                id: "decoder".into(),
                version: "1".into(),
                sha256: "0".repeat(64),
            },
            placement: Placement {
                backend: "test".into(),
                device: Device::Cpu,
                precision: Precision::Fp32,
                partitions: None,
            },
        }
    }
}

impl Session for Recording {
    fn model(&self) -> &ModelIdentity {
        &self.model
    }

    fn placement(&self) -> &Placement {
        &self.placement
    }

    fn inputs(&self) -> &[TensorSpec] {
        &[]
    }

    fn outputs(&self) -> &[TensorSpec] {
        &[]
    }

    fn run(&self, inputs: &[Tensor], _: &Cancel) -> Result<Vec<Tensor>, crate::Error> {
        assert_eq!(inputs[0].shape(), EMBEDDING_SHAPE);
        let coords = &inputs[1];
        self.calls.lock().unwrap().push(coords.shape().to_vec());
        let (b, n) = (coords.shape()[0] as usize, coords.shape()[1] as usize);
        let values = coords.f32().unwrap();
        let mut masks = Vec::new();
        let mut scores = Vec::new();
        for i in 0..b {
            let x = values[i * n * 2];
            masks.extend(std::iter::repeat_n(x, 2 * 4 * 4));
            scores.extend([0.1, 0.9]);
        }
        Ok(vec![
            Tensor::new("masks", vec![b as u64, 2, 4, 4], TensorData::F32(masks))?,
            Tensor::new(
                "iou_predictions",
                vec![b as u64, 2],
                TensorData::F32(scores),
            )?,
        ])
    }
}

fn embedding() -> Embedding {
    let values = vec![0.0; EMBEDDING_SHAPE.iter().product::<u64>() as usize];
    let tensor = Tensor::new(
        "anything",
        EMBEDDING_SHAPE.to_vec(),
        TensorData::F32(values),
    )
    .unwrap();
    Embedding::new((64, 32), tensor).unwrap()
}

#[test]
fn decoding_batches_prompts_of_equal_length_and_answers_in_order() {
    let decoder = Recording::new();
    let mut embedding = embedding();
    let point = |x: f32| Prompt {
        positive: vec![Point::new(x, 1.0)],
        ..Prompt::default()
    };
    let boxed = Prompt {
        bbox: Some((Point::new(30.0, 1.0), Point::new(40.0, 9.0))),
        ..Prompt::default()
    };
    let prompts = [point(10.0), boxed, point(20.0)];
    let results =
        Segmenter::decode_many(&decoder, &mut embedding, &prompts, &Cancel::new()).unwrap();
    // One call of two single-point prompts, one of the two-corner box.
    assert_eq!(
        *decoder.calls.lock().unwrap(),
        vec![vec![2, 1, 2], vec![1, 2, 2]]
    );
    let first: Vec<f32> = results.iter().map(|c| c[0].logits.data[0]).collect();
    // Each mask holds its prompt's prompt-frame x (the 64 px input scales by 16 to 1024).
    assert_eq!(first, vec![159.5, 479.5, 319.5]);
    assert!(results.iter().all(|c| c[0].predicted_iou == 0.9));
    assert_eq!(results[0][0].logits.source, (64, 32));
    assert_eq!(
        results[0][0].logits.cells_per_px,
        (16.0 * 4.0 / 1024.0, 16.0 * 4.0 / 1024.0)
    );
    // The embedding is back for the next decode.
    assert_eq!(embedding.bytes(), 4 * 256 * 64 * 64);
}

#[test]
fn decoding_refuses_an_empty_prompt_and_embeddings_refuse_other_shapes() {
    let decoder = Recording::new();
    let error = Segmenter::decode_many(
        &decoder,
        &mut embedding(),
        &[Prompt::default()],
        &Cancel::new(),
    )
    .unwrap_err();
    assert_eq!(error.kind.code(), "validation");
    let wrong = Tensor::new("e", vec![1, 2], TensorData::F32(vec![0.0, 1.0])).unwrap();
    assert_eq!(
        Embedding::new((1, 1), wrong).unwrap_err().kind.code(),
        "incompatible"
    );
}
