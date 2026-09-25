//! A local, bounded speech gate for online recognition.
//!
//! The detector sees 16 kHz audio in 512-sample frames. The recognizer sees
//! only detected speech, with enough preceding audio to retain word starts.

use std::{collections::VecDeque, path::Path};

use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};

use crate::{
    audio_pipeline::MODEL_SAMPLE_RATE,
    error::{AppError, Result},
};

pub const VAD_SAMPLE_RATE: i32 = MODEL_SAMPLE_RATE;
const VAD_FRAME_SAMPLES: usize = 512;
const PRE_ROLL_SAMPLES: usize = 6_400;
const SILENCE_BRIDGE_SAMPLES: usize = 3_200;
const MAX_SEGMENT_SECONDS: f32 = 30.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GateFlush {
    pub samples: Vec<f32>,
    pub speech_detected: bool,
}

/// The pure audio-routing state. Keeping it separate makes boundary behavior
/// testable without loading the native VAD model.
#[derive(Debug, Default)]
struct GateState {
    pre_roll: VecDeque<f32>,
    active: bool,
    saw_speech: bool,
}

impl GateState {
    fn accept_frame(&mut self, frame: &[f32], detected: bool) -> Vec<f32> {
        self.push_pre_roll(frame);

        let output = match (self.active, detected) {
            (false, false) => Vec::new(),
            (false, true) => {
                let mut output = Vec::with_capacity(
                    self.pre_roll.len()
                        + if self.saw_speech {
                            SILENCE_BRIDGE_SAMPLES
                        } else {
                            0
                        },
                );
                if self.saw_speech {
                    output.resize(SILENCE_BRIDGE_SAMPLES, 0.0);
                }
                output.extend(self.pre_roll.iter().copied());
                self.active = true;
                self.saw_speech = true;
                output
            }
            (true, true) => frame.to_vec(),
            (true, false) => {
                // The VAD turns off only after its configured silence. Keep
                // this boundary frame so a final word is not clipped.
                self.active = false;
                frame.to_vec()
            }
        };
        if !output.is_empty() {
            self.pre_roll.clear();
        }
        output
    }

    fn push_pre_roll(&mut self, samples: &[f32]) {
        self.pre_roll.extend(samples.iter().copied());
        let excess = self.pre_roll.len().saturating_sub(PRE_ROLL_SAMPLES);
        self.pre_roll.drain(..excess);
    }

    fn reset(&mut self) {
        self.pre_roll.clear();
        self.active = false;
        self.saw_speech = false;
    }
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
        let mut frames = Vec::new();
        while self.pending.len() >= VAD_FRAME_SAMPLES {
            frames.push(self.pending.drain(..VAD_FRAME_SAMPLES).collect());
        }
        frames
    }
}

