mod app;
mod auth;
mod config;
mod daemon;
mod grants;
mod paths;
mod table;

use anyhow::Error;
use std::process::ExitCode;
use tracing_subscriber::{filter::LevelFilter, EnvFilter};

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match app::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", format_error_chain(&error));
            ExitCode::FAILURE
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
    use crate::daemon::DAEMON_NOT_RUNNING;

    #[test]
    fn format_error_chain_includes_context_and_root_cause() {
        let error =
            anyhow!(DAEMON_NOT_RUNNING).context("failed to create daemon client for status");
        let output = format_error_chain(&error);

        assert!(output.contains("failed to create daemon client for status"));
        assert!(output.contains(DAEMON_NOT_RUNNING));
    }
}
