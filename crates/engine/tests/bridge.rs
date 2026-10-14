//! Bridge client tests against a local stub sync server (never the real
//! network): the envelope shape (never bare GeoJSON), auth headers, 409
//! conflict handling, and the `ovimap.push` / `ovimap.pull` commands.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cadcraft_engine::Session;
use cadcraft_engine::bridge::{BridgeClient, BridgeEnvelope, PushOutcome};
use serde_json::{Value, json};

struct Stub {
    port: u16,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
    /// Bodies received, in order.
    bodies: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    /// `respond` maps a raw request (request line + headers + body) to
    /// `(status, body)`.
    fn new(respond: impl Fn(&str) -> (u16, String) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _ = listener.set_nonblocking(true);
        let respond = Arc::new(respond);
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let bodies2 = Arc::clone(&bodies);
        let stop2 = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut buf = [0u8; 65536];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                        let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                        let (status, resp_body) = respond(&req);
                        seen2.lock().unwrap().push(req);
                        bodies2.lock().unwrap().push(body);
                        let reason = match status {
                            200 => "OK",
                            404 => "Not Found",
                            409 => "Conflict",
                            _ => "Error",
                        };
                        let head = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            resp_body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(resp_body.as_bytes());
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Stub { port, stop, seen, bodies }
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().unwrap().clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

const GEOJSON: &str = r#"{"type":"FeatureCollection","features":[]}"#;

fn client_for(stub: &Stub) -> BridgeClient {
    BridgeClient::new(&format!("http://127.0.0.1:{}", stub.port), "tok-123", "dev-1", "CadCraft Test").unwrap()
}

#[test]
fn push_sends_envelope_not_bare_geojson() {
    let stub = Stub::new(|_| (200, r#"{"code":0,"data":{"rev":7},"message":"ok"}"#.to_string()));
    let client = client_for(&stub);
    let env = client.envelope(3, GEOJSON.to_string(), None);
    let out = client.push("proj 1", &env).unwrap();
    assert_eq!(out, PushOutcome::Updated { rev: 7 });
    let reqs = stub.requests();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].starts_with("PUT /project/proj%201/cadcraft "), "project id is URL-encoded, got {}", reqs[0].lines().next().unwrap_or(""));
    // Header names are case-insensitive on the wire (ureq writes them lowercase).
    assert!(reqs[0].to_lowercase().contains("authorization: bearer tok-123"), "the token travels in the Authorization header");
    assert!(!reqs[0].lines().next().unwrap_or("").contains("tok-123"), "token never in the URL");
    let bodies = stub.bodies();
    assert_eq!(bodies.len(), 1);
    let body: Value = serde_json::from_str(&bodies[0]).unwrap();
    assert!(body.is_object(), "the push body is a JSON envelope object");
    assert_eq!(body["baseRev"], 3);
    assert_eq!(body["geojson"], GEOJSON, "the GeoJSON rides inside the envelope, never bare");
    assert_eq!(body["deviceId"], "dev-1");
    assert_eq!(body["deviceName"], "CadCraft Test");
    assert!(body["updatedAt"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(body.get("dxf").is_none(), "no DXF means no dxf member");
    assert!(!bodies[0].contains("tok-123"), "the token never travels in the body");
}

#[test]
fn push_409_returns_server_copy() {
    let stub = Stub::new(|_| (409, r#"{"code":409,"data":{"rev":9,"geojson":"SERVER","dxf":"RFhG"},"message":"baseRev mismatch"}"#.to_string()));
    let client = client_for(&stub);
    let env = client.envelope(3, GEOJSON.to_string(), None);
    let out = client.push("p1", &env).unwrap();
    match out {
        PushOutcome::Conflict { server_rev, server_geojson, server_dxf } => {
            assert_eq!(server_rev, 9);
            assert_eq!(server_geojson.as_deref(), Some("SERVER"));
            assert_eq!(server_dxf.as_deref(), Some(b"DXF".as_slice()));
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn push_409_in_200_envelope_is_conflict_too() {
    let stub = Stub::new(|_| (200, r#"{"code":409,"data":{"rev":4,"geojson":"G"},"message":"stale"}"#.to_string()));
    let client = client_for(&stub);
    let env = client.envelope(1, GEOJSON.to_string(), None);
    let out = client.push("p1", &env).unwrap();
    assert!(matches!(out, PushOutcome::Conflict { server_rev: 4, .. }), "code 409 in a 200 response is still a conflict");
}

#[test]
fn pull_returns_server_copy() {
    let stub = Stub::new(|_| {
        (200, r#"{"code":0,"data":{"rev":5,"updatedAt":"2026-10-14T00:00:00Z","geojson":"G2","dxf":"WFhz"},"message":"ok"}"#.to_string())
    });
    let client = client_for(&stub);
    let pulled = client.pull("p1").unwrap();
    assert_eq!(pulled.rev, 5);
    assert_eq!(pulled.updated_at, "2026-10-14T00:00:00Z");
    assert_eq!(pulled.geojson, "G2");
    assert_eq!(pulled.dxf.as_deref(), Some(b"XXs".as_slice()));
    let reqs = stub.requests();
    assert!(reqs[0].starts_with("GET /project/p1/cadcraft "), "pull is a GET on the same endpoint");
}

#[test]
fn client_rejects_missing_token_and_bad_url() {
    assert!(BridgeClient::new("http://127.0.0.1:1", "", "d", "n").is_err(), "no token, no client");
    assert!(BridgeClient::new("ftp://example.com", "t", "d", "n").is_err(), "only http(s) base URLs");
    assert!(BridgeClient::new("http://example.com/", "t", "", "n").is_err(), "device id is required");
    assert!(BridgeClient::new("http://example.com/", "t", "d", "n").is_ok());
}

#[test]
fn envelope_serializes_camel_case() {
    let env = BridgeEnvelope::new(2, GEOJSON.to_string(), Some(b"DXF".to_vec()), "d", "n");
    let text = serde_json::to_string(&env).unwrap();
    assert!(text.contains("\"baseRev\":2"), "camelCase baseRev, got {text}");
    assert!(text.contains("\"deviceId\":\"d\""));
    assert!(text.contains("\"dxf\":\"RFhG\""), "DXF rides base64 in the envelope");
    assert_eq!(env.dxf_bytes().as_deref(), Some(b"DXF".as_slice()));
}

#[test]
fn command_push_and_pull_roundtrip() {
    let stub = Stub::new(|req| {
        if req.starts_with("PUT") {
            (200, r#"{"code":0,"data":{"rev":11},"message":"ok"}"#.to_string())
        } else {
            (200, r#"{"code":0,"data":{"rev":11,"updatedAt":"2026-10-14T00:00:00Z","geojson":"PULLED"},"message":"ok"}"#.to_string())
        }
    });
    let base = format!("http://127.0.0.1:{}", stub.port);
    let mut s = Session::new();
    let pushed = s
        .execute(
            "ovimap.push",
            &json!({"projectId": "p1", "baseRev": 10, "geojson": GEOJSON, "baseUrl": base, "token": "tok-1", "deviceId": "d", "deviceName": "n"}),
        )
        .unwrap();
    assert_eq!(pushed["outcome"], "updated");
    assert_eq!(pushed["rev"], 11);
    let pulled = s.execute("ovimap.pull", &json!({"projectId": "p1", "baseUrl": base, "token": "tok-1"})).unwrap();
    assert_eq!(pulled["rev"], 11);
    assert_eq!(pulled["geojson"], "PULLED");
    assert_eq!(pulled["hasDxf"], false);
}

#[test]
fn command_push_conflict_reports_server_copy() {
    let stub = Stub::new(|_| (409, r#"{"code":409,"data":{"rev":8,"geojson":"SERVER"},"message":"stale"}"#.to_string()));
    let base = format!("http://127.0.0.1:{}", stub.port);
    let mut s = Session::new();
    let out = s.execute("ovimap.push", &json!({"projectId": "p1", "baseRev": 1, "geojson": GEOJSON, "baseUrl": base, "token": "tok-1"})).unwrap();
    assert_eq!(out["outcome"], "conflict");
    assert_eq!(out["serverRev"], 8);
    assert_eq!(out["serverGeojson"], "SERVER");
}