/// Silero VAD that gates 16 kHz input chunks before a recognizer.
pub struct SpeechGate {
    detector: VoiceActivityDetector,
    input: GateInput,
    state: GateState,
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
            state: GateState::default(),
        })
    }

    pub fn start_session(&mut self) {
        self.reset_session();
    }

    pub fn accept_audio(&mut self, sample_rate: i32, samples: &[f32]) -> Result<Vec<f32>> {
        let frames = self.input.accept_audio(sample_rate, samples)?;
        Ok(frames
            .iter()
            .flat_map(|frame| self.accept_frame(frame))
            .collect())
    }

    pub fn finish_session(&mut self) -> GateFlush {
        let mut output = self
            .input
            .finish_session()
            .iter()
            .flat_map(|frame| self.accept_frame(frame))
            .collect::<Vec<_>>();

        self.detector.flush();
        if !self.state.saw_speech {
            while let Some(samples) = self
                .detector
                .front()
                .map(|segment| segment.samples().to_vec())
            {
                output.extend(samples);
                self.state.saw_speech = true;
                self.detector.pop();
            }
        }
        self.clear_segments();
        let speech_detected = self.state.saw_speech;
        self.reset_session();
        GateFlush {
            samples: output,
            speech_detected,
        }
    }

    pub fn cancel_session(&mut self) {
        self.reset_session();
    }

    fn accept_frame(&mut self, frame: &[f32]) -> Vec<f32> {
        self.detector.accept_waveform(frame);
        let output = self.state.accept_frame(frame, self.detector.detected());
        self.clear_segments();
        output
    }

    fn clear_segments(&self) {
        while !self.detector.is_empty() {
            self.detector.pop();
        }
    }

    fn reset_session(&mut self) {
        self.detector.reset();
        self.input.reset();
        self.state.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: f32) -> Vec<f32> {
        vec![value; VAD_FRAME_SAMPLES]
    }

    #[test]
    fn accepts_speech_at_the_first_frame() {
        let mut state = GateState::default();
        let first = frame(1.0);
        assert_eq!(state.accept_frame(&first, true), first);
        assert!(state.saw_speech);
    }

    #[test]
    fn keeps_pre_roll_before_detected_speech() {
        let mut state = GateState::default();
        let silence = frame(0.0);
        let speech = frame(1.0);
        state.accept_frame(&silence, false);
        let output = state.accept_frame(&speech, true);
        assert_eq!(&output[..VAD_FRAME_SAMPLES], &silence);
        assert_eq!(&output[VAD_FRAME_SAMPLES..], &speech);
    }

    #[test]
    fn no_speech_does_not_emit_samples() {
        let mut state = GateState::default();
        for _ in 0..20 {
            assert!(state.accept_frame(&frame(0.0), false).is_empty());
        }
        assert!(!state.saw_speech);
    }

    #[test]
    fn long_pauses_keep_pre_roll_bounded() {
        let mut state = GateState::default();
        for _ in 0..20 {
            state.accept_frame(&frame(0.0), false);
        }
        let output = state.accept_frame(&frame(1.0), true);
        assert_eq!(output.len(), PRE_ROLL_SAMPLES);
    }

    #[test]
    fn keeps_the_last_boundary_frame_after_speech() {
        let mut state = GateState::default();
        let speech = frame(1.0);
        let last = frame(0.25);
        state.accept_frame(&speech, true);
        assert_eq!(state.accept_frame(&last, false), last);
        assert!(!state.active);
    }

    #[test]
    fn adds_a_short_silence_bridge_between_regions() {
        let mut state = GateState::default();
        let speech = frame(1.0);
        let silence = frame(0.0);
        state.accept_frame(&speech, true);
        state.accept_frame(&silence, false);
        let output = state.accept_frame(&speech, true);
        let expected = [vec![0.0; SILENCE_BRIDGE_SAMPLES], speech].concat();
        assert_eq!(output, expected);
    }

    #[test]
    fn restart_never_replays_emitted_frames() {
        let mut state = GateState::default();
        let first = frame(1.0);
        let boundary = frame(2.0);
        let second = frame(3.0);
        assert_eq!(state.accept_frame(&first, true), first);
        assert_eq!(state.accept_frame(&boundary, false), boundary);
        let output = state.accept_frame(&second, true);
        let expected = [vec![0.0; SILENCE_BRIDGE_SAMPLES], second].concat();
        assert_eq!(output, expected);
    }

    #[test]
    fn repeated_transitions_preserve_only_unemitted_preroll() {
        let mut state = GateState::default();
        let mut nonzero = Vec::new();
        for (value, detected) in [
            (1.0, true),
            (2.0, false),
            (3.0, false),
            (4.0, true),
            (5.0, false),
            (6.0, true),
        ] {
            nonzero.extend(
                state.accept_frame(&frame(value), detected)
                    .into_iter()
                    .filter(|sample| *sample != 0.0),
            );
        }
        let expected = (1..=6)
            .flat_map(|value| frame(value as f32))
            .collect::<Vec<_>>();
        assert_eq!(nonzero, expected);
    }

    #[test]
    fn reset_clears_a_repeated_session() {
        let mut state = GateState::default();
        state.accept_frame(&frame(1.0), true);
        state.reset();
        assert!(!state.active);
        assert!(!state.saw_speech);
        assert!(state.pre_roll.is_empty());
    }

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
    fn trailing_partial_boundary_is_emitted_once() {
        let mut input = GateInput::new();
        let mut state = GateState::default();
        let frames = input.accept_16khz(&vec![0.5; VAD_FRAME_SAMPLES + 13]);
        assert_eq!(frames.len(), 1);
        assert_eq!(state.accept_frame(&frames[0], true), frame(0.5));
        let tail = input.finish_session();
        assert_eq!(tail.len(), 1);
        assert_eq!(&tail[0][..13], &[0.5; 13]);
        assert!(tail[0][13..].iter().all(|sample| *sample == 0.0));
        assert_eq!(state.accept_frame(&tail[0], false), tail[0]);
        assert!(state.accept_frame(&frame(0.0), false).is_empty());
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
