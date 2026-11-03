//! Local host playback uses the same packets and presentation epoch as the phones.
//! The callback owns its renderer and touches only bounded preallocated queues and
//! atomics. Capture/output routing and hardware timing remain explicit.
use crate::budget::OutputBudget;
use crossbeam_queue::ArrayQueue;
use roomsync_core::{
    AudioPacket,
    clock::ClockModel,
    render::{ChannelMap, Renderer},
};
use roomsync_windows::{AdapterError, RenderConfig, RenderFill, RenderStream, RenderTiming};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
};

const QUEUE_PACKETS: usize = 128;
const CALIBRATION_NS: u64 = 500_000_000;
const CLOCK_MAX_AGE_NS: u64 = 200_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LocalFault {
    InvalidTiming = 1,
    ChangedTiming = 2,
    RenderFailed = 3,
    InvalidPacket = 4,
}
impl LocalFault {
    fn message(self) -> &'static str {
        match self {
            Self::InvalidTiming => {
                "The Windows speaker returned invalid or stale timing. Restart the group after checking the output device."
            }
            Self::ChangedTiming => {
                "Windows speaker buffering changed after calibration. Restart the group to recalibrate."
            }
            Self::RenderFailed => {
                "Windows speaker rendering failed. Restart the group after checking the output device."
            }
            Self::InvalidPacket => {
                "The local speaker received a packet for a different timeline. Restart the group."
            }
        }
    }
}

#[derive(Debug)]
pub enum LocalOutputError {
    Adapter(AdapterError),
    Fault(LocalFault),
    Stopped,
    QueueFull,
    AlreadyArmed,
    InvalidTimeline,
}
impl fmt::Display for LocalOutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adapter(error) => error.fmt(f),
            Self::Fault(error) => f.write_str(error.message()),
            Self::Stopped => f.write_str("The Windows speaker stopped. Restart the group after checking the output device."),
            Self::QueueFull => f.write_str("The Windows speaker queue overflowed. Playback stopped instead of accumulating delay."),
            Self::AlreadyArmed => f.write_str("The local speaker timeline was already started; restart the group to change it."),
            Self::InvalidTimeline => f.write_str("The local speaker received an invalid timeline."),
        }
    }
}
impl std::error::Error for LocalOutputError {}
impl From<AdapterError> for LocalOutputError {
    fn from(value: AdapterError) -> Self {
        Self::Adapter(value)
    }
}

#[derive(Default)]
struct Probe {
    first_now: Option<u64>,
    first_clock: Option<u64>,
    last_clock: Option<(u64, u64)>,
    last_callback: Option<u64>,
    advances: u32,
    max_horizon: u64,
    max_block: u64,
    frozen: Option<OutputBudget>,
}
impl Probe {
    fn observe(
        &mut self,
        timing: RenderTiming,
        frames: usize,
    ) -> Result<Option<OutputBudget>, LocalFault> {
        if timing.sample_rate != 48_000
            || frames == 0
            || frames > 4_800
            || timing.discontinuity
            || self
                .last_callback
                .is_some_and(|last| timing.callback_qpc_ns < last)
            || timing.clock_frequency == 0
            || timing.clock_qpc_ns == 0
            || timing.clock_qpc_ns > timing.callback_qpc_ns
            || timing.callback_qpc_ns - timing.clock_qpc_ns > CLOCK_MAX_AGE_NS
            || timing.estimated_dac_start_ns < timing.callback_qpc_ns
            || self.last_clock.is_some_and(|(position, stamp)| {
                timing.clock_position < position || timing.clock_qpc_ns < stamp
            })
        {
            return Err(LocalFault::InvalidTiming);
        }
        self.last_callback = Some(timing.callback_qpc_ns);
        let horizon = timing.estimated_dac_start_ns - timing.callback_qpc_ns;
        if horizon > 1_000_000_000 {
            return Err(LocalFault::InvalidTiming);
        }
        let block = (frames as u64 * 1_000_000_000).div_ceil(u64::from(timing.sample_rate));
        if let Some(budget) = self.frozen {
            if horizon + block > budget.output_horizon_ns + budget.render_block_ns + 10_000_000 {
                return Err(LocalFault::ChangedTiming);
            }
            self.last_clock = Some((timing.clock_position, timing.clock_qpc_ns));
            return Ok(None);
        }
        let advanced = self.last_clock.is_some_and(|(position, stamp)| {
            timing.clock_position > position && timing.clock_qpc_ns > stamp
        });
        self.last_clock = Some((timing.clock_position, timing.clock_qpc_ns));
        self.first_now.get_or_insert(timing.callback_qpc_ns);
        self.first_clock.get_or_insert(timing.clock_qpc_ns);
        self.max_horizon = self.max_horizon.max(horizon);
        self.max_block = self.max_block.max(block);
        if timing.clock_advancing && advanced {
            self.advances = self.advances.saturating_add(1);
        }
        if self.advances >= 5
            && timing
                .callback_qpc_ns
                .saturating_sub(self.first_now.unwrap())
                >= CALIBRATION_NS
            && timing
                .clock_qpc_ns
                .saturating_sub(self.first_clock.unwrap())
                >= CALIBRATION_NS
        {
            let budget = OutputBudget {
                output_horizon_ns: self.max_horizon,
                render_block_ns: self.max_block,
            };
            self.frozen = Some(budget);
            Ok(Some(budget))
        } else {
            Ok(None)
        }
    }
}

