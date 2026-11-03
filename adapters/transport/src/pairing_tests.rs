use super::*;
use roomsync_core::protocol::SAMPLES_PER_PACKET;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

fn key() -> PairingKey {
    PairingKey::from_hex(&"42".repeat(32)).unwrap()
}
fn config() -> Config {
    Config {
        handshake_timeout: Duration::from_millis(150),
        retry_interval: Duration::from_millis(50),
        ..Config::default()
    }
}
fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}
fn watchdog(cancel: Arc<AtomicBool>) {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(3));
        cancel.store(true, Ordering::Release);
    });
}
pub(super) fn pair() -> (Session, Session) {
    let listener = Endpoint::bind_listener(localhost(), key(), Config::default()).unwrap();
    let host_addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    watchdog(stop.clone());
    let host_stop = stop.clone();
    let host = thread::spawn(move || {
        listener
            .accept_waiting(MonotonicClock::default(), &host_stop, |_| {})
            .unwrap()
    });
    let receiver = Endpoint::bind(localhost(), host_addr, key(), Config::default())
        .unwrap()
        .join_cancellable(MonotonicClock::default(), &stop, |_| {})
        .unwrap();
    (host.join().unwrap(), receiver)
}

#[test]
fn listener_waits_across_attempt_timeouts_for_late_receiver() {
    let listener = Endpoint::bind_listener(localhost(), key(), config()).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    watchdog(stop.clone());
    let host_stop = stop.clone();
    let host = thread::spawn(move || {
        listener
            .accept_waiting(MonotonicClock::default(), &host_stop, |_| {})
            .unwrap()
    });
    thread::sleep(Duration::from_millis(450));
    let receiver = Endpoint::bind(localhost(), addr, key(), config())
        .unwrap()
        .join_cancellable(MonotonicClock::default(), &stop, |_| {})
        .unwrap();
    let host = host.join().unwrap();
    assert_eq!(host.peer_addr(), receiver.local_addr().unwrap());
    assert_eq!(host.info().epoch_host_ns, 0);
}
#[test]
fn receiver_can_start_before_host_and_keep_its_original_port() {
    let reserve = UdpSocket::bind(localhost()).unwrap();
    let host_addr = reserve.local_addr().unwrap();
    drop(reserve);
    let endpoint = Endpoint::bind(localhost(), host_addr, key(), config()).unwrap();
    let original = endpoint.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    watchdog(stop.clone());
    let receiver_stop = stop.clone();
    let receiver = thread::spawn(move || {
        endpoint.join_cancellable(MonotonicClock::default(), &receiver_stop, |_| {})
    });
    thread::sleep(Duration::from_millis(450));
    let mut host = Endpoint::bind_listener(host_addr, key(), config())
        .unwrap()
        .accept_waiting(MonotonicClock::default(), &stop, |_| {})
        .unwrap();
    // accept_waiting means the host authenticated READY, not that the peer has
    // received ACCEPTED. Keep servicing ACK retries exactly as the app does.
    while !receiver.is_finished() && !stop.load(Ordering::Acquire) {
        host.poll(Duration::from_millis(5)).unwrap();
    }
    let receiver = receiver.join().unwrap().unwrap();
    assert_eq!(receiver.local_addr().unwrap(), original);
    assert_eq!(host.peer_addr(), original);
}
#[test]
fn cancelled_pairing_returns_promptly_in_both_roles() {
    for receiver in [false, true] {
        let stop = Arc::new(AtomicBool::new(false));
        let child_stop = stop.clone();
        let endpoint = Endpoint::bind(
            localhost(),
            "127.0.0.1:49999".parse().unwrap(),
            key(),
            Config::default(),
        )
        .unwrap();
        let listener = Endpoint::bind_listener(localhost(), key(), Config::default()).unwrap();
        let child = thread::spawn(move || {
            if receiver {
                endpoint.join_cancellable(MonotonicClock::default(), &child_stop, |_| {})
            } else {
                listener.accept_waiting(MonotonicClock::default(), &child_stop, |_| {})
            }
        });
        thread::sleep(Duration::from_millis(30));
        let start = Instant::now();
        stop.store(true, Ordering::Release);
        assert!(matches!(child.join().unwrap(), Err(Error::Cancelled)));
        assert!(start.elapsed() < Duration::from_millis(350));
    }
}
#[test]
fn common_timeline_is_acked_before_audio_and_retries_do_not_reset_receiver() {
    let (mut host, mut receiver) = pair();
    assert_eq!(receiver.info().epoch_host_ns, 0);
    let packet = AudioPacket::new(host.info().session_id, 0, [0; SAMPLES_PER_PACKET]).unwrap();
    assert!(matches!(
        host.send_audio(&packet),
        Err(Error::TimelineNotReady)
    ));
    receiver.request_clock().unwrap();
    host.poll(Duration::from_millis(5)).unwrap();
    assert!(matches!(
        receiver.poll(Duration::from_millis(100)).unwrap(),
        Some(Event::ClockSample(_))
    ));
    let info = SessionInfo {
        session_id: 77,
        epoch_host_ns: host.clock().now_ns().unwrap() + 3_000_000_000,
    };
    if host.peer_readiness().is_none() {
        announce_default_readiness(&mut host, &mut receiver);
    }
    host.set_timeline_with_lead(info, 200_000_000).unwrap();
    assert!(
        matches!(receiver.poll(Duration::from_millis(100)).unwrap(), Some(Event::Timeline(v)) if v == info)
    );
    assert_eq!(receiver.info(), info);
    let packet = AudioPacket::new(77, 0, [23; SAMPLES_PER_PACKET]).unwrap();
    assert!(matches!(
        host.send_audio(&packet),
        Err(Error::TimelineNotReady)
    ));
    assert!(
        matches!(host.poll(Duration::from_millis(100)).unwrap(), Some(Event::TimelineAcknowledged(v)) if v == info)
    );
    if host.peer_readiness().is_none() {
        announce_default_readiness(&mut host, &mut receiver);
    }
    host.set_timeline_with_lead(info, 200_000_000).unwrap();
    assert!(receiver.poll(Duration::from_millis(5)).unwrap().is_none());
    assert!(
        matches!(host.poll(Duration::from_millis(100)).unwrap(), Some(Event::TimelineAcknowledged(v)) if v == info)
    );
    host.send_audio(&packet).unwrap();
    assert!(
        matches!(receiver.poll(Duration::from_millis(100)).unwrap(), Some(Event::Audio(v)) if v == packet)
    );
    assert!(
        host.set_timeline_with_lead(
            SessionInfo {
                session_id: 78,
                ..info
            },
            200_000_000
        )
        .is_err()
    );
}
#[test]
fn unauthenticated_and_replayed_hello_candidates_do_not_claim_listener() {
    let listener = Endpoint::bind_listener(localhost(), key(), Config::default()).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    watchdog(stop.clone());
    let host_stop = stop.clone();
    let host = thread::spawn(move || {
        listener
            .accept_waiting(MonotonicClock::default(), &host_stop, |_| {})
            .unwrap()
    });
    let attacker = UdpSocket::bind(localhost()).unwrap();
    attacker.send_to(b"RoomSync receiver v3", addr).unwrap();
    // A captured valid HELLO proves membership, not a fresh sender: no READY follows.
    let mut state = handshake(&key(), true).unwrap();
    let hello = handshake_frame(&mut state, HELLO, b"RoomSync receiver v3").unwrap();
    attacker.send_to(&hello, addr).unwrap();
    let receiver = Endpoint::bind(localhost(), addr, key(), Config::default())
        .unwrap()
        .join_cancellable(MonotonicClock::default(), &stop, |_| {})
        .unwrap();
    let host = host.join().unwrap();
    assert_eq!(host.peer_addr(), receiver.local_addr().unwrap());
    assert_ne!(host.peer_addr(), attacker.local_addr().unwrap());
}
#[test]
fn diagnostics_distinguish_wrong_key_from_no_traffic_and_bound_auth_work() {
    let listener = Endpoint::bind_listener(localhost(), key(), Config::default()).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let host_stop = stop.clone();
    let progress = Arc::new(Mutex::new(PairingProgress::default()));
    let seen = progress.clone();
    let host = thread::spawn(move || {
        listener.accept_waiting(MonotonicClock::default(), &host_stop, |p| {
            *seen.lock().unwrap() = p
        })
    });
    let attacker = UdpSocket::bind(localhost()).unwrap();
    let wrong_key = PairingKey::from_hex(&"ab".repeat(32)).unwrap();
    let mut state = handshake(&wrong_key, true).unwrap();
    let hello = handshake_frame(&mut state, HELLO, b"RoomSync receiver v3").unwrap();
    for _ in 0..64 {
        attacker.send_to(&hello, addr).unwrap();
    }
    thread::sleep(Duration::from_millis(350));
    stop.store(true, Ordering::Release);
    assert!(matches!(host.join().unwrap(), Err(Error::Cancelled)));
    let p = *progress.lock().unwrap();
    assert!(p.datagrams_received > 0);
    assert!(p.authentication_rejected > 0);
    assert!(p.authentication_rejected <= 32);
    assert!(p.rate_limited > 0);
    assert!(p.description().contains("authentication failed"));
    attacker.set_nonblocking(true).unwrap();
    let mut bytes = [0; 600];
    assert!(attacker.recv_from(&mut bytes).is_err()); // No response to a wrong key.
    assert!(
        PairingProgress::default()
            .description()
            .contains("Waiting for peer")
    );
}

