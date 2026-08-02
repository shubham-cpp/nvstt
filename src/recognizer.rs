use std::path::{Path, PathBuf};

use sherpa_onnx::{LinearResampler, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};

use crate::error::{AppError, Result};

pub trait StreamingRecognizer: Send {
    fn start_session(&mut self) -> Result<()>;
    fn accept_audio(&mut self, sample_rate: i32, samples: &[f32]) -> Result<()>;
    fn finish_session(&mut self) -> Result<String>;
    fn cancel_session(&mut self) -> Result<()>;
}

#[derive(Debug)]
pub struct UnavailableRecognizer {
    reason: String,
}

impl UnavailableRecognizer {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl StreamingRecognizer for UnavailableRecognizer {
    fn start_session(&mut self) -> Result<()> {
        Err(AppError::Unavailable(self.reason.clone()))
    }

    fn accept_audio(&mut self, _sample_rate: i32, _samples: &[f32]) -> Result<()> {
        Err(AppError::Unavailable(self.reason.clone()))
    }

    fn finish_session(&mut self) -> Result<String> {
        Err(AppError::Unavailable(self.reason.clone()))
    }

    fn cancel_session(&mut self) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct StaticRecognizer {
    transcript: String,
    active: bool,
}

impl StaticRecognizer {
    pub fn new(transcript: impl Into<String>) -> Self {
        Self {
            transcript: transcript.into(),
            active: false,
        }
    }
}

impl StreamingRecognizer for StaticRecognizer {
    fn start_session(&mut self) -> Result<()> {
        self.active = true;
        Ok(())
    }

    fn accept_audio(&mut self, _sample_rate: i32, _samples: &[f32]) -> Result<()> {
        if !self.active {
            return Err(AppError::InvalidState(
                "recognizer is not active".to_owned(),
            ));
        }
        Ok(())
    }

    fn finish_session(&mut self) -> Result<String> {
        if !self.active {
            return Err(AppError::InvalidState(
                "recognizer is not active".to_owned(),
            ));
        }
        self.active = false;
        Ok(self.transcript.clone())
    }

    fn cancel_session(&mut self) -> Result<()> {
        self.active = false;
        Ok(())
    }
}

const MODEL_SAMPLE_RATE: i32 = 16_000;
const DEFAULT_NUM_THREADS: i32 = 8;

/// Parakeet Unified through sherpa-onnx's buffered streaming RNNT runtime.
pub struct ParakeetRecognizer {
    recognizer: OnlineRecognizer,
    stream: Option<OnlineStream>,
    resampler: Option<LinearResampler>,
    input_sample_rate: Option<i32>,
}

impl ParakeetRecognizer {
    pub fn from_model_dir(model_dir: impl Into<PathBuf>) -> Result<Self> {
        let model_dir = model_dir.into();
        let encoder = required_model_file(&model_dir, &["encoder.int8.onnx", "encoder.onnx"])?;
        let decoder = required_model_file(&model_dir, &["decoder.int8.onnx", "decoder.onnx"])?;
        let joiner = required_model_file(&model_dir, &["joiner.int8.onnx", "joiner.onnx"])?;
        let tokens = required_model_file(&model_dir, &["tokens.txt"])?;

        let mut config = OnlineRecognizerConfig::default();
        config.model_config.transducer.encoder = Some(encoder.to_string_lossy().into_owned());
        config.model_config.transducer.decoder = Some(decoder.to_string_lossy().into_owned());
        config.model_config.transducer.joiner = Some(joiner.to_string_lossy().into_owned());
        config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
        config.model_config.provider = Some("cpu".to_owned());
        config.model_config.num_threads = DEFAULT_NUM_THREADS;
        config.enable_endpoint = true;
        config.decoding_method = Some("greedy_search".to_owned());

        let recognizer = OnlineRecognizer::create(&config).ok_or_else(|| {
            AppError::Unavailable(format!(
                "sherpa-onnx could not load Parakeet from {}",
                model_dir.display()
            ))
        })?;

        Ok(Self {
            recognizer,
            stream: None,
            resampler: None,
            input_sample_rate: None,
        })
    }

