//! Finding and talking to a live session: which session is found, which session files and registry
//! entries are refused, the handshake that proves an address is the session, and the bounded
//! framing, identities, token and failures of a call.
use super::scripted_session::{Endpoint, Reply, TOKEN, schema};
use crate::Paths;
use crate::live::*;
use luxforge_core::{
    ApiFailure, LiveSessionEntry, LocalServer, OwnerHandle, PROTOCOL, live_session_file,
};
use luxforge_testbase::paths::{temp_catalog, temp_path};
use serde_json::json;
use std::{fs::File, net::TcpListener, path::Path, time::Duration};

fn write_session(catalog: &Path, text: &str) {
    std::fs::write(live_session_file(catalog), text).unwrap();
}

fn session_text(address: &str) -> String {
    json!({"protocol": PROTOCOL, "address": address, "token": "t"}).to_string()
}

fn refusal(catalog: &Path) -> ApiFailure {
    LiveSession::discover(catalog).unwrap_err()
}

/// Register a session in `paths`' registry as a running desktop does, holding its lock until the
/// answered file is dropped.
fn register(paths: &Paths, name: &str, entry: &serde_json::Value) -> File {
    let dir = paths.live_sessions();
    std::fs::create_dir_all(&dir).unwrap();
    let lock = File::create(dir.join(format!("{name}.lock"))).unwrap();
    lock.lock().unwrap();
    std::fs::write(dir.join(format!("{name}.json")), entry.to_string()).unwrap();
    lock
}

fn entry(catalog: &Path) -> serde_json::Value {
    serde_json::to_value(LiveSessionEntry {
        protocol: PROTOCOL.into(),
        pid: 7,
        catalog: catalog.to_path_buf(),
        session_file: live_session_file(catalog),
    })
    .unwrap()
}

