//! Windows 10 1703+ WASAPI endpoint loopback capture and native shared render.
//!
//! Capture is stereo float32 at exactly 48 kHz through Windows' audio converter.
//! Render also requests 48 kHz stereo; Windows converts to the endpoint mix. No network
//! sockets, virtual drivers, endpoint mute, or automatic device switching are used.

pub mod convert;
#[cfg(windows)]
mod native;

use crossbeam_queue::ArrayQueue;
use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread::JoinHandle;

pub const CAPTURE_SAMPLE_RATE: u32 = 48_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    UnsupportedPlatform,
    UnsupportedFormat,
    InvalidConfig,
    InvalidData,
    ReopenRequired,
    System,
    ThreadPanic,
}

#[derive(Clone, Debug)]
pub struct AdapterError {
    pub kind: ErrorKind,
    pub operation: &'static str,
    pub message: String,
    pub hresult: Option<i32>,
}
impl AdapterError {
    pub fn new(kind: ErrorKind, operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            operation,
            message: message.into(),
            hresult: None,
        }
    }
    /// Re-open after user/device recovery and reset the synchronization stream epoch.
    pub fn can_reopen(&self) -> bool {
        self.kind == ErrorKind::ReopenRequired
    }
}
impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.operation, self.message)?;
        if let Some(hr) = self.hresult {
            write!(f, " (HRESULT 0x{:08X})", hr as u32)?;
        }
        Ok(())
    }
}
impl std::error::Error for AdapterError {}

#[derive(Clone, Debug)]
pub struct EndpointInfo {
    pub id: String,
    pub name: String,
    /// Native engine mix rate, not necessarily the hardware's physical clock rate.
    pub mix_sample_rate: u32,
    pub mix_channels: u16,
    /// Format delivered to the callback/queue. Always two float32 channels.
    pub stream_sample_rate: u32,
    pub buffer_frames: u32,
    /// WASAPI's maximum stream latency. Not an acoustic measurement.
    pub maximum_stream_latency_ns: u64,
    pub engine_period_ns: u64,
    pub mmcss_enabled: bool,
}

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    /// A specific render endpoint, or the current default console render endpoint.
    pub endpoint_id: Option<String>,
    /// Upper bound for the driver-selected event buffer, not a latency request.
    pub max_buffer_duration_ms: u32,
    /// Total preallocated audio blocks, including any held by the consumer.
    pub queue_blocks: usize,
}
impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            endpoint_id: None,
            max_buffer_duration_ms: 200,
            queue_blocks: 16,
        }
    }
}
#[derive(Clone, Debug)]
pub struct RenderConfig {
    pub endpoint_id: Option<String>,
    /// Upper bound for the driver-selected event buffer, not a latency request.
    pub max_buffer_duration_ms: u32,
    /// Measured signed device/acoustic correction. Positive means sound arrives later.
    /// Zero means uncalibrated. Never use this to pretend a Bluetooth path is measured.
    pub extra_output_delay_ns: i64,
}
impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            endpoint_id: None,
            max_buffer_duration_ms: 200,
            extra_output_delay_ns: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StreamStats {
    pub frames: u64,
    pub packets: u64,
    pub dropped_packets: u64,
    pub discontinuities: u64,
    pub invalid_timestamps: u64,
    pub silent_frames: u64,
    pub event_timeouts: u64,
    pub render_underruns: u64,
}
#[derive(Default)]
struct Counters {
    frames: AtomicU64,
    packets: AtomicU64,
    dropped_packets: AtomicU64,
    discontinuities: AtomicU64,
    invalid_timestamps: AtomicU64,
    silent_frames: AtomicU64,
    event_timeouts: AtomicU64,
    render_underruns: AtomicU64,
}
impl Counters {
    fn snapshot(&self) -> StreamStats {
        StreamStats {
            frames: self.frames.load(Ordering::Relaxed),
            packets: self.packets.load(Ordering::Relaxed),
            dropped_packets: self.dropped_packets.load(Ordering::Relaxed),
            discontinuities: self.discontinuities.load(Ordering::Relaxed),
            invalid_timestamps: self.invalid_timestamps.load(Ordering::Relaxed),
            silent_frames: self.silent_frames.load(Ordering::Relaxed),
            event_timeouts: self.event_timeouts.load(Ordering::Relaxed),
            render_underruns: self.render_underruns.load(Ordering::Relaxed),
        }
    }
}

