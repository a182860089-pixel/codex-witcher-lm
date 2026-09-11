//! Auto-detect system / local outbound HTTP(S) proxies and diagnose upstream reachability.
//!
//! Detection order (no manual selection required):
//! 1. Process environment variables
//! 2. User / machine environment variables (Windows registry)
//! 3. WinINET system proxy settings
//! 4. Common local proxy ports that are currently listening

use std::net::SocketAddr;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const PORT_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

const COMMON_LOCAL_PORTS: &[u16] = &[
    7890, 7891, 7892, 7893, 7897, 10808, 10809, 1080, 20171, 20170, 6152, 8118, 8888, 1087,
];

const DEFAULT_PROBE_HOST: &str = "https://lumingapi.store";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboundProxyStatus {
    pub detected: bool,
    pub proxy_url: Option<String>,
    pub source: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub listening: Option<bool>,
    pub listener_hint: Option<String>,
    pub candidates: Vec<LocalProxyCandidate>,
    pub summary: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalProxyCandidate {
    pub proxy_url: String,
    pub host: String,
    pub port: u16,
    pub listening: bool,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboundProbeResult {
    pub mode: String,
    pub url: String,
    pub ok: bool,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboundNetworkReport {
    pub proxy: OutboundProxyStatus,
    pub probe_base: String,
    pub direct: OutboundProbeResult,
    pub via_proxy: Option<OutboundProbeResult>,
    pub verdict: String,
    pub recommendations: Vec<String>,
    pub started_proxy: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnoseOutboundInput {
    pub probe_base_url: Option<String>,
    pub try_start: Option<bool>,
}

pub fn detect_outbound_proxy() -> OutboundProxyStatus {
    let mut candidates: Vec<LocalProxyCandidate> = Vec::new();

    if let Some(url) = env_proxy_url_from_process() {
        push_candidate(&mut candidates, &url, "process-env");
    }
    for (url, source) in env_proxy_urls_from_registry() {
        push_candidate(&mut candidates, &url, &source);
    }
    if let Some(url) = windows_ie_proxy_url() {
        push_candidate(&mut candidates, &url, "system-proxy");
    }

    for port in COMMON_LOCAL_PORTS {
        let listening = tcp_open("127.0.0.1", *port);
        if listening {
            let url = format!("http://127.0.0.1:{port}");
            push_candidate(&mut candidates, &url, "local-port-scan");
        }
    }

    // Prefer configured system/env proxy; otherwise first listening local candidate.
    let preferred = candidates
        .iter()
        .find(|c| c.source != "local-port-scan" && c.listening)
        .or_else(|| candidates.iter().find(|c| c.source != "local-port-scan"))
        .or_else(|| candidates.iter().find(|c| c.listening))
        .cloned();

    let listener_hint = preferred
        .as_ref()
        .and_then(|p| listener_process_hint(p.port));

    match preferred {
        Some(choice) => {
            let listening = Some(choice.listening);
            let summary = if choice.listening {
                format!("已自动检测到代理 {}", display_proxy(&choice.proxy_url))
            } else {
                format!(
                    "已检测到代理配置 {}，但端口未在监听",
                    display_proxy(&choice.proxy_url)
                )
            };
            let detail = format!(
                "来源: {}。本机扫描到 {} 个候选。{}",
                source_label(&choice.source),
                candidates.len(),
                listener_hint
                    .as_deref()
                    .map(|h| format!("监听进程提示: {h}"))
                    .unwrap_or_default()
            );
            OutboundProxyStatus {
                detected: true,
                proxy_url: Some(choice.proxy_url.clone()),
                source: choice.source.clone(),
                host: Some(choice.host.clone()),
                port: Some(choice.port),
                listening,
                listener_hint,
                candidates,
                summary,
                detail,
            }
        }
        None => OutboundProxyStatus {
            detected: false,
            proxy_url: None,
            source: "none".to_string(),
            host: None,
            port: None,
            listening: None,
            listener_hint: None,
            candidates,
            summary: "未检测到系统代理或本机常见代理端口".to_string(),
            detail: "已检查环境变量、系统代理设置，以及 7890/10809/1080 等常见本地端口。"
                .to_string(),
        },
    }
}

pub async fn diagnose_outbound_network(input: DiagnoseOutboundInput) -> OutboundNetworkReport {
    let try_start = input.try_start.unwrap_or(false);
    let mut proxy = detect_outbound_proxy();
    let mut started_proxy = None;

    if try_start && proxy.listening != Some(true) {
        if let Some(msg) = try_start_common_proxy_apps() {
            started_proxy = Some(msg);
            // Re-detect after a short wait for the listener.
            tokio::time::sleep(Duration::from_millis(800)).await;
            proxy = detect_outbound_proxy();
        }
    }

    let probe_base = normalize_probe_base(
        input
            .probe_base_url
            .as_deref()
            .unwrap_or(DEFAULT_PROBE_HOST),
    );
    let models_url = format!("{probe_base}/v1/models");

    let direct = probe_models("direct", &models_url, None).await;
    let via_proxy = if let Some(url) = proxy.proxy_url.as_deref() {
        if proxy.listening == Some(false) {
            Some(OutboundProbeResult {
                mode: "via-proxy".to_string(),
                url: models_url.clone(),
                ok: false,
                status: None,
                latency_ms: 0,
                message: "代理端口未监听，跳过经代理探测".to_string(),
            })
        } else {
            Some(probe_models("via-proxy", &models_url, Some(url)).await)
        }
    } else {
        None
    };

    let (verdict, recommendations) = build_verdict(&proxy, &direct, via_proxy.as_ref());

    OutboundNetworkReport {
        proxy,
        probe_base,
        direct,
        via_proxy,
        verdict,
        recommendations,
        started_proxy,
    }
}

pub async fn ensure_outbound_proxy() -> OutboundNetworkReport {
    diagnose_outbound_network(DiagnoseOutboundInput {
        probe_base_url: None,
        try_start: Some(true),
    })
    .await
}

fn build_verdict(
    proxy: &OutboundProxyStatus,
    direct: &OutboundProbeResult,
    via_proxy: Option<&OutboundProbeResult>,
) -> (String, Vec<String>) {
    let mut tips = Vec::new();

    if !proxy.detected {
        tips.push("未找到系统代理。若目标站有 Cloudflare 限制，请先打开 Clash Verge / mihomo 等本地代理。".into());
        tips.push("打开后无需在本应用里手选节点：会自动读取系统代理或扫描 7890 等端口。".into());
        return ("未检测到出站代理；直连可能被目标站拦截。".into(), tips);
    }

    if proxy.listening == Some(false) {
        tips.push(format!(
            "系统写着代理 {}，但本地端口没在听——先启动代理客户端。",
            proxy.proxy_url.as_deref().unwrap_or("?")
        ));
        tips.push("可点「检测并尝试启动代理」；若自动启动失败，手动打开 Verge/Clash 即可。".into());
        return ("代理已配置但未运行".into(), tips);
    }

    match via_proxy {
        Some(via) if via.ok => {
            if direct.ok {
                tips.push("直连与代理均可到达探测地址。".into());
                (
                    format!(
                        "代理可用（{}）。本地转发上游可走该代理。",
                        display_proxy(proxy.proxy_url.as_deref().unwrap_or(""))
                    ),
                    tips,
                )
            } else {
                tips.push(
                    "直连失败、经代理成功：该渠道需要走系统/本地代理（自动检测即可，不用手选）。"
                        .into(),
                );
                tips.push(
                    "注意：models 通了不代表推理可用；若 /v1/responses 返回 503，是上游服务问题。"
                        .into(),
                );
                (
                    format!(
                        "必须走代理，且当前代理可用（{}）。",
                        display_proxy(proxy.proxy_url.as_deref().unwrap_or(""))
                    ),
                    tips,
                )
            }
        }
        Some(via) => {
            tips.push(format!("经代理探测失败：{}", via.message));
            tips.push(
                "检查 mihomo/Clash 是否在线、规则是否放行目标域名、系统代理端口是否正确。".into(),
            );
            ("已检测到代理，但经代理访问探测地址失败".into(), tips)
        }
        None => {
            tips.push("没有可用的代理 URL。".into());
            ("未检测到可用代理".into(), tips)
        }
    }
}

async fn probe_models(mode: &str, url: &str, proxy_url: Option<&str>) -> OutboundProbeResult {
    let started = std::time::Instant::now();
    let client = match build_client(proxy_url) {
        Ok(client) => client,
        Err(message) => {
            return OutboundProbeResult {
                mode: mode.to_string(),
                url: url.to_string(),
                ok: false,
                status: None,
                latency_ms: 0,
                message,
            };
        }
    };

    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let latency_ms = started.elapsed().as_millis() as u64;
            let body = response.text().await.unwrap_or_default();
            let message = classify_probe_body(status, &body);
            // Reachable enough when we get API JSON rather than Cloudflare HTML.
            let ok = if is_cloudflare_block(status, &body) {
                false
            } else {
                // 401 API_KEY_REQUIRED means TLS + route work without a key.
                matches!(status, 200 | 401 | 404 | 429) || (status == 403 && body.contains("API"))
            };
            OutboundProbeResult {
                mode: mode.to_string(),
                url: url.to_string(),
                ok,
                status: Some(status),
                latency_ms,
                message,
            }
        }
        Err(error) => OutboundProbeResult {
            mode: mode.to_string(),
            url: url.to_string(),
            ok: false,
            status: None,
            latency_ms: started.elapsed().as_millis() as u64,
            message: shorten(&error.to_string(), 180),
        },
    }
}

fn build_client(proxy_url: Option<&str>) -> Result<Client, String> {
    let mut builder = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("codex-provider-switcher/0.3.5-outbound-probe");

    match proxy_url {
        Some(url) => {
            let proxy = reqwest::Proxy::all(url)
                .map_err(|e| format!("invalid proxy URL: {e}"))?
                .no_proxy(reqwest::NoProxy::from_string(
                    "127.0.0.1,localhost,::1,[::1]",
                ));
            builder = builder.proxy(proxy);
        }
        None => {
            builder = builder.no_proxy();
        }
    }

    builder
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))
}

