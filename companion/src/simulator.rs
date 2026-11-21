use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::protocol::{DeviceEvent, PROTO_VERSION};
use crate::transport::DeviceTransport;

/// Default localhost port for `--simulate` mode.
pub const DEFAULT_SIMULATOR_PORT: u16 = 47291;

const MAX_PACKET_HISTORY: usize = 128;
const MAX_HTTP_REQUEST_BYTES: usize = 16 * 1024;
const CONNECTION_STALE_AFTER: Duration = Duration::from_secs(3);

const SIMULATOR_HTML: &str = include_str!("simulator.html");

#[derive(Debug, Clone, Serialize)]
struct StoredPacket {
    seq: u64,
    bytes: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct PacketsResponse {
    reset: bool,
    seq: u64,
    packets: Vec<StoredPacket>,
}

#[derive(Debug, Deserialize)]
struct IncomingEventBody {
    bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub(crate) struct SimState {
    next_seq: u64,
    tx_history: VecDeque<StoredPacket>,
    rx_events: VecDeque<Vec<u8>>,
    last_seen: Option<Instant>,
}

type SharedSimState = Arc<Mutex<SimState>>;

/// Local HTTP loopback bridge presenting a browser-based pixel-accurate `128×64` desk unit.
pub struct SimulatorTransport {
    port: u16,
    state: SharedSimState,
}

impl SimulatorTransport {
    /// Starts the simulator HTTP server on `127.0.0.1:<port>`.
    pub fn start(port: u16) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .map_err(|e| format!("Failed to bind simulator on 127.0.0.1:{port}: {e}"))?;
        let state: SharedSimState = Arc::new(Mutex::new(SimState::default()));
        let bg_state = Arc::clone(&state);

        thread::Builder::new()
            .name("sim-http".to_string())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let s = Arc::clone(&bg_state);
                    let _ = handle_connection(stream, &s);
                }
            })
            .map_err(|e| format!("Failed to spawn simulator HTTP thread: {e}"))?;

        Ok(Self { port, state })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl DeviceTransport for SimulatorTransport {
    fn label(&self) -> String {
        format!("Simulator ({})", self.url())
    }

    fn is_connected(&self) -> bool {
        let Ok(guard) = self.state.lock() else {
            return false;
        };
        guard
            .last_seen
            .is_some_and(|t| t.elapsed() <= CONNECTION_STALE_AFTER)
    }

    fn send_packet(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| "simulator state lock poisoned".to_string())?;
        guard.next_seq += 1;
        let seq = guard.next_seq;
        if guard.tx_history.len() == MAX_PACKET_HISTORY {
            guard.tx_history.pop_front();
        }
        guard.tx_history.push_back(StoredPacket {
            seq,
            bytes: bytes.to_vec(),
        });
        Ok(())
    }

    fn poll_events(&mut self) -> Vec<Vec<u8>> {
        let Ok(mut guard) = self.state.lock() else {
            return Vec::new();
        };
        guard.rx_events.drain(..).collect()
    }
}

pub(crate) fn handle_http_request(
    method: &str,
    target: &str,
    body: &[u8],
    state: &SharedSimState,
) -> (u16, &'static str, Vec<u8>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => (
            200,
            "text/html; charset=utf-8",
            SIMULATOR_HTML.as_bytes().to_vec(),
        ),
        ("GET", "/api/packets") => {
            let since = parse_query_u64(query, "since").unwrap_or(0);
            let Ok(mut guard) = state.lock() else {
                return (500, "text/plain; charset=utf-8", b"Internal Error".to_vec());
            };
            guard.last_seen = Some(Instant::now());

            // Browser has a higher sequence number (companion restarted) -> ask it to reset to 0.
            if since > guard.next_seq {
                let payload = PacketsResponse {
                    reset: true,
                    seq: guard.next_seq,
                    packets: Vec::new(),
                };
                let json = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
                return (200, "application/json", json);
            }

            if since == 0 {
                guard.rx_events.push_back(
                    DeviceEvent::Hello {
                        proto_ver: PROTO_VERSION,
                        battery_pct: 95,
                    }
                    .encode(),
                );
            }

            let packets: Vec<StoredPacket> = guard
                .tx_history
                .iter()
                .filter(|p| p.seq > since)
                .cloned()
                .collect();
            let payload = PacketsResponse {
                reset: false,
                seq: guard.next_seq,
                packets,
            };
            let json = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
            (200, "application/json", json)
        }
        ("POST", "/api/event") => {
            let Ok(parsed) = serde_json::from_slice::<IncomingEventBody>(body) else {
                return (400, "text/plain; charset=utf-8", b"Invalid JSON".to_vec());
            };
            if parsed.bytes.is_empty() || parsed.bytes.len() > 32 {
                return (
                    400,
                    "text/plain; charset=utf-8",
                    b"Invalid packet length".to_vec(),
                );
            }
            let Ok(mut guard) = state.lock() else {
                return (500, "text/plain; charset=utf-8", b"Internal Error".to_vec());
            };
            guard.last_seen = Some(Instant::now());
            guard.rx_events.push_back(parsed.bytes);
            (200, "application/json", br#"{"ok":true}"#.to_vec())
        }
        _ => (404, "text/plain; charset=utf-8", b"Not Found".to_vec()),
    }
}

