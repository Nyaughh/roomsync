//! Fixed 320 ms reorder window. Insert and sample access are O(1), with no heap work.
use crate::protocol::{AudioPacket, FRAMES_PER_PACKET, MAX_SEQUENCE};

pub const BUFFER_PACKETS: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferStats {
    pub accepted: u64,
    pub duplicates: u64,
    pub late: u64,
    pub too_far: u64,
    pub wrong_session: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertResult {
    Accepted,
    Duplicate,
    Late,
    TooFar,
    WrongSession,
}

#[derive(Debug)]
pub struct JitterBuffer {
    session_id: u64,
    floor_sequence: u64,
    slots: [Option<AudioPacket>; BUFFER_PACKETS],
    stats: BufferStats,
}
impl JitterBuffer {
    pub fn new(session_id: u64) -> Option<Self> {
        (session_id != 0).then(|| Self {
            session_id,
            floor_sequence: 0,
            slots: std::array::from_fn(|_| None),
            stats: BufferStats::default(),
        })
    }
    pub fn stats(&self) -> BufferStats {
        self.stats
    }
    pub fn floor_sequence(&self) -> u64 {
        self.floor_sequence
    }
    pub fn push(&mut self, packet: AudioPacket) -> InsertResult {
        if packet.session_id() != self.session_id {
            self.stats.wrong_session = self.stats.wrong_session.saturating_add(1);
            return InsertResult::WrongSession;
        }
        let sequence = packet.sequence();
        if sequence < self.floor_sequence {
            self.stats.late = self.stats.late.saturating_add(1);
            return InsertResult::Late;
        }
        if sequence - self.floor_sequence >= BUFFER_PACKETS as u64 {
            self.stats.too_far = self.stats.too_far.saturating_add(1);
            return InsertResult::TooFar;
        }
        let slot = &mut self.slots[(sequence % BUFFER_PACKETS as u64) as usize];
        if slot.as_ref().is_some_and(|p| p.sequence() == sequence) {
            self.stats.duplicates = self.stats.duplicates.saturating_add(1);
            return InsertResult::Duplicate;
        }
        *slot = Some(packet);
        self.stats.accepted = self.stats.accepted.saturating_add(1);
        InsertResult::Accepted
    }
    pub fn sample(&self, frame: u64) -> Option<[f32; 2]> {
        let sequence = frame / FRAMES_PER_PACKET as u64;
        if sequence < self.floor_sequence || sequence > MAX_SEQUENCE {
            return None;
        }
        let packet = self.slots[(sequence % BUFFER_PACKETS as u64) as usize].as_ref()?;
        if packet.sequence() != sequence {
            return None;
        }
        let index = (frame % FRAMES_PER_PACKET as u64) as usize * 2;
        Some([
            packet.samples()[index] as f32 / 32768.0,
            packet.samples()[index + 1] as f32 / 32768.0,
        ])
    }
    /// O(window), never O(timestamp gap), including after long suspend/resume.
    pub fn discard_before_frame(&mut self, frame: u64) {
        let sequence = frame / FRAMES_PER_PACKET as u64;
        if sequence <= self.floor_sequence {
            return;
        }
        self.floor_sequence = sequence;
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|p| p.sequence() < sequence) {
                *slot = None;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SAMPLES_PER_PACKET;
    fn p(seq: u64) -> AudioPacket {
        AudioPacket::new(1, seq, [100; SAMPLES_PER_PACKET]).unwrap()
    }
    #[test]
    fn reorder_duplicate_late_and_future_are_bounded() {
        let mut b = JitterBuffer::new(1).unwrap();
        assert_eq!(b.push(p(2)), InsertResult::Accepted);
        assert_eq!(b.push(p(0)), InsertResult::Accepted);
        assert_eq!(b.push(p(1)), InsertResult::Accepted);
        assert_eq!(b.push(p(1)), InsertResult::Duplicate);
        assert_eq!(b.push(p(128)), InsertResult::TooFar);
        b.discard_before_frame(240);
        assert_eq!(b.push(p(1)), InsertResult::Late);
        assert!(b.sample(239).is_none());
        assert!(b.sample(240).is_some());
        assert_eq!(b.push(p(129)), InsertResult::Accepted);
        assert_eq!(
            b.push(AudioPacket::new(2, 3, [0; SAMPLES_PER_PACKET]).unwrap()),
            InsertResult::WrongSession
        );
    }
    #[test]
    fn huge_discontinuity_has_bounded_work() {
        let mut b = JitterBuffer::new(1).unwrap();
        b.push(p(0));
        b.discard_before_frame(u64::MAX);
        assert!(b.sample(0).is_none());
        assert!(b.sample(u64::MAX).is_none());
    }
}
