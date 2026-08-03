use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// The model selected for a new installation.
pub const DEFAULT_MODEL: &str = "nemotron-speech-streaming-en-0.6b";
pub const DEFAULT_STREAMING_PROFILE: &str = "560ms";
pub const PARAKEET_UNIFIED_MODEL: &str = "parakeet-unified-en-0.6b";
pub const NEMOTRON_STREAMING_MODEL: &str = "nemotron-speech-streaming-en-0.6b";

const MODEL_RELEASE_BASE_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";
const SILERO_VAD_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecognizerFamily {
    OnlineTransducer,
}

#[derive(Clone, Copy, Debug)]
pub struct StreamingProfileSpec {
    pub name: &'static str,
    pub artifact: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct RequiredModelFileSpec {
    pub name: &'static str,
    pub alternatives: &'static [&'static str],
}

pub const ONLINE_TRANSDUCER_REQUIRED_FILES: [RequiredModelFileSpec; 4] = [
    RequiredModelFileSpec {
        name: "encoder",
        alternatives: &["encoder.int8.onnx", "encoder.onnx"],
    },
    RequiredModelFileSpec {
        name: "decoder",
        alternatives: &["decoder.int8.onnx", "decoder.onnx"],
    },
    RequiredModelFileSpec {
        name: "joiner",
        alternatives: &["joiner.int8.onnx", "joiner.onnx"],
    },
    RequiredModelFileSpec {
        name: "tokens",
        alternatives: &["tokens.txt"],
    },
];

#[derive(Clone, Copy, Debug)]
pub struct ModelSpec {
    pub name: &'static str,
    pub family: RecognizerFamily,
    pub profiles: &'static [StreamingProfileSpec],
    pub archive_base_url: &'static str,
    pub required_files: &'static [RequiredModelFileSpec],
}

const PARAKEET_PROFILES: [StreamingProfileSpec; 3] = [
    StreamingProfileSpec {
        name: "240ms",
        artifact: "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-240ms",
    },
    StreamingProfileSpec {
        name: "560ms",
        artifact: "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms",
    },
    StreamingProfileSpec {
        name: "1120ms",
        artifact: "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-1120ms",
    },
];

const NEMOTRON_PROFILES: [StreamingProfileSpec; 4] = [
    StreamingProfileSpec {
        name: "80ms",
        artifact: "sherpa-onnx-nemotron-speech-streaming-en-0.6b-80ms-int8-2026-04-25",
    },
    StreamingProfileSpec {
        name: "160ms",
        artifact: "sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25",
    },
    StreamingProfileSpec {
        name: "560ms",
        artifact: "sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25",
    },
    StreamingProfileSpec {
        name: "1120ms",
        artifact: "sherpa-onnx-nemotron-speech-streaming-en-0.6b-1120ms-int8-2026-04-25",
    },
];

const MODEL_REGISTRY: [ModelSpec; 2] = [
    ModelSpec {
        name: PARAKEET_UNIFIED_MODEL,
        family: RecognizerFamily::OnlineTransducer,
        profiles: &PARAKEET_PROFILES,
        archive_base_url: MODEL_RELEASE_BASE_URL,
        required_files: &ONLINE_TRANSDUCER_REQUIRED_FILES,
    },
    ModelSpec {
        name: NEMOTRON_STREAMING_MODEL,
        family: RecognizerFamily::OnlineTransducer,
        profiles: &NEMOTRON_PROFILES,
        archive_base_url: MODEL_RELEASE_BASE_URL,
        required_files: &ONLINE_TRANSDUCER_REQUIRED_FILES,
    },
];

/// The closed list of supported native recognition models.
pub fn model_registry() -> &'static [ModelSpec] {
    &MODEL_REGISTRY
}

pub fn model_spec(name: &str) -> Option<&'static ModelSpec> {
    model_registry().iter().find(|spec| spec.name == name)
}

fn default_model() -> String {
    DEFAULT_MODEL.to_owned()
}

fn default_streaming_profile() -> String {
    DEFAULT_STREAMING_PROFILE.to_owned()
}

fn default_speech_gate() -> bool {
    true
}

