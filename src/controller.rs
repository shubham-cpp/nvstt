//! A single thread owns capture, recognition, history, and delivery.
//! Admission reserves an operation before it can wait in the command channel.
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use crate::{
    app::Daemon,
    domain::{DaemonState, StatusSnapshot},
    error::{AppError, Result},
    ipc::{IpcRequest, IpcResponse},
};
use tokio::sync::{oneshot, watch};

#[derive(Clone)]
pub(crate) struct Controller {
    commands: mpsc::SyncSender<Operation>,
    snapshots: watch::Receiver<StatusSnapshot>,
    busy: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

struct Reservation(Arc<AtomicBool>);
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

enum Operation {
    Request {
        request: IpcRequest,
        reply: oneshot::Sender<IpcResponse>,
        _reservation: Reservation,
    },
    Shutdown,
}

#[derive(Debug)]
pub(crate) struct Rejection {
    code: &'static str,
    message: &'static str,
}
impl Rejection {
    fn response(self) -> IpcResponse {
        IpcResponse::Error {
            code: self.code.to_owned(),
            message: self.message.to_owned(),
        }
    }
}
fn rejection(code: &'static str, message: &'static str) -> Rejection {
    Rejection { code, message }
}

impl Controller {
    pub fn snapshots(&self) -> watch::Receiver<StatusSnapshot> {
        self.snapshots.clone()
    }
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    pub fn snapshot(&self) -> StatusSnapshot {
        self.snapshots.borrow().clone()
    }

    pub fn submit(
        &self,
        request: IpcRequest,
        generation: Option<u64>,
    ) -> std::result::Result<oneshot::Receiver<IpcResponse>, Rejection> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(rejection("unavailable", "Daemon is shutting down"));
        }
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(rejection("busy", "Daemon is busy; try again"));
        }
        let reservation = Reservation(self.busy.clone());
        let snapshot = self.snapshot();
        if generation.is_some_and(|value| value != snapshot.generation) {
            return Err(rejection("stale", "Menu changed; open it again"));
        }
        if matches!(
            request,
            IpcRequest::AudioInputs | IpcRequest::SelectInput { .. } | IpcRequest::ClearError
        ) && snapshot.state != DaemonState::Idle
        {
            return Err(rejection(
                "busy",
                "Change inputs or clear errors between dictations",
            ));
        }
        if request_is_cancel(&request) && snapshot.state != DaemonState::Listening {
            return Err(rejection(
                "invalid_state",
                "No recording is available to cancel",
            ));
        }
        let (reply, receive) = oneshot::channel();
        self.commands
            .try_send(Operation::Request {
                request,
                reply,
                _reservation: reservation,
            })
            .map_err(|_| rejection("unavailable", "Controller is unavailable"))?;
        Ok(receive)
    }

    pub async fn request(&self, request: IpcRequest, generation: Option<u64>) -> IpcResponse {
        if matches!(request, IpcRequest::Status) {
            return IpcResponse::Status {
                snapshot: self.snapshot(),
            };
        }
        match self.submit(request, generation) {
            Ok(reply) => reply
                .await
                .unwrap_or_else(|_| rejection("unavailable", "Controller stopped").response()),
            Err(error) => error.response(),
        }
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.commands.try_send(Operation::Shutdown);
    }
}

fn request_is_cancel(request: &IpcRequest) -> bool {
    matches!(request, IpcRequest::Cancel)
}

