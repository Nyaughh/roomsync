# Experimental PCM record, version 1

This document describes the **inner** PCM record only. Transport authentication,
replay defense, pairing and clock exchanges belong to `adapters/transport`.
Do not put these records directly on an untrusted network. This is a private
experimental protocol, not an implementation of RTP, AES67, Dante, PTP or a
claim of conformance with those standards.

Every audio record is exactly **512 bytes**. No compression, optional fields,
variable channels or native-layout casts are allowed. PCM payload is interleaved
48,000 Hz signed 16-bit stereo, 120 frames / 2.5 ms. Encoded audio bandwidth is
1.536 Mbit/s per receiver before headers, transport and network overhead.

| Byte offset | Length | Field |
| --- | --- | --- |
| 0 | 4 | ASCII `RSYN` |
| 4 | 1 | Version, exactly 1 |
| 5 | 1 | Sample encoding, exactly 1 = signed PCM16 little-endian |
| 6 | 1 | Channels, exactly 2 |
| 7 | 1 | Reserved, zero |
| 8 | 8 | Nonzero session ID, big-endian |
| 16 | 8 | Sequence number starting at 0, big-endian |
| 24 | 4 | Sample rate, big-endian, exactly 48000 |
| 28 | 2 | Frame count, big-endian, exactly 120 |
| 30 | 2 | Reserved, zero |
| 32 | 480 | 240 little-endian i16 samples: L0,R0,L1,R1,... |

First source frame is sequence × 120. Sequence arithmetic is checked against
`MAX_SEQUENCE`; sequence numbers never wrap. A new session is required on reset.
Decoder rejects any short/long record, unsupported format, nonzero reserved field,
zero session or arithmetic-overflow sequence. Transport must receive into a
buffer large enough to detect oversize datagrams; truncated datagrams must never
be accidentally accepted as valid 512-byte messages.

The authenticated control session negotiates a host **monotonic** nanosecond
presentation epoch for source frame zero. Do not use wall time or UTC. A device
must translate that epoch using the fitted host/local clock mapping, account for
output latency, and render the correct source frame at its actual DAC timestamp.

Clock exchange timestamps follow NTP's four-timestamp convention: local send,
host receive, host send, local receive. The estimator bounds RTT to 100 ms and
uses a fixed 64-observation history. Low-RTT weighted regression estimates clock
offset and drift. It requires eight selected samples spanning at least one
second before lock. Drift beyond ±500 ppm and >20 ms unexplained locked-clock
steps are rejected; known suspend/resume requires a new estimator/session.
Mapping expires after five seconds without an accepted exchange. Asymmetric
network paths remain an unobservable source of error; uncertainty is not a
hardware or acoustic accuracy guarantee.
