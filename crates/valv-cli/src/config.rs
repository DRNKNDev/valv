use anyhow::{Context, Result};
use serde::Deserialize;

use crate::{error::CliError, paths::config_path};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CliConfig {
    pub(crate) backend_url: String,
    #[serde(default)]
    pub(crate) device_token: Option<String>,
}

impl CliConfig {
    pub(crate) fn token(&self) -> Result<&str> {
        self.device_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| CliError::no_credential().into())
    }
}

pub(crate) fn load_config() -> Result<CliConfig> {
    let path = config_path().context("failed to determine config path")?;
    let text = std::fs::read_to_string(&path).map_err(|_| CliError::not_configured())?;
    let config = toml::from_str::<CliConfig>(&text).context("failed to parse config.toml")?;
    if config.backend_url.trim().is_empty() {
        return Err(CliError::not_configured().into());
    }
    config.token()?;
    Ok(config)
}

#[derive(Debug, Clone, Deserialize)]
struct BackendUrlOnly {
    backend_url: String,
}

pub(crate) fn load_backend_url() -> Result<String> {
    let path = config_path().context("failed to determine config path")?;
    let text = std::fs::read_to_string(&path).map_err(|_| CliError::not_configured())?;
    let parsed =
        toml::from_str::<BackendUrlOnly>(&text).context("failed to parse config.toml")?;
    if parsed.backend_url.trim().is_empty() {
        return Err(CliError::not_configured().into());
    }
    Ok(parsed.backend_url)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ConfigPeek {
    #[serde(default)]
    pub(crate) device_token: Option<String>,
    #[serde(default)]
    pub(crate) device_name: Option<String>,
}

pub(crate) fn peek_config() -> Option<ConfigPeek> {
    peek_config_at(&config_path().ok()?)
}

fn peek_config_at(path: &std::path::Path) -> Option<ConfigPeek> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str::<ConfigPeek>(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credential_less_config_toml_parses_instead_of_erroring_on_a_missing_field() {
        let parsed = toml::from_str::<CliConfig>("backend_url = \"https://api.valv.dev\"\n");

        assert!(
            parsed.is_ok(),
            "the config ensure_daemon itself writes (no device_token key at all) must parse: {parsed:?}"
        );
    }

    #[test]
    fn load_config_reports_no_credential_rather_than_a_parse_failure_when_the_token_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "backend_url = \"https://api.valv.dev\"\n").unwrap();

        let config = toml::from_str::<CliConfig>(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let error = config.token().unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("a missing device_token should be a CliError");

        assert_eq!(cli_error.payload.code, "no_credential");
    }

    #[test]
    fn peek_config_at_reads_device_name_and_token_without_requiring_either() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "backend_url = \"https://api.valv.dev\"\ndevice_name = \"Test Box\"\n",
        )
        .unwrap();

        let peek = peek_config_at(&path).unwrap();

        assert_eq!(peek.device_name.as_deref(), Some("Test Box"));
        assert!(peek.device_token.is_none());
    }

    #[test]
    fn peek_config_at_returns_none_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(peek_config_at(&dir.path().join("missing.toml")).is_none());
    }
}
