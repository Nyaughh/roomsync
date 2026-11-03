use super::*;
use std::{ptr, sync::mpsc, thread};
use wasapi::{
    AudioClient, Device, DeviceEnumerator, Direction, SampleType, StreamMode, WasapiError,
    WaveFormat,
};
use windows::{
    Win32::{
        Foundation::{HANDLE, S_FALSE, S_OK},
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
            AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_E_DEVICE_INVALIDATED,
            AUDCLNT_E_RESOURCES_INVALIDATED, AUDCLNT_E_SERVICE_NOT_RUNNING,
            AUDCLNT_E_UNSUPPORTED_FORMAT, IAudioCaptureClient, IAudioClock,
        },
        System::{
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            Threading::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW},
        },
    },
    core::{Interface, w},
};

const MAX_BUFFER_FRAMES: usize = 65_536;
const MAX_CALLBACK_FRAMES: usize = 8_000;

fn win_error(operation: &'static str, e: windows::core::Error) -> AdapterError {
    let code = e.code();
    let kind = if [
        AUDCLNT_E_DEVICE_INVALIDATED,
        AUDCLNT_E_RESOURCES_INVALIDATED,
        AUDCLNT_E_SERVICE_NOT_RUNNING,
    ]
    .contains(&code)
    {
        ErrorKind::ReopenRequired
    } else if code == AUDCLNT_E_UNSUPPORTED_FORMAT {
        ErrorKind::UnsupportedFormat
    } else {
        ErrorKind::System
    };
    AdapterError {
        kind,
        operation,
        message: e.to_string(),
        hresult: Some(code.0),
    }
}
fn api_error(operation: &'static str, e: WasapiError) -> AdapterError {
    match e {
        WasapiError::Windows(e) => win_error(operation, e),
        WasapiError::UnsupportedFormat | WasapiError::UnsupportedSubformat(_) => {
            AdapterError::new(ErrorKind::UnsupportedFormat, operation, e.to_string())
        }
        WasapiError::DeviceNotFound(_) => {
            AdapterError::new(ErrorKind::ReopenRequired, operation, e.to_string())
        }
        _ => AdapterError::new(ErrorKind::System, operation, e.to_string()),
    }
}

struct ComApartment;
impl ComApartment {
    fn new() -> Result<Self, AdapterError> {
        wasapi::initialize_mta()
            .ok()
            .map_err(|e| win_error("initialize COM", e))?;
        Ok(Self)
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}
struct Mmcss(Option<HANDLE>);
impl Mmcss {
    fn new() -> Self {
        let mut task_index = 0;
        // SAFETY: static UTF-16 name and valid output index; registration is local to this thread.
        Self(unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task_index) }.ok())
    }
}
impl Drop for Mmcss {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
        }
    }
}
// Struct fields drop in declaration order: release IAudioClient before closing
// its event. Services and Running guards are local and drop before this owner.
struct OpenStream {
    client: AudioClient,
    event: wasapi::Handle,
}

struct Running<'a>(&'a AudioClient);
impl Drop for Running<'_> {
    fn drop(&mut self) {
        let _ = self.0.stop_stream();
    }
}

