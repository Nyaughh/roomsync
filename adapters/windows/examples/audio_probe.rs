//! Local-only diagnostic: no audio is saved or transmitted.
use roomsync_windows::{
    CaptureConfig, CaptureStream, RenderConfig, RenderFill, RenderStream, list_render_endpoints,
};
use std::{
    error::Error,
    thread,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("devices") => {
            for device in list_render_endpoints()? {
                println!(
                    "{}\n  ID: {}\n  Native mix: {} Hz, {} channels",
                    device.name, device.id, device.mix_sample_rate, device.mix_channels
                );
            }
        }
        Some("capture") => {
            let seconds = parse_seconds(args.get(1))?;
            let config = CaptureConfig {
                endpoint_id: args.get(2).cloned(),
                ..Default::default()
            };
            let stream = CaptureStream::start(config)?;
            println!(
                "Loopback: {}\nStereo {} Hz, {}-frame device buffer. No playback, recording, or network.",
                stream.endpoint.name,
                stream.endpoint.stream_sample_rate,
                stream.endpoint.buffer_frames
            );
            println!("Play ordinary audio in another app. Silence may produce no packets.");
            let until = Instant::now() + Duration::from_secs(seconds);
            let mut peak = 0.0f32;
            let mut first_qpc = None;
            while Instant::now() < until && !stream.is_finished() {
                while let Some(packet) = stream.try_recv() {
                    first_qpc = first_qpc.or(packet.qpc_ns);
                    for frame in packet.frames() {
                        peak = peak.max(frame[0].abs()).max(frame[1].abs());
                    }
                }
                thread::sleep(Duration::from_millis(2));
            }
            let stats = stream.stop()?;
            println!("First QPC ns: {first_qpc:?}; peak: {peak:.4}; {stats:#?}");
        }
        Some("tone") => {
            let seconds = parse_seconds(args.get(1))?;
            let config = RenderConfig {
                endpoint_id: args.get(2).cloned(),
                ..Default::default()
            };
            eprintln!(
                "Playing a quiet (-30 dBFS) 440 Hz test tone. Lower speaker/headphone volume first."
            );
            let mut phase = 0.0f32;
            let stream = RenderStream::start(config, move |timing, out| {
                for frame in out {
                    let sample = phase.sin() * 0.031_622_78;
                    *frame = [sample, sample];
                    phase = (phase + std::f32::consts::TAU * 440.0 / timing.sample_rate as f32)
                        % std::f32::consts::TAU;
                }
                RenderFill::Audio
            })?;
            println!(
                "Rendering to {} at {} Hz; timing is estimated, not acoustically calibrated.",
                stream.endpoint.name, stream.endpoint.stream_sample_rate
            );
            let until = Instant::now() + Duration::from_secs(seconds);
            while Instant::now() < until && !stream.is_finished() {
                thread::sleep(Duration::from_millis(50));
            }
            println!("{:#?}", stream.stop()?);
        }
        _ => {
            println!(
                "Local Windows WASAPI diagnostics (Windows 10 1703+):\n  audio_probe devices\n  audio_probe capture [seconds=5] [endpoint-id]\n  audio_probe tone [seconds=5] [endpoint-id]\n\nNo sockets, driver installation, automatic muting, or loopback-to-speaker echo path."
            );
        }
    }
    Ok(())
}
fn parse_seconds(value: Option<&String>) -> Result<u64, Box<dyn Error>> {
    let seconds = value.map(|s| s.parse()).transpose()?.unwrap_or(5);
    if !(1..=3600).contains(&seconds) {
        return Err("seconds must be between 1 and 3600".into());
    }
    Ok(seconds)
}
