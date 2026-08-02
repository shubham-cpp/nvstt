//! Local model installation and readiness checks.
//!
//! Model files are deliberately inspected without loading the native runtime.
//! This keeps `nvstt model status` useful when the model is not installed and
//! avoids a potentially expensive ONNX load for a read-only CLI command.

use std::path::PathBuf;

use serde::Serialize;

use crate::{config::Config, paths::AppPaths};

/// The files accepted by the sherpa-onnx Parakeet loader.
///
/// INT8 files are the supported release artifact. The unquantized names are
/// accepted because the recognizer also supports them for local experiments.
const MODEL_FILE_ALTERNATIVES: [(&str, [&str; 2]); 4] = [
    ("encoder", ["encoder.int8.onnx", "encoder.onnx"]),
    ("decoder", ["decoder.int8.onnx", "decoder.onnx"]),
    ("joiner", ["joiner.int8.onnx", "joiner.onnx"]),
    ("tokens", ["tokens.txt", "tokens.txt"]),
];

#[derive(Clone, Debug, Serialize)]
pub struct ModelFileStatus {
    pub name: String,
    pub path: PathBuf,
    pub present: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelStatus {
    pub model: String,
    pub artifact: String,
    pub path: PathBuf,
    pub ready: bool,
    pub files: Vec<ModelFileStatus>,
}

impl ModelStatus {
    pub fn inspect(config: &Config, paths: &AppPaths) -> Self {
        let path = paths.model_dir.join(config.artifact_name());
        let files = MODEL_FILE_ALTERNATIVES
            .iter()
            .map(|(name, alternatives)| {
                let selected = alternatives
                    .iter()
                    .map(|file| path.join(file))
                    .find(|candidate| candidate.is_file())
                    .unwrap_or_else(|| path.join(alternatives[0]));
                ModelFileStatus {
                    name: (*name).to_owned(),
                    present: selected.is_file(),
                    path: selected,
                }
            })
            .collect::<Vec<_>>();

        Self {
            model: config.model.clone(),
            artifact: config.artifact_name().to_owned(),
            path,
            ready: files.iter().all(|file| file.present),
            files,
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

        let missing = self.missing_files();
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

        let status = ModelStatus::inspect(&Config::default(), &paths);
        assert!(status.ready);
        assert!(status.missing_files().is_empty());
    }
}
