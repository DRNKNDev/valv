use std::sync::atomic::Ordering;

use axum::{extract::State, response::IntoResponse, Json};
use tokio::time::{sleep, Duration};
use valv_sync::protocol::ipc::DaemonStatus;

use crate::DaemonState;

pub(crate) async fn get_status(State(state): State<DaemonState>) -> impl IntoResponse {
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
    Json(DaemonStatus {
        paused: state.paused.load(Ordering::Acquire),
        backend_connected,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        mounts,
    })
}

pub(crate) async fn get_mounts(State(state): State<DaemonState>) -> impl IntoResponse {
    let mounts = state
        .mounts
        .lock()
        .await
        .iter()
        .map(|mount| mount.status())
        .collect::<Vec<_>>();
    Json(mounts)
}

pub(crate) async fn post_pause(State(state): State<DaemonState>) -> axum::http::StatusCode {
    state.paused.store(true, Ordering::Release);
    axum::http::StatusCode::NO_CONTENT
}

pub(crate) async fn post_resume(State(state): State<DaemonState>) -> axum::http::StatusCode {
    sleep(Duration::from_millis(250)).await;
    state.paused.store(false, Ordering::Release);
    axum::http::StatusCode::NO_CONTENT
}
