#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

use url::Url;

const CODEX_CLI_NOT_FOUND: &str =
    "the official Codex CLI could not be found; install or update @openai/codex";
const MAX_LOGIN_URL_BYTES: usize = 8 * 1024;

pub fn open_login_url(value: &str) -> Result<(), String> {
    let url = validate_login_url(value)?;
    open_validated_login_url(url.as_str())
}

fn validate_login_url(value: &str) -> Result<Url, String> {
    if value.is_empty()
        || value.len() > MAX_LOGIN_URL_BYTES
        || value.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err("Codex returned an invalid login URL".to_string());
    }
    let url = Url::parse(value).map_err(|_| "Codex returned an invalid login URL".to_string())?;
    let host = url
        .host_str()
        .ok_or_else(|| "Codex returned an invalid login URL".to_string())?;
    let trusted_host = host.eq_ignore_ascii_case("auth.openai.com")
        || host.eq_ignore_ascii_case("chatgpt.com")
        || host
            .to_ascii_lowercase()
            .strip_suffix(".chatgpt.com")
            .is_some_and(|prefix| !prefix.is_empty());
    if url.scheme() != "https"
        || url.port().is_some_and(|port| port != 443)
        || !trusted_host
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Codex returned an untrusted login URL".to_string());
    }
    Ok(url)
}

fn path_entries() -> impl Iterator<Item = PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
}

#[cfg(target_os = "macos")]
pub fn open_codex() -> Result<String, String> {
    let mut bundles = discover_signed_codex_bundles()?;
    if bundles.len() != 1 {
        return Err(
            "expected exactly one OpenAI-signed com.openai.codex application bundle".to_string(),
        );
    }
    let bundle = bundles.pop().expect("bundle count was checked");
    let status = std::process::Command::new("/usr/bin/open")
        .arg(&bundle)
        .status()
        .map_err(|_| "could not invoke macOS Launch Services".to_string())?;
    if !status.success() {
        return Err("the official Codex application could not be opened".to_string());
    }
    Ok("signed-bundle:com.openai.codex@2DC432GLL2".to_string())
}

#[cfg(target_os = "macos")]
pub fn codex_cli_path() -> Result<PathBuf, String> {
    use std::collections::BTreeSet;

    let (package_name, target_name) = if cfg!(target_arch = "aarch64") {
        ("codex-darwin-arm64", "aarch64-apple-darwin")
    } else {
        ("codex-darwin-x64", "x86_64-apple-darwin")
    };
    let mut candidates = BTreeSet::new();
    for directory in macos_cli_search_roots() {
        let shim = directory.join("codex");
        let Ok(canonical) = shim.canonicalize() else {
            continue;
        };
        if canonical.file_name().and_then(|value| value.to_str()) == Some("codex")
            && code_signature_matches(&canonical, false)
        {
            candidates.insert(canonical);
            continue;
        }
        if canonical.file_name().and_then(|value| value.to_str()) != Some("codex.js") {
            continue;
        }
        let Some(package_root) = canonical.parent().and_then(Path::parent) else {
            continue;
        };
        if package_root.file_name().and_then(|value| value.to_str()) != Some("codex")
            || package_root
                .parent()
                .and_then(Path::file_name)
                .and_then(|value| value.to_str())
                != Some("@openai")
        {
            continue;
        }
        let native = package_root
            .join("node_modules")
            .join("@openai")
            .join(package_name)
            .join("vendor")
            .join(target_name)
            .join("bin")
            .join("codex");
        let Ok(native) = native.canonicalize() else {
            continue;
        };
        if code_signature_matches(&native, false) {
            candidates.insert(native);
        }
    }
    if candidates.len() != 1 {
        return Err(CODEX_CLI_NOT_FOUND.to_string());
    }
    Ok(candidates.pop_first().expect("candidate count was checked"))
}

#[cfg(target_os = "macos")]
fn macos_cli_search_roots() -> Vec<PathBuf> {
    use std::collections::BTreeSet;

    let mut roots = path_entries().collect::<BTreeSet<_>>();
    roots.insert(PathBuf::from("/opt/homebrew/bin"));
    roots.insert(PathBuf::from("/usr/local/bin"));
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && home.is_absolute()
    {
        roots.insert(home.join(".local/bin"));
        roots.insert(home.join(".npm-global/bin"));
        roots.insert(home.join("Library/pnpm"));
    }
    if let Some(pnpm_home) = std::env::var_os("PNPM_HOME").map(PathBuf::from)
        && pnpm_home.is_absolute()
    {
        roots.insert(pnpm_home);
    }
    roots.into_iter().collect()
}

