//! Small dependency-free WAV reader and writer.

use std::{fs, io::Write, path::Path};

use crate::error::{AppError, Result};

#[derive(Clone, Debug)]
pub struct Waveform {
    pub sample_rate: i32,
    pub samples: Vec<f32>,
}

/// Write mono 32-bit IEEE float samples as a RIFF/WAVE file.
pub(crate) fn write_float_wav(
    writer: &mut impl Write,
    sample_rate: i32,
    samples: &[f32],
) -> Result<()> {
    let data_bytes = samples
        .len()
        .checked_mul(4)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n <= u32::MAX - 48)
        .ok_or_else(|| AppError::Unavailable("recording exceeds WAV size limit".into()))?;
    let byte_rate = u32::try_from(sample_rate)
        .ok()
        .and_then(|rate| rate.checked_mul(4))
        .filter(|rate| *rate > 0)
        .ok_or_else(|| AppError::Unavailable("invalid WAV sample rate".into()))?;

    writer.write_all(b"RIFF")?;
    writer.write_all(&(48 + data_bytes).to_le_bytes())?;
    writer.write_all(b"WAVEfmt ")?;
    writer.write_all(&16_u32.to_le_bytes())?;
    writer.write_all(&3_u16.to_le_bytes())?;
    writer.write_all(&1_u16.to_le_bytes())?;
    writer.write_all(&(sample_rate as u32).to_le_bytes())?;
    writer.write_all(&byte_rate.to_le_bytes())?;
    writer.write_all(&4_u16.to_le_bytes())?;
    writer.write_all(&32_u16.to_le_bytes())?;
    writer.write_all(b"fact")?;
    writer.write_all(&4_u32.to_le_bytes())?;
    writer.write_all(&(data_bytes / 4).to_le_bytes())?;
    writer.write_all(b"data")?;
    writer.write_all(&data_bytes.to_le_bytes())?;
    for sample in samples {
        writer.write_all(&sample.to_le_bytes())?;
    }
    Ok(())
}

/// Read 8-bit or 16-bit PCM or 32-bit IEEE float RIFF/WAVE and downmix to mono.
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
    let mut data_truncated = false;
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
            b"data" => {
                data_truncated = end - offset != size;
                data = Some(&bytes[offset..end]);
            }
            _ => {}
        }
        offset = end + (size & 1);
    }

    let bits_per_sample = bits_per_sample.unwrap_or_default();
    if !matches!(
        (audio_format, bits_per_sample),
        (Some(1), 8 | 16) | (Some(3), 32)
    ) {
        return Err(AppError::Unavailable(
            "evaluation WAV must use 8-bit or 16-bit PCM or 32-bit IEEE float".to_owned(),
        ));
    }
    let channels = channels
        .ok_or_else(|| AppError::Unavailable("WAV has no channel metadata".to_owned()))?
        as usize;
    let sample_rate = sample_rate
        .ok_or_else(|| AppError::Unavailable("WAV has no sample-rate metadata".to_owned()))?
        as i32;
    let data = data.ok_or_else(|| AppError::Unavailable("WAV has no data chunk".to_owned()))?;
    if audio_format == Some(3) && data_truncated {
        return Err(AppError::Unavailable("WAV data is truncated".to_owned()));
    }
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
            32 => frame
                .as_chunks::<4>()
                .0
                .iter()
                .map(|sample| f32::from_le_bytes(*sample))
                .sum::<f32>(),
            _ => unreachable!("validated WAV bit depth"),
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
    fn float_wav_keeps_boundary_samples_and_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.wav");
        let values = [0.125_f32, -0.5, 0.25];
        let mut file = std::fs::File::create(&path).unwrap();
        write_float_wav(&mut file, 48_000, &values).unwrap();
        drop(file);
        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[4..8], &60_u32.to_le_bytes());
        assert_eq!(&bytes[20..22], &3_u16.to_le_bytes());
        assert_eq!(&bytes[28..32], &192_000_u32.to_le_bytes());
        assert_eq!(&bytes[44..48], &3_u32.to_le_bytes());
        assert_eq!(&bytes[52..56], &12_u32.to_le_bytes());
        let wave = read_wav(&path).unwrap();
        assert_eq!(wave.sample_rate, 48_000);
        assert_eq!(wave.samples, values);
    }

    #[test]
    fn empty_float_wav_has_zero_frames() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("empty.wav");
        let mut bytes = Vec::new();
        write_float_wav(&mut bytes, 16_000, &[]).unwrap();
        assert_eq!(bytes.len(), 56);
        assert_eq!(&bytes[4..8], &48_u32.to_le_bytes());
        assert_eq!(&bytes[36..40], b"fact");
        assert_eq!(&bytes[44..48], &0_u32.to_le_bytes());
        assert_eq!(&bytes[48..52], b"data");
        assert_eq!(&bytes[52..56], &0_u32.to_le_bytes());
        fs::write(&path, bytes).unwrap();
        let wave = read_wav(&path).unwrap();
        assert_eq!(wave.sample_rate, 16_000);
        assert!(wave.samples.is_empty());
    }

    #[test]
    fn unsupported_wav_format_still_fails() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("unsupported.wav");
        let mut bytes = Vec::new();
        write_float_wav(&mut bytes, 16_000, &[0.5]).unwrap();
        bytes[20..22].copy_from_slice(&2_u16.to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(read_wav(&path).is_err());
    }

    #[test]
    fn truncated_or_misaligned_float_data_fails() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("invalid.wav");
        let mut bytes = Vec::new();
        write_float_wav(&mut bytes, 16_000, &[0.5]).unwrap();
        fs::write(&path, &bytes[..bytes.len() - 4]).unwrap();
        assert!(read_wav(&path).is_err());

        fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(read_wav(&path).is_err());

        bytes[52..56].copy_from_slice(&3_u32.to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(read_wav(&path).is_err());
    }

    #[test]
    fn float_wav_rejects_invalid_sample_rates() {
        let mut bytes = Vec::new();
        for rate in [0, -1, i32::MAX] {
            assert!(write_float_wav(&mut bytes, rate, &[]).is_err());
            assert!(bytes.is_empty());
        }
    }

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
