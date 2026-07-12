use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::paths::config_path;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CliConfig {
    pub(crate) backend_url: String,
    pub(crate) device_token: String,
}

pub(crate) fn load_config() -> Result<CliConfig> {
    let path = config_path().context("failed to determine config path")?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("Not signed in. Run: valv auth login ({})", path.display()))?;
    let config = toml::from_str::<CliConfig>(&text).context("failed to parse config.toml")?;
    if config.backend_url.trim().is_empty() {
        return Err(anyhow!(
            "Missing backend_url in config.toml. Run: valv auth login"
        ));
    }
    if config.device_token.trim().is_empty() {
        return Err(anyhow!(
            "Missing device_token in config.toml. Run: valv auth login"
        ));
    }
    Ok(config)
}
