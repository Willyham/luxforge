//! Merge kinds through the real owner (`docs/design/panorama-merge.md`, "Generalizing the merge
//! boundary"): the schemas list the kinds this build has a provider for, with each kind's options
//! under its name; a panorama request in a build without its stitch is planned but `not-ready` to
//! merge, naming the provider and why; another kind's options, a count outside the kind's bounds and malformed options are
//! `validation` naming their cause; a rerun's kind must be its merge's; and a merge's answers and
//! provenance are tagged by its kind.
use super::*;
use luxforge_test_macros::slow;

/// What `schema.list` declares for `method`'s field `name`.
fn declared(schema: &Value, method: &str, name: &str) -> Value {
    schema["methods"][method]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|parameter| parameter["name"] == name)
        .cloned()
        .unwrap_or(Value::Null)
}

#[test]
fn merge_kind_schemas_list_the_registered_kinds_and_their_options() {
    let bench = Bench::new("kind-schema", 1);
    let schema = bench.ok("schema.list", json!({}));
    for (method, required) in [
        ("merge.plan", true),
        ("merge.create", true),
        ("merge.rerun", false),
    ] {
        let kind = declared(&schema, method, "kind");
        assert_eq!(kind["kind"], "enum", "{method}: {kind}");
        assert_eq!(kind["options"], json!(["hdr", "panorama"]), "{method}");
        assert_eq!(kind["required"], required, "{method}");
        let hdr = declared(&schema, method, "hdr");
        assert_eq!(hdr["kind"], "json", "{method}: {hdr}");
        assert_eq!(hdr["required"], false, "{method}");
        for moved in ["reference", "exposure_overrides", "alignment", "deghost"] {
            assert!(
                hdr["notes"].as_str().unwrap().contains(moved),
                "{method}: hdr names {moved}"
            );
            assert_eq!(
                declared(&schema, method, moved),
                Value::Null,
                "{method}: {moved} is HDR's, under hdr"
            );
        }
        let panorama = declared(&schema, method, "panorama");
        assert_eq!(panorama["kind"], "json", "{method}: {panorama}");
        assert_eq!(panorama["required"], false, "{method}");
        for named in ["reference", "projection", "boundary_warp"] {
            assert!(
                panorama["notes"].as_str().unwrap().contains(named),
                "{method}: panorama names {named}"
            );
        }
    }
    for method in ["merge.preview", "merge.preview-update"] {
        for field in ["hdr", "panorama"] {
            let options = declared(&schema, method, field);
            assert_eq!(options["kind"], "json", "{method} {field}: {options}");
            assert_eq!(options["required"], false, "{method} {field}");
        }
    }
    // A request in the old shape names the field it no longer takes.
    let old = bench.failure(
        "merge.plan",
        json!({"kind": "hdr", "targets": {"kind": "paths", "paths": bench.paths(0)},
               "deghost": "off"}),
    );
    assert_eq!(old["code"], "validation");
    assert!(
        old["message"]
            .as_str()
            .unwrap()
            .contains("unknown field `deghost`"),
        "{old}"
    );
}

