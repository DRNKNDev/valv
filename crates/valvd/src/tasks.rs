use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

use axum::{extract::State, http::StatusCode, Json};
use chrono::Utc;
use serde::Deserialize;
use tokio::{
    sync::mpsc,
    time::{interval_at, sleep, sleep_until, Instant, MissedTickBehavior},
};
use valv_sync::{
    persistence::{
        chunks as chunk_store,
        mounts as mount_store,
        nodes::LocalNode,
        versions::{self, upsert_version, LocalVersion},
    },
    protocol::{
        ipc::{
            AccountStatus, PrincipalScope, PrincipalStatus, PrincipalType, SyncRequest,
            SyncSummary,
        },
        sync::{manifest_content_hash, ChunkRef, WsPushNotification},
    },
    storage::download_chunks,
    sync_engine::{
        delta_pull::{pull_delta, PulledNode},
        local_push::{push_local_with_update_required, PushSummary},
        update_required::is_update_required,
        ws_client::ws_push_loop,
    },
    watch::{fs_watch_task, DirtySignal, WatchMount},
};

use crate::{
    control::compute_credential,
    error::{backend_response_or_error, is_unauthenticated, require_token, DaemonError},
    DaemonState, MountState,
};

pub(crate) async fn post_sync(
    State(state): State<DaemonState>,
    Json(req): Json<SyncRequest>,
) -> Result<Json<SyncSummary>, DaemonError> {
    if req.folder_id.as_deref().is_some_and(str::is_empty) {
        return Err(DaemonError::BadRequest(
            "folder_id cannot be empty".to_owned(),
        ));
    }
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

    let mut summary = SyncSummary::default();
    for mount in targets {
        let mount_summary =
            run_full_sync_mount(state.clone(), mount, MaterializeScope::Full).await?;
        merge_sync_summary(&mut summary, mount_summary);
    }

    Ok(Json(summary))
}

pub(crate) async fn spawn_mount_tasks(state: &DaemonState) {
    let mounts = state.mounts.lock().await.clone();
    for mount in mounts {
        spawn_tasks_for_mount(state, mount).await;
    }
}

pub(crate) fn spawn_account_status_task(state: &DaemonState) -> tokio::task::JoinHandle<()> {
    let state = state.clone();
    tokio::spawn(async move {
        account_status_loop(state).await;
    })
}

async fn account_status_loop(state: DaemonState) {
    let normal_period = Duration::from_secs(5 * 60);
    let not_found_period = Duration::from_secs(60 * 60);
    let mut period = normal_period;

    loop {
        let mut ticker = interval_at(Instant::now() + period, period);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker.tick().await;

        // Runs first so this tick can discover a device_token is actually a
        // legacy access key before deciding whether to poll account/usage.
        poll_principal_status_once(&state).await;
        let outcome = if compute_credential(&state).await == valv_sync::protocol::ipc::Credential::Account {
            poll_account_status_once(&state).await
        } else {
            *state.account.lock().await = None;
            AccountPollOutcome::Unchanged
        };
        period = if matches!(outcome, AccountPollOutcome::NotFound) {
            not_found_period
        } else {
            normal_period
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountPollOutcome {
    Updated,
    NotFound,
    Unchanged,
}

async fn poll_account_status_once(state: &DaemonState) -> AccountPollOutcome {
    if state.config.backend_url.is_empty() {
        return AccountPollOutcome::Unchanged;
    }
    let Some(device_token) = non_empty_device_token(state) else {
        return AccountPollOutcome::Unchanged;
    };

    let response = state
        .client
        .get(format!(
            "{}/account/usage",
            valv_sync::api_base(&state.config.backend_url)
        ))
        .bearer_auth(device_token)
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(error = %error, "account status poll failed");
            return AccountPollOutcome::Unchanged;
        }
    };

    match backend_response_or_error(response).await {
        Ok(response) => match response.json::<AccountStatus>().await {
            Ok(account) => {
                *state.account.lock().await = Some(account);
                AccountPollOutcome::Updated
            }
            Err(error) => {
                tracing::warn!(error = %error, "account status response decode failed");
                AccountPollOutcome::Unchanged
            }
        },
        Err(DaemonError::Backend { status, .. }) if status == StatusCode::NOT_FOUND => {
            *state.account.lock().await = None;
            AccountPollOutcome::NotFound
        }
        Err(error) => {
            tracing::warn!(error = %error, "account status poll returned an error");
            AccountPollOutcome::Unchanged
        }
    }
}

fn non_empty_device_token(state: &DaemonState) -> Option<&str> {
    state
        .config
        .device_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
}

async fn distinct_credential_tokens(state: &DaemonState) -> Vec<String> {
    let mut tokens = Vec::new();
    if let Some(token) = non_empty_device_token(state) {
        tokens.push(token.to_owned());
    }
    let mounts = state.mounts.lock().await;
    for mount in mounts.iter() {
        if let Some(token) = &mount.mount_token {
            if !tokens.contains(token) {
                tokens.push(token.clone());
            }
        }
    }
    tokens
}

async fn mark_credential_rejected(state: &DaemonState, token: &str) {
    if non_empty_device_token(state) == Some(token) {
        state.device_token_rejected.store(true, Ordering::Release);
    }
    let mounts = state.mounts.lock().await;
    for mount in mounts.iter() {
        if mount.mount_token.as_deref() == Some(token) {
            mount.rejected.store(true, Ordering::Release);
        }
    }
}

async fn clear_credential_rejected(state: &DaemonState, token: &str) {
    if non_empty_device_token(state) == Some(token) {
        state.device_token_rejected.store(false, Ordering::Release);
    }
    let mounts = state.mounts.lock().await;
    for mount in mounts.iter() {
        if mount.mount_token.as_deref() == Some(token) {
            mount.rejected.store(false, Ordering::Release);
        }
    }
}

async fn poll_principal_status_once(state: &DaemonState) {
    if state.config.backend_url.is_empty() {
        return;
    }
    let tokens = distinct_credential_tokens(state).await;
    if tokens.is_empty() {
        *state.principal.lock().await = None;
        return;
    }

    let mut principal_type: Option<PrincipalType> = None;
    let mut email: Option<String> = None;
    let mut scopes: Vec<PrincipalScope> = Vec::new();
    let mut resolved_any = false;

    for token in &tokens {
        let response = state
            .client
            .get(format!("{}/me", valv_sync::api_base(&state.config.backend_url)))
            .bearer_auth(token)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(error = %error, "principal poll failed");
                continue;
            }
        };
        if response.status() == StatusCode::UNAUTHORIZED {
            mark_credential_rejected(state, token).await;
            continue;
        }
        let principal = match backend_response_or_error(response).await {
            Ok(response) => match response.json::<PrincipalStatus>().await {
                Ok(principal) => principal,
                Err(error) => {
                    tracing::warn!(error = %error, "principal response decode failed");
                    continue;
                }
            },
            Err(error) => {
                tracing::warn!(error = %error, "principal poll returned an error");
                continue;
            }
        };
        clear_credential_rejected(state, token).await;
        resolved_any = true;
        match principal.principal_type {
            PrincipalType::Account => {
                principal_type = Some(PrincipalType::Account);
                email = principal.email.or(email);
            }
            PrincipalType::AccessKey => {
                principal_type.get_or_insert(PrincipalType::AccessKey);
                scopes.extend(principal.scopes);
            }
        }
    }

    if resolved_any {
        if let Some(principal_type) = principal_type {
            *state.principal.lock().await = Some(PrincipalStatus {
                principal_type,
                email,
                scopes,
            });
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct UpdateStatus {
    pub(crate) latest_version: Option<String>,
    pub(crate) update_available: Option<bool>,
}

impl UpdateStatus {
    pub(crate) fn as_status_fields(&self) -> (Option<String>, Option<bool>) {
        (self.latest_version.clone(), self.update_available)
    }
}

const UPDATE_CHECK_BASE_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
const UPDATE_CHECK_JITTER: Duration = Duration::from_secs(2 * 60 * 60);

pub(crate) fn spawn_update_check_task(state: &DaemonState) -> tokio::task::JoinHandle<()> {
    let state = state.clone();
    tokio::spawn(async move {
        update_check_loop(state).await;
    })
}

pub(crate) fn should_spawn_update_check(no_update_check_env: Option<&str>) -> bool {
    no_update_check_env != Some("1")
}

async fn update_check_loop(state: DaemonState) {
    let jitter = random_jitter(UPDATE_CHECK_JITTER);
    let period = UPDATE_CHECK_BASE_PERIOD + jitter;

    loop {
        let mut ticker = interval_at(Instant::now() + period, period);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker.tick().await;

        poll_update_status_once(&state).await;
    }
}

fn random_jitter(max: Duration) -> Duration {
    let entropy = u64::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..8].try_into().unwrap());
    let max_nanos = max.as_nanos().max(1);
    let offset_nanos = (entropy as u128) % max_nanos;
    Duration::from_nanos(offset_nanos as u64)
}

async fn poll_update_status_once(state: &DaemonState) {
    let repo = valv_sync::update::DEFAULT_REPO;
    let running_version = env!("CARGO_PKG_VERSION");
    let outcome = valv_sync::update::resolve_latest_version(
        &state.client,
        repo,
        valv_sync::update::Component::Valvd,
        "VALVD_VERSION",
    )
    .await;
    let latest_version = outcome.as_ref().ok().cloned();
    {
        let mut update_status = state.update_status.lock().await;
        apply_update_check_outcome(&mut update_status, outcome, running_version);
    }

    let Some(latest_version) = latest_version else {
        return;
    };
    if !valv_sync::update::is_newer_version(&latest_version, running_version) {
        return;
    }
    if !crate::self_update::is_app_managed_install() {
        return;
    }
    let no_self_update = std::env::var("VALV_NO_SELF_UPDATE").ok();
    if !should_attempt_self_update(no_self_update.as_deref()) {
        return;
    }
    if let Err(error) = crate::self_update::attempt_self_update(&state.client, repo, &latest_version).await {
        tracing::warn!(error = %error, latest_version = %latest_version, "valvd self-update failed");
    }
}

fn should_attempt_self_update(no_self_update_env: Option<&str>) -> bool {
    no_self_update_env != Some("1")
}

fn apply_update_check_outcome(
    update_status: &mut UpdateStatus,
    outcome: anyhow::Result<String>,
    running_version: &str,
) {
    match outcome {
        Ok(latest_version) => {
            let update_available = valv_sync::update::is_newer_version(&latest_version, running_version);
            update_status.latest_version = Some(latest_version);
            update_status.update_available = Some(update_available);
        }
        Err(error) => {
            tracing::warn!(error = %error, "update-availability check failed");
        }
    }
}

const FS_WATCH_TASK_INDEX: usize = 2;