#[cfg(target_os = "macos")]
fn open_validated_login_url(value: &str) -> Result<(), String> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg(value)
        .status()
        .map_err(|_| "could not open the Codex login page".to_string())?;
    if !status.success() {
        return Err("could not open the Codex login page".to_string());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn discover_signed_codex_bundles() -> Result<Vec<std::path::PathBuf>, String> {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::process::Command;

    const MAX_MDFIND_BYTES: usize = 1024 * 1024;
    let mut candidates = vec![
        PathBuf::from("/Applications/ChatGPT.app"),
        PathBuf::from("/Applications/Codex.app"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(&home).join("Applications/ChatGPT.app"));
        candidates.push(PathBuf::from(home).join("Applications/Codex.app"));
    }

    let spotlight = Command::new("/usr/bin/mdfind")
        .arg("kMDItemCFBundleIdentifier == \"com.openai.codex\"")
        .output()
        .map_err(|_| "could not query macOS application registration".to_string())?;
    if !spotlight.status.success() || spotlight.stdout.len() > MAX_MDFIND_BYTES {
        return Err("macOS application registration query failed".to_string());
    }
    let discovered = std::str::from_utf8(&spotlight.stdout)
        .map_err(|_| "macOS application registration returned invalid text".to_string())?;
    candidates.extend(
        discovered
            .lines()
            .filter(|line| !line.is_empty())
            .take(65)
            .map(PathBuf::from),
    );
    if discovered.lines().filter(|line| !line.is_empty()).count() > 64 {
        return Err("too many Codex bundle candidates were registered".to_string());
    }

    let mut verified = BTreeSet::new();
    for candidate in candidates {
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if canonical.extension().and_then(|value| value.to_str()) != Some("app")
            || !canonical.join("Contents/Info.plist").is_file()
            || !bundle_identifier_matches(&canonical)
            || !bundle_signature_matches(&canonical)
        {
            continue;
        }
        verified.insert(canonical);
    }
    Ok(verified.into_iter().collect())
}

#[cfg(target_os = "macos")]
fn bundle_identifier_matches(bundle: &std::path::Path) -> bool {
    std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(bundle.join("Contents/Info.plist"))
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|value| value.trim() == "com.openai.codex")
}

#[cfg(target_os = "macos")]
fn bundle_signature_matches(bundle: &std::path::Path) -> bool {
    code_signature_matches(bundle, true)
}

#[cfg(target_os = "macos")]
fn code_signature_matches(path: &std::path::Path, deep: bool) -> bool {
    const TEAM_ID: &str = "2DC432GLL2";
    const REQUIREMENT: &str =
        "anchor apple generic and certificate leaf[subject.OU] = \"2DC432GLL2\"";
    let mut verify = std::process::Command::new("/usr/bin/codesign");
    verify.arg("--verify");
    if deep {
        verify.arg("--deep");
    }
    let verified = verify
        .args(["--strict", "--test-requirement"])
        .arg(format!("={REQUIREMENT}"))
        .arg(path)
        .output()
        .is_ok_and(|output| output.status.success());
    if !verified {
        return false;
    }
    let Ok(details) = std::process::Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=4"])
        .arg(path)
        .output()
    else {
        return false;
    };
    details.status.success()
        && String::from_utf8(details.stderr)
            .ok()
            .is_some_and(|stderr| {
                stderr
                    .lines()
                    .any(|line| line.trim() == format!("TeamIdentifier={TEAM_ID}"))
            })
}

