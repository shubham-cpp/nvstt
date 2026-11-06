use std::{
    env, fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use tokio::net::UnixListener;
use tracing::{info, warn};

use crate::{
    audio_input::{CpalInputs, DevelopmentInputs, InputProvider},
    audio_pipeline::{AudioPipeline, MODEL_SAMPLE_RATE},
    config::Config,
    delivery::{NativeFirstSink, TextSink},
    dictation_transcript::{
        DictationTranscript, EmptyTranscript, Replacements, dictation_transcript,
    },
    domain::{
        AttemptOutcome, DaemonState, DeliveryOutcome, DeliveryStatus, FailureStage, HistoryRecord,
        InputPreference, StatusSnapshot, TranscriptionStatus, new_session_id, now_ms,
    },
    error::{AppError, Result},
    history::{HistoryStore, JsonHistoryStore},
    ipc::{CommandResult, IpcRequest, IpcResponse, error_response, read_request, write_response},
    model::ModelStatus,
    notifier::{DesktopNotifier, NotificationEvent, Notifier},
    paths::AppPaths,
    recognizer::{
        RecognitionOutcome, StaticRecognizer, StreamingRecognizer, UnavailableRecognizer,
        create_recognizer,
    },
    recorder::{AudioSource, CaptureReport, CpalRecorder, NoopRecorder, Recorder},
    recordings::{CaptureStatus, RecordingInput, RecordingSettings, RecordingStore, SaveWarning},
};

struct RecognitionWorker {
    command_tx: Sender<WorkerCommand>,
    completion_rx: Receiver<WorkerCompletion>,
    join: Option<thread::JoinHandle<()>>,
    #[cfg(test)]
    finish_sent: Option<Sender<()>>,
}

enum WorkerCommand {
    Finish { capture_only: bool },
    Cancel,
}

struct WorkerResult {
    audio: Vec<f32>,
    sample_rate: i32,
    capture: CaptureReport,
    drain_failed: bool,
    outcome: Result<RecognitionOutcome>,
}

struct WorkerCompletion {
    recognizer: Box<dyn StreamingRecognizer>,
    result: Option<WorkerResult>,
}

impl RecognitionWorker {
    fn spawn(
        recognizer: Box<dyn StreamingRecognizer>,
        source: AudioSource,
        denoise: bool,
    ) -> Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let (completion_tx, completion_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("nvstt-recognizer".to_owned())
            .spawn(move || {
                run_recognition_worker(recognizer, source, denoise, command_rx, completion_tx)
            })
            .map_err(|error| {
                AppError::Unavailable(format!("could not start recognition worker: {error}"))
            })?;
        Ok(Self {
            command_tx,
            completion_rx,
            join: Some(join),
            #[cfg(test)]
            finish_sent: None,
        })
    }

    fn finish(
        mut self,
        capture_only: bool,
    ) -> Result<(Box<dyn StreamingRecognizer>, WorkerResult)> {
        self.command_tx
            .send(WorkerCommand::Finish { capture_only })
            .map_err(|_| {
                AppError::Unavailable("recognition worker stopped unexpectedly".to_owned())
            })?;
        #[cfg(test)]
        if let Some(sent) = self.finish_sent.take() {
            sent.send(()).expect("notify test that Finish is queued");
        }
        let join_result = self.join.take().expect("worker join handle").join();
        if join_result.is_err() {
            return Err(AppError::Unavailable(
                "recognition worker panicked".to_owned(),
            ));
        }
        let completion = self.completion_rx.recv().map_err(|_| {
            AppError::Unavailable("recognition worker returned no result".to_owned())
        })?;
        let result = completion
            .result
            .ok_or_else(|| AppError::Unavailable("recognition worker canceled".to_owned()))?;
        Ok((completion.recognizer, result))
    }

    fn cancel(mut self) -> Result<Box<dyn StreamingRecognizer>> {
        self.command_tx.send(WorkerCommand::Cancel).map_err(|_| {
            AppError::Unavailable("recognition worker stopped unexpectedly".to_owned())
        })?;
        let join_result = self.join.take().expect("worker join handle").join();
        if join_result.is_err() {
            return Err(AppError::Unavailable(
                "recognition worker panicked".to_owned(),
            ));
        }
        let completion = self.completion_rx.recv().map_err(|_| {
            AppError::Unavailable("recognition worker returned no result".to_owned())
        })?;
        Ok(completion.recognizer)
    }
}

impl Drop for RecognitionWorker {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = self.command_tx.send(WorkerCommand::Cancel);
            let _ = join.join();
        }
    }
}

