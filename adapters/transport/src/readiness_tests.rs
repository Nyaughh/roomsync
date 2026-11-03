use super::pairing_tests::pair;
use super::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

fn readiness(horizon: u64) -> ReceiverReadiness {
    ReceiverReadiness {
        output_horizon_ns: horizon,
        render_block_ns: 10_000_000,
    }
}
fn info(host: &Session) -> SessionInfo {
    SessionInfo {
        session_id: 889,
        epoch_host_ns: host.clock().now_ns().unwrap() + 3_000_000_000,
    }
}
fn take_datagram(socket: &UdpSocket) -> Vec<u8> {
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut bytes = [0; MAX_DATAGRAM_BYTES + 1];
    let (n, _) = socket.recv_from(&mut bytes).unwrap();
    bytes[..n].to_vec()
}
fn announce(host: &mut Session, receiver: &mut Session, value: ReceiverReadiness) {
    receiver.report_readiness(value).unwrap();
    assert!(
        matches!(host.poll(Duration::from_millis(100)).unwrap(), Some(Event::ReceiverReady(v)) if v == value)
    );
    assert!(
        matches!(receiver.poll(Duration::from_millis(100)).unwrap(), Some(Event::ReadinessAcknowledged(v)) if v == value)
    );
}

#[test]
fn readiness_is_required_before_timeline_and_legacy_api_cannot_bypass_it() {
    let (mut host, mut receiver) = pair();
    let timeline = info(&host);
    assert!(matches!(
        host.set_timeline_with_lead(timeline, 250_000_000),
        Err(Error::ReceiverNotReady)
    ));
    assert!(matches!(
        host.set_timeline(timeline),
        Err(Error::ReceiverNotReady)
    ));
    receiver.request_clock().unwrap();
    host.poll(Duration::from_millis(5)).unwrap();
    assert!(matches!(
        receiver.poll(Duration::from_millis(100)).unwrap(),
        Some(Event::ClockSample(_))
    ));
    host.heartbeat().unwrap();
    assert!(matches!(
        receiver.poll(Duration::from_millis(100)).unwrap(),
        Some(Event::Heartbeat)
    ));
    announce(&mut host, &mut receiver, readiness(120_000_000));
    assert!(matches!(
        host.set_timeline(timeline),
        Err(Error::ReceiverNotReady)
    ));
    assert!(host.set_timeline_with_lead(timeline, 100_000_000).is_err());
    host.set_timeline_with_lead(timeline, 250_000_000).unwrap();
    assert!(
        matches!(receiver.poll(Duration::from_millis(100)).unwrap(), Some(Event::Timeline(v)) if v == timeline)
    );
    assert_eq!(receiver.negotiated_lead_ns(), Some(250_000_000));
    assert!(
        matches!(host.poll(Duration::from_millis(100)).unwrap(), Some(Event::TimelineAcknowledged(v)) if v == timeline)
    );
}

#[test]
fn lost_readiness_and_ack_recover_without_duplicate_ready_event() {
    let (mut host, mut receiver) = pair();
    let value = readiness(100_000_000);
    receiver.report_readiness(value).unwrap();
    let _lost_report = take_datagram(&host.socket);
    receiver.report_readiness(value).unwrap();
    assert!(
        matches!(host.poll(Duration::from_millis(100)).unwrap(), Some(Event::ReceiverReady(v)) if v == value)
    );
    let _lost_ack = take_datagram(&receiver.socket);
    assert!(!receiver.readiness_acknowledged());
    receiver.report_readiness(value).unwrap();
    assert!(host.poll(Duration::from_millis(5)).unwrap().is_none());
    assert!(
        matches!(receiver.poll(Duration::from_millis(100)).unwrap(), Some(Event::ReadinessAcknowledged(v)) if v == value)
    );
    assert!(receiver.readiness_acknowledged());
    assert_eq!(host.peer_readiness(), Some(value));
}

