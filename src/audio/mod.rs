//! Audio processing utilities — PCM analysis, silence detection

/// Check if a PCM buffer is mostly silence (below amplitude threshold)
pub fn is_silence(pcm: &[i16], threshold: i16) -> bool {
    if pcm.is_empty() {
        return true;
    }
    let rms = rms_amplitude(pcm);
    rms < threshold as f64
}

/// Compute RMS (root mean square) amplitude of PCM samples
pub fn rms_amplitude(pcm: &[i16]) -> f64 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / pcm.len() as f64).sqrt()
}

/// Compute peak amplitude
pub fn peak_amplitude(pcm: &[i16]) -> i16 {
    pcm.iter().map(|s| s.abs()).max().unwrap_or(0)
}

/// Convert i16 PCM to little-endian bytes (for WebSocket binary frames)
pub fn pcm_to_bytes(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// Convert little-endian bytes back to i16 PCM
pub fn bytes_to_pcm(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_silence_detection() {
        let silence = vec![0i16; 1600];
        assert!(is_silence(&silence, 100));

        let noise: Vec<i16> = (0..1600).map(|i| (i % 100) as i16 * 100).collect();
        assert!(!is_silence(&noise, 100));
    }

    #[test]
    fn test_rms_amplitude() {
        let silence = vec![0i16; 100];
        assert_eq!(rms_amplitude(&silence), 0.0);

        let loud = vec![1000i16; 100];
        assert!((rms_amplitude(&loud) - 1000.0).abs() < 0.1);

        assert_eq!(rms_amplitude(&[]), 0.0);
    }

    #[test]
    fn test_peak_amplitude() {
        assert_eq!(peak_amplitude(&[0, 100, -200, 50]), 200);
        assert_eq!(peak_amplitude(&[0, 0, 0]), 0);
        assert_eq!(peak_amplitude(&[]), 0);
    }

    #[test]
    fn test_pcm_byte_roundtrip() {
        let original: Vec<i16> = vec![0, 1000, -1000, i16::MAX, i16::MIN];
        let bytes = pcm_to_bytes(&original);
        let restored = bytes_to_pcm(&bytes);
        assert_eq!(original, restored);
    }

    #[test]
    fn test_pcm_to_bytes_length() {
        let pcm = vec![0i16; 1600];
        assert_eq!(pcm_to_bytes(&pcm).len(), 3200);
    }
}