    fn feed_model(&self, samples: &[f32], sample_rate: i32) {
        let Some(stream) = self.stream.as_ref() else {
            return;
        };
        if !samples.is_empty() {
            stream.accept_waveform(sample_rate, samples);
        }
        while self.recognizer.is_ready(stream) {
            self.recognizer.decode(stream);
        }
    }

    fn flush_resampler(&self) {
        if let (Some(resampler), Some(_)) = (&self.resampler, &self.stream) {
            let samples = resampler.resample(&[], true);
            self.feed_model(&samples, MODEL_SAMPLE_RATE);
        }
    }
}

impl StreamingRecognizer for ParakeetRecognizer {
    fn start_session(&mut self) -> Result<()> {
        if self.stream.is_some() {
            return Err(AppError::InvalidState(
                "recognizer session is already active".to_owned(),
            ));
        }
        self.stream = Some(self.recognizer.create_stream());
        self.resampler = None;
        self.input_sample_rate = None;
        Ok(())
    }

    fn accept_audio(&mut self, sample_rate: i32, samples: &[f32]) -> Result<()> {
        if self.stream.is_none() {
            return Err(AppError::InvalidState(
                "recognizer session is not active".to_owned(),
            ));
        }
        if sample_rate <= 0 {
            return Err(AppError::Unavailable(
                "audio input reported an invalid sample rate".to_owned(),
            ));
        }
        if let Some(previous) = self.input_sample_rate {
            if previous != sample_rate {
                return Err(AppError::Unavailable(
                    "audio input sample rate changed during a dictation".to_owned(),
                ));
            }
        } else {
            self.input_sample_rate = Some(sample_rate);
            if sample_rate != MODEL_SAMPLE_RATE {
                self.resampler = Some(LinearResampler::create(sample_rate, MODEL_SAMPLE_RATE).ok_or_else(|| {
                    AppError::Unavailable(format!(
                        "could not resample audio from {sample_rate} Hz to {MODEL_SAMPLE_RATE} Hz"
                    ))
                })?);
            }
        }

        if let Some(resampler) = &self.resampler {
            let resampled = resampler.resample(samples, false);
            self.feed_model(&resampled, MODEL_SAMPLE_RATE);
        } else {
            self.feed_model(samples, sample_rate);
        }
        Ok(())
    }

    fn finish_session(&mut self) -> Result<String> {
        let Some(stream) = self.stream.as_ref() else {
            return Err(AppError::InvalidState(
                "recognizer session is not active".to_owned(),
            ));
        };
        self.flush_resampler();
        stream.input_finished();
        while self.recognizer.is_ready(stream) {
            self.recognizer.decode(stream);
        }
        let transcript = self
            .recognizer
            .get_result(stream)
            .map(|result| result.text)
            .unwrap_or_default();
        self.stream = None;
        self.resampler = None;
        self.input_sample_rate = None;
        Ok(transcript)
    }

    fn cancel_session(&mut self) -> Result<()> {
        self.stream = None;
        self.resampler = None;
        self.input_sample_rate = None;
        Ok(())
    }
}

fn required_model_file(model_dir: &Path, names: &[&str]) -> Result<PathBuf> {
    names
        .iter()
        .map(|name| model_dir.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            AppError::Unavailable(format!(
                "missing Parakeet model file in {} (expected one of: {})",
                model_dir.display(),
                names.join(", ")
            ))
        })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn reports_missing_model_files_before_loading_native_runtime() {
        let directory = tempdir().expect("temporary model directory");
        let error = match ParakeetRecognizer::from_model_dir(directory.path()) {
            Ok(_) => panic!("an empty directory must not load"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("missing Parakeet model file"));
    }
}