#[test]
fn older_readiness_and_ack_cannot_replace_current_measurement() {
    let (mut host, mut receiver) = pair();
    let older = readiness(80_000_000);
    let newer = readiness(180_000_000);
    receiver.report_readiness(older).unwrap();
    let a = take_datagram(&host.socket);
    receiver.report_readiness(newer).unwrap();
    let b = take_datagram(&host.socket);
    receiver.socket.send_to(&b, receiver.peer).unwrap();
    receiver.socket.send_to(&a, receiver.peer).unwrap();
    assert!(
        matches!(host.poll(Duration::from_millis(100)).unwrap(), Some(Event::ReceiverReady(v)) if v == newer)
    );
    assert!(host.poll(Duration::from_millis(5)).unwrap().is_none());
    assert_eq!(host.peer_readiness(), Some(newer));
    assert!(
        matches!(receiver.poll(Duration::from_millis(100)).unwrap(), Some(Event::ReadinessAcknowledged(v)) if v == newer)
    );
    let stale_ack = host.seal(&readiness_payload(READINESS_ACK, older)).unwrap();
    host.socket.send_to(&stale_ack, host.peer).unwrap();
    assert!(receiver.poll(Duration::from_millis(5)).unwrap().is_none());
    assert_eq!(receiver.reported_readiness, Some(newer));
}

#[test]
fn budgets_are_bounded_without_overflow_or_silent_clamping() {
    let (mut host, mut receiver) = pair();
    for invalid in [
        ReceiverReadiness {
            output_horizon_ns: u64::MAX,
            render_block_ns: 1,
        },
        ReceiverReadiness {
            output_horizon_ns: 1_000_000_001,
            render_block_ns: 1,
        },
        ReceiverReadiness {
            output_horizon_ns: 0,
            render_block_ns: 0,
        },
        ReceiverReadiness {
            output_horizon_ns: 0,
            render_block_ns: 100_000_001,
        },
    ] {
        assert!(invalid.minimum_lead_ns().is_err());
        assert!(receiver.report_readiness(invalid).is_err());
        let bytes = receiver
            .seal(&readiness_payload(RECEIVER_READINESS, invalid))
            .unwrap();
        receiver.socket.send_to(&bytes, receiver.peer).unwrap();
    }
    assert!(host.poll(Duration::from_millis(10)).unwrap().is_none());
    assert!(host.peer_readiness().is_none());
    announce(&mut host, &mut receiver, readiness(0));
    for invalid_lead in [0, 9_999_999, 300_000_001, u64::MAX] {
        assert!(
            host.set_timeline_with_lead(info(&host), invalid_lead)
                .is_err()
        );
    }
    // Valid measurement, unsupported hardware route: telemetry is accepted, but
    // the 1.1s requirement cannot be squeezed into the fixed 320 ms jitter window.
    announce(
        &mut host,
        &mut receiver,
        ReceiverReadiness {
            output_horizon_ns: 1_000_000_000,
            render_block_ns: 100_000_000,
        },
    );
    assert!(
        host.set_timeline_with_lead(info(&host), 300_000_000)
            .is_err()
    );
}

#[test]
fn receiver_rejects_authenticated_timeline_below_its_declared_minimum() {
    let (mut host, mut receiver) = pair();
    announce(&mut host, &mut receiver, readiness(120_000_000));
    let bytes = host
        .seal(&timeline_lead_payload(
            SET_TIMELINE,
            info(&host),
            100_000_000,
        ))
        .unwrap();
    host.socket.send_to(&bytes, host.peer).unwrap();
    assert!(matches!(
        receiver.poll(Duration::from_millis(100)),
        Err(Error::InvalidConfig(_))
    ));
    assert!(matches!(receiver.poll(Duration::ZERO), Err(Error::Closed)));
}

