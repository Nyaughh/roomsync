//! Android JNI boundary. Socket I/O has its own thread. AudioTrack's writer owns
//! rendering; no network operation runs on Android's audio or UI thread.
use jni::{
    JNIEnv,
    objects::{JCharArray, JClass, JFloatArray, JString},
    sys::{jint, jlong, jstring},
};
use roomsync_core::{
    clock::ClockSynchronizer,
    jitter::InsertResult,
    render::{ChannelMap, Renderer},
};
use roomsync_transport::{
    Config, Endpoint, Event, MonotonicClock, PairingKey, PairingProgress, ReceiverReadiness,
    SessionInfo,
};
use std::{
    net::{IpAddr, SocketAddr, UdpSocket},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const MAX_RENDER_FRAMES: usize = 960;

fn monotonic_ns() -> roomsync_transport::Result<u64> {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: Valid stack timespec; CLOCK_MONOTONIC is AudioTimestamp.nanoTime's domain.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) } != 0
        || t.tv_sec < 0
        || t.tv_nsec < 0
    {
        return Err(roomsync_transport::Error::ClockUnavailable);
    }
    (t.tv_sec as u64)
        .checked_mul(1_000_000_000)
        .and_then(|s| s.checked_add(t.tv_nsec as u64))
        .ok_or(roomsync_transport::Error::ClockUnavailable)
}

#[derive(Clone)]
struct RecentSamples {
    recent: [u64; 256],
    count: usize,
    next: usize,
    maximum: u64,
}
impl Default for RecentSamples {
    fn default() -> Self {
        Self {
            recent: [0; 256],
            count: 0,
            next: 0,
            maximum: 0,
        }
    }
}
impl RecentSamples {
    fn record(&mut self, value: u64) {
        self.recent[self.next] = value;
        self.next = (self.next + 1) % self.recent.len();
        self.count = (self.count + 1).min(self.recent.len());
        self.maximum = self.maximum.max(value);
    }
    fn p95(&self) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let mut sorted = self.recent;
        sorted[..self.count].sort_unstable();
        sorted[(self.count * 95).div_ceil(100) - 1]
    }
    fn describe_ms(&self) -> String {
        if self.count == 0 {
            "not measured".into()
        } else {
            format!(
                "{:.1} / {:.1}",
                self.p95() as f64 / 1e6,
                self.maximum as f64 / 1e6
            )
        }
    }
}

/// Diagnostic samples are fixed-size and never affect playout or admission.
#[derive(Clone, Default)]
struct ReceiveTiming {
    poll_overrun: RecentSamples,
    between_polls: RecentSamples,
    audio_gap: RecentSamples,
    network_lock: RecentSamples,
    render_lock: RecentSamples,
    render_work: RecentSamples,
    source_age: RecentSamples,
}
impl ReceiveTiming {
    fn describe(&self) -> String {
        format!(
            "Receive timing, per-metric p95 last up to 256 / session max including startup (ms):\nAudio receive gap: {} · Estimated source age: {}\nSocket poll overrun beyond 5 ms: {} · Between polls: {}\nAudio insert lock wait: {} · Render state-lock wait: {} · Render work: {}\nSource age is inferred from the shared schedule; it includes capture, host sending, transit and receiver processing, not network-only delay.",
            self.audio_gap.describe_ms(),
            self.source_age.describe_ms(),
            self.poll_overrun.describe_ms(),
            self.between_polls.describe_ms(),
            self.network_lock.describe_ms(),
            self.render_lock.describe_ms(),
            self.render_work.describe_ms(),
        )
    }
}
fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}
fn source_age_ns(
    clock: roomsync_core::clock::ClockModel,
    timeline: SessionInfo,
    lead_ns: u64,
    last_frame: u64,
    received_ns: u64,
) -> Option<u64> {
    if !clock.is_locked() || !clock.is_fresh_at(received_ns) {
        return None;
    }
    // Source time is common playback time minus the negotiated lead. This
    // includes capture batching, host dispatch, transit and local processing;
    // it is deliberately not described as a network latency measurement.
    let age = clock.host_elapsed_ns(received_ns, timeline.epoch_host_ns) + lead_ns as f64
        - last_frame as f64 * (1e9 / 48_000.0);
    age.is_finite().then_some(age.max(0.0) as u64)
}

