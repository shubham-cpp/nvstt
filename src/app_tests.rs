use std::sync::{Arc, Mutex};

use tempfile::{TempDir, tempdir};

use super::*;

#[test]
fn vocabulary_edits_apply_between_dictations_and_do_not_leak_removed_rules() {
    use crate::vocabulary::Vocabulary;
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
    let path = directory.path().join("vocabulary.json");
    daemon.vocabulary_path = Some(path.clone());
    daemon.recognizer = Some(Box::new(StaticRecognizer::new(
        "para keet has twenty one files",
    )));
    Vocabulary::add(&path, "para keet", "Parakeet").unwrap();
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    Vocabulary::add(&path, "para keet", "Changed").unwrap();
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert_eq!(effects.lock().unwrap().sent[0], "Parakeet has 21 files");
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert_eq!(effects.lock().unwrap().sent[1], "Changed has 21 files");
    Vocabulary::remove(&path, "para keet").unwrap();
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert_eq!(effects.lock().unwrap().sent[2], "para keet has 21 files");
    fs::write(&path, "broken json").unwrap();
    assert!(!daemon.handle(IpcRequest::Toggle).is_ok());
    assert_eq!(saved_recordings(&directory).len(), 3);
    assert_eq!(effects.lock().unwrap().sent.len(), 3);
}

#[test]
fn finalization_errors_reset_recognizer_before_next_dictation() {
    struct StickyFailure {
        active: bool,
        fail_audio: bool,
    }
    impl StreamingRecognizer for StickyFailure {
        fn start_session(&mut self) -> Result<()> {
            if self.active {
                return Err(AppError::InvalidState("already active".into()));
            }
            self.active = true;
            Ok(())
        }
        fn accept_audio(&mut self, _: i32, _: &[f32]) -> Result<()> {
            if self.fail_audio {
                Err(AppError::Unavailable("audio failure".into()))
            } else {
                Ok(())
            }
        }
        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            Err(AppError::Unavailable("finish failure".into()))
        }
        fn cancel_session(&mut self) -> Result<()> {
            self.active = false;
            Ok(())
        }
    }
    for fail_audio in [true, false] {
        let mut recognizer = StickyFailure {
            active: false,
            fail_audio,
        };
        recognizer.start_session().unwrap();
        let mut source = AudioSource::test_source(16_000, vec![0.25; 317]);
        let result = finish_worker_session(
            &mut source,
            &mut recognizer,
            &mut AudioPipeline::new(false),
            Vec::new(),
            None,
            false,
            false,
        );
        assert!(result.outcome.is_err());
        assert_eq!(result.audio.len(), 317);
        recognizer
            .start_session()
            .expect("failed finalization must not poison the next dictation");
    }
}
use crate::{
    delivery::StaticSink, dictation_transcript::Replacements, history::JsonHistoryStore,
    notifier::NoopNotifier, recorder::TestCapture,
};

struct SyntheticRecorder {
    samples: Arc<Vec<f32>>,
    source: Option<AudioSource>,
}

impl Recorder for SyntheticRecorder {
    fn start(&mut self) -> Result<()> {
        self.source = Some(AudioSource::test_source(
            48_000,
            self.samples.as_ref().clone(),
        ));
        Ok(())
    }
    fn stop(&mut self) -> Result<CaptureReport> {
        Ok(CaptureReport::default())
    }
    fn cancel(&mut self) -> Result<()> {
        self.source.take();
        Ok(())
    }
    fn audio_source(&mut self) -> Result<AudioSource> {
        Ok(self.source.take().unwrap())
    }
}

fn saved_recordings(
    directory: &TempDir,
) -> Vec<(crate::recordings::RecordingMetadata, crate::audio::Waveform)> {
    let mut entries: Vec<_> = fs::read_dir(directory.path().join("recordings"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|path| {
            let metadata =
                serde_json::from_slice(&fs::read(path.join("metadata.json")).unwrap()).unwrap();
            let wave = crate::audio::read_wav(&path.join("audio.wav")).unwrap();
            (metadata, wave)
        })
        .collect()
}

#[derive(Default)]
struct Effects {
    sent: Vec<String>,
    records: Vec<HistoryRecord>,
    delivery_updates: usize,
}

struct CountingSink(Arc<Mutex<Effects>>);

impl TextSink for CountingSink {
    fn send_final_text(&mut self, text: &str) -> Result<DeliveryOutcome> {
        self.0.lock().unwrap().sent.push(text.to_owned());
        Ok(DeliveryOutcome::Delivered {
            backend: "test".to_owned(),
        })
    }
}

struct CountingHistory(Arc<Mutex<Effects>>);

impl HistoryStore for CountingHistory {
    fn append(&mut self, record: HistoryRecord) -> Result<()> {
        self.0.lock().unwrap().records.push(record);
        Ok(())
    }

    fn update_delivery(
        &mut self,
        _id: &str,
        _status: DeliveryStatus,
        _backend: Option<String>,
    ) -> Result<()> {
        self.0.lock().unwrap().delivery_updates += 1;
        Ok(())
    }

    fn list(&self, limit: usize) -> Result<Vec<HistoryRecord>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .records
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect())
    }
}

#[derive(Clone, Copy)]
enum CaptureFault {
    None,
    Queue,
    Backend,
    Both,
    Duration,
}

struct FixtureRecorder {
    fault: CaptureFault,
    session: Option<TestCapture>,
}

impl Recorder for FixtureRecorder {
    fn start(&mut self) -> Result<()> {
        let mut session = if matches!(self.fault, CaptureFault::Duration) {
            TestCapture::with_capacity(101)
        } else {
            TestCapture::new()
        };
        match self.fault {
            CaptureFault::None => session.push(&[0.25]),
            CaptureFault::Queue => session.push(&[0.25, 0.5, 0.75]),
            CaptureFault::Backend => session.backend_error(),
            CaptureFault::Duration => session.push(&vec![0.25; 101]),
            CaptureFault::Both => {
                session.push(&[0.25, 0.5, 0.75]);
                session.backend_error();
            }
        }
        self.fault = CaptureFault::None;
        self.session = Some(session);
        Ok(())
    }

    fn stop(&mut self) -> Result<CaptureReport> {
        let session = self.session.as_mut().unwrap();
        session.close();
        Ok(session.report())
    }

    fn cancel(&mut self) -> Result<()> {
        if let Some(mut session) = self.session.take() {
            session.close();
        }
        Ok(())
    }

