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
        .collect::<Vec<_>>();
    let update_required = mounts.iter().any(|mount| mount.update_required);
    let backend_connected = state.backend_health.is_connected();
    let account = state.account.lock().await.clone();
    Ok(Json(DaemonStatus {
        paused: state.paused.load(Ordering::Acquire),
        backend_connected,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        update_required,
        mounts,
        account,
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
    use tokio::sync::Notify;

    use crate::config::DaemonConfig;
    use crate::MountState;

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
            account: Arc::new(Mutex::new(None)),
            backend_health: Arc::new(crate::BackendHealth::default()),
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
    async fn get_status_defaults_connected_without_backend_signal() {
        let response = get_status(State(test_state(connected_config())))
            .await
            .unwrap();

        assert!(response.0.backend_connected);
        assert!(!response.0.paused);
        assert!(!response.0.update_required);
    }

    #[tokio::test]
    async fn get_status_reports_disconnected_after_backend_failure() {
        let state = test_state(connected_config());
        state.backend_health.record_failure();

        let response = get_status(State(state)).await.unwrap();

        assert!(!response.0.backend_connected);
    }

    #[tokio::test]
    async fn get_status_aggregates_update_required_from_mounts() {
        let state = test_state(connected_config());
        *state.mounts.lock().await = vec![test_mount(true)];

        let response = get_status(State(state)).await.unwrap().0;

        assert!(response.backend_connected);
        assert!(response.update_required);
        assert!(response.mounts[0].update_required);
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

    fn test_mount(update_required: bool) -> MountState {
        let update_required_flag = Arc::new(AtomicBool::new(update_required));
        MountState {
            path: "/sync".to_owned(),
            folder_id: "folder-1".to_owned(),
            grant_id: None,
            scope_node_id: None,
            mount_token: None,
            can_write: true,
            name: "Test Folder".to_owned(),
            active_syncs: 0,
            pending_ops: 0,
            last_synced_at: None,
            update_required,
            update_required_flag,
            error: None,
            sync_lock: Arc::new(Mutex::new(())),
            cursor_notify: Arc::new(Notify::new()),
        }
    }
}
