//! The DNG correction reference's fixture, `fixtures/raw-dng-reference.json`: the supplied DJI
//! Air 2S DNG's correction and calibration metadata, synthetic vectors and 18 sparse camera-plane
//! references, all computed by `luxforge_reference::dng`. `luxforge-raw`'s tests check production
//! against it.
//!
//! The owner's DNG and the sparse dump taken from it stay private, so the fixture keeps no gain-map
//! values and no photograph data. `committed_dng_reference_matches_the_reference` (not ignored)
//! recomputes every section the fixture's own recorded warp coefficients, calibration tags and
//! geometry determine. `dng_reference_regenerates_the_fixture` (ignored) regenerates the whole
//! document from the original and the dump and requires it to equal the committed one value for
//! value:
//!
//! ```sh
//! LUXFORGE_DNG_SOURCE=/path/to/mavic_air_2s.DNG \
//! LUXFORGE_DNG_REFERENCE_DUMP=/path/to/ignored/sparse.csv \
//!   cargo test --release --locked -p luxforge-raw --lib dump_dji_sparse_uncorrected_reference -- --ignored
//! LUXFORGE_DNG_SOURCE=/path/to/mavic_air_2s.DNG \
//! LUXFORGE_DNG_REFERENCE_DUMP=/path/to/ignored/sparse.csv \
//!   cargo test -p luxforge-reference --test studies dng:: -- --ignored
//! ```
//!
//! Set `LUXFORGE_DNG_REFERENCE_JSON` to a new path as well to write the regenerated document
//! there for review.

use luxforge_reference::dng::{
    COLOR_STAGE_ORDERING, ColorStage, Dng, Field, GainMap, Opcode, Payload, Source, SparseSample,
    Warp, WhiteBalance, compensated_sum, gain_interpolate, maximum, parse, parse_sparse,
    sdk_bicubic_weights, sparse_reference, strict_clip, warp_source_position,
    warp_source_position_exact_identity, warp_source_position_spec_endpoints, wb_dual_calibration,
    wb_fixed_cm2,
};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, fs, path::PathBuf};

fn fixture_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/luxforge-reference; fixtures/ is repo-root-level.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/raw-dng-reference.json")
}

fn committed() -> Value {
    serde_json::from_slice(&fs::read(fixture_path()).expect("read the committed DNG reference"))
        .expect("parse the committed DNG reference")
}

// ---------------------------------------------------------------------------------------------
// The document's sections.
// ---------------------------------------------------------------------------------------------

fn source_json(source: Source) -> Value {
    match source {
        Source::Exact(position) => json!(position),
        Source::Mapped(position) => json!(position),
    }
}

fn swapped(source: Source) -> Source {
    match source {
        Source::Exact([a, b]) => Source::Exact([b, a]),
        Source::Mapped([a, b]) => Source::Mapped([b, a]),
    }
}

fn field_json(field: &Field) -> Value {
    match field {
        Field::Longs(values) => json!(values),
        Field::Rationals(pairs) => json!(pairs),
    }
}

fn raw_fields_json(fields: &BTreeMap<u16, Field>) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(tag, field)| (tag.to_string(), field_json(field)))
            .collect(),
    )
}

fn color_stage_json(stage: &ColorStage) -> Value {
    json!({
        "unique_camera_model": stage.unique_camera_model,
        "calibration_illuminants": stage.calibration_illuminants,
        "analog_balance": stage.analog_balance,
        "as_shot_neutral": stage.as_shot_neutral,
        "as_shot_neutral_values": stage.as_shot_neutral_values(),
        "as_shot_green_normalized_sensor_gains": stage.as_shot_green_normalized_sensor_gains(),
        "color_matrix1": stage.color_matrix1,
        "color_matrix2": stage.color_matrix2,
        "ordering": COLOR_STAGE_ORDERING,
    })
}

/// An opcode without its gain-map values, which stay out of the committed fixture.
fn opcode_json(op: &Opcode) -> Value {
    let mut value = json!({
        "index": op.index, "id": op.id, "version": op.version, "flags": op.flags,
        "byte_count": op.byte_count,
    });
    let fields = value.as_object_mut().unwrap();
    match &op.payload {
        Payload::GainMap(gain) => {
            fields.insert("area".into(), json!(gain.area));
            fields.insert("points".into(), json!(gain.points));
            fields.insert("spacing".into(), json!(gain.spacing));
            fields.insert("origin".into(), json!(gain.origin));
            fields.insert("planes".into(), json!(gain.planes));
        }
        Payload::Warp(warp) => {
            fields.insert("planes".into(), json!(warp.planes));
            fields.insert("coefficients".into(), json!(warp.coefficients));
            fields.insert("center".into(), json!(warp.center));
        }
        Payload::Other => {}
    }
    value
}

