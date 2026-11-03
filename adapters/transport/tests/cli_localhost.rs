//! Two real processes and real localhost UDP, not a simulated network.
use std::{
    fs,
    net::UdpSocket,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn wait(mut child: Child) -> std::process::Output {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!("CLI timed out: {}", String::from_utf8_lossy(&out.stderr));
        }
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn two_cli_processes_transfer_pcm_exactly_over_authenticated_udp() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = TempDir(
        std::env::temp_dir().join(format!("roomsync-net-test-{}-{unique}", std::process::id())),
    );
    fs::create_dir(&dir.0).unwrap();
    let key = dir.0.join("private.key");
    let input = dir.0.join("input.pcm");
    let output = dir.0.join("output.pcm");
    let pcm: Vec<u8> = (0..64 * 240)
        .flat_map(|i| ((i as i16).wrapping_mul(131)).to_le_bytes())
        .collect();
    fs::write(&input, &pcm).unwrap();
    let binary = env!("CARGO_BIN_EXE_roomsync-net");
    let generated = Command::new(binary)
        .arg("generate-key")
        .arg(&key)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    // Never print key material or include it in a process argument.
    assert!(
        !String::from_utf8_lossy(&generated.stdout)
            .contains(fs::read_to_string(&key).unwrap().trim())
    );
    let a = UdpSocket::bind("127.0.0.1:0").unwrap();
    let b = UdpSocket::bind("127.0.0.1:0").unwrap();
    let host = a.local_addr().unwrap().to_string();
    let receiver = b.local_addr().unwrap().to_string();
    drop(a);
    drop(b);
    let sender = Command::new(binary)
        .args(["send", &host, &receiver])
        .arg(&key)
        .arg(&input)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let receiver = Command::new(binary)
        .args(["receive", &receiver, &host])
        .arg(&key)
        .arg(&output)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let sent = wait(sender);
    let received = wait(receiver);
    assert!(
        sent.status.success(),
        "sender: {}",
        String::from_utf8_lossy(&sent.stderr)
    );
    assert!(
        received.status.success(),
        "receiver: {}",
        String::from_utf8_lossy(&received.stderr)
    );
    assert_eq!(fs::read(output).unwrap(), pcm);
    let summary = String::from_utf8_lossy(&received.stdout);
    assert!(
        summary.contains("Received 64 PCM packets, 0 missing positions"),
        "{summary}"
    );
    assert!(summary.contains("clock_locked=true"), "{summary}");
}
