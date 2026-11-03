//! Four-timestamp synchronization with bounded history and low-RTT weighted
//! regression. Symmetric paths are assumed; path asymmetry is not measurable here.

const HISTORY: usize = 64;
const MAX_RTT_NS: u64 = 100_000_000;
const RTT_WINDOW_NS: f64 = 2_000_000.0;
const MAX_DRIFT: f64 = 500.0e-6;
const MIN_SPAN_NS: f64 = 1_000_000_000.0;

#[derive(Clone, Copy, Debug)]
pub struct ClockExchange {
    pub local_send_ns: u64,
    pub host_receive_ns: u64,
    pub host_send_ns: u64,
    pub local_receive_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockError {
    InvalidOrder,
    ExcessiveRtt,
    Stale,
    ImplausibleDrift,
    Discontinuity,
}

#[derive(Clone, Copy, Debug)]
struct Observation {
    local_ns: u64,
    offset_ns: f64,
    rtt_ns: f64,
}

/// Immutable mapping snapshot safe to pass to the audio thread by value.
#[derive(Clone, Copy, Debug)]
pub struct ClockModel {
    anchor_local_ns: u64,
    offset_ns: f64,
    drift: f64,
    uncertainty_ns: f64,
    locked: bool,
    valid_until_local_ns: u64,
}

impl ClockModel {
    /// For same-clock local rendering or deterministic tests, not remote calibration.
    pub fn identity() -> Self {
        Self {
            anchor_local_ns: 0,
            offset_ns: 0.0,
            drift: 0.0,
            uncertainty_ns: 0.0,
            locked: true,
            valid_until_local_ns: u64::MAX,
        }
    }
    pub fn is_locked(&self) -> bool {
        self.locked
    }
    /// Remote mappings expire after five seconds without an accepted exchange.
    pub fn is_fresh_at(&self, local_ns: u64) -> bool {
        local_ns <= self.valid_until_local_ns
    }
    pub fn drift_ppm(&self) -> f64 {
        self.drift * 1_000_000.0
    }
    pub fn host_rate_per_local_tick(&self) -> f64 {
        1.0 + self.drift
    }
    /// Conservative statistical indicator, not a guarantee against asymmetric routing.
    pub fn uncertainty_ns(&self) -> f64 {
        self.uncertainty_ns
    }
    pub fn offset_at(&self, local_ns: u64) -> f64 {
        self.offset_ns + signed_delta(local_ns, self.anchor_local_ns) * self.drift
    }
    /// Difference to a host epoch calculated without losing large timestamp precision.
    pub fn host_elapsed_ns(&self, local_ns: u64, host_epoch_ns: u64) -> f64 {
        signed_delta(local_ns, host_epoch_ns) + self.offset_at(local_ns)
    }
}

fn signed_delta(a: u64, b: u64) -> f64 {
    (i128::from(a) - i128::from(b)) as f64
}

#[derive(Debug)]
pub struct ClockSynchronizer {
    samples: [Option<Observation>; HISTORY],
    cursor: usize,
    count: usize,
    last_receive_ns: Option<u64>,
    model: Option<ClockModel>,
}

impl Default for ClockSynchronizer {
    fn default() -> Self {
        Self::new()
    }
}
impl ClockSynchronizer {
    pub fn new() -> Self {
        Self {
            samples: [None; HISTORY],
            cursor: 0,
            count: 0,
            last_receive_ns: None,
            model: None,
        }
    }
    pub fn model(&self) -> Option<ClockModel> {
        self.model
    }
    /// Use a fresh instance after a suspend/resume or known monotonic-clock change.
    /// Bad samples never alter the current mapping or accepted history.
    pub fn observe(&mut self, e: ClockExchange) -> Result<ClockModel, ClockError> {
        let elapsed = e
            .local_receive_ns
            .checked_sub(e.local_send_ns)
            .ok_or(ClockError::InvalidOrder)?;
        let processing = e
            .host_send_ns
            .checked_sub(e.host_receive_ns)
            .ok_or(ClockError::InvalidOrder)?;
        let rtt = elapsed
            .checked_sub(processing)
            .ok_or(ClockError::InvalidOrder)?;
        if rtt > MAX_RTT_NS {
            return Err(ClockError::ExcessiveRtt);
        }
        if self
            .last_receive_ns
            .is_some_and(|last| e.local_receive_ns <= last)
        {
            return Err(ClockError::Stale);
        }
        let local_ns = e.local_send_ns + elapsed / 2;
        let offset_ns = ((i128::from(e.host_receive_ns) - i128::from(e.local_send_ns))
            + (i128::from(e.host_send_ns) - i128::from(e.local_receive_ns)))
            as f64
            * 0.5;
        if let Some(model) = self.model {
            // A large step after lock is not slewed into live audio silently.
            if model.locked
                && (offset_ns - model.offset_at(local_ns)).abs() > 20_000_000.0 + rtt as f64 / 2.0
            {
                return Err(ClockError::Discontinuity);
            }
        }
        let old = self.samples[self.cursor];
        self.samples[self.cursor] = Some(Observation {
            local_ns,
            offset_ns,
            rtt_ns: rtt as f64,
        });
        let new_count = (self.count + 1).min(HISTORY);
        let mut candidate = self.fit(local_ns, new_count);
        if !candidate.locked {
            if let Some(previous) = self.model.filter(|m| m.locked) {
                // Bounded holdover: an underconstrained refit cannot refresh expiry.
                candidate = previous;
            }
        }
        if candidate.drift.abs() > MAX_DRIFT {
            self.samples[self.cursor] = old;
            return Err(ClockError::ImplausibleDrift);
        }
        self.cursor = (self.cursor + 1) % HISTORY;
        self.count = new_count;
        self.last_receive_ns = Some(e.local_receive_ns);
        self.model = Some(candidate);
        Ok(candidate)
    }
    fn fit(&self, anchor: u64, count: usize) -> ClockModel {
        let min_rtt = self
            .samples
            .iter()
            .flatten()
            .map(|o| o.rtt_ns)
            .fold(f64::INFINITY, f64::min);
        // Expand the low-delay selection enough to retain a fit spanning one
        // second. A benign route/RTT step must not abruptly erase clock lock.
        let mut rtts = [f64::INFINITY; HISTORY];
        for (i, o) in self.samples.iter().flatten().enumerate() {
            rtts[i] = o.rtt_ns;
        }
        rtts[..count].sort_unstable_by(f64::total_cmp);
        let mut threshold = min_rtt + RTT_WINDOW_NS;
        for rtt in rtts.iter().take(count) {
            threshold = threshold.max(*rtt);
            let mut oldest = u64::MAX;
            let mut newest = 0;
            let mut selected = 0;
            for o in self
                .samples
                .iter()
                .flatten()
                .filter(|o| o.rtt_ns <= threshold)
            {
                oldest = oldest.min(o.local_ns);
                newest = newest.max(o.local_ns);
                selected += 1;
            }
            if selected >= 8
                && newest.saturating_sub(oldest) as f64 >= MIN_SPAN_NS
                && newest >= anchor.saturating_sub(2_000_000_000)
            {
                break;
            }
        }
        let mut weight_sum = 0.0;
        let mut x_sum = 0.0;
        let mut y_sum = 0.0;
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut used = 0;
        let mut newest_selected = 0;
        for o in self
            .samples
            .iter()
            .flatten()
            .filter(|o| o.rtt_ns <= threshold)
        {
            let weight = 1.0 / (1.0 + (o.rtt_ns - min_rtt) / 100_000.0);
            let x = signed_delta(o.local_ns, anchor);
            weight_sum += weight;
            x_sum += weight * x;
            y_sum += weight * o.offset_ns;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            used += 1;
            newest_selected = newest_selected.max(o.local_ns);
        }
        let mean_x = x_sum / weight_sum;
        let mean_y = y_sum / weight_sum;
        let locked = count >= 8 && used >= 8 && max_x - min_x >= MIN_SPAN_NS;
        let mut covariance = 0.0;
        let mut variance = 0.0;
        for o in self
            .samples
            .iter()
            .flatten()
            .filter(|o| o.rtt_ns <= threshold)
        {
            let weight = 1.0 / (1.0 + (o.rtt_ns - min_rtt) / 100_000.0);
            let x = signed_delta(o.local_ns, anchor) - mean_x;
            covariance += weight * x * (o.offset_ns - mean_y);
            variance += weight * x * x;
        }
        let drift = if locked && variance > 0.0 {
            covariance / variance
        } else {
            0.0
        };
        let offset_ns = mean_y - drift * mean_x;
        let mut residual: f64 = 0.0;
        for o in self
            .samples
            .iter()
            .flatten()
            .filter(|o| o.rtt_ns <= threshold)
        {
            residual = residual
                .max((o.offset_ns - offset_ns - signed_delta(o.local_ns, anchor) * drift).abs());
        }
        ClockModel {
            anchor_local_ns: anchor,
            offset_ns,
            drift,
            uncertainty_ns: min_rtt / 2.0 + residual,
            locked,
            valid_until_local_ns: newest_selected.saturating_add(5_000_000_000),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn exchange(t: u64, offset: i64, drift_ppm: f64, delay: u64) -> ClockExchange {
        let map = |v: u64| (v as f64 * (1.0 + drift_ppm / 1e6) + offset as f64).round() as u64;
        ClockExchange {
            local_send_ns: t,
            host_receive_ns: map(t + delay),
            host_send_ns: map(t + delay + 100_000),
            local_receive_ns: t + 2 * delay + 100_000,
        }
    }
    #[test]
    fn estimates_drift_and_large_clock_offset() {
        let mut sync = ClockSynchronizer::new();
        for i in 0..64 {
            sync.observe(exchange(
                10_000_000_000 + i * 250_000_000,
                7_000_000_000,
                125.0,
                500_000,
            ))
            .unwrap();
        }
        let m = sync.model().unwrap();
        assert!(m.is_locked());
        assert!((m.drift_ppm() - 125.0).abs() < 0.1);
        assert!((m.offset_at(30_000_000_000) - 7_003_750_000.0).abs() < 20.0);
    }
    #[test]
    fn rejects_stale_invalid_and_discontinuous() {
        let mut sync = ClockSynchronizer::new();
        let e = exchange(1_000_000_000, 0, 0.0, 500_000);
        sync.observe(e).unwrap();
        assert_eq!(sync.observe(e).unwrap_err(), ClockError::Stale);
        let mut invalid = e;
        invalid.local_receive_ns = 0;
        assert_eq!(sync.observe(invalid).unwrap_err(), ClockError::InvalidOrder);
        for i in 1..16 {
            sync.observe(exchange(1_000_000_000 + i * 250_000_000, 0, 0.0, 500_000))
                .unwrap();
        }
        assert_eq!(
            sync.observe(exchange(9_000_000_000, 1_000_000_000, 0.0, 500_000))
                .unwrap_err(),
            ClockError::Discontinuity
        );
        assert!((sync.model().unwrap().drift_ppm()).abs() < 0.01);
    }
    #[test]
    fn excluded_observation_does_not_refresh_holdover_expiry() {
        let mut sync = ClockSynchronizer::new();
        for i in 0..64 {
            sync.observe(exchange(1_000_000_000 + i * 250_000_000, 0, 0.0, 500_000))
                .unwrap();
        }
        let expiry = sync.model().unwrap().valid_until_local_ns;
        let first = sync
            .observe(exchange(17_000_000_000, 0, 0.0, 5_000_000))
            .unwrap();
        assert_eq!(first.valid_until_local_ns, expiry);
        for i in 1..16 {
            sync.observe(exchange(
                17_000_000_000 + i * 250_000_000,
                0,
                0.0,
                5_000_000,
            ))
            .unwrap();
        }
        let adapted = sync.model().unwrap();
        assert!(adapted.is_locked());
        assert!(adapted.valid_until_local_ns > expiry);
    }
}
