//! Loopback-only browser controls. The browser never carries the audio stream.
use crate::Result;
use qrcodegen::{QrCode, QrCodeEcc};
use roomsync_transport::PairingKey;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const MAX_REQUEST_BYTES: usize = 16_384;
const MAX_BODY_BYTES: usize = 4_096;
const PAGE: &str = include_str!("../ui/index.html");
const SCRIPT: &str = include_str!("../ui/app.js");
const STYLE: &str = include_str!("../ui/style.css");

#[derive(Clone, Serialize)]
struct Interface {
    name: String,
    address: String,
}
#[derive(Serialize)]
struct Device {
    id: String,
    name: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    pub host_ip: Ipv4Addr,
    pub base_port: u16,
    pub receivers: usize,
    pub latency_ms: u64,
    pub capture_device: Option<String>,
}
pub(crate) struct Plan {
    pub settings: Settings,
    pub key: Zeroizing<String>,
}
#[derive(Clone, Serialize)]
pub(crate) struct PeerState {
    pub index: usize,
    pub message: String,
}
#[derive(Clone, Serialize)]
pub(crate) struct PublicState {
    pub phase: String,
    pub title: String,
    pub message: String,
    pub peers: Vec<PeerState>,
    pub packets: u64,
    pub lead_ms: Option<u64>,
    pub capture_allowance_ms: f64,
    pub capture_peak: f32,
    pub source_peak: f32,
    pub capture_age_ms: f64,
    pub receiver_count: usize,
    pub prepared: bool,
    pub windows_supported: bool,
}
pub(crate) type SharedState = Arc<Mutex<PublicState>>;
pub(crate) fn update(state: &SharedState, f: impl FnOnce(&mut PublicState)) {
    if let Ok(mut state) = state.lock() {
        f(&mut state);
    }
}

struct Controller {
    authority: String,
    origin: String,
    csrf: String,
    nonce: String,
    interfaces: Vec<Interface>,
    devices: Vec<Device>,
    state: SharedState,
    prepared: Option<Plan>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    ui_test: bool,
}