#[cfg(target_os = "macos")]
pub fn authorize_codex_parent() -> Result<(), String> {
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    unsafe extern "C" {
        fn proc_pidpath(pid: libc::c_int, buffer: *mut c_void, buffer_size: u32) -> libc::c_int;
    }

    let parent_pid = unsafe { libc::getppid() };
    if parent_pid <= 1 {
        return Err("credential helper caller identity is unavailable".to_string());
    }
    let mut buffer = vec![0_u8; 4096];
    let length = unsafe {
        proc_pidpath(
            parent_pid,
            buffer.as_mut_ptr().cast::<c_void>(),
            buffer.len() as u32,
        )
    };
    if length <= 0 {
        return Err("credential helper caller identity is unavailable".to_string());
    }
    buffer.truncate(length as usize);
    if let Some(nul) = buffer.iter().position(|byte| *byte == 0) {
        buffer.truncate(nul);
    }
    let executable = PathBuf::from(std::ffi::OsString::from_vec(buffer))
        .canonicalize()
        .map_err(|_| "credential helper caller path is unavailable".to_string())?;
    let bundle = executable
        .ancestors()
        .find(|path| {
            path.extension().and_then(|value| value.to_str()) == Some("app")
                && bundle_identifier_matches(path)
        })
        .ok_or_else(|| "credential helper caller is not the Codex app".to_string())?;
    if !bundle_signature_matches(bundle) || !code_signature_matches(&executable, false) {
        return Err("credential helper caller signature is not trusted".to_string());
    }
    Ok(())
}

#[cfg(windows)]
pub fn open_codex() -> Result<String, String> {
    with_windows_runtime(open_codex_windows)
}

#[cfg(windows)]
pub fn codex_cli_path() -> Result<PathBuf, String> {
    use std::collections::BTreeSet;

    let mut candidates = BTreeSet::new();
    let mut roots = path_entries().collect::<Vec<_>>();
    if let Some(app_data) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(app_data).join("npm"));
    }
    for root in roots {
        let native = root
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("node_modules")
            .join("@openai")
            .join("codex-win32-x64")
            .join("vendor")
            .join("x86_64-pc-windows-msvc")
            .join("bin")
            .join("codex.exe");
        let Ok(native) = native.canonicalize() else {
            continue;
        };
        let Some(native_text) = native.to_str() else {
            continue;
        };
        if is_official_windows_npm_codex_path(native_text)
            && authenticode_signer_is_openai(native_text)
        {
            candidates.insert(native);
        }
    }
    if candidates.len() != 1 {
        return Err(CODEX_CLI_NOT_FOUND.to_string());
    }
    Ok(candidates.pop_first().expect("candidate count was checked"))
}

