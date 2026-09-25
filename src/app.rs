use std::{
    env, fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::Path,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use tokio::net::UnixListener;
use tracing::{info, warn};

use crate::{
    audio_pipeline::{AudioPipeline, MODEL_SAMPLE_RATE},
    config::Config,
    delivery::{NativeFirstSink, TextSink},
    dictation_transcript::{DictationTranscript, EmptyTranscript, dictation_transcript},
    domain::{
        DaemonState, DeliveryOutcome, DeliveryStatus, HistoryRecord, StatusSnapshot,
        TranscriptionStatus, new_session_id, now_ms,
    },
    error::{AppError, Result},
    history::{HistoryStore, JsonHistoryStore},
    ipc::{CommandResult, IpcRequest, IpcResponse, error_response, read_request, write_response},
    model::ModelStatus,
    notifier::{DesktopNotifier, NotificationEvent, Notifier},
    paths::AppPaths,
    recognizer::{
        RecognitionOutcome, StaticRecognizer, StreamingRecognizer, UnavailableRecognizer,
        create_recognizer, execution_provider,
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
        let _ = recognizer.cancel_session();
        Err(error)
    } else if capture_only {
        let _ = recognizer.cancel_session();
        Err(AppError::Unavailable(
            "recognition worker capture only".to_owned(),
        ))
    } else if let Some(error) = worker_error {
        let _ = recognizer.cancel_session();
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
    status: StatusSnapshot,
    recorder: Box<dyn Recorder>,
    recognizer: Option<Box<dyn StreamingRecognizer>>,
    worker: Option<RecognitionWorker>,
    delivery: Box<dyn TextSink>,
    history: Box<dyn HistoryStore>,
    notifier: Box<dyn Notifier>,
    recordings: RecordingStore,
    session_started: Option<Instant>,
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
        let status = StatusSnapshot {
            state: DaemonState::Starting,
            model: config.model.clone(),
            streaming_profile: config.streaming_profile.clone(),
            speech_gate_enabled: config.speech_gate,
            execution_provider: execution_provider().to_owned(),
            model_ready: false,
            model_path: None,
            message: "starting".to_owned(),
            session_id: None,
        };
        Self {
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
        }
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
        match startup_error {
            Some(error) => self.safe_notify(NotificationEvent::InitializationFailed(error)),
            None => self.safe_notify(NotificationEvent::Initialized),
        }
    }

    pub fn handle(&mut self, request: IpcRequest) -> IpcResponse {
        match request {
            IpcRequest::Toggle => self.toggle(),
            IpcRequest::Cancel => self.cancel(),
            IpcRequest::Status => IpcResponse::Status {
                snapshot: self.status.clone(),
            },
            IpcRequest::History { limit } => match self.history.list(limit) {
                Ok(records) => IpcResponse::History { records },
                Err(error) => error_response(&error),
            },
        }
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
        let model_ready = self.status.model_ready;
        let model_path = self.status.model_path.clone();
        let execution_provider = self.status.execution_provider.clone();
        self.status = StatusSnapshot {
            state: DaemonState::Listening,
            model: self.config.model.clone(),
            streaming_profile: self.config.streaming_profile.clone(),
            speech_gate_enabled: self.config.speech_gate,
            execution_provider,
            model_ready,
            model_path,
            message: "listening".to_owned(),
            session_id: Some(session_id),
        };
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
                    dictation_transcript(recognition, &self.config.replacements, self.config.itn)
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
        Self::with_recording_warning(response, warning)
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

        self.status.state = DaemonState::Delivering;
        self.status.message = "delivering".to_owned();
        let outcome = match self.delivery.send_final_text(&transcript) {
            Ok(outcome) => outcome,
            Err(error) => DeliveryOutcome::Failed {
                reason: error.to_string(),
            },
        };
        if let Err(error) = self
            .history
            .update_delivery(&id, outcome.status(), outcome.backend())
        {
            warn!(error = %error, "could not update delivery result in history");
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
        &self,
        _code: &str,
        message: impl Into<String>,
        transcription: TranscriptionStatus,
        delivery: DeliveryStatus,
    ) -> IpcResponse {
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

pub async fn run_daemon(paths: AppPaths, config: Config) -> Result<()> {
    paths.create_user_dirs()?;
    let listener = bind_socket(&paths.socket_path)?;
    let mut daemon = default_daemon(config, &paths);
    daemon.initialize();
    info!(socket = %paths.socket_path.display(), "nvstt daemon started");

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    let result: Result<()> = loop {
        tokio::select! {
            shutdown_result = &mut shutdown => {
                if shutdown_result.is_ok() {
                    info!("shutdown requested");
                }
                break shutdown_result;
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(stream) => stream,
                    Err(error) => break Err(error.into()),
                };
                let (reader, writer) = stream.into_split();
                let response = match read_request(reader).await {
                    Ok(request) => daemon.handle(request),
                    Err(error) => error_response(&error),
                };
                if let Err(error) = write_response(writer, &response).await {
                    warn!(error = %error, "could not write IPC response");
                }
            }
        }
    };

    let cleanup = remove_socket(&paths.socket_path);
    result?;
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

fn default_daemon(config: Config, paths: &AppPaths) -> Daemon {
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
        Box::new(DesktopNotifier),
        RecordingStore::new(paths.state_dir.join("recordings")),
    );
    daemon.set_model_status(&model_status);
    daemon
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tempfile::{TempDir, tempdir};

    use super::*;
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
            let mut session = TestCapture::new();
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
            assert_eq!(
                metadata.capture.dropped_samples > 0,
                matches!(
                    fault,
                    CaptureFault::Queue | CaptureFault::Both | CaptureFault::Duration
                )
            );
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
}
