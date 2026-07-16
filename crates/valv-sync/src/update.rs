
use std::{
    env, fs,
    path::PathBuf,
    process::Command as ProcessCommand,
};

use anyhow::{anyhow, Context, Result};
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const DEFAULT_REPO: &str = "DRNKNDev/valv";

pub const MINISIGN_PUBLIC_KEY: &str = "RWRgHhzVeIwqVsZrfOb3oGNC7TMurQXTgq63Yr0gFk5HuUfhBZr6dqxZ";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Cli,
    Valvd,
}

impl Component {
    pub fn tag_prefix(self) -> &'static str {
        match self {
            Component::Cli => "cli-v",
            Component::Valvd => "valvd-v",
        }
    }
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
}

const RELEASES_PAGE_SIZE: u32 = 100;
const MAX_RELEASES_PAGES: u32 = 20;

pub async fn resolve_latest_version(
    client: &reqwest::Client,
    repo: &str,
    component: Component,
    pinned_version_env: &str,
) -> Result<String> {
    resolve_latest_version_from(client, GITHUB_API_BASE, repo, component, pinned_version_env).await
}

const GITHUB_API_BASE: &str = "https://api.github.com";

async fn resolve_latest_version_from(
    client: &reqwest::Client,
    api_base: &str,
    repo: &str,
    component: Component,
    pinned_version_env: &str,
) -> Result<String> {
    if let Ok(pinned) = std::env::var(pinned_version_env) {
        if !pinned.is_empty() {
            return Ok(pinned.trim_start_matches('v').to_owned());
        }
    }

    let prefix = component.tag_prefix();
    let mut best: Option<([u64; 3], String)> = None;

    for page in 1..=MAX_RELEASES_PAGES {
        let url = format!(
            "{api_base}/repos/{repo}/releases?per_page={RELEASES_PAGE_SIZE}&page={page}"
        );
        let response = client
            .get(&url)
            .header("User-Agent", "valv-update-check")
            .send()
            .await
            .with_context(|| format!("failed to list releases from {url}"))?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "GitHub releases listing for {repo} returned {}",
                response.status()
            ));
        }
        let releases: Vec<GithubRelease> = response
            .json()
            .await
            .context("failed to parse GitHub releases listing response")?;
        let page_len = releases.len();

        for release in releases {
            let Some(version) = release.tag_name.strip_prefix(prefix) else {
                continue;
            };
            if version.is_empty() {
                continue;
            }
            let Some(parsed) = parse_semver_prefix(version) else {
                continue;
            };
            let is_better = best.as_ref().map_or(true, |(current, _)| parsed > *current);
            if is_better {
                best = Some((parsed, version.to_owned()));
            }
        }

        if page_len < RELEASES_PAGE_SIZE as usize {
            break;
        }
    }

    let (_, version) = best
        .ok_or_else(|| anyhow!("no {prefix}* release found for {repo}"))?;
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

pub fn component_asset_name(binary_name: &str, version: &str, target: &str) -> String {
    format!("{binary_name}-{version}-{target}.tar.gz")
}

pub fn component_release_base(repo: &str, component: Component, version: &str) -> String {
    format!(
        "https://github.com/{repo}/releases/download/{}{version}",
        component.tag_prefix()
    )
}

pub fn detect_target(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        (os, arch) => Err(anyhow!(
            "unsupported platform {os}/{arch}; supported targets are macOS arm64 and Linux x86_64"
        )),
    }
}

pub async fn download_release_asset(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .header("User-Agent", "valv-update-check")
        .send()
        .await
        .with_context(|| format!("failed to download {url}"))?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "failed to download {url}: HTTP {}",
            response.status()
        ));
    }
    Ok(response
        .bytes()
        .await
        .with_context(|| format!("failed to read response body from {url}"))?
        .to_vec())
}

