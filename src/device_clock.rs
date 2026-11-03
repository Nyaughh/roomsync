//! Estimate the DAC's frame period in the receiver's local monotonic clock.
//!
//! A device's sample clock is independent of both its CPU clock and the remote
//! host clock. Supply a hardware-correlated presentation timestamp and the
//! cumulative output-frame index of that exact timestamp. Callback entry time,
//! queued packet counts and wall-clock timestamps are not suitable substitutes.
//!
//! All storage and work are bounded; observing a callback performs no allocation,
//! locking or I/O. The estimate is usable only after at least eight observations
//! spanning 500 ms. Observations are retained at least 50 ms apart. This is a
//! calibration-quality gate, not a statistical confidence guarantee: timestamp
//! bias, slowly varying output latency and correlated errors are not identifiable.
//! In particular, the fit residual is not a bound on true rate error.

const HISTORY: usize = 64;
const MIN_OBSERVATIONS: usize = 8;
const MIN_SAMPLE_INTERVAL_NS: u64 = 50_000_000;
const MIN_SPAN_NS: u64 = 500_000_000;
const MAX_OBSERVATION_GAP_NS: u64 = 2_000_000_000;
const MAX_RATE_ERROR: f64 = 2_000.0e-6;
const MAX_STEP_ERROR_NS: f64 = 5_000_000.0;
const MAX_RESIDUAL_NS: f64 = 250_000.0;

/// An estimate expires one second after the newest observation used in its fit.
pub const DEVICE_RATE_MAX_AGE_NS: u64 = 1_000_000_000;

#[derive(Clone, Copy, Debug)]
struct Observation {
    local_ns: u64,
    frames: u64,
}

#[derive(Clone, Copy, Debug)]
struct Estimate {
    ns_per_frame: f64,
    max_residual_ns: f64,
    updated_at_ns: u64,
}

/// Receiver-local DAC rate estimator, independent of remote clock synchronization.
///
/// Fall back to `1e9 / nominal_sample_rate` while this returns `None`. Reset on a
/// device/route change, even if the new device has the same nominal sample rate.
/// The frame counter counts output frames, including silence and concealment,
/// rather than source frames consumed by the resampler.
#[derive(Debug)]
pub struct DeviceClockEstimator {
    samples: [Option<Observation>; HISTORY],
    cursor: usize,
    count: usize,
    nominal_sample_rate: u32,
    last_seen: Option<Observation>,
    last_sample_ns: Option<u64>,
    estimate: Option<Estimate>,
}

