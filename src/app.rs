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
    config::Config,
    delivery::{NativeFirstSink, TextSink},
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
        ParakeetRecognizer, StaticRecognizer, StreamingRecognizer, UnavailableRecognizer,
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
    transcript: Option<Result<String>>,
}

impl RecognitionWorker {
    fn spawn(recognizer: Box<dyn StreamingRecognizer>, source: AudioSource) -> Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let (completion_tx, completion_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("nvstt-recognizer".to_owned())
            .spawn(move || run_recognition_worker(recognizer, source, command_rx, completion_tx))
            .map_err(|error| {
                AppError::Unavailable(format!("could not start recognition worker: {error}"))
            })?;
        Ok(Self {
            command_tx,
            completion_rx,
            join: Some(join),
        })
    }

    fn finish(mut self) -> Result<(Box<dyn StreamingRecognizer>, Result<String>)> {
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
        let transcript = completion.transcript.unwrap_or_else(|| {
            Err(AppError::Unavailable(
                "recognition worker canceled".to_owned(),
            ))
        });
        Ok((completion.recognizer, transcript))
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
    source: AudioSource,
    command_rx: Receiver<WorkerCommand>,
    completion_tx: Sender<WorkerCompletion>,
) {
    let mut worker_error: Option<AppError> = None;
    let transcript = loop {
        if worker_error.is_none() {
            match source.drain() {
                Ok(samples) if !samples.is_empty() => {
                    if let Err(error) = recognizer.accept_audio(source.sample_rate(), &samples) {
                        worker_error = Some(error);
                    }
                }
                Ok(_) => {}
                Err(error) => worker_error = Some(error),
            }
        }

        match command_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(WorkerCommand::Finish) => {
                if worker_error.is_none() {
                    loop {
                        match source.drain() {
                            Ok(samples) if !samples.is_empty() => {
                                if let Err(error) =
                                    recognizer.accept_audio(source.sample_rate(), &samples)
                                {
                                    worker_error = Some(error);
                                    break;
                                }
                            }
                            Ok(_) => break,
                            Err(error) => {
                                worker_error = Some(error);
                                break;
                            }
                        }
                    }
                }
                if worker_error.is_none() {
                    match source.overflowed() {
                        Ok(true) => {
                            worker_error = Some(AppError::Unavailable(
                                "audio capture exceeded the 30 minute limit".to_owned(),
                            ));
                        }
                        Ok(false) => {}
                        Err(error) => worker_error = Some(error),
                    }
                }
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
        transcript,
    });
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

        if let Some(source) = self.recorder.audio_source() {
            match RecognitionWorker::spawn(recognizer, source) {
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
        } else {
            self.recognizer = Some(recognizer);
        }

        let session_id = new_session_id();
        self.session_started = Some(Instant::now());
        let model_ready = self.status.model_ready;
        let model_path = self.status.model_path.clone();
        self.status = StatusSnapshot {
            state: DaemonState::Listening,
            model: self.config.model.clone(),
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

        let captured_audio = match self.recorder.stop() {
            Ok(audio) => audio,
            Err(error) => {
                return self.transcription_failure(error.to_string());
            }
        };

        let transcript = if let Some(worker) = self.worker.take() {
            match worker.finish() {
                Ok((recognizer, Ok(transcript))) => {
                    self.recognizer = Some(recognizer);
                    transcript
                }
                Ok((recognizer, Err(error))) => {
                    self.recognizer = Some(recognizer);
                    return self.transcription_failure(error.to_string());
                }
                Err(error) => return self.transcription_failure(error.to_string()),
            }
        } else {
            let Some(recognizer) = self.recognizer.as_mut() else {
                return self.transcription_failure("recognizer is not available".to_owned());
            };
            if let Some(audio) = captured_audio {
                let chunk_size = (audio.sample_rate.max(1) as usize / 10).max(1);
                for chunk in audio.samples.chunks(chunk_size) {
                    if let Err(error) = recognizer.accept_audio(audio.sample_rate, chunk) {
                        return self.transcription_failure(error.to_string());
                    }
                }
            }
            match recognizer.finish_session() {
                Ok(transcript) => transcript,
                Err(error) => return self.transcription_failure(error.to_string()),
            }
        };

        if transcript.trim().is_empty() {
            return self.transcription_failure("transcript was empty".to_owned());
        }
        let transcript = transcript.trim().to_owned();

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
                match ParakeetRecognizer::from_model_dir(model_dir) {
                    Ok(recognizer) => (Box::new(CpalRecorder::default()), Box::new(recognizer)),
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
    use crate::{delivery::StaticSink, history::JsonHistoryStore, notifier::NoopNotifier};

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
}
