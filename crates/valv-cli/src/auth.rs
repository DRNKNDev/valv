use std::{
    fs, future::Future, io::ErrorKind, path::Path, process::Command as ProcessCommand,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use url::form_urlencoded;
use uuid::Uuid;

use crate::paths::config_path;

const AUTH_LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone)]
pub(crate) struct AuthLoginArgs {
    pub(crate) web_base_url: String,
    pub(crate) backend_url: String,
    pub(crate) device_name: String,
    pub(crate) open_browser: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairingCallback {
    pub(crate) device_id: String,
    pub(crate) device_token: String,
}

pub(crate) async fn cmd_auth_login(args: AuthLoginArgs) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("bind local auth callback listener")?;
    let port = listener.local_addr()?.port();
    let state = Uuid::new_v4().simple().to_string();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("device_flow", "1")
        .append_pair("device_name", &args.device_name)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("state", &state)
        .finish();
    let login_url = format!("{}/login?{query}", args.web_base_url.trim_end_matches('/'));

    if args.open_browser {
        open_browser(&login_url)?;
    }
    println!("Open this URL to sign in:\n{login_url}");

    let callback =
        wait_for_callback(accept_one_callback(listener, &state), AUTH_LOGIN_TIMEOUT).await?;
    write_config(
        &config_path()?,
        &args.backend_url,
        &callback.device_id,
        &callback.device_token,
        &args.device_name,
    )?;
    println!("Signed in as device {}", callback.device_id);
    Ok(())
}

async fn wait_for_callback<F>(callback: F, timeout_duration: Duration) -> Result<PairingCallback>
where
    F: Future<Output = Result<PairingCallback>>,
{
    match timeout(timeout_duration, callback).await {
        Ok(result) => result,
        Err(_) => {
            eprintln!("Timed out waiting for sign-in. Run `valv auth login` again.");
            Err(anyhow!("timed out waiting for sign-in"))
        }
    }
}

pub(crate) async fn accept_one_callback(
    listener: TcpListener,
    expected_state: &str,
) -> Result<PairingCallback> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buffer = vec![0; 4096];
        let n = stream.read(&mut buffer).await?;
        let request = String::from_utf8_lossy(&buffer[..n]);
        let request_line = request.lines().next().unwrap_or_default();
        let callback = parse_callback_request_line(request_line, expected_state);

        let (status, body) = if callback.is_ok() {
            (
                "200 OK",
                "<!doctype html><title>Valv signed in</title><p>You can close this tab.</p>",
            )
        } else {
            (
                "400 Bad Request",
                "<!doctype html><title>Valv sign-in failed</title><p>Invalid sign-in callback.</p>",
            )
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await?;
        if let Ok(callback) = callback {
            return Ok(callback);
        }
    }
}

fn parse_callback_request_line(
    request_line: &str,
    expected_state: &str,
) -> Result<PairingCallback> {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" {
        return Err(anyhow!("auth callback must use GET"));
    }
    let Some(query) = target.strip_prefix("/callback?") else {
        return Err(anyhow!("auth callback path must be /callback"));
    };
    let state = query_param(query, "state").ok_or_else(|| anyhow!("missing state"))?;
    if state != expected_state {
        return Err(anyhow!("invalid state"));
    }
    let device_id = query_param(query, "device_id").ok_or_else(|| anyhow!("missing device_id"))?;
    let device_token =
        query_param(query, "device_token").ok_or_else(|| anyhow!("missing device_token"))?;
    Ok(PairingCallback {
        device_id,
        device_token,
    })
}

fn query_param(query: &str, key: &str) -> Option<String> {
    form_urlencoded::parse(query.as_bytes())
        .find_map(|(name, value)| (name == key).then(|| value.into_owned()))
}

fn write_config(
    path: &Path,
    backend_url: &str,
    device_id: &str,
    device_token: &str,
    device_name: &str,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = format!(
        "backend_url = \"{}\"\ndevice_id = \"{}\"\ndevice_token = \"{}\"\ndevice_name = \"{}\"\nmounts = []\n",
        toml_escape(backend_url),
        toml_escape(device_id),
        toml_escape(device_token),
        toml_escape(device_name),
    );
    fs::write(path, contents)?;
    set_owner_only_permissions(path)?;
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
    let status = if cfg!(target_os = "macos") {
        ProcessCommand::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        ProcessCommand::new("cmd")
            .args(["/C", "start", "", url])
            .status()
    } else {
        ProcessCommand::new("xdg-open").arg(url).status()
    };

    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(anyhow!("failed to open browser: {status}")),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            println!("Could not find a browser opener; copy the URL above.");
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn default_web_base_url() -> String {
    std::env::var("VALV_WEB_BASE_URL").unwrap_or_else(|_| "https://valvsync.com".to_owned())
}

fn default_backend_url() -> String {
    std::env::var("VALV_BACKEND_URL").unwrap_or_else(|_| "https://api.valvsync.com".to_owned())
}

pub(crate) fn default_auth_login_args(open_browser: bool) -> AuthLoginArgs {
    AuthLoginArgs {
        web_base_url: default_web_base_url(),
        backend_url: default_backend_url(),
        device_name: default_device_name(),
        open_browser,
    }
}

fn default_device_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "Valv Device".to_owned())
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn set_owner_only_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    use super::*;

    #[tokio::test]
    async fn loopback_server_accepts_one_callback() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(accept_one_callback(listener, "expected-state"));

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /callback?device_id=device-1&device_token=token-1&state=expected-state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        let callback = handle.await.unwrap().unwrap();
        assert_eq!(
            callback,
            PairingCallback {
                device_id: "device-1".to_owned(),
                device_token: "token-1".to_owned(),
            }
        );
        assert!(response.starts_with("HTTP/1.1 200 OK"));
    }

    #[test]
    fn callback_parser_rejects_missing_or_wrong_state() {
        assert!(parse_callback_request_line(
            "GET /callback?device_id=device-1&device_token=token-1 HTTP/1.1",
            "expected-state"
        )
        .is_err());
        assert!(parse_callback_request_line(
            "GET /callback?device_id=device-1&device_token=token-1&state=wrong HTTP/1.1",
            "expected-state"
        )
        .is_err());
    }

    #[test]
    fn callback_parser_rejects_raw_multibyte_after_percent_without_panicking() {
        assert!(parse_callback_request_line(
            "GET /callback?state=%€&device_id=device-1&device_token=token-1 HTTP/1.1",
            "expected-state"
        )
        .is_err());
    }

    #[tokio::test]
    async fn callback_wait_times_out_instead_of_blocking_forever() {
        let started = std::time::Instant::now();
        let error = wait_for_callback(std::future::pending(), Duration::from_millis(10))
            .await
            .unwrap_err();

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(error.to_string().contains("timed out waiting for sign-in"));
    }
}