    fn audio_source(&mut self) -> Result<AudioSource> {
        Ok(self.session.as_mut().unwrap().take_source())
    }
}

struct ThreadedRecorder {
    source: Option<AudioSource>,
    producer: Option<thread::JoinHandle<()>>,
    callback_pause: Option<(Sender<()>, Receiver<()>)>,
    stop_events: Sender<&'static str>,
}

impl Recorder for ThreadedRecorder {
    fn start(&mut self) -> Result<()> {
        let mut capture = TestCapture::new();
        self.source = Some(capture.take_source());
        let pause = self.callback_pause.take();
        self.producer = Some(thread::spawn(move || {
            capture.push(&[0.25]);
            if let Some((entered, resume)) = pause {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(5)).unwrap();
                // The last callback completes only after stop has begun.
                capture.push(&[0.5]);
                capture.backend_error();
            }
            capture.close();
        }));
        Ok(())
    }

    fn stop(&mut self) -> Result<CaptureReport> {
        self.stop_events.send("stop entered").unwrap();
        self.producer.take().unwrap().join().unwrap();
        self.stop_events.send("producer joined").unwrap();
        // Require the real worker integrity check even with a clean stop report.
        Ok(CaptureReport::default())
    }

    fn cancel(&mut self) -> Result<()> {
        if let Some(producer) = self.producer.take() {
            producer.join().unwrap();
        }
        self.source.take();
        Ok(())
    }

    fn audio_source(&mut self) -> Result<AudioSource> {
        Ok(self.source.take().unwrap())
    }
}

struct StopErrorRecorder(FixtureRecorder);

impl Recorder for StopErrorRecorder {
    fn start(&mut self) -> Result<()> {
        self.0.start()
    }

    fn stop(&mut self) -> Result<CaptureReport> {
        self.0.stop()?;
        Err(AppError::Unavailable(
            "injected backend capture error".to_owned(),
        ))
    }

    fn cancel(&mut self) -> Result<()> {
        self.0.cancel()
    }

    fn audio_source(&mut self) -> Result<AudioSource> {
        self.0.audio_source()
    }
}

fn observed_daemon(fault: CaptureFault) -> (Daemon, Arc<Mutex<Effects>>, TempDir) {
    let directory = tempdir().unwrap();
    let effects = Arc::new(Mutex::new(Effects::default()));
    let mut daemon = Daemon::new(
        Config::default(),
        Box::new(FixtureRecorder {
            fault,
            session: None,
        }),
        Box::new(StaticRecognizer::new("final transcript")),
        Box::new(CountingSink(Arc::clone(&effects))),
        Box::new(CountingHistory(Arc::clone(&effects))),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(directory.path().join("recordings")),
    );
    daemon.initialize();
    (daemon, effects, directory)
}

#[test]
fn protected_technical_text_is_delivered_once_and_stored_exactly() {
    let (mut daemon, effects, _directory) = observed_daemon(CaptureFault::None);
    daemon.recognizer = Some(Box::new(StaticRecognizer::new("um ER diagram in C++.")));
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(effects.lock().unwrap().sent.is_empty());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let effects = effects.lock().unwrap();
    assert_eq!(effects.sent, vec!["ER diagram in C++."]);
    assert_eq!(effects.records.len(), 1);
    assert_eq!(effects.records[0].transcript, "ER diagram in C++.");
}

#[test]
fn successful_stop_delivers_once_and_only_after_stop() {
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let session_id = daemon.status.session_id.clone().unwrap();
    {
        let effects = effects.lock().unwrap();
        assert!(effects.sent.is_empty());
        assert!(effects.records.is_empty());
        assert_eq!(effects.delivery_updates, 0);
    }
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let effects = effects.lock().unwrap();
    assert_eq!(effects.sent, vec!["final transcript"]);
    assert_eq!(effects.records.len(), 1);
    assert_eq!(effects.records[0].transcript, "final transcript");
    assert_eq!(effects.delivery_updates, 1);
    let saved = saved_recordings(&directory);
    assert_eq!(saved.len(), 1);
    let (metadata, wave) = &saved[0];
    assert_eq!(metadata.version, 1);
    assert_eq!(metadata.session_id, session_id);
    assert!(metadata.stopped_at_ms > 0);
    assert_eq!(metadata.sample_rate, 16_000);
    assert_eq!(metadata.frames, 1);
    assert_eq!(metadata.model, daemon.config.model);
    assert_eq!(metadata.streaming_profile, daemon.config.streaming_profile);
    assert_eq!(metadata.speech_gate, daemon.config.speech_gate);
    assert_eq!(metadata.denoise, daemon.config.denoise);
    assert_eq!(metadata.itn, daemon.config.itn);
    assert_eq!(metadata.transcription, TranscriptionStatus::Succeeded);
    assert_eq!(metadata.capture.dropped_samples, 0);
    assert!(!metadata.capture.backend_failed);
    assert!(!metadata.capture.duration_exceeded);
    assert!(!metadata.capture.stop_failed);
    assert!(!metadata.capture.drain_failed);
    assert_eq!(wave.sample_rate, 16_000);
    assert_eq!(wave.samples, [0.25]);
}

#[test]
fn post_publication_warnings_preserve_delivery_and_no_speech() {
    for no_speech in [false, true] {
        for sync_failure in [true, false] {
            let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
            if sync_failure {
                daemon.recordings.fail_root_sync_for_test();
            } else {
                daemon.recordings.fail_prune_for_test();
            }
            if no_speech {
                daemon.recognizer = Some(Box::new(StaticRecognizer::no_speech()));
            }
            assert!(daemon.handle(IpcRequest::Toggle).is_ok());
            let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
                panic!("expected command");
            };
            assert!(result.ok);
            assert_eq!(
                result.transcription,
                if no_speech {
                    TranscriptionStatus::NoSpeech
                } else {
                    TranscriptionStatus::Succeeded
                }
            );
            assert_eq!(
                result.delivery,
                if no_speech {
                    DeliveryStatus::NotAttempted
                } else {
                    DeliveryStatus::Delivered
                }
            );
            let effects = effects.lock().unwrap();
            assert_eq!(effects.sent.len(), if no_speech { 0 } else { 1 });
            assert_eq!(effects.records.len(), if no_speech { 0 } else { 1 });
            assert_eq!(saved_recordings(&directory).len(), 1);
            assert!(result.message.contains(if sync_failure {
                "directory sync failed"
            } else {
                "old audio could not be pruned"
            }));
        }
    }
}

