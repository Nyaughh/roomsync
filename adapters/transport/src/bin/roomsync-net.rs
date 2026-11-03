//! Raw PCM network smoke-test utility, not an audio device player.
use roomsync_core::{AudioPacket, clock::ClockSynchronizer, protocol::SAMPLES_PER_PACKET};
use roomsync_transport::{Config, Endpoint, Event, MonotonicClock, PairingKey, Session};
use std::{
    error::Error,
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    net::SocketAddr,
    path::Path,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const PCM_BYTES: usize = SAMPLES_PER_PACKET * 2;
const MAX_PACKETS: u64 = 240_000; // Ten minutes; bounds output including sparse seeks.
const PACKET_NS: u64 = 2_500_000;
const SEND_AHEAD_NS: u64 = 50_000_000;

fn main() {
    if let Err(error) = run() {
        eprintln!("roomsync-net: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "generate-key" {
        let key = PairingKey::generate()?;
        let mut file = create_private(&args[1])?;
        file.write_all(key.to_hex_for_storage().as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        println!(
            "Created pairing-key file. Transfer it privately to the designated peer; never commit it."
        );
        return Ok(());
    }
    if args.len() != 5 || !matches!(args[0].as_str(), "send" | "receive") {
        return Err("usage:\n  roomsync-net generate-key PRIVATE_KEY_FILE\n  roomsync-net send LOCAL_IP:PORT PEER_IP:PORT PRIVATE_KEY_FILE INPUT.pcm\n  roomsync-net receive LOCAL_IP:PORT PEER_IP:PORT PRIVATE_KEY_FILE OUTPUT.pcm\nPCM format: 48 kHz stereo signed 16-bit little-endian; input must contain whole 120-frame packets, maximum 10 minutes. OUTPUT must not exist. This utility records packets, it does not play an audio device.".into());
    }
    let local: SocketAddr = args[1].parse()?;
    let peer: SocketAddr = args[2].parse()?;
    let key = read_key(&args[3])?;
    let endpoint = Endpoint::bind(local, peer, key, Config::default())?;
    let clock = MonotonicClock::default();
    if args[0] == "send" {
        send(endpoint, clock, &args[4])
    } else {
        receive(endpoint, clock, &args[4])
    }
}
fn create_private(path: impl AsRef<Path>) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn read_key(path: &str) -> Result<PairingKey> {
    let file = File::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            return Err(
                "key file must not be readable by other users; set its permissions to 600".into(),
            );
        }
    }
    let mut text = Zeroizing::new(String::new());
    file.take(66).read_to_string(&mut text)?;
    Ok(PairingKey::from_hex(&text)?)
}
fn send(endpoint: Endpoint, clock: MonotonicClock, path: &str) -> Result<()> {
    let input = File::open(path)?;
    let len = input.metadata()?.len();
    if len == 0 || len % PCM_BYTES as u64 != 0 || len / PCM_BYTES as u64 > MAX_PACKETS {
        return Err("input must contain 1..240000 complete 480-byte PCM packets".into());
    }
    let mut input = BufReader::new(input);
    println!("Waiting for the configured receiver at the authenticated endpoint...");
    let mut session = endpoint.accept(clock, Duration::from_secs(2))?;
    let packets = len / PCM_BYTES as u64;
    println!("Paired. Sending {packets} PCM packets with a two-second clock warmup.");
    for sequence in 0..packets {
        let send_at =
            session.info().epoch_host_ns.saturating_sub(SEND_AHEAD_NS) + sequence * PACKET_NS;
        wait_until(&mut session, send_at)?;
        let mut bytes = [0; PCM_BYTES];
        input.read_exact(&mut bytes)?;
        let mut samples = [0; SAMPLES_PER_PACKET];
        for (sample, pair) in samples.iter_mut().zip(bytes.chunks_exact(2)) {
            *sample = i16::from_le_bytes([pair[0], pair[1]]);
        }
        session.send_audio(&AudioPacket::new(
            session.info().session_id,
            sequence,
            samples,
        )?)?;
        session.poll(Duration::ZERO)?;
    }
    // Leave time for any late audio datagrams before the best-effort close marker.
    let drain = Instant::now() + Duration::from_millis(100);
    while Instant::now() < drain {
        session.poll(Duration::from_millis(5))?;
    }
    session.finish_audio(packets)?;
    println!("Sent {packets} packets; this confirms transmission, not DAC alignment or receipt.");
    Ok(())
}
fn wait_until(session: &mut Session, target_ns: u64) -> Result<()> {
    loop {
        let now = session.clock().now_ns()?;
        if now >= target_ns {
            return Ok(());
        }
        if matches!(
            session.poll(Duration::from_nanos(target_ns - now).min(Duration::from_millis(5)))?,
            Some(Event::Closed)
        ) {
            return Err("receiver closed the session".into());
        }
    }
}
fn receive(endpoint: Endpoint, clock: MonotonicClock, path: &str) -> Result<()> {
    // Reserve the chosen output safely before joining; never overwrite a file.
    let mut output = create_private(path)?;
    let mut session = endpoint.join(clock)?;
    let mut sync = ClockSynchronizer::new();
    let mut next_clock = Instant::now();
    let mut received = 0u64;
    let mut highest = None::<u64>;
    let mut clock_samples = 0u64;
    println!("Paired. Recording authenticated PCM packets; no speaker playback is performed.");
    loop {
        if Instant::now() >= next_clock {
            match session.request_clock() {
                Ok(()) | Err(roomsync_transport::Error::PendingClockLimit) => {}
                Err(error) => return Err(error.into()),
            }
            next_clock = Instant::now() + Duration::from_millis(100);
        }
        match session.poll(Duration::from_millis(10))? {
            Some(Event::Audio(packet)) => {
                if packet.sequence() >= MAX_PACKETS {
                    return Err("ten-minute output limit exceeded".into());
                }
                output.seek(SeekFrom::Start(packet.sequence() * PCM_BYTES as u64))?;
                let mut bytes = [0; PCM_BYTES];
                for (sample, pair) in packet.samples().iter().zip(bytes.chunks_exact_mut(2)) {
                    pair.copy_from_slice(&sample.to_le_bytes());
                }
                output.write_all(&bytes)?;
                received += 1;
                highest = Some(highest.map_or(packet.sequence(), |old| old.max(packet.sequence())));
            }
            Some(Event::ClockSample(exchange)) => {
                if sync.observe(exchange).is_ok() {
                    clock_samples += 1;
                }
            }
            Some(Event::Closed) => break,
            _ => {}
        }
    }
    output.sync_all()?;
    let expected = session
        .peer_audio_packet_count()
        .ok_or("peer closed without an authenticated final packet count")?;
    if expected > MAX_PACKETS || received > expected || highest.is_some_and(|n| n >= expected) {
        return Err("invalid authenticated final packet count".into());
    }
    let gaps = expected - received;
    let locked = sync.model().is_some_and(|m| m.is_locked());
    println!(
        "Received {received} PCM packets, {gaps} missing positions, {clock_samples} accepted clock samples, clock_locked={locked}."
    );
    if gaps != 0 {
        return Err("packet loss left the PCM recording incomplete".into());
    }
    Ok(())
}
