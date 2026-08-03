use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, Stream, StreamConfig};
use tracing::warn;

use crate::error::{AppError, Result};

const MAX_CAPTURE_SECONDS: usize = 30 * 60;

/// A mono PCM capture returned when a recording session stops.
#[derive(Clone, Debug, Default)]
pub struct RecordedAudio {
    pub sample_rate: i32,
    pub samples: Vec<f32>,
}

pub trait Recorder: Send {
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<Option<RecordedAudio>>;
    fn cancel(&mut self) -> Result<()>;

    fn audio_source(&self) -> Option<AudioSource> {
        None
    }
}

#[derive(Debug, Default)]
pub struct NoopRecorder {
    active: bool,
}

impl Recorder for NoopRecorder {
    fn start(&mut self) -> Result<()> {
        if self.active {
            return Err(AppError::InvalidState(
                "recorder is already active".to_owned(),
            ));
        }
        self.active = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<Option<RecordedAudio>> {
        if !self.active {
            return Err(AppError::InvalidState("recorder is not active".to_owned()));
        }
        self.active = false;
        Ok(None)
    }

    fn cancel(&mut self) -> Result<()> {
        self.active = false;
        Ok(())
    }
}

#[derive(Debug)]
struct CaptureState {
    sample_rate: i32,
    channels: usize,
    samples: VecDeque<f32>,
    total_samples: usize,
    overflowed: bool,
}

impl CaptureState {
    fn new(sample_rate: i32, channels: usize) -> Self {
        Self {
            sample_rate,
            channels,
            samples: VecDeque::new(),
            total_samples: 0,
            overflowed: false,
        }
    }
}

/// A live mono PCM source consumed by the recognition worker.
#[derive(Clone, Debug)]
pub struct AudioSource {
    capture: Arc<Mutex<CaptureState>>,
}

impl AudioSource {
    pub(crate) fn sample_rate(&self) -> i32 {
        self.capture
            .lock()
            .map(|state| state.sample_rate)
            .unwrap_or_default()
    }

    pub(crate) fn drain(&self) -> Result<Vec<f32>> {
        let mut state = self
            .capture
            .lock()
            .map_err(|_| AppError::Unavailable("audio capture lock was poisoned".to_owned()))?;
        Ok(state.samples.drain(..).collect())
    }

    pub(crate) fn overflowed(&self) -> Result<bool> {
        let state = self
            .capture
            .lock()
            .map_err(|_| AppError::Unavailable("audio capture lock was poisoned".to_owned()))?;
        Ok(state.overflowed)
    }

    #[cfg(test)]
    pub(crate) fn test_source(sample_rate: i32, samples: Vec<f32>) -> Self {
        let total_samples = samples.len();
        Self {
            capture: Arc::new(Mutex::new(CaptureState {
                sample_rate,
                channels: 1,
                samples: samples.into(),
                total_samples,
                overflowed: false,
            })),
        }
    }
}

/// Default microphone recorder backed by the user's default CPAL input device.
///
/// The callback only converts and stores PCM. It does not run inference or do
/// blocking I/O. The daemon drains the capture after the stop toggle.
#[derive(Default)]
pub struct CpalRecorder {
    stream: Option<Stream>,
    capture: Option<Arc<Mutex<CaptureState>>>,
    stream_error: Option<Arc<Mutex<Option<String>>>>,
}

impl CpalRecorder {
    fn build_stream<T>(
        device: &cpal::Device,
        config: StreamConfig,
        capture: Arc<Mutex<CaptureState>>,
        stream_error: Arc<Mutex<Option<String>>>,
    ) -> std::result::Result<Stream, String>
    where
        T: cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        let error_callback = move |error: cpal::Error| {
            if let Ok(mut slot) = stream_error.lock() {
                *slot = Some(error.to_string());
            }
        };
        let data_callback = move |data: &[T], _info: &cpal::InputCallbackInfo| {
            append_interleaved(data, &capture);
        };
        device
            .build_input_stream(config, data_callback, error_callback, None)
            .map_err(|error| error.to_string())
    }
}

