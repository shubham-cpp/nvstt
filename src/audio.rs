//! Small dependency-free WAV reader for local model evaluation.

use std::{fs, path::Path};

use crate::error::{AppError, Result};

#[derive(Clone, Debug)]
pub struct Waveform {
    pub sample_rate: i32,
    pub samples: Vec<f32>,
}

/// Read an 8-bit or 16-bit PCM RIFF/WAVE file and downmix it to mono.
pub fn read_wav(path: &Path) -> Result<Waveform> {
    let bytes = fs::read(path)?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(AppError::Unavailable(
            "sample is not a RIFF/WAVE file".to_owned(),
        ));
    }

    let mut offset = 12;
    let mut sample_rate = None;
    let mut channels = None;
    let mut bits_per_sample = None;
    let mut audio_format = None;
    let mut data = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        let end = offset.saturating_add(size).min(bytes.len());
        match id {
            b"fmt " if end - offset >= 16 => {
                audio_format = Some(u16::from_le_bytes(
                    bytes[offset..offset + 2].try_into().unwrap(),
                ));
                channels = Some(u16::from_le_bytes(
                    bytes[offset + 2..offset + 4].try_into().unwrap(),
                ));
                sample_rate = Some(u32::from_le_bytes(
                    bytes[offset + 4..offset + 8].try_into().unwrap(),
                ));
                bits_per_sample = Some(u16::from_le_bytes(
                    bytes[offset + 14..offset + 16].try_into().unwrap(),
                ));
            }
            b"data" => data = Some(&bytes[offset..end]),
            _ => {}
        }
        offset = end + (size & 1);
    }

    let bits_per_sample = bits_per_sample.unwrap_or_default();
    if audio_format != Some(1) || !matches!(bits_per_sample, 8 | 16) {
        return Err(AppError::Unavailable(
            "evaluation WAV must use 8-bit or 16-bit PCM".to_owned(),
        ));
    }
    let channels = channels
        .ok_or_else(|| AppError::Unavailable("WAV has no channel metadata".to_owned()))?
        as usize;
    let sample_rate = sample_rate
        .ok_or_else(|| AppError::Unavailable("WAV has no sample-rate metadata".to_owned()))?
        as i32;
    let data = data.ok_or_else(|| AppError::Unavailable("WAV has no data chunk".to_owned()))?;
    let bytes_per_sample = (bits_per_sample / 8) as usize;
    if channels == 0 || data.len() % (channels * bytes_per_sample) != 0 {
        return Err(AppError::Unavailable(
            "WAV data is not aligned to its channel count".to_owned(),
        ));
    }
    let mut samples = Vec::with_capacity(data.len() / (channels * bytes_per_sample));
    for frame in data.chunks_exact(channels * bytes_per_sample) {
        let sum = match bits_per_sample {
            8 => frame
                .iter()
                .map(|sample| (*sample as f32 - 128.0) / 128.0)
                .sum::<f32>(),
            16 => frame
                .as_chunks::<2>()
                .0
                .iter()
                .map(|sample| i16::from_le_bytes(*sample) as f32 / i16::MAX as f32)
                .sum::<f32>(),
            _ => unreachable!("validated PCM bit depth"),
        };
        samples.push(sum / channels as f32);
    }
    Ok(Waveform {
        sample_rate,
        samples,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn downmixes_16_bit_stereo() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("sample.wav");
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&36_u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_000_u32.to_le_bytes());
        bytes.extend_from_slice(&64_000_u32.to_le_bytes());
        bytes.extend_from_slice(&4_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&4_u32.to_le_bytes());
        bytes.extend_from_slice(&i16::MAX.to_le_bytes());
        bytes.extend_from_slice(&0_i16.to_le_bytes());
        fs::write(&path, bytes).expect("write WAV");

        let waveform = read_wav(&path).expect("read WAV");
        assert_eq!(waveform.sample_rate, 16_000);
        assert_eq!(waveform.samples.len(), 1);
        assert!((waveform.samples[0] - 0.5).abs() < 0.01);
    }

    #[test]
    fn accepts_8_bit_pcm() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("sample.wav");
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&37_u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&8_000_u32.to_le_bytes());
        bytes.extend_from_slice(&8_000_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&8_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.push(128);
        bytes.push(0);
        fs::write(&path, bytes).expect("write WAV");

        let waveform = read_wav(&path).expect("read WAV");
        assert_eq!(waveform.sample_rate, 8_000);
        assert_eq!(waveform.samples, vec![0.0]);
    }
}
