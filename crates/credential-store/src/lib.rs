#[cfg(any(target_os = "macos", target_os = "windows"))]
const SERVICE: &str = "dev.codex-provider-switcher.credentials";

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
        Err(keyring::Error::NoEntry) => Ok(false),
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
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(redacted_error(error)),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn delete(account: &str) -> Result<(), String> {
    validate_account(account)?;
    Err("the native credential store is supported only on macOS and Windows".to_string())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account).map_err(redacted_error)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn redacted_error(error: keyring::Error) -> String {
    match error {
        keyring::Error::NoEntry => "credential is not stored".to_string(),
        keyring::Error::Ambiguous(_) => "credential store returned an ambiguous entry".to_string(),
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
}
