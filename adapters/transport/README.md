# RoomSync authenticated UDP transport

Real UDP sender/receiver transport and a raw-PCM file smoke-test utility. Linux localhost is tested. This crate alone does **not** capture or play an audio device, prove speaker alignment, remove physical latency, or provide a production security audit.

## Build and test

From the repository root:

```sh
cargo test --manifest-path adapters/transport/Cargo.toml
cargo clippy --manifest-path adapters/transport/Cargo.toml --all-targets -- -D warnings
cargo build --release --manifest-path adapters/transport/Cargo.toml
```

The integration test launches two actual processes, performs pairing and clock synchronization, and compares 64 received PCM packets byte-for-byte with the input. Unit tests exercise packet loss/reordering, tampering, source verification, replay, wrong key/session, malformed/oversized datagrams, idle timeout, confirmation retries, clock backpressure, and explicit end-of-stream counts.

## Two-machine file smoke test

Create a key outside the repository and transfer the file privately to the other designated machine. The command refuses to overwrite existing files. On Unix it creates private mode-0600 files and refuses permissive key-file permissions. On Windows use a private user directory with appropriate ACLs. The CLI does not inspect or harden Windows ACLs; secure Windows key storage is a pre-release gate. Do not put the key in command-line arguments, screenshots, logs, chat, or version control.

```sh
roomsync-net generate-key /private/path/roomsync.key

# Host has 192.168.1.10; receiver has 192.168.1.11.
roomsync-net send 192.168.1.10:47100 192.168.1.11:47101 /private/path/roomsync.key input.pcm
roomsync-net receive 192.168.1.11:47101 192.168.1.10:47100 /private/path/roomsync.key output.pcm
```

Use explicit reachable IP addresses and exact UDP ports on both machines. Open only those ports to the designated peer in the local firewall; do not expose them through public port forwarding. For one-machine testing use 127.0.0.1 with different ports. There is no broadcast, discovery service, anonymous listener, password fallback, or NAT traversal.

Input is raw signed PCM16 little-endian, stereo, 48 kHz. Each packet is 120 frames / 480 PCM bytes; input length must be a positive multiple of 480 bytes and at most ten minutes. The receiver writes packets to their sequence positions, so interior missing packets become zero-filled holes. It checks an authenticated final expected count, including trailing/all-packet loss, and exits with an error on loss or an absent finish marker. This is a bounded diagnostic recording, **not** a playout jitter buffer. No retransmission of audio is attempted. UDP close can be lost; idle timeout then reports incomplete reception.

## Interactive pairing (v0.1.1 apps)

The desktop and Android interactive apps use the new cancellable APIs. **Update both apps together.** Their authenticated v2 HELLO identifies support for an unstarted session and a separately acknowledged playback timeline. The older finite API and file CLI remain available for their original diagnostic flow.

- Host: `Endpoint::bind_listener(local, key, config)?.accept_waiting(clock, &stop_atomic, progress_callback)` listens until Stop, without requiring the phone IP in advance. The local listen address/port and the random pairing key must still be explicitly configured.
- Receiver: `Endpoint::bind(local, host, key, config)?.join_cancellable(clock, &stop_atomic, progress_callback)` retries on the **same UDP socket and source port** until paired or stopped. Either app can start first. A fresh Noise attempt uses new ephemeral keys; retransmissions reuse the exact same encrypted handshake bytes.
- Both methods check cancellation at least once per 100 ms of socket wait. Application progress callbacks must return promptly. Bind errors and unrecoverable socket/clock errors still fail explicitly.
- Successful pairing returns `SessionInfo { epoch_host_ns: 0, ... }`. This means **paired but not scheduled**, never permission to render audio. Continue authenticated clock exchange and host heartbeats while waiting for other people to join.
- After all expected peers are ready, the host chooses one common random session id and future host epoch. Call `Session::set_timeline(info)` for each peer, repeat periodically until `Event::TimelineAcknowledged(info)`, and require sufficient clock warmup before that epoch. Audio sends fail with `TimelineNotReady` until the ACK arrives.
- The receiver gets `Event::Timeline(info)` only after validation and automatic acknowledgment; initialize its renderer then. Repeating identical metadata does not reset the renderer. Older out-of-order timeline controls cannot roll back a newer start. Timeline changes after audio has started are rejected.
- `PairingProgress` reports stage, received packet count, HELLO sends, authenticated WELCOMEs, confirmation sends, wrong-source, malformed, authentication-rejected, incompatible-version, rate-limit, OS peer-unavailable, attempt count and elapsed time. These are observations, not proof that a firewall, Wi-Fi, or a particular key is the cause. No key, key hash, payload or session key is logged.

The listener allocates at most four candidate records. A valid Noise HELLO proves possession of the pairing key but may be a recording, so its source is only a **temporary candidate**. Admission and source pinning require READY authenticated with the fresh session key. Candidates expire without replay extending their deadlines. HELLO and READY authentication work share a 32-operation-per-second budget, and retries are at least 50 ms apart. Invalid traffic cannot claim a session; a captured HELLO cannot prevent a different valid client from pairing. LAN denial of service is still possible and is not claimed to be solved.

New regression tests use real UDP for both start orders, a host arriving six seconds after the receiver (beyond the old Android deadline), cancellation, authentication diagnostics, replayed-candidate isolation, and timeline ordering/acknowledgment.