#[test]
fn capture_failures_do_not_deliver_or_append_history_and_recover() {
    for fault in [
        CaptureFault::Queue,
        CaptureFault::Backend,
        CaptureFault::Both,
        CaptureFault::Duration,
    ] {
        let (mut daemon, effects, directory) = observed_daemon(fault);
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        let response = daemon.handle(IpcRequest::Toggle);
        let IpcResponse::Command { result } = response else {
            panic!("expected command")
        };
        assert!(!result.ok);
        assert_eq!(result.transcription, TranscriptionStatus::Failed);
        assert_eq!(result.delivery, DeliveryStatus::NotAttempted);
        assert_eq!(result.status.state, DaemonState::Idle);
        assert!(result.transcript.is_none());
        {
            let effects = effects.lock().unwrap();
            assert!(effects.sent.is_empty());
            assert!(effects.records.is_empty());
            assert_eq!(effects.delivery_updates, 0);
        }
        let saved = saved_recordings(&directory);
        assert_eq!(saved.len(), 1);
        let (metadata, wave) = &saved[0];
        assert_eq!(metadata.transcription, TranscriptionStatus::Failed);
        assert!(!metadata.capture.stop_failed);
        assert!(!metadata.capture.drain_failed);
        assert_eq!(
            metadata.capture.duration_exceeded,
            matches!(fault, CaptureFault::Duration)
        );
        assert_eq!(
            metadata.capture.backend_failed,
            matches!(fault, CaptureFault::Backend | CaptureFault::Both)
        );
        if matches!(fault, CaptureFault::Duration) {
            assert_eq!(metadata.capture.dropped_samples, 0);
            assert_eq!(wave.samples.len(), 100);
        } else {
            assert_eq!(
                metadata.capture.dropped_samples > 0,
                matches!(fault, CaptureFault::Queue | CaptureFault::Both)
            );
        }
        assert_eq!(metadata.frames, wave.samples.len());
        assert_eq!(wave.sample_rate, 16_000);
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        assert_eq!(saved_recordings(&directory).len(), 2);
        let effects = effects.lock().unwrap();
        assert_eq!(effects.sent, vec!["final transcript"]);
        assert_eq!(effects.records.len(), 1);
        assert_eq!(effects.records[0].transcript, "final transcript");
        assert_eq!(effects.delivery_updates, 1);
    }
}

#[test]
fn stop_during_callback_checks_stable_failure_before_final_decode_and_recovers() {
    #[derive(Default)]
    struct Calls {
        samples: Vec<f32>,
        finishes: usize,
        cancels: usize,
    }

    struct PausedRecognizer {
        calls: Arc<Mutex<Calls>>,
        first_audio: Option<(Sender<()>, Receiver<()>)>,
        fail_first_audio: bool,
    }

    impl StreamingRecognizer for PausedRecognizer {
        fn start_session(&mut self) -> Result<()> {
            Ok(())
        }

        fn accept_audio(&mut self, rate: i32, samples: &[f32]) -> Result<()> {
            assert_eq!(rate, MODEL_SAMPLE_RATE);
            self.calls
                .lock()
                .unwrap()
                .samples
                .extend_from_slice(samples);
            if let Some((entered, resume)) = self.first_audio.take() {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(5)).unwrap();
                if self.fail_first_audio {
                    return Err(AppError::Unavailable(
                        "earlier recognizer failure".to_owned(),
                    ));
                }
            }
            Ok(())
        }

        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            self.calls.lock().unwrap().finishes += 1;
            Ok(RecognitionOutcome::Transcript(
                "final transcript".to_owned(),
            ))
        }

        fn cancel_session(&mut self) -> Result<()> {
            self.calls.lock().unwrap().cancels += 1;
            Ok(())
        }
    }

    for fail_first_audio in [false, true] {
        let (callback_entered_tx, callback_entered_rx) = mpsc::channel();
        let (callback_resume_tx, callback_resume_rx) = mpsc::channel();
        let (audio_entered_tx, audio_entered_rx) = mpsc::channel();
        let (audio_resume_tx, audio_resume_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
        daemon.config.denoise = false;
        daemon.recorder = Box::new(ThreadedRecorder {
            source: None,
            producer: None,
            callback_pause: Some((callback_entered_tx, callback_resume_rx)),
            stop_events: stop_tx,
        });
        daemon.recognizer = Some(Box::new(PausedRecognizer {
            calls: Arc::clone(&calls),
            first_audio: Some((audio_entered_tx, audio_resume_rx)),
            fail_first_audio,
        }));
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        daemon.worker.as_mut().unwrap().finish_sent = Some(finish_tx);
        // Bounded receives are deadlock guards, not elapsed-time assertions.
        callback_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        audio_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        {
            let effects = effects.lock().unwrap();
            assert!(effects.sent.is_empty());
            assert!(effects.records.is_empty());
            assert_eq!(effects.delivery_updates, 0);
        }
        let stop = thread::spawn(move || {
            let response = daemon.handle(IpcRequest::Toggle);
            (daemon, response)
        });
        assert_eq!(
            stop_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "stop entered",
        );
        assert_eq!(calls.lock().unwrap().samples, vec![0.25]);
        assert_eq!(calls.lock().unwrap().finishes, 0);
        callback_resume_tx.send(()).unwrap();
        assert_eq!(
            stop_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "producer joined",
        );
        // Do not let the live worker drain the final sample before Finish.
        finish_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let before_release_ms = now_ms();
        let deadline = Instant::now() + Duration::from_secs(5);
        while now_ms() < before_release_ms + 10 {
            assert!(Instant::now() < deadline, "clock did not advance");
            thread::sleep(Duration::from_millis(1));
        }
        audio_resume_tx.send(()).unwrap();
        let (mut daemon, response) = stop.join().unwrap();
        let IpcResponse::Command { result } = response else {
            panic!("expected command")
        };
        assert!(!result.ok);
        assert_eq!(result.transcription, TranscriptionStatus::Failed);
        assert_eq!(result.delivery, DeliveryStatus::NotAttempted);
        assert_eq!(result.status.state, DaemonState::Idle);
        assert!(result.transcript.is_none());
        assert!(result.message.contains("backend capture error"));
        assert!(!result.message.contains("mono samples dropped"));
        assert!(!result.message.contains("earlier recognizer failure"));
        let saved = saved_recordings(&directory);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].0.transcription, TranscriptionStatus::Failed);
        assert!(saved[0].0.capture.backend_failed);
        assert!(!saved[0].0.capture.stop_failed);
        assert!(saved[0].0.stopped_at_ms <= before_release_ms);
        assert_eq!(saved[0].0.frames, 2);
        assert_eq!(saved[0].1.samples, [0.25, 0.5]);
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.samples, vec![0.25]);
            assert_eq!(calls.finishes, 0);
            assert!(calls.cancels > 0);
            let effects = effects.lock().unwrap();
            assert!(effects.sent.is_empty());
            assert!(effects.records.is_empty());
            assert_eq!(effects.delivery_updates, 0);
        }
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        assert!(effects.lock().unwrap().sent.is_empty());
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        let calls = calls.lock().unwrap();
        assert_eq!(calls.samples, vec![0.25, 0.25]);
        assert_eq!(calls.finishes, 1);
        let effects = effects.lock().unwrap();
        assert_eq!(effects.sent, vec!["final transcript"]);
        assert_eq!(effects.records.len(), 1);
        assert_eq!(effects.records[0].transcript, "final transcript");
        assert_eq!(effects.delivery_updates, 1);
    }
}

