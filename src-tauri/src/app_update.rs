use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_INSTALLER_BYTES: u64 = 80 * 1024 * 1024;
const RELEASE_HOST: &str = "github.com";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSource {
    github_owner: String,
    github_repo: String,
}

fn update_source() -> &'static UpdateSource {
    static SOURCE: OnceLock<UpdateSource> = OnceLock::new();
    SOURCE.get_or_init(|| {
        serde_json::from_str(include_str!("../update-source.json"))
            .expect("src-tauri/update-source.json is invalid")
    })
}

fn releases_api_url() -> String {
    let source = update_source();
    format!(
        "https://api.github.com/repos/{}/{}/releases?per_page=20",
        source.github_owner, source.github_repo
    )
}

fn release_path_prefix() -> String {
    let source = update_source();
    format!("/{}/{}/releases/", source.github_owner, source.github_repo)
}

fn github_release_base() -> String {
    let source = update_source();
    format!(
        "https://github.com/{}/{}",
        source.github_owner, source.github_repo
    )
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateStatus {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub skipped: bool,
    pub release_notes: String,
    pub release_url: String,
    pub download_url: String,
    pub asset_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SemVer {
    major: u64,
    minor: u64,
    patch: u64,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn validate_version_string(version: &str) -> Result<(), String> {
    parse_semver(version)
        .map(|_| ())
        .ok_or_else(|| "update version is invalid".to_string())
}

pub async fn check_for_update(
    current: &str,
    skipped_version: Option<&str>,
) -> Result<AppUpdateStatus, String> {
    let releases = fetch_releases().await?;
    Ok(status_from_releases(
        current,
        &releases,
        skipped_version,
        current_asset_marker(),
    ))
}

pub fn status_from_releases(
    current: &str,
    releases: &[GithubRelease],
    skipped_version: Option<&str>,
    asset_marker: &str,
) -> AppUpdateStatus {
    let current_version = current.trim().trim_start_matches('v').to_string();
    let Some(offer) = select_newer_release(&current_version, releases, asset_marker) else {
        return AppUpdateStatus {
            current_version: current_version.clone(),
            latest_version: current_version,
            update_available: false,
            skipped: false,
            release_notes: String::new(),
            release_url: String::new(),
            download_url: String::new(),
            asset_name: None,
        };
    };
    let skipped = skipped_version
        .and_then(parse_semver)
        .zip(parse_semver(&offer.latest_version))
        .is_some_and(|(skipped, latest)| skipped == latest);
    AppUpdateStatus { skipped, ..offer }
}

pub fn open_release_url(url: &str) -> Result<(), String> {
    let allowed = validate_release_url(url)?;
    open_https_url(&allowed)
}

async fn fetch_releases() -> Result<Vec<GithubRelease>, String> {
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .http1_only()
        .tcp_nodelay(true)
        .user_agent(format!("codex-provider-switcher/{}", current_version()))
        .build()
        .map_err(|_| "could not create the update client".to_string())?;
    let response = client
        .get(releases_api_url())
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "update request timed out".to_string()
            } else {
                "could not check for updates".to_string()
            }
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("update endpoint returned HTTP {}", status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("update response is too large".to_string());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "could not read the update response".to_string())?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("update response is too large".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<Vec<GithubRelease>>(&bytes)
        .map_err(|_| "update endpoint returned an unsupported response".to_string())
}

fn select_newer_release(
    current: &str,
    releases: &[GithubRelease],
    asset_marker: &str,
) -> Option<AppUpdateStatus> {
    let current_version = parse_semver(current)?;
    let mut best: Option<(SemVer, &GithubRelease)> = None;
    for release in releases {
        if release.draft {
            continue;
        }
        let Some(version) = parse_semver(&release.tag_name) else {
            continue;
        };
        if version <= current_version {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(best_version, _)| version > *best_version)
        {
            best = Some((version, release));
        }
    }
    let (version, release) = best?;
    let asset = release
        .assets
        .iter()
        .find(|asset| !asset_marker.is_empty() && asset.name.contains(asset_marker));
    let release_url = validate_release_url(&release.html_url).ok()?;
    let download_url = asset
        .and_then(|asset| validate_release_url(&asset.browser_download_url).ok())
        .unwrap_or_else(|| release_url.clone());
    Some(AppUpdateStatus {
        current_version: format_semver(current_version),
        latest_version: format_semver(version),
        update_available: true,
        skipped: false,
        release_notes: sanitize_notes(release.body.as_deref().unwrap_or_default()),
        release_url,
        download_url,
        asset_name: asset.map(|asset| asset.name.clone()),
    })
}

fn current_asset_marker() -> &'static str {
    if cfg!(windows) {
        "Windows-x64-Setup.exe"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macOS-arm64.dmg"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "macOS-x64.dmg"
    } else {
        ""
    }
}

fn parse_semver(value: &str) -> Option<SemVer> {
    let core = value
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or_default();
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(SemVer {
        major,
        minor,
        patch,
    })
}

fn format_semver(version: SemVer) -> String {
    format!("{}.{}.{}", version.major, version.minor, version.patch)
}

fn sanitize_notes(body: &str) -> String {
    let trimmed = body.replace('\r', "").trim().to_string();
    if trimmed.chars().count() <= 800 {
        trimmed
    } else {
        let mut truncated: String = trimmed.chars().take(800).collect();
        truncated.push('\u{2026}');
        truncated
    }
}

fn validate_release_url(value: &str) -> Result<String, String> {
    let parsed = Url::parse(value.trim()).map_err(|_| "update URL is invalid".to_string())?;
    if parsed.scheme() != "https"
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.host_str() != Some(RELEASE_HOST)
        || !parsed.path().starts_with(release_path_prefix().as_str())
    {
        return Err("update URL is not an allowed GitHub release link".to_string());
    }
    Ok(parsed.as_str().to_string())
}

fn open_https_url(url: &str) -> Result<(), String> {
    let result = {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            std::process::Command::new("cmd")
                .args(["/C", "start", "", url])
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = url;
            Err(std::io::Error::other("opening updates is unsupported"))
        }
    };
    result
        .map(|_| ())
        .map_err(|_| "could not open the update page".to_string())
}

pub async fn download_and_launch_installer(
    url: &str,
    asset_name: Option<&str>,
) -> Result<(), String> {
    let download_url = validate_download_url(url)?;
    let named = match asset_name {
        Some(name) => Some(sanitize_asset_name(name)?),
        None => None,
    };
    let url_name = url_asset_name(&download_url)?;
    let asset = match named {
        Some(name) if name != url_name => {
            return Err("update asset name does not match the download URL".to_string());
        }
        Some(name) => name,
        None => url_name,
    };
    let installer_path = download_installer(&download_url, &asset).await?;
    launch_installer_after_exit(&installer_path)
}

fn validate_download_url(value: &str) -> Result<String, String> {
    let parsed = Url::parse(value.trim()).map_err(|_| "update URL is invalid".to_string())?;
    let download_prefix = format!("{}download/", release_path_prefix());
    if parsed.scheme() != "https"
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.host_str() != Some(RELEASE_HOST)
        || !parsed.path().starts_with(download_prefix.as_str())
    {
        return Err("update URL is not an allowed GitHub installer link".to_string());
    }
    Ok(parsed.as_str().to_string())
}

fn allowed_redirect_url(url: &Url) -> bool {
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some()
    {
        return false;
    }
    match url.host_str() {
        Some("github.com") => url.path().starts_with(release_path_prefix().as_str()),
        Some(host) => {
            host.ends_with(".githubusercontent.com")
                && !host.starts_with('.')
                && host != ".githubusercontent.com"
        }
        None => false,
    }
}

fn sanitize_asset_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name == "."
        || name == ".."
        || name.contains("..")
    {
        return Err("update asset name is invalid".to_string());
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err("update asset name is invalid".to_string());
    }
    let expected_ext = if cfg!(windows) {
        ".exe"
    } else if cfg!(target_os = "macos") {
        ".dmg"
    } else {
        return Err("installing updates is unsupported on this platform".to_string());
    };
    if !name.ends_with(expected_ext) {
        return Err("update asset type is not an installer for this platform".to_string());
    }
    Ok(name.to_string())
}

