//! Android JNI boundary. Socket I/O has its own thread. AudioTrack's writer owns
//! rendering; no network operation runs on Android's audio or UI thread.
use jni::{
    JNIEnv,
    objects::{JCharArray, JClass, JFloatArray, JString},
    sys::{jint, jlong, jstring},
};
use roomsync_core::{
    clock::ClockSynchronizer,
    render::{ChannelMap, Renderer},
};
use roomsync_transport::{
    Config, Endpoint, Event, MonotonicClock, PairingKey, PairingProgress, SessionInfo,
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
        while !self.stop.load(Ordering::Relaxed) {
            if Instant::now() >= clock_due {
                match session.request_clock() {
                    Ok(()) | Err(roomsync_transport::Error::PendingClockLimit) => (),
                    Err(e) => return Err(e.to_string()),
                }
                clock_due = Instant::now() + Duration::from_millis(100);
            }
            match session
                .poll(Duration::from_millis(5))
                .map_err(|e| e.to_string())?
            {
                Some(Event::Audio(packet)) => {
                    let mut state = self.state.lock().map_err(|_| "Playback state failed")?;
                    if let Some(renderer) = state.renderer.as_mut() {
                        renderer.push(packet);
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
    fn render(&self, dac_ns: u64, out: &mut [[f32; 2]]) -> Result<(), String> {
        out.fill([0.0; 2]);
        if out.len() > MAX_RENDER_FRAMES {
            return Err("Render request exceeds bounded 960 frames".into());
        }
        if self.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut state = self.state.lock().map_err(|_| "Playback state failed")?;
        if let Some(e) = &state.error {
            return Err(e.clone());
        }
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
        state.rendered += report.frames as u64;
        state.concealed += report.concealed_frames as u64;
        state.waiting += report.waiting_frames as u64;
        state.realignments += u64::from(report.realigned);
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
        format!(
            "{}\n{}\nPackets: {} · Rejected: {}\nConcealed: {} frames · Realignments: {}\n{}",
            if s.renderer.is_some() {
                "Encrypted session established"
            } else {
                "Paired securely. Waiting for the Windows playback timeline."
            },
            s.endpoint,
            s.packets,
            s.rejected,
            s.concealed,
            s.realignments,
            clock
        )
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
            session.set_timeline(info).unwrap();
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