impl Default for DeviceClockEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceClockEstimator {
    pub fn new() -> Self {
        Self {
            samples: [None; HISTORY],
            cursor: 0,
            count: 0,
            nominal_sample_rate: 0,
            last_seen: None,
            last_sample_ns: None,
            estimate: None,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Return local nanoseconds per output frame, if calibrated and fresh.
    ///
    /// Freshness is evaluated in the same local monotonic clock as observations.
    /// Calling this before the last fitted timestamp also returns `None`.
    pub fn estimate_at(&self, local_ns: u64) -> Option<f64> {
        self.fresh_estimate(local_ns).map(|e| e.ns_per_frame)
    }

    /// Maximum timestamp residual in the fresh fit, not a true error bound.
    pub fn max_residual_ns_at(&self, local_ns: u64) -> Option<f64> {
        self.fresh_estimate(local_ns).map(|e| e.max_residual_ns)
    }

    fn fresh_estimate(&self, local_ns: u64) -> Option<Estimate> {
        self.estimate.filter(|e| {
            local_ns
                .checked_sub(e.updated_at_ns)
                .is_some_and(|age| age <= DEVICE_RATE_MAX_AGE_NS)
        })
    }

    /// Observe the presentation time of `cumulative_output_frames`.
    ///
    /// Exact duplicate pairs are ignored. Backwards/non-progressing timestamps
    /// or frame counts, a changed nominal rate, an observation gap over two
    /// seconds, large timestamp steps, implausible fitted rates (>2,000 ppm),
    /// and excessive fit residuals clear calibration. The current observation
    /// then starts a new window. A zero nominal rate clears all state.
    ///
    /// Sub-50-ms callbacks still undergo continuity checks, but do not grow the
    /// regression history. Small timestamp jitter is tolerated; it is not treated
    /// as a rate measurement from a single short callback interval.
    pub fn observe(
        &mut self,
        dac_start_local_ns: u64,
        cumulative_output_frames: u64,
        nominal_sample_rate: u32,
    ) -> Option<f64> {
        if nominal_sample_rate == 0 {
            self.reset();
            return None;
        }
        let current = Observation {
            local_ns: dac_start_local_ns,
            frames: cumulative_output_frames,
        };
        if self.nominal_sample_rate != nominal_sample_rate || self.last_seen.is_none() {
            self.restart(current, nominal_sample_rate);
            return None;
        }
        let previous = self.last_seen?;
        if current.local_ns == previous.local_ns && current.frames == previous.frames {
            return self.estimate_at(current.local_ns);
        }
        let deltas = current
            .local_ns
            .checked_sub(previous.local_ns)
            .zip(current.frames.checked_sub(previous.frames));
        let Some((elapsed_ns, elapsed_frames)) = deltas else {
            self.restart(current, nominal_sample_rate);
            return None;
        };
        let nominal_ns_per_frame = 1e9 / f64::from(nominal_sample_rate);
        let expected_ns = elapsed_frames as f64 * nominal_ns_per_frame;
        if elapsed_ns == 0
            || elapsed_frames == 0
            || elapsed_ns > MAX_OBSERVATION_GAP_NS
            || (elapsed_ns as f64 - expected_ns).abs()
                > MAX_STEP_ERROR_NS + expected_ns * MAX_RATE_ERROR
        {
            self.restart(current, nominal_sample_rate);
            return None;
        }
        self.last_seen = Some(current);
        if self
            .last_sample_ns
            .is_some_and(|last| current.local_ns - last < MIN_SAMPLE_INTERVAL_NS)
        {
            return self.estimate_at(current.local_ns);
        }
        self.samples[self.cursor] = Some(current);
        self.cursor = (self.cursor + 1) % HISTORY;
        self.count = (self.count + 1).min(HISTORY);
        self.last_sample_ns = Some(current.local_ns);
        if let Some(candidate) = self.fit(current) {
            let rate_error = nominal_ns_per_frame / candidate.ns_per_frame - 1.0;
            if !candidate.ns_per_frame.is_finite()
                || candidate.ns_per_frame <= 0.0
                || rate_error.abs() > MAX_RATE_ERROR
                || candidate.max_residual_ns > MAX_RESIDUAL_NS
            {
                self.restart(current, nominal_sample_rate);
                return None;
            }
            self.estimate = Some(candidate);
        }
        self.estimate_at(current.local_ns)
    }

    fn restart(&mut self, current: Observation, nominal_sample_rate: u32) {
        self.reset();
        self.nominal_sample_rate = nominal_sample_rate;
        self.samples[0] = Some(current);
        self.cursor = 1;
        self.count = 1;
        self.last_seen = Some(current);
        self.last_sample_ns = Some(current.local_ns);
    }

    fn fit(&self, anchor: Observation) -> Option<Estimate> {
        if self.count < MIN_OBSERVATIONS {
            return None;
        }
        let oldest_ns = self.samples.iter().flatten().map(|o| o.local_ns).min()?;
        if anchor.local_ns - oldest_ns < MIN_SPAN_NS {
            return None;
        }
        // Subtract integers before conversion, so large monotonic epochs and
        // cumulative frame counters do not lose their low bits in f64.
        let delta = |o: &Observation| {
            (
                (i128::from(o.frames) - i128::from(anchor.frames)) as f64,
                (i128::from(o.local_ns) - i128::from(anchor.local_ns)) as f64,
            )
        };
        let (mut x_sum, mut y_sum) = (0.0, 0.0);
        for o in self.samples.iter().flatten() {
            let (x, y) = delta(o);
            x_sum += x;
            y_sum += y;
        }
        let mean_x = x_sum / self.count as f64;
        let mean_y = y_sum / self.count as f64;
        let (mut covariance, mut variance) = (0.0, 0.0);
        for o in self.samples.iter().flatten() {
            let (x, y) = delta(o);
            covariance += (x - mean_x) * (y - mean_y);
            variance += (x - mean_x) * (x - mean_x);
        }
        if variance <= 0.0 {
            return None;
        }
        let ns_per_frame = covariance / variance;
        let mut max_residual_ns: f64 = 0.0;
        for o in self.samples.iter().flatten() {
            let (x, y) = delta(o);
            max_residual_ns = max_residual_ns.max((y - mean_y - (x - mean_x) * ns_per_frame).abs());
        }
        Some(Estimate {
            ns_per_frame,
            max_residual_ns,
            updated_at_ns: anchor.local_ns,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn train(rate: u32, ppm: f64) -> (DeviceClockEstimator, u64, u64) {
        let mut estimator = DeviceClockEstimator::new();
        let period = 1e9 / (f64::from(rate) * (1.0 + ppm / 1e6));
        let mut last = (0, 0);
        for i in 0..1_000 {
            let frames = i * u64::from(rate / 100);
            let local_ns = 5_000_000_000 + (frames as f64 * period).round() as u64;
            estimator.observe(local_ns, frames, rate);
            last = (local_ns, frames);
        }
        (estimator, last.0, last.1)
    }

    #[test]
    fn tracks_independent_dac_drift_at_48_and_96_khz() {
        for rate in [48_000, 96_000] {
            for ppm in [-500.0, 0.0, 500.0] {
                let (estimator, time, _) = train(rate, ppm);
                let measured_rate = 1e9 / estimator.estimate_at(time).unwrap();
                let measured_ppm = (measured_rate / f64::from(rate) - 1.0) * 1e6;
                assert!((measured_ppm - ppm).abs() < 0.01, "{measured_ppm} vs {ppm}");
            }
        }
    }

    #[test]
    fn timestamp_jitter_is_regressed_not_used_as_callback_rate() {
        for rate in [48_000, 96_000] {
            let mut estimator = DeviceClockEstimator::new();
            let period = 1e9 / (f64::from(rate) * 1.0005);
            let mut rng = 0x57a8b4_u64;
            let mut last_time = 0;
            for i in 0..2_000 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let jitter = (rng % 80_001) as i64 - 40_000;
                let frames = i * u64::from(rate / 100);
                let time =
                    (5_000_000_000_i64 + (frames as f64 * period).round() as i64 + jitter) as u64;
                estimator.observe(time, frames, rate);
                last_time = time;
            }
            let measured = 1e9 / estimator.estimate_at(last_time).unwrap();
            let ppm = (measured / f64::from(rate) - 1.0) * 1e6;
            assert!((ppm - 500.0).abs() < 25.0, "{ppm}");
            assert!(estimator.max_residual_ns_at(last_time).unwrap() < 100_000.0);
        }
    }

    #[test]
    fn requires_observation_count_and_time_span() {
        let mut estimator = DeviceClockEstimator::new();
        // Frequent callbacks cannot fill the history prematurely.
        for i in 0..500 {
            assert!(estimator.observe(i * 1_000_000, i * 48, 48_000).is_none());
        }
        assert!(estimator.observe(500_000_000, 24_000, 48_000).is_some());
        let mut sparse = DeviceClockEstimator::new();
        for i in 0..7 {
            assert!(sparse.observe(i * 100_000_000, i * 4_800, 48_000).is_none());
        }
        assert!(sparse.observe(700_000_000, 33_600, 48_000).is_some());
    }

    #[test]
    fn exact_duplicates_are_ignored_but_backwards_pairs_reset() {
        let (mut estimator, time, frames) = train(48_000, 0.0);
        let before = estimator.estimate_at(time);
        assert_eq!(estimator.observe(time, frames, 48_000), before);
        assert!(estimator.observe(time - 1, frames + 480, 48_000).is_none());
        assert!(estimator.estimate_at(time).is_none());
        let (mut estimator, time, frames) = train(48_000, 0.0);
        assert!(
            estimator
                .observe(time + 10_000_000, frames - 1, 48_000)
                .is_none()
        );
        assert!(estimator.estimate_at(time + 10_000_000).is_none());
    }

    #[test]
    fn nonprogressing_pairs_forward_steps_and_idle_gaps_reset() {
        for case in 0..4 {
            let (mut estimator, time, frames) = train(48_000, 0.0);
            let (next_time, next_frames) = match case {
                0 => (time, frames + 480),
                1 => (time + 10_000_000, frames),
                2 => (time + 60_000_000, frames + 480),
                _ => (time + 3_000_000_000, frames + 144_000),
            };
            assert!(estimator.observe(next_time, next_frames, 48_000).is_none());
            assert!(estimator.estimate_at(next_time).is_none());
        }
    }

    #[test]
    fn rate_change_invalid_rate_and_expired_estimates_do_not_reuse_calibration() {
        let (mut estimator, time, frames) = train(48_000, 0.0);
        assert!(
            estimator
                .estimate_at(time + DEVICE_RATE_MAX_AGE_NS + 1)
                .is_none()
        );
        assert!(estimator.estimate_at(0).is_none());
        assert!(
            estimator
                .observe(time + 10_000_000, frames + 960, 96_000)
                .is_none()
        );
        assert!(estimator.estimate_at(time + 10_000_000).is_none());
        let (mut estimator, time, frames) = train(48_000, 0.0);
        assert!(estimator.observe(time, frames, 0).is_none());
        assert!(estimator.estimate_at(time).is_none());
    }

    #[test]
    fn rejects_implausible_rates_and_excessive_timestamp_noise() {
        for ppm in [-3_000.0, 3_000.0] {
            let (estimator, time, _) = train(48_000, ppm);
            assert!(estimator.estimate_at(time).is_none());
        }
        let mut estimator = DeviceClockEstimator::new();
        for i in 0..1_000 {
            let jitter = if i % 2 == 0 { 1_000_000 } else { 0 };
            assert!(
                estimator
                    .observe(i * 60_000_000 + jitter, i * 2_880, 48_000)
                    .is_none()
            );
        }
    }

    #[test]
    fn large_integer_epochs_and_frame_counters_keep_precision() {
        let mut estimator = DeviceClockEstimator::new();
        let time_origin = u64::MAX / 2;
        let frame_origin = u64::MAX / 2;
        let mut last_time = 0;
        for i in 0..100 {
            last_time = time_origin + i * 60_000_000;
            estimator.observe(last_time, frame_origin + i * 5_760, 96_000);
        }
        let actual = estimator.estimate_at(last_time).unwrap();
        assert!((actual - 1e9 / 96_000.0).abs() < 1e-8);
    }
}
