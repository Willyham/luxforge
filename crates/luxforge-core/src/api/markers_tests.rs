//! The markers `schema.list` carries for an agent's adapter: which methods only permission
//! authority may call, whether each method changes anything, whether a change can be taken back
//! and whether it can reach the network, and which answer fields name a picture written for
//! display. They are held
//! over the built-in and the developer registries, generated methods included, and the authority
//! marker is held to the check the catalog owner makes.
use super::*;
use crate::api::{ApiRequest, owner::OwnerHandle};
use std::collections::BTreeSet;

/// The methods only permission authority may call at all. A method that needs it for one argument
/// value, as `usage.set` does to opt in, is not one.
const PERMISSION_ONLY: &[&str] = &[
    "module.permission.grant",
    "usage.prompt.claim",
    "update.install",
    "update.move-to-applications",
    "catalog.empty-removed",
    "session.disconnect",
];

/// The host methods that change something without a mutation envelope: session state, user
/// preferences and flags, jobs, loaded models and the updater's record. Each was judged from
/// what its handler does, so a method joins or leaves this list deliberately, never by default.
const CHANGES_WITHOUT_ENVELOPE: &[&str] = &[
    "preferences.set",
    "flags.set",
    "update.check",
    "update.skip",
    "job.adopt",
    "job.cancel",
    "source.prepare",
    "module.runtime.load",
    "module.runtime.unload",
    "preview.select",
    "preview.compare",
    "preview.return-current",
    "view.set",
    "workspace.set",
    "session.identify",
    "session.disconnect",
    "draft.begin",
    "draft.set",
    "draft.cancel",
    "draft.reapply",
    "source.check",
    "merge.preview",
    "merge.preview-update",
    "merge.preview-close",
    "stack.set-expanded",
    "browse.view",
    "browse.select",
];

fn registries() -> [(&'static str, ModuleRegistry); 2] {
    [
        ("built-in", ModuleRegistry::builtin()),
        ("developer", ModuleRegistry::developer()),
    ]
}

/// Every entry of the method table declares whether it changes anything: a read, or a change that
/// can or cannot be taken back. One that declares none is refused by name, so a new operation
/// cannot inherit a default; a mutating entry cannot be a read, and only a change reaches the
/// network.
#[test]
fn every_host_method_declares_whether_it_reads_or_changes_and_mutations_change() {
    let undeclared: Vec<&str> = METHODS
        .iter()
        .filter(|spec| spec.effects.read_only().is_none())
        .map(|spec| spec.name)
        .collect();
    assert!(
        undeclared.is_empty(),
        "methods declaring neither read nor a change: {undeclared:?}"
    );
    let mutating_reads: Vec<&str> = METHODS
        .iter()
        .filter(|spec| Method::Host(spec).mutates() && spec.effects.read_only() == Some(true))
        .map(|spec| spec.name)
        .collect();
    assert!(
        mutating_reads.is_empty(),
        "mutating methods declared a read: {mutating_reads:?}"
    );
    let network_reads: Vec<&str> = METHODS
        .iter()
        .filter(|spec| spec.effects.network && spec.effects.read_only() != Some(false))
        .map(|spec| spec.name)
        .collect();
    assert!(
        network_reads.is_empty(),
        "reads declaring network: {network_reads:?}"
    );
    let changes: BTreeSet<&str> = METHODS
        .iter()
        .filter(|spec| !Method::Host(spec).mutates() && spec.effects.read_only() == Some(false))
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        changes,
        CHANGES_WITHOUT_ENVELOPE.iter().copied().collect(),
        "the host methods that change something without a mutation envelope"
    );
}

