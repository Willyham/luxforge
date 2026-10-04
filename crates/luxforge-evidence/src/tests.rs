//! Every step kind parses from the shape a script writes and writes itself back in that same shape,
//! and a malformed step fails the whole script with an error that names it.
use super::*;

#[test]
fn pinch_steps_round_trip_and_reject_invalid_canvas_positions() {
    let step = Step::Pinch(PinchStep {
        delta: 1.2_f64.ln(),
        x: 0.37,
        y: 0.42,
    });
    let written = write(std::slice::from_ref(&step));
    assert_eq!(parse(&written.to_string()).unwrap(), vec![step]);
    for input in [
        r#"[{"pinch":{"delta":0.1,"x":-0.1,"y":0.5}}]"#,
        r#"[{"pinch":{"delta":0.1,"x":0.5,"y":1.1}}]"#,
        r#"[{"pinch":{"delta":0.1,"x":0.5,"y":0.5,"extra":true}}]"#,
    ] {
        assert!(parse(input).is_err());
    }
    assert!(
        Step::Pinch(PinchStep {
            delta: f64::INFINITY,
            x: 0.5,
            y: 0.5
        })
        .validate()
        .is_err()
    );
}

#[test]
fn mask_reapply_round_trips_as_the_notice_button_and_requires_true() {
    let step: Step = MaskStep::Reapply.into();
    let value = write(std::slice::from_ref(&step));
    assert_eq!(value, json!([{"mask":{"reapply":true}}]));
    assert_eq!(parse(&value.to_string()).unwrap(), vec![step]);
    assert!(
        parse(r#"[{"mask":{"reapply":false}}]"#)
            .unwrap_err()
            .contains("takes true")
    );
}

#[test]
fn query_choice_steps_round_trip_and_enforce_input_bounds() {
    let script = r#"[{"controls":{"gesture":"query-choice-search","action":"select-lens-profile","text":"Nikon"}},{"controls":{"gesture":"query-choice-page","action":"select-lens-profile","page":1}},{"controls":{"gesture":"query-choice-shared","action":"select-lens-profile","parameter":"focal","text":"35"}},{"controls":{"gesture":"query-choice-select-first","action":"select-lens-profile"}},{"controls":{"gesture":"query-choice-retry","action":"select-lens-profile"}},{"controls":{"gesture":"query-choice-apply","action":"select-lens-profile"}},{"controls":{"gesture":"query-choice-change","action":"select-lens-profile","open":true}},{"controls":{"gesture":"query-choice-report","action":"select-lens-profile"}}]"#;
    let steps = parse(script).unwrap();
    assert_eq!(steps.len(), 8);
    assert_eq!(
        serde_json::to_value(&steps).unwrap(),
        serde_json::from_str::<Value>(script).unwrap()
    );
    assert!(parse(r#"[{"controls":{"gesture":"query-choice-page","action":"select-lens-profile","page":100}}]"#).is_err());
    let module_rejected = serde_json::json!([{"controls":{"gesture":"query-choice-search","action":"select-lens-profile","text":"x".repeat(65)}}]);
    assert!(parse(&module_rejected.to_string()).is_ok());
    let excessive = serde_json::json!([{"controls":{"gesture":"query-choice-search","action":"select-lens-profile","text":"x".repeat(257)}}]);
    assert!(parse(&excessive.to_string()).is_err());
    assert!(parse(r#"[{"controls":{"gesture":"query-choice-retry","action":""}}]"#).is_err());
    assert!(parse(r#"[{"controls":{"gesture":"query-choice-apply","action":""}}]"#).is_err());
    assert!(
        parse(r#"[{"controls":{"gesture":"query-choice-change","action":"select-lens-profile"}}]"#)
            .is_err(),
        "Change names whether it opens or closes"
    );
    assert!(parse(r#"[{"controls":{"gesture":"query-choice-search","action":"select-lens-profile","text":"x\n"}}]"#).is_err());
}

#[test]
fn native_canvas_hover_round_trips_and_bounds_both_fractions() {
    let step = Step::canvas_hover(0.25, 0.75);
    assert_eq!(
        parse(&write(std::slice::from_ref(&step)).to_string()).unwrap(),
        vec![step]
    );
    for (x, y) in [(-0.1, 0.5), (1.1, 0.5), (0.5, -0.1), (0.5, 1.1)] {
        assert!(
            parse(&json!([{"canvas_hover":{"x":x,"y":y}}]).to_string())
                .unwrap_err()
                .contains("canvas_hover")
        );
    }
}

#[test]
fn native_hover_sweep_round_trips_and_bounds_its_work_and_duration() {
    let step = Step::CanvasHoverSweep {
        points: vec![[0.2, 0.3], [0.8, 0.7]],
        interval_ms: 16,
    };
    assert_eq!(
        parse(&write(std::slice::from_ref(&step)).to_string()).unwrap(),
        vec![step]
    );
    for step in [
        Step::CanvasHoverSweep {
            points: vec![],
            interval_ms: 16,
        },
        Step::CanvasHoverSweep {
            points: vec![[0.5, 0.5]; 241],
            interval_ms: 16,
        },
        Step::CanvasHoverSweep {
            points: vec![[0.5, 0.5]; 240],
            interval_ms: 1000,
        },
        Step::CanvasHoverSweep {
            points: vec![[1.1, 0.5]],
            interval_ms: 16,
        },
    ] {
        assert!(parse(&write(&[step]).to_string()).is_err());
    }
}

#[test]
fn idle_step_round_trips_and_bounds_its_windows() {
    let script = r#"[{"idle":{"settle_ms":500,"ms":1000}}]"#;
    let steps = parse(script).expect("a valid idle check");
    assert_eq!(
        steps,
        vec![Step::Idle(IdleStep {
            settle_ms: 500,
            ms: 1000
        })]
    );
    assert_eq!(parse(&write(&steps).to_string()).unwrap(), steps);
    assert!(parse(r#"[{"idle":{"settle_ms":0,"ms":1000}}]"#).is_err());
    assert!(parse(r#"[{"idle":{"settle_ms":500,"ms":20000}}]"#).is_err());
}

#[test]
fn view_idle_step_round_trips_and_bounds_its_deadline() {
    let script = r#"[{"view_idle":{"view":{"zoom":"fit"},"ms":1000}}]"#;
    let steps = parse(script).expect("a valid native idle probe");
    assert_eq!(
        steps,
        vec![Step::ViewIdle(ViewIdleStep {
            view: ViewStep::Fit,
            ms: 1000
        })]
    );
    assert_eq!(parse(&write(&steps).to_string()).unwrap(), steps);
    assert!(parse(r#"[{"view_idle":{"view":{"zoom":"fit"},"ms":0}}]"#).is_err());
}
use serde_json::json;

/// Parse `script`, and check every step writes itself back exactly as it was written.
fn round_trip(script: Value) -> Vec<Step> {
    let steps = parse(&script.to_string()).unwrap_or_else(|error| panic!("{script}: {error}"));
    assert_eq!(write(&steps), script);
    steps
}

/// `script` fails to parse with an error containing `expected`.
fn refused(script: Value, expected: &str) {
    let error = parse(&script.to_string()).expect_err(&script.to_string());
    assert!(error.contains(expected), "{script}: {error}");
}

#[test]
fn a_script_is_a_bounded_array_of_one_key_steps() {
    assert_eq!(parse("[]").unwrap(), Vec::new());
    for (script, expected) in [
        ("{}", "array of steps"),
        ("not json", "not JSON"),
        ("[{}]", "evidence script step 1: "),
        (r#"[{"wait":{"ms":1},"pan":{"x":0,"y":0}}]"#, "step 1: "),
        ("[5]", "step 1: "),
    ] {
        let error = parse(script).expect_err(script);
        assert!(error.contains(expected), "{script}: {error}");
    }
    let many = Value::Array(vec![json!({"draft":{"swap":true}}); MAX_SCRIPT_STEPS + 1]);
    refused(many, "at most 64");
    let most = Value::Array(vec![json!({"draft":{"swap":true}}); MAX_SCRIPT_STEPS]);
    assert_eq!(round_trip(most).len(), MAX_SCRIPT_STEPS);
}

/// The acceptance's deliberately broken steps: an unknown kind and a missing field each fail the
/// script, naming the step's position, its kind and what is wrong.
#[test]
fn an_unknown_kind_or_a_missing_field_fails_naming_the_step() {
    let error = parse(r#"[{"wait":{"ms":5}},{"zoom":"fit"}]"#).unwrap_err();
    assert!(
        error.starts_with("evidence script step 2 (zoom): unknown variant `zoom`, expected one of"),
        "{error}"
    );
    assert!(
        error.contains("`api`") && error.contains("`mask`"),
        "{error}"
    );
    let error = parse(r#"[{"slider":{"parameter":"exposure","values":[1]}}]"#).unwrap_err();
    assert_eq!(
        error,
        "evidence script step 1 (slider): missing field `action`"
    );
    let error = parse(r#"[{"pick":{"x":1}}]"#).unwrap_err();
    assert_eq!(error, "evidence script step 1 (pick): missing field `y`");
    let error =
        parse(r#"[{"field":{"action":"a","parameter":"b","text":"1","nowhere":1}}]"#).unwrap_err();
    assert!(
        error.starts_with("evidence script step 1 (field): unknown field `nowhere`"),
        "{error}"
    );
}

#[test]
fn api_draft_view_workspace_preview_palette_and_pick_round_trip() {
    let steps = round_trip(json!([
        {"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":0.0}}},
        {"api":{"method":"history.undo"}},
        {"draft":{"start":true}},
        {"draft":{"reapply":true}},
        {"draft":{"angle":7.5}},
        {"draft":{"nudge":-1}},
        {"draft":{"angle_rail":[0.25,0.75]}},
        {"draft":{"preset":"3:2"}},
        {"draft":{"rect":[10.0,20.0,300.0,200.0]}},
        {"draft":{"swap":true}},
        {"draft":{"lock":true}},
        {"draft":{"option":false}},
        {"draft":{"guide":true}},
        {"draft":{"apply":true}},
        {"draft":{"cancel":true}},
        {"view":{"zoom":"fit"}},
        {"view":{"zoom":250.0}},
        {"workspace":{"state_panel":false}},
        {"workspace":{"tools_panel":true,"thirds":true}},
        {"workspace":{"clip_shadows":true,"clip_highlights":false}},
        {"workspace":{"mode":"mask","mask_overlay":"mask-on-black","mask_overlay_colour":"red"}},
        {"preview":{"sequence":0}},
        {"preview":"current"},
        {"palette":{"query":"rotate"}},
        {"palette":{"run":"rotate"}},
        {"pick":{"x":120,"y":80}},
        {"wait":{"ms":1000}},
        {"pan":{"x":0.5,"y":1.0}},
        {"performance":{"expanded":true}},
        {"performance_cancel":{"row":0}},
        {"gallery":{"page":8}},
        {"gallery":{"page":null}},
        {"tools_scroll":1.0},
        {"agent":{"method":"edit.transform","params":{"transform":"rotate-right"}}},
        {"settings":{"open":true}},
        {"flag":{"id":"proof.number","value":75}},
        {"flag":{"id":"proof.choice","value":null}},
    ]));
    assert_eq!(steps[1], Step::api("history.undo"));
    assert_eq!(steps[2], Step::Draft(DraftStep::Start));
    assert_eq!(steps[4], Step::Draft(DraftStep::Angle(7.5)));
    assert_eq!(
        steps[8],
        Step::Draft(DraftStep::Rect([10., 20., 300., 200.]))
    );
    assert_eq!(steps[15], Step::View(ViewStep::Fit));
    assert_eq!(steps[16], Step::View(ViewStep::Percent(250.0)));
    assert_eq!(
        steps[18],
        WorkspaceStep::default()
            .tools_panel(true)
            .thirds(true)
            .into()
    );
    assert_eq!(steps[22], Step::Preview(PreviewStep::Current));
    assert_eq!(steps[25], Step::Pick(PickStep { x: 120, y: 80 }));
    assert_eq!(steps[31], Step::gallery(None));
    assert_eq!(
        steps[33],
        Step::agent("edit.transform", json!({"transform":"rotate-right"}))
    );
    assert_eq!(steps[34], Step::settings(true));
    assert_eq!(steps[35], Step::flag("proof.number", Some(json!(75))));
    assert_eq!(steps[36], Step::flag("proof.choice", None));
    // An integer where a number is expected is the same number.
    assert_eq!(
        parse(r#"[{"draft":{"rect":[10,20,300,200]}},{"view":{"zoom":100}}]"#).unwrap(),
        vec![
            Step::Draft(DraftStep::Rect([10., 20., 300., 200.])),
            Step::View(ViewStep::Percent(100.0))
        ]
    );

    for (script, expected) in [
        (json!({"api":{}}), "missing field `method`"),
        (
            json!({"api":{"method":" "}}),
            "api method takes a non-empty string",
        ),
        (
            json!({"api":{"method":"x","extra":1}}),
            "unknown field `extra`",
        ),
        (
            json!({"api":{"method":"x","params":[]}}),
            "invalid type: sequence",
        ),
        (
            json!({"api":{"method":"edit.crop","params":{"mutation":{}}}}),
            "may not set it",
        ),
        (
            json!({"api":{"method":"edit.crop","params":{"asset_id":"a"}}}),
            "may not set it",
        ),
        (json!({"agent":{}}), "missing field `method`"),
        (
            json!({"agent":{"method":""}}),
            "agent method takes a non-empty string",
        ),
        (
            json!({"agent":{"method":"edit.crop","params":{"mutation":{}}}}),
            "may not set it",
        ),
        (json!({"draft":{"start":false}}), "takes true"),
        (json!({"draft":{"angle":"7"}}), "invalid type: string \"7\""),
        (json!({"draft":{"nudge":2}}), "takes -1 or 1"),
        (
            json!({"draft":{"nudge":-0.5}}),
            "invalid type: floating point",
        ),
        (json!({"draft":{"rect":[1,2,0,4]}}), "positive extents"),
        (json!({"draft":{"rect":[1,2,3]}}), "invalid length 3"),
        (
            json!({"draft":{"angle_rail":[]}}),
            "one or more rail fractions",
        ),
        (
            json!({"draft":{"angle_rail":[1.5]}}),
            "one or more rail fractions",
        ),
        (
            json!({"draft":{"nowhere":true}}),
            "unknown variant `nowhere`",
        ),
        (json!({"view":{"zoom":2}}), "10 to 1600"),
        (json!({"view":{"zoom":"close"}}), "\"fit\" or a percentage"),
        (json!({"view":{"pan":1}}), "unknown field `pan`"),
        (json!({"workspace":{}}), "at least one of"),
        (
            json!({"workspace":{"mode":""}}),
            "workspace mode takes a non-empty",
        ),
        (
            json!({"workspace":{"nowhere":true}}),
            "unknown field `nowhere`",
        ),
        (
            json!({"workspace":{"clip_shadows":1}}),
            "expected a boolean",
        ),
        (json!({"preview":{"sequence":-1}}), "expected u64"),
        (json!({"preview":{"entry":1}}), "unknown variant `entry`"),
        (json!({"preview":true}), "invalid type: boolean"),
        (json!({"palette":{"query":1}}), "expected a string"),
        (
            json!({"palette":{"filter":"x"}}),
            "unknown variant `filter`",
        ),
        (json!({"pick":{"x":-1,"y":2}}), "expected u32"),
        (json!({"pick":{"x":1,"y":2,"z":3}}), "unknown field `z`"),
        (json!({"pick":5}), "invalid type: integer `5`"),
        (json!({"pick":{"x":120}}), "missing field `y`"),
        (json!({"pick":{"x":120,"y":-2}}), "expected u32"),
        (
            json!({"pick":{"x":1,"y":2,"mode":"m"}}),
            "unknown field `mode`",
        ),
        (json!({"wait":{"ms":0}}), "integer from 1 to 10000"),
        (json!({"wait":{"ms":10001}}), "integer from 1 to 10000"),
        (json!({"wait":{"ms":1.5}}), "expected u64"),
        (json!({"wait":{"seconds":1}}), "unknown field `seconds`"),
        (json!({"wait":5}), "invalid type: integer `5`"),
        (json!({"pan":{"x":0.5}}), "missing field `y`"),
        (json!({"pan":{"x":1.5,"y":0}}), "fraction from 0 to 1"),
        (json!({"pan":{"x":0,"y":0,"z":0}}), "unknown field `z`"),
        (json!({"performance":{}}), "missing field `expanded`"),
        (
            json!({"performance_cancel":{"row":4}}),
            "performance_cancel row must be below 4",
        ),
        (json!({"performance":{"expanded":1}}), "expected a boolean"),
        (
            json!({"performance":{"expanded":true,"module":"x"}}),
            "unknown field `module`",
        ),
        (json!({"performance":true}), "invalid type: boolean"),
        (json!({"settings":{}}), "missing field `open`"),
        (
            json!({"flag":{"id":"proof.number"}}),
            "missing field `value`",
        ),
        (
            json!({"flag":{"id":" ","value":true}}),
            "flag id takes a non-empty string",
        ),
        (json!({"gallery":{}}), "missing field `page`"),
        (json!({"gallery":{"page":-1}}), "expected usize"),
        (json!({"tools_scroll":1.5}), "fraction from 0 to 1"),
    ] {
        refused(json!([script]), expected);
    }
}

#[test]
fn slider_double_click_field_and_reset_steps_round_trip() {
    let steps = round_trip(json!([
        {"slider":{"action":"set-basic","parameter":"exposure","values":[0.25,0.5,0.75],"release":true,"cancel":false}},
        {"slider":{"action":"set-basic","parameter":"exposure","values":[1.0],"release":false,"cancel":true}},
        {"slider":{"action":"set-basic","parameter":"exposure","values":[0.1,0.2],"release":true,"cancel":false,"interval_ms":8}},
        {"slider_draft":"discard"},
        {"slider_draft":"reapply"},
        {"double_click":{"action":"set-raw","parameter":"temperature","value":5000.0,"gap_ms":120}},
        {"field":{"action":"set-basic","parameter":"exposure","text":"1.5","submit":true}},
        {"reset":{"module":"luxforge.basic"}},
        {"reset":{"module":"luxforge.basic","group":"Tone"}},
    ]));
    assert_eq!(
        steps[0],
        SliderStep::new("set-basic", "exposure", [0.25, 0.5, 0.75])
            .release()
            .into()
    );
    assert_eq!(
        steps[2],
        SliderStep::new("set-basic", "exposure", [0.1, 0.2])
            .release()
            .paced(8)
            .into()
    );
    assert_eq!(steps[6], Step::field("set-basic", "exposure", "1.5", true));
    assert_eq!(steps[7], Step::reset("luxforge.basic", None));
    // Left out, both flags are false: the gesture stays open.
    let open = parse(r#"[{"slider":{"action":"a","parameter":"p","values":[1]}}]"#).unwrap();
    assert_eq!(open, vec![SliderStep::new("a", "p", [1.0]).into()]);
    assert_eq!(
        open[0].to_value(),
        json!({"slider":{"action":"a","parameter":"p","values":[1.0],"release":false,"cancel":false}})
    );

    for (script, expected) in [
        (
            json!({"slider":{"action":"a","parameter":"b","values":[]}}),
            "at least one value",
        ),
        (
            json!({"slider":{"action":"a","parameter":"b","values":["1"]}}),
            "invalid type: string \"1\"",
        ),
        (
            json!({"slider":{"action":"a","parameter":"b","values":[1],"release":true,"cancel":true}}),
            "not both",
        ),
        (
            json!({"slider":{"action":"a","parameter":"b","values":[1],"nowhere":true}}),
            "unknown field `nowhere`",
        ),
        (
            json!({"slider":{"action":"a","parameter":"b","values":[1],"interval_ms":0}}),
            "positive integer",
        ),
        (
            json!({"slider":{"action":"a","parameter":"b","values":[1],"interval_ms":-8}}),
            "expected u64",
        ),
        (
            json!({"slider":{"action":"a","parameter":"b","values":[1],"interval_ms":8.5}}),
            "expected u64",
        ),
        (json!({"slider_draft":"apply"}), "unknown variant `apply`"),
        (
            json!({"double_click":{"action":"a","parameter":"p","value":1,"gap_ms":251}}),
            "from 0 to 250",
        ),
        (
            json!({"double_click":{"action":"a","parameter":"p","value":1}}),
            "missing field `gap_ms`",
        ),
        (
            json!({"double_click":{"action":"a","parameter":"p","gap_ms":0}}),
            "missing field `value`",
        ),
        (
            json!({"double_click":{"action":"a","parameter":"p","value":1,"gap_ms":0,"x":1}}),
            "unknown field `x`",
        ),
        (
            json!({"field":{"action":"a","parameter":"b"}}),
            "missing field `text`",
        ),
        (json!({"reset":{}}), "missing field `module`"),
        (
            json!({"reset":{"module":"m","nowhere":1}}),
            "unknown field `nowhere`",
        ),
        (json!({"reset":{"module":"m","group":""}}), "reset group"),
    ] {
        refused(json!([script]), expected);
    }
}

#[test]
fn generated_control_steps_round_trip_and_refuse_invalid_fractions_and_ambiguous_events() {
    let steps = round_trip(json!([
        {"section":{"module":"luxforge.controls","expanded":true}},
        {"group":{"module":"luxforge.controls","path":[0],"expanded":false}},
        {"tab":{"module":"luxforge.mixer","index":2}},
        {"controls":{"action":"set-controls","parameter":"amount","gesture":"slider","fractions":[0.25,0.75],"finish":"release"}},
        {"controls":{"action":"set-controls","parameter":"enabled","gesture":"discrete","value":true}},
        {"picker":{"action":"set-controls","parameter":"rgb","open":true,"finish":"open"}},
        {"picker":{"action":"set-controls","parameter":"rgb","hue":0.125,"plane":[0.75,0.875],"finish":"cancel"}},
        {"curve":{"action":"set-controls","parameter":"master","event":"move","index":1,"points":[[0.5,0.375]],"finish":"open"}},
        {"curve":{"action":"set-controls","parameter":"master","event":"add","point":[0.25,0.25]}},
        {"curve":{"action":"set-controls","parameter":"master","event":"remove","index":1}},
        {"curve":{"action":"set-controls","parameter":"master","event":"channel","index":1}},
    ]));
    assert!(matches!(
        steps[3],
        Step::Controls(ControlsStep::Slider {
            finish: SliderEnd::Release,
            ..
        })
    ));
    assert!(matches!(
        &steps[8],
        Step::Curve(CurveStep {
            event: CurveStepEvent::Add([0.25, 0.25]),
            ..
        })
    ));
    // A finish left out is an open gesture.
    assert_eq!(
        parse(r#"[{"picker":{"action":"a","parameter":"p","open":true}}]"#).unwrap()[0],
        steps[5].clone().with_action("a", "p")
    );

    for (script, expected) in [
        (
            json!({"controls":{"action":"a","parameter":"p","gesture":"slider","fractions":[1.1]}}),
            "fraction from 0 to 1",
        ),
        (
            json!({"controls":{"action":"a","parameter":"p","gesture":"slider","fractions":[]}}),
            "nonempty fractions",
        ),
        (
            json!({"controls":{"action":"a","parameter":"p","gesture":"slider","fractions":[0.5],"value":1}}),
            "unknown field `value`",
        ),
        (
            json!({"controls":{"action":"a","parameter":"p","gesture":"discrete","value":1,"finish":"release"}}),
            "unknown field `finish`",
        ),
        (
            json!({"controls":{"action":"a","parameter":"p","gesture":"discrete","value":null}}),
            "typed value",
        ),
        (
            json!({"controls":{"action":"a","parameter":"p","gesture":"drag"}}),
            "unknown variant `drag`",
        ),
        (
            json!({"picker":{"action":"a","parameter":"p","plane":[0.5,-0.1]}}),
            "fraction from 0 to 1",
        ),
        (
            json!({"picker":{"action":"a","parameter":"p"}}),
            "open, hue or plane",
        ),
        (
            json!({"picker":{"action":"a","parameter":"p","open":true,"finish":"release"}}),
            "without a drag",
        ),
        (
            json!({"picker":{"action":"a","parameter":"p","open":false,"hue":0.5}}),
            "while closing",
        ),
        (
            json!({"curve":{"action":"a","parameter":"p","event":"add","point":[0.5,0.5],"finish":"release"}}),
            "unknown field `finish`",
        ),
        (
            json!({"curve":{"action":"a","parameter":"p","event":"move","index":1,"points":[]}}),
            "nonempty points",
        ),
        (
            json!({"curve":{"action":"a","parameter":"p","event":"move","index":1,"point":[0.5,0.5]}}),
            "unknown field `point`",
        ),
        (
            json!({"curve":{"action":"a","parameter":"p","event":"bend"}}),
            "unknown variant `bend`",
        ),
        (
            json!({"group":{"module":"m","path":[],"expanded":true}}),
            "nonempty path",
        ),
        (
            json!({"group":{"module":"m","path":[-1],"expanded":true}}),
            "expected usize",
        ),
        (json!({"tab":{"module":"m","index":-1}}), "expected usize"),
        (
            json!({"section":{"module":"m"}}),
            "missing field `expanded`",
        ),
    ] {
        refused(json!([script]), expected);
    }
}

/// The Points disclosure and a typed coordinate are curve events of their own on the wire, `points`
/// and `type`, and neither takes a finish: opening the list is view state and a typed coordinate
/// commits on its Enter.
#[test]
fn curve_points_and_type_steps_round_trip_on_the_wire() {
    let steps = round_trip(json!([
        {"curve":{"action":"set-curve","parameter":"luminance","event":"points","open":true}},
        {"curve":{"action":"set-curve","parameter":"luminance","event":"type","index":2,"axis":1,"text":"0.8"}},
        {"curve":{"action":"set-curve","parameter":"luminance","event":"points","open":false}},
    ]));
    assert_eq!(
        steps[0],
        Step::Curve(CurveStep {
            action: "set-curve".into(),
            parameter: "luminance".into(),
            event: CurveStepEvent::Points(true),
            finish: SliderEnd::Open,
        })
    );
    assert_eq!(
        steps[1],
        Step::Curve(CurveStep {
            action: "set-curve".into(),
            parameter: "luminance".into(),
            event: CurveStepEvent::Type {
                index: 2,
                axis: 1,
                text: "0.8".into(),
            },
            finish: SliderEnd::Open,
        })
    );
    assert!(matches!(
        &steps[2],
        Step::Curve(CurveStep {
            event: CurveStepEvent::Points(false),
            ..
        })
    ));

    for (script, expected) in [
        (
            json!({"curve":{"action":"a","parameter":"p","event":"points"}}),
            "missing field `open`",
        ),
        (
            json!({"curve":{"action":"a","parameter":"p","event":"points","open":true,"finish":"release"}}),
            "unknown field `finish`",
        ),
        (
            json!({"curve":{"action":"a","parameter":"p","event":"type","index":1,"axis":0}}),
            "missing field `text`",
        ),
        (
            json!({"curve":{"action":"a","parameter":"p","event":"type","index":1,"axis":2,"text":"0.5"}}),
            "curve type axis",
        ),
        (
            json!({"curve":{"action":"a","parameter":"p","event":"type","index":1,"axis":0,"text":"0.5","finish":"release"}}),
            "unknown field `finish`",
        ),
    ] {
        refused(json!([script]), expected);
    }
}

#[test]
fn every_mask_verb_round_trips_its_script() {
    let steps = round_trip(json!([
        {"mask":{"select":0}},
        {"mask":{"select":{"name":"Mask 1"}}},
        {"mask":{"select":"01JA00000000000000000000"}},
        {"mask":{"select_component":1}},
        {"mask":{"select_component":null}},
        {"mask":{"hover":{"name":"Linear 1"}}},
        {"mask":{"hover":null}},
        {"mask":{"mode":"subtract"}},
        {"mask":{"new":"linear"}},
        {"mask":{"add":"radial"}},
        {"mask":{"paint":"new-mask"}},
        {"mask":{"paint":"new-brush"}},
        {"mask":{"paint":{"component":{"name":"Brush 1"}}}},
        {"mask":{"brush":{"size":0.08,"feather":50.0,"flow":100.0}}},
        {"mask":{"brush":{"erase":true,"erase_held":false,"limit_to_colour":true,"colour_refine":20.0}}},
        {"mask":{"brush":{"nudge":["size",-2.0]}}},
        {"mask":{"stroke":{"points":[[0.3,0.3],[0.4,0.35]],"release":true}}},
        {"mask":{"stroke":{"points":[[0.5,0.5]],"release":false,"interval_ms":24}}},
        {"mask":{"stroke":{"points":[[0.5,0.5]],"release":true,"interval_ms":24,"settle_between":true}}},
        {"mask":{"sweep":{"from":[0.5,0.2],"to":[0.5,0.8]}}},
        {"mask":{"release":true}},
        {"mask":{"drag":{"handle":"radius+x","points":[[0.4,0.4],[0.45,0.4]]}}},
        {"mask":{"drag":{"handle":"middle","points":[[0.5,0.5]],"release":false}}},
        {"mask":{"apply":true}},
        {"mask":{"cancel":true}},
        {"mask":{"pick":true}},
        {"mask":{"row":{"component":1,"mode":"intersect"}}},
        {"mask":{"row":{"component":{"name":"Radial 1"},"invert":true}}},
        {"mask":{"row":{"component":0,"index":2}}},
        {"mask":{"row":{"component":2,"delete":true}}},
        {"mask":{"row":{"component":0,"delete_stroke":1}}},
        {"mask":{"row":{"component":{"name":"Brush 1"},"delete_stroke":{"name":"Stroke 2"}}}},
        {"mask":{"row":{"component":0,"delete_stroke":"01JA0000000000000000000B"}}},
        {"mask":{"eye":{"name":"Foreground"}}},
        {"mask":{"menu":"new-mask"}},
        {"mask":{"menu":"add-component"}},
    ]));
    assert_eq!(steps[0], MaskStep::Select(Reference::Index(0)).into());
    assert_eq!(steps[1], MaskStep::Select(Reference::name("Mask 1")).into());
    assert_eq!(
        steps[2],
        MaskStep::Select(Reference::Id("01JA00000000000000000000".into())).into()
    );
    assert_eq!(steps[4], MaskStep::SelectComponent(None).into());
    assert_eq!(steps[6], MaskStep::Hover(None).into());
    assert_eq!(steps[10], MaskStep::Paint(PaintStep::NewMask).into());
    assert_eq!(
        steps[12],
        MaskStep::Paint(PaintStep::Component(Reference::name("Brush 1"))).into()
    );
    assert_eq!(
        steps[13],
        MaskStep::Brush(BrushStep {
            size: Some(0.08),
            feather: Some(50.0),
            flow: Some(100.0),
            ..BrushStep::default()
        })
        .into()
    );
    assert_eq!(
        steps[16],
        MaskStep::stroke([[0.3, 0.3], [0.4, 0.35]], true).into()
    );
    assert_eq!(
        steps[18],
        MaskStep::Stroke {
            points: vec![[0.5, 0.5]],
            release: true,
            interval_ms: Some(24),
            settle_between: true,
        }
        .into()
    );
    assert_eq!(
        steps[21],
        MaskStep::Drag {
            handle: DragHandle::RadiusPlusX,
            points: vec![[0.4, 0.4], [0.45, 0.4]],
            release: true,
        }
        .into()
    );
    assert_eq!(
        steps[22],
        MaskStep::Drag {
            handle: DragHandle::Middle,
            points: vec![[0.5, 0.5]],
            release: false,
        }
        .into()
    );
    assert_eq!(
        steps[30],
        MaskStep::Row(MaskRow::new(0, RowStep::DeleteStroke(Reference::Index(1)))).into()
    );
    assert_eq!(
        steps[33],
        MaskStep::Eye(Reference::name("Foreground")).into()
    );
    assert_eq!(steps[34], MaskStep::Menu(KindMenuStep::NewMask).into());
    assert_eq!(steps[35], MaskStep::Menu(KindMenuStep::AddComponent).into());
    // A stroke is released unless it says otherwise.
    assert_eq!(
        parse(r#"[{"mask":{"stroke":{"points":[[0.5,0.5]]}}}]"#).unwrap()[0],
        MaskStep::stroke([[0.5, 0.5]], true).into()
    );

    for (script, expected) in [
        (
            json!({"mask":{"nowhere":true}}),
            "unknown variant `nowhere`",
        ),
        (
            json!({"mask":{"select":0,"add":"linear"}}),
            "expected map with a single key",
        ),
        (json!({"mask":{"select":true}}), "position in the list"),
        (
            json!({"mask":{"select":{"id":"x"}}}),
            "position in the list",
        ),
        (json!({"mask":{"select":{"name":" "}}}), "non-empty string"),
        (json!({"mask":{"select":""}}), "non-empty string"),
        (json!({"mask":{"select":null}}), "position in the list"),
        (json!({"mask":{"apply":false}}), "takes true"),
        (json!({"mask":{"new":""}}), "component kind"),
        (
            json!({"mask":{"sweep":{"from":[0.5,0.2]}}}),
            "missing field `to`",
        ),
        (
            json!({"mask":{"sweep":{"from":[0,0],"to":[0,9]}}}),
            "-1 to 2",
        ),
        (
            json!({"mask":{"drag":{"handle":"corner","points":[[0.1,0.1]]}}}),
            "unknown variant `corner`",
        ),
        (
            json!({"mask":{"drag":{"handle":"start","points":[]}}}),
            "at least one point",
        ),
        (
            json!({"mask":{"row":{"mode":"add"}}}),
            "missing field `component`",
        ),
        (json!({"mask":{"row":{"component":0}}}), "one of mode"),
        (
            json!({"mask":{"row":{"component":0,"mode":"add","invert":true}}}),
            "one of mode",
        ),
        (
            json!({"mask":{"row":{"component":0,"delete":false}}}),
            "takes true",
        ),
        (
            json!({"mask":{"row":{"component":0,"colour":"red"}}}),
            "unknown field `colour`",
        ),
        (
            json!({"mask":{"paint":"new-shape"}}),
            "unknown variant `new-shape`",
        ),
        (
            json!({"mask":{"paint":{"kind":"brush"}}}),
            "unknown variant `kind`",
        ),
        (
            json!({"mask":{"paint":{"component":true}}}),
            "position in the list",
        ),
        (json!({"mask":{"brush":{}}}), "mask brush takes size"),
        (
            json!({"mask":{"brush":{"size":"big"}}}),
            "invalid type: string",
        ),
        (json!({"mask":{"brush":{"erase":1}}}), "expected a boolean"),
        (
            json!({"mask":{"brush":{"nudge":["size"]}}}),
            "invalid length 1",
        ),
        (
            json!({"mask":{"brush":{"opacity":1}}}),
            "unknown field `opacity`",
        ),
        // Density is deliberately absent, so naming one is refused rather than ignored.
        (
            json!({"mask":{"brush":{"density":50}}}),
            "unknown field `density`",
        ),
        (
            json!({"mask":{"stroke":{"points":[]}}}),
            "at least one point",
        ),
        (json!({"mask":{"stroke":{"points":[[0.5,9.0]]}}}), "-1 to 2"),
        (
            json!({"mask":{"stroke":{"points":[[0.5,0.5]],"interval_ms":0}}}),
            "positive integer",
        ),
        (
            json!({"mask":{"stroke":{"points":[[0.5,0.5]],"settle_between":true}}}),
            "settle_between needs interval_ms",
        ),
        (
            json!({"mask":{"stroke":{"points":[[0.5,0.5]],"speed":2}}}),
            "unknown field `speed`",
        ),
        (
            json!({"mask":{"row":{"component":0,"delete_stroke":true}}}),
            "position in the list",
        ),
        (json!({"mask":{"eye":null}}), "position in the list"),
        (json!({"mask":{"eye":{"name":""}}}), "non-empty string"),
        (json!({"mask":{"menu":"mask"}}), "unknown variant `mask`"),
        (json!({"mask":{"menu":true}}), "invalid type: boolean"),
    ] {
        refused(json!([script]), expected);
    }
}

#[test]
fn preset_steps_round_trip() {
    let steps = round_trip(json!([
        {"preset":{"name":"Warm"}},
        {"preset":{"name":"Warm","group":"Mine"}},
        {"preset_create":{"name":"Cool","groups":["Basic · Tone"]}},
        {"preset_create":{"name":"Cool","group":"Mine","groups":[],"submit":false}},
        {"preset_delete":{"name":"Cool","group":"Mine"}},
        {"preset_import":{"path":"fixtures/presets/develop.xmp"}},
    ]));
    assert_eq!(steps[0], Step::preset("Warm"));
    assert!(matches!(
        &steps[2],
        Step::PresetCreate(PresetCreateStep { submit: true, .. })
    ));
    assert_eq!(
        steps[5],
        Step::preset_import("fixtures/presets/develop.xmp")
    );
    for (script, expected) in [
        (json!({"preset":{}}), "missing field `name`"),
        (json!({"preset":{"name":""}}), "preset name"),
        (
            json!({"preset":{"name":"a","colour":"x"}}),
            "unknown field `colour`",
        ),
        (
            json!({"preset_create":{"name":"a"}}),
            "missing field `groups`",
        ),
        (
            json!({"preset_create":{"name":"a","groups":[" "]}}),
            "preset_create groups",
        ),
        (
            json!({"preset_create":{"name":"a","groups":[],"submit":"no"}}),
            "expected a boolean",
        ),
        (json!({"preset_import":{}}), "missing field `path`"),
    ] {
        refused(json!([script]), expected);
    }
}

#[test]
fn export_steps_round_trip_and_take_a_bare_file_name() {
    let steps = round_trip(json!([
        {"export":{"menu":true}},
        {"export":{"file":{"name":"a.jpg"}}},
        {"export":{"file":{"name":"b.jpeg","keep_metadata":true}}},
    ]));
    assert_eq!(steps[0], Step::export_menu());
    assert_eq!(steps[1], Step::export("a.jpg", false));
    assert_eq!(steps[2], Step::export("b.jpeg", true));
    for (script, expected) in [
        (json!({"export":{"menu":false}}), "takes true"),
        (json!({"export":{"file":{}}}), "missing field `name`"),
        (json!({"export":{"file":{"name":" "}}}), "export file name"),
        (
            json!({"export":{"file":{"name":"out/a.jpg"}}}),
            "not a path",
        ),
        (json!({"export":{"file":{"name":".."}}}), "not a path"),
        (
            json!({"export":{"file":{"name":"a.jpg","quality":90}}}),
            "unknown field `quality`",
        ),
    ] {
        refused(json!([script]), expected);
    }
}

#[test]
fn capability_steps_round_trip_and_an_api_step_is_kept_through_the_redactor() {
    let module = "luxforge.capabilities";
    let steps = round_trip(json!([
        {"capability": {"module": module, "task": {"task": "generate"}}},
        {"capability": {"module": module, "consent": "allow", "wait": false}},
        {"capability": {"module": module, "consent": "deny"}},
        {"capability": {"module": module, "apply": true}},
        {"capability": {"module": module, "settle": true}},
        {"api": {"method": "module.settings.set-secret", "params": {"module_id": module, "setting": "api-key", "value": "script-sentinel"}}},
    ]));
    assert_eq!(
        steps[1],
        CapabilityStep::new(module, CapabilityAction::Consent(true))
            .no_wait()
            .into()
    );
    // An `api` step's parameters are kept as the redactor leaves them, and every other step
    // exactly as written.
    let redact = |method: &str, params: &Value| {
        let mut params = params.clone();
        if method == "module.settings.set-secret" {
            params["value"] = json!("<redacted>");
        }
        params
    };
    assert_eq!(
        steps[5].kept(redact),
        json!({"api": {"method": "module.settings.set-secret", "params": {"module_id": module, "setting": "api-key", "value": "<redacted>"}}})
    );
    // An `agent` step's parameters too, although the desktop sends only edits through one.
    let agent = Step::agent(
        "module.settings.set-secret",
        json!({"module_id": module, "setting": "api-key", "value": "script-sentinel"}),
    );
    assert_eq!(
        agent.kept(redact),
        json!({"agent": {"method": "module.settings.set-secret", "params": {"module_id": module, "setting": "api-key", "value": "<redacted>"}}})
    );
    for step in &steps[..5] {
        assert_eq!(step.kept(redact), step.to_value());
    }

    for (script, expected) in [
        (json!({"capability": {"module": module}}), "exactly one of"),
        (
            json!({"capability": {"module": module, "apply": true, "settle": true}}),
            "exactly one of",
        ),
        (
            json!({"capability": {"consent": "allow"}}),
            "missing field `module`",
        ),
        (
            json!({"capability": {"module": module, "section": "settings"}}),
            "unknown field `section`",
        ),
        (
            json!({"capability": {"module": module, "consent": "maybe"}}),
            "unknown variant `maybe`",
        ),
        (
            json!({"capability": {"module": module, "apply": false}}),
            "takes true",
        ),
        (
            json!({"capability": {"module": module, "task": {"task": " "}}}),
            "capability task",
        ),
        (
            json!({"capability": {"module": module, "settle": true, "colour": 1}}),
            "unknown field `colour`",
        ),
    ] {
        refused(json!([script]), expected);
    }
}

#[test]
fn a_reference_in_request_parameters_is_read_as_a_step_names_one() {
    assert_eq!(
        Reference::from_value(json!({"name": "Mask 1"})),
        Ok(Reference::name("Mask 1"))
    );
    assert_eq!(Reference::from_value(json!(2)), Ok(Reference::Index(2)));
    assert_eq!(
        Reference::from_value(json!("01JA")),
        Ok(Reference::Id("01JA".into()))
    );
    assert!(Reference::from_value(json!({"name": ""})).is_err());
    assert!(Reference::from_value(json!(true)).is_err());
}

impl Step {
    /// The same picker step on another control, for comparing a step whose defaults were left out.
    fn with_action(self, action: &str, parameter: &str) -> Self {
        match self {
            Self::Picker(step) => Self::Picker(PickerStep {
                action: action.into(),
                parameter: parameter.into(),
                ..step
            }),
            other => other,
        }
    }
}

#[test]
fn every_select_step_round_trips_and_a_malformed_one_is_refused() {
    let steps = round_trip(json!([
        {"select": {"switch": "select"}},
        {"select": {"source": "Konstanz \u{b7} 12\u{2013}13 Sep"}},
        {"select": {"folder": "/Volumes/SSD/Pictures"}},
        {"select": {"arrow": {"direction": "right"}}},
        {"select": {"arrow": {"direction": "up", "extend": true}}},
        {"select": {"choose": {"menu": "group", "item": "Day"}}},
        {"select": {"choose": {"menu": "pick", "item": "Picked"}}},
        {"select": {"click": {"position": 5, "shift": true}}},
        {"select": {"click": {"position": 0, "command": true}}},
        {"select": {"agent_pick": {"positions": [5, 6]}}},
        {"select": {"agent_pick": {"positions": [5], "picked": false}}},
        {"select": {"switch": "develop"}},
        {"select": {"library": "undo"}},
        {"select": {"library": "redo"}},
        {"select": {"pick_all": {"position": 12}}},
        {"select": {"first_look": "/Volumes/SSD/Pictures"}},
        {"select": "continue_in_background"},
        {"select": "cancel_work"},
        {"select": {"add_folder": "/Volumes/SSD/Card dumps"}},
    ]));
    assert_eq!(
        steps[12],
        Step::Select(SelectStep::Library(LibraryKey::Undo))
    );
    assert_eq!(
        steps[14],
        Step::Select(SelectStep::PickAll { position: 12 })
    );
    assert_eq!(
        steps[15],
        Step::Select(SelectStep::FirstLook("/Volumes/SSD/Pictures".into()))
    );
    assert_eq!(steps[16], Step::Select(SelectStep::ContinueInBackground));
    assert_eq!(steps[17], Step::Select(SelectStep::CancelWork));
    assert_eq!(
        steps[18],
        Step::Select(SelectStep::AddFolder("/Volumes/SSD/Card dumps".into()))
    );
    assert_eq!(
        steps[3],
        Step::Select(SelectStep::Arrow {
            direction: ArrowKey::Right,
            extend: false
        })
    );
    assert_eq!(
        steps[9],
        Step::Select(SelectStep::AgentPick {
            positions: vec![5, 6],
            picked: true
        })
    );
    refused(json!([{"select": {"source": " "}}]), "select source");
    refused(json!([{"select": {"folder": ""}}]), "select folder");
    refused(
        json!([{"select": {"add_folder": " "}}]),
        "select add_folder",
    );
    refused(
        json!([{"select": {"first_look": " "}}]),
        "select first_look",
    );
    refused(
        json!([{"select": {"agent_pick": {"positions": []}}}]),
        "1 to 64 positions",
    );
    refused(
        json!([{"select": {"choose": {"menu": "lens", "item": "x"}}}]),
        "unknown variant `lens`",
    );
    refused(
        json!([{"select": {"switch": "library"}}]),
        "unknown variant",
    );
}

#[test]
fn loupe_steps_round_trip_and_bound_their_presses() {
    let steps = round_trip(json!([
        {"loupe": {"arrows": {"direction": "right", "count": 1}}},
        {"loupe": {"arrows": {"direction": "left", "count": 30, "interval_ms": 30}}},
        {"loupe": {"pointer": [0.25, 0.75]}},
    ]));
    assert_eq!(
        steps[0],
        Step::Loupe(LoupeStep::Arrows(LoupeArrows {
            direction: ArrowKey::Right,
            count: 1,
            interval_ms: None,
        }))
    );
    assert_eq!(
        steps[1],
        Step::Loupe(LoupeStep::Arrows(LoupeArrows {
            direction: ArrowKey::Left,
            count: 30,
            interval_ms: Some(30),
        }))
    );
    assert_eq!(steps[2], Step::Loupe(LoupeStep::Pointer([0.25, 0.75])));
    for (arrows, expected) in [
        (json!({"direction": "right", "count": 0}), "from 1 to 240"),
        (
            json!({"direction": "right", "count": 241, "interval_ms": 30}),
            "from 1 to 240",
        ),
        (
            json!({"direction": "right", "count": 1, "interval_ms": 30}),
            "one loupe arrow press takes no interval_ms",
        ),
        (
            json!({"direction": "right", "count": 2}),
            "need interval_ms from 1 to 1000",
        ),
        (
            json!({"direction": "right", "count": 2, "interval_ms": 0}),
            "need interval_ms from 1 to 1000",
        ),
    ] {
        refused(json!([{"loupe": {"arrows": arrows}}]), expected);
    }
    refused(
        json!([{"loupe": {"pointer": [1.5, 0.5]}}]),
        "loupe pointer x needs a finite fraction",
    );
}

#[test]
fn a_grid_scroll_round_trips_and_bounds_its_speed_and_frames() {
    let steps = round_trip(json!([{"grid_scroll": {"px_per_frame": 60.0, "frames": 240}}]));
    assert_eq!(
        steps[0],
        Step::GridScroll(GridScrollStep {
            px_per_frame: 60.0,
            frames: 240,
        })
    );
    refused(
        json!([{"grid_scroll": {"px_per_frame": 0.5, "frames": 240}}]),
        "px_per_frame takes a number from 1 to 2000",
    );
    refused(
        json!([{"grid_scroll": {"px_per_frame": 60.0, "frames": 1001}}]),
        "frames takes an integer from 1 to 1000",
    );
    refused(
        json!([{"grid_scroll": {"px_per_frame": 60.0}}]),
        "missing field `frames`",
    );
}