#[test]
fn reordered_authenticated_timeline_does_not_roll_back_newer_start() {
    let (mut host, mut receiver) = pair();
    let a = SessionInfo {
        session_id: 21,
        epoch_host_ns: host.clock().now_ns().unwrap() + 2_000_000_000,
    };
    let b = SessionInfo {
        session_id: 22,
        epoch_host_ns: a.epoch_host_ns + 1_000_000_000,
    };
    announce_default_readiness(&mut host, &mut receiver);
    let older = host
        .seal(&timeline_lead_payload(SET_TIMELINE, a, 200_000_000))
        .unwrap();
    let newer = host
        .seal(&timeline_lead_payload(SET_TIMELINE, b, 200_000_000))
        .unwrap();
    host.info = b;
    host.negotiated_lead_ns = Some(200_000_000);
    host.socket.send_to(&newer, host.peer).unwrap();
    host.socket.send_to(&older, host.peer).unwrap();
    assert!(
        matches!(receiver.poll(Duration::from_millis(100)).unwrap(), Some(Event::Timeline(v)) if v == b)
    );
    assert!(receiver.poll(Duration::from_millis(5)).unwrap().is_none());
    assert_eq!(receiver.info(), b);
    assert!(
        matches!(host.poll(Duration::from_millis(100)).unwrap(), Some(Event::TimelineAcknowledged(v)) if v == b)
    );
    let packet = AudioPacket::new(b.session_id, 0, [9; SAMPLES_PER_PACKET]).unwrap();
    host.send_audio(&packet).unwrap();
    assert!(
        matches!(receiver.poll(Duration::from_millis(100)).unwrap(), Some(Event::Audio(v)) if v == packet)
    );
}