#[test]
fn cancel_with_a_full_queue_has_no_effects_and_recovers() {
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::Queue);
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(daemon.handle(IpcRequest::Cancel).is_ok());
    assert!(saved_recordings(&directory).is_empty());
    {
        let effects = effects.lock().unwrap();
        assert!(effects.sent.is_empty());
        assert!(effects.records.is_empty());
        assert_eq!(effects.delivery_updates, 0);
    }
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let effects = effects.lock().unwrap();
    assert_eq!(effects.sent, vec!["final transcript"]);
    assert_eq!(effects.records.len(), 1);
    assert_eq!(effects.delivery_updates, 1);
}

#[test]
fn recorder_stop_error_never_reaches_history_or_delivery() {
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
    daemon.recorder = Box::new(StopErrorRecorder(FixtureRecorder {
        fault: CaptureFault::None,
        session: None,
    }));
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("expected command")
    };
    assert!(!result.ok);
    assert_eq!(result.transcription, TranscriptionStatus::Failed);
    assert_eq!(result.delivery, DeliveryStatus::NotAttempted);
    assert_eq!(result.status.state, DaemonState::Idle);
    assert!(result.transcript.is_none());
    assert!(result.message.contains("injected backend capture error"));
    let saved = saved_recordings(&directory);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].0.transcription, TranscriptionStatus::Failed);
    assert!(saved[0].0.capture.stop_failed);
    assert_eq!(saved[0].1.samples, [0.25]);
    let effects = effects.lock().unwrap();
    assert!(effects.sent.is_empty());
    assert!(effects.records.is_empty());
    assert_eq!(effects.delivery_updates, 0);
}

#[test]
fn capture_failure_overrides_prior_worker_error_without_finalizing() {
    struct MustNotFinish;
    impl StreamingRecognizer for MustNotFinish {
        fn start_session(&mut self) -> Result<()> {
            Ok(())
        }

        fn accept_audio(&mut self, _rate: i32, _samples: &[f32]) -> Result<()> {
            panic!("must not decode failed capture")
        }

        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            panic!("must not finalize failed capture")
        }

        fn cancel_session(&mut self) -> Result<()> {
            Ok(())
        }
    }
    let mut capture = TestCapture::new();
    capture.push(&[1.0, 2.0, 3.0]);
    capture.backend_error();
    let mut source = capture.take_source();
    capture.close();
    let result = finish_worker_session(
        &mut source,
        &mut MustNotFinish,
        &mut AudioPipeline::new(false),
        Vec::new(),
        Some(AppError::Unavailable(
            "earlier recognizer failure".to_owned(),
        )),
        false,
        false,
    );
    assert_eq!(result.audio, vec![1.0, 2.0]);
    assert_eq!(result.capture.dropped_samples, 1);
    assert!(result.capture.backend_failed);
    let error = result.outcome.unwrap_err().to_string();
    assert!(error.contains("mono samples dropped"));
    assert!(error.contains("backend capture error"));
    assert!(!error.contains("earlier recognizer failure"));
}

#[derive(Default)]
struct CountingRecognizer {
    accepted_samples: usize,
    active: bool,
}

impl StreamingRecognizer for CountingRecognizer {
    fn start_session(&mut self) -> Result<()> {
        self.active = true;
        Ok(())
    }

    fn accept_audio(&mut self, _sample_rate: i32, samples: &[f32]) -> Result<()> {
        if !self.active {
            return Err(AppError::InvalidState(
                "recognizer is not active".to_owned(),
            ));
        }
        self.accepted_samples += samples.len();
        Ok(())
    }

    fn finish_session(&mut self) -> Result<RecognitionOutcome> {
        self.active = false;
        Ok(RecognitionOutcome::Transcript(
            self.accepted_samples.to_string(),
        ))
    }

    fn cancel_session(&mut self) -> Result<()> {
        self.active = false;
        Ok(())
    }
}

fn test_daemon(outcome: DeliveryOutcome) -> (Daemon, TempDir) {
    let directory = tempdir().expect("temp directory");
    let history = Box::new(JsonHistoryStore::new(directory.path().join("history.json")));
    let daemon = Daemon::new(
        Config::default(),
        Box::new(NoopRecorder::default()),
        Box::new(StaticRecognizer::new("final transcript")),
        Box::new(StaticSink::new(outcome)),
        history,
        Box::new(NoopNotifier::default()),
        RecordingStore::new(directory.path().join("recordings")),
    );
    (daemon, directory)
}