/// Owns one preallocated buffer. Dropping this packet recycles the storage.
/// Holding every packet stalls no audio thread: new capture packets are dropped instead.
pub struct CapturedPacket {
    storage: Option<Vec<[f32; 2]>>,
    pool: Arc<ArrayQueue<Vec<[f32; 2]>>>,
    frame_count: usize,
    /// First frame's QPC time in nanoseconds, or None on TIMESTAMP_ERROR.
    pub qpc_ns: Option<u64>,
    pub device_position_frames: u64,
    pub discontinuity: bool,
    pub silent: bool,
}
impl CapturedPacket {
    pub fn frames(&self) -> &[[f32; 2]] {
        &self.storage.as_ref().unwrap()[..self.frame_count]
    }
}
impl Drop for CapturedPacket {
    fn drop(&mut self) {
        if let Some(storage) = self.storage.take() {
            let _ = self.pool.push(storage);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RenderTiming {
    pub sample_rate: u32,
    /// Estimate for callback frame zero, in this machine's QPC nanosecond domain.
    /// Device clock observations are extrapolated to the stream write cursor.
    /// Includes configured extra_output_delay_ns. Not measured acoustic synchronization.
    pub estimated_dac_start_ns: u64,
    pub clock_position: u64,
    pub clock_frequency: u64,
    pub clock_qpc_ns: u64,
    pub queued_frames: u32,
    pub write_cursor_frames: u64,
    pub timestamp_estimated: bool,
    pub discontinuity: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderFill {
    Audio,
    Silence,
}

struct Worker {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<Result<(), AdapterError>>>,
    counters: Arc<Counters>,
}
impl Worker {
    fn stop(&mut self) -> Result<StreamStats, AdapterError> {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| {
                AdapterError::new(
                    ErrorKind::ThreadPanic,
                    "audio thread",
                    "audio worker panicked",
                )
            })??;
        }
        Ok(self.counters.snapshot())
    }
    fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub struct CaptureStream {
    pub endpoint: EndpointInfo,
    queue: Arc<ArrayQueue<CapturedPacket>>,
    worker: Worker,
}
impl CaptureStream {
    pub fn start(config: CaptureConfig) -> Result<Self, AdapterError> {
        validate_buffer(config.max_buffer_duration_ms)?;
        if !(2..=128).contains(&config.queue_blocks) {
            return Err(AdapterError::new(
                ErrorKind::InvalidConfig,
                "capture configuration",
                "queue_blocks must be between 2 and 128",
            ));
        }
        #[cfg(windows)]
        {
            native::start_capture(config)
        }
        #[cfg(not(windows))]
        {
            let _ = config;
            Err(unsupported())
        }
    }
    /// Nonblocking; no allocation. Poll from your non-realtime transport thread.
    pub fn try_recv(&self) -> Option<CapturedPacket> {
        self.queue.pop()
    }
    pub fn stats(&self) -> StreamStats {
        self.worker.counters.snapshot()
    }
    pub fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }
    pub fn stop(mut self) -> Result<StreamStats, AdapterError> {
        self.worker.stop()
    }
}

pub struct RenderStream {
    pub endpoint: EndpointInfo,
    worker: Worker,
}
impl RenderStream {
    /// The callback runs on the dedicated WASAPI worker and owns its state.
    /// It must not block, allocate, log, do network I/O, or take contended locks.
    /// `out` is zeroed before every call; return Silence to set WASAPI's SILENT flag.
    pub fn start<F>(config: RenderConfig, callback: F) -> Result<Self, AdapterError>
    where
        F: FnMut(RenderTiming, &mut [[f32; 2]]) -> RenderFill + Send + 'static,
    {
        validate_buffer(config.max_buffer_duration_ms)?;
        if config.extra_output_delay_ns.unsigned_abs() > 10_000_000_000 {
            return Err(AdapterError::new(
                ErrorKind::InvalidConfig,
                "render configuration",
                "output delay correction must be within +/-10 seconds",
            ));
        }
        #[cfg(windows)]
        {
            native::start_render(config, callback)
        }
        #[cfg(not(windows))]
        {
            let _ = (config, callback);
            Err(unsupported())
        }
    }
    pub fn stats(&self) -> StreamStats {
        self.worker.counters.snapshot()
    }
    pub fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }
    pub fn stop(mut self) -> Result<StreamStats, AdapterError> {
        self.worker.stop()
    }
}

/// Enumerate active render endpoints on a dedicated COM-initialized thread.
pub fn list_render_endpoints() -> Result<Vec<EndpointInfo>, AdapterError> {
    #[cfg(windows)]
    {
        native::list_endpoints()
    }
    #[cfg(not(windows))]
    {
        Err(unsupported())
    }
}

/// QPC monotonic nanoseconds. Use this for control clock synchronization as well.
/// Never subtract a Rust Instant epoch from a WASAPI timestamp.
pub fn qpc_now_ns() -> Result<u64, AdapterError> {
    #[cfg(windows)]
    {
        native::qpc_now_ns()
    }
    #[cfg(not(windows))]
    {
        Err(unsupported())
    }
}
fn validate_buffer(ms: u32) -> Result<(), AdapterError> {
    if !(5..=200).contains(&ms) {
        return Err(AdapterError::new(
            ErrorKind::InvalidConfig,
            "configuration",
            "max_buffer_duration_ms must be between 5 and 200",
        ));
    }
    Ok(())
}
#[cfg(not(windows))]
fn unsupported() -> AdapterError {
    AdapterError::new(
        ErrorKind::UnsupportedPlatform,
        "WASAPI",
        "requires Windows 10 version 1703 or later",
    )
}

/// Project a stream write cursor from a correlated audio-clock/QPC observation.
/// Device clock position units are deliberately divided by its actual frequency.
pub fn project_dac_time_ns(
    qpc_ns: u64,
    clock_position: u64,
    clock_frequency: u64,
    write_cursor_frames: u64,
    sample_rate: u32,
    correction_ns: i64,
) -> Option<u64> {
    if clock_frequency == 0 || sample_rate == 0 {
        return None;
    }
    let write_ns =
        u128::from(write_cursor_frames).checked_mul(1_000_000_000)? / u128::from(sample_rate);
    let played_ns =
        u128::from(clock_position).checked_mul(1_000_000_000)? / u128::from(clock_frequency);
    let projected = i128::from(qpc_ns) + i128::try_from(write_ns).ok()?
        - i128::try_from(played_ns).ok()?
        + i128::from(correction_ns);
    u64::try_from(projected).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_units_are_not_assumed_to_be_frames() {
        assert_eq!(
            project_dac_time_ns(1_000_000_000, 192_000, 192_000, 48_480, 48_000, 0),
            Some(1_010_000_000)
        );
        assert_eq!(
            project_dac_time_ns(1_000_000_000, 192_000, 192_000, 48_480, 48_000, 2_000_000),
            Some(1_012_000_000)
        );
        assert_eq!(project_dac_time_ns(1, 1, 0, 1, 48_000, 0), None);
        assert_eq!(project_dac_time_ns(1, 1, 1, 1, 0, 0), None);
    }
    #[test]
    fn capture_packet_storage_recycles_and_is_bounded() {
        let pool = Arc::new(ArrayQueue::new(2));
        pool.push(vec![[0.0; 2]; 480]).unwrap();
        pool.push(vec![[0.0; 2]; 480]).unwrap();
        let a = pool.pop().unwrap();
        let b = pool.pop().unwrap();
        assert!(pool.pop().is_none());
        let packet = CapturedPacket {
            storage: Some(a),
            pool: pool.clone(),
            frame_count: 120,
            qpc_ns: Some(0),
            device_position_frames: 0,
            discontinuity: false,
            silent: true,
        };
        assert_eq!(packet.frames().len(), 120);
        drop(packet);
        assert_eq!(pool.len(), 1);
        pool.push(b).unwrap();
        assert_eq!(pool.len(), 2);
    }
    #[test]
    fn invalid_configuration_rejected_before_start() {
        assert!(
            CaptureStream::start(CaptureConfig {
                queue_blocks: 0,
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            RenderStream::start(
                RenderConfig {
                    max_buffer_duration_ms: 0,
                    ..Default::default()
                },
                |_, _| RenderFill::Silence
            )
            .is_err()
        );
    }
    #[cfg(not(windows))]
    #[test]
    fn platform_support_is_explicit() {
        assert_eq!(
            CaptureStream::start(Default::default()).err().unwrap().kind,
            ErrorKind::UnsupportedPlatform
        );
    }
}