fn device_for(id: Option<&str>) -> Result<Device, AdapterError> {
    let enumerator = DeviceEnumerator::new().map_err(|e| api_error("enumerate endpoints", e))?;
    let device = match id {
        Some(id) => enumerator.get_device(id),
        None => enumerator.get_default_device(&Direction::Render),
    }
    .map_err(|e| api_error("select render endpoint", e))?;
    if device.get_direction() != Direction::Render {
        return Err(AdapterError::new(
            ErrorKind::InvalidConfig,
            "select endpoint",
            "loopback and playback require a render endpoint, not a microphone",
        ));
    }
    Ok(device)
}
fn describe(device: &Device, client: &AudioClient) -> Result<EndpointInfo, AdapterError> {
    let mix = client
        .get_mixformat()
        .map_err(|e| api_error("get mix format", e))?;
    let (period, _) = client
        .get_device_period()
        .map_err(|e| api_error("get engine period", e))?;
    Ok(EndpointInfo {
        id: device
            .get_id()
            .map_err(|e| api_error("get endpoint ID", e))?,
        name: device
            .get_friendlyname()
            .map_err(|e| api_error("get endpoint name", e))?,
        mix_sample_rate: mix.get_samplespersec(),
        mix_channels: mix.get_nchannels(),
        stream_sample_rate: CAPTURE_SAMPLE_RATE,
        buffer_frames: 0,
        maximum_stream_latency_ns: 0,
        engine_period_ns: (period.max(0) as u64).saturating_mul(100),
        mmcss_enabled: false,
    })
}
fn initialize(
    id: Option<&str>,
    capture: bool,
    buffer_ms: u32,
) -> Result<(OpenStream, EndpointInfo), AdapterError> {
    let device = device_for(id)?;
    let mut client = device
        .get_iaudioclient()
        .map_err(|e| api_error("activate audio client", e))?;
    let mut info = describe(&device, &client)?;
    let rate = CAPTURE_SAMPLE_RATE;
    if !(8_000..=384_000).contains(&rate)
        || !(8_000..=384_000).contains(&info.mix_sample_rate)
        || info.mix_channels == 0
    {
        return Err(AdapterError::new(
            ErrorKind::UnsupportedFormat,
            "initialize stream",
            "endpoint mix sample rate outside supported 8–384 kHz range",
        ));
    }
    let format = WaveFormat::new(32, 32, &SampleType::Float, rate as usize, 2, Some(3));
    // Explicit, negotiated stream format. This enables AUTOCONVERTPCM and
    // SRC_DEFAULT_QUALITY. It never re-labels native 44.1/96 kHz bytes as 48 kHz.
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        // Shared event mode requires duration=periodicity=0. The engine chooses.
        buffer_duration_hns: 0,
    };
    client
        .initialize_client(
            &format,
            &if capture {
                Direction::Capture
            } else {
                Direction::Render
            },
            &mode,
        )
        .map_err(|e| {
            api_error(
                "initialize stereo float32 shared stream with Windows conversion",
                e,
            )
        })?;
    // The event MUST be attached before client teardown, even if a later check fails.
    let event = client
        .set_get_eventhandle()
        .map_err(|e| api_error("attach WASAPI event", e))?;
    let stream = OpenStream { client, event };
    let client = &stream.client;
    info.stream_sample_rate = rate;
    info.buffer_frames = client
        .get_buffer_size()
        .map_err(|e| api_error("get buffer size", e))?;
    if info.buffer_frames == 0
        || info.buffer_frames as usize > MAX_BUFFER_FRAMES
        || u64::from(info.buffer_frames) * 1000 > u64::from(rate) * u64::from(buffer_ms)
    {
        return Err(AdapterError::new(
            ErrorKind::UnsupportedFormat,
            "initialize stream",
            "driver buffer exceeds bounded adapter capacity",
        ));
    }
    // SAFETY: client has been initialized; method writes only to wrapper-owned result.
    let latency = unsafe { client.as_iaudioclient().GetStreamLatency() }
        .map_err(|e| win_error("get stream latency", e))?;
    info.maximum_stream_latency_ns = (latency.max(0) as u64).saturating_mul(100);
    Ok((stream, info))
}

pub(super) fn list_endpoints() -> Result<Vec<EndpointInfo>, AdapterError> {
    thread::Builder::new()
        .name("roomsync-enumerate".into())
        .spawn(|| {
            let _com = ComApartment::new()?;
            let enumerator =
                DeviceEnumerator::new().map_err(|e| api_error("enumerate endpoints", e))?;
            let devices = enumerator
                .get_device_collection(&Direction::Render)
                .map_err(|e| api_error("list render endpoints", e))?;
            let count = devices
                .get_nbr_devices()
                .map_err(|e| api_error("count endpoints", e))?;
            let mut result = Vec::with_capacity(count as usize);
            for i in 0..count {
                let device = devices
                    .get_device_at_index(i)
                    .map_err(|e| api_error("get endpoint", e))?;
                let client = device
                    .get_iaudioclient()
                    .map_err(|e| api_error("activate endpoint", e))?;
                result.push(describe(&device, &client)?);
            }
            Ok(result)
        })
        .map_err(|e| {
            AdapterError::new(ErrorKind::System, "spawn enumeration thread", e.to_string())
        })?
        .join()
        .map_err(|_| {
            AdapterError::new(
                ErrorKind::ThreadPanic,
                "enumeration",
                "enumeration worker panicked",
            )
        })?
}