#[test]
fn identical_ready_after_timeline_is_harmless_but_changed_budget_stops() {
    let (mut host, mut receiver) = pair();
    let value = readiness(100_000_000);
    announce(&mut host, &mut receiver, value);
    host.set_timeline_with_lead(info(&host), 250_000_000)
        .unwrap();
    assert!(matches!(
        receiver.poll(Duration::from_millis(100)).unwrap(),
        Some(Event::Timeline(_))
    ));
    assert!(matches!(
        host.poll(Duration::from_millis(100)).unwrap(),
        Some(Event::TimelineAcknowledged(_))
    ));
    receiver.report_readiness(value).unwrap();
    assert!(host.poll(Duration::from_millis(5)).unwrap().is_none());
    assert!(
        matches!(receiver.poll(Duration::from_millis(100)).unwrap(), Some(Event::ReadinessAcknowledged(v)) if v == value)
    );
    assert!(matches!(
        receiver.report_readiness(readiness(200_000_000)),
        Err(Error::ReadinessChanged)
    ));
    let changed = receiver
        .seal(&readiness_payload(
            RECEIVER_READINESS,
            readiness(200_000_000),
        ))
        .unwrap();
    receiver.socket.send_to(&changed, receiver.peer).unwrap();
    assert!(matches!(
        host.poll(Duration::from_millis(100)),
        Err(Error::ReadinessChanged)
    ));
    assert!(matches!(host.poll(Duration::ZERO), Err(Error::Closed)));
}

#[test]
fn cancelling_while_readiness_is_unacknowledged_cannot_start_audio() {
    let (mut host, mut receiver) = pair();
    receiver.report_readiness(readiness(100_000_000)).unwrap();
    let _lost = take_datagram(&host.socket);
    receiver.close().unwrap();
    assert!(matches!(
        host.poll(Duration::from_millis(100)).unwrap(),
        Some(Event::Closed)
    ));
    assert!(matches!(
        receiver.report_readiness(readiness(100_000_000)),
        Err(Error::Closed)
    ));
    assert!(host.peer_readiness().is_none());
}

#[test]
fn wrong_roles_cannot_report_or_ack_output_readiness() {
    let (mut host, mut receiver) = pair();
    assert!(matches!(
        host.report_readiness(readiness(1)),
        Err(Error::WrongRole)
    ));
    assert!(matches!(
        receiver.set_timeline_with_lead(info(&host), 250_000_000),
        Err(Error::WrongRole)
    ));
    let wrong = host
        .seal(&readiness_payload(RECEIVER_READINESS, readiness(1)))
        .unwrap();
    host.socket.send_to(&wrong, host.peer).unwrap();
    assert!(receiver.poll(Duration::from_millis(5)).unwrap().is_none());
    assert!(receiver.reported_readiness.is_none());
}

#[test]
fn older_authenticated_interactive_version_gets_an_update_response() {
    let key = PairingKey::from_hex(&"42".repeat(32)).unwrap();
    let key_copy = PairingKey::from_hex(&"42".repeat(32)).unwrap();
    let listener =
        Endpoint::bind_listener("127.0.0.1:0".parse().unwrap(), key, Config::default()).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let child_stop = stop.clone();
    let host = thread::spawn(move || {
        listener.accept_waiting(MonotonicClock::default(), &child_stop, |_| {})
    });
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut state = handshake(&key_copy, true).unwrap();
    let hello = handshake_frame(&mut state, HELLO, b"RoomSync receiver v2").unwrap();
    socket.send_to(&hello, addr).unwrap();
    let response = take_datagram(&socket);
    assert!(is_frame(&response, WELCOME));
    let mut plain = [0; MAX_PLAIN];
    let n = state.read_message(&response[5..], &mut plain).unwrap();
    assert_eq!(n, 24);
    assert_eq!(&plain[..8], b"RSUPDT03");
    stop.store(true, Ordering::Release);
    assert!(matches!(host.join().unwrap(), Err(Error::Cancelled)));
}
