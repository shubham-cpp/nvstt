//! Deterministic local verification for a PCM WAV file.
//!
//! This intentionally stays as an example instead of part of the daemon's
//! public CLI. It verifies the recognizer and model artifact without needing a
//! microphone or a Wayland session:
//!
//! ```text
//! cargo run --example transcribe_wav -- --model-dir /path/to/model sample.wav
//! ```

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use nvstt::recognizer::StreamingRecognizer;
use nvstt::{error::Result, recognizer::ParakeetRecognizer};

fn main() -> Result<()> {
    let (model_dir, wav_path, max_seconds) = parse_args()?;
    let mut wav = read_wav(&wav_path)?;
    if let Some(max_seconds) = max_seconds {
        wav.samples.truncate(wav.sample_rate as usize * max_seconds);
    }
    let mut recognizer = ParakeetRecognizer::from_model_dir(model_dir)?;
    recognizer.start_session()?;

    for chunk in wav.samples.chunks((wav.sample_rate as usize / 10).max(1)) {
        recognizer.accept_audio(wav.sample_rate, chunk)?;
    }
    let transcript = recognizer.finish_session()?;
    println!("{transcript}");
    Ok(())
}

fn parse_args() -> Result<(PathBuf, PathBuf, Option<usize>)> {
    let mut args = env::args_os().skip(1);
    let mut model_dir = None;
    let mut wav_path = None;
    let mut max_seconds = None;
    while let Some(arg) = args.next() {
        if arg == "--model-dir" {
            model_dir = args.next().map(PathBuf::from);
        } else if arg == "--seconds" {
            let value = args.next().ok_or_else(|| {
                nvstt::error::AppError::Config("--seconds requires a positive integer".to_owned())
            })?;
            max_seconds = Some(value.to_string_lossy().parse::<usize>().map_err(|_| {
                nvstt::error::AppError::Config("--seconds requires a positive integer".to_owned())
            })?);
            if max_seconds == Some(0) {
                return Err(nvstt::error::AppError::Config(
                    "--seconds requires a positive integer".to_owned(),
                ));
            }
        } else if wav_path.is_none() {
            wav_path = Some(PathBuf::from(arg));
        } else {
            return Err(nvstt::error::AppError::Config(
                "usage: transcribe_wav --model-dir MODEL_DIR [--seconds N] AUDIO.wav".to_owned(),
            ));
        }
    }
    let model_dir = model_dir.ok_or_else(|| {
        nvstt::error::AppError::Config(
            "usage: transcribe_wav --model-dir MODEL_DIR [--seconds N] AUDIO.wav".to_owned(),
        )
    })?;
    let wav_path = wav_path.ok_or_else(|| {
        nvstt::error::AppError::Config(
            "usage: transcribe_wav --model-dir MODEL_DIR [--seconds N] AUDIO.wav".to_owned(),
        )
    })?;
    Ok((model_dir, wav_path, max_seconds))
}

#[derive(Debug)]
struct Wav {
    sample_rate: i32,
    samples: Vec<f32>,
}

fn read_wav(path: &Path) -> Result<Wav> {
    let bytes = fs::read(path)?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(nvstt::error::AppError::Unavailable(
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

    if audio_format != Some(1) || bits_per_sample != Some(16) {
        return Err(nvstt::error::AppError::Unavailable(
            "verification WAV must use 16-bit PCM".to_owned(),
        ));
    }
    let channels = channels.ok_or_else(|| {
        nvstt::error::AppError::Unavailable("WAV has no channel metadata".to_owned())
    })? as usize;
    let sample_rate = sample_rate.ok_or_else(|| {
        nvstt::error::AppError::Unavailable("WAV has no sample-rate metadata".to_owned())
    })? as i32;
    let data = data
        .ok_or_else(|| nvstt::error::AppError::Unavailable("WAV has no data chunk".to_owned()))?;
    if channels == 0 || data.len() % (channels * 2) != 0 {
        return Err(nvstt::error::AppError::Unavailable(
            "WAV data is not aligned to its channel count".to_owned(),
        ));
    }
    let mut samples = Vec::with_capacity(data.len() / (channels * 2));
    for frame in data.chunks_exact(channels * 2) {
        let sum = frame
            .chunks_exact(2)
            .map(|sample| i16::from_le_bytes([sample[0], sample[1]]) as f32 / i16::MAX as f32)
            .sum::<f32>();
        samples.push(sum / channels as f32);
    }
    Ok(Wav {
        sample_rate,
        samples,
    })
}
