mod wizard;
use crossbeam_queue::ArrayQueue;
use roomsync_core::{
    AudioPacket,
    clock::{ClockModel, ClockSynchronizer},
    device_clock::DeviceClockEstimator,
    render::{ChannelMap, Renderer},
};
use roomsync_desktop::CapturePacketizer;
use roomsync_transport::{Config, Endpoint, Event, MonotonicClock, PairingKey, SessionInfo};
use roomsync_windows::{
    CaptureConfig, CaptureStream, RenderConfig, RenderFill, RenderStream, list_render_endpoints,
    qpc_now_ns,
};
use std::{
    error::Error,
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const HELP: &str = "RoomSync desktop prototype (Windows 10 1703+)\n\n  roomsync-desktop list-devices\n  roomsync-desktop generate-key PRIVATE_KEY_FILE\n  roomsync-desktop host --key FILE --bind IP:PORT --peer IP:PORT [--peer IP:PORT ...] [--latency-ms 100] [--capture-device ID]\n  roomsync-desktop receive --key FILE --bind IP:PORT --host IP:PORT [--channel stereo|left|right|mono] [--output-device ID] [--output-delay-ms 0]\n\nHost uses consecutive local ports, one per listed peer (maximum 8). Configure each receiver's host port accordingly. Share the generated key privately, never in a public repository. Ctrl+C stops audio. This unsigned prototype has not been hardware-calibrated; no zero-latency or measured synchronization promise. Windows system playback only; no microphone capture. The host's original playback is NOT delayed: use an inaudible output/headphones to prevent echo. No device settings are changed.";

fn main() {
    if let Err(error) = run() {
        eprintln!("RoomSync: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "wizard") || (cfg!(windows) && args.is_empty()) {
        args = wizard::arguments()?;
    }
    if args.is_empty() || matches!(args[0].as_str(), "help" | "--help" | "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if args[0] == "generate-key" && args.len() == 2 {
        let key = PairingKey::generate()?;
        let mut file = create_private(&args[1])?;
        file.write_all(key.to_hex_for_storage().as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        println!(
            "Created a private pairing-key file. Transfer its contents privately to your receivers. Never commit or publicly post it."
        );
        return Ok(());
    }
    if args[0] == "list-devices" && args.len() == 1 {
        for endpoint in list_render_endpoints()? {
            println!(
                "{}\n  ID: {}\n  Native mix: {} Hz / {} channels",
                endpoint.name, endpoint.id, endpoint.mix_sample_rate, endpoint.mix_channels
            );
        }
        return Ok(());
    }
    let options = Options::parse(&args)?;
    // Fail explicitly before binding anything on unsupported operating systems.
    qpc_now_ns()?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))?;
    match args[0].as_str() {
        "host" => host(options, stop),
        "receive" => receive(options, stop),
        _ => Err("unknown command; use --help".into()),
    }
}

#[derive(Debug)]
struct Options {
    key_path: String,
    bind: SocketAddr,
    peers: Vec<SocketAddr>,
    host: Option<SocketAddr>,
    capture_device: Option<String>,
    output_device: Option<String>,
    latency_ms: u64,
    output_delay_ns: i64,
    channel: ChannelMap,
}
impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        if args.is_empty() || !matches!(args[0].as_str(), "host" | "receive") {
            return Err("unknown command; use --help".into());
        }
        let mut key = None;
        let mut bind = None;
        let mut peers = Vec::new();
        let mut host = None;
        let mut capture_device = None;
        let mut output_device = None;
        let mut latency_ms = 100u64;
        let mut output_delay_ns = 0i64;
        let mut channel = ChannelMap::Stereo;
        let mut seen = std::collections::HashSet::new();
        for pair in args[1..].chunks(2) {
            if pair.len() != 2 {
                return Err("every option requires a value; use --help".into());
            }
            if pair[0] != "--peer" && !seen.insert(pair[0].clone()) {
                return Err("duplicate option".into());
            }
            match pair[0].as_str() {
                "--key" => key = Some(pair[1].clone()),
                "--bind" => bind = Some(pair[1].parse()?),
                "--peer" => peers.push(pair[1].parse()?),
                "--host" => host = Some(pair[1].parse()?),
                "--capture-device" => capture_device = Some(pair[1].clone()),
                "--output-device" => output_device = Some(pair[1].clone()),
                "--latency-ms" => latency_ms = pair[1].parse()?,
                "--output-delay-ms" => {
                    let value: f64 = pair[1].parse()?;
                    if !value.is_finite() || value.abs() > 20.0 {
                        return Err("output delay must be finite and within +/-20 ms".into());
                    }
                    output_delay_ns = (value * 1e6).round() as i64;
                }
                "--channel" => {
                    channel = match pair[1].as_str() {
                        "stereo" => ChannelMap::Stereo,
                        "left" => ChannelMap::Left,
                        "right" => ChannelMap::Right,
                        "mono" => ChannelMap::Mono,
                        _ => return Err("channel must be stereo, left, right or mono".into()),
                    }
                }
                _ => return Err(format!("unknown option {}", pair[0]).into()),
            }
        }
        let result = Self {
            key_path: key.ok_or("--key is required")?,
            bind: bind.ok_or("--bind is required")?,
            peers,
            host,
            capture_device,
            output_device,
            latency_ms,
            output_delay_ns,
            channel,
        };
        if result.bind.port() == 0 {
            return Err("choose an explicit nonzero local UDP port".into());
        }
        if !(40..=250).contains(&latency_ms) {
            return Err("latency must be within 40..=250 ms".into());
        }
        if args[0] == "host" {
            if result.peers.is_empty()
                || result.peers.len() > 8
                || result.host.is_some()
                || result.output_device.is_some()
                || seen.contains("--channel")
                || seen.contains("--output-delay-ms")
            {
                return Err("host requires 1..=8 peers and accepts only host options".into());
            }
            if u32::from(result.bind.port()) + result.peers.len() as u32 - 1 > u16::MAX as u32 {
                return Err("consecutive host ports overflow".into());
            }
            let mut unique = std::collections::HashSet::new();
            if result.peers.iter().any(|p| !unique.insert(*p)) {
                return Err("duplicate peer endpoint".into());
            }
        } else if result.host.is_none()
            || !result.peers.is_empty()
            || result.capture_device.is_some()
            || seen.contains("--latency-ms")
        {
            return Err("receive requires --host and accepts only receiver options".into());
        }
        Ok(result)
    }
}
fn qpc_source() -> roomsync_transport::Result<u64> {
    qpc_now_ns().map_err(|_| roomsync_transport::Error::ClockUnavailable)
}
fn clock() -> MonotonicClock {
    MonotonicClock::from_source(qpc_source)
}
fn create_private(path: impl AsRef<Path>) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn read_key(path: &str) -> Result<PairingKey> {
    let file = File::open(path)?;
    if file.metadata()?.len() > 66 {
        return Err("pairing-key file is too large".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            return Err("pairing-key file permissions must be 600".into());
        }
    }
    let mut text = Zeroizing::new(String::new());
    file.take(66).read_to_string(&mut text)?;
    Ok(PairingKey::from_hex(&text)?)
}

