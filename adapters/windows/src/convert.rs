//! Allocation-free conversions. WASAPI negotiates stereo float32; PCM16 is the wire boundary.

use crate::{AdapterError, ErrorKind};

/// Replace non-finite audio with silence and prevent float/integer overflow.
pub fn finite_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

pub fn f32_to_pcm16(sample: f32) -> i16 {
    (finite_sample(sample) as f64 * 32768.0)
        .round()
        .clamp(-32768.0, 32767.0) as i16
}

pub fn pcm16_to_f32(sample: i16) -> f32 {
    sample as f32 / 32768.0
}

/// Decode little-endian interleaved stereo float32 without alignment assumptions.
/// For a WASAPI SILENT packet `bytes` must be empty and is never inspected.
pub fn decode_stereo_f32(
    bytes: &[u8],
    silent: bool,
    out: &mut [[f32; 2]],
) -> Result<(), AdapterError> {
    if silent {
        out.fill([0.0; 2]);
        return Ok(());
    }
    if bytes.len() != out.len().saturating_mul(8) {
        return Err(AdapterError::new(
            ErrorKind::InvalidData,
            "decode",
            "float32 byte length does not match stereo frame count",
        ));
    }
    for (frame, bytes) in out.iter_mut().zip(bytes.chunks_exact(8)) {
        frame[0] = finite_sample(f32::from_le_bytes(bytes[..4].try_into().unwrap()));
        frame[1] = finite_sample(f32::from_le_bytes(bytes[4..8].try_into().unwrap()));
    }
    Ok(())
}

pub fn encode_stereo_f32(frames: &[[f32; 2]], out: &mut [u8]) -> Result<(), AdapterError> {
    if out.len() != frames.len().saturating_mul(8) {
        return Err(AdapterError::new(
            ErrorKind::InvalidData,
            "encode",
            "float32 byte length does not match stereo frame count",
        ));
    }
    for (frame, bytes) in frames.iter().zip(out.chunks_exact_mut(8)) {
        bytes[..4].copy_from_slice(&finite_sample(frame[0]).to_le_bytes());
        bytes[4..8].copy_from_slice(&finite_sample(frame[1]).to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_pcm_endpoints_and_nonfinite() {
        assert_eq!(f32_to_pcm16(-1.0), i16::MIN);
        assert_eq!(f32_to_pcm16(1.0), i16::MAX);
        assert_eq!(f32_to_pcm16(2.0), i16::MAX);
        assert_eq!(f32_to_pcm16(f32::NAN), 0);
        assert_eq!(f32_to_pcm16(f32::INFINITY), 0);
        assert_eq!(pcm16_to_f32(i16::MIN), -1.0);
        assert_eq!(pcm16_to_f32(i16::MAX), 32767.0 / 32768.0);
    }
    #[test]
    fn all_pcm16_values_roundtrip() {
        for n in i16::MIN..=i16::MAX {
            assert_eq!(f32_to_pcm16(pcm16_to_f32(n)), n);
        }
    }
    #[test]
    fn silence_does_not_require_or_read_bytes() {
        let mut frames = [[0.8, -0.5]; 8];
        decode_stereo_f32(&[], true, &mut frames).unwrap();
        assert_eq!(frames, [[0.0; 2]; 8]);
    }
    #[test]
    fn stereo_order_roundtrip_and_sanitize() {
        let frames = [[0.5, -0.25], [f32::NAN, 2.0]];
        let mut bytes = [0; 16];
        encode_stereo_f32(&frames, &mut bytes).unwrap();
        let mut decoded = [[0.0; 2]; 2];
        decode_stereo_f32(&bytes, false, &mut decoded).unwrap();
        assert_eq!(decoded, [[0.5, -0.25], [0.0, 1.0]]);
    }
    #[test]
    fn wrong_size_rejected_and_unaligned_bytes_supported() {
        let mut frame = [[0.0; 2]];
        assert!(decode_stereo_f32(&[0; 7], false, &mut frame).is_err());
        let mut bytes = [0; 9];
        bytes[1..5].copy_from_slice(&0.5f32.to_le_bytes());
        bytes[5..].copy_from_slice(&(-0.75f32).to_le_bytes());
        decode_stereo_f32(&bytes[1..], false, &mut frame).unwrap();
        assert_eq!(frame, [[0.5, -0.75]]);
    }
}
