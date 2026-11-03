use super::*;
use roomsync_core::protocol::SAMPLES_PER_PACKET;
use std::thread;

fn key(byte: u8) -> PairingKey {
    PairingKey(Zeroizing::new([byte; 32]))
}
fn sockets() -> (UdpSocket, UdpSocket) {
    (
        UdpSocket::bind("127.0.0.1:0").unwrap(),
        UdpSocket::bind("127.0.0.1:0").unwrap(),
    )
}
fn pair_with_config(config: Config) -> (Session, Session) {
    let (host_socket, receiver_socket) = sockets();
    let host_addr = host_socket.local_addr().unwrap();
    let receiver_addr = receiver_socket.local_addr().unwrap();
    let handle = thread::spawn(move || {
        Endpoint::from_socket(host_socket, receiver_addr, key(42), config)
            .unwrap()
            .accept(MonotonicClock::default(), Duration::from_secs(2))
            .unwrap()
    });
    let receiver = Endpoint::from_socket(receiver_socket, host_addr, key(42), config)
        .unwrap()
        .join(MonotonicClock::default())
        .unwrap();
    (handle.join().unwrap(), receiver)
}
fn pair() -> (Session, Session) {
    pair_with_config(Config::default())
}
fn packet(s: &Session, seq: u64) -> AudioPacket {
    AudioPacket::new(s.info.session_id, seq, [seq as i16; SAMPLES_PER_PACKET]).unwrap()
}
fn payload(packet: &AudioPacket) -> [u8; MAX_PLAIN] {
    let mut p = [0; MAX_PLAIN];
    p[0] = AUDIO;
    let mut encoded = [0; SESSION_PACKET_BYTES];
    packet.encode(&mut encoded);
    p[1..].copy_from_slice(&encoded);
    p
}
fn expect_audio(receiver: &mut Session, seq: u64) {
    match receiver.poll(Duration::from_millis(100)).unwrap() {
        Some(Event::Audio(p)) => {
            assert_eq!(p.sequence(), seq);
            assert_eq!(p.samples()[0], seq as i16);
        }
        other => panic!("expected audio {seq}, got {other:?}"),
    }
}

#[test]
fn actual_udp_audio_clock_epoch_and_disconnect() {
    let (mut host, mut receiver) = pair();
    assert_eq!(host.info(), receiver.info());
    assert!(host.info().epoch_host_ns >= 2_000_000_000);
    assert_eq!(AUDIO_DATAGRAM_BYTES, 542);
    host.send_audio(&packet(&host, 0)).unwrap();
    expect_audio(&mut receiver, 0);
    receiver.request_clock().unwrap();
    host.poll(Duration::from_millis(10)).unwrap();
    match receiver.poll(Duration::from_millis(100)).unwrap() {
        Some(Event::ClockSample(x)) => {
            assert!(x.local_receive_ns >= x.local_send_ns);
            assert!(x.host_send_ns >= x.host_receive_ns);
        }
        event => panic!("expected clock sample, got {event:?}"),
    }
    host.close().unwrap();
    assert!(matches!(
        receiver.poll(Duration::from_millis(100)).unwrap(),
        Some(Event::Closed)
    ));
    assert!(matches!(receiver.request_clock(), Err(Error::Closed)));
}

#[test]
fn rejects_unauthenticated_tampered_oversized_and_wrong_source_datagrams() {
    let (mut host, mut receiver) = pair();
    let target = receiver.local_addr().unwrap();
    // Valid key but a different UDP source is not a paired endpoint.
    let valid = host.seal(&payload(&packet(&host, 1))).unwrap();
    let outsider = UdpSocket::bind("127.0.0.1:0").unwrap();
    outsider.send_to(&valid, target).unwrap();
    host.socket
        .send_to(&[7; MAX_DATAGRAM_BYTES + 20], target)
        .unwrap();
    host.socket.send_to(b"not a packet", target).unwrap();
    let mut tampered = host.seal(&payload(&packet(&host, 2))).unwrap();
    tampered[20] ^= 1;
    host.socket.send_to(&tampered, target).unwrap();
    // Bad tag must not consume the counter: authentic original nonce still works.
    host.socket.send_to(&valid, target).unwrap();
    expect_audio(&mut receiver, 1);
    let s = receiver.stats();
    assert_eq!(s.source_rejected, 1);
    assert_eq!(s.malformed_rejected, 2);
    assert_eq!(s.authentication_rejected, 1);
}