#[test]
#[slow]
fn merge_kind_panorama_without_its_stitch_plans_but_is_not_ready_to_merge() {
    let bench = Bench::new("kind-panorama", 1);
    // A build without the panorama stitch.
    bench.lane(|lane| lane.remove_panorama());
    let targets = json!({"kind": "paths", "paths": bench.paths(0)});
    let first = bench.merged(0, "first");
    // Planning reads headers alone and names the provider's state.
    let plan = bench.ok(
        "merge.plan",
        json!({"kind": "panorama", "targets": targets}),
    );
    assert_eq!(plan["kind"], "panorama", "{plan}");
    assert_eq!(plan["provider"]["module"], "luxforge.panorama");
    assert_eq!(plan["provider"]["version"], 1);
    assert_eq!(plan["provider"]["available"], false);
    assert_eq!(
        plan["provider"]["reason"],
        "this build has no panorama stitch"
    );
    for (method, params) in [
        (
            "merge.create",
            json!({"kind": "panorama", "targets": targets, "folder": {"kind": "existing", "folder_id": bench.folder},
                   "mutation": envelope("panorama")}),
        ),
        (
            "merge.preview",
            json!({"kind": "panorama", "targets": targets}),
        ),
    ] {
        let refused = bench.failure(method, params);
        assert_eq!(refused["code"], "not-ready", "{method}: {refused}");
        assert_eq!(
            refused["message"],
            "panorama merging is not available: this build has no panorama stitch",
            "{method}"
        );
        assert_eq!(refused["data"]["kind"], "panorama", "{method}");
        assert_eq!(refused["data"]["provider"], "luxforge.panorama", "{method}");
    }
    // A rerun's kind is its merge's.
    let rerun = bench.failure(
        "merge.rerun",
        json!({"merge_id": first["merge_id"], "kind": "panorama",
               "mutation": envelope("panorama-rerun")}),
    );
    assert_eq!(rerun["code"], "validation", "{rerun}");
    assert_eq!(
        rerun["data"],
        json!({"kind": "hdr", "requested": "panorama"})
    );
    // A kind that is none is the enumeration's to refuse.
    let unknown = bench.failure(
        "merge.plan",
        json!({"kind": "focus-stack", "targets": targets}),
    );
    assert_eq!(unknown["code"], "validation", "{unknown}");
    assert!(
        unknown["message"]
            .as_str()
            .unwrap()
            .starts_with("parameter kind must be one of hdr"),
        "{unknown}"
    );
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 1);
    assert!(bench.spools().is_empty());
}

#[test]
fn merge_kind_cross_kind_fields_counts_and_options_name_their_cause() {
    let bench = Bench::new("kind-fields", 1);
    let paths = bench.paths(0);
    let targets = json!({"kind": "paths", "paths": paths});
    // Another kind's options.
    let cross = bench.failure(
        "merge.plan",
        json!({"kind": "hdr", "targets": targets, "panorama": {"projection": "auto"}}),
    );
    assert_eq!(cross["code"], "validation", "{cross}");
    assert_eq!(
        cross["message"],
        "panorama holds panorama options, and this is an HDR merge"
    );
    assert_eq!(cross["data"], json!({"kind": "hdr", "field": "panorama"}));
    let cross = bench.failure(
        "merge.create",
        json!({"kind": "panorama", "targets": targets, "hdr": {"deghost": "off"},
               "folder": {"kind": "existing", "folder_id": bench.folder}, "mutation": envelope("cross")}),
    );
    assert_eq!(cross["code"], "validation", "{cross}");
    assert_eq!(cross["data"], json!({"kind": "panorama", "field": "hdr"}));
    // A field HDR's options do not have.
    let field = bench.failure(
        "merge.plan",
        json!({"kind": "hdr", "targets": targets, "hdr": {"projection": "auto"}}),
    );
    assert_eq!(field["code"], "validation", "{field}");
    assert!(
        field["message"]
            .as_str()
            .unwrap()
            .starts_with("hdr: unknown field `projection`"),
        "{field}"
    );
    assert_eq!(field["data"], json!({"kind": "hdr", "field": "hdr"}));
    // A count outside HDR's bounds: its typed refusal, naming the kind.
    let one = bench.failure(
        "merge.plan",
        json!({"kind": "hdr", "targets": {"kind": "paths", "paths": [paths[0]]}}),
    );
    assert_eq!(one["code"], "validation", "{one}");
    assert_eq!(one["message"], "an HDR merge takes 2 to 9 exposures, not 1");
    assert_eq!(
        one["data"],
        json!({"reason": "input-count", "count": 1, "kind": "hdr"})
    );
    let ten: Vec<PathBuf> = (0..10)
        .map(|at| bench.files[0].with_file_name(format!("frame-{at}.NEF")))
        .collect();
    let many = bench.failure(
        "merge.create",
        json!({"kind": "hdr", "targets": {"kind": "paths", "paths": ten},
               "folder": {"kind": "existing", "folder_id": bench.folder}, "mutation": envelope("ten")}),
    );
    assert_eq!(many["data"]["kind"], "hdr", "{many}");
    assert_eq!(many["data"]["count"], 10, "{many}");
    // HDR's own options, under hdr, plan and merge as before.
    let plan = bench.ok(
        "merge.plan",
        json!({"kind": "hdr", "targets": targets,
               "hdr": {"reference": paths[2], "alignment": "off", "deghost": "off"}}),
    );
    assert_eq!(plan["kind"], "hdr");
    assert_eq!(plan["reference"], 2, "{plan}");
    assert_eq!(
        plan["parameters"]["hdr"]["alignment"],
        json!({"alignment": "off", "deghost": "off"})
    );
    assert_eq!(plan["parameters"]["hdr"]["exposure"]["reference"], 2);
}