struct StopGuard(Arc<AtomicBool>);
impl Drop for StopGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
fn host(options: Options, stop: Arc<AtomicBool>) -> Result<()> {
    let _stop_guard = StopGuard(stop.clone());
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(|_| "OS random source failed")?;
    let info = SessionInfo {
        session_id: u64::from_be_bytes(random).max(1),
        epoch_host_ns: qpc_now_ns()?
            .checked_add(15_000_000_000)
            .ok_or("clock overflow")?,
    };
    let (ready_tx, ready_rx) = mpsc::sync_channel(options.peers.len());
    let mut queues = Vec::new();
    let mut workers = Vec::new();
    println!(
        "Pairing {} receiver(s). Shared playback starts after a 15-second clock warmup.\nHost original audio is not delayed. Keep that output inaudible to avoid echo. No endpoint is muted or changed.",
        options.peers.len()
    );
    for (index, peer) in options.peers.iter().enumerate() {
        let mut bind = options.bind;
        bind.set_port(bind.port() + index as u16);
        let endpoint = Endpoint::bind(
            bind,
            *peer,
            read_key(&options.key_path)?,
            Config {
                handshake_timeout: Duration::from_secs(10),
                ..Config::default()
            },
        )?;
        println!("Receiver {}: {} -> {}", index + 1, bind, peer);
        let queue = Arc::new(ArrayQueue::<AudioPacket>::new(128));
        queues.push(queue.clone());
        let stopped = stop.clone();
        let ready = ready_tx.clone();
        workers.push(
            thread::Builder::new()
                .name(format!("roomsync-peer-{index}"))
                .spawn(move || -> std::result::Result<(), String> {
                    let mut session = match endpoint.accept_with_info(clock(), info) {
                        Ok(s) => {
                            let _ = ready.send(Ok(index));
                            s
                        }
                        Err(e) => {
                            let message = e.to_string();
                            let _ = ready.send(Err(message.clone()));
                            return Err(message);
                        }
                    };
                    let mut last_heartbeat = Instant::now();
                    while !stopped.load(Ordering::Acquire) {
                        for _ in 0..64 {
                            let Some(packet) = queue.pop() else {
                                break;
                            };
                            session.send_audio(&packet).map_err(|e| e.to_string())?;
                        }
                        if matches!(
                            session
                                .poll(Duration::from_millis(1))
                                .map_err(|e| e.to_string())?,
                            Some(Event::Closed)
                        ) {
                            return Err("receiver closed its session".into());
                        }
                        if last_heartbeat.elapsed() >= Duration::from_millis(500) {
                            session.heartbeat().map_err(|e| e.to_string())?;
                            last_heartbeat = Instant::now();
                        }
                    }
                    let _ = session.close();
                    Ok(())
                })?,
        );
    }
    drop(ready_tx);
    let operation = (|| -> Result<()> {
        for _ in &options.peers {
            let index = ready_rx.recv_timeout(Duration::from_secs(12))??;
            println!("Receiver {} authenticated.", index + 1);
        }
        let capture = CaptureStream::start(CaptureConfig {
            endpoint_id: options.capture_device,
            ..CaptureConfig::default()
        })?;
        println!(
            "Capturing: {}. Requested presentation lead: {} ms (not measured acoustic latency). Ctrl+C stops.",
            capture.endpoint.name, options.latency_ms
        );
        let source_epoch = info
            .epoch_host_ns
            .saturating_sub(options.latency_ms * 1_000_000);
        let mut packetizer = CapturePacketizer::new(info.session_id, source_epoch)?;
        let mut capture_rate = DeviceClockEstimator::new();
        let mut captured_frames = 0u64;
        let mut seen_audio = false;
        let mut last_stats = Instant::now();
        let result = (|| -> Result<()> {
            while !stop.load(Ordering::Acquire) {
                if workers.iter().any(thread::JoinHandle::is_finished) {
                    return Err(
                        "a receiver connection ended; restarting the session is required".into(),
                    );
                }
                if capture.is_finished() {
                    return Err(
                        "capture device ended; reopen the session after checking the device".into(),
                    );
                }
                if let Some(packet) = capture.try_recv() {
                    let timestamp = packet
                        .qpc_ns
                        .ok_or("capture timestamp unavailable; session stopped")?;
                    if seen_audio && packet.discontinuity {
                        capture_rate.reset();
                        packetizer.mark_discontinuity();
                    }
                    seen_audio = true;
                    let period = capture_rate
                        .observe(timestamp, captured_frames, 48_000)
                        .unwrap_or(1e9 / 48_000.0);
                    captured_frames = captured_frames.saturating_add(packet.frames().len() as u64);
                    packetizer.consume(timestamp, period, packet.frames(), |audio| {
                        for queue in &queues { queue.push(audio.clone()).map_err(|_| "network queue overflow; session stopped to avoid growing latency")?; }
                        Ok(())
                    })?;
                } else {
                    thread::sleep(Duration::from_millis(1));
                }
                if last_stats.elapsed() >= Duration::from_secs(5) {
                    let stats = capture.stats();
                    println!(
                        "source packets {}, captured frames {}, capture queue drops {}",
                        packetizer.packets_emitted(),
                        stats.frames,
                        stats.dropped_packets
                    );
                    last_stats = Instant::now();
                }
            }
            Ok(())
        })();
        let stopped = capture.stop();
        result?;
        stopped?;
        Ok(())
    })();
    stop.store(true, Ordering::Release);
    let mut worker_error = None;
    for worker in workers {
        match worker.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => worker_error = Some(e),
            Err(_) => worker_error = Some("network worker panicked".into()),
        }
    }
    operation?;
    if let Some(error) = worker_error {
        return Err(error.into());
    }
    Ok(())
}

