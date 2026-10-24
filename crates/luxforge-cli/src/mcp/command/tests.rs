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
            headless: false,
            profile: Profile::Compact,
            images: true,
        }))
    );
    assert_eq!(
        parsed(&["--headless", "--catalog", "/c.sqlite"]),
        Ok(Parsed::Serve(Options {
            catalog: Some("/c.sqlite".into()),
            headless: true,
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
            headless: false,
            profile: Profile::Compact,
            images: false,
        }))
    );
    assert_eq!(parsed(&["--no-images", "--help"]), Ok(Parsed::Help));
    assert_eq!(parsed(&["--version"]), Ok(Parsed::Version));
}

#[test]
fn the_typed_profiles_offer_every_namespace_or_those_named() {
    let profile = |args: &[&str]| match parsed(args) {
        Ok(Parsed::Serve(options)) => options.profile,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        profile(&["--tools", "all"]),
        Profile::Typed(Namespaces::All)
    );
    assert_eq!(
        profile(&["--tools", "edit,asset"]),
        Profile::Typed(Namespaces::Only(vec!["edit".into(), "asset".into()]))
    );
    let unknown = parsed(&["--tools", "edit,nope"]).unwrap_err();
    assert!(unknown.contains("nope"), "{unknown}");
}

#[test]
fn unknown_or_repeated_flags_are_usage_errors() {
    for args in [
        &["--tools", "edit,nope"][..],
        &["--tools", ""],
        &["--tools", "all", "--tools", "compact"],
        &["--tools"],
        &["--catalog"],
        &["--catalog", "a", "--catalog", "b"],
        &["--no-images", "--no-images"],
        &["--headless"],
        &["--headless", "--no-images"],
        &["--headless", "--headless", "--catalog", "a"],
        &["config"],
    ] {
        assert!(parsed(args).is_err(), "{args:?}");
    }
    assert!(parsed(&["--headless"]).unwrap_err().contains("--catalog"));
}

#[test]
fn a_usage_error_exits_2_with_the_failure_on_standard_error() {
    let mut stderr = Vec::new();
    let status = run(
        ["--tools".into(), "nope".into()],
        &mut std::io::empty(),
        Box::new(std::io::sink()),
        &mut stderr,
        None,
    );
    assert_eq!(status, EXIT_USAGE);
    let error: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(error["error"]["code"], USAGE);
}
