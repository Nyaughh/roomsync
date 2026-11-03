use roomsync_core::simulation::{SimulationConfig, run};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args == ["--help"] || args == ["help"] {
        println!(
            "RoomSync native core prototype\n\n  roomsync simulate [seconds]\n\nDeterministic virtual devices only. Not a system-audio capture/playback app.\nPlatform and authenticated-network adapters are separate crates."
        );
        return;
    }
    let result = match args.first().map(String::as_str) {
        Some("simulate") if args.len() <= 2 => {
            let seconds = match args.get(1).map(|s| s.parse::<u32>()).transpose() {
                Ok(value) => value.unwrap_or(30),
                Err(_) => {
                    eprintln!("seconds must be an integer in 1..=300");
                    std::process::exit(2);
                }
            };
            run(SimulationConfig {
                seconds,
                ..SimulationConfig::default()
            })
        }
        _ => {
            eprintln!("unknown command; use --help");
            std::process::exit(2);
        }
    };
    match result {
        Ok(result) => {
            println!(
                "VIRTUAL NETWORK TEST, not acoustic/device measurements\n48 kHz stereo PCM16; 2.5 ms packets; 80 ms simulated playout lead; 2% loss"
            );
            for (i, d) in result.devices.iter().enumerate() {
                println!(
                    "receiver {}: CPU drift {:+.0} ppm, fitted {:+.2} ppm; independent DAC drift {:+.0} ppm; maximum modeled clock error {:.2} us; maximum timeline error {:.3} frames; concealed {}/{} frames; reordered {}, duplicates {}, late {}",
                    i + 1,
                    d.drift_ppm,
                    d.estimated_drift_ppm,
                    d.dac_drift_ppm,
                    d.max_clock_error_us,
                    d.max_timeline_error_frames,
                    d.concealed_frames,
                    d.rendered_frames,
                    d.reordered_packets,
                    d.duplicate_packets,
                    d.late_packets
                );
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}