#[test]
fn final_only_delivery_keeps_transcript_in_history() {
    let (mut daemon, _directory) = test_daemon(DeliveryOutcome::Delivered {
        backend: "test".to_owned(),
    });
    daemon.initialize();

    let start = daemon.handle(IpcRequest::Toggle);
    assert!(start.is_ok());
    let finish = daemon.handle(IpcRequest::Toggle);
    assert!(finish.is_ok());

    match finish {
        IpcResponse::Command { result } => {
            assert_eq!(result.transcript.as_deref(), Some("final transcript"));
            assert_eq!(result.transcription, TranscriptionStatus::Succeeded);
            assert_eq!(result.delivery, DeliveryStatus::Delivered);
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn failed_delivery_still_reports_transcription_success() {
    let (mut daemon, _directory) = test_daemon(DeliveryOutcome::Failed {
        reason: "no input backend".to_owned(),
    });
    daemon.initialize();
    let _ = daemon.handle(IpcRequest::Toggle);
    let finish = daemon.handle(IpcRequest::Toggle);

    match finish {
        IpcResponse::Command { result } => {
            assert!(!result.ok);
            assert_eq!(result.transcription, TranscriptionStatus::Succeeded);
            assert_eq!(result.delivery, DeliveryStatus::Failed);
            assert_eq!(result.transcript.as_deref(), Some("final transcript"));
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn filled_pauses_are_removed_before_history_and_delivery() {
    let directory = tempdir().expect("temporary directory");
    let history_path = directory.path().join("history.json");
    let mut daemon = Daemon::new(
        Config::default(),
        Box::new(NoopRecorder::default()),
        Box::new(StaticRecognizer::new("uh hello um world")),
        Box::new(StaticSink::new(DeliveryOutcome::Delivered {
            backend: "test".to_owned(),
        })),
        Box::new(JsonHistoryStore::new(&history_path)),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(directory.path().join("recordings")),
    );
    daemon.initialize();
    let _ = daemon.handle(IpcRequest::Toggle);
    let finish = daemon.handle(IpcRequest::Toggle);

    match finish {
        IpcResponse::Command { result } => {
            assert_eq!(result.transcript.as_deref(), Some("hello world"));
            assert_eq!(result.transcription, TranscriptionStatus::Succeeded);
            assert_eq!(result.delivery, DeliveryStatus::Delivered);
        }
        other => panic!("unexpected response: {other:?}"),
    }
    let records = JsonHistoryStore::new(&history_path)
        .list(10)
        .expect("list history");
    assert_eq!(records[0].transcript, "hello world");
}

#[test]
fn short_stutters_are_collapsed_before_history_and_delivery() {
    let (mut daemon, _directory) = test_daemon(DeliveryOutcome::Delivered {
        backend: "test".to_owned(),
    });
    daemon.recognizer = Some(Box::new(StaticRecognizer::new("I I I I think")));
    daemon.initialize();
    let _ = daemon.handle(IpcRequest::Toggle);
    let finish = daemon.handle(IpcRequest::Toggle);
    match finish {
        IpcResponse::Command { result } => {
            assert_eq!(result.transcript.as_deref(), Some("I think"));
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn replacements_are_applied_before_history_and_delivery() {
    let directory = tempdir().expect("temporary directory");
    let history_path = directory.path().join("history.json");
    let config = Config {
        replacements: Replacements::from_pairs([("nv stt".to_owned(), "nvstt".to_owned())]),
        ..Config::default()
    };
    let mut daemon = Daemon::new(
        config,
        Box::new(NoopRecorder::default()),
        Box::new(StaticRecognizer::new("uh nv stt")),
        Box::new(StaticSink::new(DeliveryOutcome::Delivered {
            backend: "test".to_owned(),
        })),
        Box::new(JsonHistoryStore::new(&history_path)),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(directory.path().join("recordings")),
    );
    daemon.initialize();
    let _ = daemon.handle(IpcRequest::Toggle);
    let finish = daemon.handle(IpcRequest::Toggle);
    match finish {
        IpcResponse::Command { result } => {
            assert_eq!(result.transcript.as_deref(), Some("nvstt"));
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn default_itn_is_applied_before_history_and_delivery() {
    let directory = tempdir().expect("temporary directory");
    let history_path = directory.path().join("history.json");
    let mut daemon = Daemon::new(
        Config::default(),
        Box::new(NoopRecorder::default()),
        Box::new(StaticRecognizer::new("I have twenty one apples")),
        Box::new(StaticSink::new(DeliveryOutcome::Delivered {
            backend: "test".to_owned(),
        })),
        Box::new(JsonHistoryStore::new(&history_path)),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(directory.path().join("recordings")),
    );
    daemon.initialize();
    let _ = daemon.handle(IpcRequest::Toggle);
    let finish = daemon.handle(IpcRequest::Toggle);
    match finish {
        IpcResponse::Command { result } => {
            assert_eq!(result.transcript.as_deref(), Some("I have 21 apples"));
        }
        other => panic!("unexpected response: {other:?}"),
    }
    let records = JsonHistoryStore::new(&history_path)
        .list(10)
        .expect("list history");
    assert_eq!(records[0].transcript, "I have 21 apples");
}

#[test]
fn filler_only_speech_has_no_delivery_or_history_record() {
    let directory = tempdir().expect("temporary directory");
    let history_path = directory.path().join("history.json");
    let mut daemon = Daemon::new(
        Config::default(),
        Box::new(NoopRecorder::default()),
        Box::new(StaticRecognizer::new("um uh um.")),
        Box::new(StaticSink::new(DeliveryOutcome::Delivered {
            backend: "test".to_owned(),
        })),
        Box::new(JsonHistoryStore::new(&history_path)),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(directory.path().join("recordings")),
    );
    daemon.initialize();
    let _ = daemon.handle(IpcRequest::Toggle);
    let finish = daemon.handle(IpcRequest::Toggle);

    match finish {
        IpcResponse::Command { result } => {
            assert!(result.ok);
            assert!(result.transcript.is_none());
            assert_eq!(result.transcription, TranscriptionStatus::NoSpeech);
            assert_eq!(result.delivery, DeliveryStatus::NotAttempted);
        }
        other => panic!("unexpected response: {other:?}"),
    }
    assert!(
        JsonHistoryStore::new(&history_path)
            .list(10)
            .expect("list history")
            .is_empty()
    );
}

#[test]
fn no_speech_has_no_delivery_or_history_record() {
    let directory = tempdir().expect("temporary directory");
    let history_path = directory.path().join("history.json");
    let mut daemon = Daemon::new(
        Config::default(),
        Box::new(NoopRecorder::default()),
        Box::new(StaticRecognizer::no_speech()),
        Box::new(StaticSink::new(DeliveryOutcome::Delivered {
            backend: "test".to_owned(),
        })),
        Box::new(JsonHistoryStore::new(&history_path)),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(directory.path().join("recordings")),
    );
    daemon.initialize();
    let _ = daemon.handle(IpcRequest::Toggle);
    let finish = daemon.handle(IpcRequest::Toggle);

    match finish {
        IpcResponse::Command { result } => {
            assert!(result.ok);
            assert!(result.transcript.is_none());
            assert_eq!(result.transcription, TranscriptionStatus::NoSpeech);
            assert_eq!(result.delivery, DeliveryStatus::NotAttempted);
        }
        other => panic!("unexpected response: {other:?}"),
    }
    assert!(
        JsonHistoryStore::new(&history_path)
            .list(10)
            .expect("list history")
            .is_empty()
    );
    let saved_entries = saved_recordings(&directory);
    assert_eq!(saved_entries.len(), 1);
    let (metadata, wave) = &saved_entries[0];
    assert_eq!(metadata.transcription, TranscriptionStatus::NoSpeech);
    assert_eq!(metadata.frames, 0);
    assert!(wave.samples.is_empty());
    assert_eq!(wave.sample_rate, 16_000);
}

#[test]
fn empty_transcript_still_saves_failed_attempt() {
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
    daemon.recognizer = Some(Box::new(StaticRecognizer::new("  ")));
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("command")
    };
    assert!(!result.ok);
    assert_eq!(result.transcription, TranscriptionStatus::Failed);
    assert!(result.message.contains("transcript was empty"));
    assert_eq!(result.delivery, DeliveryStatus::NotAttempted);
    assert!(effects.lock().unwrap().records.is_empty());
    let saved = saved_recordings(&directory);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].0.transcription, TranscriptionStatus::Failed);
    assert_eq!(saved[0].1.samples, [0.25]);
}

#[test]
fn recognizer_failure_saves_raw_audio_without_delivery() {
    struct FailingRecognizer;
    impl StreamingRecognizer for FailingRecognizer {
        fn start_session(&mut self) -> Result<()> {
            Ok(())
        }
        fn accept_audio(&mut self, _: i32, _: &[f32]) -> Result<()> {
            Ok(())
        }
        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            Err(AppError::Unavailable("decode failed".into()))
        }
        fn cancel_session(&mut self) -> Result<()> {
            Ok(())
        }
    }
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
    daemon.recognizer = Some(Box::new(FailingRecognizer));
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("command")
    };
    assert!(!result.ok);
    assert_eq!(result.transcription, TranscriptionStatus::Failed);
    assert!(result.message.contains("decode failed"));
    assert_eq!(result.delivery, DeliveryStatus::NotAttempted);
    assert!(effects.lock().unwrap().records.is_empty());
    let saved = saved_recordings(&directory);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].0.transcription, TranscriptionStatus::Failed);
    assert_eq!(saved[0].1.samples, [0.25]);
}

#[test]
fn worker_panic_warns_without_creating_a_misleading_wav() {
    struct PanickingRecognizer;
    impl StreamingRecognizer for PanickingRecognizer {
        fn start_session(&mut self) -> Result<()> {
            Ok(())
        }
        fn accept_audio(&mut self, _: i32, _: &[f32]) -> Result<()> {
            Ok(())
        }
        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            panic!("decode panic")
        }
        fn cancel_session(&mut self) -> Result<()> {
            Ok(())
        }
    }
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
    daemon.recognizer = Some(Box::new(PanickingRecognizer));
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("command")
    };
    assert!(!result.ok);
    assert_eq!(result.transcription, TranscriptionStatus::Failed);
    assert!(result.message.contains("recognition worker panicked"));
    assert!(result.message.contains("audio was not saved"));
    assert!(saved_recordings(&directory).is_empty());
    assert!(effects.lock().unwrap().records.is_empty());
}

#[test]
fn initialize_reconciles_staging_without_blocking_dictation() {
    let (mut daemon, directory) = test_daemon(DeliveryOutcome::Delivered {
        backend: "test".to_owned(),
    });
    let staging = directory.path().join("recordings/.staging-1700000000000-1");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("audio.wav"), b"partial").unwrap();
    daemon.initialize();
    assert!(!staging.exists());
    assert_eq!(daemon.status.state, DaemonState::Idle);
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    assert_eq!(saved_recordings(&directory).len(), 1);
}

#[test]
fn failed_start_does_not_save_audio() {
    let (mut daemon, _effects, directory) = observed_daemon(CaptureFault::None);
    daemon.recognizer = Some(Box::new(UnavailableRecognizer::new("start failed")));
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("command")
    };
    assert!(!result.ok);
    assert_eq!(result.transcription, TranscriptionStatus::NotStarted);
    assert!(saved_recordings(&directory).is_empty());
}

#[test]
fn recorder_start_failure_does_not_save_audio() {
    struct StartErrorRecorder;
    impl Recorder for StartErrorRecorder {
        fn start(&mut self) -> Result<()> {
            Err(AppError::Unavailable("recorder start failed".into()))
        }
        fn stop(&mut self) -> Result<CaptureReport> {
            panic!("no active capture")
        }
        fn cancel(&mut self) -> Result<()> {
            Ok(())
        }
        fn audio_source(&mut self) -> Result<AudioSource> {
            panic!("no active capture")
        }
    }
    let (mut daemon, _effects, directory) = observed_daemon(CaptureFault::None);
    daemon.recorder = Box::new(StartErrorRecorder);
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("command")
    };
    assert!(!result.ok);
    assert_eq!(result.transcription, TranscriptionStatus::NotStarted);
    assert!(saved_recordings(&directory).is_empty());
}