#[cfg(windows)]
fn open_validated_login_url(value: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::HSTRING;

    let target = HSTRING::from(value);
    let result = unsafe { ShellExecuteW(None, None, &target, None, None, SW_SHOWNORMAL) };
    if result.0 as isize <= 32 {
        return Err("could not open the Codex login page".to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn open_codex_windows() -> Result<String, String> {
    use windows::ApplicationModel::PackageSignatureKind;
    use windows::Management::Deployment::PackageManager;
    use windows::core::HSTRING;

    let manager =
        PackageManager::new().map_err(|_| "Windows package discovery failed".to_string())?;
    let packages = manager
        .FindPackagesByUserSecurityId(&HSTRING::new())
        .map_err(|_| "Windows package discovery failed".to_string())?;
    let mut candidates = Vec::new();
    for package in packages {
        let id = package
            .Id()
            .map_err(|_| "Windows package identity could not be read".to_string())?;
        if id
            .Name()
            .map_err(|_| "Windows package identity could not be read".to_string())?
            .to_string()
            != "OpenAI.Codex"
        {
            continue;
        }
        if package
            .IsResourcePackage()
            .map_err(|_| "Windows package identity could not be read".to_string())?
            || package
                .IsDevelopmentMode()
                .map_err(|_| "Windows package identity could not be read".to_string())?
            || package
                .SignatureKind()
                .map_err(|_| "Windows package identity could not be read".to_string())?
                != PackageSignatureKind::Store
            || !package
                .Status()
                .and_then(|status| status.VerifyIsOK())
                .map_err(|_| "Windows package status could not be read".to_string())?
        {
            continue;
        }
        candidates.push(package);
    }
    if candidates.len() != 1 {
        return Err(
            "expected exactly one registered official OpenAI.Codex Store package".to_string(),
        );
    }

    let package = candidates.pop().expect("candidate count was checked");
    let family_name = package
        .Id()
        .and_then(|id| id.FamilyName())
        .map_err(|_| "Windows package family identity could not be read".to_string())?
        .to_string();
    let entries = package
        .GetAppListEntriesAsync()
        .and_then(|operation| operation.join())
        .map_err(|_| "Codex application registration could not be read".to_string())?;
    let mut launchable = Vec::new();
    for entry in entries {
        let app_user_model_id = entry
            .AppUserModelId()
            .map_err(|_| "Codex application identity could not be read".to_string())?
            .to_string();
        if valid_app_user_model_id(&app_user_model_id)
            && app_user_model_id.starts_with(&format!("{family_name}!"))
        {
            launchable.push(entry);
        }
    }
    if launchable.len() != 1 {
        return Err(
            "expected exactly one launchable application in the OpenAI.Codex package".to_string(),
        );
    }
    let launched = launchable
        .pop()
        .expect("launchable count was checked")
        .LaunchAsync()
        .and_then(|operation| operation.join())
        .map_err(|_| "Windows could not activate Codex".to_string())?;
    if !launched {
        return Err("Windows declined to activate Codex".to_string());
    }
    Ok(format!("store-package:{family_name}"))
}

#[cfg(windows)]
pub fn authorize_codex_parent() -> Result<(), String> {
    with_windows_runtime(authorize_codex_parent_windows)
}

#[cfg(windows)]
fn authorize_codex_parent_windows() -> Result<(), String> {
    use std::mem;

    use windows::ApplicationModel::PackageSignatureKind;
    use windows::Management::Deployment::PackageManager;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot;
    use windows::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W;
    use windows::Win32::System::Diagnostics::ToolHelp::Process32FirstW;
    use windows::Win32::System::Diagnostics::ToolHelp::Process32NextW;
    use windows::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::System::Threading::OpenProcess;
    use windows::Win32::System::Threading::PROCESS_NAME_WIN32;
    use windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
    use windows::Win32::System::Threading::QueryFullProcessImageNameW;
    use windows::core::HSTRING;
    use windows::core::PWSTR;

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|_| "credential helper caller identity is unavailable".to_string())?;
    let mut entry = PROCESSENTRY32W {
        dwSize: mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut parent_pid = None;
    let current_pid = unsafe { GetCurrentProcessId() };
    let first = unsafe { Process32FirstW(snapshot, &mut entry) };
    if first.is_ok() {
        loop {
            if entry.th32ProcessID == current_pid {
                parent_pid = Some(entry.th32ParentProcessID);
                break;
            }
            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    let _ = unsafe { CloseHandle(snapshot) };
    let parent_pid = parent_pid
        .filter(|pid| *pid > 0)
        .ok_or_else(|| "credential helper caller identity is unavailable".to_string())?;
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, parent_pid) }
        .map_err(|_| "credential helper caller identity is unavailable".to_string())?;
    let mut path_buffer = vec![0_u16; 32_768];
    let mut path_length = path_buffer.len() as u32;
    let path_result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path_buffer.as_mut_ptr()),
            &mut path_length,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    path_result.map_err(|_| "credential helper caller path is unavailable".to_string())?;
    path_buffer.truncate(path_length as usize);
    let parent_path = String::from_utf16(&path_buffer)
        .map_err(|_| "credential helper caller path is unavailable".to_string())?;
    let parent_path = std::path::PathBuf::from(parent_path)
        .canonicalize()
        .map_err(|_| "credential helper caller path is unavailable".to_string())?;
    let parent_path = parent_path
        .to_str()
        .ok_or_else(|| "credential helper caller path is unavailable".to_string())?;

    let manager =
        PackageManager::new().map_err(|_| "Windows package discovery failed".to_string())?;
    let packages = manager
        .FindPackagesByUserSecurityId(&HSTRING::new())
        .map_err(|_| "Windows package discovery failed".to_string())?;
    let mut matching_packages = 0_usize;
    for package in packages {
        let is_official = package
            .Id()
            .and_then(|id| id.Name())
            .is_ok_and(|name| name == "OpenAI.Codex")
            && package.IsResourcePackage().is_ok_and(|value| !value)
            && package.IsDevelopmentMode().is_ok_and(|value| !value)
            && package
                .SignatureKind()
                .is_ok_and(|kind| kind == PackageSignatureKind::Store)
            && package
                .Status()
                .and_then(|status| status.VerifyIsOK())
                .is_ok_and(|value| value);
        if !is_official {
            continue;
        }
        let Ok(installed_path) = package.InstalledPath() else {
            continue;
        };
        if path_is_within_case_insensitive(parent_path, &installed_path.to_string()) {
            matching_packages += 1;
        }
    }
    if matching_packages == 1 {
        return Ok(());
    }
    if is_official_windows_npm_codex_path(parent_path) && authenticode_signer_is_openai(parent_path)
    {
        return Ok(());
    }
    Err("credential helper caller is not a trusted official Codex process".to_string())
}

#[cfg(windows)]
fn authenticode_signer_is_openai(path: &str) -> bool {
    use std::mem;
    use std::ptr;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Security::Cryptography::CERT_NAME_SIMPLE_DISPLAY_TYPE;
    use windows::Win32::Security::Cryptography::CertGetNameStringW;
    use windows::Win32::Security::WinTrust::WINTRUST_ACTION_GENERIC_VERIFY_V2;
    use windows::Win32::Security::WinTrust::WINTRUST_DATA;
    use windows::Win32::Security::WinTrust::WINTRUST_DATA_0;
    use windows::Win32::Security::WinTrust::WINTRUST_FILE_INFO;
    use windows::Win32::Security::WinTrust::WTD_CACHE_ONLY_URL_RETRIEVAL;
    use windows::Win32::Security::WinTrust::WTD_CHOICE_FILE;
    use windows::Win32::Security::WinTrust::WTD_REVOKE_NONE;
    use windows::Win32::Security::WinTrust::WTD_STATEACTION_CLOSE;
    use windows::Win32::Security::WinTrust::WTD_STATEACTION_VERIFY;
    use windows::Win32::Security::WinTrust::WTD_UI_NONE;
    use windows::Win32::Security::WinTrust::WTHelperGetProvSignerFromChain;
    use windows::Win32::Security::WinTrust::WTHelperProvDataFromStateData;
    use windows::Win32::Security::WinTrust::WinVerifyTrustEx;
    use windows::core::PCWSTR;

    let path_wide = path.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(path_wide.as_ptr()),
        hFile: HANDLE::default(),
        pgKnownSubject: ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let verified = unsafe { WinVerifyTrustEx(HWND::default(), &mut action, &mut data) } == 0;
    let signer_matches = if verified {
        let provider = unsafe { WTHelperProvDataFromStateData(data.hWVTStateData) };
        let signer = if provider.is_null() {
            ptr::null_mut()
        } else {
            unsafe { WTHelperGetProvSignerFromChain(provider, 0, false, 0) }
        };
        if signer.is_null()
            || unsafe { (*signer).csCertChain == 0 || (*signer).pasCertChain.is_null() }
        {
            false
        } else {
            let certificate = unsafe { (*(*signer).pasCertChain).pCert };
            if certificate.is_null() {
                false
            } else {
                let required = unsafe {
                    CertGetNameStringW(certificate, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, None)
                };
                if required <= 1 {
                    false
                } else {
                    let mut name = vec![0_u16; required as usize];
                    let written = unsafe {
                        CertGetNameStringW(
                            certificate,
                            CERT_NAME_SIMPLE_DISPLAY_TYPE,
                            0,
                            None,
                            Some(&mut name),
                        )
                    };
                    if written <= 1 {
                        false
                    } else {
                        name.truncate((written - 1) as usize);
                        String::from_utf16(&name).is_ok_and(|name| {
                            matches!(
                                name.as_str(),
                                "OpenAI OpCo, LLC" | "OpenAI, L.L.C." | "OpenAI"
                            )
                        })
                    }
                }
            }
        }
    } else {
        false
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    let _ = unsafe { WinVerifyTrustEx(HWND::default(), &mut action, &mut data) };
    signer_matches
}

#[cfg(windows)]
fn with_windows_runtime<T: Send + 'static>(
    operation: fn() -> Result<T, String>,
) -> Result<T, String> {
    std::thread::spawn(move || {
        use windows::Win32::System::WinRT::RO_INIT_MULTITHREADED;
        use windows::Win32::System::WinRT::RoInitialize;
        use windows::Win32::System::WinRT::RoUninitialize;

        struct RuntimeGuard;
        impl Drop for RuntimeGuard {
            fn drop(&mut self) {
                unsafe { RoUninitialize() };
            }
        }

        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|_| "Windows Runtime initialization failed".to_string())?;
        let _guard = RuntimeGuard;
        operation()
    })
    .join()
    .map_err(|_| "Windows Runtime worker failed".to_string())?
}

#[cfg(windows)]
fn path_is_within_case_insensitive(child: &str, parent: &str) -> bool {
    let child = child
        .strip_prefix(r"\\?\")
        .unwrap_or(child)
        .replace('/', "\\");
    let parent = parent
        .strip_prefix(r"\\?\")
        .unwrap_or(parent)
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\");
    child
        .get(..parent.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&parent))
        && child.as_bytes().get(parent.len()) == Some(&b'\\')
}

#[cfg(any(windows, test))]
fn is_official_windows_npm_codex_path(path: &str) -> bool {
    const X64_SUFFIX: &str = "\\node_modules\\@openai\\codex\\node_modules\\@openai\\codex-win32-x64\\vendor\\x86_64-pc-windows-msvc\\bin\\codex.exe";
    let normalized = path
        .strip_prefix(r"\\?\")
        .unwrap_or(path)
        .replace('/', "\\")
        .to_ascii_lowercase();
    normalized.ends_with(X64_SUFFIX)
}

#[cfg(windows)]
fn valid_app_user_model_id(value: &str) -> bool {
    let Some((family, app)) = value.split_once('!') else {
        return false;
    };
    !family.is_empty()
        && family.len() <= 128
        && !app.is_empty()
        && app.len() <= 64
        && family
            .bytes()
            .chain(app.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn open_codex() -> Result<String, String> {
    Err("opening Codex is supported only on macOS and Windows".to_string())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn codex_cli_path() -> Result<PathBuf, String> {
    for directory in path_entries() {
        let candidate = directory.join("codex");
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map_err(|_| CODEX_CLI_NOT_FOUND.to_string());
        }
    }
    Err(CODEX_CLI_NOT_FOUND.to_string())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open_validated_login_url(_value: &str) -> Result<(), String> {
    Err("opening the Codex login page is supported only on macOS and Windows".to_string())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn authorize_codex_parent() -> Result<(), String> {
    Err("credential helper caller verification is supported only on macOS and Windows".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_official_https_login_urls() {
        assert!(validate_login_url("https://auth.openai.com/oauth/authorize?state=x").is_ok());
        assert!(validate_login_url("https://chatgpt.com/auth/callback").is_ok());
        assert!(validate_login_url("https://login.chatgpt.com/").is_ok());
        assert!(validate_login_url("https://auth.openai.com:443/oauth/authorize").is_ok());
        assert!(validate_login_url("http://auth.openai.com/oauth/authorize").is_err());
        assert!(validate_login_url("https://auth.openai.com:444/oauth/authorize").is_err());
        assert!(validate_login_url("https://auth.openai.com.example.test/").is_err());
        assert!(validate_login_url("https://user@auth.openai.com/").is_err());
        assert!(
            validate_login_url(&format!(
                "https://auth.openai.com/{}",
                "x".repeat(MAX_LOGIN_URL_BYTES)
            ))
            .is_err()
        );
    }

    #[test]
    fn recognizes_only_the_official_npm_codex_layout() {
        assert!(is_official_windows_npm_codex_path(
            r"C:\Users\Admin\AppData\Roaming\npm\node_modules\@openai\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe"
        ));
        assert!(is_official_windows_npm_codex_path(
            r"\\?\D:\Tools\node_modules\@openai\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe"
        ));
        assert!(!is_official_windows_npm_codex_path(
            r"C:\Users\Admin\bin\codex.exe"
        ));
        assert!(!is_official_windows_npm_codex_path(
            r"C:\Users\Admin\node_modules\other\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe"
        ));
    }

    #[cfg(windows)]
    #[test]
    fn validates_app_user_model_ids() {
        assert!(valid_app_user_model_id("OpenAI.Codex_test!App"));
        assert!(!valid_app_user_model_id("OpenAI.Codex test!App"));
        assert!(!valid_app_user_model_id("OpenAI.Codex_test"));
    }
}