#[test]
fn live_session_client_finds_the_running_session_or_the_named_catalog_creating_nothing() {
    let root = temp_path("live-find");
    let paths = Paths::resolve(Some(&root)).unwrap();
    let find = |explicit: Option<&Path>| find_session(explicit, Some(&paths));
    let none = find(None).unwrap_err();
    assert_eq!(none.code, NO_SESSION, "{none:?}");
    assert!(!root.exists(), "finding creates nothing");
    assert_eq!(find_session(None, None).unwrap_err().code, USAGE);
    // A named catalog is read from its own session file, whatever is registered.
    let named = root.join("named.sqlite");
    assert_eq!(find(Some(&named)).unwrap_err().code, NO_SESSION);
    std::fs::create_dir_all(&root).unwrap();
    write_session(&named, &session_text("127.0.0.1:9"));
    assert_eq!(find(Some(&named)).unwrap().catalog, named);
    // One running desktop is the session, whichever catalog its next launch would open.
    let open = root.join("Elsewhere").join("catalog.sqlite");
    std::fs::create_dir_all(open.parent().unwrap()).unwrap();
    write_session(&open, &session_text("127.0.0.1:10"));
    let first = register(&paths, "7-a", &entry(&open));
    let found = find(None).unwrap();
    assert_eq!(
        (found.catalog.as_path(), found.address.port()),
        (open.as_path(), 10)
    );
    // An entry whose process has gone is not running, and is removed.
    let stale = paths.live_sessions().join("8-gone.json");
    std::fs::write(&stale, entry(&named).to_string()).unwrap();
    std::fs::write(stale.with_extension("lock"), b"").unwrap();
    assert_eq!(find(None).unwrap().catalog, open);
    assert!(!stale.exists());
    // Two running desktops are never guessed between.
    let second = register(&paths, "9-b", &entry(&named));
    let several = find(None).unwrap_err();
    assert_eq!(several.code, USAGE, "{several:?}");
    assert_eq!(
        several.data.unwrap()["sessions"],
        json!([{"catalog": open, "pid": 7}, {"catalog": named, "pid": 7}])
    );
    drop(second);
    drop(first);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_session_client_refuses_a_registry_entry_it_cannot_use() {
    let root = temp_path("live-entries");
    let paths = Paths::resolve(Some(&root)).unwrap();
    let catalog = root.join("catalog.sqlite");
    let foreign = json!({"protocol": "luxforge-jsonl-0", "pid": 7, "catalog": catalog,
                         "session_file": live_session_file(&catalog)});
    let elsewhere = json!({"protocol": PROTOCOL, "pid": 7, "catalog": catalog,
                           "session_file": root.join("other.json")});
    for (text, code) in [
        (json!({"protocol": PROTOCOL}), INVALID_SESSION),
        (foreign, UNSUPPORTED_PROTOCOL),
        (elsewhere, INVALID_SESSION),
        // A well-formed entry whose desktop has not written its session file yet.
        (entry(&catalog), NO_SESSION),
    ] {
        let lock = register(&paths, "7-entry", &text);
        let refused = find_session(None, Some(&paths)).unwrap_err();
        assert_eq!(refused.code, code, "{text}: {refused:?}");
        drop(lock);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_session_client_refuses_missing_malformed_and_foreign_sessions() {
    let catalog = temp_catalog("live-refusals");
    let missing = refusal(&catalog);
    assert_eq!(missing.code, NO_SESSION, "{missing:?}");
    assert_eq!(missing.data.unwrap()["catalog"], json!(catalog));
    let session = |protocol: &str, address: &str, token: &str| {
        json!({"protocol": protocol, "address": address, "token": token}).to_string()
    };
    for (text, code) in [
        ("not json".to_owned(), INVALID_SESSION),
        (
            json!({"protocol": PROTOCOL, "address": "127.0.0.1:1", "token": "t", "extra": 1})
                .to_string(),
            INVALID_SESSION,
        ),
        (
            session("luxforge-jsonl-0", "127.0.0.1:1", "t"),
            UNSUPPORTED_PROTOCOL,
        ),
        (session(PROTOCOL, "192.0.2.1:1", "t"), INVALID_SESSION),
        (session(PROTOCOL, "[::1]:1", "t"), INVALID_SESSION),
        (session(PROTOCOL, "127.0.0.1:1", ""), INVALID_SESSION),
        (
            session(PROTOCOL, "127.0.0.1:1", &"t".repeat(SESSION_FILE_LIMIT)),
            INVALID_SESSION,
        ),
    ] {
        write_session(&catalog, &text);
        assert_eq!(refusal(&catalog).code, code, "{text:.80}");
    }
    write_session(&catalog, &session(PROTOCOL, "127.0.0.2:9", "secret-token"));
    let found = LiveSession::discover(&catalog).unwrap();
    assert_eq!(found.address.to_string(), "127.0.0.2:9");
    assert!(
        !format!("{found:?}").contains("secret-token"),
        "the token stays out of debug output"
    );
    std::fs::remove_file(live_session_file(&catalog)).unwrap();
}

#[test]
fn live_session_client_reports_a_session_nothing_answers_as_stale() {
    let catalog = temp_catalog("live-stale");
    // An address that was served once and no longer is, as after the editor quit uncleanly.
    let address = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap();
    write_session(&catalog, &session_text(&address.to_string()));
    let session = LiveSession::discover(&catalog).unwrap();
    let started = std::time::Instant::now();
    let stale = LiveClient::connect(&session).err().unwrap();
    assert_eq!(stale.code, STALE_SESSION, "{stale:?}");
    assert!(
        started.elapsed() <= CONNECT_TIMEOUT * 2,
        "bounded by the connect timeout"
    );
    std::fs::remove_file(live_session_file(&catalog)).unwrap();
}

#[test]
fn live_session_client_handshake_proves_the_address_is_the_session() {
    // An address that accepts and never answers, as another program now holding it might: the
    // handshake's deadline makes it stale rather than a hang.
    let endpoint = Endpoint::serve_all(|_, _| Reply::Silence);
    let session = LiveSession::discover(&endpoint.catalog).unwrap();
    let silent = LiveClient::connect_within(&session, Duration::from_millis(200))
        .err()
        .unwrap();
    assert_eq!(silent.code, STALE_SESSION, "{silent:?}");
    drop(endpoint);
    for (reply, code) in [
        // Something that is not this protocol at all.
        (
            Reply::Raw(b"HTTP/1.1 400 Bad Request".to_vec()),
            STALE_SESSION,
        ),
        (Reply::Close, STALE_SESSION),
        // Another session, whose token is not this one's.
        (
            Reply::Error(json!({"code": "protocol", "message": "invalid live-session token"})),
            STALE_SESSION,
        ),
        // A session that speaks another protocol.
        (
            Reply::Result(json!({"protocol": "luxforge-jsonl-0", "methods": {}})),
            UNSUPPORTED_PROTOCOL,
        ),
    ] {
        let mut reply = Some(reply);
        let endpoint = Endpoint::serve_all(move |_, _| reply.take().unwrap());
        let session = LiveSession::discover(&endpoint.catalog).unwrap();
        let refused = LiveClient::connect(&session).err().unwrap();
        assert_eq!(refused.code, code, "{refused:?}");
    }
    // A full session's refusal is the session's own answer, kept whole.
    let full = json!({"code": "resource-limit", "message": "full", "data": {"max_clients": 8}});
    let answer = full.clone();
    let endpoint = Endpoint::serve_all(move |_, _| {
        Reply::Raw(serde_json::to_vec(&json!({"id": "", "sequence": 0, "error": answer})).unwrap())
    });
    let session = LiveSession::discover(&endpoint.catalog).unwrap();
    let refused = LiveClient::connect(&session).err().unwrap();
    assert_eq!(serde_json::to_value(&refused).unwrap(), full);
    // The session's schema comes back from the handshake, and later calls have no deadline.
    let endpoint = Endpoint::serve(|_, _| Reply::Result(json!({"ok": true})));
    let session = LiveSession::discover(&endpoint.catalog).unwrap();
    let (mut client, listed) = LiveClient::connect(&session).unwrap();
    assert_eq!(&listed, schema());
    client.call("catalog.info", json!({})).unwrap();
}

#[test]
fn live_session_client_sends_the_token_with_new_identities_and_keeps_failures_whole() {
    let failure = json!({
        "code": "consent-required",
        "message": "needs consent",
        "job_id": "job-1",
        "data": {"consent": {"scope": "photo"}},
    });
    let answer = failure.clone();
    let endpoint = Endpoint::serve(move |n, _| match n {
        0 => Reply::Result(json!({"ok": true})),
        _ => Reply::Error(answer.clone()),
    });
    let session = LiveSession::discover(&endpoint.catalog).unwrap();
    let (mut client, _) = LiveClient::connect(&session).unwrap();
    assert_eq!(
        client.call("catalog.info", json!({})).unwrap(),
        json!({"ok": true})
    );
    let refused = client
        .call("module.task.start", json!({"a": 1}))
        .unwrap_err();
    assert_eq!(serde_json::to_value(&refused).unwrap(), failure);
    drop(client);
    let requests = endpoint.requests();
    assert_eq!(requests.len(), 3, "the handshake and two calls");
    for request in &requests {
        assert_eq!(request["token"], TOKEN);
        let id = uuid::Uuid::parse_str(request["id"].as_str().unwrap()).unwrap();
        assert_eq!(id.get_version_num(), 4);
    }
    assert_ne!(requests[1]["id"], requests[2]["id"]);
    assert_eq!(requests[2]["params"], json!({"a": 1}));
}

#[test]
fn live_session_client_bounds_requests_and_answers() {
    let endpoint = Endpoint::serve(|n, _| match n {
        // An answer one byte past the limit with its newline.
        0 => Reply::Raw(vec![b' '; RESPONSE_LIMIT]),
        _ => unreachable!("the connection is not used after an oversized answer"),
    });
    let session = LiveSession::discover(&endpoint.catalog).unwrap();
    let (mut client, _) = LiveClient::connect(&session).unwrap();
    let oversized = client
        .call(
            "catalog.info",
            json!({"pad": "x".repeat(luxforge_core::MAX_REQUEST_BYTES)}),
        )
        .unwrap_err();
    assert_eq!(oversized.code, "resource-limit", "{oversized:?}");
    let long = client.call("catalog.info", json!({})).unwrap_err();
    assert_eq!(long.code, "resource-limit", "{long:?}");
    drop(client);
    assert_eq!(
        endpoint.requests().len(),
        2,
        "the handshake and one call: the oversized request was never written"
    );
}

#[test]
fn live_session_client_reports_lost_connections_and_foreign_answers() {
    let endpoint = Endpoint::serve(|n, _| match n {
        0 => Reply::Raw(br#"{"id":"another","sequence":0,"result":{}}"#.to_vec()),
        1 => Reply::Raw(b"{not an answer".to_vec()),
        _ => Reply::Close,
    });
    let session = LiveSession::discover(&endpoint.catalog).unwrap();
    let (mut client, _) = LiveClient::connect(&session).unwrap();
    assert_eq!(
        client.call("a", json!({})).unwrap_err().code,
        PROTOCOL_ERROR
    );
    assert_eq!(
        client.call("b", json!({})).unwrap_err().code,
        PROTOCOL_ERROR
    );
    assert_eq!(
        client.call("c", json!({})).unwrap_err().code,
        CONNECTION_LOST
    );
    drop(client);
    assert_eq!(endpoint.requests().len(), 4);
}

#[test]
fn live_session_client_reads_a_real_owner_and_is_refused_without_its_token() {
    let catalog = temp_catalog("live-owner");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let server = match LocalServer::start(owner.clone(), &live_session_file(&catalog)) {
        Ok(server) => server,
        Err(error) if luxforge_testbase::loopback_forbidden(&error.detail) => {
            owner.stop();
            join.join().unwrap();
            return;
        }
        Err(error) => panic!("cannot start the live session: {error}"),
    };
    let session = LiveSession::discover(&catalog).unwrap();
    let (mut client, schema) = LiveClient::connect(&session).unwrap();
    assert_eq!(schema["protocol"], PROTOCOL);
    let unknown = client.call("no.such-method", json!({})).unwrap_err();
    assert_eq!(unknown.code, PROTOCOL_ERROR, "{unknown:?}");
    drop(client);
    // The same address with another token is not this session.
    let forged = LiveSession {
        token: "forged".into(),
        ..session
    };
    let refused = LiveClient::connect(&forged).err().unwrap();
    assert_eq!(refused.code, STALE_SESSION, "{refused:?}");
    assert!(refused.message.contains("invalid live-session token"));
    drop(server);
    assert!(!live_session_file(&catalog).exists());
    owner.stop();
    join.join().unwrap();
    std::fs::remove_file(catalog).unwrap();
}
