#[cfg(any(target_os = "macos", target_os = "windows"))]
const SERVICE: &str = "dev.codex-provider-switcher.credentials";

#[cfg(target_os = "macos")]
use keyring::Entry as NativeEntry;
#[cfg(target_os = "macos")]
use keyring::Error as CredentialError;
#[cfg(target_os = "windows")]
use keyring_core::Entry as NativeEntry;
#[cfg(target_os = "windows")]
use keyring_core::Error as CredentialError;

pub fn validate_account(account: &str) -> Result<(), String> {
    let fingerprint = account
        .strip_prefix("endpoint-v1-")
        .or_else(|| account.strip_prefix("proxy-client-v1-"));
    let Some(fingerprint) = fingerprint else {
        return Err("invalid provider credential account".to_string());
    };
    if fingerprint.len() != 64
        || !fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid provider credential account".to_string());
    }
    Ok(())
}

pub fn validate_secret(secret: &str) -> Result<(), String> {
    if secret.is_empty()
        || secret.len() > 8_192
        || secret.contains('\0')
        || secret.contains('\r')
        || secret.contains('\n')
    {
        return Err("credential must be 1-8192 characters without line breaks".to_string());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn store(account: &str, secret: &str) -> Result<(), String> {
    validate_account(account)?;
    validate_secret(secret)?;
    entry(account)?.set_password(secret).map_err(redacted_error)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn store(account: &str, secret: &str) -> Result<(), String> {
    validate_account(account)?;
    validate_secret(secret)?;
    Err("the native credential store is supported only on macOS and Windows".to_string())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn get(account: &str) -> Result<String, String> {
    validate_account(account)?;
    entry(account)?.get_password().map_err(redacted_error)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn get(account: &str) -> Result<String, String> {
    validate_account(account)?;
    Err("the native credential store is supported only on macOS and Windows".to_string())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn exists(account: &str) -> Result<bool, String> {
    validate_account(account)?;
    match entry(account)?.get_password() {
        Ok(_) => Ok(true),
        Err(CredentialError::NoEntry) => Ok(false),
        Err(error) => Err(redacted_error(error)),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn exists(account: &str) -> Result<bool, String> {
    validate_account(account)?;
    Err("the native credential store is supported only on macOS and Windows".to_string())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn delete(account: &str) -> Result<(), String> {
    validate_account(account)?;
    match entry(account)?.delete_credential() {
        Ok(()) | Err(CredentialError::NoEntry) => Ok(()),
        Err(error) => Err(redacted_error(error)),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn delete(account: &str) -> Result<(), String> {
    validate_account(account)?;
    Err("the native credential store is supported only on macOS and Windows".to_string())
}

#[cfg(target_os = "macos")]
fn entry(account: &str) -> Result<NativeEntry, String> {
    keyring::Entry::new(SERVICE, account).map_err(redacted_error)
}

#[cfg(target_os = "windows")]
fn entry(account: &str) -> Result<NativeEntry, String> {
    use std::collections::HashMap;

    use keyring_core::api::CredentialStoreApi;

    let store = windows_native_keyring_store::Store::new().map_err(redacted_error)?;
    let modifiers = HashMap::from([("persistence", "Local")]);
    store
        .build(SERVICE, account, Some(&modifiers))
        .map_err(redacted_error)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn redacted_error(error: CredentialError) -> String {
    match error {
        CredentialError::NoEntry => "credential is not stored".to_string(),
        CredentialError::Ambiguous(_) => "credential store returned an ambiguous entry".to_string(),
        _ => "native credential store operation failed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_accounts_and_multiline_secrets() {
        assert!(
            validate_account(
                "endpoint-v1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )
            .is_ok()
        );
        assert!(
            validate_account(
                "proxy-client-v1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )
            .is_ok()
        );
        assert!(validate_account("acme-provider").is_err());
        assert!(validate_account("../acme").is_err());
        assert!(validate_secret("token-value").is_ok());
        assert!(validate_secret("token\nvalue").is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_credentials_are_local_and_survive_new_entries() {
        use std::time::SystemTime;
        use std::time::UNIX_EPOCH;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            ^ u128::from(std::process::id());
        let account = format!("endpoint-v1-{unique:064x}");
        let mut stored = false;
        let result = (|| -> Result<(), String> {
            validate_account(&account)?;
            let first = entry(&account)?;
            match first.set_password("test-local-persistence") {
                Ok(()) => {}
                Err(CredentialError::NoStorageAccess(_)) => {
                    eprintln!(
                        "Windows Credential Manager Local persistence is unavailable in this non-interactive logon session"
                    );
                    return Ok(());
                }
                Err(error) => {
                    return Err(format!("Windows Local credential write failed: {error:?}"));
                }
            }
            stored = true;
            let reopened = entry(&account)?;
            if reopened
                .get_password()
                .map_err(|error| format!("Windows Local credential read failed: {error:?}"))?
                != "test-local-persistence"
            {
                return Err("credential value changed after reopening the entry".to_string());
            }
            let attributes = reopened
                .get_attributes()
                .map_err(|error| format!("Windows credential attribute read failed: {error:?}"))?;
            if attributes.get("persistence").map(String::as_str) != Some("Local") {
                return Err("Windows credential persistence is not Local".to_string());
            }
            Ok(())
        })();
        let cleanup = if stored { delete(&account) } else { Ok(()) };
        result.unwrap();
        cleanup.unwrap();
    }
}
