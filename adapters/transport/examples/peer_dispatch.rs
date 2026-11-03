//! Localhost diagnostic for the host's independent peer-worker scheduling policy.
//! It uses real authenticated Session I/O, bounded queues and four-packet/10ms
//! source batches. Optional argument injects a worker stall in peer2 (milliseconds).
//! This measures this machine only; it does not simulate Wi-Fi or audio hardware.
//! Queues use bounded std channels rather than the desktop's ArrayQueue, so this
//! is a scheduling-policy probe, not a benchmark of the desktop queue itself.
use roomsync_core::{AudioPacket, protocol::SAMPLES_PER_PACKET};
use roomsync_transport::{
    Config, Endpoint, Event, MonotonicClock, PairingKey, Session, SessionInfo,
};
use std::{
    error::Error,
    net::UdpSocket,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const PACKETS: u64 = 800;
const BATCH: u64 = 4;
const QUEUE_CAPACITY: usize = 128;
const DRAIN_LIMIT: usize = 64;

fn pair(clock: MonotonicClock, info: SessionInfo) -> Result<(Session, Session)> {
    let host = UdpSocket::bind("127.0.0.1:0")?;
    let receiver = UdpSocket::bind("127.0.0.1:0")?;
    let host_addr = host.local_addr()?;
    let receiver_addr = receiver.local_addr()?;
    let key = PairingKey::generate()?;
    let receiver_key = PairingKey::from_hex(&key.to_hex_for_storage())?;
    let config = Config {
        idle_timeout: Duration::from_secs(10),
        ..Config::default()
    };
    let receiver = thread::spawn(move || {
        Endpoint::from_socket(receiver, host_addr, receiver_key, config)?
            .join(MonotonicClock::default())
    });
    let mut host =
        Endpoint::from_socket(host, receiver_addr, key, config)?.accept_with_info(clock, info)?;
    // An admitted host still services READY retries until its peer confirms.
    while !receiver.is_finished() {
        host.poll(Duration::from_millis(2))?;
    }
    Ok((
        host,
        receiver
            .join()
            .map_err(|_| "receiver pairing thread failed")??,
    ))
}
#[derive(Default)]
struct SendStats {
    sent: u64,
    max_batch: usize,
    queue_ns: Vec<u64>,
    send_ns: Vec<u64>,
    max_worker_gap_ns: u64,
}
#[derive(Default)]
struct ReceiveStats {
    received: u64,
    last_sequence: Option<u64>,
    gaps_ns: Vec<u64>,
}
fn ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}
fn percentile(values: &[u64], percent: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[(sorted.len() - 1) * percent / 100]
}
fn run_sender(
    mut session: Session,
    queue: mpsc::Receiver<(AudioPacket, Instant)>,
    stall: Duration,
) -> Result<SendStats> {
    let mut stats = SendStats::default();
    let mut last_loop = Instant::now();
    let mut injected = false;
    while stats.sent < PACKETS {
        let now = Instant::now();
        stats.max_worker_gap_ns = stats
            .max_worker_gap_ns
            .max(ns(now.duration_since(last_loop)));
        last_loop = now;
        let mut batch = 0;
        for _ in 0..DRAIN_LIMIT {
            let Ok((packet, enqueued)) = queue.try_recv() else {
                break;
            };
            if !injected && packet.sequence() >= 100 && !stall.is_zero() {
                thread::sleep(stall);
                injected = true;
            }
            stats.queue_ns.push(ns(enqueued.elapsed()));
            let start = Instant::now();
            session.send_audio(&packet)?;
            stats.send_ns.push(ns(start.elapsed()));
            stats.sent += 1;
            batch += 1;
        }
        stats.max_batch = stats.max_batch.max(batch);
        session.poll(Duration::from_millis(2))?;
    }
    let end = Instant::now() + Duration::from_millis(30);
    while Instant::now() < end {
        session.poll(Duration::from_millis(2))?;
    }
    session.close()?;
    Ok(stats)
}
fn run_receiver(mut session: Session) -> Result<ReceiveStats> {
    let mut stats = ReceiveStats::default();
    let mut last_audio = None::<Instant>;
    let mut heartbeat = Instant::now();
    loop {
        if heartbeat.elapsed() >= Duration::from_millis(200) {
            session.heartbeat()?;
            heartbeat = Instant::now();
        }
        match session.poll(Duration::from_millis(5))? {
            Some(Event::Audio(packet)) => {
                if stats
                    .last_sequence
                    .is_some_and(|last| packet.sequence() <= last)
                {
                    return Err("unexpected sequence order on localhost".into());
                }
                let now = Instant::now();
                if let Some(last) = last_audio {
                    stats.gaps_ns.push(ns(now.duration_since(last)));
                }
                last_audio = Some(now);
                stats.last_sequence = Some(packet.sequence());
                stats.received += 1;
            }
            Some(Event::Closed) => break,
            _ => {}
        }
    }
    Ok(stats)
}
fn main() -> Result<()> {
    let stall_ms = std::env::args()
        .nth(1)
        .map(|s| s.parse::<u64>())
        .transpose()?
        .unwrap_or(0);
    if stall_ms > 200 {
        return Err("injected peer2 stall must be 0..=200ms".into());
    }
    let clock = MonotonicClock::default();
    let info = SessionInfo {
        session_id: 1,
        epoch_host_ns: clock.now_ns()? + 5_000_000_000,
    };
    let mut producers = Vec::new();
    let mut senders = Vec::new();
    let mut receivers = Vec::new();
    for index in 0..2 {
        let (host, receiver) = pair(clock.clone(), info)?;
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        producers.push(tx);
        let stall = if index == 1 {
            Duration::from_millis(stall_ms)
        } else {
            Duration::ZERO
        };
        senders.push(thread::spawn(move || run_sender(host, rx, stall)));
        receivers.push(thread::spawn(move || run_receiver(receiver)));
    }
    let started = Instant::now();
    for batch in 0..PACKETS / BATCH {
        let due = started + Duration::from_millis(batch * 10);
        if due > Instant::now() {
            thread::sleep(due.saturating_duration_since(Instant::now()));
        }
        for offset in 0..BATCH {
            let sequence = batch * BATCH + offset;
            let packet = AudioPacket::new(
                info.session_id,
                sequence,
                [sequence as i16; SAMPLES_PER_PACKET],
            )?;
            for queue in &producers {
                queue.try_send((packet.clone(), Instant::now()))?;
            }
        }
    }
    for (index, (sender, receiver)) in senders.into_iter().zip(receivers).enumerate() {
        let sent = sender.join().map_err(|_| "sender thread failed")??;
        let received = receiver.join().map_err(|_| "receiver thread failed")??;
        println!(
            "peer={} injected_stall_ms={} sent={} received={} max_batch={} queue_p95_ms={:.3} queue_max_ms={:.3} send_p95_us={:.3} send_max_us={:.3} max_worker_gap_ms={:.3} receive_gap_p95_ms={:.3} receive_gap_max_ms={:.3}",
            index + 1,
            if index == 1 { stall_ms } else { 0 },
            sent.sent,
            received.received,
            sent.max_batch,
            percentile(&sent.queue_ns, 95) as f64 / 1e6,
            percentile(&sent.queue_ns, 100) as f64 / 1e6,
            percentile(&sent.send_ns, 95) as f64 / 1e3,
            percentile(&sent.send_ns, 100) as f64 / 1e3,
            sent.max_worker_gap_ns as f64 / 1e6,
            percentile(&received.gaps_ns, 95) as f64 / 1e6,
            percentile(&received.gaps_ns, 100) as f64 / 1e6
        );
        if received.received != PACKETS {
            return Err(
                "localhost delivery was incomplete; inspect scheduling and socket limits".into(),
            );
        }
    }
    Ok(())
}