#[test]
fn saving_failure_preserves_transcription_delivery_and_history() {
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
    let root = directory.path().join("blocked-recordings");
    fs::write(&root, b"keep").unwrap();
    daemon.recordings = RecordingStore::new(root.clone());
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("command")
    };
    assert!(result.ok);
    assert_eq!(result.transcription, TranscriptionStatus::Succeeded);
    assert_eq!(result.delivery, DeliveryStatus::Delivered);
    assert_eq!(result.transcript.as_deref(), Some("final transcript"));
    assert!(result.message.contains("audio was not saved"));
    assert_eq!(fs::read(root).unwrap(), b"keep");
    let effects = effects.lock().unwrap();
    assert_eq!(effects.records.len(), 1);
    assert_eq!(effects.sent, ["final transcript"]);
    assert_eq!(effects.delivery_updates, 1);
}

#[test]
fn saving_failure_does_not_change_no_speech_or_delivery_failure() {
    for (recognizer, delivery, status, ok) in [
        (
            StaticRecognizer::no_speech(),
            DeliveryOutcome::Delivered {
                backend: "test".into(),
            },
            TranscriptionStatus::NoSpeech,
            true,
        ),
        (
            StaticRecognizer::new("text"),
            DeliveryOutcome::Failed {
                reason: "sink failed".into(),
            },
            TranscriptionStatus::Succeeded,
            false,
        ),
    ] {
        let (mut daemon, directory) = test_daemon(delivery);
        daemon.recognizer = Some(Box::new(recognizer));
        let root = directory.path().join("blocked-recordings");
        fs::write(&root, b"keep").unwrap();
        daemon.recordings = RecordingStore::new(root);
        daemon.initialize();
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
            panic!("command")
        };
        assert_eq!(result.ok, ok);
        assert_eq!(result.transcription, status);
        assert_eq!(
            result.delivery,
            if ok {
                DeliveryStatus::NotAttempted
            } else {
                DeliveryStatus::Failed
            }
        );
        assert!(result.message.contains("audio was not saved"));
    }
}