pub fn verify_tarball_checksum(
    asset: &str,
    tarball_bytes: &[u8],
    sha256sums_bytes: &[u8],
) -> Result<()> {
    let sha256sums =
        std::str::from_utf8(sha256sums_bytes).context("SHA256SUMS is not valid UTF-8")?;
    let expected = checksum_for_asset(sha256sums, asset)
        .ok_or_else(|| anyhow!("SHA256SUMS does not contain {asset}"))?;
    let actual = sha256_hex(tarball_bytes);
    if actual != expected {
        return Err(anyhow!(
            "checksum mismatch for {asset}: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn checksum_for_asset(sha256sums: &str, asset: &str) -> Option<String> {
    for line in sha256sums.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if name == asset || name == format!("*{asset}") {
            return Some(hash.to_owned());
        }
    }
    None
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn extract_tarball(tarball_bytes: &[u8]) -> Result<PathBuf> {
    let extract_dir =
        env::temp_dir().join(format!("valv-update-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&extract_dir)
        .with_context(|| format!("failed to create {}", extract_dir.display()))?;
    let tarball_path = extract_dir.join("download.tar.gz");
    fs::write(&tarball_path, tarball_bytes)
        .with_context(|| format!("failed to write {}", tarball_path.display()))?;

    let status = ProcessCommand::new("tar")
        .arg("-xzf")
        .arg(&tarball_path)
        .arg("-C")
        .arg(&extract_dir)
        .status()
        .context("failed to run tar")?;
    if !status.success() {
        return Err(anyhow!("tar extraction failed with status {status}"));
    }
    let _ = fs::remove_file(&tarball_path);
    Ok(extract_dir)
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

    async fn releases_list_server(status_line: &str, body: &str) -> String {
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

        let version = resolve_latest_version(
            &client,
            "DRNKNDev/valv",
            Component::Cli,
            "VALV_UPDATE_TEST_PIN",
        )
        .await
        .unwrap();

        std::env::remove_var("VALV_UPDATE_TEST_PIN");
        assert_eq!(version, "9.9.9");
    }

    #[tokio::test]
    async fn resolve_latest_version_ignores_an_empty_pin_and_resolves_live() {
        std::env::set_var("VALV_UPDATE_TEST_EMPTY_PIN", "");
        let base_url = releases_list_server("200 OK", r#"[{"tag_name":"cli-v0.6.1"}]"#).await;
        let client = reqwest::Client::new();

        let version = resolve_latest_version_from(
            &client,
            &base_url,
            "test/repo",
            Component::Cli,
            "VALV_UPDATE_TEST_EMPTY_PIN",
        )
        .await
        .unwrap();

        std::env::remove_var("VALV_UPDATE_TEST_EMPTY_PIN");
        assert_eq!(version, "0.6.1");
    }

    #[test]
    fn github_release_tag_name_strips_component_prefix() {
        let release: GithubRelease = serde_json::from_str(r#"{"tag_name":"cli-v1.2.3"}"#).unwrap();
        assert_eq!(
            release.tag_name.strip_prefix(Component::Cli.tag_prefix()),
            Some("1.2.3")
        );
    }

    #[tokio::test]
    async fn resolve_latest_version_from_parses_tag_name_and_strips_prefix() {
        let base_url = releases_list_server("200 OK", r#"[{"tag_name":"cli-v0.4.2"}]"#).await;
        let client = reqwest::Client::new();

        let version = resolve_latest_version_from(
            &client,
            &base_url,
            "test/repo",
            Component::Cli,
            "VALV_UPDATE_TEST_UNSET_A",
        )
        .await
        .unwrap();

        assert_eq!(version, "0.4.2");
    }

    #[tokio::test]
    async fn resolve_latest_version_from_picks_the_highest_semver_matching_the_prefix() {
        let base_url = releases_list_server(
            "200 OK",
            r#"[{"tag_name":"valvd-v0.5.0"},{"tag_name":"cli-v0.3.1"},{"tag_name":"cli-v0.4.0"},{"tag_name":"cli-v0.2.9"}]"#,
        )
        .await;
        let client = reqwest::Client::new();

        let version = resolve_latest_version_from(
            &client,
            &base_url,
            "test/repo",
            Component::Cli,
            "VALV_UPDATE_TEST_UNSET_C",
        )
        .await
        .unwrap();

        assert_eq!(version, "0.4.0");
    }

    #[tokio::test]
    async fn resolve_latest_version_from_ignores_non_matching_prefixes() {
        let base_url =
            releases_list_server("200 OK", r#"[{"tag_name":"cli-v0.4.0"},{"tag_name":"macos-v1.0.0"}]"#)
                .await;
        let client = reqwest::Client::new();

        let version = resolve_latest_version_from(
            &client,
            &base_url,
            "test/repo",
            Component::Valvd,
            "VALV_UPDATE_TEST_UNSET_D",
        )
        .await;

        assert!(version.is_err());
    }

    #[tokio::test]
    async fn resolve_latest_version_from_errors_on_non_success_status() {
        let base_url = releases_list_server("404 Not Found", r#"{"message":"Not Found"}"#).await;
        let client = reqwest::Client::new();

        assert!(resolve_latest_version_from(
            &client,
            &base_url,
            "test/repo",
            Component::Cli,
            "VALV_UPDATE_TEST_UNSET_B"
        )
        .await
        .is_err());
    }

    #[test]
    fn component_asset_name_uses_binary_prefix() {
        assert_eq!(
            component_asset_name("valvd", "0.3.1", "aarch64-apple-darwin"),
            "valvd-0.3.1-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn component_release_base_uses_tag_prefix() {
        assert_eq!(
            component_release_base("DRNKNDev/valv", Component::Valvd, "0.3.1"),
            "https://github.com/DRNKNDev/valv/releases/download/valvd-v0.3.1"
        );
    }

    #[test]
    fn detect_target_maps_supported_platforms() {
        assert_eq!(
            detect_target("macos", "aarch64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            detect_target("linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert!(detect_target("windows", "x86_64").is_err());
    }

    #[test]
    fn verify_tarball_checksum_rejects_a_tampered_download() {
        let tarball_bytes = b"real tarball bytes";
        let expected = sha256_hex(tarball_bytes);
        let sha256sums = format!("{expected}  valvd-0.1.0-aarch64-apple-darwin.tar.gz\n");

        assert!(verify_tarball_checksum(
            "valvd-0.1.0-aarch64-apple-darwin.tar.gz",
            tarball_bytes,
            sha256sums.as_bytes()
        )
        .is_ok());

        let tampered = b"tampered tarball bytes";
        assert!(verify_tarball_checksum(
            "valvd-0.1.0-aarch64-apple-darwin.tar.gz",
            tampered,
            sha256sums.as_bytes()
        )
        .is_err());
    }
}
