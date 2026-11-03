//! Deliberate UDP phase delays, using the production five-second phase budget.
//! Neither timing is a test retry or an enlarged product timeout.
use super::*;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

#[test]
fn authenticated_confirmation_has_its_own_deadline() {
    let config = Config::default();
    assert_eq!(config.handshake_timeout, Duration::from_secs(5));
    let key = || PairingKey::from_hex(&"53".repeat(32)).unwrap();
    let listener = Endpoint::bind_listener("127.0.0.1:0".parse().unwrap(), key(), config).unwrap();
    let host_addr = listener.local_addr().unwrap();
    let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    relay
        .set_read_timeout(Some(Duration::from_millis(10)))
        .unwrap();
    let endpoint = Endpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        relay.local_addr().unwrap(),
        key(),
        config,
    )
    .unwrap();
    let receiver_addr = endpoint.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let accepted = Arc::new(AtomicBool::new(false));
    let host_stop = stop.clone();
    let host_accepted = accepted.clone();
    let host = thread::spawn(move || -> Result<Session> {
        let mut session = listener.accept_waiting(MonotonicClock::default(), &host_stop, |_| {})?;
        host_accepted.store(true, Ordering::Release);
        // A real host continues polling, including final-ACK retransmissions.
        while !host_stop.load(Ordering::Acquire) {
            session.poll(Duration::from_millis(5))?;
        }
        Ok(session)
    });
    let progress = Arc::new(Mutex::new(PairingProgress::default()));
    let seen = progress.clone();
    let receiver_stop = stop.clone();
    let receiver = thread::spawn(move || {
        endpoint.join_cancellable(MonotonicClock::default(), &receiver_stop, |p| {
            *seen.lock().unwrap() = p
        })
    });
    let mut first_hello = None::<Instant>;
    let mut welcome = None::<Vec<u8>>;
    let mut acknowledgement = None::<Vec<u8>>;
    let mut welcome_released = false;
    let mut ack_released = false;
    let mut acknowledgements_seen = 0;
    let fail_safe = Instant::now() + Duration::from_secs(8);
    let mut bytes = [0; MAX_DATAGRAM_BYTES + 1];
    while !receiver.is_finished() && Instant::now() < fail_safe {
        if let Some(started) = first_hello {
            // WELCOME arrives late in HELLO's five-second attempt, but READY
            // still reaches the host well inside its own candidate deadline.
            if !welcome_released && started.elapsed() >= Duration::from_secs(4) {
                if let Some(bytes) = &welcome {
                    relay.send_to(bytes, receiver_addr).unwrap();
                    welcome_released = true;
                }
            }
            // ACCEPTED arrives after the original HELLO deadline, yet less than
            // a full confirmation timeout after the authenticated transition.
            if !ack_released && started.elapsed() >= Duration::from_secs(6) {
                if let Some(bytes) = &acknowledgement {
                    relay.send_to(bytes, receiver_addr).unwrap();
                    ack_released = true;
                }
            }
        }
        match relay.recv_from(&mut bytes) {
            Ok((n, from)) if from == receiver_addr => {
                if is_frame(&bytes[..n], HELLO) && first_hello.is_none() {
                    first_hello = Some(Instant::now());
                }
                relay.send_to(&bytes[..n], host_addr).unwrap();
            }
            Ok((n, from)) if from == host_addr => {
                if is_frame(&bytes[..n], WELCOME) && !welcome_released {
                    welcome = Some(bytes[..n].to_vec());
                } else if is_frame(&bytes[..n], DATA)
                    && n >= HEADER
                    && read_u64(&bytes[5..HEADER]) == 0
                    && !ack_released
                {
                    acknowledgements_seen += 1;
                    // Drop the first ACCEPTED completely: the host must poll
                    // its returned Session and service a repeated READY.
                    if acknowledgements_seen > 1 {
                        acknowledgement = Some(bytes[..n].to_vec());
                    }
                } else {
                    relay.send_to(&bytes[..n], receiver_addr).unwrap();
                }
            }
            Ok(_) => panic!("unexpected relay source"),
            Err(e)
                if transient(&e)
                    || matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused
                    ) => {}
            Err(e) => panic!("relay I/O: {e}"),
        }
    }
    stop.store(true, Ordering::Release);
    let receiver_result = receiver.join().unwrap();
    let host_result = host.join().unwrap();
    assert!(
        accepted.load(Ordering::Acquire),
        "host never authenticated READY"
    );
    assert!(
        welcome_released && ack_released,
        "delay schedule did not run"
    );
    let receiver = receiver_result.unwrap_or_else(|e| {
        panic!(
            "receiver lost an authenticated confirmation: {e}; progress={:?}",
            *progress.lock().unwrap()
        )
    });
    assert!(host_result.is_ok(), "active host stopped unexpectedly");
    assert_eq!(receiver.local_addr().unwrap(), receiver_addr);
}