/// Every method `schema.list` lists, the generated `edit.*`, `query.*`, `mask.*` and `task.*`
/// included, carries a boolean `read_only`; every one that changes something carries a boolean
/// `reversible` and no read does; a mutating method is never a read; `network` appears only as
/// `true` and only on a change.
#[test]
fn schema_list_marks_every_method_read_only_or_reversible_in_both_registries() {
    for (label, registry) in registries() {
        let schema = schemas(&registry);
        let methods = schema["methods"].as_object().unwrap();
        let mut families = BTreeSet::new();
        for (name, entry) in methods {
            let read_only = entry["read_only"]
                .as_bool()
                .unwrap_or_else(|| panic!("{label}: {name} has no read_only"));
            if entry["mutates"] == true {
                assert!(!read_only, "{label}: {name} mutates and is read_only");
                families.insert(name.split('.').next().unwrap().to_owned());
            }
            if read_only {
                assert!(
                    entry.get("reversible").is_none(),
                    "{label}: the read {name} declares reversible"
                );
                assert!(
                    entry.get("network").is_none(),
                    "{label}: the read {name} declares network"
                );
            } else {
                assert!(
                    entry["reversible"].is_boolean(),
                    "{label}: {name} changes something without reversible"
                );
            }
            if let Some(network) = entry.get("network") {
                assert_eq!(network, &json!(true), "{label}: {name}");
            }
        }
        // Generated methods are covered, not only the table: actions and mask commands are
        // history entries, so history undo takes them back, and a query writes nothing.
        for family in ["edit", "mask"] {
            assert!(families.contains(family), "{label}: no mutating {family}.*");
        }
        for (name, entry) in methods {
            if name.starts_with("edit.") || (name.starts_with("mask.") && entry["mutates"] == true)
            {
                assert_eq!(entry["reversible"], json!(true), "{label}: {name}");
                assert!(entry.get("network").is_none(), "{label}: {name}");
            }
            if name.starts_with("query.") {
                assert_eq!(entry["mutates"], json!(false), "{label}: {name}");
                assert_eq!(entry["read_only"], json!(true), "{label}: {name}");
            }
            if name.starts_with("task.") {
                assert_eq!(entry["read_only"], json!(false), "{label}: {name}");
            }
        }
    }
}

/// A task reaches the network exactly when a capability it uses sends a request; its job adds
/// outputs and changes nothing that exists, so it is reversible. The developer registry's tasks
/// send nothing; the capabilities proof module's task sends a remote request.
#[test]
fn a_task_is_marked_from_the_capabilities_it_uses() {
    let mut proof = ModuleRegistry::developer();
    proof
        .register(std::sync::Arc::new(crate::CapabilitiesProofModule::new(
            "http://127.0.0.1:9",
        )))
        .unwrap();
    let mut remote_tasks = 0;
    for registry in [ModuleRegistry::developer(), proof] {
        let schema = schemas(&registry);
        let mut tasks = 0;
        for descriptor in registry.descriptors() {
            for task in &descriptor.tasks {
                tasks += 1;
                let entry = &schema["methods"][task_method(&task.id)];
                assert_eq!(entry["reversible"], json!(true), "{}", task.id);
                let remote =
                    descriptor.capabilities.iter().any(|capability| {
                        task.uses.contains(&capability.id)
                            && matches!(
                            capability.kind,
                            crate::capabilities::descriptor::CapabilityKind::RemoteImageRequest {
                                ..
                            }
                        )
                    });
                remote_tasks += usize::from(remote);
                assert_eq!(
                    entry.get("network") == Some(&json!(true)),
                    remote,
                    "{}",
                    task.id
                );
            }
        }
        assert!(tasks > 0, "the registry declares a task");
    }
    assert!(remote_tasks > 0, "a task that sends a request is exercised");
}

/// `authority: "permissions"` is on exactly the permission-only methods, in both registries, and
/// agrees with the catalog contracts' own declaration.
#[test]
fn the_authority_marker_names_exactly_the_permission_only_methods() {
    let expected: BTreeSet<&str> = PERMISSION_ONLY.iter().copied().collect();
    for (label, registry) in registries() {
        let schema = schemas(&registry);
        let marked: BTreeSet<&str> = schema["methods"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, entry)| entry.get("authority").is_some())
            .map(|(name, entry)| {
                assert_eq!(entry["authority"], json!("permissions"), "{label}: {name}");
                name.as_str()
            })
            .collect();
        assert_eq!(marked, expected, "{label}");
    }
    for contract in crate::catalog_types::api::CATALOG_METHODS {
        let spec = METHODS
            .iter()
            .find(|spec| spec.name == contract.name)
            .unwrap();
        assert_eq!(
            spec.permission_only.is_some(),
            contract.permission_authority,
            "{}",
            contract.name
        );
    }
}

