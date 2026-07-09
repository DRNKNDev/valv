
use anyhow::{anyhow, Context, Result};
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;

pub const DEFAULT_REPO: &str = "DRNKNDev/valv";

// TODO(founder): replace placeholder before first signed release.
pub const MINISIGN_PUBLIC_KEY: &str =
    "RWQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

pub fn verify_sha256sums(sha256sums_bytes: &[u8], minisig_bytes: &[u8]) -> Result<()> {
    let public_key = PublicKey::from_base64(MINISIGN_PUBLIC_KEY)
        .context("embedded minisign public key is invalid")?;
    let minisig_str =
        std::str::from_utf8(minisig_bytes).context("SHA256SUMS.minisig is not valid UTF-8")?;
    let signature =
        Signature::decode(minisig_str).context("SHA256SUMS.minisig is malformed")?;
    public_key
        .verify(sha256sums_bytes, &signature, false)
        .context("SHA256SUMS.minisig does not verify against SHA256SUMS")?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
}

pub async fn resolve_latest_version(
    client: &reqwest::Client,
    repo: &str,
    pinned_version_env: &str,
) -> Result<String> {
    resolve_latest_version_from(client, GITHUB_API_BASE, repo, pinned_version_env).await
}

const GITHUB_API_BASE: &str = "https://api.github.com";

async fn resolve_latest_version_from(
    client: &reqwest::Client,
    api_base: &str,
    repo: &str,
    pinned_version_env: &str,
) -> Result<String> {
    if let Ok(pinned) = std::env::var(pinned_version_env) {
        if !pinned.is_empty() {
            return Ok(pinned.trim_start_matches('v').to_owned());
        }
    }

    let url = format!("{api_base}/repos/{repo}/releases/latest");
    let response = client
        .get(&url)
        .header("User-Agent", "valv-update-check")
        .send()
        .await
        .with_context(|| format!("failed to resolve latest release from {url}"))?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "GitHub releases/latest for {repo} returned {}",
            response.status()
        ));
    }
    let release = response
        .json::<GithubRelease>()
        .await
        .context("failed to parse GitHub releases/latest response")?;
    let version = release.tag_name.trim_start_matches('v').to_owned();
    if version.is_empty() {
        return Err(anyhow!(
            "GitHub releases/latest response for {repo} had an empty tag_name"
        ));
    }
    Ok(version)
}

pub fn is_newer_version(candidate: &str, current: &str) -> bool {
    let Some(candidate) = parse_semver_prefix(candidate) else {
        return false;
    };
    let Some(current) = parse_semver_prefix(current) else {
        return false;
    };
    candidate > current
}

