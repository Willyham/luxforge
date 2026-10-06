//! The session's `renderer`: which renderer draws the desktop's picture, the owner's one value in
//! every client's session, which the host reports through a desktop-internal path and no method
//! sets (`docs/design/gpu-first.md`, "Headless and portability").
use super::*;
use crate::{HostConfig, ModuleRegistry, Renderer, RendererReason};

fn owner_with(name: &str, renderer: Renderer) -> (OwnerHandle, JoinHandle<()>, PathBuf) {
    let catalog = luxforge_testbase::paths::temp_path(&format!("renderer-{name}.sqlite"));
    let (owner, join) = OwnerHandle::start_with_host(
        &catalog,
        Arc::new(ModuleRegistry::builtin()),
        HostConfig {
            renderer,
            ..HostConfig::unconfigured()
        },
    )
    .unwrap();
    (owner, join, catalog)
}

fn call(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> ApiResponse {
    owner
        .call(
            client,
            ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .unwrap()
}

/// `client`'s session as `session.state` answers it.
fn state(owner: &OwnerHandle, client: ClientId) -> Value {
    call(owner, client, "session.state", json!({}))
        .result
        .expect("session.state answers")
}

fn stop(owner: OwnerHandle, join: JoinHandle<()>, catalog: PathBuf) {
    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(catalog);
}

/// An owner that draws nothing — `luxforge-json`'s, any host but the desktop's — reports the
/// reference renderer with no reason, in every client's session.
#[test]
fn session_renderer_of_an_owner_that_draws_nothing_is_the_reference_with_no_reason() {
    let catalog = luxforge_testbase::paths::temp_path("renderer-headless.sqlite");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let edit = owner.register();
    let permitted = owner.register_with(ClientAuthority::Permissions);
    for client in [edit, permitted] {
        assert_eq!(
            state(&owner, client)["renderer"],
            json!({"record": "reference", "reason": null})
        );
    }
    stop(owner, join, catalog);
}

/// The desktop's launch says what it knows before its window opens, and every client reads it; the
/// host's report changes it in every session, touching each that held another value, so a client
/// keeping its newest session sees the change; a report of the same value touches nothing. A client
/// that registers later reads the current value. No method sets it: `workspace.set` refuses the
/// field and changes nothing.
#[test]
fn session_renderer_is_the_owners_one_value_reported_by_the_host_and_set_by_no_method() {
    let (owner, join, catalog) = owner_with(
        "reported",
        Renderer::reference(RendererReason::SurfacePending),
    );
    let desktop = owner.register_with(ClientAuthority::Permissions);
    let agent = owner.register();
    let before = state(&owner, agent);
    assert_eq!(
        before["renderer"],
        json!({"record": "reference", "reason": "surface-pending"})
    );
    assert_eq!(state(&owner, desktop)["renderer"], before["renderer"]);

    // An agent cannot claim a renderer: the field is no parameter of any method.
    let refused = call(
        &owner,
        agent,
        "workspace.set",
        json!({"renderer": {"record": "gpu", "reason": null}}),
    );
    assert!(refused.result.is_none(), "{refused:?}");
    assert_eq!(
        state(&owner, agent),
        before,
        "a refused call changes nothing"
    );

    // The host reports the GPU: its own client's session answers it, and every other reads it.
    let reported = owner.report_renderer(desktop, Renderer::gpu()).unwrap();
    assert_eq!(reported.renderer, Renderer::gpu());
    let after = state(&owner, agent);
    assert_eq!(after["renderer"], json!({"record": "gpu", "reason": null}));
    assert!(
        after["revision"].as_u64() > before["revision"].as_u64(),
        "{before} then {after}"
    );
    assert_eq!(state(&owner, desktop)["renderer"], after["renderer"]);

    // The same value again touches no session.
    let unchanged = owner.report_renderer(desktop, Renderer::gpu()).unwrap();
    assert_eq!(unchanged.revision, reported.revision);
    assert_eq!(state(&owner, agent)["revision"], after["revision"]);

    // A lost device: the reference, with the surface's own reason, for every client, a late one
    // included.
    owner
        .report_renderer(desktop, Renderer::reference(RendererReason::DeviceLost))
        .unwrap();
    let late = owner.register();
    for client in [agent, desktop, late] {
        assert_eq!(
            state(&owner, client)["renderer"],
            json!({"record": "reference", "reason": "device-lost"})
        );
    }
    stop(owner, join, catalog);
}

/// The record reads only its two shapes, the GPU with no reason and the reference with one or
/// none, and only the reasons the desktop's surface names.
#[test]
fn session_renderer_reads_only_its_two_shapes() {
    let read = |value: Value| serde_json::from_value::<Renderer>(value);
    for renderer in [
        Renderer::gpu(),
        Renderer::headless(),
        Renderer::reference(RendererReason::SurfacePending),
        Renderer::reference(RendererReason::NoAdapter),
        Renderer::reference(RendererReason::DeviceLost),
    ] {
        let written = serde_json::to_value(renderer).unwrap();
        assert_eq!(read(written).unwrap(), renderer);
    }
    assert_eq!(
        serde_json::to_value(Renderer::reference(RendererReason::NoAdapter)).unwrap(),
        json!({"record": "reference", "reason": "no-adapter"})
    );
    assert_eq!(Renderer::default(), Renderer::headless());
    assert!(read(json!({"record": "gpu", "reason": "no-adapter"})).is_err());
    assert!(read(json!({"record": "reference", "reason": "pipeline-melted"})).is_err());
    assert!(read(json!({"record": "cpu", "reason": null})).is_err());
    assert!(read(json!({"record": "gpu", "reason": null, "adapter": "x"})).is_err());
    // A session the desktop reads back keeps it.
    let session = ClientSession {
        renderer: Renderer::gpu(),
        ..ClientSession::default()
    };
    let written = serde_json::to_value(&session).unwrap();
    assert_eq!(
        written["renderer"],
        json!({"record": "gpu", "reason": null})
    );
    assert_eq!(
        serde_json::from_value::<ClientSession>(written)
            .unwrap()
            .renderer,
        Renderer::gpu()
    );
}

/// `schema.list` describes the field: its records and reasons in the session's own spelling, and
/// `session.state`'s notes name it.
#[test]
fn session_renderer_is_described_by_schema_list() {
    let (owner, join, catalog) = owner_with("schema", Renderer::headless());
    let client = owner.register();
    let listed = call(&owner, client, "schema.list", json!({}))
        .result
        .unwrap();
    assert_eq!(listed["renderer"]["records"], json!(["gpu", "reference"]));
    assert_eq!(
        listed["renderer"]["reasons"],
        json!(["surface-pending", "no-adapter", "device-lost"])
    );
    for reason in RendererReason::ALL {
        let written = serde_json::to_value(reason).unwrap();
        assert_eq!(written, json!(reason.as_str()));
        assert!(
            listed["renderer"]["notes"]
                .as_str()
                .unwrap()
                .contains(reason.as_str()),
            "{reason:?}"
        );
    }
    assert!(
        listed["methods"]["session.state"]["notes"]
            .as_str()
            .unwrap()
            .contains("renderer {record, reason}")
    );
    stop(owner, join, catalog);
}