fn run_recognition_worker(
    mut recognizer: Box<dyn StreamingRecognizer>,
    mut source: AudioSource,
    denoise: bool,
    command_rx: Receiver<WorkerCommand>,
    completion_tx: Sender<WorkerCompletion>,
) {
    let mut audio_pipeline = AudioPipeline::new(denoise);
    let mut worker_error: Option<AppError> = None;
    let mut audio = Vec::new();
    let mut drain_failed = false;
    let result = loop {
        if !drain_failed {
            feed_available_audio(
                &mut source,
                recognizer.as_mut(),
                &mut audio_pipeline,
                &mut audio,
                &mut worker_error,
                &mut drain_failed,
                true,
            );
        }

        match command_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(WorkerCommand::Finish { capture_only }) => {
                break Some(finish_worker_session(
                    &mut source,
                    recognizer.as_mut(),
                    &mut audio_pipeline,
                    audio,
                    worker_error,
                    drain_failed,
                    capture_only,
                ));
            }
            Ok(WorkerCommand::Cancel) => {
                let _ = recognizer.cancel_session();
                break None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = recognizer.cancel_session();
                break Some(WorkerResult {
                    audio,
                    sample_rate: source.sample_rate(),
                    capture: source.capture_report(),
                    drain_failed,
                    outcome: Err(AppError::Unavailable(
                        "recognition worker command channel closed".to_owned(),
                    )),
                });
            }
        }
    };

    let _ = completion_tx.send(WorkerCompletion { recognizer, result });
}

// Returns true when the current queue snapshot is empty or the source has failed.
fn feed_available_audio(
    source: &mut AudioSource,
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
    audio: &mut Vec<f32>,
    worker_error: &mut Option<AppError>,
    drain_failed: &mut bool,
    recognize: bool,
) -> bool {
    let samples = match source.drain() {
        Ok(samples) => samples,
        Err(error) => {
            *drain_failed = true;
            if worker_error.is_none() {
                *worker_error = Some(error);
            }
            return true;
        }
    };
    if samples.is_empty() {
        return true;
    }
    audio.extend_from_slice(&samples);
    if recognize && worker_error.is_none() {
        *worker_error =
            feed_recognizer(recognizer, audio_pipeline, source.sample_rate(), &samples).err();
    }
    false
}

fn finish_worker_session(
    source: &mut AudioSource,
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
    mut audio: Vec<f32>,
    mut worker_error: Option<AppError>,
    mut drain_failed: bool,
    capture_only: bool,
) -> WorkerResult {
    let pending_start = audio.len();
    while !drain_failed
        && !feed_available_audio(
            source,
            recognizer,
            audio_pipeline,
            &mut audio,
            &mut worker_error,
            &mut drain_failed,
            false,
        )
    {}

    let outcome = if let Err(error) = source.integrity_result() {
        Err(error)
    } else if capture_only {
        Err(AppError::Unavailable(
            "recognition worker capture only".to_owned(),
        ))
    } else if let Some(error) = worker_error {
        Err(error)
    } else {
        feed_recognizer(
            recognizer,
            audio_pipeline,
            source.sample_rate(),
            &audio[pending_start..],
        )
        .and_then(|()| audio_pipeline.finish())
        .and_then(|samples| {
            if !samples.is_empty() {
                recognizer.accept_audio(MODEL_SAMPLE_RATE, &samples)?;
            }
            recognizer.finish_session()
        })
    };
    if outcome.is_err() {
        let _ = recognizer.cancel_session();
    }
    WorkerResult {
        audio,
        sample_rate: source.sample_rate(),
        capture: source.capture_report(),
        drain_failed,
        outcome,
    }
}

fn feed_recognizer(
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
    sample_rate: i32,
    samples: &[f32],
) -> Result<()> {
    let samples = audio_pipeline.accept_audio(sample_rate, samples)?;
    if !samples.is_empty() {
        recognizer.accept_audio(MODEL_SAMPLE_RATE, &samples)?;
    }
    Ok(())
}

pub struct Daemon {
    config: Config,
    vocabulary_path: Option<PathBuf>,
    session_replacements: Replacements,
    status: StatusSnapshot,
    recorder: Box<dyn Recorder>,
    recognizer: Option<Box<dyn StreamingRecognizer>>,
    worker: Option<RecognitionWorker>,
    delivery: Box<dyn TextSink>,
    history: Box<dyn HistoryStore>,
    notifier: Box<dyn Notifier>,
    recordings: RecordingStore,
    session_started: Option<Instant>,
    inputs: Box<dyn InputProvider>,
    publisher: Option<tokio::sync::watch::Sender<StatusSnapshot>>,
    shutting_down: Arc<AtomicBool>,
}