fn parse_query_u64(query: &str, key: &str) -> Option<u64> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == key
        {
            return v.parse::<u64>().ok();
        }
    }
    None
}

fn handle_connection(mut stream: TcpStream, state: &SharedSimState) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));

    let mut raw = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let header_end;

    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        raw.extend_from_slice(&chunk[..n]);
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
        if raw.len() >= MAX_HTTP_REQUEST_BYTES {
            write_http_response(&mut stream, 400, "text/plain; charset=utf-8", b"Too Large")?;
            return Ok(());
        }
    }

    let (method, target, content_len) = {
        let head = String::from_utf8_lossy(&raw[..header_end]);
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("/").to_string();

        let mut content_len = 0usize;
        for line in lines {
            if let Some((name, val)) = line.split_once(':')
                && name.trim().eq_ignore_ascii_case("content-length")
            {
                content_len = val.trim().parse::<usize>().unwrap_or(0);
            }
        }
        (method, target, content_len)
    };

    if content_len > MAX_HTTP_REQUEST_BYTES {
        write_http_response(&mut stream, 413, "text/plain; charset=utf-8", b"Too Large")?;
        return Ok(());
    }

    while raw.len() < header_end + content_len {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        if raw.len() > MAX_HTTP_REQUEST_BYTES + header_end {
            break;
        }
    }

    let body_end = (header_end + content_len).min(raw.len());
    let body = &raw[header_end..body_end];
    let (status, content_type, resp_body) = handle_http_request(&method, &target, body, state);
    write_http_response(&mut stream, status, content_type, &resp_body)
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{DeviceButton, HostPacket, PowerMode};

    #[test]
    fn serves_html_on_root_and_404_on_unknown() {
        let state: SharedSimState = Arc::new(Mutex::new(SimState::default()));

        let (code, ctype, body) = handle_http_request("GET", "/", b"", &state);
        assert_eq!(code, 200);
        assert!(ctype.starts_with("text/html"));
        let html = String::from_utf8(body).unwrap();
        assert!(html.contains("<canvas id=\"oled\""));

        let (not_found, _, _) = handle_http_request("GET", "/nope", b"", &state);
        assert_eq!(not_found, 404);
    }

    #[test]
    fn packets_endpoint_triggers_hello_on_since_zero_and_streams_deltas() {
        let state: SharedSimState = Arc::new(Mutex::new(SimState::default()));

        // Seed 2 outbound host packets
        {
            let mut guard = state.lock().unwrap();
            guard.next_seq = 2;
            guard.tx_history.push_back(StoredPacket {
                seq: 1,
                bytes: HostPacket::PowerHint {
                    mode: PowerMode::Awake,
                }
                .encode(),
            });
            guard.tx_history.push_back(StoredPacket {
                seq: 2,
                bytes: HostPacket::PowerHint {
                    mode: PowerMode::Dim,
                }
                .encode(),
            });
        }

        let (code, _, body) = handle_http_request("GET", "/api/packets?since=0", b"", &state);
        assert_eq!(code, 200);
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("\"seq\":2"));
        assert!(text.contains("\"reset\":false"));

        // since=0 queued a Hello event
        {
            let mut guard = state.lock().unwrap();
            let ev = guard.rx_events.pop_front().unwrap();
            assert_eq!(
                DeviceEvent::decode(&ev).unwrap(),
                DeviceEvent::Hello {
                    proto_ver: PROTO_VERSION,
                    battery_pct: 95
                }
            );
        }

        // Subsequent poll since=1 returns only packet 2
        let (_, _, delta_body) = handle_http_request("GET", "/api/packets?since=1", b"", &state);
        let val: serde_json::Value = serde_json::from_slice(&delta_body).unwrap();
        assert_eq!(val["packets"].as_array().unwrap().len(), 1);
        assert_eq!(val["packets"][0]["seq"], 2);
    }

    #[test]
    fn event_endpoint_accepts_raw_device_bytes_and_rejects_invalid_payloads() {
        let state: SharedSimState = Arc::new(Mutex::new(SimState::default()));

        let raw = DeviceEvent::Button(DeviceButton::Next).encode();
        let payload = serde_json::to_vec(&serde_json::json!({ "bytes": raw })).unwrap();
        let (ok_code, _, _) = handle_http_request("POST", "/api/event", &payload, &state);
        assert_eq!(ok_code, 200);

        let queued = state.lock().unwrap().rx_events.pop_front().unwrap();
        assert_eq!(
            DeviceEvent::decode(&queued).unwrap(),
            DeviceEvent::Button(DeviceButton::Next)
        );

        let (bad_json, _, _) = handle_http_request("POST", "/api/event", b"not json", &state);
        assert_eq!(bad_json, 400);

        let (empty_bytes, _, _) =
            handle_http_request("POST", "/api/event", br#"{"bytes":[]}"#, &state);
        assert_eq!(empty_bytes, 400);
    }
}
