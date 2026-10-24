//! The queue's bound and cancellations, and the two threads' order, over a scripted handler.
use super::*;
use serde_json::json;
use std::{
    io::BufReader,
    sync::mpsc::{Receiver, Sender, channel},
};

fn request(id: i64) -> Request {
    Request {
        id: json!(id),
        method: "work".into(),
        params: json!({}),
    }
}

#[test]
fn the_queue_holds_its_limit_in_order_and_drops_a_cancelled_waiting_request() {
    let queue = Queue::default();
    for id in 0..QUEUE_LIMIT as i64 {
        queue.push(request(id)).unwrap();
    }
    assert_eq!(
        queue.push(request(99)).unwrap_err().id,
        json!(99),
        "a full queue hands the request back"
    );
    assert!(queue.cancel(&json!(1).to_string()));
    assert!(!queue.cancel(&json!(1).to_string()), "already gone");
    let (first, cancel) = queue.pop().unwrap();
    assert_eq!(first.id, json!(0));
    assert!(queue.cancel(&json!(0).to_string()), "the running request");
    assert!(cancel.is_cancelled());
    queue.finish();
    assert!(!queue.cancel(&json!(0).to_string()), "finished");
    assert_eq!(queue.pop().unwrap().0.id, json!(2), "1 was dropped");
    queue.close();
    assert!(queue.pop().is_none(), "closing drops what waits");
    assert!(queue.push(request(5)).is_err());
}

/// Answers each request with `{"answered": id}`, holding a request whose method is `hold` until
/// the test releases it, and saying when it starts one.
struct Scripted {
    started: Sender<Value>,
    release: Receiver<()>,
}

impl Handle for Scripted {
    fn handle(&mut self, request: &Request, cancel: &Cancel) -> Vec<Value> {
        self.started.send(request.id.clone()).unwrap();
        if request.method == "hold" {
            self.release.recv().unwrap();
        }
        assert!(
            request.method == "hold" || !cancel.is_cancelled(),
            "only the held request is cancelled while it runs"
        );
        vec![json!({"answered": request.id})]
    }
}

/// Lines written to the output, one message each, delivered as they are written.
struct Lines(Sender<Value>);

impl Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let _ = self.0.send(serde_json::from_slice(line).unwrap());
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn requests_are_answered_in_order_while_the_reader_refuses_and_cancels_at_once() {
    let (input, mut feed) = std::io::pipe().unwrap();
    let (written, output) = channel();
    let (started, starts) = channel();
    let (release, released) = channel();
    let server = std::thread::spawn(move || {
        serve(
            &mut BufReader::new(input),
            Output::new(Box::new(Lines(written))),
            Subscriptions::default(),
            Scripted {
                started,
                release: released,
            },
        );
    });
    let mut send = |line: &str| writeln!(feed, "{line}").unwrap();
    send(r#"{"jsonrpc":"2.0","id":0,"method":"hold"}"#);
    assert_eq!(
        starts.recv().unwrap(),
        json!(0),
        "the worker holds request 0"
    );
    for id in 1..=QUEUE_LIMIT + 1 {
        send(&format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"work"}}"#));
    }
    let refused = output.recv().unwrap();
    assert_eq!(refused["id"], json!(QUEUE_LIMIT + 1), "{refused}");
    assert_eq!(refused["error"]["code"], json!(QUEUE_FULL));
    assert_eq!(refused["error"]["data"]["limit"], json!(QUEUE_LIMIT));
    // A cancelled waiting request frees its slot and is never answered.
    send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#);
    // A line the reader cannot read is answered at once, ahead of the queue.
    send("{not json");
    assert_eq!(output.recv().unwrap()["error"]["code"], json!(-32700));
    send(&format!(
        r#"{{"jsonrpc":"2.0","id":{},"method":"work"}}"#,
        QUEUE_LIMIT + 2
    ));
    // A line longer than the limit is refused unread.
    let long = format!(r#"{{"x":"{}"}}"#, "a".repeat(LINE_LIMIT));
    send(&long);
    let too_long = output.recv().unwrap();
    assert_eq!(
        too_long["error"]["data"]["limit"],
        json!(LINE_LIMIT),
        "{too_long}"
    );
    assert!(too_long.get("id").is_none());
    release.send(()).unwrap();
    let answered: Vec<Value> = (0..=QUEUE_LIMIT)
        .map(|_| output.recv().unwrap()["answered"].clone())
        .collect();
    let mut expected = vec![json!(0)];
    expected.extend(
        (2..=QUEUE_LIMIT + 2)
            .filter(|id| *id != QUEUE_LIMIT + 1)
            .map(|id| json!(id)),
    );
    assert_eq!(answered, expected);
    drop(feed);
    server.join().unwrap();
    assert!(output.try_recv().is_err(), "nothing more was written");
}

#[test]
fn a_request_cancelled_while_it_runs_is_not_answered() {
    let (input, mut feed) = std::io::pipe().unwrap();
    let (written, output) = channel();
    let (started, starts) = channel();
    let (release, released) = channel();
    let subscriptions = Subscriptions::default();
    subscriptions.open(&json!(9), false, || {});
    let server = {
        let subscriptions = subscriptions.clone();
        std::thread::spawn(move || {
            serve(
                &mut BufReader::new(input),
                Output::new(Box::new(Lines(written))),
                subscriptions,
                Scripted {
                    started,
                    release: released,
                },
            );
        })
    };
    writeln!(feed, r#"{{"jsonrpc":"2.0","id":"held","method":"hold"}}"#).unwrap();
    assert_eq!(starts.recv().unwrap(), json!("held"));
    writeln!(
        feed,
        r#"{{"jsonrpc":"2.0","method":"notifications/cancelled","params":{{"requestId":"held"}}}}"#
    )
    .unwrap();
    // The cancellation of an open subscription closes it.
    writeln!(
        feed,
        r#"{{"jsonrpc":"2.0","method":"notifications/cancelled","params":{{"requestId":9}}}}"#
    )
    .unwrap();
    writeln!(feed, r#"{{"jsonrpc":"2.0","id":"next","method":"work"}}"#).unwrap();
    // The reader answers an unreadable line at once, so its answer proves every line before it
    // was read before the held request is released.
    writeln!(feed, "{{not json").unwrap();
    assert_eq!(output.recv().unwrap()["error"]["code"], json!(-32700));
    release.send(()).unwrap();
    assert_eq!(starts.recv().unwrap(), json!("next"));
    assert_eq!(output.recv().unwrap(), json!({"answered": "next"}));
    drop(feed);
    server.join().unwrap();
    assert!(!lock(&subscriptions.0).contains_key(&json!(9).to_string()));
}