fn default_speech_gate_for_model(model: &str) -> bool {
    model == NEMOTRON_STREAMING_MODEL
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_streaming_profile")]
    pub streaming_profile: String,
    #[serde(default = "default_speech_gate")]
    pub speech_gate: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
            streaming_profile: default_streaming_profile(),
            speech_gate: default_speech_gate(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(path)?;
        let value: toml::Value = toml::from_str(&contents)?;
        let model = match value.get("model") {
            None => default_model(),
            Some(toml::Value::String(model)) => model.clone(),
            Some(toml::Value::Table(table)) => table
                .get("name")
                .and_then(toml::Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    AppError::Config("[model] must contain a string `name` field".to_owned())
                })?,
            Some(_) => {
                return Err(AppError::Config(
                    "`model` must be a string or a [model] table".to_owned(),
                ));
            }
        };
        let streaming_profile = value
            .get("streaming_profile")
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| nested_string(&value, "streaming_profile"))
            .unwrap_or_else(default_streaming_profile);
        let speech_gate = value
            .get("speech_gate")
            .and_then(toml::Value::as_bool)
            .or_else(|| nested_bool(&value, "speech_gate"))
            // Preserve a pre-Nemotron Parakeet configuration exactly as it was.
            .unwrap_or_else(|| default_speech_gate_for_model(&model));
        let config = Self {
            model,
            streaming_profile,
            speech_gate,
        };
        config.validate()?;
        Ok(config)
    }

    /// Build a validated candidate configuration without writing user settings.
    pub fn for_model(
        model: impl Into<String>,
        streaming_profile: impl Into<String>,
    ) -> Result<Self> {
        let model = model.into();
        let config = Self {
            speech_gate: default_speech_gate_for_model(&model),
            model,
            streaming_profile: streaming_profile.into(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_model_overrides(
        &self,
        model: Option<String>,
        streaming_profile: Option<String>,
    ) -> Result<Self> {
        let model_changed = model.is_some();
        let model = model.unwrap_or_else(|| self.model.clone());
        let config = Self {
            speech_gate: if model_changed {
                default_speech_gate_for_model(&model)
            } else {
                self.speech_gate
            },
            model,
            streaming_profile: streaming_profile.unwrap_or_else(|| self.streaming_profile.clone()),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let Some(model) = self.model_spec() else {
            let names = model_registry()
                .iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(AppError::Config(format!(
                "unsupported model '{}'; expected one of: {names}",
                self.model
            )));
        };
        if self.profile_spec().is_none() {
            let profiles = model
                .profiles
                .iter()
                .map(|profile| profile.name)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(AppError::Config(format!(
                "unsupported streaming profile '{}' for '{}'; expected one of: {profiles}",
                self.streaming_profile, self.model
            )));
        }
        Ok(())
    }

    pub fn model_spec(&self) -> Option<&'static ModelSpec> {
        model_spec(&self.model)
    }

    pub fn recognizer_family(&self) -> Option<RecognizerFamily> {
        self.model_spec().map(|spec| spec.family)
    }

    pub fn artifact_name(&self) -> &'static str {
        self.profile_spec()
            .expect("Config must be validated before its artifact is used")
            .artifact
    }

    pub fn model_download_url(&self) -> String {
        let model = self
            .model_spec()
            .expect("Config must be validated before its archive is used");
        format!(
            "{}/{}.tar.bz2",
            model.archive_base_url,
            self.artifact_name()
        )
    }

    pub fn vad_download_url(&self) -> Option<&'static str> {
        self.speech_gate.then_some(SILERO_VAD_URL)
    }

    pub fn required_model_files(&self) -> &'static [RequiredModelFileSpec] {
        self.model_spec()
            .expect("Config must be validated before its model files are used")
            .required_files
    }

    fn profile_spec(&self) -> Option<&'static StreamingProfileSpec> {
        self.model_spec()?
            .profiles
            .iter()
            .find(|profile| profile.name == self.streaming_profile)
    }
}

fn nested_string(value: &toml::Value, field: &str) -> Option<String> {
    value
        .get("model")
        .and_then(toml::Value::as_table)
        .and_then(|model| model.get(field))
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned)
}

fn nested_bool(value: &toml::Value, field: &str) -> Option<bool> {
    value
        .get("model")
        .and_then(toml::Value::as_table)
        .and_then(|model| model.get(field))
        .and_then(toml::Value::as_bool)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn new_install_defaults_to_nemotron_with_the_speech_gate() {
        let config = Config::default();
        assert_eq!(config.model, NEMOTRON_STREAMING_MODEL);
        assert_eq!(config.streaming_profile, "560ms");
        assert!(config.speech_gate);
        assert_eq!(
            config.artifact_name(),
            "sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25"
        );
    }

    #[test]
    fn accepts_flat_legacy_parakeet_configuration_without_enabling_vad() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        fs::write(&path, "model = \"parakeet-unified-en-0.6b\"\n").expect("write config");
        let config = Config::load(&path).expect("load config");
        assert_eq!(config.model, PARAKEET_UNIFIED_MODEL);
        assert_eq!(config.streaming_profile, DEFAULT_STREAMING_PROFILE);
        assert!(!config.speech_gate);
    }

    #[test]
    fn accepts_legacy_nested_model_name_configuration() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "[model]\nname = \"parakeet-unified-en-0.6b\"\nstreaming_profile = \"1120ms\"\nspeech_gate = true\n",
        )
        .expect("write config");
        let config = Config::load(&path).expect("load config");
        assert_eq!(config.model, PARAKEET_UNIFIED_MODEL);
        assert_eq!(config.streaming_profile, "1120ms");
        assert!(config.speech_gate);
    }

    #[test]
    fn validates_profiles_per_model() {
        let config = Config::for_model(PARAKEET_UNIFIED_MODEL, "80ms")
            .expect_err("Parakeet has no 80 ms profile");
        assert!(config.to_string().contains("unsupported streaming profile"));

        let config = Config::for_model(NEMOTRON_STREAMING_MODEL, "560ms")
            .expect("Nemotron has the pinned 560 ms profile");
        assert_eq!(
            config.recognizer_family(),
            Some(RecognizerFamily::OnlineTransducer)
        );
    }

    #[test]
    fn model_override_uses_the_candidate_default_gate() {
        let legacy =
            Config::for_model(PARAKEET_UNIFIED_MODEL, "1120ms").expect("valid legacy model");
        assert!(!legacy.speech_gate);
        let candidate = legacy
            .with_model_overrides(
                Some(NEMOTRON_STREAMING_MODEL.to_owned()),
                Some("560ms".to_owned()),
            )
            .expect("valid candidate");
        assert!(candidate.speech_gate);
    }
}
