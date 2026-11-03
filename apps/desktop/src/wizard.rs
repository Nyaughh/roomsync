use crate::{Result, create_private, read_key};
use roomsync_transport::PairingKey;
use roomsync_windows::qpc_now_ns;
use std::io::Write;

fn prompt(question: &str) -> Result<String> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if input.len() > 1024 {
        return Err("input is too long".into());
    }
    Ok(input.trim().to_owned())
}

pub fn arguments() -> Result<Vec<String>> {
    qpc_now_ns()?;
    println!(
        "RoomSync Windows -> Android setup\n\nConnect both devices to the same trusted Wi-Fi/LAN. This prototype uses no microphone and makes no zero-latency promise. Keep the Windows capture output inaudible (for example, headphones) to avoid hearing it ahead of the phones. No audio settings will be changed.\n"
    );
    let phone =
        prompt("Android Wi-Fi IPv4 address (or comma-separated addresses for up to 8 phones): ")?;
    let phones: Vec<std::net::Ipv4Addr> = phone
        .split(',')
        .map(|s| s.trim().parse())
        .collect::<std::result::Result<_, _>>()?;
    if phones.is_empty() || phones.len() > 8 {
        return Err("enter 1..=8 phone IPv4 addresses".into());
    }
    if phones
        .iter()
        .any(|ip| ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() || ip.is_loopback())
    {
        return Err("enter the phone's concrete LAN IPv4 address".into());
    }
    // UDP connect selects a local route without transmitting a datagram.
    let route = std::net::UdpSocket::bind("0.0.0.0:0")?;
    route.connect((phones[0], 5001))?;
    let local_ip = route.local_addr()?.ip();
    let data = std::env::var_os("LOCALAPPDATA").ok_or("Windows LOCALAPPDATA is unavailable")?;
    let directory = std::path::PathBuf::from(data).join("RoomSync");
    std::fs::create_dir_all(&directory)?;
    let key_path = directory.join("pairing.key");
    if !key_path.exists() {
        let key = PairingKey::generate()?;
        let mut file = create_private(&key_path)?;
        file.write_all(key.to_hex_for_storage().as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        println!("Created a pairing key in your Windows app-data folder.");
    }
    let key_path = key_path
        .to_str()
        .ok_or("pairing-key path is not Unicode")?
        .to_owned();
    println!(
        "\nAndroid settings: receiver port 5001; desired channel stereo, left, right or mono."
    );
    for (i, phone) in phones.iter().enumerate() {
        println!("Phone {phone}: host {local_ip}, host port {}", 5000 + i);
    }
    if prompt(
        "Show the pairing key now so you can paste it privately into your receiver(s)? [y/N] ",
    )?
    .eq_ignore_ascii_case("y")
    {
        let key = read_key(&key_path)?;
        println!(
            "\nPRIVATE PAIRING KEY (do not post or include in screenshots):\n{}\n",
            key.to_hex_for_storage().as_str()
        );
    } else {
        return Err("setup paused; rerun the wizard when ready to pair privately".into());
    }
    let _ = prompt(
        "Fill Android without connecting yet. Press Enter here, then tap Connect on Android within 10 seconds. ",
    )?;
    let mut args = vec![
        "host".into(),
        "--key".into(),
        key_path,
        "--bind".into(),
        format!("{local_ip}:5000"),
    ];
    for phone in phones {
        args.extend(["--peer".into(), format!("{phone}:5001")]);
    }
    Ok(args)
}
