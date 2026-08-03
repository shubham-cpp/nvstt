use crate::error::{AppError, Result};
use std::thread;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationEvent {
    Initialized,
    InitializationFailed(String),
    ListeningStarted,
    Finalizing,
    NoSpeechDetected,
    Transcribed,
    Delivered(String),
    CopiedToClipboard,
    TranscriptionFailed(String),
    DeliveryFailed(String),
    Canceled,
    Permission(String),
}

impl NotificationEvent {
    fn summary_and_body(&self) -> (&'static str, String) {
        match self {
            Self::Initialized => (
                "Ready",
                "nvstt daemon initialized; server started".to_owned(),
            ),
            Self::InitializationFailed(reason) => ("Initialization failed", reason.clone()),
            Self::ListeningStarted => ("Listening", "Dictation started".to_owned()),
            Self::Finalizing => ("Transcribing", "Finalizing dictation".to_owned()),
            Self::NoSpeechDetected => ("No speech", "Nothing was sent".to_owned()),
            Self::Transcribed => ("Transcribed", "Transcript ready".to_owned()),
            Self::Delivered(backend) => {
                ("Delivered", format!("Transcript delivered via {backend}"))
            }
            Self::CopiedToClipboard => (
                "Copied",
                "Automatic delivery was unavailable; transcript copied to clipboard".to_owned(),
            ),
            Self::TranscriptionFailed(reason) => ("Transcription failed", reason.clone()),
            Self::DeliveryFailed(reason) => ("Delivery failed", reason.clone()),
            Self::Canceled => ("Canceled", "Dictation canceled".to_owned()),
            Self::Permission(message) => ("Permission", message.clone()),
        }
    }
}

pub trait Notifier: Send {
    fn notify(&mut self, event: NotificationEvent) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn notify(&mut self, event: NotificationEvent) -> Result<()> {
        let (summary, body) = event.summary_and_body();
        // notify-rust's zbus backend uses a private synchronous Tokio runtime.
        // Calling it directly from our async daemon would try to start that
        // runtime from inside the daemon runtime and panic. Run the blocking
        // notification call on a short-lived thread when an async context is
        // active. The synchronous path keeps unit-test and library callers
        // simple.
        let show = move || {
            notify_rust::Notification::new()
                .summary(summary)
                .body(&body)
                .appname("nvstt")
                .show()
                .map(|_| ())
                .map_err(|error| error.to_string())
        };
        let result = if tokio::runtime::Handle::try_current().is_ok() {
            match thread::spawn(show).join() {
                Ok(result) => result,
                Err(_) => Err("notification thread panicked".to_owned()),
            }
        } else {
            show()
        };
        result.map_err(AppError::Notification)
    }
}

#[derive(Debug, Default)]
pub struct NoopNotifier {
    pub events: Vec<NotificationEvent>,
}

impl Notifier for NoopNotifier {
    fn notify(&mut self, event: NotificationEvent) -> Result<()> {
        self.events.push(event);
        Ok(())
    }
}
