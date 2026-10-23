use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode};
use serde_json::Value;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES_URL: &str = "https://github.com/cebdan/mcp-commander/releases";
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/cebdan/mcp-commander/releases/latest";

#[derive(Debug, PartialEq, Eq)]
pub enum UpdateCheck {
    NoRelease,
    Available { version: String, url: String },
    UpToDate { version: String },
    AheadOfRelease { version: String },
    UnknownVersion { tag: String, url: String },
}

pub async fn check() -> Result<UpdateCheck> {
    check_at(LATEST_RELEASE_API).await
}

async fn check_at(api_url: &str) -> Result<UpdateCheck> {
    let client = Client::builder().timeout(Duration::from_secs(15)).build()?;
    let response = client.get(api_url)
        .header("User-Agent", "MCP-Commander-Update-Check")
        .header("Accept", "application/vnd.github+json")
        .send().await.context("Cannot reach GitHub")?;
    if response.status() == StatusCode::NOT_FOUND { return Ok(UpdateCheck::NoRelease); }
    if !response.status().is_success() {
        bail!("GitHub returned HTTP {}", response.status());
    }
    let body: Value = response.json().await.context("GitHub returned invalid release data")?;
    let tag = body["tag_name"].as_str().context("GitHub release has no tag")?;
    let url = body["html_url"].as_str().unwrap_or(RELEASES_URL);
    Ok(classify(tag, url, CURRENT_VERSION))
}

fn classify(tag: &str, url: &str, installed: &str) -> UpdateCheck {
    let release_url = if url.starts_with("https://github.com/cebdan/mcp-commander/releases/") {
        url.to_owned()
    } else {
        RELEASES_URL.to_owned()
    };
    let Some(release) = parse_version(tag) else {
        return UpdateCheck::UnknownVersion { tag: tag.into(), url: release_url };
    };
    let Some(current) = parse_version(installed) else {
        return UpdateCheck::UnknownVersion { tag: tag.into(), url: release_url };
    };
    if release > current {
        UpdateCheck::Available { version: tag.into(), url: release_url }
    } else if release == current {
        UpdateCheck::UpToDate { version: tag.into() }
    } else {
        UpdateCheck::AheadOfRelease { version: tag.into() }
    }
}

fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    let version = tag.trim().strip_prefix('v').or_else(|| tag.trim().strip_prefix('V'))
        .unwrap_or(tag.trim());
    let parts: Vec<_> = version.split('.').collect();
    if !(2..=3).contains(&parts.len()) { return None; }
    let major = parts[0].parse().ok()?;
    let minor = parts[1].parse().ok()?;
    let patch = if parts.len() == 3 { parts[2].parse().ok()? } else { 0 };
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::{Read, Write}, net::TcpListener};

    #[test]
    fn compares_two_or_three_part_release_tags() {
        let url = "https://github.com/cebdan/mcp-commander/releases/tag/v2026.55";
        assert!(matches!(classify("v2026.55", url, "2026.54.0"),
            UpdateCheck::Available { version, .. } if version == "v2026.55"));
        assert_eq!(classify("2026.54", url, "2026.54.0"),
            UpdateCheck::UpToDate { version: "2026.54".into() });
        assert_eq!(classify("v2026.53.2", url, "2026.54.0"),
            UpdateCheck::AheadOfRelease { version: "v2026.53.2".into() });
    }

    #[test]
    fn unknown_release_tags_do_not_claim_an_update() {
        let result = classify("preview-build", "https://other.example/release", "2026.54.0");
        assert_eq!(result, UpdateCheck::UnknownVersion {
            tag: "preview-build".into(), url: RELEASES_URL.into() });
    }

    #[tokio::test]
    async fn github_404_means_no_published_release() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") { break; }
            }
            let request = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
            assert!(request.contains("user-agent: mcp-commander-update-check"));
            let body = r#"{"message":"Not Found"}"#;
            write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        assert_eq!(check_at(&format!("http://{address}")).await.unwrap(), UpdateCheck::NoRelease);
        server.join().unwrap();
    }
}