/// The owner refuses an edit client every method the marker names, `forbidden` with the declared
/// refusal and before its handler runs or a retry is answered, and refuses a permission-authority client
/// none of them on authority; a method the marker does not name is never refused this way. So the
/// marker and the check are one declaration.
#[test]
fn the_owner_refuses_exactly_the_marked_methods_to_an_edit_client() {
    let catalog =
        std::env::temp_dir().join(format!("luxforge-markers-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&catalog);
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let edit = owner.register();
    let permissions = owner.register_with(ClientAuthority::Permissions);
    let call = |client, method: &str, request_id: &str| {
        owner
            .call(
                client,
                ApiRequest {
                    id: request_id.into(),
                    method: method.into(),
                    params: json!({"mutation": {"request_id": request_id, "actor": "test"}}),
                    token: None,
                },
            )
            .unwrap()
            .error
    };
    for spec in METHODS {
        let Some(refusal) = spec.permission_only else {
            continue;
        };
        let refused = call(edit, spec.name, &format!("edit-{}", spec.name))
            .unwrap_or_else(|| panic!("{} answered an edit client", spec.name));
        assert_eq!(
            (refused.code.as_str(), refused.message.as_str()),
            ("forbidden", refusal),
            "{}",
            spec.name
        );
        // A retry of the same request is refused the same way, never answered from a table.
        let retried = call(edit, spec.name, &format!("edit-{}", spec.name)).unwrap();
        assert_eq!(retried.code, "forbidden", "{}", spec.name);
        if let Some(error) = call(permissions, spec.name, &format!("admin-{}", spec.name)) {
            assert_ne!(error.code, "forbidden", "{}: {}", spec.name, error.message);
        }
    }
    // `usage.set` needs permission authority only to opt in, so it is not marked, and an edit
    // client's opt-out is not refused on authority.
    assert!(
        METHODS
            .iter()
            .all(|spec| spec.permission_only.is_none() || PERMISSION_ONLY.contains(&spec.name))
    );
    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(&catalog);
}

/// `preview.read` names its answer's picture fields, and `job.read` and `job.wait` name, per job
/// kind, the fields of a ready result that hold one: the preview jobs only, never an export's.
#[test]
fn images_are_marked_on_preview_answers_and_preview_job_results() {
    let schema = schemas(&ModuleRegistry::builtin());
    let methods = &schema["methods"];
    assert_eq!(
        methods["preview.read"]["images"],
        json!(["preview.path", "fallback.path"])
    );
    let catalog_kinds: BTreeSet<&str> = crate::catalog_types::jobs::CATALOG_JOBS
        .iter()
        .map(|job| job.job_kind)
        .collect();
    let preview_kinds = ["preview-extract", "preview-region", "preview-render"];
    for (method, record) in [("job.read", ""), ("job.wait", "job.")] {
        let marker = &methods[method]["job_images"];
        assert_eq!(marker["kind"], json!(format!("{record}kind")), "{method}");
        let fields = marker["fields"].as_object().unwrap();
        let kinds: BTreeSet<&str> = fields.keys().map(String::as_str).collect();
        assert_eq!(kinds, preview_kinds.into_iter().collect(), "{method}");
        for (kind, paths) in fields {
            assert!(catalog_kinds.contains(kind.as_str()), "{kind}");
            assert_eq!(paths, &json!([format!("{record}result.path")]), "{kind}");
        }
    }
    // Only the preview answers carry pictures; an export's path is the person's file.
    let marked: BTreeSet<&str> = methods
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, entry)| entry.get("images").is_some() || entry.get("job_images").is_some())
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        marked,
        ["job.read", "job.wait", "preview.read"]
            .into_iter()
            .collect()
    );
    assert!(!fields_name_export(&schema));
}

/// Whether any job-kind picture declaration names an export kind.
fn fields_name_export(schema: &Value) -> bool {
    ["job.read", "job.wait"].iter().any(|method| {
        schema["methods"][method]["job_images"]["fields"]
            .as_object()
            .is_some_and(|fields| fields.keys().any(|kind| kind.contains("export")))
    })
}
