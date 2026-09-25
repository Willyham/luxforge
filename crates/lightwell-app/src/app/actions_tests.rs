//! Copy as JSON request copies exactly the request a control or the open crop draft would send.
use super::{
    message::{ActionMessage, CropMessage},
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
    assert!(editor.status.contains("Copied"), "{}", editor.status);
    finish(editor, catalog);
}

/// The CropFrame section's Apply reads current pointer-composed values, not generated fields,
/// so it needs its own copy path rather than the generic `request_for`.
#[test]
fn copy_as_json_request_for_the_open_crop_draft_matches_its_own_apply() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 6);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    editor.open_draft(lightwell_core::CropStage {
        width: 480,
        height: 320,
        angle: 0.0,
    });
    let (method, request, _) = editor
        .crop_request()
        .expect("a request")
        .expect("a valid draft");
    assert_eq!(method, "edit.crop");
    assert_eq!(request["asset_id"], json!(asset));
    assert_eq!(request["mutation"]["expected_revision"], json!(6));
    assert!(request.get("angle").is_some() && request.get("width").is_some());

    let _ = editor.update(Message::Action(ActionMessage::CopyDraftRequest));
    assert_eq!(editor.status, "Copied the edit.crop request");

    // With no draft open there is nothing to copy, and the status says so plainly.
    editor.set_crop(None);
    let _ = editor.update(Message::Action(ActionMessage::CopyDraftRequest));
    assert_eq!(editor.status, "No crop draft to copy");
    finish(editor, catalog);
}
