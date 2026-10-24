//! The headless target without a child: where its binary is, which catalogs are the same, and a
//! binary that cannot start. The process tests (`tests/mcp/headless.rs`) run the real child.
use super::*;

#[test]
fn the_child_is_the_luxforge_json_beside_the_adapter() {
    let adapter = Path::new("/opt/luxforge/bin")
        .join(format!("luxforge-mcp{}", std::env::consts::EXE_SUFFIX));
    assert_eq!(
        beside(&adapter),
        Path::new("/opt/luxforge/bin")
            .join(format!("luxforge-json{}", std::env::consts::EXE_SUFFIX))
    );
}

#[test]
fn catalogs_are_the_same_by_path_or_by_the_file_they_resolve_to() {
    let dir = luxforge_testbase::paths::temp_dir("mcp-headless-same-catalog");
    let catalog = dir.join("catalog.sqlite");
    std::fs::write(&catalog, b"").unwrap();
    assert!(same_catalog(&catalog, &catalog));
    assert!(same_catalog(
        &dir.join(".").join("catalog.sqlite"),
        &catalog
    ));
    assert!(!same_catalog(&dir.join("other.sqlite"), &catalog));
    // Neither exists: only the same path is the same catalog.
    assert!(same_catalog(
        &dir.join("new.sqlite"),
        &dir.join("new.sqlite")
    ));
    assert!(!same_catalog(&dir.join("a.sqlite"), &dir.join("b.sqlite")));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_binary_that_cannot_start_is_headless_failed_and_names_it() {
    let dir = luxforge_testbase::paths::temp_dir("mcp-headless-missing-binary");
    let binary = dir.join("luxforge-json");
    let mut target = HeadlessTarget {
        catalog: dir.join("catalog.sqlite"),
        binary: binary.clone(),
        paths: None,
    };
    assert_eq!(target.list_changes(), ListChanges::OwnCalls);
    let Err(error) = target.attach("Claude Code via MCP") else {
        panic!("a missing binary started");
    };
    assert_eq!(error.code, HEADLESS_FAILED);
    assert!(
        error.message.contains(&binary.display().to_string()),
        "{error:?}"
    );
    assert_eq!(error.data.unwrap()["binary"], json!(binary));
    std::fs::remove_dir_all(dir).unwrap();
}
