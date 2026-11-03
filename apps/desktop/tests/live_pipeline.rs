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
    max_cursor_error_frames: f64,
    same_timeline_samples: Vec<[f32; 2]>,
}

fn live_pipeline(lead_ms: u64, horizon_ms: u64, network_ms: u64) -> ResultStats {
    live_pipeline_ns(
        lead_ms * 1_000_000,
        horizon_ms * 1_000_000,
        network_ms * 1_000_000,
    )
}

fn live_pipeline_ns(lead_ns: u64, horizon_ns: u64, network_ns: u64) -> ResultStats {
    live_pipeline_delays(lead_ns, horizon_ns, |_| network_ns)
}

fn live_pipeline_delays(
    lead_ns: u64,
    horizon_ns: u64,
    delivery_delay_ns: impl Fn(u64) -> u64,
) -> ResultStats {
    live_pipeline_phase(lead_ns, horizon_ns, delivery_delay_ns, 0)
}

fn live_pipeline_phase(
    lead_ns: u64,
    horizon_ns: u64,
    delivery_delay_ns: impl Fn(u64) -> u64,
    render_phase_ns: u64,
) -> ResultStats {
    let epoch = 5_000_000_000_u64;
    let mut packetizer = CapturePacketizer::new(1, epoch - lead_ns).unwrap();
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
                events.push((timestamp + 10_000_000 + delivery_delay_ns(block), decoded));
                Ok(())
            })
            .unwrap();
    }
    // UDP arrivals can reorder. Do not model delayed packets as a reliable
    // stream that artificially blocks all newer packets behind one late packet.
    events.sort_by_key(|event| event.0);
    let mut renderer = Renderer::new(1, epoch, ChannelMap::Stereo).unwrap();
    let mut output = [[0.0; 2]; 480];
    let (mut next, mut concealed, mut frames, mut audible, mut realigned) = (0, 0, 0, 0, 0);
    let mut same_timeline_samples = Vec::new();
    let mut max_cursor_error_frames = 0.0_f64;
    for block in 0..900_u64 {
        let now = block * 10_000_000 + render_phase_ns;
        while next < events.len() && events[next].0 <= now {
            renderer.push(events[next].1.clone());
            next += 1;
        }
        let report = renderer
            .render(
                now + horizon_ns,
                48_000,
                ClockModel::identity(),
                &mut output,
            )
            .unwrap();
        let dac_start = now + horizon_ns;
        if (epoch + 1_000_000_000..epoch + 2_000_000_000).contains(&dac_start) {
            same_timeline_samples.extend_from_slice(&output);
        }
        let expected_cursor =
            (dac_start as f64 - epoch as f64) * 48_000.0 / 1e9 + output.len() as f64;
        max_cursor_error_frames = max_cursor_error_frames
            .max((renderer.source_cursor().unwrap() - expected_cursor).abs());
        if report.waiting_frames < report.frames {
            // Include the first nonnegative source frame already queued ahead of
            // wall time, not only the later callbacks after now >= epoch.
            frames += report.frames - report.waiting_frames;
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
        max_cursor_error_frames,
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
    for network_ms in [0, 5, 20, 30] {
        let stats = live_pipeline(budget.lead_ns / 1_000_000, 200, network_ms);
        assert_eq!(stats.concealed, 0, "network={network_ms} {stats:?}");
    }
}

#[test]
fn lower_floor_and_packet_rounding_keep_the_actual_capture_to_dac_reserve() {
    let budget = choose_lead(
        40_000_000,
        10_000_000,
        &[OutputBudget {
            output_horizon_ns: 17_200_000,
            render_block_ns: 10_000_000,
        }],
    )
    .unwrap();
    assert_eq!(budget.lead_ns, 67_500_000);
    for network_ns in [0, 5_000_000, 20_000_000, 30_000_000] {
        let stats = live_pipeline_ns(budget.lead_ns, 17_200_000, network_ns);
        assert_eq!(stats.concealed, 0, "network={network_ns} {stats:?}");
        assert_eq!(stats.audible, stats.frames, "{stats:?}");
        assert_eq!(stats.realigned, 0, "{stats:?}");
        assert_eq!(stats.too_far, 0, "{stats:?}");
    }
}

#[test]
fn delayed_delivery_can_cut_one_receiver_without_losing_the_shared_timeline() {
    // Model a bounded delivery/processing burst, not a claim about whether the
    // delay occurred in Windows, Wi-Fi, the kernel socket or Android scheduling.
    // Four source blocks per second arrive 170 ms late; the rest arrive in 5 ms.
    let burst = |block: u64| {
        if block % 100 < 4 {
            170_000_000
        } else {
            5_000_000
        }
    };
    let affected = live_pipeline_delays(130_000_000, 65_900_000, burst);
    assert!(affected.concealed > 0 && affected.late > 0, "{affected:?}");
    assert!(affected.concealed < affected.frames / 4, "{affected:?}");
    assert_eq!(
        affected.realigned, 0,
        "Missing audio must not shift the clock"
    );
    assert_eq!(affected.too_far, 0);
    assert!(affected.max_cursor_error_frames < 0.001);

    // One common 250 ms lead covers this particular delay profile. Neither
    // receiver receives a private epoch offset, and their DAC samples agree.
    let normal = live_pipeline_delays(250_000_000, 65_900_000, |_| 5_000_000);
    let protected = live_pipeline_delays(250_000_000, 65_900_000, burst);
    assert_eq!(protected.concealed, 0, "{protected:?}");
    assert_eq!(protected.late, 0, "{protected:?}");
    assert_eq!(protected.realigned, 0);
    assert_eq!(protected.too_far, 0);
    assert_eq!(
        protected.same_timeline_samples,
        normal.same_timeline_samples
    );

    // More buffering is not an unlimited cure. The user-reported worst late
    // lag is larger than the added 120 ms, so do not promise all cuts disappear.
    let extreme = live_pipeline_delays(250_000_000, 65_900_000, |block| {
        if block % 100 < 4 {
            290_000_000
        } else {
            5_000_000
        }
    });
    assert!(extreme.concealed > 0 && extreme.late > 0, "{extreme:?}");
    assert_eq!(extreme.realigned, 0);
    assert_eq!(extreme.too_far, 0);
}

#[test]
fn delayed_delivery_headroom_covers_capture_and_render_phase_not_only_aligned_callbacks() {
    let burst = |block: u64| {
        if block % 100 < 4 {
            170_000_000
        } else {
            5_000_000
        }
    };
    let mut saw_uncovered_phase = false;
    for phase_ns in [0, 1_000_000, 2_500_000, 5_000_000, 7_500_000, 9_900_000] {
        let trial = live_pipeline_phase(250_000_000, 65_900_000, burst, phase_ns);
        saw_uncovered_phase |= trial.concealed > 0;
        assert_eq!(trial.realigned, 0);
        // 65.9 ms output horizon + 10 ms whole render batch + 10 ms capture
        // availability + 170 ms modeled delivery delay, rounded to 2.5 ms.
        // This is a test bound, not a new preference or an automatic network fit.
        let bounded = live_pipeline_phase(257_500_000, 65_900_000, burst, phase_ns);
        assert_eq!(bounded.concealed, 0, "phase {phase_ns}: {bounded:?}");
        assert_eq!(bounded.late, 0);
        assert_eq!(bounded.too_far, 0);
        assert_eq!(bounded.realigned, 0);
        assert_eq!(bounded.audible, bounded.frames);
        assert!(bounded.max_cursor_error_frames < 0.001);
    }
    assert!(
        saw_uncovered_phase,
        "Do not turn the aligned250 ms example into a universal guarantee"
    );
}