fn matrix_json(matrix: &[f64; 9]) -> Value {
    json!([&matrix[0..3], &matrix[3..6], &matrix[6..9]])
}

fn wb_json(wb: &WhiteBalance) -> Value {
    let mut value = json!({
        "temperature_kelvin": wb.temperature_kelvin,
        "tint_luxforge_units": wb.tint,
        "whitepoint_xy": wb.whitepoint_xy,
        "whitepoint_xyz_y1": wb.whitepoint_xyz,
        "xyz_to_camera_matrix": matrix_json(&wb.matrix),
        "camera_white_response": wb.camera_white_response,
        "green_normalized_sensor_gains": wb.green_normalized_sensor_gains,
    });
    let fields = value.as_object_mut().unwrap();
    match wb.matrix2_weight {
        Some(weight) => {
            fields.insert("production_reference".into(), json!(false));
            fields.insert(
                "selection".into(),
                json!("inverse-CCT dual-calibration interpolation comparator"),
            );
            fields.insert(
                "calibration_illuminants_cct_kelvin".into(),
                json!(luxforge_reference::dng::CALIBRATION_CCT_KELVIN),
            );
            fields.insert("matrix2_inverse_cct_weight".into(), json!(weight));
        }
        None => {
            let valid = wb.valid_for_gain_contract();
            fields.insert("production_reference".into(), json!(true));
            fields.insert(
                "selection".into(),
                json!("fixed ColorMatrix2 D65 XYZ-to-camera"),
            );
            fields.insert("valid_for_luxforge_gain_contract".into(), json!(valid));
            fields.insert(
                "validation_reason".into(),
                json!(if valid {
                    "within positive finite 0..32 gain range"
                } else {
                    "rejected: a sensor gain exceeds the 0..32 contract"
                }),
            );
        }
    }
    value
}

/// The white-balance references: the production fixed-D65 policy at 5500 K/+10 and at the
/// 2000 K/+100 endpoint, and the dual-calibration comparator at 5500 K/+10. The short key follows
/// the production policy.
fn wb_sections(stage: &ColorStage) -> Map<String, Value> {
    let fixed = wb_json(&wb_fixed_cm2(stage, 5_500.0, 10.0));
    Map::from_iter([
        (
            "wb_reference_5500k_tint10_dual_calibration_comparator".into(),
            wb_json(&wb_dual_calibration(stage, 5_500.0, 10.0)),
        ),
        ("wb_reference_5500k_tint10_fixed_cm2".into(), fixed.clone()),
        (
            "wb_reference_2000k_tint100_fixed_cm2".into(),
            wb_json(&wb_fixed_cm2(stage, 2_000.0, 100.0)),
        ),
        ("wb_reference_5500k_tint10".into(), fixed),
    ])
}

/// The active area, its size and the coordinate conventions, and the default and best-quality
/// final sizes, from the raw IFD's fields.
fn geometry_sections(dng_fields: &BTreeMap<u16, Field>, active: [i64; 4]) -> Map<String, Value> {
    let rationals = |tag: u16| match &dng_fields[&tag] {
        Field::Rationals(pairs) => pairs.clone(),
        Field::Longs(_) => panic!("tag {tag} is not rational"),
    };
    let size = [active[2] - active[0], active[3] - active[1]];
    let (crop, scale, best) = (rationals(50720), rationals(50718), rationals(50780)[0]);
    let ratio = |[n, d]: [u32; 2]| f64::from(n) / f64::from(d);
    let default_final =
        [0, 1].map(|i| ratio(crop[i]) * f64::from(scale[i][0]) / f64::from(scale[i][1]));
    let best_final = default_final.map(|side| side * f64::from(best[0]) / f64::from(best[1]));
    Map::from_iter([
        ("active_area".into(), json!(active)),
        ("active_area_size".into(), json!(size)),
        (
            "coordinate_space".into(),
            json!({
                "active_area_raw_tlbr": active,
                "active_local_bounds_tlbr": [0, 0, size[0], size[1]],
                "opcode_area_is_active_local": true,
                "raw_to_active_local": "(row - active_top, col - active_left)",
            }),
        ),
        ("default_final_size".into(), json!(default_final)),
        ("best_quality_final_size".into(), json!(best_final)),
    ])
}