pub(super) fn qpc_now_ns() -> Result<u64, AdapterError> {
    static FREQUENCY: std::sync::OnceLock<Result<u64, AdapterError>> = std::sync::OnceLock::new();
    let frequency = FREQUENCY
        .get_or_init(|| {
            let mut frequency = 0;
            // SAFETY: valid output pointer; QPC frequency is fixed for the boot session.
            unsafe { QueryPerformanceFrequency(&mut frequency) }
                .map_err(|e| win_error("QPC frequency", e))?;
            if frequency <= 0 {
                return Err(AdapterError::new(
                    ErrorKind::InvalidData,
                    "QPC",
                    "invalid counter frequency",
                ));
            }
            Ok(frequency as u64)
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let mut ticks = 0;
    // SAFETY: valid output pointer; this API needs no COM state.
    unsafe { QueryPerformanceCounter(&mut ticks) }.map_err(|e| win_error("QPC counter", e))?;
    if ticks < 0 {
        return Err(AdapterError::new(
            ErrorKind::InvalidData,
            "QPC",
            "invalid performance counter",
        ));
    }
    let ns = (ticks as u128 * 1_000_000_000) / u128::from(*frequency);
    u64::try_from(ns).map_err(|_| {
        AdapterError::new(
            ErrorKind::InvalidData,
            "QPC",
            "performance counter overflow",
        )
    })
}

pub(super) fn start_capture(config: CaptureConfig) -> Result<CaptureStream, AdapterError> {
    let queue = Arc::new(ArrayQueue::new(config.queue_blocks));
    let counters = Arc::new(Counters::default());
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel(1);
    let worker_queue = queue.clone();
    let worker_stop = stop.clone();
    let worker_counters = counters.clone();
    let join = thread::Builder::new()
        .name("roomsync-loopback".into())
        .spawn(move || {
            let result = capture_loop(config, worker_queue, worker_stop, worker_counters, &tx);
            if let Err(e) = &result {
                let _ = tx.try_send(Err(e.clone()));
            }
            result
        })
        .map_err(|e| AdapterError::new(ErrorKind::System, "spawn capture thread", e.to_string()))?;
    let mut worker = Worker {
        stop,
        join: Some(join),
        counters,
    };
    match rx.recv() {
        Ok(Ok(endpoint)) => Ok(CaptureStream {
            endpoint,
            queue,
            worker,
        }),
        Ok(Err(e)) => {
            let _ = worker.stop();
            Err(e)
        }
        Err(_) => {
            let _ = worker.stop();
            Err(AdapterError::new(
                ErrorKind::ThreadPanic,
                "capture startup",
                "worker exited before startup",
            ))
        }
    }
}

/// Releases each acquired capture packet even on conversion failure/unwind.
struct CaptureLease<'a> {
    client: &'a IAudioCaptureClient,
    frames: u32,
    released: bool,
}
impl CaptureLease<'_> {
    fn release(mut self) -> Result<(), AdapterError> {
        // Release is attempted once only, including on an error with uncertain driver state.
        self.released = true;
        unsafe { self.client.ReleaseBuffer(self.frames) }
            .map_err(|e| win_error("release capture packet", e))
    }
}
impl Drop for CaptureLease<'_> {
    fn drop(&mut self) {
        if !self.released {
            let _ = unsafe { self.client.ReleaseBuffer(self.frames) };
        }
    }
}

