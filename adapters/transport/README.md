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

### Two-receiver dispatch diagnostic

```sh
cargo run --release --manifest-path adapters/transport/Cargo.toml --example peer_dispatch -- 0
cargo run --release --manifest-path adapters/transport/Cargo.toml --example peer_dispatch -- 120
```

This localhost-only probe sends 800 authenticated audio packets to each of two independently serviced receivers. It copies the desktop scheduling policy: four source packets every 10 ms, a 128-packet bounded queue per peer, at most 64 sends before each 2 ms transport poll. Its queues are standard bounded channels, not the desktop's lock-free `ArrayQueue`; it does not benchmark that queue implementation or exercise audio hardware. The argument injects one 0..=200 ms worker stall into receiver 2's **sender**, not the network. It reports queue residence, send-call wall time, worker gaps, send-batch size, and receiver arrival gaps. Temporary pairing keys are never printed or saved.

For example, one cloud Linux run delivered every packet to both peers. The baseline maximum queue residence was 10–12 ms. A 120 ms injected stall produced a 52-packet burst and 124 ms maximum queue residence on peer 2; a 200 ms stall produced a 64-packet burst and 209 ms maximum queue residence. These numbers vary with the machine and load. They establish that late audio can accumulate and arrive in a burst without queue overflow or packet loss, not that this caused any particular phone's cutouts. There is no timing pass/fail threshold in this probe.

Desktop peers have independent queues, sockets and workers. Capture fanout is nonblocking, and queue overflow fails the session rather than dropping silently. There is no intentional peer-index priority. An OS scheduler stall or a blocked send can nevertheless delay an individual worker. [Windows documents that blocking UDP `sendto` can wait for transmit-buffer space](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-sendto); neither that condition nor Windows receive-timeout granularity is established by this Linux probe. Use per-peer enqueue-to-send age, send-call time, worker gaps and observed queue depth alongside receiver diagnostics to distinguish host dispatch from downstream delay. This diagnostic changes no transport behavior or wire format.

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

## Interactive pairing and output readiness

The desktop and Android interactive apps use the new cancellable APIs. **Update both apps together.** Their authenticated revision3 HELLO identifies mandatory output readiness and a separately acknowledged playback timeline with a chosen send lead. The older finite API and file CLI remain available for their original diagnostic flow.

- Host: `Endpoint::bind_listener(local, key, config)?.accept_waiting(clock, &stop_atomic, progress_callback)` listens until Stop, without requiring the phone IP in advance. The local listen address/port and the random pairing key must still be explicitly configured.
- Receiver: `Endpoint::bind(local, host, key, config)?.join_cancellable(clock, &stop_atomic, progress_callback)` retries on the **same UDP socket and source port** until paired or stopped. Either app can start first. A fresh Noise attempt uses new ephemeral keys; retransmissions reuse the exact same encrypted handshake bytes.
- Each interactive HELLO attempt and subsequent authenticated READY/ACCEPTED confirmation phase gets the configured timeout budget separately. Only successfully authenticating WELCOME starts the confirmation budget; duplicate or unauthenticated traffic cannot extend it. An accepted host must keep polling its Session to answer a retried READY if the first ACCEPTED packet was lost. This timing correction does not change the wire protocol or revision3 compatibility.
- Both methods check cancellation at least once per 100 ms of socket wait. Application progress callbacks must return promptly. Bind errors and unrecoverable socket/clock errors still fail explicitly.
- Successful pairing returns `SessionInfo { epoch_host_ns: 0, ... }`. This means **paired but not scheduled**, never permission to render audio. Continue authenticated clock exchange and host heartbeats while waiting for other people to join.
- Each receiver must first run its actual output queue, calibrate the write-ahead horizon, and call `Session::report_readiness(ReceiverReadiness { output_horizon_ns, render_block_ns })`. Both fields are nanoseconds. The horizon is bounded to 0..=1s and the render block to 1ns..=100ms. Zero horizon is valid; zero block is not. These values never control an allocation.
- The application owns retries: resend the same readiness about every 250 ms until `session.readiness_acknowledged()` or `Event::ReadinessAcknowledged`. Lost reports or ACKs converge by retrying; identical reports are re-ACKed without another `ReceiverReady` event. The host receives `Event::ReceiverReady` and can inspect `peer_readiness()`.
- After all expected peers have reported readiness, the host computes a group lead covering every measured horizon plus render block, its actual capture/engine allowance, and a network/clock guard. The supported send-lead cap is 300 ms, leaving 20 ms of the existing 320 ms jitter window. A larger required budget must fail with a clear unsupported-output-route error, never be silently clamped.
- The host chooses one common random session id and future host epoch. Call `Session::set_timeline_with_lead(info, chosen_lead_ns)` for each peer, repeat periodically until `Event::TimelineAcknowledged(info)`, and require sufficient clock warmup before that epoch. Transport checks the declared lead against the receiver minimum and 300 ms cap. Both receiver and host can read `negotiated_lead_ns()`. The receiver validates and acknowledges the selected lead as well as the session/epoch. Audio sends fail with `TimelineNotReady` until that ACK arrives.
- The receiver gets `Event::Timeline(info)` only after readiness and lead validation plus automatic acknowledgment; initialize its renderer then. Repeating identical metadata does not reset the renderer. Older out-of-order readiness or timeline controls cannot roll back a newer value. An identical readiness retry after scheduling is harmless, including an ACK still in flight. Changed output timing after scheduling requires stopping and recalibrating, never silently retiming the live session. Timeline or lead changes after audio has started are rejected.
- `PairingProgress` reports stage, received packet count, HELLO sends, authenticated WELCOMEs, confirmation sends, wrong-source, malformed, authentication-rejected, incompatible-version, rate-limit, OS peer-unavailable, attempt count and elapsed time. These are observations, not proof that a firewall, Wi-Fi, or a particular key is the cause. No key, key hash, payload or session key is logged.

The listener allocates at most four candidate records. A valid Noise HELLO proves possession of the pairing key but may be a recording, so its source is only a **temporary candidate**. Admission and source pinning require READY authenticated with the fresh session key. Candidates expire without replay extending their deadlines. HELLO and READY authentication work share a 32-operation-per-second budget, and retries are at least 50 ms apart. Invalid traffic cannot claim a session; a captured HELLO cannot prevent a different valid client from pairing. LAN denial of service is still possible and is not claimed to be solved.

New regression tests use real UDP for both start orders, a host arriving six seconds after the receiver (beyond the old Android deadline), cancellation, authentication diagnostics, replayed-candidate isolation, readiness loss/ACK loss/reordering, timing budget limits, version rejection, and timeline ordering/acknowledgment.

The listener sends an authenticated update-version response to older key-valid interactive clients. A new client rejects authenticated incompatible responses explicitly. An older server that does not respond to the new HELLO cannot be distinguished remotely from an unavailable server; update both apps together and use host-side version diagnostics. The legacy finite file-transfer API remains a diagnostic tool and does not certify physical output readiness.

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
        // Treat PendingClockLimit as backpressure; expired slots free after 300 ms.
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
