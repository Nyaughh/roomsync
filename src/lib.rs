//! Experimental, allocation-free audio timing primitives. No platform capture or
//! playback is implied. All clock timestamps are monotonic nanoseconds, never UTC.
//!
//! Control/authentication belongs outside this crate. Never expose the raw PCM
//! protocol to an untrusted network without an authenticated encrypted transport.
#![forbid(unsafe_code)]

pub mod clock;
pub mod device_clock;
pub mod jitter;
pub mod protocol;
pub mod render;
pub mod simulation;

pub use protocol::{AudioPacket, FRAMES_PER_PACKET, SAMPLE_RATE, SESSION_PACKET_BYTES};