fn classify_probe_body(status: u16, body: &str) -> String {
    if is_cloudflare_block(status, body) {
        return format!("{status} Cloudflare 拦截（直连常见）");
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(msg) = value
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .or_else(|| value.pointer("/message").and_then(|v| v.as_str()))
        {
            return format!("{status} {msg}");
        }
        if value.get("data").is_some() || value.get("models").is_some() {
            return format!("{status} models 列表可读");
        }
    }
    if body.to_ascii_lowercase().contains("<html") {
        return format!("{status} HTML 响应");
    }
    format!("{status} {}", shorten(body.trim(), 80))
}

fn is_cloudflare_block(status: u16, body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    status == 403
        && (lower.contains("cloudflare")
            || lower.contains("attention required")
            || lower.contains("you have been blocked")
            || lower.contains("unable to access"))
}

fn normalize_probe_base(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    // Allow pasted markdown links accidentally.
    let trimmed = trimmed
        .trim_start_matches('[')
        .split(']')
        .next()
        .unwrap_or(trimmed);
    let trimmed = if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("http://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("https://{rest}")
    } else if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    // Strip trailing /v1 so we can append /v1/models consistently.
    trimmed
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/')
        .to_string()
}

fn push_candidate(out: &mut Vec<LocalProxyCandidate>, raw_url: &str, source: &str) {
    let Some((proxy_url, host, port)) = normalize_detected_proxy(raw_url) else {
        return;
    };
    if out.iter().any(|c| c.proxy_url == proxy_url) {
        // Upgrade source priority if better.
        if let Some(existing) = out.iter_mut().find(|c| c.proxy_url == proxy_url) {
            if source_rank(source) < source_rank(&existing.source) {
                existing.source = source.to_string();
            }
        }
        return;
    }
    let listening = if host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost") {
        tcp_open("127.0.0.1", port)
    } else {
        true
    };
    out.push(LocalProxyCandidate {
        proxy_url,
        host,
        port,
        listening,
        source: source.to_string(),
    });
}

