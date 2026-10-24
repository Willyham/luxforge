//! The connected clients (`docs/design/mcp.md`, "Core additions"): `session.identify` names a
//! client, `session.clients` lists the others with their metadata and `session.disconnect` closes
//! one, each change recorded in the event log, against a real owner and real loopback clients.
use super::*;
use crate::LocalServer;
use luxforge_testbase::{Gate, HANG, paths, wait_until};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
};

/// One loopback connection to a live session, answering one request at a time.
struct Connection {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
    token: String,
}

impl Connection {
    fn open(server: &LocalServer) -> Self {
        let writer = TcpStream::connect(server.info().address).unwrap();
        writer.set_read_timeout(Some(HANG)).unwrap();
        let reader = BufReader::new(writer.try_clone().unwrap());
        Self {
            writer,
            reader,
            token: server.info().token.clone(),
        }
    }

    fn send(&mut self, id: &str, method: &str, params: Value) {
        // One write a line: a line written in pieces waits on Nagle's algorithm for the owner's
        // delayed acknowledgement.
        let line = json!({"id": id, "method": method, "params": params, "token": self.token});
        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
    }

    fn answer(&mut self) -> ApiResponse {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("{error}: {line:?}"))
    }

    fn ask(&mut self, method: &str, params: Value) -> ApiResponse {
        self.send(method, method, params);
        self.answer()
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let response = self.ask(method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.unwrap()
    }
}

