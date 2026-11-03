//! Authenticated peer admission and shared timeline orchestration for local controls.
use crate::control::{Plan, SharedState, update};
use crate::{Result, clock};
use crossbeam_queue::ArrayQueue;
use roomsync_core::{AudioPacket, device_clock::DeviceClockEstimator};
use roomsync_desktop::{
    CapturePacketizer,
    budget::{OutputBudget, choose_lead},
};
use roomsync_transport::{Config, Endpoint, Event, PairingKey, SessionInfo};
use roomsync_windows::{CaptureConfig, CaptureStream, qpc_now_ns};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub fn run(plan: Plan, cancel: Arc<AtomicBool>, state: SharedState) -> Result<()> {
    qpc_now_ns()?;
    let _guard = crate::StopGuard(cancel.clone());
    let count = plan.settings.receivers;
    // Bind all requested sockets before starting threads, so port conflicts are
    // reported atomically and cannot leave a half-started group listening.
    let mut listeners = Vec::with_capacity(count);
    for index in 0..count {
        let addr = SocketAddr::from((
            plan.settings.host_ip,
            plan.settings.base_port + index as u16,
        ));
        let key = PairingKey::from_hex(plan.key.as_str())?;
        listeners.push(Endpoint::bind_listener(addr, key, Config::default())?);
    }
    let shared_timeline = Arc::new(Mutex::new(None::<(SessionInfo, u64)>));
    let readiness = Arc::new(Mutex::new(vec![None::<OutputBudget>; count]));
    let connected: Vec<_> = (0..count)
        .map(|_| Arc::new(AtomicBool::new(false)))
        .collect();
    let acknowledged: Vec<_> = (0..count)
        .map(|_| Arc::new(AtomicBool::new(false)))
        .collect();
    let (fail_tx, fail_rx) = mpsc::sync_channel::<String>(count);
    let mut queues = Vec::with_capacity(count);
    let mut workers = Vec::with_capacity(count);
    for (index, listener) in listeners.into_iter().enumerate() {
        let queue = Arc::new(ArrayQueue::<AudioPacket>::new(128));
        queues.push(queue.clone());
        let stop = cancel.clone();
        let timeline = shared_timeline.clone();
        let peer_readiness = readiness.clone();
        let peer_ready = connected[index].clone();
        let peer_ack = acknowledged[index].clone();
        let ui = state.clone();
        let failures = fail_tx.clone();
        workers.push(
            thread::Builder::new()
                .name(format!("roomsync-listener-{index}"))
                .spawn(move || -> std::result::Result<(), String> {
                    let operation = (|| -> std::result::Result<(), String> {
                        let mut session = match listener.accept_waiting(clock(), &stop, |progress| {
                            update(&ui, |s| {
                                if let Some(peer) = s.peers.get_mut(index) {
                                    peer.message = format!(
                                        "{} ({} UDP packets received)",
                                        progress.description(), progress.datagrams_received
                                    );
                                }
                            });
                        }) {
                            Ok(session) => session,
                            Err(roomsync_transport::Error::Cancelled) if stop.load(Ordering::Acquire) => return Ok(()),
                            Err(error) => return Err(format!("Receiver {}: {error}", index + 1)),
                        };
                        let peer = session.peer_addr();
                        peer_ready.store(true, Ordering::Release);
                        update(&ui, |s| {
                            if let Some(p) = s.peers.get_mut(index) {
                                p.message = format!("Authenticated at {peer}; waiting for output calibration");
                            }
                        });
                        let mut heartbeat = Instant::now();
                        let mut timeline_retry = Instant::now() - Duration::from_secs(1);
                        let mut calibrated = None;
                        while !stop.load(Ordering::Acquire) {
                            let target = *timeline.lock().map_err(|_| "timeline state unavailable")?;
                            if let Some((info, lead_ns)) = target {
                                if !peer_ack.load(Ordering::Acquire) && timeline_retry.elapsed() >= Duration::from_millis(100) {
                                    session.set_timeline_with_lead(info, lead_ns).map_err(|e| e.to_string())?;
                                    timeline_retry = Instant::now();
                                }
                            }
                            for _ in 0..64 {
                                let Some(packet) = queue.pop() else { break; };
                                session.send_audio(&packet).map_err(|e| e.to_string())?;
                            }
                            match session.poll(Duration::from_millis(2)).map_err(|e| e.to_string())? {
                                Some(Event::ReceiverReady(value)) => {
                                    let output = OutputBudget {
                                        output_horizon_ns: value.output_horizon_ns,
                                        render_block_ns: value.render_block_ns,
                                    };
                                    // A readiness report is a completed calibration, not a
                                    // moving target. Freeze it even before timeline publication
                                    // so the main thread cannot budget a stale snapshot.
                                    if calibrated.is_some_and(|previous| previous != output) {
                                        return Err(format!("Receiver {} changed its output timing after calibration. Reconnect the group.", index + 1));
                                    }
                                    calibrated = Some(output);
                                    peer_readiness.lock().map_err(|_| "readiness state unavailable")?[index] = Some(output);
                                    update(&ui, |s| {
                                        if let Some(p) = s.peers.get_mut(index) {
                                            p.message = format!("Output calibrated: {:.1} ms ahead + {:.1} ms batch", value.output_horizon_ns as f64 / 1e6, value.render_block_ns as f64 / 1e6);
                                        }
                                    });
                                }
                                Some(Event::TimelineAcknowledged(info)) if target.is_some_and(|(expected, _)| expected == info) => {
                                    peer_ack.store(true, Ordering::Release);
                                    update(&ui, |s| {
                                        if let Some(p) = s.peers.get_mut(index) {
                                            p.message = format!("Connected at {peer}; shared timeline confirmed");
                                        }
                                    });
                                }
                                Some(Event::Closed) => return Err(format!("Receiver {} disconnected. Stop and start listening to reconnect the group.", index + 1)),
                                _ => {}
                            }
                            if heartbeat.elapsed() >= Duration::from_millis(500) {
                                session.heartbeat().map_err(|e| e.to_string())?;
                                heartbeat = Instant::now();
                            }
                        }
                        let _ = session.close();
                        Ok(())
                    })();
                    if let Err(error) = &operation { let _ = failures.try_send(error.clone()); }
                    operation
                })?,
        );
    }
    drop(fail_tx);
    let operation = (|| -> Result<()> {
        while !connected.iter().all(|ready| ready.load(Ordering::Acquire)) {
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            if let Ok(error) = fail_rx.try_recv() {
                return Err(error.into());
            }
            thread::sleep(Duration::from_millis(20));
        }
        update(&state, |s| {
            s.phase = "calibrating".into();
            s.title = "Calibrating phone audio".into();
            s.message =
                "Measuring each output queue before choosing one safe shared playback delay."
                    .into();
        });
        let outputs = loop {
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            if let Ok(error) = fail_rx.try_recv() {
                return Err(error.into());
            }
            let values: Option<Vec<_>> = readiness
                .lock()
                .map_err(|_| "readiness state unavailable")?
                .iter()
                .copied()
                .collect();
            if let Some(values) = values {
                break values;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let capture = CaptureStream::start(CaptureConfig {
            endpoint_id: plan.settings.capture_device.clone(),
            ..CaptureConfig::default()
        })?;
        let buffer_ns = (u64::from(capture.endpoint.buffer_frames) * 1_000_000_000)
            .div_ceil(u64::from(capture.endpoint.stream_sample_rate));
        let capture_allowance = buffer_ns
            .max(capture.endpoint.maximum_stream_latency_ns)
            .max(capture.endpoint.engine_period_ns);
        let budget = choose_lead(
            plan.settings.latency_ms * 1_000_000,
            capture_allowance,
            &outputs,
        )?;
        update(&state, |s| {
            s.lead_ms = Some(budget.lead_ns as f64 / 1_000_000.0);
            s.capture_allowance_ms = capture_allowance as f64 / 1e6;
        });
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|_| "OS random source unavailable")?;
        let info = SessionInfo {
            session_id: u64::from_be_bytes(random).max(1),
            epoch_host_ns: qpc_now_ns()?
                .checked_add(5_000_000_000)
                .ok_or("clock overflow")?,
        };
        *shared_timeline
            .lock()
            .map_err(|_| "timeline state unavailable")? = Some((info, budget.lead_ns));
        update(&state, |s| {
            s.phase = "synchronizing".into();
            s.title = "Everyone is connected".into();
            s.message =
                "Confirming one shared playback timeline, then warming up clocks for five seconds."
                    .into();
        });
        while !acknowledged.iter().all(|ack| ack.load(Ordering::Acquire)) {
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            if let Ok(error) = fail_rx.try_recv() {
                return Err(error.into());
            }
            if qpc_now_ns()?.saturating_add(2_000_000_000) >= info.epoch_host_ns {
                return Err("A receiver did not confirm the playback timeline. Stop and start listening again; no audio was started.".into());
            }
            thread::sleep(Duration::from_millis(20));
        }
        let source_epoch = info.epoch_host_ns.saturating_sub(budget.lead_ns);
        let mut packetizer = CapturePacketizer::new(info.session_id, source_epoch)?;
        let mut capture_rate = DeviceClockEstimator::new();
        let mut captured_frames = 0u64;
        let mut seen_audio = false;
        let mut ui_update = Instant::now();
        let mut announced = false;
        let mut capture_peak = 0.0f32;
        let mut source_peak = 0.0f32;
        let mut capture_age_ms = 0.0f64;
        let capture_result = (|| -> Result<()> {
            while !cancel.load(Ordering::Acquire) {
                if let Ok(error) = fail_rx.try_recv() {
                    return Err(error.into());
                }
                if capture.is_finished() {
                    return Err("Windows capture stopped. Check the output device, then stop and restart listening.".into());
                }
                if let Some(packet) = capture.try_recv() {
                    let stamp = packet.qpc_ns.ok_or(
                        "Windows could not provide a capture timestamp; restart listening",
                    )?;
                    if seen_audio && packet.discontinuity {
                        capture_rate.reset();
                        packetizer.mark_discontinuity();
                    }
                    seen_audio = true;
                    capture_peak = packet
                        .frames()
                        .iter()
                        .flatten()
                        .filter(|value| value.is_finite())
                        .fold(capture_peak, |peak, value| peak.max(value.abs()));
                    capture_age_ms =
                        capture_age_ms.max(qpc_now_ns()?.saturating_sub(stamp) as f64 / 1e6);
                    let period = capture_rate
                        .observe(stamp, captured_frames, 48_000)
                        .unwrap_or(1e9 / 48_000.0);
                    captured_frames = captured_frames.saturating_add(packet.frames().len() as u64);
                    packetizer.consume(stamp, period, packet.frames(), |audio| {
                        source_peak = audio.samples().iter().fold(source_peak, |peak, sample| {
                            peak.max(sample.unsigned_abs() as f32 / 32768.0)
                        });
                        for queue in &queues {
                            queue.push(audio.clone()).map_err(|_| "Network queue overflow; session stopped instead of accumulating delay")?;
                        }
                        Ok(())
                    })?;
                } else {
                    thread::sleep(Duration::from_millis(1));
                }
                if !announced && qpc_now_ns()? >= info.epoch_host_ns {
                    announced = true;
                    update(&state, |s| {
                        s.phase = "streaming".into();
                        s.title = "Shared timeline is running".into();
                        s.message="Play music through the Windows capture output. Keep its original speaker inaudible to avoid echo. Synchronization is estimated, not acoustically calibrated.".into();
                    });
                }
                if ui_update.elapsed() >= Duration::from_millis(500) {
                    update(&state, |s| {
                        s.packets = packetizer.packets_emitted();
                        s.capture_peak = capture_peak;
                        s.source_peak = source_peak;
                        s.capture_age_ms = capture_age_ms;
                    });
                    capture_peak = 0.0;
                    source_peak = 0.0;
                    capture_age_ms = 0.0;
                    ui_update = Instant::now();
                }
            }
            Ok(())
        })();
        let stopped = capture.stop();
        capture_result?;
        stopped?;
        Ok(())
    })();
    let user_stopped = cancel.load(Ordering::Acquire);
    cancel.store(true, Ordering::Release);
    let mut worker_error = None;
    for worker in workers {
        match worker.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => worker_error = Some(error),
            Err(_) => worker_error = Some("Network worker ended unexpectedly".into()),
        }
    }
    operation?;
    if !user_stopped {
        if let Some(error) = worker_error {
            return Err(error.into());
        }
    }
    Ok(())
}