/// The warp samples, over the gain map's area bounds: SDK source positions on each plane, a
/// non-unit pixel aspect (the supplied file has square pixels), a non-central centre (the
/// authentic [0.5, 0.5] would hide an x/y swap), identity exactness and the specification's
/// literal endpoints.
fn warp_samples(warp: &Warp, bounds: [i64; 4]) -> Map<String, Value> {
    let sdk = |warp: &Warp, row: i64, col: i64, scale: f64, plane: usize| {
        warp_source_position(warp, row as f64, col as f64, bounds, scale, plane)
    };
    let noncentral = Warp {
        center: [0.25, 0.75],
        ..warp.clone()
    };
    let warp_source: Vec<Value> = [
        (0, 0, 0),
        (1000, 1000, 0),
        (1000, 1000, 1),
        (1000, 1000, 2),
        (1824, 2736, 1),
        (3647, 5471, 2),
    ]
    .into_iter()
    .map(|(row, col, plane)| {
        json!({"row": row, "col": col, "plane": plane, "value": sdk(warp, row, col, 1.0, plane)})
    })
    .collect();
    let identity: Vec<Value> = [(4, 4), (1000, 1000), (3643, 5467)]
        .into_iter()
        .map(|(row, col)| {
            let value = warp_source_position_exact_identity(warp, row, col, bounds, 1.0, 1);
            json!({"row": row, "col": col, "plane": 1, "value": source_json(value)})
        })
        .collect();
    let literal: Vec<Value> = [(4, 4, 0), (850, 1304, 0), (2900, 4574, 2), (3643, 5467, 2)]
        .into_iter()
        .map(|(row, col, plane)| {
            let value = warp_source_position_spec_endpoints(
                warp, row as f64, col as f64, bounds, 1.0, plane,
            );
            json!({"row": row, "col": col, "plane": plane, "value": value})
        })
        .collect();
    Map::from_iter([
        ("warp_source".into(), json!(warp_source)),
        (
            "warp_nonunit_pixel_scale".into(),
            json!({"pixel_scale_v": 1.25, "row": 1000, "col": 1000, "plane": 0,
                "value": sdk(warp, 1000, 1000, 1.25, 0)}),
        ),
        (
            "warp_noncentral_center".into(),
            json!({"center_xy": [0.25, 0.75], "pixel_scale_v": 1.0, "row": 1000, "col": 1000,
                "plane": 0, "value": sdk(&noncentral, 1000, 1000, 1.0, 0)}),
        ),
        (
            "warp_identity_exactness".into(),
            json!({"policy": "identity coefficient planes return destination coordinates exactly and skip resampling",
                "samples": identity}),
        ),
        (
            "warp_spec_literal_endpoints".into(),
            json!({"convention": "x1/y1 are bottom-right pixel coordinates (DNG 1.7.1 prose)",
                "samples": literal}),
        ),
    ])
}

/// The synthetic vectors: a 2×2 gain map over square bounds away from the origin, and a bicubic
/// reconstruction whose float headroom and strict per-opcode clipping differ.
fn synthetic_samples() -> Map<String, Value> {
    let square = GainMap {
        area: [10, 20, 14, 24, 0, 1, 1, 1],
        points: [2, 2],
        spacing: [0.5, 0.5],
        origin: [0.0, 0.0],
        planes: 1,
        values: vec![1.0, 2.0, 3.0, 4.0],
    };
    let square_samples: Vec<Value> = [(10, 20), (11, 21), (13, 23)]
        .into_iter()
        .map(|(row, col)| {
            json!({"row": row, "col": col,
                "value": gain_interpolate(&square, row, col, 0, [10, 20, 14, 24])})
        })
        .collect();
    let source: [f64; 16] = [
        -0.25, 0.0, 0.0, 0.0, 0.0, 0.2, 0.8, 0.0, 0.0, 0.8, 1.2, 0.0, 0.0, 0.0, 0.0, 1.5,
    ];
    let weights = sdk_bicubic_weights(64, 64);
    let unclipped = compensated_sum(weights.iter().zip(source).map(|(w, v)| w * v));
    let strict = strict_clip(compensated_sum(
        weights.iter().zip(source).map(|(w, v)| w * strict_clip(v)),
    ));
    Map::from_iter([
        (
            "synthetic_gain_square_nonzero_origin".into(),
            json!({"bounds_tlbr": [10, 20, 14, 24], "points_vh": [2, 2],
                "spacing_vh": [0.5, 0.5], "origin_vh": [0.0, 0.0],
                "values": [[[1.0], [2.0]], [[3.0], [4.0]]], "samples": square_samples}),
        ),
        (
            "synthetic_bicubic_headroom".into(),
            json!({"phase_128_yx": [64, 64],
                "source_4x4": source.chunks(4).collect::<Vec<_>>(),
                "unclipped_bicubic_value": unclipped, "strict_dng_clipped_value": strict,
                "negative_scalar_unclipped": -0.25, "negative_scalar_strict_dng_clipped": 0.0}),
        ),
    ])
}

