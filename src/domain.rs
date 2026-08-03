use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    Starting,
    #[default]
    Idle,
    Listening,
    Finalizing,
    Delivering,
    Error,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StatusSnapshot {
    pub state: DaemonState,
    pub model: String,
    /// Streaming latency profile selected for the configured model.
    #[serde(default)]
    pub streaming_profile: String,
    /// Whether the local Silero speech gate is enabled for this model.
    #[serde(default)]
    pub speech_gate_enabled: bool,
    /// Native inference provider selected by this binary.
    #[serde(default)]
    pub execution_provider: String,
    /// Whether all files for the configured model are present locally.
    #[serde(default)]
    pub model_ready: bool,
    /// Resolved model directory. This is metadata only; it never contains
    /// transcript text or model contents.
    #[serde(default)]
    pub model_path: Option<String>,
    pub message: String,
    pub session_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionStatus {
    NotStarted,
    NoSpeech,
    Succeeded,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    NotAttempted,
    Delivered,
    CopiedToClipboard,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HistoryRecord {
    pub id: String,
    /// Unix epoch milliseconds. `u64` keeps the JSON API portable because
    /// serde_json intentionally does not encode Rust `u128` values.
    pub created_at_ms: u64,
    pub duration_ms: u64,
    pub model: String,
    pub transcript: String,
    pub transcription_status: TranscriptionStatus,
    pub delivery_status: DeliveryStatus,
    pub delivery_backend: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DeliveryOutcome {
    Delivered { backend: String },
    CopiedToClipboard { backend: String },
    Failed { reason: String },
}

impl DeliveryOutcome {
    pub fn status(&self) -> DeliveryStatus {
        match self {
            Self::Delivered { .. } => DeliveryStatus::Delivered,
            Self::CopiedToClipboard { .. } => DeliveryStatus::CopiedToClipboard,
            Self::Failed { .. } => DeliveryStatus::Failed,
        }
    }

    pub fn backend(&self) -> Option<String> {
        match self {
            Self::Delivered { backend } | Self::CopiedToClipboard { backend } => {
                Some(backend.clone())
            }
            Self::Failed { .. } => None,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Delivered { backend } => format!("delivered via {backend}"),
            Self::CopiedToClipboard { backend } => format!("copied via {backend}"),
            Self::Failed { reason } => reason.clone(),
        }
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub fn new_session_id() -> String {
    format!("{}-{}", now_ms(), NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

impl HistoryRecord {
    pub fn new(id: String, duration_ms: u64, model: String, transcript: String) -> Self {
        Self {
            id,
            created_at_ms: now_ms(),
            duration_ms,
            model,
            transcript,
            transcription_status: TranscriptionStatus::Succeeded,
            delivery_status: DeliveryStatus::NotAttempted,
            delivery_backend: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_record_has_json_safe_timestamp() {
        let record = HistoryRecord::new(
            "session".to_owned(),
            42,
            "parakeet-unified-en-0.6b".to_owned(),
            "hello".to_owned(),
        );
        let json = serde_json::to_string(&record).expect("history record should serialize");
        assert!(json.contains("created_at_ms"));
    }
}
