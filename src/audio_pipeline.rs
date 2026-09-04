//! Worker-side audio cleanup and sample-rate conversion.

use nnnoiseless::DenoiseState;
use rubato::{Fft, FixedSync, Indexing, Resampler, audioadapter_buffers::direct::InterleavedSlice};

use crate::error::{AppError, Result};

pub const MODEL_SAMPLE_RATE: i32 = 16_000;
const DENOISE_SAMPLE_RATE: i32 = 48_000;
const RESAMPLE_CHUNK_FRAMES: usize = 1_024;
const PCM_SCALE: f32 = i16::MAX as f32;

/// Converts one input stream to the 16 kHz format used by VAD and ASR.
pub struct AudioPipeline {
    denoise: bool,
    input_sample_rate: Option<i32>,
    input_resampler: Option<StreamResampler>,
    model_resampler: Option<StreamResampler>,
    denoiser: Option<NoiseSuppressor>,
}

impl AudioPipeline {
    pub fn new(denoise: bool) -> Self {
        Self {
            denoise,
            input_sample_rate: None,
            input_resampler: None,
            model_resampler: None,
            denoiser: None,
        }
    }

    pub fn accept_audio(&mut self, sample_rate: i32, samples: &[f32]) -> Result<Vec<f32>> {
        self.prepare(sample_rate)?;
        if samples.is_empty() {
            return Ok(Vec::new());
        }

        if !self.denoise {
            return match &mut self.input_resampler {
                Some(resampler) => resampler.accept(samples),
                None => Ok(samples.to_vec()),
            };
        }

        let at_48khz = match &mut self.input_resampler {
            Some(resampler) => resampler.accept(samples)?,
            None => samples.to_vec(),
        };
        self.process_denoised(&at_48khz)
    }

    pub fn finish(&mut self) -> Result<Vec<f32>> {
        if self.input_sample_rate.is_none() {
            return Ok(Vec::new());
        }

        if !self.denoise {
            return match &mut self.input_resampler {
                Some(resampler) => resampler.finish(),
                None => Ok(Vec::new()),
            };
        }

        let mut output = Vec::new();
        let at_48khz = match &mut self.input_resampler {
            Some(resampler) => resampler.finish()?,
            None => Vec::new(),
        };
        output.extend(self.process_denoised(&at_48khz)?);

        let denoised_tail = self
            .denoiser
            .as_mut()
            .expect("denoiser exists after preparation")
            .finish();
        output.extend(self.resample_to_model_rate(&denoised_tail)?);
        if let Some(resampler) = &mut self.model_resampler {
            output.extend(resampler.finish()?);
        }
        Ok(output)
    }

    fn prepare(&mut self, sample_rate: i32) -> Result<()> {
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
            return Ok(());
        }

        self.input_sample_rate = Some(sample_rate);
        let intermediate_rate = if self.denoise {
            DENOISE_SAMPLE_RATE
        } else {
            MODEL_SAMPLE_RATE
        };
        if sample_rate != intermediate_rate {
            self.input_resampler = Some(StreamResampler::new(sample_rate, intermediate_rate)?);
        }
        if self.denoise {
            self.denoiser = Some(NoiseSuppressor::new());
            self.model_resampler = Some(StreamResampler::new(
                DENOISE_SAMPLE_RATE,
                MODEL_SAMPLE_RATE,
            )?);
        }
        Ok(())
    }

    fn process_denoised(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let denoised = self
            .denoiser
            .as_mut()
            .expect("denoiser exists after preparation")
            .accept(samples);
        self.resample_to_model_rate(&denoised)
    }

    fn resample_to_model_rate(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        self.model_resampler
            .as_mut()
            .expect("model resampler exists when denoise is enabled")
            .accept(samples)
    }
}

struct StreamResampler {
    input_rate: i32,
    output_rate: i32,
    inner: Fft<f32>,
    pending: Vec<f32>,
    input_buffer: Vec<f32>,
    output_buffer: Vec<f32>,
    delay_remaining: usize,
    total_input: usize,
    total_output: usize,
}

impl StreamResampler {
    fn new(input_rate: i32, output_rate: i32) -> Result<Self> {
        let inner = Fft::<f32>::new(
            input_rate as usize,
            output_rate as usize,
            RESAMPLE_CHUNK_FRAMES,
            1,
            FixedSync::Input,
        )
        .map_err(|error| {
            AppError::Unavailable(format!(
                "could not resample audio from {input_rate} Hz to {output_rate} Hz: {error}"
            ))
        })?;
        let input_frames = inner.input_frames_max();
        let output_frames = inner.output_frames_max();
        let delay_remaining = inner.output_delay();
        Ok(Self {
            input_rate,
            output_rate,
            inner,
            pending: Vec::with_capacity(input_frames * 2),
            input_buffer: vec![0.0; input_frames],
            output_buffer: vec![0.0; output_frames],
            delay_remaining,
            total_input: 0,
            total_output: 0,
        })
    }

    fn accept(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        self.total_input += samples.len();
        self.pending.extend_from_slice(samples);
        let mut output = Vec::new();
        loop {
            let needed = self.inner.input_frames_next();
            if self.pending.len() < needed {
                break;
            }
            self.input_buffer[..needed].copy_from_slice(&self.pending[..needed]);
            let consumed = self.process_chunk(None, &mut output)?;
            self.pending.drain(..consumed);
        }
        Ok(output)
    }