/// The samples that read the gain map's values: map points, interpolated gains (plane 3 repeats
/// the last plane), edge replication outside the opcode's area, and the clipping contract.
fn gain_samples(gain: &GainMap, bounds: [i64; 4]) -> Map<String, Value> {
    let planes = gain.planes as usize;
    let grid: Vec<Value> = [(0, 0), (0, 31), (16, 16), (31, 0), (31, 31)]
        .into_iter()
        .map(|(row, col)| {
            let value: Vec<f64> = (0..planes).map(|p| gain.value(row, col, p)).collect();
            json!({"map_row": row, "map_col": col, "value": value})
        })
        .collect();
    let at = |samples: &[(i64, i64, usize)]| -> Vec<Value> {
        samples
            .iter()
            .map(|&(row, col, plane)| {
                json!({"row": row, "col": col, "plane": plane,
                    "value": gain_interpolate(gain, row, col, plane, bounds)})
            })
            .collect()
    };
    let first = gain.value(0, 0, 0);
    Map::from_iter([
        ("gain_grid".into(), json!(grid)),
        (
            "gain".into(),
            json!(at(&[
                (0, 0, 0),
                (1824, 2736, 1),
                (3647, 5471, 2),
                (100, 5000, 3)
            ])),
        ),
        (
            "gain_edge_replication".into(),
            json!(at(&[(-1, -1, 0), (3648, 5472, 2)])),
        ),
        (
            "clipping_contract".into(),
            json!({"opcode_list_2_3_logical_range": [0.0, 1.0], "gain_input": 0.3,
                "gain": first, "float_headroom_product": 0.3 * first,
                "strict_dng_clipped_product": strict_clip(0.3 * first),
                "proposed_float_headroom_is_equal_only_when_product_is_in_range": true}),
        ),
    ])
}

fn sparse_json(samples: &[SparseSample]) -> Value {
    let rows: Vec<Value> = samples
        .iter()
        .map(|s| {
            let [raw_x, raw_y] = s.out_raw_xy;
            json!({
                "out_raw_xy": [raw_x, raw_y],
                "plane": s.plane,
                "source_raw_yx": source_json(s.source_raw),
                "source_raw_xy": source_json(swapped(s.source_raw)),
                "mapped_csv_source_raw_xy": s.mapped_csv_source_raw_xy,
                "max_abs_error_to_mapped_csv_pixels": s.max_abs_error_to_mapped_csv_pixels,
                "source_active_local_yx": source_json(s.source_active_local),
                "phase_128_yx": s.phase_128,
                "reference_float_headroom": s.reference_float_headroom,
                "reference_strict_dng_clipped": s.reference_strict_dng_clipped,
                "production_sparse_csv_value": s.production_sparse_csv_value,
                "abs_error_to_csv_value": s.abs_error_to_csv_value,
            })
        })
        .collect();
    json!({
        "provenance": "independent f32 GainMap + Adobe SDK bicubic phase-128 evaluation of an ignored 8x8 sparse adapter dump",
        "count": samples.len(),
        "max_abs_error_to_csv_value": maximum(samples.iter().map(|s| s.abs_error_to_csv_value)),
        "max_abs_error_to_mapped_csv_pixels":
            maximum(samples.iter().map(|s| s.max_abs_error_to_mapped_csv_pixels)),
        "samples": rows,
    })
}

