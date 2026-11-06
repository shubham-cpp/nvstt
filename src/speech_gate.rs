//! Session-level speech detection.
//!
//! VAD classifies a session but never removes audio inside it. Quiet speech
//! and long pauses must reach the recognizer unchanged.

use std::path::Path;

use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};

use crate::{
    audio_pipeline::MODEL_SAMPLE_RATE,
    error::{AppError, Result},
};

pub const VAD_SAMPLE_RATE: i32 = MODEL_SAMPLE_RATE;
const VAD_FRAME_SAMPLES: usize = 512;
const MAX_SEGMENT_SECONDS: f32 = 30.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GateFlush {
    pub samples: Vec<f32>,
    pub speech_detected: bool,
}

/// Exact VAD framing. This is separate from the detector so edge cases do not
/// need an installed VAD model in unit tests.
struct GateInput {
    pending: Vec<f32>,
}

impl GateInput {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    fn accept_audio(&mut self, sample_rate: i32, samples: &[f32]) -> Result<Vec<Vec<f32>>> {
        if sample_rate != VAD_SAMPLE_RATE {
            return Err(AppError::Unavailable(format!(
                "speech gate requires {VAD_SAMPLE_RATE} Hz audio, got {sample_rate} Hz"
            )));
        }
        Ok(self.accept_16khz(samples))
    }

    fn finish_session(&mut self) -> Vec<Vec<f32>> {
        let mut frames = Vec::new();
        if !self.pending.is_empty() {
            let mut last_frame = std::mem::take(&mut self.pending);
            last_frame.resize(VAD_FRAME_SAMPLES, 0.0);
            frames.push(last_frame);
        }
        frames
    }

    fn reset(&mut self) {
        self.pending.clear();
    }

    fn accept_16khz(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.pending.extend_from_slice(samples);
        let complete = self.pending.len() / VAD_FRAME_SAMPLES * VAD_FRAME_SAMPLES;
        let frames = self.pending[..complete]
            .as_chunks::<VAD_FRAME_SAMPLES>()
            .0
            .iter()
            .map(|frame| frame.to_vec())
            .collect();
        self.pending.drain(..complete);
        frames
    }
}

/// Silero VAD that classifies a session without cutting the ASR waveform.
pub struct SpeechGate {
    detector: VoiceActivityDetector,
    input: GateInput,
    saw_speech: bool,
}

impl SpeechGate {
    pub fn from_model_path(model_path: impl AsRef<Path>) -> Result<Self> {
        let model_path = model_path.as_ref();
        if !model_path.is_file() {
            return Err(AppError::Unavailable(format!(
                "missing Silero VAD model at {}",
                model_path.display()
            )));
        }

        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(model_path.to_string_lossy().into_owned()),
                threshold: 0.5,
                min_silence_duration: 0.5,
                min_speech_duration: 0.25,
                window_size: VAD_FRAME_SAMPLES as i32,
                max_speech_duration: MAX_SEGMENT_SECONDS,
            },
            sample_rate: VAD_SAMPLE_RATE,
            num_threads: 1,
            // Keep VAD on the CPU. The ASR provider is separately selected by
            // the build, so CUDA loading remains visible and independent.
            provider: Some("cpu".to_owned()),
            debug: std::env::var_os("NVSTT_RECOGNIZER_DEBUG").is_some(),
            ..VadModelConfig::default()
        };
        let detector =
            VoiceActivityDetector::create(&config, MAX_SEGMENT_SECONDS).ok_or_else(|| {
                AppError::Unavailable(format!(
                    "sherpa-onnx could not load Silero VAD from {}",
                    model_path.display()
                ))
            })?;

        Ok(Self {
            detector,
            input: GateInput::new(),
            saw_speech: false,
        })
    }

    pub fn start_session(&mut self) {
        self.reset_session();
    }

    pub fn accept_audio(&mut self, sample_rate: i32, samples: &[f32]) -> Result<Vec<f32>> {
        let frames = self.input.accept_audio(sample_rate, samples)?;
        for frame in frames {
            self.accept_frame(&frame);
        }
        // Preserve original samples, including a partial detector frame.
        Ok(samples.to_vec())
    }

    pub fn finish_session(&mut self) -> GateFlush {
        for frame in self.input.finish_session() {
            self.accept_frame(&frame);
        }
        self.detector.flush();
        self.saw_speech |= !self.detector.is_empty();
        self.clear_segments();
        let speech_detected = self.saw_speech;
        self.reset_session();
        GateFlush {
            // All original samples were already forwarded. Detector padding
            // and queued segments must never be replayed into ASR.
            samples: Vec::new(),
            speech_detected,
        }
    }

    pub fn cancel_session(&mut self) {
        self.reset_session();
    }

    fn accept_frame(&mut self, frame: &[f32]) {
        self.detector.accept_waveform(frame);
        self.saw_speech |= self.detector.detected() || !self.detector.is_empty();
        self.clear_segments();
    }

    fn clear_segments(&self) {
        while !self.detector.is_empty() {
            self.detector.pop();
        }
    }

    fn reset_session(&mut self) {
        self.detector.reset();
        self.input.reset();
        self.saw_speech = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_arbitrary_chunk_sizes_and_keeps_the_last_sample() {
        let input_samples = (0..(VAD_FRAME_SAMPLES * 2 + 1))
            .map(|sample| sample as f32)
            .collect::<Vec<_>>();
        let mut input = GateInput::new();
        let mut frames = Vec::new();
        for chunk in input_samples.chunks(137) {
            frames.extend(
                input
                    .accept_audio(VAD_SAMPLE_RATE, chunk)
                    .expect("accept chunk"),
            );
        }
        frames.extend(input.finish_session());
        assert!(frames.iter().all(|frame| frame.len() == VAD_FRAME_SAMPLES));
        let output = frames.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(&output[..input_samples.len()], &input_samples);
        assert_eq!(output[input_samples.len()], 0.0);
    }

    #[test]
    fn rejects_non_16khz_input() {
        let mut input = GateInput::new();
        let error = input
            .accept_audio(48_000, &[0.25; VAD_FRAME_SAMPLES])
            .expect_err("wrong rate must fail");
        assert!(error.to_string().contains("requires 16000 Hz"));
    }

    #[test]
    fn input_reset_allows_a_repeated_session_after_cancel() {
        let mut input = GateInput::new();
        input
            .accept_audio(VAD_SAMPLE_RATE, &[1.0; 100])
            .expect("first session");
        input.reset();
        let frames = input
            .accept_audio(VAD_SAMPLE_RATE, &[2.0; VAD_FRAME_SAMPLES])
            .expect("second session");
        assert_eq!(frames, vec![vec![2.0; VAD_FRAME_SAMPLES]]);
    }
}