    fn finish(&mut self) -> Result<Vec<f32>> {
        let expected_output = ((self.total_input as u128 * self.output_rate as u128)
            .div_ceil(self.input_rate as u128)) as usize;
        let mut output = Vec::new();

        if !self.pending.is_empty() {
            let partial_len = self.pending.len();
            self.input_buffer.fill(0.0);
            self.input_buffer[..partial_len].copy_from_slice(&self.pending);
            self.process_chunk(Some(partial_len), &mut output)?;
            self.pending.clear();
        }

        while self.total_output < expected_output {
            self.input_buffer.fill(0.0);
            self.process_chunk(Some(0), &mut output)?;
        }
        let excess = self.total_output.saturating_sub(expected_output);
        if excess > 0 {
            output.truncate(output.len().saturating_sub(excess));
            self.total_output = expected_output;
        }
        Ok(output)
    }

    fn process_chunk(
        &mut self,
        partial_len: Option<usize>,
        output: &mut Vec<f32>,
    ) -> Result<usize> {
        let input_frames = self.inner.input_frames_next();
        let output_frames = self.inner.output_frames_next();
        let input =
            InterleavedSlice::new(&self.input_buffer, 1, input_frames).map_err(|error| {
                AppError::Unavailable(format!("could not prepare resampler input: {error}"))
            })?;
        let mut output_adapter =
            InterleavedSlice::new_mut(&mut self.output_buffer, 1, output_frames).map_err(
                |error| {
                    AppError::Unavailable(format!("could not prepare resampler output: {error}"))
                },
            )?;
        let indexing = partial_len.map(|length| Indexing::new().partial_len(length));
        let (consumed, produced) = self
            .inner
            .process_into_buffer(&input, &mut output_adapter, indexing.as_ref())
            .map_err(|error| AppError::Unavailable(format!("could not resample audio: {error}")))?;

        let skipped = self.delay_remaining.min(produced);
        self.delay_remaining -= skipped;
        output.extend_from_slice(&self.output_buffer[skipped..produced]);
        self.total_output += produced - skipped;
        Ok(consumed)
    }
}

struct NoiseSuppressor {
    state: Box<DenoiseState<'static>>,
    pending: Vec<f32>,
    first_frame: bool,
}

impl NoiseSuppressor {
    fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            pending: Vec::with_capacity(DenoiseState::FRAME_SIZE * 2),
            first_frame: true,
        }
    }

    fn accept(&mut self, samples: &[f32]) -> Vec<f32> {
        self.pending.extend_from_slice(samples);
        let mut output = Vec::new();
        while self.pending.len() >= DenoiseState::FRAME_SIZE {
            let frame: Vec<f32> = self
                .pending
                .drain(..DenoiseState::FRAME_SIZE)
                .map(|sample| sample.clamp(-1.0, 1.0) * PCM_SCALE)
                .collect();
            self.process_frame(&frame, &mut output);
        }
        output
    }

    fn finish(&mut self) -> Vec<f32> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let mut frame = std::mem::take(&mut self.pending);
        frame.resize(DenoiseState::FRAME_SIZE, 0.0);
        for sample in &mut frame {
            *sample = sample.clamp(-1.0, 1.0) * PCM_SCALE;
        }
        let mut output = Vec::with_capacity(DenoiseState::FRAME_SIZE);
        self.process_frame(&frame, &mut output);
        output
    }

    fn process_frame(&mut self, input: &[f32], output: &mut Vec<f32>) {
        let mut denoised = [0.0; DenoiseState::FRAME_SIZE];
        self.state.process_frame(&mut denoised, input);
        if self.first_frame {
            self.first_frame = false;
            return;
        }
        output.extend(denoised.into_iter().map(|sample| sample / PCM_SCALE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_kilohertz_audio_passes_through_unchanged() {
        let samples = vec![0.25, -0.5, 0.75];
        let mut pipeline = AudioPipeline::new(false);
        assert_eq!(
            pipeline
                .accept_audio(MODEL_SAMPLE_RATE, &samples)
                .expect("accept audio"),
            samples
        );
        assert!(pipeline.finish().expect("finish pipeline").is_empty());
    }

    #[test]
    fn resampling_preserves_the_expected_duration() {
        let mut pipeline = AudioPipeline::new(false);
        let input = vec![0.0; 44_100];
        let mut output = pipeline.accept_audio(44_100, &input).expect("accept audio");
        output.extend(pipeline.finish().expect("finish pipeline"));
        assert_eq!(output.len(), MODEL_SAMPLE_RATE as usize);
    }

    #[test]
    fn rejects_a_sample_rate_change() {
        let mut pipeline = AudioPipeline::new(false);
        pipeline.accept_audio(48_000, &[0.0]).expect("first rate");
        let error = pipeline
            .accept_audio(44_100, &[0.0])
            .expect_err("rate change must fail");
        assert!(error.to_string().contains("sample rate changed"));
    }

    #[test]
    fn denoise_output_stays_in_normalized_pcm_range() {
        let mut pipeline = AudioPipeline::new(true);
        let input = vec![0.25; 960];
        let mut output = pipeline.accept_audio(48_000, &input).expect("accept audio");
        output.extend(pipeline.finish().expect("finish pipeline"));
        assert!(output.iter().all(|sample| (-1.0..=1.0).contains(sample)));
    }
}
