//! These regressions model capture availability and output queueing, not preloaded
//! audio. They reproduce the silent legal-output configuration missed by the
//! original smoke test, then test one shared calibrated budget for mixed devices.
use roomsync_core::{
    clock::ClockModel,
    render::{ChannelMap, Renderer},
};
use roomsync_desktop::{
    CapturePacketizer,
    budget::{OutputBudget, choose_lead},
};

#[derive(Debug)]
struct ResultStats {
    frames: usize,
    concealed: usize,
    audible: usize,
    late: u64,
    too_far: u64,
    realigned: usize,
    same_timeline_samples: Vec<[f32; 2]>,
}

fn live_pipeline(lead_ms: u64, horizon_ms: u64, network_ms: u64) -> ResultStats {
    let epoch = 5_000_000_000_u64;
    let mut packetizer = CapturePacketizer::new(1, epoch - lead_ms * 1_000_000).unwrap();
    let mut events = Vec::new();
    for block in 0..1_000_u64 {
        let timestamp = block * 10_000_000;
        // A block cannot be sent until all its samples exist. This differs from
        // preloading synthetic PCM seconds before the actual capture time.
        let samples: [[f32; 2]; 480] = std::array::from_fn(|index| {
            let phase = ((block * 480 + index as u64) % 997) as f32 / 997.0;
            [0.2 + phase * 0.3, -0.1 - phase * 0.2]
        });
        packetizer
            .consume(timestamp, 1e9 / 48_000.0, &samples, |packet| {
                let mut encoded = [0; roomsync_core::SESSION_PACKET_BYTES];
                packet.encode(&mut encoded);
                let decoded = roomsync_core::AudioPacket::decode(&encoded).unwrap();
                events.push((timestamp + (10 + network_ms) * 1_000_000, decoded));
                Ok(())
            })
            .unwrap();
    }
    let mut renderer = Renderer::new(1, epoch, ChannelMap::Stereo).unwrap();
    let mut output = [[0.0; 2]; 480];
    let (mut next, mut concealed, mut frames, mut audible, mut realigned) = (0, 0, 0, 0, 0);
    let mut same_timeline_samples = Vec::new();
    for block in 0..900_u64 {
        let now = block * 10_000_000;
        while next < events.len() && events[next].0 <= now {
            renderer.push(events[next].1.clone());
            next += 1;
        }
        let report = renderer
            .render(
                now + horizon_ms * 1_000_000,
                48_000,
                ClockModel::identity(),
                &mut output,
            )
            .unwrap();
        let dac_start = now + horizon_ms * 1_000_000;
        if (epoch + 1_000_000_000..epoch + 2_000_000_000).contains(&dac_start) {
            same_timeline_samples.extend_from_slice(&output);
        }
        if now >= epoch {
            frames += output.len();
            concealed += report.concealed_frames;
            audible += output
                .iter()
                .filter(|f| f[0].abs() > 0.01 && f[1] < -0.01)
                .count();
            realigned += usize::from(report.realigned);
        }
    }
    ResultStats {
        frames,
        concealed,
        audible,
        late: renderer.stats().late,
        too_far: renderer.stats().too_far,
        realigned,
        same_timeline_samples,
    }
}

#[test]
fn reproduces_old_all_concealed_failure_without_transport_loss() {
    let stats = live_pipeline(100, 150, 5);
    assert_eq!(stats.concealed, stats.frames, "{stats:?}");
    assert_eq!(stats.audible, 0, "{stats:?}");
    assert!(stats.late > 1_000, "{stats:?}");
    assert_eq!(stats.realigned, 0, "{stats:?}");
}

#[test]
fn calibrated_common_lead_renders_mixed_hardware_horizons() {
    for slowest_ms in [40_u64, 90, 150, 200, 240, 250] {
        let budget = choose_lead(
            100_000_000,
            10_000_000,
            &[
                OutputBudget {
                    output_horizon_ns: 0,
                    render_block_ns: 10_000_000,
                },
                OutputBudget {
                    output_horizon_ns: slowest_ms * 1_000_000,
                    render_block_ns: 10_000_000,
                },
            ],
        )
        .unwrap();
        let mut baseline = None;
        for horizon_ms in [0, slowest_ms] {
            let stats = live_pipeline(budget.lead_ns / 1_000_000, horizon_ms, 5);
            assert_eq!(
                stats.concealed, 0,
                "budget={budget:?} horizon={horizon_ms} {stats:?}"
            );
            assert_eq!(stats.audible, stats.frames, "{stats:?}");
            assert_eq!(stats.too_far, 0, "{stats:?}");
            assert_eq!(stats.realigned, 0, "{stats:?}");
            if let Some(expected) = &baseline {
                assert_eq!(
                    &stats.same_timeline_samples, expected,
                    "different output queues must receive identical PCM at the same DAC timeline"
                );
            } else {
                baseline = Some(stats.same_timeline_samples);
            }
        }
    }
}

#[test]
fn render_batch_tail_and_network_delay_are_included() {
    let budget = choose_lead(
        100_000_000,
        10_000_000,
        &[OutputBudget {
            output_horizon_ns: 200_000_000,
            render_block_ns: 10_000_000,
        }],
    )
    .unwrap();
    for network_ms in [0, 5, 20] {
        let stats = live_pipeline(budget.lead_ns / 1_000_000, 200, network_ms);
        assert_eq!(stats.concealed, 0, "network={network_ms} {stats:?}");
    }
}
