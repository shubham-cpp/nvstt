use std::fmt::Display;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, Stream, StreamConfig};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::error::{AppError, Result};

const CAPTURE_QUEUE_SECONDS: usize = 5;
const MAX_CAPTURE_SECONDS: usize = 30 * 60;
const NOOP_SAMPLE_RATE: i32 = 16_000;

pub trait Recorder: Send {
    fn start(&mut self) -> Result<()>;
    /// Stop capturing. The live source stays readable so the worker can drain.
    fn stop(&mut self) -> Result<CaptureReport>;
    fn cancel(&mut self) -> Result<()>;
    /// Take the sole PCM consumer for this capture session.
    fn audio_source(&mut self) -> Result<AudioSource>;
}

#[derive(Debug, Default)]
pub struct NoopRecorder {
    active: bool,
    writer: Option<CaptureWriter>,
    source: Option<AudioSource>,
    integrity: Option<Arc<CaptureIntegrity>>,
}

impl Recorder for NoopRecorder {
    fn start(&mut self) -> Result<()> {
        if self.active {
            return Err(AppError::InvalidState(
                "recorder is already active".to_owned(),
            ));
        }
        let (writer, source) = capture_pair(
            NOOP_SAMPLE_RATE,
            1,
            NOOP_SAMPLE_RATE as usize * CAPTURE_QUEUE_SECONDS,
            NOOP_SAMPLE_RATE as usize * MAX_CAPTURE_SECONDS,
        );
        self.integrity = Some(Arc::clone(&source.integrity));
        self.writer = Some(writer);
        self.source = Some(source);
        self.active = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<CaptureReport> {
        if !self.active {
            return Err(AppError::InvalidState("recorder is not active".to_owned()));
        }
        self.active = false;
        self.writer.take();
        Ok(self
            .integrity
            .as_ref()
            .expect("active capture integrity")
            .report())
    }

    fn cancel(&mut self) -> Result<()> {
        self.writer.take();
        self.source.take();
        self.integrity.take();
        self.active = false;
        Ok(())
    }

    fn audio_source(&mut self) -> Result<AudioSource> {
        take_audio_source(&mut self.source)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureReport {
    pub dropped_samples: usize,
    pub backend_failed: bool,
    pub duration_exceeded: bool,
}

impl CaptureReport {
    pub(crate) fn failed(&self) -> bool {
        self.dropped_samples != 0 || self.backend_failed || self.duration_exceeded
    }
}

#[derive(Debug, Default)]
struct CaptureIntegrity {
    dropped_samples: AtomicUsize,
    duration_exceeded: AtomicBool,
    backend_failed: AtomicBool,
    backend_message: Mutex<Option<String>>,
}

impl CaptureIntegrity {
    fn report(&self) -> CaptureReport {
        CaptureReport {
            dropped_samples: self.dropped_samples.load(Ordering::SeqCst),
            backend_failed: self.backend_failed.load(Ordering::SeqCst),
            duration_exceeded: self.duration_exceeded.load(Ordering::SeqCst),
        }
    }

    fn record_backend_error(&self, error: &impl Display) {
        self.backend_failed.store(true, Ordering::SeqCst);
        if let Ok(mut message) = self.backend_message.try_lock() {
            *message = Some(error.to_string());
        }
    }

    fn result(&self) -> Result<()> {
        let mut reasons = Vec::new();
        let dropped = self.dropped_samples.load(Ordering::SeqCst);
        if dropped != 0 {
            reasons.push(format!(
                "{dropped} mono samples dropped by the capture queue"
            ));
        }
        if self.backend_failed.load(Ordering::SeqCst) {
            let detail = self
                .backend_message
                .try_lock()
                .ok()
                .and_then(|message| message.clone());
            reasons.push(match detail {
                Some(detail) => format!("backend capture error: {detail}"),
                None => "backend capture error".to_owned(),
            });
        }
        if self.duration_exceeded.load(Ordering::SeqCst) {
            reasons.push("audio capture exceeded the 30 minute limit".to_owned());
        }
        if reasons.is_empty() {
            Ok(())
        } else {
            Err(AppError::Unavailable(format!(
                "audio capture integrity failure: {}",
                reasons.join("; ")
            )))
        }
    }
}

#[derive(Debug)]
struct CaptureWriter {
    producer: Producer<f32>,
    integrity: Arc<CaptureIntegrity>,
    channels: usize,
    total_samples: usize,
    max_samples: usize,
}

impl CaptureWriter {
    fn accept_interleaved<T>(&mut self, data: &[T])
    where
        T: Sample,
        f32: cpal::FromSample<T>,
    {
        for frame in data.chunks(self.channels) {
            let beyond_limit = self.total_samples >= self.max_samples;
            self.total_samples = self.total_samples.saturating_add(1);
            if beyond_limit {
                self.integrity
                    .duration_exceeded
                    .store(true, Ordering::SeqCst);
                continue;
            }
            let sum = frame
                .iter()
                .map(|sample| f32::from_sample(*sample))
                .sum::<f32>();
            if self.producer.push(sum / frame.len() as f32).is_err() {
                self.integrity
                    .dropped_samples
                    .fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

/// A live mono PCM source consumed by the recognition worker.
#[derive(Debug)]
pub struct AudioSource {
    consumer: Consumer<f32>,
    sample_rate: i32,
    integrity: Arc<CaptureIntegrity>,
    #[cfg(test)]
    drain_step: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
}

fn capture_pair(
    sample_rate: i32,
    channels: usize,
    capacity: usize,
    max_samples: usize,
) -> (CaptureWriter, AudioSource) {
    let (producer, consumer) = RingBuffer::new(capacity);
    let integrity = Arc::new(CaptureIntegrity::default());
    (
        CaptureWriter {
            producer,
            integrity: Arc::clone(&integrity),
            channels,
            total_samples: 0,
            max_samples,
        },
        AudioSource {
            consumer,
            sample_rate,
            integrity,
            #[cfg(test)]
            drain_step: None,
        },
    )
}

impl AudioSource {
    pub(crate) fn sample_rate(&self) -> i32 {
        self.sample_rate
    }

    pub(crate) fn drain(&mut self) -> Result<Vec<f32>> {
        let available = self.consumer.slots();
        self.drain_snapshot(available)
    }

    fn drain_snapshot(&mut self, available: usize) -> Result<Vec<f32>> {
        let mut samples = Vec::with_capacity(available);
        for _ in 0..available {
            samples.push(self.consumer.pop().map_err(|_| {
                AppError::Unavailable("capture queue snapshot became unreadable".to_owned())
            })?);
            #[cfg(test)]
            if let Some((popped, resume)) = &self.drain_step {
                popped.send(()).expect("notify producer after a live pop");
                resume
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("producer must release the live drain");
            }
        }
        Ok(samples)
    }

    pub(crate) fn integrity_result(&self) -> Result<()> {
        self.integrity.result()
    }

    pub(crate) fn capture_report(&self) -> CaptureReport {
        self.integrity.report()
    }

    #[cfg(test)]
    pub(crate) fn test_source(sample_rate: i32, samples: Vec<f32>) -> Self {
        let (mut writer, source) = capture_pair(sample_rate, 1, samples.len().max(1), usize::MAX);
        writer.accept_interleaved(&samples);
        source
    }
}

/// Default microphone recorder backed by the user's default CPAL input device.
///
/// The callback only converts and stores PCM. It does not run inference or do
/// blocking I/O. The daemon drains the capture after the stop toggle.
#[derive(Default)]
pub struct CpalRecorder {
    stream: Option<Stream>,
    source: Option<AudioSource>,
    integrity: Option<Arc<CaptureIntegrity>>,
}

impl CpalRecorder {
    fn build_stream<T>(
        device: &cpal::Device,
        config: StreamConfig,
        mut writer: CaptureWriter,
    ) -> std::result::Result<Stream, String>
    where
        T: cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        let integrity = Arc::clone(&writer.integrity);
        let error_callback = move |error: cpal::Error| {
            integrity.record_backend_error(&error);
        };
        let data_callback = move |data: &[T], _info: &cpal::InputCallbackInfo| {
            writer.accept_interleaved(data);
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

        let (writer, source) = capture_pair(
            sample_rate,
            channels,
            sample_rate as usize * CAPTURE_QUEUE_SECONDS,
            sample_rate as usize * MAX_CAPTURE_SECONDS,
        );
        let integrity = Arc::clone(&source.integrity);
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let stream = match sample_format {
            SampleFormat::F32 => Self::build_stream::<f32>(&device, config, writer),
            SampleFormat::I8 => Self::build_stream::<i8>(&device, config, writer),
            SampleFormat::I16 => Self::build_stream::<i16>(&device, config, writer),
            SampleFormat::I24 => Self::build_stream::<cpal::I24>(&device, config, writer),
            SampleFormat::I32 => Self::build_stream::<i32>(&device, config, writer),
            SampleFormat::I64 => Self::build_stream::<i64>(&device, config, writer),
            SampleFormat::U8 => Self::build_stream::<u8>(&device, config, writer),
            SampleFormat::U16 => Self::build_stream::<u16>(&device, config, writer),
            SampleFormat::U24 => Self::build_stream::<cpal::U24>(&device, config, writer),
            SampleFormat::U32 => Self::build_stream::<u32>(&device, config, writer),
            SampleFormat::U64 => Self::build_stream::<u64>(&device, config, writer),
            SampleFormat::F64 => Self::build_stream::<f64>(&device, config, writer),
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
        self.source = Some(source);
        self.integrity = Some(integrity);
        Ok(())
    }

    fn stop(&mut self) -> Result<CaptureReport> {
        let stream = self
            .stream
            .take()
            .ok_or_else(|| AppError::InvalidState("recorder is not active".to_owned()))?;
        drop(stream);
        Ok(self
            .integrity
            .as_ref()
            .expect("active capture integrity")
            .report())
    }

    fn cancel(&mut self) -> Result<()> {
        self.stream.take();
        self.source.take();
        self.integrity.take();
        Ok(())
    }

    fn audio_source(&mut self) -> Result<AudioSource> {
        take_audio_source(&mut self.source)
    }
}

fn take_audio_source(source: &mut Option<AudioSource>) -> Result<AudioSource> {
    source.take().ok_or_else(|| {
        AppError::InvalidState("capture consumer is unavailable or already acquired".to_owned())
    })
}

#[cfg(test)]
pub(crate) struct TestCapture {
    writer: Option<CaptureWriter>,
    source: Option<AudioSource>,
    integrity: Arc<CaptureIntegrity>,
}

#[cfg(test)]
impl TestCapture {
    pub(crate) fn new() -> Self {
        let (writer, source) = capture_pair(16_000, 1, 2, 100);
        Self {
            integrity: Arc::clone(&source.integrity),
            writer: Some(writer),
            source: Some(source),
        }
    }

    pub(crate) fn push(&mut self, samples: &[f32]) {
        self.writer.as_mut().unwrap().accept_interleaved(samples);
    }

    pub(crate) fn backend_error(&self) {
        self.integrity
            .record_backend_error(&"injected backend failure");
    }

    pub(crate) fn close(&mut self) {
        self.writer.take();
    }

    pub(crate) fn report(&self) -> CaptureReport {
        self.integrity.report()
    }

    pub(crate) fn take_source(&mut self) -> AudioSource {
        self.source.take().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_has_only_one_consumer() {
        let mut recorder = NoopRecorder::default();
        recorder.start().unwrap();
        let _source = recorder.audio_source().unwrap();
        assert!(recorder.audio_source().is_err());
    }

    #[test]
    fn noop_stop_reports_queue_loss_without_failing_shutdown() {
        let mut recorder = NoopRecorder::default();
        recorder.start().unwrap();
        let samples = vec![0.25_f32; NOOP_SAMPLE_RATE as usize * (CAPTURE_QUEUE_SECONDS + 1)];
        recorder
            .writer
            .as_mut()
            .unwrap()
            .accept_interleaved(&samples);
        let report = recorder.stop().unwrap();
        assert!(report.dropped_samples > 0);
        assert!(!report.backend_failed);
    }

    #[test]
    fn queue_loss_is_counted_without_overwriting_audio() {
        let (mut writer, mut source) = capture_pair(16_000, 1, 2, 100);
        writer.accept_interleaved(&[1.0_f32, 2.0, 3.0]);
        assert_eq!(source.drain().unwrap(), vec![1.0, 2.0]);
        assert_eq!(source.capture_report().dropped_samples, 1);
        assert!(!source.capture_report().backend_failed);
        let message = source.integrity_result().unwrap_err().to_string();
        assert!(message.contains("1 mono samples dropped"));
    }

    #[test]
    fn duration_counts_input_that_did_not_fit_the_queue() {
        let (mut writer, source) = capture_pair(16_000, 1, 1, 2);
        writer.accept_interleaved(&[1.0_f32, 2.0, 3.0]);
        let message = source.integrity_result().unwrap_err().to_string();
        assert!(message.contains("1 mono samples dropped"));
        assert!(message.contains("30 minute limit"));
        assert!(source.capture_report().duration_exceeded);
        assert_eq!(writer.total_samples, 3);
    }

    #[test]
    fn duration_boundary_is_independent_of_queue_capacity() {
        for (input, expected, exceeded) in [
            (&[1.0_f32][..], &[1.0_f32][..], false),
            (&[1.0_f32, 2.0][..], &[1.0_f32, 2.0][..], false),
            (&[1.0_f32, 2.0, 3.0][..], &[1.0_f32, 2.0][..], true),
        ] {
            let (mut writer, mut source) = capture_pair(16_000, 1, 4, 2);
            writer.accept_interleaved(input);
            assert_eq!(writer.total_samples, input.len());
            drop(writer);
            assert_eq!(source.drain().unwrap(), expected);
            assert_eq!(source.integrity.dropped_samples.load(Ordering::SeqCst), 0);
            if exceeded {
                let message = source.integrity_result().unwrap_err().to_string();
                assert!(message.contains("30 minute limit"));
                assert!(!message.contains("mono samples dropped"));
            } else {
                source.integrity_result().unwrap();
            }
        }
    }

    #[test]
    fn backend_failure_survives_an_unavailable_diagnostic_slot() {
        let (_writer, source) = capture_pair(16_000, 1, 2, 100);
        let guard = source.integrity.backend_message.lock().unwrap();
        source
            .integrity
            .record_backend_error(&"injected device failure");
        drop(guard);
        let message = source.integrity_result().unwrap_err().to_string();
        assert!(message.contains("backend capture error"));
        assert!(source.capture_report().backend_failed);
        assert!(!message.contains("mono samples dropped"));
    }

    #[test]
    fn old_consumer_cannot_read_the_next_session() {
        let mut recorder = NoopRecorder::default();
        recorder.start().unwrap();
        let mut old = recorder.audio_source().unwrap();
        recorder
            .writer
            .as_mut()
            .unwrap()
            .accept_interleaved(&[1.0_f32, 2.0]);
        recorder.cancel().unwrap();
        recorder.start().unwrap();
        let mut new = recorder.audio_source().unwrap();
        recorder
            .writer
            .as_mut()
            .unwrap()
            .accept_interleaved(&[3.0_f32, 4.0]);
        assert_eq!(old.drain().unwrap(), vec![1.0, 2.0]);
        assert_eq!(new.drain().unwrap(), vec![3.0, 4.0]);
        assert!(old.drain().unwrap().is_empty());
        assert!(new.drain().unwrap().is_empty());
        assert!(!Arc::ptr_eq(&old.integrity, &new.integrity));
    }

    #[test]
    fn producer_shutdown_does_not_need_a_live_consumer() {
        let (mut writer, source) = capture_pair(16_000, 1, 1, 100);
        writer.accept_interleaved(&[1.0_f32, 2.0]);
        drop(source);
        assert!(writer.producer.is_abandoned());
        drop(writer);
    }

    #[test]
    fn concurrent_producer_preserves_order_without_exceeding_capacity() {
        use std::sync::Barrier;
        let (mut writer, mut source) = capture_pair(16_000, 1, 64, 1_000);
        let barrier = Arc::new(Barrier::new(2));
        let producer_barrier = Arc::clone(&barrier);
        let handle = std::thread::spawn(move || {
            for batch in 0..4 {
                let values = (batch * 64..(batch + 1) * 64)
                    .map(|n| n as f32)
                    .collect::<Vec<_>>();
                writer.accept_interleaved(&values);
                producer_barrier.wait();
                producer_barrier.wait();
            }
        });
        let mut actual = Vec::new();
        for _ in 0..4 {
            barrier.wait();
            actual.extend(source.drain().unwrap());
            barrier.wait();
        }
        handle.join().unwrap();
        assert_eq!(actual, (0..256).map(|n| n as f32).collect::<Vec<_>>());
        source.integrity_result().unwrap();
    }

    #[test]
    fn live_drain_stays_bounded_while_each_pop_allows_more_publication() {
        use std::sync::{Barrier, mpsc};
        use std::time::Duration;

        let (mut writer, mut source) = capture_pair(16_000, 1, 4, 100);
        writer.accept_interleaved(&[0.0_f32, 1.0, 2.0, 3.0]);
        let start = Arc::new(Barrier::new(2));
        let producer_start = Arc::clone(&start);
        let (popped_tx, popped_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        source.drain_step = Some((popped_tx, resume_rx));
        let producer = std::thread::spawn(move || {
            producer_start.wait();
            // Channel pairs form per-pop barriers with deadlock failure guards.
            for value in [4.0_f32, 5.0, 6.0, 7.0] {
                popped_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                writer.accept_interleaved(&[value]);
                resume_tx.send(()).unwrap();
            }
            drop(popped_rx);
            drop(resume_tx);
            // Keep the producer open until both actual drains have returned.
            finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        start.wait();
        let first = source.drain().unwrap();
        source.drain_step = None;
        let remaining = source.drain().unwrap();
        finished_tx.send(()).unwrap();
        producer.join().unwrap();
        assert_eq!(first, vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(remaining, vec![4.0, 5.0, 6.0, 7.0]);
        assert!(source.drain().unwrap().is_empty());
        source.integrity_result().unwrap();
    }

    #[test]
    fn final_sample_and_loss_are_visible_after_producer_join() {
        use std::sync::Barrier;
        let (mut writer, mut source) = capture_pair(16_000, 1, 1, 100);
        let barrier = Arc::new(Barrier::new(2));
        let producer_barrier = Arc::clone(&barrier);
        let handle = std::thread::spawn(move || {
            producer_barrier.wait();
            writer.accept_interleaved(&[7.0_f32, 8.0]);
        });
        barrier.wait();
        handle.join().unwrap();
        assert_eq!(source.drain().unwrap(), vec![7.0]);
        assert!(
            source
                .integrity_result()
                .unwrap_err()
                .to_string()
                .contains("1 mono samples dropped")
        );
    }

    #[test]
    fn entry_snapshot_does_not_expand_with_new_audio() {
        let (mut writer, mut source) = capture_pair(16_000, 1, 4, 100);
        writer.accept_interleaved(&[1.0_f32, 2.0]);
        let snapshot = source.consumer.slots();
        writer.accept_interleaved(&[3.0_f32, 4.0]);
        assert_eq!(source.drain_snapshot(snapshot).unwrap(), vec![1.0, 2.0]);
        assert_eq!(source.drain().unwrap(), vec![3.0, 4.0]);
    }

    #[test]
    fn noop_recorder_exposes_a_live_source_after_start() {
        let mut recorder = NoopRecorder::default();
        assert!(recorder.audio_source().is_err());
        recorder.start().expect("start noop recorder");
        let mut source = recorder.audio_source().expect("live source");
        assert_eq!(source.sample_rate(), NOOP_SAMPLE_RATE);
        recorder.stop().expect("stop noop recorder");
        assert!(source.drain().expect("drain").is_empty());
    }

    #[test]
    fn downmixes_interleaved_samples() {
        let (mut writer, mut source) = capture_pair(16_000, 2, 2, 100);
        writer.accept_interleaved(&[1.0_f32, -1.0, 0.5, 0.5]);
        assert_eq!(source.drain().unwrap(), vec![0.0, 0.5]);
    }
}
