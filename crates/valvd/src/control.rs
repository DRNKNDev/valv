use std::sync::atomic::Ordering;

use axum::{extract::State, Json};
use tokio::time::{sleep, Duration};
use valv_sync::protocol::ipc::{DaemonStatus, MountStatus};

use crate::{error::DaemonError, DaemonState};

pub(crate) async fn get_status(
    State(state): State<DaemonState>,
) -> Result<Json<DaemonStatus>, DaemonError> {
    let mounts = state
        .mounts
        .lock()
        .await
        .iter()
        .map(|mount| mount.status())
        .collect();
    let backend_connected = !state.config.backend_url.is_empty()
        && !state.config.device_id.is_empty()
        && !state.config.device_token.is_empty()
        && !state.config.device_name.is_empty();
    Ok(Json(DaemonStatus {
        paused: state.paused.load(Ordering::Acquire),
        backend_connected,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        mounts,
    }))
}

pub(crate) async fn get_mounts(
    State(state): State<DaemonState>,
) -> Result<Json<Vec<MountStatus>>, DaemonError> {
    let mounts = state
        .mounts
        .lock()
        .await
        .iter()
        .map(|mount| mount.status())
        .collect::<Vec<_>>();
    Ok(Json(mounts))
}

pub(crate) async fn post_pause(
    State(state): State<DaemonState>,
) -> Result<axum::http::StatusCode, DaemonError> {
    state.paused.store(true, Ordering::Release);
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub(crate) async fn post_resume(
    State(state): State<DaemonState>,
) -> Result<axum::http::StatusCode, DaemonError> {
    sleep(Duration::from_millis(250)).await;
    state.paused.store(false, Ordering::Release);
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{atomic::AtomicBool, Arc},
    };

    use rusqlite::Connection;
    use tokio::sync::Mutex;

    use crate::config::DaemonConfig;

    use super::*;

    fn test_state(config: DaemonConfig) -> DaemonState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../valv-sync/src/persistence/schema.sql"))
            .unwrap();
        DaemonState {
            paused: Arc::new(AtomicBool::new(false)),
            fs_events_paused: Arc::new(AtomicBool::new(false)),
            mounts: Arc::new(Mutex::new(Vec::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            db: Arc::new(Mutex::new(conn)),
            client: reqwest::Client::new(),
            config,
        }
    }

    fn connected_config() -> DaemonConfig {
        DaemonConfig {
            backend_url: "http://127.0.0.1:1".to_owned(),
            device_id: "device-1".to_owned(),
            device_token: "token".to_owned(),
            device_name: "Test Device".to_owned(),
            mounts: Vec::new(),
        }
    }

    #[tokio::test]
    async fn get_status_reports_connected_config() {
        let response = get_status(State(test_state(connected_config())))
            .await
            .unwrap();

        assert!(response.0.backend_connected);
        assert!(!response.0.paused);
    }

    #[tokio::test]
    async fn get_status_reports_disconnected_when_config_is_incomplete() {
        let mut config = connected_config();
        config.device_token.clear();

        let response = get_status(State(test_state(config))).await.unwrap();

        assert!(!response.0.backend_connected);
    }

    #[tokio::test]
    async fn pause_and_resume_toggle_state() {
        let state = test_state(connected_config());

        assert_eq!(
            post_pause(State(state.clone())).await.unwrap(),
            axum::http::StatusCode::NO_CONTENT
        );
        assert!(state.paused.load(Ordering::Acquire));
        assert_eq!(
            post_resume(State(state.clone())).await.unwrap(),
            axum::http::StatusCode::NO_CONTENT
        );
        assert!(!state.paused.load(Ordering::Acquire));
    }
}