impl Daemon {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        recorder: Box<dyn Recorder>,
        recognizer: Box<dyn StreamingRecognizer>,
        delivery: Box<dyn TextSink>,
        history: Box<dyn HistoryStore>,
        notifier: Box<dyn Notifier>,
        recordings: RecordingStore,
    ) -> Self {
        let status = StatusSnapshot::starting(&config);
        Self {
            session_replacements: config.replacements.clone(),
            vocabulary_path: None,
            config,
            status,
            recorder,
            recognizer: Some(recognizer),
            worker: None,
            delivery,
            history,
            notifier,
            recordings,
            session_started: None,
            inputs: Box::new(DevelopmentInputs),
            publisher: None,
            shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn attach_runtime(
        &mut self,
        publisher: tokio::sync::watch::Sender<StatusSnapshot>,
        shutting_down: Arc<AtomicBool>,
    ) {
        self.publisher = Some(publisher);
        self.shutting_down = shutting_down;
    }

    fn publish(&mut self) {
        self.status.revision = self.status.revision.saturating_add(1);
        if let Some(publisher) = &self.publisher {
            publisher.send_replace(self.status.clone());
        }
    }

    pub(crate) fn shutdown(&mut self) {
        if self.worker.is_some() || self.status.state == DaemonState::Listening {
            let _ = self.cancel();
        }
    }

    fn refresh_inputs(&mut self) -> Result<()> {
        match self.inputs.catalog() {
            Ok(catalog) => {
                if self.status.audio.inputs != catalog.inputs
                    || self.status.audio.default_input != catalog.default_input
                    || self.status.audio.stale
                {
                    self.status.generation = self.status.generation.saturating_add(1);
                }
                self.status.audio.inputs = catalog.inputs;
                self.status.audio.default_input = catalog.default_input;
                self.status.audio.available = self.status.audio.selected_input().is_some();
                self.status.audio.stale = false;
                self.status.feedback.command_error = None;
                self.publish();
                Ok(())
            }
            Err(error) => {
                if !self.status.audio.stale {
                    self.status.generation = self.status.generation.saturating_add(1);
                }
                self.status.audio.stale = true;
                self.status.audio.available = false;
                self.status.feedback.command_error = Some("Input refresh failed".to_owned());
                self.publish();
                Err(error)
            }
        }
    }

    fn select_input(&mut self, preference: InputPreference) -> IpcResponse {
        if self.status.state != DaemonState::Idle {
            return IpcResponse::Error {
                code: "busy".to_owned(),
                message: "Change inputs between dictations".to_owned(),
            };
        }
        if let Err(error) = self.refresh_inputs() {
            return error_response(&error);
        }
        if let Err(error) = self.inputs.resolve(&preference) {
            self.status.feedback.command_error = Some("Selected input unavailable".to_owned());
            self.publish();
            return error_response(&error);
        }
        if let Err(error) = self.recorder.select_input(preference.clone()) {
            return error_response(&error);
        }
        self.status.audio.selected = preference;
        self.status.audio.available = true;
        self.status.feedback.command_error = None;
        self.status.generation = self.status.generation.saturating_add(1);
        self.publish();
        self.command_success(
            None,
            TranscriptionStatus::NotStarted,
            DeliveryStatus::NotAttempted,
            "input selected for the next dictation",
        )
    }

    /// Attach the read-only model readiness metadata exposed through status.
    pub fn set_model_status(&mut self, model: &ModelStatus) {
        self.status.model_ready = model.ready;
        self.status.model_path = Some(model.path.display().to_string());
    }

    pub fn initialize(&mut self) {
        if let Err(error) = self.recordings.reconcile() {
            warn!(error = %error, "could not reconcile recent audio recordings");
        }
        let startup_error = match self.recognizer.as_mut() {
            None => Some("recognizer is not available".to_owned()),
            Some(recognizer) => match recognizer.start_session() {
                Ok(()) => recognizer
                    .cancel_session()
                    .err()
                    .map(|error| error.to_string()),
                Err(error) => Some(error.to_string()),
            },
        };
        self.status.state = DaemonState::Idle;
        self.status.message = startup_error
            .clone()
            .map(|error| format!("model unavailable: {error}"))
            .unwrap_or_else(|| "ready".to_owned());
        self.status.feedback.initialization_failed = startup_error.is_some();
        let _ = self.refresh_inputs();
        self.publish();
        match startup_error {
            Some(error) => self.safe_notify(NotificationEvent::InitializationFailed(error)),
            None => self.safe_notify(NotificationEvent::Initialized),
        }
    }

    pub fn handle(&mut self, request: IpcRequest) -> IpcResponse {
        let mut response = match request {
            IpcRequest::AudioInputs => {
                if self.status.state != DaemonState::Idle {
                    return IpcResponse::Error {
                        code: "busy".to_owned(),
                        message: "Refresh inputs between dictations".to_owned(),
                    };
                }
                match self.refresh_inputs() {
                    Ok(()) => IpcResponse::AudioInputs {
                        audio: self.status.audio.clone(),
                    },
                    Err(error) => error_response(&error),
                }
            }
            IpcRequest::SelectInput { preference } => self.select_input(preference),
            IpcRequest::ClearError => {
                if matches!(
                    self.status.feedback.outcome,
                    Some(AttemptOutcome::Failed { .. })
                ) {
                    self.status.feedback.outcome = None;
                }
                self.status.feedback.command_error = None;
                self.publish();
                self.command_success(
                    None,
                    TranscriptionStatus::NotStarted,
                    DeliveryStatus::NotAttempted,
                    "last error cleared",
                )
            }
            IpcRequest::Toggle => self.toggle(),
            IpcRequest::Cancel => self.cancel(),
            IpcRequest::Status => IpcResponse::Status {
                snapshot: self.status.clone(),
            },
            IpcRequest::History { limit } => match self.history.list(limit) {
                Ok(records) => IpcResponse::History { records },
                Err(error) => error_response(&error),
            },
        };
        if let IpcResponse::Command { result } = &mut response {
            result.status = self.status.clone();
        }
        response
    }

    fn toggle(&mut self) -> IpcResponse {
        match self.status.state {
            DaemonState::Idle => self.start_listening(),
            DaemonState::Listening => self.finish_listening(),
            state => self.command_failure(
                "busy",
                format!("cannot toggle while daemon is in {state:?} state"),
                TranscriptionStatus::NotStarted,
                DeliveryStatus::NotAttempted,
            ),
        }
    }

    fn start_listening(&mut self) -> IpcResponse {
        let vocabulary = match &self.vocabulary_path {
            Some(path) => crate::vocabulary::Vocabulary::load(path),
            None => Ok(crate::vocabulary::Vocabulary::default()),
        };
        let vocabulary = match vocabulary {
            Ok(vocabulary) => vocabulary,
            Err(error) => {
                return self.command_failure(
                    "vocabulary_unavailable",
                    error.to_string(),
                    TranscriptionStatus::NotStarted,
                    DeliveryStatus::NotAttempted,
                );
            }
        };
        // Pin rules for this utterance. Edits during capture apply next time.
        self.session_replacements = vocabulary.apply_to(&self.config.replacements);
        let active = match self.inputs.resolve(&self.status.audio.selected) {
            Ok(input) => input,
            Err(error) => {
                self.status.audio.available = false;
                self.status.audio.stale = true;
                return self.command_failure(
                    "input_unavailable",
                    error.to_string(),
                    TranscriptionStatus::NotStarted,
                    DeliveryStatus::NotAttempted,
                );
            }
        };
        if self.status.audio.selected == InputPreference::SystemDefault {
            if self.status.audio.default_input.as_ref() != Some(&active) {
                self.status.generation = self.status.generation.saturating_add(1);
            }
            self.status.audio.default_input = Some(active.clone());
        }
        // Pin the resolved default for this capture. A later default change
        // takes effect only when the next session resolves it again.
        if let Err(error) = self.recorder.select_input(InputPreference::Device {
            id: active.id.clone(),
        }) {
            return self.command_failure(
                "input_unavailable",
                error.to_string(),
                TranscriptionStatus::NotStarted,
                DeliveryStatus::NotAttempted,
            );
        }

        let Some(mut recognizer) = self.recognizer.take() else {
            return self.command_failure(
                "recognizer_unavailable",
                "recognizer is not available",
                TranscriptionStatus::NotStarted,
                DeliveryStatus::NotAttempted,
            );
        };
        if let Err(error) = recognizer.start_session() {
            self.recognizer = Some(recognizer);
            return self.command_failure(
                "recognizer_unavailable",
                error.to_string(),
                TranscriptionStatus::NotStarted,
                DeliveryStatus::NotAttempted,
            );
        }
        if let Err(error) = self.recorder.start() {
            let _ = recognizer.cancel_session();
            self.recognizer = Some(recognizer);
            return self.command_failure(
                "recorder_unavailable",
                error.to_string(),
                TranscriptionStatus::NotStarted,
                DeliveryStatus::NotAttempted,
            );
        }

        let source = match self.recorder.audio_source() {
            Ok(source) => source,
            Err(error) => {
                let _ = self.recorder.cancel();
                let _ = recognizer.cancel_session();
                self.recognizer = Some(recognizer);
                return self.command_failure(
                    "recorder_unavailable",
                    error.to_string(),
                    TranscriptionStatus::NotStarted,
                    DeliveryStatus::NotAttempted,
                );
            }
        };

        match RecognitionWorker::spawn(recognizer, source, self.config.denoise) {
            Ok(worker) => self.worker = Some(worker),
            Err(error) => {
                let _ = self.recorder.cancel();
                self.recognizer = Some(Box::new(UnavailableRecognizer::new(error.to_string())));
                return self.command_failure(
                    "recognizer_unavailable",
                    error.to_string(),
                    TranscriptionStatus::NotStarted,
                    DeliveryStatus::NotAttempted,
                );
            }
        }

        let session_id = new_session_id();
        self.session_started = Some(Instant::now());
        self.status.state = DaemonState::Listening;
        self.status.message = "listening".to_owned();
        self.status.session_id = Some(session_id);
        self.status.audio.active = Some(active);
        self.status.audio.available = true;
        self.status.feedback.initialization_failed = false;
        self.status.feedback.outcome = None;
        self.status.feedback.command_error = None;
        self.status.feedback.warning = None;
        self.status.generation = self.status.generation.saturating_add(1);
        self.publish();
        self.safe_notify(NotificationEvent::ListeningStarted);
        self.command_success(
            None,
            TranscriptionStatus::NotStarted,
            DeliveryStatus::NotAttempted,
            "listening",
        )
    }

    fn finish_listening(&mut self) -> IpcResponse {
        self.status.state = DaemonState::Finalizing;
        self.status.message = "finalizing".to_owned();
        self.publish();
        self.safe_notify(NotificationEvent::Finalizing);

        let stop = self.recorder.stop();
        let stopped_at_ms = now_ms();
        let stop_failed = stop.is_err();
        let capture_only = stop_failed || stop.as_ref().is_ok_and(CaptureReport::failed);
        let worker = self.finish_recognition_worker(capture_only);
        let stop_error = stop.err();
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                warn!(error = %error, "recognition worker returned no audio; audio was not saved");
                let reason = stop_error.unwrap_or(error).to_string();
                self.status.feedback.warning = Some("audio was not saved".to_owned());
                let response = self.transcription_failure(reason);
                return Self::with_recording_warning(
                    response,
                    Some("audio was not saved".to_owned()),
                );
            }
        };
        let transcription = if let Some(error) = stop_error {
            Err(error.to_string())
        } else {
            worker
                .outcome
                .map_err(|error| error.to_string())
                .and_then(|recognition| {
                    dictation_transcript(recognition, &self.session_replacements, self.config.itn)
                        .map_err(|EmptyTranscript| "transcript was empty".to_owned())
                })
        };
        let status = match &transcription {
            Ok(DictationTranscript::NoContent) => TranscriptionStatus::NoSpeech,
            Ok(DictationTranscript::Ready(_)) => TranscriptionStatus::Succeeded,
            Err(_) => TranscriptionStatus::Failed,
        };
        let recording = RecordingInput {
            session_id: self
                .status
                .session_id
                .clone()
                .expect("listening session ID"),
            stopped_at_ms,
            sample_rate: worker.sample_rate,
            settings: RecordingSettings {
                model: self.config.model.clone(),
                streaming_profile: self.config.streaming_profile.clone(),
                speech_gate: self.config.speech_gate,
                denoise: self.config.denoise,
                itn: self.config.itn,
            },
            capture: CaptureStatus {
                dropped_samples: worker.capture.dropped_samples,
                backend_failed: worker.capture.backend_failed,
                duration_exceeded: worker.capture.duration_exceeded,
                stop_failed,
                drain_failed: worker.drain_failed,
            },
            transcription: status,
            samples: worker.audio,
        };
        let warning = match self.recordings.save(&recording) {
            Ok(saved) => {
                let warnings = saved
                    .warnings
                    .into_iter()
                    .map(|warning| match warning {
                        SaveWarning::DirectorySyncFailed(error) => {
                            format!("recording saved, but directory sync failed: {error}")
                        }
                        SaveWarning::PruneFailed(error) => format!(
                            "old audio could not be pruned; retention prune failed: {error}"
                        ),
                    })
                    .collect::<Vec<_>>();
                let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
                if let Some(ref warning) = warning {
                    warn!(path = %saved.path.display(), %warning, "recent audio saved with warning");
                }
                warning
            }
            Err(error) => {
                warn!(error = %error, "audio was not saved");
                Some("audio was not saved".to_owned())
            }
        };
        let response = match transcription {
            Ok(DictationTranscript::NoContent) => self.no_speech(),
            Ok(DictationTranscript::Ready(transcript)) => self.deliver_transcript(transcript),
            Err(reason) => self.transcription_failure(reason),
        };
        if let Some(warning) = &warning {
            self.add_warning(warning.clone());
        }
        self.publish();
        Self::with_recording_warning(response, warning)
    }

    fn add_warning(&mut self, warning: String) {
        match &mut self.status.feedback.warning {
            Some(existing) => {
                existing.push_str("; ");
                existing.push_str(&warning);
            }
            slot => *slot = Some(warning),
        }
    }

    fn with_recording_warning(mut response: IpcResponse, warning: Option<String>) -> IpcResponse {
        if let (IpcResponse::Command { result }, Some(warning)) = (&mut response, warning) {
            result.message.push_str("; ");
            result.message.push_str(&warning);
        }
        response
    }

    fn deliver_transcript(&mut self, transcript: String) -> IpcResponse {
        self.safe_notify(NotificationEvent::Transcribed);
        let id = self
            .status
            .session_id
            .clone()
            .unwrap_or_else(new_session_id);
        let duration_ms = self
            .session_started
            .take()
            .map(|started| started.elapsed().as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or_default();
        let record = HistoryRecord::new(
            id.clone(),
            duration_ms,
            self.config.model.clone(),
            transcript.clone(),
        );
        let history_warning = self
            .history
            .append(record)
            .err()
            .map(|error| error.to_string());

        if let Some(warning) = &history_warning {
            self.add_warning(format!("history append failed: {warning}"));
        }
        self.status.state = DaemonState::Delivering;
        self.status.message = "delivering".to_owned();
        self.publish();
        let outcome = if self.shutting_down.load(Ordering::Acquire) {
            DeliveryOutcome::Failed {
                reason: "shutdown requested before delivery".to_owned(),
            }
        } else {
            match self.delivery.send_final_text(&transcript) {
                Ok(outcome) => outcome,
                Err(error) => DeliveryOutcome::Failed {
                    reason: error.to_string(),
                },
            }
        };
        if let Err(error) = self
            .history
            .update_delivery(&id, outcome.status(), outcome.backend())
        {
            warn!(error = %error, "could not update delivery result in history");
            self.add_warning(format!("history delivery update failed: {error}"));
        }

        let message = match history_warning {
            Some(warning) => format!("{}; history warning: {warning}", outcome.message()),
            None => outcome.message(),
        };
        match &outcome {
            DeliveryOutcome::Delivered { backend } => {
                self.safe_notify(NotificationEvent::Delivered(backend.clone()));
            }
            DeliveryOutcome::CopiedToClipboard { .. } => {
                self.safe_notify(NotificationEvent::CopiedToClipboard);
            }
            DeliveryOutcome::Failed { reason } => {
                self.safe_notify(NotificationEvent::DeliveryFailed(reason.clone()));
            }
        }

        self.status.feedback.outcome = Some(match &outcome {
            DeliveryOutcome::Delivered { .. } => AttemptOutcome::Delivered,
            DeliveryOutcome::CopiedToClipboard { .. } => AttemptOutcome::Copied,
            DeliveryOutcome::Failed { reason } => AttemptOutcome::Failed {
                stage: FailureStage::Delivery,
                reason: reason.clone(),
            },
        });
        let delivery_status = outcome.status();
        self.reset_to_idle();
        self.command_result(
            !matches!(outcome, DeliveryOutcome::Failed { .. }),
            Some(transcript),
            TranscriptionStatus::Succeeded,
            delivery_status,
            message,
        )
    }

    fn cancel(&mut self) -> IpcResponse {
        match self.status.state {
            DaemonState::Listening | DaemonState::Finalizing => {
                let recorder_error = self.recorder.cancel().err();
                let worker_error = if let Some(worker) = self.worker.take() {
                    match worker.cancel() {
                        Ok(recognizer) => {
                            self.recognizer = Some(recognizer);
                            None
                        }
                        Err(error) => Some(error),
                    }
                } else {
                    None
                };
                let recognizer_error = self
                    .recognizer
                    .as_mut()
                    .and_then(|recognizer| recognizer.cancel_session().err());
                self.session_started = None;
                self.status.feedback.outcome = Some(AttemptOutcome::Canceled);
                self.reset_to_idle();
                self.safe_notify(NotificationEvent::Canceled);
                let message = recorder_error
                    .or(worker_error)
                    .or(recognizer_error)
                    .map(|error| format!("canceled with warning: {error}"))
                    .unwrap_or_else(|| "canceled".to_owned());
                self.command_success(
                    None,
                    TranscriptionStatus::NotStarted,
                    DeliveryStatus::NotAttempted,
                    message,
                )
            }
            state => self.command_failure(
                "invalid_state",
                format!("cannot cancel while daemon is in {state:?} state"),
                TranscriptionStatus::NotStarted,
                DeliveryStatus::NotAttempted,
            ),
        }
    }

    fn transcription_failure(&mut self, reason: String) -> IpcResponse {
        let _ = self.recorder.cancel();
        if let Some(worker) = self.worker.take()
            && let Ok(recognizer) = worker.cancel()
        {
            self.recognizer = Some(recognizer);
        }
        if let Some(recognizer) = self.recognizer.as_mut() {
            let _ = recognizer.cancel_session();
        }
        self.session_started = None;
        self.status.feedback.outcome = Some(AttemptOutcome::Failed {
            stage: FailureStage::Transcription,
            reason: reason.clone(),
        });
        self.reset_to_idle();
        self.safe_notify(NotificationEvent::TranscriptionFailed(reason.clone()));
        self.command_failure(
            "transcription_failed",
            reason,
            TranscriptionStatus::Failed,
            DeliveryStatus::NotAttempted,
        )
    }

    fn no_speech(&mut self) -> IpcResponse {
        self.session_started = None;
        self.status.feedback.outcome = Some(AttemptOutcome::NoSpeech);
        self.reset_to_idle();
        self.safe_notify(NotificationEvent::NoSpeechDetected);
        self.command_success(
            None,
            TranscriptionStatus::NoSpeech,
            DeliveryStatus::NotAttempted,
            "no speech detected; nothing was sent",
        )
    }

    fn finish_recognition_worker(&mut self, capture_only: bool) -> Result<WorkerResult> {
        let worker = self.worker.take().ok_or_else(|| {
            AppError::Unavailable("recognition worker is not available".to_owned())
        })?;
        let (recognizer, result) = worker.finish(capture_only)?;
        self.recognizer = Some(recognizer);
        Ok(result)
    }

    fn reset_to_idle(&mut self) {
        self.status.state = DaemonState::Idle;
        self.status.message = "ready".to_owned();
        self.status.session_id = None;
        self.status.audio.active = None;
        self.status.generation = self.status.generation.saturating_add(1);
        self.publish();
    }

    fn safe_notify(&mut self, event: NotificationEvent) {
        if let Err(error) = self.notifier.notify(event) {
            warn!(error = %error, "desktop notification failed");
        }
    }

    fn command_success(
        &self,
        transcript: Option<String>,
        transcription: TranscriptionStatus,
        delivery: DeliveryStatus,
        message: impl Into<String>,
    ) -> IpcResponse {
        self.command_result(true, transcript, transcription, delivery, message)
    }

    fn command_failure(
        &mut self,
        _code: &str,
        message: impl Into<String>,
        transcription: TranscriptionStatus,
        delivery: DeliveryStatus,
    ) -> IpcResponse {
        let message = message.into();
        if self.status.state == DaemonState::Idle
            && transcription == TranscriptionStatus::NotStarted
        {
            self.status.feedback.outcome = Some(AttemptOutcome::Failed {
                stage: FailureStage::Start,
                reason: message.clone(),
            });
            self.publish();
            self.safe_notify(NotificationEvent::StartFailed(message.clone()));
        }
        self.command_result(false, None, transcription, delivery, message)
    }

    fn command_result(
        &self,
        ok: bool,
        transcript: Option<String>,
        transcription: TranscriptionStatus,
        delivery: DeliveryStatus,
        message: impl Into<String>,
    ) -> IpcResponse {
        IpcResponse::Command {
            result: CommandResult {
                ok,
                status: self.status.clone(),
                transcript,
                transcription,
                delivery,
                message: message.into(),
            },
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub async fn run_daemon(paths: AppPaths, config: Config) -> Result<()> {
    run_daemon_with_tray(paths, config, true).await
}

pub async fn run_daemon_with_tray(
    paths: AppPaths,
    config: Config,
    tray_enabled: bool,
) -> Result<()> {
    paths.create_user_dirs()?;
    let listener = bind_socket(&paths.socket_path)?;
    let starting = StatusSnapshot::starting(&config);
    let host_available = Arc::new(AtomicBool::new(false));
    let factory_paths = paths.clone();
    let notification_host = host_available.clone();
    let (controller, join) = match crate::controller::start(starting, move || {
        default_daemon(config, &factory_paths, notification_host)
    }) {
        Ok(runtime) => runtime,
        Err(error) => {
            drop(listener);
            let _ = remove_socket(&paths.socket_path);
            return Err(error);
        }
    };
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let tray = tray_enabled.then(|| {
        tokio::spawn(crate::tray::run(
            controller.clone(),
            host_available,
            stop_rx,
        ))
    });
    let mut clients = tokio::task::JoinSet::new();
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    info!(socket = %paths.socket_path.display(), "nvstt daemon started");
    let result: Result<()> = loop {
        tokio::select! {
            result = &mut shutdown => break result,
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            accepted = listener.accept(), if clients.len() < 32 => {
                let (stream, _) = match accepted { Ok(value) => value, Err(error) => break Err(error.into()) };
                let handle = controller.clone();
                clients.spawn(async move {
                    let (reader, writer) = stream.into_split();
                    let response = match tokio::time::timeout(Duration::from_secs(3), read_request(reader)).await {
                        Ok(Ok(request)) => handle.request(request, None).await,
                        Ok(Err(error)) => error_response(&error),
                        Err(_) => IpcResponse::Error { code: "timeout".to_owned(), message: "request read timed out".to_owned() },
                    };
                    let _ = tokio::time::timeout(Duration::from_secs(3), write_response(writer, &response)).await;
                });
            }
        }
    };
    controller.shutdown();
    stop_tx.send_replace(true);
    if let Some(tray) = tray {
        let _ = tray.await;
    }
    let joined = tokio::task::spawn_blocking(move || join.join()).await;
    clients.abort_all();
    let cleanup = remove_socket(&paths.socket_path);
    result?;
    if !matches!(joined, Ok(Ok(()))) {
        return Err(AppError::Unavailable(
            "controller shutdown failed".to_owned(),
        ));
    }
    cleanup
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(AppError::Io),
            _ = terminate.recv() => Ok(()),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.map_err(AppError::Io)
    }
}

fn remove_socket(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn bind_socket(path: &Path) -> Result<UnixListener> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            return Err(AppError::InvalidState(format!(
                "IPC path exists and is not a Unix socket: {}",
                path.display()
            )));
        }
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                return Err(AppError::InvalidState(format!(
                    "daemon already running at {}",
                    path.display()
                )));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                remove_socket(path)?
            }
            Err(error) => return Err(error.into()),
        }
    }

    let listener = UnixListener::bind(path)?;
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        drop(listener);
        let _ = remove_socket(path);
        return Err(error.into());
    }
    Ok(listener)
}