pub(crate) fn start(
    starting: StatusSnapshot,
    factory: impl FnOnce() -> Daemon + Send + 'static,
) -> Result<(Controller, thread::JoinHandle<()>)> {
    let (publisher, snapshots) = watch::channel(starting);
    let (commands, receive) = mpsc::sync_channel(2);
    let busy = Arc::new(AtomicBool::new(true));
    let shutdown = Arc::new(AtomicBool::new(false));
    let handle = Controller {
        commands,
        snapshots,
        busy: busy.clone(),
        shutdown: shutdown.clone(),
    };
    let join = thread::Builder::new()
        .name("nvstt-controller".to_owned())
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| {
                let mut daemon = factory();
                daemon.attach_runtime(publisher.clone(), shutdown.clone());
                daemon.initialize();
                busy.store(false, Ordering::Release);
                while let Ok(operation) = receive.recv() {
                    if shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    match operation {
                        Operation::Request {
                            request,
                            reply,
                            _reservation,
                        } => {
                            let response = daemon.handle(request);
                            drop(_reservation);
                            let _ = reply.send(response);
                        }
                        Operation::Shutdown => break,
                    }
                }
                daemon.shutdown();
            }));
            shutdown.store(true, Ordering::Release);
            busy.store(false, Ordering::Release);
            if result.is_err() {
                let mut status = publisher.borrow().clone();
                status.state = DaemonState::Error;
                status.message = "controller stopped unexpectedly".to_owned();
                status.audio.active = None;
                status.feedback.initialization_failed = true;
                status.revision = status.revision.saturating_add(1);
                publisher.send_replace(status);
                tracing::error!("controller stopped unexpectedly");
            }
        })
        .map_err(|error| AppError::Unavailable(format!("could not start controller: {error}")))?;
    Ok((handle, join))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        delivery::{StaticSink, TextSink},
        domain::{AttemptOutcome, DeliveryOutcome, FailureStage, InputPreference},
        history::JsonHistoryStore,
        notifier::NoopNotifier,
        recognizer::{RecognitionOutcome, StaticRecognizer, StreamingRecognizer},
        recorder::NoopRecorder,
        recordings::RecordingStore,
    };
    use tempfile::{TempDir, tempdir};

    struct GatedRecognizer {
        inner: StaticRecognizer,
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        release: mpsc::Receiver<()>,
    }
    impl StreamingRecognizer for GatedRecognizer {
        fn start_session(&mut self) -> Result<()> {
            self.inner.start_session()
        }
        fn accept_audio(&mut self, rate: i32, samples: &[f32]) -> Result<()> {
            self.inner.accept_audio(rate, samples)
        }
        fn finish_session(&mut self) -> Result<RecognitionOutcome> {
            self.entered.send(()).unwrap();
            self.release
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            self.inner.finish_session()
        }
        fn cancel_session(&mut self) -> Result<()> {
            self.inner.cancel_session()
        }
    }

    struct GatedSink {
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        release: mpsc::Receiver<()>,
    }
    impl TextSink for GatedSink {
        fn send_final_text(&mut self, _text: &str) -> Result<DeliveryOutcome> {
            self.entered.send(()).unwrap();
            self.release
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            Ok(DeliveryOutcome::Delivered {
                backend: "test".to_owned(),
            })
        }
    }

    fn daemon(
        directory: &TempDir,
        recognizer: Box<dyn StreamingRecognizer>,
        sink: Box<dyn TextSink>,
    ) -> Daemon {
        Daemon::new(
            Config::default(),
            Box::new(NoopRecorder::default()),
            recognizer,
            sink,
            Box::new(JsonHistoryStore::new(directory.path().join("history.json"))),
            Box::new(NoopNotifier::default()),
            RecordingStore::new(directory.path().join("recordings")),
        )
    }

    async fn ready(handle: &Controller) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while handle.snapshot().state == DaemonState::Starting || handle.is_busy() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    fn assert_busy(response: IpcResponse) {
        assert!(matches!(response, IpcResponse::Error { code, .. } if code == "busy"));
    }

    #[tokio::test]
    async fn status_remains_responsive_through_finalization_and_delivery() {
        let directory = tempdir().unwrap();
        let (final_entered, mut final_wait) = tokio::sync::mpsc::unbounded_channel();
        let (final_release, final_gate) = mpsc::channel();
        let (delivery_entered, mut delivery_wait) = tokio::sync::mpsc::unbounded_channel();
        let (delivery_release, delivery_gate) = mpsc::channel();
        let daemon = daemon(
            &directory,
            Box::new(GatedRecognizer {
                inner: StaticRecognizer::new("final transcript"),
                entered: final_entered,
                release: final_gate,
            }),
            Box::new(GatedSink {
                entered: delivery_entered,
                release: delivery_gate,
            }),
        );
        let (handle, join) =
            start(StatusSnapshot::starting(&Config::default()), move || daemon).unwrap();
        ready(&handle).await;
        assert!(handle.request(IpcRequest::Toggle, None).await.is_ok());
        let old_generation = handle.snapshot().generation;
        let finish = handle.submit(IpcRequest::Toggle, None).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), final_wait.recv())
            .await
            .unwrap()
            .unwrap();
        for (state, release, wait) in [
            (DaemonState::Finalizing, &final_release, &mut delivery_wait),
            (DaemonState::Delivering, &delivery_release, &mut final_wait),
        ] {
            let status = tokio::time::timeout(
                std::time::Duration::from_millis(100),
                handle.request(IpcRequest::Status, None),
            )
            .await
            .unwrap();
            assert!(matches!(status, IpcResponse::Status { snapshot } if snapshot.state == state));
            for request in [
                IpcRequest::Toggle,
                IpcRequest::Cancel,
                IpcRequest::AudioInputs,
                IpcRequest::SelectInput {
                    preference: InputPreference::SystemDefault,
                },
            ] {
                assert_busy(handle.request(request, None).await);
            }
            release.send(()).unwrap();
            if state == DaemonState::Finalizing {
                tokio::time::timeout(std::time::Duration::from_secs(2), wait.recv())
                    .await
                    .unwrap()
                    .unwrap();
            }
        }
        assert!(finish.await.unwrap().is_ok());
        assert!(!handle.is_busy());
        assert!(
            matches!(handle.request(IpcRequest::SelectInput { preference:InputPreference::SystemDefault },Some(old_generation)).await,
            IpcResponse::Error { code,.. } if code == "stale")
        );
        // Receiving completion must allow an immediate follow-up.
        for _ in 0..100 {
            assert!(
                handle
                    .request(
                        IpcRequest::SelectInput {
                            preference: InputPreference::SystemDefault
                        },
                        None
                    )
                    .await
                    .is_ok()
            );
        }
        assert!(matches!(
            handle.snapshot().feedback.outcome,
            Some(AttemptOutcome::Delivered)
        ));
        handle.shutdown();
        join.join().unwrap();
    }

    #[tokio::test]
    async fn shutdown_during_finalization_prevents_delivery() {
        let directory = tempdir().unwrap();
        let (entered, mut wait) = tokio::sync::mpsc::unbounded_channel();
        let (release, gate) = mpsc::channel();
        let daemon = daemon(
            &directory,
            Box::new(GatedRecognizer {
                inner: StaticRecognizer::new("final transcript"),
                entered,
                release: gate,
            }),
            Box::new(StaticSink::new(DeliveryOutcome::Delivered {
                backend: "must not run".to_owned(),
            })),
        );
        let (handle, join) =
            start(StatusSnapshot::starting(&Config::default()), move || daemon).unwrap();
        ready(&handle).await;
        assert!(handle.request(IpcRequest::Toggle, None).await.is_ok());
        let finish = handle.submit(IpcRequest::Toggle, None).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), wait.recv())
            .await
            .unwrap()
            .unwrap();
        handle.shutdown();
        release.send(()).unwrap();
        assert!(!finish.await.unwrap().is_ok());
        join.join().unwrap();
        assert!(matches!(
            handle.snapshot().feedback.outcome,
            Some(AttemptOutcome::Failed {
                stage: FailureStage::Delivery,
                ..
            })
        ));
        assert!(
            matches!(handle.request(IpcRequest::Toggle,None).await, IpcResponse::Error { code,.. } if code=="unavailable")
        );
    }

    #[tokio::test]
    async fn controller_panic_keeps_status_and_rejects_mutations() {
        let (handle, join) = start(StatusSnapshot::starting(&Config::default()), || {
            panic!("test factory")
        })
        .unwrap();
        join.join().unwrap();
        assert_eq!(handle.snapshot().state, DaemonState::Error);
        assert!(
            matches!(handle.request(IpcRequest::Toggle,None).await,IpcResponse::Error { code,.. } if code=="unavailable")
        );
        assert!(handle.request(IpcRequest::Status, None).await.is_ok());
    }
}
