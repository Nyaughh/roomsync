//! Callback-safe stereo playout driven by a hardware-correlated local timestamp.
//! Linear interpolation provides drift correction, not mastering-quality resampling.
use crate::clock::ClockModel;
use crate::device_clock::DeviceClockEstimator;
use crate::jitter::{BufferStats, InsertResult, JitterBuffer};
use crate::protocol::{AudioPacket, SAMPLE_RATE};

const MAX_CALLBACK_FRAMES: usize = 8_192;
const REALIGN_FRAMES: f64 = 240.0;
const MAX_SLEW: f64 = 200.0e-6;
const RECOVERY_FRAMES: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelMap {
    Stereo,
    Left,
    Right,
    Mono,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderReport {
    pub frames: usize,
    pub concealed_frames: usize,
    pub waiting_frames: usize,
    pub realigned: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderError {
    InvalidSession,
    UnsupportedSampleRate,
    CallbackTooLarge,
}

#[derive(Debug)]
pub struct Renderer {
    buffer: JitterBuffer,
    host_epoch_ns: u64,
    channel_map: ChannelMap,
    cursor: Option<f64>,
    timeline_started: bool,
    last_good: [f32; 2],
    recovery: u32,
    device_clock: DeviceClockEstimator,
    output_frames: u64,
}
impl Renderer {
    pub fn new(
        session_id: u64,
        host_epoch_ns: u64,
        channel_map: ChannelMap,
    ) -> Result<Self, RenderError> {
        Ok(Self {
            buffer: JitterBuffer::new(session_id).ok_or(RenderError::InvalidSession)?,
            host_epoch_ns,
            channel_map,
            cursor: None,
            timeline_started: false,
            last_good: [0.0; 2],
            recovery: RECOVERY_FRAMES,
            device_clock: DeviceClockEstimator::new(),
            output_frames: 0,
        })
    }
    pub fn push(&mut self, packet: AudioPacket) -> InsertResult {
        self.buffer.push(packet)
    }
    pub fn stats(&self) -> BufferStats {
        self.buffer.stats()
    }
    /// Next fractional source frame, for diagnostics (not a hardware measurement).
    pub fn source_cursor(&self) -> Option<f64> {
        self.cursor
    }
    /// Device route/rate changes require a new timing calibration. Discontinuities
    /// larger than 5 ms re-anchor with a short fade and are reported, never hidden.
    ///
    /// `local_dac_start_ns` MUST describe when out[0] will reach the DAC, not when
    /// this function is called. Include measured/calibrated downstream output delay.
    /// Clock updates and packet transfer must be done outside the OS audio callback,
    /// or via a bounded preallocated SPSC mailbox owned by the adapter.
    pub fn render(
        &mut self,
        local_dac_start_ns: u64,
        device_sample_rate: u32,
        clock: ClockModel,
        out: &mut [[f32; 2]],
    ) -> Result<RenderReport, RenderError> {
        if !(48_000..=192_000).contains(&device_sample_rate) {
            out.fill([0.0; 2]);
            return Err(RenderError::UnsupportedSampleRate);
        }
        if out.len() > MAX_CALLBACK_FRAMES {
            out.fill([0.0; 2]);
            return Err(RenderError::CallbackTooLarge);
        }
        let ns_per_output_frame = self
            .device_clock
            .observe(local_dac_start_ns, self.output_frames, device_sample_rate)
            .unwrap_or(1e9 / device_sample_rate as f64);
        self.output_frames = self.output_frames.saturating_add(out.len() as u64);
        let mut report = RenderReport {
            frames: out.len(),
            ..RenderReport::default()
        };
        if !clock.is_locked() || !clock.is_fresh_at(local_dac_start_ns) {
            out.fill([0.0; 2]);
            report.waiting_frames = out.len();
            return Ok(report);
        }
        let target = clock.host_elapsed_ns(local_dac_start_ns, self.host_epoch_ns)
            * SAMPLE_RATE as f64
            / 1e9;
        // Clock fitting can improve during the silent warmup. There is no
        // audible continuity to preserve yet: carrying the old phase into the
        // first sample would leave receivers needlessly out of step for seconds.
        // Once the source timeline starts, keep the bounded live-audio slew.
        let mut position = if self.timeline_started {
            self.cursor.unwrap_or(target)
        } else {
            target
        };
        let error = target - position;
        if error.abs() > REALIGN_FRAMES {
            position = target;
            self.last_good = [0.0; 2];
            self.recovery = 0;
            report.realigned = true;
        }
        // Correct small estimator updates gradually rather than dropping frames.
        let slew = if report.realigned {
            0.0
        } else {
            (error / (SAMPLE_RATE as f64 * 2.0)).clamp(-MAX_SLEW, MAX_SLEW)
        };
        let step = SAMPLE_RATE as f64 * ns_per_output_frame / 1e9
            * (clock.host_rate_per_local_tick() + slew);
        for output in out.iter_mut() {
            let value = if position < 0.0 {
                report.waiting_frames += 1;
                [0.0; 2]
            } else {
                self.timeline_started = true;
                let index = position.floor() as u64;
                let fraction = (position - position.floor()) as f32;
                let a = self.buffer.sample(index);
                let b = if fraction < 1e-6 {
                    a
                } else {
                    index.checked_add(1).and_then(|i| self.buffer.sample(i))
                };
                match a.zip(b) {
                    Some((a, b)) => {
                        let sample = [
                            a[0] + (b[0] - a[0]) * fraction,
                            a[1] + (b[1] - a[1]) * fraction,
                        ];
                        self.recovery = (self.recovery + 1).min(RECOVERY_FRAMES);
                        let gain = self.recovery as f32 / RECOVERY_FRAMES as f32;
                        self.last_good = [sample[0] * gain, sample[1] * gain];
                        self.last_good
                    }
                    None => {
                        report.concealed_frames += 1;
                        self.recovery = 0;
                        // Bounded cheap decay instead of stale repeated packets.
                        self.last_good[0] *= 0.98;
                        self.last_good[1] *= 0.98;
                        if self.last_good[0].abs() < 1e-12 {
                            self.last_good[0] = 0.0;
                        }
                        if self.last_good[1].abs() < 1e-12 {
                            self.last_good[1] = 0.0;
                        }
                        self.last_good
                    }
                }
            };
            *output = match self.channel_map {
                ChannelMap::Stereo => value,
                ChannelMap::Left => [value[0]; 2],
                ChannelMap::Right => [value[1]; 2],
                ChannelMap::Mono => [(value[0] + value[1]) * 0.5; 2],
            };
            position += step;
        }
        self.cursor = Some(position);
        if position >= 1.0 {
            self.buffer
                .discard_before_frame((position as u64).saturating_sub(1));
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{FRAMES_PER_PACKET, SAMPLES_PER_PACKET};
    fn packet() -> AudioPacket {
        let mut s = [0; SAMPLES_PER_PACKET];
        for f in s.chunks_exact_mut(2) {
            f[0] = 16_384;
            f[1] = -8_192;
        }
        AudioPacket::new(1, 0, s).unwrap()
    }
    #[test]
    fn maps_channels_and_waits_for_presentation() {
        for (map, expected) in [
            (ChannelMap::Stereo, [0.5, -0.25]),
            (ChannelMap::Left, [0.5, 0.5]),
            (ChannelMap::Right, [-0.25, -0.25]),
            (ChannelMap::Mono, [0.125, 0.125]),
        ] {
            let mut r = Renderer::new(1, 1_000_000_000, map).unwrap();
            r.push(packet());
            let mut out = [[0.0; 2]; FRAMES_PER_PACKET];
            r.render(1_000_000_000, SAMPLE_RATE, ClockModel::identity(), &mut out)
                .unwrap();
            assert_eq!(out[0], expected);
            assert_eq!(out[119], expected);
        }
        let mut r = Renderer::new(1, 1_000_000_000, ChannelMap::Stereo).unwrap();
        r.push(packet());
        let mut out = [[1.0; 2]; 120];
        let report = r
            .render(900_000_000, SAMPLE_RATE, ClockModel::identity(), &mut out)
            .unwrap();
        assert_eq!(report.waiting_frames, 120);
        assert!(out.iter().all(|v| *v == [0.0; 2]));
    }
    #[test]
    fn losses_fade_to_silence_and_bad_configuration_clears_output() {
        let mut r = Renderer::new(1, 0, ChannelMap::Stereo).unwrap();
        r.push(packet());
        let mut out = [[0.0; 2]; 120];
        r.render(0, SAMPLE_RATE, ClockModel::identity(), &mut out)
            .unwrap();
        let report = r
            .render(2_500_000, SAMPLE_RATE, ClockModel::identity(), &mut out)
            .unwrap();
        assert_eq!(report.concealed_frames, 120);
        assert!(out[119][0] < out[0][0]);
        assert_eq!(
            r.render(0, 0, ClockModel::identity(), &mut out),
            Err(RenderError::UnsupportedSampleRate)
        );
        assert!(out.iter().all(|v| *v == [0.0; 2]));
    }
    fn shifted_clock(offset_ns: i64) -> ClockModel {
        use crate::clock::{ClockExchange, ClockSynchronizer};
        let mut sync = ClockSynchronizer::new();
        for index in 0..16 {
            let t = 1_000_000_000_u64 + index * 100_000_000;
            sync.observe(ClockExchange {
                local_send_ns: t,
                host_receive_ns: t.checked_add_signed(offset_ns).unwrap() + 500_000,
                host_send_ns: t.checked_add_signed(offset_ns).unwrap() + 600_000,
                local_receive_ns: t + 1_100_000,
            })
            .unwrap();
        }
        let model = sync.model().unwrap();
        assert!(model.is_locked());
        model
    }

    #[test]
    fn warmup_clock_corrections_do_not_leave_initial_speaker_phase_error() {
        for rate in [48_000, 96_000, 192_000] {
            for offset in [-2_000_000, 2_000_000] {
                let epoch = 5_000_000_000;
                let mut receiver = Renderer::new(1, epoch, ChannelMap::Stereo).unwrap();
                let mut reference = Renderer::new(1, epoch, ChannelMap::Stereo).unwrap();
                let mut output = vec![[0.0; 2]; rate as usize / 400];
                let mut reference_output = output.clone();
                // Include the interpolation neighbor for oversampled output.
                for sequence in 0..2 {
                    let source = AudioPacket::new(1, sequence, *packet().samples()).unwrap();
                    receiver.push(source.clone());
                    reference.push(source);
                }
                receiver
                    .render(4_995_000_000, rate, shifted_clock(offset), &mut output)
                    .unwrap();
                reference
                    .render(
                        4_995_000_000,
                        rate,
                        ClockModel::identity(),
                        &mut reference_output,
                    )
                    .unwrap();
                let corrected = receiver
                    .render(4_997_500_000, rate, ClockModel::identity(), &mut output)
                    .unwrap();
                reference
                    .render(
                        4_997_500_000,
                        rate,
                        ClockModel::identity(),
                        &mut reference_output,
                    )
                    .unwrap();
                assert_eq!(corrected.waiting_frames, output.len());
                assert!(
                    output.iter().flatten().all(|sample| *sample == 0.0),
                    "A stale warmup phase must not start audio early"
                );
                assert!(!corrected.realigned);
                assert_eq!(
                    receiver.source_cursor(),
                    reference.source_cursor(),
                    "Before any source audio plays, follow the latest clock instead of slewing an obsolete warmup phase"
                );
                let first = receiver
                    .render(epoch, rate, ClockModel::identity(), &mut output)
                    .unwrap();
                reference
                    .render(epoch, rate, ClockModel::identity(), &mut reference_output)
                    .unwrap();
                assert_eq!(first.waiting_frames, 0);
                assert_eq!(first.concealed_frames, 0);
                assert!(!first.realigned);
                assert_eq!(
                    output, reference_output,
                    "The first source packet must play at the same DAC time"
                );
            }
        }
    }

    #[test]
    fn live_clock_correction_still_slews_instead_of_jumping() {
        let mut receiver = Renderer::new(1, 5_000_000_000, ChannelMap::Stereo).unwrap();
        let mut output = [[0.0; 2]; 120];
        receiver
            .render(
                5_000_000_000,
                SAMPLE_RATE,
                ClockModel::identity(),
                &mut output,
            )
            .unwrap();
        let report = receiver
            .render(
                5_002_500_000,
                SAMPLE_RATE,
                shifted_clock(2_000_000),
                &mut output,
            )
            .unwrap();
        assert!(!report.realigned);
        let cursor = receiver.source_cursor().unwrap();
        assert!(
            (240.0..241.0).contains(&cursor),
            "Live phase must slew, got {cursor}"
        );
    }
}