#[test]
fn worker_drains_the_last_audio_before_final_flush() {
    let samples = vec![0.25; 777];
    let source = AudioSource::test_source(16_000, samples.clone());
    let mut recognizer = CountingRecognizer::default();
    recognizer.start_session().expect("start recognizer");
    let worker =
        RecognitionWorker::spawn(Box::new(recognizer), source, false).expect("spawn worker");
    let (_, result) = worker.finish(false).expect("finish worker");
    assert_eq!(result.audio, samples);
    assert_eq!(result.sample_rate, 16_000);
    assert_eq!(
        result.outcome.expect("recognition outcome"),
        RecognitionOutcome::Transcript(samples.len().to_string())
    );
}

#[test]
fn worker_keeps_audio_after_recognizer_error() {
    struct FailingRecognizer {
        first_audio: Sender<()>,
        calls: Arc<Mutex<(usize, usize, usize)>>,
    }

    impl StreamingRecognizer for FailingRecognizer {
        fn start_session(&mut self) -> Result<()> {
            Ok(())
        }
        fn accept_audio(&mut self, _: i32, _: &[f32]) -> Result<()> {
            self.calls.lock().unwrap().0 += 1;
            self.first_audio.send(()).unwrap();
            Err(AppError::Unavailable(
                "injected recognizer error".to_owned(),
            ))
        }
        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            self.calls.lock().unwrap().1 += 1;
            panic!("must not finalize failed recognition")
        }
        fn cancel_session(&mut self) -> Result<()> {
            self.calls.lock().unwrap().2 += 1;
            Ok(())
        }
    }

    let calls = Arc::new(Mutex::new((0, 0, 0)));
    let (first_audio, first_received) = mpsc::channel();
    let mut capture = TestCapture::new();
    capture.push(&[0.25, 0.5]);
    let recognizer = FailingRecognizer {
        first_audio,
        calls: Arc::clone(&calls),
    };
    let worker =
        RecognitionWorker::spawn(Box::new(recognizer), capture.take_source(), false).unwrap();
    // The first drain frees both slots before the third sample arrives.
    first_received.recv_timeout(Duration::from_secs(5)).unwrap();
    capture.push(&[0.75]);
    capture.close();
    let (_, result) = worker.finish(false).unwrap();
    assert_eq!(result.audio, vec![0.25, 0.5, 0.75]);
    assert_eq!(result.sample_rate, 16_000);
    assert_eq!(result.capture.dropped_samples, 0);
    assert!(!result.drain_failed);
    assert!(
        result
            .outcome
            .unwrap_err()
            .to_string()
            .contains("injected recognizer error")
    );
    assert_eq!(*calls.lock().unwrap(), (1, 0, 1));
}

#[test]
fn capture_loss_overrides_recognizer_error_and_keeps_available_audio() {
    struct FailingRecognizer(Sender<()>, Receiver<()>);
    impl StreamingRecognizer for FailingRecognizer {
        fn start_session(&mut self) -> Result<()> {
            Ok(())
        }
        fn accept_audio(&mut self, _: i32, _: &[f32]) -> Result<()> {
            self.0.send(()).unwrap();
            self.1.recv_timeout(Duration::from_secs(5)).unwrap();
            Err(AppError::Unavailable(
                "injected recognizer error".to_owned(),
            ))
        }
        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            panic!("must not finalize failed capture")
        }
        fn cancel_session(&mut self) -> Result<()> {
            Ok(())
        }
    }
    let (first_audio, first_received) = mpsc::channel();
    let (resume, paused) = mpsc::channel();
    let mut capture = TestCapture::new();
    capture.push(&[0.25, 0.5]);
    let worker = RecognitionWorker::spawn(
        Box::new(FailingRecognizer(first_audio, paused)),
        capture.take_source(),
        false,
    )
    .unwrap();
    first_received.recv_timeout(Duration::from_secs(5)).unwrap();
    // The worker cannot drain again until the intended overflow is recorded.
    capture.push(&[0.75, 1.0, 1.25]);
    capture.close();
    resume.send(()).unwrap();
    let (_, result) = worker.finish(false).unwrap();
    assert_eq!(&result.audio[..2], &[0.25, 0.5]);
    assert!(result.capture.dropped_samples > 0);
    let error = result.outcome.unwrap_err().to_string();
    assert!(error.contains("mono samples dropped"));
    assert!(!error.contains("injected recognizer error"));
}

#[test]
fn capture_only_finishes_without_finalizing_after_recorder_stop_error() {
    struct MustNotFinish(Arc<Mutex<usize>>);
    impl StreamingRecognizer for MustNotFinish {
        fn start_session(&mut self) -> Result<()> {
            Ok(())
        }
        fn accept_audio(&mut self, _: i32, _: &[f32]) -> Result<()> {
            Ok(())
        }
        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            panic!("must not finalize after stop error")
        }
        fn cancel_session(&mut self) -> Result<()> {
            *self.0.lock().unwrap() += 1;
            Ok(())
        }
    }
    let cancels = Arc::new(Mutex::new(0));
    let mut capture = TestCapture::new();
    capture.push(&[0.25, 0.5]);
    let worker = RecognitionWorker::spawn(
        Box::new(MustNotFinish(Arc::clone(&cancels))),
        capture.take_source(),
        false,
    )
    .unwrap();
    capture.close();
    let (_, result) = worker.finish(true).unwrap();
    assert_eq!(result.audio, vec![0.25, 0.5]);
    assert!(result.outcome.is_err());
    assert_eq!(*cancels.lock().unwrap(), 1);
}

