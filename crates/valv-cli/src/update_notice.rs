
use std::{
    fs,
    io::IsTerminal,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use valv_sync::{
    protocol::ipc::DaemonStatus,
    update::{self as shared_update, is_newer_version, resolve_latest_version},
};

use crate::{daemon::daemon_client, paths::local_state_dir};

const CACHE_FILE_NAME: &str = "update-check.json";
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct UpdateCheckState {
    checked_at_unix_secs: u64,
    latest_version: String,
}

pub(crate) async fn maybe_print_update_notice() {
    let no_update_check = std::env::var("VALV_NO_UPDATE_CHECK").ok();
    if should_skip_notice(no_update_check.as_deref(), std::io::stderr().is_terminal()) {
        return;
    }

    let bypass_cache = daemon_reports_update_required().await;
    let Some(latest_version) = resolve_notice_version(bypass_cache).await else {
        return;
    };

    if is_newer_version(&latest_version, env!("CARGO_PKG_VERSION")) {
        eprintln!(
            "A newer version of valv is available ({latest_version}). Run 'valv update' to install it."
        );
    }
}

fn should_skip_notice(no_update_check_env: Option<&str>, stderr_is_tty: bool) -> bool {
    if no_update_check_env == Some("1") {
        return true;
    }
    !stderr_is_tty
}

async fn resolve_notice_version(bypass_cache: bool) -> Option<String> {
    let cache_path = local_state_dir().ok()?.join(CACHE_FILE_NAME);

    if !bypass_cache {
        if let Some(cached) = read_fresh_cache(&cache_path, now_unix_secs()) {
            return Some(cached.latest_version);
        }
    }

    let client = reqwest::Client::new();
    let latest_version =
        resolve_latest_version(&client, shared_update::DEFAULT_REPO, "VALV_VERSION")
            .await
            .ok()?;
    write_cache(
        &cache_path,
        &UpdateCheckState {
            checked_at_unix_secs: now_unix_secs(),
            latest_version: latest_version.clone(),
        },
    );
    Some(latest_version)
}

fn read_fresh_cache(cache_path: &std::path::Path, now: u64) -> Option<UpdateCheckState> {
    let contents = fs::read_to_string(cache_path).ok()?;
    let state = serde_json::from_str::<UpdateCheckState>(&contents).ok()?;
    if cache_is_fresh(state.checked_at_unix_secs, now, CACHE_TTL) {
        Some(state)
    } else {
        None
    }
}

fn cache_is_fresh(checked_at_unix_secs: u64, now: u64, ttl: Duration) -> bool {
    now.saturating_sub(checked_at_unix_secs) < ttl.as_secs()
}

fn write_cache(cache_path: &std::path::Path, state: &UpdateCheckState) {
    let Some(parent) = cache_path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    if let Ok(contents) = serde_json::to_string(state) {
        let _ = fs::write(cache_path, contents);
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

async fn daemon_reports_update_required() -> bool {
    let Ok(client) = daemon_client() else {
        return false;
    };
    let Ok(response) = client.get("http://localhost/status").send().await else {
        return false;
    };
    let Ok(status) = response.json::<DaemonStatus>().await else {
        return false;
    };
    status_requires_notice_bypass(&status)
}

fn status_requires_notice_bypass(status: &DaemonStatus) -> bool {
    status.update_required
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_skip_notice_when_escape_hatch_is_set() {
        assert!(should_skip_notice(Some("1"), true));
    }

    #[test]
    fn should_skip_notice_when_stderr_is_not_a_tty() {
        assert!(should_skip_notice(None, false));
    }

    #[test]
    fn should_not_skip_notice_on_an_interactive_terminal_without_the_escape_hatch() {
        assert!(!should_skip_notice(None, true));
    }

    #[test]
    fn escape_hatch_only_matches_the_literal_value_one() {
        assert!(!should_skip_notice(Some("true"), true));
    }

    #[test]
    fn cache_is_fresh_within_24_hours() {
        let checked_at = 1_000_000;
        assert!(cache_is_fresh(checked_at, checked_at + 60, CACHE_TTL));
        assert!(cache_is_fresh(
            checked_at,
            checked_at + CACHE_TTL.as_secs() - 1,
            CACHE_TTL
        ));
    }

    #[test]
    fn cache_is_stale_after_24_hours() {
        let checked_at = 1_000_000;
        assert!(!cache_is_fresh(
            checked_at,
            checked_at + CACHE_TTL.as_secs(),
            CACHE_TTL
        ));
        assert!(!cache_is_fresh(
            checked_at,
            checked_at + CACHE_TTL.as_secs() + 3600,
            CACHE_TTL
        ));
    }

    #[test]
    fn read_fresh_cache_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("missing.json");

        assert!(read_fresh_cache(&cache_path, now_unix_secs()).is_none());
    }

    #[test]
    fn write_then_read_fresh_cache_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("update-check.json");
        let now = now_unix_secs();
        let state = UpdateCheckState {
            checked_at_unix_secs: now,
            latest_version: "9.9.9".to_owned(),
        };

        write_cache(&cache_path, &state);
        let read_back = read_fresh_cache(&cache_path, now + 60).unwrap();

        assert_eq!(read_back, state);
    }

    #[test]
    fn status_requires_notice_bypass_reflects_update_required() {
        let mut status = DaemonStatus {
            paused: false,
            backend_connected: true,
            version: "0.1.0".into(),
            update_required: false,
            mounts: vec![],
            account: None,
            latest_version: None,
            update_available: None,
        };
        assert!(!status_requires_notice_bypass(&status));

        status.update_required = true;
        assert!(status_requires_notice_bypass(&status));
    }

    #[test]
    fn read_fresh_cache_ignores_a_stale_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("update-check.json");
        let checked_at = 1_000_000;
        write_cache(
            &cache_path,
            &UpdateCheckState {
                checked_at_unix_secs: checked_at,
                latest_version: "9.9.9".to_owned(),
            },
        );

        let stale_now = checked_at + CACHE_TTL.as_secs() + 1;

        assert!(read_fresh_cache(&cache_path, stale_now).is_none());
    }
}