struct Playout {
    paired: bool,
    timeline: Option<SessionInfo>,
    progress: PairingProgress,
    endpoint: String,
    renderer: Option<Renderer>,
    sync: ClockSynchronizer,
    error: Option<String>,
    packets: u64,
    rejected: u64,
    rendered: u64,
    concealed: u64,
    waiting: u64,
    realignments: u64,
    readiness: Option<ReceiverReadiness>,
    negotiated_lead_ns: Option<u64>,
    output_horizon_ns: u64,
    latest_source_frame: Option<u64>,
    input_peak: f32,
    output_peak: f32,
    last_concealed: usize,
    last_waiting: usize,
    last_frames: usize,
    late_frames: RecentSamples,
    timing: ReceiveTiming,
}
impl Default for Playout {
    fn default() -> Self {
        Self {
            paired: false,
            timeline: None,
            progress: PairingProgress::default(),
            endpoint: String::new(),
            renderer: None,
            sync: ClockSynchronizer::new(),
            error: None,
            packets: 0,
            rejected: 0,
            rendered: 0,
            concealed: 0,
            waiting: 0,
            realignments: 0,
            readiness: None,
            negotiated_lead_ns: None,
            output_horizon_ns: 0,
            latest_source_frame: None,
            input_peak: 0.0,
            output_peak: 0.0,
            last_concealed: 0,
            last_waiting: 0,
            last_frames: 0,
            late_frames: RecentSamples::default(),
            timing: ReceiveTiming::default(),
        }
    }
}
struct Receiver {
    stop: AtomicBool,
    state: Mutex<Playout>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl Receiver {
    fn start(
        peer: SocketAddr,
        local_port: u16,
        key: PairingKey,
        map: ChannelMap,
    ) -> Result<Arc<Self>, String> {
        if local_port == 0 {
            return Err("Receiver port must be 1–65535".into());
        }
        let local = SocketAddr::new(
            if peer.is_ipv4() {
                "0.0.0.0".parse().unwrap()
            } else {
                "::".parse().unwrap()
            },
            local_port,
        );
        let endpoint =
            Endpoint::bind(local, peer, key, Config::default()).map_err(|e| e.to_string())?;
        // UDP connect chooses a route without transmitting any datagrams. This
        // diagnostic does not replace the real receiver's fixed source port.
        let route_ip = UdpSocket::bind(SocketAddr::new(local.ip(), 0))
            .and_then(|s| {
                s.connect(peer)?;
                s.local_addr()
            })
            .ok()
            .map(|address| address.ip());
        let endpoint_text = format!(
            "Host: {peer}\nPhone UDP bind: {local}\nSelected route IP: {}",
            route_ip.map_or_else(|| "unavailable".into(), |ip| ip.to_string())
        );
        let receiver = Arc::new(Self {
            stop: AtomicBool::new(false),
            state: Mutex::new(Playout {
                endpoint: endpoint_text,
                ..Playout::default()
            }),
            worker: Mutex::new(None),
        });
        let cloned = receiver.clone();
        let worker = thread::Builder::new()
            .name("roomsync-network".into())
            .spawn(move || {
                if let Err(e) = cloned.run(endpoint, map) {
                    if let Ok(mut state) = cloned.state.lock() {
                        state.error = Some(e);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        *receiver.worker.lock().map_err(|_| "Worker lock failed")? = Some(worker);
        Ok(receiver)
    }
    fn run(&self, endpoint: Endpoint, map: ChannelMap) -> Result<(), String> {
        let clock = MonotonicClock::from_source(monotonic_ns);
        let mut session = endpoint
            .join_cancellable(clock, &self.stop, |progress| {
                if let Ok(mut state) = self.state.lock() {
                    state.progress = progress;
                }
            })
            .map_err(|e| e.to_string())?;
        {
            let mut state = self.state.lock().map_err(|_| "Playback state failed")?;
            state.paired = true;
        }
        let mut clock_due = Instant::now();
        let mut readiness_due = Instant::now();
        let mut poll_finished = Instant::now();
        let mut last_audio_ns = None;
        while !self.stop.load(Ordering::Relaxed) {
            if !session.readiness_acknowledged() && Instant::now() >= readiness_due {
                let readiness = self
                    .state
                    .lock()
                    .map_err(|_| "Playback state failed")?
                    .readiness;
                if let Some(value) = readiness {
                    session.report_readiness(value).map_err(|e| e.to_string())?;
                    readiness_due = Instant::now() + Duration::from_millis(250);
                }
            }
            if Instant::now() >= clock_due {
                match session.request_clock() {
                    Ok(()) | Err(roomsync_transport::Error::PendingClockLimit) => (),
                    Err(e) => return Err(e.to_string()),
                }
                clock_due = Instant::now() + Duration::from_millis(100);
            }
            let between_polls = elapsed_ns(poll_finished);
            let poll_started = Instant::now();
            let event = session
                .poll(Duration::from_millis(5))
                .map_err(|e| e.to_string())?;
            let poll_overrun = elapsed_ns(poll_started).saturating_sub(5_000_000);
            poll_finished = Instant::now();
            match event {
                Some(Event::Audio(packet)) => {
                    let received_ns = monotonic_ns().map_err(|e| e.to_string())?;
                    let lock_started = Instant::now();
                    let mut state = self.state.lock().map_err(|_| "Playback state failed")?;
                    state.timing.network_lock.record(elapsed_ns(lock_started));
                    if let Some(previous) = last_audio_ns {
                        state
                            .timing
                            .audio_gap
                            .record(received_ns.saturating_sub(previous));
                    }
                    last_audio_ns = Some(received_ns);
                    let last_frame = packet.first_frame()
                        + roomsync_core::protocol::FRAMES_PER_PACKET as u64
                        - 1;
                    state.latest_source_frame = Some(
                        state
                            .latest_source_frame
                            .map_or(last_frame, |old| old.max(last_frame)),
                    );
                    if let (Some(clock), Some(timeline), Some(lead)) =
                        (state.sync.model(), state.timeline, state.negotiated_lead_ns)
                    {
                        if let Some(age) = source_age_ns(
                            clock,
                            timeline,
                            lead,
                            last_frame,
                            monotonic_ns().map_err(|e| e.to_string())?,
                        ) {
                            state.timing.source_age.record(age);
                        }
                    }
                    state.input_peak = packet
                        .samples()
                        .iter()
                        .map(|v| f32::from(*v).abs() / 32768.0)
                        .fold(0.0, f32::max);
                    if let Some(renderer) = state.renderer.as_mut() {
                        // Every insertion is counted by renderer.stats(), including
                        // late/too-far PCM that passed transport authentication.
                        let cursor = renderer.source_cursor();
                        if renderer.push(packet) == InsertResult::Late {
                            if let Some(cursor) = cursor {
                                state
                                    .late_frames
                                    .record((cursor - last_frame as f64).max(0.0).ceil() as u64);
                            }
                        }
                    }
                    state.packets += 1;
                }
                Some(Event::Timeline(info)) => {
                    let mut state = self.state.lock().map_err(|_| "Playback state failed")?;
                    if state.timeline != Some(info) {
                        state.renderer = Some(
                            Renderer::new(info.session_id, info.epoch_host_ns, map)
                                .map_err(|e| format!("Renderer: {e:?}"))?,
                        );
                        state.timeline = Some(info);
                        state.negotiated_lead_ns = session.negotiated_lead_ns();
                    }
                }
                Some(Event::ClockSample(sample)) => {
                    if let Ok(mut state) = self.state.lock() {
                        if matches!(
                            state.sync.observe(sample),
                            Err(roomsync_core::clock::ClockError::Discontinuity)
                        ) {
                            return Err("Host clock discontinuity. Restart the host and reconnect to recalibrate.".into());
                        }
                    }
                }
                Some(Event::Closed) => return Err("Host ended the session".into()),
                _ => (),
            }
            if let Ok(mut state) = self.state.lock() {
                let stats = session.stats();
                state.timing.poll_overrun.record(poll_overrun);
                state.timing.between_polls.record(between_polls);
                state.rejected = stats.authentication_rejected
                    + stats.replay_rejected
                    + stats.malformed_rejected
                    + stats.source_rejected
                    + stats.wrong_session_rejected;
            }
        }
        let _ = session.close();
        Ok(())
    }
    fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    fn finish(&self) {
        self.cancel();
        if let Ok(mut handle) = self.worker.lock() {
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
    }
    fn report_readiness(&self, value: ReceiverReadiness) -> Result<(), String> {
        if value.output_horizon_ns > 1_000_000_000
            || !(1..=100_000_000).contains(&value.render_block_ns)
        {
            return Err("Output calibration is outside supported bounds".into());
        }
        let mut state = self.state.lock().map_err(|_| "Playback state failed")?;
        if !state.paired || state.timeline.is_some() {
            return Err(
                "Output calibration must finish after pairing and before playback. Reconnect."
                    .into(),
            );
        }
        if state.readiness.is_some_and(|old| old != value) {
            return Err("Output calibration changed. Reconnect to renegotiate playback.".into());
        }
        state.readiness = Some(value);
        Ok(())
    }
    fn render(&self, dac_ns: u64, out: &mut [[f32; 2]]) -> Result<(), String> {
        out.fill([0.0; 2]);
        if out.len() > MAX_RENDER_FRAMES {
            return Err("Render request exceeds bounded 960 frames".into());
        }
        if self.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let lock_started = Instant::now();
        let mut state = self.state.lock().map_err(|_| "Playback state failed")?;
        state.timing.render_lock.record(elapsed_ns(lock_started));
        let render_started = Instant::now();
        if let Some(e) = &state.error {
            return Err(e.clone());
        }
        state.output_horizon_ns = dac_ns.saturating_sub(monotonic_ns().map_err(|e| e.to_string())?);
        state.output_peak = 0.0;
        state.last_frames = out.len();
        state.last_concealed = 0;
        state.last_waiting = out.len();
        let Some(model) = state.sync.model() else {
            state.waiting += out.len() as u64;
            return Ok(());
        };
        let Some(renderer) = state.renderer.as_mut() else {
            return Ok(());
        };
        let report = renderer
            .render(dac_ns, 48_000, model, out)
            .map_err(|e| format!("Renderer: {e:?}"))?;
        state.output_peak = out.iter().flatten().map(|v| v.abs()).fold(0.0, f32::max);
        state.last_concealed = report.concealed_frames;
        state.last_waiting = report.waiting_frames;
        state.rendered += report.frames as u64;
        state.concealed += report.concealed_frames as u64;
        state.waiting += report.waiting_frames as u64;
        state.realignments += u64::from(report.realigned);
        state.timing.render_work.record(elapsed_ns(render_started));
        Ok(())
    }
    fn phase(&self) -> i32 {
        let Ok(state) = self.state.lock() else {
            return -1;
        };
        if state.error.is_some() {
            return -1;
        }
        if self.stop.load(Ordering::Relaxed) {
            return 0;
        }
        if state.paired && state.renderer.is_none() {
            return 4;
        }
        if state.renderer.is_some()
            && state
                .sync
                .model()
                .is_some_and(|m| m.is_locked() && monotonic_ns().is_ok_and(|n| m.is_fresh_at(n)))
        {
            3
        } else if state.renderer.is_some() {
            2
        } else {
            1
        }
    }
    fn status(&self) -> String {
        let Ok(s) = self.state.lock() else {
            return "Playback state failed".into();
        };
        if let Some(e) = &s.error {
            return format!("Stopped: {e}\n{}", s.endpoint);
        }
        let clock = match s.sync.model() {
            Some(m) if m.is_locked() && monotonic_ns().is_ok_and(|n| m.is_fresh_at(n)) => format!(
                "clock estimate {:.0} ppm; network uncertainty ±{:.2} ms",
                m.drift_ppm(),
                m.uncertainty_ns() / 1e6
            ),
            _ => "measuring host clock".into(),
        };
        if !s.paired {
            return format!(
                "{}\n{}\nWaiting: {} s · Attempts: {}\nHello sent: {} · Verified replies: {} · Confirmations sent: {}\nUDP received: {} · Wrong source: {} · Authentication rejected: {}\nProtocol mismatch: {} · Malformed: {} · Peer unavailable: {}\nKeep Windows RoomSync open. Stop cancels immediately.",
                s.progress.description(),
                s.endpoint,
                s.progress.elapsed.as_secs(),
                s.progress.attempts,
                s.progress.hello_sent,
                s.progress.welcomes_authenticated,
                s.progress.confirmations_sent,
                s.progress.datagrams_received,
                s.progress.wrong_source,
                s.progress.authentication_rejected,
                s.progress.protocol_mismatch,
                s.progress.malformed,
                s.progress.peer_unreachable
            );
        }
        let health = if s.renderer.is_none() {
            if s.readiness.is_some() {
                "Paired securely. Output calibrated; waiting for Windows and the other receivers."
            } else {
                "Paired securely. Measuring output latency before playback."
            }
        } else if s.last_frames == 0 || s.last_waiting == s.last_frames {
            "Encrypted session ready. Waiting for the synchronized start or fresh clock lock."
        } else if s.last_concealed + s.last_waiting == s.last_frames {
            "No playable samples for this output deadline. Receiving silence from packet concealment."
        } else if s.output_peak == 0.0 {
            "Received audio for this deadline is silent. Check the Windows source playback."
        } else {
            "Nonzero audio rendered against measured timestamps."
        };
        let buffer = s.renderer.as_ref().map_or_else(|| "Jitter buffer: waiting for timeline".into(), |r| {
            let stats = r.stats();
            format!("Jitter accepted: {} · Late: {} · Too far: {} · Duplicates: {}\nNext source frame: {} · Newest received: {}",
                stats.accepted, stats.late, stats.too_far, stats.duplicates,
                r.source_cursor().map_or_else(|| "not started".into(), |v| format!("{v:.0}")),
                s.latest_source_frame.map_or_else(|| "none".into(), |v| v.to_string()))
        });
        let text = format!(
            "{}\n{}\nPackets: {} · Transport rejected: {}\n{}\nLate-packet lag: p95 {} frames (last {}), max {} frames\nConcealed: {} frames · Realignments: {}\nLatest block missing: {}/{} · Waiting: {}\nPCM peak received: {:.4} · Rendered: {:.4}\nNext-write DAC horizon: {:.1} ms · Calibrated max: {} · Shared lead: {}\n{}",
            health,
            s.endpoint,
            s.packets,
            s.rejected,
            buffer,
            s.late_frames.p95(),
            s.late_frames.count,
            s.late_frames.maximum,
            s.concealed,
            s.realignments,
            s.last_concealed,
            s.last_frames,
            s.last_waiting,
            s.input_peak,
            s.output_peak,
            s.output_horizon_ns as f64 / 1e6,
            s.readiness.map_or_else(
                || "measuring".into(),
                |v| format!("{:.1} ms", v.output_horizon_ns as f64 / 1e6)
            ),
            s.negotiated_lead_ns
                .map_or_else(|| "waiting".into(), |v| format!("{:.1} ms", v as f64 / 1e6)),
            clock
        );
        // Snapshot bounded numbers before sorting/formatting the extra timing
        // diagnostics. Copy-details must not add long work to the render mutex.
        let timing = s.timing.clone();
        drop(s);
        format!("{text}\n{}", timing.describe())
    }
}

struct Active {
    id: u64,
    receiver: Arc<Receiver>,
}
static ACTIVE: OnceLock<Mutex<Option<Active>>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);
fn active() -> &'static Mutex<Option<Active>> {
    ACTIVE.get_or_init(|| Mutex::new(None))
}
fn find(id: jlong) -> Option<Arc<Receiver>> {
    active()
        .lock()
        .ok()?
        .as_ref()
        .filter(|v| v.id == id as u64)
        .map(|v| v.receiver.clone())
}
fn exception(env: &mut JNIEnv, message: impl AsRef<str>) {
    let _ = env.throw_new("java/lang/IllegalStateException", message.as_ref());
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_roomsync_NativeReceiver_start(
    mut env: JNIEnv,
    _: JClass,
    host: JString,
    host_port: jint,
    local_port: jint,
    chars: JCharArray,
    channel: jint,
) -> jlong {
    let mut start = || -> Result<jlong, String> {
        let host: String = env.get_string(&host).map_err(|e| e.to_string())?.into();
        let ip: IpAddr = host
            .trim()
            .parse()
            .map_err(|_| "Use a numeric host IPv4 or IPv6 address")?;
        if !(1..=65535).contains(&host_port) || !(1..=65535).contains(&local_port) {
            return Err("Ports must be 1–65535".into());
        }
        let len = env.get_array_length(&chars).map_err(|e| e.to_string())?;
        if len != 64 {
            return Err("Pairing key must contain exactly 64 hexadecimal characters".into());
        }
        let mut raw = Zeroizing::new(vec![0u16; 64]);
        env.get_char_array_region(&chars, 0, &mut raw)
            .map_err(|e| e.to_string())?;
        let secret = Zeroizing::new(String::from_utf16(&raw).map_err(|_| "Invalid pairing key")?);
        let key = PairingKey::from_hex(&secret).map_err(|e| e.to_string())?;
        let map = match channel {
            0 => ChannelMap::Stereo,
            1 => ChannelMap::Left,
            2 => ChannelMap::Right,
            3 => ChannelMap::Mono,
            _ => return Err("Invalid channel selection".into()),
        };
        let mut slot = active().lock().map_err(|_| "Receiver lock failed")?;
        if slot.is_some() {
            return Err("Previous receiver is still stopping; try again shortly".into());
        }
        let receiver = Receiver::start(
            SocketAddr::new(ip, host_port as u16),
            local_port as u16,
            key,
            map,
        )?;
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        *slot = Some(Active { id, receiver });
        Ok(id as jlong)
    };
    match start() {
        Ok(id) => id,
        Err(e) => {
            exception(&mut env, e);
            0
        }
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_roomsync_NativeReceiver_phase(
    _: JNIEnv,
    _: JClass,
    id: jlong,
) -> jint {
    find(id).map_or(0, |r| r.phase())
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_roomsync_NativeReceiver_status(
    mut env: JNIEnv,
    _: JClass,
    id: jlong,
) -> jstring {
    let text = find(id).map_or_else(|| "Stopped".into(), |r| r.status());
    match env.new_string(text) {
        Ok(s) => s.into_raw(),
        Err(e) => {
            exception(&mut env, e.to_string());
            std::ptr::null_mut()
        }
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_roomsync_NativeReceiver_reportReadiness(
    mut env: JNIEnv,
    _: JClass,
    id: jlong,
    horizon: jlong,
    block: jlong,
) {
    let result = if horizon < 0 || block <= 0 {
        Err("Invalid output calibration".into())
    } else if let Some(receiver) = find(id) {
        receiver.report_readiness(ReceiverReadiness {
            output_horizon_ns: horizon as u64,
            render_block_ns: block as u64,
        })
    } else {
        Err("Receiver has stopped".into())
    };
    if let Err(e) = result {
        exception(&mut env, e);
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_roomsync_NativeReceiver_render(
    mut env: JNIEnv,
    _: JClass,
    id: jlong,
    dac_ns: jlong,
    frames: jint,
    target: JFloatArray,
) {
    let work = || -> Result<(), String> {
        if dac_ns < 0 || !(1..=MAX_RENDER_FRAMES as i32).contains(&frames) {
            return Err("Invalid timestamp or frame count".into());
        }
        if env.get_array_length(&target).map_err(|e| e.to_string())? < frames * 2 {
            return Err("Audio output array too small".into());
        }
        let receiver = find(id).ok_or("Receiver has stopped")?;
        let mut out = [[0.0f32; 2]; MAX_RENDER_FRAMES];
        receiver.render(dac_ns as u64, &mut out[..frames as usize])?;
        let mut flat = [0.0; MAX_RENDER_FRAMES * 2];
        for (o, v) in flat
            .chunks_exact_mut(2)
            .zip(out.iter())
            .take(frames as usize)
        {
            o.copy_from_slice(v);
        }
        env.set_float_array_region(&target, 0, &flat[..frames as usize * 2])
            .map_err(|e| e.to_string())
    };
    if let Err(e) = work() {
        exception(&mut env, e);
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_roomsync_NativeReceiver_cancel(_: JNIEnv, _: JClass, id: jlong) {
    if let Some(r) = find(id) {
        r.cancel();
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_roomsync_NativeReceiver_stop(_: JNIEnv, _: JClass, id: jlong) {
    if let Some(r) = find(id) {
        r.finish();
    }
    if let Ok(mut slot) = active().lock() {
        if slot.as_ref().is_some_and(|v| v.id == id as u64) {
            *slot = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monotonic_clock_is_system_domain_and_advances() {
        let a = monotonic_ns().unwrap();
        thread::sleep(Duration::from_millis(1));
        assert!(monotonic_ns().unwrap() > a);
    }
    #[test]
    fn waiting_outputs_silence_and_request_is_bounded() {
        let r = Receiver {
            stop: AtomicBool::new(false),
            state: Mutex::new(Playout::default()),
            worker: Mutex::new(None),
        };
        let mut out = [[1.0; 2]; 120];
        r.render(monotonic_ns().unwrap(), &mut out).unwrap();
        assert!(out.iter().all(|s| *s == [0.0; 2]));
        assert!(r.render(0, &mut [[0.0; 2]; 961]).is_err());
    }
    #[test]
    fn late_frame_diagnostics_are_bounded_and_explicit() {
        let mut samples = RecentSamples::default();
        assert_eq!(samples.p95(), 0);
        for n in 1..=100 {
            samples.record(n);
        }
        assert_eq!(samples.p95(), 95);
        assert_eq!(samples.maximum, 100);
        for _ in 0..300 {
            samples.record(20);
        }
        assert_eq!(samples.count, 256);
        assert_eq!(samples.p95(), 20);
        assert_eq!(samples.maximum, 100);
    }
    #[test]
    fn estimated_source_age_uses_common_schedule_not_dac_queue() {
        use roomsync_core::clock::{ClockExchange, ClockModel};
        let timeline = SessionInfo {
            session_id: 1,
            epoch_host_ns: 10_000_000_000,
        };
        // Frame 4800 was scheduled at 10s - 130ms + 100ms = 9.970s.
        assert_eq!(
            source_age_ns(
                ClockModel::identity(),
                timeline,
                130_000_000,
                4_800,
                10_025_000_000
            ),
            Some(55_000_000)
        );
        // A stalled receiver, host, or network all grow this age. It cannot
        // identify any of them alone and does not incorporate a DAC horizon.
        assert_eq!(
            source_age_ns(
                ClockModel::identity(),
                timeline,
                130_000_000,
                4_800,
                10_145_000_000
            ),
            Some(175_000_000)
        );
        let mut sync = ClockSynchronizer::new();
        for i in 0..16 {
            let t = 1_000_000_000 + i * 100_000_000;
            sync.observe(ClockExchange {
                local_send_ns: t,
                host_receive_ns: t + 500_000,
                host_send_ns: t + 600_000,
                local_receive_ns: t + 1_100_000,
            })
            .unwrap();
        }
        assert!(
            source_age_ns(
                sync.model().unwrap(),
                timeline,
                130_000_000,
                4_800,
                10_145_000_000
            )
            .is_none(),
            "Expired clock cannot measure source age"
        );
    }
    #[test]
    fn diagnostic_histograms_do_not_mix_poll_and_mutex_delays() {
        let mut timing = ReceiveTiming::default();
        assert!(
            timing
                .describe()
                .contains("Estimated source age: not measured")
        );
        timing.poll_overrun.record(120_000_000);
        timing.network_lock.record(5_000);
        timing.audio_gap.record(150_000_000);
        let snapshot = timing.clone();
        for _ in 0..300 {
            timing.poll_overrun.record(0);
        }
        assert_eq!(timing.poll_overrun.p95(), 0);
        assert_eq!(timing.poll_overrun.maximum, 120_000_000);
        assert_eq!(snapshot.poll_overrun.p95(), 120_000_000);
        assert_eq!(snapshot.network_lock.maximum, 5_000);
        assert_eq!(snapshot.render_lock.maximum, 0);
        assert!(
            snapshot
                .describe()
                .contains("inferred from the shared schedule")
        );
    }
    #[test]
    fn readiness_is_measured_once_before_timeline_and_bounded() {
        let receiver = Receiver {
            stop: AtomicBool::new(false),
            state: Mutex::new(Playout::default()),
            worker: Mutex::new(None),
        };
        let value = ReceiverReadiness {
            output_horizon_ns: 150_000_000,
            render_block_ns: 10_000_000,
        };
        assert!(receiver.report_readiness(value).is_err());
        receiver.state.lock().unwrap().paired = true;
        assert!(
            receiver
                .report_readiness(ReceiverReadiness {
                    output_horizon_ns: u64::MAX,
                    ..value
                })
                .is_err()
        );
        assert!(
            receiver
                .report_readiness(ReceiverReadiness {
                    render_block_ns: 0,
                    ..value
                })
                .is_err()
        );
        receiver.report_readiness(value).unwrap();
        receiver.report_readiness(value).unwrap();
        assert!(
            receiver
                .report_readiness(ReceiverReadiness {
                    output_horizon_ns: 200_000_000,
                    ..value
                })
                .is_err()
        );
        receiver.state.lock().unwrap().timeline = Some(SessionInfo {
            session_id: 1,
            epoch_host_ns: 1,
        });
        assert!(receiver.report_readiness(value).is_err());
    }
    #[test]
    fn invalid_handle_and_port_are_rejected() {
        assert!(find(-1).is_none());
        assert!(
            Receiver::start(
                "127.0.0.1:10000".parse().unwrap(),
                0,
                PairingKey::generate().unwrap(),
                ChannelMap::Stereo
            )
            .is_err()
        );
    }
    #[test]
    fn encrypted_loopback_clocks_and_renders_real_pcm() {
        use std::net::UdpSocket;
        let host_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let host_addr = host_socket.local_addr().unwrap();
        drop(host_socket);
        let port_reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let receiver_addr = port_reservation.local_addr().unwrap();
        drop(port_reservation);
        let key = PairingKey::generate().unwrap();
        let key_copy = PairingKey::from_hex(&key.to_hex_for_storage()).unwrap();
        let host = Endpoint::bind_listener(host_addr, key, Config::default()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let sender = thread::spawn(move || {
            let mut session = host
                .accept_waiting(
                    MonotonicClock::from_source(monotonic_ns),
                    &AtomicBool::new(false),
                    |_| {},
                )
                .unwrap();
            // A second phone may take a while. Stay alive without any playback
            // epoch, even beyond the normal authenticated idle timeout.
            let wait_until = Instant::now() + Duration::from_millis(3_500);
            let mut heartbeat = Instant::now();
            while Instant::now() < wait_until {
                session.poll(Duration::from_millis(5)).unwrap();
                if heartbeat.elapsed() >= Duration::from_millis(500) {
                    session.heartbeat().unwrap();
                    heartbeat = Instant::now();
                }
            }
            let info = SessionInfo {
                session_id: session.info().session_id,
                epoch_host_ns: monotonic_ns().unwrap() + 2_000_000_000,
            };
            session.set_timeline_with_lead(info, 100_000_000).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                assert!(Instant::now() < deadline, "timeline not acknowledged");
                if matches!(
                    session.poll(Duration::from_millis(10)).unwrap(),
                    Some(Event::TimelineAcknowledged(_))
                ) {
                    break;
                }
            }
            tx.send(info.epoch_host_ns).unwrap();
            for sequence in 0..10 {
                session
                    .send_audio(
                        &roomsync_core::AudioPacket::new(
                            info.session_id,
                            sequence,
                            [10_000; roomsync_core::protocol::SAMPLES_PER_PACKET],
                        )
                        .unwrap(),
                    )
                    .unwrap();
            }
            let until = Instant::now() + Duration::from_secs(4);
            while Instant::now() < until {
                if matches!(
                    session.poll(Duration::from_millis(5)),
                    Ok(Some(Event::Closed)) | Err(_)
                ) {
                    break;
                }
            }
        });
        let receiver = Receiver::start(
            host_addr,
            receiver_addr.port(),
            key_copy,
            ChannelMap::Stereo,
        )
        .unwrap();
        let paired_deadline = Instant::now() + Duration::from_secs(2);
        while receiver.phase() == 1 && Instant::now() < paired_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            receiver.phase(),
            4,
            "paired receiver must wait for a real timeline"
        );
        receiver
            .report_readiness(ReceiverReadiness {
                output_horizon_ns: 40_000_000,
                render_block_ns: 10_000_000,
            })
            .unwrap();
        let epoch = rx.recv_timeout(Duration::from_secs(6)).unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        while (receiver.phase() != 3 || receiver.state.lock().unwrap().packets < 10)
            && Instant::now() < until
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(receiver.phase(), 3, "{}", receiver.status());
        let mut out = [[0.0; 2]; 120];
        receiver.render(epoch + 2_500_000, &mut out).unwrap();
        assert!(
            out.iter().any(|frame| frame[0] > 0.2),
            "Expected authenticated decoded PCM, got {}",
            receiver.status()
        );
        receiver.finish();
        sender.join().unwrap();
    }
    #[test]
    fn waits_beyond_old_deadline_and_cancel_releases_port() {
        let host_reserve = UdpSocket::bind("127.0.0.1:0").unwrap();
        let host = host_reserve.local_addr().unwrap();
        let reserve = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = reserve.local_addr().unwrap().port();
        drop(reserve);
        let receiver = Receiver::start(
            host,
            port,
            PairingKey::generate().unwrap(),
            ChannelMap::Stereo,
        )
        .unwrap();
        thread::sleep(Duration::from_millis(5_300));
        assert_eq!(receiver.phase(), 1, "{}", receiver.status());
        let now = Instant::now();
        receiver.finish();
        assert!(now.elapsed() < Duration::from_secs(2));
        let restarted = Receiver::start(
            host,
            port,
            PairingKey::generate().unwrap(),
            ChannelMap::Stereo,
        )
        .unwrap();
        restarted.finish();
    }
}