fn capture_loop(
    config: CaptureConfig,
    queue: Arc<ArrayQueue<CapturedPacket>>,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    startup: &mpsc::SyncSender<Result<EndpointInfo, AdapterError>>,
) -> Result<(), AdapterError> {
    let _com = ComApartment::new()?;
    let _ = qpc_now_ns()?;
    let mmcss = Mmcss::new();
    let (stream, mut info) = initialize(
        config.endpoint_id.as_deref(),
        true,
        config.max_buffer_duration_ms,
    )?;
    let client = &stream.client;
    let event = &stream.event;
    info.mmcss_enabled = mmcss.0.is_some();
    // Use GetBuffer directly: the wasapi 0.26 high-level read helper dereferences
    // SILENT buffers, which need not contain readable sample memory.
    let capture: IAudioCaptureClient = unsafe { client.as_iaudioclient().GetService() }
        .map_err(|e| win_error("get capture service", e))?;
    let pool = Arc::new(ArrayQueue::new(config.queue_blocks));
    for _ in 0..config.queue_blocks {
        let _ = pool.push(vec![[0.0; 2]; info.buffer_frames as usize]);
    }
    client
        .start_stream()
        .map_err(|e| api_error("start loopback", e))?;
    let _running = Running(client);
    let _ = startup.send(Ok(info.clone()));
    let mut lost_since_delivery = false;
    while !stop.load(Ordering::Acquire) {
        match event.wait_for_event(100) {
            Ok(()) => {}
            Err(WasapiError::EventTimeout) => {
                counters.event_timeouts.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => return Err(api_error("wait for capture event", e)),
        }
        // Timeout is normal during system silence. Polling after the wait also
        // detects invalidation; all currently available packets are drained.
        while !stop.load(Ordering::Acquire)
            && unsafe { capture.GetNextPacketSize() }
                .map_err(|e| win_error("get next capture packet", e))?
                > 0
        {
            let mut data = ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut position = 0;
            let mut timestamp = 0;
            unsafe {
                capture.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut position),
                    Some(&mut timestamp),
                )
            }
            .map_err(|e| win_error("acquire capture packet", e))?;
            if frames == 0 {
                continue;
            } // AUDCLNT_S_BUFFER_EMPTY: no release required.
            let lease = CaptureLease {
                client: &capture,
                frames,
                released: false,
            };
            if frames > info.buffer_frames {
                return Err(AdapterError::new(
                    ErrorKind::InvalidData,
                    "capture",
                    "driver packet larger than endpoint buffer",
                ));
            }
            counters
                .frames
                .fetch_add(u64::from(frames), Ordering::Relaxed);
            counters.packets.fetch_add(1, Ordering::Relaxed);
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            let discontinuity = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
            let invalid_time = flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0;
            if discontinuity {
                counters.discontinuities.fetch_add(1, Ordering::Relaxed);
            }
            if invalid_time {
                counters.invalid_timestamps.fetch_add(1, Ordering::Relaxed);
            }
            if silent {
                counters
                    .silent_frames
                    .fetch_add(u64::from(frames), Ordering::Relaxed);
            }
            if let Some(mut storage) = pool.pop() {
                let bytes = if silent {
                    &[][..]
                } else {
                    if data.is_null() {
                        return Err(AdapterError::new(
                            ErrorKind::InvalidData,
                            "capture",
                            "non-silent WASAPI packet has null sample pointer",
                        ));
                    }
                    // SAFETY: WASAPI guarantees `frames * block_align` readable bytes
                    // until ReleaseBuffer; negotiated block_align is stereo f32 = 8.
                    unsafe { std::slice::from_raw_parts(data, frames as usize * 8) }
                };
                convert::decode_stereo_f32(bytes, silent, &mut storage[..frames as usize])?;
                lease.release()?;
                let packet = CapturedPacket {
                    storage: Some(storage),
                    pool: pool.clone(),
                    frame_count: frames as usize,
                    qpc_ns: if invalid_time {
                        None
                    } else {
                        timestamp.checked_mul(100)
                    },
                    device_position_frames: position,
                    discontinuity: discontinuity || lost_since_delivery,
                    silent,
                };
                if queue.push(packet).is_err() {
                    counters.dropped_packets.fetch_add(1, Ordering::Relaxed);
                    lost_since_delivery = true;
                } else {
                    lost_since_delivery = false;
                }
            } else {
                lease.release()?;
                counters.dropped_packets.fetch_add(1, Ordering::Relaxed);
                lost_since_delivery = true;
            }
        }
    }
    Ok(())
}