fn source_rank(source: &str) -> u8 {
    match source {
        "process-env" => 0,
        "user-env" => 1,
        "machine-env" => 2,
        "system-proxy" => 3,
        "local-port-scan" => 9,
        _ => 5,
    }
}

fn source_label(source: &str) -> &'static str {
    match source {
        "process-env" => "进程环境变量",
        "user-env" => "用户环境变量",
        "machine-env" => "系统环境变量",
        "system-proxy" => "Windows 系统代理",
        "local-port-scan" => "本机端口扫描",
        _ => "自动检测",
    }
}

fn display_proxy(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn normalize_detected_proxy(raw: &str) -> Option<(String, String, u16)> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    // WinINET can be "http=127.0.0.1:7890;https=127.0.0.1:7890"
    if value.contains('=') && (value.contains("http=") || value.contains("https=")) {
        return parse_windows_proxy_server(value);
    }
    let with_scheme = if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    let url = url::Url::parse(&with_scheme).ok()?;
    let host = url.host_str()?.to_string();
    let port = url.port_or_known_default()?;
    let proxy_url = if url.scheme() == "socks5" || url.scheme() == "socks5h" {
        format!("{}://{host}:{port}", url.scheme())
    } else {
        format!("http://{host}:{port}")
    };
    Some((proxy_url, host, port))
}

fn parse_windows_proxy_server(raw: &str) -> Option<(String, String, u16)> {
    // Prefer https= then http= then first token.
    let mut picked: Option<&str> = None;
    for part in raw.split(|c| c == ';' || c == ' ') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once('=') {
            let k = k.trim().to_ascii_lowercase();
            if k == "https" {
                return normalize_detected_proxy(v.trim());
            }
            if k == "http" && picked.is_none() {
                picked = Some(v.trim());
            }
        } else if picked.is_none() {
            picked = Some(part);
        }
    }
    picked.and_then(normalize_detected_proxy)
}