#[test]
fn real_six_second_late_host_exceeds_old_android_click_deadline() {
    let reserve = UdpSocket::bind(localhost()).unwrap();
    let host_addr = reserve.local_addr().unwrap();
    drop(reserve);
    let endpoint = Endpoint::bind(localhost(), host_addr, key(), Config::default()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let timer = stop.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(12));
        timer.store(true, Ordering::Release);
    });
    let receiver_stop = stop.clone();
    let receiver = thread::spawn(move || {
        endpoint
            .join_cancellable(MonotonicClock::default(), &receiver_stop, |_| {})
            .unwrap()
    });
    thread::sleep(Duration::from_secs(6));
    let host = Endpoint::bind_listener(host_addr, key(), Config::default())
        .unwrap()
        .accept_waiting(MonotonicClock::default(), &stop, |_| {})
        .unwrap();
    let receiver = receiver.join().unwrap();
    assert_eq!(host.peer_addr(), receiver.local_addr().unwrap());
    assert_eq!(receiver.info().epoch_host_ns, 0);
}

fn announce_default_readiness(host: &mut Session, receiver: &mut Session) {
    let readiness = ReceiverReadiness {
        output_horizon_ns: 90_000_000,
        render_block_ns: 10_000_000,
    };
    receiver.report_readiness(readiness).unwrap();
    assert!(
        matches!(host.poll(Duration::from_millis(100)).unwrap(), Some(Event::ReceiverReady(value)) if value == readiness)
    );
    assert!(
        matches!(receiver.poll(Duration::from_millis(100)).unwrap(), Some(Event::ReadinessAcknowledged(value)) if value == readiness)
    );
}
