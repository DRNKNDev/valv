use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

use axum::{extract::State, http::StatusCode, Json};
use chrono::Utc;
use tokio::{
    sync::mpsc,
    time::{interval, MissedTickBehavior},
};
use valv_sync::{
    protocol::{ipc::SyncRequest, sync::WsPushNotification},
    sync_engine::{delta_pull::pull_delta, ws_client::ws_push_loop},
    watch::{fs_watch_task, WatchMount},
};

use crate::{DaemonState, MountState};

pub(crate) async fn post_sync(
    State(state): State<DaemonState>,
    Json(req): Json<SyncRequest>,
) -> StatusCode {
    let targets = {
        let mounts = state.mounts.lock().await;
        mounts
            .iter()
            .filter(|mount| {
                req.folder_id
                    .as_ref()
                    .is_none_or(|folder_id| folder_id == &mount.folder_id)
            })
            .cloned()
            .collect::<Vec<_>>()
    };

    for mount in targets {
        let state = state.clone();
        tokio::spawn(async move {
            pull_mount_once(&state, &mount).await;
        });
    }

    StatusCode::NO_CONTENT
}

pub(crate) async fn spawn_mount_tasks(state: &DaemonState) {
    let mounts = state.mounts.lock().await.clone();
    for mount in mounts {
        spawn_tasks_for_mount(state, mount).await;
    }
}

pub(crate) async fn spawn_tasks_for_mount(state: &DaemonState, mount: MountState) {
    let (notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(32);
    let token = mount.effective_token(&state.config).to_owned();

    let sync_state = state.clone();
    let sync_mount = mount.clone();
    let sync_handle = tokio::spawn(async move {
        sync_loop(sync_state, sync_mount, notify_rx).await;
    });

    let ws_backend_url = state.config.backend_url.clone();
    let ws_token = token.clone();
    let ws_folder_id = mount.folder_id.clone();
    let ws_handle = tokio::spawn(async move {
        if let Err(error) =
            ws_push_loop(&ws_backend_url, &ws_token, vec![ws_folder_id], notify_tx).await
        {
            eprintln!("websocket task failed: {error}");
        }
    });

    let fs_handle = tokio::spawn({
        let paused = state.paused.clone();
        let db = state.db.clone();
        let client = state.client.clone();
        let backend_url = state.config.backend_url.clone();
        let device_name = state.config.device_name.clone();
        let watch_mount = WatchMount {
            path: PathBuf::from(&mount.path),
            folder_id: mount.folder_id.clone(),
            device_name,
        };
        async move {
            if let Err(error) =
                fs_watch_task(watch_mount, paused, db, client, backend_url, token).await
            {
                eprintln!("filesystem watch task failed: {error}");
            }
        }
    });

    state
        .tasks
        .lock()
        .await
        .extend([sync_handle, ws_handle, fs_handle]);
}

async fn sync_loop(
    state: DaemonState,
    mount: MountState,
    mut notify_rx: mpsc::Receiver<WsPushNotification>,
) {
    let mut ticker = interval(Duration::from_secs(30));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {},
            notification = notify_rx.recv() => {
                let Some(notification) = notification else {
                    return;
                };
                if notification.folder_id != mount.folder_id {
                    continue;
                }
            }
        }

        if state.paused.load(Ordering::Acquire) {
            continue;
        }

        pull_mount_once(&state, &mount).await;
    }
}

async fn pull_mount_once(state: &DaemonState, mount: &MountState) {
    set_mount_syncing(state, &mount.folder_id, true, None).await;
    let result = {
        let mut conn = state.db.lock().await;
        let token = mount.effective_token(&state.config).to_owned();
        pull_delta(
            &state.client,
            &state.config.backend_url,
            &token,
            &mount.folder_id,
            &mut conn,
        )
        .await
    };
    let error = result.err().map(|err| err.to_string());
    set_mount_syncing(state, &mount.folder_id, false, error).await;
}

pub(crate) async fn cancel_mount_tasks(state: &DaemonState) {
    for task in state.tasks.lock().await.drain(..) {
        task.abort();
    }
}

async fn set_mount_syncing(
    state: &DaemonState,
    folder_id: &str,
    syncing: bool,
    error: Option<String>,
) {
    let mut mounts = state.mounts.lock().await;
    if let Some(mount) = mounts.iter_mut().find(|mount| mount.folder_id == folder_id) {
        mount.syncing = syncing;
        mount.error = error;
        if !syncing && mount.error.is_none() {
            mount.last_synced_at = Some(Utc::now().to_rfc3339());
        }
    }
}
