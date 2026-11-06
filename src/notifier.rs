use crate::error::{AppError, Result};
use std::thread;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationEvent {
    Initialized,
    InitializationFailed(String),
    StartFailed(String),
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
            Self::StartFailed(reason) => ("Could not start dictation", reason.clone()),
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

/// Keep routine phase notifications when no tray host is reachable.
pub struct TrayAwareNotifier<N> {
    inner: N,
    host: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<N: Notifier> TrayAwareNotifier<N> {
    pub fn new(inner: N, host: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self { inner, host }
    }
}

impl<N: Notifier> Notifier for TrayAwareNotifier<N> {
    fn notify(&mut self, event: NotificationEvent) -> Result<()> {
        let routine = matches!(
            event,
            NotificationEvent::Initialized
                | NotificationEvent::ListeningStarted
                | NotificationEvent::Finalizing
                | NotificationEvent::Transcribed
                | NotificationEvent::Delivered(_)
                | NotificationEvent::Canceled
                | NotificationEvent::NoSpeechDetected
        );
        if !routine || !self.host.load(std::sync::atomic::Ordering::Acquire) {
            self.inner.notify(event)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn host_suppresses_phases_but_keeps_failures_and_clipboard_feedback() {
        let host = Arc::new(AtomicBool::new(true));
        let mut notifier = TrayAwareNotifier::new(NoopNotifier::default(), host.clone());
        for event in [
            NotificationEvent::ListeningStarted,
            NotificationEvent::Finalizing,
            NotificationEvent::Delivered("test".to_owned()),
            NotificationEvent::Canceled,
        ] {
            notifier.notify(event).unwrap();
        }
        assert!(notifier.inner.events.is_empty());
        for event in [
            NotificationEvent::StartFailed("input".to_owned()),
            NotificationEvent::InitializationFailed("model".to_owned()),
            NotificationEvent::TranscriptionFailed("capture".to_owned()),
            NotificationEvent::DeliveryFailed("delivery".to_owned()),
            NotificationEvent::CopiedToClipboard,
        ] {
            notifier.notify(event).unwrap();
        }
        assert_eq!(notifier.inner.events.len(), 5);
        host.store(false, Ordering::Release);
        notifier
            .notify(NotificationEvent::ListeningStarted)
            .unwrap();
        assert_eq!(notifier.inner.events.len(), 6);
    }
}
