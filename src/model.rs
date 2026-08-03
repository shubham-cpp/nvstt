//! Local model installation and readiness checks.
//!
//! Model files are deliberately inspected without loading the native runtime.
//! This keeps `nvstt model status` useful when the model is not installed and
//! avoids a potentially expensive ONNX load for a read-only CLI command.

use std::path::PathBuf;

use serde::Serialize;

use crate::{config::Config, paths::AppPaths};

#[derive(Clone, Debug, Serialize)]
pub struct ModelFileStatus {
    pub name: String,
    pub path: PathBuf,
    pub present: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SpeechGateStatus {
    pub enabled: bool,
    pub path: PathBuf,
    pub ready: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelStatus {
    pub model: String,
    pub streaming_profile: String,
    pub artifact: String,
    pub path: PathBuf,
    pub ready: bool,
    pub files: Vec<ModelFileStatus>,
    pub speech_gate: SpeechGateStatus,
}

impl ModelStatus {
    pub fn inspect(config: &Config, paths: &AppPaths) -> Self {
        let path = paths.model_dir.join(config.artifact_name());
        let files = config
            .required_model_files()
            .iter()
            .map(|file| {
                let selected = file
                    .alternatives
                    .iter()
                    .map(|file| path.join(file))
                    .find(|candidate| candidate.is_file())
                    .unwrap_or_else(|| path.join(file.alternatives[0]));
                ModelFileStatus {
                    name: file.name.to_owned(),
                    present: selected.is_file(),
                    path: selected,
                }
            })
            .collect::<Vec<_>>();
        let vad_path = path.join("silero_vad.onnx");
        let speech_gate = SpeechGateStatus {
            enabled: config.speech_gate,
            ready: !config.speech_gate || vad_path.is_file(),
            path: vad_path,
        };

        Self {
            model: config.model.clone(),
            streaming_profile: config.streaming_profile.clone(),
            artifact: config.artifact_name().to_owned(),
            path,
            ready: files.iter().all(|file| file.present) && speech_gate.ready,
            files,
            speech_gate,
        }
    }

    pub fn missing_files(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|file| !file.present)
            .map(|file| file.name.clone())
            .collect()
    }

    pub fn message(&self) -> String {
        if self.ready {
            return format!("model ready at {}", self.path.display());
        }

        let mut missing = self.missing_files();
        if !self.speech_gate.ready {
            missing.push("silero VAD".to_owned());
        }
        format!(
            "model is not installed at {}; missing {}",
            self.path.display(),
            missing.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;

    fn paths(root: &Path) -> AppPaths {
        AppPaths {
            config_path: root.join("config/config.toml"),
            state_dir: root.join("state"),
            history_path: root.join("state/history.json"),
            runtime_dir: root.join("runtime"),
            socket_path: root.join("runtime/nvstt.sock"),
            model_dir: root.join("models"),
        }
    }

    #[test]
    fn reports_missing_files() {
        let directory = tempdir().expect("temporary directory");
        let status = ModelStatus::inspect(&Config::default(), &paths(directory.path()));
        assert!(!status.ready);
        assert_eq!(status.missing_files().len(), 4);
        assert!(!status.speech_gate.ready);
    }

    #[test]
    fn accepts_the_int8_artifact_file_set() {
        let directory = tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        let model_path = paths.model_dir.join(Config::default().artifact_name());
        std::fs::create_dir_all(&model_path).expect("model directory");
        for file in [
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "joiner.int8.onnx",
            "tokens.txt",
        ] {
            std::fs::write(model_path.join(file), b"test").expect("model file");
        }
        std::fs::write(model_path.join("silero_vad.onnx"), b"test").expect("VAD file");

        let status = ModelStatus::inspect(&Config::default(), &paths);
        assert!(status.ready);
        assert!(status.missing_files().is_empty());
        assert!(status.speech_gate.ready);
    }

    #[test]
    fn a_legacy_parakeet_model_is_ready_without_vad() {
        let directory = tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        let config = Config::for_model(crate::config::PARAKEET_UNIFIED_MODEL, "1120ms")
            .expect("valid Parakeet config");
        let model_path = paths.model_dir.join(config.artifact_name());
        std::fs::create_dir_all(&model_path).expect("model directory");
        for file in [
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "joiner.int8.onnx",
            "tokens.txt",
        ] {
            std::fs::write(model_path.join(file), b"test").expect("model file");
        }

        let status = ModelStatus::inspect(&config, &paths);
        assert!(status.ready);
        assert!(!status.speech_gate.enabled);
        assert!(status.speech_gate.ready);
    }
}
