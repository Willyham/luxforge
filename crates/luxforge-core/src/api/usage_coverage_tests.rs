//! The usage statistics coverage inventory accounts for every registered command: each host
//! method, generated module action, query and task, and mask command is in its table with an event
//! or an exemption, and the table names nothing that is not registered
//! (`docs/engineering/usage-statistics-coverage.md`).
use crate::{ModuleRegistry, usage::Feature};
use std::collections::BTreeSet;

const INVENTORY: &str = include_str!("../../../../docs/engineering/usage-statistics-coverage.md");

/// The commands the inventory's "Registered commands" table lists, with their event cells.
fn inventoried() -> Vec<(String, String)> {
    let table = INVENTORY
        .split("## Registered commands")
        .nth(1)
        .expect("the inventory has a Registered commands section");
    table
        .lines()
        .filter_map(|line| {
            let mut cells = line.strip_prefix("| `")?.splitn(2, "` |");
            let name = cells.next()?.to_owned();
            let event = cells.next()?.split(" | ").next()?.trim().to_owned();
            Some((name, event))
        })
        .collect()
}

#[test]
fn usage_every_registered_command_is_in_the_coverage_inventory() {
    let registered: BTreeSet<String> = super::schemas(&ModuleRegistry::builtin())["methods"]
        .as_object()
        .expect("schema.list lists its methods")
        .keys()
        .cloned()
        .collect();
    let rows = inventoried();
    let listed: BTreeSet<String> = rows.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(rows.len(), listed.len(), "a command is listed twice");
    let missing: Vec<_> = registered.difference(&listed).collect();
    let stale: Vec<_> = listed.difference(&registered).collect();
    assert!(
        missing.is_empty(),
        "registered commands missing from the usage coverage inventory: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "the inventory lists commands that are not registered: {stale:?}"
    );
    for (name, event) in &rows {
        assert!(
            event.starts_with("Exempt (read)")
                || event.starts_with("Exempt (internal)")
                || event.starts_with("Exempt (privacy)")
                || event.starts_with('`'),
            "{name}: an event or an exemption, not {event:?}"
        );
    }
    // Statistics never count themselves.
    for (name, event) in rows.iter().filter(|(name, _)| name.starts_with("usage.")) {
        assert!(event.starts_with("Exempt"), "{name} records {event}");
    }
}

/// Every interaction the registry defines has a producer the inventory names.
#[test]
fn usage_every_registry_feature_has_a_producer_in_the_inventory() {
    for feature in Feature::ALL {
        let named = format!("`{}`", feature.as_str());
        let bare = format!(" {} ", feature.as_str());
        assert!(
            INVENTORY.contains(&named) || INVENTORY.contains(&bare),
            "{} has no producer in the inventory",
            feature.as_str()
        );
    }
}

/// The host methods the owner records interactions for are inventoried with the same feature.
#[test]
fn usage_the_owners_interaction_table_agrees_with_the_inventory() {
    let rows = inventoried();
    for (name, event) in &rows {
        if let Some((feature, _)) =
            crate::api::owner::usage_events::host_feature(name, &serde_json::Value::Null)
        {
            assert!(
                event.contains(feature.as_str()),
                "{name} records {} but the inventory says {event}",
                feature.as_str()
            );
        }
    }
}

/// A panorama merge is an explicit exemption, recorded in the inventory, and never counted as an
/// HDR merge; an HDR merge, named or rerun without a kind, is `hdr-merge` as before.
#[test]
fn merge_kind_usage_exempts_panorama_merges_and_counts_hdr_merges() {
    use crate::api::owner::usage_events::host_feature;
    use serde_json::json;
    for method in ["merge.create", "merge.rerun"] {
        assert!(
            host_feature(method, &json!({"kind": "panorama"})).is_none(),
            "{method}"
        );
        for params in [json!({"kind": "hdr"}), json!({})] {
            let (feature, _) = host_feature(method, &params).expect(method);
            assert_eq!(feature, Feature::HdrMerge, "{method} {params}");
        }
        let (_, event) = inventoried()
            .into_iter()
            .find(|(name, _)| name == method)
            .expect(method);
        assert!(event.contains("hdr-merge"), "{method}: {event}");
        let row = INVENTORY
            .lines()
            .find(|line| line.starts_with(&format!("| `{method}` |")))
            .unwrap();
        assert!(row.contains("panorama merge is exempt"), "{row}");
    }
}
