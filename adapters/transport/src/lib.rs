//! Authenticated, fixed-peer UDP sessions for RoomSync.
//!
//! Noise NNpsk0 supplies a standard mutually authenticated handshake with a random
//! 256-bit pre-shared key, ephemeral X25519 and ChaCha20-Poly1305. Never use a
//! password or short PIN as the pairing key. Packet and audio sequence replay
//! windows allow bounded reordering. Socket I/O belongs on a worker thread, never
//! a real-time audio callback. There is no internal audio queue.
#![forbid(unsafe_code)]

use roomsync_core::{AudioPacket, SESSION_PACKET_BYTES, clock::ClockExchange};
use snow::{Builder, HandshakeState, StatelessTransportState};
use std::{
    fmt, io,
    net::{SocketAddr, UdpSocket},
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

const PATTERN: &str = "Noise_NNpsk0_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"RoomSync UDP v1 / PCM16LE 48000Hz stereo 120frames";
const MAGIC: &[u8; 4] = b"RSN1";
const HELLO: u8 = 1;
const WELCOME: u8 = 2;
const DATA: u8 = 3;
const READY: u8 = 1;
const ACCEPTED: u8 = 2;
const AUDIO: u8 = 3;
const CLOCK_REQUEST: u8 = 4;
const CLOCK_REPLY: u8 = 5;
const CLOSE: u8 = 6;
const HEARTBEAT: u8 = 7;
const SET_TIMELINE: u8 = 8;
const TIMELINE_ACK: u8 = 9;
const RECEIVER_READINESS: u8 = 10;
const READINESS_ACK: u8 = 11;

/// Maximum negotiated send lead, leaving 20ms headroom in the 320ms core window.
pub const MAX_NEGOTIATED_LEAD_NS: u64 = 300_000_000;

mod pairing;
pub use pairing::{PairingListener, PairingProgress, PairingStage};
const HEADER: usize = 13;
const MAX_PLAIN: usize = 1 + SESSION_PACKET_BYTES;
const MAX_PENDING_CLOCKS: usize = 8;
const MAX_READS_PER_POLL: usize = 64;
/// Full UDP payload cap; the 512-byte core audio record becomes 542 bytes.
pub const MAX_DATAGRAM_BYTES: usize = HEADER + MAX_PLAIN + 16;
pub const AUDIO_DATAGRAM_BYTES: usize = MAX_DATAGRAM_BYTES;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidConfig(&'static str),
    InvalidKey,
    Crypto,
    HandshakeTimeout,
    IdleTimeout,
    Closed,
    WrongRole,
    WrongSession,
    PendingClockLimit,
    SequenceExhausted,
    ClockUnavailable,
    Cancelled,
    TimelineNotReady,
    ReceiverNotReady,
    ReadinessChanged,
    IncompatibleProtocol,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "UDP I/O: {e}"),
            Self::InvalidConfig(s) => write!(f, "invalid transport configuration: {s}"),
            Self::InvalidKey => {
                f.write_str("pairing key must be exactly 64 hexadecimal characters")
            }
            Self::Crypto => f.write_str("cryptographic operation failed"),
            Self::HandshakeTimeout => {
                f.write_str("authenticated pairing timed out; check peer addresses and pairing key")
            }
            Self::IdleTimeout => f.write_str("peer stopped sending authenticated packets"),
            Self::Closed => f.write_str("session is closed"),
            Self::WrongRole => f.write_str("operation is not valid for this session role"),
            Self::WrongSession => f.write_str("audio packet belongs to another session"),
            Self::PendingClockLimit => f.write_str("too many outstanding clock requests"),
            Self::Cancelled => f.write_str("pairing cancelled"),
            Self::TimelineNotReady => {
                f.write_str("waiting for an authenticated playback timeline acknowledgment")
            }
            Self::ReceiverNotReady => f.write_str("waiting for authenticated receiver output calibration"),
            Self::ReadinessChanged => f.write_str("receiver output timing changed after scheduling; stop and reconnect to calibrate again"),
            Self::IncompatibleProtocol => f.write_str("peer uses a different audio-readiness protocol; update both apps to the same release"),
            Self::ClockUnavailable => {
                f.write_str("monotonic hardware clock unavailable; session must stop")
            }
            Self::SequenceExhausted => {
                f.write_str("packet counter exhausted; establish a new session")
            }
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<snow::Error> for Error {
    fn from(_: snow::Error) -> Self {
        Self::Crypto
    }
}
pub type Result<T> = std::result::Result<T, Error>;

/// This wrapper is redacted in Debug and erased on drop. The cryptographic
/// dependency also holds internal copies; whole-process erasure is not guaranteed.
/// It authenticates a
/// pair, not a user identity. Revoke access by creating a new key on both peers.
pub struct PairingKey(Zeroizing<[u8; 32]>);
impl fmt::Debug for PairingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingKey([REDACTED])")
    }
}
impl PairingKey {
    pub fn generate() -> Result<Self> {
        let mut bytes = Zeroizing::new([0; 32]);
        getrandom::fill(bytes.as_mut()).map_err(|_| Error::Crypto)?;
        Ok(Self(bytes))
    }
    pub fn from_hex(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.len() != 64 {
            return Err(Error::InvalidKey);
        }
        let mut bytes = Zeroizing::new([0; 32]);
        for (i, pair) in text.as_bytes().chunks_exact(2).enumerate() {
            let a = hex_digit(pair[0]).ok_or(Error::InvalidKey)?;
            let b = hex_digit(pair[1]).ok_or(Error::InvalidKey)?;
            bytes[i] = a * 16 + b;
        }
        Ok(Self(bytes))
    }
    /// Explicit export only for writing a new private pairing-key file. Do not log.
    pub fn to_hex_for_storage(&self) -> Zeroizing<String> {
        use std::fmt::Write;
        let mut out = Zeroizing::new(String::with_capacity(64));
        for byte in self.0.iter() {
            let _ = write!(&mut *out, "{byte:02x}");
        }
        out
    }
}
fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct MonotonicClock {
    origin: Instant,
    source: Option<fn() -> Result<u64>>,
}
impl Default for MonotonicClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            source: None,
        }
    }
}
impl MonotonicClock {
    /// Use an existing monotonic clock, such as Windows QPC nanoseconds, so
    /// transport and hardware presentation timestamps occupy the SAME domain.
    /// The source must never move backward.
    pub fn from_source(source: fn() -> Result<u64>) -> Self {
        Self {
            origin: Instant::now(),
            source: Some(source),
        }
    }
    /// All components of one endpoint MUST share this clock (or its clone).
    pub fn now_ns(&self) -> Result<u64> {
        self.source.map_or_else(
            || Ok(self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64),
            |source| source(),
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub handshake_timeout: Duration,
    pub idle_timeout: Duration,
    pub retry_interval: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(3),
            retry_interval: Duration::from_millis(100),
        }
    }
}
impl Config {
    fn validate(self) -> Result<Self> {
        if self.handshake_timeout.is_zero()
            || self.handshake_timeout > Duration::from_secs(60)
            || self.idle_timeout.is_zero()
            || self.idle_timeout > Duration::from_secs(60)
            || self.retry_interval.is_zero()
            || self.retry_interval > self.handshake_timeout
        {
            return Err(Error::InvalidConfig(
                "timeouts must be positive and at most 60 seconds; retry must fit handshake",
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: u64,
    /// Host monotonic nanoseconds when audio frame zero should reach the DAC.
    pub epoch_host_ns: u64,
}
/// Measured receiver write-ahead and one render block, both in nanoseconds.
/// This is telemetry, not a request to allocate a buffer of the supplied size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverReadiness {
    pub output_horizon_ns: u64,
    pub render_block_ns: u64,
}
impl ReceiverReadiness {
    pub fn validate(self) -> Result<Self> {
        if self.output_horizon_ns > 1_000_000_000
            || self.render_block_ns == 0
            || self.render_block_ns > 100_000_000
        {
            return Err(Error::InvalidConfig(
                "receiver output horizon must be 0..=1s and render block 1ns..=100ms",
            ));
        }
        Ok(self)
    }
    pub fn minimum_lead_ns(self) -> Result<u64> {
        let checked = self.validate()?;
        checked
            .output_horizon_ns
            .checked_add(checked.render_block_ns)
            .ok_or(Error::InvalidConfig("receiver timing budget overflow"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Host,
    Receiver,
}
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Fixed inline audio avoids per-packet heap allocation.
pub enum Event {
    Audio(AudioPacket),
    ClockSample(ClockExchange),
    Timeline(SessionInfo),
    TimelineAcknowledged(SessionInfo),
    ReceiverReady(ReceiverReadiness),
    ReadinessAcknowledged(ReceiverReadiness),
    Heartbeat,
    Closed,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub audio_received: u64,
    pub authentication_rejected: u64,
    pub source_rejected: u64,
    pub malformed_rejected: u64,
    pub replay_rejected: u64,
    pub wrong_session_rejected: u64,
}

/// A fixed, explicitly configured peer. No discovery, broadcast, implicit public
/// listener or unauthenticated fallback is provided.
pub struct Endpoint {
    socket: UdpSocket,
    peer: SocketAddr,
    key: PairingKey,
    config: Config,
}
impl Endpoint {
    pub fn bind(
        local: SocketAddr,
        peer: SocketAddr,
        key: PairingKey,
        config: Config,
    ) -> Result<Self> {
        Self::from_socket(UdpSocket::bind(local)?, peer, key, config)
    }
    pub fn from_socket(
        socket: UdpSocket,
        peer: SocketAddr,
        key: PairingKey,
        config: Config,
    ) -> Result<Self> {
        if peer.port() == 0
            || peer.ip().is_unspecified()
            || peer.ip().is_multicast()
            || matches!(peer.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast())
        {
            return Err(Error::InvalidConfig(
                "peer must have a concrete unicast IP address and nonzero UDP port",
            ));
        }
        if socket.local_addr()?.is_ipv4() != peer.is_ipv4() {
            return Err(Error::InvalidConfig(
                "local and peer address families differ",
            ));
        }
        socket.set_nonblocking(false)?;
        Ok(Self {
            socket,
            peer,
            key,
            config: config.validate()?,
        })
    }
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    /// Wait for the designated receiver. Start lead time is negotiated under
    /// authentication. Allow enough lead time for at least one second of clock sync.
    pub fn accept(self, clock: MonotonicClock, start_lead: Duration) -> Result<Session> {
        self.accept_inner(clock, start_lead, None)
    }

    /// Establish another fixed-peer transport for a shared multi-speaker timeline.
    /// Use a freshly random, nonzero session id and a common future host epoch;
    /// each peer still negotiates independent Noise keys and replay counters.
    pub fn accept_with_info(self, clock: MonotonicClock, info: SessionInfo) -> Result<Session> {
        let now = clock.now_ns()?;
        if info.session_id == 0
            || info.epoch_host_ns <= now
            || info.epoch_host_ns.saturating_sub(now) > 60_000_000_000
        {
            return Err(Error::InvalidConfig(
                "shared session needs a nonzero id and an epoch within the next 60 seconds",
            ));
        }
        self.accept_inner(clock, Duration::ZERO, Some(info))
    }

    fn accept_inner(
        self,
        clock: MonotonicClock,
        start_lead: Duration,
        fixed_info: Option<SessionInfo>,
    ) -> Result<Session> {
        if start_lead > Duration::from_secs(60) {
            return Err(Error::InvalidConfig("start lead exceeds 60 seconds"));
        }
        let deadline = Instant::now() + self.config.handshake_timeout;
        let mut buf = [0; MAX_DATAGRAM_BYTES + 1];
        loop {
            let Some(n) = read_until(
                &self.socket,
                self.peer,
                &mut buf,
                deadline,
                self.config.retry_interval,
            )?
            else {
                if Instant::now() >= deadline {
                    return Err(Error::HandshakeTimeout);
                }
                continue;
            };
            if !is_frame(&buf[..n], HELLO) {
                continue;
            }
            let mut state = handshake(&self.key, false)?;
            let mut plain = [0; MAX_PLAIN];
            let Ok(p) = state.read_message(&buf[5..n], &mut plain) else {
                continue;
            };
            if &plain[..p] != b"RoomSync receiver v1" {
                continue;
            }
            let hello = buf[..n].to_vec(); // One bounded handshake retry record.
            let mut random = [0; 8];
            getrandom::fill(&mut random).map_err(|_| Error::Crypto)?;
            let info = fixed_info.unwrap_or(SessionInfo {
                session_id: u64::from_be_bytes(random).max(1),
                epoch_host_ns: clock
                    .now_ns()?
                    .checked_add(start_lead.as_nanos() as u64)
                    .ok_or(Error::SequenceExhausted)?,
            });
            let mut welcome_payload = [0; 24];
            welcome_payload[..8].copy_from_slice(b"RSACPT01");
            welcome_payload[8..16].copy_from_slice(&info.session_id.to_be_bytes());
            welcome_payload[16..24].copy_from_slice(&info.epoch_host_ns.to_be_bytes());
            let welcome = handshake_frame(&mut state, WELCOME, &welcome_payload)?;
            self.socket.send_to(&welcome, self.peer)?;
            let crypto = state.into_stateless_transport_mode()?;
            let mut session = Session::new(self, clock, Role::Host, info, crypto);
            let mut last_welcome = Instant::now();
            // Complete key confirmation before allowing application audio.
            loop {
                if Instant::now() >= deadline {
                    return Err(Error::HandshakeTimeout);
                }
                let Some(n) = read_until(
                    &session.socket,
                    session.peer,
                    &mut buf,
                    deadline,
                    session
                        .config
                        .retry_interval
                        .saturating_sub(last_welcome.elapsed()),
                )?
                else {
                    session.socket.send_to(&welcome, session.peer)?;
                    last_welcome = Instant::now();
                    continue;
                };
                if buf[..n] == hello {
                    if last_welcome.elapsed() >= session.config.retry_interval {
                        session.socket.send_to(&welcome, session.peer)?;
                        last_welcome = Instant::now();
                    }
                    continue;
                }
                if session.confirm(&buf[..n], READY, &mut plain) {
                    if fixed_info.is_some() && session.clock.now_ns()? >= info.epoch_host_ns {
                        return Err(Error::InvalidConfig(
                            "shared session epoch elapsed during pairing; negotiate a new timeline",
                        ));
                    }
                    let accepted = session.seal(&[ACCEPTED])?;
                    session.socket.send_to(&accepted, session.peer)?;
                    session.confirmation = Some(Confirmation {
                        ready: buf[..n].to_vec(),
                        accepted,
                        expires: Instant::now() + session.config.handshake_timeout,
                        last_reply: Instant::now(),
                    });
                    session.last_authenticated = Instant::now();
                    return Ok(session);
                }
            }
        }
    }

    pub fn join(self, clock: MonotonicClock) -> Result<Session> {
        let deadline = Instant::now() + self.config.handshake_timeout;
        let mut state = handshake(&self.key, true)?;
        let hello = handshake_frame(&mut state, HELLO, b"RoomSync receiver v1")?;
        let mut buf = [0; MAX_DATAGRAM_BYTES + 1];
        let mut plain = [0; MAX_PLAIN];
        self.socket.send_to(&hello, self.peer)?;
        let mut last_hello = Instant::now();
        loop {
            if Instant::now() >= deadline {
                return Err(Error::HandshakeTimeout);
            }
            let Some(n) = read_until(
                &self.socket,
                self.peer,
                &mut buf,
                deadline,
                self.config
                    .retry_interval
                    .saturating_sub(last_hello.elapsed()),
            )?
            else {
                self.socket.send_to(&hello, self.peer)?;
                last_hello = Instant::now();
                continue;
            };
            if !is_frame(&buf[..n], WELCOME) {
                continue;
            }
            // A failed Noise read may mutate handshake state. Rebuild and replay
            // our original first message using the SAME ephemeral key is not safe
            // to improvise. Fail closed, letting the caller start a fresh attempt.
            let n = state
                .read_message(&buf[5..n], &mut plain)
                .map_err(|_| Error::Crypto)?;
            if n != 24 || &plain[..8] != b"RSACPT01" {
                return Err(Error::Crypto);
            }
            let info = SessionInfo {
                session_id: read_u64(&plain[8..16]),
                epoch_host_ns: read_u64(&plain[16..24]),
            };
            if info.session_id == 0 {
                return Err(Error::Crypto);
            }
            let crypto = state.into_stateless_transport_mode()?;
            let mut session = Session::new(self, clock, Role::Receiver, info, crypto);
            let ready = session.seal(&[READY])?;
            session.socket.send_to(&ready, session.peer)?;
            let mut last_ready = Instant::now();
            loop {
                if Instant::now() >= deadline {
                    return Err(Error::HandshakeTimeout);
                }
                let Some(n) = read_until(
                    &session.socket,
                    session.peer,
                    &mut buf,
                    deadline,
                    session
                        .config
                        .retry_interval
                        .saturating_sub(last_ready.elapsed()),
                )?
                else {
                    session.socket.send_to(&ready, session.peer)?;
                    last_ready = Instant::now();
                    continue;
                };
                if session.confirm(&buf[..n], ACCEPTED, &mut plain) {
                    session.last_authenticated = Instant::now();
                    return Ok(session);
                }
            }
        }
    }
}

fn handshake(key: &PairingKey, initiator: bool) -> Result<HandshakeState> {
    let builder = Builder::new(PATTERN.parse()?)
        .prologue(PROLOGUE)?
        .psk(0, &key.0)?;
    Ok(if initiator {
        builder.build_initiator()?
    } else {
        builder.build_responder()?
    })
}
fn handshake_frame(state: &mut HandshakeState, kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let mut out = [0; MAX_DATAGRAM_BYTES];
    out[..4].copy_from_slice(MAGIC);
    out[4] = kind;
    let n = state.write_message(payload, &mut out[5..])?;
    Ok(out[..n + 5].to_vec())
}
fn is_frame(bytes: &[u8], kind: u8) -> bool {
    bytes.len() >= 5
        && bytes.len() <= MAX_DATAGRAM_BYTES
        && &bytes[..4] == MAGIC
        && bytes[4] == kind
}
fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes(bytes[..8].try_into().expect("validated eight-byte field"))
}
// Windows reports WSAEMSGSIZE instead of returning a truncated UDP payload.
fn oversized_datagram(e: &io::Error) -> bool {
    cfg!(windows) && e.raw_os_error() == Some(10040)
}
fn transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}
fn read_until(
    socket: &UdpSocket,
    peer: SocketAddr,
    buf: &mut [u8; MAX_DATAGRAM_BYTES + 1],
    deadline: Instant,
    interval: Duration,
) -> Result<Option<usize>> {
    let retry_deadline = deadline.min(Instant::now() + interval);
    loop {
        let remaining = retry_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        socket.set_read_timeout(Some(remaining))?;
        match socket.recv_from(buf) {
            Ok((n, from)) if from == peer && n <= MAX_DATAGRAM_BYTES => return Ok(Some(n)),
            // Ignored packets are not timer expiry and must not accelerate retries.
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted || oversized_datagram(&e) => continue,
            Err(e) if transient(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
    }
}

#[derive(Default)]
struct ReplayWindow {
    highest: Option<u64>,
    bits: u128,
}
impl ReplayWindow {
    fn permits(&self, sequence: u64) -> bool {
        match self.highest {
            None => true,
            Some(highest) if sequence > highest => true,
            Some(highest) => {
                let age = highest - sequence;
                age < 128 && (self.bits & (1u128 << age)) == 0
            }
        }
    }
    fn commit(&mut self, sequence: u64) {
        match self.highest {
            None => {
                self.highest = Some(sequence);
                self.bits = 1;
            }
            Some(highest) if sequence > highest => {
                let shift = sequence - highest;
                self.bits = if shift >= 128 {
                    1
                } else {
                    (self.bits << shift) | 1
                };
                self.highest = Some(sequence);
            }
            Some(highest) => self.bits |= 1u128 << (highest - sequence),
        }
    }
}
struct Confirmation {
    ready: Vec<u8>,
    accepted: Vec<u8>,
    expires: Instant,
    last_reply: Instant,
}

pub struct Session {
    socket: UdpSocket,
    peer: SocketAddr,
    clock: MonotonicClock,
    role: Role,
    info: SessionInfo,
    crypto: StatelessTransportState,
    config: Config,
    next_nonce: u64,
    replay: ReplayWindow,
    audio_replay: ReplayWindow,
    pending_clocks: [Option<u64>; MAX_PENDING_CLOCKS],
    last_authenticated: Instant,
    closed: bool,
    confirmation: Option<Confirmation>,
    stats: Stats,
    peer_audio_packet_count: Option<u64>,
    audio_started: bool,
    timeline_acknowledged: bool,
    timeline_control_nonce: Option<u64>,
    readiness_required: bool,
    reported_readiness: Option<ReceiverReadiness>,
    peer_readiness: Option<ReceiverReadiness>,
    readiness_control_nonce: Option<u64>,
    readiness_acknowledged: bool,
    negotiated_lead_ns: Option<u64>,
}
impl Session {
    fn new(
        endpoint: Endpoint,
        clock: MonotonicClock,
        role: Role,
        info: SessionInfo,
        crypto: StatelessTransportState,
    ) -> Self {
        Self::new_parts(
            endpoint.socket,
            endpoint.peer,
            endpoint.config,
            clock,
            role,
            info,
            crypto,
        )
    }
    fn new_parts(
        socket: UdpSocket,
        peer: SocketAddr,
        config: Config,
        clock: MonotonicClock,
        role: Role,
        info: SessionInfo,
        crypto: StatelessTransportState,
    ) -> Self {
        Self {
            socket,
            peer,
            clock,
            role,
            info,
            crypto,
            config,
            next_nonce: 0,
            replay: ReplayWindow::default(),
            audio_replay: ReplayWindow::default(),
            pending_clocks: [None; MAX_PENDING_CLOCKS],
            last_authenticated: Instant::now(),
            closed: false,
            confirmation: None,
            stats: Stats::default(),
            peer_audio_packet_count: None,
            audio_started: false,
            timeline_acknowledged: info.epoch_host_ns != 0,
            timeline_control_nonce: None,
            readiness_required: false,
            reported_readiness: None,
            peer_readiness: None,
            readiness_control_nonce: None,
            readiness_acknowledged: false,
            negotiated_lead_ns: None,
        }
        // endpoint.key is zeroized here; transport state owns derived session keys.
    }
    pub fn info(&self) -> SessionInfo {
        self.info
    }
    pub fn role(&self) -> Role {
        self.role
    }
    pub fn clock(&self) -> &MonotonicClock {
        &self.clock
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }
    /// Authenticated expected packet count, available after a finish_audio close.
    pub fn peer_audio_packet_count(&self) -> Option<u64> {
        self.peer_audio_packet_count
    }
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer
    }
    fn ensure_open(&mut self) -> Result<()> {
        if self.closed {
            return Err(Error::Closed);
        }
        if self.last_authenticated.elapsed() >= self.config.idle_timeout {
            self.closed = true;
            return Err(Error::IdleTimeout);
        }
        Ok(())
    }
    fn seal(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        if payload.len() > MAX_PLAIN {
            return Err(Error::InvalidConfig("payload exceeds fixed cap"));
        }
        if self.next_nonce == u64::MAX {
            self.closed = true;
            return Err(Error::SequenceExhausted);
        }
        let nonce = self.next_nonce;
        self.next_nonce += 1; // Reserve before encryption or send, never reuse on error.
        let mut out = [0; MAX_DATAGRAM_BYTES];
        out[..4].copy_from_slice(MAGIC);
        out[4] = DATA;
        out[5..HEADER].copy_from_slice(&nonce.to_be_bytes());
        let n = self
            .crypto
            .write_message(nonce, payload, &mut out[HEADER..])?;
        Ok(out[..HEADER + n].to_vec())
    }
    fn send(&mut self, payload: &[u8]) -> Result<()> {
        self.ensure_open()?;
        let bytes = self.seal(payload)?;
        self.socket.send_to(&bytes, self.peer)?;
        Ok(())
    }
    pub fn send_audio(&mut self, packet: &AudioPacket) -> Result<()> {
        if self.role != Role::Host {
            return Err(Error::WrongRole);
        }
        if self.info.epoch_host_ns == 0 || !self.timeline_acknowledged {
            return Err(Error::TimelineNotReady);
        }
        if packet.session_id() != self.info.session_id {
            return Err(Error::WrongSession);
        }
        let mut payload = [0; MAX_PLAIN];
        payload[0] = AUDIO;
        let mut encoded = [0; SESSION_PACKET_BYTES];
        packet.encode(&mut encoded);
        payload[1..].copy_from_slice(&encoded);
        self.send(&payload)?;
        self.audio_started = true;
        Ok(())
    }

    /// Latest authenticated receiver telemetry on the host. No secret material.
    pub fn peer_readiness(&self) -> Option<ReceiverReadiness> {
        self.peer_readiness
    }
    /// Chosen send-ahead duration, authenticated in the readiness-capable timeline.
    pub fn negotiated_lead_ns(&self) -> Option<u64> {
        self.negotiated_lead_ns
    }
    pub fn readiness_acknowledged(&self) -> bool {
        self.readiness_acknowledged
    }

    /// Report measured output timing while paired but not yet scheduled. Retry
    /// the same value until ReadinessAcknowledged. The CALLER owns periodic
    /// resends (for example every 250 ms); poll does not schedule these sends.
    /// A route change after a timeline
    /// requires closing and reconnecting, never silently changing live timing.
    pub fn report_readiness(&mut self, readiness: ReceiverReadiness) -> Result<()> {
        if self.role != Role::Receiver {
            return Err(Error::WrongRole);
        }
        readiness.validate()?;
        if self.info.epoch_host_ns != 0 && self.reported_readiness != Some(readiness) {
            return Err(Error::ReadinessChanged);
        }
        if self.reported_readiness != Some(readiness) {
            self.readiness_acknowledged = false;
        }
        self.reported_readiness = Some(readiness);
        self.send(&readiness_payload(RECEIVER_READINESS, readiness))
    }

    /// Legacy timeline API. New interactive sessions must declare an approved
    /// send lead with set_timeline_with_lead after receiving output readiness.
    pub fn set_timeline(&mut self, info: SessionInfo) -> Result<()> {
        if self.role != Role::Host {
            return Err(Error::WrongRole);
        }
        if self.readiness_required {
            return Err(Error::ReceiverNotReady);
        }
        self.set_timeline_inner(info, None)
    }

    pub fn set_timeline_with_lead(&mut self, info: SessionInfo, lead_ns: u64) -> Result<()> {
        if self.role != Role::Host {
            return Err(Error::WrongRole);
        }
        let readiness = self.peer_readiness.ok_or(Error::ReceiverNotReady)?;
        validate_lead(lead_ns, readiness)?;
        self.set_timeline_inner(info, Some(lead_ns))
    }

    fn set_timeline_inner(&mut self, info: SessionInfo, lead: Option<u64>) -> Result<()> {
        if self.role != Role::Host {
            return Err(Error::WrongRole);
        }
        let now = self.clock.now_ns()?;
        if info.session_id == 0
            || info.epoch_host_ns <= now
            || info.epoch_host_ns - now > 60_000_000_000
        {
            return Err(Error::InvalidConfig(
                "timeline must have a nonzero id and start within the next 60 seconds",
            ));
        }
        if self.audio_started && (info != self.info || lead != self.negotiated_lead_ns) {
            return Err(Error::InvalidConfig(
                "cannot change timeline or send lead after audio starts",
            ));
        }
        if info != self.info || lead != self.negotiated_lead_ns {
            self.timeline_acknowledged = false;
        }
        self.info = info;
        self.negotiated_lead_ns = lead;
        if let Some(lead) = lead {
            self.send(&timeline_lead_payload(SET_TIMELINE, info, lead))
        } else {
            self.send(&timeline_payload(SET_TIMELINE, info))
        }
    }

    pub fn request_clock(&mut self) -> Result<()> {
        if self.role != Role::Receiver {
            return Err(Error::WrongRole);
        }
        let t1 = self.clock.now_ns()?;
        let stale_ns = self
            .config
            .idle_timeout
            .min(Duration::from_millis(300))
            .as_nanos() as u64;
        for pending in &mut self.pending_clocks {
            if pending.is_some_and(|t| t1.saturating_sub(t) >= stale_ns) {
                *pending = None;
            }
        }
        let slot = self
            .pending_clocks
            .iter()
            .position(Option::is_none)
            .ok_or(Error::PendingClockLimit)?;
        if self.pending_clocks.contains(&Some(t1)) {
            return Err(Error::PendingClockLimit);
        }
        let mut payload = [0; 9];
        payload[0] = CLOCK_REQUEST;
        payload[1..].copy_from_slice(&t1.to_be_bytes());
        self.send(&payload)?;
        self.pending_clocks[slot] = Some(t1);
        Ok(())
    }
    pub fn heartbeat(&mut self) -> Result<()> {
        self.send(&[HEARTBEAT])
    }
    /// Authenticated best-effort UDP close. Lost close is bounded by peer idle timeout.
    pub fn close(&mut self) -> Result<()> {
        let result = self.send(&[CLOSE]);
        self.closed = true;
        result
    }

    /// Finish a finite host stream whose logical sequences are 0..total_packets.
    /// The authenticated total lets a recorder detect missing trailing/all data.
    /// Delivery remains best-effort; a missing final marker yields idle timeout.
    pub fn finish_audio(&mut self, total_packets: u64) -> Result<()> {
        if self.role != Role::Host {
            return Err(Error::WrongRole);
        }
        let mut payload = [0; 9];
        payload[0] = CLOSE;
        payload[1..].copy_from_slice(&total_packets.to_be_bytes());
        let result = self.send(&payload);
        self.closed = true;
        result
    }

    fn confirm(&mut self, bytes: &[u8], expected: u8, plain: &mut [u8; MAX_PLAIN]) -> bool {
        let Some((len, nonce)) = self.decrypt(bytes, plain) else {
            return false;
        };
        if len == 1 && plain[0] == expected {
            self.replay.commit(nonce);
            true
        } else {
            // Early audio/control must not push a retried ACK out of the window.
            false
        }
    }

    fn open(&mut self, bytes: &[u8], plain: &mut [u8; MAX_PLAIN]) -> Option<(usize, u64)> {
        let (len, nonce) = self.decrypt(bytes, plain)?;
        self.replay.commit(nonce);
        Some((len, nonce))
    }

    fn decrypt(&mut self, bytes: &[u8], plain: &mut [u8; MAX_PLAIN]) -> Option<(usize, u64)> {
        if !is_frame(bytes, DATA) || bytes.len() < HEADER + 17 {
            self.stats.malformed_rejected += 1;
            return None;
        }
        let nonce = read_u64(&bytes[5..HEADER]);
        if !self.replay.permits(nonce) {
            self.stats.replay_rejected += 1;
            return None;
        }
        let Ok(n) = self.crypto.read_message(nonce, &bytes[HEADER..], plain) else {
            self.stats.authentication_rejected += 1;
            return None;
        };
        Some((n, nonce))
    }

    /// Receives at most one event. Handles clock requests automatically on the
    /// host. The host MUST poll regularly, including during handshake retries and
    /// streaming. Returns None on poll timeout; idle timeout permanently closes.
    /// Zero timeout performs a bounded nonblocking poll. Invalid packets cannot
    /// extend the idle deadline, and at most 64 datagrams are processed per call.
    pub fn poll(&mut self, timeout: Duration) -> Result<Option<Event>> {
        self.ensure_open()?;
        let deadline = Instant::now() + timeout.min(self.config.idle_timeout);
        let mut bytes = [0; MAX_DATAGRAM_BYTES + 1];
        let mut plain = [0; MAX_PLAIN];
        self.socket.set_nonblocking(timeout.is_zero())?;
        for _ in 0..MAX_READS_PER_POLL {
            self.ensure_open()?;
            if !timeout.is_zero() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(None);
                }
                let idle_left = self
                    .config
                    .idle_timeout
                    .saturating_sub(self.last_authenticated.elapsed());
                self.socket.set_read_timeout(Some(
                    remaining.min(idle_left).max(Duration::from_nanos(1)),
                ))?;
            }
            let (n, from) = match self.socket.recv_from(&mut bytes) {
                Ok(pair) => pair,
                Err(e) if transient(&e) => {
                    self.ensure_open()?;
                    return Ok(None);
                }
                Err(e) if oversized_datagram(&e) => {
                    self.stats.malformed_rejected += 1;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let received_ns = self.clock.now_ns()?;
            if from != self.peer {
                self.stats.source_rejected += 1;
                continue;
            }
            if n > MAX_DATAGRAM_BYTES {
                self.stats.malformed_rejected += 1;
                continue;
            }
            if let Some(confirm) = &mut self.confirmation {
                if Instant::now() < confirm.expires && bytes[..n] == confirm.ready {
                    if confirm.last_reply.elapsed() >= self.config.retry_interval {
                        self.socket.send_to(&confirm.accepted, self.peer)?;
                        confirm.last_reply = Instant::now();
                    }
                    continue; // Exact retransmit does not refresh liveness.
                }
                if Instant::now() >= confirm.expires {
                    self.confirmation = None;
                }
            }
            let Some((len, nonce)) = self.open(&bytes[..n], &mut plain) else {
                continue;
            };
            match (plain[0], len) {
                (AUDIO, MAX_PLAIN) if self.role == Role::Receiver => {
                    if self.info.epoch_host_ns == 0 {
                        self.stats.malformed_rejected += 1;
                        continue;
                    }
                    let Ok(packet) = AudioPacket::decode(&plain[1..len]) else {
                        self.stats.malformed_rejected += 1;
                        continue;
                    };
                    if packet.session_id() != self.info.session_id {
                        self.stats.wrong_session_rejected += 1;
                        continue;
                    }
                    if !self.audio_replay.permits(packet.sequence()) {
                        self.stats.replay_rejected += 1;
                        continue;
                    }
                    self.audio_replay.commit(packet.sequence());
                    self.stats.audio_received += 1;
                    self.audio_started = true;
                    self.last_authenticated = Instant::now();
                    return Ok(Some(Event::Audio(packet)));
                }
                (RECEIVER_READINESS, 17) if self.role == Role::Host => {
                    if self
                        .readiness_control_nonce
                        .is_some_and(|previous| nonce < previous)
                    {
                        self.stats.malformed_rejected += 1;
                        continue;
                    }
                    let readiness = decode_readiness(&plain);
                    if readiness.validate().is_err() {
                        self.stats.malformed_rejected += 1;
                        continue;
                    }
                    if self.info.epoch_host_ns != 0 && self.peer_readiness != Some(readiness) {
                        self.closed = true;
                        return Err(Error::ReadinessChanged);
                    }
                    let changed = self.peer_readiness != Some(readiness);
                    self.peer_readiness = Some(readiness);
                    self.readiness_control_nonce = Some(nonce);
                    self.last_authenticated = Instant::now();
                    self.send(&readiness_payload(READINESS_ACK, readiness))?;
                    if changed {
                        return Ok(Some(Event::ReceiverReady(readiness)));
                    }
                }
                (READINESS_ACK, 17) if self.role == Role::Receiver => {
                    let readiness = decode_readiness(&plain);
                    if readiness.validate().is_err() || self.reported_readiness != Some(readiness) {
                        self.stats.malformed_rejected += 1;
                        continue;
                    }
                    self.readiness_acknowledged = true;
                    self.last_authenticated = Instant::now();
                    return Ok(Some(Event::ReadinessAcknowledged(readiness)));
                }
                (SET_TIMELINE, 17 | 25) if self.role == Role::Receiver => {
                    if self
                        .timeline_control_nonce
                        .is_some_and(|previous| nonce < previous)
                    {
                        self.stats.malformed_rejected += 1;
                        continue;
                    }
                    let info = SessionInfo {
                        session_id: read_u64(&plain[1..9]),
                        epoch_host_ns: read_u64(&plain[9..17]),
                    };
                    let lead = if len == 25 {
                        Some(read_u64(&plain[17..25]))
                    } else {
                        None
                    };
                    if info.session_id == 0
                        || info.epoch_host_ns == 0
                        || (self.audio_started
                            && (info != self.info || lead != self.negotiated_lead_ns))
                    {
                        self.stats.malformed_rejected += 1;
                        continue;
                    }
                    if self.readiness_required && lead.is_none() {
                        self.closed = true;
                        return Err(Error::IncompatibleProtocol);
                    }
                    if let Some(lead) = lead {
                        let Some(readiness) = self.reported_readiness else {
                            self.closed = true;
                            return Err(Error::ReceiverNotReady);
                        };
                        if let Err(error) = validate_lead(lead, readiness) {
                            self.closed = true;
                            return Err(error);
                        }
                    }
                    let changed = info != self.info || lead != self.negotiated_lead_ns;
                    self.timeline_control_nonce = Some(nonce);
                    self.info = info;
                    self.negotiated_lead_ns = lead;
                    self.last_authenticated = Instant::now();
                    if let Some(lead) = lead {
                        self.send(&timeline_lead_payload(TIMELINE_ACK, info, lead))?;
                    } else {
                        self.send(&timeline_payload(TIMELINE_ACK, info))?;
                    }
                    if changed {
                        self.audio_replay = ReplayWindow::default();
                        return Ok(Some(Event::Timeline(info)));
                    }
                }
                (TIMELINE_ACK, 17 | 25) if self.role == Role::Host => {
                    let info = SessionInfo {
                        session_id: read_u64(&plain[1..9]),
                        epoch_host_ns: read_u64(&plain[9..17]),
                    };
                    let lead = if len == 25 {
                        Some(read_u64(&plain[17..25]))
                    } else {
                        None
                    };
                    if info != self.info
                        || lead != self.negotiated_lead_ns
                        || info.epoch_host_ns == 0
                    {
                        self.stats.malformed_rejected += 1;
                        continue;
                    }
                    self.timeline_acknowledged = true;
                    self.last_authenticated = Instant::now();
                    return Ok(Some(Event::TimelineAcknowledged(info)));
                }
                (CLOCK_REQUEST, 9) if self.role == Role::Host => {
                    self.last_authenticated = Instant::now();
                    let mut reply = [0; 25];
                    reply[0] = CLOCK_REPLY;
                    reply[1..9].copy_from_slice(&plain[1..9]);
                    reply[9..17].copy_from_slice(&received_ns.to_be_bytes());
                    reply[17..25].copy_from_slice(&self.clock.now_ns()?.to_be_bytes());
                    self.send(&reply)?;
                }
                (CLOCK_REPLY, 25) if self.role == Role::Receiver => {
                    let t1 = read_u64(&plain[1..9]);
                    let Some(slot) = self.pending_clocks.iter().position(|p| *p == Some(t1)) else {
                        self.stats.malformed_rejected += 1;
                        continue;
                    };
                    self.pending_clocks[slot] = None;
                    let t2 = read_u64(&plain[9..17]);
                    let t3 = read_u64(&plain[17..25]);
                    if t3 < t2 || received_ns < t1 {
                        self.stats.malformed_rejected += 1;
                        continue;
                    }
                    self.last_authenticated = Instant::now();
                    return Ok(Some(Event::ClockSample(ClockExchange {
                        local_send_ns: t1,
                        host_receive_ns: t2,
                        host_send_ns: t3,
                        local_receive_ns: received_ns,
                    })));
                }
                (HEARTBEAT, 1) => {
                    self.last_authenticated = Instant::now();
                    return Ok(Some(Event::Heartbeat));
                }
                (CLOSE, 9) if self.role == Role::Receiver => {
                    self.peer_audio_packet_count = Some(read_u64(&plain[1..9]));
                    self.closed = true;
                    return Ok(Some(Event::Closed));
                }
                (CLOSE, 1) => {
                    self.closed = true;
                    return Ok(Some(Event::Closed));
                }
                _ => self.stats.malformed_rejected += 1,
            }
            plain.zeroize();
        }
        Ok(None)
    }
}

fn validate_lead(lead_ns: u64, readiness: ReceiverReadiness) -> Result<()> {
    let minimum = readiness.minimum_lead_ns()?;
    if lead_ns == 0 || lead_ns > MAX_NEGOTIATED_LEAD_NS || lead_ns < minimum {
        return Err(Error::InvalidConfig(
            "send lead must cover receiver horizon plus render block and be at most 300ms; unsupported output route, do not clamp",
        ));
    }
    Ok(())
}
fn readiness_payload(kind: u8, readiness: ReceiverReadiness) -> [u8; 17] {
    let mut payload = [0; 17];
    payload[0] = kind;
    payload[1..9].copy_from_slice(&readiness.output_horizon_ns.to_be_bytes());
    payload[9..17].copy_from_slice(&readiness.render_block_ns.to_be_bytes());
    payload
}
fn decode_readiness(plain: &[u8; MAX_PLAIN]) -> ReceiverReadiness {
    ReceiverReadiness {
        output_horizon_ns: read_u64(&plain[1..9]),
        render_block_ns: read_u64(&plain[9..17]),
    }
}
fn timeline_lead_payload(kind: u8, info: SessionInfo, lead_ns: u64) -> [u8; 25] {
    let mut payload = [0; 25];
    payload[..17].copy_from_slice(&timeline_payload(kind, info));
    payload[17..].copy_from_slice(&lead_ns.to_be_bytes());
    payload
}
fn timeline_payload(kind: u8, info: SessionInfo) -> [u8; 17] {
    let mut payload = [0; 17];
    payload[0] = kind;
    payload[1..9].copy_from_slice(&info.session_id.to_be_bytes());
    payload[9..17].copy_from_slice(&info.epoch_host_ns.to_be_bytes());
    payload
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod pairing_tests;

#[cfg(test)]
mod readiness_tests;
