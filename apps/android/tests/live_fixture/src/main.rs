//! Test-only live source. Known public test key, loopback-only listener, no capture.
use roomsync_core::protocol::SAMPLE_RATE;
use roomsync_desktop::{
    CapturePacketizer,
    budget::{OutputBudget, choose_lead},
};
use roomsync_transport::{Config, Endpoint, Event, MonotonicClock, PairingKey, SessionInfo};
use std::{
    io::{self, Write},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn now_ns() -> roomsync_transport::Result<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return Err(roomsync_transport::Error::ClockUnavailable);
    }
    Ok(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let lead_ms: u64 = std::env::args().nth(1).unwrap().parse()?;
    let listener = Endpoint::bind_listener(
        "127.0.0.1:0".parse()?,
        PairingKey::from_hex(&"1".repeat(64))?,
        Config::default(),
    )?;
    println!("{}", listener.local_addr()?.port());
    io::stdout().flush()?;
    let mut session = listener.accept_waiting(
        MonotonicClock::from_source(now_ns),
        &AtomicBool::new(false),
        |_| {},
    )?;
    let wait = Instant::now();
    let mut heartbeat = Instant::now();
    while session.peer_readiness().is_none() {
        if wait.elapsed() > Duration::from_secs(4) {
            return Err("readiness missing".into());
        }
        session.poll(Duration::from_millis(1))?;
        if heartbeat.elapsed() > Duration::from_millis(500) {
            session.heartbeat()?;
            heartbeat = Instant::now();
        }
    }
    let ready = session.peer_readiness().unwrap();
    let lead = match choose_lead(
        lead_ms * 1_000_000,
        10_000_000,
        &[OutputBudget {
            output_horizon_ns: ready.output_horizon_ns,
            render_block_ns: ready.render_block_ns,
        }],
    ) {
        Ok(budget) => budget.lead_ns,
        Err(reason) => {
            println!("UNSUPPORTED: {reason}");
            session.close()?;
            return Ok(());
        }
    };
    let info = SessionInfo {
        session_id: session.info().session_id,
        epoch_host_ns: now_ns()? + 1_500_000_000,
    };
    // The transport now rejects the former insufficient fixed100ms timeline.
    if ready.output_horizon_ns > 100_000_000 {
        assert!(session.set_timeline_with_lead(info, 100_000_000).is_err());
    }
    session.set_timeline_with_lead(info, lead)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if Instant::now() > deadline {
            return Err("timeline ack missing".into());
        }
        if matches!(
            session.poll(Duration::from_millis(1))?,
            Some(Event::TimelineAcknowledged(_))
        ) {
            break;
        }
    }
    // Fault injection reproduces the previous source-availability bug without
    // weakening readiness validation in the actual transport or receiver.
    let source_lead = if std::env::args().any(|a| a == "legacy-source") {
        100_000_000
    } else {
        lead
    };
    let source_epoch = info.epoch_host_ns - source_lead;
    let mut source = CapturePacketizer::new(info.session_id, source_epoch)?;
    let mut block = 0u64;
    let mut send_lags = Vec::new();
    let until = Instant::now() + Duration::from_secs(8);
    let mut heartbeat = Instant::now();
    while Instant::now() < until {
        // A 10ms WASAPI capture buffer plus 5ms simulated network delay. A
        // packet is never made available before its live samples exist.
        // Like the production host queue, drain already-available capture data
        // before blocking for network control. One block per poll can accumulate
        // artificial backlog when OS socket timeout granularity exceeds10ms.
        for _ in 0..64 {
            let stamp = source_epoch + block * 10_000_000;
            let now = now_ns()?;
            if now < stamp + 15_000_000 {
                break;
            }
            send_lags.push(now - (stamp + 15_000_000));
            source.consume(
                stamp,
                1e9 / f64::from(SAMPLE_RATE),
                &[[0.5, -0.25]; 480],
                |p| session.send_audio(&p).map_err(|_| "send failed"),
            )?;
            block += 1;
        }
        match session.poll(Duration::from_millis(1)) {
            Ok(Some(Event::Closed)) | Err(_) => break,
            _ => {}
        }
        if heartbeat.elapsed() > Duration::from_millis(500) {
            session.heartbeat()?;
            heartbeat = Instant::now();
        }
    }
    send_lags.sort_unstable();
    if !send_lags.is_empty() {
        let p95 = send_lags[(send_lags.len() * 95).div_ceil(100).saturating_sub(1)];
        println!(
            "Fixture capture-send extra lateness: p95={:.3}ms max={:.3}ms; capture-to-send includes10ms collection+5ms simulated network; samples={}",
            p95 as f64 / 1e6,
            send_lags.last().unwrap().to_owned() as f64 / 1e6,
            send_lags.len()
        );
        println!(
            "FIXTURE_OVER_BUDGET={}",
            send_lags.iter().filter(|v| **v > 25_000_000).count()
        );
    }
    Ok(())
}