fn receive(options: Options, stop: Arc<AtomicBool>) -> Result<()> {
    let endpoint = Endpoint::bind(
        options.bind,
        options.host.ok_or("host missing")?,
        read_key(&options.key_path)?,
        Config {
            handshake_timeout: Duration::from_secs(10),
            ..Config::default()
        },
    )?;
    println!("Pairing with the configured Windows host...");
    let mut session = endpoint.join(clock())?;
    let info = session.info();
    let audio = Arc::new(ArrayQueue::new(128));
    let models = Arc::new(ArrayQueue::<ClockModel>::new(8));
    let concealed = Arc::new(AtomicU64::new(0));
    let callback_failed = Arc::new(AtomicBool::new(false));
    let callback_audio = audio.clone();
    let callback_models = models.clone();
    let callback_concealed = concealed.clone();
    let callback_error = callback_failed.clone();
    let mut renderer = Renderer::new(info.session_id, info.epoch_host_ns, options.channel)
        .map_err(|_| "invalid renderer session")?;
    let mut model = None;
    let render = RenderStream::start(
        RenderConfig {
            endpoint_id: options.output_device,
            extra_output_delay_ns: options.output_delay_ns,
            ..RenderConfig::default()
        },
        move |timing, out| {
            for _ in 0..8 {
                let Some(next) = callback_models.pop() else {
                    break;
                };
                model = Some(next);
            }
            for _ in 0..128 {
                let Some(packet) = callback_audio.pop() else {
                    break;
                };
                renderer.push(packet);
            }
            let Some(clock) = model else {
                return RenderFill::Silence;
            };
            match renderer.render(
                timing.estimated_dac_start_ns,
                timing.sample_rate,
                clock,
                out,
            ) {
                Ok(report) => {
                    callback_concealed.fetch_add(report.concealed_frames as u64, Ordering::Relaxed);
                    if report.waiting_frames == report.frames {
                        RenderFill::Silence
                    } else {
                        RenderFill::Audio
                    }
                }
                Err(_) => {
                    callback_error.store(true, Ordering::Release);
                    RenderFill::Silence
                }
            }
        },
    )?;
    println!(
        "Authenticated. Output: {}. Channel: {:?}. Output timing is estimated, not acoustically calibrated. Ctrl+C stops.",
        render.endpoint.name, options.channel
    );
    let mut sync = ClockSynchronizer::new();
    let mut next_sync = Instant::now();
    let mut last_stats = Instant::now();
    let mut queue_drops = 0u64;
    let mut rejected_sync = 0u64;
    let result = (|| -> Result<()> {
        while !stop.load(Ordering::Acquire) {
            if render.is_finished() || callback_failed.load(Ordering::Acquire) {
                return Err(
                    "output device stopped or timing configuration failed; restart the receiver"
                        .into(),
                );
            }
            if Instant::now() >= next_sync {
                match session.request_clock() {
                    Ok(()) | Err(roomsync_transport::Error::PendingClockLimit) => {}
                    Err(e) => return Err(e.into()),
                }
                next_sync = Instant::now() + Duration::from_millis(100);
            }
            match session.poll(Duration::from_millis(5))? {
                Some(Event::Audio(packet)) => {
                    if audio.push(packet).is_err() {
                        queue_drops += 1;
                    }
                }
                Some(Event::ClockSample(exchange)) => match sync.observe(exchange) {
                    Ok(model) => {
                        if models.is_full() {
                            models.pop();
                        }
                        let _ = models.push(model);
                    }
                    Err(roomsync_core::clock::ClockError::Discontinuity) => {
                        return Err(
                            "host/local clock changed abruptly; restart the receiver session"
                                .into(),
                        );
                    }
                    Err(_) => rejected_sync = rejected_sync.saturating_add(1),
                },
                Some(Event::Closed) => break,
                _ => {}
            }
            if last_stats.elapsed() >= Duration::from_secs(5) {
                println!(
                    "clock locked/fresh {}, received {}, concealed frames {}, queue drops {}, rejected clock samples {}",
                    sync.model().is_some_and(
                        |m| m.is_locked() && m.is_fresh_at(qpc_now_ns().unwrap_or(u64::MAX))
                    ),
                    session.stats().audio_received,
                    concealed.load(Ordering::Relaxed),
                    queue_drops,
                    rejected_sync
                );
                last_stats = Instant::now();
            }
        }
        Ok(())
    })();
    let _ = session.close();
    let stopped = render.stop();
    result?;
    stopped?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }
    #[test]
    fn parser_accepts_bounded_multi_peer_host() {
        let p = Options::parse(&args(
            "host --key k --bind 127.0.0.1:4000 --peer 127.0.0.1:5000 --peer 127.0.0.1:5001",
        ))
        .unwrap();
        assert_eq!(p.peers.len(), 2);
    }
    #[test]
    fn parser_rejects_overflow_duplicate_and_nonfinite_options() {
        for s in [
            "host --key k --bind 127.0.0.1:65535 --peer 127.0.0.1:5000 --peer 127.0.0.1:5001",
            "receive --key k --bind 127.0.0.1:4000 --host 127.0.0.1:5000 --output-delay-ms NaN",
            "host --key k --key k --bind 127.0.0.1:4000 --peer 127.0.0.1:5000",
        ] {
            assert!(Options::parse(&args(s)).is_err());
        }
    }
}
