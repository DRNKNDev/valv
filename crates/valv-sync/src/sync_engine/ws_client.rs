use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::{
    sync::mpsc,
    time::{interval_at, sleep, Duration, Instant, MissedTickBehavior},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::protocol::sync::WsPushNotification;

// A ping wakes a hibernated Cloudflare Durable Object (real cost, not just
// latency), so this is the loosest cadence that still leaves the idle
// deadline's required ~2x margin for one missed/delayed pong.
const PING_INTERVAL: Duration = Duration::from_secs(22);
// Strictly less than the sync poll floor (delta-pull-loop) and at least 2x
// PING_INTERVAL, so a single missed pong never trips it.
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(55);

pub async fn ws_push_loop(
    backend_url: &str,
    token: &str,
    folder_ids: Vec<String>,
    tx: mpsc::Sender<WsPushNotification>,
    pre_connect_jitter: Duration,
) -> Result<()> {
    let ws_url = derive_ws_url(backend_url, token)?;

    if !pre_connect_jitter.is_zero() {
        sleep(pre_connect_jitter).await;
    }

    loop {
        match connect_and_forward(&ws_url, &folder_ids, tx.clone()).await {
            Ok(()) => {}
            Err(err) => eprintln!("websocket disconnected: {err}"),
        }
        sleep(Duration::from_secs(2)).await;
    }
}

pub fn derive_ws_url(backend_url: &str, token: &str) -> Result<String> {
    let trimmed = backend_url.trim_end_matches('/');
    let ws_base = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_owned()
    } else {
        return Err(anyhow!("backend_url must start with http:// or https://"));
    };

    Ok(format!("{ws_base}/ws?token={token}"))
}

async fn connect_and_forward(
    ws_url: &str,
    folder_ids: &[String],
    tx: mpsc::Sender<WsPushNotification>,
) -> Result<()> {
    connect_and_forward_with_intervals(ws_url, folder_ids, tx, PING_INTERVAL, READ_IDLE_TIMEOUT)
        .await
}