/// The largest coordinate difference between the SDK's exclusive bounds and the specification's
/// literal bottom-right pixel, over the sparse samples' output pixels.
fn endpoint_comparison(
    outputs: impl IntoIterator<Item = ([i64; 2], usize)>,
    warp: &Warp,
    bounds: [i64; 4],
    active: [i64; 4],
) -> Value {
    let deltas = outputs.into_iter().map(|([out_x, out_y], plane)| {
        let (row, col) = ((out_y - active[0]) as f64, (out_x - active[1]) as f64);
        let sdk = warp_source_position(warp, row, col, bounds, 1.0, plane);
        let literal = warp_source_position_spec_endpoints(warp, row, col, bounds, 1.0, plane);
        maximum([0, 1].map(|i| (sdk[i] - literal[i]).abs())).unwrap()
    });
    json!({
        "sdk_uses_exclusive_dng_rect_right_bottom": true,
        "spec_literal_uses_bottom_right_pixel_coordinates": true,
        "sparse_max_abs_coordinate_delta_pixels": maximum(deltas),
    })
}

/// The whole document, from the original and, where given, its sparse dump.
fn document(source_file: &str, dng: &Dng, sparse: Option<&str>) -> Value {
    let gain = dng.gain_map().expect("the DNG has a GainMap");
    let warp = dng.warp().expect("the DNG has a WarpRectilinear");
    // Opcode area bounds are in active-area-local pixel coordinates, not the default crop's.
    let bounds = gain.bounds();
    let active = dng.active_area().expect("the DNG's active area");
    let mut samples = gain_samples(gain, bounds);
    samples.extend(warp_samples(warp, bounds));
    samples.extend(synthetic_samples());
    let mut document = Map::from_iter([
        ("source_file".into(), json!(source_file)),
        ("file_bytes".into(), json!(dng.file_bytes)),
        ("raw_ifd".into(), json!(dng.raw_ifd)),
        ("opcode_list3_offset".into(), json!(dng.opcode_list3_offset)),
        ("opcode_list3_bytes".into(), json!(dng.opcode_list3_bytes)),
        ("raw_fields".into(), raw_fields_json(&dng.raw_fields)),
        ("color_stage".into(), color_stage_json(&dng.color_stage)),
        ("opcode_count".into(), json!(dng.opcode_count)),
        (
            "opcodes".into(),
            json!(dng.opcodes.iter().map(opcode_json).collect::<Vec<_>>()),
        ),
        ("reference_samples".into(), Value::Object(samples)),
    ]);
    document.extend(geometry_sections(&dng.raw_fields, active));
    document.extend(wb_sections(&dng.color_stage));
    if let Some(csv) = sparse {
        let dump = parse_sparse(csv).expect("read the sparse dump");
        let sparse = sparse_reference(&dump, gain, warp, active).expect("the sparse reference");
        document.insert("sparse_reference".into(), sparse_json(&sparse));
        document.insert(
            "warp_endpoint_comparison".into(),
            endpoint_comparison(
                sparse.iter().map(|s| (s.out_raw_xy, s.plane)),
                warp,
                bounds,
                active,
            ),
        );
    }
    Value::Object(document)
}

// ---------------------------------------------------------------------------------------------
// Reading the recorded parameters back from the committed fixture.
// ---------------------------------------------------------------------------------------------

fn pairs<T: serde::de::DeserializeOwned>(value: &Value) -> Vec<[T; 2]> {
    serde_json::from_value(value.clone()).expect("rational pairs")
}

fn recorded_stage(fixture: &Value) -> ColorStage {
    let stage = &fixture["color_stage"];
    ColorStage {
        unique_camera_model: stage["unique_camera_model"].as_str().unwrap().into(),
        calibration_illuminants: serde_json::from_value(stage["calibration_illuminants"].clone())
            .unwrap(),
        analog_balance: pairs(&stage["analog_balance"]),
        as_shot_neutral: pairs(&stage["as_shot_neutral"]),
        color_matrix1: pairs(&stage["color_matrix1"]),
        color_matrix2: pairs(&stage["color_matrix2"]),
    }
}

