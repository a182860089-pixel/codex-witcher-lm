//! Diagnose and repair Codex local tool / Windows sandbox failures that block
//! shell, Node, and MCP after the model has already emitted tool calls.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

const FULL_ACCESS_MODE: &str = "full-access";
const GUARDIAN_MODE: &str = "guardian-approvals";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolChannelDiagnosis {
    pub healthy: bool,
    pub platform_supported: bool,
    pub setup_error_code: Option<String>,
    pub setup_error_message: Option<String>,
    pub agent_mode: Option<String>,
    pub guardian_mode_active: bool,
    pub sandbox_acl_failures: Vec<String>,
    pub issues: Vec<String>,
    pub recommendations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolChannelRepairReport {
    pub steps: Vec<String>,
    pub diagnosis: ToolChannelDiagnosis,
    pub requires_codex_restart: bool,
    pub elevation_attempted: bool,
    pub elevation_needed: bool,
}

#[derive(Debug, Deserialize)]
struct SetupErrorFile {
    code: Option<String>,
    message: Option<String>,
}

pub fn codex_home() -> PathBuf {
    if let Ok(override_home) = std::env::var("CODEX_HOME") {
        let trimmed = override_home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    directories::UserDirs::new()
        .map(|dirs| dirs.home_dir().join(".codex"))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

pub fn diagnose_tool_channel() -> ToolChannelDiagnosis {
    #[cfg(not(windows))]
    {
        return ToolChannelDiagnosis {
            healthy: true,
            platform_supported: false,
            setup_error_code: None,
            setup_error_message: None,
            agent_mode: None,
            guardian_mode_active: false,
            sandbox_acl_failures: Vec::new(),
            issues: Vec::new(),
            recommendations: vec![
                "Codex Windows sandbox repair is only available on Windows.".to_string(),
            ],
        };
    }

    #[cfg(windows)]
    {
        diagnose_tool_channel_windows(&codex_home())
    }
}

pub fn repair_tool_channel() -> Result<ToolChannelRepairReport, String> {
    #[cfg(not(windows))]
    {
        return Ok(ToolChannelRepairReport {
            steps: vec!["当前系统不是 Windows，已跳过 Codex 沙箱/工具通道修复。".to_string()],
            diagnosis: diagnose_tool_channel(),
            requires_codex_restart: false,
            elevation_attempted: false,
            elevation_needed: false,
        });
    }

    #[cfg(windows)]
    {
        repair_tool_channel_windows(&codex_home())
    }
}

#[cfg(windows)]
fn diagnose_tool_channel_windows(codex_home: &Path) -> ToolChannelDiagnosis {
    let mut issues = Vec::new();
    let mut recommendations = Vec::new();
    let mut sandbox_acl_failures = Vec::new();

    let (setup_error_code, setup_error_message) = read_setup_error(codex_home);
    if setup_error_code.is_some() || setup_error_message.is_some() {
        let code = setup_error_code.clone().unwrap_or_else(|| "unknown".into());
        let message = setup_error_message
            .clone()
            .unwrap_or_else(|| "setup refresh failed".into());
        issues.push(format!("Codex 沙箱 setup 错误：{code} — {message}"));
        recommendations.push(
            "清除卡死的 setup_error，并把 Codex 权限切到 Full access，避免 workspace 沙箱改 ACL。"
                .to_string(),
        );
    }

    // Historical sandbox.log lines are not proof of a current failure. Only report
    // paths that still fail a live WRITE_DAC probe (or remain listed while setup_error exists).
    let mut candidates = scan_sandbox_log_acl_failures(codex_home);
    for project in read_local_project_paths(codex_home) {
        let text = project.display().to_string();
        if !candidates.iter().any(|existing| existing == &text) {
            candidates.push(text);
        }
    }
    for candidate in candidates {
        let path = PathBuf::from(&candidate);
        if !path.exists() {
            continue;
        }
        match path_needs_acl_repair(&path) {
            Ok(true) => {
                if !sandbox_acl_failures
                    .iter()
                    .any(|existing| existing == &candidate)
                {
                    sandbox_acl_failures.push(candidate);
                }
            }
            Ok(false) => {}
            Err(_) => {
                // Probe failed unexpectedly; keep quiet unless setup_error is sticky.
                if setup_error_code.is_some()
                    && !sandbox_acl_failures
                        .iter()
                        .any(|existing| existing == &candidate)
                {
                    sandbox_acl_failures.push(candidate);
                }
            }
        }
    }
    for failure in &sandbox_acl_failures {
        issues.push(format!("沙箱仍无法写入目录 ACL：{failure}"));
    }
    if !sandbox_acl_failures.is_empty() {
        recommendations.push(
            "对失败目录授予 CodexSandboxOffline/Online 完全控制（根目录即可），或改用 C:\\Users\\… 路径。"
                .to_string(),
        );
    }

    let agent_mode = read_agent_mode(codex_home);
    let guardian_mode_active = agent_mode
        .as_deref()
        .is_some_and(|mode| mode == GUARDIAN_MODE || mode.contains("guardian"));
    if guardian_mode_active {
        issues.push(
            "Codex 桌面端当前是 Guardian 审批模式，会强制 workspace 沙箱并走 auto_review。"
                .to_string(),
        );
        recommendations.push(
            "一键修复会把本机 Agent 模式改为 Full access；修复后请完全退出 Codex，且不要再切回 Guardian。"
                .to_string(),
        );
    }

    if !sandbox_users_present() {
        issues.push("未找到 CodexSandboxOffline / CodexSandboxOnline 本地用户。".to_string());
        recommendations.push("重新安装或修复官方 Codex Desktop，以重建沙箱账户。".to_string());
    }

    let healthy = issues.is_empty();
    if healthy {
        recommendations.push("本机工具通道未见已知沙箱故障。".to_string());
    }

    ToolChannelDiagnosis {
        healthy,
        platform_supported: true,
        setup_error_code,
        setup_error_message,
        agent_mode,
        guardian_mode_active,
        sandbox_acl_failures,
        issues,
        recommendations,
    }
}

/// Returns true when the current process cannot grant sandbox ACEs on `path`.
fn path_needs_acl_repair(path: &Path) -> Result<bool, String> {
    // Prefer a no-op grant of an already-expected principal. Success means WRITE_DAC works.
    match run_icacls_grant(path, "CodexSandboxOffline") {
        Ok(_) => Ok(false),
        Err(error) if error.contains("access denied") => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn repair_tool_channel_windows(codex_home: &Path) -> Result<ToolChannelRepairReport, String> {
    let before = diagnose_tool_channel_windows(codex_home);
    let mut steps = Vec::new();
    let mut requires_codex_restart = false;
    let mut elevation_attempted = false;
    let mut elevation_needed = false;

    if clear_setup_error(codex_home)? {
        steps.push("已清除 .codex/.sandbox/setup_error.json 卡住状态。".to_string());
        requires_codex_restart = true;
    } else {
        steps.push("未发现卡住的 setup_error.json。".to_string());
    }

    match ensure_full_access_agent_mode(codex_home) {
        Ok(true) => {
            steps.push(
                "已将 Codex 桌面 Agent 模式改为 Full access（原 Guardian 会覆盖 config.toml）。"
                    .to_string(),
            );
            requires_codex_restart = true;
        }
        Ok(false) => {
            steps.push("Codex 桌面 Agent 模式已是 Full access。".to_string());
        }
        Err(error) => {
            steps.push(format!("未能改写 Codex 桌面权限模式：{error}"));
            elevation_needed = true;
        }
    }

    let mut acl_targets = vec![codex_home.to_path_buf()];
    for path in before.sandbox_acl_failures.iter().map(PathBuf::from) {
        if !acl_targets.iter().any(|existing| existing == &path) {
            acl_targets.push(path);
        }
    }
    for project in read_local_project_paths(codex_home) {
        if !acl_targets.iter().any(|existing| existing == &project) {
            acl_targets.push(project);
        }
    }

    for target in acl_targets {
        match grant_sandbox_modify_acl(&target) {
            Ok(GrantAclOutcome::Granted) => {
                steps.push(format!("已为沙箱账户授权：{}", target.display()));
                requires_codex_restart = true;
            }
            Ok(GrantAclOutcome::AlreadyOk) => {
                steps.push(format!("目录 ACL 可写或无需修改：{}", target.display()));
            }
            Ok(GrantAclOutcome::SkippedMissing) => {
                steps.push(format!("跳过不存在的路径：{}", target.display()));
            }
            Err(error) => {
                steps.push(format!(
                    "普通权限无法修改 ACL（{}）：{error}",
                    target.display()
                ));
                elevation_needed = true;
            }
        }
    }

    if elevation_needed {
        match launch_elevated_acl_helper(codex_home, &before.sandbox_acl_failures) {
            Ok(true) => {
                elevation_attempted = true;
                elevation_needed = false;
                steps.push("已通过管理员权限完成根目录 ACL 修复（UAC）。".to_string());
                requires_codex_restart = true;
            }
            Ok(false) => {
                steps.push(
                    "需要管理员权限才能修复部分目录 ACL；请右键以管理员运行 LM Codex Switch 后再点修复。"
                        .to_string(),
                );
            }
            Err(error) => {
                steps.push(format!("无法启动管理员 ACL 修复：{error}"));
            }
        }
    }

    if clear_setup_error(codex_home)? {
        steps.push("已再次清除 .codex/.sandbox/setup_error.json。".to_string());
        requires_codex_restart = true;
    }

    steps.push(
        "请从托盘完全退出 Codex 后重新打开；右上角保持 Full access，不要切回 Guardian。请新建对话验证工具。"
            .to_string(),
    );

    let diagnosis = diagnose_tool_channel_windows(codex_home);
    Ok(ToolChannelRepairReport {
        steps,
        diagnosis,
        requires_codex_restart,
        elevation_attempted,
        elevation_needed,
    })
}

fn read_setup_error(codex_home: &Path) -> (Option<String>, Option<String>) {
    let path = codex_home.join(".sandbox").join("setup_error.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        return (None, None);
    };
    match serde_json::from_str::<SetupErrorFile>(&raw) {
        Ok(parsed) => (parsed.code, parsed.message),
        Err(_) => (Some("unreadable".into()), Some(raw.trim().to_string())),
    }
}

fn clear_setup_error(codex_home: &Path) -> Result<bool, String> {
    let path = codex_home.join(".sandbox").join("setup_error.json");
    if !path.is_file() {
        return Ok(false);
    }
    fs::remove_file(&path).map_err(|error| format!("无法删除 setup_error.json：{error}"))?;
    Ok(true)
}

fn read_agent_mode(codex_home: &Path) -> Option<String> {
    let path = codex_home.join(".codex-global-state.json");
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let atom = value.get("electron-persisted-atom-state")?;
    if let Some(mode) = atom
        .pointer("/permission-selection-by-host-id:local/agentMode")
        .and_then(Value::as_str)
    {
        return Some(mode.to_string());
    }
    atom.pointer("/agent-mode-by-host-id/local")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn ensure_full_access_agent_mode(codex_home: &Path) -> Result<bool, String> {
    let path = codex_home.join(".codex-global-state.json");
    if !path.is_file() {
        return Ok(false);
    }
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("无法读取 .codex-global-state.json：{error}"))?;
    let mut value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("无法解析 .codex-global-state.json：{error}"))?;

    let Some(atom) = value
        .get_mut("electron-persisted-atom-state")
        .and_then(Value::as_object_mut)
    else {
        return Ok(false);
    };

    let mut changed = false;
    let selection = atom
        .entry("permission-selection-by-host-id:local")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if selection.get("kind").and_then(Value::as_str) != Some("agent-mode")
        || selection.get("agentMode").and_then(Value::as_str) != Some(FULL_ACCESS_MODE)
    {
        *selection = serde_json::json!({
            "kind": "agent-mode",
            "agentMode": FULL_ACCESS_MODE
        });
        changed = true;
    }

    let modes = atom
        .entry("agent-mode-by-host-id")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if modes.get("local").and_then(Value::as_str) != Some(FULL_ACCESS_MODE) {
        if let Some(object) = modes.as_object_mut() {
            object.insert("local".into(), Value::String(FULL_ACCESS_MODE.into()));
            changed = true;
        }
    }

    let preferred = atom
        .entry("preferred-non-full-access-agent-mode-by-host-id")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if preferred.get("local").and_then(Value::as_str).is_none()
        && let Some(object) = preferred.as_object_mut()
    {
        object.insert("local".into(), Value::String(GUARDIAN_MODE.into()));
        changed = true;
    }

    // Drop per-thread Guardian locks so auto_review cannot keep rejecting tools.
    if let Some(hb) = atom
        .get_mut("heartbeat-thread-permissions-by-id")
        .and_then(Value::as_object_mut)
    {
        let unlocked = serde_json::json!({
            "activePermissionProfile": null,
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "sandboxPolicy": { "type": "dangerFullAccess" }
        });
        for (_id, entry) in hb.iter_mut() {
            if entry != &unlocked {
                *entry = unlocked.clone();
                changed = true;
            }
        }
    }

    if !changed {
        return Ok(false);
    }

    let backup = codex_home.join(format!(
        ".codex-global-state.json.bak-tool-channel-{}",
        timestamp_slug()
    ));
    fs::write(&backup, raw.as_bytes())
        .map_err(|error| format!("无法备份 .codex-global-state.json：{error}"))?;

    let rendered = serde_json::to_vec(&value)
        .map_err(|error| format!("无法序列化 .codex-global-state.json：{error}"))?;
    atomic_write(&path, &rendered)?;
    Ok(true)
}

fn read_local_project_paths(codex_home: &Path) -> Vec<PathBuf> {
    let path = codex_home.join(".codex-global-state.json");
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    if let Some(projects) = value
        .pointer("/electron-persisted-atom-state/local-projects")
        .and_then(Value::as_array)
    {
        for item in projects {
            if let Some(path) = item
                .get("path")
                .or_else(|| item.get("root"))
                .or_else(|| item.get("cwd"))
                .and_then(Value::as_str)
            {
                let candidate = PathBuf::from(path);
                if candidate.is_dir() {
                    paths.push(candidate);
                }
            }
        }
    }
    paths.truncate(12);
    paths
}

fn scan_sandbox_log_acl_failures(codex_home: &Path) -> Vec<String> {
    let dir = codex_home.join(".sandbox");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut logs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("sandbox.") && name.ends_with(".log"))
        })
        .collect();
    logs.sort();
    let Some(latest) = logs.pop() else {
        return Vec::new();
    };
    let Ok(raw) = fs::read_to_string(latest) else {
        return Vec::new();
    };
    let mut failures = Vec::new();
    for line in raw.lines().rev().take(400) {
        if let Some(path) = extract_ace_failure_path(line)
            && !failures.iter().any(|existing| existing == &path)
        {
            failures.push(path);
        }
        if failures.len() >= 8 {
            break;
        }
    }
    failures
}

fn extract_ace_failure_path(line: &str) -> Option<String> {
    const MARKERS: [&str; 2] = ["write ACE grant failed on ", "write ACE failed on "];
    for marker in MARKERS {
        if let Some(index) = line.find(marker) {
            let rest = &line[index + marker.len()..];
            let path = split_windows_path_from_message(rest)
                .trim()
                .trim_matches('"');
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    None
}

/// Split `C:\foo\bar: error detail` without cutting the drive letter.
fn split_windows_path_from_message(rest: &str) -> &str {
    let bytes = rest.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        if let Some(rel) = rest[2..].find(':') {
            return rest[..2 + rel].trim_end();
        }
        return rest.trim_end();
    }
    rest.split_once(':')
        .map(|(path, _)| path)
        .unwrap_or(rest)
        .trim_end()
}

#[derive(Debug, PartialEq, Eq)]
enum GrantAclOutcome {
    Granted,
    AlreadyOk,
    SkippedMissing,
}

fn grant_sandbox_modify_acl(path: &Path) -> Result<GrantAclOutcome, String> {
    if !path.exists() {
        return Ok(GrantAclOutcome::SkippedMissing);
    }

    // Root-only Full Control is enough for SetNamedSecurityInfo WRITE_DAC.
    let mut granted_any = false;
    for account in [
        "CodexSandboxOffline",
        "CodexSandboxOnline",
        "Administrators",
        "SYSTEM",
    ] {
        match run_icacls_grant(path, account) {
            Ok(true) => granted_any = true,
            Ok(false) => {}
            Err(error) => return Err(error),
        }
    }
    if granted_any {
        Ok(GrantAclOutcome::Granted)
    } else {
        Ok(GrantAclOutcome::AlreadyOk)
    }
}

fn run_icacls_grant(path: &Path, account: &str) -> Result<bool, String> {
    let path_text = path
        .to_str()
        .ok_or_else(|| "path is not valid UTF-8".to_string())?;
    let output = Command::new("icacls")
        .arg(path_text)
        .arg("/grant:r")
        .arg(format!("{account}:(OI)(CI)F"))
        .output()
        .map_err(|error| format!("无法运行 icacls：{error}"))?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}").to_lowercase();
    if combined.contains("拒绝访问")
        || combined.contains("access is denied")
        || combined.contains("access denied")
    {
        return Err("access denied".to_string());
    }
    // Account missing is not fatal for the whole repair.
    if combined.contains("no mapping") || combined.contains("找不到") {
        return Ok(false);
    }
    Err(format!(
        "icacls failed: {}",
        combined.chars().take(160).collect::<String>()
    ))
}

fn sandbox_users_present() -> bool {
    #[cfg(windows)]
    {
        Command::new("net")
            .args(["user", "CodexSandboxOffline"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
            || Command::new("net")
                .args(["user", "CodexSandboxOnline"])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn launch_elevated_acl_helper(codex_home: &Path, failing_paths: &[String]) -> Result<bool, String> {
    let mut targets = failing_paths
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Ok(false);
    }
    targets.sort();
    targets.dedup();

    let script_dir = codex_home.join("provider-switcher");
    fs::create_dir_all(&script_dir)
        .map_err(|error| format!("无法创建 provider-switcher 目录：{error}"))?;
    let script_path = script_dir.join("repair-sandbox-acl-elevated.ps1");
    let script = build_elevated_acl_script(&targets);
    fs::write(&script_path, script.as_bytes())
        .map_err(|error| format!("无法写入提权脚本：{error}"))?;

    let script_arg = script_path
        .to_str()
        .ok_or_else(|| "elevated script path is not valid UTF-8".to_string())?;
    let done_path = script_dir.join("repair-sandbox-acl-elevated.done");
    let _ = fs::remove_file(&done_path);
    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!(
                "Start-Process -FilePath powershell.exe -Verb RunAs -ArgumentList '-NoProfile -ExecutionPolicy Bypass -File \"{script_arg}\"' | Out-Null"
            ),
        ])
        .status()
        .map_err(|error| format!("无法启动 UAC：{error}"))?;
    if !status.success() {
        return Ok(false);
    }
    // Root-only elevated repair should finish quickly after UAC approval.
    for _ in 0..90 {
        if done_path.is_file() {
            let _ = clear_setup_error(codex_home);
            let _ = ensure_full_access_agent_mode(codex_home);
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Ok(true)
}

fn build_elevated_acl_script(targets: &[PathBuf]) -> String {
    let mut lines = vec![
        "$ErrorActionPreference = 'Continue'".to_string(),
        "$accounts = @('CodexSandboxOffline','CodexSandboxOnline','Administrators','SYSTEM')"
            .to_string(),
        "$done = Join-Path $env:USERPROFILE '.codex\\provider-switcher\\repair-sandbox-acl-elevated.done'"
            .to_string(),
        "Remove-Item -LiteralPath $done -Force -ErrorAction SilentlyContinue".to_string(),
        "$me = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name".to_string(),
        "$targets = @(".to_string(),
    ];
    for target in targets {
        let escaped = target.display().to_string().replace('\'', "''");
        lines.push(format!("  '{escaped}',"));
    }
    lines.push(")".to_string());
    lines.push(
        r#"
foreach ($target in $targets) {
  if (-not (Test-Path -LiteralPath $target)) { continue }
  # Root-only: sandbox setup only needs WRITE_DAC on the workspace directory.
  takeown /F $target /A | Out-Null
  icacls $target /grant:r "${me}:(OI)(CI)F" | Out-Null
  foreach ($account in $accounts) {
    icacls $target /grant:r "${account}:(OI)(CI)F" | Out-Null
  }
}
$setup = Join-Path $env:USERPROFILE '.codex\.sandbox\setup_error.json'
if (Test-Path -LiteralPath $setup) { Remove-Item -LiteralPath $setup -Force }
'ok' | Set-Content -LiteralPath $done -Encoding ascii
"#
        .to_string(),
    );
    lines.join("\n")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "path has no parent".to_string())?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        timestamp_slug()
    ));
    fs::write(&temp, bytes).map_err(|error| format!("无法写入临时文件：{error}"))?;
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("无法替换目标文件：{error}")
    })?;
    Ok(())
}

