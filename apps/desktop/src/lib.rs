//! Timestamped source normalization and packetization, shared by the desktop CLI.
use roomsync_core::{AudioPacket, FRAMES_PER_PACKET, SAMPLE_RATE, protocol::SAMPLES_PER_PACKET};
use roomsync_windows::convert::f32_to_pcm16;

/// Converts capture's timestamped PCM onto the canonical host-clock 48 kHz grid.
/// Linear interpolation is a drift normalizer, not a general sample-rate converter.
/// Windows must deliver nominal 48 kHz. Missing capture positions remain silent
/// on the original timeline; invalid/backward clocks stop the session.
pub struct CapturePacketizer {
    session_id: u64,
    source_epoch_ns: u64,
    previous: Option<(f64, [f32; 2])>,
    samples: [i16; SAMPLES_PER_PACKET],
    filled: usize,
    next_frame: u64,
    sequence: u64,
    emitted_packets: u64,
}
impl CapturePacketizer {
    pub fn new(session_id: u64, source_epoch_ns: u64) -> Result<Self, &'static str> {
        if session_id == 0 {
            return Err("invalid capture session");
        }
        Ok(Self {
            session_id,
            source_epoch_ns,
            previous: None,
            samples: [0; SAMPLES_PER_PACKET],
            filled: 0,
            next_frame: 0,
            sequence: 0,
            emitted_packets: 0,
        })
    }
    pub fn packets_emitted(&self) -> u64 {
        self.emitted_packets
    }
    /// A flagged drop is not ordinary timestamp jitter. Discard the unfinished
    /// packet and re-anchor the next capture block onto the original host epoch.
    pub fn mark_discontinuity(&mut self) {
        self.previous = None;
        self.samples.fill(0);
        self.filled = 0;
    }

    pub fn consume(
        &mut self,
        qpc_ns: u64,
        frame_duration_ns: f64,
        frames: &[[f32; 2]],
        mut emit: impl FnMut(AudioPacket) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        let nominal = 1e9 / SAMPLE_RATE as f64;
        if !frame_duration_ns.is_finite()
            || !(nominal * 0.998..=nominal * 1.002).contains(&frame_duration_ns)
        {
            return Err("capture sample clock is outside the supported drift range");
        }
        if frames.is_empty() {
            return Ok(());
        }
        let observed_start = (i128::from(qpc_ns) - i128::from(self.source_epoch_ns)) as f64;
        let mut start = observed_start;
        let mut period = frame_duration_ns;
        if let Some((previous_time, _)) = self.previous {
            let predicted = previous_time + frame_duration_ns;
            let error = observed_start - predicted;
            if error < -5_000_000.0 {
                return Err("capture timestamps moved backward; restart the session");
            }
            if error > 5_000_000.0 {
                // Loopback can stop producing buffers while the endpoint is idle.
                // Keep the global epoch; skipped source positions become silence.
                self.previous = None;
                self.samples.fill(0);
                self.filled = 0;
            } else {
                // Correlated QPC stamps have jitter. Preserve continuity and steer
                // rate gently instead of re-anchoring each block to raw timestamps.
                start = predicted;
                period +=
                    (error / (frames.len() as f64 * 50.0)).clamp(-nominal * 0.001, nominal * 0.001);
            }
        }
        for (i, sample) in frames.iter().enumerate() {
            let time = start + i as f64 * period;
            if let Some((previous_time, previous_sample)) = self.previous {
                if time <= previous_time {
                    return Err("capture timestamps moved backward; restart the session");
                }
                if time - previous_time > 5_000_000.0 {
                    return Err("capture lost more than 5 ms; restart the session");
                }
                let mut emitted = 0;
                while self.next_frame as f64 * nominal <= time {
                    let target = self.next_frame as f64 * nominal;
                    if target < previous_time {
                        return Err("capture started after its negotiated timeline");
                    }
                    // At most a few outputs can fit between adjacent drifted input
                    // samples. A timestamp gap cannot create unbounded CPU work.
                    if emitted >= 4 {
                        return Err("capture timestamp gap exceeded interpolation budget");
                    }
                    let fraction = ((target - previous_time) / (time - previous_time)) as f32;
                    for ch in 0..2 {
                        self.samples[self.filled * 2 + ch] = f32_to_pcm16(
                            previous_sample[ch] + (sample[ch] - previous_sample[ch]) * fraction,
                        );
                    }
                    self.filled += 1;
                    self.next_frame = self
                        .next_frame
                        .checked_add(1)
                        .ok_or("capture timeline exhausted")?;
                    emitted += 1;
                    if self.filled == FRAMES_PER_PACKET {
                        let packet = AudioPacket::new(self.session_id, self.sequence, self.samples)
                            .map_err(|_| "capture sequence exhausted")?;
                        emit(packet)?;
                        self.emitted_packets = self.emitted_packets.saturating_add(1);
                        self.sequence += 1;
                        self.filled = 0;
                    }
                }
            } else if time > 0.0 {
                // Loopback may be silent until the user starts playback. Preserve
                // the common timeline and leave earlier positions silent.
                self.next_frame = (time / nominal).ceil() as u64;
                self.sequence = self.next_frame / FRAMES_PER_PACKET as u64;
                self.filled = (self.next_frame % FRAMES_PER_PACKET as u64) as usize;
            }
            self.previous = Some((time, *sample));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamped_packetizer_preserves_channel_order_and_sequence() {
        let mut p = CapturePacketizer::new(9, 1_000_000_000).unwrap();
        let frames = vec![[0.5, -0.25]; 481];
        let mut packets = Vec::new();
        p.consume(999_979_167, 1e9 / 48_000.0, &frames, |packet| {
            packets.push(packet);
            Ok(())
        })
        .unwrap();
        assert_eq!(packets.len(), 4);
        assert_eq!(packets[3].sequence(), 3);
        assert_eq!(&packets[0].samples()[..2], &[16_384, -8_192]);
    }
    #[test]
    fn idle_gap_preserves_timeline_but_backward_capture_fails_closed() {
        let mut p = CapturePacketizer::new(1, 1_000_000_000).unwrap();
        p.consume(999_000_000, 1e9 / 48_000.0, &[[0.0; 2]; 10], |_| Ok(()))
            .unwrap();
        p.consume(1_010_000_000, 1e9 / 48_000.0, &[[0.0; 2]; 10], |_| Ok(()))
            .unwrap();
        assert!(p.next_frame >= 480);
        assert!(
            p.consume(998_000_000, 1e9 / 48_000.0, &[[0.0; 2]; 10], |_| Ok(()))
                .is_err()
        );
    }
    #[test]
    fn drifted_capture_normalizes_to_host_clock_duration() {
        for ppm in [-500.0, 500.0] {
            let mut p = CapturePacketizer::new(1, 10_000_000).unwrap();
            let duration = 1e9 / (48_000.0 * (1.0 + ppm / 1e6));
            let mut count = 0;
            let mut frame = 0u64;
            for _ in 0..101 {
                let start = 9_000_000 + (frame as f64 * duration).round() as u64;
                p.consume(start, duration, &[[0.1; 2]; 480], |_| {
                    count += 1;
                    Ok(())
                })
                .unwrap();
                frame += 480;
            }
            let expected =
                (((frame - 1) as f64 * duration - 1_000_000.0) * 48_000.0 / 1e9 + 1.0) as u64 / 120;
            assert_eq!(count, expected);
        }
    }
    #[test]
    fn block_timestamp_jitter_does_not_break_continuous_capture() {
        let mut p = CapturePacketizer::new(1, 1_000_000_000).unwrap();
        let mut count = 0;
        for block in 0..500u64 {
            let jitter = if block % 2 == 0 { 40_000i64 } else { -40_000 };
            let stamp = (1_000_000_000i64 + block as i64 * 10_000_000 + jitter) as u64;
            p.consume(stamp, 1e9 / 48_000.0, &[[0.1; 2]; 480], |_| {
                count += 1;
                Ok(())
            })
            .unwrap();
        }
        assert!((1998..=2001).contains(&count));
    }
    #[test]
    fn initially_idle_source_resumes_at_its_original_epoch() {
        let mut p = CapturePacketizer::new(1, 1_000_000_000).unwrap();
        let mut sequences = Vec::new();
        p.consume(3_000_000_000, 1e9 / 48_000.0, &[[0.1; 2]; 480], |packet| {
            sequences.push(packet.sequence());
            Ok(())
        })
        .unwrap();
        assert_eq!(sequences[0], 800);
    }
    #[test]
    fn explicit_short_capture_drop_does_not_compress_the_timeline() {
        let mut p = CapturePacketizer::new(1, 1_000_000_000).unwrap();
        p.consume(1_000_000_000, 1e9 / 48_000.0, &[[0.5; 2]; 480], |_| Ok(()))
            .unwrap();
        p.mark_discontinuity();
        let mut next = Vec::new();
        p.consume(1_011_250_000, 1e9 / 48_000.0, &[[0.5; 2]; 480], |packet| {
            next.push(packet);
            Ok(())
        })
        .unwrap();
        assert_eq!(next[0].sequence(), 4);
        assert!(next[0].samples()[..120].iter().all(|s| *s == 0));
        assert!(next[0].samples()[122..].iter().all(|s| *s == 16_384));
    }
}
