//! Allocation-free clock validation, independently exercised without Windows.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClockFault {
    InvalidTimestamp,
    Regressed,
    Stalled,
}

pub(crate) fn queue_discontinuity(clock_started: bool, padding: u32) -> bool {
    // Priming may drain before the first hardware observation. It is still
    // silence and must not be mistaken for a discontinuity in an armed timeline.
    clock_started && padding == 0
}

pub(crate) struct ClockContinuity {
    previous: Option<(u64, u64)>,
    last_advance_ns: u64,
    stale_limit_ns: u64,
}
impl ClockContinuity {
    pub(crate) fn new(started_ns: u64, engine_period_ns: u64) -> Self {
        Self {
            previous: None,
            last_advance_ns: started_ns,
            stale_limit_ns: engine_period_ns
                .saturating_mul(4)
                .clamp(100_000_000, 500_000_000),
        }
    }

    pub(crate) fn observe(
        &mut self,
        now_ns: u64,
        position: u64,
        clock_qpc_ns: u64,
    ) -> Result<bool, ClockFault> {
        // Correlated QPC is converted by Windows to 100 ns units. A 1 ms
        // tolerance allows rounding, but not a clock from a different epoch.
        if clock_qpc_ns == 0
            || clock_qpc_ns > now_ns.saturating_add(1_000_000)
            || now_ns.saturating_sub(clock_qpc_ns) > self.stale_limit_ns
        {
            return Err(ClockFault::InvalidTimestamp);
        }
        let advancing = if let Some((previous_position, previous_qpc)) = self.previous {
            if position < previous_position || clock_qpc_ns < previous_qpc {
                return Err(ClockFault::Regressed);
            }
            position > previous_position && clock_qpc_ns > previous_qpc
        } else {
            false
        };
        if now_ns < self.last_advance_ns {
            return Err(ClockFault::Regressed);
        }
        if advancing {
            self.last_advance_ns = now_ns;
        } else if now_ns - self.last_advance_ns > 500_000_000 {
            return Err(ClockFault::Stalled);
        }
        self.previous = Some((position, clock_qpc_ns));
        Ok(advancing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const START: u64 = 10_000_000_000;

    #[test]
    fn empty_startup_queue_is_not_an_armed_timeline_discontinuity() {
        assert!(!queue_discontinuity(false, 0));
        assert!(!queue_discontinuity(false, 480));
        assert!(!queue_discontinuity(true, 480));
        assert!(queue_discontinuity(true, 0));
    }

    #[test]
    fn initial_zero_and_duplicate_clock_are_not_advancing() {
        let mut clock = ClockContinuity::new(START, 10_000_000);
        assert_eq!(clock.observe(START, 0, START), Ok(false));
        assert_eq!(
            clock.observe(START + 10_000_000, 480, START + 10_000_000),
            Ok(true)
        );
        assert_eq!(
            clock.observe(START + 15_000_000, 480, START + 10_000_000),
            Ok(false)
        );
        assert_eq!(
            clock.observe(START + 20_000_000, 960, START + 20_000_000),
            Ok(true)
        );
    }

    #[test]
    fn fresh_qpc_cannot_hide_frozen_hardware() {
        let mut clock = ClockContinuity::new(START, 10_000_000);
        assert_eq!(clock.observe(START, 10, START), Ok(false));
        assert_eq!(
            clock.observe(START + 400_000_000, 10, START + 400_000_000),
            Ok(false)
        );
        assert_eq!(
            clock.observe(START + 500_000_001, 10, START + 500_000_001),
            Err(ClockFault::Stalled)
        );
    }

    #[test]
    fn invalid_regressing_stale_and_future_timestamps_fail_closed() {
        for (now, position, qpc, fault) in [
            (
                START + 10_000_000,
                9,
                START + 10_000_000,
                ClockFault::Regressed,
            ),
            (START + 10_000_000, 11, START - 1, ClockFault::Regressed),
            (START + 110_000_000, 11, START, ClockFault::InvalidTimestamp),
            (START + 10_000_000, 11, 0, ClockFault::InvalidTimestamp),
            (
                START + 10_000_000,
                11,
                START + 12_000_000,
                ClockFault::InvalidTimestamp,
            ),
        ] {
            let mut clock = ClockContinuity::new(START, 10_000_000);
            clock.observe(START, 10, START).unwrap();
            assert_eq!(clock.observe(now, position, qpc), Err(fault));
        }
    }

    #[test]
    fn large_engine_period_still_has_bounded_staleness() {
        let mut clock = ClockContinuity::new(START, u64::MAX);
        assert_eq!(clock.observe(START, 10, START), Ok(false));
        assert_eq!(
            clock.observe(START + 500_000_001, 11, START),
            Err(ClockFault::InvalidTimestamp)
        );
    }
}
