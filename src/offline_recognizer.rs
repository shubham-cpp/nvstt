//! Full-utterance Parakeet TDT recognition. Audio is bounded before native inference.
use std::path::Path;

use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig};

use crate::{
    audio_pipeline::MODEL_SAMPLE_RATE,
    error::{AppError, Result},
    recognizer::{
        RecognitionOutcome, StreamingRecognizer, execution_provider, required_model_file,
    },
};

// Attention memory grows with utterance length. Fail explicitly and retain
// the original recording for recovery rather than exhausting the process.
const MAX_AUDIO_SAMPLES: usize = MODEL_SAMPLE_RATE as usize * 120;

pub(crate) struct ParakeetTdtRecognizer {
    recognizer: OfflineRecognizer,
    audio: Option<Vec<f32>>,
}

impl ParakeetTdtRecognizer {
    pub(crate) fn new(directory: &Path, model_name: &str, num_threads: i32) -> Result<Self> {
        let file = |names: &[&str]| -> Result<Option<String>> {
            Ok(Some(
                required_model_file(directory, model_name, names)?
                    .to_string_lossy()
                    .into_owned(),
            ))
        };
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.transducer.encoder = file(&["encoder.int8.onnx", "encoder.onnx"])?;
        config.model_config.transducer.decoder = file(&["decoder.int8.onnx", "decoder.onnx"])?;
        config.model_config.transducer.joiner = file(&["joiner.int8.onnx", "joiner.onnx"])?;
        config.model_config.tokens = file(&["tokens.txt"])?;
        config.model_config.model_type = Some("nemo_transducer".to_owned());
        config.model_config.provider = Some(execution_provider().to_owned());
        config.model_config.num_threads = num_threads;
        config.feat_config.feature_dim = 128;
        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| AppError::Unavailable(format!("could not load {model_name}")))?;
        Ok(Self {
            recognizer,
            audio: None,
        })
    }
}

impl StreamingRecognizer for ParakeetTdtRecognizer {
    fn start_session(&mut self) -> Result<()> {
        if self.audio.is_some() {
            return Err(AppError::InvalidState(
                "recognizer session is already active".into(),
            ));
        }
        self.audio = Some(Vec::new());
        Ok(())
    }

    fn accept_audio(&mut self, sample_rate: i32, samples: &[f32]) -> Result<()> {
        let audio = self
            .audio
            .as_mut()
            .ok_or_else(|| AppError::InvalidState("recognizer session is not active".into()))?;
        if sample_rate != MODEL_SAMPLE_RATE {
            return Err(AppError::Unavailable(
                "Parakeet TDT requires 16000 Hz audio".into(),
            ));
        }
        if samples.len() > MAX_AUDIO_SAMPLES.saturating_sub(audio.len()) {
            return Err(AppError::Unavailable(
                "Parakeet TDT dictation exceeds 120 seconds; use Parakeet Unified for longer dictations".into(),
            ));
        }
        audio.extend_from_slice(samples);
        Ok(())
    }

    fn finish_session(&mut self) -> Result<RecognitionOutcome> {
        let audio = self
            .audio
            .take()
            .ok_or_else(|| AppError::InvalidState("recognizer session is not active".into()))?;
        if audio.is_empty() {
            return Ok(RecognitionOutcome::NoSpeech);
        }
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(MODEL_SAMPLE_RATE, &audio);
        self.recognizer.decode(&stream);
        let result = stream
            .get_result()
            .ok_or_else(|| AppError::Unavailable("recognizer returned an invalid result".into()))?;
        if result.text.trim().is_empty() {
            Ok(RecognitionOutcome::NoSpeech)
        } else {
            Ok(RecognitionOutcome::Transcript(result.text))
        }
    }

    fn cancel_session(&mut self) -> Result<()> {
        self.audio = None;
        Ok(())
    }
}
