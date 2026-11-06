use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    dictation_transcript::Replacements,
    error::{AppError, Result},
};

/// The model selected for a new installation.
pub const DEFAULT_MODEL: &str = PARAKEET_UNIFIED_MODEL;
pub const DEFAULT_STREAMING_PROFILE: &str = "1120ms";
pub const PARAKEET_UNIFIED_MODEL: &str = "parakeet-unified-en-0.6b";
pub const NEMOTRON_STREAMING_MODEL: &str = "nemotron-speech-streaming-en-0.6b";
pub const PARAKEET_TDT_MODEL: &str = "parakeet-tdt-0.6b-v3";

const MODEL_RELEASE_BASE_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";
const SILERO_VAD_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecognizerFamily {
    OnlineTransducer,
    OfflineTransducer,
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

const MODEL_REGISTRY: [ModelSpec; 3] = [
    ModelSpec {
        name: PARAKEET_TDT_MODEL,
        family: RecognizerFamily::OfflineTransducer,
        profiles: &[StreamingProfileSpec {
            name: "offline",
            artifact: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        }],
        archive_base_url: MODEL_RELEASE_BASE_URL,
        required_files: &ONLINE_TRANSDUCER_REQUIRED_FILES,
    },
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

fn profile_for_model(model: &str) -> String {
    if model == PARAKEET_TDT_MODEL {
        "offline".to_owned()
    } else if model == NEMOTRON_STREAMING_MODEL {
        "560ms".to_owned()
    } else {
        default_streaming_profile()
    }
}

fn default_speech_gate() -> bool {
    false
}

fn default_itn() -> bool {
    true
}

pub(crate) fn default_num_threads() -> i32 {
    4
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default = "default_num_threads")]
    pub num_threads: i32,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_streaming_profile")]
    pub streaming_profile: String,
    #[serde(default = "default_speech_gate")]
    pub speech_gate: bool,
    #[serde(default)]
    pub denoise: bool,
    #[serde(default = "default_itn")]
    pub itn: bool,
    #[serde(default)]
    pub replacements: Replacements,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            num_threads: default_num_threads(),
            model: default_model(),
            streaming_profile: default_streaming_profile(),
            speech_gate: default_speech_gate(),
            denoise: false,
            itn: default_itn(),
            replacements: Replacements::default(),
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
        let streaming_profile = setting(&value, "model", "streaming_profile", toml::Value::as_str)?
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| profile_for_model(&model));
        let speech_gate = setting(&value, "model", "speech_gate", toml::Value::as_bool)?
            .unwrap_or_else(default_speech_gate);
        let denoise = setting(&value, "audio", "denoise", toml::Value::as_bool)?.unwrap_or(false);
        let itn = setting(&value, "text", "itn", toml::Value::as_bool)?.unwrap_or_else(default_itn);
        let replacements = parse_replacements(&value)?;
        let num_threads = setting(&value, "model", "num_threads", toml::Value::as_integer)?
            .map(i32::try_from)
            .transpose()
            .map_err(|_| AppError::Config("num_threads is out of range".into()))?
            .unwrap_or_else(default_num_threads);
        let config = Self {
            num_threads,
            model,
            streaming_profile,
            speech_gate,
            denoise,
            itn,
            replacements,
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
            num_threads: default_num_threads(),
            speech_gate: default_speech_gate(),
            model,
            streaming_profile: streaming_profile.into(),
            denoise: false,
            itn: default_itn(),
            replacements: Replacements::default(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_model_overrides(
        &self,
        model: Option<String>,
        streaming_profile: Option<String>,
    ) -> Result<Self> {
        let model_changed = model
            .as_ref()
            .is_some_and(|selected| selected != &self.model);
        let model = model.unwrap_or_else(|| self.model.clone());
        let profile = streaming_profile.unwrap_or_else(|| {
            if model_changed {
                profile_for_model(&model)
            } else {
                self.streaming_profile.clone()
            }
        });
        let config = Self {
            num_threads: self.num_threads,
            speech_gate: if model_changed {
                default_speech_gate()
            } else {
                self.speech_gate
            },
            model,
            streaming_profile: profile,
            denoise: self.denoise,
            itn: self.itn,
            replacements: self.replacements.clone(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if !(1..=32).contains(&self.num_threads) {
            return Err(AppError::Config(
                "num_threads must be between 1 and 32".into(),
            ));
        }
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

fn parse_replacements(value: &toml::Value) -> Result<Replacements> {
    let Some(table) = value
        .get("text")
        .and_then(|text| text.get("replacements"))
        .or_else(|| value.get("replacements"))
    else {
        return Ok(Replacements::default());
    };
    let Some(map) = table.as_table() else {
        return Err(AppError::Config(
            "`replacements` must be a table of string to string".to_owned(),
        ));
    };
    let mut pairs = Vec::new();
    for (pattern, replacement) in map {
        let Some(replacement) = replacement.as_str() else {
            return Err(AppError::Config(format!(
                "replacement for '{pattern}' must be a string"
            )));
        };
        if pattern.split_whitespace().next().is_none() {
            return Err(AppError::Config(
                "replacement pattern must not be empty".to_owned(),
            ));
        }
        pairs.push((pattern.clone(), replacement.to_owned()));
    }
    Ok(Replacements::from_pairs(pairs))
}

// Accept both serialized flat settings and the documented section form.
// A present but invalid value is an error, never an implicit default.
fn setting<'a, T>(
    value: &'a toml::Value,
    section: &str,
    field: &str,
    parse: impl FnOnce(&'a toml::Value) -> Option<T>,
) -> Result<Option<T>> {
    let nested = match value.get(section) {
        Some(toml::Value::Table(table)) => table.get(field),
        Some(toml::Value::String(_)) if section == "model" => None,
        Some(_) => return Err(AppError::Config(format!("[{section}] must be a table"))),
        None => None,
    };
    if value.get(field).is_some() && nested.is_some() {
        return Err(AppError::Config(format!(
            "{field} is set both at the top level and in [{section}]"
        )));
    }
    value
        .get(field)
        .or(nested)
        .map(|input| {
            parse(input)
                .ok_or_else(|| AppError::Config(format!("invalid value for {section}.{field}")))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn serialized_configuration_round_trips_pipeline_options() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let config = Config {
            denoise: true,
            itn: true,
            ..Config::default()
        };
        fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn rejects_mistyped_settings_instead_of_using_defaults() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        for contents in [
            "speech_gate = 'false'",
            "streaming_profile = 560",
            "[audio]\ndenoise = 'false'",
            "[text]\nitn = 0",
            "[model]\nname = 'parakeet-unified-en-0.6b'\nspeech_gate = 'false'",
        ] {
            fs::write(&path, contents).unwrap();
            assert!(Config::load(&path).is_err(), "accepted {contents}");
        }
    }

    #[test]
    fn new_install_preserves_audio_and_model_text_with_parakeet() {
        let config = Config::default();
        assert_eq!(config.model, PARAKEET_UNIFIED_MODEL);
        assert_eq!(config.streaming_profile, "1120ms");
        assert!(!config.speech_gate);
        assert!(!config.denoise);
        assert!(config.itn);
        assert_eq!(
            config.artifact_name(),
            "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-1120ms"
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
    fn replacement_table_keeps_technical_keys_in_toml() {
        let input = "\"C++\" = \"cpp\"\n\"C#\" = \"csharp\"\n\".env\" = \"environment\"\n";
        let rules: crate::dictation_transcript::Replacements = toml::from_str(input).unwrap();
        let encoded = toml::to_string(&rules).unwrap();
        let restored: crate::dictation_transcript::Replacements = toml::from_str(&encoded).unwrap();
        assert_eq!(rules, restored);
    }

    #[test]
    fn loads_text_replacements() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "[model]\nname = \"nemotron-speech-streaming-en-0.6b\"\n[text.replacements]\n\"nv stt\" = \"nvstt\"\nparakeet = \"Parakeet\"\n",
        )
        .expect("write config");
        let config = Config::load(&path).expect("load config");
        let outcome =
            crate::recognizer::RecognitionOutcome::Transcript("nv stt uses parakeet".to_owned());
        match crate::dictation_transcript::dictation_transcript(
            outcome,
            &config.replacements,
            config.itn,
        ) {
            Ok(crate::dictation_transcript::DictationTranscript::Ready(text)) => {
                assert_eq!(text, "nvstt uses Parakeet");
            }
            other => panic!("unexpected dictation content: {other:?}"),
        }
    }

    #[test]
    fn loads_audio_and_text_pipeline_options() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        fs::write(&path, "[audio]\ndenoise = true\n[text]\nitn = false\n").expect("write config");
        let config = Config::load(&path).expect("load config");
        assert!(config.denoise);
        assert!(!config.itn);
    }

    #[test]
    fn rejects_a_non_string_replacement() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        fs::write(&path, "[text.replacements]\nfoo = 1\n").expect("write config");
        let error = Config::load(&path).expect_err("non-string replacement");
        assert!(error.to_string().contains("must be a string"));
    }

    #[test]
    fn selecting_the_same_model_preserves_explicit_settings() {
        let config = Config {
            speech_gate: true,
            streaming_profile: "1120ms".into(),
            ..Config::default()
        };
        let selected = config
            .with_model_overrides(Some(config.model.clone()), None)
            .unwrap();
        assert_eq!(config, selected);
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
        assert!(!candidate.speech_gate);
    }
}