fn parse_semver_prefix(version: &str) -> Option<[u64; 3]> {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some([major, minor, patch])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[test]
    fn is_newer_version_compares_numerically_not_lexically() {
        assert!(is_newer_version("0.10.0", "0.9.0"));
        assert!(!is_newer_version("0.8.9", "0.9.0"));
        assert!(!is_newer_version("0.9.0", "0.9.0"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
    }

    #[test]
    fn is_newer_version_ignores_prerelease_suffix() {
        assert!(is_newer_version("0.9.0-rc.1", "0.8.0"));
    }

    #[test]
    fn is_newer_version_treats_unparseable_versions_as_not_newer() {
        assert!(!is_newer_version("not-a-version", "0.9.0"));
        assert!(!is_newer_version("0.9.0", "not-a-version"));
    }

    const TEST_PUBLIC_KEY: &str = "RWTJbnV6v7BP0Vazx/pGTbh0nb4+kVJhyFzdhbmYylqnAfOvAPIEV8+0";
    const OTHER_PUBLIC_KEY: &str = "RWR59jSX4JA4yAhm7W6/tzpEsZ+K0yB9mO9W57oKYSzV09RKpW2dP4vn";
    const TEST_SHA256SUMS: &[u8] =
        b"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef  valv-0.1.0-aarch64-apple-darwin.tar.gz\n";
    const TEST_MINISIG: &str = "untrusted comment: signature from minisign secret key\nRUTJbnV6v7BP0eKeFWwZJyloRrHUnYgYWpQ0oR8orMJfMjhk47PFa8wWsm23qcmvZwuOtH7QSNVUdD4zD11LDDO+npkECmwseAQ=\ntrusted comment: timestamp:1783576619\tfile:SHA256SUMS\thashed\nT92OBjFXmhZqzzMJHFUcRtUBJoqwbhA2Byeni/D+dNmnnzWIKeNlHQffkyGh4KRtWXEf8Vzk6Ufcj9kvyT0TAg==\n";

    fn verify_with_key(public_key_b64: &str, sha256sums: &[u8], minisig: &str) -> Result<()> {
        let public_key = PublicKey::from_base64(public_key_b64).unwrap();
        let signature = Signature::decode(minisig).context("decode signature")?;
        public_key
            .verify(sha256sums, &signature, false)
            .context("verify")?;
        Ok(())
    }

    #[test]
    fn placeholder_public_key_constant_decodes() {
        assert!(PublicKey::from_base64(MINISIGN_PUBLIC_KEY).is_ok());
    }

    #[test]
    fn verify_sha256sums_accepts_a_genuine_signature() {
        assert!(verify_with_key(TEST_PUBLIC_KEY, TEST_SHA256SUMS, TEST_MINISIG).is_ok());
    }

    #[test]
    fn verify_sha256sums_rejects_a_tampered_file() {
        let tampered = b"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef  valv-0.1.0-x86_64-unknown-linux-gnu.tar.gz\n";
        assert!(verify_with_key(TEST_PUBLIC_KEY, tampered, TEST_MINISIG).is_err());
    }

    #[test]
    fn verify_sha256sums_rejects_the_wrong_key() {
        assert!(verify_with_key(OTHER_PUBLIC_KEY, TEST_SHA256SUMS, TEST_MINISIG).is_err());
    }

    #[test]
    fn verify_sha256sums_rejects_a_malformed_minisig() {
        assert!(verify_with_key(TEST_PUBLIC_KEY, TEST_SHA256SUMS, "not a minisig file").is_err());
    }

    #[test]
    fn verify_sha256sums_rejects_non_utf8_minisig_bytes() {
        let result = verify_sha256sums(TEST_SHA256SUMS, &[0xFF, 0xFE, 0x00, 0x01]);
        assert!(result.is_err());
    }

    async fn latest_release_server(status_line: &str, body: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let status_line = status_line.to_owned();
        let body = body.to_owned();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 2048];
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
    async fn resolve_latest_version_honors_the_pin_env_var() {
        std::env::set_var("VALV_UPDATE_TEST_PIN", "v9.9.9");
        let client = reqwest::Client::new();

        let version = resolve_latest_version(&client, "DRNKNDev/valv", "VALV_UPDATE_TEST_PIN")
            .await
            .unwrap();

        std::env::remove_var("VALV_UPDATE_TEST_PIN");
        assert_eq!(version, "9.9.9");
    }

    #[tokio::test]
    async fn resolve_latest_version_ignores_an_empty_pin_and_resolves_live() {
        std::env::set_var("VALV_UPDATE_TEST_EMPTY_PIN", "");
        let base_url = latest_release_server("200 OK", r#"{"tag_name":"v0.6.1"}"#).await;
        let client = reqwest::Client::new();

        let version = resolve_latest_version_from(
            &client,
            &base_url,
            "test/repo",
            "VALV_UPDATE_TEST_EMPTY_PIN",
        )
        .await
        .unwrap();

        std::env::remove_var("VALV_UPDATE_TEST_EMPTY_PIN");
        assert_eq!(version, "0.6.1");
    }

    #[test]
    fn github_release_tag_name_strips_leading_v() {
        let release: GithubRelease = serde_json::from_str(r#"{"tag_name":"v1.2.3"}"#).unwrap();
        assert_eq!(release.tag_name.trim_start_matches('v'), "1.2.3");
    }

    #[tokio::test]
    async fn resolve_latest_version_from_parses_tag_name_and_strips_v() {
        let base_url = latest_release_server("200 OK", r#"{"tag_name":"v0.4.2"}"#).await;
        let client = reqwest::Client::new();

        let version =
            resolve_latest_version_from(&client, &base_url, "test/repo", "VALV_UPDATE_TEST_UNSET_A")
                .await
                .unwrap();

        assert_eq!(version, "0.4.2");
    }

    #[tokio::test]
    async fn resolve_latest_version_from_errors_on_non_success_status() {
        let base_url = latest_release_server("404 Not Found", r#"{"message":"Not Found"}"#).await;
        let client = reqwest::Client::new();

        assert!(resolve_latest_version_from(
            &client,
            &base_url,
            "test/repo",
            "VALV_UPDATE_TEST_UNSET_B"
        )
        .await
        .is_err());
    }
}