## App integration

```rust,no_run
use roomsync_transport::{Config, Endpoint, Event, MonotonicClock, PairingKey};
use roomsync_core::clock::ClockSynchronizer;
use std::{net::SocketAddr, time::Duration};

fn receiver(local: SocketAddr, host: SocketAddr, key: PairingKey)
    -> roomsync_transport::Result<()>
{
    let clock = MonotonicClock::default();
    let mut session = Endpoint::bind(local, host, key, Config::default())?.join(clock)?;
    let info = session.info(); // authenticated session_id and epoch_host_ns
    let mut sync = ClockSynchronizer::new();
    session.request_clock()?;
    loop {
        match session.poll(Duration::from_millis(10))? {
            Some(Event::ClockSample(exchange)) => { let _ = sync.observe(exchange); }
            Some(Event::Audio(packet)) => {
                // Put packet in a bounded core RenderEngine/jitter buffer.
                // Use info.epoch_host_ns and sync.model() for presentation timing.
                let _ = (packet, info);
            }
            Some(Event::Closed) => break,
            _ => {}
        }
        // A real worker requests clock samples about every 100ms, not every poll.
        // Treat PendingClockLimit as backpressure; expired slots free after300ms.
    }
    Ok(())
}
```

`Endpoint::accept(clock, start_lead)` establishes a host session. `accept_with_info(clock, SessionInfo)` establishes independent encrypted sessions sharing one multi-speaker timeline. The supplied id must be fresh/random/nonzero and its host epoch within the next 60 seconds. Pairing fails if that shared epoch has elapsed before confirmation. The application must also verify enough warmup remains for clock lock before starting playout. A shared timeline does not reuse cipher keys or counters. Configure each receiver's peer address explicitly. Allow at least one second of clock observations before playout; the CLI uses two seconds and sends PCM 50 ms ahead. Host applications must continuously `poll`, including while sending, to answer clock requests and retry lost confirmation packets. Audio sent before the receiver finishes key confirmation can be discarded; honor the future epoch and clock warmup.

Run socket I/O, encryption, and file operations on a worker, never an audio callback. `poll` returns inline fixed-size packets and stores no audio queue. The transport currently uses bounded per-datagram heap allocations on send; a real-time callback must not call it. The application owns its bounded capture/render queues and chooses an appropriate dropout policy.

The default clock has an arbitrary `Instant` epoch. Hardware timestamps **must** be in the same local clock domain. Use `MonotonicClock::from_source(fn() -> Result<u64>)` to supply the actual monotonic source, for example Windows QPC nanoseconds, and propagate source failure as `Error::ClockUnavailable`; do not fall back silently to a different epoch. Map the four authenticated timestamps through `ClockSynchronizer`. Shared-clock synchronization cannot remove unknown device/Bluetooth delay or guarantee physical alignment without hardware measurement.

## Wire and security boundary

- Standard `snow` Noise pattern: `Noise_NNpsk0_25519_ChaChaPoly_SHA256`, with a protocol/version/PCM-format prologue and random 256-bit PSK. This is not a human password protocol. Noise handshake keys are ephemeral and directional. A PSK proves membership in the configured pair, not a person or independent per-device identity; use unique keys per peer if independent revocation is required.
- HELLO and WELCOME are Noise handshake messages. Encrypted READY/ACCEPTED provide explicit key confirmation. Session id and host presentation epoch are authenticated in WELCOME. Retries resend identical bytes, never re-encrypt different content under a used nonce.
- Audio is the core's canonical 512-byte record inside a one-byte encrypted message kind, a 16-byte AEAD tag, and 13-byte framing/nonce: **542-byte maximum UDP payload**. Oversized/truncated records are rejected before decoding. Nonce changes invalidate authentication; magic/type framing must exactly match the protocol.
- Counters are separate per direction and fail closed before nonce exhaustion. Authentication is checked before a 128-packet replay window advances; audio sequence numbers have their own window. Reordering within 128 packets is accepted. New handshakes derive fresh keys; old-session packets fail authentication. Early data cannot advance the confirmation replay window.
- Established sessions pin the full authenticated source IP and port. Fixed-peer receivers require the configured host; interactive listeners learn a source only after fresh key confirmation. No allocation uses an untrusted packet length. There is one bounded handshake retry cache, eight clock-request slots, fixed receive buffers, and at most 64 received datagrams per application poll. Ignored traffic does not accelerate handshake retransmissions or reset authenticated idle deadlines.
- Each clock reply must match an outstanding request. Lost requests expire independently of session liveness. Default authenticated idle timeout is three seconds; default pairing timeout is five seconds. On timeout/disconnect, rebuild the session, clock model, and playout state rather than silently reusing them.
- The PairingKey wrapper is redacted in Debug and zeroized on drop, but Snow 0.10.0 also holds internal PSK/DH/cipher-key copies that are not comprehensively zeroized. This is not a whole-process secret-erasure guarantee. Export is explicit and intended only for private storage. The implementation has adversarial tests but has not undergone an independent cryptographic or protocol audit. UDP source spoofing, link flooding, and on-path denial of service cannot be prevented here.

Upstream references: [Noise framework](https://noiseprotocol.org/noise.html), [snow API](https://docs.rs/snow/0.10.0/snow/), [stateless UDP transport state](https://docs.rs/snow/0.10.0/snow/struct.StatelessTransportState.html).