#[test]
fn reordering_drops_duplicates_and_wrong_sessions_are_bounded() {
    let (mut host, mut receiver) = pair();
    let target = receiver.local_addr().unwrap();
    let a = host.seal(&payload(&packet(&host, 0))).unwrap();
    let _dropped = host.seal(&payload(&packet(&host, 1))).unwrap();
    let c = host.seal(&payload(&packet(&host, 2))).unwrap();
    host.socket.send_to(&c, target).unwrap();
    expect_audio(&mut receiver, 2);
    host.socket.send_to(&a, target).unwrap();
    expect_audio(&mut receiver, 0);
    host.socket.send_to(&a, target).unwrap(); // Transport nonce replay.
    host.send_audio(&packet(&host, 0)).unwrap(); // New nonce but audio duplicate.
    let wrong = AudioPacket::new(host.info.session_id ^ 1, 3, [0; SAMPLES_PER_PACKET]).unwrap();
    assert!(matches!(host.send_audio(&wrong), Err(Error::WrongSession)));
    let bytes = host.seal(&payload(&wrong)).unwrap();
    host.socket.send_to(&bytes, target).unwrap();
    host.send_audio(&packet(&host, 200)).unwrap();
    expect_audio(&mut receiver, 200);
    host.send_audio(&packet(&host, 1)).unwrap(); // Too old for audio window.
    assert!(receiver.poll(Duration::from_millis(5)).unwrap().is_none());
    assert_eq!(receiver.stats.replay_rejected, 3);
    assert_eq!(receiver.stats.wrong_session_rejected, 1);
}

#[test]
fn wrong_key_never_establishes_audio_session() {
    let (host_socket, receiver_socket) = sockets();
    let host_addr = host_socket.local_addr().unwrap();
    let receiver_addr = receiver_socket.local_addr().unwrap();
    let config = Config {
        handshake_timeout: Duration::from_millis(100),
        retry_interval: Duration::from_millis(10),
        ..Config::default()
    };
    let host = thread::spawn(move || {
        Endpoint::from_socket(host_socket, receiver_addr, key(1), config)
            .unwrap()
            .accept(MonotonicClock::default(), Duration::ZERO)
    });
    let receiver = Endpoint::from_socket(receiver_socket, host_addr, key(2), config)
        .unwrap()
        .join(MonotonicClock::default());
    assert!(matches!(receiver, Err(Error::HandshakeTimeout)));
    assert!(matches!(host.join().unwrap(), Err(Error::HandshakeTimeout)));
}

#[test]
fn invalid_traffic_does_not_reset_timeout() {
    let config = Config {
        idle_timeout: Duration::from_millis(50),
        ..Config::default()
    };
    let (host, mut receiver) = pair_with_config(config);
    thread::sleep(Duration::from_millis(25));
    host.socket
        .send_to(b"bad", receiver.local_addr().unwrap())
        .unwrap();
    assert!(matches!(
        receiver.poll(Duration::from_millis(100)),
        Err(Error::IdleTimeout)
    ));
    assert!(matches!(receiver.poll(Duration::ZERO), Err(Error::Closed)));
}

#[test]
fn replay_window_handles_boundaries_without_overflow() {
    let mut window = ReplayWindow::default();
    window.commit(1);
    assert!(!window.permits(1));
    assert!(window.permits(0));
    window.commit(129);
    assert!(!window.permits(1));
    assert!(window.permits(2));
    window.commit(u64::MAX - 1);
    assert!(!window.permits(129));
    assert!(window.permits(u64::MAX));
}

#[test]
fn rejects_invalid_config_keys_and_role_misuse() {
    assert!(PairingKey::from_hex("short").is_err());
    assert!(PairingKey::from_hex(&"z".repeat(64)).is_err());
    let generated = PairingKey::generate().unwrap();
    assert_eq!(format!("{generated:?}"), "PairingKey([REDACTED])");
    assert!(PairingKey::from_hex(&generated.to_hex_for_storage()).is_ok());
    assert!(
        Endpoint::bind(
            "127.0.0.1:0".parse().unwrap(),
            "0.0.0.0:1".parse().unwrap(),
            key(1),
            Config::default()
        )
        .is_err()
    );
    let (mut host, mut receiver) = pair();
    assert!(matches!(host.request_clock(), Err(Error::WrongRole)));
    assert!(matches!(
        receiver.send_audio(&packet(&receiver, 0)),
        Err(Error::WrongRole)
    ));
    for _ in 0..MAX_PENDING_CLOCKS {
        receiver.request_clock().unwrap();
    }
    assert!(matches!(
        receiver.request_clock(),
        Err(Error::PendingClockLimit)
    ));
}

#[test]
fn confirmation_retry_is_exact_and_does_not_reset_liveness() {
    let (mut host, receiver) = pair();
    thread::sleep(host.config.retry_interval);
    let confirm = host.confirmation.as_ref().unwrap();
    receiver
        .socket
        .send_to(&confirm.ready, host.local_addr().unwrap())
        .unwrap();
    let last = host.last_authenticated;
    host.poll(Duration::from_millis(5)).unwrap();
    assert_eq!(last, host.last_authenticated);
    let mut bytes = [0; MAX_DATAGRAM_BYTES];
    receiver
        .socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let (n, _) = receiver.socket.recv_from(&mut bytes).unwrap();
    assert_eq!(&bytes[..n], host.confirmation.as_ref().unwrap().accepted);
}

