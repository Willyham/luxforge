//! The command line's flags.
use super::*;

fn parsed(args: &[&str]) -> Result<Parsed, String> {
    parse(args.iter().map(Into::into))
}

#[test]
fn the_flags_choose_the_catalog_profile_and_pictures() {
    assert_eq!(
        parsed(&[]),
        Ok(Parsed::Serve(Options {
            catalog: None,
            profile: Profile::Compact,
            images: true,
        }))
    );
    assert_eq!(
        parsed(&[
            "--catalog",
            "/c.sqlite",
            "--tools",
            "compact",
            "--no-images"
        ]),
        Ok(Parsed::Serve(Options {
            catalog: Some("/c.sqlite".into()),
            profile: Profile::Compact,
            images: false,
        }))
    );
    assert_eq!(parsed(&["--no-images", "--help"]), Ok(Parsed::Help));
    assert_eq!(parsed(&["--version"]), Ok(Parsed::Version));
}

#[test]
fn typed_profiles_and_unknown_or_repeated_flags_are_usage_errors() {
    for args in [
        &["--tools", "all"][..],
        &["--tools", "edit,asset"],
        &["--tools"],
        &["--catalog"],
        &["--catalog", "a", "--catalog", "b"],
        &["--no-images", "--no-images"],
        &["--headless"],
        &["config"],
    ] {
        assert!(parsed(args).is_err(), "{args:?}");
    }
    assert!(
        parsed(&["--tools", "all"])
            .unwrap_err()
            .contains("--tools compact")
    );
}

#[test]
fn a_usage_error_exits_2_with_the_failure_on_standard_error() {
    let mut stderr = Vec::new();
    let status = run(
        ["--tools".into(), "all".into()],
        &mut std::io::empty(),
        Box::new(std::io::sink()),
        &mut stderr,
        None,
    );
    assert_eq!(status, EXIT_USAGE);
    let error: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(error["error"]["code"], USAGE);
}
