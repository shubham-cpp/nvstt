use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

pub const DEFAULT_MODEL: &str = "parakeet-unified-en-0.6b";
pub const DEFAULT_ARTIFACT: &str = "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default = "default_model")]
    pub model: String,
}

fn default_model() -> String {
    DEFAULT_MODEL.to_owned()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
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
        let config = Self { model };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.model != DEFAULT_MODEL {
            return Err(AppError::Config(format!(
                "unsupported model '{}'; expected '{}'",
                self.model, DEFAULT_MODEL
            )));
        }
        Ok(())
    }

    pub fn artifact_name(&self) -> &'static str {
        DEFAULT_ARTIFACT
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn accepts_flat_model_configuration() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        fs::write(&path, "model = \"parakeet-unified-en-0.6b\"\n").expect("write config");
        assert_eq!(
            Config::load(&path).expect("load config").model,
            DEFAULT_MODEL
        );
    }

    #[test]
    fn accepts_legacy_nested_model_name_configuration() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        fs::write(&path, "[model]\nname = \"parakeet-unified-en-0.6b\"\n").expect("write config");
        assert_eq!(
            Config::load(&path).expect("load config").model,
            DEFAULT_MODEL
        );
    }
}
