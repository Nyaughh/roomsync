use roomsync_core::clock::ClockModel;
use roomsync_core::clock::{ClockExchange, ClockSynchronizer};
use roomsync_core::protocol::{AudioPacket, SAMPLES_PER_PACKET, SESSION_PACKET_BYTES};
use roomsync_core::render::{ChannelMap, Renderer};

#[test]
fn bounded_random_input_never_panics() {
    let mut state = 0x42abcdef98765432_u64;
    let mut bytes = [0_u8; SESSION_PACKET_BYTES + 1];
    for _ in 0..20_000 {
        for byte in &mut bytes {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        let len = state as usize % bytes.len();
        let _ = AudioPacket::decode(&bytes[..len]);
    }
}

#[test]
fn every_one_bit_mutation_is_rejected_or_round_trips_canonically() {
    let packet = AudioPacket::new(9, 88, [17; SAMPLES_PER_PACKET]).unwrap();
    let mut original = [0; SESSION_PACKET_BYTES];
    packet.encode(&mut original);
    for byte in 0..SESSION_PACKET_BYTES {
        for bit in 0..8 {
            let mut mutated = original;
            mutated[byte] ^= 1 << bit;
            if let Ok(decoded) = AudioPacket::decode(&mutated) {
                let mut canonical = [0; SESSION_PACKET_BYTES];
                decoded.encode(&mut canonical);
                assert_eq!(canonical, mutated);
            }
        }
    }
}

#[test]
fn timestamp_extremes_and_oversized_callback_are_safe() {
    let values = [0, 1, u64::MAX / 2, u64::MAX - 1, u64::MAX];
    for &a in &values {
        for &b in &values {
            for &c in &values {
                for &d in &values {
                    let mut clock = ClockSynchronizer::new();
                    let _ = clock.observe(ClockExchange {
                        local_send_ns: a,
                        host_receive_ns: b,
                        host_send_ns: c,
                        local_receive_ns: d,
                    });
                }
            }
        }
    }
    let mut renderer = Renderer::new(1, u64::MAX, ChannelMap::Stereo).unwrap();
    let mut out = vec![[1.0; 2]; 8_193];
    assert!(
        renderer
            .render(u64::MAX, 48_000, ClockModel::identity(), &mut out)
            .is_err()
    );
    assert!(out.iter().flatten().all(|sample| *sample == 0.0));
}

#[test]
fn stale_clock_mapping_silences_receiver() {
    let mut sync = ClockSynchronizer::new();
    for i in 0..16 {
        let t = 1_000_000_000 + i * 250_000_000;
        sync.observe(ClockExchange {
            local_send_ns: t,
            host_receive_ns: t + 500_000,
            host_send_ns: t + 600_000,
            local_receive_ns: t + 1_100_000,
        })
        .unwrap();
    }
    let model = sync.model().unwrap();
    assert!(model.is_locked());
    let mut receiver = Renderer::new(1, 0, ChannelMap::Stereo).unwrap();
    let mut out = [[1.0; 2]; 120];
    let report = receiver
        .render(20_000_000_000, 48_000, model, &mut out)
        .unwrap();
    assert_eq!(report.waiting_frames, 120);
    assert!(out.iter().flatten().all(|sample| *sample == 0.0));
}

#[test]
fn benign_rtt_steps_do_not_erase_established_clock_lock() {
    for (before, after) in [(500_000, 5_000_000), (5_000_000, 500_000)] {
        let mut sync = ClockSynchronizer::new();
        for i in 0..128 {
            let t = 1_000_000_000 + i * 250_000_000;
            let delay = if i < 64 { before } else { after };
            let model = sync
                .observe(ClockExchange {
                    local_send_ns: t,
                    host_receive_ns: t + delay,
                    host_send_ns: t + delay + 100_000,
                    local_receive_ns: t + 2 * delay + 100_000,
                })
                .unwrap();
            if i >= 8 {
                assert!(model.is_locked(), "lost lock at observation {i}");
            }
        }
    }
}

#[test]
fn downsampling_requires_a_real_antialiasing_resampler() {
    let mut receiver = Renderer::new(1, 0, ChannelMap::Stereo).unwrap();
    let mut out = [[1.0; 2]; 120];
    for rate in [8_000, 16_000, 44_100] {
        assert_eq!(
            receiver.render(0, rate, ClockModel::identity(), &mut out),
            Err(roomsync_core::render::RenderError::UnsupportedSampleRate)
        );
        assert!(out.iter().flatten().all(|sample| *sample == 0.0));
    }
}
