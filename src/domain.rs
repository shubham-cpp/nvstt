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
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub audio: AudioStatus,
    #[serde(default)]
    pub feedback: Feedback,
}

impl StatusSnapshot {
    pub fn starting(config: &crate::config::Config) -> Self {
        Self {
            state: DaemonState::Starting,
            model: config.model.clone(),
            streaming_profile: config.streaming_profile.clone(),
            speech_gate_enabled: config.speech_gate,
            execution_provider: crate::recognizer::execution_provider().to_owned(),
            model_ready: false,
            model_path: None,
            message: "starting".to_owned(),
            session_id: None,
            revision: 0,
            generation: 0,
            audio: AudioStatus::default(),
            feedback: Feedback::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum InputPreference {
    #[default]
    SystemDefault,
    Device {
        id: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Source,
    Monitor,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AudioInput {
    pub id: String,
    pub name: String,
    pub kind: InputKind,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AudioStatus {
    pub selected: InputPreference,
    pub active: Option<AudioInput>,
    pub default_input: Option<AudioInput>,
    pub inputs: Vec<AudioInput>,
    pub available: bool,
    pub stale: bool,
}

impl AudioStatus {
    pub fn selected_input(&self) -> Option<&AudioInput> {
        match &self.selected {
            InputPreference::SystemDefault => self.default_input.as_ref(),
            InputPreference::Device { id } => self.inputs.iter().find(|input| &input.id == id),
        }
    }

    pub fn label(&self) -> String {
        if let Some(active) = &self.active {
            return active.name.clone();
        }
        let unavailable = if self.stale || !self.available {
            " (unavailable; refresh inputs)"
        } else {
            ""
        };
        match &self.selected {
            InputPreference::SystemDefault => self.default_input.as_ref().map_or_else(
                || "System default unavailable".to_owned(),
                |input| format!("System default: {}{unavailable}", input.name),
            ),
            InputPreference::Device { .. } => self.selected_input().map_or_else(
                || "Selected input unavailable".to_owned(),
                |input| format!("{}{unavailable}", input.name),
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    Start,
    Transcription,
    Delivery,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AttemptOutcome {
    Delivered,
    Copied,
    NoSpeech,
    Canceled,
    Failed { stage: FailureStage, reason: String },
}

impl AttemptOutcome {
    /// Deliberately excludes raw backend errors, which can contain private text.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Delivered => "Delivered",
            Self::Copied => "Copied; automatic delivery unavailable",
            Self::NoSpeech => "No speech",
            Self::Canceled => "Canceled",
            Self::Failed { stage, .. } => match stage {
                FailureStage::Start => "Could not start dictation; check nvstt status",
                FailureStage::Transcription => "Transcription failed; check nvstt status",
                FailureStage::Delivery => "Delivery failed; check nvstt status",
            },
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Feedback {
    pub outcome: Option<AttemptOutcome>,
    pub initialization_failed: bool,
    pub command_error: Option<String>,
    pub warning: Option<String>,
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
    fn older_status_payload_keeps_new_fields_optional() {
        let status: StatusSnapshot = serde_json::from_str(
            r#"{"state":"idle","model":"legacy","message":"ready","session_id":null}"#,
        )
        .unwrap();
        assert_eq!(status.state, DaemonState::Idle);
        assert_eq!(status.audio.selected, InputPreference::SystemDefault);
        assert_eq!(status.generation, 0);
        assert!(status.feedback.outcome.is_none());
    }

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