pub(super) fn start_render<F>(
    config: RenderConfig,
    callback: F,
) -> Result<RenderStream, AdapterError>
where
    F: FnMut(RenderTiming, &mut [[f32; 2]]) -> RenderFill + Send + 'static,
{
    let counters = Arc::new(Counters::default());
    let stop = Arc::new(AtomicBool::new(false));
    let worker_counters = counters.clone();
    let worker_stop = stop.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    let join = thread::Builder::new()
        .name("roomsync-render".into())
        .spawn(move || {
            let result = render_loop(config, callback, worker_stop, worker_counters, &tx);
            if let Err(e) = &result {
                let _ = tx.try_send(Err(e.clone()));
            }
            result
        })
        .map_err(|e| AdapterError::new(ErrorKind::System, "spawn render thread", e.to_string()))?;
    let mut worker = Worker {
        stop,
        join: Some(join),
        counters,
    };
    match rx.recv() {
        Ok(Ok(endpoint)) => Ok(RenderStream { endpoint, worker }),
        Ok(Err(e)) => {
            let _ = worker.stop();
            Err(e)
        }
        Err(_) => {
            let _ = worker.stop();
            Err(AdapterError::new(
                ErrorKind::ThreadPanic,
                "render startup",
                "worker exited before startup",
            ))
        }
    }
}

fn read_render_clock(clock: &IAudioClock) -> Result<(u64, u64), AdapterError> {
    // Generated wrappers turn S_FALSE into Ok(()), losing WASAPI's explicit
    // inaccurate-reading indication. Preserve the HRESULT and retry at most once.
    for _ in 0..2 {
        let mut position = 0;
        let mut timestamp = 0;
        // SAFETY: a live IAudioClock and valid output pointers on its COM thread.
        let result =
            unsafe { (clock.vtable().GetPosition)(clock.as_raw(), &mut position, &mut timestamp) };
        if result == S_OK {
            return Ok((position, timestamp));
        }
        if result != S_FALSE {
            return Err(win_error("get render clock", result.into()));
        }
    }
    Err(AdapterError::new(
        ErrorKind::ReopenRequired,
        "render clock",
        "device could not supply an accurate clock reading; reconnect playback",
    ))
}