pub(crate) async fn spawn_tasks_for_mount(state: &DaemonState, mount: MountState) {
    let Some(token) = mount.effective_token(&state.config).map(str::to_owned) else {
        mount.watcher_alive.store(false, Ordering::Release);
        tracing::warn!(
            folder_id = %mount.folder_id,
            path = %mount.path,
            "mount has no usable credential; not spawning sync tasks"
        );
        return;
    };
    let (notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(32);
    let dirty_signal = DirtySignal::new();

    state
        .notify_senders
        .lock()
        .await
        .insert(mount.path.clone(), notify_tx.clone());
    spawn_boot_catchup(&mount, notify_tx.clone());

    // Spawned before sync_loop so sync_loop can take ownership of the
    // JoinHandle itself and await its completion; state.tasks below only
    // gets an abort handle, for unmount/remount/SIGTERM teardown.
    let fs_handle = spawn_fs_watch_handle(state, &mount, dirty_signal.clone(), token.clone());
    let fs_abort_handle = fs_handle.abort_handle();

    let sync_state = state.clone();
    let sync_mount = mount.clone();
    let sync_dirty_signal = dirty_signal.clone();
    let sync_handle = tokio::spawn(async move {
        sync_loop(sync_state, sync_mount, notify_rx, sync_dirty_signal, fs_handle).await;
    });

    let ws_backend_url = state.config.backend_url.clone();
    let ws_folder_id = mount.folder_id.clone();
    let ws_backend_health = state.backend_health.clone();
    let ws_pre_connect_jitter = random_jitter(MOUNT_STARTUP_JITTER_WINDOW);
    let ws_handle = tokio::spawn(async move {
        if let Err(error) = ws_push_loop(
            &ws_backend_url,
            &token,
            vec![ws_folder_id.clone()],
            notify_tx,
            ws_pre_connect_jitter,
        )
        .await
        {
            ws_backend_health.record_failure();
            tracing::error!(
                folder_id = %ws_folder_id,
                error = %error,
                "websocket task failed"
            );
        }
    });

    state.tasks.lock().await.insert(
        mount.path.clone(),
        vec![sync_handle.abort_handle(), ws_handle.abort_handle(), fs_abort_handle],
    );
}

// A few seconds, scaled for typical single-digit-to-low-tens mount counts on
// one machine. Sampled independently for the boot catch-up delay and the WS
// pre-connect delay, so the two triggers aren't guaranteed to land together.
const MOUNT_STARTUP_JITTER_WINDOW: Duration = Duration::from_secs(3);

// WS-independent startup catch-up: enqueues one synthetic notification per
// mount into its own sync_loop notify channel, staggered per mount so many
// persisted mounts don't all hit sync.db at once. Does not wait on or
// require ws_push_loop's connection, so a boot where WS can't connect but
// HTTPS still works still catches up.
fn spawn_boot_catchup(mount: &MountState, notify_tx: mpsc::Sender<WsPushNotification>) {
    let jitter = random_jitter(MOUNT_STARTUP_JITTER_WINDOW);
    let folder_id = mount.folder_id.clone();
    tokio::spawn(async move {
        sleep(jitter).await;
        let _ = notify_tx
            .send(WsPushNotification {
                folder_id,
                server_seq: 0,
            })
            .await;
    });
}

fn spawn_fs_watch_handle(
    state: &DaemonState,
    mount: &MountState,
    dirty_signal: DirtySignal,
    token: String,
) -> tokio::task::JoinHandle<()> {
    let paused = state.paused.clone();
    let fs_events_paused = state.fs_events_paused.clone();
    let db = state.db.clone();
    let client = state.client.clone();
    let backend_url = state.config.backend_url.clone();
    let device_name = state.config.device_name.clone();
    let fs_folder_id = mount.folder_id.clone();
    let watch_mount = WatchMount {
        path: PathBuf::from(&mount.path),
        folder_id: mount.folder_id.clone(),
        device_name,
        update_required: mount.update_required_flag.clone(),
        needs_reconcile: dirty_signal,
        sync_lock: mount.sync_lock.clone(),
    };
    tokio::spawn(async move {
        if let Err(error) = fs_watch_task(
            watch_mount,
            paused,
            fs_events_paused,
            db,
            client,
            backend_url,
            token,
        )
        .await
        {
            tracing::error!(
                folder_id = %fs_folder_id,
                error = %error,
                "filesystem watch task failed"
            );
        }
    })
}

const DIRTY_RECONCILE_DEBOUNCE: Duration = Duration::from_millis(1000);

// Distinct from DIRTY_RECONCILE_DEBOUNCE above even though both are ~1s: this
// one bounds the WS-notify select! arm (armed once on first arrival, never
// blocking the loop), the other is the Dirty wake's own inline sleep.
const WS_NOTIFY_DEBOUNCE: Duration = Duration::from_millis(1000);

const SYNC_POLL_FLOOR: Duration = Duration::from_secs(180);
const SYNC_POLL_JITTER: Duration = Duration::from_secs(90);

// A freshly (re)spawned fs_watch task that exits again before living this
// long is considered a quick death and backs off before the next respawn
// attempt; one that lives past this resets the backoff to zero.
const WATCHER_LIVENESS_THRESHOLD: Duration = Duration::from_secs(3);
const WATCHER_RESPAWN_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const WATCHER_RESPAWN_MAX_BACKOFF: Duration = Duration::from_secs(30);

fn next_watcher_backoff(previous: Duration, lived: Duration) -> Duration {
    if lived >= WATCHER_LIVENESS_THRESHOLD {
        Duration::ZERO
    } else if previous.is_zero() {
        WATCHER_RESPAWN_INITIAL_BACKOFF
    } else {
        (previous * 2).min(WATCHER_RESPAWN_MAX_BACKOFF)
    }
}

enum SyncLoopWake {
    Periodic,
    Dirty,
}

// The fs_watch task's JoinHandle, owned by sync_loop so it can select! on
// the task's completion instead of polling is_finished() on each wake.
struct WatcherState {
    handle: Option<tokio::task::JoinHandle<()>>,
    spawned_at: Instant,
    backoff: Duration,
    respawn_at: Option<Instant>,
}

// Polling a JoinHandle after it has already resolved panics, so this must
// only ever be awaited from a select! arm guarded by `watcher.handle.is_some()`.
async fn await_watcher_exit(
    handle: &mut Option<tokio::task::JoinHandle<()>>,
) -> Result<(), tokio::task::JoinError> {
    match handle {
        Some(inner) => inner.await,
        None => std::future::pending().await,
    }
}

async fn sync_loop(
    state: DaemonState,
    mount: MountState,
    mut notify_rx: mpsc::Receiver<WsPushNotification>,
    dirty_signal: DirtySignal,
    fs_watch_handle: tokio::task::JoinHandle<()>,
) {
    // interval_at (not interval) delays the first tick by a full period.
    // post_mount already runs tree_resync + materialize_mount_files before
    // this task is spawned, so an immediate first tick buys no correctness
    // benefit. The period itself is a jittered ~3-4.5 minute floor, computed
    // once per mount here (not per tick), so every mount's whole tick
    // sequence de-aligns from every other mount's rather than all firing
    // together at a fixed 30s cadence. WS heartbeat/catch-up (ws-push-client)
    // and this loop's own boot/resume catch-up now carry the real-time load;
    // this ticker is purely the correctness backstop for a WS failure this
    // change's other mechanisms fail to detect.
    let period = SYNC_POLL_FLOOR + random_jitter(SYNC_POLL_JITTER);
    let mut ticker = interval_at(Instant::now() + period, period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut watcher = WatcherState {
        handle: Some(fs_watch_handle),
        spawned_at: Instant::now(),
        backoff: Duration::ZERO,
        respawn_at: None,
    };

    // Armed on the first matching notification in a quiescent period; later
    // arrivals before it fires are coalesced, not extended (see WS_NOTIFY_DEBOUNCE).
    let mut notify_deadline: Option<Instant> = None;

    loop {
        let wake = tokio::select! {
            _ = ticker.tick() => SyncLoopWake::Periodic,
            notification = notify_rx.recv() => {
                let Some(notification) = notification else {
                    return;
                };
                if notification.folder_id != mount.folder_id {
                    continue;
                }
                // A synthetic catch-up notification (server_seq 0: boot, resume,
                // or WS (re)connect) means we may have missed *local* changes
                // during downtime or a pause, not only remote ones. Mark the
                // mount dirty so the ensuing wake runs a push-first reconcile:
                // an un-pushed local edit is then submitted (and conflict-forked
                // server-side) before the remote version lands. A pull-only
                // reconcile would overwrite and lose it. The Dirty wake's own
                // debounce coalesces a burst, so don't also arm the pull-only
                // deadline here (that would double the work). A real
                // remote-change push carries server_seq > 0 and stays on the
                // lighter pull-only path below.
                if notification.server_seq == 0 {
                    dirty_signal.mark();
                    continue;
                }
                notify_deadline.get_or_insert_with(|| Instant::now() + WS_NOTIFY_DEBOUNCE);
                continue;
            }
            // Absent a pending notification, notify_deadline is None and this
            // arm is disabled, so Instant::now() here is never actually
            // awaited (same pattern as the watcher respawn_at arm below).
            // Non-blocking by construction: unlike the Dirty wake's inline
            // sleep, this is its own select! arm, so the loop stays
            // responsive to the watcher-exit/respawn-backoff arms for the
            // whole debounce window instead of stalling on it.
            _ = sleep_until(notify_deadline.unwrap_or_else(Instant::now)), if notify_deadline.is_some() => {
                notify_deadline = None;
                SyncLoopWake::Periodic
            }
            _ = dirty_signal.notified() => SyncLoopWake::Dirty,
            result = await_watcher_exit(&mut watcher.handle), if watcher.handle.is_some() => {
                handle_watcher_exit(&state, &mount, &dirty_signal, &mut watcher, result).await;
                continue;
            }
            // Absent a pending backoff, respawn_at is None and this arm is
            // disabled, so Instant::now() here is never actually awaited.
            _ = sleep_until(watcher.respawn_at.unwrap_or_else(Instant::now)), if watcher.respawn_at.is_some() => {
                watcher.respawn_at = None;
                respawn_watcher(&state, &mount, &dirty_signal, &mut watcher).await;
                continue;
            }
        };

        if state.paused.load(Ordering::Acquire) {
            continue;
        }

        match wake {
            // Also reached when the WS-notify debounce deadline above fires;
            // it shares this exact dirty-then-reconcile-else-pull decision by
            // construction rather than a separately maintained copy of it.
            SyncLoopWake::Periodic => {
                if dirty_signal.take() {
                    reconcile_mount(&state, &mount).await;
                } else {
                    pull_mount_once(&state, &mount).await;
                }
            }
            SyncLoopWake::Dirty => {
                sleep(DIRTY_RECONCILE_DEBOUNCE).await;
                if dirty_signal.take() {
                    reconcile_mount(&state, &mount).await;
                }
            }
        }
    }
}

async fn reconcile_mount(state: &DaemonState, mount: &MountState) {
    tracing::debug!(folder_id = %mount.folder_id, "diag reconcile_mount (dirty-triggered full_sync)");
    // Dirty reconciles (catch-up or fs-watcher) push-first but pull with
    // Background scope so an un-pushed offline delete is not resurrected. An
    // online edit's path is present, so Background still materializes the
    // winner; only the offline-delete case diverges from a Full sweep.
    if let Err(error) =
        run_full_sync_mount(state.clone(), mount.clone(), MaterializeScope::Background).await
    {
        tracing::warn!(error = %error, folder_id = %mount.folder_id, "reconcile sync task panicked");
    }
}

async fn handle_watcher_exit(
    state: &DaemonState,
    mount: &MountState,
    dirty_signal: &DirtySignal,
    watcher: &mut WatcherState,
    result: Result<(), tokio::task::JoinError>,
) {
    if let Err(error) = result {
        if !error.is_cancelled() {
            tracing::warn!(
                folder_id = %mount.folder_id,
                error = %error,
                "fs_watch task panicked"
            );
        }
    }
    mount.watcher_alive.store(false, Ordering::Release);
    dirty_signal.mark();

    let lived = watcher.spawned_at.elapsed();
    watcher.backoff = next_watcher_backoff(watcher.backoff, lived);
    watcher.handle = None;

    if watcher.backoff.is_zero() {
        respawn_watcher(state, mount, dirty_signal, watcher).await;
    } else {
        watcher.respawn_at = Some(Instant::now() + watcher.backoff);
    }
}

// Mirrors the teardown-race invariant the old is_finished() poll embodied:
// only respawns while holding state.tasks with the mount's entry still
// present, and stores the fresh abort handle under that same lock, so a
// watcher exit racing DELETE /mount or a remount can't spawn an
// un-cancellable or duplicate watcher.
async fn respawn_watcher(
    state: &DaemonState,
    mount: &MountState,
    dirty_signal: &DirtySignal,
    watcher: &mut WatcherState,
) {
    let Some(token) = mount.effective_token(&state.config).map(str::to_owned) else {
        tracing::warn!(
            folder_id = %mount.folder_id,
            path = %mount.path,
            "fs_watch task died and mount has no usable credential; retrying on backoff"
        );
        watcher.backoff = next_watcher_backoff(watcher.backoff, Duration::ZERO);
        watcher.respawn_at = Some(Instant::now() + watcher.backoff);
        return;
    };

    let mut tasks = state.tasks.lock().await;
    let Some(handles) = tasks.get_mut(&mount.path) else {
        return;
    };
    let new_handle = spawn_fs_watch_handle(state, mount, dirty_signal.clone(), token);
    handles[FS_WATCH_TASK_INDEX] = new_handle.abort_handle();
    drop(tasks);

    watcher.handle = Some(new_handle);
    watcher.spawned_at = Instant::now();
    mount.watcher_alive.store(true, Ordering::Release);
    // Re-mark once the watcher is back up: the exit-time mark can be consumed
    // by a reconcile while the watcher is still down (across the backoff
    // window), so a change made then would otherwise never be picked up.
    dirty_signal.mark();
}

async fn pull_mount_once(state: &DaemonState, mount: &MountState) {
    let Some(token) = mount.effective_token(&state.config).map(str::to_owned) else {
        end_mount_sync(state, &mount.folder_id, Some("mount_has_no_credential".to_owned())).await;
        return;
    };
    let _sync_guard = mount.sync_lock.lock().await;
    tracing::debug!(folder_id = %mount.folder_id, "diag pull_mount_once begin (pull-only)");
    begin_mount_sync(state, &mount.folder_id).await;
    let (cursor_before, result) = {
        let mut conn = state.db.lock().await;
        let before = mount_store::get_cursor(&conn, &mount.folder_id).unwrap_or(0);
        let result = pull_delta(
            &state.client,
            &state.config.backend_url,
            &token,
            &mount.folder_id,
            &mut conn,
        )
        .await;
        (before, result)
    };
    let mut cursor_after = cursor_before;
    let error = match result {
        Ok((up_to_seq, pulled)) => {
            state.backend_health.record_success();
            clear_credential_rejected(state, &token).await;
            cursor_after = up_to_seq;
            let was_paused = pause_watchers(state);
            let cleanup_error = cleanup_deleted_mount_paths(state, mount)
                .await
                .err()
                .map(|err| err.to_string());
            let apply_error =
                apply_pulled_fs_changes(state, mount, pulled, MaterializeScope::Background, &HashMap::new())
                    .await
                    .err()
                    .map(|err| err.to_string());
            resume_watchers_after_debounce(state, was_paused).await;
            apply_error.or(cleanup_error)
        }
        Err(err) => {
            if is_update_required(&err).is_some() {
                state.backend_health.record_success();
                mark_mount_update_required(state, &mount.folder_id).await;
            } else if is_unauthenticated(&err) {
                state.backend_health.record_success();
                mark_credential_rejected(state, &token).await;
            } else {
                state.backend_health.record_failure();
            }
            Some(err.to_string())
        }
    };
    let succeeded = error.is_none();
    end_mount_sync(state, &mount.folder_id, error).await;
    // Only wake fp/watch waiters when the pull actually advanced the cursor. A
    // no-op catch-up/notify pull that advances nothing must not spuriously wake
    // a blocked watch into returning an unchanged cursor.
    if succeeded && cursor_after > cursor_before {
        mount.cursor_notify.notify_waiters();
    }
}

async fn full_sync_mount(
    state: &DaemonState,
    mount: &MountState,
    scope: MaterializeScope,
) -> SyncSummary {
    let mut summary = SyncSummary::default();
    let Some(token) = mount.effective_token(&state.config).map(str::to_owned) else {
        summary.errors += 1;
        end_mount_sync(state, &mount.folder_id, Some("mount_has_no_credential".to_owned())).await;
        return summary;
    };
    let _sync_guard = mount.sync_lock.lock().await;
    tracing::debug!(folder_id = %mount.folder_id, "diag full_sync_mount begin (push+pull)");
    begin_mount_sync(state, &mount.folder_id).await;
    if mount.update_required {
        mount.update_required_flag.store(true, Ordering::Release);
    }
    let cursor_before = {
        let conn = state.db.lock().await;
        mount_store::get_cursor(&conn, &mount.folder_id).unwrap_or(0)
    };

    let push_result = push_local_with_update_required(
        PathBuf::from(&mount.path).as_path(),
        &mount.folder_id,
        mount.scope_node_id.as_deref(),
        &state.db,
        &state.client,
        &state.config.backend_url,
        &token,
        &state.config.device_name,
        &mount.update_required_flag,
    )
    .await;
    let mut push_forbidden = false;
    let mut superseded_moves: HashMap<String, PathBuf> = HashMap::new();
    match push_result {
        Ok(push_summary) => {
            push_forbidden = push_summary.forbidden;
            merge_push_summary(&mut summary, &push_summary);
            clear_credential_rejected(state, &token).await;
            set_mount_pending_ops(
                state,
                &mount.folder_id,
                push_summary.creates_submitted + push_summary.versions_submitted,
            )
            .await;
            superseded_moves = push_summary.superseded_moves;
        }
        Err(error) => {
            if is_update_required(&error).is_some() {
                state.backend_health.record_success();
                mark_mount_update_required(state, &mount.folder_id).await;
                tracing::error!(
                    folder_id = %mount.folder_id,
                    error = %error,
                    "push_local halted because an update is required"
                );
                summary.errors += 1;
                set_mount_pending_ops(state, &mount.folder_id, 0).await;
                end_mount_sync(state, &mount.folder_id, Some(error.to_string())).await;
                return summary;
            }
            if is_unauthenticated(&error) {
                state.backend_health.record_success();
                mark_credential_rejected(state, &token).await;
            } else {
                state.backend_health.record_failure();
            }
            tracing::error!(
                folder_id = %mount.folder_id,
                error = %error,
                "push_local failed"
            );
            summary.errors += 1;
        }
    }

    let pull_result = {
        let mut conn = state.db.lock().await;
        pull_delta(
            &state.client,
            &state.config.backend_url,
            &token,
            &mount.folder_id,
            &mut conn,
        )
        .await
    };
    let mut cursor_after = cursor_before;
    let error = match pull_result {
        Ok((up_to_seq, pulled)) => {
            state.backend_health.record_success();
            clear_credential_rejected(state, &token).await;
            summary.pulled_ops = up_to_seq;
            cursor_after = up_to_seq;
            let was_paused = pause_watchers(state);
            let mut apply_error = None;
            if let Err(error) =
                apply_pulled_fs_changes(state, mount, pulled, scope, &superseded_moves).await
            {
                tracing::error!(
                    folder_id = %mount.folder_id,
                    error = %error,
                    "apply pulled filesystem changes failed"
                );
                summary.errors += 1;
                apply_error = Some(error.to_string());
            }
            let mut materialize_error = None;
            // A background/catch-up reconcile (Background scope) must not run the
            // full materialize sweep: it re-downloads every live mirror node
            // whose path is absent, resurrecting a file the user deleted locally
            // while offline (the delete can be live-but-superseded in the
            // mirror). Do only the tombstone cleanup the sweep would have done;
            // content materialization is left to the scope-gated apply above,
            // which withholds resurrection under Background. An explicit
            // `valv sync` (Full) keeps the full sweep.
            let materialize_result = match scope {
                MaterializeScope::Full => materialize_mount_files(state, mount).await,
                MaterializeScope::Background => cleanup_deleted_mount_paths(state, mount).await,
            };
            if let Err(error) = materialize_result {
                tracing::error!(
                    folder_id = %mount.folder_id,
                    error = %error,
                    "materialize files failed"
                );
                summary.errors += 1;
                materialize_error = Some(error.to_string());
            }
            resume_watchers_after_debounce(state, was_paused).await;
            apply_error.or(materialize_error)
        }
        Err(error) => {
            if is_update_required(&error).is_some() {
                state.backend_health.record_success();
                mark_mount_update_required(state, &mount.folder_id).await;
            } else if is_unauthenticated(&error) {
                state.backend_health.record_success();
                mark_credential_rejected(state, &token).await;
            } else {
                state.backend_health.record_failure();
            }
            summary.errors += 1;
            Some(error.to_string())
        }
    };

    let mount_error = error.clone().or_else(|| {
        push_forbidden
            .then(|| "a write to this mount was refused: insufficient permission".to_owned())
    });

    set_mount_pending_ops(state, &mount.folder_id, summary.errors).await;
    let pull_succeeded = error.is_none();
    end_mount_sync(state, &mount.folder_id, mount_error).await;
    // Wake fp/watch waiters only when the cursor actually advanced (a local
    // push is reflected back through the pull), so a no-op reconcile - e.g. a
    // dirty-marked catch-up on an unchanged mount - does not spuriously wake a
    // blocked watch into returning an unchanged cursor.
    if pull_succeeded && cursor_after > cursor_before {
        mount.cursor_notify.notify_waiters();
    }
    summary
}

fn pause_watchers(state: &DaemonState) -> bool {
    state.fs_events_paused.swap(true, Ordering::AcqRel)
}

async fn resume_watchers_after_debounce(state: &DaemonState, was_paused: bool) {
    if !was_paused {
        sleep(Duration::from_millis(250)).await;
        state.fs_events_paused.store(false, Ordering::Release);
    }
}

#[derive(Debug)]
struct MaterializeNode {
    node_id: String,
    parent_id: Option<String>,
    name: String,
    node_type: String,
    current_version_id: Option<String>,
    deleted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteVersion {
    version_id: String,
    content_hash: String,
    size_bytes: u64,
    manifest: Vec<ChunkRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaterializeScope {
    Background,
    Full,
}

pub fn node_abs_path(
    nodes_by_id: &HashMap<String, LocalNode>,
    mount_root: &Path,
    scope_node_id: Option<&str>,
    node_id: &str,
) -> Option<PathBuf> {
    let mut segments = Vec::new();
    let mut current_id = node_id;
    loop {
        let node = nodes_by_id.get(current_id)?;
        if scope_node_id == Some(node.node_id.as_str()) || node.parent_id.is_none() {
            break;
        }
        segments.push(node.name.clone());
        current_id = node.parent_id.as_deref()?;
    }

    let mut path = mount_root.to_path_buf();
    for segment in segments.into_iter().rev() {
        path.push(segment);
    }
    Some(path)
}

async fn apply_pulled_fs_changes(
    state: &DaemonState,
    mount: &MountState,
    pulled: Vec<PulledNode>,
    scope: MaterializeScope,
    superseded_moves: &HashMap<String, PathBuf>,
) -> anyhow::Result<()> {
    if pulled.is_empty() {
        return Ok(());
    }

    let nodes_by_id = load_nodes_by_id(state, &mount.folder_id).await?;
    let mount_root = PathBuf::from(&mount.path);
    for pulled_node in pulled {
        if let Err(error) = apply_pulled_fs_change(
            state,
            mount,
            &nodes_by_id,
            &mount_root,
            &pulled_node,
            scope,
            superseded_moves,
        )
        .await
        {
            tracing::error!(
                folder_id = %mount.folder_id,
                node_id = %pulled_node.node_id,
                op_type = %pulled_node.op_type,
                error = %error,
                "failed to apply pulled filesystem change"
            );
        }
    }
    Ok(())
}

async fn load_nodes_by_id(
    state: &DaemonState,
    folder_id: &str,
) -> anyhow::Result<HashMap<String, LocalNode>> {
    let conn = state.db.lock().await;
    let mut stmt = conn.prepare(
        "SELECT node_id, folder_id, parent_id, name, node_type, current_version_id, server_seq, deleted_at, pushed_size_bytes, pushed_mtime_nanos
         FROM nodes
         WHERE folder_id = ?1",
    )?;
    let rows = stmt
        .query_map([folder_id], |row| {
            Ok(LocalNode {
                node_id: row.get(0)?,
                folder_id: row.get(1)?,
                parent_id: row.get(2)?,
                name: row.get(3)?,
                node_type: row.get(4)?,
                current_version_id: row.get(5)?,
                server_seq: row.get(6)?,
                deleted_at: row.get(7)?,
                pushed_size_bytes: row.get(8)?,
                pushed_mtime_nanos: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|node| (node.node_id.clone(), node))
        .collect())
}

async fn apply_pulled_fs_change(
    state: &DaemonState,
    mount: &MountState,
    nodes_by_id: &HashMap<String, LocalNode>,
    mount_root: &Path,
    pulled: &PulledNode,
    scope: MaterializeScope,
    superseded_moves: &HashMap<String, PathBuf>,
) -> anyhow::Result<()> {
    match pulled.op_type.as_str() {
        "create" if pulled.node_type == "folder" => {
            if let Some(path) = node_abs_path(
                nodes_by_id,
                mount_root,
                mount.scope_node_id.as_deref(),
                &pulled.node_id,
            ) {
                fs::create_dir_all(path)?;
            }
        }
        "create" if pulled.node_type == "file" => {
            if let Some(version_id) = pulled.new_version_id.as_deref() {
                if should_materialize_canonical(
                    state,
                    mount,
                    nodes_by_id,
                    mount_root,
                    pulled,
                    scope,
                )
                .await?
                {
                    write_canonical_version(
                        state,
                        mount,
                        nodes_by_id,
                        mount_root,
                        pulled,
                        version_id,
                    )
                    .await?;
                }
            }
        }
        "rename" | "move" => {
            let Some(old_path) = pre_op_abs_path(nodes_by_id, mount_root, mount, pulled) else {
                return Ok(());
            };
            let Some(new_path) = node_abs_path(
                nodes_by_id,
                mount_root,
                mount.scope_node_id.as_deref(),
                &pulled.node_id,
            ) else {
                return Ok(());
            };
            if old_path == new_path {
                return Ok(());
            }
            if old_path.exists() {
                if let Some(parent) = new_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(old_path, new_path)?;
            } else if let Some(diverged_path) = superseded_moves
                .get(&pulled.node_id)
                .filter(|path| path.exists() && path.as_path() != new_path && !new_path.exists())
            {
                // This device locally renamed/moved this same node during the
                // window and lost the race (push_local's submit was
                // superseded), leaving the folder on disk under the losing
                // name. Reuse that folder for the winning name instead of
                // materializing a fresh copy and orphaning the loser (which a
                // later scan would push as a duplicate node). This also carries
                // any children created under it during the pause.
                if let Some(parent) = new_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(diverged_path, &new_path)?;
            } else {
                // old_path is already gone locally (e.g. a concurrent local
                // rename/delete raced this same node). We can't move it into
                // place, but the mirror DB now tracks this node at new_path
                // regardless, so materialize just this one node there.
                // Deliberately scoped to this single node rather than a full
                // materialize_mount_files sweep: that would also re-download
                // any other node the DB still tracks as live but which is
                // merely missing on disk because the user deleted it locally
                // and push_local (which would tell the server about that)
                // hasn't run yet.
                materialize_single_node(state, mount, nodes_by_id, &pulled.node_id, &new_path)
                    .await?;
            }
        }
        "new_version" if pulled.is_conflict_copy => {
            tracing::debug!(
                node_id = %pulled.node_id,
                actor = %pulled.actor_device_id,
                self_device = %state.config.device_id,
                version = ?pulled.new_version_id,
                "diag pull conflict-copy op received"
            );
            if pulled.actor_device_id == state.config.device_id {
                tracing::debug!(node_id = %pulled.node_id, "diag pull conflict-copy authored by self; skipping");
                return Ok(());
            }
            let Some(version_id) = pulled.new_version_id.as_deref() else {
                return Ok(());
            };
            let Some(canonical_path) = node_abs_path(
                nodes_by_id,
                mount_root,
                mount.scope_node_id.as_deref(),
                &pulled.node_id,
            ) else {
                return Ok(());
            };
            let date = pulled
                .applied_at
                .split('T')
                .next()
                .unwrap_or(&pulled.applied_at);
            let conflict_path = conflict_copy_path(&canonical_path, &pulled.actor_device_id, date)?;
            if let Some(parent) = conflict_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let bytes =
                download_and_store_version(state, mount, &pulled.node_id, version_id).await?;
            tracing::debug!(
                node_id = %pulled.node_id,
                version = %version_id,
                dst = %conflict_path.display(),
                bytes = bytes.len(),
                "diag pull materialized conflict copy from download"
            );
            fs::write(conflict_path, bytes)?;
        }
        "new_version" => {
            if pulled.old_version_id == pulled.new_version_id {
                return Ok(());
            }
            let Some(version_id) = pulled.new_version_id.as_deref() else {
                return Ok(());
            };
            if should_materialize_canonical(state, mount, nodes_by_id, mount_root, pulled, scope)
                .await?
            {
                write_canonical_version(state, mount, nodes_by_id, mount_root, pulled, version_id)
                    .await?;
            }
        }
        "delete" => {}
        _ => {}
    }
    Ok(())
}

/// Materializes a single tracked node at `path`, downloading file content if
/// needed. Unlike `materialize_mount_files`, this touches only the given
/// node rather than sweeping the whole tree, so it can safely be used from
/// background pull handling without risking resurrecting other nodes the
/// user has deleted locally but not yet pushed.
async fn materialize_single_node(
    state: &DaemonState,
    mount: &MountState,
    nodes_by_id: &HashMap<String, LocalNode>,
    node_id: &str,
    path: &Path,
) -> anyhow::Result<()> {
    let Some(node) = nodes_by_id.get(node_id) else {
        return Ok(());
    };
    if node.node_type == "folder" {
        fs::create_dir_all(path)?;
        return Ok(());
    }
    let Some(version_id) = node.current_version_id.as_deref() else {
        return Ok(());
    };
    let already_materialized = {
        let conn = state.db.lock().await;
        versions::has_materialized_content_for_version(&conn, version_id)? && path.exists()
    };
    if already_materialized {
        return Ok(());
    }
    materialize_version(state, mount, node_id, version_id, path).await?;
    Ok(())
}

fn pre_op_abs_path(
    nodes_by_id: &HashMap<String, LocalNode>,
    mount_root: &Path,
    mount: &MountState,
    pulled: &PulledNode,
) -> Option<PathBuf> {
    let mut old_nodes = nodes_by_id.clone();
    let node = old_nodes.get_mut(&pulled.node_id)?;
    if let Some(old_name) = &pulled.old_name {
        node.name = old_name.clone();
    }
    node.parent_id = pulled.old_parent_id.clone();
    node_abs_path(
        &old_nodes,
        mount_root,
        mount.scope_node_id.as_deref(),
        &pulled.node_id,
    )
}

async fn write_canonical_version(
    state: &DaemonState,
    mount: &MountState,
    nodes_by_id: &HashMap<String, LocalNode>,
    mount_root: &Path,
    pulled: &PulledNode,
    version_id: &str,
) -> anyhow::Result<()> {
    let Some(path) = node_abs_path(
        nodes_by_id,
        mount_root,
        mount.scope_node_id.as_deref(),
        &pulled.node_id,
    ) else {
        return Ok(());
    };
    materialize_version(state, mount, &pulled.node_id, version_id, &path).await?;
    Ok(())
}

async fn download_and_store_version(
    state: &DaemonState,
    mount: &MountState,
    node_id: &str,
    version_id: &str,
) -> anyhow::Result<Vec<u8>> {
    let (version, bytes) = download_verified_version(state, mount, node_id, version_id).await?;
    persist_version_metadata(state, mount, node_id, &version).await?;
    Ok(bytes)
}

async fn materialize_version(
    state: &DaemonState,
    mount: &MountState,
    node_id: &str,
    version_id: &str,
    path: &Path,
) -> anyhow::Result<()> {
    let (version, bytes) = download_verified_version(state, mount, node_id, version_id).await?;
    write_atomic(path, &bytes)?;
    persist_materialized_version(state, mount, node_id, &version).await?;
    Ok(())
}

async fn download_verified_version(
    state: &DaemonState,
    mount: &MountState,
    node_id: &str,
    version_id: &str,
) -> anyhow::Result<(RemoteVersion, Vec<u8>)> {
    let version = fetch_remote_version(state, mount, node_id, version_id).await?;
    let token = require_token(mount.effective_token(&state.config))?;
    let bytes = download_chunks(
        &state.client,
        &state.config.backend_url,
        &token,
        &version.manifest,
    )
    .await?;
    let actual_hash = manifest_content_hash(&version.manifest);
    if actual_hash != version.content_hash {
        return Err(anyhow::anyhow!(
            "content hash mismatch for version {}: expected {}, got {}",
            version.version_id,
            version.content_hash,
            actual_hash
        ));
    }
    if bytes.len() as u64 != version.size_bytes {
        return Err(anyhow::anyhow!(
            "content size mismatch for version {}: expected {}, got {}",
            version.version_id,
            version.size_bytes,
            bytes.len()
        ));
    }
    Ok((version, bytes.to_vec()))
}

async fn persist_materialized_version(
    state: &DaemonState,
    mount: &MountState,
    node_id: &str,
    version: &RemoteVersion,
) -> anyhow::Result<()> {
    persist_version_metadata(state, mount, node_id, version).await?;
    let conn = state.db.lock().await;
    versions::mark_content_materialized(&conn, &version.version_id)?;
    Ok(())
}

async fn persist_version_metadata(
    state: &DaemonState,
    mount: &MountState,
    node_id: &str,
    version: &RemoteVersion,
) -> anyhow::Result<()> {
    {
        let conn = state.db.lock().await;
        upsert_version(
            &conn,
            &LocalVersion {
                version_id: version.version_id.clone(),
                node_id: node_id.to_owned(),
                folder_id: mount.folder_id.clone(),
                content_hash: version.content_hash.clone(),
                size_bytes: version.size_bytes,
                manifest_json: serde_json::to_string(&version.manifest)?,
            },
        )?;
        for chunk in &version.manifest {
            chunk_store::mark_uploaded(&conn, &chunk.chunk_hash, chunk.length)?;
        }
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("path has no valid file name: {}", path.display()))?;
    let temp_path = path.with_file_name(format!(
        ".{file_name}.valv-tmp-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let write_result = (|| -> anyhow::Result<()> {
        let mut file = fs::File::create(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    write_result
}

async fn should_materialize_canonical(
    state: &DaemonState,
    mount: &MountState,
    nodes_by_id: &HashMap<String, LocalNode>,
    mount_root: &Path,
    pulled: &PulledNode,
    scope: MaterializeScope,
) -> anyhow::Result<bool> {
    if scope == MaterializeScope::Full {
        return Ok(true);
    }
    let Some(path) = node_abs_path(
        nodes_by_id,
        mount_root,
        mount.scope_node_id.as_deref(),
        &pulled.node_id,
    ) else {
        return Ok(false);
    };
    let has_prior_materialized_content = {
        let conn = state.db.lock().await;
        versions::has_materialized_content_for_node(&conn, &pulled.node_id)?
    };
    Ok(!(has_prior_materialized_content && !path.exists()))
}

fn conflict_copy_path(
    original_path: &Path,
    device_name: &str,
    date: &str,
) -> anyhow::Result<PathBuf> {
    let file_name = original_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!("path has no valid file name: {}", original_path.display())
        })?;
    let conflict_name = match original_path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => {
            let stem = original_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| {
                    anyhow::anyhow!("path has no valid file stem: {}", original_path.display())
                })?;
            format!("{stem} (conflicted copy, {device_name}, {date}).{ext}")
        }
        None => format!("{file_name} (conflicted copy, {device_name}, {date})"),
    };
    disambiguate_conflict_path(original_path.with_file_name(conflict_name))
}

fn disambiguate_conflict_path(desired: PathBuf) -> anyhow::Result<PathBuf> {
    if !desired.exists() {
        return Ok(desired);
    }
    let parent = desired.parent().map(Path::to_path_buf);
    let stem = desired
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow::anyhow!("path has no valid file stem: {}", desired.display()))?;
    let extension = desired.extension().and_then(|ext| ext.to_str());
    for counter in 2..=20 {
        let file_name = match extension {
            Some(ext) => format!("{stem} ({counter}).{ext}"),
            None => format!("{stem} ({counter})"),
        };
        let candidate = parent.as_ref().map_or_else(
            || PathBuf::from(&file_name),
            |parent| parent.join(&file_name),
        );
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow::anyhow!(
        "conflict copy path exhausted disambiguation attempts for {}",
        desired.display()
    ))
}

pub(crate) async fn materialize_mount_files(
    state: &DaemonState,
    mount: &MountState,
) -> anyhow::Result<()> {
    cleanup_deleted_mount_paths(state, mount).await?;

    let nodes = {
        let conn = state.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT node_id, parent_id, name, node_type, current_version_id
             FROM nodes
             WHERE folder_id = ?1 AND deleted_at IS NULL
             ORDER BY parent_id IS NOT NULL, name ASC",
        )?;
        let rows = stmt
            .query_map([&mount.folder_id], |row| {
                Ok(MaterializeNode {
                    node_id: row.get(0)?,
                    parent_id: row.get(1)?,
                    name: row.get(2)?,
                    node_type: row.get(3)?,
                    current_version_id: row.get(4)?,
                    deleted_at: None,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let Some(root) = nodes.iter().find(|node| node.parent_id.is_none()) else {
        return Ok(());
    };

    let mut children_map: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, node) in nodes.iter().enumerate() {
        if let Some(parent_id) = &node.parent_id {
            children_map.entry(parent_id.clone()).or_default().push(idx);
        }
    }

    let mut paths = HashMap::from([(root.node_id.clone(), PathBuf::from(&mount.path))]);
    fs::create_dir_all(&mount.path)?;

    let mut queue = VecDeque::new();
    queue.push_back(root.node_id.clone());

    while let Some(parent_id) = queue.pop_front() {
        let Some(child_indices) = children_map.get(&parent_id).cloned() else {
            continue;
        };
        for idx in child_indices {
            let node = &nodes[idx];
            let parent_path = paths[&parent_id].clone();
            let path = parent_path.join(&node.name);
            paths.insert(node.node_id.clone(), path.clone());
            queue.push_back(node.node_id.clone());

            if node.node_type == "folder" {
                fs::create_dir_all(&path)?;
                continue;
            }

            let Some(version_id) = node.current_version_id.as_deref() else {
                continue;
            };
            let already_materialized = {
                let conn = state.db.lock().await;
                versions::has_materialized_content_for_version(&conn, version_id)? && path.exists()
            };
            if already_materialized {
                continue;
            }
            materialize_version(state, mount, &node.node_id, version_id, &path).await?;
        }
    }

    Ok(())
}

async fn cleanup_deleted_mount_paths(
    state: &DaemonState,
    mount: &MountState,
) -> anyhow::Result<()> {
    let nodes = {
        let conn = state.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT node_id, parent_id, name, node_type, current_version_id, deleted_at
             FROM nodes
             WHERE folder_id = ?1",
        )?;
        let rows = stmt
            .query_map([&mount.folder_id], |row| {
                Ok(MaterializeNode {
                    node_id: row.get(0)?,
                    parent_id: row.get(1)?,
                    name: row.get(2)?,
                    node_type: row.get(3)?,
                    current_version_id: row.get(4)?,
                    deleted_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let Some(root) = nodes.iter().find(|node| node.parent_id.is_none()) else {
        return Ok(());
    };

    let mut children_map: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, node) in nodes.iter().enumerate() {
        if let Some(parent_id) = &node.parent_id {
            children_map.entry(parent_id.clone()).or_default().push(idx);
        }
    }

    let mut paths = HashMap::from([(root.node_id.clone(), PathBuf::from(&mount.path))]);
    let mut queue = VecDeque::new();
    let mut deleted_paths = Vec::new();
    queue.push_back(root.node_id.clone());

    while let Some(parent_id) = queue.pop_front() {
        let Some(child_indices) = children_map.get(&parent_id).cloned() else {
            continue;
        };
        for idx in child_indices {
            let node = &nodes[idx];
            let parent_path = paths[&parent_id].clone();
            let path = parent_path.join(&node.name);
            paths.insert(node.node_id.clone(), path.clone());
            queue.push_back(node.node_id.clone());

            if node.deleted_at.is_some() {
                deleted_paths.push((path, node.node_type.clone()));
            }
        }
    }

    deleted_paths.sort_by_key(|(path, _)| path.components().count());
    for (path, node_type) in deleted_paths.into_iter().rev() {
        let result = if node_type == "folder" {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if let Err(error) = result {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
        }
    }

    Ok(())
}

async fn fetch_remote_version(
    state: &DaemonState,
    mount: &MountState,
    node_id: &str,
    version_id: &str,
) -> anyhow::Result<RemoteVersion> {
    let token = require_token(mount.effective_token(&state.config))?;
    let versions = state
        .client
        .get(format!(
            "{}/folders/{}/versions/{}",
            valv_sync::api_base(&state.config.backend_url),
            mount.folder_id,
            node_id,
        ))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<RemoteVersion>>()
        .await?;
    versions
        .into_iter()
        .find(|version| version.version_id == version_id)
        .ok_or_else(|| anyhow::anyhow!("version {version_id} not found for node {node_id}"))
}

async fn run_full_sync_mount(
    state: DaemonState,
    mount: MountState,
    scope: MaterializeScope,
) -> Result<SyncSummary, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(full_sync_mount(&state, &mount, scope))
    })
    .await
}

pub(crate) async fn cancel_mount_tasks(state: &DaemonState) {
    for (_, tasks) in state.tasks.lock().await.drain() {
        for task in tasks {
            task.abort();
        }
    }
    state.notify_senders.lock().await.clear();
}

pub(crate) async fn cancel_tasks_for_mount(state: &DaemonState, path: &str) {
    if let Some(tasks) = state.tasks.lock().await.remove(path) {
        for task in tasks {
            task.abort();
        }
    }
    state.notify_senders.lock().await.remove(path);
}

async fn begin_mount_sync(state: &DaemonState, folder_id: &str) {
    let mut mounts = state.mounts.lock().await;
    if let Some(mount) = mounts.iter_mut().find(|mount| mount.folder_id == folder_id) {
        mount.active_syncs = mount.active_syncs.saturating_add(1);
        if !mount.update_required && !mount.update_required_flag.load(Ordering::Acquire) {
            mount.error = None;
        }
    }
}

pub(crate) async fn end_mount_sync(state: &DaemonState, folder_id: &str, error: Option<String>) {
    let mut mounts = state.mounts.lock().await;
    if let Some(mount) = mounts.iter_mut().find(|mount| mount.folder_id == folder_id) {
        mount.active_syncs = mount.active_syncs.saturating_sub(1);
        mount.error = error;
        if mount.active_syncs == 0 && mount.error.is_none() {
            mount.last_synced_at = Some(Utc::now().to_rfc3339());
        }
    }
}

pub(crate) async fn mark_mount_update_required(state: &DaemonState, folder_id: &str) {
    let mut mounts = state.mounts.lock().await;
    if let Some(mount) = mounts.iter_mut().find(|mount| mount.folder_id == folder_id) {
        mount.update_required = true;
        mount.update_required_flag.store(true, Ordering::Release);
    }
}

async fn set_mount_pending_ops(state: &DaemonState, folder_id: &str, pending_ops: u64) {
    let mut mounts = state.mounts.lock().await;
    if let Some(mount) = mounts.iter_mut().find(|mount| mount.folder_id == folder_id) {
        mount.pending_ops = pending_ops;
    }
}

fn merge_push_summary(summary: &mut SyncSummary, push_summary: &PushSummary) {
    summary.creates_submitted += push_summary.creates_submitted;
    summary.versions_submitted += push_summary.versions_submitted;
    summary.deletes_submitted += push_summary.deletes_submitted;
    summary.errors += push_summary.errors;
}

fn merge_sync_summary(summary: &mut SyncSummary, mount_summary: SyncSummary) {
    summary.creates_submitted += mount_summary.creates_submitted;
    summary.versions_submitted += mount_summary.versions_submitted;
    summary.deletes_submitted += mount_summary.deletes_submitted;
    summary.pulled_ops += mount_summary.pulled_ops;
    summary.errors += mount_summary.errors;
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{atomic::AtomicBool, Arc},
    };

    use rusqlite::Connection;
    use sha2::{Digest, Sha256};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Mutex,
        time::{timeout, Duration},
    };
    use valv_sync::{
        persistence::{
            mounts,
            nodes::{self, LocalNode},
        },
        protocol::ipc::{AccountStatus, SyncRequest},
    };

    use crate::config::DaemonConfig;

    use super::*;

    fn test_state() -> DaemonState {
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
            config: DaemonConfig {
                backend_url: "http://127.0.0.1:1".to_owned(),
                device_id: "device-1".to_owned(),
                device_token: Some("token".to_owned()),
                device_name: "Test Device".to_owned(),
                mounts: Vec::new(),
            },
        }
    }

    async fn test_state_with_backend(
        version_id: &str,
        content_hash: String,
        size_bytes: u64,
        manifest: Vec<ChunkRef>,
        chunks: HashMap<String, Vec<u8>>,
    ) -> DaemonState {
        let mut state = test_state();
        state.config.backend_url =
            materialization_server(version_id, content_hash, size_bytes, manifest, chunks).await;
        state
    }

    fn test_chunk_hash(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    async fn materialization_server(
        version_id: &str,
        content_hash: String,
        size_bytes: u64,
        manifest: Vec<ChunkRef>,
        chunks: HashMap<String, Vec<u8>>,
    ) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let version_id = version_id.to_owned();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                let bytes_read = stream.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..bytes_read]);
                let first_line = request.lines().next().unwrap_or_default();
                let path = first_line.split_whitespace().nth(1).unwrap_or("/");

                if path == "/api/folders/folder-1/versions/n1" {
                    let body = serde_json::json!([
                        {
                            "version_id": version_id,
                            "content_hash": content_hash,
                            "size_bytes": size_bytes,
                            "manifest": manifest,
                        }
                    ])
                    .to_string();
                    write_test_response(&mut stream, "application/json", body.as_bytes()).await;
                    continue;
                }

                if path == "/api/objects/batch" {
                    let objects = manifest
                        .iter()
                        .map(|chunk| {
                            serde_json::json!({
                                "oid": chunk.chunk_hash,
                                "size": chunk.length,
                                "actions": {
                                    "download": {
                                        "href": format!("http://{addr}/chunks/{}", chunk.chunk_hash)
                                    }
                                }
                            })
                        })
                        .collect::<Vec<_>>();
                    let body = serde_json::json!({
                        "transfer": "basic",
                        "objects": objects,
                    })
                    .to_string();
                    write_test_response(&mut stream, "application/json", body.as_bytes()).await;
                    continue;
                }

                if let Some(chunk_hash) = path.strip_prefix("/chunks/") {
                    if let Some(bytes) = chunks.get(chunk_hash) {
                        write_test_response(&mut stream, "application/octet-stream", bytes).await;
                        continue;
                    }
                }

                write_test_status(&mut stream, "404 Not Found", b"not found").await;
            }
        });
        format!("http://{addr}")
    }

    async fn write_test_response(
        stream: &mut tokio::net::TcpStream,
        content_type: &str,
        body: &[u8],
    ) {
        write_test_status_with_type(stream, "200 OK", content_type, body).await;
    }

    async fn write_test_status(stream: &mut tokio::net::TcpStream, status_line: &str, body: &[u8]) {
        write_test_status_with_type(stream, status_line, "text/plain", body).await;
    }

    async fn write_test_status_with_type(
        stream: &mut tokio::net::TcpStream,
        status_line: &str,
        content_type: &str,
        body: &[u8],
    ) {
        let header = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
    }

    async fn account_usage_server(status_line: &str, body: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let status_line = status_line.to_owned();
        let body = body.to_owned();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 2048];
            let _ = stream.read(&mut buffer).await.unwrap();
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn post_sync_with_no_mounts_returns_empty_summary() {
        let response = post_sync(State(test_state()), Json(SyncRequest { folder_id: None }))
            .await
            .unwrap();

        assert_eq!(response.0, SyncSummary::default());
    }

    #[tokio::test]
    async fn post_sync_rejects_empty_folder_id() {
        let error = post_sync(
            State(test_state()),
            Json(SyncRequest {
                folder_id: Some(String::new()),
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DaemonError::BadRequest(_)));
    }

    #[tokio::test]
    async fn account_status_poll_200_populates_cache() {
        let mut state = test_state();
        state.config.backend_url = account_usage_server(
            "200 OK",
            r#"{"plan":"Pro","status":"active","usage_bytes":123,"quota_bytes":456,"current_period_end":"2026-07-15T00:00:00Z"}"#,
        )
        .await;

        let outcome = poll_account_status_once(&state).await;
        let account = state.account.lock().await.clone().unwrap();

        assert_eq!(outcome, AccountPollOutcome::Updated);
        assert_eq!(account.plan.as_deref(), Some("Pro"));
        assert_eq!(account.status, "active");
        assert_eq!(account.usage_bytes, 123);
        assert_eq!(account.quota_bytes, Some(456));
    }

    #[test]
    fn update_check_success_populates_cache() {
        let mut update_status = UpdateStatus::default();

        apply_update_check_outcome(&mut update_status, Ok("9.9.9".to_owned()), "0.1.0");

        assert_eq!(update_status.latest_version.as_deref(), Some("9.9.9"));
        assert_eq!(update_status.update_available, Some(true));
    }

    #[test]
    fn update_check_success_with_no_newer_release_reports_unavailable() {
        let mut update_status = UpdateStatus::default();

        apply_update_check_outcome(&mut update_status, Ok("0.1.0".to_owned()), "0.1.0");

        assert_eq!(update_status.latest_version.as_deref(), Some("0.1.0"));
        assert_eq!(update_status.update_available, Some(false));
    }

    #[test]
    fn update_check_failure_after_prior_success_preserves_cache() {
        let mut update_status = UpdateStatus::default();
        apply_update_check_outcome(&mut update_status, Ok("9.9.9".to_owned()), "0.1.0");

        apply_update_check_outcome(
            &mut update_status,
            Err(anyhow::anyhow!("network error")),
            "0.1.0",
        );

        assert_eq!(update_status.latest_version.as_deref(), Some("9.9.9"));
        assert_eq!(update_status.update_available, Some(true));
    }

    #[test]
    fn update_check_failure_before_any_success_stays_absent() {
        let mut update_status = UpdateStatus::default();

        apply_update_check_outcome(
            &mut update_status,
            Err(anyhow::anyhow!("network error")),
            "0.1.0",
        );

        assert!(update_status.latest_version.is_none());
        assert!(update_status.update_available.is_none());
    }

    #[tokio::test]
    async fn poll_update_status_once_uses_pinned_version_without_network() {
        std::env::set_var("VALVD_VERSION", "v42.0.0");
        std::env::set_var("VALV_NO_SELF_UPDATE", "1");
        let state = test_state();

        poll_update_status_once(&state).await;

        std::env::remove_var("VALVD_VERSION");
        std::env::remove_var("VALV_NO_SELF_UPDATE");
        let (latest_version, update_available) = state.update_status.lock().await.as_status_fields();
        assert_eq!(latest_version.as_deref(), Some("42.0.0"));
        assert_eq!(update_available, Some(true));
    }

    #[test]
    fn should_spawn_update_check_only_suppresses_on_exactly_one() {
        assert!(should_spawn_update_check(None));
        assert!(should_spawn_update_check(Some("0")));
        assert!(should_spawn_update_check(Some("true")));
        assert!(!should_spawn_update_check(Some("1")));
    }

    #[test]
    fn should_attempt_self_update_only_suppresses_on_exactly_one() {
        assert!(should_attempt_self_update(None));
        assert!(should_attempt_self_update(Some("0")));
        assert!(should_attempt_self_update(Some("true")));
        assert!(!should_attempt_self_update(Some("1")));
    }

    #[test]
    fn random_jitter_stays_within_bounds() {
        let max = Duration::from_secs(100);
        for _ in 0..1000 {
            assert!(random_jitter(max) < max);
        }
    }

    #[test]
    fn random_jitter_of_zero_max_is_zero() {
        assert_eq!(random_jitter(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn sync_poll_floor_period_stays_within_the_jittered_three_to_four_and_a_half_minute_range() {
        for _ in 0..1000 {
            let period = SYNC_POLL_FLOOR + random_jitter(SYNC_POLL_JITTER);
            assert!(period >= SYNC_POLL_FLOOR);
            assert!(period < SYNC_POLL_FLOOR + SYNC_POLL_JITTER);
        }
        assert_eq!(SYNC_POLL_FLOOR, Duration::from_secs(180));
        assert_eq!(SYNC_POLL_FLOOR + SYNC_POLL_JITTER, Duration::from_secs(270));
    }

    #[test]
    fn boot_catchup_and_ws_pre_connect_jitter_are_independently_sampled() {
        // spawn_tasks_for_mount calls random_jitter(MOUNT_STARTUP_JITTER_WINDOW)
        // separately for the boot catch-up delay and the ws_push_loop
        // pre-connect delay; this guards against a future refactor
        // accidentally computing one jitter value and reusing it for both.
        let mut saw_different_pair = false;
        for _ in 0..200 {
            let boot = random_jitter(MOUNT_STARTUP_JITTER_WINDOW);
            let ws = random_jitter(MOUNT_STARTUP_JITTER_WINDOW);
            if boot != ws {
                saw_different_pair = true;
                break;
            }
        }
        assert!(
            saw_different_pair,
            "the boot enqueue and WS pre-connect jitter must be independent random samples"
        );
    }

    #[test]
    fn pulled_conflict_copy_path_gets_counter_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.md");
        let base = path.with_file_name("report (conflicted copy, other-device, 2026-07-08).md");
        fs::write(&base, b"first").unwrap();

        let conflict = conflict_copy_path(&path, "other-device", "2026-07-08").unwrap();

        assert_eq!(
            conflict.file_name().and_then(|name| name.to_str()),
            Some("report (conflicted copy, other-device, 2026-07-08) (2).md")
        );
        assert_eq!(fs::read(base).unwrap(), b"first");
    }

    #[test]
    fn write_atomic_replaces_target_and_cleans_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, b"old").unwrap();

        write_atomic(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        let temp_leftovers = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.contains(".valv-tmp-"))
            })
            .count();
        assert_eq!(temp_leftovers, 0);
    }

    #[tokio::test]
    async fn materialize_version_accepts_manifest_content_hash() {
        let content = b"hello manifest".to_vec();
        let chunk_hash = test_chunk_hash(&content);
        let manifest = vec![ChunkRef {
            chunk_hash: chunk_hash.clone(),
            offset: 0,
            length: content.len() as u64,
        }];
        let state = test_state_with_backend(
            "v1",
            manifest_content_hash(&manifest),
            content.len() as u64,
            manifest.clone(),
            HashMap::from([(chunk_hash, content.clone())]),
        )
        .await;
        let mount = test_mount("/tmp/unused");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");

        materialize_version(&state, &mount, "n1", "v1", &path)
            .await
            .unwrap();

        assert_eq!(fs::read(&path).unwrap(), content);
        let conn = state.db.lock().await;
        assert!(versions::get_version(&conn, "v1").unwrap().is_some());
        assert!(versions::has_materialized_content_for_node(&conn, "n1").unwrap());
    }

    #[tokio::test]
    async fn materialize_version_rejects_manifest_content_hash_mismatch_without_writes() {
        let content = b"hello manifest".to_vec();
        let chunk_hash = test_chunk_hash(&content);
        let manifest = vec![ChunkRef {
            chunk_hash: chunk_hash.clone(),
            offset: 0,
            length: content.len() as u64,
        }];
        let different_manifest = vec![ChunkRef {
            chunk_hash: test_chunk_hash(b"different chunk"),
            offset: 0,
            length: content.len() as u64,
        }];
        let state = test_state_with_backend(
            "v1",
            manifest_content_hash(&different_manifest),
            content.len() as u64,
            manifest,
            HashMap::from([(chunk_hash, content)]),
        )
        .await;
        let mount = test_mount("/tmp/unused");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");

        let error = materialize_version(&state, &mount, "n1", "v1", &path)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("content hash mismatch"));
        assert!(!path.exists());
        let conn = state.db.lock().await;
        assert!(versions::get_version(&conn, "v1").unwrap().is_none());
    }

    #[tokio::test]
    async fn materialize_version_rejects_size_mismatch_without_writes() {
        let content = b"hello manifest".to_vec();
        let chunk_hash = test_chunk_hash(&content);
        let manifest = vec![ChunkRef {
            chunk_hash: chunk_hash.clone(),
            offset: 0,
            length: content.len() as u64,
        }];
        let state = test_state_with_backend(
            "v1",
            manifest_content_hash(&manifest),
            (content.len() + 1) as u64,
            manifest,
            HashMap::from([(chunk_hash, content)]),
        )
        .await;
        let mount = test_mount("/tmp/unused");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");

        let error = materialize_version(&state, &mount, "n1", "v1", &path)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("content size mismatch"));
        assert!(!path.exists());
        let conn = state.db.lock().await;
        assert!(versions::get_version(&conn, "v1").unwrap().is_none());
    }

    #[tokio::test]
    async fn materialize_version_inserts_versions_row_only_after_rename_succeeds() {
        let content = b"hello ordering".to_vec();
        let chunk_hash = test_chunk_hash(&content);
        let manifest = vec![ChunkRef {
            chunk_hash: chunk_hash.clone(),
            offset: 0,
            length: content.len() as u64,
        }];
        let state = test_state_with_backend(
            "v1",
            manifest_content_hash(&manifest),
            content.len() as u64,
            manifest,
            HashMap::from([(chunk_hash, content)]),
        )
        .await;
        let mount = test_mount("/tmp/unused");
        let dir = tempfile::tempdir().unwrap();
        // The target path is an existing directory, so verification (hash/size)
        // succeeds and the temp file is written, but the final `fs::rename` onto
        // `path` fails (can't rename a file over a directory). This proves the
        // `versions` row is only inserted after the rename step actually
        // succeeds - a failure at (or before) rename must leave no row behind.
        let path = dir.path().join("file.txt");
        fs::create_dir_all(&path).unwrap();

        let error = materialize_version(&state, &mount, "n1", "v1", &path)
            .await
            .unwrap_err();

        assert!(path.is_dir(), "rename must not have replaced the directory");
        let conn = state.db.lock().await;
        assert!(
            versions::get_version(&conn, "v1").unwrap().is_none(),
            "no versions row should exist when rename failed: {error}"
        );
        let temp_leftovers = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.contains(".valv-tmp-"))
            })
            .count();
        assert_eq!(temp_leftovers, 0, "failed rename must clean up its temp file");
    }

    #[tokio::test]
    async fn materialize_mount_files_reattempts_download_for_metadata_only_version_row() {
        let content = b"real materialized content".to_vec();
        let chunk_hash = test_chunk_hash(&content);
        let manifest = vec![ChunkRef {
            chunk_hash: chunk_hash.clone(),
            offset: 0,
            length: content.len() as u64,
        }];
        let state = test_state_with_backend(
            "v1",
            manifest_content_hash(&manifest),
            content.len() as u64,
            manifest.clone(),
            HashMap::from([(chunk_hash, content.clone())]),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mount = test_mount(dir.path().to_string_lossy().as_ref());
        // A stale/partial leftover file already sits at the target path, and a
        // metadata-only `versions` row (content_materialized_at still NULL,
        // e.g. mirrored in from an op-log entry) already exists for `v1`. Bare
        // row + path existence must NOT be treated as "already materialized".
        let stale_path = dir.path().join("file.txt");
        fs::write(&stale_path, b"stale leftover, not the real content").unwrap();
        {
            let conn = state.db.lock().await;
            mounts::upsert_mount(&conn, &mount.path, &mount.folder_id, None, None, None, true)
                .unwrap();
            nodes::upsert_node(&conn, &root_node()).unwrap();
            nodes::upsert_node(
                &conn,
                &LocalNode {
                    node_id: "n1".into(),
                    folder_id: mount.folder_id.clone(),
                    parent_id: Some("root".into()),
                    name: "file.txt".into(),
                    node_type: "file".into(),
                    current_version_id: Some("v1".into()),
                    server_seq: 1,
                    deleted_at: None,
                    pushed_size_bytes: None,
                    pushed_mtime_nanos: None,
                },
            )
            .unwrap();
            upsert_version(
                &conn,
                &LocalVersion {
                    version_id: "v1".into(),
                    node_id: "n1".into(),
                    folder_id: mount.folder_id.clone(),
                    content_hash: "metadata-only-placeholder-hash".into(),
                    size_bytes: 999,
                    manifest_json: "[]".into(),
                },
            )
            .unwrap();
            assert!(versions::has_any_version_for_node(&conn, "n1").unwrap());
            assert!(!versions::has_materialized_content_for_version(&conn, "v1").unwrap());
        }

        materialize_mount_files(&state, &mount).await.unwrap();

        assert_eq!(fs::read(&stale_path).unwrap(), content);
        let conn = state.db.lock().await;
        assert!(versions::has_materialized_content_for_version(&conn, "v1").unwrap());
        let stored = versions::get_version(&conn, "v1").unwrap().unwrap();
        assert_eq!(stored.content_hash, manifest_content_hash(&manifest));
    }

    #[tokio::test]
    async fn background_scope_materializes_without_prior_version() {
        let state = test_state();
        let dir = tempfile::tempdir().unwrap();
        let mount = test_mount(dir.path().to_string_lossy().as_ref());
        let nodes_by_id = nodes_with_file("n1", "file.txt");
        let pulled = pulled_new_version("n1", "v1");

        let should = should_materialize_canonical(
            &state,
            &mount,
            &nodes_by_id,
            dir.path(),
            &pulled,
            MaterializeScope::Background,
        )
        .await
        .unwrap();

        assert!(should);
    }

    #[tokio::test]
    async fn background_scope_materializes_when_path_exists() {
        let state = test_state();
        let dir = tempfile::tempdir().unwrap();
        let mount = test_mount(dir.path().to_string_lossy().as_ref());
        fs::write(dir.path().join("file.txt"), b"old").unwrap();
        {
            let conn = state.db.lock().await;
            upsert_version(
                &conn,
                &LocalVersion {
                    version_id: "old-v".into(),
                    node_id: "n1".into(),
                    folder_id: mount.folder_id.clone(),
                    content_hash: "hash".into(),
                    size_bytes: 3,
                    manifest_json: "[]".into(),
                },
            )
            .unwrap();
            versions::mark_content_materialized(&conn, "old-v").unwrap();
        }
        let nodes_by_id = nodes_with_file("n1", "file.txt");
        let pulled = pulled_new_version("n1", "v2");

        let should = should_materialize_canonical(
            &state,
            &mount,
            &nodes_by_id,
            dir.path(),
            &pulled,
            MaterializeScope::Background,
        )
        .await
        .unwrap();

        assert!(should);
    }

    #[tokio::test]
    async fn background_scope_materializes_with_metadata_only_version_row_and_absent_path() {
        let state = test_state();
        let dir = tempfile::tempdir().unwrap();
        let mount = test_mount(dir.path().to_string_lossy().as_ref());
        {
            let conn = state.db.lock().await;
            upsert_version(
                &conn,
                &LocalVersion {
                    version_id: "mirrored-v".into(),
                    node_id: "n1".into(),
                    folder_id: mount.folder_id.clone(),
                    content_hash: "hash".into(),
                    size_bytes: 3,
                    manifest_json: "[]".into(),
                },
            )
            .unwrap();
            assert!(versions::has_any_version_for_node(&conn, "n1").unwrap());
            assert!(!versions::has_materialized_content_for_node(&conn, "n1").unwrap());
        }
        let nodes_by_id = nodes_with_file("n1", "file.txt");
        let pulled = pulled_new_version("n1", "v1");

        let should = should_materialize_canonical(
            &state,
            &mount,
            &nodes_by_id,
            dir.path(),
            &pulled,
            MaterializeScope::Background,
        )
        .await
        .unwrap();

        assert!(should);
    }

    #[tokio::test]
    async fn background_scope_withholds_when_prior_version_and_path_absent() {
        let state = test_state();
        let dir = tempfile::tempdir().unwrap();
        let mount = test_mount(dir.path().to_string_lossy().as_ref());
        {
            let conn = state.db.lock().await;
            upsert_version(
                &conn,
                &LocalVersion {
                    version_id: "old-v".into(),
                    node_id: "n1".into(),
                    folder_id: mount.folder_id.clone(),
                    content_hash: "hash".into(),
                    size_bytes: 3,
                    manifest_json: "[]".into(),
                },
            )
            .unwrap();
            versions::mark_content_materialized(&conn, "old-v").unwrap();
        }
        let nodes_by_id = nodes_with_file("n1", "file.txt");
        let pulled = pulled_new_version("n1", "v2");

        let should = should_materialize_canonical(
            &state,
            &mount,
            &nodes_by_id,
            dir.path(),
            &pulled,
            MaterializeScope::Background,
        )
        .await
        .unwrap();
        let full_should = should_materialize_canonical(
            &state,
            &mount,
            &nodes_by_id,
            dir.path(),
            &pulled,
            MaterializeScope::Full,
        )
        .await
        .unwrap();

        assert!(!should);
        assert!(full_should);
    }

    #[tokio::test]
    async fn account_status_poll_404_clears_cache() {
        let mut state = test_state();
        *state.account.lock().await = Some(AccountStatus {
            plan: Some("Pro".to_owned()),
            status: "active".to_owned(),
            usage_bytes: 123,
            quota_bytes: Some(456),
            current_period_end: None,
        });
        state.config.backend_url =
            account_usage_server("404 Not Found", r#"{"error":"not_found"}"#).await;

        let outcome = poll_account_status_once(&state).await;

        assert_eq!(outcome, AccountPollOutcome::NotFound);
        assert!(state.account.lock().await.is_none());
    }

    #[tokio::test]
    async fn account_status_poll_5xx_preserves_cache() {
        let mut state = test_state();
        let previous = AccountStatus {
            plan: Some("Pro".to_owned()),
            status: "past_due".to_owned(),
            usage_bytes: 123,
            quota_bytes: Some(456),
            current_period_end: None,
        };
        *state.account.lock().await = Some(previous.clone());
        state.config.backend_url =
            account_usage_server("500 Internal Server Error", r#"{"error":"boom"}"#).await;

        let outcome = poll_account_status_once(&state).await;

        assert_eq!(outcome, AccountPollOutcome::Unchanged);
        assert_eq!(*state.account.lock().await, Some(previous));
    }

    #[tokio::test]
    async fn poll_principal_status_once_resolves_access_key_and_its_scope() {
        let mut state = test_state();
        state.config.device_token = None;
        let mount = test_access_key_mount("mount-token-1");
        *state.mounts.lock().await = vec![mount];
        state.config.backend_url = account_usage_server(
            "200 OK",
            r#"{"type":"access_key","scopes":[{"folder_id":"folder-1","folder_name":"Design","scope_label":"Design","can_write":true}]}"#,
        )
        .await;

        poll_principal_status_once(&state).await;

        let principal = state.principal.lock().await.clone().unwrap();
        assert_eq!(principal.principal_type, PrincipalType::AccessKey);
        assert!(principal.email.is_none());
        assert_eq!(principal.scopes.len(), 1);
        assert_eq!(principal.scopes[0].folder_name, "Design");
    }

    #[tokio::test]
    async fn poll_principal_status_once_marks_mount_rejected_on_401() {
        let mut state = test_state();
        state.config.device_token = None;
        let mount = test_access_key_mount("mount-token-1");
        let rejected_flag = mount.rejected.clone();
        *state.mounts.lock().await = vec![mount];
        state.config.backend_url =
            account_usage_server("401 Unauthorized", r#"{"error":"unauthenticated"}"#).await;

        poll_principal_status_once(&state).await;

        assert!(rejected_flag.load(Ordering::Acquire));
        assert!(state.principal.lock().await.is_none());
    }

    #[tokio::test]
    async fn poll_principal_status_once_clears_stale_principal_when_no_tokens_remain() {
        let mut state = test_state();
        state.config.device_token = None;
        *state.principal.lock().await = Some(PrincipalStatus {
            principal_type: PrincipalType::AccessKey,
            email: None,
            scopes: Vec::new(),
        });

        poll_principal_status_once(&state).await;

        assert!(state.principal.lock().await.is_none());
    }

    fn test_access_key_mount(mount_token: &str) -> MountState {
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
            cursor_notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    #[tokio::test]
    async fn backend_health_tracks_pull_outage_recovery_and_ignores_apply_failure() {
        let dir = Path::new("/tmp").join(format!(
            "valvd-health-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        let mount_path = dir.join("mount-file");
        fs::write(&mount_path, b"not a directory").unwrap();
        let mut state = test_state();
        let mount = test_mount(mount_path.to_string_lossy().as_ref());
        {
            let conn = state.db.lock().await;
            mounts::upsert_mount(&conn, &mount.path, &mount.folder_id, None, None, None, true)
                .unwrap();
            nodes::upsert_node(
                &conn,
                &LocalNode {
                    node_id: "root-node".into(),
                    folder_id: mount.folder_id.clone(),
                    parent_id: None,
                    name: "Mount".into(),
                    node_type: "folder".into(),
                    current_version_id: None,
                    server_seq: 0,
                    deleted_at: None,
                    pushed_size_bytes: None,
                    pushed_mtime_nanos: None,
                },
            )
            .unwrap();
        }
        *state.mounts.lock().await = vec![mount.clone()];

        pull_mount_once(&state, &mount).await;
        assert!(
            !crate::control::get_status(State(state.clone()))
                .await
                .unwrap()
                .0
                .backend_connected
        );

        state.config.backend_url = delta_server(vec![r#"{"ops":[],"up_to_seq":0}"#]).await;
        pull_mount_once(&state, &mount).await;
        assert!(
            crate::control::get_status(State(state.clone()))
                .await
                .unwrap()
                .0
                .backend_connected
        );

        state.config.backend_url = delta_server(vec![
            r#"{"ops":[{"server_seq":1,"node_id":"remote-folder","op_type":"create","op_payload":{"node_id":"remote-folder","parent_id":"root-node","name":"remote-folder","type":"folder"},"actor_device_id":"other-device","applied_at":"2026-07-06T00:00:00Z"}],"up_to_seq":0}"#,
        ])
        .await;
        pull_mount_once(&state, &mount).await;
        assert!(
            crate::control::get_status(State(state))
                .await
                .unwrap()
                .0
                .backend_connected
        );
    }

    #[tokio::test]
    async fn update_required_pull_sets_mount_and_daemon_status_without_backend_disconnect() {
        let mut state = test_state();
        let mount = test_mount("/sync");
        {
            let conn = state.db.lock().await;
            mounts::upsert_mount(&conn, &mount.path, &mount.folder_id, None, None, None, true)
                .unwrap();
        }
        *state.mounts.lock().await = vec![mount.clone()];
        state.config.backend_url = delta_server(vec![
            r#"{"ops":[{"server_seq":1,"node_id":"future-node","op_type":"future_op","op_payload":{},"actor_device_id":"other-device","applied_at":"2026-07-08T00:00:00Z"}],"up_to_seq":1}"#,
        ])
        .await;

        pull_mount_once(&state, &mount).await;
        let status = crate::control::get_status(State(state)).await.unwrap().0;

        assert!(status.backend_connected);
        assert!(status.update_required);
        assert_eq!(status.mounts.len(), 1);
        assert!(status.mounts[0].update_required);
    }

    #[tokio::test]
    async fn update_required_push_sets_mount_and_stops_further_ops() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
        fs::write(dir.path().join("b.txt"), b"bravo").unwrap();
        let mut state = test_state();
        let mount = test_mount(dir.path().to_string_lossy().as_ref());
        {
            let conn = state.db.lock().await;
            mounts::upsert_mount(&conn, &mount.path, &mount.folder_id, None, None, None, true)
                .unwrap();
            nodes::upsert_node(
                &conn,
                &LocalNode {
                    node_id: "root-node".into(),
                    folder_id: mount.folder_id.clone(),
                    parent_id: None,
                    name: "Mount".into(),
                    node_type: "folder".into(),
                    current_version_id: None,
                    server_seq: 0,
                    deleted_at: None,
                    pushed_size_bytes: None,
                    pushed_mtime_nanos: None,
                },
            )
            .unwrap();
        }
        *state.mounts.lock().await = vec![mount.clone()];
        let (backend_url, server) = push_update_required_server().await;
        state.config.backend_url = backend_url;

        let summary = full_sync_mount(&state, &mount, MaterializeScope::Full).await;
        let requests = server.await.unwrap();
        let status = crate::control::get_status(State(state)).await.unwrap().0;

        assert_eq!(summary.errors, 1);
        assert_eq!(requests, vec!["POST /api/folders/folder-1/ops"]);
        assert!(status.backend_connected);
        assert!(status.update_required);
        assert_eq!(status.mounts.len(), 1);
        assert!(status.mounts[0].update_required);
    }

    #[tokio::test]
    async fn forbidden_push_sets_mount_error_even_though_pull_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
        let mut state = test_state();
        let mount = test_mount(dir.path().to_string_lossy().as_ref());
        {
            let conn = state.db.lock().await;
            mounts::upsert_mount(&conn, &mount.path, &mount.folder_id, None, None, None, true)
                .unwrap();
            nodes::upsert_node(
                &conn,
                &LocalNode {
                    node_id: "root-node".into(),
                    folder_id: mount.folder_id.clone(),
                    parent_id: None,
                    name: "Mount".into(),
                    node_type: "folder".into(),
                    current_version_id: None,
                    server_seq: 0,
                    deleted_at: None,
                    pushed_size_bytes: None,
                    pushed_mtime_nanos: None,
                },
            )
            .unwrap();
        }
        *state.mounts.lock().await = vec![mount.clone()];
        let (backend_url, server) = push_forbidden_server().await;
        state.config.backend_url = backend_url;

        let summary = full_sync_mount(&state, &mount, MaterializeScope::Full).await;
        let requests = server.await.unwrap();
        let status = crate::control::get_status(State(state)).await.unwrap().0;

        assert_eq!(summary.errors, 1);
        assert!(requests.contains(&"POST /api/folders/folder-1/ops".to_owned()));
        assert!(status.mounts[0].error.is_some());
    }

    #[tokio::test]
    async fn a_persistent_materialize_failure_sets_a_durable_mount_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let mut state = test_state();
        let mount = test_mount(dir.path().to_string_lossy().as_ref());
        {
            let conn = state.db.lock().await;
            mounts::upsert_mount(&conn, &mount.path, &mount.folder_id, None, None, None, true)
                .unwrap();
            nodes::upsert_node(
                &conn,
                &LocalNode {
                    node_id: "root-node".into(),
                    folder_id: mount.folder_id.clone(),
                    parent_id: None,
                    name: "Mount".into(),
                    node_type: "folder".into(),
                    current_version_id: None,
                    server_seq: 0,
                    deleted_at: None,
                    pushed_size_bytes: None,
                    pushed_mtime_nanos: None,
                },
            )
            .unwrap();
        }
        *state.mounts.lock().await = vec![mount.clone()];
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

        state.config.backend_url = delta_server(vec![
            r#"{"ops":[{"server_seq":1,"node_id":"remote-folder","op_type":"create","op_payload":{"node_id":"remote-folder","parent_id":"root-node","name":"remote-folder","type":"folder"},"actor_device_id":"other-device","applied_at":"2026-07-06T00:00:00Z"}],"up_to_seq":0}"#,
        ])
        .await;

        let summary = full_sync_mount(&state, &mount, MaterializeScope::Full).await;
        let status = crate::control::get_status(State(state)).await.unwrap().0;

        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(summary.errors, 1);
        assert!(status.mounts[0].error.is_some());
        assert_eq!(status.mounts[0].pending_ops, 1);
    }

    #[tokio::test]
    async fn update_required_push_short_circuits_when_mount_already_halted() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
        let mut state = test_state();
        let mut mount = test_mount(dir.path().to_string_lossy().as_ref());
        mount.update_required = true;
        {
            let conn = state.db.lock().await;
            mounts::upsert_mount(&conn, &mount.path, &mount.folder_id, None, None, None, true)
                .unwrap();
            nodes::upsert_node(
                &conn,
                &LocalNode {
                    node_id: "root-node".into(),
                    folder_id: mount.folder_id.clone(),
                    parent_id: None,
                    name: "Mount".into(),
                    node_type: "folder".into(),
                    current_version_id: None,
                    server_seq: 0,
                    deleted_at: None,
                    pushed_size_bytes: None,
                    pushed_mtime_nanos: None,
                },
            )
            .unwrap();
        }
        *state.mounts.lock().await = vec![mount.clone()];
        let (backend_url, server) = push_update_required_server().await;
        state.config.backend_url = backend_url;

        let summary = full_sync_mount(&state, &mount, MaterializeScope::Full).await;
        let requests = server.await.unwrap();
        let status = crate::control::get_status(State(state)).await.unwrap().0;

        assert_eq!(summary.errors, 1);
        assert!(requests.is_empty());
        assert!(status.update_required);
        assert!(status.mounts[0].update_required);
    }

    fn test_mount(path: &str) -> MountState {
        MountState {
            path: path.to_owned(),
            folder_id: "folder-1".to_owned(),
            grant_id: None,
            scope_node_id: None,
            mount_token: None,
            can_write: true,
            name: "Mount".to_owned(),
            active_syncs: 0,
            pending_ops: 0,
            last_synced_at: None,
            update_required: false,
            update_required_flag: Arc::new(AtomicBool::new(false)),
            rejected: Arc::new(AtomicBool::new(false)),
            error: None,
            watcher_alive: Arc::new(AtomicBool::new(true)),
            sync_lock: Arc::new(Mutex::new(())),
            cursor_notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn local_file_node(node_id: &str, name: &str) -> LocalNode {
        LocalNode {
            node_id: node_id.to_owned(),
            folder_id: "folder-1".to_owned(),
            parent_id: Some("root".to_owned()),
            name: name.to_owned(),
            node_type: "file".to_owned(),
            current_version_id: Some("old-v".to_owned()),
            server_seq: 1,
            deleted_at: None,
            pushed_size_bytes: None,
            pushed_mtime_nanos: None,
        }
    }

    fn root_node() -> LocalNode {
        LocalNode {
            node_id: "root".to_owned(),
            folder_id: "folder-1".to_owned(),
            parent_id: None,
            name: "Mount".to_owned(),
            node_type: "folder".to_owned(),
            current_version_id: None,
            server_seq: 0,
            deleted_at: None,
            pushed_size_bytes: None,
            pushed_mtime_nanos: None,
        }
    }

    fn nodes_with_file(node_id: &str, name: &str) -> HashMap<String, LocalNode> {
        HashMap::from([
            ("root".to_owned(), root_node()),
            (node_id.to_owned(), local_file_node(node_id, name)),
        ])
    }

    fn pulled_new_version(node_id: &str, version_id: &str) -> PulledNode {
        PulledNode {
            node_id: node_id.to_owned(),
            op_type: "new_version".to_owned(),
            is_conflict_copy: false,
            actor_device_id: "other-device".to_owned(),
            applied_at: "2026-07-08T00:00:00Z".to_owned(),
            old_name: Some("file.txt".to_owned()),
            old_parent_id: None,
            old_version_id: Some("old-v".to_owned()),
            new_name: "file.txt".to_owned(),
            new_parent_id: None,
            new_version_id: Some(version_id.to_owned()),
            node_type: "file".to_owned(),
        }
    }

    async fn delta_server(responses: Vec<&'static str>) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for body in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 2048];
                let _ = stream.read(&mut buffer).await.unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        format!("http://{addr}")
    }

    async fn push_update_required_server() -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            while let Ok(Ok((mut stream, _))) =
                timeout(Duration::from_millis(200), listener.accept()).await
            {
                let (request_line, _body) = read_http_request(&mut stream).await;
                requests.push(request_line.clone());
                if request_line == "POST /api/folders/folder-1/ops" {
                    write_http_response(
                        &mut stream,
                        "200 OK",
                        br#"{"result":"future","server_seq":7,"node_id":"future-node"}"#,
                    )
                    .await;
                } else if request_line == "GET /api/folders/folder-1/ops?since=0" {
                    write_http_response(&mut stream, "200 OK", br#"{"ops":[],"up_to_seq":0}"#)
                        .await;
                } else {
                    write_http_response(&mut stream, "404 Not Found", b"{}").await;
                }
            }
            requests
        });
        (format!("http://{addr}"), server)
    }

    async fn push_forbidden_server() -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            while let Ok(Ok((mut stream, _))) =
                timeout(Duration::from_millis(200), listener.accept()).await
            {
                let (request_line, _body) = read_http_request(&mut stream).await;
                requests.push(request_line.clone());
                if request_line == "POST /api/folders/folder-1/ops" {
                    write_http_response(&mut stream, "403 Forbidden", b"{}").await;
                } else if request_line == "GET /api/folders/folder-1/ops?since=0" {
                    write_http_response(&mut stream, "200 OK", br#"{"ops":[],"up_to_seq":0}"#)
                        .await;
                } else {
                    write_http_response(&mut stream, "404 Not Found", b"{}").await;
                }
            }
            requests
        });
        (format!("http://{addr}"), server)
    }

    async fn push_transient_failure_server() -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            while let Ok(Ok((mut stream, _))) =
                timeout(Duration::from_millis(200), listener.accept()).await
            {
                let (request_line, _body) = read_http_request(&mut stream).await;
                requests.push(request_line.clone());
                if request_line == "POST /api/folders/folder-1/ops" {
                    write_http_response(&mut stream, "503 Service Unavailable", b"{}").await;
                } else if request_line == "GET /api/folders/folder-1/ops?since=0" {
                    write_http_response(&mut stream, "200 OK", br#"{"ops":[],"up_to_seq":0}"#)
                        .await;
                } else {
                    write_http_response(&mut stream, "404 Not Found", b"{}").await;
                }
            }
            requests
        });
        (format!("http://{addr}"), server)
    }

    #[tokio::test]
    async fn a_transient_push_failure_leaves_pending_ops_nonzero_and_no_mount_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
        let mut state = test_state();
        let mount = test_mount(dir.path().to_string_lossy().as_ref());
        {
            let conn = state.db.lock().await;
            mounts::upsert_mount(&conn, &mount.path, &mount.folder_id, None, None, None, true)
                .unwrap();
            nodes::upsert_node(
                &conn,
                &LocalNode {
                    node_id: "root-node".into(),
                    folder_id: mount.folder_id.clone(),
                    parent_id: None,
                    name: "Mount".into(),
                    node_type: "folder".into(),
                    current_version_id: None,
                    server_seq: 0,
                    deleted_at: None,
                    pushed_size_bytes: None,
                    pushed_mtime_nanos: None,
                },
            )
            .unwrap();
        }
        *state.mounts.lock().await = vec![mount.clone()];
        let (backend_url, server) = push_transient_failure_server().await;
        state.config.backend_url = backend_url;

        let summary = full_sync_mount(&state, &mount, MaterializeScope::Full).await;
        let _ = server.await.unwrap();
        let status = crate::control::get_status(State(state)).await.unwrap().0;

        assert_eq!(summary.errors, 1);
        assert_eq!(
            status.mounts[0].pending_ops, 1,
            "a pass with errors must not report pending_ops == 0: that is what the CLI's sync barrier reads as settled"
        );
        assert!(
            status.mounts[0].error.is_none(),
            "a transient push failure is retryable, not a persistent mount error"
        );
    }

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end;
        loop {
            let n = stream.read(&mut tmp).await.unwrap();
            assert!(n > 0, "connection closed before headers");
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                header_end = pos + 4;
                break;
            }
        }
        let headers = String::from_utf8_lossy(&buf[..header_end]);
        let request_line = headers.lines().next().unwrap();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap();
        let path = parts.next().unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length: ")
                    .or_else(|| line.strip_prefix("content-length: "))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = buf[header_end..].to_vec();
        while body.len() < content_length {
            let n = stream.read(&mut tmp).await.unwrap();
            assert!(n > 0, "connection closed before body");
            body.extend_from_slice(&tmp[..n]);
        }
        body.truncate(content_length);
        (format!("{method} {path}"), body)
    }

    async fn write_http_response(
        stream: &mut tokio::net::TcpStream,
        status_line: &str,
        body: &[u8],
    ) {
        let response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
    }

    async fn request_counting_server(
        ops_response_body: &'static str,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let requests_for_server = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let (request_line, _body) = read_http_request(&mut stream).await;
                requests_for_server.lock().unwrap().push(request_line.clone());
                if request_line.starts_with("GET /api/folders/folder-1/ops") {
                    write_http_response(&mut stream, "200 OK", ops_response_body.as_bytes()).await;
                } else if request_line == "POST /api/folders/folder-1/ops" {
                    write_http_response(
                        &mut stream,
                        "200 OK",
                        br#"{"result":"applied","server_seq":1,"node_id":"n"}"#,
                    )
                    .await;
                } else {
                    write_http_response(&mut stream, "404 Not Found", b"{}").await;
                }
            }
        });
        (format!("http://{addr}"), requests)
    }

    fn pull_request_count(requests: &std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> usize {
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.starts_with("GET /api/folders/folder-1/ops"))
            .count()
    }

    async fn dirty_reconcile_test_state() -> (
        DaemonState,
        MountState,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        tempfile::TempDir,
    ) {
        let mut state = test_state();
        let dir = tempfile::tempdir().unwrap();
        let mount = test_mount(dir.path().to_string_lossy().as_ref());
        {
            let conn = state.db.lock().await;
            mounts::upsert_mount(&conn, &mount.path, &mount.folder_id, None, None, None, true)
                .unwrap();
            nodes::upsert_node(&conn, &root_node()).unwrap();
        }
        let (backend_url, requests) = request_counting_server(r#"{"ops":[],"up_to_seq":0}"#).await;
        state.config.backend_url = backend_url;
        *state.mounts.lock().await = vec![mount.clone()];
        (state, mount, requests, dir)
    }

    #[tokio::test]
    async fn sync_loop_reconciles_on_a_dirty_signal_without_waiting_for_the_periodic_tick() {
        let (state, mount, requests, _dir) = dirty_reconcile_test_state().await;
        let (_notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(4);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            tokio::spawn(std::future::pending::<()>()),
        ));

        dirty_signal.mark();
        sleep(Duration::from_millis(1600)).await;
        handle.abort();

        assert_eq!(
            pull_request_count(&requests),
            1,
            "a dirty signal should trigger a reconcile well before the 30s ticker"
        );
    }

    #[tokio::test]
    async fn sync_loop_coalesces_a_burst_of_dirty_signals_into_one_reconcile() {
        let (state, mount, requests, _dir) = dirty_reconcile_test_state().await;
        let (_notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(4);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            tokio::spawn(std::future::pending::<()>()),
        ));

        for _ in 0..5 {
            dirty_signal.mark();
        }
        sleep(Duration::from_millis(1600)).await;
        handle.abort();

        assert_eq!(
            pull_request_count(&requests),
            1,
            "a burst of dirty signals must coalesce into exactly one reconcile"
        );
    }

    #[tokio::test]
    async fn sync_loop_debounces_a_single_ws_notification_before_pulling() {
        let (state, mount, requests, _dir) = dirty_reconcile_test_state().await;
        let (notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(4);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            tokio::spawn(std::future::pending::<()>()),
        ));

        notify_tx
            .send(WsPushNotification {
                folder_id: mount.folder_id.clone(),
                // A real remote-change push (server_seq > 0) takes the
                // pull-only debounce path; a catch-up (server_seq 0) would
                // instead route through the dirty/push-first reconcile.
                server_seq: 1,
            })
            .await
            .unwrap();

        sleep(Duration::from_millis(400)).await;
        assert_eq!(
            pull_request_count(&requests),
            0,
            "a WS notification must not pull immediately on arrival"
        );

        sleep(Duration::from_millis(900)).await;
        handle.abort();

        assert_eq!(
            pull_request_count(&requests),
            1,
            "a single WS notification must pull once the ~1s debounce elapses"
        );
    }

    #[tokio::test]
    async fn sync_loop_coalesces_a_burst_of_ws_notifications_from_the_first_arrival() {
        let (state, mount, requests, _dir) = dirty_reconcile_test_state().await;
        let (notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(8);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            tokio::spawn(std::future::pending::<()>()),
        ));

        // Arrivals spread over 400ms (t=0, 200, 400). If the debounce
        // reset/extended on each arrival (trailing-edge), its deadline would
        // sit at 400ms+1000ms=1400ms; a fixed-from-first-arrival deadline
        // sits at 0ms+1000ms=1000ms. Waiting to ~1.2s from the first arrival
        // distinguishes the two: only the fixed-from-first deadline has
        // fired by then.
        for _ in 0..3 {
            notify_tx
                .send(WsPushNotification {
                    folder_id: mount.folder_id.clone(),
                    // Real remote-change pushes: exercise the pull-only debounce.
                    server_seq: 1,
                })
                .await
                .unwrap();
            sleep(Duration::from_millis(200)).await;
        }
        sleep(Duration::from_millis(800)).await;
        handle.abort();

        assert_eq!(
            pull_request_count(&requests),
            1,
            "a burst of WS notifications must coalesce into exactly one pull, \
             firing ~1s after the first arrival and not extended by later ones"
        );
    }

    #[tokio::test]
    async fn sync_loop_runs_a_push_first_reconcile_for_a_catch_up_notification() {
        // A catch-up notification (server_seq 0: boot/resume/WS-connect) must
        // push-first, not pull-only: an un-pushed local edit made during a
        // pause or downtime has to be submitted (and conflict-forked
        // server-side) before any remote version is pulled, or it is lost.
        let (state, mount, requests, dir) = dirty_reconcile_test_state().await;
        // Give push_local something to submit so a push-first reconcile is
        // distinguishable from a pull-only reconcile by the request log.
        fs::write(dir.path().join("a.txt"), b"alpha").unwrap();

        let (notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(4);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            tokio::spawn(std::future::pending::<()>()),
        ));

        notify_tx
            .send(WsPushNotification {
                folder_id: mount.folder_id.clone(),
                server_seq: 0,
            })
            .await
            .unwrap();

        sleep(Duration::from_millis(1500)).await;
        handle.abort();

        let log = requests.lock().unwrap().clone();
        let first_post = log
            .iter()
            .position(|line| line == "POST /api/folders/folder-1/ops");
        let first_get = log
            .iter()
            .position(|line| line.starts_with("GET /api/folders/folder-1/ops"));
        assert!(
            first_post.is_some(),
            "a catch-up notification must trigger a push-first reconcile (POST), \
             not a pull-only pull"
        );
        // Push must precede pull: prove the reconcile order, not merely that a
        // POST happened at some point.
        assert!(
            first_get.is_none() || first_post < first_get,
            "the reconcile must push before it pulls (POST before GET)"
        );
        // The catch-up must not also arm the pull-only deadline (that would
        // double the work into a second, separate pull).
        assert_eq!(
            pull_request_count(&requests),
            1,
            "a catch-up reconcile pulls exactly once"
        );
    }

    #[tokio::test]
    async fn ws_notify_wake_reconciles_when_the_dirty_flag_is_already_set() {
        // Drives this through a real sync_loop rather than inlining the
        // decision, so a real regression in the match arm is actually
        // caught. A real remote-change notification (server_seq > 0) takes the
        // pull-only deadline path, but its Periodic wake still reconciles
        // (push-first) when the dirty flag is already set. sync_loop's Dirty
        // select! arm shares the same DirtySignal/Notify, which otherwise
        // races: DirtySignal::mark() wakes any pending dirty_signal.notified()
        // near-instantly, normally winning the select! and consuming the
        // flag via its own debounce before the independently-armed
        // WS-notify deadline (~1s out) gets a chance to observe it. Pausing
        // first sidesteps that race deterministically: the Dirty wake still
        // fires and consumes the Notify permit, but hits the `if
        // state.paused { continue }` guard before its own take()+reconcile,
        // so the flag stays set. Only once resumed does the already-armed
        // WS-notify debounce fire and observe that still-set flag.
        let (state, mount, requests, dir) = dirty_reconcile_test_state().await;
        // A push with nothing local to push makes no POST call at all, which
        // would make reconcile and pull-only indistinguishable by request
        // log; give it a local file so push_local has something to submit.
        fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
        state.paused.store(true, Ordering::Release);

        let (notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(4);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            tokio::spawn(std::future::pending::<()>()),
        ));

        dirty_signal.mark();
        sleep(Duration::from_millis(150)).await;

        notify_tx
            .send(WsPushNotification {
                folder_id: mount.folder_id.clone(),
                // Real remote change: arms the pull-only deadline whose Periodic
                // wake must still reconcile because the dirty flag is set.
                server_seq: 1,
            })
            .await
            .unwrap();
        sleep(Duration::from_millis(150)).await;

        state.paused.store(false, Ordering::Release);
        sleep(Duration::from_millis(1500)).await;
        handle.abort();

        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.starts_with("POST")),
            "a WS-notify debounce firing after resume must see the still-set \
             dirty flag and reconcile, not just pull-only"
        );
    }

    #[tokio::test]
    async fn spawn_tasks_for_mount_enqueues_a_boot_catchup_independent_of_ws() {
        // dirty_reconcile_test_state's backend server speaks plain HTTP, not
        // the WebSocket upgrade handshake, so ws_push_loop can never connect
        // here - this is exactly the "WS can't connect but HTTPS still
        // works" boot scenario the boot catch-up must cover on its own.
        let (state, mount, requests, _dir) = dirty_reconcile_test_state().await;

        spawn_tasks_for_mount(&state, mount.clone()).await;

        let mut saw_pull = false;
        for _ in 0..100 {
            if pull_request_count(&requests) > 0 {
                saw_pull = true;
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
        cancel_tasks_for_mount(&state, &mount.path).await;

        assert!(
            saw_pull,
            "spawn_tasks_for_mount must enqueue a boot catch-up pull even when \
             the WebSocket connection never establishes"
        );
    }

    // Spawns a task that has already exited, standing in for an fs_watch
    // task that died immediately (e.g. a watch_mount registration failure).
    async fn already_finished_watcher_handle() -> tokio::task::JoinHandle<()> {
        let handle = tokio::spawn(async {});
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
        handle
    }

    // sync_loop's respawn only proceeds while the mount's state.tasks entry
    // is present (the teardown-race invariant), so tests that drive a
    // respawn need a placeholder entry with the initial fs_watch abort
    // handle at FS_WATCH_TASK_INDEX.
    async fn insert_watcher_task_entry(
        state: &DaemonState,
        mount: &MountState,
        fs_abort_handle: tokio::task::AbortHandle,
    ) {
        state.tasks.lock().await.insert(
            mount.path.clone(),
            vec![
                tokio::spawn(std::future::pending::<()>()).abort_handle(),
                tokio::spawn(std::future::pending::<()>()).abort_handle(),
                fs_abort_handle,
            ],
        );
    }

    async fn wait_for_watcher_alive(mount: &MountState, want: bool, attempts: u32) -> bool {
        for _ in 0..attempts {
            if mount.watcher_alive.load(Ordering::Acquire) == want {
                return true;
            }
            sleep(Duration::from_millis(50)).await;
        }
        mount.watcher_alive.load(Ordering::Acquire) == want
    }

    // test_mount() initializes watcher_alive to true, so waiting on `true`
    // alone would trivially pass before sync_loop even runs its first
    // iteration; wait for the death to be observed first.
    async fn wait_for_watcher_respawn(mount: &MountState) -> bool {
        // Generous windows: a dead-watcher dirty mark drives a reconcile whose
        // ~1s debounce plus full-sync keeps sync_loop off its select! (so the
        // next exit is observed only afterward), same as any in-flight wake.
        wait_for_watcher_alive(mount, false, 160).await
            && wait_for_watcher_alive(mount, true, 160).await
    }

    #[tokio::test]
    async fn sync_loop_detects_a_dead_watcher_and_respawns_it_without_a_ticker_tick() {
        let (state, mount, _requests, _dir) = dirty_reconcile_test_state().await;
        let dead_handle = already_finished_watcher_handle().await;
        insert_watcher_task_entry(&state, &mount, dead_handle.abort_handle()).await;

        let (_notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(4);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            dead_handle,
        ));

        assert!(
            wait_for_watcher_alive(&mount, false, 20).await,
            "a dead watcher must be observed as not alive"
        );
        assert!(
            wait_for_watcher_alive(&mount, true, 80).await,
            "the watcher must be respawned and observed alive again, well before the 30s ticker"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn sync_loop_reports_watcher_not_alive_promptly_not_falsely_alive() {
        let (state, mount, _requests, _dir) = dirty_reconcile_test_state().await;
        let dead_handle = already_finished_watcher_handle().await;
        insert_watcher_task_entry(&state, &mount, dead_handle.abort_handle()).await;

        let (_notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(4);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            dead_handle,
        ));

        assert!(
            wait_for_watcher_alive(&mount, false, 20).await,
            "a watcher that dies at registration must be reported not-alive promptly, \
             not falsely alive on the strength of the spawn call alone"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn sync_loop_detects_a_second_death_of_a_respawned_watcher() {
        let (state, mount, _requests, _dir) = dirty_reconcile_test_state().await;
        let dead_handle = already_finished_watcher_handle().await;
        insert_watcher_task_entry(&state, &mount, dead_handle.abort_handle()).await;

        let (_notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(4);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            dead_handle,
        ));

        assert!(
            wait_for_watcher_respawn(&mount).await,
            "the first respawn must succeed"
        );

        // Abort the now-live watcher directly through the same abort handle
        // DELETE /mount or a remount would use, forcing a second death.
        {
            let tasks = state.tasks.lock().await;
            tasks.get(&mount.path).unwrap()[FS_WATCH_TASK_INDEX].abort();
        }

        assert!(
            wait_for_watcher_respawn(&mount).await,
            "the respawned watcher's later death must also be detected and re-armed on the new handle"
        );
        handle.abort();
    }

    #[test]
    fn watcher_backoff_grows_then_caps_and_resets_on_stable() {
        let mut backoff = Duration::ZERO;
        backoff = next_watcher_backoff(backoff, Duration::ZERO);
        assert_eq!(backoff, WATCHER_RESPAWN_INITIAL_BACKOFF);
        backoff = next_watcher_backoff(backoff, Duration::ZERO);
        assert_eq!(backoff, WATCHER_RESPAWN_INITIAL_BACKOFF * 2);
        backoff = next_watcher_backoff(backoff, Duration::ZERO);
        assert_eq!(backoff, WATCHER_RESPAWN_INITIAL_BACKOFF * 4);

        let capped = next_watcher_backoff(WATCHER_RESPAWN_MAX_BACKOFF, Duration::ZERO);
        assert_eq!(
            capped, WATCHER_RESPAWN_MAX_BACKOFF,
            "backoff must not grow past the cap"
        );

        let reset = next_watcher_backoff(WATCHER_RESPAWN_MAX_BACKOFF, WATCHER_LIVENESS_THRESHOLD);
        assert_eq!(
            reset,
            Duration::ZERO,
            "a watcher that lived past the liveness threshold resets the backoff"
        );
    }

    #[tokio::test]
    async fn sync_loop_respawns_a_finished_watcher_and_marks_the_mount_dirty() {
        let (state, mount, requests, _dir) = dirty_reconcile_test_state().await;
        let dead_handle = already_finished_watcher_handle().await;
        insert_watcher_task_entry(&state, &mount, dead_handle.abort_handle()).await;

        let (_notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(4);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            dead_handle,
        ));

        assert!(
            wait_for_watcher_respawn(&mount).await,
            "a healthy respawned watcher must be observed alive again"
        );

        // With the first ticker tick +30s away and no notification, the only
        // thing that can hit the backend here is the dead-watcher dirty mark
        // driving a reconcile.
        let mut saw_reconcile = false;
        for _ in 0..60 {
            if !requests.lock().unwrap().is_empty() {
                saw_reconcile = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        handle.abort();
        assert!(
            saw_reconcile,
            "a dead watcher must mark the mount dirty and drive a reconcile"
        );

        let tasks = state.tasks.lock().await;
        let handles = tasks.get(&mount.path).unwrap();
        assert!(
            !handles[FS_WATCH_TASK_INDEX].is_finished(),
            "the finished handle must have been replaced with a fresh one"
        );
    }

    #[tokio::test]
    async fn sync_loop_respawns_a_dead_watcher_while_paused_but_defers_reconcile_until_resume() {
        let (state, mount, requests, _dir) = dirty_reconcile_test_state().await;
        state.paused.store(true, Ordering::Release);

        let dead_handle = already_finished_watcher_handle().await;
        insert_watcher_task_entry(&state, &mount, dead_handle.abort_handle()).await;

        let (_notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(4);
        let dirty_signal = DirtySignal::new();
        let handle = tokio::spawn(sync_loop(
            state.clone(),
            mount.clone(),
            notify_rx,
            dirty_signal.clone(),
            dead_handle,
        ));

        assert!(
            wait_for_watcher_respawn(&mount).await,
            "a dead watcher must still be detected and respawned while the daemon is paused"
        );
        assert_eq!(
            pull_request_count(&requests),
            0,
            "the paused guard must defer the respawn's mirror-mutating reconcile"
        );

        state.paused.store(false, Ordering::Release);
        dirty_signal.mark();
        sleep(Duration::from_millis(1600)).await;
        handle.abort();

        assert_eq!(
            pull_request_count(&requests),
            1,
            "the deferred reconcile must run once the daemon resumes"
        );
    }
}