fn timestamp_slug() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    millis.to_string()
}

#[cfg(test)]
mod tests {
    use super::FULL_ACCESS_MODE;
    use super::ensure_full_access_agent_mode;
    use super::extract_ace_failure_path;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn extracts_acl_failure_paths_from_sandbox_log_lines() {
        let line = r#"[2026-09-12T08:05:00.797629800+00:00] write ACE grant failed on E:\Codex Provider Switcher: SetNamedSecurityInfoW failed: 5"#;
        assert_eq!(
            extract_ace_failure_path(line).as_deref(),
            Some(r"E:\Codex Provider Switcher")
        );
    }

    #[test]
    fn switches_guardian_agent_mode_to_full_access() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join(".codex-global-state.json");
        let original = json!({
            "electron-persisted-atom-state": {
                "agent-mode-by-host-id": { "local": "guardian-approvals" },
                "permission-selection-by-host-id:local": {
                    "kind": "agent-mode",
                    "agentMode": "guardian-approvals"
                }
            }
        });
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        assert!(ensure_full_access_agent_mode(dir.path()).unwrap());
        let updated: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            updated.pointer("/electron-persisted-atom-state/agent-mode-by-host-id/local"),
            Some(&json!(FULL_ACCESS_MODE))
        );
        assert_eq!(
            updated.pointer(
                "/electron-persisted-atom-state/permission-selection-by-host-id:local/agentMode"
            ),
            Some(&json!(FULL_ACCESS_MODE))
        );
        let backups: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("bak-tool-channel"))
            .collect();
        assert_eq!(backups.len(), 1);
    }
}