fn call(
    owner: &OwnerHandle,
    client: ClientId,
    id: &str,
    method: &str,
    params: Value,
) -> ApiResponse {
    owner
        .call(
            client,
            ApiRequest {
                id: id.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .unwrap()
}

fn ok(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Value {
    let response = call(owner, client, method, method, params);
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.unwrap()
}

fn refused(response: ApiResponse) -> crate::ApiFailure {
    response.error.expect("refused")
}

/// The `session.*` events the log holds, as `(method, request_id, client_id)`.
fn session_events(owner: &OwnerHandle, client: ClientId) -> Vec<(String, String, Value)> {
    ok(owner, client, "events.since", json!({"after": 0}))["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["method"].as_str().unwrap().starts_with("session."))
        .map(|event| {
            (
                event["method"].as_str().unwrap().to_owned(),
                event["request_id"].as_str().unwrap().to_owned(),
                event["client_id"].clone(),
            )
        })
        .collect()
}

fn event(method: &str, request_id: &str, client: &Value) -> (String, String, Value) {
    (method.to_owned(), request_id.to_owned(), client.clone())
}

/// Two loopback clients: each names itself, sees the other with its label, transport, authority and
/// request count and never itself or the desktop's own client; a later identification replaces
/// the label. Their connections, identifications and a disconnection are in the event log, and a
/// held `events.wait` is answered by the disconnection.
#[test]
fn session_clients_lists_the_other_loopback_client_and_the_log_records_its_connection() {
    let catalog = paths::temp_path("clients-listed.sqlite");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let server = match LocalServer::start(owner.clone(), &paths::temp_path("clients-listed.json")) {
        Ok(server) => server,
        Err(error) if luxforge_testbase::loopback_forbidden(&error.detail) => {
            owner.stop();
            join.join().unwrap();
            return;
        }
        Err(error) => panic!("cannot start the live session: {error}"),
    };
    let desktop = owner.register_desktop();
    // Each is answered before the next connects, so they are registered in this order.
    let mut first = Connection::open(&server);
    let first_id = first.ok("session.identify", json!({"label": "first"}));
    assert_eq!(first_id["label"], "first");
    let first_id = first_id["client_id"].clone();
    let mut second = Connection::open(&server);
    let second_id = second.ok("session.identify", json!({"label": "second"}))["client_id"].clone();
    second.ok("catalog.info", json!({}));

    let listed = first.ok("session.clients", json!({}));
    let rows = listed["clients"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "only the other client: {listed}");
    let row = &rows[0];
    assert_eq!(row["client_id"], second_id);
    assert_eq!(row["label"], "second");
    assert_eq!(row["authority"], "edit");
    assert_eq!(row["transport"], "loopback");
    assert_eq!(row["requests"], 2, "its identification and catalog.info");
    let connected_at = row["connected_at"].as_i64().unwrap();
    assert!(connected_at > 0, "{row}");
    assert!(
        row["last_request_at"].as_i64().unwrap() >= connected_at,
        "{row}"
    );
    assert_eq!(
        row.as_object().unwrap().len(),
        7,
        "metadata only, never what a request asked: {row}"
    );

    // The desktop's own client sees both, in the order they connected; nobody lists the desktop.
    let seen = ok(&owner, desktop, "session.clients", json!({}));
    let ids: Vec<&Value> = seen["clients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| &row["client_id"])
        .collect();
    assert_eq!(ids, [&first_id, &second_id]);
    assert_eq!(seen["clients"][0]["requests"], 2, "{seen}");

    // A later identification replaces the label; a label is 1 to 64 characters with something
    // other than whitespace, and a refused one changes nothing.
    second.ok("session.identify", json!({"label": "renamed"}));
    for label in [
        json!(""),
        json!("   "),
        json!("x".repeat(65)),
        json!("two\nlines"),
    ] {
        let error = refused(second.ask("session.identify", json!({"label": label})));
        assert_eq!(error.code, "validation", "{label}: {error:?}");
    }
    let relisted = first.ok("session.clients", json!({}));
    assert_eq!(relisted["clients"][0]["label"], "renamed");

    // A held wait from the desktop is answered by the second client's disconnection.
    let after =
        ok(&owner, desktop, "events.since", json!({"after": 0}))["current_sequence"].clone();
    let waiting = std::thread::spawn({
        let owner = owner.clone();
        move || {
            ok(
                &owner,
                desktop,
                "events.wait",
                json!({"after": after, "timeout_ms": 30_000}),
            )
        }
    });
    wait_until("the desktop's held wait", || owner.event_waits() == 1);
    drop(second);
    let woken = waiting.join().unwrap();
    assert_eq!(woken["events"].as_array().unwrap().len(), 1, "{woken}");
    assert_eq!(woken["events"][0]["method"], "session.disconnect");
    assert_eq!(woken["events"][0]["request_id"], "");
    assert_eq!(woken["events"][0]["client_id"], second_id);

    assert_eq!(
        session_events(&owner, desktop),
        [
            event("session.connect", "", &first_id),
            event("session.identify", "session.identify", &first_id),
            event("session.connect", "", &second_id),
            event("session.identify", "session.identify", &second_id),
            event("session.identify", "session.identify", &second_id),
            event("session.disconnect", "", &second_id),
        ]
    );
    let after = first.ok("session.clients", json!({}));
    assert_eq!(after["clients"], json!([]), "the gone client is forgotten");

    // A client the host runs in its own process is listed as desktop-internal, and its
    // registration is no event.
    let internal = owner.register();
    ok(&owner, internal, "catalog.info", json!({}));
    let listed = first.ok("session.clients", json!({}));
    assert_eq!(listed["clients"][0]["transport"], "desktop-internal");
    assert!(listed["clients"][0].get("label").is_none(), "{listed}");
    assert_eq!(session_events(&owner, desktop).len(), 6);

    drop(first);
    drop(server);
    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(catalog);
}

/// The desktop's permission authority disconnects a loopback client: its connection closes, its
/// draft ends, the `clients` job it alone wanted stops and its held wait is dropped, and the log
/// records it once. An edit client is refused `forbidden`; the caller's own id and an unknown id
/// are refused by name.
#[test]
fn session_disconnect_closes_a_client_and_ends_its_session_as_its_own_disconnection_would() {
    let catalog = paths::temp_path("clients-disconnect.sqlite");
    let asset = {
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        service.import(&paths::jpeg()).unwrap().asset.id.to_string()
    };
    let sources = Arc::new(Gate::new());
    let hold = sources.clone();
    let (owner, join) =
        OwnerHandle::start_holding_sources(&catalog, Arc::new(move || hold.pass())).unwrap();
    let server =
        match LocalServer::start(owner.clone(), &paths::temp_path("clients-disconnect.json")) {
            Ok(server) => server,
            Err(error) if luxforge_testbase::loopback_forbidden(&error.detail) => {
                owner.stop();
                join.join().unwrap();
                return;
            }
            Err(error) => panic!("cannot start the live session: {error}"),
        };
    let desktop = owner.register_desktop();
    let editor = owner.register();
    let mut target = Connection::open(&server);
    let target_id = target.ok("session.identify", json!({"label": "target"}))["client_id"].clone();
    let client = ClientId(target_id.as_u64().unwrap());
    let draft = target.ok(
        "draft.begin",
        json!({"asset_id": asset, "action": "set-basic"}),
    )["draft_id"]
        .clone();
    assert_eq!(
        owner.draft_of(client).map(|draft| json!(draft)),
        Some(draft)
    );
    // Its preparation is held before its work, so it is still running when the client goes.
    sources.shut();
    let reached = sources.reached();
    let prepared = target.ok("source.prepare", json!({"asset_id": asset}));
    let job = prepared["job_id"].clone();
    sources.wait_reached(reached + 1, "the held preparation");
    let current =
        ok(&owner, desktop, "events.since", json!({"after": 0}))["current_sequence"].clone();
    target.send(
        "held",
        "events.wait",
        json!({"after": current, "timeout_ms": 30_000}),
    );
    wait_until("the target's held wait", || owner.event_waits() == 1);

    let forbidden = refused(call(
        &owner,
        editor,
        "edit",
        "session.disconnect",
        json!({"client_id": target_id}),
    ));
    assert_eq!(forbidden.code, "forbidden", "{forbidden:?}");
    let own = refused(call(
        &owner,
        desktop,
        "own",
        "session.disconnect",
        json!({"client_id": desktop}),
    ));
    assert_eq!(own.code, "validation");
    let unknown = refused(call(
        &owner,
        desktop,
        "unknown",
        "session.disconnect",
        json!({"client_id": 9999}),
    ));
    assert_eq!(unknown.code, "validation");
    assert!(unknown.message.contains("9999"), "{}", unknown.message);
    assert_eq!(owner.event_waits(), 1, "a refusal changes nothing");

    let closed = ok(
        &owner,
        desktop,
        "session.disconnect",
        json!({"client_id": target_id}),
    );
    assert_eq!(closed["client_id"], target_id);
    // The connection is closed: the client reads to its end with no answer to its held wait.
    let mut rest = String::new();
    target.reader.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "", "the held wait is dropped unanswered");
    assert_eq!(owner.event_waits(), 0);
    assert_eq!(
        owner.draft_of(client),
        None,
        "the draft ended with the session"
    );
    wait_until("the closed connection's worker", || server.connected() == 0);
    // Nobody wants the preparation any more, so it stopped and no longer joins: the same request
    // starts a new job.
    let renewed = ok(&owner, editor, "source.prepare", json!({"asset_id": asset}));
    assert_ne!(renewed["job_id"], job, "the stopped job was not joined");
    // Recorded once, under the desktop's request, though the transport reported the close too.
    let events = session_events(&owner, desktop);
    assert_eq!(
        events.last().unwrap(),
        &event("session.disconnect", "session.disconnect", &target_id)
    );
    assert_eq!(
        events
            .iter()
            .filter(|(method, ..)| method == "session.disconnect")
            .count(),
        1,
        "{events:?}"
    );
    assert_eq!(
        ok(&owner, desktop, "session.clients", json!({}))["clients"][0]["client_id"],
        json!(editor)
    );

    sources.open();
    owner.disconnect(editor);
    drop(server);
    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(catalog);
}