fn render_loop<F>(
    config: RenderConfig,
    mut callback: F,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    startup: &mpsc::SyncSender<Result<EndpointInfo, AdapterError>>,
) -> Result<(), AdapterError>
where
    F: FnMut(RenderTiming, &mut [[f32; 2]]) -> RenderFill,
{
    let _com = ComApartment::new()?;
    let _ = qpc_now_ns()?;
    let mmcss = Mmcss::new();
    let (stream, mut info) = initialize(
        config.endpoint_id.as_deref(),
        false,
        config.max_buffer_duration_ms,
    )?;
    if let Some(source) = config.loopback_source_endpoint_id.as_deref() {
        // Check the resolved device, not only the caller's requested string.
        // No audio has been primed or started at this point.
        validate_loopback_route(source, &info.id)?;
        validate_local_output_buffer(
            info.buffer_frames,
            info.stream_sample_rate,
            info.engine_period_ns,
        )?;
    }
    let client = &stream.client;
    let event = &stream.event;
    info.mmcss_enabled = mmcss.0.is_some();
    let render = client
        .get_audiorenderclient()
        .map_err(|e| api_error("get render service", e))?;
    // SAFETY: the initialized render client supplies an owned IAudioClock.
    let clock: IAudioClock = unsafe { client.as_iaudioclient().GetService() }
        .map_err(|e| win_error("get audio clock", e))?;
    let frequency =
        unsafe { clock.GetFrequency() }.map_err(|e| win_error("get audio clock frequency", e))?;
    if frequency == 0 {
        return Err(AdapterError::new(
            ErrorKind::InvalidData,
            "render clock",
            "zero audio clock frequency",
        ));
    }
    let mut raw = vec![0; info.buffer_frames as usize * 8];
    let mut frames = vec![[0.0; 2]; (info.buffer_frames as usize).min(MAX_CALLBACK_FRAMES)];
    // Prime before Start. Startup carries no trustworthy advancing clock, so use silence.
    render
        .write_to_device(
            info.buffer_frames as usize,
            &raw,
            Some(wasapi::BufferFlags {
                silent: true,
                ..wasapi::BufferFlags::none()
            }),
        )
        .map_err(|e| api_error("prime render silence", e))?;
    let mut written = u64::from(info.buffer_frames);
    client
        .start_stream()
        .map_err(|e| api_error("start rendering", e))?;
    let _running = Running(client);
    let _ = startup.send(Ok(info.clone()));
    let mut consecutive_timeouts = 0;
    let mut continuity = timing::ClockContinuity::new(qpc_now_ns()?, info.engine_period_ns);
    let mut clock_started = false;
    while !stop.load(Ordering::Acquire) {
        match event.wait_for_event(100) {
            Ok(()) => consecutive_timeouts = 0,
            Err(WasapiError::EventTimeout) => {
                counters.event_timeouts.fetch_add(1, Ordering::Relaxed);
                consecutive_timeouts += 1;
                // Query detects normal device invalidation before generic timeout error.
                client
                    .get_current_padding()
                    .map_err(|e| api_error("render timeout device check", e))?;
                if consecutive_timeouts >= 20 {
                    return Err(AdapterError::new(
                        ErrorKind::ReopenRequired,
                        "render event",
                        "audio device stopped signaling for two seconds; reopen and reset synchronization",
                    ));
                }
                continue;
            }
            Err(e) => return Err(api_error("wait for render event", e)),
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        let padding = client
            .get_current_padding()
            .map_err(|e| api_error("get render padding", e))?;
        if padding > info.buffer_frames {
            return Err(AdapterError::new(
                ErrorKind::InvalidData,
                "render padding",
                "driver padding exceeds buffer",
            ));
        }
        let mut available = (info.buffer_frames - padding) as usize;
        // Check even a full buffer: repeated events must not mask a stalled DAC.
        let (position, timestamp) = read_render_clock(&clock)?;
        let clock_qpc_ns = timestamp.checked_mul(100).ok_or_else(|| {
            AdapterError::new(
                ErrorKind::InvalidData,
                "render clock",
                "QPC timestamp overflow",
            )
        })?;
        let now = qpc_now_ns()?;
        let clock_advancing = continuity
            .observe(now, position, clock_qpc_ns)
            .map_err(|_| {
                AdapterError::new(
                    ErrorKind::ReopenRequired,
                    "render clock",
                    "device clock is invalid, stale, frozen, or regressing; reconnect playback",
                )
            })?;
        clock_started |= clock_advancing;
        if available == 0 {
            continue;
        }
        // In synchronized local mode, empty padding makes continuity uncertain:
        // the engine could have inserted silence not represented by `written`,
        // even while previously submitted frames remain in flight at the DAC.
        // A false-positive reconnect is preferable to silently shifted audio.
        if config.loopback_source_endpoint_id.is_some() && padding == 0 {
            counters.render_underruns.fetch_add(1, Ordering::Relaxed);
            return Err(AdapterError::new(
                ErrorKind::ReopenRequired,
                "local playback queue",
                "playback queue emptied after priming; continuity is uncertain, reconnect or choose another output",
            ));
        }
        let predicted = project_dac_time_ns(
            clock_qpc_ns,
            position,
            frequency,
            written,
            info.stream_sample_rate,
            0,
        );
        // Padding excludes engine/driver in-flight data, so it cannot safely be
        // used to invent a new DAC cursor after starvation. Fail closed instead.
        if predicted.is_none_or(|t| t < now) {
            counters.render_underruns.fetch_add(1, Ordering::Relaxed);
            return Err(AdapterError::new(
                ErrorKind::ReopenRequired,
                "render cursor",
                "playback fell behind its DAC timeline; reopen and reset synchronization",
            ));
        }
        let mut chunk_padding = padding;
        while available > 0 {
            let count = available.min(frames.len());
            let out = &mut frames[..count];
            out.fill([0.0; 2]);
            let estimated = project_dac_time_ns(
                clock_qpc_ns,
                position,
                frequency,
                written,
                info.stream_sample_rate,
                config.extra_output_delay_ns,
            )
            .ok_or_else(|| {
                AdapterError::new(
                    ErrorKind::InvalidData,
                    "render timing",
                    "DAC projection overflow",
                )
            })?;
            let timing = RenderTiming {
                sample_rate: info.stream_sample_rate,
                callback_qpc_ns: qpc_now_ns()?,
                estimated_dac_start_ns: estimated,
                clock_position: position,
                clock_frequency: frequency,
                clock_qpc_ns,
                clock_advancing,
                queued_frames: chunk_padding,
                write_cursor_frames: written,
                timestamp_estimated: true,
                // Zero padding is uncertain continuity, not proof of an audible
                // glitch. Guarded local mode exits above rather than calling back.
                discontinuity: timing::queue_discontinuity(clock_started, padding),
            };
            let requested_fill = callback(timing, out);
            // A stream can report position zero until primed data reaches the DAC.
            // No user-provided audio is emitted before an advancing observation.
            let fill = if clock_started {
                requested_fill
            } else {
                RenderFill::Silence
            };
            let bytes = &mut raw[..count * 8];
            let flags = if fill == RenderFill::Silence {
                counters
                    .silent_frames
                    .fetch_add(count as u64, Ordering::Relaxed);
                // The wrapper copies bytes even for SILENT: zero them as defense in depth.
                bytes.fill(0);
                Some(wasapi::BufferFlags {
                    silent: true,
                    ..wasapi::BufferFlags::none()
                })
            } else {
                convert::encode_stereo_f32(out, bytes)?;
                None
            };
            // A slow callback must not enqueue samples tagged for a time that has
            // already passed. Use the uncorrected hardware deadline, not an
            // acoustic correction that could hide starvation.
            let submit_now_ns = qpc_now_ns()?;
            if project_dac_time_ns(
                clock_qpc_ns,
                position,
                frequency,
                written,
                info.stream_sample_rate,
                0,
            )
            .is_none_or(|deadline| deadline <= submit_now_ns)
            {
                counters.render_underruns.fetch_add(1, Ordering::Relaxed);
                return Err(AdapterError::new(
                    ErrorKind::ReopenRequired,
                    "render callback deadline",
                    "render callback missed its DAC timeline; reconnect playback",
                ));
            }
            if config.loopback_source_endpoint_id.is_some()
                && client
                    .get_current_padding()
                    .map_err(|e| api_error("check local playback queue", e))?
                    == 0
            {
                counters.render_underruns.fetch_add(1, Ordering::Relaxed);
                return Err(AdapterError::new(
                    ErrorKind::ReopenRequired,
                    "local playback queue",
                    "playback queue emptied during callback; reconnect playback",
                ));
            }
            render
                .write_to_device(count, bytes, flags)
                .map_err(|e| api_error("write render packet", e))?;
            counters.frames.fetch_add(count as u64, Ordering::Relaxed);
            counters.packets.fetch_add(1, Ordering::Relaxed);
            written = written.checked_add(count as u64).ok_or_else(|| {
                AdapterError::new(
                    ErrorKind::InvalidData,
                    "render cursor",
                    "frame cursor overflow",
                )
            })?;
            chunk_padding += count as u32;
            available -= count;
        }
    }
    Ok(())
}
