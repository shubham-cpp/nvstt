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
        TranscriptionStatus, new_session_id,
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
    recorder::{AudioSource, CpalRecorder, NoopRecorder, Recorder},
};

struct RecognitionWorker {
    command_tx: Sender<WorkerCommand>,
    completion_rx: Receiver<WorkerCompletion>,
    join: Option<thread::JoinHandle<()>>,
}

enum WorkerCommand {
    Finish,
    Cancel,
}

struct WorkerCompletion {
    recognizer: Box<dyn StreamingRecognizer>,
    outcome: Option<Result<RecognitionOutcome>>,
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
        })
    }

    fn finish(mut self) -> Result<(Box<dyn StreamingRecognizer>, Result<RecognitionOutcome>)> {
        self.command_tx.send(WorkerCommand::Finish).map_err(|_| {
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
        let outcome = completion.outcome.unwrap_or_else(|| {
            Err(AppError::Unavailable(
                "recognition worker canceled".to_owned(),
            ))
        });
        Ok((completion.recognizer, outcome))
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
    let outcome = loop {
        feed_audio_if_healthy(
            &mut source,
            recognizer.as_mut(),
            &mut audio_pipeline,
            &mut worker_error,
        );

        match command_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(WorkerCommand::Finish) => {
                finish_audio_if_healthy(
                    &mut source,
                    recognizer.as_mut(),
                    &mut audio_pipeline,
                    &mut worker_error,
                );
                break Some(match worker_error {
                    Some(error) => Err(error),
                    None => recognizer.finish_session(),
                });
            }
            Ok(WorkerCommand::Cancel) => {
                let _ = recognizer.cancel_session();
                break None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let error =
                    AppError::Unavailable("recognition worker command channel closed".to_owned());
                break Some(Err(error));
            }
        }
    };

    let _ = completion_tx.send(WorkerCompletion {
        recognizer,
        outcome,
    });
}

fn feed_audio_if_healthy(
    source: &mut AudioSource,
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
    worker_error: &mut Option<AppError>,
) {
    if worker_error.is_none() {
        *worker_error = feed_available_audio(source, recognizer, audio_pipeline).err();
    }
}

fn finish_audio_if_healthy(
    source: &mut AudioSource,
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
    worker_error: &mut Option<AppError>,
) {
    if worker_error.is_none() {
        *worker_error = finish_audio(source, recognizer, audio_pipeline).err();
    }
}

fn feed_available_audio(
    source: &mut AudioSource,
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
) -> Result<()> {
    let samples = source.drain()?;
    if samples.is_empty() {
        return Ok(());
    }
    feed_recognizer(recognizer, audio_pipeline, source.sample_rate(), &samples)
}

fn drain_audio(
    source: &mut AudioSource,
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
) -> Result<()> {
    loop {
        let samples = source.drain()?;
        if samples.is_empty() {
            return Ok(());
        }
        feed_recognizer(recognizer, audio_pipeline, source.sample_rate(), &samples)?;
    }
}

fn finish_audio(
    source: &mut AudioSource,
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
) -> Result<()> {
    drain_audio(source, recognizer, audio_pipeline)?;
    let samples = audio_pipeline.finish()?;
    if !samples.is_empty() {
        recognizer.accept_audio(MODEL_SAMPLE_RATE, &samples)?;
    }
    source.integrity_result()?;
    Ok(())
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
            session_started: None,
        }
    }

    /// Attach the read-only model readiness metadata exposed through status.
    pub fn set_model_status(&mut self, model: &ModelStatus) {
        self.status.model_ready = model.ready;
        self.status.model_path = Some(model.path.display().to_string());
    }

    pub fn initialize(&mut self) {
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

        if let Err(error) = self.recorder.stop() {
            return self.transcription_failure(error.to_string());
        }

        let recognition = match self.finish_recognition_worker() {
            Ok(outcome) => outcome,
            Err(error) => return self.transcription_failure(error.to_string()),
        };

        let transcript =
            match dictation_transcript(recognition, &self.config.replacements, self.config.itn) {
                Ok(DictationTranscript::NoContent) => return self.no_speech(),
                Ok(DictationTranscript::Ready(transcript)) => transcript,
                Err(EmptyTranscript) => {
                    return self.transcription_failure("transcript was empty".to_owned());
                }
            };

        self.deliver_transcript(transcript)
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

    fn finish_recognition_worker(&mut self) -> Result<RecognitionOutcome> {
        let worker = self.worker.take().ok_or_else(|| {
            AppError::Unavailable("recognition worker is not available".to_owned())
        })?;
        let (recognizer, outcome) = worker.finish()?;
        self.recognizer = Some(recognizer);
        outcome
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
    );
    daemon.set_model_status(&model_status);
    daemon
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::{
        delivery::StaticSink, dictation_transcript::Replacements, history::JsonHistoryStore,
        notifier::NoopNotifier,
    };

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

    fn test_daemon(outcome: DeliveryOutcome) -> Daemon {
        let directory = tempdir().expect("temp directory");
        let history = Box::new(JsonHistoryStore::new(directory.path().join("history.json")));
        // Leak the temporary directory for the duration of this unit test so
        // the history path remains valid while the daemon owns it.
        std::mem::forget(directory);
        Daemon::new(
            Config::default(),
            Box::new(NoopRecorder::default()),
            Box::new(StaticRecognizer::new("final transcript")),
            Box::new(StaticSink::new(outcome)),
            history,
            Box::new(NoopNotifier::default()),
        )
    }

    #[test]
    fn final_only_delivery_keeps_transcript_in_history() {
        let mut daemon = test_daemon(DeliveryOutcome::Delivered {
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
        let mut daemon = test_daemon(DeliveryOutcome::Failed {
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
        let mut daemon = test_daemon(DeliveryOutcome::Delivered {
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
    fn worker_drains_the_last_audio_before_final_flush() {
        let samples = vec![0.25; 777];
        let source = AudioSource::test_source(16_000, samples.clone());
        let mut recognizer = CountingRecognizer::default();
        recognizer.start_session().expect("start recognizer");
        let worker =
            RecognitionWorker::spawn(Box::new(recognizer), source, false).expect("spawn worker");
        let (_, outcome) = worker.finish().expect("finish worker");
        assert_eq!(
            outcome.expect("recognition outcome"),
            RecognitionOutcome::Transcript(samples.len().to_string())
        );
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