fn env_proxy_url_from_process() -> Option<String> {
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn env_proxy_urls_from_registry() -> Vec<(String, String)> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        for (root, label) in [
            (winreg::enums::HKEY_CURRENT_USER, "user-env"),
            (winreg::enums::HKEY_LOCAL_MACHINE, "machine-env"),
        ] {
            if let Ok(key) = winreg::RegKey::predef(root)
                .open_subkey(r"System\CurrentControlSet\Control\Session Manager\Environment")
            {
                for name in [
                    "HTTPS_PROXY",
                    "https_proxy",
                    "HTTP_PROXY",
                    "http_proxy",
                    "ALL_PROXY",
                    "all_proxy",
                ] {
                    if let Ok(value) = key.get_value::<String, _>(name) {
                        let value = value.trim();
                        if !value.is_empty() {
                            out.push((value.to_string(), label.to_string()));
                            break;
                        }
                    }
                }
            }
        }
    }
    let _ = &out;
    out
}

#[cfg(windows)]
fn windows_ie_proxy_url() -> Option<String> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let settings = hkcu
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    let enabled: u32 = settings.get_value("ProxyEnable").unwrap_or(0);
    if enabled == 0 {
        return None;
    }
    let raw: String = settings.get_value("ProxyServer").ok()?;
    Some(raw)
}

#[cfg(not(windows))]
fn windows_ie_proxy_url() -> Option<String> {
    None
}

fn tcp_open(host: &str, port: u16) -> bool {
    let Ok(addr) = format!("{host}:{port}").parse::<SocketAddr>() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, PORT_PROBE_TIMEOUT).is_ok()
}

fn listener_process_hint(port: u16) -> Option<String> {
    #[cfg(windows)]
    {
        let output = Command::new("netstat")
            .args(["-ano", "-p", "tcp"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let needle = format!("127.0.0.1:{port}");
        let mut pid: Option<u32> = None;
        for line in text.lines() {
            if line.contains(&needle) && line.contains("LISTENING") {
                if let Some(last) = line.split_whitespace().last() {
                    if let Ok(value) = last.parse::<u32>() {
                        pid = Some(value);
                        break;
                    }
                }
            }
        }
        let pid = pid?;
        let name = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .ok()
            .and_then(|o| {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                // "name.exe","pid","session","..."
                s.split(',').next().map(|n| n.trim_matches('"').to_string())
            })
            .unwrap_or_else(|| format!("pid={pid}"));
        Some(format!("{name} (pid {pid})"))
    }
    #[cfg(not(windows))]
    {
        let _ = port;
        None
    }
}

fn try_start_common_proxy_apps() -> Option<String> {
    let candidates = known_proxy_executables();
    if candidates.is_empty() {
        return None;
    }
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        match Command::new(&path).spawn() {
            Ok(child) => {
                return Some(format!(
                    "已尝试启动 {} (pid {:?})",
                    path.display(),
                    child.id()
                ));
            }
            Err(_) => continue,
        }
    }
    None
}

fn known_proxy_executables() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push = |p: PathBuf| {
        if p.is_file() && !out.contains(&p) {
            out.push(p);
        }
    };

    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        for rel in [
            r"Programs\Clash Verge\Clash Verge.exe",
            r"Clash Verge\Clash Verge.exe",
            r"Programs\mihomo\mihomo.exe",
            r"Programs\Clash for Windows\Clash for Windows.exe",
            r"v2rayN\v2rayN.exe",
        ] {
            push(local.join(rel));
        }
    }
    if let Ok(roaming) = std::env::var("APPDATA") {
        let roaming = PathBuf::from(roaming);
        push(roaming.join(r"clash_win\Clash for Windows.exe"));
    }
    if let Ok(pf) = std::env::var("ProgramFiles") {
        let pf = PathBuf::from(pf);
        push(pf.join(r"Clash Verge\Clash Verge.exe"));
        push(pf.join(r"v2rayN\v2rayN.exe"));
    }
    // Also search running-associated folders is hard; keep list tight.

    out
}

fn shorten(input: &str, max: usize) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out = trimmed
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_plain_host_port() {
        let (url, host, port) = normalize_detected_proxy("127.0.0.1:7890").unwrap();
        assert_eq!(url, "http://127.0.0.1:7890");
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 7890);
    }

    #[test]
    fn parses_wininet_list() {
        let (url, host, port) =
            parse_windows_proxy_server("http=127.0.0.1:7890;https=127.0.0.1:7890").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 7890);
        assert_eq!(url, "http://127.0.0.1:7890");
    }

    #[test]
    fn probe_base_strips_v1_and_markdown() {
        assert_eq!(
            normalize_probe_base("https://lumingapi.store/v1"),
            "https://lumingapi.store"
        );
        assert_eq!(
            normalize_probe_base("lumingapi.store"),
            "https://lumingapi.store"
        );
    }
}
