//! Deterministic virtual clocks/network, deliberately separate from hardware tests.
use crate::clock::{ClockExchange, ClockSynchronizer};
use crate::protocol::{AudioPacket, FRAMES_PER_PACKET, SAMPLE_RATE, SAMPLES_PER_PACKET};
use crate::render::{ChannelMap, Renderer};

#[derive(Clone, Copy, Debug)]
pub struct SimulationConfig {
    pub seconds: u32,
    pub loss_per_mille: u32,
    pub burst_delay_ms: u32,
    pub seed: u64,
}
impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            seconds: 30,
            loss_per_mille: 20,
            burst_delay_ms: 45,
            seed: 0x5eed,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct DeviceResult {
    pub drift_ppm: f64,
    pub dac_drift_ppm: f64,
    pub estimated_drift_ppm: f64,
    pub max_clock_error_us: f64,
    pub max_timeline_error_frames: f64,
    pub rendered_frames: u64,
    pub concealed_frames: u64,
    pub lost_packets: u64,
    pub duplicate_packets: u64,
    pub late_packets: u64,
    pub reordered_packets: u64,
}
#[derive(Clone, Debug)]
pub struct SimulationResult {
    pub devices: Vec<DeviceResult>,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}
struct Event {
    host_arrival: u64,
    sequence: u64,
}

