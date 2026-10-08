//! Copy as JSON request copies exactly the request a control or the open crop draft would send, and
//! an analysis action's explanation is the report its own answer carried.
use super::{
    message::{action::ActionMessage, crop::CropMessage},
    testing::{finish, opened},
    *,
};

#[test]
fn copy_as_json_request_writes_what_the_control_would_send() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 5);
    let request = editor
        .request_for("crop-reset", None)
        .expect("a copyable request");
    assert_eq!(request["method"], json!("edit.crop-reset"));
    assert_eq!(request["params"]["asset_id"], json!(asset));
    assert_eq!(request["params"]["mutation"]["expected_revision"], json!(5));
    assert!(
        request["params"]["mutation"]["request_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("desktop-")),
        "{request}"
    );
    let _ = editor.update(Message::Action(ActionMessage::CopyRequest {
        action: "crop-reset".into(),
        parameter: None,
        preset: None,
    }));
    assert!(
        editor.status.text.contains("Copied"),
        "{}",
        editor.status.text
    );
    finish(editor, catalog);
}

/// The CropFrame section's Apply reads current pointer-composed values, not generated fields,
/// so it needs its own copy path rather than the generic `request_for`.
#[test]
fn copy_as_json_request_for_the_open_crop_draft_matches_its_own_apply() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 6);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let (method, request) = editor
        .crop_copy_request()
        .expect("a request")
        .expect("a valid draft");
    assert_eq!(method, "edit.crop");
    assert_eq!(request["asset_id"], json!(asset));
    assert_eq!(request["mutation"]["expected_revision"], json!(6));
    assert!(request.get("angle").is_some() && request.get("width").is_some());

    let _ = editor.update(Message::Action(ActionMessage::CopyDraftRequest));
    assert_eq!(editor.status.text, "Copied the edit.crop request");

    // With no draft open there is nothing to copy, and the status says so plainly.
    editor.gesture = None;
    let _ = editor.update(Message::Action(ActionMessage::CopyDraftRequest));
    assert_eq!(editor.status.text, "No crop draft to copy");
    finish(editor, catalog);
}

/// An analysis action's explanation is the report its own answer carried: it is in the tooltip's
/// place, pinned to the entry the command left current, as the command's read-back lands, with no
/// second message and no query of the analysis. An answer that carried none says so.
#[test]
fn an_analysis_actions_report_comes_with_its_own_answer() {
    let (mut editor, catalog, asset, shown) = opened(Vec::new(), 5);
    let report = serde_json::json!({"algorithm": "auto-tone/1", "summary": "Auto tone"});
    let analysed = |editor: &mut Editor, serial: u64, analysis: Option<&serde_json::Value>| {
        let committed = super::testing::entry(&asset, 6 + serial, Some(&shown));
        let mut refresh = super::testing::refresh_for(
            &asset,
            &committed,
            vec![committed.clone()],
            &[&committed],
            false,
        );
        if let Some(analysis) = analysis {
            refresh
                .analysis
                .insert("auto-tone".into(), analysis.clone());
        }
        editor.controls.ui.analysis_serial = serial;
        editor.controls.ui.analysis_pending = Some("auto-tone".into());
        let _ = editor.update(Message::Action(ActionMessage::Analysed {
            action: "auto-tone".into(),
            query: "auto-tone".into(),
            serial,
            result: Ok(Box::new(refresh)),
        }));
        assert_eq!(editor.controls.ui.analysis_pending, None);
        committed.id
    };
    let committed = analysed(&mut editor, 1, Some(&report));
    assert_eq!(
        editor.controls.ui.analysis_reports.get("auto-tone"),
        Some(&(asset.clone(), committed, report)),
        "the report is in place as the read-back lands"
    );
    editor.controls.ui.analysis_reports.clear();
    analysed(&mut editor, 2, None);
    assert!(
        editor.controls.ui.analysis_reports.is_empty(),
        "nothing is asked again for a report the answer did not carry"
    );
    finish(editor, catalog);
}
