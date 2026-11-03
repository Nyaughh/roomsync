//! Fixed-size, canonical wire representation. No native-endian casts or allocations.

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const FRAMES_PER_PACKET: usize = 120;
pub const SAMPLES_PER_PACKET: usize = FRAMES_PER_PACKET * CHANNELS;
pub const HEADER_BYTES: usize = 32;
pub const SESSION_PACKET_BYTES: usize = HEADER_BYTES + SAMPLES_PER_PACKET * 2;
/// Reserved headroom prevents frame arithmetic wrapping inside a session.
pub const MAX_SEQUENCE: u64 = (u64::MAX / FRAMES_PER_PACKET as u64) - 1;
const MAGIC: [u8; 4] = *b"RSYN";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioPacket {
    session_id: u64,
    sequence: u64,
    samples: [i16; SAMPLES_PER_PACKET],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketError {
    Length,
    Magic,
    Version,
    Format,
    Reserved,
    Session,
    Sequence,
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid audio packet: {self:?}")
    }
}
impl std::error::Error for PacketError {}

impl AudioPacket {
    pub fn new(
        session_id: u64,
        sequence: u64,
        samples: [i16; SAMPLES_PER_PACKET],
    ) -> Result<Self, PacketError> {
        if session_id == 0 {
            return Err(PacketError::Session);
        }
        if sequence > MAX_SEQUENCE {
            return Err(PacketError::Sequence);
        }
        Ok(Self {
            session_id,
            sequence,
            samples,
        })
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn first_frame(&self) -> u64 {
        self.sequence * FRAMES_PER_PACKET as u64
    }
    pub fn samples(&self) -> &[i16; SAMPLES_PER_PACKET] {
        &self.samples
    }

    /// Writes only to a caller-owned fixed-size buffer, with every byte initialized.
    pub fn encode(&self, out: &mut [u8; SESSION_PACKET_BYTES]) {
        out.fill(0);
        out[..4].copy_from_slice(&MAGIC);
        out[4] = 1; // protocol version
        out[5] = 1; // little-endian signed PCM16
        out[6] = CHANNELS as u8;
        out[8..16].copy_from_slice(&self.session_id.to_be_bytes());
        out[16..24].copy_from_slice(&self.sequence.to_be_bytes());
        out[24..28].copy_from_slice(&SAMPLE_RATE.to_be_bytes());
        out[28..30].copy_from_slice(&(FRAMES_PER_PACKET as u16).to_be_bytes());
        for (sample, bytes) in self
            .samples
            .iter()
            .zip(out[HEADER_BYTES..].chunks_exact_mut(2))
        {
            bytes.copy_from_slice(&sample.to_le_bytes());
        }
    }

    /// Rejects oversized and truncated packets, unknown flags and noncanonical formats.
    pub fn decode(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.len() != SESSION_PACKET_BYTES {
            return Err(PacketError::Length);
        }
        if bytes[..4] != MAGIC {
            return Err(PacketError::Magic);
        }
        if bytes[4] != 1 {
            return Err(PacketError::Version);
        }
        if bytes[5] != 1
            || bytes[6] != CHANNELS as u8
            || bytes[24..28] != SAMPLE_RATE.to_be_bytes()
            || bytes[28..30] != (FRAMES_PER_PACKET as u16).to_be_bytes()
        {
            return Err(PacketError::Format);
        }
        if bytes[7] != 0 || bytes[30] != 0 || bytes[31] != 0 {
            return Err(PacketError::Reserved);
        }
        let session_id =
            u64::from_be_bytes(bytes[8..16].try_into().map_err(|_| PacketError::Length)?);
        let sequence =
            u64::from_be_bytes(bytes[16..24].try_into().map_err(|_| PacketError::Length)?);
        let mut samples = [0; SAMPLES_PER_PACKET];
        for (sample, bytes) in samples
            .iter_mut()
            .zip(bytes[HEADER_BYTES..].chunks_exact(2))
        {
            *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
        }
        Self::new(session_id, sequence, samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_pcm_round_trip() {
        let mut samples = [0; SAMPLES_PER_PACKET];
        samples[0] = i16::MIN;
        samples[1] = i16::MAX;
        samples[239] = -42;
        let p = AudioPacket::new(7, 19, samples).unwrap();
        let mut bytes = [0; SESSION_PACKET_BYTES];
        p.encode(&mut bytes);
        assert_eq!(AudioPacket::decode(&bytes), Ok(p));
        assert_eq!(bytes.len(), 512);
    }
    #[test]
    fn rejects_all_truncated_lengths_and_reserved_fields() {
        let p = AudioPacket::new(1, 0, [0; SAMPLES_PER_PACKET]).unwrap();
        let mut bytes = [0; SESSION_PACKET_BYTES];
        p.encode(&mut bytes);
        for n in 0..SESSION_PACKET_BYTES {
            assert_eq!(AudioPacket::decode(&bytes[..n]), Err(PacketError::Length));
        }
        for index in [7, 30, 31] {
            bytes[index] = 1;
            assert_eq!(AudioPacket::decode(&bytes), Err(PacketError::Reserved));
            bytes[index] = 0;
        }
    }
    #[test]
    fn rejects_overflow_and_zero_session() {
        assert_eq!(
            AudioPacket::new(0, 0, [0; SAMPLES_PER_PACKET]),
            Err(PacketError::Session)
        );
        assert_eq!(
            AudioPacket::new(1, u64::MAX, [0; SAMPLES_PER_PACKET]),
            Err(PacketError::Sequence)
        );
        assert!(AudioPacket::decode(&[0; SESSION_PACKET_BYTES + 1]).is_err());
    }
}
