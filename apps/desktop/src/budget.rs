//! Shared capture-to-DAC scheduling budget. Every receiver must have its source
//! samples before its output writer is asked to fill a hardware queue.

pub const MAX_PLAYOUT_LEAD_NS: u64 = 300_000_000;
pub const NETWORK_CLOCK_GUARD_NS: u64 = 30_000_000;
const ROUND_UP_NS: u64 =
    roomsync_core::FRAMES_PER_PACKET as u64 * 1_000_000_000 / roomsync_core::SAMPLE_RATE as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputBudget {
    /// Measured maximum future presentation time of output frame zero.
    pub output_horizon_ns: u64,
    /// Duration of the entire output batch, not just its first frame.
    pub render_block_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeadBudget {
    pub lead_ns: u64,
    pub receiver_requirement_ns: u64,
    pub capture_allowance_ns: u64,
}

/// Keep 20 ms headroom inside the core's fixed 320 ms reorder window. The chosen
/// lead is one group-wide value, never an independent receiver epoch offset.
/// A group has up to eight remote receivers plus the optional local host output.
/// `preferred_ns` is a minimum preference, not permission to underrun a device.
/// Round only to a 2.5 ms audio packet, not an arbitrary 10 ms block: all actual
/// output-batch, capture and network/clock requirements are already included.
pub fn choose_lead(
    preferred_ns: u64,
    capture_allowance_ns: u64,
    receivers: &[OutputBudget],
) -> Result<LeadBudget, &'static str> {
    if !(40_000_000..=250_000_000).contains(&preferred_ns)
        || receivers.is_empty()
        || receivers.len() > 9
    {
        return Err("Invalid playback timing budget");
    }
    if capture_allowance_ns > 200_000_000 {
        return Err(
            "Windows capture buffering is outside the supported range. Choose a lower-latency capture output, then reconnect.",
        );
    }
    let mut maximum = 0;
    for receiver in receivers {
        if receiver.output_horizon_ns > 1_000_000_000
            || !(1..=100_000_000).contains(&receiver.render_block_ns)
        {
            return Err("Receiver reported an invalid output timing requirement");
        }
        maximum = maximum.max(receiver.output_horizon_ns + receiver.render_block_ns);
    }
    let required = maximum + capture_allowance_ns + NETWORK_CLOCK_GUARD_NS;
    let rounded = required.max(preferred_ns).div_ceil(ROUND_UP_NS) * ROUND_UP_NS;
    if rounded > MAX_PLAYOUT_LEAD_NS {
        return Err(
            "This output needs more buffering than RoomSync's safe 300 ms limit. Use the phone speaker or a lower-latency wired output, then reconnect.",
        );
    }
    Ok(LeadBudget {
        lead_ns: rounded,
        receiver_requirement_ns: maximum,
        capture_allowance_ns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn group_lead_covers_slowest_output_and_capture_availability() {
        let budget = choose_lead(
            100_000_000,
            10_000_000,
            &[
                OutputBudget {
                    output_horizon_ns: 40_000_000,
                    render_block_ns: 10_000_000,
                },
                OutputBudget {
                    output_horizon_ns: 200_000_000,
                    render_block_ns: 10_000_000,
                },
            ],
        )
        .unwrap();
        assert_eq!(budget.lead_ns, 250_000_000);
        assert_eq!(budget.receiver_requirement_ns, 210_000_000);
    }
    #[test]
    fn unsupported_horizons_fail_instead_of_clamping_or_expanding_buffer() {
        assert!(
            choose_lead(
                100_000_000,
                20_000_000,
                &[OutputBudget {
                    output_horizon_ns: 260_000_000,
                    render_block_ns: 10_000_000
                }]
            )
            .is_err()
        );
        assert!(choose_lead(100_000_000, 10_000_000, &[]).is_err());
        assert!(
            choose_lead(
                0,
                10_000_000,
                &[OutputBudget {
                    output_horizon_ns: 0,
                    render_block_ns: 10_000_000
                }]
            )
            .is_err()
        );
        assert!(
            choose_lead(
                100_000_000,
                u64::MAX,
                &[OutputBudget {
                    output_horizon_ns: 0,
                    render_block_ns: 10_000_000
                }]
            )
            .is_err()
        );
        assert!(
            choose_lead(
                100_000_000,
                10_000_000,
                &[OutputBudget {
                    output_horizon_ns: 0,
                    render_block_ns: 0
                }]
            )
            .is_err()
        );
        assert!(
            choose_lead(
                100_000_000,
                10_000_000,
                &[OutputBudget {
                    output_horizon_ns: u64::MAX,
                    render_block_ns: 10_000_000
                }]
            )
            .is_err()
        );
    }
    #[test]
    fn packet_rounding_reduces_only_unneeded_delay_not_required_headroom() {
        let output = [OutputBudget {
            output_horizon_ns: 17_200_000,
            render_block_ns: 10_000_000,
        }];
        let minimum = choose_lead(40_000_000, 9_300_000, &output).unwrap();
        assert_eq!(minimum.lead_ns, 67_500_000);
        assert!(
            minimum.lead_ns
                >= minimum.receiver_requirement_ns
                    + minimum.capture_allowance_ns
                    + NETWORK_CLOCK_GUARD_NS
        );
        let requested = choose_lead(100_000_000, 9_300_000, &output).unwrap();
        assert_eq!(
            requested.lead_ns, 100_000_000,
            "Explicit higher user preferences stay respected"
        );
        let slow = choose_lead(
            40_000_000,
            20_000_000,
            &[OutputBudget {
                output_horizon_ns: 200_000_000,
                render_block_ns: 10_000_000,
            }],
        )
        .unwrap();
        assert_eq!(
            slow.lead_ns, 260_000_000,
            "A lower default cannot underbudget slow hardware"
        );
    }
    #[test]
    fn local_host_is_included_with_up_to_eight_remote_outputs() {
        let mut outputs = vec![
            OutputBudget {
                output_horizon_ns: 20_000_000,
                render_block_ns: 10_000_000
            };
            8
        ];
        outputs.push(OutputBudget {
            output_horizon_ns: 150_000_000,
            render_block_ns: 10_000_000,
        });
        assert_eq!(
            choose_lead(40_000_000, 10_000_000, &outputs)
                .unwrap()
                .lead_ns,
            200_000_000
        );
        outputs.push(outputs[0]);
        assert!(choose_lead(40_000_000, 10_000_000, &outputs).is_err());
    }
}