#[test]
#[slow]
fn merge_kind_answers_provenance_and_reruns_are_tagged_by_kind() {
    let bench = Bench::new("kind-tagged", 1);
    let result = bench.merged(0, "tagged");
    assert_eq!(result["kind"], "hdr", "{result}");
    assert_eq!(result["exposure_range_ev"].as_array().unwrap().len(), 2);
    let inspected = bench.ok("merge.inspect", json!({"merge_id": result["merge_id"]}));
    assert_eq!(inspected["kind"], "hdr");
    let provenance = &inspected["provenance"];
    assert_eq!(provenance["format"], 2);
    assert_eq!(provenance["kind"], "hdr");
    let keys =
        |value: &Value| -> Vec<String> { value.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(keys(&provenance["parameters"]), ["hdr"]);
    assert_eq!(keys(&provenance["resolved"]), ["hdr"]);
    assert_eq!(inspected["parameters"], provenance["parameters"]);
    // The stored provenance and its exact-match key are the kind-tagged shapes.
    let (kind, stored, key): (String, String, String) = bench.database(|connection| {
        connection.query_row(
            "SELECT kind, provenance_json, match_key FROM merges",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    });
    assert_eq!(kind, "hdr");
    let stored: MergeProvenance = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored.match_key().unwrap(), key);
    assert_eq!(stored.reference(), 1);
    let offsets: Vec<Option<f64>> = bench.database(|connection| {
        connection
            .prepare("SELECT offset_ev FROM merge_inputs ORDER BY ordinal")?
            .query_map([], |row| row.get(0))?
            .collect()
    });
    assert!(offsets.iter().all(Option::is_some), "{offsets:?}");
    // A rerun's kind, when named, is its merge's.
    let rerun = bench.ok(
        "merge.rerun",
        json!({"merge_id": result["merge_id"], "kind": "hdr", "hdr": {"deghost": "off"},
               "mutation": envelope("rerun-hdr")}),
    );
    let rerun = bench.settled(&rerun["job_id"]);
    assert_eq!(rerun["status"], "ready", "{rerun}");
    assert_eq!(rerun["kind"], "hdr-merge");
    assert_eq!(rerun["result"]["kind"], "hdr");
    let tagged = bench.ok(
        "merge.inspect",
        json!({"merge_id": rerun["result"]["merge_id"]}),
    );
    assert_eq!(tagged["parameters"]["hdr"]["alignment"]["deghost"], "off");
}

#[test]
#[slow]
fn merge_kind_provenance_validates_its_kind_and_bounds() {
    let bench = Bench::new("kind-validate", 1);
    let result = bench.merged(0, "validate");
    let inspected = bench.ok("merge.inspect", json!({"merge_id": result["merge_id"]}));
    let provenance: MergeProvenance =
        serde_json::from_value(inspected["provenance"].clone()).unwrap();
    provenance.validate().unwrap();
    let mismatched = MergeProvenance {
        kind: MergeKind::Panorama,
        ..provenance.clone()
    };
    let error = mismatched.validate().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert_eq!(
        error.detail,
        "a panorama merge records hdr parameters and a hdr resolution; both must be its own"
    );
    let mut one = provenance.clone();
    one.inputs.truncate(1);
    let error = one.validate().unwrap_err();
    assert_eq!(error.detail, "a hdr merge takes 2 to 9 inputs, not 1");
    assert_eq!(error.data.unwrap()["kind"], "hdr");
    let older = MergeProvenance {
        format: 1,
        ..provenance
    };
    assert_eq!(older.validate().unwrap_err().kind, ErrorKind::Incompatible);
}