impl Controller {
    fn new(port: u16) -> Result<Self> {
        let authority = format!("127.0.0.1:{port}");
        let mut interfaces: Vec<_> = if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|interface| {
                let std::net::IpAddr::V4(ip) = interface.ip() else {
                    return None;
                };
                ((interface.is_oper_up()
                    || interface.oper_status == if_addrs::IfOperStatus::Unknown)
                    && ip.is_private()
                    && !ip.is_link_local())
                .then(|| Interface {
                    name: interface.name,
                    address: ip.to_string(),
                })
            })
            .collect();
        interfaces.sort_by(|a, b| {
            (interface_rank(&a.name), &a.address, &a.name).cmp(&(
                interface_rank(&b.name),
                &b.address,
                &b.name,
            ))
        });
        interfaces.dedup_by(|a, b| a.address == b.address);
        let devices = roomsync_windows::list_render_endpoints()
            .unwrap_or_default()
            .into_iter()
            .map(|d| Device {
                id: d.id,
                name: d.name,
            })
            .collect();
        Ok(Self {
            origin: format!("http://{authority}"),
            authority,
            csrf: random_hex()?,
            nonce: random_hex()?,
            interfaces,
            devices,
            state: Arc::new(Mutex::new(PublicState {
                phase: "idle".into(),
                title: "Ready to set up".into(),
                message: "Create a private setup code. There is no connection deadline.".into(),
                peers: Vec::new(),
                packets: 0,
                lead_ms: None,
                capture_allowance_ms: 0.0,
                capture_peak: 0.0,
                source_peak: 0.0,
                capture_age_ms: 0.0,
                receiver_count: 0,
                prepared: false,
                windows_supported: cfg!(windows),
            })),
            prepared: None,
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
            ui_test: false,
        })
    }
    fn active(&self) -> bool {
        self.worker.as_ref().is_some_and(|w| !w.is_finished())
    }
    fn reap(&mut self) {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && self
                .worker
                .take()
                .is_some_and(|worker| worker.join().is_err())
        {
            update(&self.state, |s| {
                s.phase = "error".into();
                s.title = "Host stopped".into();
                s.message = "The host worker ended unexpectedly. Restart listening.".into();
            });
        }
    }
    fn authorize(&self, request: &Request) -> std::result::Result<(), &'static str> {
        if request.headers.get("host") != Some(&self.authority) {
            return Err("Host not allowed");
        }
        if request.path.starts_with("/api/") {
            if request.headers.get("x-roomsync-csrf") != Some(&self.csrf) {
                return Err("Control token missing or invalid");
            }
            if request
                .headers
                .get("sec-fetch-site")
                .is_some_and(|s| s == "cross-site")
            {
                return Err("Cross-site control is not allowed");
            }
        }
        if request.method == "POST" {
            if request.headers.get("origin") != Some(&self.origin) {
                return Err("Origin not allowed");
            }
            if request
                .headers
                .get("content-type")
                .is_none_or(|s| s.split(';').next() != Some("application/json"))
            {
                return Err("Expected application/json");
            }
        }
        Ok(())
    }
    fn respond(&mut self, request: Request) -> Response {
        if let Err(error) = self.authorize(&request) {
            return Response::error(403, error);
        }
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") => Response::text(
                200,
                "text/html; charset=utf-8",
                PAGE.replace("__NONCE__", &self.nonce)
                    .replace("__CSRF__", &self.csrf),
            ),
            ("GET", "/app.js") => {
                Response::text(200, "application/javascript; charset=utf-8", SCRIPT.into())
            }
            ("GET", "/style.css") => Response::text(200, "text/css; charset=utf-8", STYLE.into()),
            ("GET", "/favicon.ico") => Response::text(204, "image/x-icon", String::new()),
            ("GET", "/api/config") => Response::json(
                200,
                &serde_json::json!({"windows_supported":cfg!(windows) && !self.ui_test,"ui_test":self.ui_test,"interfaces":self.interfaces,"devices":self.devices,"prepared_settings":self.prepared.as_ref().map(|p| &p.settings)}),
            ),
            ("GET", "/api/status") => match self.state.lock() {
                Ok(state) => Response::json(200, &*state),
                Err(_) => Response::error(500, "Controller state unavailable"),
            },
            ("POST", "/api/prepare") => self.prepare(&request.body),
            ("POST", "/api/setup") => self.setup(&request.body),
            ("POST", "/api/start") => self.start(),
            ("POST", "/api/stop") => {
                self.cancel.store(true, Ordering::Release);
                if self.active() {
                    update(&self.state, |s| {
                        s.phase = "stopping".into();
                        s.title = "Stopping safely".into();
                        s.message = "Closing the listeners and audio stream.".into();
                    });
                }
                Response::json(200, &serde_json::json!({"ok":true}))
            }
            _ => Response::error(404, "Not found"),
        }
    }
    fn prepare(&mut self, body: &[u8]) -> Response {
        if self.active() {
            return Response::error(
                409,
                "Stop the current session before preparing another code",
            );
        }
        let settings: Settings = match serde_json::from_slice(body) {
            Ok(s) => s,
            Err(_) => return Response::error(400, "Invalid setup settings"),
        };
        if !(1..=8).contains(&settings.receivers)
            || settings.base_port < 1024
            || u32::from(settings.base_port) + settings.receivers as u32 > 65_536
            || !(40..=250).contains(&settings.latency_ms)
            || !self
                .interfaces
                .iter()
                .any(|i| i.address == settings.host_ip.to_string())
        {
            return Response::error(
                400,
                "Choose a listed LAN address, 1–8 receivers, valid consecutive UDP ports, and a 40–250 ms presentation lead",
            );
        }
        if settings
            .capture_device
            .as_ref()
            .is_some_and(|id| !self.devices.iter().any(|d| &d.id == id))
        {
            return Response::error(400, "Choose a listed capture output");
        }
        let key = match PairingKey::generate() {
            Ok(key) => key.to_hex_for_storage(),
            Err(_) => return Response::error(500, "Secure random source unavailable"),
        };
        let count = settings.receivers;
        self.prepared = Some(Plan { settings, key });
        update(&self.state, |s| {
            s.prepared = true;
            s.receiver_count = count;
            s.phase = "prepared".into();
            s.title = "Your private setup is ready".into();
            s.message = if cfg!(windows) { "Reveal and copy the complete code into Android, then start listening. Android may connect before or after you start." } else { "This cloud/Linux controller can prepare and display setup, but real audio hosting requires Windows. No audio stream is running." }.into();
            s.packets = 0;
            s.lead_ms = None;
            s.capture_allowance_ms = 0.0;
            s.capture_peak = 0.0;
            s.source_peak = 0.0;
            s.capture_age_ms = 0.0;
            s.peers = (0..count)
                .map(|index| PeerState {
                    index,
                    message: "Not listening yet".into(),
                })
                .collect();
        });
        Response::json(200, &serde_json::json!({"ok":true}))
    }
    fn setup(&self, body: &[u8]) -> Response {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Selection {
            index: usize,
        }
        let selection: Selection = match serde_json::from_slice(body) {
            Ok(s) => s,
            Err(_) => return Response::error(400, "Select a receiver"),
        };
        let Some(plan) = &self.prepared else {
            return Response::error(409, "Prepare setup first");
        };
        if selection.index >= plan.settings.receivers {
            return Response::error(400, "Unknown receiver");
        }
        let payload = format!(
            "RS1|{}|{}|5001|{}",
            plan.settings.host_ip,
            plan.settings.base_port + selection.index as u16,
            plan.key.as_str()
        );
        let qr = match QrCode::encode_text(&payload, QrCodeEcc::Medium) {
            Ok(qr) => qr,
            Err(_) => return Response::error(500, "Unable to encode setup QR"),
        };
        let modules: Vec<String> = (0..qr.size())
            .map(|y| {
                (0..qr.size())
                    .map(|x| if qr.get_module(x, y) { '1' } else { '0' })
                    .collect()
            })
            .collect();
        Response::json(
            200,
            &serde_json::json!({"payload":payload,"modules":modules}),
        )
    }
    fn start(&mut self) -> Response {
        self.reap();
        if self.active() {
            return Response::error(409, "Already listening or streaming");
        }
        if !cfg!(windows) || self.ui_test {
            return Response::error(
                400,
                "Real audio hosting requires Windows. This Linux/cloud page is setup-only; no audio has started.",
            );
        }
        let Some(plan) = &self.prepared else {
            return Response::error(409, "Prepare setup first");
        };
        let plan = Plan {
            settings: plan.settings.clone(),
            key: plan.key.clone(),
        };
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let state = self.state.clone();
        update(&state, |s| {
            s.phase = "listening".into();
            s.title = "Waiting for Android".into();
            s.message = "Listening until you press Stop. Paste the complete code into Android and connect whenever ready.".into();
            s.packets = 0;
            s.lead_ms = None;
            s.capture_allowance_ms = 0.0;
            s.capture_peak = 0.0;
            s.source_peak = 0.0;
            s.capture_age_ms = 0.0;
            for peer in &mut s.peers {
                peer.message = "Opening listener".into();
            }
        });
        match thread::Builder::new()
            .name("roomsync-host-controller".into())
            .spawn(move || {
                let result = crate::control_host::run(plan, cancel.clone(), state.clone());
                update(&state, |s| {
                    if cancel.load(Ordering::Acquire) && result.is_ok() {
                        s.phase = "prepared".into();
                        s.title = "Stopped".into();
                        s.message =
                            "The setup code is unchanged. Start listening again whenever ready."
                                .into();
                    } else if let Err(error) = result {
                        s.phase = "error".into();
                        s.title = "Host needs attention".into();
                        s.message = error.to_string();
                    } else {
                        s.phase = "prepared".into();
                        s.title = "Stopped".into();
                        s.message = "Start listening again whenever ready.".into();
                    }
                })
            }) {
            Ok(worker) => {
                self.worker = Some(worker);
                Response::json(200, &serde_json::json!({"ok":true}))
            }
            Err(_) => {
                update(&self.state, |s| {
                    s.phase = "error".into();
                    s.title = "Unable to start".into();
                    s.message = "Could not start the host worker.".into();
                });
                Response::error(500, "Unable to start the host worker")
            }
        }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn interface_rank(name: &str) -> u8 {
    let name = name.to_ascii_lowercase();
    if [
        "vpn",
        "virtual",
        "vethernet",
        "default switch",
        "vmware",
        "vbox",
        "hyper-v",
        "tailscale",
        "wireguard",
        "wsl",
        "docker",
        "tunnel",
    ]
    .iter()
    .any(|word| name.contains(word))
    {
        2
    } else if ["wi-fi", "wifi", "wireless", "ethernet"]
        .iter()
        .any(|word| name.contains(word))
    {
        0
    } else {
        1
    }
}

fn random_hex() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "OS random source unavailable")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}
fn read_request(stream: &mut TcpStream) -> std::io::Result<Request> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = Vec::with_capacity(2048);
    let mut scratch = [0u8; 1024];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::other("request timed out"));
        }
        stream.set_read_timeout(Some(remaining))?;
        let n = stream.read(&mut scratch)?;
        if n == 0 {
            return Err(std::io::Error::other("incomplete request"));
        }
        if bytes.len() + n > MAX_REQUEST_BYTES {
            return Err(std::io::Error::other("request too large"));
        }
        bytes.extend_from_slice(&scratch[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = std::str::from_utf8(&bytes[..end])
                .map_err(|_| std::io::Error::other("invalid request headers"))?;
            let mut lines = head.split("\r\n");
            let first = lines
                .next()
                .ok_or_else(|| std::io::Error::other("missing request line"))?;
            let parts: Vec<_> = first.split(' ').collect();
            if parts.len() != 3
                || !matches!(parts[0], "GET" | "POST")
                || !parts[1].starts_with('/')
                || parts[2] != "HTTP/1.1"
            {
                return Err(std::io::Error::other("unsupported request"));
            }
            let (method, path) = (parts[0].to_owned(), parts[1].to_owned());
            let mut headers = HashMap::new();
            for line in lines {
                let (name, value) = line
                    .split_once(':')
                    .ok_or_else(|| std::io::Error::other("invalid header"))?;
                if name.is_empty()
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    || value.bytes().any(|b| b < 32 && b != b'\t' || b == 127)
                {
                    return Err(std::io::Error::other("invalid header"));
                }
                let name = name.to_ascii_lowercase();
                if headers.insert(name, value.trim().to_owned()).is_some() {
                    return Err(std::io::Error::other("duplicate header"));
                }
            }
            if headers.contains_key("transfer-encoding") {
                return Err(std::io::Error::other("chunked requests are unsupported"));
            }
            let length: usize = match headers.get("content-length") {
                Some(v) => v
                    .parse()
                    .map_err(|_| std::io::Error::other("invalid body length"))?,
                None => 0,
            };
            if length > MAX_BODY_BYTES || method == "GET" && length != 0 {
                return Err(std::io::Error::other("invalid body size"));
            }
            let expected = end + 4 + length;
            if bytes.len() > expected {
                return Err(std::io::Error::other("pipelined requests are unsupported"));
            }
            if bytes.len() == expected {
                return Ok(Request {
                    method,
                    path,
                    headers,
                    body: bytes[end + 4..].to_vec(),
                });
            }
        }
    }
}
struct Response {
    status: u16,
    kind: &'static str,
    body: Vec<u8>,
}
impl Response {
    fn text(status: u16, kind: &'static str, text: String) -> Self {
        Self {
            status,
            kind,
            body: text.into_bytes(),
        }
    }
    fn json(status: u16, value: &impl Serialize) -> Self {
        match serde_json::to_vec(value) {
            Ok(body) => Self {
                status,
                kind: "application/json; charset=utf-8",
                body,
            },
            Err(_) => Self::text(
                500,
                "application/json",
                r#"{"error":"serialization failed"}"#.into(),
            ),
        }
    }
    fn error(status: u16, message: &str) -> Self {
        Self::json(status, &serde_json::json!({"error":message}))
    }
    fn write(self, stream: &mut TcpStream, nonce: &str) -> std::io::Result<()> {
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let label = match self.status {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            409 => "Conflict",
            _ => "Internal Server Error",
        };
        write!(
            stream,
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nPragma: no-cache\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'self' 'nonce-{}'; style-src 'self'; connect-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n\r\n",
            self.status,
            label,
            self.kind,
            self.body.len(),
            nonce
        )?;
        stream.write_all(&self.body)
    }
}

