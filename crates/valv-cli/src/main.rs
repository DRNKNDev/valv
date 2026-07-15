mod app;
mod auth;
mod config;
mod daemon;
mod error;
mod format;
mod grants;
mod paths;
mod table;
mod update;
mod update_notice;

use anyhow::Error;
use std::process::ExitCode;
use tracing_subscriber::{filter::LevelFilter, EnvFilter};

#[cfg(test)]
static LOOPBACK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// HOME is process-global; this serializes tests that override it for a real config/socket path.
#[cfg(test)]
pub(crate) static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match app::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            tracing::debug!("{}", format_error_chain(&failure.error));
            error::report(&failure.error, failure.json)
        }
    }
}

fn format_error_chain(error: &Error) -> String {
    format!("{error:#}")
}

fn init_tracing() {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn format_error_chain_includes_context_and_root_cause() {
        let error = anyhow!("daemon socket connect refused")
            .context("failed to create daemon client for status");
        let output = format_error_chain(&error);

        assert!(output.contains("failed to create daemon client for status"));
        assert!(output.contains("daemon socket connect refused"));
    }

    #[test]
    fn format_error_chain_never_names_the_deleted_install_command() {
        let error = crate::error::CliError::daemon_not_running();
        let output = format_error_chain(&anyhow!(error));

        assert!(!output.contains("valv daemon install"));
    }
}