/// Rejects large user-provided runs to keep memory/time bounded in the CLI.
pub fn run(config: SimulationConfig) -> Result<SimulationResult, &'static str> {
    if config.seconds == 0
        || config.seconds > 300
        || config.loss_per_mille > 500
        || config.burst_delay_ms > 1_000
    {
        return Err("simulation requires 1..=300 seconds, <=50% loss, <=1000 ms extra delay");
    }
    let mut results = Vec::new();
    for (device, drift_ppm) in [-180.0, 0.0, 135.0].into_iter().enumerate() {
        let rate = 1.0 + drift_ppm / 1e6;
        let dac_drift_ppm = [-500.0, 220.0, 500.0][device];
        let dac_rate = 1.0 + dac_drift_ppm / 1e6;
        let offset = 2_000_000_000.0 + device as f64 * 100_000_000.0;
        let local = |host: f64| ((host - offset) / rate).round() as u64;
        let host = |local: u64| local as f64 * rate + offset;
        let epoch = 20_000_000_000_u64;
        let mut rng =
            Rng((config.seed ^ (device as u64 + 1).wrapping_mul(0x123456789abcdef)).max(1));
        let mut sync = ClockSynchronizer::new();
        for i in 0..64 {
            let start = 3_000_000_000.0 + i as f64 * 250_000_000.0;
            let outbound = 500_000.0 + (rng.next() % 50_001) as f64;
            let inbound = 500_000.0 + (rng.next() % 50_001) as f64;
            sync.observe(ClockExchange {
                local_send_ns: local(start),
                host_receive_ns: (start + outbound) as u64,
                host_send_ns: (start + outbound + 100_000.0) as u64,
                local_receive_ns: local(start + outbound + 100_000.0 + inbound),
            })
            .map_err(|_| "clock training failed")?;
        }
        let mut model = sync.model().ok_or("missing clock model")?;
        let mut renderer = Renderer::new(42, epoch, ChannelMap::Stereo)
            .map_err(|_| "renderer construction failed")?;
        let packets = config.seconds as u64 * SAMPLE_RATE as u64 / FRAMES_PER_PACKET as u64 + 256;
        let mut events = Vec::with_capacity(packets as usize);
        let mut result = DeviceResult {
            drift_ppm,
            dac_drift_ppm,
            ..DeviceResult::default()
        };
        for sequence in 0..packets {
            if rng.next() % 1_000 < config.loss_per_mille as u64 {
                result.lost_packets += 1;
                continue;
            }
            let delay = 500_000 + rng.next() % 7_500_001;
            let burst = if sequence % 199 < 4 {
                config.burst_delay_ms as u64 * 1_000_000
            } else {
                0
            };
            let arrival = epoch - 80_000_000 + sequence * 2_500_000 + delay + burst;
            events.push(Event {
                host_arrival: arrival,
                sequence,
            });
            if sequence % 97 == 0 {
                events.push(Event {
                    host_arrival: arrival + 700_000,
                    sequence,
                });
            }
        }
        events.sort_unstable_by_key(|e| (e.host_arrival, e.sequence));
        let mut next_event = 0;
        let mut highest_received = 0;
        let blocks = config.seconds as u64 * 400;
        let local_epoch = local(epoch as f64);
        let mut out = [[0.0; 2]; FRAMES_PER_PACKET];
        for block in 0..blocks {
            let local_dac = local_epoch + (block as f64 * 2_500_000.0 / dac_rate).round() as u64;
            let host_dac = host(local_dac);
            while next_event < events.len() && events[next_event].host_arrival as f64 <= host_dac {
                let sequence = events[next_event].sequence;
                if sequence < highest_received {
                    result.reordered_packets += 1;
                }
                highest_received = highest_received.max(sequence);
                let mut samples = [0; SAMPLES_PER_PACKET];
                for (i, frame) in samples.chunks_exact_mut(2).enumerate() {
                    let phase = (sequence * FRAMES_PER_PACKET as u64 + i as u64) as f64
                        * std::f64::consts::TAU
                        * 440.0
                        / SAMPLE_RATE as f64;
                    frame[0] = (phase.sin() * 16_000.0) as i16;
                    frame[1] = (phase.cos() * 12_000.0) as i16;
                }
                renderer.push(
                    AudioPacket::new(42, sequence, samples)
                        .map_err(|_| "packet construction failed")?,
                );
                next_event += 1;
            }
            if block % 100 == 0 {
                let start = host_dac - 10_000_000.0;
                let outbound = 500_000.0 + (rng.next() % 50_001) as f64;
                let inbound = 500_000.0 + (rng.next() % 50_001) as f64;
                model = sync
                    .observe(ClockExchange {
                        local_send_ns: local(start),
                        host_receive_ns: (start + outbound) as u64,
                        host_send_ns: (start + outbound + 100_000.0) as u64,
                        local_receive_ns: local(start + outbound + 100_000.0 + inbound),
                    })
                    .map_err(|_| "live synchronization failed")?;
            }
            result.max_clock_error_us = result.max_clock_error_us.max(
                (model.host_elapsed_ns(local_dac, epoch) - (host_dac - epoch as f64)).abs()
                    / 1_000.0,
            );
            let report = renderer
                .render(local_dac, SAMPLE_RATE, model, &mut out)
                .map_err(|_| "render failed")?;
            if out
                .iter()
                .flatten()
                .any(|sample| !sample.is_finite() || sample.abs() > 1.0)
            {
                return Err("invalid rendered sample");
            }
            result.rendered_frames += report.frames as u64;
            result.concealed_frames += report.concealed_frames as u64;
            let expected_end = (host_dac - epoch as f64) * SAMPLE_RATE as f64 / 1e9
                + FRAMES_PER_PACKET as f64 * rate / dac_rate;
            result.max_timeline_error_frames = result
                .max_timeline_error_frames
                .max((renderer.source_cursor().unwrap_or_default() - expected_end).abs());
        }
        result.estimated_drift_ppm = model.drift_ppm();
        result.duplicate_packets = renderer.stats().duplicates;
        result.late_packets = renderer.stats().late;
        results.push(result);
    }
    Ok(SimulationResult { devices: results })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loss_reorder_and_drift_keep_virtual_timelines_bounded() {
        let r = run(SimulationConfig::default()).unwrap();
        for d in r.devices {
            assert!((d.estimated_drift_ppm - d.drift_ppm).abs() < 3.0, "{d:?}");
            assert!(d.max_clock_error_us < 100.0, "{d:?}");
            assert!(d.max_timeline_error_frames < 16.0, "{d:?}");
            assert!(
                d.concealed_frames > 0 && d.concealed_frames < d.rendered_frames / 10,
                "{d:?}"
            );
            assert!(d.reordered_packets > 0 && d.duplicate_packets > 0, "{d:?}");
        }
    }
    #[test]
    fn late_bursts_remain_bounded_without_panics() {
        let r = run(SimulationConfig {
            seconds: 3,
            loss_per_mille: 0,
            burst_delay_ms: 160,
            seed: 99,
        })
        .unwrap();
        assert!(
            r.devices
                .iter()
                .all(|d| d.late_packets > 0 && d.concealed_frames > 0)
        );
    }
    #[test]
    fn clean_network_does_not_conceal_full_packets() {
        let r = run(SimulationConfig {
            seconds: 3,
            loss_per_mille: 0,
            burst_delay_ms: 0,
            seed: 88,
        })
        .unwrap();
        // Small fitted offset can put first sample just before the epoch; no entire gap.
        assert!(
            r.devices.iter().all(|d| d.concealed_frames < 4),
            "{:?}",
            r.devices
        );
    }
}