#[test]
fn injected_clock_domain_is_preserved() {
    fn hardware_clock() -> Result<u64> {
        Ok(987_654_321)
    }
    assert_eq!(
        MonotonicClock::from_source(hardware_clock)
            .now_ns()
            .unwrap(),
        987_654_321
    );
}

#[test]
fn early_data_cannot_push_confirmation_out_of_replay_window() {
    let (mut host, mut receiver) = pair();
    receiver.replay = ReplayWindow::default(); // Model join before ACCEPTED arrives.
    let mut plain = [0; MAX_PLAIN];
    for sequence in 0..200 {
        let bytes = host.seal(&payload(&packet(&host, sequence))).unwrap();
        assert!(!receiver.confirm(&bytes, ACCEPTED, &mut plain));
    }
    let ack = &host.confirmation.as_ref().unwrap().accepted;
    assert!(receiver.confirm(ack, ACCEPTED, &mut plain));
    assert!(!receiver.confirm(ack, ACCEPTED, &mut plain));
}

#[test]
fn ignored_datagrams_do_not_accelerate_retry_timer() {
    let (target, peer) = sockets();
    let attacker = UdpSocket::bind("127.0.0.1:0").unwrap();
    for _ in 0..50 {
        attacker
            .send_to(b"garbage", target.local_addr().unwrap())
            .unwrap();
    }
    let started = Instant::now();
    let mut bytes = [0; MAX_DATAGRAM_BYTES + 1];
    let received = read_until(
        &target,
        peer.local_addr().unwrap(),
        &mut bytes,
        started + Duration::from_secs(1),
        Duration::from_millis(40),
    )
    .unwrap();
    assert!(received.is_none());
    assert!(started.elapsed() >= Duration::from_millis(35));
}

#[test]
fn lost_clock_replies_release_bounded_slots() {
    let (_host, mut receiver) = pair();
    for _ in 0..MAX_PENDING_CLOCKS {
        receiver.request_clock().unwrap();
    }
    assert!(matches!(
        receiver.request_clock(),
        Err(Error::PendingClockLimit)
    ));
    thread::sleep(Duration::from_millis(310));
    receiver.request_clock().unwrap();
    assert_eq!(receiver.pending_clocks.iter().flatten().count(), 1);
}

#[test]
fn authenticated_finish_reports_missing_tail_or_all_audio() {
    let (mut host, mut receiver) = pair();
    host.finish_audio(20).unwrap();
    assert!(matches!(
        receiver.poll(Duration::from_millis(100)).unwrap(),
        Some(Event::Closed)
    ));
    assert_eq!(receiver.peer_audio_packet_count(), Some(20));
    assert_eq!(receiver.stats.audio_received, 0);
}

#[test]
fn fixed_session_timeline_is_negotiated_without_sharing_cipher_state() {
    let (host_socket, receiver_socket) = sockets();
    let host_addr = host_socket.local_addr().unwrap();
    let receiver_addr = receiver_socket.local_addr().unwrap();
    let clock = MonotonicClock::default();
    let info = SessionInfo {
        session_id: 234,
        epoch_host_ns: clock.now_ns().unwrap() + 5_000_000_000,
    };
    let host = thread::spawn(move || {
        Endpoint::from_socket(host_socket, receiver_addr, key(42), Config::default())
            .unwrap()
            .accept_with_info(clock, info)
            .unwrap()
    });
    let receiver = Endpoint::from_socket(receiver_socket, host_addr, key(42), Config::default())
        .unwrap()
        .join(MonotonicClock::default())
        .unwrap();
    assert_eq!(receiver.info(), info);
    assert_eq!(host.join().unwrap().info(), info);
}

#[test]
fn clock_source_failures_propagate_without_epoch_fallback() {
    fn bad_clock() -> Result<u64> {
        Err(Error::ClockUnavailable)
    }
    assert!(matches!(
        MonotonicClock::from_source(bad_clock).now_ns(),
        Err(Error::ClockUnavailable)
    ));
}

#[test]
fn every_datagram_length_and_invalid_tag_is_rejected_without_panic() {
    let (_host, mut receiver) = pair();
    let mut plain = [0; MAX_PLAIN];
    for len in 0..=MAX_DATAGRAM_BYTES + 1 {
        let mut bytes = vec![0; len];
        if len >= HEADER {
            bytes[..4].copy_from_slice(MAGIC);
            bytes[4] = DATA;
            bytes[5..HEADER].copy_from_slice(&1000u64.to_be_bytes());
        }
        assert!(receiver.open(&bytes, &mut plain).is_none());
    }
}