impl Recorder for CpalRecorder {
    fn start(&mut self) -> Result<()> {
        if self.stream.is_some() {
            return Err(AppError::InvalidState(
                "recorder is already active".to_owned(),
            ));
        }

        let host = cpal::default_host();
        let device = host.default_input_device().ok_or_else(|| {
            AppError::Unavailable("no default audio input device is available".to_owned())
        })?;
        let supported = device.default_input_config().map_err(|error| {
            AppError::Unavailable(format!(
                "could not read the default input configuration: {error}"
            ))
        })?;
        let sample_rate = supported.sample_rate() as i32;
        let channels = supported.channels() as usize;
        if channels == 0 || sample_rate <= 0 {
            return Err(AppError::Unavailable(
                "audio input reported an invalid channel or sample rate".to_owned(),
            ));
        }

        let capture = Arc::new(Mutex::new(CaptureState::new(sample_rate, channels)));
        let stream_error = Arc::new(Mutex::new(None));
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let stream = match sample_format {
            SampleFormat::F32 => Self::build_stream::<f32>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::I8 => Self::build_stream::<i8>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::I16 => Self::build_stream::<i16>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::I24 => Self::build_stream::<cpal::I24>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::I32 => Self::build_stream::<i32>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::I64 => Self::build_stream::<i64>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::U8 => Self::build_stream::<u8>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::U16 => Self::build_stream::<u16>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::U24 => Self::build_stream::<cpal::U24>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::U32 => Self::build_stream::<u32>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::U64 => Self::build_stream::<u64>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            SampleFormat::F64 => Self::build_stream::<f64>(
                &device,
                config,
                Arc::clone(&capture),
                Arc::clone(&stream_error),
            ),
            format => {
                return Err(AppError::Unavailable(format!(
                    "unsupported audio sample format: {format}"
                )));
            }
        }
        .map_err(|error| {
            AppError::Unavailable(format!("could not build audio input stream: {error}"))
        })?;

        stream.play().map_err(|error| {
            AppError::Unavailable(format!("could not start audio input stream: {error}"))
        })?;
        self.stream = Some(stream);
        self.capture = Some(capture);
        self.stream_error = Some(stream_error);
        Ok(())
    }

    fn stop(&mut self) -> Result<Option<RecordedAudio>> {
        let Some(stream) = self.stream.take() else {
            return Err(AppError::InvalidState("recorder is not active".to_owned()));
        };
        drop(stream);
        if let Some(error) = self
            .stream_error
            .take()
            .and_then(|slot| slot.lock().ok().and_then(|error| error.clone()))
        {
            warn!(error = %error, "audio input stream reported an error");
        }
        Ok(None)
    }

    fn cancel(&mut self) -> Result<()> {
        self.stream.take();
        self.capture.take();
        self.stream_error.take();
        Ok(())
    }

    fn audio_source(&self) -> Option<AudioSource> {
        self.capture.as_ref().map(|capture| AudioSource {
            capture: Arc::clone(capture),
        })
    }
}

fn append_interleaved<T>(data: &[T], capture: &Arc<Mutex<CaptureState>>)
where
    T: Sample,
    f32: cpal::FromSample<T>,
{
    let Ok(mut state) = capture.try_lock() else {
        return;
    };
    let max_samples = state.sample_rate as usize * MAX_CAPTURE_SECONDS;
    for frame in data.chunks(state.channels) {
        if state.total_samples >= max_samples {
            state.overflowed = true;
            break;
        }
        let sum = frame
            .iter()
            .map(|sample| f32::from_sample(*sample))
            .sum::<f32>();
        state.samples.push_back(sum / frame.len() as f32);
        state.total_samples += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmixes_interleaved_samples() {
        let capture = Arc::new(Mutex::new(CaptureState::new(16_000, 2)));
        append_interleaved(&[1.0_f32, -1.0, 0.5, 0.5], &capture);
        let state = capture.lock().expect("capture lock");
        assert_eq!(
            state.samples.iter().copied().collect::<Vec<_>>(),
            vec![0.0, 0.5]
        );
    }
}
