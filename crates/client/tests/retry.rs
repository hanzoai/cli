//! What the HTTP wire does when the server is busy or failing: it waits and tries again, as the server
//! said or by a doubling wait, four times at most; a refusal is never made again; a POST is made again after a
//! failure only when it names itself with a `requestId`. A real socket, scripted.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hanzo_client::{Http, Method, Request, Transport};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One scripted answer: a status and headers, or a connection dropped without one.
#[derive(Clone)]
enum Answer {
    Status(u16, &'static str),
    Drop,
}

/// A server that answers each connection from the script (the last for ever), counting them.
struct Server {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    async fn new(script: Vec<Answer>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if socket.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    head.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let length = text
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                socket.read_exact(&mut body).await.ok();
                let n = {
                    let mut seen = log.lock().unwrap();
                    seen.push(String::from_utf8_lossy(&body).to_string());
                    seen.len()
                };
                match script[(n - 1).min(script.len() - 1)].clone() {
                    Answer::Drop => drop(socket),
                    Answer::Status(code, headers) => {
                        let reply = format!(
                            "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n{headers}\r\n{{}}"
                        );
                        socket.write_all(reply.as_bytes()).await.ok();
                    }
                }
            }
        });
        Server { url, seen }
    }

    fn tries(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn bodies(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

fn get(server: &Server) -> Request {
    Request::new(Method::GET, format!("{}/v1/x", server.url))
}

fn post(server: &Server, body: Value) -> Request {
    Request::new(Method::POST, format!("{}/v1/x", server.url)).body(body)
}

#[tokio::test]
async fn a_429_waits_the_retry_after_then_succeeds() {
    let server = Server::new(vec![Answer::Status(429, "Retry-After: 1\r\n"), Answer::Status(200, "")]).await;
    let began = Instant::now();
    let reply = Http::default().send(get(&server)).await.unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(server.tries(), 2);
    assert!(began.elapsed() >= Duration::from_secs(1), "did not wait the Retry-After: {:?}", began.elapsed());
}

#[tokio::test]
async fn a_5xx_that_later_succeeds_is_made_again() {
    for code in [500, 502, 503, 504] {
        let server = Server::new(vec![Answer::Status(code, ""), Answer::Status(200, "")]).await;
        let reply = Http::default().send(get(&server)).await.unwrap();
        assert_eq!((reply.status, server.tries()), (200, 2), "after a {code}");
    }
}

#[tokio::test]
async fn a_dropped_connection_is_made_again() {
    let server = Server::new(vec![Answer::Drop, Answer::Status(200, "")]).await;
    assert_eq!(Http::default().send(get(&server)).await.unwrap().status, 200);
    assert_eq!(server.tries(), 2);
}

#[tokio::test]
async fn a_refusal_is_never_made_again() {
    for code in [400, 401, 403, 404, 409, 422] {
        let server = Server::new(vec![Answer::Status(code, ""), Answer::Status(200, "")]).await;
        let reply = Http::default().send(get(&server)).await.unwrap();
        assert_eq!((reply.status, server.tries()), (code, 1), "a {code}");
    }
}

#[tokio::test]
async fn a_request_that_keeps_failing_is_made_four_times_and_its_last_answer_returned() {
    let server = Server::new(vec![Answer::Status(503, "")]).await;
    let reply = Http::default().send(get(&server)).await.unwrap();
    assert_eq!((reply.status, server.tries()), (503, 4));
}

/// A POST may have been processed before it failed, so it is made again only when it names itself.
#[tokio::test]
async fn a_post_is_made_again_after_a_failure_only_with_a_request_id() {
    let bare = Server::new(vec![Answer::Status(503, ""), Answer::Status(200, "")]).await;
    let reply = Http::default().send(post(&bare, json!({"prompt": "x"}))).await.unwrap();
    assert_eq!((reply.status, bare.tries()), (503, 1));

    let named = Server::new(vec![Answer::Status(503, ""), Answer::Status(200, "")]).await;
    let body = json!({"prompt": "x", "requestId": "r-1"});
    let reply = Http::default().send(post(&named, body)).await.unwrap();
    assert_eq!((reply.status, named.tries()), (200, 2));
    let sent = named.bodies();
    assert_eq!(sent[0], sent[1], "a try carried another body");
    assert!(sent[0].contains("r-1"));
}

/// A 429 was never processed, so even a POST with no id is made again.
#[tokio::test]
async fn a_post_is_made_again_after_a_429() {
    let server = Server::new(vec![Answer::Status(429, "Retry-After: 1\r\n"), Answer::Status(200, "")]).await;
    let reply = Http::default().send(post(&server, json!({"prompt": "x"}))).await.unwrap();
    assert_eq!((reply.status, server.tries()), (200, 2));
}

/// A wait past the hold is not waited for: the 429 is answered as it was.
#[tokio::test]
async fn a_wait_past_the_hold_is_answered_at_once() {
    let server = Server::new(vec![Answer::Status(429, "Retry-After: 3600\r\n"), Answer::Status(200, "")]).await;
    let began = Instant::now();
    let reply = Http::default().send(get(&server)).await.unwrap();
    assert_eq!((reply.status, server.tries()), (429, 1));
    assert!(began.elapsed() < Duration::from_secs(5));
}
