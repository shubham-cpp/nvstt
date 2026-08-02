use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::{
    domain::{DeliveryStatus, HistoryRecord, StatusSnapshot, TranscriptionStatus},
    error::{AppError, Result},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "command")]
pub enum IpcRequest {
    Toggle,
    Cancel,
    Status,
    History { limit: usize },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CommandResult {
    pub ok: bool,
    pub status: StatusSnapshot,
    pub transcript: Option<String>,
    pub transcription: TranscriptionStatus,
    pub delivery: DeliveryStatus,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IpcResponse {
    Command { result: CommandResult },
    Status { snapshot: StatusSnapshot },
    History { records: Vec<HistoryRecord> },
    Error { code: String, message: String },
}

impl IpcResponse {
    pub fn is_ok(&self) -> bool {
        match self {
            Self::Command { result } => result.ok,
            Self::Status { .. } | Self::History { .. } => true,
            Self::Error { .. } => false,
        }
    }
}

pub async fn send_request(socket_path: &Path, request: &IpcRequest) -> Result<IpcResponse> {
    let stream = UnixStream::connect(socket_path).await.map_err(|error| {
        AppError::Ipc(format!(
            "cannot connect to {}: {error}",
            socket_path.display()
        ))
    })?;
    let (reader, mut writer) = stream.into_split();
    let payload = serde_json::to_vec(request)?;
    writer.write_all(&payload).await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let bytes = reader.read_line(&mut line).await?;
    if bytes == 0 {
        return Err(AppError::Ipc(
            "daemon closed the connection without a response".to_owned(),
        ));
    }
    serde_json::from_str(line.trim()).map_err(AppError::from)
}

pub async fn read_request(reader: tokio::net::unix::OwnedReadHalf) -> Result<IpcRequest> {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let bytes = reader.read_line(&mut line).await?;
    if bytes == 0 {
        return Err(AppError::Ipc("client closed the connection".to_owned()));
    }
    serde_json::from_str(line.trim()).map_err(AppError::from)
}

pub async fn write_response(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    response: &IpcResponse,
) -> Result<()> {
    let payload = serde_json::to_vec(response)?;
    writer.write_all(&payload).await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;
    Ok(())
}

pub fn error_response(error: &AppError) -> IpcResponse {
    IpcResponse::Error {
        code: error_code(error).to_owned(),
        message: error.to_string(),
    }
}

fn error_code(error: &AppError) -> &'static str {
    match error {
        AppError::InvalidState(_) => "invalid_state",
        AppError::Unavailable(_) => "unavailable",
        AppError::Ipc(_) => "ipc_error",
        AppError::Config(_) => "config_error",
        AppError::History(_) => "history_error",
        AppError::Notification(_) => "notification_error",
        AppError::Io(_) | AppError::Json(_) | AppError::TomlParse(_) | AppError::TomlEncode(_) => {
            "internal_error"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::HistoryRecord;

    #[test]
    fn serializes_history_response_for_ipc() {
        let record = HistoryRecord::new(
            "id".to_owned(),
            12,
            "model".to_owned(),
            "transcript".to_owned(),
        );
        let response = IpcResponse::History {
            records: vec![record],
        };
        let payload =
            serde_json::to_vec(&response).expect("history response must be JSON serializable");
        serde_json::from_slice::<IpcResponse>(&payload)
            .expect("history response must be JSON deserializable");
    }
}
