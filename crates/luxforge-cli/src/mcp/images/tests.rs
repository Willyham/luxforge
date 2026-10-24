//! Pictures found through the markers alone, and the file bound.
use super::*;
use luxforge_testbase::paths::temp_dir;

#[test]
fn marked_fields_come_from_images_and_the_job_kinds_fields_each_path_once() {
    let preview_read = json!({"images": ["preview.path", "fallback.path"]});
    assert_eq!(
        marked(
            &preview_read,
            &json!({"preview": {"path": "/a.jpg"}, "fallback": {"path": "/a.jpg"}})
        ),
        [("preview.path".to_owned(), "/a.jpg".to_owned())]
    );
    assert_eq!(
        marked(
            &preview_read,
            &json!({"job_id": "j", "fallback": {"path": "/b.jpg"}})
        ),
        [("fallback.path".to_owned(), "/b.jpg".to_owned())]
    );
    assert!(marked(&preview_read, &json!({"job_id": "j"})).is_empty());
    let job_wait = json!({"job_images": {"kind": "job.kind",
                                         "fields": {"preview-render": ["job.result.path"]}}});
    assert_eq!(
        marked(
            &job_wait,
            &json!({"job": {"kind": "preview-render", "result": {"path": "/c.jpg"}}})
        ),
        [("job.result.path".to_owned(), "/c.jpg".to_owned())]
    );
    // An export's job names a path too, but its kind lists no picture.
    assert!(
        marked(
            &job_wait,
            &json!({"job": {"kind": "export", "result": {"path": "/out.jpg"}}})
        )
        .is_empty()
    );
    assert!(marked(&json!({"mutates": false}), &json!({"path": "/d.jpg"})).is_empty());
}

#[test]
fn a_picture_is_read_within_the_limit_and_otherwise_warned_about() {
    let dir = temp_dir("mcp-images");
    let small = dir.join("small.jpg");
    std::fs::write(&small, [0xff, 0xd8, 0xff, 0xd9]).unwrap();
    assert_eq!(
        read("preview.path", small.to_str().unwrap()).unwrap(),
        "/9j/2Q=="
    );
    let large = dir.join("large.jpg");
    std::fs::File::create(&large)
        .unwrap()
        .set_len(IMAGE_LIMIT + 1)
        .unwrap();
    let warning = read("preview.path", large.to_str().unwrap()).unwrap_err();
    assert_eq!(warning.code, IMAGE_TOO_LARGE);
    assert_eq!(warning.data.unwrap()["bytes"], json!(IMAGE_LIMIT + 1));
    let missing = read("preview.path", dir.join("gone.jpg").to_str().unwrap()).unwrap_err();
    assert_eq!(missing.code, IMAGE_UNREADABLE);
    let _ = std::fs::remove_dir_all(dir);
}
