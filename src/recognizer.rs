use std::path::{Path, PathBuf};

use sherpa_onnx::{LinearResampler, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};

use crate::{
    config::{Config, PARAKEET_UNIFIED_MODEL, RecognizerFamily},
    error::{AppError, Result},
    speech_gate::{SpeechGate, VAD_SAMPLE_RATE},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecognitionOutcome {
    Transcript(String),
    NoSpeech,
}

pub trait StreamingRecognizer: Send {
    fn start_session(&mut self) -> Result<()>;
    fn accept_audio(&mut self, sample_rate: i32, samples: &[f32]) -> Result<()>;
    fn finish_session(&mut self) -> Result<RecognitionOutcome>;
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

    fn finish_session(&mut self) -> Result<RecognitionOutcome> {
        Err(AppError::Unavailable(self.reason.clone()))
    }

    fn cancel_session(&mut self) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct StaticRecognizer {
    outcome: RecognitionOutcome,
    active: bool,
}

impl StaticRecognizer {
    pub fn new(transcript: impl Into<String>) -> Self {
        Self {
            outcome: RecognitionOutcome::Transcript(transcript.into()),
            active: false,
        }
    }

    pub fn no_speech() -> Self {
        Self {
            outcome: RecognitionOutcome::NoSpeech,
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

    fn finish_session(&mut self) -> Result<RecognitionOutcome> {
        if !self.active {
            return Err(AppError::InvalidState(
                "recognizer is not active".to_owned(),
            ));
        }
        self.active = false;
        Ok(self.outcome.clone())
    }

    fn cancel_session(&mut self) -> Result<()> {
        self.active = false;
        Ok(())
    }
}

const MODEL_SAMPLE_RATE: i32 = 16_000;
const DEFAULT_NUM_THREADS: i32 = 8;

#[cfg(all(feature = "cpu-runtime", feature = "cuda-runtime"))]
compile_error!("select either cpu-runtime or cuda-runtime, not both");

#[cfg(not(any(feature = "cpu-runtime", feature = "cuda-runtime")))]
compile_error!("select a native runtime feature");

#[cfg(feature = "cuda-runtime")]
const EXECUTION_PROVIDER: &str = "cuda";

#[cfg(not(feature = "cuda-runtime"))]
const EXECUTION_PROVIDER: &str = "cpu";

pub fn execution_provider() -> &'static str {
    EXECUTION_PROVIDER
}

/// A native, cache-aware online transducer recognizer.
///
/// Sherpa-ONNX selects the exact transducer implementation from the ONNX
/// files. This supports both buffered Parakeet Unified and cache-aware
/// Nemotron without a Python sidecar or a fallback provider.
pub struct OnlineTransducerRecognizer {
    recognizer: OnlineRecognizer,
    stream: Option<OnlineStream>,
    resampler: Option<LinearResampler>,
    input_sample_rate: Option<i32>,
}

impl OnlineTransducerRecognizer {
    pub fn from_model_dir(model_dir: impl Into<PathBuf>, model_name: &str) -> Result<Self> {
        let model_dir = model_dir.into();
        let encoder = required_model_file(
            &model_dir,
            model_name,
            &["encoder.int8.onnx", "encoder.onnx"],
        )?;
        let decoder = required_model_file(
            &model_dir,
            model_name,
            &["decoder.int8.onnx", "decoder.onnx"],
        )?;
        let joiner =
            required_model_file(&model_dir, model_name, &["joiner.int8.onnx", "joiner.onnx"])?;
        let tokens = required_model_file(&model_dir, model_name, &["tokens.txt"])?;

        let mut config = OnlineRecognizerConfig::default();
        config.model_config.transducer.encoder = Some(encoder.to_string_lossy().into_owned());
        config.model_config.transducer.decoder = Some(decoder.to_string_lossy().into_owned());
        config.model_config.transducer.joiner = Some(joiner.to_string_lossy().into_owned());
        config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
        config.model_config.provider = Some(EXECUTION_PROVIDER.to_owned());
        config.model_config.num_threads = DEFAULT_NUM_THREADS;
        config.model_config.debug = std::env::var_os("NVSTT_RECOGNIZER_DEBUG").is_some();
        config.enable_endpoint = true;
        config.decoding_method = Some("greedy_search".to_owned());

        let recognizer = OnlineRecognizer::create(&config).ok_or_else(|| {
            AppError::Unavailable(format!(
                "sherpa-onnx could not load {model_name} from {} using the {EXECUTION_PROVIDER} execution provider",
                model_dir.display(),
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

impl StreamingRecognizer for OnlineTransducerRecognizer {
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

    fn finish_session(&mut self) -> Result<RecognitionOutcome> {
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
        Ok(RecognitionOutcome::Transcript(transcript))
    }

    fn cancel_session(&mut self) -> Result<()> {
        self.stream = None;
        self.resampler = None;
        self.input_sample_rate = None;
        Ok(())
    }
}

/// Compatibility facade for direct Parakeet benchmarks and local rollback.
pub struct ParakeetRecognizer(OnlineTransducerRecognizer);

impl ParakeetRecognizer {
    pub fn from_model_dir(model_dir: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self(OnlineTransducerRecognizer::from_model_dir(
            model_dir,
            PARAKEET_UNIFIED_MODEL,
        )?))
    }
}

impl StreamingRecognizer for ParakeetRecognizer {
    fn start_session(&mut self) -> Result<()> {
        self.0.start_session()
    }

    fn accept_audio(&mut self, sample_rate: i32, samples: &[f32]) -> Result<()> {
        self.0.accept_audio(sample_rate, samples)
    }

    fn finish_session(&mut self) -> Result<RecognitionOutcome> {
        self.0.finish_session()
    }

    fn cancel_session(&mut self) -> Result<()> {
        self.0.cancel_session()
    }
}

struct VadGatedRecognizer {
    recognizer: Box<dyn StreamingRecognizer>,
    gate: SpeechGate,
    active: bool,
}

impl VadGatedRecognizer {
    fn new(recognizer: Box<dyn StreamingRecognizer>, vad_path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            recognizer,
            gate: SpeechGate::from_model_path(vad_path)?,
            active: false,
        })
    }
}

impl StreamingRecognizer for VadGatedRecognizer {
    fn start_session(&mut self) -> Result<()> {
        if self.active {
            return Err(AppError::InvalidState(
                "recognizer session is already active".to_owned(),
            ));
        }
        self.gate.start_session();
        if let Err(error) = self.recognizer.start_session() {
            self.gate.cancel_session();
            return Err(error);
        }
        self.active = true;
        Ok(())
    }

    fn accept_audio(&mut self, sample_rate: i32, samples: &[f32]) -> Result<()> {
        if !self.active {
            return Err(AppError::InvalidState(
                "recognizer session is not active".to_owned(),
            ));
        }
        let gated = self.gate.accept_audio(sample_rate, samples)?;
        if !gated.is_empty() {
            self.recognizer.accept_audio(VAD_SAMPLE_RATE, &gated)?;
        }
        Ok(())
    }

    fn finish_session(&mut self) -> Result<RecognitionOutcome> {
        if !self.active {
            return Err(AppError::InvalidState(
                "recognizer session is not active".to_owned(),
            ));
        }
        let gate_flush = self.gate.finish_session();
        self.active = false;
        if !gate_flush.speech_detected {
            self.recognizer.cancel_session()?;
            return Ok(RecognitionOutcome::NoSpeech);
        }
        if !gate_flush.samples.is_empty() {
            self.recognizer
                .accept_audio(VAD_SAMPLE_RATE, &gate_flush.samples)?;
        }
        self.recognizer.finish_session()
    }

    fn cancel_session(&mut self) -> Result<()> {
        self.gate.cancel_session();
        self.active = false;
        self.recognizer.cancel_session()
    }
}

/// Construct the pinned native recognizer for the selected model directory.
pub fn create_recognizer(
    config: &Config,
    model_dir: impl Into<PathBuf>,
) -> Result<Box<dyn StreamingRecognizer>> {
    config.validate()?;
    let model_dir = model_dir.into();
    let recognizer: Box<dyn StreamingRecognizer> = match config.recognizer_family() {
        Some(RecognizerFamily::OnlineTransducer) => Box::new(
            OnlineTransducerRecognizer::from_model_dir(&model_dir, &config.model)?,
        ),
        None => {
            return Err(AppError::Config(format!(
                "unsupported recognizer family for '{}'",
                config.model
            )));
        }
    };

    if config.speech_gate {
        Ok(Box::new(VadGatedRecognizer::new(
            recognizer,
            model_dir.join("silero_vad.onnx"),
        )?))
    } else {
        Ok(recognizer)
    }
}

fn required_model_file(model_dir: &Path, model_name: &str, names: &[&str]) -> Result<PathBuf> {
    names
        .iter()
        .map(|name| model_dir.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            AppError::Unavailable(format!(
                "missing {model_name} model file in {} (expected one of: {})",
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
        let error = match OnlineTransducerRecognizer::from_model_dir(
            directory.path(),
            "nemotron-speech-streaming-en-0.6b",
        ) {
            Ok(_) => panic!("an empty directory must not load"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("missing nemotron-speech-streaming-en-0.6b model file")
        );
    }

    #[test]
    fn static_recognizer_can_return_no_speech() {
        let mut recognizer = StaticRecognizer::no_speech();
        recognizer.start_session().expect("start static session");
        assert_eq!(
            recognizer.finish_session().expect("finish static session"),
            RecognitionOutcome::NoSpeech
        );
    }
}
