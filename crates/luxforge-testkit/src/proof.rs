//! The capability proof's side of the exchange its fake provider serves, from the core's constants:
//! `luxforge_testbase::ProofEndpoint` depends on no workspace crate, so it is handed this. The
//! endpoint's own tests live here, against the core's real protocol.
use luxforge_core::{
    PROOF_GENERATE_PATH, PROOF_PALETTE, PROOF_PALETTE_PATH,
    capabilities::data::{SAMPLE_GRID_BYTES, SAMPLE_GRID_SAMPLES},
    palette_bytes,
};
use luxforge_testbase::ProofProtocol;

/// The proof module's paths, its pinned palette and a wrong one of the same length (gains of one,
/// a well-formed palette the pin refuses), and the size of a `sample-grid-8` body.
pub fn proof_protocol() -> ProofProtocol {
    ProofProtocol {
        palette_path: PROOF_PALETTE_PATH,
        generate_path: PROOF_GENERATE_PATH,
        palette: PROOF_PALETTE.to_vec(),
        wrong_palette: palette_bytes([1.0, 1.0, 1.0]).to_vec(),
        grid_bytes: SAMPLE_GRID_BYTES,
        grid_samples: SAMPLE_GRID_SAMPLES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::capabilities::data::sample_grid_body;
    use luxforge_testbase::{ProofAnswer, ProofEndpoint, TestServer};
    use std::{
        io::{Read, Write},
        net::TcpStream,
        thread,
        time::Duration,
    };

    /// The origin an endpoint answered in process names.
    const IN_PROCESS_ORIGIN: &str = "https://proof.example";

    fn server(endpoint: &ProofEndpoint) -> &TestServer {
        endpoint.server().expect("a started endpoint")
    }

    /// One raw exchange with the endpoint.
    fn exchange(endpoint: &ProofEndpoint, request: &[u8]) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(server(endpoint).address()).unwrap();
        stream.write_all(request).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let status = std::str::from_utf8(&response[9..12])
            .unwrap()
            .parse()
            .unwrap();
        let end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        (status, response[end + 4..].to_vec())
    }

    fn post(key: &str, body: &[u8]) -> Vec<u8> {
        let mut request = format!(
            "POST /generate HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(body);
        request
    }

    #[test]
    fn the_endpoint_serves_the_palette_checks_the_key_and_answers_a_tint_of_the_grid() {
        let endpoint = ProofEndpoint::start("secret-key", proof_protocol()).unwrap();
        assert!(endpoint.base_url().starts_with("http://127.0.0.1:"));
        assert_eq!(
            endpoint.generate_url(),
            format!("{}/generate", endpoint.base_url())
        );
        let (status, body) = exchange(
            &endpoint,
            b"GET /proof-palette.bin HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert_eq!(status, 200);
        assert_eq!(body, PROOF_PALETTE);
        endpoint.serve_wrong_palette(true);
        let (_, wrong) = exchange(
            &endpoint,
            b"GET /proof-palette.bin HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert_eq!(wrong.len(), PROOF_PALETTE.len());
        assert_ne!(wrong, PROOF_PALETTE);
        let grid: Vec<[u8; 3]> = (0..SAMPLE_GRID_SAMPLES)
            .map(|index| [index as u8, 100, 255])
            .collect();
        let body = sample_grid_body(&grid).unwrap();
        let (status, answer) = exchange(&endpoint, &post("secret-key", &body));
        assert_eq!(status, 200);
        // Means 31, 100 and 255, a quarter of each added to 192.
        assert_eq!(answer, br#"{"rgb":[199,217,255]}"#);
        let (status, _) = exchange(&endpoint, &post("wrong-key", &body));
        assert_eq!(status, 401);
        let (status, _) = exchange(&endpoint, &post("secret-key", b"{}"));
        assert_eq!(status, 400);
        endpoint.fail_next(500);
        assert_eq!(exchange(&endpoint, &post("secret-key", &body)).0, 500);
        assert_eq!(
            exchange(&endpoint, &post("secret-key", &body)).0,
            200,
            "once"
        );
        assert_eq!(
            exchange(&endpoint, b"GET /generate HTTP/1.1\r\nHost: x\r\n\r\n").0,
            405
        );
        assert_eq!(
            exchange(&endpoint, b"GET /other HTTP/1.1\r\nHost: x\r\n\r\n").0,
            404
        );
        let recorded = endpoint.requests();
        assert_eq!(recorded.len(), 9);
        let generated = &recorded[2];
        assert_eq!(generated.samples.as_deref(), Some(grid.as_slice()));
        assert_eq!(generated.rgb, Some([199, 217, 255]));
        assert_eq!(generated.header("authorization"), Some("<redacted>"));
        assert!(generated.authorized);
        assert!(!recorded[3].authorized);
        assert!(!format!("{recorded:?}").contains("secret-key"));
        assert!(
            server(&endpoint).requests().is_empty(),
            "the server under the endpoint keeps no request, so no copy of the key"
        );
    }

    /// An endpoint answered in process gives the same answers and keeps the same record, with
    /// header names compared as the server reads them.
    #[test]
    fn an_endpoint_answered_in_process_answers_as_the_server_does() {
        let endpoint = ProofEndpoint::in_process("secret-key", proof_protocol());
        assert_eq!(endpoint.base_url(), IN_PROCESS_ORIGIN);
        assert_eq!(
            endpoint.generate_url(),
            format!("{IN_PROCESS_ORIGIN}/generate")
        );
        let palette = endpoint.answer("GET", PROOF_PALETTE_PATH, &[], b"");
        assert_eq!(
            palette,
            ProofAnswer {
                status: 200,
                content_type: "application/octet-stream",
                body: PROOF_PALETTE.to_vec(),
            }
        );
        let grid: Vec<[u8; 3]> = (0..SAMPLE_GRID_SAMPLES)
            .map(|index| [index as u8, 100, 255])
            .collect();
        let body = sample_grid_body(&grid).unwrap();
        let headers = |key: &str| {
            vec![
                ("Authorization".to_owned(), format!("Bearer {key}")),
                ("Content-Type".to_owned(), "application/json".to_owned()),
            ]
        };
        let generated = endpoint.answer("POST", "/generate", &headers("secret-key"), &body);
        assert_eq!(
            (generated.status, generated.body.as_slice()),
            (200, &br#"{"rgb":[199,217,255]}"#[..])
        );
        assert_eq!(
            endpoint
                .answer("POST", "/generate", &headers("wrong-key"), &body)
                .status,
            401
        );
        let recorded = endpoint.requests();
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[1].header("authorization"), Some("<redacted>"));
        assert_eq!(recorded[1].header("content-type"), Some("application/json"));
        assert!(recorded[1].authorized && !recorded[2].authorized);
        assert!(!format!("{recorded:?}").contains("secret-key"));
    }

    /// Send `request` from a thread of its own, answering the whole response.
    fn sent(endpoint: &ProofEndpoint, request: Vec<u8>) -> thread::JoinHandle<Vec<u8>> {
        let address = server(endpoint).address();
        thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(&request).unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            response
        })
    }

    #[test]
    fn a_held_answer_waits_at_its_gate_and_dropping_the_endpoint_releases_it() {
        let endpoint = ProofEndpoint::start("key", proof_protocol()).unwrap();
        let body = sample_grid_body(&[[10, 20, 30]; SAMPLE_GRID_SAMPLES]).unwrap();
        endpoint.generation().shut();
        let waiting = sent(&endpoint, post("key", &body));
        endpoint
            .generation()
            .wait_reached(1, "the first generation");
        assert!(endpoint.generation().holding(), "its answer is held");
        assert_eq!(
            endpoint.requests().len(),
            1,
            "a held request is already recorded"
        );
        endpoint.generation().open();
        assert!(waiting.join().unwrap().starts_with(b"HTTP/1.1 200"));
        endpoint.generation().shut();
        let waiting = sent(&endpoint, post("key", &body));
        endpoint
            .generation()
            .wait_reached(2, "the second generation");
        drop(endpoint);
        assert!(
            waiting.join().unwrap().starts_with(b"HTTP/1.1 200"),
            "dropping the endpoint released the held answer"
        );
    }

    #[test]
    fn the_palette_gate_holds_only_the_download() {
        let endpoint = ProofEndpoint::start("key", proof_protocol()).unwrap();
        endpoint.palette().shut();
        // A generation is not held by the palette's gate.
        let body = sample_grid_body(&[[10, 20, 30]; SAMPLE_GRID_SAMPLES]).unwrap();
        assert_eq!(exchange(&endpoint, &post("key", &body)).0, 200);
        let waiting = sent(
            &endpoint,
            b"GET /proof-palette.bin HTTP/1.1\r\nHost: x\r\n\r\n".to_vec(),
        );
        endpoint.palette().wait_reached(1, "the download");
        assert!(endpoint.palette().holding(), "the palette answer is held");
        endpoint.palette().open();
        let response = waiting.join().unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        assert!(response.ends_with(&PROOF_PALETTE));
    }

    /// The rendered smoke's delay holds an answer at its gate for at most the delay: it is
    /// answered without the gate being opened. Zero opens the gate.
    #[test]
    fn a_delay_releases_a_held_answer_by_itself() {
        let endpoint = ProofEndpoint::start("key", proof_protocol()).unwrap();
        let body = sample_grid_body(&[[10, 20, 30]; SAMPLE_GRID_SAMPLES]).unwrap();
        endpoint.set_delay(Duration::from_millis(1));
        endpoint.set_palette_delay(Duration::from_millis(1));
        assert!(endpoint.generation().is_shut() && endpoint.palette().is_shut());
        assert_eq!(exchange(&endpoint, &post("key", &body)).0, 200);
        let (status, palette) = exchange(
            &endpoint,
            b"GET /proof-palette.bin HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert_eq!((status, palette.as_slice()), (200, &PROOF_PALETTE[..]));
        endpoint.set_delay(Duration::ZERO);
        endpoint.set_palette_delay(Duration::ZERO);
        assert!(!endpoint.generation().is_shut() && !endpoint.palette().is_shut());
    }
}
