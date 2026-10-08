//! Finding and talking to a live session: which catalog is targeted, which session files are
//! refused, and the bounded framing, identities, token and failures of a call.
use super::endpoint::{Endpoint, Reply, TOKEN};
use super::*;
use crate::Paths;
use luxforge_core::{LocalServer, OwnerHandle, live_session_file};
use luxforge_testbase::paths::{temp_catalog, temp_path};
use serde_json::json;
use std::{net::TcpListener, path::Path};

fn write_session(catalog: &Path, text: &str) {
    std::fs::write(live_session_file(catalog), text).unwrap();
}

fn refusal(catalog: &Path) -> ApiFailure {
    LiveSession::discover(catalog).unwrap_err()
}

#[test]
fn live_session_client_targets_the_explicit_or_the_desktops_catalog_creating_nothing() {
    let root = temp_path("live-target");
    let paths = Paths::resolve(Some(&root)).unwrap();
    let named = root.join("named.sqlite");
    assert_eq!(
        target_catalog(Some(named.clone()), Some(&paths)).unwrap(),
        named
    );
    // Nothing stored: the default, as an ordinary desktop launch opens it.
    assert_eq!(
        target_catalog(None, Some(&paths)).unwrap(),
        paths.default_catalog()
    );
    assert!(!root.exists(), "targeting creates nothing");
    // A stored catalog whose folder exists is the desktop's, and so the client's.
    let stored = root.join("Photos").join("catalog.sqlite");
    std::fs::create_dir_all(stored.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(
        paths.config.join("preferences.json"),
        json!({"format": 1, "catalog": stored}).to_string(),
    )
    .unwrap();
    assert_eq!(target_catalog(None, Some(&paths)).unwrap(), stored);
    assert_eq!(target_catalog(Some(named.clone()), None).unwrap(), named);
    assert_eq!(target_catalog(None, None).unwrap_err().code, USAGE);
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
            json!({"protocol": PROTOCOL_NAME, "address": "127.0.0.1:1", "token": "t", "extra": 1})
                .to_string(),
            INVALID_SESSION,
        ),
        (
            session("luxforge-jsonl-0", "127.0.0.1:1", "t"),
            UNSUPPORTED_PROTOCOL,
        ),
        (session(PROTOCOL_NAME, "192.0.2.1:1", "t"), INVALID_SESSION),
        (session(PROTOCOL_NAME, "[::1]:1", "t"), INVALID_SESSION),
        (session(PROTOCOL_NAME, "127.0.0.1:1", ""), INVALID_SESSION),
        (
            session(
                PROTOCOL_NAME,
                "127.0.0.1:1",
                &"t".repeat(SESSION_FILE_LIMIT),
            ),
            INVALID_SESSION,
        ),
    ] {
        write_session(&catalog, &text);
        assert_eq!(refusal(&catalog).code, code, "{text:.80}");
    }
    write_session(
        &catalog,
        &session(PROTOCOL_NAME, "127.0.0.2:9", "secret-token"),
    );
    let found = LiveSession::discover(&catalog).unwrap();
    assert_eq!(found.address.to_string(), "127.0.0.2:9");
    assert!(
        !format!("{found:?}").contains("secret-token"),
        "the token stays out of debug output"
    );
    std::fs::remove_file(live_session_file(&catalog)).unwrap();
}

const PROTOCOL_NAME: &str = luxforge_core::PROTOCOL;

#[test]
fn live_session_client_reports_a_session_nothing_answers_as_stale() {
    let catalog = temp_catalog("live-stale");
    // An address that was served once and no longer is, as after the editor quit uncleanly.
    let address = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap();
    write_session(
        &catalog,
        &json!({"protocol": PROTOCOL_NAME, "address": address, "token": "t"}).to_string(),
    );
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
    let mut client = LiveClient::connect(&session).unwrap();
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
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request["token"], TOKEN);
        let id = uuid::Uuid::parse_str(request["id"].as_str().unwrap()).unwrap();
        assert_eq!(id.get_version_num(), 4);
    }
    assert_ne!(requests[0]["id"], requests[1]["id"]);
    assert_eq!(requests[1]["params"], json!({"a": 1}));
}

#[test]
fn live_session_client_bounds_requests_and_answers() {
    let endpoint = Endpoint::serve(|n, _| match n {
        // An answer one byte past the limit with its newline.
        0 => Reply::Raw(vec![b' '; RESPONSE_LIMIT]),
        _ => unreachable!("the connection is not used after an oversized answer"),
    });
    let session = LiveSession::discover(&endpoint.catalog).unwrap();
    let mut client = LiveClient::connect(&session).unwrap();
    let oversized = client
        .call("catalog.info", json!({"pad": "x".repeat(REQUEST_LIMIT)}))
        .unwrap_err();
    assert_eq!(oversized.code, "resource-limit", "{oversized:?}");
    let long = client.call("catalog.info", json!({})).unwrap_err();
    assert_eq!(long.code, "resource-limit", "{long:?}");
    drop(client);
    assert_eq!(
        endpoint.requests().len(),
        1,
        "the oversized request was never written"
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
    let mut client = LiveClient::connect(&session).unwrap();
    assert_eq!(client.call("a", json!({})).unwrap_err().code, "protocol");
    assert_eq!(client.call("b", json!({})).unwrap_err().code, "protocol");
    assert_eq!(
        client.call("c", json!({})).unwrap_err().code,
        CONNECTION_LOST
    );
    drop(client);
    assert_eq!(endpoint.requests().len(), 3);
}

#[test]
fn live_session_client_reads_a_real_owner_and_is_refused_without_its_token() {
    let catalog = temp_catalog("live-owner");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let server = match LocalServer::start(owner.clone(), &live_session_file(&catalog)) {
        Ok(server) => server,
        // A sandbox that forbids loopback listening cannot run this test.
        Err(error) if error.detail.contains("Operation not permitted") => {
            owner.stop();
            join.join().unwrap();
            return;
        }
        Err(error) => panic!("cannot start the live session: {error}"),
    };
    let session = LiveSession::discover(&catalog).unwrap();
    let mut client = LiveClient::connect(&session).unwrap();
    let schema = client.call("schema.list", json!({})).unwrap();
    assert_eq!(schema["protocol"], PROTOCOL_NAME);
    let unknown = client.call("no.such-method", json!({})).unwrap_err();
    assert_eq!(unknown.code, "protocol", "{unknown:?}");
    drop(client);
    // The same address with another token is refused by the owner, and the failure is kept.
    let forged = LiveSession {
        token: "forged".into(),
        ..session
    };
    let mut client = LiveClient::connect(&forged).unwrap();
    let refused = client.call("catalog.info", json!({})).unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.message.as_str()),
        ("protocol", "invalid live-session token")
    );
    drop(client);
    drop(server);
    assert!(!live_session_file(&catalog).exists());
    owner.stop();
    join.join().unwrap();
    std::fs::remove_file(catalog).unwrap();
}