fn url_asset_name(url: &str) -> Result<String, String> {
    let parsed = Url::parse(url).map_err(|_| "update URL is invalid".to_string())?;
    let name = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "update URL is missing an installer file name".to_string())?;
    sanitize_asset_name(name)
}

async fn download_installer(url: &str, asset_name: &str) -> Result<PathBuf, String> {
    let redirect = Policy::custom(|attempt| {
        if attempt.previous().len() >= 8 {
            return attempt.error("too many redirects");
        }
        if allowed_redirect_url(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("update download redirected to a disallowed host")
        }
    });
    let client = reqwest::Client::builder()
        .redirect(redirect)
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_REQUEST_TIMEOUT)
        .http1_only()
        .tcp_nodelay(true)
        .user_agent(format!("codex-provider-switcher/{}", current_version()))
        .build()
        .map_err(|_| "could not create the update client".to_string())?;
    let response = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "update download timed out".to_string()
            } else {
                "could not download the installer".to_string()
            }
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "update download returned HTTP {}",
            response.status().as_u16()
        ));
    }
    if !allowed_redirect_url(response.url()) {
        return Err("update download redirected to a disallowed host".to_string());
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_INSTALLER_BYTES)
    {
        return Err("update installer is too large".to_string());
    }
    let path = std::env::temp_dir().join(format!(
        "lm-codex-switch-update-{}-{}",
        std::process::id(),
        asset_name
    ));
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    let mut file = File::create(&path).map_err(|_| "could not stage the installer".to_string())?;
    let mut bytes = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "could not read the installer download".to_string())?;
        bytes = bytes.saturating_add(chunk.len() as u64);
        if bytes > MAX_INSTALLER_BYTES {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err("update installer is too large".to_string());
        }
        if file.write_all(&chunk).is_err() {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err("could not write the installer".to_string());
        }
    }
    if file.sync_all().is_err() {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err("could not finalize the installer".to_string());
    }
    drop(file);
    if let Err(error) = verify_installer_bytes(&path) {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

fn verify_installer_bytes(path: &Path) -> Result<(), String> {
    let metadata =
        std::fs::metadata(path).map_err(|_| "could not read the installer".to_string())?;
    if metadata.len() < 64 {
        return Err("downloaded installer is empty or truncated".to_string());
    }
    let mut file = File::open(path).map_err(|_| "could not read the installer".to_string())?;
    let mut magic = [0u8; 2];
    file.read_exact(&mut magic)
        .map_err(|_| "could not read the installer".to_string())?;
    #[cfg(windows)]
    {
        if magic != *b"MZ" {
            return Err("downloaded installer is not a Windows executable".to_string());
        }
    }
    #[cfg(target_os = "macos")]
    {
        if magic[0] == b'<' {
            return Err("downloaded installer is not a disk image".to_string());
        }
    }
    Ok(())
}

fn launch_installer_after_exit(path: &Path) -> Result<(), String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "installer path is not valid UTF-8".to_string())?;
    let pid = std::process::id();
    let result = {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            let escaped = path_str.replace('\'', "''");
            let script = format!(
                "while (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 250 }}; Start-Process -FilePath '{escaped}'"
            );
            std::process::Command::new("powershell")
                .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
                .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
        #[cfg(target_os = "macos")]
        {
            let escaped = path_str.replace('\'', "'\\''");
            let script = format!(
                "while kill -0 {pid} 2>/dev/null; do sleep 0.25; done; open '{escaped}'"
            );
            std::process::Command::new("/bin/sh")
                .args(["-c", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (path_str, pid);
            Err(std::io::Error::other("installing updates is unsupported"))
        }
    };
    result
        .map(|_| ())
        .map_err(|_| "could not launch the installer".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, draft: bool, asset: &str) -> GithubRelease {
        GithubRelease {
            tag_name: tag.to_string(),
            draft,
            html_url: format!("{}/releases/tag/{tag}", github_release_base()),
            body: Some(format!("notes for {tag}")),
            assets: vec![GithubAsset {
                name: format!("Codex.Provider.Switcher_{asset}"),
                browser_download_url: format!(
                    "{}/releases/download/{tag}/Codex.Provider.Switcher_{asset}",
                    github_release_base()
                ),
            }],
        }
    }

    #[test]
    fn newer_prerelease_is_offered_and_skipped_version_is_flagged() {
        let releases = vec![
            release("v0.3.5", false, "0.3.5_Windows-x64-Setup.exe"),
            release("v0.3.4", false, "0.3.4_Windows-x64-Setup.exe"),
        ];
        let available = status_from_releases("0.3.4", &releases, None, "Windows-x64-Setup.exe");
        assert!(available.update_available);
        assert!(!available.skipped);
        assert_eq!(available.latest_version, "0.3.5");
        assert!(available.download_url.contains("Windows-x64-Setup.exe"));

        let skipped =
            status_from_releases("0.3.4", &releases, Some("0.3.5"), "Windows-x64-Setup.exe");
        assert!(skipped.update_available);
        assert!(skipped.skipped);
    }

    #[test]
    fn drafts_and_older_tags_are_ignored() {
        let releases = vec![
            release("v0.4.0", true, "0.4.0_Windows-x64-Setup.exe"),
            release("v0.3.3", false, "0.3.3_Windows-x64-Setup.exe"),
        ];
        let status = status_from_releases("0.3.4", &releases, None, "Windows-x64-Setup.exe");
        assert!(!status.update_available);
        assert_eq!(status.latest_version, "0.3.4");
    }

    #[test]
    fn untrusted_urls_are_rejected() {
        assert!(validate_release_url("https://evil.example/releases/tag/v1").is_err());
        assert!(
            validate_release_url(&format!("{}/releases/tag/v0.3.5", github_release_base())).is_ok()
        );
        assert!(
            validate_release_url(&format!(
                "https://user:pass@github.com/{}/{}/releases/tag/v0.3.5",
                update_source().github_owner,
                update_source().github_repo
            ))
            .is_err()
        );
    }

    #[test]
    fn installer_download_urls_are_restricted_to_release_assets() {
        let ok = format!(
            "{}/releases/download/v0.3.7/Codex.Provider.Switcher_0.3.7_Windows-x64-Setup.exe",
            github_release_base()
        );
        assert!(validate_download_url(&ok).is_ok());
        assert!(
            validate_download_url(&format!("{}/releases/tag/v0.3.7", github_release_base())).is_err()
        );
        assert!(validate_download_url("https://evil.example/releases/download/v0.3.7/a.exe").is_err());
        assert!(allowed_redirect_url(
            &Url::parse("https://release-assets.githubusercontent.com/objects/abc").unwrap()
        ));
        assert!(!allowed_redirect_url(
            &Url::parse("https://evil.githubusercontent.com.example/objects/abc").unwrap()
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_installer_asset_names_are_sanitized() {
        assert_eq!(
            sanitize_asset_name("Codex.Provider.Switcher_0.3.7_Windows-x64-Setup.exe").unwrap(),
            "Codex.Provider.Switcher_0.3.7_Windows-x64-Setup.exe"
        );
        assert!(sanitize_asset_name("../evil.exe").is_err());
        assert!(sanitize_asset_name("payload.dll").is_err());
        assert!(sanitize_asset_name("foo bar.exe").is_err());
    }
}
