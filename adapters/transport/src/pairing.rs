//! Cancellable, diagnostic pairing. The legacy finite fixed-peer API remains
//! available; interactive applications use these v2 handshake entry points.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

const HELLO_V2: &[u8] = b"RoomSync receiver v2";
const CANDIDATES: usize = 4;
const AUTH_PER_SECOND: u32 = 32;
const WAKE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingStage {
    WaitingForPeer,
    Authenticating,
    Confirming,
    Paired,
}
#[derive(Clone, Copy, Debug)]
pub struct PairingProgress {
    pub stage: PairingStage,
    pub datagrams_received: u64,
    pub wrong_source: u64,
    pub malformed: u64,
    pub authentication_rejected: u64,
    pub protocol_mismatch: u64,
    pub rate_limited: u64,
    pub peer_unreachable: u64,
    pub attempts: u64,
    pub hello_sent: u64,
    pub welcomes_authenticated: u64,
    pub confirmations_sent: u64,
    pub elapsed: Duration,
}
impl Default for PairingProgress {
    fn default() -> Self {
        Self {
            stage: PairingStage::WaitingForPeer,
            datagrams_received: 0,
            wrong_source: 0,
            malformed: 0,
            authentication_rejected: 0,
            protocol_mismatch: 0,
            rate_limited: 0,
            peer_unreachable: 0,
            attempts: 0,
            hello_sent: 0,
            welcomes_authenticated: 0,
            confirmations_sent: 0,
            elapsed: Duration::ZERO,
        }
    }
}
impl PairingProgress {
    /// Reports evidence, never guesses that Wi-Fi or a firewall is the cause.
    pub fn description(&self) -> &'static str {
        if self.stage == PairingStage::Paired {
            "Peer authenticated"
        } else if self.stage == PairingStage::Confirming {
            "Setup code verified; waiting for encrypted confirmation"
        } else if self.protocol_mismatch > 0 {
            "Peer reached us but uses a different pairing version; update both apps"
        } else if self.authentication_rejected > 0 {
            "UDP reached us but authentication failed; verify the setup code"
        } else if self.wrong_source > 0 {
            "UDP arrived from an unexpected address or port"
        } else if self.malformed > 0 {
            "UDP arrived but did not match RoomSync packets"
        } else if self.peer_unreachable > 0 {
            "The operating system reported the peer unavailable; still retrying"
        } else {
            "Waiting for peer UDP traffic; start either app whenever ready"
        }
    }
}
struct Reporter<'a, F: FnMut(PairingProgress)> {
    cancel: &'a AtomicBool,
    callback: F,
    status: PairingProgress,
    started: Instant,
    last_report: Instant,
    reported_stage: PairingStage,
}
impl<'a, F: FnMut(PairingProgress)> Reporter<'a, F> {
    fn new(cancel: &'a AtomicBool, callback: F) -> Self {
        let now = Instant::now();
        Self {
            cancel,
            callback,
            status: PairingProgress::default(),
            started: now,
            last_report: now - Duration::from_secs(1),
            reported_stage: PairingStage::WaitingForPeer,
        }
    }
    fn check(&mut self) -> Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if self.last_report.elapsed() >= Duration::from_millis(250)
            || self.reported_stage != self.status.stage
        {
            self.emit();
        }
        Ok(())
    }
    fn emit(&mut self) {
        self.status.elapsed = self.started.elapsed();
        self.last_report = Instant::now();
        self.reported_stage = self.status.stage;
        (self.callback)(self.status);
    }
    fn send_probe(&mut self, socket: &UdpSocket, bytes: &[u8], peer: SocketAddr) -> Result<bool> {
        self.check()?;
        match socket.send_to(bytes, peer) {
            Ok(_) => Ok(true),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::NetworkUnreachable
                        | io::ErrorKind::HostUnreachable
                ) =>
            {
                self.status.peer_unreachable += 1;
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }
    fn receive(
        &mut self,
        socket: &UdpSocket,
        bytes: &mut [u8; MAX_DATAGRAM_BYTES + 1],
        timeout: Duration,
    ) -> Result<Option<(usize, SocketAddr)>> {
        self.check()?;
        socket.set_read_timeout(Some(timeout.min(WAKE).max(Duration::from_millis(1))))?;
        match socket.recv_from(bytes) {
            Ok((n, from)) => {
                self.status.datagrams_received = self.status.datagrams_received.saturating_add(1);
                if n > MAX_DATAGRAM_BYTES {
                    self.status.malformed += 1;
                    Ok(None)
                } else {
                    Ok(Some((n, from)))
                }
            }
            Err(e) if oversized_datagram(&e) => {
                self.status.malformed += 1;
                Ok(None)
            }
            Err(e) if transient(&e) => Ok(None),
            // Windows may surface an ICMP response to a UDP probe this way.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
                ) =>
            {
                self.status.peer_unreachable += 1;
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// An explicitly bound host socket. A source is only admitted after possession
/// of the PSK AND fresh key confirmation, never by first-packet arrival.
pub struct PairingListener {
    socket: UdpSocket,
    key: PairingKey,
    config: Config,
}
impl Endpoint {
    pub fn bind_listener(
        local: SocketAddr,
        key: PairingKey,
        config: Config,
    ) -> Result<PairingListener> {
        PairingListener::bind(local, key, config)
    }
    /// Wait/retry on this same local UDP socket until paired or cancelled. Every
    /// fresh attempt creates new Noise ephemeral keys. No click timing deadline.
    pub fn join_cancellable(
        self,
        clock: MonotonicClock,
        cancel: &AtomicBool,
        progress: impl FnMut(PairingProgress),
    ) -> Result<Session> {
        let mut report = Reporter::new(cancel, progress);
        let mut bytes = [0; MAX_DATAGRAM_BYTES + 1];
        let mut plain = [0; MAX_PLAIN];
        let mut next_attempt = Instant::now();
        'attempt: loop {
            report.check()?;
            while Instant::now() < next_attempt {
                let _ = report.receive(
                    &self.socket,
                    &mut bytes,
                    next_attempt.saturating_duration_since(Instant::now()),
                )?;
            }
            report.status.stage = PairingStage::WaitingForPeer;
            report.status.attempts += 1;
            let mut state = handshake(&self.key, true)?;
            let hello = handshake_frame(&mut state, HELLO, HELLO_V2)?;
            let deadline = Instant::now() + self.config.handshake_timeout;
            let retry = self.config.retry_interval.max(Duration::from_millis(50));
            if report.send_probe(&self.socket, &hello, self.peer)? {
                report.status.hello_sent += 1;
            }
            let mut next_send = Instant::now() + retry;
            next_attempt = next_send;
            loop {
                report.check()?;
                if Instant::now() >= deadline {
                    continue 'attempt;
                }
                if Instant::now() >= next_send {
                    if report.send_probe(&self.socket, &hello, self.peer)? {
                        report.status.hello_sent += 1;
                    }
                    next_send = Instant::now() + retry;
                }
                let Some((n, from)) = report.receive(
                    &self.socket,
                    &mut bytes,
                    next_send
                        .saturating_duration_since(Instant::now())
                        .min(deadline.saturating_duration_since(Instant::now())),
                )?
                else {
                    continue;
                };
                if from != self.peer {
                    report.status.wrong_source += 1;
                    continue;
                }
                if !is_frame(&bytes[..n], WELCOME) {
                    report.status.malformed += 1;
                    continue;
                }
                report.status.stage = PairingStage::Authenticating;
                let Ok(len) = state.read_message(&bytes[5..n], &mut plain) else {
                    report.status.authentication_rejected += 1;
                    next_attempt = Instant::now() + retry;
                    continue 'attempt;
                };
                if len != 24
                    || &plain[..8] != b"RSACPT01"
                    || read_u64(&plain[8..16]) == 0
                    || read_u64(&plain[16..24]) != 0
                {
                    report.status.protocol_mismatch += 1;
                    next_attempt = Instant::now() + retry;
                    continue 'attempt;
                }
                let info = SessionInfo {
                    session_id: read_u64(&plain[8..16]),
                    epoch_host_ns: 0,
                };
                report.status.welcomes_authenticated += 1;
                let crypto = state.into_stateless_transport_mode()?;
                let mut session = Session::new_parts(
                    self.socket.try_clone()?,
                    self.peer,
                    self.config,
                    clock.clone(),
                    Role::Receiver,
                    info,
                    crypto,
                );
                let ready = session.seal(&[READY])?;
                if report.send_probe(&self.socket, &ready, self.peer)? {
                    report.status.confirmations_sent += 1;
                }
                let mut next_ready = Instant::now() + retry;
                report.status.stage = PairingStage::Confirming;
                loop {
                    report.check()?;
                    if Instant::now() >= deadline {
                        continue 'attempt;
                    }
                    if Instant::now() >= next_ready {
                        if report.send_probe(&self.socket, &ready, self.peer)? {
                            report.status.confirmations_sent += 1;
                        }
                        next_ready = Instant::now() + retry;
                    }
                    let Some((n, from)) = report.receive(
                        &self.socket,
                        &mut bytes,
                        next_ready
                            .saturating_duration_since(Instant::now())
                            .min(deadline.saturating_duration_since(Instant::now())),
                    )?
                    else {
                        continue;
                    };
                    if from != self.peer {
                        report.status.wrong_source += 1;
                        continue;
                    }
                    // A retried WELCOME is expected when READY was lost.
                    if is_frame(&bytes[..n], WELCOME) {
                        continue;
                    }
                    let auth_before = session.stats.authentication_rejected;
                    if session.confirm(&bytes[..n], ACCEPTED, &mut plain) {
                        session.last_authenticated = Instant::now();
                        report.status.stage = PairingStage::Paired;
                        report.emit();
                        return Ok(session);
                    }
                    report.status.authentication_rejected +=
                        session.stats.authentication_rejected - auth_before;
                }
            }
        }
    }
}
struct Candidate {
    peer: SocketAddr,
    hello: Vec<u8>,
    welcome: Vec<u8>,
    crypto: StatelessTransportState,
    info: SessionInfo,
    expires: Instant,
    next_send: Instant,
}
impl PairingListener {
    pub fn bind(local: SocketAddr, key: PairingKey, config: Config) -> Result<Self> {
        let socket = UdpSocket::bind(local)?;
        socket.set_nonblocking(false)?;
        Ok(Self {
            socket,
            key,
            config: config.validate()?,
        })
    }
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }
    pub fn accept_waiting(
        self,
        clock: MonotonicClock,
        cancel: &AtomicBool,
        progress: impl FnMut(PairingProgress),
    ) -> Result<Session> {
        let mut report = Reporter::new(cancel, progress);
        let mut candidates: [Option<Candidate>; CANDIDATES] = std::array::from_fn(|_| None);
        let mut bytes = [0; MAX_DATAGRAM_BYTES + 1];
        let mut plain = [0; MAX_PLAIN];
        let retry = self.config.retry_interval.max(Duration::from_millis(50));
        let mut auth_window = Instant::now();
        let mut auth_count = 0;
        loop {
            report.check()?;
            let now = Instant::now();
            for candidate in &mut candidates {
                if candidate.as_ref().is_some_and(|c| now >= c.expires) {
                    *candidate = None;
                }
                if let Some(candidate) = candidate {
                    if now >= candidate.next_send {
                        if self
                            .socket
                            .send_to(&candidate.welcome, candidate.peer)
                            .is_err()
                        {
                            report.status.peer_unreachable += 1;
                        }
                        candidate.next_send = Instant::now() + retry;
                    }
                }
            }
            report.status.stage = if candidates.iter().any(Option::is_some) {
                PairingStage::Confirming
            } else {
                PairingStage::WaitingForPeer
            };
            let next = candidates
                .iter()
                .flatten()
                .map(|c| c.next_send.min(c.expires))
                .min()
                .unwrap_or(now + WAKE);
            let Some((n, from)) = report.receive(
                &self.socket,
                &mut bytes,
                next.saturating_duration_since(Instant::now()),
            )?
            else {
                continue;
            };
            if from.port() == 0
                || from.ip().is_unspecified()
                || from.ip().is_multicast()
                || matches!(from.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast())
            {
                report.status.wrong_source += 1;
                continue;
            }
            if auth_window.elapsed() >= Duration::from_secs(1) {
                auth_window = Instant::now();
                auth_count = 0;
            }
            if is_frame(&bytes[..n], HELLO) {
                if candidates
                    .iter()
                    .flatten()
                    .any(|c| c.peer == from && c.hello == bytes[..n])
                {
                    continue;
                }
                if auth_window.elapsed() >= Duration::from_secs(1) {
                    auth_window = Instant::now();
                    auth_count = 0;
                }
                if auth_count >= AUTH_PER_SECOND {
                    report.status.rate_limited += 1;
                    continue;
                }
                auth_count += 1;
                report.status.attempts += 1;
                let mut state = handshake(&self.key, false)?;
                let Ok(len) = state.read_message(&bytes[5..n], &mut plain) else {
                    report.status.authentication_rejected += 1;
                    continue;
                };
                if &plain[..len] != HELLO_V2 {
                    report.status.protocol_mismatch += 1;
                    continue;
                }
                let mut random = [0; 8];
                getrandom::fill(&mut random).map_err(|_| Error::Crypto)?;
                let info = SessionInfo {
                    session_id: u64::from_be_bytes(random).max(1),
                    epoch_host_ns: 0,
                };
                let mut payload = [0; 24];
                payload[..8].copy_from_slice(b"RSACPT01");
                payload[8..16].copy_from_slice(&info.session_id.to_be_bytes());
                let welcome = handshake_frame(&mut state, WELCOME, &payload)?;
                if self.socket.send_to(&welcome, from).is_err() {
                    report.status.peer_unreachable += 1;
                    continue;
                }
                let candidate = Candidate {
                    peer: from,
                    hello: bytes[..n].to_vec(),
                    welcome,
                    crypto: state.into_stateless_transport_mode()?,
                    info,
                    expires: Instant::now() + self.config.handshake_timeout,
                    next_send: Instant::now() + retry,
                };
                let slot = candidates
                    .iter()
                    .position(|c| c.as_ref().is_some_and(|c| c.peer == from))
                    .or_else(|| candidates.iter().position(Option::is_none))
                    .unwrap_or_else(|| {
                        candidates
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, c)| c.as_ref().map(|c| c.expires))
                            .map(|(i, _)| i)
                            .unwrap_or(0)
                    });
                candidates[slot] = Some(candidate);
            } else if is_frame(&bytes[..n], DATA) && n >= HEADER + 17 {
                let Some(slot) = candidates
                    .iter()
                    .position(|c| c.as_ref().is_some_and(|c| c.peer == from))
                else {
                    report.status.wrong_source += 1;
                    continue;
                };
                let candidate = candidates[slot].as_ref().expect("occupied candidate slot");
                let nonce = read_u64(&bytes[5..HEADER]);
                if nonce != 0 {
                    report.status.malformed += 1;
                    continue;
                }
                if auth_count >= AUTH_PER_SECOND {
                    report.status.rate_limited += 1;
                    continue;
                }
                auth_count += 1;
                let Ok(len) = candidate
                    .crypto
                    .read_message(nonce, &bytes[HEADER..n], &mut plain)
                else {
                    report.status.authentication_rejected += 1;
                    continue;
                };
                if len != 1 || plain[0] != READY {
                    report.status.malformed += 1;
                    continue;
                }
                let candidate = candidates[slot].take().expect("confirmed candidate slot");
                let mut session = Session::new_parts(
                    self.socket.try_clone()?,
                    from,
                    self.config,
                    clock.clone(),
                    Role::Host,
                    candidate.info,
                    candidate.crypto,
                );
                session.replay.commit(0);
                let accepted = session.seal(&[ACCEPTED])?;
                if session.socket.send_to(&accepted, from).is_err() {
                    report.status.peer_unreachable += 1;
                    continue;
                }
                session.confirmation = Some(Confirmation {
                    ready: bytes[..n].to_vec(),
                    accepted,
                    expires: Instant::now() + self.config.handshake_timeout,
                    last_reply: Instant::now(),
                });
                session.last_authenticated = Instant::now();
                report.status.stage = PairingStage::Paired;
                report.emit();
                return Ok(session);
            } else {
                report.status.malformed += 1;
            }
        }
    }
}