pub fn run(open_browser: bool, ui_test: bool) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let mut controller = Controller::new(listener.local_addr()?.port())?;
    if ui_test {
        controller.ui_test = true;
        controller.interfaces = vec![Interface {
            name: "UI test fixture (not a real network)".into(),
            address: "192.0.2.10".into(),
        }];
        controller.devices.clear();
        update(&controller.state, |s| {
            s.windows_supported = false;
            s.title = "UI test mode".into();
            s.message = "Synthetic setup address only. This mode cannot start network audio or claim a connected receiver.".into();
        });
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    let signal = shutdown.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))?;
    let url = format!("{}/", controller.origin);
    println!(
        "RoomSync controls: {url}\nKeep this console open. Ctrl+C stops listeners, audio and the local controller. Pairing secrets are never printed here."
    );
    #[cfg(windows)]
    if open_browser {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .spawn();
    }
    #[cfg(not(windows))]
    let _ = open_browser;
    while !shutdown.load(Ordering::Acquire) {
        controller.reap();
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                let response = match read_request(&mut stream) {
                    Ok(request) => controller.respond(request),
                    Err(_) => Response::error(400, "Malformed or oversized request"),
                };
                let _ = response.write(&mut stream, &controller.nonce);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(c: &Controller, method: &str, path: &str) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            headers: HashMap::from([
                ("host".into(), c.authority.clone()),
                ("origin".into(), c.origin.clone()),
                ("x-roomsync-csrf".into(), c.csrf.clone()),
                ("content-type".into(), "application/json".into()),
            ]),
            body: b"{}".to_vec(),
        }
    }
    #[test]
    fn control_requires_host_origin_and_token() {
        let c = Controller::new(43210).unwrap();
        let mut r = request(&c, "POST", "/api/start");
        assert!(c.authorize(&r).is_ok());
        r.headers.remove("origin");
        assert!(c.authorize(&r).is_err());
        r.headers.insert("origin".into(), c.origin.clone());
        r.headers.insert("host".into(), "evil.example:43210".into());
        assert!(c.authorize(&r).is_err());
        r.headers.insert("host".into(), c.authority.clone());
        r.headers.remove("x-roomsync-csrf");
        assert!(c.authorize(&r).is_err());
    }
    #[test]
    fn get_cannot_start_or_stop_audio() {
        let mut c = Controller::new(43210).unwrap();
        assert_eq!(c.respond(request(&c, "GET", "/api/start")).status, 404);
        assert_eq!(c.respond(request(&c, "GET", "/api/stop")).status, 404);
        assert!(!c.active());
    }
    #[test]
    fn prepared_key_is_only_in_explicit_private_setup_response() {
        let mut c = Controller::new(43210).unwrap();
        c.interfaces = vec![Interface {
            name: "test".into(),
            address: "192.168.1.8".into(),
        }];
        let mut r = request(&c, "POST", "/api/prepare");
        r.body=br#"{"host_ip":"192.168.1.8","base_port":5000,"receivers":1,"latency_ms":100,"capture_device":null}"#.to_vec();
        assert_eq!(c.respond(r).status, 200);
        let key = c.prepared.as_ref().unwrap().key.to_string();
        let status = c.respond(request(&c, "GET", "/api/status"));
        assert!(!String::from_utf8(status.body).unwrap().contains(&key));
        let mut setup = request(&c, "POST", "/api/setup");
        setup.body = br#"{"index":0}"#.to_vec();
        let response = c.respond(setup);
        assert_eq!(response.status, 200);
        let value: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(value["payload"], format!("RS1|192.168.1.8|5000|5001|{key}"));
        assert!(value["modules"].as_array().unwrap().len() > 20);
    }
}