#[test]
#[ignore]
fn measure_two_minute_stop_to_result_with_and_without_storage() {
    struct SignalingRecognizer {
        inner: StaticRecognizer,
        ready: Sender<()>,
        sent: bool,
    }

    impl StreamingRecognizer for SignalingRecognizer {
        fn start_session(&mut self) -> Result<()> {
            self.sent = false;
            self.inner.start_session()
        }
        fn accept_audio(&mut self, rate: i32, samples: &[f32]) -> Result<()> {
            self.inner.accept_audio(rate, samples)?;
            if !self.sent {
                self.ready.send(()).unwrap();
                self.sent = true;
            }
            Ok(())
        }
        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            self.inner.finish_session()
        }
        fn cancel_session(&mut self) -> Result<()> {
            self.inner.cancel_session()
        }
    }

    fn trial(daemon: &mut Daemon, ready: &Receiver<()>, expect_warning: bool) -> f64 {
        assert!(daemon.handle(IpcRequest::Toggle).is_ok());
        // Wait outside the timer for the worker to process its first audio chunk.
        // Remaining audio may still be processed during the timed stop.
        ready.recv_timeout(Duration::from_secs(30)).unwrap();
        let started = Instant::now();
        let response = daemon.handle(IpcRequest::Toggle);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let IpcResponse::Command { result } = response else {
            panic!("expected command result")
        };
        assert!(result.ok, "{}", result.message);
        assert_eq!(result.transcription, TranscriptionStatus::Succeeded);
        assert_eq!(result.delivery, DeliveryStatus::Delivered);
        assert_eq!(
            result.message.contains("audio was not saved"),
            expect_warning
        );
        ms
    }

    let samples = Arc::new(vec![0.25_f32; 48_000 * 120]);
    let (mut saved, saved_effects, saved_dir) = observed_daemon(CaptureFault::None);
    let (mut control, control_effects, control_dir) = observed_daemon(CaptureFault::None);
    for daemon in [&mut saved, &mut control] {
        daemon.recorder = Box::new(SyntheticRecorder {
            samples: Arc::clone(&samples),
            source: None,
        });
    }
    let (saved_ready_tx, saved_ready_rx) = mpsc::channel();
    let (control_ready_tx, control_ready_rx) = mpsc::channel();
    for (daemon, ready) in [
        (&mut saved, saved_ready_tx),
        (&mut control, control_ready_tx),
    ] {
        daemon.recognizer = Some(Box::new(SignalingRecognizer {
            inner: StaticRecognizer::new("final transcript"),
            ready,
            sent: false,
        }));
    }
    let blocked_root = control_dir.path().join("blocked-recordings");
    fs::write(&blocked_root, b"keep").unwrap();
    control.recordings = RecordingStore::new(blocked_root.clone());

    let mut saved_ms = Vec::new();
    let mut control_ms = Vec::new();
    for n in 0..20 {
        // Alternate order to limit bias from cache and filesystem warmup.
        if n % 2 == 0 {
            control_ms.push(trial(&mut control, &control_ready_rx, true));
            saved_ms.push(trial(&mut saved, &saved_ready_rx, false));
        } else {
            saved_ms.push(trial(&mut saved, &saved_ready_rx, false));
            control_ms.push(trial(&mut control, &control_ready_rx, true));
        }
    }
    let saved_trials = saved_ms.clone();
    let control_trials = control_ms.clone();
    saved_ms.sort_by(f64::total_cmp);
    control_ms.sort_by(f64::total_cmp);
    println!("saved stop-to-result ms (20): {saved_trials:?}");
    println!("blocked-store control stop-to-result ms (20): {control_trials:?}");
    println!(
        "saved p50={:.2} ms p95={:.2} ms; control p50={:.2} ms p95={:.2} ms; delta p50={:.2} ms p95={:.2} ms",
        saved_ms[9],
        saved_ms[18],
        control_ms[9],
        control_ms[18],
        saved_ms[9] - control_ms[9],
        saved_ms[18] - control_ms[18],
    );
    assert_eq!(saved_effects.lock().unwrap().sent.len(), 20);
    assert_eq!(control_effects.lock().unwrap().sent.len(), 20);
    assert_eq!(fs::read(blocked_root).unwrap(), b"keep");
    let saved_entries = saved_recordings(&saved_dir);
    assert_eq!(saved_entries.len(), 7);
    assert!(
        saved_entries
            .iter()
            .all(|(meta, wave)| meta.frames == samples.len()
                && wave.sample_rate == 48_000
                && wave.samples.len() == samples.len())
    );
}

#[test]
#[ignore]
fn measure_two_minute_capture_peak_memory() {
    let samples = Arc::new(vec![0.25_f32; 48_000 * 120]);
    let (mut daemon, effects, directory) = observed_daemon(CaptureFault::None);
    daemon.recorder = Box::new(SyntheticRecorder {
        samples,
        source: None,
    });
    assert!(daemon.handle(IpcRequest::Toggle).is_ok());
    let IpcResponse::Command { result } = daemon.handle(IpcRequest::Toggle) else {
        panic!("expected command");
    };
    assert!(result.ok);
    assert_eq!(effects.lock().unwrap().sent.len(), 1);
    assert_eq!(saved_recordings(&directory).len(), 1);
}

#[test]
fn parakeet_rollback_configuration_keeps_final_only_delivery() {
    let directory = tempdir().expect("temporary directory");
    let config = Config::for_model(crate::config::PARAKEET_UNIFIED_MODEL, "1120ms")
        .expect("valid rollback config");
    assert!(!config.speech_gate);
    let mut daemon = Daemon::new(
        config,
        Box::new(NoopRecorder::default()),
        Box::new(StaticRecognizer::new("rollback transcript")),
        Box::new(StaticSink::new(DeliveryOutcome::Delivered {
            backend: "test".to_owned(),
        })),
        Box::new(JsonHistoryStore::new(directory.path().join("history.json"))),
        Box::new(NoopNotifier::default()),
        RecordingStore::new(directory.path().join("recordings")),
    );
    daemon.initialize();
    let _ = daemon.handle(IpcRequest::Toggle);
    let finish = daemon.handle(IpcRequest::Toggle);

    match finish {
        IpcResponse::Command { result } => {
            assert_eq!(result.status.model, crate::config::PARAKEET_UNIFIED_MODEL);
            assert_eq!(result.transcript.as_deref(), Some("rollback transcript"));
            assert_eq!(result.transcription, TranscriptionStatus::Succeeded);
        }
        other => panic!("unexpected response: {other:?}"),
    }
}
