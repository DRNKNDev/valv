use std::sync::atomic::Ordering;

use axum::{extract::State, Json};
use tokio::time::{sleep, Duration};
use valv_sync::protocol::{
    ipc::{Credential, DaemonStatus, MountStatus, PrincipalType},
    sync::WsPushNotification,
};

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
    let principal = state.principal.lock().await.clone();
    let (latest_version, update_available) = state.update_status.lock().await.as_status_fields();
    let credential = compute_credential(&state).await;
    Ok(Json(DaemonStatus {
        paused: state.paused.load(Ordering::Acquire),
        backend_connected,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        update_required,
        mounts,
        account,
        latest_version,
        update_available,
        credential,
        principal,
    }))
}

pub(crate) async fn compute_credential(state: &DaemonState) -> Credential {
    let mounts = state.mounts.lock().await;
    let has_mount_token = mounts.iter().any(|mount| mount.mount_token.is_some());
    let any_mount_rejected = mounts
        .iter()
        .any(|mount| mount.rejected.load(Ordering::Acquire));
    drop(mounts);

    let has_device_token = state
        .config
        .device_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty());
    let device_token_rejected = state.device_token_rejected.load(Ordering::Acquire);

    if device_token_rejected || any_mount_rejected {
        return Credential::Rejected;
    }
    if !has_device_token && !has_mount_token {
        return Credential::None;
    }

    let principal = state.principal.lock().await;
    match principal.as_ref().map(|principal| principal.principal_type) {
        Some(PrincipalType::Account) => Credential::Account,
        Some(PrincipalType::AccessKey) => Credential::AccessKey,
        None if !has_device_token => Credential::AccessKey,
        None => Credential::Pending,
    }
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

    // A WS notification that arrived while paused was read off notify_rx and
    // discarded by sync_loop's `if state.paused { continue }` guard, with
    // nothing else to re-trigger a pull on resume. Re-enqueue one synthetic
    // notification per mount so a change that arrived during the pause is
    // still picked up promptly instead of waiting for the next ticker floor.
    // try_send (not send().await): a full channel must never stall this HTTP
    // response - the ticker/other notifications still cover a dropped enqueue.
    let mounts = state.mounts.lock().await.clone();
    let senders = state.notify_senders.lock().await.clone();
    for mount in mounts {
        if let Some(sender) = senders.get(&mount.path) {
            let _ = sender.try_send(WsPushNotification {
                folder_id: mount.folder_id,
                server_seq: 0,
            });
        }
    }

    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{atomic::AtomicBool, Arc},
    };

    use rusqlite::Connection;
    use tokio::sync::mpsc;
    use tokio::sync::Mutex;
    use tokio::sync::Notify;
    use tokio::time::timeout;

    use crate::config::DaemonConfig;
    use crate::MountState;
    use valv_sync::protocol::ipc::{PrincipalScope, PrincipalStatus};

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
            notify_senders: Arc::new(Mutex::new(HashMap::new())),
            account: Arc::new(Mutex::new(None)),
            principal: Arc::new(Mutex::new(None)),
            device_token_rejected: Arc::new(AtomicBool::new(false)),
            update_status: Arc::new(Mutex::new(Default::default())),
            backend_health: Arc::new(crate::BackendHealth::default()),
            pending_uploads: Arc::new(Mutex::new(std::collections::HashSet::new())),
            deferred_deletes: Arc::new(Mutex::new(HashMap::new())),
            db: Arc::new(Mutex::new(conn)),
            client: reqwest::Client::new(),
            config,
        }
    }

    fn connected_config() -> DaemonConfig {
        DaemonConfig {
            backend_url: "http://127.0.0.1:1".to_owned(),
            device_id: "device-1".to_owned(),
            device_token: Some("token".to_owned()),
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
    async fn get_status_omits_update_fields_before_any_check_completes() {
        let response = get_status(State(test_state(connected_config())))
            .await
            .unwrap();

        assert!(response.0.latest_version.is_none());
        assert!(response.0.update_available.is_none());
    }

    #[tokio::test]
    async fn get_status_includes_update_fields_after_a_successful_check() {
        let state = test_state(connected_config());
        {
            let mut update_status = state.update_status.lock().await;
            update_status.latest_version = Some("9.9.9".to_owned());
            update_status.update_available = Some(true);
        }

        let response = get_status(State(state)).await.unwrap();

        assert_eq!(response.0.latest_version.as_deref(), Some("9.9.9"));
        assert_eq!(response.0.update_available, Some(true));
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

    #[tokio::test]
    async fn post_resume_enqueues_a_catch_up_notification_per_mount() {
        let state = test_state(connected_config());
        let mount = test_mount(false);
        *state.mounts.lock().await = vec![mount.clone()];
        let (tx, mut rx) = mpsc::channel::<WsPushNotification>(4);
        state
            .notify_senders
            .lock()
            .await
            .insert(mount.path.clone(), tx);

        post_resume(State(state.clone())).await.unwrap();

        let notification = timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            notification,
            WsPushNotification {
                folder_id: mount.folder_id.clone(),
                server_seq: 0
            }
        );
    }

    #[tokio::test]
    async fn post_resume_does_not_fail_when_a_mount_has_no_notify_sender_yet() {
        let state = test_state(connected_config());
        *state.mounts.lock().await = vec![test_mount(false)];

        assert_eq!(
            post_resume(State(state)).await.unwrap(),
            axum::http::StatusCode::NO_CONTENT
        );
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
            rejected: Arc::new(AtomicBool::new(false)),
            error: None,
            watcher_alive: Arc::new(AtomicBool::new(true)),
            sync_lock: Arc::new(Mutex::new(())),
            cursor_notify: Arc::new(Notify::new()),
        }
    }

    fn access_key_mount(mount_token: &str) -> MountState {
        MountState {
            path: "/sync".to_owned(),
            folder_id: "folder-1".to_owned(),
            grant_id: Some("grant-1".to_owned()),
            scope_node_id: Some("scope-1".to_owned()),
            mount_token: Some(mount_token.to_owned()),
            can_write: true,
            name: "Test Folder".to_owned(),
            active_syncs: 0,
            pending_ops: 0,
            last_synced_at: None,
            update_required: false,
            update_required_flag: Arc::new(AtomicBool::new(false)),
            rejected: Arc::new(AtomicBool::new(false)),
            error: None,
            watcher_alive: Arc::new(AtomicBool::new(true)),
            sync_lock: Arc::new(Mutex::new(())),
            cursor_notify: Arc::new(Notify::new()),
        }
    }

    fn credential_less_config() -> DaemonConfig {
        DaemonConfig {
            backend_url: "http://127.0.0.1:1".to_owned(),
            device_id: String::new(),
            device_token: None,
            device_name: "Test Device".to_owned(),
            mounts: Vec::new(),
        }
    }

    #[tokio::test]
    async fn get_status_reports_none_with_no_credential_of_any_kind() {
        let state = test_state(credential_less_config());

        let response = get_status(State(state)).await.unwrap().0;

        assert_eq!(response.credential, Credential::None);
        assert!(response.principal.is_none());
    }

    #[tokio::test]
    async fn get_status_reports_pending_for_a_bare_device_token_before_any_poll() {
        let state = test_state(connected_config());

        let response = get_status(State(state)).await.unwrap().0;

        assert_eq!(response.credential, Credential::Pending);
    }

    #[tokio::test]
    async fn get_status_reports_access_key_for_a_mount_token_with_no_device_token() {
        let state = test_state(credential_less_config());
        *state.mounts.lock().await = vec![access_key_mount("mount-token-1")];

        let response = get_status(State(state)).await.unwrap().0;

        assert_eq!(response.credential, Credential::AccessKey);
    }

    #[tokio::test]
    async fn get_status_cutover_to_access_key_happens_the_instant_a_mount_is_added() {
        let state = test_state(credential_less_config());
        assert_eq!(
            get_status(State(state.clone())).await.unwrap().0.credential,
            Credential::None
        );

        state
            .mounts
            .lock()
            .await
            .push(access_key_mount("mount-token-1"));

        assert_eq!(
            get_status(State(state)).await.unwrap().0.credential,
            Credential::AccessKey
        );
    }

    #[tokio::test]
    async fn get_status_reports_rejected_when_device_token_is_rejected() {
        let state = test_state(connected_config());
        state.device_token_rejected.store(true, Ordering::Release);

        let response = get_status(State(state)).await.unwrap().0;

        assert_eq!(response.credential, Credential::Rejected);
    }

    #[tokio::test]
    async fn get_status_reports_rejected_when_a_mount_token_is_rejected() {
        let state = test_state(credential_less_config());
        let mount = access_key_mount("mount-token-1");
        mount.rejected.store(true, Ordering::Release);
        *state.mounts.lock().await = vec![mount];

        let response = get_status(State(state)).await.unwrap().0;

        assert_eq!(response.credential, Credential::Rejected);
    }

    #[tokio::test]
    async fn get_status_reports_cached_principal_and_its_scopes() {
        let state = test_state(credential_less_config());
        *state.mounts.lock().await = vec![access_key_mount("mount-token-1")];
        *state.principal.lock().await = Some(PrincipalStatus {
            principal_type: PrincipalType::AccessKey,
            email: None,
            scopes: vec![PrincipalScope {
                folder_id: "folder-1".to_owned(),
                folder_name: "Design".to_owned(),
                scope_label: "Design".to_owned(),
                can_write: true,
            }],
        });

        let response = get_status(State(state)).await.unwrap().0;

        assert_eq!(response.credential, Credential::AccessKey);
        let principal = response.principal.unwrap();
        assert!(principal.email.is_none());
        assert_eq!(principal.scopes.len(), 1);
        assert_eq!(principal.scopes[0].folder_name, "Design");
    }

    #[tokio::test]
    async fn get_status_follows_the_backend_classification_over_pending() {
        let state = test_state(connected_config());
        *state.principal.lock().await = Some(PrincipalStatus {
            principal_type: PrincipalType::AccessKey,
            email: None,
            scopes: Vec::new(),
        });

        let response = get_status(State(state)).await.unwrap().0;

        assert_eq!(response.credential, Credential::AccessKey);
    }
}