#[derive(Default)]
struct Shared {
    ready: AtomicBool,
    horizon: AtomicU64,
    block: AtomicU64,
    fault: AtomicU8,
    concealed: AtomicU64,
    late: AtomicU64,
    too_far: AtomicU64,
    peak: AtomicU32,
}
impl Shared {
    fn fail(&self, fault: LocalFault) {
        let _ = self
            .fault
            .compare_exchange(0, fault as u8, Ordering::Release, Ordering::Relaxed);
    }
    fn fault(&self) -> Option<LocalFault> {
        match self.fault.load(Ordering::Acquire) {
            0 => None,
            1 => Some(LocalFault::InvalidTiming),
            2 => Some(LocalFault::ChangedTiming),
            4 => Some(LocalFault::InvalidPacket),
            _ => Some(LocalFault::RenderFailed),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LocalOutputStats {
    pub concealed_frames: u64,
    pub late_packets: u64,
    pub too_far_packets: u64,
    pub peak: f32,
}

struct Callback {
    probe: Probe,
    shared: Arc<Shared>,
    packets: Arc<ArrayQueue<AudioPacket>>,
    start: Arc<ArrayQueue<Box<Renderer>>>,
    renderer: Option<Box<Renderer>>,
}
impl Callback {
    fn render(&mut self, timing: RenderTiming, out: &mut [[f32; 2]]) -> RenderFill {
        out.fill([0.0; 2]);
        if self.shared.fault().is_some() {
            return RenderFill::Silence;
        }
        match self.probe.observe(timing, out.len()) {
            Ok(Some(budget)) => {
                self.shared
                    .horizon
                    .store(budget.output_horizon_ns, Ordering::Relaxed);
                self.shared
                    .block
                    .store(budget.render_block_ns, Ordering::Relaxed);
                self.shared.ready.store(true, Ordering::Release);
            }
            Ok(None) => {}
            Err(error) => {
                self.shared.fail(error);
                return RenderFill::Silence;
            }
        }
        // The renderer is constructed on the control thread. Moving one box here
        // performs no allocation, and the timeline may be armed only once.
        if self.renderer.is_none() {
            self.renderer = self.start.pop();
        }
        let Some(renderer) = &mut self.renderer else {
            return RenderFill::Silence;
        };
        for _ in 0..QUEUE_PACKETS {
            let Some(packet) = self.packets.pop() else {
                break;
            };
            if renderer.push(packet) == roomsync_core::jitter::InsertResult::WrongSession {
                self.shared.fail(LocalFault::InvalidPacket);
                return RenderFill::Silence;
            }
        }
        let report = match renderer.render(
            timing.estimated_dac_start_ns,
            timing.sample_rate,
            ClockModel::identity(),
            out,
        ) {
            Ok(report) => report,
            Err(_) => {
                self.shared.fail(LocalFault::RenderFailed);
                return RenderFill::Silence;
            }
        };
        self.shared
            .concealed
            .fetch_add(report.concealed_frames as u64, Ordering::Relaxed);
        self.shared
            .late
            .store(renderer.stats().late, Ordering::Relaxed);
        self.shared
            .too_far
            .store(renderer.stats().too_far, Ordering::Relaxed);
        let peak = out
            .iter()
            .flatten()
            .fold(0.0f32, |peak, value| peak.max(value.abs()));
        self.shared
            .peak
            .fetch_max(peak.to_bits(), Ordering::Relaxed);
        if report.waiting_frames == report.frames {
            RenderFill::Silence
        } else {
            RenderFill::Audio
        }
    }
}

pub struct LocalOutput {
    stream: Option<RenderStream>,
    shared: Arc<Shared>,
    packets: Arc<ArrayQueue<AudioPacket>>,
    start: Arc<ArrayQueue<Box<Renderer>>>,
    armed: bool,
}
impl LocalOutput {
    pub fn start(
        capture_endpoint_id: &str,
        output_endpoint_id: &str,
    ) -> Result<Self, LocalOutputError> {
        roomsync_windows::validate_loopback_route(capture_endpoint_id, output_endpoint_id)?;
        let shared = Arc::new(Shared::default());
        let packets = Arc::new(ArrayQueue::new(QUEUE_PACKETS));
        let start = Arc::new(ArrayQueue::new(1));
        let mut callback = Callback {
            probe: Probe::default(),
            shared: shared.clone(),
            packets: packets.clone(),
            start: start.clone(),
            renderer: None,
        };
        let stream = RenderStream::start(
            RenderConfig {
                endpoint_id: Some(output_endpoint_id.to_owned()),
                loopback_source_endpoint_id: Some(capture_endpoint_id.to_owned()),
                ..RenderConfig::default()
            },
            move |timing, out| callback.render(timing, out),
        )?;
        // Check resolved endpoint identity as well as the requested IDs.
        roomsync_windows::validate_loopback_route(capture_endpoint_id, &stream.endpoint.id)?;
        Ok(Self {
            stream: Some(stream),
            shared,
            packets,
            start,
            armed: false,
        })
    }
    pub fn check(&mut self) -> Result<(), LocalOutputError> {
        if let Some(error) = self.shared.fault() {
            return Err(LocalOutputError::Fault(error));
        }
        if self.stream.as_ref().is_none_or(RenderStream::is_finished) {
            // Join on the control thread to preserve the actual asynchronous
            // adapter error instead of replacing it with a generic stop message.
            if let Some(stream) = self.stream.take() {
                stream.stop()?;
            }
            return Err(LocalOutputError::Stopped);
        }
        Ok(())
    }
    pub fn budget(&self) -> Option<OutputBudget> {
        self.shared
            .ready
            .load(Ordering::Acquire)
            .then(|| OutputBudget {
                output_horizon_ns: self.shared.horizon.load(Ordering::Relaxed),
                render_block_ns: self.shared.block.load(Ordering::Relaxed),
            })
    }
    pub fn arm(&mut self, session_id: u64, epoch_host_ns: u64) -> Result<(), LocalOutputError> {
        self.check()?;
        if self.armed {
            return Err(LocalOutputError::AlreadyArmed);
        }
        if epoch_host_ns == 0 || self.budget().is_none() {
            return Err(LocalOutputError::InvalidTimeline);
        }
        let renderer = Box::new(
            Renderer::new(session_id, epoch_host_ns, ChannelMap::Stereo)
                .map_err(|_| LocalOutputError::InvalidTimeline)?,
        );
        self.start
            .push(renderer)
            .map_err(|_| LocalOutputError::AlreadyArmed)?;
        self.armed = true;
        Ok(())
    }
    pub fn push(&self, packet: AudioPacket) -> Result<(), LocalOutputError> {
        if !self.armed {
            return Err(LocalOutputError::InvalidTimeline);
        }
        self.packets
            .push(packet)
            .map_err(|_| LocalOutputError::QueueFull)
    }
    pub fn stats(&self) -> LocalOutputStats {
        LocalOutputStats {
            concealed_frames: self.shared.concealed.load(Ordering::Relaxed),
            late_packets: self.shared.late.load(Ordering::Relaxed),
            too_far_packets: self.shared.too_far.load(Ordering::Relaxed),
            peak: f32::from_bits(self.shared.peak.swap(0, Ordering::Relaxed)),
        }
    }
    pub fn stop(mut self) -> Result<(), LocalOutputError> {
        if let Some(stream) = self.stream.take() {
            stream.stop()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn timing(now: u64, horizon: u64) -> RenderTiming {
        RenderTiming {
            sample_rate: 48_000,
            estimated_dac_start_ns: now + horizon,
            clock_position: now * 48_000 / 1_000_000_000,
            clock_frequency: 48_000,
            clock_qpc_ns: now,
            callback_qpc_ns: now,
            queued_frames: 480,
            write_cursor_frames: 0,
            timestamp_estimated: true,
            discontinuity: false,
            clock_advancing: true,
        }
    }
    #[test]
    fn requires_advancing_half_second_and_keeps_measured_maximum() {
        let mut probe = Probe::default();
        for index in 0..50 {
            assert_eq!(
                probe.observe(
                    timing(
                        1_000_000_000 + index * 10_000_000,
                        if index == 10 { 25_000_000 } else { 20_000_000 }
                    ),
                    480
                ),
                Ok(None)
            );
        }
        assert_eq!(
            probe.observe(timing(1_500_000_000, 20_000_000), 480),
            Ok(Some(OutputBudget {
                output_horizon_ns: 25_000_000,
                render_block_ns: 10_000_000
            }))
        );
        assert_eq!(
            probe.observe(timing(1_510_000_000, 50_000_000), 480),
            Err(LocalFault::ChangedTiming)
        );
    }
    #[test]
    fn duplicate_or_invalid_clocks_never_publish_readiness() {
        let mut probe = Probe::default();
        for _ in 0..100 {
            assert_eq!(
                probe.observe(timing(1_000_000_000, 20_000_000), 480),
                Ok(None)
            );
        }
        let mut stale = timing(1_300_000_000, 20_000_000);
        stale.clock_qpc_ns = 1_000_000_000;
        assert_eq!(probe.observe(stale, 480), Err(LocalFault::InvalidTiming));
        let mut invalid = timing(1_400_000_000, 20_000_000);
        invalid.clock_frequency = 0;
        assert_eq!(probe.observe(invalid, 480), Err(LocalFault::InvalidTiming));
        assert_eq!(
            probe.observe(timing(1_400_000_000, 20_000_000), 4_801),
            Err(LocalFault::InvalidTiming)
        );
    }
    #[test]
    fn callback_is_silent_until_armed_then_uses_shared_epoch_and_packets() {
        let shared = Arc::new(Shared::default());
        let packets = Arc::new(ArrayQueue::new(QUEUE_PACKETS));
        let start = Arc::new(ArrayQueue::new(1));
        let mut callback = Callback {
            probe: Probe::default(),
            shared: shared.clone(),
            packets: packets.clone(),
            start: start.clone(),
            renderer: None,
        };
        let mut out = [[1.0; 2]; 480];
        for index in 0..60 {
            assert_eq!(
                callback.render(
                    timing(1_000_000_000 + index * 10_000_000, 20_000_000),
                    &mut out
                ),
                RenderFill::Silence
            );
        }
        assert!(shared.ready.load(Ordering::Acquire));
        let epoch = 2_000_000_000;
        start
            .push(Box::new(
                Renderer::new(9, epoch, ChannelMap::Stereo).unwrap(),
            ))
            .unwrap();
        for sequence in 0..8 {
            packets
                .push(
                    AudioPacket::new(
                        9,
                        sequence,
                        [8_192; roomsync_core::protocol::SAMPLES_PER_PACKET],
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(
            callback.render(timing(epoch - 30_000_000, 20_000_000), &mut out),
            RenderFill::Silence
        );
        assert!(out.iter().flatten().all(|value| *value == 0.0));
        assert_eq!(
            callback.render(timing(epoch - 20_000_000, 20_000_000), &mut out),
            RenderFill::Audio
        );
        assert!(out.iter().flatten().all(|value| *value == 0.25));
        assert_eq!(shared.concealed.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn wrong_session_fails_silent_and_callback_work_stays_bounded() {
        let shared = Arc::new(Shared::default());
        let packets = Arc::new(ArrayQueue::new(QUEUE_PACKETS));
        let start = Arc::new(ArrayQueue::new(1));
        let mut callback = Callback {
            probe: Probe::default(),
            shared: shared.clone(),
            packets: packets.clone(),
            start: start.clone(),
            renderer: None,
        };
        for index in 0..=50 {
            callback.render(
                timing(1_000_000_000 + index * 10_000_000, 20_000_000),
                &mut [[0.0; 2]; 480],
            );
        }
        start
            .push(Box::new(
                Renderer::new(9, 2_000_000_000, ChannelMap::Stereo).unwrap(),
            ))
            .unwrap();
        for sequence in 0..QUEUE_PACKETS {
            packets
                .push(
                    AudioPacket::new(
                        8,
                        sequence as u64,
                        [1; roomsync_core::protocol::SAMPLES_PER_PACKET],
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert!(
            packets
                .push(
                    AudioPacket::new(8, 129, [1; roomsync_core::protocol::SAMPLES_PER_PACKET])
                        .unwrap()
                )
                .is_err()
        );
        let mut out = [[1.0; 2]; 480];
        assert_eq!(
            callback.render(timing(1_980_000_000, 20_000_000), &mut out),
            RenderFill::Silence
        );
        assert_eq!(shared.fault(), Some(LocalFault::InvalidPacket));
        assert!(out.iter().flatten().all(|sample| *sample == 0.0));
    }

    #[test]
    fn live_virtual_capture_reaches_local_and_remote_on_the_identical_timeline() {
        use crate::{CapturePacketizer, budget::choose_lead};
        for (local_horizon, phone_horizon) in [(20_000_000, 100_000_000), (160_000_000, 30_000_000)]
        {
            let shared = Arc::new(Shared::default());
            let packets = Arc::new(ArrayQueue::new(QUEUE_PACKETS));
            let start = Arc::new(ArrayQueue::new(1));
            let mut local = Callback {
                probe: Probe::default(),
                shared: shared.clone(),
                packets: packets.clone(),
                start: start.clone(),
                renderer: None,
            };
            let mut out = [[0.0; 2]; 480];
            for index in 0..=60 {
                local.render(
                    timing(1_000_000_000 + index * 10_000_000, local_horizon),
                    &mut out,
                );
            }
            let local_budget = local.probe.frozen.unwrap();
            let lead = choose_lead(
                40_000_000,
                10_000_000,
                &[
                    local_budget,
                    OutputBudget {
                        output_horizon_ns: phone_horizon,
                        render_block_ns: 10_000_000,
                    },
                ],
            )
            .unwrap()
            .lead_ns;
            let epoch = 3_000_000_000;
            start
                .push(Box::new(
                    Renderer::new(9, epoch, ChannelMap::Stereo).unwrap(),
                ))
                .unwrap();
            let mut phone = Renderer::new(9, epoch, ChannelMap::Stereo).unwrap();
            let mut source = CapturePacketizer::new(9, epoch - lead).unwrap();
            let mut events = Vec::new();
            for block in 0..700_u64 {
                let stamp = 1_000_000_000 + block * 10_000_000;
                let frames = std::array::from_fn::<_, 480, _>(|frame| {
                    let phase = ((block * 480 + frame as u64) % 997) as f32 / 997.0;
                    [0.2 + phase * 0.3, -0.1 - phase * 0.2]
                });
                source
                    .consume(stamp, 1e9 / 48_000.0, &frames, |packet| {
                        events.push((stamp + 10_000_000, packet));
                        Ok(())
                    })
                    .unwrap();
            }
            let (mut next_local, mut next_phone) = (0, 0);
            let (mut local_pcm, mut phone_pcm) = (Vec::new(), Vec::new());
            let mut phone_concealed = 0;
            for block in 61..600_u64 {
                let now = 1_000_000_000 + block * 10_000_000;
                while next_local < events.len() && events[next_local].0 <= now {
                    packets.push(events[next_local].1.clone()).unwrap();
                    next_local += 1;
                }
                while next_phone < events.len() && events[next_phone].0 + 5_000_000 <= now {
                    phone.push(events[next_phone].1.clone());
                    next_phone += 1;
                }
                local.render(timing(now, local_horizon), &mut out);
                if (epoch + 1_000_000_000..epoch + 2_000_000_000).contains(&(now + local_horizon)) {
                    local_pcm.extend_from_slice(&out);
                }
                let report = phone
                    .render(
                        now + phone_horizon,
                        48_000,
                        ClockModel::identity(),
                        &mut out,
                    )
                    .unwrap();
                phone_concealed += report.concealed_frames;
                if (epoch + 1_000_000_000..epoch + 2_000_000_000).contains(&(now + phone_horizon)) {
                    phone_pcm.extend_from_slice(&out);
                }
            }
            assert_eq!(shared.fault(), None);
            assert_eq!(shared.concealed.load(Ordering::Relaxed), 0);
            assert_eq!(phone_concealed, 0);
            assert_eq!(local_pcm.len(), 48_000);
            assert_eq!(
                local_pcm, phone_pcm,
                "local={local_horizon} remote={phone_horizon} lead={lead}"
            );
        }
    }
}