fn recorded_opcode(fixture: &Value, id: u64) -> &Value {
    fixture["opcodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["id"] == id)
        .expect("the recorded opcode")
}

fn recorded_warp(fixture: &Value) -> Warp {
    let warp = recorded_opcode(fixture, 1);
    Warp {
        planes: warp["planes"].as_u64().unwrap() as u32,
        coefficients: serde_json::from_value(warp["coefficients"].clone()).unwrap(),
        center: serde_json::from_value(warp["center"].clone()).unwrap(),
    }
}

fn recorded_fields(fixture: &Value) -> BTreeMap<u16, Field> {
    fixture["raw_fields"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(tag, value)| {
            let field = if value[0].is_array() {
                Field::Rationals(pairs(value))
            } else {
                Field::Longs(serde_json::from_value(value.clone()).unwrap())
            };
            (tag.parse().unwrap(), field)
        })
        .collect()
}

/// Every path at which `actual` and `expected` differ, `expected` read as the fixture.
fn differences(path: &str, actual: &Value, expected: &Value, out: &mut Vec<String>) {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            for key in a.keys().chain(e.keys().filter(|k| !a.contains_key(*k))) {
                let (a, e) = (a.get(key), e.get(key));
                match (a, e) {
                    (Some(a), Some(e)) => differences(&format!("{path}.{key}"), a, e, out),
                    _ => out.push(format!("{path}.{key}: {a:?} vs fixture {e:?}")),
                }
            }
        }
        (Value::Array(a), Value::Array(e)) if a.len() == e.len() => {
            for (i, (a, e)) in a.iter().zip(e).enumerate() {
                differences(&format!("{path}[{i}]"), a, e, out);
            }
        }
        _ if actual != expected => out.push(format!("{path}: {actual} vs fixture {expected}")),
        _ => {}
    }
}

fn assert_same(actual: &Value, expected: &Value) {
    let mut out = Vec::new();
    differences("$", actual, expected, &mut out);
    assert!(
        out.is_empty(),
        "differs from the fixture:\n{}",
        out.join("\n")
    );
}

// ---------------------------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------------------------

/// Every section the fixture's own recorded parameters determine, recomputed and compared value
/// for value, integer and float kinds included: the colour stage's derived values and white
/// balances, the geometry, the warp samples and the synthetic vectors.
#[test]
fn committed_dng_reference_matches_the_reference() {
    let fixture = committed();
    let stage = recorded_stage(&fixture);
    assert_same(&color_stage_json(&stage), &fixture["color_stage"]);
    for (key, value) in wb_sections(&stage) {
        assert_same(&value, &fixture[&key]);
    }
    let fields = recorded_fields(&fixture);
    assert_same(&raw_fields_json(&fields), &fixture["raw_fields"]);
    let active: [i64; 4] = serde_json::from_value(fixture["active_area"].clone()).unwrap();
    for (key, value) in geometry_sections(&fields, active) {
        assert_same(&value, &fixture[&key]);
    }
    let area: Vec<i64> =
        serde_json::from_value(recorded_opcode(&fixture, 9)["area"].clone()).unwrap();
    let bounds = [area[0], area[1], area[2], area[3]];
    let samples = &fixture["reference_samples"];
    for (key, value) in warp_samples(&recorded_warp(&fixture), bounds)
        .into_iter()
        .chain(synthetic_samples())
    {
        assert_same(&value, &samples[&key]);
    }
    // The endpoint conventions' largest difference over the sparse samples' output pixels.
    let outputs: Vec<([i64; 2], usize)> = fixture["sparse_reference"]["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let out = serde_json::from_value(s["out_raw_xy"].clone()).unwrap();
            (out, s["plane"].as_u64().unwrap() as usize)
        })
        .collect();
    assert_eq!(outputs.len(), 18);
    assert_same(
        &endpoint_comparison(outputs, &recorded_warp(&fixture), bounds, active),
        &fixture["warp_endpoint_comparison"],
    );
}

/// The whole fixture, regenerated from the private original and its sparse dump, equals the
/// committed one value for value.
#[test]
#[ignore = "requires the owner's DJI Air 2S DNG and a sparse dump taken from it"]
fn dng_reference_regenerates_the_fixture() {
    let source = PathBuf::from(std::env::var("LUXFORGE_DNG_SOURCE").expect("DNG source path"));
    let dump = std::env::var("LUXFORGE_DNG_REFERENCE_DUMP").expect("sparse dump path");
    let bytes = fs::read(&source).expect("read the DNG");
    let dng = parse(&bytes).expect("read the DNG's correction metadata");
    let csv = fs::read_to_string(dump).expect("read the sparse dump");
    let name = source.file_name().unwrap().to_str().unwrap();
    let regenerated = document(name, &dng, Some(&csv));
    if let Ok(path) = std::env::var("LUXFORGE_DNG_REFERENCE_JSON") {
        let text = serde_json::to_string_pretty(&regenerated).unwrap() + "\n";
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes()))
            .expect("write the regenerated reference to a new path");
    }
    assert_same(&regenerated, &committed());
}
