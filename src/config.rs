use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub sdk_paths: SdkPaths,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SdkPaths {
    pub python: PathBuf,
    pub go: PathBuf,
    pub rust: PathBuf,
    pub typescript: PathBuf,
}

impl Config {
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let config_path = path.unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap()
                .join("hanzo")
                .join("config.toml")
        });

        if config_path.exists() {
            let content = std::fs::read_to_string(config_path)?;
            Ok(toml::from_str(&content)?)
        } else {
            Ok(Self::default())
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: Some("https://api.hanzo.ai".to_string()),
            default_model: Some("claude-3-opus".to_string()),
            sdk_paths: SdkPaths::default(),
        }
    }
}

impl Default for SdkPaths {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap();
        Self {
            python: home.join("work/hanzo/sdk/src/py"),
            go: home.join("work/hanzo/sdk/src/go"),
            rust: home.join("work/hanzo/sdk/src/rs"),
            typescript: home.join("work/hanzo/sdk/src/js"),
        }
    }
}