fn default_daemon(config: Config, paths: &AppPaths, host: Arc<AtomicBool>) -> Daemon {
    let model_status = ModelStatus::inspect(&config, paths);
    let (recorder, recognizer): (Box<dyn Recorder>, Box<dyn StreamingRecognizer>) =
        match env::var("NVSTT_DEV_TRANSCRIPT") {
            Ok(transcript) => (
                Box::new(NoopRecorder::default()),
                Box::new(StaticRecognizer::new(transcript)),
            ),
            Err(_) => {
                let model_dir = paths.model_dir.join(config.artifact_name());
                match create_recognizer(&config, model_dir) {
                    Ok(recognizer) => (Box::new(CpalRecorder::default()), recognizer),
                    Err(error) => (
                        Box::new(CpalRecorder::default()),
                        Box::new(UnavailableRecognizer::new(error.to_string())),
                    ),
                }
            }
        };
    let mut daemon = Daemon::new(
        config,
        recorder,
        recognizer,
        Box::new(NativeFirstSink::new_with_restore_token(
            paths.state_dir.join("remote-desktop.restore-token"),
        )),
        Box::new(JsonHistoryStore::new(&paths.history_path)),
        Box::new(crate::notifier::TrayAwareNotifier::new(
            DesktopNotifier,
            host,
        )),
        RecordingStore::new(paths.state_dir.join("recordings")),
    );
    if env::var("NVSTT_DEV_TRANSCRIPT").is_err() {
        daemon.inputs = Box::new(CpalInputs);
    }
    daemon.vocabulary_path = Some(paths.vocabulary_path());
    daemon.set_model_status(&model_status);
    daemon
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "app_tray_tests.rs"]
mod tray_tests;