// ping_interval/read_idle_timeout are parameterized only so tests can drive
// the select! below on millisecond timescales; connect_and_forward always
// calls this with the fixed PING_INTERVAL/READ_IDLE_TIMEOUT constants above.
async fn connect_and_forward_with_intervals(
    ws_url: &str,
    folder_ids: &[String],
    tx: mpsc::Sender<WsPushNotification>,
    ping_interval: Duration,
    read_idle_timeout: Duration,
) -> Result<()> {
    let (mut socket, _) = connect_async(ws_url).await?;
    for folder_id in folder_ids {
        socket
            .send(Message::Text(
                json!({"type":"subscribe","folder_id":folder_id}).to_string(),
            ))
            .await?;
    }

    // Connect-time catch-up: gives a freshly (re)connected client an
    // immediate reason to pull, on every connect including reconnects.
    for folder_id in folder_ids {
        let notification = WsPushNotification {
            folder_id: folder_id.clone(),
            server_seq: 0,
        };
        if tx.send(notification).await.is_err() {
            return Ok(());
        }
    }

    let mut ping_ticker = interval_at(Instant::now() + ping_interval, ping_interval);
    ping_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let idle_timeout = sleep(read_idle_timeout);
    tokio::pin!(idle_timeout);

    loop {
        tokio::select! {
            // Biased with socket.next() first: when a frame and the
            // ping/idle arms are both ready in the same poll, the frame must
            // win so it resets the idle deadline before that deadline (also
            // ready) would otherwise spuriously fire.
            biased;
            message = socket.next() => {
                match message {
                    Some(Ok(message)) => {
                        // Any frame counts as liveness: data, Pong, or a
                        // server-originated Ping keepalive.
                        idle_timeout.as_mut().reset(Instant::now() + read_idle_timeout);
                        let Ok(text) = message.into_text() else {
                            eprintln!("ignoring non-text websocket message");
                            continue;
                        };
                        match serde_json::from_str::<WsPushNotification>(&text) {
                            Ok(notification) => {
                                if tx.send(notification).await.is_err() {
                                    return Ok(());
                                }
                            }
                            Err(err) => eprintln!("ignoring malformed websocket message: {err}"),
                        }
                    }
                    Some(Err(err)) => return Err(err.into()),
                    None => return Ok(()),
                }
            }
            _ = ping_ticker.tick() => {
                socket.send(Message::Ping(Vec::new())).await?;
            }
            () = &mut idle_timeout => {
                return Err(anyhow!("websocket read-idle deadline elapsed with no frames received"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{net::TcpListener, time::timeout};
    use tokio_tungstenite::accept_async;

    #[test]
    fn https_backend_derives_wss_url() {
        assert_eq!(
            derive_ws_url("https://api.valv.dev", "secret").unwrap(),
            "wss://api.valv.dev/ws?token=secret"
        );
    }

    #[test]
    fn http_backend_derives_ws_url() {
        assert_eq!(
            derive_ws_url("http://localhost:3000", "secret").unwrap(),
            "ws://localhost:3000/ws?token=secret"
        );
    }

    #[tokio::test]
    async fn future_shaped_json_message_is_ignored_without_disconnect() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _subscribe = socket.next().await.unwrap().unwrap();
            socket
                .send(Message::Text(
                    r#"{"type":"future","payload":{"folder_id":"f1"}}"#.into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    r#"{"folder_id":"f1","server_seq":77}"#.into(),
                ))
                .await
                .unwrap();
            socket.close(None).await.unwrap();
        });
        let (tx, mut rx) = mpsc::channel(4);

        let ws_url = format!("ws://{addr}/ws?token=secret");
        let folder_ids = vec!["f1".to_owned()];
        let client =
            tokio::spawn(async move { connect_and_forward(&ws_url, &folder_ids, tx).await });

        let catch_up = timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            catch_up,
            WsPushNotification {
                folder_id: "f1".into(),
                server_seq: 0
            },
            "connect_and_forward must send a synthetic catch-up notification before its read loop"
        );

        let notification = timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        drop(rx);
        client.await.unwrap().unwrap();
        server.await.unwrap();

        assert_eq!(
            notification,
            WsPushNotification {
                folder_id: "f1".into(),
                server_seq: 77
            }
        );
    }

    #[tokio::test]
    async fn ping_is_sent_after_the_interval_elapses_on_an_idle_connection() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _subscribe = socket.next().await.unwrap().unwrap();
            let frame = timeout(Duration::from_millis(500), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(frame, Message::Ping(_)), "expected a Ping frame, got {frame:?}");
        });
        let (tx, _rx) = mpsc::channel(4);

        let ws_url = format!("ws://{addr}/ws?token=secret");
        let folder_ids = vec!["f1".to_owned()];
        let client = tokio::spawn(async move {
            connect_and_forward_with_intervals(
                &ws_url,
                &folder_ids,
                tx,
                Duration::from_millis(50),
                Duration::from_millis(500),
            )
            .await
        });

        server.await.unwrap();
        client.abort();
    }

    #[tokio::test]
    async fn no_frames_within_the_idle_deadline_disconnects_with_an_error() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _subscribe = socket.next().await.unwrap().unwrap();
            // Deliberately sends nothing back; holds the socket open past the
            // client's idle deadline.
            sleep(Duration::from_millis(400)).await;
        });
        let (tx, _rx) = mpsc::channel(4);

        let ws_url = format!("ws://{addr}/ws?token=secret");
        let folder_ids = vec!["f1".to_owned()];
        let result = timeout(
            Duration::from_millis(600),
            connect_and_forward_with_intervals(
                &ws_url,
                &folder_ids,
                tx,
                Duration::from_secs(10),
                Duration::from_millis(150),
            ),
        )
        .await
        .unwrap();

        assert!(
            result.is_err(),
            "an idle connection must return Err once the read-idle deadline elapses"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_data_message_resets_the_idle_deadline() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _subscribe = socket.next().await.unwrap().unwrap();
            for _ in 0..3 {
                sleep(Duration::from_millis(100)).await;
                socket
                    .send(Message::Text(r#"{"folder_id":"f1","server_seq":1}"#.into()))
                    .await
                    .unwrap();
            }
            sleep(Duration::from_millis(200)).await;
        });
        let (tx, _rx) = mpsc::channel(8);

        let ws_url = format!("ws://{addr}/ws?token=secret");
        let folder_ids = vec!["f1".to_owned()];
        let client = tokio::spawn(async move {
            connect_and_forward_with_intervals(
                &ws_url,
                &folder_ids,
                tx,
                Duration::from_secs(10),
                Duration::from_millis(150),
            )
            .await
        });

        // A 150ms idle deadline would have already tripped once (well before
        // this 350ms mark) without each of the three 100ms-spaced messages
        // resetting it.
        sleep(Duration::from_millis(350)).await;
        assert!(
            !client.is_finished(),
            "each data message must reset the idle deadline, keeping the connection alive"
        );

        client.abort();
        server.abort();
    }

    #[tokio::test]
    async fn a_server_sent_ping_resets_the_idle_deadline() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _subscribe = socket.next().await.unwrap().unwrap();
            for _ in 0..3 {
                sleep(Duration::from_millis(100)).await;
                socket.send(Message::Ping(Vec::new())).await.unwrap();
            }
            sleep(Duration::from_millis(200)).await;
        });
        let (tx, _rx) = mpsc::channel(8);

        let ws_url = format!("ws://{addr}/ws?token=secret");
        let folder_ids = vec!["f1".to_owned()];
        let client = tokio::spawn(async move {
            connect_and_forward_with_intervals(
                &ws_url,
                &folder_ids,
                tx,
                Duration::from_secs(10),
                Duration::from_millis(150),
            )
            .await
        });

        sleep(Duration::from_millis(350)).await;
        assert!(
            !client.is_finished(),
            "a server-sent Ping keepalive must reset the idle deadline like any other frame"
        );

        client.abort();
        server.abort();
    }

    #[tokio::test]
    async fn catch_up_notification_sent_for_each_subscribed_folder_on_connect() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _subscribe_f1 = socket.next().await.unwrap().unwrap();
            let _subscribe_f2 = socket.next().await.unwrap().unwrap();
            sleep(Duration::from_millis(200)).await;
        });
        let (tx, mut rx) = mpsc::channel(4);

        let ws_url = format!("ws://{addr}/ws?token=secret");
        let folder_ids = vec!["f1".to_owned(), "f2".to_owned()];
        let client =
            tokio::spawn(async move { connect_and_forward(&ws_url, &folder_ids, tx).await });

        let first = timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
        let second = timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();

        assert_eq!(
            first,
            WsPushNotification { folder_id: "f1".into(), server_seq: 0 }
        );
        assert_eq!(
            second,
            WsPushNotification { folder_id: "f2".into(), server_seq: 0 }
        );

        client.abort();
        server.abort();
    }

    #[tokio::test]
    async fn catch_up_notification_is_sent_again_on_every_reconnect() {
        for _ in 0..2 {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = accept_async(stream).await.unwrap();
                let _subscribe = socket.next().await.unwrap().unwrap();
                socket.close(None).await.unwrap();
            });
            let (tx, mut rx) = mpsc::channel(4);

            let ws_url = format!("ws://{addr}/ws?token=secret");
            let folder_ids = vec!["f1".to_owned()];
            let client =
                tokio::spawn(async move { connect_and_forward(&ws_url, &folder_ids, tx).await });

            let notification = timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                notification,
                WsPushNotification { folder_id: "f1".into(), server_seq: 0 }
            );

            client.await.unwrap().unwrap();
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn nonzero_pre_connect_jitter_delays_the_first_connect_attempt() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let backend_url = format!("http://{addr}");
        let (tx, _rx) = mpsc::channel(4);

        let started_at = Instant::now();
        let loop_handle = tokio::spawn(async move {
            ws_push_loop(
                &backend_url,
                "secret",
                vec!["f1".to_owned()],
                tx,
                Duration::from_millis(300),
            )
            .await
        });

        let (_stream, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let elapsed = started_at.elapsed();

        loop_handle.abort();
        assert!(
            elapsed >= Duration::from_millis(250),
            "a non-zero pre-connect jitter must delay the first connect attempt, elapsed={elapsed:?}"
        );
    }

    #[tokio::test]
    async fn zero_pre_connect_jitter_connects_immediately() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let backend_url = format!("http://{addr}");
        let (tx, _rx) = mpsc::channel(4);

        let started_at = Instant::now();
        let loop_handle = tokio::spawn(async move {
            ws_push_loop(
                &backend_url,
                "secret",
                vec!["f1".to_owned()],
                tx,
                Duration::ZERO,
            )
            .await
        });

        let (_stream, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let elapsed = started_at.elapsed();

        loop_handle.abort();
        assert!(
            elapsed < Duration::from_millis(200),
            "a zero pre-connect jitter must connect immediately, elapsed={elapsed:?}"
        );
    }

    #[tokio::test]
    async fn pre_connect_jitter_does_not_delay_a_reconnect() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let backend_url = format!("http://{addr}");
        let (tx, _rx) = mpsc::channel(4);

        // Well above the fixed 2s reconnect backoff: if a bug re-applied
        // this jitter on reconnect, elapsed would land near 2s+3s=5s, far
        // past the 3s assertion threshold below - a small jitter close to
        // the backoff (e.g. 300ms) can't reliably distinguish "backoff
        // only" from "backoff plus jitter" against the same threshold.
        let pre_connect_jitter = Duration::from_secs(3);

        let loop_handle = tokio::spawn(async move {
            ws_push_loop(
                &backend_url,
                "secret",
                vec!["f1".to_owned()],
                tx,
                pre_connect_jitter,
            )
            .await
        });

        let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let _subscribe = socket.next().await.unwrap().unwrap();
        socket.close(None).await.unwrap();
        drop(socket);

        // ws_push_loop's reconnect backoff is a fixed 2s; this must not be
        // additionally delayed by the pre-connect jitter a second time.
        let reconnect_started_at = Instant::now();
        let (_stream2, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let elapsed = reconnect_started_at.elapsed();

        loop_handle.abort();
        assert!(
            elapsed < Duration::from_secs(3),
            "a reconnect must not re-apply the pre-connect jitter on top of the 2s backoff, elapsed={elapsed:?}"
        );
    }
}
